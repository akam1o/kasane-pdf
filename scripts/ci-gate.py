#!/usr/bin/env python3
"""The decision of pr.yml's `gate` job: does this run pass?

    NEEDS='${{ toJSON(needs) }}' EVENT='${{ github.event_name }}' scripts/ci-gate.py

Every job of EXPECTED must be among the results the gate receives and
must have succeeded. The only exception: a job of the full gate (FULL)
may be skipped on a pull request, where it does not run. A failed,
cancelled or timed-out job ("failure" or "cancelled"), a result that is
missing or unknown, a job the gate does not expect, and any skip
elsewhere — a job of the full gate skipped on a push or a dispatch
means the gate did not run in full — fail the gate.
scripts/test_ci_scripts.py tests this function; scripts/check-workflows.py
checks that EXPECTED is exactly the jobs of pr.yml other than `gate` and
that FULL is exactly those that skip pull requests (reading this file
as data, never running it).
"""
import json
import os
import sys

# Every job of pr.yml but `gate`.
EXPECTED = frozenset({
    "engine", "features-lint", "features-test", "render-fuzz-smoke", "r2-depth-linux", "wasm", "audit",
    "crate-package", "compat-smoke", "msrv", "semver", "redaction-model", "crate-package-full", "renderers-linux",
})
# The jobs of pr.yml that run outside pull requests only.
FULL = frozenset({"msrv", "semver", "redaction-model", "crate-package-full", "renderers-linux"})


def failures(needs, event):
    """The jobs that keep the gate from passing ({} when it passes)."""
    bad = {}
    if not needs:
        return {"(none)": "the gate needs no job"}
    for job in EXPECTED - set(needs):
        bad[job] = "missing"
    for job in set(needs) - EXPECTED:
        bad[job] = "unexpected job"
    for job, v in needs.items():
        result = (v or {}).get("result")
        if result == "success":
            continue
        if result == "skipped" and event == "pull_request" and job in FULL:
            continue
        bad[job] = result
    return bad


def main():
    needs = json.loads(os.environ["NEEDS"])
    event = os.environ["EVENT"]
    bad = failures(needs, event)
    for job in sorted(set(needs) | EXPECTED):
        r = (needs.get(job) or {}).get("result", "missing")
        print(f"{job:22} {r}{'  <-- not a pass' if job in bad else ''}")
    if bad:
        sys.exit(f"gate failed: {', '.join(sorted(bad))}")
    print(f"gate passed ({event})")


if __name__ == "__main__":
    main()
