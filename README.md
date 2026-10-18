# kasane-pdf

A PDF parsing/processing engine written in Rust. Early development —
currently at milestone **M0**: lexical parsing, classic cross-reference
tables, indirect-reference resolution, and page-tree reading.

This is **not** a wrapper around an existing PDF engine (PDFium, MuPDF,
Poppler are not used). The syntax parser, document model, and (future)
renderer and writer are implemented here.

## Status

**M0 is complete.** See [`docs/coverage.md`](docs/coverage.md) for the
detailed support matrix and [`docs/design.md`](docs/design.md) for
architecture and licensing notes.

What works today:

- PDF basic objects: null, bool, integer, real, name (with `#xx`
  escapes), literal and hexadecimal strings, arrays, dictionaries,
  stream markers (raw data captured, not decoded).
- Classic `xref` tables with subsections and `trailer` dictionaries.
- `/Prev` chains (incremental updates that stay in classic format).
- Indirect reference resolution with cycle detection and a resolution
  budget.
- Document catalog and page tree with inherited `MediaBox`, `CropBox`,
  `Rotate`, `Resources`.
- A `kasane-pdf` CLI that prints page count and page sizes.

What is deliberately **not** supported yet (produces a distinct error,
never a partial result presented as success):

- xref streams, hybrid-reference files, object streams
- Encrypted documents
- Stream decoding (`/FlateDecode`, …), content streams, rendering,
  text extraction, editing, writing

## Requirements

- Rust 1.70 or later (`edition = "2021"`)

## Usage

Library:

```rust
use kasane_pdf::Document;

let data = std::fs::read("input.pdf")?;
let doc = Document::from_bytes(&data)?;
for page in doc.pages() {
    println!("page {}: {}", page.index() + 1, page.media_box());
}
# Ok::<(), kasane_pdf::Error>(())
```

CLI:

```console
$ cargo run -- tests/fixtures/pages.pdf
file: tests/fixtures/pages.pdf
pages: 3
  page 1: media box [0 0 612 792] (612 x 792 pts), rotate 90  (3 0 R)
  page 2: media box [0 0 400 400] (400 x 400 pts)  (6 0 R)
  page 3: media box [0 0 200 100] (200 x 100 pts)  (7 0 R)
```

Errors are printed with a stable category in brackets, e.g.
`error [unsupported feature]: unsupported PDF feature: xref stream (…)`.

## Testing

```console
$ cargo test            # unit + integration tests (fixtures in tests/fixtures/)
$ cargo fmt --check
$ cargo clippy --all-targets
```

The fixtures are hand-crafted and deterministic; regenerate them with
`python3 tests/fixtures/make_fixtures.py` (no dependency on any other
PDF tool).

## Safety-oriented design

All parsing is bounded by [`Limits`](src/limits.rs): input size, indirect
reference budget, page-tree depth/nodes, collection sizes, string
lengths, nesting depth. Malformed input returns a typed
[`Error`](src/error.rs); it must never panic or loop without bound.

## License

Not yet decided. Until it is, the source is all rights reserved.
The dependency policy (permissive licenses only, no AGPL/GPL engine
cores) is documented in [`docs/design.md`](docs/design.md); there are
currently **zero** external crate dependencies.

## Naming

`kasane-pdf` (crate), `kasane_pdf` (Rust path), `kasane-pdf` (CLI).
The crates.io name availability and any trademark considerations have
not been confirmed; nothing is published yet.

## References

- ISO 32000-1:2008 — *Document management — Portable document format —
  Part 1: PDF 1.7* (and its errata).
- ISO 32000-2:2020 — PDF 2.0, for features beyond 1.7.

Section numbers cited in source comments refer to ISO 32000-1 unless
noted otherwise.
