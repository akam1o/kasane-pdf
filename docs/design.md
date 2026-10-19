# kasane-pdf design notes

## Goals and non-goals

kasane-pdf is an independent PDF engine: syntax parsing, document model,
content interpretation, and writing are implemented in this repository.
Existing engines (PDFium, MuPDF, Poppler, …) may be used *only* as
development-time verification tools — never as the parsing, rendering,
or saving core of the product.

The long-term consumer is a Flutter-based macOS/iPadOS app ("Kakomi"),
but kasane-pdf is developed as a standalone Rust library + CLI. A
C-compatible API and Apple packaging are future concerns, not M0.

## Module map (M7.1)

```
src/
  lib.rs       public API + crate docs
  error.rs     Error enum with stable kind() categories
  limits.rs    Limits: all resource bounds in one place
  object.rs    Object, IndirectRef, Dict, Name types (§7.3)
  lexer.rs     byte-level tokens: words, names, strings, delimiters
  parser.rs    recursive-descent objects, indirect objects, stream
               capture, header/startxref location
  xref.rs      classic + stream xref sections, trailer, /Prev and
               /XRefStm (hybrid) merging
  filter.rs    stream /Filter decoding: FlateDecode + predictors
  objstm.rs    /ObjStm parsing (header, bounds, member objects)
  matrix.rs    Matrix [a b c d e f] affine transform (§8.3)
  content.rs   content-stream parser → Vec<Operation> (§7.8, §8–§9)
  render.rs    page → Pixmap rasterizer + PNG encoder (§8 drawing);
               also runs headless for extract_text (extraction_only:
               no path rasterising, clip masks, or image decode;
               ActualText/OC detection shared with render)
  font.rs      Font enum: Simple (embedded TrueType + /FontFile2 +
               WinAnsi + /Differences → code→glyph-name→gid) and
               Composite (Type0 + Identity-H + CIDFontType2 + /FontFile2:
               2-byte BE codes → CID → CIDToGIDMap → GID; /DW//W widths)
  tounicode.rs ToUnicode CMap parser (§9.10.3): 1- or 2-byte
               codespaces (uniform width required), bfchar, bfrange
               sequential + array → UTF-16BE
  text_extract.rs extract_text → ExtractedText: positioned Unicode
               per character code, user-space origin/advance/bounds
  document.rs  Document: load pipeline + lazy resolve() + page tree walk
               + page_contents() + page_operations() + page_resource()
               + decoded-bytes budget + ObjStm cache
  page.rs      Page, Rect value types
  main.rs      kasane-pdf CLI (info + contents + operations + render
               + text)
tests/
  fixtures/    hand-crafted PDFs + make_fixtures.py generator
  parsing.rs   M0 integration tests (kept)
  m1.rs        M1 integration tests
  m2.rs        M2 integration tests
  m3.rs        M3 integration tests
  m5.rs        M5/M5.1 text-rendering tests
  m6.rs        M6 ToUnicode + extraction tests
  m6_1.rs      M6.1 ActualText/OC refusal, budgets, coordinates
  m7.rs        M7 + M7.1 Type0/CIDFontType2/Identity-H: dict
               validation, 2-byte codes, CIDToGIDMap, DW/W, ToUnicode,
               Tw rule, exact §9.4.4 advance arithmetic, real Noto
               Sans JP (OFL) subset glyphs, regression targets
```

Layering is one-directional: `lexer` < `parser` < `xref` < `document`
< `render` < `main`. `filter`, `objstm`, `matrix`, `content` sit
beside `parser`/`xref`; `render` uses `document` + `content` +
`matrix` + `limits`. `object`, `error`, `limits`, `page` are leaf
types used everywhere.

## Key decisions

- **Parse from bytes, no I/O in the library.** `Document::from_bytes`
  takes `&[u8]`; the CLI does file reading. This keeps the engine usable
  from FFI later (caller-owned buffers) and testable in-memory.

- **Offsets come from xref, not from scanning.** Indirect objects are
  parsed at their recorded offset; a missing or wrong `endobj` is
  tolerated only because the offset already identifies the object.

- **Newest-section-wins merge for `/Prev`.** Entries are keyed by
  `(obj, gen)`; the chain is walked newest→oldest and earlier sections
  only fill gaps. Trailer dicts merge the same way, which is how `/Info`
  and `/Root` survive an incremental update.

- **Hybrid-reference (`/XRefStm`) precedence.** Within a *single* update
  section the `/XRefStm` stream *replaces* entries seen in the classic
  table. This matches both the intent of §7.5.8.4 (the classic table is
  a fallback for pre-1.5 readers) and the qpdf implementation note:
  "read the xref table first; then replace any object that it has
  already seen with any defined in the xref stream". Across update
  sections the normal newest-first `/Prev` merge applies, so a newer
  section still overrides an older one regardless of format. A `/Prev`
  key inside an `/XRefStm` dictionary is ignored — the classic
  trailer's `/Prev` drives the chain.

- **The page tree is walked eagerly at load time.** It is small,
  security-sensitive (cycles, depth), and needed by every later
  feature. Everything else — contents, resources, annotations — stays
  lazily resolved through `Document::resolve`.

- **Streams are captured raw; decoding is lazy.** `Object::Stream`
  carries `dict` + encoded `data`. Decoding happens only through
  `Document::decode_stream` (used by `page_contents` and object-stream
  resolution), so a document whose contents use an unsupported filter
  still parses and reports pages normally. Stream extent uses a
  consistent direct `/Length` when available, else an `endstream` scan;
  an indirect `/Length` is fine because the scan is used.

- **Failures are categorical.** `Error::Unsupported` and
  `Error::Encrypted` are distinct from `Parse`/`Structure` so callers
  and the CLI can distinguish "broken file" from "valid file using a
  feature we don't implement yet". `Error::kind()` gives a stable
  machine-readable category for automation.

- **Partial results are never returned.** Any error aborts
  `Document::from_bytes`; there is no "best-effort document". The same
  rule applies to `page_operations` — an error discards the whole
  operation list, it is never returned truncated.

- **Content streams are parsed postfix-style into `Operation`s, not
  executed.** `content.rs` collects operands until an operator token
  arrives, validates count and type against the operator tables in
  §8–§9, and emits a typed `Operation`. Rendering, text Unicode
  mapping and XObject execution are explicitly out of M2.

- **`/Contents` arrays are one logical stream.** §7.8.1 says a
  Contents array "shall be as if the stream objects were
  concatenated", so `ContentParser` carries the operand stack,
  `q`/`Q` depth, `BT`/`ET`/`BX`/`EX`/marked-content state across
  element boundaries. Splitting an operand list or a `q…Q` pair
  across two streams is therefore accepted.

- **Content operands are direct objects only.** The object-file parser
  accepts `n g R` and `stream` bodies; in a content stream those are
  illegal, so `content.rs` has its own operand collector over the
  shared `Lexer` (numbers, names, literal/hex strings, `[` arrays,
  `<< >>` dicts, `true`/`false`/`null`). A word that is not one of
  those primitives is an operator — this is how `10 0 R` becomes the
  unknown-operator error for `R` instead of an `Object::Ref`.

- **Structure state is validated, not interpreted.** `q`/`Q` depth is
  bounded by `max_gsave_depth`, `Q` underflow and unclosed `q`,
  `BT`/`ET`, `BX`/`EX` and `BMC`/`BDC`/`EMC` at end of input are
  `Error::Structure`. Inside `BX … EX`, unknown operators produce
  `Operation::Unknown` rather than an error (§7.8.2). `BI`/`ID`/`EI`
  are `Error::Unsupported` — the image data between `ID` and `EI` is
  binary and cannot be tokenized by scanning for `EI`.

- **`SC`/`SCN`/`sc`/`scn` keep their operands uninterpreted.** Their
  arity depends on the active colour space (which needs resources
  resolved), so the parser only enforces the syntactic bound: 0–4
  numbers plus, for the `N` forms, an optional trailing pattern name.
  A later stage can validate against the resolved colour space.

- **`Matrix::concat` is PDF `cm` order.** The `cm` operand
  concatenates to the *left* of the CTM, so `existing.concat(cm)`
  computes `cm × existing`. `transform` applies
  `x' = a·x + c·y + e`, `y' = b·x + d·y + f`. Concatenation is
  non-commutative and tested for it. `Matrix` is a value type; the
  page-level `CropBox`/`Rotate` → device mapping is a separate,
  still-unimplemented, concern.

- **Resource names stay symbolic until needed.** `Tf`/`Do`/`gs`/`CS`/
  `cs`/`sh` and `DP`/`BDC` properties keep their `Name`s. They can be
  resolved on demand through `Document::page_resource`, which uses the
  page's effective (inherited) `/Resources` per Table 30 — the
  nearest ancestor's dictionary is used *whole*, never merged
  per-key. Only the requested category/name is resolved; unused
  resources are not fetched or interpreted.

- **Rendering is a separate, opt-in layer.** `render.rs` executes the
  parsed `Operation`s against `tiny_skia::Pixmap`. It supports only
  the basic-drawing subset (paths, clipping, Device colours);
  everything else is `Error::Unsupported` and produces no output —
  never a half-rendered page passed off as success. Text, XObjects,
  shadings, ExtGState, patterns, non-device colour spaces,
  annotations, `/UserUnit` ≠ 1 and Optional Content (`/OC` or `/OCMD`
  marked content whose `/Properties` entry resolves to an OCG/OCMD
  dictionary) are all refused — the latter because visibility is
  document-level state we cannot evaluate honestly.

- **Paths are kept in user space and transformed at paint time.** The
  internal representation is an implementation choice (the spec does
  not mandate one); what matters is that the painted result matches
  the CTM in effect at paint time. We store elements untransformed
  and apply `CTM × page→pixmap` once when the path is consumed.
  Stroke parameters (width, dash, miter limit) are user-space values;
  tiny-skia transforms the stroke outline under the same paint
  transform so non-uniform scales and shears distort the stroke
  correctly.

- **`W`/`W*` mark the path; the clip installs when the path is
  consumed.** §8.5.4: `W`/`W*` come *after the last path-construction
  operator and before the painting op*; they modify the clip not at
  their own position but after the path is painted — the paint op
  draws under the *pre-W* clip, then the new clip takes effect for
  subsequent painting. `W`+`n` installs the clip without painting.
  The clip is a pixmap-sized `tiny_skia::Mask` that `q`/`Q`
  save/restore; a later `W` in a new path intersects via
  `Mask::intersect_path`. `W` on an empty path empties the clip.

- **Placement is enforced at render time.** §8.2 Figure 9 restricts
  a *path object* to path-construction ops, `W`/`W*`, and the
  terminating painting op. `cm`, `q`/`Q`, colour ops, marked content
  etc. inside a path object — and `m`/`l`/`re` after `W`/`W*` or a
  second `W`/`W*` in the same path — are rejected with
  `Error::Structure`. The parse-only `page_operations()` API still
  accepts such sequences: placement is a render-time restriction,
  not a parsing error. Poppler tolerates many of these placements;
  we report them instead of guessing at unspecified semantics.

- **Singular CTM is refused.** A `cm` sequence whose accumulated CTM
  has determinant exactly 0 (finite elements, non-invertible) makes
  painting undefinable; we return `Error::Unsupported` at paint time
  rather than emit nothing. Near-singular but invertible matrices
  (large anisotropy, tiny determinant) are still rendered.

- **Page geometry is exact.** The user→pixmap transform composes
  CropBox offset, `/Rotate` (clockwise as displayed), dpi/72 scaling
  and the y-up→y-down flip. Pixel dims use checked arithmetic and the
  render limits are validated before allocation.

- **Colour conversion is device-space only.** `DeviceGray`/`DeviceRGB`
  map directly; `DeviceCMYK` uses the naive `1 − min(1, c + k)`
  formula (§10.4.3 note) — explicitly *not* colour-managed.
  `DefaultGray`/`DefaultRGB`/`DefaultCMYK` substitute the matching
  device space (§8.6.5.3). Out-of-range colour values clamp to [0,1]
  at output (the device clips them).

## Safety / hostile input

Every unbounded operation has a `Limits` bound:

| Limit | Guards |
|---|---|
| `max_input_bytes` | total input size |
| `max_reference_depth` | indirect-resolution budget (shared per Document) |
| `max_page_tree_depth` / `max_page_tree_nodes` | tree recursion / inflation via huge `/Kids` |
| `max_array_elements` / `max_dict_entries` | single-object collection size |
| `max_string_bytes` | name/string length |
| `max_nesting_depth` | `[[[…]]]` / `<<…>>` recursion and `(` nesting |
| `max_stream_bytes` | decoded size of a single stream (checked after each filter stage) |
| `max_total_decoded_bytes` | cumulative *work* (bytes decoded) across all streams in a Document; cached ObjStm reuse does not re-charge |
| `max_filter_chain` | filters applied to one stream |
| `max_objstm_entries` / `max_objstm_bytes` | `/N` and `/First`+body bounds in object streams |
| `max_operations` / `max_operand_stack` / `max_gsave_depth` | parsed ops per page, pending operands, `q` stack depth |
| `max_render_pixels` / `max_render_total_pixels` / `max_render_buffer_bytes` | output pixmap size |
| `max_path_elements` | path-construction commands per painted path |
| (xref) section & entry counts | `/Prev` chains and subsection loops |

Additionally: cycle detection on reference chains, the `/Prev` chain,
and page-tree node revisits. Object-stream bodies are decoded once per
Document and cached, so repeated member access does not multiply the
decoded-bytes cost. `unsafe` is not used anywhere.

## Licensing policy

- kasane-pdf's own license is **undecided** (private commercial
  distribution must remain possible; AGPL has been rejected by the
  developer). Until decided: all rights reserved, `publish = false`.
- Dependencies: only permissive licenses (MIT / Apache-2.0 / BSD /
  Zlib-style). No copyleft code may be linked into the engine.
  Current dependencies: `flate2` (MIT or Apache-2.0) with the
  `rust_backend` (`miniz_oxide`, MIT) for `/FlateDecode`; `tiny-skia`
  (BSD-3-Clause) for rasterization; `png` (MIT or Apache-2.0) for PNG
  encoding; `jpeg-decoder` 0.3 (MIT or Apache-2.0, `rayon` disabled)
  for `DCTDecode` image data; `ttf-parser` 0.25 (MIT or Apache-2.0,
  `std` + `glyph-names` only) for embedded TrueType outlines.
- Acceptable building blocks per the project brief: compression,
  image decoding, font handling, generic 2D rasterization — e.g.
  `flate2`/`miniz_oxide` (MIT), `freetype`/`fontdue`, `tiny-skia`
  (Skia-derived, BSD). Each addition must have its license and
  redistribution terms re-checked and recorded here before use.
- PDFium (BSD) / MuPDF (AGPL) / Poppler (GPL) are excluded from the
  product path by policy; MuPDF and Poppler are doubly excluded by
  license.

## Future milestones (long-term roadmap)

1. Parsing — object streams, xref streams, hybrid-reference files,
   stream filters (`FlateDecode` + PNG/TIFF predictors). *(M1 done;
   other filters still unsupported.)*
   - M2 done: content-stream parsing into typed `Operation`s, `Matrix`
     type, page-resource lookup.
   - M3 done: basic raster rendering (paths, clipping, Device colours)
     to PNG via tiny-skia.
   - M4 done: Form and Image XObjects (`Do`) — form `/Matrix`/`BBox`/
     own-`Resources` handling, 8-bit DeviceGray/RGB images with raw/
     Flate/DCTDecode, recursion and resource limits.
   - M5 done: embedded TrueType text — `/FontFile2`, `WinAnsiEncoding`
     (plus `Differences`), `Tj`/`TJ`/`'`/`"`, rendering modes 0–3.
     No shaping, kerning, or hinting.
   - M6 done: `/ToUnicode` CMap parsing + positioned `extract_text`.
   - M6.1 done: ActualText/OC refusal, Unicode output budget,
     headless extraction, documented coordinate semantics.
   - M7 done: Type 0 composite fonts — `/Encoding /Identity-H`,
     CIDFontType2 descendant, `/FontFile2` TrueType program,
     `/CIDToGIDMap` (Identity / stream / absent → Identity),
     `/DW`/`/W` widths (both group forms, unexpanded), 2-byte
     ToUnicode (uniform 2-byte codespace; mixed-width refused).
     `Tw` never applies (no single-byte code 32 exists).
   - M7.1 done: verification against a real Japanese font — 17-glyph
     OFL subset of Noto Sans JP (`tests/fixtures/KasaneJP.ttf`,
     generated by `make_jp_font.py`) with code=CID ≠ GID, plus
     numeric §9.4.4 advance regression tests covering both M5→M7
     fixes (TJ ×Tfs·Th; extraction Tc/Tw ÷Tfs). Remaining:
     Identity-V/vertical writing, CIDFontType0/CFF, arbitrary
     Encoding CMaps, `/UseCMap`, non-embedded fonts, shaping.
2. Rendering — transparency, patterns, ICC colour, masks
   (`/SMask`, `/ImageMask`), Type 0/CID + non-embedded fonts,
   remaining filters.
3. Text extraction — font encodings, CMaps, ToUnicode, CJK + vertical
   writing, positioned extraction, search.
4. Editing & saving — page/annotation/link editing, full rewrite output.
5. Safe redaction — see below.

## Redaction design constraints (recorded, not implemented)

These requirements shape document-model choices being made now:

- Painting a black rectangle over content is **not** redaction; the
  underlying text/images must be removed from the content stream.
- Invisible/OCR text and image soft masks (`/SMask`) must be handled.
- Shared resources: removing an image or Form XObject that is also used
  outside the redacted area must not corrupt the retained content —
  object-level analysis of all referencers is required.
- Metadata surfaces: annotations, form fields, alternate text
  (`/Alt`, `/ActualText`), document metadata, embedded files.
- Distribution output is a **full rewrite**: no incremental update, no
  leftover objects — which M0's parse-into-owned-model approach is
  aligned with.
- Link policy must be explicit: links overlapping the redaction area
  are removed; links fully outside must be preserved where possible.
- Structures we cannot safely process must produce a clear error, not a
  best-effort result.
- Distinguish "remove the occurrence at this position" from "remove all
  occurrences of this information" (e.g. a repeated footer string).
- Safety is verified externally: text/image extraction by an
  independent engine plus internal-structure inspection.

## Specification references

- ISO 32000-1:2008 (PDF 1.7) + Adobe errata — section numbers cited in
  code comments.
- ISO 32000-2:2020 (PDF 2.0) — consulted for clarifications; features
  new in 2.0 are tracked separately.
- Record consulted sections in code comments as `§x.y.z`.
