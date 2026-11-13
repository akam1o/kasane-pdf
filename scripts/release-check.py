#!/usr/bin/env python3
"""Before main is moved to a release commit and the tag is created: has
this exact commit passed the CI parts of the release gate?
(docs/releasing.md, "The release gate")

    scripts/release-check.py <sha> [--require-nightly] [--no-main]

Reads every workflow run GitHub has for <sha> (`gh api`, all pages) and
passes only when:

  * a pr.yml run on a push or a dispatch (the full gate — a pull
    request's run is the fast gate) concluded `success`, and its `gate`
    job did;
  * codeql-release.yml (pushes to release/**, or dispatched on the
    commit) concluded `success` on it — the run that fails on any result
    that is not a reviewed alert. The default setup's analysis does not
    count: its success means the analysis ran, not that it found
    nothing new;
  * origin/main is an ancestor of <sha> (main is fast-forwarded to it,
    no new commit) — not with --no-main (a patch release from an older
    tag, which main does not move to);
  * with --require-nightly, a nightly.yml run on <sha> whose corpus job
    concluded `success` with its redaction corpus and full run steps
    `success` — a successful run whose corpus job was skipped (no
    credentials) does not count, nor does a nightly of another commit.

It prints what it found and never reports a check it did not see. The
rest of the gate — long fuzzing, the local package checks, external
renderers, and in a security release the local corpus run — is not
visible here and is recorded by hand.
"""
import json
import subprocess
import sys

REPO = "akam1o/kasane-pdf"
FULL_EVENTS = {"push", "workflow_dispatch"}
PR = ".github/workflows/pr.yml"
NIGHTLY = ".github/workflows/nightly.yml"
CODEQL = ".github/workflows/codeql-release.yml"
CORPUS_JOB = "full compatibility + round-trip (511)"
CORPUS_STEPS = ("redaction corpus —", "full run", "regression check")


def corpus_ran(job):
    """The nightly's corpus job ran and every corpus step succeeded."""
    if not job or job.get("conclusion") != "success":
        return False
    steps = job.get("steps") or []
    return all(any(st.get("name", "").startswith(p) and st.get("conclusion") == "success" for st in steps)
               for p in CORPUS_STEPS)


def verdict(sha, runs, jobs, main_is_ancestor, require_nightly):
    """Problems keeping <sha> from release ([] when it may be released).

    runs: workflow runs for the commit (`id`, `path`, `event`,
    `conclusion`, `head_sha`); jobs: run id -> that run's jobs (`name`,
    `conclusion`, `steps`).
    """
    problems = []
    mine = [r for r in runs if r.get("head_sha") == sha]

    def job(r, name):
        return next((j for j in jobs.get(r.get("id")) or [] if j.get("name") == name), None)

    full = [r for r in mine if r.get("path") == PR and r.get("event") in FULL_EVENTS]
    if not any(r.get("conclusion") == "success" and (job(r, "gate") or {}).get("conclusion") == "success" for r in full):
        problems.append("no successful full gate (pr.yml on a push or dispatch, `gate` success) on this commit")
    if not any(r.get("path") == CODEQL and r.get("event") in FULL_EVENTS and r.get("conclusion") == "success"
               for r in mine):
        problems.append("no successful codeql-release.yml (no unreviewed CodeQL result) on this commit")
    if require_nightly:
        nightly = [r for r in mine if r.get("path") == NIGHTLY and r.get("conclusion") == "success"]
        if not any(corpus_ran(job(r, CORPUS_JOB)) for r in nightly):
            problems.append("no nightly.yml on this commit whose corpus job ran and succeeded")
    if not main_is_ancestor:
        problems.append("origin/main is not an ancestor: main cannot be fast-forwarded to this commit")
    return problems


def gh_all(path, key):
    out = subprocess.run(["gh", "api", "--paginate", "--slurp", path], capture_output=True, text=True,
                         check=True).stdout
    return [item for page in json.loads(out) for item in page[key]]


def main():
    args = sys.argv[1:]
    require_nightly = "--require-nightly" in args
    no_main = "--no-main" in args
    args = [a for a in args if a not in ("--require-nightly", "--no-main")]
    if len(args) != 1:
        sys.exit(__doc__)
    sha = subprocess.run(["git", "rev-parse", args[0] + "^{commit}"], capture_output=True, text=True,
                         check=True).stdout.strip()
    runs = gh_all(f"repos/{REPO}/actions/runs?head_sha={sha}&per_page=100", "workflow_runs")
    jobs = {}
    for r in runs:
        print(f"{r['path'].split('/')[-1]:20} {r['event']:18} {r['status']:10} {r.get('conclusion')}  run {r['id']}")
        if r["path"] in (PR, NIGHTLY):
            jobs[r["id"]] = gh_all(f"repos/{REPO}/actions/runs/{r['id']}/jobs?per_page=100", "jobs")
    subprocess.run(["git", "fetch", "--quiet", "origin", "main"], check=True)
    ancestor = subprocess.run(["git", "merge-base", "--is-ancestor", "origin/main", sha]).returncode == 0
    problems = verdict(sha, runs, jobs, ancestor or no_main, require_nightly)
    for p in problems:
        print("PROBLEM:", p)
    if problems:
        sys.exit(1)
    print(f"{sha}: full gate and CodeQL green{', nightly corpus green' if require_nightly else ''}"
          f"{'' if no_main else '; main fast-forwards to it'}")


if __name__ == "__main__":
    main()
