#!/usr/bin/env python3
"""The decision of pr.yml's `gate` job: does this run pass?

    NEEDS='${{ toJSON(needs) }}' EVENT='${{ github.event_name }}' scripts/ci-gate.py

Every job the gate needs must have succeeded. The only exception: a
job of the full gate (FULL) may be skipped on a pull request, where it
does not run. A failed, cancelled or timed-out job ("failure" or
"cancelled") fails the gate, and so does any skip elsewhere — a job of
the full gate skipped on a push or a dispatch means the gate did not
run in full. scripts/test_ci_scripts.py tests this function;
scripts/check-workflows.py checks that FULL is exactly the set of
pr.yml jobs that skip pull requests.
"""
import json
import os
import sys

# The jobs of pr.yml that run outside pull requests only.
FULL = frozenset({"msrv", "semver", "redaction-model", "crate-package-full"})


def failures(needs, event):
    """The jobs that keep the gate from passing ({} when it passes)."""
    bad = {}
    if not needs:
        return {"(none)": "the gate needs no job"}
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
    for job in sorted(needs):
        r = needs[job].get("result")
        print(f"{job:22} {r}{'  <-- not a pass' if job in bad else ''}")
    if bad:
        sys.exit(f"gate failed: {', '.join(sorted(bad))}")
    print(f"gate passed ({event})")


if __name__ == "__main__":
    main()
