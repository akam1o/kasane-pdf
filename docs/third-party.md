# Third-party components and licences

kasane-pdf itself is Copyright 2026 akam1o and licensed under
the Apache License, Version 2.0 (`LICENSE`, `NOTICE`). This file lists
everything else that ships with or is
linked into it, with the licence each component is used under.
Regenerate the crate table from `cargo metadata` after changing
dependencies; the policy that governs additions is in
[`architecture.md`](architecture.md) § Licensing policy.

## Rust crates linked into the engine

Every crate in the dependency graph of `kasane-pdf` (default
features, normal dependencies, as resolved by `Cargo.lock`). All are
permissively licensed; where a crate offers a choice (`A OR B`) it
is used under the Apache-2.0 or MIT option.

| crate | version | licence | source |
|---|---|---|---|
| `adler2` | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | <https://github.com/oyvindln/adler2> |
| `arrayref` | 0.3.9 | BSD-2-Clause | <https://github.com/droundy/arrayref> |
| `arrayvec` | 0.7.8 | MIT OR Apache-2.0 | <https://github.com/bluss/arrayvec> |
| `bitflags` | 2.13.2 | MIT OR Apache-2.0 | <https://github.com/bitflags/bitflags> |
| `bytemuck` | 1.25.2 | Zlib OR Apache-2.0 OR MIT | <https://github.com/Lokathor/bytemuck> |
| `cfg-if` | 1.0.5 | MIT OR Apache-2.0 | <https://github.com/rust-lang/cfg-if> |
| `crc32fast` | 1.5.2 | MIT OR Apache-2.0 | <https://github.com/srijs/rust-crc32fast> |
| `fdeflate` | 0.3.7 | MIT OR Apache-2.0 | <https://github.com/image-rs/fdeflate> |
| `flate2` | 1.1.10 | MIT OR Apache-2.0 | <https://github.com/rust-lang/flate2-rs> |
| `hayro-ccitt` | 0.4.0 | Apache-2.0 OR MIT | <https://github.com/LaurenzV/hayro> |
| `hayro-jbig2` | 0.3.1 | Apache-2.0 OR MIT | <https://github.com/LaurenzV/hayro> |
| `hayro-jpeg2000` | 0.4.1 | Apache-2.0 OR MIT | <https://github.com/LaurenzV/hayro> |
| `jpeg-decoder` | 0.3.2 | MIT OR Apache-2.0 | <https://github.com/image-rs/jpeg-decoder> |
| `log` | 0.4.34 | MIT OR Apache-2.0 | <https://github.com/rust-lang/log> |
| `miniz_oxide` | 0.8.9 | MIT OR Zlib OR Apache-2.0 | <https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide> |
| `miniz_oxide` | 0.9.1 | MIT OR Zlib OR Apache-2.0 | <https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide> |
| `png` | 0.18.1 | MIT OR Apache-2.0 | <https://github.com/image-rs/image-png> |
| `simd-adler32` | 0.3.10 | MIT | <https://github.com/mcountryman/simd-adler32> |
| `strict-num` | 0.1.1 | MIT | <https://github.com/RazrFalcon/strict-num> |
| `tiny-skia` | 0.12.0 | BSD-3-Clause | <https://github.com/linebender/tiny-skia> |
| `tiny-skia-path` | 0.12.0 | BSD-3-Clause | <https://github.com/linebender/tiny-skia/tree/master/path> |
| `ttf-parser` | 0.25.1 | MIT OR Apache-2.0 | <https://github.com/harfbuzz/ttf-parser> — unmaintained notice RUSTSEC-2026-0192 (not a vulnerability); kept deliberately, see [`decisions/ttf-parser.md`](decisions/ttf-parser.md) |
| `zlib-rs` | 0.6.8 | Zlib | <https://github.com/trifectatechfoundation/zlib-rs> |

`tiny-skia` / `tiny-skia-path` (BSD-3-Clause) and `arrayref`
(BSD-2-Clause) require their copyright notices to be reproduced in
binary distributions; a binary built from this repository must carry
them (the crate sources include the texts).

The compatibility runner `compat/runner` (`kasane-compat`, a
development tool that is not part of the engine) additionally uses
`serde`, `serde_json`, `sha2` and `libc`, all MIT OR Apache-2.0.

## Bundled font data (not Apache-licensed)

| asset | what | licence |
|---|---|---|
| `fonts/liberation/*.ttf` | Liberation Sans / Serif / Mono 2.1.5, unmodified; substitutes for non-embedded Standard 14-style fonts (`src/fontsubst.rs`) | SIL Open Font License 1.1 — `fonts/liberation/LICENSE`, provenance and SHA-256 in `fonts/liberation/README.md` |
| `tests/fixtures/KasaneJP.ttf` | 17-glyph subset of Noto Sans JP, renamed | SIL OFL 1.1 — `tests/fixtures/OFL-1.1.txt`, notice in `NOTICE` |
| `tests/fixtures/type1_cff.bin` | 800-byte CFF subset of Source Sans Pro Bold, internal names changed to `KasaneSansPro` (OFL Reserved Font Name 'Source' not used) | SIL OFL 1.1 — Copyright 2010, 2012 Adobe Systems Incorporated; `tests/fixtures/OFL-1.1.txt`, notice in `NOTICE` |
| `tests/fixtures/eexec_cmsy10.bin` | 2-glyph Type 1 subset of CMBSY10 v003.002 (AMS Computer Modern, 2009 Type 1 release), internal names changed to `KSBSY10` (Reserved Font Name not used); its clear-text header carries the AMS copyright and the OFL statement | SIL OFL 1.1 — Copyright (c) 1997, 2009 American Mathematical Society; `tests/fixtures/OFL-1.1.txt`, notice in `NOTICE` |
| `tests/fixtures/KasaneTest.ttf`, `KasaneCIDTest.ttf` | synthetic fonts generated from scratch | none (generated) |

## Optional `cjk-fallback` assets (`assets/cjk/`, committed)

The assets are committed so that the crate published on crates.io —
and the tagged source it is built from — contain them: a crate built
with `--features cjk-fallback` needs no network and no extra step.
`scripts/fetch-cjk-assets.sh` is how they were obtained and how they
are updated: every file comes from a commit pinned in the script and
is verified against `scripts/cjk-assets.sha256` before anything is
installed; `scripts/fetch-cjk-assets.sh --check` verifies the
committed copy against the lock (CI runs it on every pull request):

| asset | source | licence |
|---|---|---|
| Adobe-Japan1 predefined CMaps (27 files) | adobe-type-tools/cmap-resources | BSD-3-Clause (`assets/cjk/cmap/LICENSE`) |
| `Adobe-Japan1-UCS2` CID → Unicode mapping | adobe-type-tools/mapping-resources-pdf | BSD-3-Clause |
| Noto Sans JP Regular (JP subset OTF) | notofonts/noto-cjk | SIL OFL 1.1 (`assets/cjk/NotoSansJP-LICENSE`) |

A binary built with `--features cjk-fallback` embeds these and must
ship their licence texts.

To update the assets: change the `CMAP_REV` / `MAP_REV` / `NOTO_REV`
commit pins in `scripts/fetch-cjk-assets.sh`, run
`scripts/fetch-cjk-assets.sh --update-lock`, review the regenerated
`scripts/cjk-assets.sha256` and the fetched licence files, re-run
`cargo test --features cjk-fallback`, and commit the pins, the lock
and `assets/cjk/` together. `scripts/check-workflows.py` (run on every pull
request) fails if the pins are not commit SHAs or the lock is
incomplete.

## Development-time tools (never linked)

`qpdf`, Poppler (`pdftoppm`, `pdftotext`, `pdfinfo`, `pdfimages`),
libjpeg-turbo (`cjpeg`/`djpeg`), fontTools and OpenJPEG are invoked
by the compatibility runner, the verification scripts and the fixture
generators as external processes on the developer's machine. Their
licences do not apply to kasane-pdf binaries.

## Test-fixture PDFs and the compatibility corpus

`tests/fixtures/*.pdf` are generated by `make_fixtures.py` and
contain no third-party content beyond the fonts above. The real-world
compatibility corpus (`compat/corpus/`) is never committed; its
manifest records a licence and redistribution flag per file (see
`compat-corpus.md`).
