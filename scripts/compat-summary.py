#!/usr/bin/env python3
"""The publishable summary of a compatibility analysis
(docs/compatibility-report.md): counts only — no file ids, no text, no
paths, nothing from the corpus files themselves.

    scripts/compat-summary.py --run RUN.json --diag DIAG.json --attribution ATTR.json \
        --unicode UNICODE.json --baseline compat/baselines/corpus-full.json --out OUT.json

Inputs (all gitignored, in compat/reports/): the runner report, the
render-blocker diagnosis (scripts/diag-render-blockers.py), the
low-similarity attribution (scripts/compat-attribute.py) and the page-1
Unicode sources (`kasane-pdf text --format json` per file). The output
holds the stage and verdict counts, the SSIM distribution (file and
page level), the quality-gate classes, the blocker root causes with
their recovery outcome, the attribution classes, the Unicode sources,
feature usage against failures, and the differences from the committed
baseline (stage by stage, file counts only).
"""
import argparse
import collections
import json
import math
import statistics


def q(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, max(0, math.ceil(p * len(v)) - 1))] if v else None


def dist(v):
    return {"n": len(v), "median": round(statistics.median(v), 4) if v else None,
            **{f"p{int(p * 100)}": round(q(v, p), 4) if v else None for p in (0.05, 0.25, 0.75, 0.95)},
            "below_0_95": sum(x < 0.95 for x in v), "below_0_90": sum(x < 0.9 for x in v),
            "below_0_80": sum(x < 0.8 for x in v)}


def rated(f, stage):
    """The stage's class as the runner rates it (analysis::rated_class):
    after a failed strict parse the runner reads the recovered document,
    and its render / text / rewrite outcomes are not strict ones (skip).
    `recover` and `render_recovered` are reported as recorded."""
    c = (f.get(stage) or {}).get("class")
    if f.get("recovered") and stage in ("render", "text", "rewrite"):
        return "skip"
    return c


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    for k in ("run", "diag", "attribution", "unicode", "baseline", "out"):
        ap.add_argument(f"--{k}", required=True)
    a = ap.parse_args()
    run = json.load(open(a.run))
    files = run["files"]
    C = collections.Counter
    exp_pass = [f for f in files if f["expect"] == "pass"]
    rendered = [f for f in exp_pass if rated(f, "render") == "pass"]
    out = {"schema_version": 1, "files": len(files)}
    out["verdict"] = dict(C(f["verdict"] for f in files))
    out["expect"] = dict(C(f["expect"] for f in files))
    out["stages_all"] = {s: dict(C(rated(f, s) for f in files))
                         for s in ("parse", "recover", "render", "render_recovered", "text", "rewrite")}
    out["stages_expect_pass"] = {s: dict(C(rated(f, s) for f in exp_pass))
                                 for s in ("parse", "render", "render_recovered", "text", "rewrite")}
    out["ssim_file"] = dist([f["metrics"]["render_ssim"] for f in rendered if f["metrics"].get("render_ssim") is not None])
    pages = [p["ssim"] for f in rendered for p in f["metrics"].get("render_pages") or []
             if isinstance(p, dict) and p.get("ssim") is not None]
    out["ssim_page"] = dist(pages)
    strict_text = [f for f in exp_pass if rated(f, "text") == "pass"]
    f1 = [f["metrics"]["text_char_f1"] for f in strict_text if f["metrics"].get("text_char_f1") is not None]
    out["text_f1"] = dist(f1)
    out["text_f1_with_annotations_recovers"] = sum(
        1 for f in strict_text if f["metrics"].get("text_char_f1") is not None and f["metrics"]["text_char_f1"] < 0.9
        and f["metrics"].get("text_char_f1_with_annots") is not None and f["metrics"]["text_char_f1_with_annots"] >= 0.9)
    wa = [f["metrics"].get("text_char_f1_with_annots") for f in strict_text if f["metrics"].get("text_char_f1") is not None]
    out["text_f1_with_annotations"] = {
        "scored": sum(x is not None for x in wa), "missing": sum(x is None for x in wa),
        "at_least_0_9": sum(x is not None and x >= 0.9 for x in wa),
        "falls_below_0_9": sum(1 for f in strict_text if f["metrics"].get("text_char_f1") is not None
                               and f["metrics"]["text_char_f1"] >= 0.9
                               and f["metrics"].get("text_char_f1_with_annots") is not None
                               and f["metrics"]["text_char_f1_with_annots"] < 0.9)}
    out["text_empty_with_reference_text"] = sum(
        1 for f in strict_text if f["metrics"].get("text_char_f1") is not None and (f["metrics"].get("text_chars_ours") or 0) == 0
        and (f["metrics"].get("text_chars_ref") or 0) > 0)
    # Feature usage among rendered expect:pass files, and among those below SSIM 0.95.
    low = [f for f in rendered if f["metrics"].get("render_ssim") is not None and f["metrics"]["render_ssim"] < 0.95]
    use = collections.defaultdict(lambda: [0, 0])
    for f in rendered:
        tags = set((f.get("profile") or {}).get("font_kinds") or []) | set((f.get("profile") or {}).get("image_filters") or []) \
            | set(f.get("features") or [])
        for t in tags:
            use[t][0] += 1
            use[t][1] += f in low
    out["feature_use_rendered"] = {t: {"files": v[0], "below_0_95": v[1]} for t, v in sorted(use.items())}
    diag = json.load(open(a.diag))["files"]
    byid = {f["id"]: f for f in files}
    rc = collections.defaultdict(C)
    for r in diag:
        rc[r["root_cause"]][(byid[r["id"]].get("render_recovered") or {}).get("class") or "none"] += 1
    out["blockers_expect_pass"] = {k: dict(v) for k, v in sorted(rc.items(), key=lambda kv: -sum(kv[1].values()))}
    att = json.load(open(a.attribution))["files"]
    out["attribution_below_0_95"] = dict(C(r["class"] for r in att))
    uni = json.load(open(a.unicode))
    srcs = C()
    for r in uni:
        srcs.update(r["sources"])
    out["unicode_sources_page1"] = dict(srcs)
    out["unicode_files_with_chars"] = sum(1 for r in uni if r["chars"])
    out["unicode_files_majority_unresolved"] = sum(1 for r in uni if r["chars"] and r["sources"].get("Unresolved", 0) / r["chars"] > 0.5)
    out["unicode_files_some_unresolved"] = sum(1 for r in uni if r["chars"] and 0 < r["sources"].get("Unresolved", 0) / r["chars"] <= 0.5)
    base = json.load(open(a.baseline))["files"]
    diff = {}
    for s in ("parse", "render", "text", "rewrite"):
        moves = C()
        for fid, b in base.items():
            n = byid.get(fid)
            if n is None:
                moves["missing"] += 1
                continue
            o, w = b["stages"].get(s), (n.get(s) or {}).get("class")
            if o != w:
                moves[f"{o}->{w}"] += 1
        diff[s] = dict(moves)
    diff["verdict"] = dict(C(f"{base[fid]['verdict']}->{byid[fid]['verdict']}" for fid in base
                             if fid in byid and base[fid]["verdict"] != byid[fid]["verdict"]))
    out["vs_baseline"] = diff
    json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)
    open(a.out, "a").write("\n")
    print(json.dumps({k: out[k] for k in ("verdict", "ssim_file", "attribution_below_0_95", "vs_baseline")}, indent=1))


if __name__ == "__main__":
    main()
