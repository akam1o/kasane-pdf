"""Synthetic PDFs of non-embedded Courier-family text (no corpus content).

    scripts/courier-reference/synth.py OUTDIR

One single-page PDF per case; every font is non-embedded. Cases are named
<group>-<detail>; the group says what the case isolates.
"""
import os
import sys

OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)

ASCII = "The quick brown fox jumps over the lazy dog."
UPPER = "PACK MY BOX WITH FIVE DOZEN LIQUOR JUGS."
DIGITS = "0123456789 3.14159 2,718.28 -42 +7 1/2"
SYMBOLS = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"
PROSE = [
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do",
    "eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim",
    "ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut",
    "aliquip ex ea commodo consequat. Duis aute irure dolor in",
    "reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla",
    "pariatur. Excepteur sint occaecat cupidatat non proident, sunt in",
    "culpa qui officia deserunt mollit anim id est laborum.",
    "INT. KITCHEN - NIGHT            (V.O.)            CUT TO:",
    "| col 1  | col 2  | col 3  |    aligned   1234.50",
    "| abc    | defghi | jk     |    columns     67.00",
]


def esc(s):
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def pdf(fonts, content, size=(612, 792)):
    """fonts: {resource name: font dict text}; content: bytes."""
    objs = []
    font_refs = []
    for i, (name, d) in enumerate(fonts.items()):
        if not name.startswith("_"):  # "_X": an object that is no font resource
            font_refs.append(f"/{name} {5 + i} 0 R")
    objs.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    objs.append(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
    objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {size[0]} {size[1]}] "
                f"/Resources << /Font << {' '.join(font_refs)} >> >> /Contents 4 0 R >>".encode())
    objs.append(b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream")
    for d in fonts.values():
        objs.append(d.encode())
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offs = []
    for i, o in enumerate(objs):
        offs.append(len(out))
        out += f"{i + 1} 0 obj\n".encode() + o + b"\nendobj\n"
    x = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    for o in offs:
        out += f"{o:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()
    return bytes(out)


def type1(base, enc="/WinAnsiEncoding", widths=False):
    w = " /FirstChar 32 /LastChar 255 /Widths [" + " ".join(["600"] * 224) + "]" if widths else ""
    e = f" /Encoding {enc}" if enc else ""
    return f"<< /Type /Font /Subtype /Type1 /BaseFont /{base}{e}{w} >>"


def lines(font, size, x, y, texts, lead=None, pre=""):
    lead = lead or size * 1.2
    s = f"BT /{font} {size} Tf {lead} TL {pre} {x} {y} Td "
    for t in texts:
        s += f"({esc(t)}) Tj T* "
    return s + "ET\n"


STYLES = {"regular": "Courier", "bold": "Courier-Bold", "oblique": "Courier-Oblique",
          "boldoblique": "Courier-BoldOblique"}
cases = {}

for style, base in STYLES.items():
    body = lines("F1", 12, 50, 740, [ASCII, UPPER, DIGITS, SYMBOLS] + PROSE)
    body += lines("F1", 24, 50, 400, [ASCII[:30], UPPER[:30]])
    cases[f"style-{style}"] = pdf({"F1": type1(base)}, body.encode())

cases["chars-ascii"] = pdf({"F1": type1("Courier")}, lines("F1", 14, 40, 740, [
    "".join(chr(c) for c in range(32, 80)), "".join(chr(c) for c in range(80, 127))] * 3).encode())
cases["chars-digits"] = pdf({"F1": type1("Courier")},
                            lines("F1", 18, 40, 740, [DIGITS, "00000 11111 88888 69690 12345 67890"] * 4).encode())
cases["chars-symbols"] = pdf({"F1": type1("Courier")}, lines("F1", 18, 40, 740, [SYMBOLS, SYMBOLS[::-1]] * 4).encode())
cases["text-long"] = pdf({"F1": type1("Courier")}, lines("F1", 10, 36, 760, PROSE * 6, lead=12).encode())
cases["spacing-tc"] = pdf({"F1": type1("Courier")}, (lines("F1", 12, 40, 740, [ASCII], pre="2 Tc")
                                                    + lines("F1", 12, 40, 700, [ASCII], pre="-1 Tc")
                                                    + lines("F1", 12, 40, 660, [ASCII], pre="0 Tc")).encode())
cases["spacing-tw"] = pdf({"F1": type1("Courier")}, (lines("F1", 12, 40, 740, [ASCII], pre="6 Tw")
                                                    + lines("F1", 12, 40, 700, [ASCII], pre="0 Tw")).encode())
cases["spacing-tz"] = pdf({"F1": type1("Courier")}, (lines("F1", 12, 40, 740, [ASCII], pre="150 Tz")
                                                    + lines("F1", 12, 40, 700, [ASCII], pre="60 Tz")
                                                    + lines("F1", 12, 40, 660, [ASCII], pre="100 Tz")).encode())
cases["transform-rotate"] = pdf({"F1": type1("Courier")}, (
    "BT /F1 16 Tf 0.866 0.5 -0.5 0.866 150 300 Tm (" + esc(ASCII) + ") Tj ET\n"
    "BT /F1 16 Tf 0 1 -1 0 500 150 Tm (" + esc(UPPER) + ") Tj ET\n").encode())
cases["transform-scale"] = pdf({"F1": type1("Courier")}, (
    "q 2 0 0 2 0 0 cm " + lines("F1", 10, 20, 350, [ASCII[:28], DIGITS]) + "Q\n"
    "q 0.5 0 0 0.5 0 0 cm " + lines("F1", 20, 80, 600, PROSE[:4]) + "Q\n").encode())
cases["sizes"] = pdf({"F1": type1("Courier")}, "".join(
    lines("F1", s, 30, y, [ASCII[:40]]) for s, y in ((6, 760), (8, 740), (10, 715), (12, 690), (16, 660),
                                                    (24, 620), (36, 560), (48, 490), (72, 400))).encode())
cases["enc-standard"] = pdf({"F1": type1("Courier", enc=None)}, lines("F1", 14, 40, 740, [
    ASCII, "quotes `x' and ``y''", "".join(chr(c) for c in range(0xA1, 0xC0))]).encode("latin-1"))
cases["enc-winansi"] = pdf({"F1": type1("Courier")}, lines("F1", 14, 40, 740, [
    ASCII, "".join(chr(c) for c in range(0xC0, 0xE0)), "".join(chr(c) for c in range(0xE0, 0x100)),
    "\x80 \x85 \x89 \x91\x92 \x93\x94 \x96 \x97 \x99"]).encode("latin-1"))
cases["enc-macroman"] = pdf({"F1": type1("Courier", enc="/MacRomanEncoding")}, lines("F1", 14, 40, 740, [
    ASCII, "".join(chr(c) for c in range(0x80, 0xA0)), "".join(chr(c) for c in range(0xA0, 0xC0))]).encode("latin-1"))
diffs = ("<< /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [65 /Aacute /Eogonek /Scaron /Zcaron "
         "/lslash /dotlessi /fi /fl /Euro /trademark /Omega /Delta /afii10017 /afii10018 /Alpha /notaglyph "
         "/uni0416 /uni2603] >>")
cases["enc-differences"] = pdf({"F1": type1("Courier", enc=diffs)},
                               lines("F1", 18, 40, 740, ["ABCDEFGHIJKLMNOPQR", "A B C D E F G H I", "J K L M N O P Q R"])
                               .encode())
cases["widths-present"] = pdf({"F1": type1("Courier", widths=True)}, lines("F1", 12, 40, 740, [ASCII] + PROSE).encode())
cases["overlap-shapes"] = pdf({"F1": type1("Courier")}, (
    "0.8 g 40 690 300 60 re f 0 g 1 w 40 690 m 340 750 l S " + lines("F1", 14, 45, 730, [ASCII[:35], DIGITS])
    + "0 0 1 rg " + lines("F1", 14, 45, 600, [UPPER[:30]]) + "0 g").encode())
cases["mixed-fonts"] = pdf({"F1": type1("Courier"), "F2": type1("Helvetica"), "F3": type1("Times-Roman"),
                            "F4": type1("Courier-Bold")},
                           (lines("F1", 12, 40, 740, [ASCII]) + lines("F2", 12, 40, 720, [ASCII])
                            + lines("F3", 12, 40, 700, [ASCII]) + lines("F4", 12, 40, 680, [ASCII])
                            + "BT /F1 12 Tf 40 650 Td (Courier) Tj /F2 12 Tf (Helvetica) Tj /F4 12 Tf (Bold) Tj "
                              "/F3 12 Tf (Times) Tj ET\n").encode())
# Courier New as a non-embedded TrueType (an alias), and an unknown
# fixed-pitch font (Generic: unchanged in B by design).
W95 = " /FirstChar 32 /LastChar 126 /Widths [" + " ".join(["600"] * 95) + "]"
DESC = ("<< /Type /FontDescriptor /FontName /{n} /Flags 33 /FontBBox [-21 -680 638 1021] /ItalicAngle 0 "
        "/Ascent 833 /Descent -300 /CapHeight 571 /StemV 80 >>")


def pdf_desc(base, body):
    """A non-embedded TrueType font with a descriptor (object 6)."""
    return pdf({"F1": f"<< /Type /Font /Subtype /TrueType /BaseFont /{base} /Encoding /WinAnsiEncoding{W95} "
                      "/FontDescriptor 6 0 R >>", "_D": DESC.format(n=base)}, body.encode())


cases["alias-couriernew"] = pdf_desc("CourierNewPSMT", lines("F1", 12, 40, 740, [ASCII, DIGITS, SYMBOLS] + PROSE[:4]))
cases["generic-fixedpitch"] = pdf_desc("SomeTypewriterMono", lines("F1", 12, 40, 740, [ASCII, DIGITS]))
cases["unrelated-helvetica"] = pdf({"F1": type1("Helvetica"), "F2": type1("Times-Bold")},
                                   (lines("F1", 12, 40, 740, [ASCII] + PROSE[:5])
                                    + lines("F2", 14, 40, 600, [UPPER])).encode())
# Bad font names: not a Courier, must not pick it up.
cases["bad-names"] = pdf({"F1": type1("Courierish-Black"), "F2": type1("Cour"), "F3": type1("Courier-Unknown-Style")},
                         (lines("F1", 12, 40, 740, [ASCII]) + lines("F2", 12, 40, 720, [ASCII])
                          + lines("F3", 12, 40, 700, [ASCII])).encode())

for name, data in cases.items():
    open(os.path.join(OUT, name + ".pdf"), "wb").write(data)
print(len(cases), "cases")
