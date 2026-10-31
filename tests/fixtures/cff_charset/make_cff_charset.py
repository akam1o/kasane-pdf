"""A bare CFF (`/FontFile3 /Type1C`) program whose Top DICT selects the
predefined ISOAdobe charset (charset operand 0): glyphs `.notdef`,
`space`, `exclam` are standard strings 0, 1, 2, so the program carries
no charset table. `.notdef` is a small square (100..200), `exclam` the
rectangle 50..550 x 0..700, `space` empty; advances 600.

    python3 make_cff_charset.py     # needs fontTools
"""
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen

order = [".notdef", "space", "exclam"]
boxes = {".notdef": (100, 100, 200, 200), "exclam": (50, 0, 550, 700)}
fb = FontBuilder(1000, isTTF=False)
fb.setupGlyphOrder(order)
fb.setupCharacterMap({32: "space", 33: "exclam"})
charstrings = {}
for g in order:
    pen = T2CharStringPen(600, None)
    if g in boxes:
        x0, y0, x1, y1 = boxes[g]
        pen.moveTo((x0, y0))
        pen.lineTo((x1, y0))
        pen.lineTo((x1, y1))
        pen.lineTo((x0, y1))
        pen.closePath()
    charstrings[g] = pen.getCharString()
fb.setupCFF("Test", {}, charstrings, {})
fb.setupHorizontalMetrics({g: (600, 0) for g in order})
fb.setupHorizontalHeader()
fb.setupNameTable({"familyName": "Test", "styleName": "Regular"})
fb.setupOS2()
fb.setupPost()
data = fb.font["CFF "].compile(fb.font)
assert b"\x8b\x0f" in data[:40], "charset operand 0 expected in the Top DICT"
open("isoadobe.bin", "wb").write(data)
