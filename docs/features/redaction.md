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
| text shown with `Tr 0` in an embedded font (rotated, scaled, `Ts`/`Tz`/`Tc`/`Tw`, `TJ` kerning) | the codes whose glyph ink hits a rect are deleted — whole glyphs, so a glyph straddling a rect's edge goes entirely; advances are preserved so the rest of the line keeps its position. Content whose clip keeps it out of every rect is not a hit |
| text in a non-embedded font, with `unembedded_text_by_cell(true)` (CLI `--unembedded-text-by-cell`) | refused by default (a reader draws it with a font of its own; its ink extent is unknown). On request each code is taken by its *cell* — the advance box widened by 0.15 em each side, 0.3 em below the baseline to 1 em above — and removed when the cell meets a rect; the page's content is then wrapped in a clip (`q`, the media box minus the rects, `W* n` … `Q`) so nothing left on it paints inside a rect, whatever font draws it. Only fill text (`Tr 0`) whose every code's advance the document gives (a `/Widths` entry — not a reader's `/MissingWidth` —, a CID font's `/W` / `/DW`, or the Adobe widths of Symbol and ZapfDingbats; a Standard 14 font without `/Widths` is refused) |
| image XObjects, 8-bit DeviceGray/RGB, unfiltered / Flate / one DCT, axis-aligned | a copy with the covered samples set to 0 replaces the call; the original is not carried (next section) |
| painted paths wholly inside one rect | the path and its painting op are deleted |
| painted paths crossing a rect's edge | cut there (`paths_cut`): filled axis-aligned rectangles become their difference with the rects (exact; several only when they do not overlap); stroked straight segments are cut, in user space where the stroke is defined, at the centreline positions whose cross-section (and cap) would reach the rect — exact for butt caps; for square and round caps the cut end is pulled back by the cap's square, so nothing reaches inside but up to half a line width of the band next to the rect is lost (and a round cap's corners); a corner kept near a rect must clear it by its join (the exact miter, or the bevel/round extent). A dashed stroke's pieces each restart the pattern at the phase where they start on the original subpath (an odd array written out in full, the phase reduced into one period), so the dashes outside fall where they did (a closed dashed outline is not rejoined at its start); each piece is its own stroke, so a dashed stroke that is not opaque in the Normal blend mode (`/CA` < 1, another `/BM`) is refused when cut into pieces. Curves, degenerate dash patterns, other fill shapes and overlapping rectangles near a rect are refused |
| `/Link` annotations whose `/Rect` meets a rect | removed with their action or destination (`links_removed`); links outside every rect are kept. A removed link the structure tree refers to (`/StructParent`) is refused unless the tree is removed |
| other annotations clear of every rect | kept as they are (their appearance is drawn into `/Rect`; without one, a reader draws it from its geometry, which must lie inside `/Rect`, and the border width is added to `/Rect` for the test); their text is still searched for `elsewhere` |
| markup annotations meeting a rect, with `remove_annotations(true)` (CLI `--remove-annotations`) | refused by default. On request a Text, FreeText, Line, Square, Circle, Polygon, PolyLine, Highlight, Underline, Squiggly, StrikeOut, Caret, Stamp, Ink or Popup annotation whose `/Rect` meets a rect is removed **whole** — its appearance and text go even where they lie outside the rect (`annotations_removed`) — with its popup, the annotation a removed popup belongs to, and replies to it (`/IRT`), transitively. Its appearance streams and the forms and images they draw may not stay unless still drawn (checked by digest); its `/Contents`, `/T`, `/RC` (string or stream), `/Subj` are searched for in `elsewhere`. Anything else referring to a removed link or annotation — an action, another page's annotation or reply, a kept annotation's popup, the form, an outline — is refused (it would carry it into the output). Widgets, signatures, media, file attachments and other subtypes meeting a rect stay refused, as does one the structure tree refers to unless the tree is removed |
| shading patterns as fill or stroke colour | a pattern paints only inside what it fills, so the path or glyph is removed or cut as with any colour; tiling patterns (whose cell is content of its own) and shading patterns with their own `/ExtGState` (which may carry a soft mask) are refused |
| ExtGState (`gs`) | stroke parameters (`/LW`, `/LC`, `/LJ`, `/ML`, `/D`) are applied; colour and compositing parameters (alpha, blend mode, overprint, transfer, halftone, `/SMask /None` …) are passed through, since removal and cutting are by geometry. A soft mask, `/Font` and stroke adjustment (`/SA true`, device-dependent stroke edges) are refused |
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
(or whose data length is not fixed by their dictionary), `sh`,
ExtGState with a soft mask, `/Font` or `/SA true`, optional content and `/ActualText` marked content,
text in any mode but `Tr 0` (invisible OCR text, stroked, clipping),
non-embedded fonts (glyph extent unknown), Type 3 fonts (a CharProc
may paint anywhere), vertical writing, images with masks, soft masks,
`/Alternates` or OPI, `/Decode`, other colour spaces or filter chains, rotated or
sheared images, `/UserUnit` ≠ 1, a path crossing a rect's edge that
cannot be cut exactly (curves, degenerate dash patterns, non-rectangular fills,
overlapping filled rectangles, a corner whose join reaches a rect), a
path that also clips, content meeting a rect under a clip that is not an
axis-aligned rectangle (whether it shows there is not decided), an
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

Neither is a guarantee of absence: text inside a font program, in an
image's pixels or encoded differently is not found. Reports carry no
document text in `Debug` output.

## Known limits of exactness

- Image samples are selected by mapping the rect into image space in
  floating point; a rect edge within about 1e-16 of a sample boundary
  may leave that boundary sample (review P2, accepted: the overlap is
  below any rendering resolution).
- Glyphs are removed whole: a glyph straddling a rect's edge goes
  entirely, including its part outside.

## Not covered

Annotations other than links meeting a rect, forms nested more than
16 deep, tiling patterns, inline images meeting a rect, soft masks and fonts
set by ExtGState, shadings, curves crossing a rect, masked, CMYK or indexed images, invisible OCR text.
Each is refused, not approximated.
