#!/usr/bin/env python3
"""Before main is moved to a release commit and the tag is created: has
this exact commit passed the CI parts of the release gate?
(docs/releasing.md, "The release gate")

    scripts/release-check.py <sha> --local-results DIR [--require-nightly] [--no-main]
        [--disclosure <S> --disclosure-results DIR2 --history-results DIR3] [--legacy-local]
    scripts/release-check.py --published X.Y.Z [<ref>]

The second form is for after publication (docs/releasing.md B.4), before
S is merged: version X.Y.Z is on crates.io (not yanked), its package's
`.cargo_vcs_info.json` names the commit of the tag vX.Y.Z, and <ref>'s
(default HEAD) compat/advisories/history.json lists X.Y.Z with an
expected state for every input — without that, main's advisory history
fails (`version_not_covered`). Nothing of it exists before publication,
so the first form never asks for it.

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

and, for the local part of the gate (scripts/release-verify.py):

  * DIR holds a `release` run of scripts/release-verify.py that proves
    <sha> passed (`release-verify.py check`: the commit and tree, the
    commit's verifier and config, every check passed, digests, age, no
    newer run) — the corpora, external renderers, long fuzzing, the
    10,000-seed model and the security regressions, run locally;
  * in a security release, --disclosure <S> --disclosure-results DIR2:
    S (the disclosure commit) descends from <sha> (R), changes only
    tests/, docs/, CHANGELOG.md, compat/advisories/history.json and the
    two external-renderer baselines after it, and DIR2 proves S passed the
    `disclosure` profile (the new advisory regressions, their entries in
    both renderer baselines, and the external renderers on them). R's evidence and
    S's are checked each against its own commit and profile: one cannot
    stand in for the other. With it, --history-results DIR3: a
    `security-history` run on S (scripts/advisory-history.py — every
    published version built from crates.io, each advisory's affected range
    reproduced, the candidate safe as expected) — the evidence for the
    affected versions the advisory states.

A commit older than scripts/release-verify.py cannot carry this
evidence: --legacy-local accepts it, saying so, and nothing else does.
The package checks of docs/releasing.md stay manual. It prints what it
found and never reports a check it did not see.
"""
import importlib.util
import os
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


def release_verify():
    spec = importlib.util.spec_from_file_location(
        "release_verify", os.path.join(os.path.dirname(os.path.abspath(__file__)), "release-verify.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


S_MAY_CHANGE = ("tests/", "docs/")
# The new advisory's range and expected states are disclosure material,
# data read by scripts/advisory-history.py only.
# The new advisory inputs' entries, data read by scripts/verify-renderers.py
# only — added, never a change to what R's baseline records
# (baseline_problems).
S_BASELINES = ("compat/renderers/baseline-linux-x86_64.json", "compat/renderers/baseline-darwin-arm64.json")
S_MAY_CHANGE_FILES = ("CHANGELOG.md", "compat/advisories/history.json") + S_BASELINES


def baseline_problems(path, r_text, s_text):
    """S may add `adv:` cases to a renderer baseline R has; every case R
    records, and the platform, renderer versions, dpi and schema, stay as
    R has them (the reason may say why the cases were added)."""
    try:
        r, s = json.loads(r_text), json.loads(s_text)
    except (TypeError, ValueError):
        return [f"{path}: not readable at R or at S"]
    if not isinstance(r, dict) or not isinstance(s, dict) or not isinstance(r.get("cases"), dict) \
            or not isinstance(s.get("cases"), dict):
        return [f"{path}: not a baseline at R or at S"]
    p = []
    for k in sorted((set(r) | set(s)) - {"cases", "reason"}):
        if r.get(k) != s.get(k):
            p.append(f"{path}: {k} changed after R")
    for name, case in sorted(r["cases"].items()):
        if s["cases"].get(name) != case:
            p.append(f"{path}: {name} changed or removed after R")
    for name in sorted(set(s["cases"]) - set(r["cases"])):
        if not name.startswith("adv:"):
            p.append(f"{path}: {name} added after R, not an advisory input")
    return p


def local_problems(repo, sha, results, disclosure=None, disclosure_results=None, legacy=False,
                   profiles=("release", "disclosure", "security-history"), history_results=None):
    """Problems with the local evidence for <sha> (and S) — [] when it
    proves what the release needs."""
    rv = release_verify()
    problems = []
    has = rv.blob(repo, sha, "scripts/release-verify.py") is not None
    if not has:
        if not legacy:
            problems.append("this commit predates scripts/release-verify.py: its local gate was recorded by hand "
                            "(--legacy-local accepts that)")
    elif results is None:
        problems.append("no local verification results: run scripts/release-verify.py run --ref <sha> and pass "
                        "--local-results DIR")
    else:
        problems += [f"local: {p}" for p in rv.verify(repo, sha, results, profiles[0])]
    if (disclosure is None) != (disclosure_results is None):
        problems.append("--disclosure and --disclosure-results go together")
    elif disclosure is not None:
        s, _ = rv.resolve(repo, disclosure)
        if s == sha or subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", sha, s]).returncode != 0:
            problems.append("the disclosure commit does not descend from the release commit")
        else:
            out = subprocess.run(["git", "-C", str(repo), "diff", "--no-renames", "--name-only", "-z", sha, s],
                                 capture_output=True, check=True).stdout
            changed = [p.decode("utf-8", "backslashreplace") for p in out.split(b"\0") if p]
            other = [p for p in changed if not (p.startswith(S_MAY_CHANGE) or p in S_MAY_CHANGE_FILES)]
            if other:
                problems.append(f"the disclosure commit changes {len(other)} file(s) outside tests/, docs/, "
                                f"CHANGELOG.md, compat/advisories/history.json and the renderer baselines: the code "
                                f"verified with its tests would not be the release's")
            for path in (p for p in changed if p in S_BASELINES):
                show = lambda c: subprocess.run(["git", "-C", str(repo), "show", f"{c}:{path}"], capture_output=True,
                                                text=True).stdout or None
                problems += baseline_problems(path, show(sha), show(s))
        problems += [f"disclosure: {p}" for p in rv.verify(repo, s, disclosure_results, profiles[1])]
        # A security release states which published versions are affected:
        # the history run on S (its new advisory, its range, every
        # published version) is the evidence for that.
        if history_results is None:
            problems.append("a security release needs --history-results (scripts/release-verify.py run --ref S "
                            "--profile security-history)")
        else:
            problems += [f"history: {p}" for p in rv.verify(repo, s, history_results, profiles[2])]
    elif history_results is not None:
        problems.append("--history-results goes with --disclosure")
    return problems


# The states scripts/advisory-history.py records of an observed version.
OBSERVED_STATES = ("NA", "LEAK", "SLOW", "REMOVED", "REFUSED", "KEPT", "FAST")


def published_problems(version, index, vcs_sha, tag_sha, history):
    """What is missing after publishing `version`: `index` the crates.io
    index lines (dicts), `vcs_sha` the commit the package names (None when
    it could not be read), `tag_sha` the tag's commit (None: no tag),
    `history` the history.json to merge (a dict, None: unreadable)."""
    p = []
    if not any(e.get("vers") == version and not e.get("yanked") for e in index):
        p.append(f"{version} is not on crates.io (or is yanked)")
    if tag_sha is None:
        p.append(f"no tag v{version}")
    elif vcs_sha != tag_sha:
        p.append(f"the package of {version} does not name the commit of v{version}")
    if not isinstance(history, dict) or not isinstance(history.get("expected"), dict):
        p.append("history.json is unreadable")
        return p
    # The release just published is judged, not excluded: its package is
    # what users install.
    if version not in (history.get("versions") or []):
        p.append(f"history.json does not list {version} (advisory-history.py run --mode main, then update-expected)")
    elif not history["expected"]:
        p.append("history.json has no inputs")
    else:
        bad = [n for n, row in history["expected"].items()
               if not isinstance(row, dict) or row.get(version) not in OBSERVED_STATES]
        if bad:
            p.append(f"history.json has no expected state of {version} for {len(bad)} input(s)")
    return p


def check_published(version, ref):
    import io
    import tarfile
    import urllib.request
    if not __import__("re").fullmatch(r"\d+\.\d+\.\d+", version):
        sys.exit("release-check: --published needs X.Y.Z")
    req = lambda url: urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "kasane-pdf release-check"}),
                                             timeout=60).read()
    index = [json.loads(l) for l in req("https://index.crates.io/ka/sa/kasane-pdf").decode().splitlines() if l.strip()]
    vcs = None
    try:
        data = req(f"https://static.crates.io/crates/kasane-pdf/kasane-pdf-{version}.crate")
        with tarfile.open(fileobj=io.BytesIO(data)) as t:
            f = t.extractfile(f"kasane-pdf-{version}/.cargo_vcs_info.json")
            vcs = json.loads(f.read())["git"]["sha1"] if f else None
    except Exception:  # noqa: BLE001 — any failure reads as "not named"
        vcs = None
    t = subprocess.run(["git", "rev-parse", "-q", "--verify", f"refs/tags/v{version}^{{commit}}"],
                       capture_output=True, text=True)
    tag = t.stdout.strip() if t.returncode == 0 else None
    h = subprocess.run(["git", "show", f"{ref}:compat/advisories/history.json"], capture_output=True, text=True)
    try:
        history = json.loads(h.stdout) if h.returncode == 0 else None
    except ValueError:
        history = None
    problems = published_problems(version, index, vcs, tag, history)
    for p in problems:
        print("PROBLEM:", p)
    if problems:
        sys.exit(1)
    print(f"{version}: on crates.io from v{version} ({tag}); {ref}'s advisory history covers it")


def gh_all(path, key):
    out = subprocess.run(["gh", "api", "--paginate", "--slurp", path], capture_output=True, text=True,
                         check=True).stdout
    return [item for page in json.loads(out) for item in page[key]]


def main():
    args = sys.argv[1:]
    if args[:1] == ["--published"]:
        if len(args) not in (2, 3):
            sys.exit(__doc__)
        return check_published(args[1], args[2] if len(args) == 3 else "HEAD")
    require_nightly = "--require-nightly" in args
    no_main = "--no-main" in args
    legacy = "--legacy-local" in args
    args = [a for a in args if a not in ("--require-nightly", "--no-main", "--legacy-local")]
    opts = {}
    for flag in ("--local-results", "--disclosure", "--disclosure-results", "--history-results"):
        if flag in args:
            i = args.index(flag)
            opts[flag] = args[i + 1]
            del args[i:i + 2]
    if len(args) != 1:
        sys.exit(__doc__)
    repo = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True,
                          check=True).stdout.strip()
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
    problems += local_problems(repo, sha, opts.get("--local-results"), opts.get("--disclosure"),
                               opts.get("--disclosure-results"), legacy, history_results=opts.get("--history-results"))
    if legacy and not problems:
        print("NOTE: --legacy-local: this commit's local gate is not machine-checked")
    for p in problems:
        print("PROBLEM:", p)
    if problems:
        sys.exit(1)
    print(f"{sha}: full gate and CodeQL green{', nightly corpus green' if require_nightly else ''}"
          f"{'' if no_main else '; main fast-forwards to it'}"
          f"{'' if legacy else '; local release verification proves it'}"
          f"{'; the disclosure commit is verified' if '--disclosure' in opts else ''}")


if __name__ == "__main__":
    main()
