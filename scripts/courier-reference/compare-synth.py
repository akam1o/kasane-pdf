"""The synthetic Courier cases: kasane A and C against Poppler, MuPDF and pdf.js.

    KASANE_A=… KASANE_C=… scripts/courier-reference/compare-synth.py OUT.json DPI PDF[:PAGE] ...

(numpy, Pillow, scikit-image; pdf.js from tools/pdfjs or PDFJS_DIST.)
Poppler is whichever `pdftoppm` is first on PATH: E1's wrappers for E1.

Per page and reference: SSIM of the page, SSIM of the ink region (the
union of both images' ink, padded), the ink ratio (ours / reference:
stroke weight), and per text band of the reference the mean |dx| of the
band's left and right ink edges and |dy| of its ink centroid (position and
width). Renders are greyscale at DPI; images are cropped to the common size.
"""
import json, os, subprocess, sys, tempfile
import numpy as np
from PIL import Image
from skimage.metrics import structural_similarity

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PDFJS = os.environ.get("PDFJS_DIST", os.path.join(ROOT, "tools", "pdfjs", "node_modules", "pdfjs-dist"))
# "B" is C, the candidate (the JSON's key).
KASANE = {"A": os.environ.get("KASANE_A", ""), "B": os.environ.get("KASANE_C", "")}
INK = 160


def render(who, pdf, page, dpi, tmp):
    base = os.path.join(tmp, who)
    if who in KASANE:
        subprocess.run([KASANE[who], "render", pdf, str(page), "--output", base + ".png", "--dpi", str(dpi)],
                       capture_output=True, check=True, timeout=120)
        return np.asarray(Image.open(base + ".png").convert("L"), dtype=np.float64)
    if who == "poppler":
        subprocess.run(["pdftoppm", "-r", str(dpi), "-gray", "-f", str(page), "-l", str(page), "-singlefile", pdf, base],
                       capture_output=True, check=True, timeout=120)
        return np.asarray(Image.open(base + ".pgm").convert("L"), dtype=np.float64)
    if who == "mupdf":
        subprocess.run(["mutool", "draw", "-q", "-r", str(dpi), "-c", "gray", "-o", base + ".pgm", pdf, str(page)],
                       capture_output=True, check=True, timeout=120)
        return np.asarray(Image.open(base + ".pgm").convert("L"), dtype=np.float64)
    subprocess.run(["node", f"{HERE}/pdfjs-render.mjs", PDFJS, pdf, str(page), str(dpi), base + ".ppm"],
                   capture_output=True, check=True, timeout=120)
    return np.asarray(Image.open(base + ".ppm").convert("L"), dtype=np.float64)


def bands(ref):
    rows = (ref < INK).any(axis=1)
    out, start = [], None
    for y, r in enumerate(list(rows) + [False]):
        if r and start is None:
            start = y
        elif not r and start is not None:
            if y - start >= 3:
                out.append((start, y))
            start = None
    return out


def metrics(ours, ref):
    h, w = min(ours.shape[0], ref.shape[0]), min(ours.shape[1], ref.shape[1])
    a, b = ours[:h, :w], ref[:h, :w]
    m = {"ssim": float(structural_similarity(a, b, data_range=255))}
    ink = (a < INK) | (b < INK)
    if not ink.any():
        return dict(m, tssim=None, ink_ratio=None, dx=None, dy=None)
    ys, xs = np.nonzero(ink)
    y0, y1 = max(ys.min() - 4, 0), min(ys.max() + 5, h)
    x0, x1 = max(xs.min() - 4, 0), min(xs.max() + 5, w)
    m["tssim"] = float(structural_similarity(a[y0:y1, x0:x1], b[y0:y1, x0:x1], data_range=255))
    ia, ib = (255 - a).sum(), (255 - b).sum()
    m["ink_ratio"] = float(ia / ib) if ib else None
    dx, dy = [], []
    for s, e in bands(b):
        ca, cb = (a[s:e] < INK), (b[s:e] < INK)
        if not ca.any():
            dx.append(float(w)); dy.append(float(e - s)); continue
        xa, xb = np.nonzero(ca.any(axis=0))[0], np.nonzero(cb.any(axis=0))[0]
        dx.append((abs(xa[0] - xb[0]) + abs(xa[-1] - xb[-1])) / 2)
        ya, yb = np.nonzero(ca)[0], np.nonzero(cb)[0]
        dy.append(abs(ya.mean() - yb.mean()))
    m["dx"] = float(np.mean(dx)) if dx else None
    m["dy"] = float(np.mean(dy)) if dy else None
    m["bands"] = len(dx)
    return m


def main():
    out, dpi = sys.argv[1], int(sys.argv[2])
    res = {}
    for spec in sys.argv[3:]:
        pdf, _, page = spec.partition(":")
        page = int(page or 1)
        key = f"{os.path.basename(pdf)}:{page}"
        with tempfile.TemporaryDirectory() as tmp:
            try:
                img = {w: render(w, pdf, page, dpi, tmp) for w in ("A", "B", "poppler", "mupdf", "pdfjs")}
            except subprocess.CalledProcessError as e:
                res[key] = {"error": f"render failed: {e.cmd[0]}"}
                continue
            r = {"AB_identical": bool(img["A"].shape == img["B"].shape and (img["A"] == img["B"]).all())}
            for ref in ("poppler", "mupdf", "pdfjs"):
                r[ref] = {v: metrics(img[v], img[ref]) for v in ("A", "B")}
            res[key] = r
        print(key, "identical" if r["AB_identical"] else
              " ".join(f"{ref}:{r[ref]['A']['tssim']:.3f}->{r[ref]['B']['tssim']:.3f}"
                       if r[ref]["A"]["tssim"] is not None else f"{ref}:-" for ref in ("poppler", "mupdf", "pdfjs")),
              flush=True)
    json.dump(res, open(out, "w"), indent=1)


if __name__ == "__main__":
    main()
