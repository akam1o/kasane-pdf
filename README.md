# kasane-pdf

A PDF parsing/processing engine written in Rust. Early development —
currently at milestone **M5**: lexical parsing, classic and stream
cross-reference tables, indirect-reference resolution, stream decoding
(`FlateDecode` + predictors), object streams, page-tree reading,
content-stream parsing into typed drawing instructions, raster
rendering to PNG, and Form/Image XObject painting.

This is **not** a wrapper around an existing PDF engine (PDFium, MuPDF,
Poppler are not used). The syntax parser, document model, and (future)
renderer and writer are implemented here.

## Status

**M5 is complete.** See [`docs/coverage.md`](docs/coverage.md) for the
detailed support matrix and [`docs/design.md`](docs/design.md) for
architecture and licensing notes.

What works today:

- PDF basic objects: null, bool, integer, real, name (with `#xx`
  escapes), literal and hexadecimal strings, arrays, dictionaries.
- Classic `xref` tables with subsections, `trailer` dictionaries, and
  `/Prev` chains (incremental updates in classic format).
- xref streams (`/Type /XRef`) with `/W`, `/Index`, and entries of type
  0, 1, and 2.
- Hybrid-reference files (classic table + `/XRefStm` stream).
- Object streams (`/Type /ObjStm`) with cached decoding.
- Indirect reference resolution with cycle detection and a resolution
  budget.
- Stream decoding: no filter, `/FlateDecode` (zlib/DEFLATE), multiple
  filters in order, `/DecodeParms`, PNG predictors 10–15 and TIFF
  predictor 2 for 8-bit components.
- Document catalog and page tree with inherited `MediaBox`, `CropBox`,
  `Rotate`, `Resources`.
- `Document::page_contents()` returning decoded content streams in
  order.
- `Document::page_operations()` parsing content streams into typed
  operations (`Operation`) — graphics state, paths, painting, colour,
  text, XObject/shading invocation, marked content — with balanced
  `q`/`Q`, `BT`/`ET`, `BX`/`EX`, `BMC`/`BDC`/`EMC` checks. Concatenated
  `/Contents` arrays are parsed as one logical stream.
- `Document::page_resource()` resolving named entries in a page's
  (inherited) `/Resources` dictionary: `Font`, `XObject`, `ExtGState`,
  `ColorSpace`, `Pattern`, `Shading`, `Properties`, …
- `render_page()` rendering pages that use basic drawing (paths,
  clipping, DeviceGray/RGB/CMYK colours) plus Form and Image XObjects
  to a pixmap, honouring `MediaBox`/`CropBox`/`Rotate` and DPI; PNG
  output via `pixmap_to_png`.
- `Do` invocation: Form XObjects (own `/Matrix`, `/BBox` clipping,
  own `/Resources`, nesting, recursion detection) and Image XObjects
  (8-bit `/DeviceGray`/`/DeviceRGB`, raw + `FlateDecode` +
  `DCTDecode`, `/Decode`, `/Interpolate`, clipping). Unsupported
  attributes (`/ImageMask`, `/Mask`, `/SMask`, `/SMaskInData` ≠ 0,
  `/OC`, `/Group`, `/Ref`, unknown subtypes) are refused explicitly.
- Text rendering with embedded TrueType fonts: `/Subtype /TrueType`
  + `/FontFile2` + `WinAnsiEncoding` (named or via an `/Encoding`
  dictionary's `/BaseEncoding`/`/Differences`). `BT`/`ET`, `Tf`,
  `Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts`, `Tm`/`Td`/`TD`/`T*`,
  `Tj`/`TJ`/`'`/`"`, rendering modes 0–3. Positioning uses the PDF
  `/Widths` array — never the font's own advance widths.
  Operator placement is checked per ISO 32000-1 §8.2 Figure 9 —
  out-of-order sequences inside a path object (e.g. `cm`, `q`/`Q`,
  colour ops between path construction and painting, or path
  construction after `W`/`W*`) are refused with a typed error.
- A `kasane-pdf` CLI (`info` mode + `contents`, `operations` and
  `render` subcommands).

What is deliberately **not** supported yet (produces a distinct error,
never a partial result presented as success):

- Encrypted documents
- Stream filters other than `/FlateDecode` and `/DCTDecode` (images
  only) — `/LZWDecode`, `/ASCIIHexDecode`, `/ASCII85Decode`,
  `/RunLengthDecode`, `/CCITTFaxDecode`, `/JBIG2Decode`, `/JPXDecode`,
  `/Crypt`
- PNG predictors for components other than 8-bit, and unknown predictor
  values (e.g. `/Predictor 16`)
- Inline images (`BI`/`ID`/`EI`) — the binary image data cannot be
  tokenized reliably, so the construct is rejected outright rather than
  guessed
- Unicode conversion of text-showing strings, text extraction
- Rendering of text with non-TrueType fonts, Type 0 / CID fonts,
  Type 3 fonts, non-embedded fonts, symbolic fonts, non-WinAnsi
  encodings, text clipping modes (`Tr` 4–7), or any shaping/ligature
  behaviour; shading (`sh`), extended graphics state (`gs`), pattern
  colours, non-device colour spaces beyond DeviceGray/RGB,
  `/ImageMask`/`/Mask`/`/SMask`, transparency groups, annotations,
  `/UserUnit` ≠ 1
- Painting under a singular (non-invertible) CTM — refused with
  `Error::Unsupported`
- Editing, writing

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

$ cargo run -- contents tests/fixtures/flate_contents.pdf 1
file: tests/fixtures/flate_contents.pdf
page: 1 (1 content stream(s))
--- stream 1 (52 bytes) ---
q 1 0 0 1 0 0 cm BT /F1 12 Tf (compressed page) Tj ET Q

$ cargo run -- operations tests/fixtures/hello.pdf 1
file: tests/fixtures/hello.pdf
page: 1 (5 operation(s))
    0  BT
    1  /F1 24 Tf
    2  72 720 Td
    3  ("Hello, PDF!") Tj
    4  ET

$ cargo run -- render tests/fixtures/render_rects.pdf 1 --output out.png --dpi 144
rendered page 1 -> out.png (400x400 px, 200 pts, dpi 144)
```

Errors are printed with a stable category in brackets, e.g.
`error [unsupported feature]: unsupported PDF feature: stream filter (/LZWDecode)`.

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
cores) is documented in [`docs/design.md`](docs/design.md). Current
dependencies: `flate2` (MIT/Apache-2.0, `rust_backend`) for
`/FlateDecode`, `tiny-skia` (BSD-3-Clause) as the raster backend,
`png` (MIT/Apache-2.0) for PNG encoding.

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
