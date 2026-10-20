# kasane-pdf

A PDF parsing/processing engine written in Rust. Early development —
currently at milestone **M8**: lexical parsing, classic and stream
cross-reference tables, indirect-reference resolution, stream decoding
(`FlateDecode` + predictors), object streams, page-tree reading,
content-stream parsing into typed drawing instructions, raster
rendering to PNG, and Form/Image XObject painting.

This is **not** a wrapper around an existing PDF engine (PDFium, MuPDF,
Poppler are not used). The syntax parser, document model, and (future)
renderer and writer are implemented here.

## Status

**M8 / M8.1 / M8.2 / M9 are complete.** Rewrite any loaded document
to a fresh single-revision PDF via `kasane-pdf rewrite in.pdf
--output out.pdf` or the `rewrite(&Document)` API — preserving page
tree, contents, fonts, text and basic links. M8.1/M8.2 tightened and
validated the write guarantees; `scripts/verify_rewrite.sh` runs
`qpdf --check` (12.4.2) on every well-formed fixture — exit 0, zero
warnings.

**M9 / M9.1 adds `/Square` annotation editing.** `Document::edit()`
opens a session; `add_square(page, rect, rgb, width)` queues an
annotation; `rewrite()` emits a normal single-revision file. CLI:
`kasane-pdf annotate-rect in.pdf 1 --rect 20 30 120 80 --color 1 0 0
--width 2 --output out.pdf`. `rect` = lower-left x,y then upper-right
x,y in un-rotated PDF user space, and is the annotation's *outer*
bounding box (the stroke is drawn inside it). M9.1 corrected the
appearance transform (BBox `[0 0 w h]` + `Matrix` translate so the
stroke width is never rescaled) and hardened the synthetic
object-number allocator to never alias a real object. M9.2 extended
that: a number present in the input xref at *any* generation or entry
type is reserved, and an input reference landing on a synthetic
number is treated as a dangling ref — never silently resolved to a
synthetic body. See [`docs/coverage.md`](docs/coverage.md) for the
matrix and [`docs/design.md`](docs/design.md) for notes.

**M10 / M11 add an `inspect` command** — `kasane-pdf inspect file.pdf
1` prints a fixed-order structural summary of a single page
(MediaBox, CropBox, rotation, annotation count, `/Contents`
reference(s), `/UserUnit`), enumerates named resources per category
(`font F1 Type1 /Helvetica`, `xobject Im1 Form /BBox=[…]`, …), dumps
`tokens`/`operators`/`opstats` content-stream statistics, then one
`annot <i> <subtype> rect …` line per annotation. Diagnostic only —
never renders, edits or extracts. The file must contain exactly one
page.

**M16 adds `diagnose_redaction` + `preflight-redaction`** — the first
step toward a real redaction feature: a read-only, in-memory
diagnostic of what painted content falls inside caller-supplied
rectangles. It reports each shown character, image placement, Form
invocation, and painted path with a bounding box and
site identity (`/Contents` ref + op index + `Do` call-site chain +
character index within the string), enumerates
blockers (Optional Content, `/ActualText`, unsupported XObjects),
locations it could not pin down (`uncertain`), sharing that would
make an edit unsafe (`shared`), page annotations, and document areas
never examined (`unexamined`). It deliberately returns no "safe"
verdict — the name is `preflight-redaction` because a clean report
means only "the scan completed", never "redaction would work".
"Conservative" bounds are only claimed where the reach is provable —
a stroke's extent uses `√2 × max(|col₁|, |col₂|)` of the CTM (never
`sqrt(|det|)`, which under-reports non-uniform scaling), a `w = 0`
hairline (device-resolution dependent, unboundable in user space)
reports as `uncertain` instead of a certain target, and state a
`gs`/`sh`/Pattern changed marks everything after `uncertain` until
`Q` or a form's end restores known state.

**M17 adds `remove_text` + `experimental-remove-text`** — an
*experimental* scoped text removal, **not** a redaction tool. It
deletes the shown character codes whose filled-ink bounds hit a
rect (page-level `Tj`/`TJ`/`'`/`"`, `Tr 0` fill, supported
horizontal fonts only), rewrites the affected strings as `TJ` with
numeric elements that restore the removed advance, and writes a new
single-revision PDF through the same overlay pipeline as
`annotate-rect`. A page with `/Annots`, any `Do`/`sh`/inline
image/`gs`/Pattern/`ActualText`/OC, non-fill text modes, `w = 0`
hairlines, unsupported fonts, non-finite CTM, `/UserUnit` ≠ 1,
vertical text, or `Tfs`/`Tz` = 0 is refused outright — never
half-edited. The input document and its bytes are never modified,
and other pages sharing the original `/Contents` keep it (the
removed bytes still exist in the shared stream — this is content
editing, not secure redaction).

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
- `extract_text()` positioned text extraction (`CharInfo` per
  character code, content order): `/ToUnicode` CMap (`bfchar`,
  `bfrange` sequential + array, UTF-16BE incl. surrogate pairs,
  multi-scalar mappings) with WinAnsi-name→AGL→Unicode fallback;
  unresolved codes report `None`, never a guess. `/UseCMap`,
  `usecmap`/`usefont`, non-1-byte codespaces → `Error::Unsupported`.
  Extraction is headless — no rasterising, clip masks, or image
  decoding — `/Span` `/ActualText` or `/OC` membership in the
  executed stream is refused rather than silently ignored, and a
  cumulative `max_extracted_unicode_bytes` budget bounds the
  Unicode a page can emit (UTF-8 bytes, counted per emitted code
  so repeated/split strings cannot evade it).
- `diagnose_redaction()` + `preflight-redaction` pre-redaction
  diagnosis — targets (text/image/form/path), blockers, uncertain
  positions, shared resources, page annotations, and unexamined
  document areas, with no "safe" verdict.
- `remove_text()` + `experimental-remove-text` — experimental scoped
  text removal that deletes hit codes and preserves survivors'
  positions (not a redaction tool; see `docs/coverage.md`).
- A `kasane-pdf` CLI (`info` mode + `contents`, `operations`,
  `render`, `text`, `rewrite`, `annotate-rect`, `inspect`,
  `preflight-redaction` and `experimental-remove-text` subcommands).
- Adding `/Subtype /Square` rectangle annotations to a page via an
  [`Edit`] session — opaque RGB stroke, positive width, generated
  `/AP` appearance, existing annots/links preserved, shared `/Annots`
  arrays never mutated in place.

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
- Text extraction for fonts other than simple TrueType +
  WinAnsiEncoding or Type0+Identity-H+CIDFontType2; `/ActualText`
  replacement, Optional Content membership (`/OC`/`/OCMD` blocks,
  form `/OC`), and `/UseCMap`-chained or multi-byte-codespace
  ToUnicode CMaps — refused with `Error::Unsupported`, not silently
  ignored
- Rendering of text with non-TrueType fonts, Type 0 fonts with
  non-Identity-H encodings (Identity-V, CMap streams, predefined
  CMaps like UniJIS-*/90ms-*), CIDFontType0/CFF, Type 1, Type 3,
  non-embedded fonts, symbolic fonts, non-WinAnsi simple encodings,
  text clipping modes (`Tr` 4–7), or any shaping/ligature behaviour;
  shading (`sh`), extended graphics state (`gs`), pattern colours,
  non-device colour spaces beyond DeviceGray/RGB,
  `/ImageMask`/`/Mask`/`/SMask`, transparency groups, annotations,
  `/UserUnit` ≠ 1
- Painting under a singular (non-invertible) CTM — refused with
  `Error::Unsupported`
- Annotation subtypes other than `/Square`; editing or deleting
  existing annotations, page edits, flattening, redaction

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

$ cargo run -- preflight-redaction tests/fixtures/text_basic.pdf 1 \
    --rect 15 55 70 80
preflight-redaction — diagnosis, not a redaction
page: 1
rects: 1
  rect 0: 15 55 70 80
targets 3
  Text bounds [20 60 34.40000057220459 76.80000114440918] rects 0 @ contents[4 0 R] op#3 text code=0x0041 unicode="A" font=/F1 mode=0 origin=(20 60) via=Outline
  ...
unexamined 9
  - /Info and /XMP metadata
  - embedded files and attachments
  ...
note: a clean report means 'no obstacle found in the examined scope' — it is NOT a redaction-safety verdict.

$ cargo run -- experimental-remove-text tests/fixtures/text_basic.pdf 1 \
    --rect 35 60 38 80 --output /tmp/out.pdf
experimental-remove-text tests/fixtures/text_basic.pdf page 1 -> /tmp/out.pdf (2946 bytes)
codes removed: 1
  page 1 (1 rect)
  codes removed: 1
  scope: page-level content only — no Form/Annots/metadata/structure
# the output's Contents is now: BT /F1 24 Tf 20 60 Td [(A) -600 (C)] TJ ET
# — 'B' is gone and 'C' keeps its origin.
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
