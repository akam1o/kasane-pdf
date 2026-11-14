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
    scripts/check-workflows.py --fork-changes FILE true|false

--data-only reads DIR's workflows and scripts without running any of
them (pr-guard.yml checks a pull request's tree this way); it skips the
checks that run a tool. --fork-changes lists the CI paths (CI_PATHS)
among the NUL-separated changed paths in FILE, and fails when there is
one and the pull request is from a fork.
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
                     "scripts/test_ci_scripts.py", "scripts/test_release_verify.py"],
    "render-fuzz-smoke": ["ci-run-sharded.sh redact_model", "fuzz/ci-run.sh \"$T\" \"fuzz/seeds/$T\"",
                          "-seed=1 -runs=3000"],
}
FULL_ONLY_IF = "github.event_name != 'pull_request'"
MAIN_ONLY = "${{ github.ref == 'refs/heads/main' }}"
MAIN_ONLY_SAVE = "success() && github.ref == 'refs/heads/main'"
FUZZ_TARGETS = {"render_content", "render_graph", "redact_model"}


GATE = "gate"
GUARD = "workflow guard"
# Each required check is the name of exactly this job.
REQUIRED_JOBS = {
    "test / fmt / clippy": ("pr.yml", "engine"),
    "compat + round-trip smoke (fixtures)": ("pr.yml", "compat-smoke"),
    GATE: ("pr.yml", "gate"),
    GUARD: ("pr-guard.yml", "guard"),
}
GATE_RUN = "python3 scripts/ci-gate.py"
GATE_ENV = {"NEEDS": "${{ toJSON(needs) }}", "EVENT": "${{ github.event_name }}"}
GUARD_FORK = "${{ github.event.pull_request.head.repo.full_name != github.repository }}"
GUARD_CHECKOUT = {"path": "base", "persist-credentials": False, "fetch-depth": 0}
GUARD_RUNS = (
    "git -C base fetch --no-tags",
    "python3 base/scripts/check-workflows.py --data-only data",
    'python3 base/scripts/check-workflows.py --fork-changes changed.z "$FORK"',
)
# Paths a pull request from a fork may not change: the CI's definition,
# and the baselines and security regressions it judges against.
CI_PATHS = re.compile(
    rb"^(\.github/|scripts/|tools/|\.config/|\.cargo/|fuzz/ci-run(-sharded)?\.sh$|compat/baselines/|compat/redaction/|compat/renderers/"
    rb"|compat/quality-gate\.json$|tests/redact_advisories\.rs$)")
NOT_RUN = re.compile(r"^\s*(echo|printf|#|:|true\b)")


def runs_command(run, cmd):
    """Some line of `run` executes `cmd`: not printed, commented out or
    made to always succeed."""
    for line in (run or "").splitlines():
        if cmd in line and not NOT_RUN.match(line) and "|| true" not in line and "|| :" not in line:
            return True
    return False


def ci_changes(changed):
    """The CI paths among NUL-separated changed paths (bytes, as git
    writes them with -z: no quoting, renames listed as both paths with
    --no-renames)."""
    return [p for p in changed.split(b"\0") if p and CI_PATHS.match(p)]


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
    # The checked commands must run in the repository root: no default
    # working directory for the workflow or for a checked job.
    if "defaults" in wf:
        problem(f"{path.name}: workflow-level defaults are not allowed")
    for jid in list(PR_MUST_RUN) + ["gate"]:
        if "defaults" in (jobs.get(jid) or {}):
            problem(f"{path.name}:{jid}: job-level defaults are not allowed (the checked commands run in the repository root)")
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
            # In the repository root: compat/runner's step runs the same
            # commands on another package.
            steps = [st for st in job.get("steps") or []
                     if runs_command(st.get("run"), cmd) and "working-directory" not in st]
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
    runs = [st for st in gate.get("steps") or [] if str(st.get("run") or "").strip() == GATE_RUN]
    if len(runs) != 1 or "if" in runs[0] or runs[0].get("env") != GATE_ENV or "continue-on-error" in runs[0]:
        problem(f"{path.name}:gate: must run exactly `{GATE_RUN}` unconditionally, with env {GATE_ENV}")
    expected, _ = ci_gate_sets()
    if expected != set(jobs) - {"gate"}:
        problem(f"{path.name}:gate: scripts/ci-gate.py EXPECTED {sorted(expected or [])} differs from pr.yml's jobs {sorted(set(jobs) - {'gate'})}")
    if gate.get("name") != GATE:
        problem(f"{path.name}:gate: its name must be `{GATE}` (the required check)")
    if ci_gate_full() != full:
        problem(f"{path.name}:gate: scripts/ci-gate.py FULL {sorted(ci_gate_full())} differs from the jobs skipped on pull requests {sorted(full)}")


def check_names(wfs):
    """Each required check is the name of exactly its job (REQUIRED_JOBS)
    and of no other job of any workflow: another job of the same name —
    skipped, or always green — would answer for it. A name built from an
    expression must not be able to produce a required name."""
    files = {p.name for p, _ in wfs}
    for name, (wf_name, jid) in REQUIRED_JOBS.items():
        if wf_name not in files:
            problem(f"{wf_name} (job `{jid}`, the required check `{name}`) is missing")
    for path, wf in wfs:
        for jid, job in (wf.get("jobs") or {}).items():
            name = str(job.get("name") or jid)
            pattern = re.compile(".*".join(re.escape(part) for part in re.split(r"\$\{\{.*?\}\}", name)), re.S)
            for req, (wf_name, req_jid) in REQUIRED_JOBS.items():
                mine = (path.name, jid) == (wf_name, req_jid)
                if mine and name != req:
                    problem(f"{path.name}:{jid}: must be named `{req}` (a required check), not `{name}`")
                elif not mine and pattern.fullmatch(req):
                    problem(f"{path.name}:{jid}: its name `{name}` can be the required check `{req}`")


def check_guard(path, wf):
    """pr-guard.yml: the base branch's definition checks a pull request's
    workflows as data and refuses a fork's change to the CI; it checks
    out and runs nothing of the pull request's, and nothing switches it
    off."""
    on = wf.get("on") or wf.get(True) or {}
    if set(on) != {"pull_request_target"} or on.get("pull_request_target"):
        problem(f"{path.name}: must run on pull_request_target only, unfiltered")
    if wf.get("permissions") != {"contents": "read"}:
        problem(f"{path.name}: permissions must be exactly contents: read")
    jobs = wf.get("jobs") or {}
    if list(jobs) != ["guard"] or jobs["guard"].get("name") != GUARD:
        problem(f"{path.name}: must have exactly one job, `guard`, named `{GUARD}`")
    for jid, job in jobs.items():
        for key in ("permissions", "if", "continue-on-error", "strategy", "container", "services", "uses"):
            if key in job:
                problem(f"{path.name}:{jid}: `{key}` is not allowed in the guard")
        if "secrets" in str(job):
            problem(f"{path.name}:{jid}: no secrets")
    steps = [st for _, st in steps_of(wf)]
    checkouts = [st for st in steps if st.get("uses")]
    if len(checkouts) != 1 or not re.fullmatch(r"actions/checkout@[0-9a-f]{40}", str(checkouts[0].get("uses"))) \
            or checkouts[0].get("with") != GUARD_CHECKOUT or steps.index(checkouts[0]) != 0:
        problem(f"{path.name}: the only action must be actions/checkout first, with exactly {GUARD_CHECKOUT}")
    for st in steps:
        if "if" in st or "continue-on-error" in st:
            problem(f"{path.name}: no step of the guard may have `if` or continue-on-error ({st.get('name') or st.get('uses')})")
    for cmd in GUARD_RUNS:
        if not any(runs_command(st.get("run"), cmd) for st in steps):
            problem(f"{path.name}: does not run `{cmd}`")
    for st in steps:
        if "--fork-changes" in (st.get("run") or "") and (st.get("env") or {}).get("FORK") != GUARD_FORK:
            problem(f"{path.name}: the fork decision's FORK must be exactly `{GUARD_FORK}`")
    if "defaults" in wf or any("defaults" in j for j in jobs.values()):
        problem(f"{path.name}: no defaults (the commands run where they are written)")


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
    if sys.argv[1:2] == ["--fork-changes"]:
        # pr-guard.yml: <NUL-separated changed paths> <is the pull request from a fork?>
        ci = ci_changes(Path(sys.argv[2]).read_bytes())
        fork = sys.argv[3] == "true"
        for p in ci:
            print("  CI path changed:", p.decode("utf-8", "backslashreplace"))
        if fork and ci:
            print(f"::error::a pull request from a fork changes {len(ci)} CI path(s); a maintainer reviews it and lands it")
            return 1
        print(f"ok (fork: {fork}; CI paths changed: {len(ci)})")
        return 0
    if sys.argv[1:2] == ["--data-only"]:
        DATA = Path(sys.argv[2]).resolve()
        data_only = True
    wfs = []
    paths = sorted(list((DATA / ".github/workflows").glob("*.yml")) + list((DATA / ".github/workflows").glob("*.yaml")))
    for path in paths:
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
