#!/usr/bin/env python3
"""Run, record and check the local part of the release gate on one commit
(docs/releasing.md, "The release gate"; docs/testing.md, "Release
verification").

    scripts/release-verify.py run   --ref <commit> [--out DIR] [--profile release] [--corpus DIR] [--keep]
    scripts/release-verify.py check --ref <commit> --results DIR [--profile release]
    scripts/release-verify.py report --results DIR [--public OUT.json]

`run` checks out <commit> into a fresh worktree (DIR/src), runs **that
commit's** copy of this script and of every tool it calls, and runs each
check of the profile (scripts/release-verify.json, read from the commit)
with its own timeout, collecting exit status, duration, peak memory, a
log and a summary. A check that fails does not stop the others; one
whose prerequisites are missing is not run and says why. Before and
after every check the worktree must still be the commit, unmodified.

Results (DIR, outside any checkout; private — logs name corpus ids and
may quote tool output):

    DIR/RUNNING            present until the run ends
    DIR/result.json        written last, atomically: the run's verdict
    DIR/checks/<name>.json one per check, atomically
    DIR/logs/<name>*.log   the checks' output (local only)
    DIR/work/              intermediate files (local only)

`check` decides whether DIR proves that <commit> passed the profile:
the commit and its tree, this script's and the config's git blobs in
that commit, the profile and its checks, every check `passed` with the
commit's configuration, the files' SHA-256 digests, one run id
throughout, the run's age, and — through the local ledger of runs — that
no newer run of the same commit and profile exists (an older success is
not evidence when the latest attempt failed or was interrupted).

What this proves and what it does not: it catches a mix-up (another
commit's results, another config, a partial or interrupted run, an
edited or truncated file, a stale run). The digests are integrity
checks against accidents, not signatures: anyone who can write DIR and
the ledger can forge a consistent set. The trust is in who ran it.

`report` prints a summary; --public writes the publishable projection
(closed vocabulary and numbers: statuses, counts, durations, tool
versions — no paths, ids, commands, logs or text from any file).
"""
import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

SCHEMA = 1
STATUSES = ("passed", "failed", "timeout", "error", "incomplete", "incomparable", "interrupted")
# The release profile's checks and their kinds: a commit's config cannot
# drop one or swap its kind and still be accepted for a release.
RELEASE_CHECKS = {
    "security_regressions": "security_regressions",
    "noninterference": "noninterference",
    "redact_corpus": "redact_corpus",
    "compat_corpus": "compat_corpus",
    "external_renderers": "external_renderers",
    "fuzz": "fuzz",
}
# The security-history profile (scripts/advisory-history.py) and its kind.
HISTORY_CHECKS = {"advisory_history": "advisory_history"}
PUBLIC_CHECKS = {**RELEASE_CHECKS, **HISTORY_CHECKS}
NAME = re.compile(r"^[a-z][a-z0-9_]{0,40}$")
FILE = re.compile(r"^[a-z][a-z0-9_.-]{0,80}$")
SHA = re.compile(r"^[0-9a-f]{40}$")
MAX_JSON = 4 << 20
MAX_LOG = 512 << 20


def now_utc():
    return datetime.datetime.now(datetime.timezone.utc)


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(s):
    return datetime.datetime.strptime(s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)


def git(repo, *args, check=True):
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    if check and r.returncode != 0:
        raise SystemExit(f"release-verify: git {' '.join(args)}: {r.stderr.strip()[:200]}")
    return r.stdout.strip()


def resolve(repo, ref):
    sha = git(repo, "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}", check=False)
    if not SHA.match(sha or ""):
        raise SystemExit(f"release-verify: {ref} is not a commit")
    return sha, git(repo, "rev-parse", f"{sha}^{{tree}}")


def blob(repo, sha, path):
    return git(repo, "rev-parse", "--verify", "--quiet", f"{sha}:{path}", check=False) or None


def hash_object(path):
    return subprocess.run(["git", "hash-object", "--no-filters", str(path)], capture_output=True, text=True,
                          check=True).stdout.strip()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def write_atomic(path, data):
    path = Path(path)
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=".tmp-")
    with os.fdopen(fd, "wb") as fh:
        fh.write(data)
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)


def dump(obj):
    return (json.dumps(obj, indent=1, sort_keys=True) + "\n").encode()


def ledger_path():
    p = os.environ.get("KASANE_RELEASE_VERIFY_LEDGER")
    if p:
        return Path(p)
    base = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    return Path(base) / "kasane-pdf" / "release-verify" / "ledger.jsonl"


def ledger_append(entry):
    """Append under an exclusive lock: the ledger's order is the order in
    which runs started (wall-clock times may tie or go backwards)."""
    import fcntl
    p = ledger_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    with open(p, "a") as fh:
        fcntl.flock(fh, fcntl.LOCK_EX)
        try:
            fh.write(json.dumps(entry, sort_keys=True) + "\n")
            fh.flush()
            os.fsync(fh.fileno())
        finally:
            fcntl.flock(fh, fcntl.LOCK_UN)


def ledger_entries():
    p = ledger_path()
    if not p.exists():
        return []
    out = []
    for line in p.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except ValueError:
            continue
    return out


# ---- tools and environment -------------------------------------------------

VERSION_CMDS = {
    "rustc": ["rustc", "-V"],
    "cargo": ["cargo", "-V"],
    "python": [sys.executable, "-V"],
    "qpdf": ["qpdf", "--version"],
    "poppler": ["pdftoppm", "-v"],
    "mupdf": ["mutool", "-v"],
    "node": ["node", "--version"],
}
VERSION_TEXT = re.compile(r"[A-Za-z0-9 .()+_:/-]{1,100}")
_V = r"(\d+\.\d+\.\d+(?:-(?:nightly|beta(?:\.\d+)?))?)"
TOOL_LINE = {
    "rustc": re.compile(rf"rustc {_V}( \([0-9a-f]{{9}} \d{{4}}-\d\d-\d\d\))?( \(Homebrew\))?"),
    "rustc_fuzz": re.compile(rf"rustc {_V}( \([0-9a-f]{{9}} \d{{4}}-\d\d-\d\d\))?"),
    "cargo": re.compile(rf"cargo {_V}( \([0-9a-f]{{9}} \d{{4}}-\d\d-\d\d\))?( \(Homebrew\))?"),
    "python": re.compile(rf"Python {_V}"),
    "qpdf": re.compile(rf"qpdf version {_V}"),
    "poppler": re.compile(rf"pdftoppm version {_V}"),
    "mupdf": re.compile(rf"mutool version {_V}"),
    "node": re.compile(rf"v{_V}"),
}


def tool_versions(toolchain=None):
    v = {}
    cmds = dict(VERSION_CMDS)
    if toolchain:
        cmds["rustc_fuzz"] = ["rustc", f"+{toolchain}", "-V"]
    for name, argv in cmds.items():
        if shutil.which(argv[0]) is None:
            v[name] = None
            continue
        r = subprocess.run(argv, capture_output=True, text=True)
        line = (r.stdout + r.stderr).strip().splitlines()
        line = line[0].strip() if line and r.returncode == 0 else None
        v[name] = line if line and VERSION_TEXT.fullmatch(line) else None
    return v


def environment(toolchain=None):
    mac = platform.mac_ver()[0]
    return {
        "os": platform.system(),
        "os_version": mac or platform.release(),
        "arch": platform.machine(),
        "tools": tool_versions(toolchain),
    }


# ---- running a command -------------------------------------------------------


class Runner:
    """Runs commands for one check: output to its log, a shared deadline,
    exit status, peak memory of the process tree's waited processes."""

    def __init__(self, out, src, corpus, name, deadline, env):
        self.out, self.src, self.corpus, self.name, self.deadline = out, src, corpus, name, deadline
        self.log = out / "logs" / f"{name}.log"
        self.env = env
        self.commands, self.max_rss_mb, self.timed_out = [], 0.0, False
        self.logs = {self.log.name}
        self.groups = set()

    def logfile(self, suffix):
        """logs/<check>-<suffix>.log: every log a check writes is there,
        named after it, and recorded with its digest."""
        return self.out / "logs" / f"{self.name}-{suffix}.log"

    def sanitize(self, s):
        s = str(s)
        for real, name in ((self.out, "$OUT"), (self.src, "$SRC"), (self.corpus, "$CORPUS"),
                           (os.path.expanduser("~"), "~")):
            if real:
                s = s.replace(str(real), name)
        return s

    def start(self, argv, cwd=None, env=None, log=None):
        log = Path(log or self.log)
        if log.parent != self.out / "logs" or not (log.name == f"{self.name}.log" or log.name.startswith(f"{self.name}-")):
            raise CheckFailed("error", "internal:log_path")
        self.logs.add(log.name)
        fh = open(log, "ab")
        fh.write(("$ " + " ".join(self.sanitize(a) for a in argv) + "\n").encode())
        fh.flush()
        p = subprocess.Popen(argv, cwd=cwd or self.src, env={**self.env, **(env or {})}, stdout=fh,
                             stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        p.log_name = log.name
        self.groups.add(p.pid)
        return p, fh

    def kill_group(self, pgid, grace=0):
        """TERM (if grace), then KILL the whole process group — whatever
        its leader did, its descendants go too."""
        if grace:
            try:
                os.killpg(pgid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                return
            end = time.monotonic() + grace
            while time.monotonic() < end:
                try:
                    os.killpg(pgid, 0)
                except (ProcessLookupError, PermissionError):
                    return
                time.sleep(0.2)
        try:
            os.killpg(pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass

    def kill_all(self):
        for g in list(self.groups):
            self.kill_group(g)
            self.groups.discard(g)

    def wait(self, p, fh):
        """(exit code, or -signal) — None when the deadline killed it."""
        status = None
        try:
            while True:
                pid, st, ru = os.wait4(p.pid, os.WNOHANG)
                if pid:
                    status = st
                    rss = ru.ru_maxrss / (1 << 20) if sys.platform == "darwin" else ru.ru_maxrss / 1024
                    self.max_rss_mb = max(self.max_rss_mb, rss)
                    break
                if time.monotonic() > self.deadline:
                    self.timed_out = True
                    self.kill_group(p.pid, grace=10)
                    try:
                        os.waitpid(p.pid, 0)
                    except ChildProcessError:
                        pass
                    p.returncode = -9
                    return None
                time.sleep(0.2)
        finally:
            # Whatever the command left behind in its group goes too.
            self.kill_group(p.pid)
            self.groups.discard(p.pid)
            fh.close()
        code = os.waitstatus_to_exitcode(status)
        p.returncode = code
        return code

    def run(self, argv, cwd=None, env=None, log=None):
        p, fh = self.start(argv, cwd, env, log)
        code = self.wait(p, fh)
        self.commands.append({"argv": [self.sanitize(a) for a in argv], "exit": code, "log": p.log_name})
        return code


class CheckFailed(Exception):
    def __init__(self, status, reason, summary=None):
        super().__init__(reason)
        self.status, self.reason, self.summary = status, reason, summary or {}


def need(code, what):
    """A step that must exit 0."""
    if code is None:
        raise CheckFailed("timeout", "timeout")
    if code < 0:
        raise CheckFailed("failed", f"crashed:{what}")
    if code != 0:
        raise CheckFailed("failed", f"exit:{what}")


def count_line(log, pattern):
    m = None
    for m in re.finditer(pattern, Path(log).read_text(errors="replace")):
        pass
    return m


# ---- the checks --------------------------------------------------------------


def build_kasane(r):
    need(r.run(["cargo", "build", "--locked", "--release", "--bin", "kasane-pdf"]), "build")
    return Path(r.env["CARGO_TARGET_DIR"]) / "release" / "kasane-pdf"


def advisory_dump(r, work):
    dump_dir = work / "advisories"
    shutil.rmtree(dump_dir, ignore_errors=True)
    dump_dir.mkdir(parents=True)
    need(r.run(["cargo", "test", "--locked", "--release", "--test", "redact_advisories"],
               env={"KASANE_ADVISORY_DUMP": str(dump_dir)}), "advisory_tests")
    return dump_dir


def check_security_regressions(r, cfg, work):
    k = build_kasane(r)
    log = r.logfile("verify-redact")
    need(r.run([sys.executable, "scripts/verify-redact.py", str(k)], log=log), "verify_redact")
    m = count_line(log, r"(\d+)/(\d+) cases as expected")
    if not m:
        raise CheckFailed("error", "no_summary:verify_redact")
    vr_ok, vr_total = int(m.group(1)), int(m.group(2))
    dump_dir = advisory_dump(r, work)
    alog = r.logfile("advisories")
    need(r.run([sys.executable, "scripts/check-advisory-regressions.py", str(k), str(dump_dir), "--expect-safe"], log=alog),
         "advisories")
    text = alog.read_text(errors="replace")
    verdicts = re.findall(r"^GHSA-\S+\s+\S+\s+([A-Z/]+)\b", text, re.M)
    safe = sum(v == "SAFE" for v in verdicts)
    summary = {"verify_redact_ok": vr_ok, "verify_redact_cases": vr_total, "advisory_safe": safe,
               "advisory_cases": len(verdicts), "kasane_sha256": sha256_file(k)}
    if vr_ok != vr_total or vr_total < cfg["min_verify_redact_cases"]:
        raise CheckFailed("failed", "verify_redact_cases", summary=summary)
    if safe != len(verdicts) or len(verdicts) < cfg["min_advisory_cases"]:
        raise CheckFailed("failed", "advisory_cases", summary=summary)
    return summary


def check_noninterference(r, cfg, work):
    log = r.logfile("model")
    code = r.run(["cargo", "test", "--locked", "--release", "--test", "redact_model", "--", "--nocapture"],
                 env={"KASANE_MODEL_SEEDS": str(cfg["seeds"])}, log=log)
    text = log.read_text(errors="replace")
    m = re.search(r"model: Tally \{ ok: (\d+), refused: (\d+)", text)
    summary = {"seeds": cfg["seeds"], "ok": int(m.group(1)) if m else None, "refused": int(m.group(2)) if m else None}
    need(code, "redact_model")
    if not m or not re.search(r"test result: ok\. 1 passed; 0 failed", text):
        raise CheckFailed("error", "no_summary:redact_model", summary=summary)
    # Two variants per seed: every document of every seed was redacted,
    # not a default seed count (an environment that did not reach the
    # test) or none.
    if summary["ok"] + summary["refused"] != 2 * cfg["seeds"] or summary["ok"] == 0:
        raise CheckFailed("failed", "seeds_mismatch", summary=summary)
    return summary


def read_manifest(src):
    files = []
    cur = None
    for line in (Path(src) / "compat" / "manifest.yaml").read_text().splitlines():
        m = re.match(r"^- id:\s*['\"]?([^'\"\s]+)", line)
        if m:
            cur = {"id": m.group(1)}
            files.append(cur)
            continue
        m = re.match(r"^\s+sha256:\s*['\"]?([0-9a-f]{64})", line)
        if m and cur is not None:
            cur["sha256"] = m.group(1)
    return files


def verify_corpus(src, corpus):
    """(problem or None, counts): every manifest file present, with its
    digest. Names come from the commit's manifest; files are read only
    as regular files directly in the corpus directory."""
    files = read_manifest(src)
    missing = mismatched = 0
    h = hashlib.sha256()
    for f in files:
        path = None
        for name in (f["id"] + ".pdf", f["id"]):
            if "/" in name or name.startswith("."):
                continue
            p = Path(corpus) / name
            if p.is_file() and not p.is_symlink():
                path = p
                break
        if path is None:
            missing += 1
            continue
        if sha256_file(path) != f.get("sha256"):
            mismatched += 1
        h.update(f"{f['id']} {f.get('sha256')}\n".encode())
    counts = {"files": len(files), "missing": missing, "mismatched": mismatched}
    if not files:
        return "corpus_manifest_empty", counts, None
    if missing or mismatched:
        return "corpus_incomplete", counts, None
    return None, counts, h.hexdigest()


def check_redact_corpus(r, cfg, work):
    k = build_kasane(r)
    summary = {"configs": {}}
    failed = []
    for name, args in cfg["configs"].items():
        raw, proj, cmp_ = work / f"eval-{name}.json", work / f"proj-{name}.json", work / f"cmp-{name}.json"
        extra = [x for a in args for x in ("--redact-arg", a)]
        code = r.run([sys.executable, "scripts/eval-redact-corpus.py", str(k), "--json", str(raw), *extra],
                     log=r.logfile(f"eval-{name}"))
        # 1: a run is `unexpected` (the known signal included); the
        # comparison decides. Anything else, or no results, fails.
        if code is None:
            raise CheckFailed("timeout", "timeout")
        if code not in (0, 1) or not raw.is_file() or raw.stat().st_size == 0:
            failed.append(name)
            summary["configs"][name] = {"status": "error"}
            continue
        need(r.run([sys.executable, "scripts/redact-results-projection.py", str(raw), str(proj)]), "projection")
        args2 = [sys.executable, "scripts/check-redact-corpus.py", name, str(proj), "--json", str(cmp_)]
        if cfg.get("strict_pixels"):
            args2.append("--strict-pixels")
        code = r.run(args2, log=r.logfile(f"cmp-{name}"))
        if code is None:
            raise CheckFailed("timeout", "timeout")
        doc = json.loads(cmp_.read_text()) if cmp_.is_file() else None
        runs = len(json.loads(proj.read_text())["results"])
        if doc is None:
            failed.append(name)
            summary["configs"][name] = {"status": "error"}
            continue
        summary["configs"][name] = {
            "status": doc["status"], "runs": runs, **{f"run_{k2}": v for k2, v in doc["counts"]["run"].items()},
            "regressions": len(doc["regressions"]), "improvements": len(doc["improvements"]),
            "reason_changes": len(doc["reason_changes"]), "lost": doc["lost"]["run"],
        }
        if code != 0 or doc["status"] != "passed" or runs != cfg["runs_per_config"]:
            failed.append(name)
    if failed:
        raise CheckFailed("failed", "regression", summary=summary)
    return summary


def check_compat_corpus(r, cfg, work):
    baseline = Path(r.src) / cfg["baseline"]
    if not baseline.is_file():
        raise CheckFailed("incomparable", "no_baseline")
    # As nightly.yml builds it; --locked: a stale lock fails here instead
    # of being rewritten in the worktree.
    need(r.run(["cargo", "build", "--locked", "--release", *[x for f in cfg.get("runner_features", []) for x in ("--features", f)]],
               cwd=Path(r.src) / "compat" / "runner"), "build_runner")
    runner = Path(r.env["CARGO_TARGET_DIR"]) / "release" / "kasane-compat"
    out = work / "run"
    out.mkdir(parents=True, exist_ok=True)
    need(r.run([str(runner), "run", "--corpus", str(r.corpus), "--out", str(out), *cfg["runner_args"]]), "runner")
    latest = out / "latest.json"
    if not latest.is_file():
        raise CheckFailed("error", "no_report")
    summ = work / "summary.json"
    with open(summ, "w") as fh:
        code = subprocess.run([sys.executable, "scripts/ci-regression.py", "summarize", str(latest)], cwd=r.src,
                              stdout=fh, stderr=subprocess.DEVNULL).returncode
    need(code, "summarize")
    agg = json.loads(summ.read_text())["aggregate"]
    base_files = json.loads(baseline.read_text())["aggregate"].get("files")
    summary = {k: v for k, v in agg.items() if isinstance(v, (int, float)) and not isinstance(v, bool)}
    summary["poppler"] = tool_versions().get("poppler")
    code = r.run([sys.executable, "scripts/ci-regression.py", "check", str(latest), str(baseline)],
                 log=r.logfile("regression"))
    if code is None:
        raise CheckFailed("timeout", "timeout")
    if agg.get("files") != cfg["files"] or base_files != cfg["files"]:
        raise CheckFailed("incomparable", "file_count", summary=summary)
    if agg.get("crash") or agg.get("timeout"):
        raise CheckFailed("failed", "crash_or_timeout", summary=summary)
    if code != 0:
        raise CheckFailed("failed", "regression", summary=summary)
    return summary


def check_external_renderers(r, cfg, work):
    k = build_kasane(r)
    need(r.run(["npm", "ci", "--no-audit", "--no-fund"], cwd=Path(r.src) / "tools" / "pdfjs"), "npm_ci")
    dump_dir = advisory_dump(r, work)
    out = work / "renderers.json"
    # The baseline of this platform (compat/renderers/baseline-<os>-<arch>.json):
    # another platform's is never used.
    code = r.run([sys.executable, "scripts/verify-renderers.py", str(k), str(dump_dir), "--json", str(out)],
                 log=r.logfile("renderers"))
    if code is None:
        raise CheckFailed("timeout", "timeout")
    if not out.is_file():
        raise CheckFailed("error", f"exit:verify_renderers:{code}")
    doc = json.loads(out.read_text())
    s = doc["summary"]
    summary = {"cases": s["cases"], "outcomes": s["outcomes"], "renderer_runs": s["renderer_runs"],
               "leaks": s["leaks"], "losses": s["losses"], "pixels": s["pixels"], "tools": doc["tools"],
               "incomparable": doc["incomparable"], "problems": len(doc["problems"])}
    if doc["status"] == "incomparable":
        raise CheckFailed("incomparable", "renderer_version", summary=summary)
    if doc["status"] != "passed" or code != 0:
        raise CheckFailed("failed", "leak" if s["leaks"] else "regression", summary=summary)
    return summary


def check_advisory_history(r, cfg, work):
    """Every published version of compat/advisories/history.json built from
    crates.io and judged with the commit's advisory inputs: affected
    versions LEAK, the candidate (this commit) as expected, each
    advisory's range as published."""
    k = build_kasane(r)
    need(r.run(["npm", "ci", "--no-audit", "--no-fund"], cwd=Path(r.src) / "tools" / "pdfjs"), "npm_ci")
    dump_dir = advisory_dump(r, work)
    out = work / "history.json"
    argv = [sys.executable, "scripts/advisory-history.py", "run", "--dump", str(dump_dir), "--candidate", str(k),
            "--json", str(out), "--log", str(r.logfile("builds"))]
    private = os.environ.get("KASANE_PRIVATE_ADVISORY_INPUTS")
    if private:
        argv += ["--private-inputs", private, "--private-json", str(work / "private.json")]
    code = r.run(argv, log=r.logfile("history"))
    if code is None:
        raise CheckFailed("timeout", "timeout")
    if not out.is_file():
        raise CheckFailed("error", f"exit:advisory_history:{code}")
    doc = json.loads(out.read_text())
    states = {}
    for row in doc["inputs"].values():
        for label, cell in row["cells"].items():
            states.setdefault(label, {}).setdefault(cell["state"], 0)
            states[label][cell["state"]] += 1
    summary = {
        "inputs": len(doc["inputs"]), "versions": len(doc["versions"]),
        "built": sum(v.get("status") == "built" for v in doc["versions"].values()),
        "advisories": len(doc["ranges"]), "untestable": len(doc["untestable"]), "problems": len(doc["problems"]),
        "states": states, "private_inputs": (doc.get("private") or {}).get("inputs", 0),
        "renderers": doc["renderers"],
        "binaries": {v: rec.get("binary_sha256") for v, rec in doc["versions"].items()},
    }
    if doc["status"] == "incomplete":
        raise CheckFailed("incomplete", "expectations", summary=summary)
    if doc["status"] != "passed" or code != 0:
        raise CheckFailed("failed", "history", summary=summary)
    return summary


ARTIFACT = re.compile(r"^(crash|timeout|oom|leak|slow-unit)-")


def check_fuzz(r, cfg, work):
    tc = cfg["toolchain"]
    fuzz = Path(r.src) / "fuzz"
    need(r.run(["cargo", f"+{tc}", "fuzz", "--version"], cwd=fuzz), "cargo_fuzz")
    targets = cfg["targets"]
    for t in targets:
        # `cargo fuzz build` takes one target.
        need(r.run(["cargo", f"+{tc}", "fuzz", "build", "-O", t], cwd=fuzz,
                   env={"CARGO_TARGET_DIR": str(fuzz / "target")}), "fuzz_build")
    procs = {}
    seeds = {}
    for t, seed in targets.items():
        corpus = work / f"corpus-{t}"
        shutil.rmtree(corpus, ignore_errors=True)
        corpus.mkdir(parents=True)
        for f in sorted((fuzz / "seeds" / t).iterdir()):
            if f.is_file() and not f.is_symlink():
                shutil.copyfile(f, corpus / f.name)
        seeds[t] = len(list(corpus.iterdir()))
        shutil.rmtree(fuzz / "artifacts" / t, ignore_errors=True)
        argv = ["fuzz/ci-run.sh", t, str(corpus), "--", f"-seed={seed}", f"-max_total_time={cfg['seconds']}"]
        procs[t] = r.start(argv, env={"FUZZ_TOOLCHAIN": tc, "CARGO_TARGET_DIR": str(fuzz / "target")},
                           log=r.logfile(t))
    summary = {"seconds": cfg["seconds"], "targets": {}}
    bad = []
    for t, (p, fh) in procs.items():
        code = r.wait(p, fh)
        r.commands.append({"argv": [r.sanitize(a) for a in p.args], "exit": code, "log": p.log_name})
        text = r.logfile(t).read_text(errors="replace")
        done = re.findall(r"Done (\d+) runs in (\d+) second", text)
        runs, secs = (int(done[-1][0]), int(done[-1][1])) if done else (0, 0)
        arts = [a.name for a in (fuzz / "artifacts" / t).iterdir()] if (fuzz / "artifacts" / t).is_dir() else []
        bins = sorted(fuzz.glob(f"target/*/release/{t}"))
        info = {
            "seed": targets[t], "seeds_replayed": seeds[t], "runs": runs, "elapsed_seconds": secs,
            "executed_beyond_seeds": max(0, runs - seeds[t] - 1),
            "new_inputs": len(list((work / f"corpus-{t}").iterdir())) - seeds[t],
            "crashes": sum(a.startswith(("crash-", "oom-", "leak-")) for a in arts),
            "timeouts": sum(a.startswith(("timeout-", "slow-unit-")) for a in arts),
            "exit": code, "binary_sha256": sha256_file(bins[0]) if bins else None,
        }
        summary["targets"][t] = info
        if code is None:
            bad.append(("timeout", "timeout"))
        elif code != 0 or info["crashes"] or info["timeouts"]:
            bad.append(("failed", f"fuzz_failed:{t}"))
        elif secs < cfg["seconds"] * cfg["min_fraction"]:
            bad.append(("incomplete", f"too_short:{t}"))
        elif info["executed_beyond_seeds"] <= 0:
            bad.append(("incomplete", f"no_new_inputs:{t}"))
    if bad:
        order = ("timeout", "failed", "incomplete")
        bad.sort(key=lambda b: order.index(b[0]))
        raise CheckFailed(*bad[0], summary=summary)
    return summary


def check_command(r, cfg, work):
    """A plain command (tests of this script): passes on exit 0."""
    argv = [a.replace("{src}", str(r.src)).replace("{work}", str(work)) for a in cfg["argv"]]
    need(r.run(argv), "command")
    return {"exit": 0}


KINDS = {
    "security_regressions": check_security_regressions,
    "noninterference": check_noninterference,
    "redact_corpus": check_redact_corpus,
    "compat_corpus": check_compat_corpus,
    "external_renderers": check_external_renderers,
    "fuzz": check_fuzz,
    "advisory_history": check_advisory_history,
    "command": check_command,
}
CORPUS_KINDS = ("redact_corpus", "compat_corpus")


# ---- run -----------------------------------------------------------------------


def worktree_state(src, sha, tree):
    """None when src is still exactly the commit; else a reason."""
    if git(src, "rev-parse", "HEAD", check=False) != sha:
        return "head_moved"
    if git(src, "rev-parse", "HEAD^{tree}", check=False) != tree:
        return "tree_changed"
    subprocess.run(["git", "-C", str(src), "update-index", "-q", "--refresh"], capture_output=True)
    if subprocess.run(["git", "-C", str(src), "diff-index", "--quiet", "HEAD", "--"]).returncode != 0:
        return "tracked_file_changed"
    if git(src, "status", "--porcelain", "--untracked-files=all", check=False):
        return "untracked_file_added"
    return None


def cmd_run(a):
    repo = Path(git(os.getcwd(), "rev-parse", "--show-toplevel"))
    if git(repo, "status", "--porcelain", "--untracked-files=no"):
        raise SystemExit("release-verify: this checkout has uncommitted changes — commit or stash them first, so "
                         "it is clear what is verified (the run itself uses a clean worktree of --ref)")
    sha, tree = resolve(repo, a.ref)
    if not blob(repo, sha, "scripts/release-verify.py") or not blob(repo, sha, "scripts/release-verify.json"):
        raise SystemExit(f"release-verify: {sha} has no scripts/release-verify.py / .json")
    base = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "kasane-pdf" / "release-verify"
    out = Path(a.out or base / f"{sha[:12]}-{now_utc().strftime('%Y%m%dT%H%M%SZ')}").absolute()
    if out.exists() and (out.is_symlink() or any(out.iterdir())):
        raise SystemExit(f"release-verify: {out} exists and is not empty: results go to a new directory")
    try:
        out.resolve().relative_to(repo.resolve())
        raise SystemExit("release-verify: the results directory must be outside the checkout")
    except ValueError:
        pass
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    src = out / "src"
    git(repo, "worktree", "add", "--detach", str(src), sha)
    corpus = Path(a.corpus or repo / "compat" / "corpus").absolute()
    try:
        inner = [sys.executable, str(src / "scripts" / "release-verify.py"), "_inner", "--out", str(out),
                 "--commit", sha, "--tree", tree, "--profile", a.profile, "--corpus", str(corpus)]
        for o in a.only or []:
            inner += ["--only", o]
        # macOS: no idle sleep while it runs (a sleeping machine stretches
        # the timeouts' wall-clock time and the per-file corpus limits).
        if sys.platform == "darwin" and shutil.which("caffeinate"):
            inner = ["caffeinate", "-i", *inner]
        env = dict(os.environ)
        if a.private_inputs:
            env["KASANE_PRIVATE_ADVISORY_INPUTS"] = str(Path(a.private_inputs).absolute())
        code = subprocess.run(inner, env=env).returncode
    finally:
        if not a.keep:
            git(repo, "worktree", "remove", "--force", str(src), check=False)
            shutil.rmtree(out / "target", ignore_errors=True)
            shutil.rmtree(out / "src", ignore_errors=True)
    print(f"release-verify: results in {out}")
    return code


def inner(a):
    out, sha, tree = Path(a.out), a.commit, a.tree
    src = out / "src"
    me = Path(__file__).resolve()
    # The script running is the commit's own copy.
    if me != (src / "scripts" / "release-verify.py").resolve() or hash_object(me) != blob(src, sha, "scripts/release-verify.py"):
        raise SystemExit("release-verify: the verifier is not the commit's")
    config_path = src / "scripts" / "release-verify.json"
    config = json.loads(config_path.read_text())
    profile = config["profiles"].get(a.profile)
    if profile is None:
        raise SystemExit(f"release-verify: no profile {a.profile}")
    names = [n for n in profile if not a.only or n in a.only]
    run_id = uuid.uuid4().hex
    (out / "checks").mkdir(exist_ok=True)
    (out / "logs").mkdir(exist_ok=True)
    (out / "work").mkdir(exist_ok=True)
    write_atomic(out / "RUNNING", f"{run_id}\n".encode())
    started = now_utc()
    ledger_append({"run_id": run_id, "commit": sha, "profile": a.profile if not a.only else "partial",
                   "started_at": iso(started), "event": "started"})
    toolchain = next((c.get("toolchain") for c in config["checks"].values() if c.get("toolchain")), None)
    env0 = environment(toolchain)
    child_env = dict(os.environ, CARGO_TARGET_DIR=str(out / "target"), KASANE_CORPUS_DIR=str(a.corpus),
                     CARGO_TERM_COLOR="never")
    child_env.pop("RUSTFLAGS", None)
    corpus_state = None
    results = {}
    interrupted = False
    for name in names:
        if not NAME.match(name):
            raise SystemExit(f"release-verify: bad check name {name}")
        cfg = config["checks"][name]
        t0 = now_utc()
        (out / "logs" / f"{name}.log").write_bytes(b"")
        work = out / "work" / name
        work.mkdir(parents=True, exist_ok=True)
        r = Runner(out, src, a.corpus, name, time.monotonic() + cfg["timeout"], child_env)
        status, reason, summary = "passed", None, {}
        try:
            missing = [t for t in cfg.get("tools", []) if shutil.which(t) is None]
            state = worktree_state(src, sha, tree)
            if state:
                raise CheckFailed("error", f"worktree:{state}")
            if missing:
                raise CheckFailed("error", "tool_missing:" + ",".join(sorted(missing)))
            if cfg["kind"] in CORPUS_KINDS:
                if corpus_state is None:
                    corpus_state = verify_corpus(src, a.corpus)
                problem, counts, _ = corpus_state
                summary["corpus"] = counts
                if problem:
                    raise CheckFailed("error", problem)
            got = KINDS[cfg["kind"]](r, cfg, work)
            summary.update(got)
        except CheckFailed as e:
            status, reason = e.status, e.reason
            summary.update(e.summary)
        except KeyboardInterrupt:
            status, reason, interrupted = "interrupted", "interrupted", True
        except Exception as e:  # noqa: BLE001 — recorded, never a pass
            status, reason = "error", "internal:" + type(e).__name__
            print(f"release-verify: {name}: {type(e).__name__}: {str(e)[:200]}", file=sys.stderr)
        if r.timed_out and status == "passed":
            status, reason = "timeout", "timeout"
        state = worktree_state(src, sha, tree)
        if state and status == "passed":
            status, reason = "failed", f"worktree:{state}"
        t1 = now_utc()
        r.kill_all()
        logs = sorted(n for n in r.logs if (out / "logs" / n).is_file())
        doc = {
            "schema_version": SCHEMA, "run_id": run_id, "commit": sha, "tree": tree, "check": name,
            "kind": cfg["kind"], "config": cfg, "status": status, "reason": reason,
            "started_at": iso(t0), "finished_at": iso(t1), "duration_seconds": round((t1 - t0).total_seconds(), 1),
            "max_rss_mb": round(r.max_rss_mb, 1), "commands": r.commands, "summary": summary,
            "logs": {n: sha256_file(out / "logs" / n) for n in logs},
        }
        write_atomic(out / "checks" / f"{name}.json", dump(doc))
        results[name] = {"status": status, "file": f"{name}.json", "sha256": sha256_file(out / "checks" / f"{name}.json")}
        print(f"release-verify: {name:22} {status:12} {reason or ''} ({doc['duration_seconds']} s, {doc['max_rss_mb']} MB)",
              flush=True)
        if interrupted:
            break
    env1 = environment(toolchain)
    finished = now_utc()
    overall = "passed" if names and all(v["status"] == "passed" for v in results.values()) and len(results) == len(names) else "failed"
    reason = None
    if interrupted:
        overall, reason = "interrupted", "interrupted"
    if env0 != env1:
        overall, reason = "failed", "environment_changed"
    corpus_digest = corpus_state[2] if corpus_state else None
    result = {
        "schema_version": SCHEMA, "run_id": run_id, "commit": sha, "tree": tree,
        "profile": a.profile if not a.only else "partial", "requested": names, "status": overall, "reason": reason,
        "started_at": iso(started), "finished_at": iso(finished),
        "duration_seconds": round((finished - started).total_seconds(), 1),
        "environment": env0,
        "verifier": {"path": "scripts/release-verify.py", "blob": hash_object(me)},
        "config": {"path": "scripts/release-verify.json", "blob": hash_object(config_path)},
        "corpus": {"manifest_blob": blob(src, sha, "compat/manifest.yaml"), "digest": corpus_digest},
        "checks": results,
    }
    write_atomic(out / "result.json", dump(result))
    os.unlink(out / "RUNNING")
    ledger_append({"run_id": run_id, "commit": sha, "profile": result["profile"], "started_at": iso(started),
                   "event": "finished", "status": overall})
    print(f"release-verify: {overall} ({result['duration_seconds']} s)")
    return 0 if overall == "passed" else 1


# ---- reading results safely ------------------------------------------------------


class Bad(Exception):
    pass


def open_dir(path):
    p = Path(path)
    if p.is_symlink():
        raise Bad("the results directory is a symbolic link")
    try:
        return os.open(p, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except OSError as e:
        raise Bad(f"cannot open the results directory ({e.strerror})")


def read_rel(dirfd, parts, limit):
    """Read DIR/parts... — each part a plain name, no symlink anywhere, a
    regular file of at most `limit` bytes (a FIFO or a device is refused
    without blocking)."""
    import stat as _stat
    fd = dirfd
    opened = []
    try:
        for i, part in enumerate(parts):
            if not isinstance(part, str) or not FILE.match(part) or part in (".", ".."):
                raise Bad(f"an invalid file name in the results: {part!r}")
            last = i == len(parts) - 1
            flags = os.O_RDONLY | os.O_NOFOLLOW | (os.O_NONBLOCK if last else os.O_DIRECTORY)
            try:
                fd = os.open(part, flags, dir_fd=fd)
            except FileNotFoundError:
                raise Bad(f"missing: {'/'.join(parts)}")
            except OSError as e:
                raise Bad(f"cannot read {'/'.join(parts)} (a symbolic link or not a file: {e.strerror})")
            opened.append(fd)
        st = os.fstat(fd)
        if not _stat.S_ISREG(st.st_mode):
            raise Bad(f"not a regular file: {'/'.join(parts)}")
        if st.st_size > limit:
            raise Bad(f"too large: {'/'.join(parts)}")
        chunks, size = [], 0
        while True:
            c = os.read(fd, 1 << 20)
            if not c:
                break
            size += len(c)
            if size > limit:
                raise Bad(f"too large: {'/'.join(parts)}")
            chunks.append(c)
        return b"".join(chunks)
    finally:
        for f in opened:
            os.close(f)


def exists_rel(dirfd, name):
    try:
        os.stat(name, dir_fd=dirfd, follow_symlinks=False)
        return True
    except FileNotFoundError:
        return False


def strict_json(data, what):
    def pairs(items):
        keys = [k for k, _ in items]
        if len(keys) != len(set(keys)):
            raise Bad(f"{what}: duplicate keys")
        return dict(items)
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=pairs)
    except (ValueError, UnicodeDecodeError):
        raise Bad(f"{what}: not complete JSON (truncated or edited)")


RESULT_KEYS = {"schema_version", "run_id", "commit", "tree", "profile", "requested", "status", "reason", "started_at",
               "finished_at", "duration_seconds", "environment", "verifier", "config", "corpus", "checks"}
CHECK_KEYS = {"schema_version", "run_id", "commit", "tree", "check", "kind", "config", "status", "reason", "started_at",
              "finished_at", "duration_seconds", "max_rss_mb", "commands", "summary", "logs"}


def load_results(path):
    """(result, {name: check doc}) after structural validation; raises
    Bad. Nothing is compared with a commit here."""
    dirfd = open_dir(path)
    try:
        return _load(dirfd)
    except (TypeError, KeyError, AttributeError, ValueError) as e:
        raise Bad(f"malformed results ({type(e).__name__})")
    finally:
        os.close(dirfd)


def _load(dirfd):
    if exists_rel(dirfd, "RUNNING"):
        raise Bad("the run did not finish (RUNNING is present): interrupted or still going")
    result = strict_json(read_rel(dirfd, ["result.json"], MAX_JSON), "result.json")
    if not isinstance(result, dict) or set(result) != RESULT_KEYS:
        raise Bad("result.json: fields missing or unknown")
    if result["schema_version"] != SCHEMA:
        raise Bad(f"result.json: schema {result['schema_version']} is not {SCHEMA}")
    if result["status"] not in STATUSES:
        raise Bad(f"result.json: unknown status {result['status']!r}")
    if not isinstance(result["checks"], dict) or not isinstance(result["run_id"], str):
        raise Bad("result.json: malformed")
    checks = {}
    for name, entry in result["checks"].items():
        if not NAME.match(name) or not isinstance(entry, dict) or set(entry) != {"status", "file", "sha256"}:
            raise Bad(f"result.json: malformed entry {name!r}")
        if entry["file"] != f"{name}.json":
            raise Bad(f"result.json: {name}'s file is not checks/{name}.json")
        data = read_rel(dirfd, ["checks", entry["file"]], MAX_JSON)
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            raise Bad(f"checks/{entry['file']}: its digest is not the one result.json records")
        doc = strict_json(data, f"checks/{entry['file']}")
        if not isinstance(doc, dict) or set(doc) != CHECK_KEYS:
            raise Bad(f"checks/{entry['file']}: fields missing or unknown")
        if doc["status"] not in STATUSES or doc["status"] != entry["status"]:
            raise Bad(f"checks/{entry['file']}: status {doc['status']!r} (result.json: {entry['status']!r})")
        if doc["check"] != name or doc["run_id"] != result["run_id"]:
            raise Bad(f"checks/{entry['file']}: from another check or run")
        for log, digest in (doc["logs"] or {}).items():
            if not (log == f"{name}.log" or log.startswith(f"{name}-")) or not log.endswith(".log"):
                raise Bad(f"checks/{entry['file']}: a log outside the check: {log!r}")
            if hashlib.sha256(read_rel(dirfd, ["logs", log], MAX_LOG)).hexdigest() != digest:
                raise Bad(f"logs/{log}: its digest is not the one recorded")
        if f"{name}.log" not in (doc["logs"] or {}):
            raise Bad(f"checks/{entry['file']}: no log")
        for c in doc["commands"]:
            if set(c) != {"argv", "exit", "log"} or c["log"] not in doc["logs"]:
                raise Bad(f"checks/{entry['file']}: a command whose log is not recorded")
        checks[name] = doc
    return result, checks


def verify(repo, ref, results, profile, allow_superseded=False, now=None):
    """Problems keeping `results` from proving that `ref` passed `profile`
    ([] when it does)."""
    sha, tree = resolve(repo, ref)
    try:
        result, checks = load_results(results)
    except Bad as e:
        return [str(e)]
    p = []
    if result["commit"] != sha:
        p.append(f"results of commit {str(result['commit'])[:12]}, not {sha[:12]}")
    if result["tree"] != tree:
        p.append("results of another tree")
    ver = blob(repo, sha, "scripts/release-verify.py")
    cfg_blob = blob(repo, sha, "scripts/release-verify.json")
    if not ver or not cfg_blob:
        return p + [f"{sha[:12]} has no scripts/release-verify.py / .json (verify it as its own release did)"]
    if result["verifier"] != {"path": "scripts/release-verify.py", "blob": ver}:
        p.append("made by another version of scripts/release-verify.py than the commit's")
    if result["config"] != {"path": "scripts/release-verify.json", "blob": cfg_blob}:
        p.append("made with another scripts/release-verify.json than the commit's")
    config = json.loads(git(repo, "show", f"{sha}:scripts/release-verify.json"))
    expected = config["profiles"].get(profile)
    if expected is None:
        return p + [f"the commit's config has no profile {profile}"]
    if result["profile"] != profile:
        p.append(f"profile {result['profile']!r}, not {profile!r}")
    for prof, required in (("release", RELEASE_CHECKS), ("security-history", HISTORY_CHECKS)):
        if profile == prof:
            for name, kind in required.items():
                if name not in expected or config["checks"].get(name, {}).get("kind") != kind:
                    p.append(f"the commit's {prof} profile lacks {name} ({kind})")
    if list(result["requested"]) != list(expected):
        p.append("not every check of the profile was requested")
    for name in expected:
        doc = checks.get(name)
        if doc is None:
            p.append(f"{name}: no result")
            continue
        if doc["status"] != "passed":
            p.append(f"{name}: {doc['status']} ({doc['reason']})")
        if doc["commit"] != sha or doc["tree"] != tree:
            p.append(f"{name}: of another commit")
        if doc["config"] != config["checks"].get(name):
            p.append(f"{name}: run with another configuration than the commit's")
    for name in set(checks) - set(expected):
        p.append(f"{name}: not a check of the profile")
    if result["status"] != "passed":
        p.append(f"the run is {result['status']}" + (f" ({result['reason']})" if result["reason"] else ""))
    try:
        t0, t1 = parse_iso(result["started_at"]), parse_iso(result["finished_at"])
        now = now or now_utc()
        if t1 < t0 or t1 > now + datetime.timedelta(minutes=5):
            p.append("the run's times are inconsistent")
        if now - t1 > datetime.timedelta(days=config.get("max_age_days", 30)):
            p.append(f"older than {config.get('max_age_days', 30)} days: run it again")
    except (TypeError, ValueError):
        p.append("the run's times are malformed")
    if not allow_superseded:
        # Runs of the same commit and profile, in the order they started
        # (the ledger's line order); a `partial` run does not supersede.
        mine = [e for e in ledger_entries()
                if e.get("commit") == sha and e.get("event") == "started" and e.get("profile") == result["profile"]]
        if not any(e.get("run_id") == result["run_id"] for e in mine):
            p.append("this run is not in the local ledger of runs (made elsewhere, or the ledger was lost)")
        elif mine[-1].get("run_id") != result["run_id"]:
            p.append("a newer run of this commit exists (failed, interrupted or still going): its result decides")
    return p


def cmd_check(a):
    repo = Path(git(os.getcwd(), "rev-parse", "--show-toplevel"))
    problems = verify(repo, a.ref, a.results, a.profile, a.allow_superseded)
    for x in problems:
        print("PROBLEM:", x)
    if problems:
        return 1
    print(f"release-verify: {a.results} proves {resolve(repo, a.ref)[0]} passed profile {a.profile}")
    return 0


# ---- report and the public projection ------------------------------------------

# The public projection's schema: per check kind, the summary's keys and
# their types — integers, statuses, or maps whose keys are a fixed set.
# Anything else (a key, a string, a nested number) is dropped.
INT, STATUS = "int", "status"
LOST_KEYS = ("outside_words_lost", "outside_chars_lost", "outside_chars_mupdf_lost", "outside_chars_mupdf_moved")
CORPUS_SCHEMA = {"files": INT, "missing": INT, "mismatched": INT}
SCHEMAS = {
    "security_regressions": {"verify_redact_ok": INT, "verify_redact_cases": INT, "advisory_safe": INT,
                             "advisory_cases": INT},
    "noninterference": {"seeds": INT, "ok": INT, "refused": INT},
    "redact_corpus": {"corpus": CORPUS_SCHEMA, "configs": (
        ("default", "optin", "optin-javascript", "optin-strict"),
        {"status": STATUS, "runs": INT, "run_ok": INT, "run_refused": INT, "run_unexpected": INT, "regressions": INT,
         "improvements": INT, "reason_changes": INT, "lost": {k: INT for k in LOST_KEYS}})},
    "compat_corpus": {"corpus": CORPUS_SCHEMA, **{k: INT for k in (
        "files", "crash", "timeout", "quality_flag", "quality_report", "quality_exempt", "roundtrip_full",
        "incremental_pass")}},
    "external_renderers": {"cases": INT, "renderer_runs": INT, "leaks": INT, "losses": INT, "problems": INT,
                           "outcomes": {k: INT for k in ("ok", "refused", "crash", "timeout")},
                           "pixels": {k: INT for k in ("antialias", "snap", "font_substitution", "changed")}},
    "advisory_history": {"inputs": INT, "versions": INT, "built": INT, "advisories": INT, "untestable": INT,
                         "problems": INT, "private_inputs": INT, "states": (
                             ("0.5.0", "0.5.1", "0.5.2", "0.6.0", "0.6.1", "0.6.2", "0.7.0", "0.8.0", "candidate"),
                             {k: INT for k in ("LEAK", "REMOVED", "REFUSED", "KEPT", "LOST", "FAST", "SLOW",
                                               "TIMEOUT", "CRASH", "ERROR", "NA")})},
    "fuzz": {"seconds": INT, "targets": (
        ("render_content", "render_graph", "redact_model"),
        {k: INT for k in ("seeds_replayed", "runs", "elapsed_seconds", "executed_beyond_seeds", "new_inputs",
                          "crashes", "timeouts")})},
}
REASONS = {"timeout", "tool_missing", "corpus_incomplete", "corpus_manifest_empty", "regression", "leak",
           "renderer_version", "file_count", "crash_or_timeout", "no_baseline", "no_report", "verify_redact_cases",
           "advisory_cases", "worktree", "crashed", "exit", "no_summary", "too_short", "no_new_inputs",
           "fuzz_failed", "interrupted", "internal", "environment_changed", "seeds_mismatch", "history",
           "expectations"}


def project(schema, v):
    if schema == INT:
        return v if isinstance(v, int) and not isinstance(v, bool) and 0 <= v < 10 ** 12 else None
    if schema == STATUS:
        return v if v in STATUSES else None
    if not isinstance(v, dict):
        return None
    if isinstance(schema, tuple):
        keys, sub = schema
        pairs = ((k, sub) for k in keys)
    else:
        pairs = schema.items()
    out = {}
    for k, sub in pairs:
        if k in v:
            x = project(sub, v[k])
            if x is not None:
                out[k] = x
    return out


def number(v):
    return round(v, 1) if isinstance(v, (int, float)) and not isinstance(v, bool) and 0 <= v < 10 ** 9 else None


def public(result, checks):
    # Only the version number, from a line in the tool's own format: the
    # recorded line is free text.
    tools = {}
    for k, v in (result["environment"].get("tools") or {}).items():
        m = TOOL_LINE[k].fullmatch(v) if k in TOOL_LINE and isinstance(v, str) else None
        if m:
            tools[k] = m.group(1)
    env = result["environment"]
    out = {
        "schema_version": SCHEMA,
        "commit": result["commit"] if SHA.match(str(result["commit"])) else None,
        "tree": result["tree"] if SHA.match(str(result["tree"])) else None,
        "profile": result["profile"] if result["profile"] in ("release", "disclosure", "security-history",
                                                              "partial") else None,
        "status": result["status"] if result["status"] in STATUSES else None,
        "duration_seconds": number(result["duration_seconds"]),
        "environment": {"os": env.get("os") if env.get("os") in ("Darwin", "Linux") else None,
                        "arch": env.get("arch") if env.get("arch") in ("arm64", "aarch64", "x86_64") else None,
                        "tools": tools},
        "checks": {},
    }
    for name, doc in sorted(checks.items()):
        if name not in PUBLIC_CHECKS or doc.get("kind") not in SCHEMAS:
            continue
        reason = str(doc.get("reason")).split(":", 1)[0] if doc.get("reason") is not None else None
        out["checks"][name] = {
            "status": doc["status"] if doc["status"] in STATUSES else None,
            "reason": reason if reason is None or reason in REASONS else "other",
            "duration_seconds": number(doc["duration_seconds"]),
            "max_rss_mb": number(doc["max_rss_mb"]),
            "summary": project(SCHEMAS[doc["kind"]], doc["summary"]),
        }
    return out


def cmd_report(a):
    try:
        result, checks = load_results(a.results)
    except Bad as e:
        print("PROBLEM:", e)
        return 1
    print(f"# release-verify — {result['commit']} ({result['profile']}): {result['status']}")
    print(f"\nrun {result['run_id']}, {result['started_at']} – {result['finished_at']} ({result['duration_seconds']} s)")
    print(f"environment: {result['environment']['os']} {result['environment']['os_version']} {result['environment']['arch']}")
    for k, v in sorted((result["environment"].get("tools") or {}).items()):
        print(f"  {k}: {v}")
    print("\n| check | status | reason | seconds | peak MB |\n| --- | --- | --- | ---: | ---: |")
    for name, doc in checks.items():
        print(f"| {name} | {doc['status']} | {doc['reason'] or ''} | {doc['duration_seconds']} | {doc['max_rss_mb']} |")
    for name, doc in checks.items():
        shown = project(SCHEMAS[doc["kind"]], doc["summary"]) if doc.get("kind") in SCHEMAS else {}
        print(f"\n## {name}\n\n```json\n{json.dumps(shown, indent=1, sort_keys=True)}\n```")
    if a.public:
        write_atomic(Path(a.public), dump(public(result, checks)))
        print(f"\npublic projection: {a.public}")
    return 0 if result["status"] == "passed" else 1


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--ref", required=True)
    r.add_argument("--out")
    r.add_argument("--profile", default="release")
    r.add_argument("--corpus")
    r.add_argument("--only", action="append", help="run only this check (the results are then `partial`)")
    r.add_argument("--keep", action="store_true", help="keep the worktree and the build")
    r.add_argument("--private-inputs", help="security-history: a private directory of advisory inputs (local only)")
    i = sub.add_parser("_inner")
    for k in ("--out", "--commit", "--tree", "--profile", "--corpus"):
        i.add_argument(k, required=True)
    i.add_argument("--only", action="append")
    c = sub.add_parser("check")
    c.add_argument("--ref", required=True)
    c.add_argument("--results", required=True)
    c.add_argument("--profile", default="release")
    c.add_argument("--allow-superseded", action="store_true",
                   help="accept a run although a newer run of the commit exists or the ledger lacks it")
    p = sub.add_parser("report")
    p.add_argument("--results", required=True)
    p.add_argument("--public")
    a = ap.parse_args(argv)
    return {"run": cmd_run, "_inner": inner, "check": cmd_check, "report": cmd_report}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
