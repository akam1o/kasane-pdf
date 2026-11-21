#!/usr/bin/env python3
"""Build fonts/kasane-courier/KasaneCourier-*.ttf: Courier Prime 3.018 with
the characters it lacks taken from Liberation Mono 2.1.5.

The substitute for non-embedded Courier and Courier New
(src/fontsubst.rs). Courier Prime draws what it has; every character
Liberation Mono (the previous substitute) draws and Courier Prime lacks
— Greek, Cyrillic, Latin Extended-B, more punctuation — comes from
Liberation Mono unchanged, so no character drawn before is lost.

No network. The inputs are local files checked against pinned SHA-256:

- Courier Prime 3.018 (2015 release, OFL 1.1, no Reserved Font Name),
  the four TTFs of github.com/quoteunquoteapps/CourierPrime at commit
  7fd585a2dd4c1612c79b3308e300923d1c13df93, `fonts/ttf/` (identical to
  Google Fonts' `ofl/courierprime/`):

      c=https://raw.githubusercontent.com/quoteunquoteapps/CourierPrime/7fd585a2dd4c1612c79b3308e300923d1c13df93
      for s in Regular Bold Italic BoldItalic; do curl -sSLO $c/fonts/ttf/CourierPrime-$s.ttf; done
      curl -sSLO $c/OFL.txt

- Liberation Mono 2.1.5: `fonts/liberation/LiberationMono-*.ttf` in this
  repository (unmodified release files).

What is changed, per style:

- hinting, the layout tables (GSUB, GDEF, GPOS), DSIG and the glyph
  names (`post` format 3) are dropped — the renderer uses none of them,
  glyphs are found by character, and a signature no longer holds;
- Liberation Mono's glyphs for the missing characters are added with
  their outlines unchanged;
- every character's advance is Liberation Mono's (1229 / 2048 em,
  zero for its combining marks) — Courier Prime's own is 1228 — so a
  name the Adobe AFM widths lack is laid out exactly as before (the
  three control characters only Courier Prime maps keep 1229);
- the names are Kasane Courier (no Reserved Font Name of either source:
  Liberation, Arimo, Tinos, Cousine; Courier Prime 3.018 reserves none),
  with both copyright notices and the OFL statement.

Byte-reproducible with the same fontTools version (FONTTOOLS below):
timestamps are Courier Prime's, tables are written in a fixed order.
`fonts/kasane-courier/LICENSE` is the OFL 1.1 text under both sources'
copyright notices. `scripts/kasane-courier.sha256` locks the inputs and
the outputs.

    scripts/build-courier-fallback.py --courier-prime DIR   # build, then compare with the lock
    scripts/build-courier-fallback.py --check               # verify the committed outputs only
"""
import argparse, hashlib, io, os, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "fonts", "kasane-courier")
LOCK = os.path.join(ROOT, "scripts", "kasane-courier.sha256")
FONTTOOLS = "4.68.0"
STYLES = {  # Kasane Courier style: (Courier Prime, Liberation Mono, name table subfamily)
    "Regular": ("CourierPrime-Regular.ttf", "LiberationMono-Regular.ttf", "Regular"),
    "Bold": ("CourierPrime-Bold.ttf", "LiberationMono-Bold.ttf", "Bold"),
    "Italic": ("CourierPrime-Italic.ttf", "LiberationMono-Italic.ttf", "Italic"),
    "BoldItalic": ("CourierPrime-BoldItalic.ttf", "LiberationMono-BoldItalic.ttf", "Bold Italic"),
}
INPUTS = {
    "CourierPrime-Regular.ttf": "72f793376f8e2841656bf21d77a5de010f2929bd6956a22ee848ad0c7eb978af",
    "CourierPrime-Bold.ttf": "ff1f38786c849d1c41fa8e447960abdb2bd75fdfb0cfcdeb524fad65a5af3638",
    "CourierPrime-Italic.ttf": "f1b9a5829789f7e56432a9f3bc7665ef4531dbba1c112639e48bef39621a006b",
    "CourierPrime-BoldItalic.ttf": "3355273f3ea6d6362658f9564f4e83d59e156cf1a182e3c1592b5c5943627dcc",
    # The release's licence (the repository's OFL.txt at the same commit).
    "OFL.txt": "9a755af092b494944c99f471be6fddd19b006a448fefdc4717e4ee0aa09a97b0",
}
# fonts/liberation/ (the files of liberation-fonts-ttf-2.1.5.tar.gz, SHA-256
# 7191c669bf38899f73a2094ed00f7b800553364f90e2637010a69c0e268f25d0).
LIBERATION = {
    "LiberationMono-Regular.ttf": "f2b83c763e8afd21709333370bed4774337fae82267937e2b5aea7e2fbd922c1",
    "LiberationMono-Bold.ttf": "bd62a0672d0b9b6710b01df434c80ad54fa5f0835207eb7b17b7a761463067bb",
    "LiberationMono-Italic.ttf": "605c01c711b44480a7508d349dfbf3264e81fa43d69e61cfa7d10b86e764c4d1",
    "LiberationMono-BoldItalic.ttf": "79451f3c09fe25116098853b7a2ca6e2436220ccc11af022979adbcf195be130",
}
COPYRIGHT = ("Copyright 2015 The Courier Prime Project Authors (https://github.com/quoteunquoteapps/CourierPrime). "
             "Digitized data copyright (c) 2010 Google Corporation with Reserved Font Arimo, Tinos and Cousine. "
             "Copyright (c) 2012 Red Hat, Inc. with Reserved Font Name Liberation.")
DESCRIPTION = ("Kasane Courier: Courier Prime 3.018 with the characters it lacks from Liberation Mono 2.1.5 "
               "(outlines unchanged; advances, hinting, layout tables and names changed), built for kasane-pdf "
               "by scripts/build-courier-fallback.py.")
LICENSE_DESC = ("This Font Software is licensed under the SIL Open Font License, Version 1.1. "
                "This license is available with a FAQ at: https://openfontlicense.org")
LICENSE_URL = "https://openfontlicense.org"


def sha256(b):
    return hashlib.sha256(b).hexdigest()


def check():
    ok = True
    for line in open(LOCK):
        want, rel = line.split()
        if not rel.startswith("fonts/"):
            continue
        got = sha256(open(os.path.join(ROOT, rel), "rb").read())
        print(f"{'ok ' if got == want else 'BAD'} {rel}")
        ok &= got == want
    return ok


def outlines(font, glyph):
    from fontTools.pens.recordingPen import DecomposingRecordingPen
    gs = font.getGlyphSet()
    pen = DecomposingRecordingPen(gs)
    gs[glyph].draw(pen)
    return pen.value


def prefix_names(font, prefix):
    """Every glyph but .notdef renamed `prefix` + name, components and
    cmap with it: the merge would otherwise take one font's glyph for
    the other's of the same name (a component of a composite, a glyph
    both fonts name alike)."""
    order = font.getGlyphOrder()
    new = {g: g if g == ".notdef" else prefix + g for g in order}
    glyf, hmtx = font["glyf"], font["hmtx"]
    glyphs = {}
    for g in order:
        gl = glyf[g]
        if gl.isComposite():
            for c in gl.components:
                c.glyphName = new[c.glyphName]
        glyphs[new[g]] = gl
    hmtx.metrics = {new[g]: m for g, m in hmtx.metrics.items()}
    for t in font["cmap"].tables:
        t.cmap = {u: new[g] for u, g in t.cmap.items()}
    glyf.glyphs = glyphs
    glyf.glyphOrder = [new[g] for g in order]
    font.setGlyphOrder([new[g] for g in order])


def build(style, cp_path, lm_path, subfamily):
    from fontTools import subset
    from fontTools.merge import Merger
    from fontTools.ttLib import TTFont

    cp_src, lm_src = TTFont(cp_path), TTFont(lm_path)
    cp_cmap, lm_cmap = cp_src.getBestCmap(), lm_src.getBestCmap()
    missing = sorted(set(lm_cmap) - set(cp_cmap))
    lm_adv = {u: lm_src["hmtx"][g][0] for u, g in lm_cmap.items()}

    opts = subset.Options()
    opts.hinting = False
    opts.layout_features = []
    opts.drop_tables += ["GSUB", "GPOS", "GDEF", "DSIG", "FFTM"]
    opts.glyph_names = True
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.recalc_timestamp = False
    parts = []
    for path, unicodes, donor in ((cp_path, sorted(cp_cmap), False), (lm_path, missing, True)):
        f = TTFont(path, recalcTimestamp=False)
        o = subset.Options(**vars(opts))
        # Each part keeps its .notdef: the merge takes glyph 0 of every
        # part as one, so a part without it would lose its first glyph.
        o.notdef_glyph = True
        s = subset.Subsetter(o)
        s.populate(unicodes=unicodes)
        s.subset(f)
        if donor:
            prefix_names(f, "lm.")
        buf = io.BytesIO()
        f.save(buf, reorderTables=True)
        buf.seek(0)
        parts.append(buf)
    merged = Merger().merge(parts)
    out_buf = io.BytesIO()
    merged.save(out_buf, reorderTables=True)
    out_buf.seek(0)
    # Not recalculated on save: the sources' left side bearings are kept
    # as they are (the renderer places an outline by its bearing).
    font = TTFont(out_buf, recalcTimestamp=False, recalcBBoxes=False)

    cmap = font.getBestCmap()
    if set(cmap) != set(cp_cmap) | set(lm_cmap):
        want = set(cp_cmap) | set(lm_cmap)
        raise SystemExit(f"{style}: the merged cmap is not Courier Prime's and Liberation Mono's: "
                         f"missing {sorted(hex(u) for u in want - set(cmap))[:8]}, extra {sorted(hex(u) for u in set(cmap) - want)[:8]}")
    # Advances: Liberation Mono's per character; the unencoded glyphs
    # (.notdef, components) the common 1229.
    hmtx = font["hmtx"]
    for g, (adv, lsb) in list(hmtx.metrics.items()):
        if adv:
            hmtx.metrics[g] = (1229, lsb)
    for u, g in cmap.items():
        src, sg = (cp_src, cp_cmap[u]) if u in cp_cmap else (lm_src, lm_cmap[u])
        hmtx.metrics[g] = (lm_adv.get(u, hmtx.metrics[g][0]), src["hmtx"][sg][1])
        # The glyph header box, as the source stores it: a reader places
        # a glyph by its bearing minus the header's xMin.
        mg, sgl = font["glyf"][g], src["glyf"][sg]
        if hasattr(sgl, "xMin"):
            mg.xMin, mg.yMin, mg.xMax, mg.yMax = sgl.xMin, sgl.yMin, sgl.xMax, sgl.yMax
    font["hhea"].advanceWidthMax = max(a for a, _ in hmtx.metrics.values())
    # Outlines and bearings: every character's as its source has them.
    for u, g in cmap.items():
        src, sg = (cp_src, cp_cmap[u]) if u in cp_cmap else (lm_src, lm_cmap[u])
        if outlines(font, g) != outlines(src, sg):
            raise SystemExit(f"{style}: U+{u:04X} is not drawn as its source draws it")

    head = font["head"]
    head.created, head.modified = cp_src["head"].created, cp_src["head"].modified
    head.fontRevision = 1.0
    name = font["name"]
    name.names = []
    full = "Kasane Courier" if subfamily == "Regular" else f"Kasane Courier {subfamily}"
    ps = f"KasaneCourier-{style}"
    for nid, value in ((0, COPYRIGHT), (1, "Kasane Courier"), (2, subfamily), (3, f"{ps} 1.000"), (4, full),
                       (5, "Version 1.000; Courier Prime 3.018, Liberation Mono 2.1.5"), (6, ps),
                       (10, DESCRIPTION), (13, LICENSE_DESC), (14, LICENSE_URL)):
        name.setName(value, nid, 3, 1, 0x409)
        name.setName(value, nid, 1, 0, 0)
    font["OS/2"].achVendID = "NONE"
    font["post"].isFixedPitch = 1
    # No glyph names: glyphs are found by character (cmap); the merge's
    # renamed ones would only add bytes.
    font["post"].formatType = 3.0
    buf = io.BytesIO()
    font.save(buf, reorderTables=True)
    data = buf.getvalue()
    verify(style, data, cp_src, lm_src)
    return data


def verify(style, data, cp_src, lm_src):
    """The bytes written, read back: every character of either source,
    with its source's outline, bearing and header box, and Liberation
    Mono's advance."""
    from fontTools.ttLib import TTFont
    font = TTFont(io.BytesIO(data))
    cmap, cp_cmap, lm_cmap = font.getBestCmap(), cp_src.getBestCmap(), lm_src.getBestCmap()
    if set(cmap) != set(cp_cmap) | set(lm_cmap):
        raise SystemExit(f"{style}: the font written does not map every character of both sources")
    for u, g in cmap.items():
        src, sg = (cp_src, cp_cmap[u]) if u in cp_cmap else (lm_src, lm_cmap[u])
        a, b = font["glyf"][g], src["glyf"][sg]
        box = lambda x: tuple(getattr(x, k, None) for k in ("xMin", "yMin", "xMax", "yMax"))
        adv = lm_src["hmtx"][lm_cmap[u]][0] if u in lm_cmap else 1229
        if (outlines(font, g) != outlines(src, sg) or font["hmtx"][g][1] != src["hmtx"][sg][1]
                or box(a) != box(b) or font["hmtx"][g][0] != adv):
            raise SystemExit(f"{style}: U+{u:04X} is not written as its source has it")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--courier-prime", help="directory with the four Courier Prime 3.018 TTFs")
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    if a.check:
        sys.exit(0 if check() else 1)
    if not a.courier_prime:
        ap.error("--courier-prime DIR is required to build")
    import fontTools
    if fontTools.version != FONTTOOLS:
        raise SystemExit(f"fontTools {FONTTOOLS} builds the locked bytes; this is {fontTools.version}")
    exists = os.path.exists(LOCK)
    for f, want in INPUTS.items():
        p = os.path.join(a.courier_prime, f)
        if sha256(open(p, "rb").read()) != want:
            raise SystemExit(f"{p}: not Courier Prime 3.018 (SHA-256)")
    for f, want in LIBERATION.items():
        if sha256(open(os.path.join(ROOT, "fonts", "liberation", f), "rb").read()) != want:
            raise SystemExit(f"fonts/liberation/{f}: not Liberation Mono 2.1.5 (SHA-256)")
    os.makedirs(OUT, exist_ok=True)
    lines = [f"{v}  courier-prime/{k}" for k, v in INPUTS.items()]
    lines += [f"{v}  fonts/liberation/{k}" for k, v in LIBERATION.items()]
    # The licence: the OFL text of the Courier Prime release under the
    # copyright notices of both sources.
    ofl = open(os.path.join(a.courier_prime, "OFL.txt"), encoding="utf-8").read()
    body = ofl[ofl.index("This Font Software is licensed"):]
    lic = ("Copyright 2015 The Courier Prime Project Authors (https://github.com/quoteunquoteapps/CourierPrime).\n"
           "Digitized data copyright (c) 2010 Google Corporation\n"
           "\twith Reserved Font Arimo, Tinos and Cousine.\n"
           "Copyright (c) 2012 Red Hat, Inc.\n"
           "\twith Reserved Font Name Liberation.\n\n"
           "Kasane Courier is a Modified Version of Courier Prime 3.018 and Liberation Mono 2.1.5\n"
           "(see README.md); it uses none of the Reserved Font Names above.\n\n" + body)
    open(os.path.join(OUT, "LICENSE"), "w", encoding="utf-8").write(lic)
    lines.append(f"{sha256(lic.encode())}  fonts/kasane-courier/LICENSE")
    for style, (cp, lm, sub) in STYLES.items():
        data = build(style, os.path.join(a.courier_prime, cp), os.path.join(ROOT, "fonts", "liberation", lm), sub)
        rel = f"fonts/kasane-courier/KasaneCourier-{style}.ttf"
        open(os.path.join(ROOT, rel), "wb").write(data)
        lines.append(f"{sha256(data)}  {rel}")
        print(f"{rel}: {len(data)} bytes")
    text = "\n".join(lines) + "\n"
    if exists and open(LOCK).read() != text:
        print("the build differs from scripts/kasane-courier.sha256:")
        sys.stdout.write(text)
        sys.exit(1)
    if not exists:
        open(LOCK, "w").write(text)
    print("reproduced the lock" if exists else "wrote the lock")


if __name__ == "__main__":
    main()
