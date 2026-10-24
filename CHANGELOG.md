# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

First public state of the repository. Nothing has been released to
crates.io yet; the API is not stable.

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

### Licence
- Apache License 2.0.
