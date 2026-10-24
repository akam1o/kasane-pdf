"""Generate `KasaneCIDTest.ttf` — a tiny synthetic TrueType font for
the Type0/CIDFontType2 tests.

Glyph inventory (drawn programmatically, no font data derived from
any existing typeface — same provenance as `KasaneTest.ttf`):

    gid 0  .notdef   (empty)
    gid 1  g_box     400-wide filled box  — 'A' shape
    gid 2  g_tri     triangle             — 'B' shape
    gid 3  g_bar     200-wide left bar    — 'H' shape
    gid 4  g_circle  ellipse
    gid 5  g_hole    square with square hole

Distinct shapes at *non-identity* glyph ids let tests verify that
a `/CIDToGIDMap` stream actually remaps CIDs — e.g. CID 0x0041 with
map `…g_bar` must draw the bar, not the box at gid 1.

Requires fontTools (test-time only, not a crate dependency).
"""

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPM = 1000
OUT = "KasaneCIDTest.ttf"


def main() -> None:
    fb = FontBuilder(UPM)
    order = [".notdef", "g_box", "g_tri", "g_bar", "g_circle", "g_hole"]
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({0x41: "g_box", 0x42: "g_tri"})

    def pen() -> TTGlyphPen:
        return TTGlyphPen(None)

    glyphs = {}
    p = pen()
    glyphs[".notdef"] = p.glyph()

    p = pen()
    p.moveTo((100, 0))
    p.lineTo((500, 0))
    p.lineTo((500, 700))
    p.lineTo((100, 700))
    p.closePath()
    glyphs["g_box"] = p.glyph()

    p = pen()
    p.moveTo((100, 0))
    p.lineTo((400, 0))
    p.lineTo((250, 700))
    p.closePath()
    glyphs["g_tri"] = p.glyph()

    p = pen()
    p.moveTo((100, 0))
    p.lineTo((300, 0))
    p.lineTo((300, 700))
    p.lineTo((100, 700))
    p.closePath()
    glyphs["g_bar"] = p.glyph()

    p = pen()
    p.moveTo((300, 650))
    p.qCurveTo((540, 650), (540, 350))
    p.qCurveTo((540, 60), (300, 60))
    p.qCurveTo((60, 60), (60, 350))
    p.qCurveTo((60, 650), (300, 650))
    p.closePath()
    glyphs["g_circle"] = p.glyph()

    p = pen()
    p.moveTo((60, 0))
    p.lineTo((540, 0))
    p.lineTo((540, 700))
    p.lineTo((60, 700))
    p.closePath()
    p.moveTo((160, 100))
    p.lineTo((440, 100))
    p.lineTo((440, 600))
    p.lineTo((160, 600))
    p.closePath()
    glyphs["g_hole"] = p.glyph()

    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({g: (600, 0) for g in order})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable(
        {
            "familyName": "KasaneCIDTest",
            "styleName": "Regular",
            "uniqueFontIdentifier": "KasaneCIDTest",
            "fullName": "KasaneCIDTest Regular",
            "psName": "KasaneCIDTest-Regular",
        }
    )
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
    fb.setupPost()
    fb.save(OUT)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
