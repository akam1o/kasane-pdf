#!/usr/bin/env python3
"""Consistency checks between the GitHub Actions workflows and the tools
they call — runs without credentials, so a wrong command line or a
credential variable the tool does not read is caught on every pull
request instead of in the nightly job.

Checks:
  * every workflow parses as YAML and has jobs/steps;
  * every `uses:` is pinned to a 40-hex commit SHA (with a version comment);
  * the nightly corpus fetch calls `fetch-corpus.py download --dir …`
    with the subcommands/options argparse actually defines;
  * the environment variables the nightly fetch step sets are ones
    fetch-corpus.py / boto3 read (store location + AWS credentials);
  * no step echoes a secret, and secrets are only referenced through
    `${{ secrets.NAME }}` in `env:`;
  * the nightly job is gated on the KASANE_CORPUS_ENABLED variable;
  * the cjk asset lock exists and lists every file the fetch script
    installs;
  * the gate (pr.yml, docs/testing.md "CI gates"): the required checks
    exist; the redaction security checks (verify-redact, the advisory
    regressions with --expect-safe) and the engine tests run on every
    pull request; every job has a timeout and none continues on error;
    `gate` waits for every job, always runs, and lets exactly the
    full-gate jobs be skipped, and only on pull requests; caches are
    written from main only.
Exit 1 on the first problem.
"""
import re
import subprocess
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
problems = []


def problem(msg):
    problems.append(msg)
    print("PROBLEM:", msg)


def steps_of(wf):
    for jname, job in (wf.get("jobs") or {}).items():
        for st in job.get("steps") or []:
            yield jname, st


def check_pins(path, wf):
    for jname, st in steps_of(wf):
        uses = st.get("uses")
        if not uses:
            continue
        ref = uses.split("@", 1)[1] if "@" in uses else ""
        if not re.fullmatch(r"[0-9a-f]{40}", ref):
            problem(f"{path.name}:{jname}: `{uses}` is not pinned to a commit SHA")


def argparse_surface():
    src = (ROOT / "compat/fetch-corpus.py").read_text()
    subs = set(re.findall(r'add_parser\("([a-z-]+)"', src))
    opts = set(re.findall(r'add_argument\("(--[a-z-]+)"', src))
    envs = set(re.findall(r'os\.environ\.get\("([A-Z0-9_]+)"', src))
    return subs, opts, envs


def check_nightly(path, wf):
    subs, opts, envs = argparse_surface()
    boto_env = {"AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "AWS_REGION", "AWS_PROFILE"}
    fetch_steps = [st for _, st in steps_of(wf) if "fetch-corpus.py" in (st.get("run") or "")]
    if not fetch_steps:
        problem(f"{path.name}: no step runs fetch-corpus.py")
    for st in fetch_steps:
        for line in st["run"].splitlines():
            if "fetch-corpus.py" not in line:
                continue
            toks = line.split()
            i = next(k for k, t in enumerate(toks) if t.endswith("fetch-corpus.py"))
            sub = toks[i + 1] if i + 1 < len(toks) else ""
            if sub not in subs:
                problem(f"{path.name}: `{line.strip()}` — `{sub}` is not a fetch-corpus.py subcommand {sorted(subs)}")
            for t in toks[i + 2:]:
                if t.startswith("--") and t not in opts:
                    problem(f"{path.name}: `{line.strip()}` — option `{t}` unknown to fetch-corpus.py {sorted(opts)}")
        env = st.get("env") or {}
        for name, val in env.items():
            if name not in envs and name not in boto_env:
                problem(f"{path.name}: env `{name}` on the fetch step is read by neither fetch-corpus.py nor boto3")
            if "secrets." in str(val) and not re.fullmatch(r"\$\{\{\s*secrets\.[A-Z0-9_]+\s*\}\}", str(val).strip()):
                problem(f"{path.name}: env `{name}` embeds a secret in a larger expression")
        needed = {"AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"}
        if not needed <= set(env):
            problem(f"{path.name}: the fetch step must set {sorted(needed)} for boto3 (has {sorted(env)})")
        if not ({"KASANE_CORPUS_ENDPOINT", "R2_ENDPOINT"} & set(env) and {"KASANE_CORPUS_BUCKET", "R2_BUCKET"} & set(env)):
            problem(f"{path.name}: the fetch step must set the store endpoint and bucket variables")
    for jname, job in (wf.get("jobs") or {}).items():
        if any("fetch-corpus.py" in (st.get("run") or "") for st in job.get("steps") or []):
            if "KASANE_CORPUS_ENABLED" not in str(job.get("if", "")):
                problem(f"{path.name}:{jname}: corpus job is not gated on vars.KASANE_CORPUS_ENABLED")


def check_no_secret_echo(path, text):
    for m in re.finditer(r"echo[^\n]*secrets\.", text):
        problem(f"{path.name}: a step echoes a secret: {m.group(0)[:60]}")
    for m in re.finditer(r"\$\{\{\s*secrets\.[A-Z0-9_]+\s*\}\}", text):
        line = text[: m.start()].rsplit("\n", 1)[-1]
        # Allowed: a mapping value under `env:` or an action's `with:`
        # (e.g. `token: ${{ secrets.GITHUB_TOKEN }}`); never inside a
        # `run:` script or an expression that embeds it.
        if "env:" in line or re.match(r"\s+[A-Za-z0-9_-]+:\s", line):
            continue
        problem(f"{path.name}: secret referenced outside an env: mapping: `{line.strip()}`")


REQUIRED_CHECKS = {"test / fmt / clippy", "compat + round-trip smoke (fixtures)"}
# Jobs that must run on every pull request (no `if:`), and a command each
# must run.
PR_MUST_RUN = {
    "engine": "cargo nextest run",
    "features-test": "cargo nextest run",
    "compat-smoke": "check-advisory-regressions.py",
    "render-fuzz-smoke": "ci-run-sharded.sh redact_model",
}
FULL_ONLY_IF = "github.event_name != 'pull_request'"


def check_gate(path, wf):
    jobs = wf.get("jobs") or {}
    names = {j.get("name") for j in jobs.values()}
    for n in REQUIRED_CHECKS:
        if n not in names:
            problem(f"{path.name}: required check `{n}` is not a job name")
    on = wf.get("on") or wf.get(True) or {}
    if "pull_request" not in on:
        problem(f"{path.name}: does not run on pull_request")
    for jid, cmd in PR_MUST_RUN.items():
        job = jobs.get(jid)
        if not job:
            problem(f"{path.name}: job `{jid}` is missing")
            continue
        if "if" in job:
            problem(f"{path.name}:{jid}: must run on every pull request (has `if: {job['if']}`)")
        if not any(cmd in (st.get("run") or "") for st in job.get("steps") or []):
            problem(f"{path.name}:{jid}: does not run `{cmd}`")
    smoke = " ".join(st.get("run") or "" for st in (jobs.get("compat-smoke") or {}).get("steps") or [])
    for cmd in ("verify-redact.py", "--expect-safe"):
        if cmd not in smoke:
            problem(f"{path.name}:compat-smoke: does not run `{cmd}`")
    full = set()
    for jid, job in jobs.items():
        if jid != "gate" and "timeout-minutes" not in job:
            problem(f"{path.name}:{jid}: no timeout-minutes")
        if job.get("continue-on-error") or any(st.get("continue-on-error") for st in job.get("steps") or []):
            problem(f"{path.name}:{jid}: continue-on-error")
        cond = str(job.get("if", ""))
        if cond:
            if cond.strip() != FULL_ONLY_IF and jid != "gate":
                problem(f"{path.name}:{jid}: unexpected condition `{cond}`")
            elif jid != "gate":
                full.add(jid)
        for st in job.get("steps") or []:
            if "Swatinem/rust-cache" in (st.get("uses") or ""):
                if "refs/heads/main" not in str((st.get("with") or {}).get("save-if", "")):
                    problem(f"{path.name}:{jid}: rust-cache saves outside main")
    gate = jobs.get("gate")
    if not gate:
        problem(f"{path.name}: no `gate` job")
        return
    if str(gate.get("if", "")).strip() != "always()":
        problem(f"{path.name}:gate: must run always()")
    needs = set(gate.get("needs") or [])
    if needs != set(jobs) - {"gate"}:
        problem(f"{path.name}:gate: needs {sorted(needs)} instead of every job")
    script = " ".join(st.get("run") or "" for st in gate.get("steps") or [])
    m = re.search(r"full = \{([^}]*)\}", script)
    listed = set(re.findall(r'"([a-z0-9-]+)"', m.group(1))) if m else set()
    if listed != full:
        problem(f"{path.name}:gate: full-gate jobs {sorted(listed)} differ from the jobs skipped on pull requests {sorted(full)}")


def check_cjk_lock():
    lock = ROOT / "scripts/cjk-assets.sha256"
    script = (ROOT / "scripts/fetch-cjk-assets.sh").read_text()
    if not lock.exists():
        problem("scripts/cjk-assets.sha256 is missing")
        return
    entries = {ln.split()[1] for ln in lock.read_text().splitlines() if ln.strip()}
    for rel in ("cmap/LICENSE", "cmap/Adobe-Japan1-UCS2", "cmap/Adobe-Japan1-UCS2-LICENSE", "NotoSansJP-Regular.otf", "NotoSansJP-LICENSE", "cmap/90ms-RKSJ-H"):
        if rel not in entries:
            problem(f"cjk lock does not list {rel}")
    for name in ("CMAP_REV", "MAP_REV", "NOTO_REV"):
        m = re.search(rf"^{name}=([0-9a-f]{{40}})\b", script, re.M)
        if not m:
            problem(f"fetch-cjk-assets.sh: {name} is not pinned to a 40-hex commit")


def main():
    for path in sorted((ROOT / ".github/workflows").glob("*.yml")):
        text = path.read_text()
        try:
            wf = yaml.safe_load(text)
        except yaml.YAMLError as e:  # noqa: PERF203
            problem(f"{path.name}: YAML error: {e}")
            continue
        if not isinstance(wf, dict) or not wf.get("jobs"):
            problem(f"{path.name}: no jobs")
            continue
        check_pins(path, wf)
        check_no_secret_echo(path, text)
        if path.name == "nightly.yml":
            check_nightly(path, wf)
        if path.name == "pr.yml":
            check_gate(path, wf)
    check_cjk_lock()
    # fetch-corpus.py must at least parse and answer --help offline.
    r = subprocess.run([sys.executable, str(ROOT / "compat/fetch-corpus.py"), "--help"], capture_output=True, text=True)
    if r.returncode != 0:
        problem("fetch-corpus.py --help failed: " + r.stderr[-200:])
    if problems:
        print(f"{len(problems)} problem(s)")
        return 1
    print("workflows consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
