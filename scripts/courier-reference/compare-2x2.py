#!/usr/bin/env python3
"""The three deltas of the 2×2 Courier evaluation (docs/courier-reference.md).

    scripts/courier-reference/compare-2x2.py A_E0 A_E1 C_E0 C_E1 --json OUT [--public OUT2]

Each argument is a run-2x2.sh output directory. Reported, per delta —
A/E0 → A/E1 (the reference environment), A/E1 → C/E1 (the implementation
in the new reference), A/E0 → C/E0 (the implementation in today's):

- per strict-render page: SSIM and content SSIM better / worse / same
  (beyond the nightly's tolerances, 0.002 and 0.01), with the
  distribution of the changes;
- per file: the stage classes, text F1 (default and annotation-inclusive),
  crashes and timeouts, files or pages missing;
- the second-witness classes and agreements of the pages it re-checks.

--json keeps the corpus ids (private record); --public writes the same
without ids — counts and distributions only.
"""
import argparse, json, os, statistics, sys

SSIM_TOL, CSSIM_TOL = 0.002, 0.01
STAGES = ("parse", "recover", "render", "render_recovered", "text", "rewrite")


def load(d):
    run = json.load(open(os.path.join(d, "run", "latest.json")))
    att = json.load(open(os.path.join(d, "run", "attribution.json")))
    fonts = os.path.join(d, "reference-fonts.sha256")
    return {"impl": open(os.path.join(d, "implementation.sha")).read().strip(),
            "harness": open(os.path.join(d, "harness.sha")).read().strip(),
            "reference_fonts": open(fonts).read().strip(),
            "env": open(os.path.join(d, "environment")).read().strip(),
            "files": {f["id"]: f for f in run["files"]},
            "att": {(p["id"], p["page"]): p for p in att["pages"]}}


def pages(f):
    return {p["page"]: (p.get("ssim"), (p.get("quality") or {}).get("content_ssim"))
            for p in (f["metrics"].get("render_pages") or []) if isinstance(p.get("page"), int)}


def dist(xs):
    if not xs:
        return None
    return {"n": len(xs), "min": round(min(xs), 4), "median": round(statistics.median(xs), 4), "max": round(max(xs), 4)}


def delta(a, b):
    out = {"pages": {"ssim": {"better": [], "worse": [], "same": 0}, "content_ssim": {"better": [], "worse": [], "same": 0},
                     "missing": []},
           "files": {"class_changed": [], "text_f1_changed": [], "crash_or_timeout": [], "missing": []},
           "attribution": {"class_changed": [], "agreement_changed": [], "pages_added": [], "pages_removed": []}}
    for fid in sorted(set(a["files"]) | set(b["files"])):
        fa, fb = a["files"].get(fid), b["files"].get(fid)
        if fa is None or fb is None:
            out["files"]["missing"].append(fid)
            continue
        for s in STAGES:
            if (fa.get(s) or {}).get("class") != (fb.get(s) or {}).get("class"):
                out["files"]["class_changed"].append(f"{fid}:{s}")
        for k in ("text_char_f1", "text_char_f1_with_annots"):
            if fa["metrics"].get(k) != fb["metrics"].get(k):
                out["files"]["text_f1_changed"].append(f"{fid}:{k}")
        if any((fb.get(s) or {}).get("class") in ("crash", "timeout") for s in STAGES):
            out["files"]["crash_or_timeout"].append(fid)
        pa, pb = pages(fa), pages(fb)
        for pg in sorted(set(pa) | set(pb)):
            if pg not in pa or pg not in pb:
                out["pages"]["missing"].append(f"{fid}:{pg}")
                continue
            for i, (k, tol) in enumerate((("ssim", SSIM_TOL), ("content_ssim", CSSIM_TOL))):
                x, y = pa[pg][i], pb[pg][i]
                if x is None or y is None:
                    if x != y:
                        out["pages"]["missing"].append(f"{fid}:{pg}:{k}")
                    continue
                d = y - x
                if d > tol:
                    out["pages"][k]["better"].append((f"{fid}:{pg}", round(d, 4)))
                elif d < -tol:
                    out["pages"][k]["worse"].append((f"{fid}:{pg}", round(d, 4)))
                else:
                    out["pages"][k]["same"] += 1
    for key in sorted(set(a["att"]) | set(b["att"])):
        pa, pb = a["att"].get(key), b["att"].get(key)
        name = f"{key[0]}:{key[1]}"
        if pa is None:
            out["attribution"]["pages_added"].append(name)
        elif pb is None:
            out["attribution"]["pages_removed"].append(name)
        else:
            if pa.get("class") != pb.get("class"):
                out["attribution"]["class_changed"].append(f"{name}:{pa.get('class')}->{pb.get('class')}")
            if pa.get("agreement") != pb.get("agreement"):
                out["attribution"]["agreement_changed"].append(f"{name}:{pa.get('agreement')}->{pb.get('agreement')}")
    for k in ("ssim", "content_ssim"):
        for side in ("better", "worse"):
            out["pages"][k][side + "_dist"] = dist([abs(d) for _, d in out["pages"][k][side]])
    return out


def public(d):
    """`d` without ids: counts and distributions only."""
    if isinstance(d, dict):
        return {k: public(v) for k, v in d.items()}
    if isinstance(d, list):
        return len(d)
    return d


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("a_e0"); ap.add_argument("a_e1"); ap.add_argument("c_e0"); ap.add_argument("c_e1")
    ap.add_argument("--json", required=True)
    ap.add_argument("--public")
    a = ap.parse_args()
    runs = {k: load(getattr(a, k)) for k in ("a_e0", "a_e1", "c_e0", "c_e1")}
    if runs["a_e0"]["impl"] == runs["c_e0"]["impl"] \
            or runs["a_e0"]["impl"] != runs["a_e1"]["impl"] or runs["c_e0"]["impl"] != runs["c_e1"]["impl"] \
            or [runs[k]["env"] for k in ("a_e0", "a_e1", "c_e0", "c_e1")] != ["e0", "e1", "e0", "e1"]:
        print("compare-2x2: the four runs are not A/E0, A/E1, C/E0, C/E1 of two implementations")
        return 2
    # One harness; no E1 configuration in E0, the same one in both E1 cells.
    if len({r["harness"] for r in runs.values()}) != 1 or runs["a_e0"]["reference_fonts"] \
            or runs["c_e0"]["reference_fonts"] or not runs["a_e1"]["reference_fonts"] \
            or runs["a_e1"]["reference_fonts"] != runs["c_e1"]["reference_fonts"]:
        print("compare-2x2: the cells were not run with one harness and one E1 configuration")
        return 2
    doc = {"implementations": {"A": runs["a_e0"]["impl"], "C": runs["c_e0"]["impl"]},
           "harness": runs["a_e0"]["harness"], "e1_configuration": runs["a_e1"]["reference_fonts"].splitlines(),
           "reference_environment (A/E0 -> A/E1)": delta(runs["a_e0"], runs["a_e1"]),
           "implementation in E1 (A/E1 -> C/E1)": delta(runs["a_e1"], runs["c_e1"]),
           "implementation in E0 (A/E0 -> C/E0)": delta(runs["a_e0"], runs["c_e0"])}
    json.dump(doc, open(a.json, "w"), indent=1)
    if a.public:
        json.dump(public(doc), open(a.public, "w"), indent=1)
    for name, d in doc.items():
        if name == "implementations":
            continue
        p = d["pages"]
        print(f"{name}: SSIM better {len(p['ssim']['better'])} worse {len(p['ssim']['worse'])} same {p['ssim']['same']}; "
              f"content SSIM better {len(p['content_ssim']['better'])} worse {len(p['content_ssim']['worse'])}; "
              f"classes {len(d['files']['class_changed'])}, text F1 {len(d['files']['text_f1_changed'])}, "
              f"crash/timeout {len(d['files']['crash_or_timeout'])}, missing {len(d['files']['missing']) + len(p['missing'])}; "
              f"attribution class {len(d['attribution']['class_changed'])} agreement {len(d['attribution']['agreement_changed'])} "
              f"+{len(d['attribution']['pages_added'])}/-{len(d['attribution']['pages_removed'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
