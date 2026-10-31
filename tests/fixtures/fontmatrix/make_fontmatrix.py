"""Synthetic font programs for the FontMatrix regression tests.

Each program has one glyph, `A` (code 65 under WinAnsiEncoding): a
rectangle 0..500 x 0..700 with advance 600 *in a 1000-unit em*. The
2048-unit variants draw the same rectangle in 2048 units with a
FontMatrix of 1/2048, so every variant must render identically; the
oblique and wide variants differ only by their FontMatrix.

Written from scratch (eexec / charstring encryption, Type 1 charstring
encoding) for the Type 1 programs; the CFF programs come from
fontTools. Run from this directory with a Python that has fontTools:

    python3 make_fontmatrix.py
"""

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen


def num(v):
    v = int(round(v))
    if -107 <= v <= 107:
        return bytes([v + 139])
    if 108 <= v <= 1131:
        w = v - 108
        return bytes([(w >> 8) + 247, w & 0xFF])
    if -1131 <= v <= -108:
        w = -v - 108
        return bytes([(w >> 8) + 251, w & 0xFF])
    return b"\xff" + v.to_bytes(4, "big", signed=True)


def encrypt(data, r):
    out = bytearray()
    for c in data:
        e = c ^ (r >> 8)
        out.append(e)
        r = ((e + r) * 52845 + 22719) & 0xFFFF
    return bytes(out)


def charstring(scale):
    s = lambda v: v * scale
    cs = (num(0) + num(s(600)) + b"\x0d"            # hsbw
          + num(0) + b"\x04"                         # vmoveto (from sbx, 0)
          + num(s(500)) + b"\x06"                    # hlineto
          + num(s(700)) + b"\x07"                    # vlineto
          + num(-s(500)) + b"\x06"                   # hlineto
          + b"\x09" + b"\x0e")                       # closepath endchar
    return encrypt(b"\0\0\0\0" + cs, 4330)


def type1(matrix, scale, matrix_in_private=False):
    m = " ".join(repr(x) for x in (matrix or []))
    clear = ("%!PS-AdobeFont-1.0: Test 001\n"
             "11 dict begin\n/FontName /Test def\n/PaintType 0 def\n/FontType 1 def\n"
             + ("" if matrix_in_private or matrix is None else f"/FontMatrix [{m}] readonly def\n")
             + "/FontBBox {0 0 1024 1434} readonly def\n"
             "/Encoding StandardEncoding def\ncurrentdict end\ncurrentfile eexec\n").encode()
    cs_notdef = encrypt(b"\0\0\0\0" + num(0) + num(0) + b"\x0d\x0e", 4330)
    cs_a = charstring(scale)
    private = (b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n"
               b"/ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/lenIV 4 def\n"
               + (f"/FontMatrix [{m}] def\n".encode() if matrix_in_private else b"")
               + b"/Subrs 0 array\nND\n2 index /CharStrings 2 dict dup begin\n"
               + b"/.notdef " + str(len(cs_notdef)).encode() + b" RD " + cs_notdef + b" ND\n"
               + b"/A " + str(len(cs_a)).encode() + b" RD " + cs_a + b" ND\n"
               + b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\n"
               b"mark currentfile closefile\n")
    enc = encrypt(b"\0\0\0\0" + private, 55665)
    trailer = ("0" * 64 + "\n") * 8 + "cleartomark\n"
    return clear + enc + trailer.encode()


def cff(upem, matrix=None):
    fb = FontBuilder(upem, isTTF=False)
    fb.setupGlyphOrder([".notdef", "A"])
    fb.setupCharacterMap({65: "A"})
    s = upem / 1000
    pens = {}
    for g in (".notdef", "A"):
        pen = T2CharStringPen(round(600 * s), None)
        if g == "A":
            pen.moveTo((0, 0))
            pen.lineTo((round(500 * s), 0))
            pen.lineTo((round(500 * s), round(700 * s)))
            pen.lineTo((0, round(700 * s)))
            pen.closePath()
        pens[g] = pen.getCharString()
    fb.setupCFF("Test", {"FullName": "Test"}, pens, {})
    top = fb.font["CFF "].cff.topDictIndex[0]
    if matrix is not None:
        top.FontMatrix = list(matrix)
    fb.setupHorizontalMetrics({".notdef": (round(600 * s), 0), "A": (round(600 * s), 0)})
    fb.setupHorizontalHeader()
    fb.setupNameTable({"familyName": "Test", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    return fb.font["CFF "].compile(fb.font)


if __name__ == "__main__":
    out = {
        "t1_1000.bin": type1([0.001, 0, 0, 0.001, 0, 0], 1),
        "t1_2048.bin": type1([1 / 2048, 0, 0, 1 / 2048, 0, 0], 2.048),
        "t1_2048_private.bin": type1([1 / 2048, 0, 0, 1 / 2048, 0, 0], 2.048, matrix_in_private=True),
        "t1_default.bin": type1(None, 1),
        "t1_oblique.bin": type1([0.001, 0, 0.0005, 0.001, 0, 0], 1),
        "t1_wide.bin": type1([0.002, 0, 0, 0.001, 0, 0], 1),
        "t1_singular.bin": type1([0, 0, 0, 0, 0, 0], 1),
        "cff_1000.bin": cff(1000),
        "cff_2048.bin": cff(2048, [1 / 2048, 0, 0, 1 / 2048, 0, 0]),
        "cff_oblique.bin": cff(1000, [0.001, 0, 0.0005, 0.001, 0, 0]),
    }
    for name, data in out.items():
        open(name, "wb").write(data)
