#!/usr/bin/env python3
"""Generate the multi-branch, multi-page fixture.

Structure (3-level /Pages tree, two branches, different inherited
/Resources per branch, shared name /Im1 resolves to a *different*
image in each branch):

    Catalog (1)
      └─ Pages root (2)           — NO /Resources
           ├─ Pages branch A (10) — /Resources {XObject:{/Im1→imgA},
           │                        Font:{/F1→fontA}}
           │    └─ Pages leaf-holder A (20)  — NO /Resources
           │         └─ Page 1 (3)  — edited target
           │              contents: text "ABC" + /Im1 Do
           └─ Pages branch B (11) — /Resources {XObject:{/Im1→imgB},
                                    Font:{/F1→fontA}}
                └─ Pages leaf-holder B (21)  — NO /Resources
                     └─ Page 2 (4)  — unedited
                          contents: text "ABC" + /Im1 Do

Branch A's /Im1 is a 2×2 gray image whose samples are
[0xAA,0xBB,0xCC,0xDD]; branch B's /Im1 is a *different* 2×2 gray
image [0x11,0x22,0x33,0x44]. Editing page 1 (rect covering the
top-left pixel) must blacken only branch-A's image; page 2 must be
untouched, and its /Im1 must still resolve to the *branch-B* image
(not the edited clone and not the branch-A original).

All four leaf-holder /Pages nodes have no /Resources — the strip
decision must walk through them; both branches fully materialised
→ root drops nothing (root has no /Resources), and the two
resource-bearing branch nodes (10, 11) are *not* stripped because
their /Resources is what the leaves inherit…

Note — the region editor materialises each *page's* effective Resources into the
page dict. After materialisation, branch nodes 10/11 are candidates
for stripping *their own* /Resources (all descendants materialised).
So in the output, pages 3 and 4 carry explicit /Resources, and
nodes 10 and 11 have theirs removed.
"""

import struct, zlib

FONT_PATH = "KasaneTest.ttf"


def build():
    parts = [b"%PDF-1.4\n"]
    offs = {}

    def push(n, body):
        offs[n] = sum(len(p) for p in parts)
        parts.append(b"%d 0 obj\n" % n)
        parts.append(body if isinstance(body, bytes) else body.encode())
        parts.append(b"\nendobj\n")

    # ---- objects -------------------------------------------------
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>")
    # Root /Pages: two branch kids, no /Resources.
    push(2, b"<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 >>")

    # Branch A intermediate /Pages — carries the Resources that page 3
    # inherits.  /Im1 names image 30; /F1 names font 40.
    push(10, b"<< /Type /Pages /Kids [20 0 R] /Count 1 /Parent 2 0 R "
             b"/Resources << /XObject << /Im1 30 0 R >> "
             b"/Font << /F1 40 0 R >> >> >>")
    # Leaf-holder for branch A — no /Resources (traversal must pass
    # through it).
    push(20, b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 10 0 R >>")
    # Page 1 — edited target.
    push(3, b"<< /Type /Page /Parent 20 0 R /MediaBox [0 0 200 200] "
            b"/Contents 50 0 R >>")

    # Branch B intermediate /Pages — *same name* /Im1 but a different
    # image object (31); same font ref.
    push(11, b"<< /Type /Pages /Kids [21 0 R] /Count 1 /Parent 2 0 R "
             b"/Resources << /XObject << /Im1 31 0 R >> "
             b"/Font << /F1 40 0 R >> >> >>")
    push(21, b"<< /Type /Pages /Kids [4 0 R] /Count 1 /Parent 11 0 R >>")
    # Page 2 — unedited.
    push(4, b"<< /Type /Page /Parent 21 0 R /MediaBox [0 0 200 200] "
            b"/Contents 51 0 R >>")

    # ---- images --------------------------------------------------
    # Branch-A image: 2×2 DeviceGray, samples AA BB CC DD.
    img_a = bytes([0xAA, 0xBB, 0xCC, 0xDD])
    push(30, b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
             b"/ColorSpace /DeviceGray /BitsPerComponent 8 /Length " +
             str(len(img_a)).encode() + b" >>\nstream\n" + img_a +
             b"\nendstream")
    # Branch-B image: 2×2 DeviceGray, samples 11 22 33 44 — distinct
    # from branch-A so a wrong resolution is visible.
    img_b = bytes([0x11, 0x22, 0x33, 0x44])
    push(31, b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
             b"/ColorSpace /DeviceGray /BitsPerComponent 8 /Length " +
             str(len(img_b)).encode() + b" >>\nstream\n" + img_b +
             b"\nendstream")

    # ---- font (shared) -------------------------------------------
    font_bytes = open(FONT_PATH, "rb").read()
    enc = zlib.compress(font_bytes)
    push(40, b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest "
             b"/FirstChar 32 /LastChar 255 /Widths [" +
             b" ".join([b"600"] * 224) + b"] "
             b"/Encoding /WinAnsiEncoding /FontDescriptor 41 0 R >>")
    push(41, b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 "
             b"/FontBBox [-100 -300 1200 1000] /ItalicAngle 0 "
             b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 "
             b"/FontFile2 42 0 R >>")
    push(42, b"<< /Length " + str(len(enc)).encode() +
             b" /Filter /FlateDecode /Length1 " +
             str(len(font_bytes)).encode() + b" >>\nstream\n" + enc +
             b"\nendstream")

    # ---- contents --------------------------------------------------
    # Page 1: text "ABC" at (20,140) 24pt; image drawn at 40×40
    # scaled, painted rect (20,60)-(60,100).
    body_a = (b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n"
              b"40 0 0 40 20 60 cm\n/Im1 Do\n")
    push(50, b"<< /Length " + str(len(body_a)).encode() +
             b" >>\nstream\n" + body_a + b"\nendstream")
    # Page 2: same text; image drawn lower so it is visually distinct
    # (rect 20,10)-(60,50)).
    body_b = (b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n"
              b"40 0 0 40 20 10 cm\n/Im1 Do\n")
    push(51, b"<< /Length " + str(len(body_b)).encode() +
             b" >>\nstream\n" + body_b + b"\nendstream")

    # ---- xref / trailer --------------------------------------------
    xref = sum(len(p) for p in parts)
    parts.append(b"xref\n0 60\n0000000000 65535 f \n")
    for n in range(1, 60):
        if n in offs:
            parts.append(b"%010d 00000 n \n" % offs[n])
        else:
            parts.append(b"0000000000 00000 f \n")
    parts.append(b"trailer\n<< /Size 60 /Root 1 0 R >>\n")
    parts.append(b"startxref\n%d\n%%%%EOF\n" % xref)
    return b"".join(parts)


if __name__ == "__main__":
    import sys, os
    out = sys.argv[1] if len(sys.argv) > 1 else "m224_branches.pdf"
    os.chdir(os.path.dirname(os.path.abspath(__file__)))
    data = build()
    open(out, "wb").write(data)
    print("wrote %s (%d bytes)" % (out, len(data)))
