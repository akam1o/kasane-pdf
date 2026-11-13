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
    `gate` waits for every job, always runs scripts/ci-gate.py, whose
    full-gate set is exactly the jobs that skip pull requests; the
    workflow runs on pull requests (unfiltered), pushes to main and
    release/**, and dispatch; in every workflow, caches are written from
    main only;
  * the gate's job list (scripts/ci-gate.py EXPECTED, read as data) is
    exactly pr.yml's jobs; `gate` and the required checks are names no
    other job of any workflow has; the workflow guard (pr-guard.yml)
    runs on pull_request_target with read-only permissions and runs
    nothing of the pull request's.

    scripts/check-workflows.py                 # the checkout itself
    scripts/check-workflows.py --data-only DIR # another tree, read as data

--data-only reads DIR's workflows and scripts without running any of
them (pr-guard.yml checks a pull request's tree this way); it skips the
checks that run a tool.
"""
import ast
import re
import subprocess
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
# The tree whose workflows and scripts are checked (read, never run).
DATA = ROOT
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
    src = (DATA / "compat/fetch-corpus.py").read_text()
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
# Commands that must run on every pull request: job -> commands. The step
# running each must have no `if:` (a fuzz step may select its matrix
# target, and the matrix must then include that target).
PR_MUST_RUN = {
    "engine": ["cargo fmt --check", "cargo clippy --all-targets", "cargo nextest run --profile ci",
               "cargo test --doc"],
    "features-test": ["cargo nextest run --profile ci --features cjk-fallback", "cargo test --doc --features cjk-fallback",
                      "cargo test --no-default-features --lib", "cargo test --no-default-features --doc"],
    "compat-smoke": ["scripts/check-workflows.py", "scripts/verify-redact.py",
                     "KASANE_ADVISORY_DUMP=/tmp/advisories cargo test --release --test redact_advisories",
                     "check-advisory-regressions.py target/release/kasane-pdf /tmp/advisories --expect-safe",
                     "scripts/test_ci_scripts.py"],
    "render-fuzz-smoke": ["ci-run-sharded.sh redact_model", "fuzz/ci-run.sh \"$T\" \"fuzz/seeds/$T\"",
                          "-seed=1 -runs=3000"],
}
FULL_ONLY_IF = "github.event_name != 'pull_request'"
MAIN_ONLY = "${{ github.ref == 'refs/heads/main' }}"
MAIN_ONLY_SAVE = "success() && github.ref == 'refs/heads/main'"
FUZZ_TARGETS = {"render_content", "render_graph", "redact_model"}


GATE = "gate"
GUARD = "workflow guard"


def ci_gate_sets():
    """EXPECTED and FULL of scripts/ci-gate.py, read as data (the file is
    parsed, never run: it may be a pull request's)."""
    out = {}
    tree = ast.parse((DATA / "scripts/ci-gate.py").read_text())
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
            if name in ("EXPECTED", "FULL") and isinstance(node.value, ast.Call) and node.value.args:
                try:
                    out[name] = set(ast.literal_eval(node.value.args[0]))
                except ValueError:
                    pass
    return out.get("EXPECTED"), out.get("FULL")


def ci_gate_full():
    return ci_gate_sets()[1]


def check_caches(path, wf):
    """Rust caches and actions/cache are written from main only."""
    for jid, st in steps_of(wf):
        uses = st.get("uses") or ""
        if "Swatinem/rust-cache" in uses:
            if str((st.get("with") or {}).get("save-if", "")).strip() != MAIN_ONLY:
                problem(f"{path.name}:{jid}: rust-cache save-if must be `{MAIN_ONLY}`")
        elif uses.startswith("actions/cache@"):
            problem(f"{path.name}:{jid}: actions/cache saves from any ref; use actions/cache/restore + a main-only save")
        elif uses.startswith("actions/cache/save@"):
            if str(st.get("if", "")).strip() != MAIN_ONLY_SAVE:
                problem(f"{path.name}:{jid}: actions/cache/save must be guarded by `{MAIN_ONLY_SAVE}`")


def check_fuzz_matrix(path, job):
    """Every fuzz target is in the matrix, and redact_model's jobs cover
    every shard its command deals the seeds into."""
    include = ((job.get("strategy") or {}).get("matrix") or {}).get("include") or []
    targets = {e.get("target") for e in include}
    if targets != FUZZ_TARGETS:
        problem(f"{path.name}:render-fuzz-smoke: matrix targets {sorted(t for t in targets if t)} instead of {sorted(FUZZ_TARGETS)}")
    cmd = " ".join(st.get("run") or "" for st in job.get("steps") or [])
    m = re.search(r"ci-run-sharded\.sh redact_model \S+ (\d+) \d+ \d+", cmd)
    covered = []
    for e in include:
        if e.get("target") == "redact_model":
            a, b = map(int, str(e.get("shards", "0 0")).split())
            covered += list(range(a, b + 1))
    if not m or sorted(covered) != list(range(1, int(m.group(1)) + 1)):
        problem(f"{path.name}:render-fuzz-smoke: redact_model's jobs cover shards {sorted(covered)}, not each of 1..N once")


def check_gate(path, wf):
    jobs = wf.get("jobs") or {}
    names = {j.get("name") for j in jobs.values()}
    for n in REQUIRED_CHECKS:
        if n not in names:
            problem(f"{path.name}: required check `{n}` is not a job name")
    on = wf.get("on") or wf.get(True) or {}
    if "pull_request" not in on:
        problem(f"{path.name}: does not run on pull_request")
    elif on.get("pull_request"):
        # A filter (paths, branches, types) leaves some pull requests
        # without the gate.
        problem(f"{path.name}: pull_request is filtered ({sorted(on['pull_request'])}): some pull requests get no gate")
    if "workflow_dispatch" not in on:
        problem(f"{path.name}: cannot be dispatched (the release gate)")
    branches = set(((on.get("push") or {}) if isinstance(on, dict) else {}).get("branches") or [])
    if not {"main", "release/**"} <= branches:
        problem(f"{path.name}: pushes to main and release/** must run it (has {sorted(branches)})")
    for jid, cmds in PR_MUST_RUN.items():
        job = jobs.get(jid)
        if not job:
            problem(f"{path.name}: job `{jid}` is missing")
            continue
        if "if" in job:
            problem(f"{path.name}:{jid}: must run on every pull request (has `if: {job['if']}`)")
        targets = {e.get("target") for e in ((job.get("strategy") or {}).get("matrix") or {}).get("include") or []}
        for cmd in cmds:
            steps = [st for st in job.get("steps") or [] if cmd in (st.get("run") or "")]
            if not steps:
                problem(f"{path.name}:{jid}: does not run `{cmd}`")
            for st in steps:
                cond = str(st.get("if", "")).strip()
                m = re.fullmatch(r"matrix\.target (==|!=) '([a-z_]+)'", cond)
                if cond and not (m and (m.group(1) == "!=" or m.group(2) in targets)):
                    problem(f"{path.name}:{jid}: the step running `{cmd}` has `if: {cond}`")
    check_fuzz_matrix(path, jobs.get("render-fuzz-smoke") or {})
    full = set()
    for jid, job in jobs.items():
        if jid != "gate" and "timeout-minutes" not in job:
            problem(f"{path.name}:{jid}: no timeout-minutes")
        if job.get("continue-on-error") or any(st.get("continue-on-error") for st in job.get("steps") or []):
            problem(f"{path.name}:{jid}: continue-on-error")
        cond = str(job.get("if", "")).strip()
        if cond and jid != "gate":
            if cond != FULL_ONLY_IF:
                problem(f"{path.name}:{jid}: unexpected condition `{cond}`")
            else:
                full.add(jid)
    gate = jobs.get("gate")
    if not gate:
        problem(f"{path.name}: no `gate` job")
        return
    if str(gate.get("if", "")).strip() != "always()":
        problem(f"{path.name}:gate: must run always()")
    if set(gate.get("needs") or []) != set(jobs) - {"gate"}:
        problem(f"{path.name}:gate: needs {sorted(gate.get('needs') or [])} instead of every job")
    runs = [st for st in gate.get("steps") or [] if "scripts/ci-gate.py" in (st.get("run") or "")]
    if not runs or any("if" in st for st in runs):
        problem(f"{path.name}:gate: does not run scripts/ci-gate.py unconditionally")
    expected, _ = ci_gate_sets()
    if expected != set(jobs) - {"gate"}:
        problem(f"{path.name}:gate: scripts/ci-gate.py EXPECTED {sorted(expected or [])} differs from pr.yml's jobs {sorted(set(jobs) - {'gate'})}")
    if gate.get("name") != GATE:
        problem(f"{path.name}:gate: its name must be `{GATE}` (the required check)")
    if ci_gate_full() != full:
        problem(f"{path.name}:gate: scripts/ci-gate.py FULL {sorted(ci_gate_full())} differs from the jobs skipped on pull requests {sorted(full)}")


def check_names(wfs):
    """`gate`, the guard and the required checks are each the name of one
    job of one workflow: a skipped job elsewhere with the same name would
    satisfy the required check."""
    owners = {}
    for path, wf in wfs:
        for jid, job in (wf.get("jobs") or {}).items():
            name = job.get("name") or jid
            owners.setdefault(name, []).append(f"{path.name}:{jid}")
    for name in REQUIRED_CHECKS | {GATE, GUARD}:
        found = owners.get(name, [])
        if len(found) != 1:
            problem(f"the required check `{name}` is the name of {len(found)} job(s): {found}")


def check_guard(path, wf):
    """pr-guard.yml: the base branch's definition checks a pull request's
    workflows; it checks out and runs nothing of the pull request's."""
    on = wf.get("on") or wf.get(True) or {}
    if set(on) != {"pull_request_target"} or on.get("pull_request_target"):
        problem(f"{path.name}: must run on pull_request_target only, unfiltered")
    if wf.get("permissions") != {"contents": "read"}:
        problem(f"{path.name}: permissions must be exactly contents: read")
    jobs = wf.get("jobs") or {}
    if [j.get("name") for j in jobs.values()] != [GUARD]:
        problem(f"{path.name}: must have exactly one job, `{GUARD}`")
    for jid, job in jobs.items():
        if "permissions" in job or "secrets" in str(job):
            problem(f"{path.name}:{jid}: no job-level permissions or secrets")
    for jid, st in steps_of(wf):
        w = st.get("with") or {}
        if "actions/checkout" in (st.get("uses") or ""):
            if "ref" in w or "repository" in w or w.get("persist-credentials") is not False:
                problem(f"{path.name}:{jid}: only the base branch is checked out, without credentials")
        elif st.get("uses"):
            problem(f"{path.name}:{jid}: no action other than checkout ({st['uses']})")
    runs = " ".join(st.get("run") or "" for _, st in steps_of(wf))
    if "base/scripts/check-workflows.py --data-only data" not in runs:
        problem(f"{path.name}: does not check the pull request's workflows with the base branch's script")


def check_cjk_lock():
    lock = DATA / "scripts/cjk-assets.sha256"
    script = (DATA / "scripts/fetch-cjk-assets.sh").read_text()
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
    global DATA
    data_only = False
    if sys.argv[1:2] == ["--data-only"]:
        DATA = Path(sys.argv[2]).resolve()
        data_only = True
    wfs = []
    for path in sorted((DATA / ".github/workflows").glob("*.yml")):
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
        check_caches(path, wf)
        wfs.append((path, wf))
        if path.name == "pr.yml":
            check_gate(path, wf)
        if path.name == "pr-guard.yml":
            check_guard(path, wf)
    check_names(wfs)
    if not any(p.name == "pr-guard.yml" for p, _ in wfs):
        problem("pr-guard.yml (the workflow guard) is missing")
    check_cjk_lock()
    if not data_only:
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
