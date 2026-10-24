# Decision — `ttf-parser` after RUSTSEC-2026-0192

## The advisory

`RUSTSEC-2026-0192` (2026-06-28) is an **informational `unmaintained`
notice**, not a vulnerability: the author of `ttf-parser` stated that
the crate will receive no further fixes
(<https://github.com/harfbuzz/ttf-parser/issues/217>). There is no
CVE, no known memory-safety or panic issue, and `patched = []` because
there is nothing to patch. `cargo audit` reports it as a warning.

## What kasane-pdf uses

`ttf-parser` 0.25.1 (latest; MIT OR Apache-2.0; `#![forbid(unsafe_code)]`;
**zero dependencies**) is a direct dependency with the `std` and
`glyph-names` features. The engine uses a small surface of it:

| use | where |
|---|---|
| `Face::parse`, `number_of_glyphs`, `units_per_em`, `glyph_hor_advance` | `src/font.rs`, `src/fontsubst.rs` |
| `glyph_index` (cmap), `glyph_index_by_name` (post / CFF charset) | simple fonts, substitutes |
| `outline_glyph` + the `OutlineBuilder` trait (also the sink for the Type 1 charstring interpreter) | `src/font.rs`, `src/type1.rs` |
| `tables().cff` → `glyph_cid`, `glyph_index`, `matrix`; bare `cff::Table::parse` for `/FontFile3` | CID-keyed CFF, Type1C |
| raw `loca`/`glyf` table access for `/FontFile2` programs whose cmap is absent | `src/font.rs` |

No `ttf-parser` type appears in kasane-pdf's public API, so a
replacement is an internal change.

## Options compared

| option | maintenance | safety | deps added | WASM | licence | effort / risk |
|---|---|---|---|---|---|---|
| keep `ttf-parser` 0.25.1 | none (frozen) | pure Rust, `forbid(unsafe_code)`, inputs already bounded by `Limits` before they reach it | 0 | yes (used today) | MIT/Apache | none now; risk is a future bug with no upstream fix |
| `skrifa` 0.49 (fontations / "oxidize", the advisory's suggested replacement) | active (Google Fonts) | pure Rust (`read-fonts` uses `bytemuck`; no `unsafe` in the parsing path) | `read-fonts`, `font-types`, `bytemuck`, `yoke`, `smallvec`, `once_cell` (+ proc-macro crates at build time) | yes (`no_std` capable) | MIT/Apache | rewrite of the font loader: different face / outline / cmap / CFF APIs, bare-CFF handling, glyph-name lookup via `post`; several days plus a full-corpus fidelity re-verification |
| a maintained fork of `ttf-parser` | none known with a release cadence as of 2026-10; `rustybuzz` (same author) depends on it and is in the same state | — | — | — | — | not available |
| `fontdue` / `ab_glyph` | maintained | pure Rust | few | yes | MIT/Apache | rasterisation-oriented; no CFF charset / CID lookup, no `post` names — insufficient |
| own parser for the tables used | — | — | 0 | yes | — | weeks; not justified |

## Decision

**Keep `ttf-parser` 0.25.1 for 0.1.x.** The notice does not change the
crate's safety properties, its inputs are bounded, its surface in
kasane-pdf is narrow and non-public, and the only maintained
alternative with equivalent coverage (`skrifa`) is a multi-day loader
rewrite that would need the 511-file corpus re-verified for glyph
fidelity. `cargo audit` runs in CI with this one informational id
ignored in `.cargo/audit.toml` (the justification is in that file);
any vulnerability advisory, for this or any other crate, still fails
the build.

## Conditions for migrating

Migrate to `skrifa` (tracked for 0.2.0 or earlier) when any of these
holds:

1. a vulnerability or soundness advisory is published for `ttf-parser`;
2. a font in the corpus or a user report needs a table or a fix that
   `ttf-parser` lacks (variable fonts, newer CFF2, a `cmap` subtable
   format it does not read);
3. the loader is touched for another reason that already requires
   re-verifying glyph fidelity (e.g. hinting or shaping work).

The migration plan: introduce an internal `FontProgram` trait with
exactly the five operations listed above, implement it over `skrifa`
behind the same `Font` enum, run the corpus with both back-ends, and
require identical glyph bounds (±1 font unit) on every file before
switching the default.
