# Bundled substitute fonts — Liberation 2.1.5

These twelve TrueType files are the **Liberation Fonts**, version
2.1.5, used unmodified as the engine's substitutes for non-embedded
*Standard 14*-style fonts (`src/fontsubst.rs`):

| PDF base font family | substitute |
|---|---|
| Helvetica, Arial | Liberation Sans |
| Times, Times New Roman | Liberation Serif |
| Courier, Courier New | Liberation Mono |

Liberation Sans/Serif/Mono are metric-compatible with Arial / Times
New Roman / Courier New (and therefore with Helvetica / Times /
Courier), so a PDF that omits `/Widths` for a Standard 14 font lays
out with the same advances other viewers use. Glyph *shapes* differ
from Adobe's originals — the renderer reports every substitution in
`RenderedPage::font_substitutions` so output produced this way is
never mistaken for a faithful render of the original font.

**Symbol and ZapfDingbats have no licence-clean metric-compatible
clone**; non-embedded uses of them are refused with
`Error::Unsupported`, not approximated.

## Provenance

- Source: <https://github.com/liberationfonts/liberation-fonts>,
  release 2.1.5 (`liberation-fonts-ttf-2.1.5.tar.gz`), downloaded from
  <https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz>
- Tarball SHA-256:
  `7191c669bf38899f73a2094ed00f7b800553364f90e2637010a69c0e268f25d0`
- Files are byte-identical to the release; no subsetting or renaming
  was done (the OFL *Reserved Font Name* "Liberation" therefore
  applies unchanged).

## Licence

SIL Open Font License, Version 1.1 — see [`LICENSE`](LICENSE) in this
directory (copied verbatim from the release, together with
[`AUTHORS`](AUTHORS)). The OFL permits bundling the fonts in software
that is sold, provided the fonts themselves are not sold on their own
and the licence text accompanies them; it is compatible with both the
open-source release of this crate and commercial use. The licence
applies to the font files only, not to the crate's code.

The fonts are compiled into the library with `include_bytes!`; no
font is ever looked up on the host operating system, so rendering is
reproducible across machines.
