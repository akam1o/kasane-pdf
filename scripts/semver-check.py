#!/usr/bin/env python3
"""The public API of this checkout against a published release: what
changed (detection), and whether the version change allows it
(judgement) — two separate steps (docs/api-stability.md).

    scripts/semver-check.py [--baseline-version X.Y.Z] [--json OUT] [--summary-md FILE]
    scripts/semver-check.py --baseline-crate FILE --baseline-sha256 HEX [...]   (offline, tests)

`cargo semver-checks check-release` alone decides from the version
numbers which lints to run: from 0.11.0 to 0.12.0 it runs none
("major change", every lint skipped) and reports success. Here:

- **Detection** runs, whatever the versions, every lint the tool does not
  call unnecessary (`--release-type patch`, `-vv`), against the baseline's
  published package: the `.crate` downloaded from crates.io and checked
  against the index's SHA-256, unpacked outside the checkout. Every lint
  that can require a major update must be listed as run; the ones left
  out must be exactly the tool's "unnecessary" count (minor additions,
  default level allow), recorded by name. A baseline that is the checkout
  itself, or inside it (its own `target/package/*.crate`), is refused;
  the baseline and the candidate having the same version (main between
  releases) is normal. A `--baseline-crate` file's provenance is the
  caller's word (tests, offline use): the gate always downloads
  (check-workflows.py).
- **Judgement**, per finding, by its lint's required update
  (`cargo semver-checks --list`): a `major` lint (a breaking change) is
  allowed only by a version change Cargo treats as incompatible (0.y →
  0.y+1, or a major bump), and then only when docs/api-stability.md's
  `## <version>` section names the lint and each item (whether its text
  explains them is the reviewer's call); a `minor` lint (an addition) is
  allowed by any version, as Cargo's rules for 0.x allow it in a patch.
  Every finding is listed, the allowed ones as allowed and why — nothing
  is suppressed.

Not run, failed, or not parsed is exit 2, never a pass: the tool or its
`--list` missing or failing, a build or rustdoc error, an exit status
other than 0 (no failing check) or 100 (failing checks, fully reported),
another crate, mode or versions than asked, zero checks, per-lint states
that do not add up to the totals, failures or warnings described that are
not the ones listed, an unknown, duplicated or mislevelled lint, left-out
lints that are not the tool's "unnecessary" ones or include a
breaking-change lint, a summary that does not match. A finding the
version does not allow is exit 1. Exit 0: the analysis ran in full and
every finding is allowed.

What it does not see is what cargo-semver-checks has no lint for — a
function parameter's type, for one (docs/api-stability.md).
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

CRATE = "kasane-pdf"
INDEX = "https://index.crates.io/ka/sa/kasane-pdf"
DOWNLOAD = "https://static.crates.io/crates/{name}/{name}-{version}.crate"
UA = {"User-Agent": "kasane-pdf semver-check (https://github.com/akam1o/kasane-pdf)"}

CHECKING = re.compile(r"^\s*Checking (\S+) v(\S+) -> v(\S+) \((.+)\)\s*$", re.M)
CHECKED = re.compile(r"^\s*Checked \[[^\]]*\] (\d+) checks?: (\d+) pass(?:, (\d+) fail)?(?:, (\d+) warn)?(?:, (\d+) skip)?",
                     re.M)
FINDING = re.compile(r"^--- (failure|warning) ([a-z0-9_]+): (.*) ---$", re.M)
SUMMARY = re.compile(r"^\s*Summary (.+)$", re.M)
# `-vv`: one line per lint that ran, and how many it left out.
RAN = re.compile(r"^\s*(PASS|FAIL|WARN) \[[^\]]*\]\s+(major|minor)\s+([a-z0-9_]+)\s*$", re.M)
STARTING = re.compile(r"^\s*Starting (\d+) checks?, (\d+) unnecessary\b", re.M)
# The tool's exit status with failing checks (0 without).
FINDINGS_EXIT = 100
REQUIRES = re.compile(r"semver requires new (major|minor) version: (\d+) major and (\d+) minor checks? failed")
# A lint id or a Rust path, whole (docs/api-stability.md is matched by these).
TOKEN = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*")


def parse_version(v):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", v or "")
    return tuple(int(x) for x in m.groups()) if m else None


def bump(base, cand):
    """'same', 'compatible' (Cargo: a patch, or a minor from 1.0 on) or
    'incompatible' (0.y -> 0.y+1, 0.0.z -> 0.0.z+1, a major); 'downgrade'
    for a candidate older than the baseline."""
    b, c = parse_version(base), parse_version(cand)
    if b is None or c is None:
        return None
    if c == b:
        return "same"
    if c < b:
        return "downgrade"
    key = lambda v: (v[0],) if v[0] else (0, v[1]) if v[1] else (0, 0, v[2])
    return "compatible" if key(c) == key(b) else "incompatible"


def parse_output(text):
    """The tool's report: {'checking': (name, base, cand, mode),
    'checks': {...}, 'findings': [...], 'summary': str} — fields missing
    where the text does not have them."""
    out = {"findings": []}
    m = CHECKING.search(text)
    if m:
        out["checking"] = m.groups()
    m = CHECKED.search(text)
    if m:
        total, p, f, w, s = (int(x) if x else 0 for x in m.groups())
        out["checks"] = {"total": total, "pass": p, "fail": f, "warn": w, "skip": s}
    marks = list(FINDING.finditer(text))
    for i, m in enumerate(marks):
        body = text[m.end(): marks[i + 1].start() if i + 1 < len(marks) else len(text)]
        items = []
        if "Failed in:" in body:
            for line in body.split("Failed in:", 1)[1].splitlines()[1:]:
                # Each place is indented two spaces; the tool's progress
                # (stderr, after it here) four or more.
                if not re.match(r"  \S", line):
                    break
                items.append(re.sub(r", previously in file .*$|, in file .*$|^\s+", "", line).strip())
        out["findings"].append({"kind": m.group(1), "id": m.group(2), "title": m.group(3), "items": items})
    m = SUMMARY.search(text)
    if m:
        out["summary"] = m.group(1).strip()
    ran = [(m.group(3), m.group(1), m.group(2)) for m in RAN.finditer(text)]
    out["ran"] = {i: (state, level) for i, state, level in ran}
    out["ran_duplicates"] = sorted({i for i, _, _ in ran if sum(1 for j, _, _ in ran if j == i) > 1})
    m = STARTING.search(text)
    if m:
        out["starting"] = (int(m.group(1)), int(m.group(2)))
    return out


def lint_levels(tool):
    """{lint: 'major' | 'minor'} from `--list`; {} unless it ran and every
    row parsed."""
    r = subprocess.run(tool + ["--list"], capture_output=True, text=True, timeout=120)
    if r.returncode:
        return {}
    levels = {}
    for line in r.stdout.splitlines():
        p = line.split()
        if len(p) >= 2 and p[1] in ("major", "minor"):
            if p[0] in levels:
                return {}
            levels[p[0]] = p[1]
    return levels


def judge(parsed, levels, base_version, cand_version, stability_sections, crate=CRATE):
    """(problems_not_run, findings judged, verdict problems).
    `stability_sections`: {version: that section's text}."""
    not_run = []
    ch = parsed.get("checking")
    if not ch:
        not_run.append("no 'Checking <crate> vX -> vY (...)' line: the analysis did not run")
    elif ch[0] != crate:
        not_run.append(f"the tool checked {ch[0]}, not {crate}")
    elif ch[3] != "assume patch change":
        not_run.append(f"the tool ran as '{ch[3]}', not every lint ('assume patch change')")
    elif (ch[1], ch[2]) != (base_version, cand_version):
        not_run.append(f"the tool compared v{ch[1]} -> v{ch[2]}, not v{base_version} -> v{cand_version}")
    c = parsed.get("checks")
    if not c:
        not_run.append("no 'Checked … N checks' line")
    elif c["total"] == 0:
        not_run.append("zero checks ran")
    elif c["pass"] + c["fail"] + c["warn"] != c["total"]:
        not_run.append(f"check counts do not add up: {c}")
    # Every lint that ran is listed; the ones that did not are the
    # tool's "unnecessary" additions (default level allow) — never a
    # breaking-change lint.
    ran = parsed.get("ran") or {}
    if parsed.get("ran_duplicates"):
        not_run.append(f"lints listed twice: {', '.join(parsed['ran_duplicates'])}")
    unknown = sorted(set(ran) - set(levels))
    if unknown:
        not_run.append(f"lints unknown to `--list`: {', '.join(unknown)}")
    wrong = sorted(i for i, (_, lv) in ran.items() if i in levels and levels[i] != lv)
    if wrong:
        not_run.append(f"lints listed at another level than `--list`: {', '.join(wrong)}")
    states = {k: sorted(i for i, (st, _) in ran.items() if st == k) for k in ("PASS", "FAIL", "WARN")}
    if c:
        if len(ran) != c["total"]:
            not_run.append(f"{c['total']} checks reported, {len(ran)} listed")
        for k, n in (("PASS", c["pass"]), ("FAIL", c["fail"]), ("WARN", c["warn"])):
            if len(states[k]) != n:
                not_run.append(f"{n} {k} reported, {len(states[k])} listed")
    skipped = sorted(set(levels) - set(ran))
    parsed["skipped"] = skipped
    unrun = [l for l in skipped if levels[l] == "major"]
    if unrun:
        not_run.append(f"breaking-change lints did not run: {', '.join(unrun)}")
    st = parsed.get("starting")
    if not st:
        not_run.append("no 'Starting N checks, M unnecessary' line")
    elif c and (st[0] != c["total"] or st[1] != len(skipped) or c["skip"] != st[1]):
        not_run.append(f"the left-out lints are not the tool's unnecessary ones: starting {st}, skipped {len(skipped)}")
    for kind, state in (("failure", "FAIL"), ("warning", "WARN")):
        described = sorted(f["id"] for f in parsed["findings"] if f["kind"] == kind)
        if described != states[state]:
            not_run.append(f"{kind}s described {described} are not the lints listed {state} {states[state]}")
    summary = parsed.get("summary") or ""
    if c and c["fail"] == 0 and summary != "no semver update required":
        not_run.append(f"summary '{summary}' with no failing check")
    if c and c["fail"] > 0:
        m = REQUIRES.fullmatch(summary)
        by_level = {lv: sum(1 for i in states["FAIL"] if levels.get(i) == lv) for lv in ("major", "minor")}
        want = "major" if by_level["major"] else "minor"
        if not m or (m.group(1), int(m.group(2)), int(m.group(3))) != (want, by_level["major"], by_level["minor"]):
            not_run.append(f"summary '{summary}' does not match the failing lints listed {by_level}")
    kind = bump(base_version, cand_version)
    if kind in (None, "downgrade"):
        not_run.append(f"versions not comparable: {base_version} -> {cand_version}")
    judged, refused = [], []
    for f in parsed["findings"]:
        level = levels.get(f["id"])
        j = dict(f, level=level)
        if level is None:
            not_run.append(f"lint {f['id']} unknown to `--list`")
            j["allowed"], j["why"] = False, "unknown lint"
        elif level == "minor":
            j["allowed"], j["why"] = True, "an addition: allowed by any version (Cargo's 0.x rules)"
        elif kind == "incompatible" and cand_version in stability_sections:
            # Whole tokens: `function_missing_extra` does not name
            # `function_missing`, nor `kasane_pdf::f_extra` `kasane_pdf::f`.
            named = set(TOKEN.findall(stability_sections[cand_version]))
            missing = [x for x in [f["id"]] + [item_path(i) or i for i in f["items"]] if x not in named]
            if missing or not f["items"]:
                j["allowed"], j["why"] = False, (f"breaking: {base_version} -> {cand_version} allows it, but "
                                                 f"docs/api-stability.md § {cand_version} does not name "
                                                 f"{', '.join(missing) or 'the items (none reported)'}")
            else:
                j["allowed"], j["why"] = True, (f"breaking: allowed by {base_version} -> {cand_version}; "
                                                f"docs/api-stability.md § {cand_version} names the lint and "
                                                f"the items (its text is for the reviewer to judge)")
        elif kind == "incompatible":
            j["allowed"], j["why"] = False, (f"breaking: {base_version} -> {cand_version} allows it, but "
                                             f"docs/api-stability.md has no '## {cand_version}' section")
        else:
            j["allowed"], j["why"] = False, (f"breaking: needs an incompatible version change "
                                             f"({base_version} -> {cand_version} is {kind})")
        if not j["allowed"]:
            refused.append(f"{f['id']}: {j['why']}")
        judged.append(j)
    return not_run, judged, refused


def item_path(item):
    """The path an item of "Failed in" names (`function kasane_pdf::f` ->
    `kasane_pdf::f`), or None."""
    m = re.search(r"([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)", item)
    return m.group(1) if m else None


def stability_sections(path):
    """{version: the text of its `## <version>` section}."""
    try:
        text = open(path, encoding="utf-8").read()
    except OSError:
        return {}
    out = {}
    for part in re.split(r"^## ", text, flags=re.M)[1:]:
        m = re.match(r"(\d+\.\d+\.\d+)\b", part)
        if m:
            out[m.group(1)] = part
    return out


def published(version=None):
    """(version, sha256) of the newest non-yanked release, or of `version`."""
    req = urllib.request.Request(INDEX, headers=UA)
    rows = [json.loads(l) for l in urllib.request.urlopen(req, timeout=60).read().decode().splitlines() if l.strip()]
    rows = [r for r in rows if not r.get("yanked") and parse_version(r["vers"])]
    if version:
        rows = [r for r in rows if r["vers"] == version]
    if not rows:
        raise ValueError(f"no published {CRATE} {version or ''} in the index")
    r = max(rows, key=lambda r: parse_version(r["vers"]))
    return r["vers"], r["cksum"]


def download(version, dest):
    req = urllib.request.Request(DOWNLOAD.format(name=CRATE, version=version), headers=UA)
    with urllib.request.urlopen(req, timeout=300) as r, open(dest, "wb") as fh:
        shutil.copyfileobj(r, fh)


def unpack(crate, into):
    with tarfile.open(crate, "r:gz") as t:
        t.extractall(into, filter="data")
    roots = [os.path.join(into, d) for d in os.listdir(into)]
    if len(roots) != 1 or not os.path.isfile(os.path.join(roots[0], "Cargo.toml")):
        raise ValueError("the .crate does not hold one package")
    return roots[0]


def package_of(manifest_dir):
    r = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--manifest-path",
                        os.path.join(manifest_dir, "Cargo.toml")], capture_output=True, text=True, timeout=300)
    if r.returncode:
        raise ValueError(f"cargo metadata failed in {manifest_dir}")
    pkgs = json.loads(r.stdout)["packages"]
    if len(pkgs) != 1:
        raise ValueError("expected one package")
    return pkgs[0]["name"], pkgs[0]["version"]


def inside(a, b):
    a, b = os.path.realpath(a), os.path.realpath(b)
    return a == b or a.startswith(b + os.sep)


def run(a):
    doc = {"schema_version": 1, "problems_not_run": [], "problems": []}
    cand = os.path.realpath(a.manifest_dir)
    tool = a.tool.split()
    r = subprocess.run(tool + ["--version"], capture_output=True, text=True)
    doc["tool"] = (r.stdout or r.stderr).strip() if r.returncode == 0 else None
    if not doc["tool"]:
        doc["problems_not_run"].append("cargo-semver-checks is not available")
        return doc
    name, cand_version = package_of(cand)
    doc["candidate"] = {"name": name, "version": cand_version}
    g = subprocess.run(["git", "-C", cand, "rev-parse", "HEAD"], capture_output=True, text=True)
    if g.returncode == 0:
        doc["candidate"]["commit"] = g.stdout.strip()
    work = tempfile.mkdtemp(prefix="semver-baseline-")
    try:
        if inside(work, cand):
            doc["problems_not_run"].append("the baseline would be unpacked inside the checkout")
            return doc
        if a.baseline_crate:
            # Offline: the caller vouches for where the file comes from;
            # never used by the gate (check-workflows.py).
            crate, want, source = a.baseline_crate, a.baseline_sha256, "file (caller-supplied provenance)"
            version = None
            # The checkout's own package (target/package/…) is not a release.
            if inside(crate, cand):
                doc["problems_not_run"].append("the baseline package is inside the checkout: a self-comparison")
                return doc
        else:
            version, want = published(a.baseline_version)
            crate, source = os.path.join(work, f"{CRATE}-{version}.crate"), DOWNLOAD.format(name=CRATE, version=version)
            download(version, crate)
        got = hashlib.sha256(open(crate, "rb").read()).hexdigest()
        doc["baseline"] = {"source": source, "sha256": got, "expected_sha256": want}
        if not want or got != want:
            doc["problems_not_run"].append("the baseline package's SHA-256 is not the expected one")
            return doc
        root = unpack(crate, os.path.join(work, "pkg"))
        if inside(root, cand) or inside(cand, root):
            doc["problems_not_run"].append("the baseline is the checkout, or inside it: a self-comparison")
            return doc
        bname, bversion = package_of(root)
        doc["baseline"].update(name=bname, version=bversion)
        try:
            doc["baseline"]["vcs"] = json.load(open(os.path.join(root, ".cargo_vcs_info.json")))["git"]["sha1"]
        except (OSError, ValueError, KeyError):
            doc["baseline"]["vcs"] = None
        if bname != name or (version and bversion != version):
            doc["problems_not_run"].append(f"the baseline package is {bname} {bversion}, not {name} {version}")
            return doc
        levels = lint_levels(tool)
        if not levels:
            doc["problems_not_run"].append("`cargo semver-checks --list` gave no lints")
            return doc
        argv = tool + ["check-release", "--manifest-path", os.path.join(cand, "Cargo.toml"), "--baseline-root", root,
                       "--release-type", "patch", "--color", "never", "-vv"]
        p = subprocess.run(argv, capture_output=True, text=True, timeout=3600, env=dict(os.environ, NO_COLOR="1"))
        text = re.sub(r"\x1b\[[0-9;]*m", "", p.stdout + "\n" + p.stderr)
        doc["exit"] = p.returncode
        parsed = parse_output(text)
        doc["mode"] = parsed.get("checking", (None,) * 4)[3]
        doc["checks"] = parsed.get("checks")
        doc["summary"] = parsed.get("summary")
        not_run, judged, refused = judge(parsed, levels, bversion, cand_version,
                                         stability_sections(os.path.join(cand, a.stability_doc)), crate=name)
        # 0 without failing checks, FINDINGS_EXIT with them; anything else
        # (a crash after part of a report, for one) did not run.
        fails = (parsed.get("checks") or {}).get("fail")
        if (p.returncode, bool(fails)) not in ((0, False), (FINDINGS_EXIT, True)):
            not_run.append(f"cargo semver-checks exited {p.returncode} with {fails} failing checks")
        doc["problems_not_run"] += not_run
        doc["findings"] = judged
        doc["skipped_lints"] = parsed.get("skipped")
        doc["problems"] = refused
        doc["version_change"] = bump(bversion, cand_version)
        if not_run:
            doc["tool_output_tail"] = text[-4000:]
        return doc
    finally:
        shutil.rmtree(work, ignore_errors=True)


def summary_md(doc):
    b, c = doc.get("baseline") or {}, doc.get("candidate") or {}
    lines = [f"### semver-checks: {c.get('name')} {b.get('version')} → {c.get('version')}", "",
             f"- tool: {doc.get('tool')}; baseline: {b.get('source')} (sha256 `{str(b.get('sha256'))[:16]}…`, "
             f"commit `{str(b.get('vcs'))[:12]}`); mode: {doc.get('mode')}; version change: {doc.get('version_change')}",
             f"- checks: {doc.get('checks')}"]
    for f in doc.get("findings") or []:
        lines.append(f"- {'allowed' if f['allowed'] else '**refused**'}: `{f['id']}` ({f['level']}) {f['title']} — {f['why']}"
                     + "".join(f"\n  - {i}" for i in f["items"]))
    for p in doc["problems_not_run"]:
        lines.append(f"- **not run**: {p}")
    return "\n".join(lines) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--manifest-dir", default=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--baseline-version")
    ap.add_argument("--baseline-crate")
    ap.add_argument("--baseline-sha256")
    ap.add_argument("--stability-doc", default=os.path.join("docs", "api-stability.md"))
    ap.add_argument("--tool", default="cargo semver-checks")
    ap.add_argument("--json")
    ap.add_argument("--summary-md")
    a = ap.parse_args(argv)
    if bool(a.baseline_crate) != bool(a.baseline_sha256):
        ap.error("--baseline-crate needs --baseline-sha256")
    if a.baseline_crate and a.baseline_version:
        ap.error("--baseline-crate or --baseline-version, not both")
    try:
        doc = run(a)
    except (OSError, ValueError, subprocess.SubprocessError, urllib.error.URLError) as e:
        doc = {"schema_version": 1, "problems_not_run": [f"{type(e).__name__}: {e}"], "problems": []}
    status = "not_run" if doc["problems_not_run"] else "failed" if doc["problems"] else "passed"
    doc["status"] = status
    if a.json:
        with open(a.json, "w") as fh:
            json.dump(doc, fh, indent=1)
    if a.summary_md:
        with open(a.summary_md, "a") as fh:
            fh.write(summary_md(doc))
    print(summary_md(doc), end="")
    print(f"semver-check: {status}")
    return {"passed": 0, "failed": 1}.get(status, 2)


if __name__ == "__main__":
    sys.exit(main())
