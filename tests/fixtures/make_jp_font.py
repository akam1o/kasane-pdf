"""Generate `KasaneJP.ttf` — a small subset of **Noto Sans JP**
containing only the glyphs used by the M7.1 Japanese-PDF tests.

## Source font

- **Family**: Noto Sans JP, `Variable/TTF/Subset/NotoSansJP-VF.ttf`
- **Distribution**: notofonts/noto-cjk GitHub release `Sans2.004`
- **URL**:
  https://github.com/notofonts/noto-cjk/releases/download/Sans2.004/02_NotoSansCJK-TTF-VF.zip
  (file `Variable/TTF/Subset/NotoSansJP-VF.ttf`, 9 590 732 bytes)
- **Format**: TrueType (`glyf`, units-per-em 1000), variable font —
  the subset is static (fvar/gvar dropped).
- **License**: SIL Open Font License v1.1 (`LICENSE` inside the zip).
  OFL permits subsetting/modification and redistribution; the only
  Reserved Font Name is `'Source'` (Adobe copyright text) — the
  subset is renamed `KasaneJP` so no reserved name is used.
- **Copyright**: © 2014-2021 Adobe. See `LICENSE` in the source zip.

## Subset characters

Kept glyphs (18 incl. .notdef):

    space  ( )  、 。 「 」  あ ア  日 本 語  A a 1  .

These cover hiragana, katakana, kanji, Japanese punctuation/brackets
and half-width Latin/digit/space — the glyph shapes are the real
Noto outlines, not hand-drawn approximations.

## Generation

Requires fontTools (test-time only):

    python tests/fixtures/make_jp_font.py \
        /path/to/NotoSansJP-VF.ttf KasaneJP.ttf

The committed `KasaneJP.ttf` was produced with fontTools 4.x.
"""

import sys

from fontTools import subset
from fontTools.ttLib import TTFont

TEXT = "あア日本語()、。Aa1 「」."
FAMILY = "KasaneJP"


def main() -> None:
    src, dst = sys.argv[1], sys.argv[2]
    opts = subset.Options()
    opts.notdef_outline = True
    opts.recalc_bounds = True
    opts.recalc_timestamp = False
    # Drop variation + layout tables — the PDF path never uses them
    # and they dominate file size.
    opts.drop_tables += [
        "FFTM", "GPOS", "GSUB", "GDEF", "vhea", "vmtx",
        "gvar", "fvar", "avar", "cvar", "HVAR", "VVAR", "MVAR", "STAT",
    ]
    opts.hinting = False
    opts.desubroutinize = True
    sub = subset.Subsetter(opts)
    font = subset.load_font(src, opts)
    sub.populate(text=TEXT)
    sub.subset(font)
    # Rename — OFL prohibits reusing the Reserved Font Name 'Source'
    # in a modified version; 'Noto Sans JP' is not reserved but we
    # rename anyway so the fixture cannot be mistaken for the full font.
    name = font["name"]
    name.setName(FAMILY, 1, 3, 1, 0x409)
    name.setName(f"{FAMILY} Subset", 3, 3, 1, 0x409)
    name.setName(FAMILY, 4, 3, 1, 0x409)
    name.setName(f"{FAMILY}-Regular", 6, 3, 1, 0x409)
    name.setName(
        'Subset of Noto Sans JP (OFL). Original © 2014-2021 Adobe; '
        'Reserved Font Name "Source" not used in this derivative.',
        0, 3, 1, 0x409,
    )
    font.save(dst)
    print(f"wrote {dst}")


if __name__ == "__main__":
    main()
