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
- **PDF 2.0**: files are read; features new in 2.0 are not interpreted.
  The rewriter writes a 2.0 document as 2.0 and carries its structures
  (associated files, document parts, page metadata) as objects (0.5); a
  version above 2.0 is refused.
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
- **Non-embedded fonts in `redact`**: refused, since readers choose the
  glyphs; with `embed_substitute_fonts` a Latin Standard 14 font or its
  metric-compatible alias is embedded first — Type 1, and from 0.10 a
  non-symbolic simple TrueType font (ArialMT …). A code whose glyph the
  substitute cannot name is written as its blank space glyph (readers
  draw `.notdef` differently). Symbolic TrueType, other names and CID
  fonts stay refused. In an *embedded* program (Type 1, CFF, OpenType,
  TrueType), a code the program resolves to `.notdef` is drawn
  differently by readers (MuPDF falls back to the program's own
  encoding, pdf.js to a glyph named for the same Unicode): `redact`
  (0.10) takes its extent as every glyph the program can draw, so such a
  code goes when any of them could meet a rect, and is refused when one
  of the program's glyphs cannot be read. The bounds are computed once
  per font object. A TrueType glyph whose `hmtx` bearing disagrees with
  its outline is taken at both positions readers draw it at; one whose
  bearing does not resolve (`USE_MY_METRICS` components nested past 32
  levels) or that has data but does not outline is refused.
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
- **Resource use** (robustness work planned for 0.6): a very large
  1-bit soft mask is expanded at full resolution for each use (one
  60 KB corpus page reaches 1.3 GB); 32-bit targets build (wasm32) but
  their arithmetic is not audited nor tested in CI.
- **Nesting depth.** Form XObjects, transparency groups, soft-mask
  groups, annotation appearances and Type 3 glyph procedures nest at
  most `max_xobject_depth` levels (default 48 since 0.5.1, 32 before);
  deeper is `LimitExceeded`. The interpreter recurses on the native
  stack: the default needs ≈ 1.2 MiB in a debug build and ≈ 0.25 MiB in
  release on the costliest path (soft masks), so it fits a 2 MiB thread.
  A caller raising the limit needs ≈ 25 KiB (debug) / 5 KiB (release)
  more stack per level. Tiling patterns nest at most 8 levels (fixed).
- **Clip and soft masks** (since 0.5.1): each is one byte per page
  pixel. Saved states share them and a clip that cuts nothing keeps the
  current one, so only distinct nested clips add masks; those alive at
  once may take at most `max_render_buffer_bytes` (on their own, beside
  the page and its layers), beyond which the page is `LimitExceeded`.
  Example: a 600 pt page at 600 dpi has 25 MB masks, so about ten
  distinct clips nested inside one another; at 96 dpi, about 400.
- **Extreme geometry** (since 0.5.1): a stroke wider than 2^24 device
  pixels is drawn at a narrower width that still covers the whole page
  from every point when it is made of straight segments with round
  joins or miters within their limit (curves and bevels would change
  shape), and refused (`Unsupported`, "stroke width") otherwise; a path
  reaching beyond ±2^24 device pixels is skipped when it lies entirely
  off the page (a clip there leaves nothing visible) and refused
  (`Unsupported`, "geometry extent") otherwise. Earlier releases
  panicked inside the rasteriser on both.
- **Dashed strokes** (since 0.5.1): a dashed stroke whose rasterisation
  work is estimated too high — many dash segments crossing the same
  pixel rows (Σ over rows of edges² above 10¹³ for one stroke, or
  10¹⁴ for a page's dashed strokes together), or more than 4 × 10⁶
  outline edges in all — is refused (`Unsupported`, "dashed stroke")
  rather than drawn for minutes. A page of 90 000 round-capped dots at
  600 dpi is 5 × 10¹² (drawn, 0.4 s, as in 0.5.0). The bound is on
  estimated work, not time: strokes inside it have been measured at up
  to a few seconds, and rendering time in general is not bounded
  ([`security.md`](security.md)).
- A stroke in a **pattern** colour is drawn without its dash pattern
  (since 0.1; the outline is filled with the pattern undashed).

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
  signed document is refused (an XFA packet holding an XML signature
  included); XFA forms are carried verbatim (0.5); RichMedia and
  document-level actions and scripts are refused rather than carried
  (`rewrite_with` with `ExecutableContentPolicy::Carry` carries the
  scripts; `/RichMedia` stays refused under every policy). Every other
  action is carried, and `rewrite_with` reports it. A catalog `/URI`
  base dictionary is carried and not reported (it is not an action).
  Optional content
  (`/OCProperties`) is carried when well formed and refused otherwise
  (missing `/OCGs` or `/D`, a group that is not an OCG dictionary, a
  wrong type, a cyclic or over-deep `/Order`); unknown keys are kept. A
  rewrite carrying it declares at least PDF 1.5 in the header (the
  effective version is the larger of header and catalog `/Version`; 2.0
  is written as 2.0, above is refused); an incremental save never
  changes the header.
  Preserved objects are
  carried verbatim — the rewriter is **not a sanitizer**. An input
  object stream or cross-reference stream is never rewritten as such
  (its objects are written afresh) unless an object refers to it: a
  plain rewrite then copies it; a rewrite that edits or removes anything
  (`redact`, `remove_document_metadata`, an `Edit`) refuses, since the
  copy would keep the input's objects as they were.
- **Strings readers disagree on**: a literal string with an unescaped
  carriage return is read as a line feed (§7.3.4.2; Poppler and pdf.js
  keep the CR), and an embedded CMap's codes written as literal strings
  are read like hex codes (Poppler rejects them). Rendering and
  extraction follow that reading; `redact` refuses such documents and
  fonts. In Type 0 CMaps (0.8) notdef mappings are honoured (§9.7.6.3)
  and the latest `cidchar` / `cidrange` definition of a code wins, by
  code length; `redact` refuses text showing a code readers map
  differently (notdef only, unmapped, in no codespace range, or defined
  for another code length).
- **Redaction and the structure tree** (0.8): the tree is kept; the
  elements whose content a redaction touches — and their ancestors —
  lose their text, associated files and attribute strings whole
  (elements are not split, so an element across a rect loses the
  accessibility text of its kept part too). Content outside every
  marked-content sequence relates to no element. Trees that cannot be
  edited exactly (cycles, kids of unknown kinds, a touched element
  written inside another object) are refused.
- **Redaction verification** (0.10): the use invariant compares streams
  by digest — a copy re-encoded with other filters, or the same picture
  stored differently, is not recognised; font programs, a font's code
  data and ICC profiles are not held to it (a font only the removed text
  used may stay in the page's resources with its program; it reveals
  the font's glyph repertoire); dictionaries other than annotations are
  compared only through the structure-tree and string checks.
- **Data of removed annotations** (0.10): what a removed annotation or
  field held stays when something that uses it keeps it — a kept link,
  widget or screen's action (a hidden, zero-area or off-page kept link
  counts), any kept annotation's defined keys, a form field (one without
  widgets too), an outline item, a destination, the catalog's actions
  and settings, a name tree, the structure tree. Its own data counts
  as the value of any entry defined to hold data (a URI string kept as
  another annotation's `/Contents`). Its appearance part (streams,
  resources, fonts, images, colour data) counts as used wherever the
  edited document reaches it, a private key inside a font, image or
  form included; a resource category's entries count whether the
  content uses them or not. Some unusual sharing is refused: a removed
  annotation's own data (an indirect string, a file specification)
  shared with an entry inside a kept annotation's or action's
  sub-dictionary (`/3DU`, a dash array, a launch action's `/Win`) or
  with a scalar setting (`/Lang`, `/LastModified`), or with a settings
  dictionary that has keys no reader uses (viewer preferences, mark
  information, the URI dictionary). A chain of tens of thousands of
  references to one object exceeds `max_reference_depth` and is
  refused. Readers that ignore generation numbers (MuPDF) may read a
  reference to another generation as the object kasane leaves alone.
- **Optional content membership on forms and images** (0.10): an edited
  page drawing a form or image XObject whose `/OC` is a membership
  dictionary (`/OCMD`) is refused: the output's use check counts a
  membership dictionary as off, so the form looks carried without use
  (`docs/redaction-safety-model.md`, section 4.2). 0.8.0 accepted such
  pages.
- **Appearances of kept annotations** (0.10): a shown annotation kept as
  it was keeps its own appearance streams (every state, down and
  rollover), and the same bytes as removed content there are reported;
  what an appearance it does not draw now draws in turn (a nested form,
  an image) is not excused, so such a page may be refused although the
  redaction touches no annotation (the safe direction).
- **Optional content over the output** (0.10): content under a group is
  counted as drawn only when the default viewing configuration turns
  it on, no automatic state (`/AS`) names it and its `/Usage` turns
  nothing off; a configuration with another or an empty `/Intent`, or
  `/BaseState /Unchanged`, counts as off. Some documents all readers
  draw are refused for this (the safe direction).
- **Drawn off the page** (0.10): a removed appearance or edited original
  that another page's content still draws counts as drawn even when it
  lies outside that page's boxes; it is reported in `elsewhere`, and
  `refuse_copies_elsewhere` refuses it.
- **Form fields** (0.10): a widget's field leaves the form only when no
  widget of it is left; a field keeping a widget keeps its value
  (reported in `elsewhere`) if a kept widget is shown and, for a
  button, shows the value's state; otherwise refused. Signature fields,
  JBIG2 images with shared `/JBIG2Globals` or a file header are
  refused.
- **Paths, shadings and patterns crossing a rect** (0.10): filled
  shapes are clipped (pieces of curves that straddle a cut at the
  subdivision limit become their chord); strokes are cut as before
  (dashed curves refused). An `sh` crossing a rect is cut only when it
  is an axial shading in page content, with an axis-aligned CTM and
  `/BBox`, under a rectangular clip (or none), and every value of its
  axis inside the rects is painted outside them; overlapping rects need
  opaque paint. Radial, function-based and mesh shadings, and shadings
  in forms, stay refused. A tiling pattern crossing a rect is cut only
  as the fill of one axis-aligned rectangle in page content whose part
  outside the rects holds a whole period; text, strokes and other
  shapes painted with one stay refused. Cut shadings and patterns keep
  their data whole: the shading dictionary, its function (every sample
  or coefficient), the pattern's cell stream and resources stay in the
  output and are readable with any PDF parser. The cut is accepted only
  when nothing in that data was shown by the rects alone — every
  position of the axis the rects showed, every phase of the period, is
  shown outside them on the page — so what stays is what the page still
  shows, plus what no part of the page ever showed (function values
  beyond the painted positions, a cell's content outside its `/BBox`):
  data the original carried hidden, which `redact` does not search.
- **XFA forms** (0.10): with `remove_javascript`, or when
  `remove_annotations` edits form fields, an XFA form beside its
  AcroForm is removed (`Redaction::xfa_removed`): its packets hold the
  scripts and a copy of the field values. Readers that render XFA
  (Acrobat) then show the AcroForm. A form readers must render from
  XFA alone (`/NeedsRendering true`) is refused, and so is a packet
  something else still refers to (directly or through a chain of
  references), a stream (stored or decoded) or string of the output
  holding exactly a packet's bytes, in any role, and a signed document.
  A packet's content inside larger data (part of another stream, under
  another filter than its own) is not recognised.
- **Annotations drawn away from `/Rect`** (0.10): an annotation with
  NoZoom or NoRotate kept on an edited page is refused (its appearance
  may be drawn elsewhere at another zoom), except a popup without an
  appearance stream, which draws nothing on the page.
- **Uninspected data** (0.8): `redact` reports the data it carries
  without reading it (`Redaction::uninspected`) and refuses it with
  `refuse_uninspected`; that mode is not a proof that no copy of the
  removed content remains (images and fonts are interpreted, not
  searched; no byte search is made).
- **Nesting and size limits in `redact`** (refused, never cut short):
  forms nested more than 16 deep; a form field tree deeper than 64; a
  chain of references longer than `max_reference_depth`; the `Limits`
  of the document
  (operations, codes, images) as for rendering.
- **Complexity limits in `redact`** (0.10, refused): a filled shape
  whose cut against the rects exceeds the cut budget (many word rects
  over one large curved fill, for instance); a shading or tiling fill
  whose area outside the rects splits into too many pieces. The
  pieces grow at most linearly with the rects (measured: 223 rects over
  one shading, 0.01 s).
- **Shared objects in `redact`**: an edited form or image is copied
  for the invocation that draws it inside a rect; other invocations and
  pages keep the original, reported in `elsewhere`. A content stream an
  edited page shares with another page stays as that page's contents
  (reported). A shading or tiling pattern removed with a rect, or
  painted across one without being cut, is refused when reached from
  anywhere else (another page, a form, a pattern, shared resources). A
  cut one is never modified — only the page's content changes — so it
  may be shared; its data stays whole either way.
- **Data in what `redact` keeps**: kept annotations, form fields,
  outlines, resources (fonts, colour spaces, functions, unused resource
  entries), attachments and other kept objects are carried as they
  are: their own text, values and data stay, and `redact` searches only
  the removed text in them (`elsewhere`). A font the removed text
  used stays with its whole program (its glyph repertoire, not where
  the glyphs were drawn).
- **`refuse_copies_elsewhere` and `refuse_uninspected`**:
  `refuse_copies_elsewhere` refuses when removed text (runs of four or
  more characters) is found again in the output — other pages'
  extracted text, objects' strings — when an edited original image or
  form is still drawn, or when a page could not be searched; it does
  not find the information in another form (shorter runs, other
  encodings, text inside images or fonts).
  `refuse_uninspected` refuses when the output carries data `redact`
  does not read (listed in `Redaction::uninspected`); it does not
  search interpreted data (images, fonts). Neither is a proof that no
  copy of the removed content remains.
- **Incremental save** cannot delete bytes (a removed annotation's
  object stays in the file), refuses recovered documents, encrypted
  documents and non-zero-generation references, and only appends the
  edits the `Edit` session offers (metadata, annotations, form-field
  values).
- **Signatures** are detected, not verified: no digest or certificate
  check, no `/FieldMDP` evaluation (a document with one is refused),
  no `/UR` usage rights, no timestamp or PAdES structures. The engine
  never reports a signature as valid. `UsageRightsPolicy::Remove`
  deletes usage-rights signatures only (the rights they grant are
  withdrawn) and refuses a document with any other signature, a field
  lock, `/DSS` or `/Legal`; it never re-signs. It also costs more: the
  output is reparsed and walked before it is returned, so peak memory
  is about 2.5× and time about 10× the plain rewrite's on large
  documents.
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

