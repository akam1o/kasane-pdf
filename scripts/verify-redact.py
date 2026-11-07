#!/usr/bin/env python3
"""Independent external check of `kasane-pdf redact`.

Builds small PDFs that each put a secret inside a redaction rect in a
different way, runs the CLI, and checks every *successful* output with
tools that share no code with kasane-pdf:

- qpdf --check (structure), and qpdf --qdf --object-streams=disable
  --decode-level=all, after which every object and every decodable
  stream is plain bytes: the secret text and the secret image samples
  must not occur anywhere;
- pdftotext: the secret must not be extracted; the public text must be.

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
    ("invisible-ocr-text", build(page(b"BT /F1 12 Tf 3 Tr 50 100 Td (SECRETTEXT) Tj ET")), [], "refused"),
    ("form-xobject", build(page(b"/Fm Do", res=b"/XObject << /Fm 8 0 R >>", more={
        8: stream(b"/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >>", TEXT)})), [], "refused"),
    ("text-annotation", build(page(TEXT, page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /Text /Rect [50 95 60 105] /Contents (SECRETTEXT) >>"})), [], "refused"),
]


def run():
    for tool in ("qpdf", "pdftotext"):
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
                if SAMPLES[:96] in data:
                    problems.append("original image samples present after full decode")
                text = subprocess.run(["pdftotext", out, "-"], capture_output=True).stdout
                if SECRET in text:
                    problems.append("pdftotext extracts the secret")
                if rects and PUBLIC not in text:
                    problems.append("public text lost")
            status = "ok  " if not problems else "FAIL"
            print(f"{status} {name}: {got}" + ("" if not problems else " — " + "; ".join(problems)))
            failed += bool(problems)
    print(f"{len(CASES) - failed}/{len(CASES)} cases as expected")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(run())
