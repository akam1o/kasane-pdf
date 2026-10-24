# kasane-pdf architecture & design principles

## Goals and non-goals

kasane-pdf is an independent PDF engine: syntax parsing, document model,
content interpretation, and writing are implemented in this repository.
Existing engines (PDFium, MuPDF, Poppler, …) may be used *only* as
development-time verification tools — never as the parsing, rendering,
or saving core of the product.

kasane-pdf is developed as a standalone Rust library + CLI. Embedding
it in applications through a C-compatible API, and platform
packaging, are planned but not implemented — nothing in this crate
depends on a particular host application.

## Module map

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
               /XRefStm merging; `reconstruct` = recovery scan for
               `n g obj` + trailer/XRef-dict/Catalog /Root + ObjStm
               members → RecoveryReport (opt-in, never silent)
               /XRefStm (hybrid) merging
  filter.rs    stream /Filter decoding: Flate, LZW (+ predictors),
               ASCII85, ASCIIHex, RunLength; every output capped while
               produced; image-only filters are routed to the image path
  jpx.rs       /JPXDecode (hayro-jpeg2000): codestream/JP2 → 8-bit
               samples + optional alpha, colour declaration, limits
  bilevel.rs   /JBIG2Decode (hayro-jbig2) and /CCITTFaxDecode
               (hayro-ccitt) into packed 1 bpc rows under the image
               limits; 0 = black on output, /BlackIs1 honoured
  objstm.rs    /ObjStm parsing (header, bounds, member objects)
  matrix.rs    Matrix [a b c d e f] affine transform (§8.3)
  content.rs   content-stream parser → Vec<Operation> (§7.8, §8–§9);
               inline images (BI/ID/EI) with a computed data end
  encodings.rs StandardEncoding / MacRomanEncoding tables, uniXXXX
               and gNN glyph-name forms (§9.6.6, Annex D)
  shading.rs   shadings types 1–7: axial/radial sampled through
               function.rs + colorspace.rs into a colour table
               (param_at() per pixel); function-based sampled on a
               bounded grid; mesh/patch streams decoded (bit reader,
               flags, Decode) and rasterised as Gouraud triangles
               under max_shading_triangles + a pixel budget
  acroform.rs  AcroForm widget appearance synthesis → Form XObject
               (text/check/radio/push/combo/list, /DA /MK /Q, /Parent
               inheritance), painted through the annotation path
  annot_appearance.rs  markup annotations without /AP (Square, Circle,
               Line + endings, Polygon/PolyLine, Ink, Highlight/
               Underline/StrikeOut/Squiggly, FreeText) → Form XObject
               in page space, /CA as a transparency group
  cmap.rs      /Encoding CMaps for Type 0 fonts (codespaces,
               cidchar/cidrange, usecmap chains, /WMode); predefined
               Identity-H/V always, the Adobe-Japan1 set via cjk.rs
  cjk.rs       `cjk-fallback` feature: bundled Adobe-Japan1 CMaps,
               CID→Unicode table and Noto Sans JP fallback face for
               non-embedded Japan1 CID fonts (reported as substitution)
  optcontent.rs optional-content default configuration (§8.11): OCG
               ON/OFF, /AS View usage, OCMD /P and /VE evaluation
  colorspace.rs ColorSpace: every §8.6 family → sRGB (Device*, Cal*,
               Lab, ICCBased, Indexed, Separation/DeviceN), /Default*
               remapping, bounded nesting; records ICC approximations
  function.rs  PDF functions §7.10 (types 0/2/3/4) with size, depth,
               stack and step bounds — tint transforms today, shadings
               later
  icc.rs       ICC v2/v4 reader for matrix/TRC RGB and gray profiles
               (exact); LUT/CMYK/Lab-PCS → Unsupported with a reason;
               XYZ/Bradford/sRGB helpers shared with the Cal* spaces
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
               sequential + array → UTF-16BE; malformed entries are
               skipped and reported as warnings
  warning.rs   Warning (#[non_exhaustive]) + the bounded per-Document
               sink: non-fatal deviations processed exactly
  text_extract.rs extract_text → ExtractedText: positioned Unicode
               per character code, user-space origin/advance/bounds
  document.rs  Document: load pipeline + lazy resolve() + page tree walk
               + page_contents() + page_operations() + page_resource()
               + decoded-bytes budget + ObjStm cache
  page.rs      Page, Rect value types
  redact_diag.rs  diagnose_redaction preflight walker (read-only)
  remove_text.rs  experimental scoped text removal
  remove_image.rs experimental image-region removal (raw/Flate/JPEG,
               grid-aligned CTM)
  remove_region.rs experimental text+image region removal, multi-page,
               document-metadata strip
  remove_metadata.rs document-metadata entry removal
  audit.rs     residual-information inventory (read-only)
  rewrite.rs   full single-revision rewrite
  incremental.rs incremental save (§7.5.6): original bytes verbatim +
               appended revision (updated/new objects, xref table or
               xref stream matching the input, /Prev chain)
  signature.rs signature detection (/Sig fields, /ByteRange intact?,
               /DocMDP /P, /FieldMDP) and the incremental edit policy;
               no cryptographic verification
  edit.rs      Edit session: synthetic-object overlay for annotations,
               field values and /Info; rewrite() or save_incremental()
  serialize.rs op → bytes serialiser + BoundedWriter
  main.rs      kasane-pdf CLI (info, contents, operations, render,
               text, rewrite, annotate-rect, inspect,
               preflight-redaction, experimental-remove-*,
               audit-residuals, remove-document-metadata)
tests/       (feature-organised — see tests/README.md)
  parse_*.rs / render_*.rs / text_*.rs / rewrite_roundtrip.rs /
  edit_annotations.rs / cli_*.rs / common/ / fixtures/
```

Layering is one-directional: `lexer` < `parser` < `xref` < `document`
< `render` < `main`. `filter`, `objstm`, `matrix`, `content` sit
beside `parser`/`xref`; `render` uses `document` + `content` +
`matrix` + `limits`. The edit stack (`redact_diag` → `remove_*` →
`edit` → `rewrite`) reuses the diagnosis walker and the rewrite
pipeline rather than duplicating them. `object`, `error`, `limits`,
`page` are leaf types used everywhere.

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
  mapping and XObject execution are explicitly out of the parser's scope.

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

- **`Matrix::concat` applies the receiver first.** `a.concat(b)` is
  the row-vector product `a × b` (apply `a`, then `b`). The `cm`
  operand, a form `/Matrix` and a Type3 `/FontMatrix` all concatenate
  to the *left* of the CTM (`CTM' = M × CTM`), so the renderer writes
  `m.concat(ctm)` for them — writing `ctm.concat(m)` puts `M` after
  the CTM and misplaces anything drawn under a nested transform (a
  bug that shipped until the corpus oracle test caught it). `transform` applies
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
  parsed `Operation`s against `tiny_skia::Pixmap`. Whatever it cannot
  draw *exactly* is `Error::Unsupported` and produces no output —
  never a half-rendered page passed off as success. Shadings,
  patterns, knockout transparency groups, ExtGState soft masks,
  `/Matte` soft-mask images, `/UserUnit` ≠ 1 and Optional Content
  (`/OC` or `/OCMD` marked content whose `/Properties` entry resolves
  to an OCG/OCMD dictionary) are refused — the latter because
  visibility is document-level state we cannot evaluate honestly.
  Two approximations the spec itself permits are allowed but
  *reported* on `RenderedPage` rather than hidden: bundled substitute
  fonts (`font_substitutions`) and `/Alternate` colour for ICC
  profiles the engine does not evaluate (`color_approximations`).
  Images are decoded once into a straight-RGBA buffer (`/SMask`,
  `/Mask`, `/ImageMask` folded into alpha) and composited per pixel
  with the §11.3.8 formula, which also handles the transparent
  backdrop of an offscreen transparency-group layer.

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
| `max_total_decoded_bytes` | cumulative *work* (bytes decoded) across all streams of one operation (`render_page`, `extract_text`, `rewrite`, incremental save) plus the retained ObjStm cache; refreshed per operation so page 40 of a long document has the same budget as page 1; cached ObjStm reuse does not re-charge |
| `max_filter_chain` | filters applied to one stream |
| `max_objstm_entries` / `max_objstm_bytes` | `/N` and `/First`+body bounds in object streams |
| `max_operations` / `max_operand_stack` / `max_gsave_depth` | parsed ops per page, pending operands, `q` stack depth |
| `max_render_pixels` / `max_render_total_pixels` / `max_render_buffer_bytes` | output pixmap size |
| `max_path_elements` | path-construction commands per painted path |
| (xref) section & entry counts | `/Prev` chains and subsection loops |

Additionally: cycle detection on reference chains, the `/Prev` chain,
and page-tree node revisits. Object-stream bodies are decoded once per
Document and cached, so repeated member access does not multiply the
decoded-bytes cost. `unsafe` appears in exactly four places, all in
`src/font.rs`: a parsed `ttf-parser` face or CFF table is given a
`'static` lifetime while it borrows the font program owned by the same
struct, which never moves or mutates that buffer (each site carries a
`SAFETY` comment). Nothing else in the crate is `unsafe`.

## Licensing policy

- kasane-pdf is licensed under the **Apache License, Version 2.0**
  (`LICENSE`; `license = "Apache-2.0"` in both `Cargo.toml` files).
  Per-file SPDX headers are not used — the repository-level licence
  applies to every source file, as is usual for Rust crates.
- Dependencies: only permissive licences (MIT / Apache-2.0 / BSD /
  Zlib / 0BSD). No copyleft code is linked into the engine. The
  full list of crates in the engine's dependency graph with their
  licences is kept in [`third-party.md`](third-party.md); a new
  dependency must have its licence and redistribution terms checked
  and recorded there before use. `tiny-skia` (BSD-3-Clause) requires
  its copyright notice to be reproduced in binary distributions —
  see `NOTICE` and `third-party.md`.
- Bundled font data is **not** under the Apache licence:
  `fonts/liberation/` — Liberation Sans/Serif/Mono 2.1.5 (SIL Open
  Font License 1.1, unmodified, provenance and SHA-256 in its README)
  as the substitutes for non-embedded Standard 14-style fonts; two
  OFL font subsets among the test fixtures (`NOTICE`,
  `tests/fixtures/README.md`). The OFL permits bundling the fonts in
  software, including sold software, as long as the fonts are not
  sold on their own and the licence text ships with them; it does not
  affect the crate's own licence. No font is ever loaded from the
  host OS. Symbol and ZapfDingbats have no licence-clean metric clone
  and are refused rather than approximated.
- The optional `cjk-fallback` feature embeds assets fetched at build
  time (Noto Sans JP, OFL 1.1; Adobe CMap / mapping resources,
  BSD-3-Clause) — never committed; their licence files are fetched
  with them.
- PDFium (BSD) / MuPDF (AGPL) / Poppler (GPL) are excluded from the
  product path by policy — they are used only as development-time
  verification tools (`compat/runner`, `scripts/verify-*.sh`) and
  are never linked.

## Roadmap

What exists today is described by the feature files under
[`features/`](features/) and summarised in the README; the
step-by-step history of how it was built is kept in
[`decisions/history.md`](decisions/history.md). Remaining work, in
the order the compatibility corpus suggests (see
[`compat-corpus.md`](compat-corpus.md) for the measured blockers):

1. **Rendering** — knockout transparency groups; a shading pattern
   `/ExtGState` override; non-embedded CJK fonts beyond the
   Adobe-Japan1 fallback (GB1 / CNS1 / Korea1); a per-render
   diagnostics channel so recoverable faults (an unterminated Flate
   stream, a malformed `/ToUnicode`) can be reported instead of
   refused.
2. **Recovery mode** — null handling for dangling references
   (§7.3.10) and tolerant content-stream parsing, kept strictly apart
   from the strict parser.
3. **Editing** — page-level operations (insert / delete / reorder),
   link editing, more annotation subtypes through the incremental
   writer; a C-compatible API for embedding.
4. **Signatures** — comparison with an external verifier, `/UR`
   usage rights, timestamp and PAdES structures (still never a
   cryptographic validity claim from this crate).
5. **Encryption** — standard security handler (RC4 / AES) for
   reading, with the rewriter re-encrypting or refusing.
6. **Safe redaction** — a redaction tool built on the diagnosis and
   editing primitives that can *prove* removal; today's
   `experimental-*` commands are content editors only.

## Specification references

- ISO 32000-1:2008 (PDF 1.7) + Adobe errata — section numbers cited in
  code comments.
- ISO 32000-2:2020 (PDF 2.0) — consulted for clarifications; features
  new in 2.0 are tracked separately.
- Record consulted sections in code comments as `§x.y.z`.
