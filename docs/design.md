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

## Module map (M8)

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
  m8.rs        M8 rewrite round-trip: object collection, ref remap,
               refusal conditions, link/annot preservation, ObjStm
               and incremental input, determinism
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
   - M8 done: full rewrite (`rewrite` API + CLI) — DFS from the
     trailer /Root (plus /Info), dense renumbering, classic xref
     output, /Length recomputed, stream bytes verbatim, links and
     annots carried; signed/XFA/OCProperties/RichMedia/AA refused.
     Not an editor, redactor or sanitizer — see coverage.md.
   - M8.1: rewrite guarantees tightened — named destinations
      (name/string `/Dest`, `/Names /Dests`, catalog `/Dests`,
      GoTo `/D` and OpenAction `/D` names/strings) are refused
      outright; output version = `max(header, catalog /Version)`
      with `2.0`+ refused; stream `/Length` emitted directly while
      the indirect ref slot is skipped at collection; CLI same-file
      check uses canonicalize + Unix (dev,ino); temp file uses
      `create_new(true)` + `sync_data` + `rename`. See coverage.md.
   - M8.2: rewrite validation — header-version parse failure no longer
      falls back to 1.7 (returned as `None`, rewrite refuses);
      catalog `/Version` present-but-malformed refuses rather than
      being ignored. Fixture `/Resources` added where legitimately
      absent (blank pages); adversarial inputs kept as-is. CLI
      integration tests via `CARGO_BIN_EXE`; `write_atomic` helper
      extracted for unit tests (create_new collision/symlink/rename
      failure). `scripts/verify_rewrite.sh` runs qpdf on all
      well-formed fixtures — exit 0, zero warnings required.
   - M9: minimal edit — `Document::edit()` returns an `Edit` session
      holding `/Square` additions as synthetic objects in a reserved
      number space (u32::MAX−1M+). `Edit::rewrite()` runs the normal
      rewrite path through an `(obj,gen) → body` overlay consulted
      before `doc.resolve`, so added annotations and the edited page
      dict/`/Annots` arrays land in a normal single-revision file.
      A page's `/Annots` is always materialised into a fresh array —
      shared arrays are never mutated, so annotating one page cannot
      change another. Original bytes are read-only throughout.

   - M9.1: annotation geometry corrected — the annotation Rect is
      the outer bounding box (border drawn *inside* per §12.5.5 +
      "Border Styles"). The appearance form uses BBox [0 0 w h] +
      Matrix [1 0 0 1 llx lly] so the transformed box equals Rect
      (A = identity → AA is a pure translate; stroke width is never
      rescaled) and strokes the *inner* rect inset by half-width.
      Synthetic object numbers are allocated downward from u32::MAX
      skipping any (num,0) present in the input xref or already
      taken — a synthetic ref can never alias a real object.
      Exhaustion and max_edit_objects are typed errors.

   - M9.2: synthetic-number collision hardened against generation
      mismatches and dangling input refs. Per §7.5.2 a
      `(number, generation)` pair identifies an object, so a number
      known to the input xref at *any* generation or entry type
      (in-use, free, compressed) is *reserved* — the allocator skips
      it rather than reissue it at generation 0. Separately, the
      collector records which `(num, gen)` were allocated as
      synthetics; a reference found in an *input* body that lands on
      such a number is a dangling input ref and is resolved via the
      document only — never silently resolved to a synthetic body.
      Refs inside overlay (edit-created) bodies may resolve to
      synthetics. Together this guarantees a malformed `(n, 0)` ref
      fails as `MissingReference` both before and after edit, and a
      valid `(n, 1)` object is preserved verbatim.

   - M10: `inspect` CLI — read-only structural dump of a single
      page (inherited MediaBox/CropBox/Rotate, `/Annots` count,
      per-category `/Resources` counts, `/Contents` ref(s),
      `/UserUnit`). Fixed line order; malformed structure
      (non-array `/Annots`, non-dict `/Resources`, non-ref
      `/Contents`, non-numeric `/UserUnit`) is a typed error, not
      coerced — inspect reports the file as it is or fails.

   - M11: `inspect` annotation dump — after the fixed page lines,
      one `annot <i> <subtype> rect …` line per `/Annots` element in
      array order. An element may be an indirect ref to a dict (the
      usual form) or a direct dict — real files embed annotations
      inline; both resolve to the dict for dumping. A valid `/Rect`
      (4 numbers) and name `/Subtype` are required; every remaining
      key is printed in byte order with values formatted recursively
      (refs verbatim, streams as `<stream>`, strings escaped).
      Refs inside annot values are shown, never resolved — inspect
      does not chase into `/AP`, `/A`, etc.

   - M12: `inspect` content statistics — between the `/Resources`
      counts and the `contents` line, `tokens` (every lexer token,
      operands + operators), `operators` (parsed op count), and
      `opstats` (per-name tallies, byte order) describe the
      concatenated `/Contents` streams. Absent `/Contents` → all
      zeros; a malformed stream is a typed error, not a partial
      count.

   - M12.1: `opstats` reports *actual* operator tokens, not the
      grouped `Operation::name` labels — `SC` and `SCN` are counted
      separately (never `SC/SCN`), `sc`/`scn` likewise, and an
      `Unknown` op inside `BX…EX` appears under its real name.
      `parse_content_stream_named` records each operator's bytes
      alongside the parsed `Operation`.

   - M13: `inspect` resource enumeration — after each `/Resources`
      category count, the named entries are listed one per line:
      `font F1 Type1 /Helvetica`, `xobject Im1 Form /BBox=[…]`,
      `colorspace Cs1 /DeviceRGB`, `pattern P1 1`, `shading Sh1 2`,
      `extgstate GS1 /ExtGState`. Only the top-level dict entries —
      no recursion into nested resource dicts. A resource that
      resolves to a non-dict (font/ExtGState) or non-stream
      (XObject) is a typed error.

   - M14: `inspect` document info — after the annot lines, a
      trailing `document`/`version`/`info`/`key` block dumps the
      document-level state: `version` reports the effective PDF
      version (the larger of the `%PDF-M.m` header and the catalog
      `/Version` name; a malformed header or a malformed `/Version`
      is a typed error, never a guess), `info` reports the trailer
      `/Info` entry count, and `key <name> <type>` lists each key
      in byte order with its PDF type name (`string`, `name`,
      `int`, `real`, `bool`, `array`, `dict`, `stream`, `ref`,
      `null`) — values are never resolved or shown, so a dangling
      `/Info` is a typed error while a `Producer 6 0 R` value prints
      `ref` without chasing it.
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

## M16 — pre-redaction diagnosis

`src/redact_diag.rs` implements `diagnose_redaction` +
`preflight-redaction` — a **read-only** preflight that walks a page's
content as a reader (not a renderer) and reports what a future
redaction would have to handle.

Why a separate walker instead of reusing `Renderer`: the render
interpreter skips path/image/clip work in extraction mode and
*refuses* on `gs`/`sh`/patterns — those exact constructs are the
blockers a diagnosis must *report*, not abort on. The walker
(`DiagWalker`) mirrors the render lifecycle (gs stack, CTM, text
state, `Do` recursion with `xobject_stack`, `/Resources` swap) but
records occurrences instead of painting.

Key semantics — every choice is conservative because a wrong "safe"
verdict is worse than a verbose report:

- `targets` vs `uncertain` vs `blockers` vs `incomplete` vs
  `unexamined` are kept distinct so "nothing found" is never confused
  with "could not look". `Diagnosis` has no boolean verdict.
- `Site { contents, op_index, form_path, code_index }` is the minimum
  identity needed to re-find an occurrence: `/Contents` element refs
  (every element when the array is used — `page_operations` already
  treats them as one logical stream), a parsed op index, the
  outermost-first `Do` chain (each [`FormFrame`] carries the
  call-site `op_index`, so two `Do`s of the same object are distinct
  sites), and `code_index` — the character's position in the op's
  code stream. For `Tj`/`'`/`"` it counts codes in that one string,
  restarting at 0; for `TJ` it counts *across the whole array* so
  `[(A) 100 (B)]` reports 'A' at index 0 and 'B' at index 1 —
  numeric elements produce no codes and do not advance it. No
  source map, no generic IR.
- `DiagGs` is the *whole* graphics state the diagnosis touches —
  CTM, clip, stroke state (`w`/`J`/`j`/`M`), and the text state
  (`Tc`/`Tw`/`Tz`/`TL`/`Ts`/`Tr`/`Tf` — Table 52 parameters). `q`/`Q`
  save and restore all of it, so an inner `w` or `Tf` never leaks;
  a `Form` `Do` pushes and pops the same struct for its inner ops.
  `Tm`/`Tlm`/`in_text` are *not* graphics state — they live on
  `DiagWalker` inside `BT`/`ET`.
- Text bounds are the glyph's *outline* bbox or an *estimated cell*
  (labelled `TextBounds::Cell` for space/`.notdef`/missing outlines)
  — never silently presented as a safe redaction region. A
  `glyph_id`/`outline`/`units_per_em` *error* (broken cmap, limit,
  unsupported encoding) reports `uncertain` — never a silent cell
  estimate; `LimitExceeded` propagates.
- Stroke-mode text (`Tr` 1, 2, 5, 6) inflates the glyph bounds by the
  same `stroke_reach` a path uses — a text stroke extends outside the
  ink bbox exactly like a path stroke. Clip-mode text (`Tr` 4–7) is
  a blocker (unsupported by the engine) + an `uncertain` entry — its
  extent is not estimated.
- Image bounds are the unit square under the `Do` CTM — the *whole*
  image extent, so rotation/shear/clip over-report rather than
  under-report.
- Path bounds inflate the construction points by the stroke reach —
  `w/2` for butt/round/square caps and joins, `miter_limit × w/2`
  when miter joins are in effect — scaled by `√2 × max(|col₁|,
  |col₂|)` of the CTM, the provable upper bound on a unit vector's
  stretch. `sqrt(|det|)` — the *geometric* mean of the singular
  values — is **not** conservative: under `10 0 0 0.1` it returns 1
  while the actual stretch is 10.
- A `w` of 0 is a device-pixel **hairline** — its coverage is
  resolution-dependent and cannot be bounded in user space, so a
  *stroked* path or stroke-mode text (`Tr` 1, 2, 5, 6) at `w == 0`
  reports as `uncertain`, never a certain `Target` whose bounds
  would be claimed "outside the rect". Fill-only ops (`f`, `f*`,
  `Tr` 0, 3) are unaffected — they do not stroke; fill+stroke ops
  (`B`, `b`, `B*`, `b*`) report the whole occurrence as uncertain so
  the stroke's un-evaluated reach is not dropped.
- Sharing analysis is bounded: `site_map` on this page + a
  `/Resources` scan of ≤`max_diag_share_pages` other pages, ≤
  `max_diag_share_refs` entries. `shared_scope.truncated` is reported;
  "not shared" is only claimed for *scanned* pages.
- Anything the walker cannot interpret (unsupported font, non-finite
  CTM, out-of-order path ops, inline images) stops the walk and
  records `incomplete` — partial results are never mistaken for
  "all clear". An *uninterpreted* state-changing op (`gs`, `sh`,
  Pattern colour space) does not stop the walk but marks subsequent
  geometry `uncertain` until `Q` or a form's end restores known
  state — the flag is saved and restored with `DiagGs`.

CLI: `kasane-pdf preflight-redaction in.pdf 1 --rect llx lly urx ury
[--rect …] [--format text|json]` — read-only, deterministic order,
capped output, exit 0 on a completed diagnosis (with or without
blockers), 1 on a typed error, 2 on usage errors. The name is
deliberately not `redact`.

## M17 — experimental scoped text removal

`src/remove_text.rs` implements `remove_text` + the
`experimental-remove-text` CLI — **not** a redaction tool, and the
name deliberately avoids `redact`/`sanitize`. It is an experiment in
character-level content editing: delete the shown character codes
whose filled-ink bounds hit a caller rect, rewrite the affected
strings so survivors keep their positions, emit a new
single-revision PDF.

Design choices:

- **Selection = M16's `DiagWalker`** — the same per-code targets the
  diagnosis produces (`filled` glyph outline bounds, edge-touching
  counts). One code = one removal unit even when it expands to
  several Unicode scalars. Any `blocker`/`uncertain` entry, or a
  mid-stream walk failure, is a typed refusal — the walker has
  already reasoned about `q`/`Q`, `cm`/`Tm`/`Td`/`TD`/`T*`, font
  width/space tables, `Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts`, and every
  construct the scope refuses. Walk errors keep their typed kind
  (`LimitExceeded`, `Structure`, `Parse`) — never flattened to
  `Unsupported`.
- **Op-time state snapshots (M17.1, extended M18)** — the walker
  records the text state each show op ran under (`Font` (Rc-cloned),
  `Tfs`, `Th`, `Tc`, `Tw`) into `show_states[(form_path, op_index)]`
  at the point `show_text` consumes it — after `"`'s `Tw`/`Tc`
  update, after `'`/`"`'s line move. The `form_path` is part of the
  key (`[]` at page level, `[frame]` inside a called form), so a
  form's inner ops under different invocations get distinct
  snapshots and never collide with page-level op indices. One
  snapshot per show op, bounded by `max_show_states` (M18.1). The
  map only records when the caller asks for it —
  `diagnose_redaction` walks with `record_show_states = false` and
  never fills it, so a diagnosis pays zero snapshot memory.
  Selection (`Target`) and rewrite read the *same* state, so a
  later `Tf`/`Tc`/`Tz`/`q`/`Q`/`BT` change can never rewrite an
  earlier show under the stream-end values — the M17.0 bug where
  `walker.gs.text` at end-of-stream was used for every op.
- **Page-level eligibility (M17.1, extended M18)** — a page is
  eligible only when *every* text occurrence on it is `Tr 0` fill,
  checked against all `walker.targets`, not just the hit ones —
  page-level and form-level alike. `Tr 4–7` and `w = 0` stroked
  text arrive via `blocker`/`uncertain` and refuse the same way; a
  `Tr` *set* but never used to show text is harmless.
- **One level of `/Form` (M18)** — a page-level `Do` targeting a
  `/Form` is edited *per invocation*: the call-site `form_path`
  (resource name + call-site `op_index`, not just the object ref)
  keys the removal scope, so two `Do`s of the same form or two
  names for the same object clone independently. The clone is a new
  stream object with the rewritten ops serialised uncompressed —
  `/Filter`/`/DecodeParms` are dropped because the body no longer
  carries the encoding they described, `/Length` is recomputed, and
  `/Resources`/`/Matrix`/`/BBox` plus every other key pass through
  verbatim. A deterministic `KasFm{n}` name (collision-checked
  against the page's effective `/XObject` dict) registers the clone
  in the page's materialised `/Resources`; only that `Do` is
  repointed. Nested `Do`s (`form_path` depth ≥ 2), `Image`/`Other`
  targets, and `/Group`/`/OC`/`/Ref` forms refuse — the last three
  via the walker's existing `blocker` path.
- **Rewrite = `Tj`→`TJ`, `TJ` element-split, `'`/`"` expansion** —
  a `Tj` losing codes becomes a `TJ` whose numeric elements restore
  the removed advance `-(w0 + (Tc+Tw)·1000/Tfs)` (`Tz` cancels — the
  TJ-number semantics is `w0 − n/1000` in glyph space before
  `Tfs·Th`); a `TJ` keeps its numeric elements and splits string
  elements around removed runs; `'`/`"` expand to `T*` + `Tj` (and
  `Tw`/`Tc` for `"`) so their state side effects survive an emptied
  string. `Tfs = 0` or `Tz = 0` is refused — the advance cannot be
  expressed as a TJ number.
- **Output = `Edit`/`rewrite_overlay`** — the new `/Contents` is a
  synthetic stream allocated through `Edit::alloc`, the page dict is
  overlaid, and `rewrite` produces a single revision. The input
  `Document` and its bytes are never modified; other pages sharing
  the original stream keep it untouched (the removed bytes still
  exist in the shared object — explicitly out of scope).
- **Cumulative budgets (M18.1)** — three limits the per-stream
  `max_operations` parse cap cannot express:
  - `max_diag_exec_ops` (200,000): every content-stream *execution*
    inside one `diagnose_redaction`/`remove_text` walk — page ops,
    ops inside a `Do`'d form, empty `Tj`s, state changes, unknown
    ops. A form invoked N times costs N× its op count.
  - `max_show_states` (200,000): entries in the `show_states`
    snapshot map; the map only records when `remove_text` asks,
    `diagnose_redaction` walks with snapshots off.
  - `max_edit_content_bytes` (64 MiB): cumulative bytes of
    *generated* stream bodies held in the `Edit` overlay — each
    cloned form body plus the new page `/Contents`. `serialize_ops`
    passes the cumulative count to a `BoundedWriter` whose
    `push`/`extend_from_slice` check the remaining budget *before*
    appending — a long `Tj` string or huge `TJ` array stops
    mid-write, never allocating past the bound. `charge_generated`
    counts each finished body; the writer's `max_output_bytes`
    still caps the final PDF independently.
- **Refusal list = diagnosis refusal list** — `/Annots`, `sh`/
  inline image/`gs`/Pattern/`ActualText`/OC, any `Do` inside a form,
  `Image`/non-Form XObject, any shown text mode other than `Tr 0`,
  `w = 0` hairlines, unsupported fonts, non-finite CTM, `/UserUnit`
  ≠ 1, vertical text — each is a typed `Error::Unsupported`, never
  a partial edit. `serialize_ops`/`write_op` reserialise every op in
  the stream (no verbatim copying); the match is exhaustive over
  `Operation`, so a future variant fails to compile rather than
  slipping through.
- **Audit trail** — `Removal { output, codes_removed, sites, scope }`
  reports which codes were removed (site identity from the
  diagnosis: `/Contents` ref + op index + code index) and the scope
  statement. `Removal`'s `Debug` impl is deliberately terse — output
  bytes and site contents are *not* printed so a stray `{:?}` never
  logs document text.

CLI: `kasane-pdf experimental-remove-text in.pdf 1 --rect llx lly urx
ury [--rect …] --output out.pdf` — `--output` is required, input and
output must differ (`refuse_same_file`), the write is atomic
(`write_atomic`), and a refusal leaves any existing output
untouched.

### M19.2 — image-region removal: exact union, Default* substitution, pre-decode budget

`src/remove_image.rs` implements `remove_image_region` +
`experimental-remove-image-region`. M19.2 fixes three issues without
widening scope:

- **Exact pixel union** — `image_pixel_ranges` splits rects into
  horizontal bands at every distinct y-boundary, then merges
  x-intervals within each band. A band is a maximal y-strip whose
  x-interval set is constant — the union is the exact set of covered
  pixels, not a bounding box. Corner-touching rects and L-shaped
  overlaps do not fill the pixels between them. The intermediate
  `Vec` is bounded by `rects.len()` (`max_diag_rects`), never by
  image height.
- **Default* substitution** — `validate_image` resolves the image's
  `/ColorSpace` name through the page's `/Resources /ColorSpace`
  dict, then applies the `Default*` substitution: a device name
  (`/DeviceGray`, `/DeviceRGB`) is checked against `/DefaultGray`/
  `/DefaultRGB` in the same dict. A non-device result is
  `Unsupported`; a missing entry is `Structure`. Substitution does
  not recurse — a `Default*` that is itself a device name is ignored.
- **Pre-decode budget** — `max_image_bytes` and `max_edit_content_bytes`
  are both checked *before* decoding. An image whose expected
  `Width×Height×ncomps` size exceeds the remaining cumulative budget
  is refused without allocating its buffer. A non-intersecting image
  is skipped before decode entirely. The decoded `Vec` is moved (not
  cloned) into the edited body — no extra copy.

CLI: `kasane-pdf experimental-remove-image-region in.pdf 1 --rect llx
lly urx ury [--rect …] --output out.pdf` — same atomic-write and
same-file-refusal rules as `experimental-remove-text`.

### M20 — scoped region removal (text + image)

`src/remove_region.rs` implements `remove_region` +
`experimental-remove-region` — a single integrated edit pass over
one page, not a pipeline of `remove_text` → `remove_image_region`.

Design choices:

- **One walk, one rewrite** — `DiagWalker::new` runs once over the
  original content stream; every target is keyed by its original
  `op_index`. Text rewrites and image `Do` renames are applied to
  the same output ops list in one pass; the page's `/Contents` is
  serialised once, `Edit::rewrite()` runs once.
- **Planning against the original `Document`** — the walk never
  re-parses. `op_index` never drifts: a `Tj` that loses codes
  becomes a `TJ` at the same index; a `Do` that hits a rect gets a
  new `KasIm{n}` name at the same index. Text edits do not shift
  image `op_index` values and vice versa.
- **Shared internal helpers** — `remove_text::rewrite_ops_for` (TJ
  kern logic), `remove_text::serialize_ops`/`BoundedWriter`/
  `charge_generated`, `remove_image::validate_image`/`decode_image`/
  `image_pixel_ranges`/`fresh_xobject_name`/
  `is_default_decode`/`resolve_colorspace_name`/
  `apply_default_substitution` are all reused; nothing is
  duplicated.
- **M20.1 — CTM recorded at walk time** — `DiagWalker::do_image`
  stores the current `gs.ctm` into `TargetDetail::Image.ctm` at
  each `Image` `Do` call site. Both `remove_region` and
  `remove_image_region` read the recorded CTM instead of calling
  `ctm_at_op` (deleted). The walk's `max_diag_exec_ops` budget now
  covers the entire CTM acquisition cost — no O(images × ops)
  rescan. Two `Do`s of the same image at different CTMs each
  record their own matrix, and `q`/`Q`/`cm` are reflected
  correctly because the walker's graphics-state stack is the
  source of truth.
- **M20's own eligibility** — not a union of M17+M19 refusals:
  `Tr 0` text only (page-level), image eligibility per M19.2, and
  `Form` `Do` refused (M18's form handling is *not* inherited).
- **Shared cumulative budget** — `max_edit_content_bytes` bounds
  the cloned image bodies plus the new `/Contents` *together*;
  `charge_generated` runs before each image decodes and before
  the page stream is serialised. The budget is not reset per
  edit type.
- **Atomic failure** — any typed error aborts the whole call;
  no partial rewrite is returned. A rect hitting nothing produces
  `Ok` with zero counters.
- **M21 — orphaned XObject name removal** — after the edit pass,
  names that were edited (old `Do` names) and are no longer used
  by any remaining `Do` in `new_ops` are removed from the page's
  `/Resources /XObject` dict. The page's effective `/Resources`
  is cloned (not mutated in place), so a shared inherited
  `/Resources` is untouched. The image object itself is left to
  the writer's reachability scan — if another page or another
  name still references it, it is emitted. `orphaned_names_removed`
  reports names removed from `/XObject`, not objects deleted.
- **M21.1 — content-based verification** — re-parse the output
  and match image streams by decoded sample bytes, not by
  original object number (the writer renumbers). A removed name
  does not imply the object was deleted; an image can survive
  via an inherited `/Resources` even when the page's `/XObject`
  no longer names it.
- **M22 — inherited `/Resources` materialisation** — each page's
  effective `/Resources` (as resolved by the page-tree walk) is
  written explicitly into the page dict. A parent `/Pages` can
  then drop its `/Resources` — but only when *every* descendant
  page has been materialised. The parent keeps `/Type`, `/Kids`,
  `/Count`, `/MediaBox`, `/CropBox`, `/Rotate` — only `/Resources`
  is removed. The same `Edit`/overlay applies the materialisation,
  the text/image edits, and the `/Contents` rewrite in one pass.
  This is a *structural* cleanup, not a redaction guarantee: an
  image that survives via another page, another `Do` name, or an
  unmaterialised `/Pages` is still emitted.
- **M22.1 — traversal fix** — a `/Pages` node *without*
  `/Resources` is still traversed for descendants; the
  `has_resources` check is applied only when deciding whether to
  strip. The target page's dict is built once: `Contents` and the
  materialised `/Resources` coexist in one `page_dict`, not via
  two overlay inserts.
- **M22.2 — post-order strip decision** — a `/Pages` node's
  `all_materialised` is computed only after its children's
  results are known (`node_results: HashMap<IndirectRef, bool>`).
  A child's failure propagates upward — a parent never strips
  `/Resources` when a descendant is unmaterialised. Page refs are
  indexed by `HashMap`/`HashSet` (O(1) lookup) instead of a
  `Vec::contains` linear scan (O(n) per kid).
- **M22.3 — end-state fix** — a node that finishes processing
  records its result in `node_results` whether it succeeded or
  failed. The `continue` guard is `next_kid_idx < kids.len() &&
  all_materialised` — a `false` result is *not* deferred, so the
  parent sees `false`, not `None`. This prevents infinite
  re-pushing of a failed child. A `/Pages` node is pushed back
  to the stack only when a child is pushed (`all_materialised`
  is still `true`); a `false` break records the result
  immediately.
- **M22.4 — testability split** — the traversal is
  `plan_resource_materialisation(doc, pages_with_resources) ->
  (pages_to_strip, node_results)`. Returning `node_results` lets
  the tests assert each node's `false`/`true` — including that a
  3-level `false` propagates to the root and that a `false` node
  is never re-pushed — instead of only observing the final strip
  outcome through `remove_region`. (The earlier M22.3 tests went
  through `remove_region` and tolerated any `Err`, which masked
  the case where the target page itself was uneditable.)
  Termination relies on the parser: `walk_page_tree` rejects
  page-tree cycles (`CycleDetected`) and enforces
  `max_page_tree_depth`/`max_page_tree_nodes` before the
  materialisation plan ever runs, so this pass adds no `visited`
  set of its own. Complexity is O(nodes + edges) — each `/Pages`
  node is popped once per incoming edge at most, and each
  finished node records exactly one `node_results` entry.

CLI: `kasane-pdf experimental-remove-region in.pdf 1 --rect llx lly
urx ury [--rect …] --output out.pdf` — same atomic-write and
same-file-refusal rules as the other experimental commands.

## Residual-information audit (M23)

`audit_residuals(doc, limits) -> ResidualAudit` is a **read-only**
inventory of where a PDF can still carry information. It exists so a
caller deciding what a "distributable" file must contain has an
enumerated list to work from — the audit deliberately does *not*
decide sensitivity, never deletes anything, and a clean report is
not a safety verdict.

Design choices:

- **Three passes, one structure.** (1) a BFS reachability walk from
  the audit roots (trailer `/Root`, trailer `/Info`, every page
  object) builds `reachable` and `referrers`; (2) structure-keyed
  passes read the trailer/catalog/page dicts, annotations, form
  fields, structure tree, name trees and every live xref object;
  (3) the xref/history pass re-reads each `/Prev` section so
  superseded bodies are reported *as superseded*, not merged away.
- **Completeness is explicit.** `walk_complete` is `true` only when
  the BFS drained its queue — an edges-cap, element-cap or depth
  stop leaves it `false`. Only then is `unreachable_live` a
  complete count; otherwise unvisited live objects are
  `unevaluated_live`. This is *separate* from `findings` output
  truncation (`max_audit_findings`), which never claims anything
  about reachability.
- **Missing references are findings, not objects.** An edge whose
  target is not in the xref is recorded as a `MissingReference`
  finding carrying the referrer path; the target counts as
  `missing_references`, never as `objects_examined` or `reachable`.
  It does not itself mark the walk incomplete (the queue still
  drains) — but it is never silently absent either.
- **Findings carry no content.** `/Info` contributes key names only;
  annot `/Contents`, field `/V`, JavaScript and stream bodies never
  enter a `note`. Streams report `(encoded_len, declared_/Length)`.
- **Reachability is explicit.** `ReachClass` distinguishes
  `Reachable` / `UnreachableLive` / `PreviousRevision` /
  `Unevaluated`. "No history" (one xref section) is reported as a
  single section — *not* as proof of no old bytes; physical slack
  stays under `unexamined`.
- **Partial coverage is stated.** `incomplete` records every scope
  stopped by a cap or an unreadable construct (with `limit: bool`);
  `unexamined` lists scopes never entered. A truncated audit never
  reads as a clean one.
- **Budgets are additive, not reusing parse limits silently.** New
  `Limits` fields: `max_audit_findings`, `max_audit_edges`,
  `max_audit_paths` (per-object referrer cap), `max_audit_walk_depth`
  (one object's container scan depth), `max_audit_content_ops`
  (per-page ops scanned for `/ActualText`). Document-level
  `max_reference_depth`/`max_total_decoded_bytes` still apply.
- **Path representation.** `PathStep::{TrailerKey, Key, Index,
  NameTreeEntry, ResourceName}` covers dict keys, array indices,
  name-tree leaf names and `/Resources` category+name; binary names
  are escaped at print time (`json_escape_bytes` / `escape_bytes`).
- **Name-tree traversal is iterative** (explicit stack, no call-stack
  growth) with two distinct guards: `seen` — nodes already fully
  expanded (shared kids walk exactly once and are reported as
  shared, not silently skipped) — and `path_refs` — the refs on the
  current root-to-node path (a kid whose ref is already an ancestor
  is a cycle, reported as `cycle:` in `incomplete`). Depth is
  `max_audit_walk_depth`; total work is `max_audit_elements` shared
  with the other traversals. Missing `/Kids` refs and resolution
  failures are recorded, never `if let Ok`-dropped.
- **Depth counts nesting, not breadth.** `walk_object_refs` carries
  the real container depth per frame; a 100-element array at depth
  1 does not trip a depth cap, while a 100-deep nest does — and
  only that branch is dropped (siblings still walk). Container
  *elements* are a separate `max_audit_elements` budget so wide
  structures are still bounded.

- **Incremental traversal (M23.2).** `walk_object_refs` no longer
  expands a container's children into the stack up front. A frame
  is one of:
  ```text
  enum Frame<'a> {
      Leaf(&'a Object),
      Array { it: slice::Iter<'a, Object>, idx: u32, path: Vec<PathStep> },
      Dict  { it: btree_map::Iter<'a, Vec<u8>, Object>, path: Vec<PathStep> },
  }
  ```
  Each `stack.pop()` yields the *next* element of the top
  container — `it.next()` — and pushes the (still-live) frame back,
  so a wide array keeps exactly one frame for its whole breadth.
  `max_audit_elements` is consumed at yield time, before any
  sibling paths are built, and the `Vec<PathStep>` is cloned only
  when the yielded element is a `Ref` (for the referrer record) or
  a container being descended into — never for a leaf element.
  The `path` inside each frame is the path *to the container*,
  extended with the yielded `Index`/`Key` step when a child is
  descended into. `enum Frame` holds no per-element state beyond
  the iterator position, so a budget stop never held the whole
  container in memory.
- **Name-tree entry failures are recorded (M23.2).** `audit_name_trees`
  used to `let Ok(..) = deref(..)` on the `/Names` root and on each
  tree root (`/EmbeddedFiles`, `/JavaScript`) — a missing or
  unresolvable ref silently produced "no attachments". Now each
  entry-point `Err` pushes `AuditIncomplete { scope, reason:
  "<key> does not resolve: <err-kind>", limit: matches!(e,
  LimitExceeded{..}) }` and the *sibling* tree continues. `/Names`
  resolving to a non-dict is likewise recorded. A legitimately
  absent `/Names` adds nothing — absence is not incompleteness.
  The same `Err → incomplete` rule applies to `audit_structure_tree`'s
  `StructTreeRoot`/`/K`/`/K[i]` derefs for consistency.
- **ObjStm wording corrected.** An earlier draft described objects
  "inside ObjStm (e.g. Image)" as undetected; streams cannot
  legally appear inside `/ObjStm` (only non-stream dicts can), so
  the actual gap is narrower: a compressed *dict* (annot,
  Filespec, etc.) goes unclassified because the audit reads xref
  entries, not ObjStm member bodies. The docs now state this
  distinction and that no classification feature was added.

- **Stack-height probe moved into production (M23.3).** M23.2's
  `Probe` test duplicated `walk_object_refs` to record
  `stack.len()` — a copy that could not detect a regression in the
  real walker. M23.3 replaces it with a `#[cfg(test)]`-only field
  `max_walk_stack` on the `Audit` struct plus a `push_frame!`
  macro that records the high-water mark at every stack push.
  Tests call the production `walk_object_refs` through a small
  `test_audit()` builder; the field and macro compile out in
  non-test builds, and no public API gains a measurement field.
  Wide-array, wide-dict, deep-nest, and budget-stop inputs all
  assert the frame count stays proportional to open containers
  (never to breadth), and `element_budget_stop_keeps_stack_small_and_incomplete`
  checks that a budget stop reports `walk_complete = false` with
  unvisited objects as `Unevaluated`, not `UnreachableLive`.
- **CLI-level incomplete verification (M23.3).** `cli_audit_incomplete_text_names_scope_and_reason`
  and `cli_audit_incomplete_json_structure` run the real binary
  (`CARGO_BIN_EXE_kasane-pdf`) against a PDF whose
  `/EmbeddedFiles` root is a dangling ref: text output shows the
  failing `scope` and `reason` (never a bare "incomplete"), JSON
  output shows the `{"scope","reason","limit":false}` object, the
  good `/JavaScript` sibling finding survives, the input bytes are
  unchanged, no PDF is written, and a `/Contents` secret never
  reaches stdout. `cli_audit_no_name_tree_no_incomplete` asserts a
  file with no `/Names` produces no name-tree incomplete.

CLI: `kasane-pdf audit-residuals in.pdf [--format text|json]` —
prints to stdout, writes no file, exit 0 on a completed audit.

## M24 — multi-page experimental region removal

`remove_regions` extends the M20 `remove_region` edit across
several pages in **one `Edit` session and one `rewrite`**. It is
still an *experimental content editor* — not redaction — and the
per-page eligibility list is unchanged.

Key choices:

- **Input shape** — `&[PageSpec]` where `PageSpec =
  (usize /*0-based page*/, Vec<(llx,lly,urx,ury)>)`. A page that
  repeats merges its rect lists; pages are walked in ascending
  index order regardless of input order. All validation (empty
  specs, empty rect lists, out-of-range page indices, malformed
  rects) runs *before* any page is walked, so a bad spec never
  leaves a half-built `Edit`.
- **One `Edit` for everything** — `edit_guard` is `Option<Edit>`
  shared across pages: each image clone gets `edit.alloc()` and
  lands in `overlay`, each new `/Contents` stream likewise, and
  each edited page's dict (Contents + materialised Resources +
  updated XObject names) is staged into `page_dicts` and inserted
  into the same overlay *after* every page's edits are computed.
  `rewrite` runs exactly once at the end. `M22.1`'s rule — Contents
  and materialised Resources live in *one* page dict — is applied
  per page before the dict is staged, so a page's contents edit can
  never be clobbered by another page's Resources materialisation.
- **Cumulative budgets carry forward** — `Limits` documents
  `max_diag_exec_ops`, `max_text_codes`, `max_xobject_invocations`,
  and `max_show_states` as whole-call caps. `DiagWalker` resets its
  counters per page, so `remove_regions` reads
  `exec_ops_consumed`/`text_codes_consumed`/
  `xobject_invocations_consumed`/`show_states_len` after each page
  and gives the *next* page the remaining budget (not a fresh
  `Limits`). `generated_bytes` is accumulated across pages the same
  way `remove_region` accumulates it within one page.
- **Resources materialisation once for the document** —
  `plan_resource_materialisation` runs over the whole `/Pages`
  tree after per-page edits are computed; only pages *not* already
  in `edited_refs` get a bare-Resources overlay insert (edited
  pages already carry their Resources inside the staged dict).
  `/Pages` stripping is unchanged — only when every descendant has
  been materialised.
- **Per-page independence** — each page's `DiagWalker` resolves
  `/XObject` names through *its* effective `/Resources`, so two
  pages that share the same image object each clone their own
  `KasIm{n}` and repoint only their own `Do`s. The unedited page
  keeps the original image stream and content; the writer's
  reachability scan drops the orphaned original only when nothing
  references it.
- **M24.1 — per-page rect cap + panic-free errors.** The merged
  per-page rect count is checked against `max_diag_rects` inside
  the merge loop with `checked_add` — a page repeated across
  several specs accumulates toward the same limit, so splitting a
  page's rects across repeated specs cannot bypass it. The cap is
  *per page* (matching the single-page API), not a document-wide
  budget. Every `page_index + 1` in an error message routes through
  `page_label()`, which uses `checked_add` — a `usize::MAX` index
  produces a `Structure` refusal instead of overflowing.

CLI `experimental-remove-regions` mirrors `experimental-remove-region`
with `--page <n> --rect … [--page <m> --rect …] --output <out>`:
1-based pages, `--rect` attaches to the nearest preceding `--page`
(a `--rect` before any `--page` is refused, as is a `--page` with
no rects), pages may repeat to add rects. Same `--output`,
same-file refusal, atomic write, exit codes as `remove_region`.

## Specification references

- ISO 32000-1:2008 (PDF 1.7) + Adobe errata — section numbers cited in
  code comments.
- ISO 32000-2:2020 (PDF 2.0) — consulted for clarifications; features
  new in 2.0 are tracked separately.
- Record consulted sections in code comments as `§x.y.z`.
