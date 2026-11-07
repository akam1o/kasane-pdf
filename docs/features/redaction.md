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
| text shown with `Tr 0` in an embedded font (rotated, scaled, `Ts`/`Tz`/`Tc`/`Tw`, `TJ` kerning) | the codes whose glyph ink hits a rect are deleted; advances are preserved so the rest of the line keeps its position |
| image XObjects, 8-bit DeviceGray/RGB, unfiltered / Flate / one DCT, axis-aligned | a copy with the covered samples set to 0 replaces the call; the original is not carried (next section) |
| painted paths wholly inside one rect | the path and its painting op are deleted |
| `/Link` annotations whose `/Rect` meets a rect | removed with their action or destination (`links_removed`); links outside every rect are kept. A removed link the structure tree refers to (`/StructParent`) is refused unless the tree is removed |
| document `/Info`, catalog XMP `/Metadata` | always removed |
| structure tree | refused when it carries `/Alt`, `/ActualText` or `/E`; with `remove_structure_tree(true)` the tree and `/MarkInfo` are removed |

## No route back to the original

- On every page, an `/XObject` name of an edited image's original that
  the page does not draw is removed — otherwise an inherited
  `/Resources` keeps the original reachable from pages that never
  showed it (`original_names_removed`).
- A page that still draws the original outside the rects is refused:
  its unredacted samples would stay in the output.
- When a form, Type 3 font or tiling pattern without `/Resources` would
  resolve names in the page's resources, the use is undecidable:
  refused.

## Verification (on the written bytes, before returning)

- The output parses and has exactly one revision (a full rewrite: no
  incremental history, no superseded objects).
- Running the same rects again finds no character code, nonzero image
  sample or path left to remove.
- No live object's stream — stored bytes or decoded — equals an edited
  image's original stored bytes or decoded samples (SHA-256 over every
  xref entry, object-stream members included).

A failed check is an error (`Unsupported { feature: "redaction output
failed verification", .. }`), never a returned PDF.

## Refused (typed error, no output)

On an edited page: annotations other than links, Form XObjects, inline images, `sh`,
ExtGState (`gs`), optional content and `/ActualText` marked content,
text in any mode but `Tr 0` (invisible OCR text, stroked, clipping),
non-embedded fonts (glyph extent unknown), images with masks, soft
masks, `/Decode`, other colour spaces or filter chains, rotated or
sheared images, `/UserUnit` ≠ 1, a path crossing a rect's edge, a path
that also clips. Anywhere: an edited image drawn unredacted, an
undecidable resource use, a structure tree with alternate text (by
default). Rewrite refusals (signed or encrypted documents, …) apply.

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

## Not covered

Annotations other than links and form fields on an edited page, forms
and patterns, inline images, transparency, shadings, partial paths
(table rules crossing a rect), masked, CMYK or indexed images,
invisible OCR text. Each is refused, not approximated.
