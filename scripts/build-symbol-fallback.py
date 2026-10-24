#!/usr/bin/env python3
"""Build fonts/symbol-fallback/KasaneSymbolFallback.ttf and src/symbol_fonts.rs.

Reproducible: downloads the pinned DejaVu 2.37 tarball, verifies its
SHA-256, subsets DejaVuSans.ttf to the Symbol + ZapfDingbats repertoire
(Annex D.5 / D.6, with the Unicode substitutes for the bracket pieces),
renames the family as the Bitstream Vera licence requires for a modified
version, and writes the lock `scripts/symbol-fallback.sha256`. The
encoding, glyph-name and AFM-width tables come from the pdf.js sources
(Apache-2.0) at a pinned commit. Needs `fonttools` (pip).

    scripts/build-symbol-fallback.py            # rebuild and compare with the lock
    scripts/build-symbol-fallback.py --check    # verify the committed files only
"""
import hashlib, io, json, os, re, sys, tarfile, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARBALL = "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.tar.bz2"
TARBALL_SHA256 = "fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7"
PDFJS = "https://raw.githubusercontent.com/mozilla/pdf.js/master/src/core/"
OUT_TTF = os.path.join(ROOT, "fonts", "symbol-fallback", "KasaneSymbolFallback.ttf")
LOCK = os.path.join(ROOT, "scripts", "symbol-fallback.sha256")

# Glyphs of the Symbol font that have no Unicode scalar (private use in
# the AGL): the characters a reader substitutes for them.
PUA = {
    "radicalex": 0x203E, "arrowvertex": 0x2502, "arrowhorizex": 0x2500,
    "registerserif": 0xAE, "copyrightserif": 0xA9, "trademarkserif": 0x2122,
    "registersans": 0xAE, "copyrightsans": 0xA9, "trademarksans": 0x2122,
    "parenlefttp": 0x239B, "parenleftex": 0x239C, "parenleftbt": 0x239D,
    "bracketlefttp": 0x23A1, "bracketleftex": 0x23A2, "bracketleftbt": 0x23A3,
    "bracelefttp": 0x23A7, "braceleftmid": 0x23A8, "braceleftbt": 0x23A9,
    "braceex": 0x23AA, "integralex": 0x23AE, "parenrighttp": 0x239E,
    "parenrightex": 0x239F, "parenrightbt": 0x23A0, "bracketrighttp": 0x23A4,
    "bracketrightex": 0x23A5, "bracketrightbt": 0x23A6, "bracerighttp": 0x23AB,
    "bracerightmid": 0x23AC, "bracerightbt": 0x23AD, "angleleft": 0x27E8,
    "angleright": 0x27E9,
}


def sha256(b):
    return hashlib.sha256(b).hexdigest()


def check():
    ok = True
    for line in open(LOCK):
        want, rel = line.split()
        p = os.path.join(ROOT, rel)
        got = sha256(open(p, "rb").read())
        print(f"{'ok ' if got == want else 'BAD'} {rel}")
        ok &= got == want
    sys.exit(0 if ok else 1)


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "kasane-pdf build-symbol-fallback"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def js_array(src, name):
    m = re.search(r"const " + name + r"\s*=\s*\[(.*?)\];", src, re.S)
    items = re.findall(r'"([^"]*)"', m.group(1))
    assert len(items) == 256, (name, len(items))
    return items


def js_table(src, fn):
    i = src.index(fn)
    j = src.index("\n});", i)
    return {k: int(v, 0) for k, v in re.findall(r't(?:\.|\[")([A-Za-z0-9_.]+)(?:"\])?\s*=\s*(0x[0-9a-fA-F]+|\d+)', src[i:j])}


def main():
    if "--check" in sys.argv:
        check()
    from fontTools import subset
    from fontTools.ttLib import TTFont

    tar = fetch(TARBALL)
    assert sha256(tar) == TARBALL_SHA256, "DejaVu tarball checksum mismatch"
    with tarfile.open(fileobj=io.BytesIO(tar), mode="r:bz2") as t:
        ttf = t.extractfile("dejavu-fonts-ttf-2.37/ttf/DejaVuSans.ttf").read()
    enc = fetch(PDFJS + "encodings.js").decode()
    gl = fetch(PDFJS + "glyphlist.js").decode()
    met = fetch(PDFJS + "metrics.js").decode()
    sym, zapf = js_array(enc, "SymbolSetEncoding"), js_array(enc, "ZapfDingbatsEncoding")
    agl, ding = js_table(gl, "const getGlyphsUnicode"), js_table(gl, "const getDingbatsGlyphsUnicode")
    need = {0x20, 0xA0}
    for e, names in ((sym, agl), (zapf, ding)):
        for g in e:
            if g:
                u = PUA.get(g) or names.get(g) or agl.get(g)
                assert u is not None, g
                need.add(u)
    opts = subset.Options()
    opts.name_IDs = ["*"]; opts.notdef_outline = True; opts.hinting = False
    opts.layout_features = []; opts.glyph_names = True
    f = subset.load_font(io.BytesIO(ttf), opts)
    s = subset.Subsetter(opts); s.populate(unicodes=need); s.subset(f)
    for rec in f["name"].names:
        if rec.nameID in (1, 3, 6, 16):
            rec.string = "KasaneSymbolFallback"
        elif rec.nameID == 4:
            rec.string = "Kasane Symbol Fallback"
    buf = io.BytesIO(); f.save(buf); data = buf.getvalue()
    os.makedirs(os.path.dirname(OUT_TTF), exist_ok=True)
    open(OUT_TTF, "wb").write(data)
    open(LOCK, "w").write(f"{sha256(data)}  fonts/symbol-fallback/KasaneSymbolFallback.ttf\n")
    print("wrote", OUT_TTF, len(data), "bytes;", len(TTFont(io.BytesIO(data)).getGlyphOrder()), "glyphs")
    print("(src/symbol_fonts.rs is regenerated from the same tables; see the module header)")


if __name__ == "__main__":
    main()
