#!/usr/bin/env python3
"""Minimal synthetic probes for the compatibility roadmap
(docs/compatibility-roadmap.md): each isolates one behaviour the corpus
analysis points at, and records what kasane, Poppler and MuPDF do with
it. They are measurements, not assertions — the roadmap's tasks turn
the ones they fix into tests.

    scripts/compat-probes.py <kasane-pdf> [--json OUT]

Probes (all synthetic, built with scripts/verify-redact.py's helpers):

  type3-glyph-names       Type 3 font, glyph names /A /B /C, no /ToUnicode
  truetype-symbolic       embedded TrueType, symbolic flag, no /Encoding,
                          no /ToUnicode (the program's own cmap)
  freetext-appearance     page text plus a FreeText annotation whose
                          appearance draws text (default text extraction)
  operand-count           an `re` with three operands in the content
  form-no-bbox            a form XObject without /BBox
"""
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def vr():
    spec = importlib.util.spec_from_file_location("verify_redact", os.path.join(ROOT, "scripts", "verify-redact.py"))
    mod = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, [sys.argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


def probes(v):
    pub = b"BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET"
    t3 = (b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] "
          b"/CharProcs << /A 9 0 R /B 9 0 R /C 9 0 R >> /Encoding << /Type /Encoding /Differences [65 /A /B /C] >> "
          b"/FirstChar 65 /LastChar 67 /Widths [1000 1000 1000] >>")
    proc = v.stream(b"", b"1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f")
    ttf = open(os.path.join(ROOT, "fonts", "liberation", "LiberationSans-Regular.ttf"), "rb").read()
    widths = b" ".join(b"600" for _ in range(95))
    sym = {5: b"<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 /LastChar 126 /Widths ["
              + widths + b"] /FontDescriptor 6 0 R >>",
           6: b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 4 /FontBBox [-200 -300 1200 1000] "
              b"/ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"}
    ap = v.stream(b"/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >>",
                  b"BT /F1 12 Tf 2 5 Td (ANNOTWORD) Tj ET")
    form = v.stream(b"/Type /XObject /Subtype /Form /Resources << /Font << /F1 5 0 R >> >>",
                    b"BT /F1 12 Tf 10 50 Td (FORMWORD) Tj ET")
    return {
        "type3-glyph-names": (v.build(v.page(b"BT /T3 12 Tf 10 100 Td (ABC) Tj ET " + pub,
                                             res=b"/Font << /F1 5 0 R /T3 8 0 R >>", more={8: t3, 9: proc})), "ABC"),
        "truetype-symbolic": (v.build({**v.page(b"BT /F1 12 Tf 10 100 Td (Hello) Tj ET"), **sym}), "Hello"),
        "freetext-appearance": (v.build(v.page(pub, page_extra=b"/Annots [8 0 R]", more={
            8: b"<< /Type /Annot /Subtype /FreeText /Rect [50 95 150 115] /DA (/Helv 9 Tf 0 g) /Contents (ANNOTWORD) "
               b"/AP << /N 9 0 R >> >>", 9: ap})), "ANNOTWORD"),
        "operand-count": (v.build(v.page(b"0 0 10 re f " + pub)), "PUBLICWORD"),
        "form-no-bbox": (v.build(v.page(b"/Fm Do " + pub, res=b"/Font << /F1 5 0 R >> /XObject << /Fm 8 0 R >>",
                                        more={8: form})), "FORMWORD"),
    }


def norm(t):
    return re.sub(r"\s+", "", t or "")


def run(kasane):
    v = vr()
    out = {}
    with tempfile.TemporaryDirectory() as tmp:
        for name, (pdf, word) in probes(v).items():
            p = os.path.join(tmp, name + ".pdf")
            open(p, "wb").write(pdf)
            k_render = subprocess.run([kasane, "render", p, "1", "--output", os.path.join(tmp, name + ".png")],
                                      capture_output=True, text=True)
            k_text = subprocess.run([kasane, "text", p, "1", "--format", "json"], capture_output=True, text=True)
            # One JSON object per character: the page's text is their
            # "unicode" values in order.
            k_text.stdout = "".join(json.loads(m) for m in re.findall(r'"unicode": ("(?:[^"\\]|\\.)*")', k_text.stdout))
            pop = subprocess.run(["pdftotext", p, "-"], capture_output=True, text=True)
            mu = subprocess.run(["mutool", "draw", "-q", "-F", "txt", "-o", "-", p, "1"], capture_output=True, text=True)
            out[name] = {
                "kasane_render": "ok" if k_render.returncode == 0 else
                ("refused" if "error [" in k_render.stderr else "crash"),
                "kasane_text_has_word": norm(word) in norm(k_text.stdout) if k_text.returncode == 0 else None,
                "poppler_text_has_word": norm(word) in norm(pop.stdout) if pop.returncode == 0 else None,
                "mupdf_text_has_word": norm(word) in norm(mu.stdout) if mu.returncode == 0 else None,
                "kasane_error_kind": (re.search(r"error \[([a-z ]+)\]", k_render.stderr + k_text.stderr) or [None, None])[1],
            }
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    res = run(sys.argv[1])
    for name, r in res.items():
        print(f"{name:22} render {r['kasane_render']:8} text kasane={r['kasane_text_has_word']} "
              f"poppler={r['poppler_text_has_word']} mupdf={r['mupdf_text_has_word']} {r['kasane_error_kind'] or ''}")
    if "--json" in sys.argv:
        json.dump(res, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1, sort_keys=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
