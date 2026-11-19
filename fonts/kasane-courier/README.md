# Courier substitute — Kasane Courier

`KasaneCourier-{Regular,Bold,Italic,BoldItalic}.ttf` are the engine's
substitutes for non-embedded **Courier** and **Courier New** (a Standard
14 name or its alias, `src/fontsubst.rs`); another fixed-pitch font
keeps Liberation Mono (`fonts/liberation/`).

Each is **Courier Prime 3.018** with every character it lacks taken
from **Liberation Mono 2.1.5**, the previous Courier substitute:

| | |
|---|---|
| characters | 2,308 per style: Courier Prime's 383 and the 1,925 more Liberation Mono has (Greek, Cyrillic, Latin Extended-B, punctuation …) — every character Liberation Mono drew is drawn |
| outlines and bearings | unchanged from the source of each character |
| advances | Liberation Mono's (1229 / 2048 em; Courier Prime's own are 1228): a glyph name the Adobe AFM widths lack is laid out as before |
| removed | hinting, GSUB / GDEF / GPOS, DSIG, glyph names (`post` format 3) — the renderer uses none of them |
| names | family *Kasane Courier*; none of the Reserved Font Names of the sources (Liberation, Arimo, Tinos, Cousine; Courier Prime 3.018 reserves none) |

Courier Prime is a Courier design (slab serifs, Courier's proportions);
Liberation Mono's is sans-serif. Readers draw a non-embedded Courier
with a Courier design too (Apple Courier, Nimbus Mono PS, Foxit
Fixed), so the substitute is now closer to what they show. Shapes are
still not Adobe's, and the substitution is reported in
`RenderedPage::font_substitutions` as before.

## Provenance

- Courier Prime 3.018 — <https://github.com/quoteunquoteapps/CourierPrime>
  at commit `7fd585a2dd4c1612c79b3308e300923d1c13df93`, `fonts/ttf/`
  (byte-identical to Google Fonts' `ofl/courierprime/`), with the
  repository's `OFL.txt`. This 2015 release declares no Reserved Font
  Name; the older 2013 download from quoteunquoteapps.com (which
  reserves "Courier Prime") is not used.

  | file | SHA-256 |
  |---|---|
  | `CourierPrime-Regular.ttf` | `72f793376f8e2841656bf21d77a5de010f2929bd6956a22ee848ad0c7eb978af` |
  | `CourierPrime-Bold.ttf` | `ff1f38786c849d1c41fa8e447960abdb2bd75fdfb0cfcdeb524fad65a5af3638` |
  | `CourierPrime-Italic.ttf` | `f1b9a5829789f7e56432a9f3bc7665ef4531dbba1c112639e48bef39621a006b` |
  | `CourierPrime-BoldItalic.ttf` | `3355273f3ea6d6362658f9564f4e83d59e156cf1a182e3c1592b5c5943627dcc` |
  | `OFL.txt` | `9a755af092b494944c99f471be6fddd19b006a448fefdc4717e4ee0aa09a97b0` |

- Liberation Mono 2.1.5 — `fonts/liberation/LiberationMono-*.ttf` in
  this repository (see `fonts/liberation/README.md`).

## Rebuilding

`scripts/build-courier-fallback.py` builds the four files and the
licence from those inputs, without network access, and compares them
with the lock `scripts/kasane-courier.sha256` (inputs and outputs). It
needs fontTools 4.68.0 (the version the locked bytes were built with):

```console
python3 -m venv /tmp/kc && /tmp/kc/bin/pip install fonttools==4.68.0
mkdir /tmp/cp && cd /tmp/cp
c=https://raw.githubusercontent.com/quoteunquoteapps/CourierPrime/7fd585a2dd4c1612c79b3308e300923d1c13df93
for s in Regular Bold Italic BoldItalic; do curl -sSLO $c/fonts/ttf/CourierPrime-$s.ttf; done
curl -sSLO $c/OFL.txt
cd - && /tmp/kc/bin/python scripts/build-courier-fallback.py --courier-prime /tmp/cp   # "reproduced the lock"
python3 scripts/build-courier-fallback.py --check                                       # the committed files only
```

The build checks each input's SHA-256, that the merged fonts map
exactly the characters of both sources, and that every character is
drawn with its source's outline and bearing and Liberation Mono's
advance; it fails otherwise.

## Licence

SIL Open Font License, Version 1.1 — [`LICENSE`](LICENSE), under the
copyright notices of both sources (Copyright 2015 The Courier Prime
Project Authors; Digitized data copyright (c) 2010 Google Corporation;
Copyright (c) 2012 Red Hat, Inc.). Kasane Courier is a Modified Version
of both fonts under that licence; it is never sold on its own. The
licence applies to the font files only, not to the crate's code.
