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

  ci-regression.py quality RUN.json
      Print the exact-render quality-gate evaluation (every flag, the
      exemptions applied, the distribution) without comparing.

Exact-render quality gate (compat/quality-gate.json,
docs/plans/quality-gate.md): every page of a file whose strict render
passed without a reported approximation is compared with Poppler by
the rules in the config. A file is `ok`, `flag:<rule>` (an enforced
rule fired), `report:<rule>` (a report-only rule fired), `exempt:<why>`
(the comparison is not meaningful — Poppler drew nothing, the page
sizes differ) or `n/a` (no reference). `check` fails when a file is
flagged by an enforced rule and was not flagged in the baseline — a new
silent misrender; flags already in the baseline are listed, not failed.

Only aggregates and cause ids are ever written; the corpus itself and
rendered images stay out of git.
"""
import json
import os
import sys

STAGES = ("parse", "render", "text", "rewrite")
RT_CHECKS = ("reparse", "structure", "render", "text", "annotations", "external_valid", "external_no_new")
RANK = {"pass": 0, "skip": 0, "mismatch": 1, "unsupported": 2, "error": 3, "timeout": 4, "crash": 5}


GATE_CONFIG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compat", "quality-gate.json")


def load_gate():
    with open(GATE_CONFIG) as fh:
        return json.load(fh)


def page_findings(q, gate):
    """Rules fired on one page's quality record, or an exemption."""
    ev = gate["evaluate"]
    io, ir = q.get("ink_ours", 0.0), q.get("ink_ref", 0.0)
    sizes = q.get("sizes") or []
    if len(sizes) == 4 and min(sizes[2], sizes[3]) > 0:
        rel = max(abs(sizes[0] - sizes[2]) / sizes[2], abs(sizes[1] - sizes[3]) / sizes[3])
        if rel > ev["max_size_rel_diff"]:
            return [], "page-size"
    if ir < ev["blank_reference_ink"] and io >= ev["min_ink"]:
        return [], "reference-blank"
    fired = []
    for name, r in gate["rules"].items():
        if r["kind"] == "ink_ratio":
            hit = io >= ev["min_ink"] and ir >= ev["min_ink"] and not (r["low"] <= io / ir <= r["high"])
        elif r["kind"] == "content_ssim":
            hit = q.get("content_ssim", 1.0) < r["max"] and q.get("diff_px_at_best_shift", 0.0) > r["min_diff_at_best_shift"]
        else:
            raise ValueError(f"unknown rule kind {r['kind']!r}")
        if hit:
            fired.append(name)
    return fired, None


def quality(f, gate):
    """The file's gate class (see the module docstring)."""
    if not (
        (f.get("render") or {}).get("class") == "pass"
        and not f.get("recovered")
        and not f.get("render_approximate")
    ):
        return "n/a"
    m = f.get("metrics") or {}
    pages = [p.get("quality") for p in (m.get("render_pages") or []) if p.get("quality")]
    if not pages and m.get("render_quality"):
        pages = [m["render_quality"]]
    if not pages:
        return "n/a"
    enforced, reported, exempt = set(), set(), set()
    for q in pages:
        fired, ex = page_findings(q, gate)
        if ex:
            exempt.add(ex)
        for rule in fired:
            (enforced if gate["rules"][rule]["enforced"] else reported).add(rule)
    if enforced:
        return "flag:" + ",".join(sorted(enforced))
    if reported:
        return "report:" + ",".join(sorted(reported))
    if exempt:
        return "exempt:" + ",".join(sorted(exempt))
    return "ok"


def summarize(run):
    gate = load_gate()
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
            "incremental": ((f.get("incremental") or {}).get("class") or ""),
            "quality": quality(f, gate),
        }
    agg = {
        "files": len(files),
        "crash": sum(1 for v in files.values() if v["verdict"] == "crash"),
        "timeout": sum(1 for v in files.values() if v["verdict"] == "timeout"),
    }
    for s in STAGES:
        agg[f"{s}_pass"] = sum(1 for v in files.values() if v["stages"][s] == "pass")
    agg["roundtrip_full"] = sum(1 for v in files.values() if v["roundtrip_full"])
    agg["incremental_pass"] = sum(1 for v in files.values() if v["incremental"] == "pass")
    agg["quality_flag"] = sum(1 for v in files.values() if v["quality"].startswith("flag:"))
    return {"aggregate": agg, "files": files}


def worse(new, old):
    return RANK.get(new, 9) > RANK.get(old, 9)


def check(run, base):
    cur = summarize(run)["files"]
    old = base["files"]
    regressions, improvements, known_flags, unrecorded = [], [], [], []
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
        if o.get("incremental") == "pass" and n.get("incremental") not in ("pass", None, ""):
            regressions.append(f"{fid}: incremental pass -> {n['incremental']}")
        oq, nq = o.get("quality", ""), n["quality"]
        if nq.startswith("flag:") and "quality" not in o:
            # Baseline from before the gate: nothing to compare with yet
            # (staged roll-out — regenerate the baseline to arm it).
            unrecorded.append(f"{fid}: {nq}")
        elif nq.startswith("flag:"):
            if oq.startswith("flag:"):
                known_flags.append(f"{fid}: {nq}")
            else:
                regressions.append(f"{fid}: exact render {nq} (quality gate; was {oq or 'unrecorded'})")
        elif oq.startswith("flag:"):
            improvements.append(f"{fid}: quality {oq} -> {nq}")
    missing = sorted(set(old) - set(cur))
    added = sorted(set(cur) - set(old))
    print(f"files: {len(cur)} now, {len(old)} in baseline, {len(missing)} missing, {len(added)} new")
    for line in improvements:
        print("improved:", line)
    for line in known_flags:
        print("known quality flag (in baseline):", line)
    for line in unrecorded:
        print("quality flag (baseline predates the gate, not enforced):", line)
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
    if len(argv) >= 3 and argv[1] == "quality":
        with open(argv[2]) as fh:
            run = json.load(fh)
        gate = load_gate()
        import collections
        c = collections.Counter()
        for f in run["files"]:
            q = quality(f, gate)
            c[q.split(":")[0]] += 1
            if q.startswith(("flag:", "report:", "exempt:")):
                print(f"{q}\t{f['id']}")
        print("summary:", dict(c))
        return 0
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
