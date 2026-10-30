# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- **Panic on an empty stream without a usable `/Length`** (found by
  fuzzing; present since 0.1.0). With no direct or resolvable
  `/Length`, the parser scans for `endstream` and backs off the EOL
  before it; in `stream⏎endstream` that EOL ends the `stream` line, so
  the data end fell one byte before its start and slicing panicked —
  reachable from any input, in strict and recovery mode. The back-off
  now never passes the start of the data; such a stream is empty.

## [0.2.1] - 2026-10-30

Compatible release (`cargo semver-checks` against 0.2.0: no semver
update required). Several inputs refused by 0.2.0 now succeed; the
list is in `docs/releases/v0.2.1.md` § Behaviour changes.

### Added
- **§7.3.10 undefined references.** An indirect reference to an object
  the file does not define (no cross-reference entry for that number
  and generation, or a free entry) resolves to null in strict mode, as
  the standard requires, and is recorded once as
  `Warning::UndefinedReference`. The writers and the audit still see
  `Error::MissingReference` (`Document::resolve_defined`, crate-private)
  so a rewrite never turns a dangling reference into `null` silently.
- **Open content-stream blocks.** A stream ending inside `q`, `BT`,
  `BMC`/`BDC` or `BX` is closed at its end and recorded as
  `Warning::UnterminatedContentBlock` (nothing after the end could
  depend on the closing operator). Stray closing operators stay errors.
- **Recovery rules.** `Document::from_bytes_recovering` now also
  applies, and records as `Warning::RecoveryApplied { rule:
  RecoveryRule, .. }`, the reinterpretations every major reader makes
  and ISO 32000-1 does not sanction: US Letter for a page without
  `/MediaBox`, a stray `Q` ignored, an unknown operator dropped, a
  graphics-state or colour operator inside a path object applied, an
  invalid `/Annots` entry or appearance state skipped. Strict mode
  refuses every one of them unchanged. `Document::recovery_mode()`
  reports the mode; `recovery()` still reports only a rebuilt table.
- **Knockout transparency groups** (`/K true`, §11.4.8): each object is
  composited against the group's initial backdrop and replaces earlier
  objects within its shape; isolated and non-isolated, with the group's
  alpha, blend mode and soft mask. New bound
  `Limits::max_knockout_objects` (default 4 096).
- **Symbol and ZapfDingbats fallback.** Non-embedded Symbol /
  ZapfDingbats fonts draw with a bundled 414-glyph DejaVu Sans subset
  (`fonts/symbol-fallback/`, Bitstream Vera licence) through their
  built-in encodings (Annex D.5/D.6) and the Adobe AFM widths when the
  PDF has no `/Widths`; reported as `SubstKind::SymbolFallback`.
- `Warning::UnknownRenderingIntent` (an `/RI` or `ri` name outside
  §8.6.5.8, treated as RelativeColorimetric) and
  `Warning::IgnoredFilterFragment` (an ASCII85 final group of one
  character).
- Compatibility runner: strict / recovered / approximate render
  outcomes counted independently (a recovered render is never a strict
  one), a recovery-mode render attempt after a strict render error,
  warning and recovery-note totals.

- **Oversized images drawn small.** A JPEG whose `/Width` or `/Height`
  exceeds `max_image_dimension` (or the sample limits) while the size it
  is drawn at does not is decoded with a reduced inverse DCT (1/2, 1/4
  or 1/8: the coarsest that keeps one sample per device pixel on both
  axes, else the finest that fits the limits) and reported on the new
  `RenderedPage::image_approximations()`, which says whether the decode
  is below the drawn resolution.
  An oversized soft mask without an image filter is sampled directly
  at its compositing grid (exactly the samples a full decode would
  give). The limits are unchanged; anything else still fails with
  `LimitExceeded`.

### Fixed
- Knockout groups: opacity set inside a nested form (`ca`/`CA`, an
  ExtGState soft mask, an image's `/SMask`) reduced the object's shape
  and so under-knocked earlier objects; opacity is now kept out of the
  shape pass at any depth, and `/AIS` (previously ignored) decides
  whether those values are shape (§11.6.4.4). `/AIS` must be a boolean.
- DCT images: the JPEG frame size is checked against the image
  dictionary *before* decoding, and a progressive frame's coefficient
  memory (held at full resolution whatever the output scale) is bounded
  by `max_image_bytes` up front. Previously a frame header larger than
  the dictionary, or a large progressive frame, could allocate beyond
  `max_image_bytes` before the post-decode size check.
- A colour-keyed (`/Mask` array) JPEG is never decoded at reduced size:
  the key matches exact sample values.

### Changed
- An image's `/SMask` finer than the image is composited at the mask's
  resolution (capped at the drawn size and the image limits), with the
  image's samples repeated onto that grid — §11.6.5.3 maps the two
  independently. Previously the mask was resampled down to the image's
  grid.
- A `/SMask`, `/Contents` element, appearance state or `/AP /N` that
  is null (including through an undefined reference) is treated as
  absent rather than as a type error.

## [0.2.0] - 2026-10-24

The API-stability release: the breaking changes scheduled in
`docs/api-stability.md`, made once so that later releases can add
without breaking. Behaviour on PDF input is unchanged (the 511-file
corpus renders, extracts and rewrites identically). Every change is
mechanical; `docs/migration-0.2.md` has the one-line replacement for
each.

### Breaking
- `#[non_exhaustive]` on `Error`, `ChangeKind`, `ResolveKind`,
  `SubstKind`, `DestSource`, `TargetKind`, and on the option and
  result structs `Limits`, `RenderOptions`, `RenderedPage`,
  `ExtractedText`, `CharInfo`, `SignatureInfo`, `RecoveryReport`,
  `FontSubstitution`, `MissingReference`. Exhaustive matches need a
  wildcard arm; struct literals of `Limits` / `RenderOptions` become
  `Limits::default().with_<field>(v)` / `RenderOptions::default()
  .with_dpi(d).with_limits(l)` (fields stay `pub` for reading and
  assignment).
- `Error::kind()` returns the new `ErrorKind` enum instead of
  `&'static str`; `ErrorKind::as_str()` yields the 0.1 strings
  unchanged, so CLI output is identical.
- `extract_text(&Document, &Page, &TextOptions)`: the third argument
  is the new `TextOptions` (`with_limits`, `with_hidden_content`,
  `with_region`). `extract_text_all` is removed (=
  `with_hidden_content(true)`).
- `RenderedPage` fields are private: `width()`, `height()`, `dpi()`,
  `page_size_pts()`, `font_substitutions()`, `color_approximations()`;
  the pixel buffer through the backend-neutral `pixels()`, `stride()`,
  `pixel_format()`, `into_rgba8()`, `to_png()`; `pixmap()` /
  `into_pixmap()`, the `Pixmap` re-export and `pixmap_to_png` only
  with the new default feature `tiny-skia-interop`.
- A signature's refusal is `Error::SignaturePolicy { rule:
  SignatureRule, detail }` (`ByteRangeNotIntact`, `DocMdpNoChanges`,
  `DocMdpFormFillOnly`, `DocMdpFormFillAndAnnotations`,
  `FieldMdpNotEvaluated`, `RewriteOfSignedDocument`) instead of
  `Error::Unsupported { feature: "signed document" | "edit of a
  signed document" }`.
- An invalid `dpi` is `Error::InvalidArgument { argument: "dpi" }`
  instead of `Error::Structure`.

### Added
- `ErrorKind` (`as_str`, `is_malformed_input`), `SignatureRule`,
  `Error::InvalidArgument`, `MissingReference` re-exported.
- `Limits::new`, `Limits::validate` (rejects a zero *structural*
  bound; feature bounds may still be 0 to forbid the feature), one
  `with_<field>` per bound; `RenderOptions::new/with_dpi/with_limits/
  validate`; `TextOptions`; `PixelFormat`.
- Every entry point that takes limits or options validates them first.

### Changed
- Documentation: `docs/migration-0.2.md`, `docs/api-stability.md`
  rewritten for the 0.2 contract, README and examples on the new API.

## [0.1.1] - 2026-10-24

Additive release: no public item was removed or changed in signature
(`cargo semver-checks` against 0.1.0: no semver update required).

### Added
- `Warning` (`#[non_exhaustive]`) and the per-document warning
  channel: `Document::take_warnings`, `warnings`, `warnings_dropped`,
  `warning::MAX_WARNINGS`. A non-conforming input the engine can
  still process *exactly* is accepted and recorded instead of refused.
  Two variants: `UnterminatedFlateStream` (a `/FlateDecode` stream
  ending after a sync flush without the final block and Adler-32;
  data ending inside a block is still refused) and
  `MalformedToUnicodeEntry` (a `bfchar`/`bfrange` entry that cannot
  be read is skipped; its codes stay unmapped).
- `kasane_pdf::Pixmap` re-exports the `tiny-skia` type of
  `RenderedPage::pixmap`.
- The CLI `render` and `text` subcommands print each warning as
  `warning: …` on stderr.
- Examples `rewrite`, `recovery` and `cjk_fallback` (the last behind
  `--features cjk-fallback`); `tests/public_api.rs` exercises the
  public surface the README relies on; the README code blocks are
  compiled as doctests.
- `docs/api-stability.md` (SemVer classification of the 0.1 API),
  `docs/decisions/ttf-parser.md`, a RustSec audit job in CI.

### Changed
- `/ToUnicode` hex strings may contain white space between digits
  (§7.3.4.3); previously such a CMap failed with
  `Error::Structure("invalid hex in ToUnicode")`.
- Flate decoding failures are now distinguished: `"incomplete deflate
  stream"` (truncated inside a block) versus `"corrupt deflate
  stream"` (invalid data); both remain `Error::Parse`.
- Dependencies: `crc32fast` 1.5.3; the compatibility runner uses
  `sha2` 0.11.

### Dependencies
- `ttf-parser` 0.25.1 is kept despite RUSTSEC-2026-0192 (an
  *unmaintained* notice, not a vulnerability); the rationale and the
  migration conditions are in `docs/decisions/ttf-parser.md`, and
  `.cargo/audit.toml` ignores only that informational id.

## [0.1.0] - 2026-10-24

Initial release. The API is not yet stable (0.x): breaking changes
are possible between minor versions and are recorded here.

### Added
- Strict parser for the PDF object layer, classic and stream
  cross-reference sections, object streams, incremental updates and
  hybrid files; an explicit recovery parser (`from_bytes_recovering`)
  that reports every assumption it makes.
- Page renderer (`render_page`): paths, clipping, every colour-space
  family of ISO 32000-1 §8.6, images (Flate, LZW, ASCII, RunLength,
  DCT, JBIG2, CCITT, JPX), image masks and soft masks, transparency
  groups and blend modes, ExtGState soft masks, shadings of every
  type, tiling and shading patterns, inline images, text in TrueType,
  Type 1, CFF, OpenType, Type 0 (CID) and Type 3 fonts with rendering
  modes 0–7, optional content, annotation appearances and synthesised
  form-field / markup appearances, `/UserUnit`.
- Text extraction (`extract_text`): positioned characters with
  Unicode through `/ToUnicode`, encodings, `/ActualText`.
- Full rewrite (`rewrite`) and an `Edit` session with `/Square`
  annotations, annotation modification and removal, form-field
  values, `/Info` updates; **incremental save** that preserves the
  original bytes; signature detection and a safe-by-default policy
  for edits of signed documents (no cryptographic verification).
- Experimental content editors (`remove_text`, `remove_image_region`,
  `remove_region`, `remove_regions`, `remove_document_metadata`), the
  read-only redaction diagnosis and residual-information audit.
- Bundled Liberation fonts as substitutes for non-embedded Standard
  14-style fonts; optional `cjk-fallback` feature for non-embedded
  Adobe-Japan1 CID fonts.
- Compatibility runner (`compat/runner`) with Poppler/qpdf
  cross-checks, all-page round-trip verification and CI workflows.

### Fixed
- JPEG colour-transform tests no longer depend on the decoder's SIMD
  back-end: expectations are derived in-process (JFIF decode of the
  same coefficients, or the T.871 conversion of the decoder's own raw
  samples) instead of libjpeg-turbo sample values, which differ by
  the IDCT accuracy allowance between x86-64 and arm64.
- Nightly workflow calls `fetch-corpus.py download --dir` and passes
  the store location and credentials under the names the tool and
  boto3 read; actions pinned to commit SHAs; a consistency check runs
  on every pull request.
- CJK fallback assets are fetched from pinned commits and verified
  against `scripts/cjk-assets.sha256`.
- Test-fixture font subsets renamed internally so OFL Reserved Font
  Names are not used; provenance recorded.

### Not included
- Document creation from scratch (no page / content-stream builder),
  encryption, cryptographic signature verification, knockout
  transparency groups, non-embedded CJK fonts other than Adobe-Japan1
  (with `cjk-fallback`), page-level editing — see
  `docs/limitations.md`.

### Licence
- Apache License 2.0; bundled fonts under the SIL Open Font License
  1.1 and Adobe CMap resources under BSD-3-Clause (`NOTICE`,
  `docs/third-party.md`).

[Unreleased]: https://github.com/akam1o/kasane-pdf/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.1
[0.2.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.0
[0.1.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.1
[0.1.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.0
