#!/usr/bin/env python3
"""The advisory regressions on every published version: each affected
version must still LEAK, the candidate must be safe the way it is
expected to be.

    scripts/advisory-history.py build [--cache DIR]
    scripts/advisory-history.py run --dump DIR --candidate <kasane-pdf> --json OUT
        [--expected compat/advisories/history.json] [--cache DIR]
        [--private-inputs DIR --private-json OUT2]
    scripts/advisory-history.py update-expected --results OUT --expected FILE [--accept]

**Old versions** come from crates.io, never from this checkout: for each
version of `history.json` the `.crate` is downloaded and its SHA-256
checked against the crates.io index's checksum; its
`.cargo_vcs_info.json` must name the commit of the git tag `v<version>`
and every file of the package must be that tag's (Cargo.toml against
its `.orig`; `.cargo_vcs_info.json` and the generated Cargo.toml
aside). It is then built `--locked`, release, `--bin kasane-pdf`, with
the features of `history.json` (default: none beyond the defaults) and
the toolchain of `history.json`. The record (version, tag, commit,
source, crate digest, toolchain, features, binary SHA-256, platform)
is kept in the cache (`~/.cache/kasane-pdf/advisory-history/`); a cached
binary is used only when its digest still matches its record.

**Judging** (`check-advisory-regressions.py`'s `judge_case`): LEAK,
REMOVED, REFUSED, KEPT, LOST, FAST, SLOW, TIMEOUT, CRASH, ERROR, NA. A
`safe-text` case that the CLI completed is also judged by MuPDF and
pdf.js (text extracted) and by Poppler, MuPDF and pdf.js renders (ink
left unchanged inside the rect): any of them finding it is a LEAK with
that reason; a witness that fails is an ERROR. kasane's own reading is
never a witness.

**Deciding**: every cell (input × version, and `candidate`) must be the
state `history.json` expects — REMOVED and REFUSED are not
interchangeable (a candidate refusing what it is expected to redact is
`unexpected_refusal`; the reverse, `unexpected_success`). Independently
of the expected states, each advisory's affected range is checked
against the observations: every version in it has at least one input of
the advisory that LEAKs (or is SLOW, for `fast` inputs), no version
outside it has one, and the candidate has none. A missing input, an
input `history.json` does not know, a build that failed, a crate that
is not its tag, a binary whose digest changed: never a pass.

**Private inputs** (`--private-inputs DIR`: manifest-*.tsv in the same
format, and DIR/history-expected.json): judged the same way, the detail
written only to --private-json; the public JSON gets their counts per
state, nothing else. Refused when run in GitHub Actions.

`update-expected` writes the observed states as the expected ones, only
on request: it prints every cell that changes, refuses any change that
would make a candidate state unsafe or an error, or that the range
check fails, and refuses a LEAK no longer reproduced unless --accept.
"""
import argparse
import concurrent.futures
import datetime
import hashlib
import importlib.util
import io
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INDEX = "https://index.crates.io/ka/sa/kasane-pdf"
CRATE = "https://static.crates.io/crates/kasane-pdf/kasane-pdf-{v}.crate"
SAFE_CANDIDATE = ("REMOVED", "REFUSED", "KEPT", "FAST")
VULNERABLE = ("LEAK", "SLOW")
NOT_OBSERVED = ("TIMEOUT", "CRASH", "ERROR")
VERSION = re.compile(r"^\d+\.\d+\.\d+$")


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / file)
    mod = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, [sys.argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


car = load("check_advisory_regressions", "check-advisory-regressions.py")


def cache_dir(a):
    base = a.cache or os.environ.get("KASANE_HISTORY_CACHE") or (
        Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "kasane-pdf" / "advisory-history")
    p = Path(base)
    p.mkdir(parents=True, exist_ok=True)
    return p


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def sha256_file(p):
    return sha256(Path(p).read_bytes())


def parse_version(v):
    return tuple(int(x) for x in v.split("."))


def in_range(v, rng):
    """`rng`: a list of `>=x`, `>x`, `<x`, `<=x` bounds (GHSA style)."""
    for b in rng:
        m = re.fullmatch(r"(>=|>|<=|<)\s*(\d+\.\d+\.\d+)", b.strip())
        if not m:
            raise SystemExit(f"advisory-history: a bound it cannot read: {b!r}")
        op, x = m.group(1), parse_version(m.group(2))
        vv = parse_version(v)
        if not {"<": vv < x, "<=": vv <= x, ">": vv > x, ">=": vv >= x}[op]:
            return False
    return True


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "kasane-pdf advisory-history (maintainer)"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


# ---- old versions from crates.io --------------------------------------------


def tag_commit(v, repo=ROOT):
    r = subprocess.run(["git", "-C", str(repo), "rev-parse", "--verify", "--quiet", f"v{v}^{{commit}}"],
                       capture_output=True, text=True)
    return r.stdout.strip() or None


def verify_crate_against_tag(v, data, commit, repo=ROOT):
    """Problems (closed vocabulary) comparing the package with the tag."""
    problems = []
    tf = tarfile.open(fileobj=io.BytesIO(data), mode="r:gz")
    prefix = f"kasane-pdf-{v}/"
    files = {}
    for m in tf.getmembers():
        if not m.name.startswith(prefix) or ".." in m.name.split("/") or m.issym() or m.islnk():
            problems.append("crate_entry")
            continue
        if m.isfile():
            files[m.name[len(prefix):]] = tf.extractfile(m).read()
    vcs = json.loads(files.get(".cargo_vcs_info.json", b"{}") or b"{}")
    if (vcs.get("git") or {}).get("sha1") != commit or vcs.get("git", {}).get("dirty"):
        problems.append("vcs_not_tag")
    for path, content in files.items():
        if path in (".cargo_vcs_info.json", "Cargo.toml"):
            continue
        tag_path = "Cargo.toml" if path == "Cargo.toml.orig" else path
        r = subprocess.run(["git", "-C", str(repo), "cat-file", "blob", f"{commit}:{tag_path}"], capture_output=True)
        if r.returncode != 0:
            if path == "Cargo.lock":
                continue  # generated into the package when the tag has none
            problems.append("file_not_in_tag")
        elif r.stdout != content:
            problems.append("file_differs_from_tag")
    return problems, files


def build_version(v, cfg, cache, log):
    """The record of a published version's CLI, building it if needed."""
    rec_path = cache / "records" / f"{v}.json"
    bin_path = cache / "bin" / v / "kasane-pdf"
    want = {"toolchain": cfg["toolchain"], "features": cfg["features"]}
    if rec_path.is_file() and bin_path.is_file():
        rec = json.loads(rec_path.read_text())
        if {k: rec.get(k) for k in want} == want and rec.get("status") == "built":
            if sha256_file(bin_path) != rec["binary_sha256"]:
                return dict(rec, status="error", problem="binary_digest")
            return rec
    rec = {"version": v, "tag": f"v{v}", "source": "crates.io", **want,
           "platform": f"{platform.system()}-{platform.machine()}", "status": "error"}
    commit = tag_commit(v)
    rec["commit"] = commit
    if not commit:
        return dict(rec, problem="no_tag")
    index = [json.loads(l) for l in fetch(INDEX).decode().splitlines() if l.strip()]
    entry = next((e for e in index if e["vers"] == v), None)
    if not entry or entry.get("yanked"):
        return dict(rec, problem="not_published")
    data = fetch(CRATE.format(v=v))
    rec["crate_sha256"] = sha256(data)
    if rec["crate_sha256"] != entry["cksum"]:
        return dict(rec, problem="crate_checksum")
    problems, files = verify_crate_against_tag(v, data, commit)
    if problems:
        return dict(rec, problem="crate_not_tag:" + ",".join(sorted(set(problems))))
    src = cache / "src" / v
    shutil.rmtree(src, ignore_errors=True)
    for path, content in files.items():
        p = src / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(content)
    argv = ["cargo", f"+{cfg['toolchain']}", "build", "--locked", "--release", "--bin", "kasane-pdf"]
    if cfg["features"]:
        argv += ["--features", ",".join(cfg["features"])]
    env = dict(os.environ, CARGO_TARGET_DIR=str(cache / "target" / v), CARGO_TERM_COLOR="never")
    env.pop("RUSTFLAGS", None)
    with open(log, "ab") as fh:
        fh.write(f"$ {' '.join(argv)}  (kasane-pdf {v} from crates.io)\n".encode())
        fh.flush()
        r = subprocess.run(argv, cwd=src, env=env, stdout=fh, stderr=subprocess.STDOUT)
    built = cache / "target" / v / "release" / "kasane-pdf"
    if r.returncode != 0 or not built.is_file():
        return dict(rec, problem="build_failed")
    bin_path.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, bin_path)
    bin_path.chmod(0o755)
    rustc = subprocess.run(["rustc", f"+{cfg['toolchain']}", "-V"], capture_output=True, text=True).stdout.strip()
    rec.update(status="built", rustc=rustc, binary_sha256=sha256_file(bin_path),
               built_at=datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"))
    rec_path.parent.mkdir(parents=True, exist_ok=True)
    rec_path.write_text(json.dumps(rec, indent=1, sort_keys=True) + "\n")
    shutil.rmtree(src, ignore_errors=True)
    return rec


# ---- judging -----------------------------------------------------------------


def rect_of(cli):
    parts = cli.split("|")[0].split()
    if "--rect" not in parts:
        return None
    i = parts.index("--rect")
    return [float(x) for x in parts[i + 1:i + 5]]


def witnesses(vr, src, out, expect, cli):
    """More readers on a completed safe-text case: (reasons, error)."""
    reasons = []
    secret = expect.split(":", 1)[1]
    for rd in ("mupdf", "pdfjs"):
        t = vr.text(rd, out)
        if t is None:
            return reasons, f"text_failed:{rd}"
        if vr.norm(secret) in vr.norm(t):
            reasons.append(f"text:{rd}")
    rect = rect_of(cli)
    if rect:
        with tempfile.TemporaryDirectory() as tmp:
            for rd in vr.RENDERERS:
                try:
                    c = vr.compare(vr.render(rd, src, os.path.join(tmp, f"{rd}-in")),
                                   vr.render(rd, out, os.path.join(tmp, f"{rd}-out")), rect, False)
                except Exception:  # noqa: BLE001 — a renderer that fails is an error, never "clean"
                    return reasons, f"render_failed:{rd}"
                if c is None:
                    return reasons, f"size_changed:{rd}"
                if c["ink_left"]:
                    reasons.append(f"ink:{rd}")
    return reasons, None


def judge_cell(vr, kasane, src, cli, expect, usage):
    with tempfile.TemporaryDirectory() as tmp:
        r = car.judge_case(kasane, src, cli, expect, tmp, usage)
        cell = {"state": r["state"]}
        if r["state"] == "LEAK" and expect.startswith("safe-text:"):
            cell["reasons"] = ["text:poppler"]
        if r["state"] == "REMOVED" and expect.startswith("safe-text:") and r["output"]:
            reasons, error = witnesses(vr, src, r["output"], expect, cli)
            if error:
                cell = {"state": "ERROR", "reasons": [error]}
            elif reasons:
                cell = {"state": "LEAK", "reasons": reasons}
        elif r["state"] == "LEAK" and expect.startswith("safe-text:") and r["output"]:
            reasons, _ = witnesses(vr, src, r["output"], expect, cli)
            cell["reasons"] = ["text:poppler"] + reasons
        return cell


def evaluate(rows, dump, binaries, usage, vr, jobs):
    """{input: {version: cell}} for every input and binary."""
    out = {name: {} for name, *_ in rows}
    tasks = []
    for name, ghsa, cli, expect in rows:
        src = os.path.join(dump, name)
        for label, path in binaries.items():
            tasks.append((name, label, path, src, cli, expect))

    def one(t):
        name, label, path, src, cli, expect = t
        if path is None:
            return name, label, {"state": "ERROR", "reasons": ["build_failed"]}
        if not os.path.isfile(src):
            return name, label, {"state": "ERROR", "reasons": ["input_missing"]}
        return name, label, judge_cell(vr, path, src, cli, expect, usage[label])

    with concurrent.futures.ThreadPoolExecutor(jobs) as ex:
        for name, label, cell in ex.map(one, tasks):
            out[name][label] = cell
    return out


def decide(rows, cells, expected, advisories, versions):
    """Problems (closed vocabulary) and the range check per advisory."""
    problems = []
    names = {r[0] for r in rows}
    meta = {r[0]: (r[1], r[3]) for r in rows}
    exp = expected.get("expected", {})
    for n in sorted(set(exp) - names):
        problems.append(f"input_missing:{n}")
    for n in sorted(names - set(exp)):
        problems.append(f"input_new:{n}")
    for n in sorted(names & set(exp)):
        for label, cell in cells[n].items():
            want = exp[n].get(label)
            got = cell["state"]
            if label == "candidate" and got not in SAFE_CANDIDATE:
                problems.append(f"candidate_unsafe:{n}:{got}")
            if got in NOT_OBSERVED and want != got:
                problems.append(f"not_observed:{n}:{label}:{got}")
            if want is None:
                problems.append(f"no_expectation:{n}:{label}")
            elif want != got:
                kind = "mismatch"
                if label == "candidate" and want == "REMOVED" and got == "REFUSED":
                    kind = "unexpected_refusal"
                elif label == "candidate" and want == "REFUSED" and got == "REMOVED":
                    kind = "unexpected_success"
                elif want in VULNERABLE and got not in VULNERABLE:
                    kind = "not_reproduced"
                problems.append(f"{kind}:{n}:{label}:{want}->{got}")
    ranges, untestable = {}, []
    for ghsa, adv in advisories.items():
        mine = [n for n in names if meta[n][0] == ghsa and not meta[n][1].startswith("kept-bytes:")]
        if not mine:
            # Only an advisory history.json says has no CLI input (and
            # why) may have none; it is listed as untestable, not passed.
            if adv.get("untestable"):
                untestable.append(f"{ghsa}:all")
                ranges[ghsa] = {"all": "untestable"}
            else:
                problems.append(f"range_no_inputs:{ghsa}")
            continue
        status = {}
        for v in versions + ["candidate"]:
            states = [cells[n].get(v, {}).get("state") for n in mine]
            vuln = any(s in VULNERABLE for s in states)
            affected = v != "candidate" and in_range(v, adv["affected"])
            if all(s == "NA" for s in states):
                # That version's CLI has no command for any input: its
                # place in the range cannot be checked here.
                status[v] = "untestable"
                if affected or v == "candidate":
                    untestable.append(f"{ghsa}:{v}")
                if v == "candidate":
                    problems.append(f"candidate_untestable:{ghsa}")
                continue
            status[v] = "vulnerable" if vuln else "not_vulnerable"
            if affected and not vuln:
                problems.append(f"range_not_reproduced:{ghsa}:{v}")
            if not affected and vuln:
                problems.append(f"range_outside:{ghsa}:{v}")
        ranges[ghsa] = status
    return problems, ranges, untestable


# ---- commands ------------------------------------------------------------------


def load_expected(path):
    e = json.loads(Path(path).read_text())
    for v in e["versions"]:
        if not VERSION.match(v):
            raise SystemExit(f"advisory-history: not a version: {v!r}")
    return e


def build_all(cfg, cache, log):
    recs = {}
    with concurrent.futures.ThreadPoolExecutor(2) as ex:
        for v, rec in zip(cfg["versions"], ex.map(lambda v: build_version(v, cfg, cache, log), cfg["versions"])):
            recs[v] = rec
    return recs


def record_problems(v, rec, cfg):
    """A version's record must be a build of that version, from crates.io,
    with the configured toolchain and features — nothing else stands in."""
    if rec.get("status") != "built":
        return [f"version:{v}:{rec.get('problem')}"]
    p = []
    for k in ("toolchain", "features"):
        if rec.get(k) != cfg[k]:
            p.append(f"version:{v}:{k}")
    if rec.get("version") != v or rec.get("source") != "crates.io" or rec.get("tag") != f"v{v}":
        p.append(f"version:{v}:record")
    return p


def private_summary(rows, cells, problems):
    """What the public JSON says of private inputs: counts only."""
    counts = {}
    for row in cells.values():
        for cell in row.values():
            counts[cell["state"]] = counts.get(cell["state"], 0) + 1
    return {"inputs": len(rows), "states": counts, "problems": len(problems),
            "status": "failed" if problems else "passed"}


def public_record(rec):
    keep = ("version", "tag", "commit", "source", "crate_sha256", "toolchain", "rustc", "features", "binary_sha256",
            "platform", "status", "problem")
    return {k: rec.get(k) for k in keep if k in rec}


def cmd_build(a):
    cfg = load_expected(a.expected)
    cache = cache_dir(a)
    recs = build_all(cfg, cache, cache / "build.log")
    for v, rec in recs.items():
        print(f"{v:8} {rec['status']:6} {rec.get('problem', '')} {rec.get('binary_sha256', '')[:16]}")
    return 0 if all(r["status"] == "built" for r in recs.values()) else 1


def cmd_run(a):
    if a.private_inputs and os.environ.get("GITHUB_ACTIONS"):
        raise SystemExit("advisory-history: private inputs are never run in GitHub Actions")
    if bool(a.private_inputs) != bool(a.private_json):
        raise SystemExit("advisory-history: --private-inputs and --private-json go together")
    vr = load("verify_renderers", "verify-renderers.py")
    cfg = load_expected(a.expected)
    cache = cache_dir(a)
    log = Path(a.log) if a.log else cache / "build.log"
    recs = build_all(cfg, cache, log)
    problems = [x for v, r in recs.items() for x in record_problems(v, r, cfg)]
    binaries = {v: str(cache / "bin" / v / "kasane-pdf") if r["status"] == "built" else None for v, r in recs.items()}
    binaries["candidate"] = os.path.abspath(a.candidate)
    usage = {k: car.usage_of(p) if p else "" for k, p in binaries.items()}
    tools = vr.tool_versions()
    if not all(tools.values()):
        raise SystemExit(f"advisory-history: a renderer did not report its version: {tools}")
    rows = car.read_manifests(a.dump)
    cells = evaluate(rows, a.dump, binaries, usage, vr, a.jobs)
    p2, ranges, untestable = decide(rows, cells, cfg, cfg["advisories"], cfg["versions"])
    problems += p2
    if tools != vr.tool_versions():
        problems.append("renderer_version_changed")
    private = None
    if a.private_inputs:
        prows = car.read_manifests(a.private_inputs)
        pexp_path = Path(a.private_inputs) / "history-expected.json"
        pcells = evaluate(prows, a.private_inputs, binaries, usage, vr, a.jobs)
        if pexp_path.is_file():
            pexp = json.loads(pexp_path.read_text())
            pproblems, pranges, _ = decide(prows, pcells, pexp, pexp.get("advisories", cfg["advisories"]),
                                           cfg["versions"])
        else:
            pproblems, pranges = ["private_no_expectations"], {}
        private = private_summary(prows, pcells, pproblems)
        Path(a.private_json).write_text(json.dumps({"cells": pcells, "problems": pproblems, "ranges": pranges},
                                                   indent=1, sort_keys=True) + "\n")
        if pproblems:
            problems.append("private_inputs_failed")
    status = "passed" if not problems else (
        "incomplete" if all(p.split(":")[0] in ("input_missing", "input_new", "no_expectation") for p in problems)
        else "failed")
    doc = {
        "schema_version": 1,
        "status": status,
        "problems": problems,
        "toolchain": cfg["toolchain"],
        "features": cfg["features"],
        "renderers": tools,
        "versions": {v: public_record(r) for v, r in recs.items()},
        "candidate": {"binary_sha256": sha256_file(binaries["candidate"])},
        "ranges": ranges,
        "untestable": untestable,
        "inputs": {r[0]: {"ghsa": r[1], "expect_kind": r[3].split(":")[0], "cells": cells[r[0]]} for r in rows},
        "private": private,
    }
    Path(a.json).write_text(json.dumps(doc, indent=1, sort_keys=True) + "\n")
    per = {}
    for row in cells.values():
        for label, cell in row.items():
            per.setdefault(label, {}).setdefault(cell["state"], 0)
            per[label][cell["state"]] += 1
    print(f"advisory-history: {status}; {len(rows)} inputs × {len(binaries)} binaries; renderers {tools}")
    for label in cfg["versions"] + ["candidate"]:
        print(f"  {label:10} {recs.get(label, {}).get('status', 'build') if label != 'candidate' else '':6} "
              + " ".join(f"{k}={v}" for k, v in sorted(per.get(label, {}).items())))
    for g, st in ranges.items():
        print(f"  {g}: " + " ".join(f"{v}:{'V' if s == 'vulnerable' else '-'}" for v, s in st.items()))
    for u in untestable:
        print("  untestable:", u)
    for p in problems:
        print("  problem:", p)
    return {"passed": 0, "incomplete": 3}.get(status, 1)


def cmd_update(a):
    res = json.loads(Path(a.results).read_text())
    exp = json.loads(Path(a.expected).read_text())
    blocking = [p for p in res["problems"] if p.split(":")[0] in (
        "candidate_unsafe", "not_observed", "range_not_reproduced", "range_outside", "range_no_inputs", "version",
        "renderer_version_changed", "private_inputs_failed")]
    if blocking:
        for p in blocking:
            print("REFUSED:", p)
        print("update-expected: these are failures, not expectations: fix them")
        return 1
    old = exp.get("expected", {})
    new = {n: {label: c["state"] for label, c in v["cells"].items()} for n, v in res["inputs"].items()}
    changes, lost_leaks = [], []
    for n in sorted(set(old) | set(new)):
        for label in sorted(set(old.get(n, {})) | set(new.get(n, {}))):
            o, w = old.get(n, {}).get(label), new.get(n, {}).get(label)
            if o != w:
                changes.append(f"{n} {label}: {o} -> {w}")
                if o in VULNERABLE and w not in VULNERABLE:
                    lost_leaks.append(f"{n} {label}")
    for c in changes:
        print("  change:", c)
    if lost_leaks and not a.accept:
        print(f"update-expected: {len(lost_leaks)} LEAK(s) no longer reproduced; investigate, then --accept")
        return 1
    if not changes:
        print("update-expected: no change")
        return 0
    exp["expected"] = new
    Path(a.expected).write_text(json.dumps(exp, indent=1, sort_keys=True) + "\n")
    print(f"update-expected: {len(changes)} cell(s) written to {a.expected}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    exp_default = str(ROOT / "compat" / "advisories" / "history.json")
    b = sub.add_parser("build")
    b.add_argument("--expected", default=exp_default)
    b.add_argument("--cache")
    r = sub.add_parser("run")
    r.add_argument("--dump", required=True)
    r.add_argument("--candidate", required=True)
    r.add_argument("--json", required=True)
    r.add_argument("--expected", default=exp_default)
    r.add_argument("--cache")
    r.add_argument("--log")
    r.add_argument("--jobs", type=int, default=os.cpu_count() or 2)
    r.add_argument("--private-inputs")
    r.add_argument("--private-json")
    u = sub.add_parser("update-expected")
    u.add_argument("--results", required=True)
    u.add_argument("--expected", default=exp_default)
    u.add_argument("--accept", action="store_true")
    a = ap.parse_args(argv)
    return {"build": cmd_build, "run": cmd_run, "update-expected": cmd_update}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
