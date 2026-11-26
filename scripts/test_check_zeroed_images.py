#!/usr/bin/env python3
"""scripts/check-zeroed-images.py on synthetic PDFs (no corpus): samples
under the rect zero or not, at its edges, through forms, rotated, under a
soft mask, an original carried, and what it does not follow (`unhandled`).

    python3 scripts/test_check_zeroed_images.py   (needs qpdf; KASANE_REQUIRE_QPDF=1:
    fail instead of skipping without it)
"""
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("czi", os.path.join(ROOT, "scripts", "check-zeroed-images.py"))
czi = importlib.util.module_from_spec(spec)
spec.loader.exec_module(czi)

RECT = [50, 50, 100, 100]
GRAY = b"<< /Type /XObject /Subtype /Image /Width 20 /Height 20 /ColorSpace /DeviceGray /BitsPerComponent 8 >>"
FULL = b"q 200 0 0 200 0 0 cm /Im0 Do Q"


def write_pdf(path, objs, page_extra=b"", resources=b"/XObject << /Im0 5 0 R >>"):
    """objs: {number >= 4: (dict bytes, stream data or None)}; 1-3 are the
    catalog, the page tree and the page (contents 4 0 R)."""
    base = {1: (b"<< /Type /Catalog /Pages 2 0 R >>", None),
            2: (b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>", None),
            3: (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << " + resources
                + b" >> /Contents 4 0 R " + page_extra + b">>", None)}
    base.update(objs)
    out, offs = b"%PDF-1.7\n", {}
    for n in sorted(base):
        body, data = base[n]
        offs[n] = len(out)
        if data is None:
            out += b"%d 0 obj\n%s\nendobj\n" % (n, body)
        else:
            out += b"%d 0 obj\n%s /Length %d >>\nstream\n%s\nendstream\nendobj\n" % (n, body[:-2], len(data), data)
    size = max(base) + 1
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % size
    out += b"".join(b"%010d 00000 n \n" % offs[n] if n in offs else b"0000000000 65535 f \n" for n in range(1, size))
    out += b"trailer\n<< /Root 1 0 R /Size %d >>\nstartxref\n%d\n%%%%EOF\n" % (size, xref)
    with open(path, "wb") as fh:
        fh.write(out)


def gray(value=0x80, zero=((5, 10), (10, 15)), poke=None):
    """20x20 gray samples at `value`, zero in the cells x in [x0, x1),
    y in [y0, y1) (the cells under RECT when drawn over the page), then
    `poke` {(x, y): v}."""
    b = bytearray([value] * 400)
    for y in range(*zero[1]):
        for x in range(*zero[0]):
            b[y * 20 + x] = 0
    for (x, y), v in (poke or {}).items():
        b[y * 20 + x] = v
    return bytes(b)


if os.environ.get("KASANE_REQUIRE_QPDF") and not shutil.which("qpdf"):
    sys.exit("test_check_zeroed_images: KASANE_REQUIRE_QPDF is set but qpdf is not on PATH")


@unittest.skipUnless(shutil.which("qpdf"), "qpdf")
class Check(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.d)
        self.inp = self.pdf("in", {4: (b"<< >>", FULL), 5: (GRAY[:-2] + b" /Filter /FlateDecode >>",
                                                            zlib.compress(gray(zero=((0, 0), (0, 0)))))})

    def pdf(self, name, objs, **kw):
        raw = os.path.join(self.d, name + "-raw.pdf")
        path = os.path.join(self.d, name + ".pdf")
        write_pdf(raw, objs, **kw)
        # qpdf rewrites it cleanly (the check takes a repaired file as unhandled).
        subprocess.run(["qpdf", raw, path], check=False, capture_output=True)
        return path

    def verdict(self, objs, inp=None, rect=RECT, elsewhere=False, **kw):
        out = self.pdf("out", objs, **kw)
        return czi.check(inp or self.inp, out, 1, rect, elsewhere)

    def test_zeroed_samples_are_zero(self):
        v, n = self.verdict({4: (b"<< >>", FULL), 5: (GRAY, gray())})
        self.assertEqual((v, n), ("zero", 25))

    def test_the_unredacted_page_is_a_residue(self):
        v, _ = czi.check(self.inp, self.inp, 1, RECT)
        self.assertTrue(v.startswith("residue:25 sample(s)"), v)

    def test_one_sample_left_at_each_edge_of_the_rect(self):
        for cell in ((5, 10), (9, 14), (5, 14), (9, 10)):
            v, _ = self.verdict({4: (b"<< >>", FULL), 5: (GRAY, gray(poke={cell: 1}))})
            self.assertTrue(v.startswith("residue:1 sample(s)"), (cell, v))

    def test_samples_just_outside_the_rect_are_not_counted(self):
        v, _ = self.verdict({4: (b"<< >>", FULL), 5: (GRAY, gray(poke={(4, 12): 9, (10, 12): 9, (7, 9): 9, (7, 15): 9}))})
        self.assertEqual(v, "zero")

    def test_a_quarter_turn(self):
        # page x = 200 - 200 v, page y = 200 u: the cells under the rect
        # are x 5..9 and y 5..9; those of the upright placement are not.
        content = b"q 0 200 -200 0 200 0 cm /Im0 Do Q"
        self.assertEqual(self.verdict({4: (b"<< >>", content), 5: (GRAY, gray(zero=((5, 10), (5, 10))))})[0], "zero")
        v, _ = self.verdict({4: (b"<< >>", content), 5: (GRAY, gray())})
        self.assertTrue(v.startswith("residue:"), v)
        v, _ = self.verdict({4: (b"<< >>", content), 5: (GRAY, gray(zero=((0, 0), (0, 0))))})
        self.assertTrue(v.startswith("residue:"), v)

    def test_a_cell_boundary_on_the_rect_edge(self):
        # 0.3 * 3 is 0.8999999999999999 and 0.3 * 6 1.7999999999999998:
        # the cells on either side of the edges only touch the rect.
        cells = czi.cells_in_rect((3, 0, 0, 3, 0, 0), 10, 10, [0.9, 0.9, 1.8, 1.8])
        self.assertEqual(sorted({x for x, _ in cells}), [3, 4, 5])
        self.assertEqual(sorted({y for _, y in cells}), [4, 5, 6])
        # A thousandth of a point inside is inside.
        cells = czi.cells_in_rect((3, 0, 0, 3, 0, 0), 10, 10, [0.899, 0.9, 1.801, 1.8])
        self.assertEqual(sorted({x for x, _ in cells}), [2, 3, 4, 5, 6])

    def test_rotated_or_sheared_is_unhandled(self):
        v, _ = self.verdict({4: (b"<< >>", b"q 141 141 -141 141 100 0 cm /Im0 Do Q"), 5: (GRAY, gray())})
        self.assertEqual(v, "unhandled:an image placed rotated or sheared")

    def test_through_a_form_with_its_own_resources_and_matrix(self):
        form = b"<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Matrix [200 0 0 200 0 0] " \
               b"/Resources << /XObject << /I 5 0 R >> /ColorSpace << /C /DeviceRGB >> >> >>"
        rgb = b"<< /Type /XObject /Subtype /Image /Width 20 /Height 20 /ColorSpace /C /BitsPerComponent 8 >>"
        zero = bytearray(b"\x80" * 1200)
        for y in range(10, 15):
            zero[y * 60 + 15:y * 60 + 30] = bytes(15)
        objs = {4: (b"<< >>", b"q /F Do Q"), 5: (rgb, bytes(zero)), 6: (form, b"q 1 0 0 1 0 0 cm /I Do Q")}
        self.assertEqual(self.verdict(objs, resources=b"/XObject << /F 6 0 R >>")[0], "zero")
        zero[10 * 60 + 29] = 1  # the blue of the last cell
        objs[5] = (rgb, bytes(zero))
        v, _ = self.verdict(objs, resources=b"/XObject << /F 6 0 R >>")
        self.assertTrue(v.startswith("residue:1 sample(s)"), v)

    def test_image_mask_paints_nothing_by_its_decode(self):
        # 24x20 stencil, 3 bytes a row; the rect covers columns 6..11, rows 10..14.
        def mask(fill, hole):
            rows = []
            for y in range(20):
                bits = [hole if 6 <= x < 12 and 10 <= y < 15 else fill for x in range(24)]
                rows.append(bytes(int("".join(map(str, bits[i:i + 8])), 2) for i in (0, 8, 16)))
            return b"".join(rows)
        im = b"<< /Type /XObject /Subtype /Image /Width 24 /Height 20 /ImageMask true >>"
        inv = b"<< /Type /XObject /Subtype /Image /Width 24 /Height 20 /ImageMask true /Decode [1 0] >>"
        c = {4: (b"<< >>", b"0 0 1 rg " + FULL)}
        self.assertEqual(self.verdict({**c, 5: (im, mask(0, 1))})[0], "zero")
        self.assertTrue(self.verdict({**c, 5: (im, mask(0, 0))})[0].startswith("residue:30"))
        self.assertEqual(self.verdict({**c, 5: (inv, mask(1, 0))})[0], "zero")
        self.assertTrue(self.verdict({**c, 5: (inv, mask(1, 1))})[0].startswith("residue:30"))
        odd = b"<< /Type /XObject /Subtype /Image /Width 24 /Height 20 /ImageMask true /Decode [0 0] >>"
        self.assertEqual(self.verdict({**c, 5: (odd, mask(0, 1))})[0], "unhandled:image mask /Decode")

    def test_soft_mask_must_be_opaque_under_the_rect(self):
        sm = b"<< /Type /XObject /Subtype /Image /Width 10 /Height 10 /ColorSpace /DeviceGray /BitsPerComponent 8 >>"
        for value, want in ((255, "zero"), (0, "residue:1 soft-mask")):
            s = bytearray([255] * 100)
            s[6 * 10 + 3] = value
            v, _ = self.verdict({4: (b"<< >>", FULL), 5: (GRAY[:-2] + b" /SMask 6 0 R >>", gray()), 6: (sm, bytes(s))})
            self.assertTrue(v.startswith(want), v)

    def test_an_original_still_carried(self):
        # The input's samples kept in a stream the page lists but does not draw.
        objs = {4: (b"<< >>", FULL), 5: (GRAY, gray()),
                6: (GRAY[:-2] + b" /Filter /FlateDecode >>", zlib.compress(gray(zero=((0, 0), (0, 0)))))}
        res = b"/XObject << /Im0 5 0 R /Old 6 0 R >>"
        self.assertEqual(self.verdict(objs, resources=res)[0], "residue:1 original image(s) under the rect still carried")
        self.assertEqual(self.verdict(objs, resources=res, elsewhere=True)[0], "zero")

    def test_what_it_does_not_follow(self):
        ok = {4: (b"<< >>", FULL), 5: (GRAY, gray())}
        cases = [
            ("inline image", {4: (b"<< >>", b"BI /W 1 /H 1 /CS /G /BPC 8 ID \x80 EI " + FULL)}, {}),
            ("a pattern colour", {4: (b"<< >>", b"/Pattern cs /P0 scn " + FULL)}, {}),
            ("pattern resources", {6: (b"<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceGray "
                                       b"/Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >> >>",
                                       None)},
             {"resources": b"/XObject << /Im0 5 0 R >> /Pattern << /P0 6 0 R >>"}),
            ("a graphics-state soft mask", {6: (b"<< /SMask << /S /Luminosity /G 7 0 R >> >>", None),
                                            7: (b"<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] >>", b"")},
             {"resources": b"/XObject << /Im0 5 0 R >> /ExtGState << /G0 6 0 R >>"}),
            ("an annotation with an appearance", {6: (b"<< /Type /Annot /Subtype /Stamp /Rect [0 0 9 9] "
                                                      b"/AP << /N 7 0 R >> >>", None),
                                                  7: (b"<< /Type /XObject /Subtype /Form /BBox [0 0 9 9] >>", b"")},
             {"page_extra": b"/Annots [6 0 R] "}),
            ("a filter qpdf did not remove", {5: (GRAY[:-2] + b" /Filter /DCTDecode >>", b"\xff\xd8\xff\xd9")}, {}),
            ("a /Mask", {5: (GRAY[:-2] + b" /Mask [0 10] >>", gray())}, {}),
        ]
        for why, objs, kw in cases:
            v, _ = self.verdict({**ok, **objs}, **kw)
            self.assertTrue(v.startswith("unhandled:") and why in v, (why, v))

    def test_a_repaired_output_is_unhandled(self):
        bad = os.path.join(self.d, "bad.pdf")
        write_pdf(bad, {4: (b"<< >>", FULL), 5: (GRAY, gray())})
        with open(bad, "r+b") as fh:
            fh.seek(-40, 2)
            fh.write(b"x" * 20)
        v, _ = czi.check(self.inp, bad, 1, RECT)
        self.assertTrue(v.startswith("unhandled:qpdf exit"), v)

    def test_no_image_under_the_rect_checks_nothing(self):
        v, n = self.verdict({4: (b"<< >>", b"q 40 0 0 40 0 0 cm /Im0 Do Q"), 5: (GRAY, gray())})
        self.assertEqual((v, n), ("zero", 0))


if __name__ == "__main__":
    unittest.main()
