#!/usr/bin/env python3
"""Regression gate over a kasane-compat run report.

Two modes:

  ci-regression.py summarize RUN.json > baseline.json
      Reduce a run report to the aggregate counts and the per-file
      verdicts / stage classes / round-trip classes — no paths, no
      images, no timings — so the result can be committed as a baseline.

  ci-regression.py check RUN.json baseline.json
      Exit 1 when any file present in both got *worse*: a stage that was
      `pass` and no longer is, a round-trip check that was PASS and is now
      FAIL, a `reject` file that now crashes or times out, or any crash /
      timeout at all. Improvements and files only in one side are
      reported but never fail the gate.

Only aggregates and cause ids are ever written; the corpus itself and
rendered images stay out of git.
"""
import json
import sys

STAGES = ("parse", "render", "text", "rewrite")
RT_CHECKS = ("reparse", "structure", "render", "text", "annotations", "external_valid", "external_no_new")
RANK = {"pass": 0, "skip": 0, "mismatch": 1, "unsupported": 2, "error": 3, "timeout": 4, "crash": 5}


def summarize(run):
    files = {}
    for f in run["files"]:
        rt = f.get("roundtrip") or {}
        files[f["id"]] = {
            "expect": f.get("expect", ""),
            "verdict": f.get("verdict", ""),
            "stages": {s: (f.get(s) or {}).get("class", "") for s in STAGES},
            "causes": sorted(f.get("causes") or []),
            "warnings": len((f.get("metrics") or {}).get("warnings") or []),
            "render_recovered": ((f.get("render_recovered") or {}).get("class") or "skip"),
            "render_approximate": bool(f.get("render_approximate", False)),
            "recovery_notes": int(f.get("recovery_notes", 0) or 0),
            "roundtrip": {c: (rt.get(c) or {}).get("class", "") for c in RT_CHECKS} if rt else {},
            "roundtrip_full": bool(rt.get("full")) if rt else None,
        }
    agg = {
        "files": len(files),
        "crash": sum(1 for v in files.values() if v["verdict"] == "crash"),
        "timeout": sum(1 for v in files.values() if v["verdict"] == "timeout"),
    }
    for s in STAGES:
        agg[f"{s}_pass"] = sum(1 for v in files.values() if v["stages"][s] == "pass")
    agg["roundtrip_full"] = sum(1 for v in files.values() if v["roundtrip_full"])
    return {"aggregate": agg, "files": files}


def worse(new, old):
    return RANK.get(new, 9) > RANK.get(old, 9)


def check(run, base):
    cur = summarize(run)["files"]
    old = base["files"]
    regressions, improvements = [], []
    for fid, n in cur.items():
        if n["verdict"] in ("crash", "timeout"):
            regressions.append(f"{fid}: verdict {n['verdict']}")
        o = old.get(fid)
        if o is None:
            continue
        for s in STAGES:
            if o["stages"][s] == "pass" and n["stages"][s] != "pass":
                regressions.append(f"{fid}: {s} pass -> {n['stages'][s]}")
            elif worse(n["stages"][s], o["stages"][s]):
                regressions.append(f"{fid}: {s} {o['stages'][s]} -> {n['stages'][s]}")
            elif n["stages"][s] == "pass" and o["stages"][s] != "pass":
                improvements.append(f"{fid}: {s} {o['stages'][s]} -> pass")
        if n.get("warnings", 0) and not o.get("warnings", 0) and n["verdict"] == "pass":
            print(f"note: {fid}: now accepted with warnings (was a clean pass)")
        if (
            n.get("render_recovered") == "pass"
            and o.get("render_recovered", "skip") != "pass"
            and n.get("render") != "pass"
        ):
            print(f"note: {fid}: now renders in recovery mode (not counted as strict)")
        if n.get("render_approximate") and not o.get("render_approximate") and n.get("render") == "pass":
            print(f"note: {fid}: strict render now uses a reported approximation")
        for c in RT_CHECKS:
            oc, nc = o["roundtrip"].get(c, ""), n["roundtrip"].get(c, "")
            if oc == "Pass" and nc == "Fail":
                regressions.append(f"{fid}: round-trip {c} PASS -> FAIL")
    missing = sorted(set(old) - set(cur))
    added = sorted(set(cur) - set(old))
    print(f"files: {len(cur)} now, {len(old)} in baseline, {len(missing)} missing, {len(added)} new")
    for line in improvements:
        print("improved:", line)
    for line in regressions:
        print("REGRESSION:", line)
    if regressions:
        print(f"{len(regressions)} regression(s)")
        return 1
    print("no regressions")
    return 0


def main(argv):
    if len(argv) >= 3 and argv[1] == "summarize":
        with open(argv[2]) as fh:
            json.dump(summarize(json.load(fh)), sys.stdout, indent=1, sort_keys=True)
        sys.stdout.write("\n")
        return 0
    if len(argv) >= 4 and argv[1] == "check":
        with open(argv[2]) as fh:
            run = json.load(fh)
        with open(argv[3]) as fh:
            base = json.load(fh)
        return check(run, base)
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
