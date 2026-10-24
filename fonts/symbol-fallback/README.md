# Symbol / ZapfDingbats fallback face

`KasaneSymbolFallback.ttf` is a 414-glyph subset of **DejaVu Sans 2.37**
(`DejaVuSans.ttf`, from `dejavu-fonts-ttf-2.37.tar.bz2`, SHA-256
`fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7`), covering the glyph repertoire of the Symbol and
ZapfDingbats Standard 14 fonts (Annex D.5 / D.6) with the Unicode
substitutes for their bracket pieces. Hinting and layout tables are
dropped; the family name is changed to *Kasane Symbol Fallback* as the
Bitstream Vera licence requires for a modified version.

Licence: Bitstream Vera Fonts Copyright (the DejaVu additions are in the
public domain) — see `LICENSE` in this directory and `NOTICE`.

Reproducible: `scripts/build-symbol-fallback.py` downloads the pinned
tarball, verifies its SHA-256, builds the subset with fontTools and
regenerates `src/symbol_fonts.rs`; `scripts/symbol-fallback.sha256` is
the lock of the committed files.

The renderer uses this face only for a *non-embedded* Symbol or
ZapfDingbats font, reports the substitution on
`RenderedPage::font_substitutions()` as `SubstKind::SymbolFallback`,
and takes advance widths from the PDF `/Widths` or, failing that, the
Adobe AFM metrics — never from this face.
