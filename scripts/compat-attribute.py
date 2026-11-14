#!/usr/bin/env python3
"""Attribute the low-similarity pages of a compatibility run: is the
difference kasane's, an annotation appearance, or the reference
renderers disagreeing with each other?

    .venv-corpus/bin/python scripts/compat-attribute.py <runner report.json> <kasane-pdf> \
        --out compat/reports/attribution-<run>.json [--threshold 0.95] [--summary OUT.json]

For every `expect: pass` file that rendered and whose worst page's SSIM
against Poppler is below the threshold, that page is rendered again at
72 dpi, greyscale, by kasane, Poppler (`pdftoppm`), Poppler with
annotations hidden (`-hide-annotations`) and MuPDF (`mutool draw`), and
compared pairwise: `dpx`, the share of pixels whose grey level differs by
more than 48, and the ink (non-white share) of each. The classes, first
match wins:

  size          the renders' sizes differ (page box handling)
  blank         kasane drew (almost) nothing where Poppler drew ink
  annotation_layer
                with /Annots removed from the file (every renderer then
                draws no annotation), kasane and Poppler agree (dpx falls
                below a quarter): the difference is in the annotation
                layer — appearance streams, widgets, flags
  annotation    kasane is much closer to Poppler without annotations
                (dpx halves): an annotation appearance differs
  reference_kasane_mupdf
                Poppler and MuPDF differ from each other, and kasane is
                much closer to MuPDF (under half their difference)
  reference_three_way
                Poppler and MuPDF differ from each other by more than
                kasane differs from MuPDF, but kasane is not close to
                either: the cause is not attributable
  missing_ink   kasane has much less ink than both references
  extra_ink     kasane has much more ink than both references
  kasane        kasane differs from both references, which agree

--resolution-check repeats the three pairwise comparisons from 150 and
300 dpi renders, box-downsampled to the 72 dpi grid: a difference that
vanishes at higher resolution is rasterisation (sampling, anti-aliasing,
snapping), not content; one that stays is.

The per-file results (ids, classes, measures) go to --out, which belongs
in the gitignored compat/reports/; --summary writes counts only (per
class, and per class × font kind / image filter of the profile), the
form that may be published. Requires Pillow (.venv-corpus).
"""
import argparse
import collections
import json
import os
import subprocess
import sys
import tempfile

from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DELTA = 48


def corpus_path(fid):
    base = os.environ.get("KASANE_CORPUS_DIR") or os.path.join(ROOT, "compat", "corpus")
    for name in (fid + ".pdf", fid):
        p = os.path.join(base, name)
        if os.path.isfile(p):
            return p
    return None


def grey(path):
    return Image.open(path).convert("L")


def without_annotations(pdf, tmp):
    """The same file with every /Annots key renamed (same length, so
    fix-qdf repairs the offsets): no renderer draws an annotation. None
    when qpdf cannot rewrite it."""
    plain, fixed = os.path.join(tmp, "plain.pdf"), os.path.join(tmp, "noannots.pdf")
    q = subprocess.run(["qpdf", "--qdf", "--object-streams=disable", pdf, plain], capture_output=True, timeout=120)
    if q.returncode not in (0, 3) or not os.path.exists(plain):
        return None
    data = open(plain, "rb").read().replace(b"/Annots", b"/XAnnot")
    open(plain, "wb").write(data)
    fx = subprocess.run(["fix-qdf", plain], capture_output=True, timeout=120)
    if fx.returncode != 0:
        return None
    open(fixed, "wb").write(fx.stdout)
    return fixed


def render_all(kasane, pdf, page, tmp):
    out = {}
    k = os.path.join(tmp, "k.png")
    r = subprocess.run([kasane, "render", pdf, str(page), "--dpi", "72", "--output", k], capture_output=True, timeout=120)
    out["kasane"] = grey(k) if r.returncode == 0 and os.path.exists(k) else None
    for name, extra in (("poppler", []), ("poppler_noannot", ["-hide-annotations"])):
        base = os.path.join(tmp, name)
        subprocess.run(["pdftoppm", "-r", "72", "-gray", "-png", "-f", str(page), "-l", str(page), "-singlefile",
                        *extra, pdf, base], capture_output=True, timeout=120)
        out[name] = grey(base + ".png") if os.path.exists(base + ".png") else None
    m = os.path.join(tmp, "m.png")
    subprocess.run(["mutool", "draw", "-q", "-r", "72", "-c", "gray", "-o", m, pdf, str(page)], capture_output=True,
                   timeout=120)
    out["mupdf"] = grey(m) if os.path.exists(m) else None
    na = without_annotations(pdf, tmp)
    if na:
        for name, argv in (("kasane_na", [kasane, "render", na, str(page), "--dpi", "72", "--output",
                                           os.path.join(tmp, "kna.png")]),
                           ("poppler_na", ["pdftoppm", "-r", "72", "-gray", "-png", "-f", str(page), "-l", str(page),
                                           "-singlefile", na, os.path.join(tmp, "pna")]),
                           ("mupdf_na", ["mutool", "draw", "-q", "-r", "72", "-c", "gray", "-o",
                                         os.path.join(tmp, "mna.png"), na, str(page)])):
            subprocess.run(argv, capture_output=True, timeout=120)
        for name, f in (("kasane_na", "kna.png"), ("poppler_na", "pna.png"), ("mupdf_na", "mna.png")):
            out[name] = grey(os.path.join(tmp, f)) if os.path.exists(os.path.join(tmp, f)) else None
    return out


def ink(img):
    h = img.histogram()
    return sum(h[:250]) / max(1, img.width * img.height)


def dpx(a, b):
    if a.size != b.size:
        return None
    from PIL import ImageChops
    d = ImageChops.difference(a, b).histogram()
    return sum(d[DELTA + 1:]) / max(1, a.width * a.height)


def classify(r):
    k, p, pn, m = r["kasane"], r["poppler"], r["poppler_noannot"], r["mupdf"]
    if k is None or p is None:
        return "error", {}
    meas = {"ink_kasane": ink(k), "ink_poppler": ink(p)}
    if k.size != p.size:
        return "size", meas
    meas["dpx_kp"] = dpx(k, p)
    if pn is not None:
        meas["dpx_kpn"] = dpx(k, pn)
    if m is not None and m.size == k.size:
        meas["dpx_km"] = dpx(k, m)
        meas["dpx_pm"] = dpx(p, m)
        meas["ink_mupdf"] = ink(m)
    kn, pn2, mn = r.get("kasane_na"), r.get("poppler_na"), r.get("mupdf_na")
    if kn is not None and pn2 is not None and kn.size == pn2.size:
        meas["dpx_kp_na"] = dpx(kn, pn2)
        if mn is not None and mn.size == kn.size:
            meas["dpx_km_na"] = dpx(kn, mn)
            meas["dpx_pm_na"] = dpx(pn2, mn)
    if meas["ink_poppler"] > 0.01 and meas["ink_kasane"] < 0.1 * meas["ink_poppler"]:
        return "blank", meas
    # Without annotations kasane and Poppler (nearly) agree: what differs
    # is the annotation layer (widgets, appearances) — whoever is right.
    if meas.get("dpx_kp_na") is not None and meas["dpx_kp"] > 0.002 and meas["dpx_kp_na"] < 0.25 * meas["dpx_kp"]:
        return "annotation_layer", meas
    if meas.get("dpx_kpn") is not None and meas["dpx_kp"] > 0.002 and meas["dpx_kpn"] < 0.5 * meas["dpx_kp"]:
        return "annotation", meas
    if meas.get("dpx_pm") is not None and meas["dpx_pm"] > meas["dpx_km"] and meas["dpx_pm"] > 0.002:
        # The references disagree. kasane with MuPDF (much closer to it
        # than Poppler is), or all three apart.
        return ("reference_kasane_mupdf" if meas["dpx_km"] < 0.5 * meas["dpx_pm"] else "reference_three_way"), meas
    refs = [x for x in (meas.get("ink_poppler"), meas.get("ink_mupdf")) if x is not None]
    if refs and meas["ink_kasane"] < 0.7 * min(refs):
        return "missing_ink", meas
    if refs and meas["ink_kasane"] > 1.3 * max(refs) and meas["ink_kasane"] - max(refs) > 0.005:
        return "extra_ink", meas
    return "kasane", meas


def resolution_check(kasane, pdf, page):
    """{dpi: (kasane-poppler, kasane-mupdf, poppler-mupdf)} at 72, 150,
    300 dpi, compared on the 72 dpi grid."""
    out = {}
    with tempfile.TemporaryDirectory() as t:
        for dpi in (72, 150, 300):
            k, b, m = os.path.join(t, f"k{dpi}.png"), os.path.join(t, f"p{dpi}"), os.path.join(t, f"m{dpi}.png")
            subprocess.run([kasane, "render", pdf, str(page), "--dpi", str(dpi), "--output", k], capture_output=True,
                           timeout=300)
            subprocess.run(["pdftoppm", "-r", str(dpi), "-gray", "-png", "-f", str(page), "-l", str(page),
                            "-singlefile", pdf, b], capture_output=True, timeout=300)
            subprocess.run(["mutool", "draw", "-q", "-r", str(dpi), "-c", "gray", "-o", m, pdf, str(page)],
                           capture_output=True, timeout=300)
            if not all(os.path.exists(x) for x in (k, b + ".png", m)):
                out[dpi] = None
                continue
            kk, pp, mm = grey(k), grey(b + ".png"), grey(m)
            if dpi != 72:
                w, h = max(1, round(pp.width * 72 / dpi)), max(1, round(pp.height * 72 / dpi))
                kk, pp, mm = (x.resize((w, h), Image.BOX) for x in (kk, pp, mm))
            out[dpi] = (dpx(kk, pp), dpx(kk, mm), dpx(pp, mm))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("report")
    ap.add_argument("kasane")
    ap.add_argument("--out", required=True)
    ap.add_argument("--summary")
    ap.add_argument("--threshold", type=float, default=0.95)
    ap.add_argument("--resolution-check", action="store_true")
    a = ap.parse_args()
    run = json.load(open(a.report))
    rows = []
    for f in run["files"]:
        # Strict renders only: after a failed strict parse the runner's
        # render is of the recovered document.
        if f["expect"] != "pass" or f.get("recovered") or (f.get("render") or {}).get("class") != "pass":
            continue
        pages = [p for p in (f["metrics"].get("render_pages") or []) if isinstance(p, dict) and p.get("ssim") is not None]
        if not pages:
            continue
        worst = min(pages, key=lambda p: p["ssim"])
        if worst["ssim"] >= a.threshold:
            continue
        pdf = corpus_path(f["id"])
        if pdf is None:
            rows.append({"id": f["id"], "class": "not_fetched"})
            continue
        with tempfile.TemporaryDirectory() as tmp:
            try:
                cls, meas = classify(render_all(a.kasane, pdf, worst["page"], tmp))
            except (subprocess.TimeoutExpired, OSError):
                cls, meas = "error", {}
        prof = f.get("profile") or {}
        rows.append({"id": f["id"], "category": f.get("category"), "page": worst["page"], "ssim": worst["ssim"],
                     "class": cls, **{k: round(v, 4) for k, v in meas.items() if v is not None},
                     "font_kinds": prof.get("font_kinds") or [], "image_filters": prof.get("image_filters") or [],
                     "features": f.get("features") or [], "font_substitutions": len(f["metrics"].get(
                         "render_font_substitutions") or []), "text_f1": f["metrics"].get("text_char_f1")})
        if a.resolution_check and cls not in ("error", "size"):
            rows[-1]["resolution"] = {str(k): v for k, v in resolution_check(a.kasane, pdf, worst["page"]).items()}
    with open(a.out, "w") as fh:
        json.dump({"source": os.path.basename(a.report), "threshold": a.threshold, "files": rows}, fh, indent=1)
    by = collections.Counter(r["class"] for r in rows)
    print(f"compat-attribute: {len(rows)} files below SSIM {a.threshold}: " + ", ".join(f"{k} {v}" for k, v in by.most_common()))
    if a.summary:
        cross = collections.defaultdict(collections.Counter)
        for r in rows:
            for t in r.get("font_kinds", []) + r.get("image_filters", []) + r.get("features", []):
                cross[r["class"]][t] += 1
        res = {}
        for c in by:
            for dpi in ("72", "150", "300"):
                v = [r["resolution"][dpi] for r in rows if r["class"] == c and r.get("resolution") and r["resolution"].get(dpi)
                     and None not in r["resolution"][dpi]]
                if v:
                    res.setdefault(c, {})[dpi] = {"n": len(v), **{name: round(sorted(x[i] for x in v)[len(v) // 2], 4)
                                                                   for i, name in enumerate(("kasane_poppler", "kasane_mupdf",
                                                                                             "poppler_mupdf"))}}
        json.dump({"threshold": a.threshold, "files": len(rows), "classes": dict(by), "resolution_medians": res,
                   "by_class": {c: dict(v) for c, v in cross.items()}}, open(a.summary, "w"), indent=1, sort_keys=True)


if __name__ == "__main__":
    sys.exit(main())
