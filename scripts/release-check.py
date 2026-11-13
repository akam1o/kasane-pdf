#!/usr/bin/env python3
"""Before main is moved to a release commit and the tag is created: has
this exact commit passed the release gate's CI parts? (docs/releasing.md)

    scripts/release-check.py <sha> [--require-nightly] [--no-main]

Reads the workflow runs GitHub has for <sha> (`gh api`) and passes only
when:

  * a pr.yml run on a push or a dispatch (the full gate — a pull
    request's run is the fast gate) concluded `success`, and its `gate`
    job did;
  * origin/main is an ancestor of <sha> (main is fast-forwarded to it,
    no new commit) — not with --no-main (a patch release from an older
    tag, which main does not move to);
  * with --require-nightly, a nightly.yml run (corpus) on <sha>
    concluded `success` — a nightly of another commit does not count.

It prints what it found for every workflow and never reports a check it
did not see. The corpus run of a security release is local and recorded
by hand (docs/releasing.md, B.2); everything else is required here.
"""
import json
import subprocess
import sys

REPO = "akam1o/kasane-pdf"
FULL_EVENTS = {"push", "workflow_dispatch"}


def verdict(sha, runs, gate_jobs, main_is_ancestor, require_nightly):
    """Problems keeping <sha> from release ([] when it may be released).

    runs: workflow runs for the commit (`path`, `event`, `conclusion`,
    `head_sha`, `id`); gate_jobs: run id -> conclusion of its `gate` job.
    """
    problems = []
    mine = [r for r in runs if r.get("head_sha") == sha]
    full = [r for r in mine if r.get("path") == ".github/workflows/pr.yml" and r.get("event") in FULL_EVENTS]
    if not any(r.get("conclusion") == "success" and gate_jobs.get(r.get("id")) == "success" for r in full):
        problems.append("no successful full gate (pr.yml on a push or dispatch, `gate` success) on this commit")
    if require_nightly:
        nightly = [r for r in mine if r.get("path") == ".github/workflows/nightly.yml"]
        if not any(r.get("conclusion") == "success" for r in nightly):
            problems.append("no successful nightly.yml (corpus) on this commit")
    if not main_is_ancestor:
        problems.append("origin/main is not an ancestor: main cannot be fast-forwarded to this commit")
    return problems


def gh(path):
    return json.loads(subprocess.run(["gh", "api", path], capture_output=True, text=True, check=True).stdout)


def main():
    args = sys.argv[1:]
    require_nightly = "--require-nightly" in args
    no_main = "--no-main" in args
    args = [a for a in args if a not in ("--require-nightly", "--no-main")]
    if len(args) != 1:
        sys.exit(__doc__)
    sha = subprocess.run(["git", "rev-parse", args[0] + "^{commit}"], capture_output=True, text=True,
                         check=True).stdout.strip()
    runs = gh(f"repos/{REPO}/actions/runs?head_sha={sha}&per_page=100")["workflow_runs"]
    gate_jobs = {}
    for r in runs:
        print(f"{r['path'].split('/')[-1]:20} {r['event']:18} {r['status']:10} {r.get('conclusion')}  run {r['id']}")
        if r["path"] == ".github/workflows/pr.yml":
            jobs = gh(f"repos/{REPO}/actions/runs/{r['id']}/jobs?per_page=100")["jobs"]
            gate_jobs[r["id"]] = next((j["conclusion"] for j in jobs if j["name"] == "gate"), None)
    subprocess.run(["git", "fetch", "--quiet", "origin", "main"], check=True)
    ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", "origin/main", sha]).returncode == 0
    problems = verdict(sha, runs, gate_jobs, ancestor or no_main, require_nightly)
    for p in problems:
        print("PROBLEM:", p)
    if problems:
        sys.exit(1)
    print(f"{sha}: full gate green{', nightly green' if require_nightly else ''}"
          f"{'' if no_main else '; main fast-forwards to it'}")


if __name__ == "__main__":
    main()
