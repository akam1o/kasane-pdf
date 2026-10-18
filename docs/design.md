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

## Module map (M1)

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
  document.rs  Document: load pipeline + lazy resolve() + page tree walk
               + page_contents() + decoded-bytes budget + ObjStm cache
  page.rs      Page, Rect value types
  main.rs      kasane-pdf CLI (info + contents subcommand)
tests/
  fixtures/    hand-crafted PDFs + make_fixtures.py generator
  parsing.rs   M0 integration tests (kept)
  m1.rs        M1 integration tests
```

Layering is one-directional: `lexer` < `parser` < `xref` < `document`.
`filter` and `objstm` sit beside `xref` and are used only by `document`.
`object`, `error`, `limits`, `page` are leaf types used everywhere.

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
  `Document::from_bytes`; there is no "best-effort document".

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
  `rust_backend` (`miniz_oxide`, MIT) for `/FlateDecode`.
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
2. Rendering — CTM/graphics state, paths, text, images, Form XObjects,
   clipping, transparency, color spaces.
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
