# Redaction safety model

What `redact` (CLI `redact`) guarantees, what it does not, and what it
refuses because it cannot decide — with the design decisions behind
each. The user-facing contract with every refusal listed is
[`features/redaction.md`](features/redaction.md); this document is the
model that contract follows. Nothing here claims that every PDF can be
freed of all secret information: `redact` removes what is drawn inside
the caller's rectangles and what describes it, verifies what it can,
reports what it cannot verify, and refuses what it cannot process
exactly.

## 1. Terms

- **Rects**: the caller's rectangles, per page, in default user space.
- **Removed content**: everything a reader draws inside the rects —
  glyphs, image samples, path geometry, shading and pattern data,
  annotation appearances — and the data that only that drawing uses.
- **Describing data**: data that states removed content without drawing
  it — alternate and replacement text in the structure tree and in
  marked content, annotation text, document metadata, Type 3 glyph
  procedures and Unicode mappings of removed glyphs.
- **Kept content**: what is drawn outside the rects, and everything a
  page not edited draws.

## 2. What is guaranteed, per surface

Each line is **G** (guaranteed and verified on the written bytes before
returning), **R** (reported, not guaranteed), **N** (not guaranteed and
not reported), or **X** (refused: a typed error, no output, when the
property cannot be decided).

### 2.1 What is drawn

- **G** — No character code, nonzero image sample (or mask sample),
  path segment, shading or pattern painting, link or annotation is
  drawn inside the rects in kasane's reading: the same rects run again
  on the output find nothing to remove. Content a clip or `/BBox`
  hides inside the rects counts as drawn (a clip hides ink, not data).
- **X** — Where kasane's reading of what is drawn can differ from a
  reader's: unescaped carriage returns in strings, CMap constructions
  Poppler, MuPDF and pdf.js read differently (definition order, code
  length, bf mappings in an encoding CMap, definitions after
  `endcmap`, Identity pass-through, `/WMode` conflicts, odd-length hex
  codes, stray tokens, overlapping codespace lengths), text matrices
  stretched more than 16:1, non-embedded fonts without an embedded
  substitute, shadings and tiling patterns crossing a rect, curved fills
  crossing a rect, names an appearance stream does not resolve in its
  own resources, annotations whose appearance is not placed by `/Rect`.
- **N** — Rendering differences between readers that move no data
  (anti-aliasing at the rect edge, MuPDF's rounding of simple fonts'
  widths).

### 2.2 What text extraction returns

- **G** — No removed code remains in any content stream the output
  draws, so no extractor reading the output's content returns it.
- **G** — The removed glyphs' positions and advances are not kept: a
  removed run leaves one advance (the position of the next kept glyph,
  which the layout needs); per-glyph offsets, kerning and the
  positioning operators of removed text are gone.
- **R** — The same text drawn elsewhere (another page, outside the
  rects, in an annotation, in an object's strings) is found by search
  and listed in `elsewhere`; `refuse_copies_elsewhere` refuses it.
- **N** — Text that is the same as removed text but drawn with other
  codes, other fonts, or split across shows is not searched for beyond
  the extracted Unicode.

### 2.3 Structure tree, annotations, metadata

- **G** — Every structure element the tree's references (`/K`,
  `/ParentTree`, `/IDTree`, `/MCR`, `/OBJR`, `/P`) tie to removed
  content, and its ancestors, lose `/Alt`, `/ActualText`, `/E`, `/T`,
  `/Phoneme`, `/AF`, the strings of their attribute objects and their
  user-properties lists; marked-content property lists in edited
  content lose them too. The output's tree is re-read and checked.
- **G** — A removed widget's form field leaves the interactive form
  with its value when no widget of it is left. A field keeping widgets
  keeps its value only if a kept widget is shown (on a page's
  `/Annots`, an integer `/F` without Hidden or NoView, no `/OC`) and,
  for a button value other than `/Off`, a shown kept widget is in that
  state (`/AS`, with the state in its normal appearance) — what a
  removed or unshown widget had never justifies it; its string, array
  and stream values and `/Opt` are reported in `elsewhere` (else
  **X**).
- **G** — A removed annotation or form field is not written: checked by
  identity on the edited document before it is written, and against the
  rewrite's object map (the output's object set). No dictionary of the
  output — a stream's included, at any depth — has its content, except
  the entries of an output page's `/Annots` themselves (by identity: an
  annotation alike elsewhere is not excused). Its appearance streams and what they draw
  are gone, or still drawn as the lower bound of section 4.2 counts it
  (an annotation shown, its selected normal appearance — or a button's
  states and appearances as it presents them when used —, page content
  outside optional content that is off), or as an appearance (any state,
  down or rollover) of a shown annotation the output keeps as it was —
  its own data, outside the rects — and reported in `elsewhere`
  (`refuse_copies_elsewhere` refuses it). A hidden annotation's
  appearances excuse nothing.
- **G** — Anything a removed annotation or form field held (every
  object reachable from it but its page, field and reply links: actions
  and their targets, indirect and rich text, its popup, the objects of
  its appearance, streams or not) stays only where something that uses
  it keeps it: what a kept page draws, a kept annotation's keys its
  subtype defines (`/A`, `/AA`, `/Dest`, `/PA` only on links, widgets
  and screens), a form field, an outline item's action or destination,
  a destination's page, the catalog's `/OpenAction`, `/AA` and
  optional content, a name tree. Kept only through other entries
  (private data), it is refused.
- **N** — "Drawn" includes drawing outside the page's boxes: a removed
  appearance also drawn off the page by another page's content stays
  (reported in `elsewhere`; `refuse_copies_elsewhere` refuses it).
- **G** — `/Info` and catalog `/Metadata` are removed; an edited page's
  thumbnail is removed (from every page sharing it).
- **N** — Elements are not split: an element across a rect loses the
  accessibility text of its kept part too. Non-string attribute values
  other than user properties, `/ID`, `/Lang` and keys no structure
  defines are kept. Page-level and object-level `/Metadata` streams
  other than the catalog's are kept and reported as uninspected.
- **X** — Trees that cannot be edited exactly (cycles, kids of unknown
  kinds, a touched element written inside another object); annotations
  other than links, markup and widgets meeting a rect, and widgets of
  signature fields, XFA forms or field trees that cannot be edited
  exactly; XFA forms with `remove_javascript`; signed and encrypted
  documents.

### 2.4 Original data left in objects and streams

- **G** — No object of the output is a version of something the
  redaction replaced or removed, whatever refers to it (section 4.2):
  an edited page's original content stream, an edited image's or
  form's original, a removed annotation's appearance and what it draws,
  a removed Type 3 glyph's procedure and the font's former
  `/ToUnicode`, a removed shading or pattern, a removed thumbnail —
  except where the output still draws the same bytes (then it is
  reported in `elsewhere`).
- **G** — The output is one revision, has no object or cross-reference
  stream from the input, and every object is reachable from its
  trailer.
- **N** — A re-encoded copy (the same picture under another filter, the
  same text in another encoding) is not recognised by a digest.

### 2.5 Copies in data kasane does not interpret

- **R** — Every stream the output carries is classified by the role it
  is reached in (`Redaction::uninspected`, `uninspected_data`):
  *searched* (strings and text kasane reads and searches), *interpreted*
  (a format it reads to draw: images, fonts, ICC profiles, functions,
  CMaps), *uninspected* (attachments, XMP metadata, XFA, private data,
  scripts, thumbnails, streams of no known role), *not carried*.
- **X** — With `refuse_uninspected`, an output carrying uninspected data
  or whose inventory stopped at a limit.
- **N** — Interpreted data is not searched (text in an image's pixels,
  glyph shapes in a font program), and whether an interpreted object is
  used at all is checked only for forms and images (an unused font,
  pattern or profile may hold anything). There is no byte search.

### 2.6 Differences between readers

- **G** — What kasane reads is read the way Poppler, MuPDF and pdf.js
  were measured to read it, where they agree.
- **X** — Every construction found to be read differently by them
  (section 2.1) is refused, not guessed.
- **N** — Readers not measured (Acrobat, browsers other than Firefox's
  pdf.js, printers) are not covered.

### 2.7 What is kept outside the rects

- **G** — Pages not edited draw from the same content-bearing streams
  (by digest) as in the input: an edit of a shared object does not
  change another page (section 4.2, non-interference).
- **G** — On an edited page, content outside the rects is kept: text
  and its positions, image samples outside the rects, paths and shading
  clear of the rects; elements of the structure tree not tied to
  removed content keep everything.
- **N** — A glyph, path or image pixel crossing a rect's edge is removed
  (or zeroed) whole, not split. Structure elements across a rect lose
  all their text (above).

## 3. Four kinds of data

Every byte of the output falls in one of four kinds, and the guarantees
above differ by kind:

| kind | what | what is established |
| --- | --- | --- |
| **understood and verified** | content streams kasane draws, the structure tree, annotations, strings in objects | removed content and its description are gone (2.1–2.4), verified on the output |
| **interpreted, secrets not verifiable** | image samples outside the rects, font programs, ICC profiles, functions, CMaps | read to draw; that they hold nothing about the removed content is *not* established (2.5) |
| **not interpretable** | attachments, XMP, XFA, private and unknown streams, scripts | reported as uninspected; refused under `refuse_uninspected` (2.5) |
| **not in the output** | what the rewrite drops: unreachable objects, input object streams, incremental history, `/Info`, catalog metadata | verified absent (2.4) |

"Verified" never means "proven free of secret information": it means a
named property was checked on the written bytes, by code that does not
trust the edit's own record where that is possible (section 4).

## 4. Verification design

### 4.1 Layers

1. **The edit's own record** — digests of what the session replaced or
   removed (originals of edited images and forms, original content
   streams, removed annotations' prints and appearance closures, pruned
   Type 3 procedures, replaced `/ToUnicode`, removed shadings,
   thumbnails), checked over every object of the output. These checks
   came first; each past leak of this kind was a kind of object the
   record did not list.
2. **Invariants of the input and the output alone**
   (`src/redact_verify.rs`), built on a use walk (`src/redact_usage.rs`)
   that both sides share and that does not consult the edit's record:
   - *use*: every content-bearing stream an edited page used in the input
     is, in the output, absent, or drawn again (in a content-bearing
     role, under the lower bound below);
   - *non-interference*: pages not edited draw from the same
     content-bearing streams;
   - *removed annotations*: an annotation an edited page lost (compared
     page to page, and held by no output page) is in no dictionary of
     the output;
   - *reachability*: every output object is reachable from the trailer.
3. **External checks** with tools that share no code with kasane:
   `scripts/verify-redact.py` (qpdf, pdftotext), the advisory
   regressions (`scripts/check-advisory-regressions.py`), the corpus
   evaluation (`scripts/eval-redact-corpus.py`: Poppler rendering and
   extraction inside and outside the rects), and Poppler, MuPDF and
   pdf.js in reviews.
4. **Generated tests**: `tests/redact_model.rs` and the `redact_model`
   fuzz target build documents in two variants that differ only in the
   secret and require byte-identical outputs or the same refusal.

A failed check is an error and no output.

### 4.2 Decisions

- **Use, not reference, decides what may stay.** A stream reachable
  from the output is not therefore safe: past leaks were all reachable
  copies (through a private catalog key, `/PieceInfo`, an attachment, a
  structure `/MCR /Stm`, an unused resource name). The *use* invariant
  asks instead whether a reader reads the stream to draw a page. The
  walk follows what drawing reads: `Do`, the font `Tf` or an ExtGState
  selects (and a Type 3 font's procedures for the glyphs shown), the
  patterns, soft mask groups, transfer functions and colour spaces in
  force *when something is painted* (a colour selected and never
  painted with is not used), shadings, annotation appearances,
  thumbnails.
- **Asymmetric bounds.** An undecidable point (a name that resolves to
  nothing, a stream that does not decode) counts, over the input, as
  "everything this page reaches is used", and over the output as
  "nothing more is used". Both errors lead to a refusal, never to an
  excused copy.
  Over the output, to excuse a copy, the walk counts only what a reader
  draws: an annotation shown (an integer `/F` without Hidden or NoView,
  no `/OC` or one on by default) — its normal appearance in the state
  `/AS` selects; for a button field that is neither read only nor
  locked and has an area, what it presents as it is used (a checkbox's
  or radio button's off and on states, a push button's rollover
  appearance and, with `/H /P`, its down one); page and form content
  outside `BDC /OC` sequences and XObjects with `/OC` whose group is
  off by default. A group is on only if the default configuration
  (intent `/View`) turns it on (`/OFF` after `/ON`), no automatic state
  names it and its `/Usage` turns nothing off; a membership dictionary
  counts as off; an appearance without `/Resources`
  resolving nothing (pdf.js). The other bounds resolve such an
  appearance in the page's resources (mutool, Poppler). Non-interference uses a third bound on both
  sides — everything a reader may draw (every appearance and state,
  Hidden annotations, optional content), undecidable points adding
  nothing — so an edit of hidden content of another page is not
  overlooked. Each page is walked within its own operation bound.
- **The graphics state is its current values.** What painting reads is
  held as slots — fill and stroke colour (space, pattern or `Default*`
  substitute), soft mask, transfer function, black generation,
  undercolour removal, halftone — each its latest value, with the
  resources it was selected in (a form inherits the state, and reads an
  inherited colour or mask where it was selected, not in its own
  resources). A form is walked once per resources and exact state: the
  memo key is the canonical bytes of each value and the identity of its
  resources, compared exactly (no hash decides that two states are the
  same), so equal states are not walked twice and different ones are
  never merged.
- **Digests for copies, identities for removed objects.** The rewrite
  renumbers objects and a copy can be written under any number: streams
  are compared by the SHA-256 of their stored and of their decoded
  bytes. A copy is excused only when the output draws the *same bytes*
  in a content-bearing role; empty and whitespace-only *content
  streams* carry nothing and are not compared (the same bytes as a
  palette, samples or a function are). Removed annotations and fields
  themselves are checked by identity on the edited document before it
  is written: their content may have been edited by a later pass (the
  JavaScript one), so a content print alone would miss them.
- **Content-bearing roles.** The use invariant applies to streams that
  picture or spell content (contents, forms, appearances, images and
  masks, Type 3 procedures, patterns, soft mask groups, shadings,
  functions, colour lookup tables, thumbnails, halftones). Font
  programs, a font's code data (`/ToUnicode`, CMaps, `/CIDSet`,
  `/CIDToGIDMap`) and ICC profiles are left out: a font that only the
  removed text used stays in the page's resources in ordinary
  documents, and refusing it would refuse them; what such a font
  reveals (its glyph repertoire) is stated in 2.5. A Type 3 font's
  replaced `/ToUnicode` is checked by the edit's record (layer 1).
- **Non-interference compares drawing.** A page not edited must draw
  (under the complete bound above) from the same content-bearing
  streams; a font's code data may change
  (a Type 3 font losing glyphs no page shows gets a new `/ToUnicode` on
  every page that uses it), and a thumbnail shared with an edited page
  is removed. Pages whose drawing is the same graph in both (contents,
  resources, annotations, thumbnail, compared with references replaced
  by what they refer to, a page by its index; only the back links of
  `/Annots` entries left out) are not walked again. This is one
  invariant among several: what no page draws is held to the use
  invariant and the removed-annotation checks, and the generated tests
  compare the whole output bytes.
- **Fail closed on cycles, depth and size.** Every walk is bounded by
  `Limits` (`max_audit_elements`, `max_diag_exec_ops`,
  `max_xobject_depth`, nesting caps); reaching a bound is an error, not
  a pass.
- **No byte search.** Searching the output for removed strings would
  miss compressed, re-encoded or split copies and would flag
  coincidental bytes; it is used only by the external checks and the
  tests, as one signal among others.

## 5. What would weaken the model

Changes that must come with tests showing they keep the model:

- excusing a stream by its role without checking it is used;
- adding a role to the content-bearing exemptions;
- counting a selected-but-unpainted resource as used over the output;
- accepting a construction measured to be read differently by readers;
- removing a layer-1 check because a layer-2 invariant covers it (the
  two are kept as defence in depth: they share no code path).
