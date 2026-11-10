# Redaction (`redact`, CLI `redact`)

The safety model — what is guaranteed, reported, not guaranteed or
refused, per surface, and why — is
[`../redaction-safety-model.md`](../redaction-safety-model.md).

`redact(&Document, &[PageSpec], &RedactOptions) -> Result<Redaction,
Error>` removes what is drawn inside page rectangles and returns a new
single-revision PDF **only after checking the written bytes**. It is
built on the region editor (`remove_regions`, a content editor that
makes no such promise) and adds what a distributable result needs.

Covering content with a black box is not redaction and `redact` never
does it: text codes are deleted from the content stream, image samples
are overwritten with black in a new image object, paths are deleted.

## Workflow

1. Open the document (`Document::from_bytes` / `open`).
2. Optionally preview with `diagnose_redaction` (read-only: what each
   rect hits, what blocks exact processing, which objects are shared).
3. `redact(&doc, &[(page_index, rects)], &RedactOptions::new())` —
   0-based pages, rects `(llx, lly, urx, ury)` in the page's default
   user space (un-rotated, before the CropBox), several pages and
   rects per call.
4. On `Ok`, save `output`. It has already been parsed again and
   verified (below); `Err` means nothing was produced.
5. Review `elsewhere` and `residual` (below) — the redaction is
   positional.

```sh
kasane-pdf redact in.pdf --page 1 --rect 72 600 300 640 \
  --page 3 --rect 72 100 500 140 --output out.pdf
```

## What is removed

| content inside a rect | handling |
|---|---|
| text shown with `Tr 0`–`3` in an embedded font (fill, stroke, both, invisible) (rotated, scaled, `Ts`/`Tz`/`Tc`/`Tw`, `TJ` kerning) | the codes whose glyph ink hits a rect are deleted — whole glyphs, so a glyph straddling a rect's edge goes entirely; advances are preserved so the rest of the line keeps its position, but not the removed codes' own positioning: between two shows still drawing something, the text positioning (`Td`, `TD`, `T*`, `Tm`, `TJ` numbers, shows left drawing nothing) is folded into one move to the next kept glyph, so no offset, kern or width of an individual removed glyph or word stays — only where the next kept glyph starts (visible in any rendering) and, when the leading is inherited and unknown, the number of `T*` line moves. A clip hides ink, not data: text, images and paths are hits wherever they meet a rect, clipped or not (the one exception: a shading `sh`, which fills the clip, is kept under a rectangular clip meeting no rect, its resource as it is). Text whose text matrix, CTM or their product stretches one axis over 16 times the other is refused (readers disagree where it is drawn) |
| text in a Type 3 font whose CharProcs are paths and images | each CharProc is evaluated: the bounds of what it fills and strokes (control points for curves; strokes, with the line state the glyph is shown with unless the CharProc sets its own, grown by half the width × the larger of the miter limit and √2 × the CharProc CTM's largest stretch; clips in it decide nothing; text rendering modes 1–2 add nothing, the CharProc deciding what is painted; its ops count against `max_diag_exec_ops`), mapped by `/FontMatrix` and the text rendering matrix and united with the code's advance cell (0.2 em below the baseline to 1 em above, where extraction places it) — not `/FontBBox`, which a CharProc may paint beyond. Codes meeting a rect are removed whole like any glyph; the same glyph elsewhere is kept. A CharProc with anything but path construction and painting, `q`/`Q`/`cm`, line state, device colours, `d0`/`d1`, flatness, rendering intent, unpropertied marked content and images (inline, or an image XObject from the font's resources — an image paints its unit square under the CTM; its samples are never edited since the glyph is removed whole, so masks, colour spaces and filters do not matter) — form XObjects, text, shadings, `gs`, patterns or named colour spaces, marked content with properties — is refused (a hairline stroke in a CharProc is taken as its centreline, as on a page), as is a `/FontMatrix` × text matrix stretching one axis over 16 times the other or singular. A glyph the output no longer shows loses its definition (see below) |
| image XObjects, 1, 2, 4 or 8-bit DeviceGray/RGB/CMYK, Indexed (not as a JPEG; `/Decode` absent or its default `[0 255]`), ICCBased (N 1, 3, 4, `/Alternate` absent or a device space; not a four-component JPEG), Separation or DeviceN (1–32 colorants, a device or ICC alternate, a function tint transform: a zeroed sample is tint 0 in every colorant, one colour whatever was there), the colour space direct, named or one reference away (a reference to a reference is refused: readers resolve such chains differently), unfiltered or through a chain of general filters (Flate, LZW, ASCIIHex, ASCII85, RunLength) ending in one of them, in DCT (8-bit), or in CCITTFax or JBIG2 (1-bit grey; JBIG2 without `/JBIG2Globals`, a symbol dictionary other images may share), axis-aligned | a copy with the covered samples set to 0 (packed at the image's own depth) replaces the call, written unfiltered; the original is not carried (next section) |
| an image's soft mask (`/SMask`: an 8-bit DeviceGray image, its filters as for images, no `/Matte`, `/Decode` or masks of its own) | edited with the image: a copy of the mask with its samples inside the rects (mapped through the image's unit square at the mask's own resolution) made opaque — even where the colour samples were already black, since the mask alone can hold the shape — and the image copy refers to it. The original mask is checked by digest like an edited original: gone, unless the image is still drawn elsewhere, where it is drawn with it (reported as `Removed::Image`) |
| painted paths wholly inside one rect | the path and its painting op are deleted |
| painted paths crossing a rect's edge | cut there (`paths_cut`): filled axis-aligned rectangles become their difference with the rects (exact; several only when they do not overlap); stroked straight segments are cut, in user space where the stroke is defined, at the centreline positions whose cross-section (and cap) would reach the rect — exact for butt caps; for square and round caps the cut end is pulled back by the cap's square: a square cap then ends the band exactly at the rect, and a round-capped piece whose both ends are cuts is stroked with square caps for the same result, while a round-capped piece keeping one of the path's own ends keeps its round caps and loses the two corners of the cut cap's square (≈ 0.21 × the squared half width each). A crossing that is not perpendicular also loses the sliver of band beyond the perpendicular cut; a corner kept near a rect must clear it by its join (the exact miter, or the bevel/round extent). A dashed stroke's pieces each restart the pattern at the phase where they start on the original subpath (an odd array written out in full, the phase reduced into one period), so the dashes outside fall where they did (a closed dashed outline is not rejoined at its start); each piece is its own stroke, so a dashed stroke that is not opaque in the Normal blend mode (`/CA` < 1, another `/BM`) is refused when cut into pieces. Stroked curves are cut by subdivision: a sub-curve (an exact de Casteljau piece of the original) is kept when its control points' bounds, grown by √2 × half the width under the CTM's largest stretch, clear every rect, and a part still near a rect after 14 halvings is removed — so up to that reach plus the last piece's length of band next to the rect is lost, and nothing reaches inside. Dashed curves cut at a rect, curved fills, degenerate dash patterns, other fill shapes and overlapping rectangles near a rect are refused |
| `/Link` annotations whose `/Rect` meets a rect | removed with their action or destination (`links_removed`); links outside every rect are kept. A removed link the structure tree refers to (`/StructParent`, an `/OBJR`) leaves the tree (see the structure tree row) |
| other annotations clear of every rect | kept as they are. Their appearance is drawn into `/Rect`; without one, a reader draws the annotation from its geometry (`/L`, `/QuadPoints`, `/InkList`, `/Vertices`, `/CL`), inside `/Rect` or not, so `/Rect` and the geometry's bounds are each tested, grown by the border width and by the decorations a reader adds: line endings (`/LE`: Poppler and MuPDF reach up to 9.5 × the border width, about 9.3 at the least; 15 × the width, at least 15, is taken), leader lines (`/LL`, `/LLE`, `/LLO`) and, for text markup (Highlight, Underline, StrikeOut, Squiggly), half the height of its tallest quadrilateral (a highlight's rounded ends reach 0.19 × the height in Poppler and MuPDF). A caption (`/Cap`) or a cloudy border (`/BE /S /C`) on such an annotation is drawn to no known extent: refused. Their text is still searched for `elsewhere` |
| widgets meeting a rect, with `remove_annotations(true)` (0.9) | removed whole like a markup annotation; its form field leaves the field tree (`/Fields` or its parent's `/Kids`, and `/CO`) when no widget of the field is left — the value (`/V`, `/DV`, `/RV`) and description (`/TU`, `/MK` captions) with it (`fields_removed`). A field keeping a widget (another page, outside the rects) keeps its value: it is still shown, and is searched for in `elsewhere`, as are the removed fields' values |
| markup annotations meeting a rect, with `remove_annotations(true)` (CLI `--remove-annotations`) | refused by default. On request a Text, FreeText, Line, Square, Circle, Polygon, PolyLine, Highlight, Underline, Squiggly, StrikeOut, Caret, Stamp, Ink or Popup annotation whose `/Rect` meets a rect is removed **whole** — its appearance and text go even where they lie outside the rect (`annotations_removed`) — with its popup, the annotation a removed popup belongs to, and replies to it (`/IRT`), transitively. Its appearance streams and the forms and images they draw may not stay unless still drawn (checked by digest); its `/Contents`, `/T`, `/RC` (string or stream), `/Subj` are searched for in `elsewhere`. A removed link or annotation may not stay in the output by any route — an action, another page's annotation or reply, a kept annotation's popup, the form, an outline, the structure tree: verification compares every dictionary written with each removed one (by content, references aside) and fails on a match. Widgets, signatures, media, file attachments and other subtypes meeting a rect stay refused. One the structure tree refers to leaves the tree (see the structure tree row) |
| shading patterns as fill or stroke colour | a pattern paints only inside what it fills, so the path or glyph is removed or cut as with any colour; shading patterns with their own `/ExtGState` (which may carry a soft mask) are refused |
| tiling patterns as fill or stroke colour (page content) | decided where the pattern paints: a path it paints clear of every rect is kept; one wholly inside a rect is removed, and the pattern leaves the page's `/Resources /Pattern` with the `scn` that selected it, with everything it reaches; verification checks by content digest that it is not in the output. A pattern that also paints kept content (paths and glyphs clear of the rects, image masks and forms drawn while it is selected; images with their own colours do not use it) is refused when one of its uses is removed — the cell the kept use shows is the cell the removed one did — and so is one sharing any object it reaches with anything else. A path it paints crossing a rect is refused (the cell is content a cut would only hide: clipping it leaves its data), as are text painted with one meeting a rect, a path in a form painted with one, and a pattern reached from anywhere else (another page, a form, shared resources) |
| text in a non-embedded Times, Helvetica or Courier font (or a metric-compatible alias such as Arial), with `embed_substitute_fonts(true)` (CLI `--embed-substitute-fonts`) | refused by default. On request each such font is first replaced, document-wide, by a non-symbolic TrueType font embedding the bundled Liberation face, with every code's glyph name (`/Differences`) and advance (`/Widths`) written out, so every reader draws exactly those glyphs at the same positions; the text is then redacted by ink like any embedded font's (`fonts_embedded`). The output shows Liberation's shapes wherever those fonts are used and carries the programs (≈ 0.3–0.4 MB per face). Symbol, ZapfDingbats, other non-embedded and CID fonts stay refused |
| optional content (`BDC /OC`, a form's or image's `/OC`) | taken as drawn whether a viewer shows it or not: what meets a rect is removed — hidden content too — and the rest kept |
| shadings (`sh`) | a shading fills the clip: kept under a clip that is exactly a rectangle meeting no rect. In page content, one whose extent — the clip's bounds, narrowed by its `/BBox` under the CTM — lies wholly inside one rect is removed whole (`shadings_removed`), and its resource leaves the page's `/Resources /Shading` with everything it reaches (functions, colour spaces); verification checks by content digest that no removed shading is in the output. Refused: a shading crossing a rect (its function and geometry are data a clip would only hide, and cannot be separated into an inside and an outside part), one also painted outside the rects (the kept use keeps the function over the part painted inside), one bounded by nothing, one in a form, and one reached — or reaching an object that is reached — from anywhere else (another page, another name, a pattern, shared resources, any other holder) |
| ExtGState (`gs`) | stroke parameters (`/LW`, `/LC`, `/LJ`, `/ML`, `/D`) are applied; colour and compositing parameters (alpha, blend mode, overprint, transfer, halftone, `/SMask /None` …) are passed through, since removal and cutting are by geometry. A soft mask and `/Font` are refused. Stroke adjustment (`/SA true`) changes nothing in user space: it moves a stroke's (and, in Splash, a rectangle fill's) edges to the device pixel grid, by under a device pixel. Under it, strokes, fills and stroked text are redacted as their user-space geometry — removed inside a rect, cut at its edge, kept clear of it — like the hairline row below; what the adjustment moves is kept ink near an edge (see Known limits of exactness) |
| hairlines (`0 w`) | a hairline is drawn one device pixel wide, whatever the resolution, so no user-space width bounds it; its geometry is its zero-width centreline, and that is what is redacted: removed inside a rect, cut at its edge (the cut lies on the edge), kept clear of it — on a page, in forms, as stroked text and in Type 3 CharProcs. No point of a removed or cut hairline stays inside a rect; the kept part's own pixel can colour the pixel row at a rect's edge (see Known limits of exactness). `preflight-redaction` (the diagnosis) still reports a hairline's extent as uncertain |
| Form XObjects, nested up to 16 deep (transparency groups included) | their text, paths and images inside the rects are removed, cut or zeroed in copies along the invocation chain — each copy drawn by that one invocation only (`forms_edited`), its `/Resources` naming only what it draws. Other invocations and pages keep the originals; an original still drawn is reported in `elsewhere` (`Removed::Form` / `Removed::Image`); otherwise it is not carried. A form cycle is refused |
| document `/Info`, catalog XMP `/Metadata` | always removed |
| marked-content replacement text (`/ActualText` on any tag, `/Alt`, `/E`) written in the content (`BDC` with a dictionary) | where anything in the sequence is edited (a code removed, an image zeroed, a path removed or cut, a form edited — nested sequences included), the three keys are dropped from its properties, the other keys (`/MCID` …) kept: the text may describe what the rects remove, and the kept glyphs of the sequence then extract as themselves. The dropped text is searched for in `elsewhere` like removed text. An untouched sequence keeps its text. Named properties and points stay refused |
| structure tree (§14.7) | kept and edited (below, "Structure tree"): every element whose content the edit touches — and its ancestors — loses its text (`/Alt`, `/ActualText`, `/E`, `/T`, `/Phoneme`), its associated files (`/AF`) and the strings of its attribute objects (`/A`, its classes' `/ClassMap` entries), whole (`structure_texts_removed`); references to removed annotations and edited originals leave `/K` (`structure_items_removed`). Untouched elements keep everything. With `remove_structure_tree(true)` the tree and `/MarkInfo` are removed instead |
| page thumbnails (`/Thumb`) | removed from every edited page (`thumbnails_removed`): a thumbnail pictures the page as it was. Other pages' thumbnails are kept and listed in `uninspected` |
| JavaScript, with `remove_javascript(true)` (CLI `--remove-javascript`) | refused by default when the catalog runs scripts (`/Names /JavaScript`, a JavaScript `/OpenAction`). On request every JavaScript action (`/S /JavaScript` with its `/JS`) is removed (`javascript_removed`): the name tree, and each dictionary entry or array element that is one — `/OpenAction`, `/AA` triggers, an annotation's or field's `/A`, a step of an action chain — directly or by reference; the actions after it in its chain (`/Next`) take its place, and other actions (links, URIs) stay. A script may hold any of the document's text and cannot be redacted, so it goes whole: forms no longer calculate, validate or format. Refused: an XFA form (its scripts are not edited), anything a removed script uses (its source stream …) that something kept still refers to, a single-action entry whose script is followed by several actions, and a catalog `/AA`. Verification fails when a `/JS` entry or a script tree is left |

## What a success states

Three separate statements; none implies the next:

1. **The rects** — verified: the same rects find no text, sample,
   path or link left to remove.
2. **The edited originals** — verified: no live object equals an edited
   image's or form's original (stored or decoded digest), except an
   object the output still draws (another page, or the same page
   outside the rects, directly or through forms); those displays are
   kept and listed in `elsewhere` as `Removed::Image` / `Removed::Form`. The
   same picture re-encoded differently is not detected by a digest.
3. **The rest of the document** — reported, not guaranteed:
   `elsewhere` (removed text found again; originals still drawn) and
   `residual` (the output's audit). `refuse_copies_elsewhere(true)`
   (CLI `--refuse-copies-elsewhere`) refuses when anything is found or a
   page could not be searched — narrower, still not a proof of erasure.

## No route back to the original

- On every page, an `/XObject` name of an edited image's original that
  the page does not draw is removed — otherwise an inherited
  `/Resources` keeps the original reachable from pages that never
  showed it (`original_names_removed`).
- A page that still draws the original outside the rects keeps it:
  the rects are redacted on the edited page's copy, the other display
  is reported (`Removed::Image` in `elsewhere`), or refused with
  `refuse_copies_elsewhere`.
- When a form, Type 3 font or tiling pattern without `/Resources` would
  resolve names in the page's resources, the use is undecidable:
  refused.

## Verification (on the written bytes, before returning)

- The output parses and has exactly one revision (a full rewrite: no
  incremental history, no superseded objects).
- Running the same rects again finds no character code, nonzero image
  sample, path or link left to remove, and no path left to cut.
- No live object's stream — stored bytes or decoded — equals an edited
  image's or form's original (SHA-256 over every xref entry,
  object-stream members included), except an object the output still
  invokes — by a `Do` on a page, in an annotation's appearance
  (`/AP` `/N`, `/R`, `/D`), or
  transitively in a form those draw (names resolved as a reader does,
  inherited resources included). Whether that invocation is visible
  (a clip or `/BBox` may hide it) is not decided: it is reported in
  `elsewhere` either way, and `refuse_copies_elsewhere` refuses it. Any
  other copy — one only named in some resources, or reachable
  otherwise — fails.
- No stream of the output holds, by digest, these objects the
  redaction removed whole, whatever refers to them: a removed link's or
  annotation's appearance streams and the forms, images and masks they
  draw (unless still drawn; an appearance drawing through patterns,
  shadings, soft mask groups or Type 3 fonts, or naming resources it
  does not have, is refused instead), a removed annotation's dictionary
  (also as a stream's dictionary), a removed Type 3 glyph's procedure
  (unless still a Type 3 font's glyph), the font's former `/ToUnicode`,
  the masks of images only removed glyphs drew, an edited page's
  thumbnail (unless another page's own, a different object; one shared
  with another page leaves that page too).
- No stream of the output holds an edited page's original content
  (stored or decoded digest), except one that a page still draws as its
  `/Contents` (a content stream the edited page shared with another
  page, whose text `elsewhere` then reports). Anything else referring to
  it — a catalog or page entry, application data, an attachment — fails;
  a structure element's `/MCR /Stm` to it leaves the tree.
- The output carries no object stream or cross-reference stream (the
  rewrite writes neither; one carried because something referred to it
  would hold the input's objects as they were).
- No object of the output is a removed shading or tiling pattern
  (compared by content, references replaced by what they refer to, so
  renumbering does not hide one).

- Independently of what the edit recorded, the input and the output
  are compared (`docs/redaction-safety-model.md`, section 4): every
  content-bearing stream an edited page used (contents, forms,
  appearances, images and masks, Type 3 procedures of glyphs shown,
  patterns and soft mask groups painted with, shadings, functions,
  colour lookup tables, thumbnails) is used again by the output or
  absent from it — by digest, whatever object number or reference keeps
  a copy; pages not edited draw from the same streams; an annotation an
  edited page lost is in no dictionary of the output; every object is
  reachable from the trailer. A name that resolves to nothing counts,
  over the input, as using everything the page reaches.

A failed check is an error (`Unsupported { feature: "redaction output
failed verification", .. }`), never a returned PDF.

These checks use kasane's own parser. `scripts/verify-redact.py` (run
by the PR workflow) checks the CLI with tools that share no code with
it: each synthetic case puts a secret inside a rect in a different way
(text, metadata, an inherited image, a path, a link, structure
alternate text, invisible text, a form, a text annotation); a
successful output must pass `qpdf --check`, contain neither the secret
text nor the image samples after `qpdf --qdf --object-streams=disable
--decode-level=all`, and give `pdftotext` the public text but not the
secret; the rest must be refused with no file written.

## Refused (typed error, no output)

On an edited page: annotations other than links meeting a rect,
NoZoom, NoRotate or `/FixedPrint` annotations and Text annotations
(whose icon behaves as NoZoom and NoRotate) not removed, forms nested more
than 16 deep or cyclic, inline images meeting a rect
(or whose data length is not fixed by their dictionary), `sh` crossing a rect, unbounded, in a
form or reached from elsewhere, tiling patterns painting across a rect, in a form, under text
meeting a rect or reached from elsewhere,
ExtGState with a soft mask or `/Font`, literal strings holding an
unescaped carriage return in any object of the output, in an edited
page's content or the forms it draws, or in a CMap (readers do not agree
on the bytes they hold), text in a font whose embedded CMap writes codes
as literal strings (Poppler rejects them, other readers do not), a
Type 0 code its CMap maps by no `cidchar` / `cidrange` (a notdef
mapping, no mapping, or bytes in no codespace range: readers draw
different glyphs at different widths) or whose value another code
length maps to another CID (readers key mappings by value or by bytes),
text in a CMap with constructions readers read differently (bf mappings
in an encoding CMap, definitions after `endcmap` or a second
`begincmap`, codes an embedded CMap passes through an Identity base (`usecmap` Identity) rather than listing them, a
`/WMode` other than 0 or 1, defined twice or disagreeing between the
CMap's dictionary and program, odd-length hex codes, stray tokens in a
block, codespace ranges of different lengths sharing leading bytes),
removing an annotation whose appearance draws through patterns,
shadings, soft mask groups or Type 3 fonts or names resources it does
not have,
an appearance-less annotation with a caption or a cloudy border, `/ActualText`, `/Alt` or `/E` in named marked-content
properties or on a marked-content point,
text in a clipping mode (`Tr` 4–7),
non-embedded fonts (glyph extent unknown; see `embed_substitute_fonts`), Type 3 glyphs whose CharProc is not
paths and images (see the table), vertical writing, images with stencil or colour-key masks,
soft masks other than those in the table, `/Alternates` or OPI, `/Decode`, other colour spaces or filter chains, rotated or
sheared images, a path crossing a rect's edge that
cannot be cut exactly (curves, degenerate dash patterns, non-rectangular fills,
overlapping filled rectangles, a corner whose join reaches a rect), a
path that also clips, an
operator other than path construction inside a path object (§8.2).
Anywhere: an
undecidable resource use; a structure tree that cannot be edited
exactly (below); with `refuse_copies_elsewhere`, any copy found
elsewhere; with `refuse_uninspected`, any data carried uninspected
(below). Rewrite refusals (signed or encrypted documents, …) apply.

## Structure tree

The tree is kept, so the output stays tagged. The edit records what it
touched: the marked-content sequences (`MCID`) holding an edited op —
a code removed, an image zeroed, a path removed or cut, a form edited —
on the page or in a form; the images and forms whose originals were
edited; the annotations removed. Elements are matched by content items:
an `MCID` (an integer kid on the element's `/Pg`, or an `/MCR` with its
own `/Pg` and, in a form, `/Stm`), an `/OBJR`. Every element the tree
refers to is read — from the root's `/K`, every `/ParentTree` entry and
`/IDTree` — and the page's and forms' `/StructParents` entries of
`/ParentTree` are used as well; ancestors are found through `/K` and
`/P`. An element is touched when one of its items is, or a descendant
is.

What goes, from each touched element and its ancestors: `/Alt`,
`/ActualText`, `/E`, `/T`, `/Phoneme` and `/AF` (whole: which part of a
text describes the removed content is not known, so none of it is kept —
an element across a rect loses all of its text, accessibility of the
kept part included), and every string of its attribute objects (`/A`;
for its classes `/C`, the `/ClassMap` entries — shared, so for every
element using that class): a table's `/Summary` and any other text an
attribute carries — and a user-properties attribute (`/O /UserProperties`)
loses its whole property list; other attributes (layout, numbers,
names) stay, as do an element's `/ID`, `/Lang` and keys no structure
names. The
removed texts are searched for in `elsewhere`. `/OBJR` items to removed
annotations or edited originals, and `/MCR` items into an edited form's
original, leave `/K` (they would keep those objects reachable); a
removed annotation's `/ParentTree` entry goes; form and image copies
carry no `/StructParent(s)` (their content is no longer the content the
tree names). Elements are not split or duplicated.

Matching is conservative: an `MCID` whose page is not known is touched
when that number is touched on any page or form; a page `MCID` is also
touched when a form drawn on the page touches that number (a tree may
name a form's MCIDs as the page's, and readers resolve them so, whether
or not the form has `/StructParents`); a sequence whose `MCID`
cannot be read (named `/Properties` that do not resolve) touches every
`MCID` of its stream. Edited content outside every marked-content
sequence relates to no element: the tree keeps its text (a copy of
removed text there is reported in `elsewhere`).

Refused: a cycle among elements or in `/ParentTree` / `/IDTree`, a kid
that is not an element, an `MCID`, an `/MCR`, an `/OBJR` or null, an
`/MCR` without an integer `/MCID`, an `/OBJR` whose `/Obj` is not a
reference, a touched element written directly inside another object,
and a tree beyond `max_audit_elements`. Dangling references are null.
Verification re-reads the output's tree: no element may carry a text or
attribute string that was removed, unless an untouched element of the
input carries the same text (then it is a copy, reported in
`elsewhere`).

## Uninspected data

`Redaction::uninspected` (and `Diagnosis::data`, and
`uninspected_data(&Document, &Limits)` for any document) classifies
every stream by the role it is reached in from the trailer:

| status | what |
| --- | --- |
| `Searched` | its text is extracted and searched for `elsewhere`: page content, forms drawn by pages and annotation appearances |
| `Interpreted` | a known format in a known role, not searched: images drawn, font programs and CMaps, functions, shading and pattern streams, ICC profiles, Type 3 glyph procedures, the XObjects of patterns, Type 3 fonts and soft masks. The role is what is classified: whether such data is used (a pattern, font, function or profile named but never drawn) is not checked, except for forms and images |
| `Uninspected` | carried as it is, never read: attachments (`/EF`, `/RF`), XMP metadata (other than the catalog's, which is removed), thumbnails, JavaScript source, XFA, rich text, multimedia and 3D, `/PieceInfo`, PostScript and OPI, halftones, template pages, images and forms nothing draws, streams under a key no modelled structure names, and any entry of the catalog, the page tree or a page that none names (unless it holds only a number, boolean or null) |
| `NotCarried` | nothing refers to it (input only: `redact` writes what is reachable) |

A stream reached in several roles takes the best one, except a role
whose bytes a reader hands out as they are (an attachment, metadata …):
then it is uninspected whatever else it is. Back edges (`/Parent`, `/P`)
are followed too. Object and cross-reference streams are counted as
the file's structure. Items list object, role, path from the trailer,
stored length and filters — never content; the walk is bounded by
`max_audit_elements` (`complete` is false when it stops).

`refuse_uninspected(true)` (CLI `--refuse-uninspected`) refuses an
output with any uninspected item or an incomplete inventory. This
narrows a success to "every stream carried is reached in a role whose
format `redact` reads or interprets". It is **not** a proof that no copy
of the removed content remains: interpreted data is not searched, nor
checked to be used (an unused pattern, font, function or profile may
hold anything), text in an image's
pixels or a font program), non-stream objects' strings are searched
only as text, and nothing searches bytes — compression, encryption,
encodings and application formats make a byte search no evidence of
absence. The default does not refuse: on the Corpus (117 runs) the
strict mode refused 4 of the 91 successes, in 2 files — XMP metadata on
a structure element, and application data (`/PieceInfo`).

## Positional, not semantic

`redact` removes what is *drawn inside the rects*. The same
information elsewhere is not removed:

- `elsewhere` lists where removed text (runs of four or more
  non-whitespace characters) is still shown — a page's text, including
  optional content that is off and annotation appearances — or stored —
  a non-stream object's string: an outline title, an annotation's
  `/Contents`, a field value. CR, LF and CRLF compare as one line end.
  `elsewhere_unsearched_pages` lists pages whose text could not be
  extracted for that search (a page whose annotation appearances
  cannot be read has its own text searched and is listed).
- Streams the redaction does not interpret — attachments, private or
  unknown streams, anything an object refers to under a key no
  structure gives a meaning — are carried as they are and are not
  searched: a copy of the removed content the input keeps in one stays.
  `uninspected` lists them (see "Uninspected data"), and
  `refuse_uninspected` refuses them; `residual` lists what it can
  classify.
- `residual` is the residual-information audit of the output
  (attachments, annotations, form values, actions, JavaScript,
  embedded fonts, …; see [`audit.md`](audit.md)).

- **Type 3 glyph definitions.** A CharProc can hold any drawing (a
  picture, a signature, a word), so it is not treated as a mere glyph
  shape. After the edit, a Type 3 glyph the input showed and the output
  no longer shows anywhere — page content, the forms it draws,
  annotation appearances; the same font found by its content — loses
  its definition: its `/CharProcs` entry, the images only it drew
  (dropped from the font's `/Resources`, so they and their soft masks
  are no longer carried; one the output still draws elsewhere is
  reported as `Removed::Image`), the codes naming it in the
  encoding (rewritten as `/Differences` naming only kept glyphs), their
  `/Widths` (set to 0) and their `/ToUnicode` entries (the CMap is
  rewritten). Glyphs the input never showed go too. A glyph the output
  still shows elsewhere keeps its definition: that drawing is on the
  page there (single glyphs are not reported in `elsewhere`, like
  letters of any font). A font that loses glyphs and that something
  else uses — a tiling pattern, a soft mask, a form field's `/DR`,
  another Type 3 font's resources, an ExtGState `/Font` — is refused, as
  are a direct (unreferenced) Type 3 font dictionary that loses glyphs
  and a removed glyph drawing an image from resources the font
  inherits;
  verification checks that no glyph the output no longer shows is
  still defined.

Neither report is a guarantee of absence: text inside an embedded font
program (which glyphs a font has, not where or in what order they were
shown), in an image's pixels or encoded differently is not found.
Reports carry no document text in `Debug` output.

## Known limits of exactness

- Image samples are selected by mapping the rect into image space in
  floating point; a rect edge within about 1e-16 of a sample boundary
  may leave that boundary sample (review P2, accepted: the overlap is
  below any rendering resolution).
- Glyphs are removed whole: a glyph straddling a rect's edge goes
  entirely, including its part outside.
- Geometry is exact in user space, not in device pixels. A reader
  snaps what it draws to its pixel grid and blends at edges, so kept
  content near a rect's edge can colour pixels inside the rect: up to
  half a device pixel for images drawn without interpolation and thin
  fills (measured with Poppler, MuPDF and pdf.js at 36–300 dpi), and
  for an image with `/Interpolate true` up to about one sample's width
  (the kept boundary sample blended towards its zeroed neighbour).
  Hairlines and strokes under stroke adjustment (`/SA true`) are drawn
  by device-pixel rules too: a hairline one pixel wide, an adjusted
  stroke's edges moved to the grid. Their user-space geometry is
  redacted exactly (removed, or cut on the rect's edge), so this is
  kept ink, measured within 1.5 device pixels of the edge: none inside
  that (Poppler, MuPDF and pdf.js at 72–1200 dpi, rotated, sheared and
  non-integer CTMs, dashes, round caps and joins).
  Nothing removed takes part: two inputs differing only in the data
  inside the rects give byte-identical outputs (images with and without
  interpolation, scaled, flipped and rotated by 90°, under transparency
  and blending; paths, text and Type 3 glyphs), so these pixels are the
  kept content's own.
- The layout is kept, so the gap a removed run leaves before the next
  kept glyph on its line shows the run's total width (in the file and
  in any rendering). Nothing finer stays (see the text row above).

## Not covered (known limitations)

Each of these is refused with a typed error, not approximated:

- **Text in a non-embedded font.** A reader draws it with a font of its
  own choosing, so where its glyphs reach is not known; keeping any of
  it next to a rect could keep data drawn inside the rect. Accepted
  for the Latin Standard 14 fonts after `embed_substitute_fonts` embeds
  their substitutes (a clip does not help: it hides ink, not the code).
  Type 3 glyphs whose CharProcs draw more than paths and images, vertical fonts,
  text in a clipping mode (`Tr` 4–7) and
  text under a text matrix stretching one axis over 16 times the other
  are refused too. Invisible text (`Tr 3`, OCR layers) is taken by a
  generous box around its advance (0.15 em each side, 0.5 em below the
  baseline to 1.2 em above) united with its outline; stroked text by
  its outline grown by the stroke's reach.
- **Geometry**: curved fills, dashed curves cut at a rect,
  non-rectangular or overlapping fills crossing a rect, a corner whose join reaches a rect, a translucent or blended
  dashed stroke cut into pieces. A round-capped piece that keeps one of the path's own ends loses its
  cut cap's corners, and an oblique crossing the sliver of band beyond
  the perpendicular cut (accepted, documented).
- **Images**: masked, other bit depths or filter chains, four-component
  JPEGs,
  rotated or sheared, with `/Alternates` or OPI, and inline images
  meeting a rect. (`/Interpolate` and `/OC` on an image are accepted:
  they do not change which samples are drawn where.)
- **Paint**: shadings (`sh`) and tiling patterns crossing a rect,
  inside forms or shared; shading patterns with their own ExtGState;
  soft masks; `/Font` set by ExtGState.
- **Strings**: a document with a literal string holding an unescaped
  carriage return (in an object of the output, an edited page's content
  or the forms it draws, or a CMap; the content of pages not edited is
  not checked, nothing there is placed by the redaction); text in a font whose
  embedded CMap writes codes as literal strings.
- **CMap codes**: a shown Type 0 code that its CMap (embedded or
  predefined, with what it inherits by `usecmap`) maps by no `cidchar`
  or `cidrange` (a code an embedded CMap only passes through an
  Identity base counts as unmapped: Poppler gives it CID 0, MuPDF and
  pdf.js the code; the predefined `/Identity-H` and `/Identity-V`
  encodings are not affected). Fonts that declare notdef
  mappings or leave codes unmapped redact as long as the codes they
  show are mapped. Readers measured on synthetic Type 0 fonts (Poppler
  26.10, MuPDF 1.28.6, pdf.js 6.5.438):

  | shown code | kasane (§9.7.6.3) | Poppler | MuPDF | pdf.js |
  | --- | --- | --- | --- | --- |
  | in a notdef char or range (any CID, also 0) | the notdef CID (a range maps every code to the same CID) | CID 0 (notdef ignored) | nothing drawn, no advance | CID = code (notdef ignored) |
  | in the codespace, mapped by nothing | CID 0 | CID 0 | nothing drawn, no advance | CID = code |
  | in no codespace range | CID 0, shortest codespace length | CID 0; a 2-byte prefix match takes 2 bytes (`<81 20>` against `<8140> <9FFC>`), otherwise 1 | nothing drawn, no advance, 1 byte | CID 0, 1 byte |

  The glyph, its width (`/W` is indexed by CID) and so where every
  later glyph falls differ, so a code placed outside a rect here can be
  drawn inside it by another reader. The three readers ignore the notdef
  mappings of predefined CMaps as well (measured with `90ms-RKSJ-H`,
  whose `<00> <1f>` notdef range maps to CID 231).
- **Scripts**: a catalog running JavaScript, unless `remove_javascript`
  removes every script; a catalog `/AA`.
- **Annotations**: other than links meeting a rect unless
  `remove_annotations` removes them (markup subtypes and widgets);
  a widget of a signature field, of an XFA form, of a field tree that
  cannot be edited exactly (a cycle, a direct field, a `/Parent` that is
  not the field above it, a field `/Fields` does not reach), or written
  directly in `/Annots`; signatures, media, file attachments and 3D
  meeting a rect; NoZoom,
  NoRotate and `/FixedPrint` annotations and Text annotations not
  removed; appearance-less annotations with a caption or a cloudy
  border.
- **Text substitutes**: `/ActualText` (any tag), `/Alt` and `/E` in
  named marked-content properties (`/P0 BDC`: the text would stay in
  the resources) or on a marked-content point (`DP`); a structure tree
  that cannot be edited exactly (see "Structure tree").
- **Documents**: signed or encrypted; forms nested more than 16 deep or
  cyclic.
