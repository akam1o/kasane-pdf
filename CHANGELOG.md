# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/akam1o/kasane-pdf/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.0
