# Limitations

What kasane-pdf does **not** do today, collected from the feature
files under [`features/`](features/) (each refusal there is a typed
error — nothing in this list is a silent skip). Items marked *planned*
are on the [roadmap](architecture.md#roadmap).

## Documents

- **Encryption**: the Standard security handler is read (R2–R6, RC4,
  AES-128, AES-256, crypt filters) with the empty user password or a
  password given through `Document::open` (SASLprep for R5/R6,
  PDFDocEncoding for R2–R4). Not implemented: public-key and
  third-party handlers, writing encrypted output (the writers refuse
  encrypted documents), rebuilding the cross-reference table of a
  damaged encrypted file. The undocumented Acrobat padding of R4 keys
  shorter than 128 bits is a recovery rule: strict mode refuses a file
  that needs it. See
  [`plans/encryption.md`](plans/encryption.md).
- **Strict parsing**: `Document::from_bytes` follows ISO 32000-1.
  Dangling references, a wrong `startxref`, a damaged cross-reference
  table, objects not closed by `endobj` and similar faults are errors.
  `Document::from_bytes_recovering` rebuilds the cross-reference table
  by scanning and reports every assumption. A reference to an
  *undefined* object (no cross-reference entry, or a free one) is null
  in both modes, as §7.3.10 requires, and recorded as
  `Warning::UndefinedReference`; a reference whose entry points at
  damage is still an error. Recovery mode additionally applies the
  `RecoveryRule`s (US Letter for a page without `/MediaBox`, a stray
  `Q` ignored, an unknown operator dropped, a state or colour operator
  inside a path object applied, an invalid `/Annots` entry skipped),
  each recorded as `Warning::RecoveryApplied`; strict mode refuses
  every one of them.
- **Accepted with warnings** is a narrow third outcome, not a lenient
  mode: only deviations the engine can process *exactly* are
  recorded on the `Document` as a `Warning` instead of refused
  (today: a Flate stream missing its final block after a sync flush,
  a malformed `/ToUnicode` entry). Everything else that is not
  conforming is still an error in strict mode.
- **PDF 2.0**: files are read; features new in 2.0 are not interpreted
  and the rewriter refuses a document that declares version 2.0.
- `Document` holds interior caches (`Cell`/`RefCell`) and is not
  `Sync`; use one instance per thread.

## Rendering

- **Knockout transparency groups** (`/K true`) are rendered per
  §11.4.8 (each object against the group's initial backdrop), with
  opacity kept out of each object's shape — also inside nested forms —
  and `AIS` honoured. Shape is taken from the rasterised coverage of
  one painting operator (anti-aliased edges are fractional shape); a
  nested non-knockout group counts as one object. Bounded by
  `max_knockout_objects`.
- **Shading pattern `/ExtGState`** override — refused.
- **Non-embedded fonts**: Symbol and ZapfDingbats draw with a bundled
  DejaVu Sans subset through their built-in encodings and the Adobe
  AFM widths (`SubstKind::SymbolFallback`) — glyph shapes differ from
  Adobe's and the substitution is always reported; non-embedded CID fonts
  are refused unless the `cjk-fallback` feature is enabled and the
  ordering is Adobe-Japan1 (GB1 / CNS1 / Korea1 are not bundled;
  `/Ordering (Identity)` can never be substituted soundly). Symbolic
  simple fonts with unknown names are refused. Substitutes change
  glyph shapes and are always reported.
- **Type 3 fonts in text clipping modes** (`Tr 4`–`7`) — refused.
- **Images**: `/Matte` (pre-blended soft masks), `/SMaskInData` on a
  non-JPX image, `/OC` on an image, `/Ref` — refused. An image larger
  than `max_image_dimension` renders only when it is a JPEG drawn small
  enough for a reduced inverse DCT (reported in `image_approximations`)
  or a plain soft mask sampled at its compositing grid; otherwise the
  limit is a `LimitExceeded`.
- **Colour**: no colour management beyond exact matrix/TRC ICC
  profiles; LUT, CMYK and Lab-PCS ICC profiles fall back to the
  `/Alternate` space and are reported in `color_approximations`.
  Non-`/Default` transfer functions, black generation and undercolour
  removal are refused; halftones and smoothness are accepted and
  ignored (no effect on this rasteriser). DeviceCMYK uses the
  §10.3.5 conversion (no colour management: Poppler and Quartz, which
  manage CMYK, differ from it by ΔE00 ≈ 5–25 on saturated colours; the
  compat runner reports such pages, they are not defects).
- **Text**: no hinting, no shaping (ligature substitution, kerning
  from GPOS); glyphs are placed exactly where the content stream
  puts them.
- **Annotation flags**: Hidden, NoView and NoRotate are honoured. NoZoom
  has no effect: a render at a resolution is the page at magnification
  1, and `RenderOptions` carries no viewer zoom separate from `dpi`. An
  `/F` that is not an integer is read as 0 (no flags), as before.
  Poppler additionally treats every `/Text` annotation as NoRotate and
  NoZoom; kasane-pdf follows the annotation's own `/F`.
- **Which limits parse a page**: `render_page` and `extract_text`
  parse the page's own content streams with the `Limits` the document
  was opened with (`max_operations` and the parser bounds), and every
  nested stream (forms, patterns, Type 3 glyphs, appearances) with the
  limits passed to the call. To bound the page's own parse tighter,
  open the document with those limits. (Unchanged since 0.1.)
- **Thin strokes** are drawn at their true coverage (anti-aliased);
  Poppler and Acrobat widen strokes thinner than a device pixel to one
  pixel, so hairline and 0.1-pt drawings look lighter here. Only
  `0 w` gets the one-pixel width (§8.4.3.2).
- Mesh shadings interpolate colours in sRGB after conversion, not in
  the source space; function-based shadings are sampled on a 128×128
  grid. Both are recorded as approximations in the feature file.

## Text extraction

- Positioned characters only: no layout analysis, reading order,
  word or line grouping, or search.
- A code the font cannot map to Unicode is reported as `None`, never
  guessed — including codes whose `/ToUnicode` entry was malformed
  and skipped (recorded as `Warning::MalformedToUnicodeEntry`). Text
  drawn through optional content that is OFF is excluded;
  `/ActualText` replaces the drawn codes.
- Without `/ToUnicode` (`ResolveKind` tells which source answered):
  glyph names through the Adobe Glyph List, the embedded program's
  Unicode `cmap` read backwards, and Adobe-Japan1 CIDs (with
  `cjk-fallback`). Unresolved on purpose: names without glyph-list
  meaning (`g12`, `glyph7`), symbol-only (3,0) `cmap`s, glyphs reached
  from several unrelated code points, private-use mappings, CID-keyed
  CFF fonts of the Identity ordering, the GB1 / CNS1 / Korea1
  collections (no bundled UCS2 mapping), `cmap`s covering more than
  131 072 code points, and Japan1 codes where the embedded program
  disagrees with the collection. A glyph name is trusted as the font
  declares it (a font that names a glyph `A` but draws something else
  is reported as `A`). For simple fonts other than Type 3 the older
  name table is still consulted first, unchanged, so its readings stay
  as before: ZapfDingbats names such as `a12` read as dingbats and
  `uniXXXX` names are read as written, private-use values included.
- Annotation text (`TextOptions::with_annotations`): `/RC` is reported
  as stored (XHTML, not interpreted); a field's value is read per page
  (a field with widgets on several pages reports it once per page);
  buttons and signatures have no text value; appearance text exists
  only when the annotation has an appearance the renderer draws.

## Writing and editing

- **Rewrite** produces one flat revision: superseded objects, earlier
  trailers and the incremental-update history are not carried; a
  signed document is refused; XFA, RichMedia and document-level
  actions are refused rather than carried. Optional content
  (`/OCProperties`) is carried when well formed and refused otherwise
  (missing `/OCGs` or `/D`, a group that is not an OCG dictionary, a
  wrong type, a cyclic or over-deep `/Order`); unknown keys are kept. A
  rewrite carrying it declares at least PDF 1.5 in the header (the
  effective version is the larger of header and catalog `/Version`; 2.0
  stays refused); an incremental save never changes the header.
  Preserved objects are
  carried verbatim — the rewriter is **not a sanitizer**.
- **Incremental save** cannot delete bytes (a removed annotation's
  object stays in the file), refuses recovered documents, encrypted
  documents and non-zero-generation references, and only appends the
  edits the `Edit` session offers (metadata, annotations, form-field
  values).
- **Signatures** are detected, not verified: no digest or certificate
  check, no `/FieldMDP` evaluation (a document with one is refused),
  no `/UR` usage rights, no timestamp or PAdES structures. The engine
  never reports a signature as valid.
- **Page-level editing** (insert, delete, reorder), link editing and
  other annotation subtypes — *planned*.
- The **experimental `remove_*` editors** are content editors: they
  rewrite selected operators and pixels into new objects and report
  what they changed. They do not prove the data is gone and are not
  a redaction tool; see [`security.md`](security.md).
- Output durability: the CLI writes atomically but does not fsync the
  parent directory.

## Platform

- Pure Rust library and CLI. No C API, no mobile or desktop
  application packaging — *planned, not implemented*.
