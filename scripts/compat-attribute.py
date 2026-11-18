#!/usr/bin/env python3
"""Second witness and 300 dpi re-check for the low-similarity pages of a
compatibility run (M1-a, M1-b; docs/compat-evaluation.md).

    compat-attribute.py RUN.json KASANE --out PAGES.json [--summary SUMMARY.json]
        [--threshold 0.95] [--max-pages-per-file N] [--max-pages-total 1000]
        [--timeout 120] [--jobs 4] [--features FEATURES] [--commit SHA]

For every page of an `expect: pass` file whose strict render passed and
whose SSIM against Poppler (the runner's measurement) is below the
threshold — all of them (M2): a per-file cap made the measured set
depend on the pages' ranks, so a page that improved could push a known
one into it — the page is rendered again by kasane (the KASANE binary,
built as the runner was), Poppler (`pdftoppm -cropbox`, the runner's
page box) and MuPDF (`mutool draw`), in RGB, converted to grey by one
formula (ITU-R 601 luma, Pillow) — the renderers' own greyscale modes
convert colour differently, which alone made MuPDF differ from the
others on coloured pages — and compared pairwise
on the 72 dpi grid with two measures:

  dpx  share of pixels whose grey levels differ by more than 48
  mad  mean absolute grey difference (0–255): broad tone differences
       that stay under 48 everywhere

At 72 dpi the page gets a descriptive class (first match wins; kasane's
own defect signals first, so that a disagreement between the
references cannot hide them): blank, missing_ink, extra_ink,
annotation_layer (with /Annots removed from the file kasane and Poppler
agree), reference_kasane_mupdf, reference_three_way, kasane.

The verdict comes from 300 dpi renders box-downsampled to the 72 dpi
grid. kasane is *within tolerance* of a witness when dpx < 0.5 % and
mad < 2 (TOLERANCE). The page's `agreement`:

  both            within tolerance of Poppler and of MuPDF
  poppler         of Poppler only
  mupdf           of MuPDF only
  neither         of neither: the page is *open*
  not_comparable  a render failed, timed out, produced nothing, or the
                  sizes differ; `reason` says which (never counted as
                  agreement)

and `references_agree` (Poppler within tolerance of MuPDF; None when
not comparable). Agreement with a witness is an observation, not a
proof that the 72 dpi difference was rasterisation alone; neither is
agreement between the references a proof that they are right.

PAGES.json (per page: id, page, measures, class, agreement) belongs in
the gitignored compat/reports/ or a CI artifact; --summary writes counts
only, the form that may be published. Both record the environment
(scripts/compat_env.py). Renderers can be replaced through
KASANE_ATTR_PDFTOPPM, KASANE_ATTR_MUTOOL, KASANE_ATTR_QPDF and
KASANE_ATTR_FIXQDF (tests). Requires Pillow.
"""
import argparse
import collections
import concurrent.futures
import json
import math
import os
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compat_env  # noqa: E402
from compat_verdict import AGREEMENTS, TOLERANCE, finite, verdict, within  # noqa: E402,F401

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCHEMA = 2
DELTA = 48


def tool(name, default):
    return os.environ.get(f"KASANE_ATTR_{name}") or default


def corpus_path(fid):
    base = os.environ.get("KASANE_CORPUS_DIR") or os.path.join(ROOT, "compat", "corpus")
    for name in (fid + ".pdf", fid):
        p = os.path.join(base, name)
        if os.path.isfile(p):
            return p
    return None


# ---- decisions (pure: measures in, verdict out) -------------------------

def classify72(m):
    """Descriptive class from the 72 dpi measures (see the module doc)."""
    need = ("ink_kasane", "ink_poppler", "dpx_kp")
    if not all(finite(m.get(k)) for k in need):
        return "not_comparable"
    if m["ink_poppler"] > 0.01 and m["ink_kasane"] < 0.1 * m["ink_poppler"]:
        return "blank"
    refs = [m[k] for k in ("ink_poppler", "ink_mupdf") if finite(m.get(k))]
    if refs and m["ink_kasane"] < 0.7 * min(refs):
        return "missing_ink"
    if refs and m["ink_kasane"] > 1.3 * max(refs) and m["ink_kasane"] - max(refs) > 0.005:
        return "extra_ink"
    if finite(m.get("dpx_kp_na")) and m["dpx_kp"] > 0.002 and m["dpx_kp_na"] < 0.25 * m["dpx_kp"]:
        return "annotation_layer"
    if finite(m.get("dpx_pm"), m.get("dpx_km")) and m["dpx_pm"] > m["dpx_km"] and m["dpx_pm"] > 0.002:
        return "reference_kasane_mupdf" if m["dpx_km"] < 0.5 * m["dpx_pm"] else "reference_three_way"
    return "kasane"


# ---- measuring -----------------------------------------------------------

def image():
    from PIL import Image  # Pillow: imported here so the decisions test without it
    return Image


def grey(path):
    return image().open(path).convert("L")


def ink(img):
    return sum(img.histogram()[:250]) / max(1, img.width * img.height)


def dpx(a, b):
    from PIL import ImageChops
    return sum(ImageChops.difference(a, b).histogram()[DELTA + 1:]) / max(1, a.width * a.height)


def mad(a, b):
    from PIL import ImageChops
    h = ImageChops.difference(a, b).histogram()
    return sum(i * n for i, n in enumerate(h)) / max(1, a.width * a.height)


class NotComparable(Exception):
    pass


def run(argv, what, timeout):
    try:
        r = subprocess.run(argv, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise NotComparable(f"timeout:{what}")
    except OSError:
        raise NotComparable(f"not_runnable:{what}")
    if r.returncode != 0:
        raise NotComparable(f"failed:{what}")


def render(who, kasane, pdf, page, dpi, tmp, timeout):
    """Render one page greyscale; raise NotComparable on any failure."""
    base = os.path.join(tmp, f"{who}-{dpi}")
    if who.startswith("kasane"):
        out = base + ".png"
        run([kasane, "render", pdf, str(page), "--dpi", str(dpi), "--output", out], who, timeout)
    elif who.startswith("poppler"):
        out = base + ".png"
        run([tool("PDFTOPPM", "pdftoppm"), "-r", str(dpi), "-png", "-f", str(page), "-l", str(page),
             "-singlefile", "-cropbox", pdf, base], who, timeout)
    else:
        out = base + ".png"
        run([tool("MUTOOL", "mutool"), "draw", "-q", "-r", str(dpi), "-c", "rgb", "-o", out, pdf, str(page)], who,
            timeout)
    if not os.path.exists(out) or os.path.getsize(out) == 0:
        raise NotComparable(f"no_output:{who}")
    try:
        return grey(out)
    except Exception:
        raise NotComparable(f"unreadable:{who}")


def without_annotations(pdf, tmp, timeout):
    """The file with every /Annots key renamed (same length; fix-qdf
    repairs the offsets): no renderer draws an annotation. None when
    qpdf cannot rewrite it."""
    plain, fixed = os.path.join(tmp, "plain.pdf"), os.path.join(tmp, "noannots.pdf")
    try:
        q = subprocess.run([tool("QPDF", "qpdf"), "--qdf", "--object-streams=disable", pdf, plain],
                           capture_output=True, timeout=timeout)
        if q.returncode not in (0, 3) or not os.path.exists(plain):
            return None
        with open(plain, "rb") as fh:
            data = fh.read().replace(b"/Annots", b"/XAnnot")
        with open(plain, "wb") as fh:
            fh.write(data)
        fx = subprocess.run([tool("FIXQDF", "fix-qdf"), plain], capture_output=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if fx.returncode != 0 or not fx.stdout:
        return None
    with open(fixed, "wb") as fh:
        fh.write(fx.stdout)
    return fixed


# Renderers round a page's pixel size differently (kasane rounds up,
# Poppler and MuPDF to nearest): up to this many pixels per axis the
# renders are cropped to their common size; beyond it they are not
# comparable.
SIZE_SLACK_PX = 1


def common(imgs, what):
    w, h = min(i.width for i in imgs), min(i.height for i in imgs)
    if max(i.width for i in imgs) - w > SIZE_SLACK_PX or max(i.height for i in imgs) - h > SIZE_SLACK_PX:
        raise NotComparable(what)
    return [i if i.size == (w, h) else i.crop((0, 0, w, h)) for i in imgs]


def pairwise(k, p, m):
    k, p, m = common([k, p, m], "size_mismatch")
    return {"dpx_kp": dpx(k, p), "dpx_km": dpx(k, m), "dpx_pm": dpx(p, m),
            "mad_kp": mad(k, p), "mad_km": mad(k, m), "mad_pm": mad(p, m)}


def measure_page(kasane, pdf, page, timeout):
    """(measures72, r300, reason): reason is None, or why the page is not
    comparable (then r300 is None)."""
    m72, reason, r300 = {}, None, None
    with tempfile.TemporaryDirectory() as tmp:
        try:
            k, p, m = (render(w, kasane, pdf, page, 72, tmp, timeout) for w in ("kasane", "poppler", "mupdf"))
            m72 = {"ink_kasane": ink(k), "ink_poppler": ink(p), "ink_mupdf": ink(m), **pairwise(k, p, m)}
            na = without_annotations(pdf, tmp, timeout)
            if na:
                try:
                    kn = render("kasane_na", kasane, na, page, 72, tmp, timeout)
                    pn = render("poppler_na", kasane, na, page, 72, tmp, timeout)
                    kn, pn = common([kn, pn], "size_mismatch")
                    m72["dpx_kp_na"] = dpx(kn, pn)
                except NotComparable:
                    pass  # descriptive only; the verdict does not use it
            hi = [render(w, kasane, pdf, page, 300, tmp, timeout) for w in ("kasane", "poppler", "mupdf")]
            hi = common(hi, "size_mismatch_300")
            w, h = max(1, round(hi[1].width * 72 / 300)), max(1, round(hi[1].height * 72 / 300))
            Image = image()
            r300 = pairwise(*(x.resize((w, h), Image.BOX) for x in hi))
        except NotComparable as e:
            reason, r300 = str(e), None
    return m72, r300, reason


def pages_to_check(run_report, threshold, max_pages=None):
    """(file record, page, ssim) of every strict-render page below the
    threshold, worst first; at most max_pages per file when given (an
    older selection, kept for comparisons)."""
    out = []
    for f in run_report["files"]:
        if f.get("expect") != "pass" or f.get("recovered") or (f.get("render") or {}).get("class") != "pass":
            continue
        pages = [p for p in ((f.get("metrics") or {}).get("render_pages") or [])
                 if isinstance(p, dict) and finite(p.get("ssim")) and p["ssim"] < threshold]
        pages = sorted(pages, key=lambda p: (p["ssim"], p["page"]))
        for p in (pages if max_pages is None else pages[:max_pages]):
            out.append((f, p["page"], p["ssim"]))
    return out


def within_budget(items, total):
    """The items to measure and how many are left out: when more pages
    than `total` are below the threshold, the worst `total` (by SSIM,
    then case and page), and the rest is recorded as `truncated` — the
    regression check fails on it, so a budget never passes as a
    complete measurement."""
    if total is None or len(items) <= total:
        return items, 0
    keep = sorted(items, key=lambda it: (it[2], it[0]["id"], it[1]))[:total]
    return keep, len(items) - total


def assess(item, kasane, timeout):
    f, page, ssim = item
    pdf = corpus_path(f["id"])
    if pdf is None:
        m72, r300, reason = {}, None, "not_fetched"
    else:
        m72, r300, reason = measure_page(kasane, pdf, page, timeout)
    # Quantized before the verdict: the gate recomputes it from the
    # recorded values, which must give the same answer.
    rnd = lambda d: {k: round(v, 5) for k, v in d.items() if finite(v)}
    r300 = rnd(r300) if r300 else None
    agreement, refs = verdict(r300)
    if agreement == "not_comparable" and reason is None:
        reason = "invalid_measure"
    return {"id": f["id"], "page": page, "ssim": round(ssim, 5),
            "class": classify72(m72) if reason is None or m72 else "not_comparable",
            "agreement": agreement, "references_agree": refs, "reason": reason,
            "m72": rnd(m72), "r300": r300}


def summary(rows, env, threshold, files):
    by = collections.Counter(r["agreement"] for r in rows)
    cross = collections.defaultdict(collections.Counter)
    for r in rows:
        cross[r["class"]][r["agreement"]] += 1
    return {
        "schema": SCHEMA, "env": env, "threshold": threshold, "tolerance": TOLERANCE,
        "files": files, "pages": len(rows),
        "agreement": {a: by.get(a, 0) for a in AGREEMENTS},
        "references_disagree": sum(1 for r in rows if r["references_agree"] is False),
        "not_comparable_reasons": dict(collections.Counter(r["reason"].split(":")[0] for r in rows if r["reason"])),
        "class_by_agreement": {c: dict(v) for c, v in sorted(cross.items())},
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("report")
    ap.add_argument("kasane")
    ap.add_argument("--out", required=True)
    ap.add_argument("--summary")
    ap.add_argument("--threshold", type=float, default=0.95)
    ap.add_argument("--max-pages-per-file", type=int, default=None,
                    help="cap per file (default: every page below the threshold)")
    ap.add_argument("--max-pages-total", type=int, default=1000,
                    help="pages measured at most; beyond it the result is recorded as truncated")
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--features", default="")
    ap.add_argument("--commit")
    ap.add_argument("--resolution-check", action="store_true", help="always on; accepted for older commands")
    a = ap.parse_args(argv)
    with open(a.report) as fh:
        report = json.load(fh)
    items, truncated = within_budget(pages_to_check(report, a.threshold, a.max_pages_per_file),
                                     a.max_pages_total)
    env = compat_env.environment(commit=a.commit, features=a.features, pdftoppm=tool("PDFTOPPM", "pdftoppm"),
                                 mutool=tool("MUTOOL", "mutool"), qpdf=tool("QPDF", "qpdf"))
    with concurrent.futures.ThreadPoolExecutor(max(1, a.jobs)) as ex:
        rows = list(ex.map(lambda it: assess(it, a.kasane, a.timeout), items))
    files = len({r["id"] for r in rows})
    doc = {"schema": SCHEMA, "env": env, "threshold": a.threshold, "tolerance": TOLERANCE,
           "max_pages_per_file": a.max_pages_per_file, "truncated": truncated,
           "source": os.path.basename(a.report), "pages": rows}
    with open(a.out, "w") as fh:
        json.dump(doc, fh, indent=1, sort_keys=True)
    s = summary(rows, env, a.threshold, files)
    s["truncated"] = truncated
    if a.summary:
        with open(a.summary, "w") as fh:
            json.dump(s, fh, indent=1, sort_keys=True)
            fh.write("\n")
    print(f"compat-attribute: {len(rows)} pages of {files} files below SSIM {a.threshold}: "
          + ", ".join(f"{k} {v}" for k, v in s["agreement"].items())
          + (f"; {truncated} more not measured (--max-pages-total)" if truncated else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
