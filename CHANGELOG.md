# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/akam1o/kasane-pdf/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.0
[0.1.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.1
[0.1.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.0
