# Redaction (`redact`, CLI `redact`)

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
| text shown with `Tr 0`–`3` in an embedded font (fill, stroke, both, invisible) (rotated, scaled, `Ts`/`Tz`/`Tc`/`Tw`, `TJ` kerning) | the codes whose glyph ink hits a rect are deleted — whole glyphs, so a glyph straddling a rect's edge goes entirely; advances are preserved so the rest of the line keeps its position. A clip hides ink, not data: text, images and paths are hits wherever they meet a rect, clipped or not (the one exception: a shading `sh`, which fills the clip, is accepted under a rectangular clip meeting no rect, its resource kept as it is). Text whose text matrix, CTM or their product stretches one axis over 16 times the other is refused (readers disagree where it is drawn) |
| text in a Type 3 font whose CharProcs are paths | each CharProc is evaluated: the bounds of what it fills and strokes (control points for curves; strokes, with the line state the glyph is shown with unless the CharProc sets its own, grown by half the width × the larger of the miter limit and √2 × the CharProc CTM's largest stretch; clips in it decide nothing; text rendering modes 1–2 add nothing, the CharProc deciding what is painted; its ops count against `max_diag_exec_ops`), mapped by `/FontMatrix` and the text rendering matrix and united with the code's advance cell (0.2 em below the baseline to 1 em above, where extraction places it) — not `/FontBBox`, which a CharProc may paint beyond. Codes meeting a rect are removed whole like any glyph; the same glyph elsewhere is kept. A CharProc with anything but path construction and painting, `q`/`Q`/`cm`, line state, device colours, `d0`/`d1`, flatness, rendering intent and unpropertied marked content — images (inline too), XObjects, text, shadings, `gs`, patterns or named colour spaces, marked content with properties, a hairline (`0 w`) stroke — is refused, as is a `/FontMatrix` × text matrix stretching one axis over 16 times the other or singular. The CharProcs stay in the font (see below) |
| image XObjects, 8-bit DeviceGray/RGB/CMYK, Indexed (not as a JPEG; `/Decode` absent or its default `[0 255]`) or ICCBased (N 1, 3, 4, `/Alternate` absent or a device space; not a four-component JPEG), the colour space direct, named or one reference away (a reference to a reference is refused: readers resolve such chains differently), unfiltered / Flate / one DCT, axis-aligned | a copy with the covered samples set to 0 replaces the call; the original is not carried (next section) |
| painted paths wholly inside one rect | the path and its painting op are deleted |
| painted paths crossing a rect's edge | cut there (`paths_cut`): filled axis-aligned rectangles become their difference with the rects (exact; several only when they do not overlap); stroked straight segments are cut, in user space where the stroke is defined, at the centreline positions whose cross-section (and cap) would reach the rect — exact for butt caps; for square and round caps the cut end is pulled back by the cap's square: a square cap then ends the band exactly at the rect, and a round-capped piece whose both ends are cuts is stroked with square caps for the same result, while a round-capped piece keeping one of the path's own ends keeps its round caps and loses the two corners of the cut cap's square (≈ 0.21 × the squared half width each). A crossing that is not perpendicular also loses the sliver of band beyond the perpendicular cut; a corner kept near a rect must clear it by its join (the exact miter, or the bevel/round extent). A dashed stroke's pieces each restart the pattern at the phase where they start on the original subpath (an odd array written out in full, the phase reduced into one period), so the dashes outside fall where they did (a closed dashed outline is not rejoined at its start); each piece is its own stroke, so a dashed stroke that is not opaque in the Normal blend mode (`/CA` < 1, another `/BM`) is refused when cut into pieces. Stroked curves are cut by subdivision: a sub-curve (an exact de Casteljau piece of the original) is kept when its control points' bounds, grown by √2 × half the width under the CTM's largest stretch, clear every rect, and a part still near a rect after 14 halvings is removed — so up to that reach plus the last piece's length of band next to the rect is lost, and nothing reaches inside. Dashed curves cut at a rect, curved fills, degenerate dash patterns, other fill shapes and overlapping rectangles near a rect are refused |
| `/Link` annotations whose `/Rect` meets a rect | removed with their action or destination (`links_removed`); links outside every rect are kept. A removed link the structure tree refers to (`/StructParent`) is refused unless the tree is removed |
| other annotations clear of every rect | kept as they are (their appearance is drawn into `/Rect`; without one, a reader draws it from its geometry, which must lie inside `/Rect`, and the border width is added to `/Rect` for the test); their text is still searched for `elsewhere` |
| markup annotations meeting a rect, with `remove_annotations(true)` (CLI `--remove-annotations`) | refused by default. On request a Text, FreeText, Line, Square, Circle, Polygon, PolyLine, Highlight, Underline, Squiggly, StrikeOut, Caret, Stamp, Ink or Popup annotation whose `/Rect` meets a rect is removed **whole** — its appearance and text go even where they lie outside the rect (`annotations_removed`) — with its popup, the annotation a removed popup belongs to, and replies to it (`/IRT`), transitively. Its appearance streams and the forms and images they draw may not stay unless still drawn (checked by digest); its `/Contents`, `/T`, `/RC` (string or stream), `/Subj` are searched for in `elsewhere`. A removed link or annotation may not stay in the output by any route — an action, another page's annotation or reply, a kept annotation's popup, the form, an outline, the structure tree: verification compares every dictionary written with each removed one (by content, references aside) and fails on a match. Widgets, signatures, media, file attachments and other subtypes meeting a rect stay refused, as does one the structure tree refers to unless the tree is removed |
| shading patterns as fill or stroke colour | a pattern paints only inside what it fills, so the path or glyph is removed or cut as with any colour; tiling patterns (whose cell is content of its own) and shading patterns with their own `/ExtGState` (which may carry a soft mask) are refused |
| text in a non-embedded Times, Helvetica or Courier font (or a metric-compatible alias such as Arial), with `embed_substitute_fonts(true)` (CLI `--embed-substitute-fonts`) | refused by default. On request each such font is first replaced, document-wide, by a non-symbolic TrueType font embedding the bundled Liberation face, with every code's glyph name (`/Differences`) and advance (`/Widths`) written out, so every reader draws exactly those glyphs at the same positions; the text is then redacted by ink like any embedded font's (`fonts_embedded`). The output shows Liberation's shapes wherever those fonts are used and carries the programs (≈ 0.3–0.4 MB per face). Symbol, ZapfDingbats, other non-embedded and CID fonts stay refused |
| optional content (`BDC /OC`, a form's or image's `/OC`) | taken as drawn whether a viewer shows it or not: what meets a rect is removed — hidden content too — and the rest kept |
| shadings (`sh`) | a shading fills the clip: kept under a clip that is exactly a rectangle meeting no rect, refused otherwise |
| ExtGState (`gs`) | stroke parameters (`/LW`, `/LC`, `/LJ`, `/ML`, `/D`) are applied; colour and compositing parameters (alpha, blend mode, overprint, transfer, halftone, `/SMask /None` …) are passed through, since removal and cutting are by geometry. A soft mask and `/Font` are refused. Stroke adjustment (`/SA true`) moves edges to device pixels by an amount no user-space bound covers (and Splash snaps a rectangle fill's edges too), so while it is on any stroke (paths, stroked text, a Type 3 CharProc's), and any fill meeting a rect without lying wholly inside one (which would be cut), is refused; text fills, images, fills removed whole and fills clear of every rect are accepted — their kept ink is the input's, unchanged |
| Form XObjects, nested up to 16 deep (transparency groups included) | their text, paths and images inside the rects are removed, cut or zeroed in copies along the invocation chain — each copy drawn by that one invocation only (`forms_edited`), its `/Resources` naming only what it draws. Other invocations and pages keep the originals; an original still drawn is reported in `elsewhere` (`Removed::Form` / `Removed::Image`); otherwise it is not carried. A form cycle is refused |
| document `/Info`, catalog XMP `/Metadata` | always removed |
| structure tree | refused when it carries `/Alt`, `/ActualText` or `/E`; with `remove_structure_tree(true)` the tree and `/MarkInfo` are removed |

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
(or whose data length is not fixed by their dictionary), `sh` that may paint inside a rect,
ExtGState with a soft mask or `/Font`, strokes and cut fills under
`/SA true`, `/ActualText` marked content,
text in a clipping mode (`Tr` 4–7),
non-embedded fonts (glyph extent unknown; see `embed_substitute_fonts`), Type 3 glyphs whose CharProc is not
paths only (see the table), vertical writing, images with masks, soft masks,
`/Alternates` or OPI, `/Decode`, other colour spaces or filter chains, rotated or
sheared images, a path crossing a rect's edge that
cannot be cut exactly (curves, degenerate dash patterns, non-rectangular fills,
overlapping filled rectangles, a corner whose join reaches a rect), a
path that also clips, an
operator other than path construction inside a path object (§8.2).
Anywhere: an
undecidable resource use, a structure tree with alternate text (by
default); with `refuse_copies_elsewhere`, any copy found elsewhere. Rewrite refusals (signed or encrypted documents, …) apply.

## Positional, not semantic

`redact` removes what is *drawn inside the rects*. The same
information elsewhere is not removed:

- `elsewhere` lists where removed text (runs of four or more
  non-whitespace characters) is still shown — another page's text —
  or stored — a non-stream object's string: an outline title, an
  annotation's `/Contents`, a field value. `elsewhere_unsearched_pages`
  lists pages whose text could not be extracted for that search.
- `residual` is the residual-information audit of the output
  (attachments, annotations, form values, actions, JavaScript,
  embedded fonts, …; see [`audit.md`](audit.md)).

Neither is a guarantee of absence: text inside a font program (a
removed Type 3 glyph's CharProc too: glyph definitions stay in the
font, as an embedded font program keeps its glyphs — which glyphs a
font has, not where or in what order they were shown), in an
image's pixels or encoded differently is not found. Reports carry no
document text in `Debug` output.

## Known limits of exactness

- Image samples are selected by mapping the rect into image space in
  floating point; a rect edge within about 1e-16 of a sample boundary
  may leave that boundary sample (review P2, accepted: the overlap is
  below any rendering resolution).
- Glyphs are removed whole: a glyph straddling a rect's edge goes
  entirely, including its part outside.

## Not covered (known limitations of 0.6)

Each of these is refused with a typed error, not approximated:

- **Text in a non-embedded font.** A reader draws it with a font of its
  own choosing, so where its glyphs reach is not known; keeping any of
  it next to a rect could keep data drawn inside the rect. Accepted
  for the Latin Standard 14 fonts after `embed_substitute_fonts` embeds
  their substitutes (a clip does not help: it hides ink, not the code).
  Type 3 glyphs whose CharProcs are not paths only, vertical fonts,
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
- **Paint**: tiling patterns, shading patterns with their own
  ExtGState, shadings (`sh`) that may paint inside a rect, soft masks,
  `/Font` set by ExtGState; under `/SA true`, strokes and fills cut at
  a rect.
- **Annotations**: other than links meeting a rect unless
  `remove_annotations` removes them (markup subtypes only); widgets,
  signatures, media, file attachments and 3D meeting a rect; NoZoom,
  NoRotate and `/FixedPrint` annotations and Text annotations not
  removed; appearance-less annotations whose geometry leaves `/Rect`.
- **Text substitutes**: `/ActualText` (any tag), `/Alt` and `/E` on
  marked content; a structure tree carrying alternate text unless it is
  removed.
- **Documents**: signed or encrypted; forms nested more than 16 deep or
  cyclic.
