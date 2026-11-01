"""Generate the synthetic TrueType fonts of tests/text_unicode_fallback.rs.

Every glyph is a box drawn here (no data from an existing typeface).
Requires fontTools (test-time only). Run from this directory:

    python3 make_fonts.py

* uni.ttf — Unicode cmap (3,1) and (0,3):
    gid 1 "q1"    ← U+0061                    (one code point)
    gid 2 "q2"    ← U+0041 and U+0391         (ambiguous: A / Alpha)
    gid 3 "q3"    ← U+0020 and U+00A0         (equivalent: space)
    gid 4 "q4"    ← U+E000                    (private use only)
    gid 5 "Omega" ← no cmap entry; its post name (U+2126 by the AGL)
    gid 6 "q6"    ← no cmap entry, meaningless name
    gid 7 "q7"    ← U+00E9

Glyph names carry no glyph-list meaning (`q1`; not `g_one`, which the
glyph-name rules read as the ligature "g" + "one").
* symbol.ttf — only a (3,0) symbol cmap: U+F041 → gid 1 "q1";
  no Unicode anywhere.
* big.ttf — a format 13 (3,10) subtable mapping U+10000..U+3FFFF
  (196 608 code points) to gid 1, plus (3,1) U+0061 → gid 2: more than
  the reverse-table bound, so the font's cmap is not read backwards.
* broken.ttf — uni.ttf with the cmap table's subtable offsets pointing
  past the end of the table.
"""

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable


def box():
    p = TTGlyphPen(None)
    p.moveTo((100, 0))
    p.lineTo((500, 0))
    p.lineTo((500, 700))
    p.lineTo((100, 700))
    p.closePath()
    return p.glyph()


def build(order, cmap, out, extra=None):
    fb = FontBuilder(1000)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap(cmap)
    glyphs = {n: (TTGlyphPen(None).glyph() if n == ".notdef" else box()) for n in order}
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({n: (600, 100) for n in order})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "KasaneUni", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost(keepGlyphNames=True)
    if extra:
        extra(fb.font)
    fb.save(out)


uni_order = [".notdef", "q1", "q2", "q3", "q4", "Omega", "q6", "q7"]
build(
    uni_order,
    {0x61: "q1", 0x41: "q2", 0x391: "q2", 0x20: "q3", 0xA0: "q3",
     0xE000: "q4", 0xE9: "q7"},
    "uni.ttf",
)


def symbol_only(font):
    st = CmapSubtable.newSubtable(4)
    st.platformID, st.platEncID, st.language = 3, 0, 0
    st.cmap = {0xF041: "q1"}
    font["cmap"].tables = [st]


build([".notdef", "q1"], {0x41: "q1"}, "symbol.ttf", symbol_only)


def big(font):
    st = CmapSubtable.newSubtable(13)
    st.platformID, st.platEncID, st.language = 3, 10, 0
    st.cmap = {cp: "q1" for cp in range(0x10000, 0x40000)}
    small = CmapSubtable.newSubtable(4)
    small.platformID, small.platEncID, small.language = 3, 1, 0
    small.cmap = {0x61: "q2"}
    font["cmap"].tables = [small, st]


build([".notdef", "q1", "q2"], {0x61: "q2"}, "big.ttf", big)

# broken.ttf: every cmap subtable offset beyond the table.
data = bytearray(open("uni.ttf", "rb").read())
num_tables = int.from_bytes(data[4:6], "big")
for i in range(num_tables):
    rec = 12 + 16 * i
    if data[rec:rec + 4] == b"cmap":
        off = int.from_bytes(data[rec + 8:rec + 12], "big")
        n = int.from_bytes(data[off + 2:off + 4], "big")
        for k in range(n):
            e = off + 4 + 8 * k + 4
            data[e:e + 4] = (0x7FFF0000).to_bytes(4, "big")
open("broken.ttf", "wb").write(bytes(data))
