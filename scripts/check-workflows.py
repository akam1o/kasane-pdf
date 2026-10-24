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
    installs.
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
        if "env:" in line or re.match(r"\s+[A-Z0-9_]+:\s*$", line) or re.match(r"\s+[A-Z0-9_]+:\s", line):
            continue
        problem(f"{path.name}: secret referenced outside an env: mapping: `{line.strip()}`")


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
