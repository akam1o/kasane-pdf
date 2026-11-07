#!/usr/bin/env python3
"""Independent external check of `kasane-pdf redact`.

Builds small PDFs that each put a secret inside a redaction rect in a
different way, runs the CLI, and checks every *successful* output with
tools that share no code with kasane-pdf:

- qpdf --check (structure), and qpdf --qdf --object-streams=disable
  --decode-level=all, after which every object and every decodable
  stream is plain bytes: the secret text and the secret image samples
  must not occur anywhere;
- pdftotext: the secret must not be extracted; the public text must be;
- pdftoppm: nothing is drawn inside the rect (cases without images,
  whose zeroed samples are black by design).

A case may instead be refused (non-zero exit, no output file) — the
expected outcome for the structures `redact` does not process. Exit 0
iff every case behaves as declared.

Requires: qpdf, pdftotext, python3, the kasane-pdf CLI (cargo build).
Usage: scripts/verify-redact.py [path/to/kasane-pdf]
"""

import os
import shutil
import subprocess
import sys
import tempfile
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target/debug/kasane-pdf")
FONT = os.path.join(ROOT, "fonts/liberation/LiberationSans-Regular.ttf")
SECRET = b"SECRETTEXT"
PUBLIC = b"PUBLICWORD"
SAMPLES = bytes((i * 37 + 11) % 256 for i in range(16 * 16 * 3))
RECT = ["45", "90", "160", "115"]


def stream(d, data, flate=False):
    if flate:
        data = zlib.compress(data)
        d += b" /Filter /FlateDecode"
    return b"<< " + d + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def build(objs, trailer_extra=b""):
    n = max(objs) + 1
    out = b"%PDF-1.7\n"
    offs = {}
    for i in sorted(objs):
        offs[i] = len(out)
        out += b"%d 0 obj\n" % i + objs[i] + b"\nendobj\n"
    x = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % n
    for i in range(1, n):
        out += (b"%010d 00000 n \n" % offs[i]) if i in offs else b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size %d /Root 1 0 R %s>>\nstartxref\n%d\n%%%%EOF\n" % (n, trailer_extra, x)
    return out


def page(content, res=b"/Font << /F1 5 0 R >>", page_extra=b"", cat=b"", more=None, kids=b"[3 0 R]", count=1):
    ff = open(FONT, "rb").read()
    widths = b" ".join(b"600" for _ in range(95))
    objs = {
        1: b"<< /Type /Catalog /Pages 2 0 R " + cat + b">>",
        2: b"<< /Type /Pages /Kids " + kids + b" /Count %d >>" % count,
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << " + res
        + b" >> /Contents 4 0 R " + page_extra + b">>",
        4: stream(b"", content),
        5: b"<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 /LastChar 126 /Widths ["
        + widths + b"] /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>",
        6: b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 /FontBBox [-200 -300 1200 1000] "
        b"/ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>",
        7: stream(b"/Length1 %d" % len(ff), ff, flate=True),
    }
    objs.update(more or {})
    return objs


# Helvetica's AFM widths, codes 32 … 90.
HELVETICA_32_90 = (b"278 278 355 556 556 889 667 191 333 333 389 584 278 333 278 278 556 556 556 556 556 "
                   b"556 556 556 556 556 278 278 584 584 584 556 1015 667 667 722 722 667 611 778 722 278 "
                   b"500 667 556 833 722 778 667 778 722 667 611 722 667 944 667 667 611")
TEXT = b"BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET"
IMAGE = stream(b"/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8", SAMPLES, flate=True)

# (name, pdf bytes, extra CLI args, expected: "ok" or "refused")
CASES = [
    ("text", build(page(TEXT)), [], "ok"),
    ("text-in-object-stream-metadata", build(page(TEXT), b"/Info << /Title (SECRETTEXT) >>"), [], "ok"),
    ("image-inherited-unused", build({
        **page(TEXT + b" q 100 0 0 100 50 50 cm /Im1 Do Q", res=b"/Font << /F1 5 0 R >> /XObject << /Im1 8 0 R >>",
               kids=b"[3 0 R 9 0 R]", count=2),
        2: b"<< /Type /Pages /Kids [3 0 R 9 0 R] /Count 2 /Resources << /XObject << /Im1 8 0 R >> >> >>",
        8: IMAGE,
        # Page 2 inherits /Im1 from the /Pages node and never draws it.
        9: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 10 0 R >>",
        10: stream(b"", b""),
    }), ["--rect", "0", "0", "200", "200"], "ok"),
    ("vector-path", build(page(TEXT + b" 0 0 1 rg 60 95 30 10 re f")), [], "ok"),
    ("link", build(page(TEXT, page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /Link /Rect [50 95 120 110] /A << /S /URI /URI (https://x.invalid/SECRETTEXT) >> >>"})), [], "ok"),
    ("structure-alt-text", build(page(b"/P <</MCID 0>> BDC " + TEXT + b" EMC", page_extra=b"/StructParents 0",
        cat=b"/StructTreeRoot 8 0 R", more={8: b"<< /Type /StructTreeRoot /K 9 0 R >>",
        9: b"<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 0 /ActualText (SECRETTEXT) >>"})), [], "refused"),
    ("structure-alt-text-removed", build(page(b"/P <</MCID 0>> BDC " + TEXT + b" EMC", page_extra=b"/StructParents 0",
        cat=b"/StructTreeRoot 8 0 R", more={8: b"<< /Type /StructTreeRoot /K 9 0 R >>",
        9: b"<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R /K 0 /ActualText (SECRETTEXT) >>"})), ["--remove-structure-tree"], "ok"),
    ("invisible-ocr-text", build(page(b"BT /F1 12 Tf 3 Tr 50 100 Td (SECRETTEXT) Tj ET "
                                       b"BT /F1 12 Tf 3 Tr 10 20 Td (PUBLICWORD) Tj ET")), [], "ok"),
    ("form-xobject", build(page(b"/Fm Do", res=b"/XObject << /Fm 8 0 R >>", more={
        8: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >>", TEXT)})), [], "ok"),
    ("form-in-form", build(page(b"/Fm Do", res=b"/XObject << /Fm 8 0 R >>", more={
        8: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /XObject << /In 9 0 R >> >>", b"/In Do"),
        9: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >>", TEXT)})), [], "ok"),
    ("image-in-form", build(page(TEXT + b" /Fm Do", res=b"/Font << /F1 5 0 R >> /XObject << /Fm 8 0 R >>", more={
        8: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /XObject << /Im1 9 0 R >> >>", b"q 100 0 0 20 50 92 cm /Im1 Do Q"),
        9: IMAGE})), [], "ok"),
    ("text-annotation", build(page(TEXT, page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /Text /Rect [50 95 60 105] /Contents (SECRETTEXT) >>"})), [], "refused"),
    ("text-annotation-removed", build(page(TEXT, page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /Text /Rect [50 95 60 105] /Contents (SECRETTEXT) >>"})),
     ["--remove-annotations"], "ok"),
    ("annotation-outside", build(page(TEXT, page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /Square /Rect [170 170 190 190] /Contents (Reviewed) >>"})), [], "ok"),
    # Structures common in real documents.
    ("table-rules-crossing", build(page(TEXT + b" 0.5 w 10 92 m 190 92 l 10 112 m 190 112 l 100 80 m 100 130 l S")), [], "ok"),
    ("background-box-crossing", build(page(b"0.9 g 20 85 170 35 re f 0 g " + TEXT)), [], "ok"),
    ("rotated-ctm-rule", build(page(TEXT + b" q 0 1 -1 0 200 0 cm 1 w 10 100 m 190 100 l S Q")), [], "ok"),
    ("curve-crossing", build(page(TEXT + b" 1 w 30 100 m 80 130 120 70 170 100 c S")), [], "ok"),
    ("dashed-rule-crossing", build(page(TEXT + b" [3 2] 0 d 1 w 10 100 m 190 100 l S")), [], "ok"),
    ("inline-image", build(page(TEXT + b" q 20 0 0 20 60 95 cm BI /W 2 /H 1 /CS /G /BPC 8 ID \x10\x20 EI Q")), [], "refused"),
    ("inline-image-outside", build(page(TEXT + b" q 20 0 0 20 150 20 cm BI /W 2 /H 1 /CS /G /BPC 8 ID \x10\x20 EI Q")), [], "ok"),
    ("image-shown-on-two-pages", build({
        **page(TEXT + b" q 100 0 0 30 50 90 cm /Im1 Do Q", res=b"/Font << /F1 5 0 R >> /XObject << /Im1 8 0 R >>",
               kids=b"[3 0 R 9 0 R]", count=2),
        8: IMAGE,
        9: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 8 0 R >> >> /Contents 10 0 R >>",
        10: stream(b"", b"q 50 0 0 50 0 0 cm /Im1 Do Q"),
    }), [], "ok"),
    # A non-embedded font: its ink extent is a reader's choice — refused,
    # even with every width given.
    ("non-embedded-font", build(page(TEXT, res=b"/Font << /F1 8 0 R >>", more={
        8: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 90 /Widths ["
        + HELVETICA_32_90 + b"] >>"})), [], "refused"),
    # … unless its substitute is embedded first: then every reader draws
    # exactly the embedded glyphs.
    ("non-embedded-font-embedded", build(page(TEXT, res=b"/Font << /F1 8 0 R >>", more={
        8: b"<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>"})), ["--embed-substitute-fonts"], "ok"),
    # A Type 3 font whose glyphs are paths: evaluated by their ink.
    ("type3-font", build(page(b"BT /T3 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /T3 12 Tf 10 20 Td (PUBLICWORD) Tj ET",
                              res=b"/Font << /T3 8 0 R >>", more={
        8: b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] "
           b"/CharProcs << /g 9 0 R >> /Encoding << /Type /Encoding /Differences [65 "
           + b" ".join(b"/g" for _ in range(26)) + b"] >> /FirstChar 65 /LastChar 90 "
           b"/Widths [" + b" ".join(b"700" for _ in range(26)) + b"] /ToUnicode 10 0 R /Resources << >> >>",
        9: stream(b"", b"700 0 0 0 600 700 d1 50 0 500 650 re f"),
        10: stream(b"", b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /T3 def "
                        b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfrange <41> <5A> <0041> endbfrange "
                        b"endcmap CMapName currentdict /CMap defineresource pop end end"),
    })), [], "ok"),
]
# The same file with the document-wide option: the original still drawn
# on page 2 is a copy elsewhere, so refused.
CASES.append(("image-shown-on-two-pages-refuse-copies", dict((c[0], c[1]) for c in CASES)["image-shown-on-two-pages"], ["--refuse-copies-elsewhere"], "refused"))
# Cases whose original image samples legitimately stay (another page
# draws the original; the redaction is positional and reports it).
ORIGINAL_STAYS = {"image-shown-on-two-pages"}


def rects_custom(args):
    return "--rect" in args


def ink_inside(pdf, tmp):
    """Non-white pixels Poppler draws strictly inside RECT (72 dpi, a
    one-pixel margin for anti-aliasing at the edges)."""
    base = os.path.join(tmp, "render")
    subprocess.run(["pdftoppm", "-r", "72", "-gray", "-singlefile", pdf, base], capture_output=True, check=True)
    data = open(base + ".pgm", "rb").read()
    parts = data.split(maxsplit=4)
    w, h = int(parts[1]), int(parts[2])
    pix = parts[4]
    llx, lly, urx, ury = (float(v) for v in RECT)
    count = 0
    for y in range(int(h - ury) + 1, int(h - lly) - 1):
        for x in range(int(llx) + 1, int(urx) - 1):
            if pix[y * w + x] != 255:
                count += 1
    return count


def run():
    for tool in ("qpdf", "pdftotext", "pdftoppm"):
        if shutil.which(tool) is None:
            print(f"SKIP: {tool} not found")
            return 2
    if not os.access(BIN, os.X_OK):
        print(f"kasane-pdf binary not found at {BIN} (run: cargo build)")
        return 2
    failed = 0
    with tempfile.TemporaryDirectory() as tmp:
        for name, pdf, args, expect in CASES:
            src = os.path.join(tmp, name + ".pdf")
            out = os.path.join(tmp, name + "-out.pdf")
            open(src, "wb").write(pdf)
            rects = [] if "--rect" in args else ["--rect", *RECT]
            r = subprocess.run([BIN, "redact", src, "--page", "1", *rects, *args, "--output", out],
                               capture_output=True)
            got = "ok" if r.returncode == 0 else "refused"
            problems = []
            if got != expect:
                problems.append(f"expected {expect}, got {got}: {r.stderr.decode(errors='replace').strip()[:200]}")
            if got == "refused" and os.path.exists(out):
                problems.append("refused but wrote an output file")
            if got == "ok":
                if subprocess.run(["qpdf", "--check", out], capture_output=True).returncode not in (0, 3):
                    problems.append("qpdf --check failed")
                plain = os.path.join(tmp, name + "-plain.pdf")
                subprocess.run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", out, plain],
                               capture_output=True)
                data = open(plain, "rb").read()
                if SECRET in data:
                    problems.append("secret text bytes present after full decode")
                if SAMPLES[:96] in data and name not in ORIGINAL_STAYS:
                    problems.append("original image samples present after full decode")
                if "image" not in name and not rects_custom(args):
                    ink = ink_inside(out, tmp)
                    if ink:
                        problems.append(f"{ink} non-white pixels inside the rect (Poppler render)")
                text = subprocess.run(["pdftotext", out, "-"], capture_output=True).stdout
                if SECRET in text:
                    problems.append("pdftotext extracts the secret")
                if rects and PUBLIC not in text:
                    problems.append("public text lost")
            status = "ok  " if not problems else "FAIL"
            print(f"{status} {name}: {got}" + ("" if not problems else " — " + "; ".join(problems)))
            failed += bool(problems)
    oks = sum(1 for c in CASES if c[3] == "ok")
    print(f"{len(CASES) - failed}/{len(CASES)} cases as expected; "
          f"declared succeeding: {oks}, refused: {len(CASES) - oks}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(run())
