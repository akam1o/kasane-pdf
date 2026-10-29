# Plan — a distribution-safe redaction API (memo, not scheduled)

Recorded after an external integration evaluation of
`experimental-remove-region` (2026-10). Nothing here is implemented;
the existing `remove_*` editors keep their content-editing contract
(`docs/security.md` § What the engine is *not*). The decision whether
this becomes a 0.3.0 public API is open.

## What the evaluation found

Ten synthetic inputs, region removal through the CLI at `ec65732`:
2 passed the evaluator's checks, 2 left original image data in the
output, 6 were refused with `unsupported feature` and no output. No
crash. Both residual cases reproduce and are documented behaviour of
the content editor, not contract violations:

| finding | cause | where documented |
|---|---|---|
| an image removed on the edited page survives on an unedited page | inherited `/Resources` are materialised onto every page; unused names are pruned only on the edited page, so the original stays reachable | `features/editing-region.md` (names unused before the edit are left alone) |
| an image shared with another page survives whole, although the other page only shows a clipped, public part of it | the unedited page legitimately references the original | `features/editing-multipage.md` (shared original survives) |
| pages with link annotations are refused | `/Annots` on the edited page are out of scope | `features/editing-region.md` |
| ASCII85 + Flate image filter chains are refused | only none / Flate / single DCT are edited | same |
| non-embedded fonts are refused | glyph extents cannot be determined exactly | same — and must stay refused |

## Requirements for a distribution-safe output

From `redaction-design.md` and the evaluation's acceptance criteria:

1. **No recoverable original data.** After a full rewrite, no stream
   reachable in the output may contain the redacted pixels or text.
   Unused references to an edited image are removed on every page; an
   image still *displayed* elsewhere is copied per page and the copy
   edited, so retained display is preserved. When usage cannot be
   determined (forms, patterns, masks, unknown resources), the call
   fails with a typed error and produces no output.
2. **Public content kept.** Searchable text, images and the visible
   parts of shared or clipped images outside the regions are kept;
   deleting a whole image is not a pass.
3. **Links.** `/Link` annotations intersecting a region are removed;
   those outside are kept, URI and internal destinations alike (an
   internal destination must still point at the same page). Other
   annotation types, appearance streams and form fields stay refused
   until handled explicitly.
4. **Explicit surface policy.** Shared forms, masks, annotation
   appearances, form values, `/ActualText` / `/Alt`, Info / XMP and
   attachments: each removed, handled, or refused, documented per
   surface. No partial output on an unsupported structure or a limit.
5. **Position vs information.** The API removes what is inside the
   regions; a secret displayed intentionally elsewhere is reported to
   the caller, not silently changed.
6. **External verification.** Acceptance is checked by decoding every
   object of the output (all xref entries and object-stream members),
   not by visual comparison; the evaluation's ten inputs plus rotation,
   CropBox, nested forms, transparency masks, invisible OCR text and
   internal links after page reordering form the minimum corpus.

## Shape (proposal)

A separate entry point, e.g. `redact(&Document, &[RedactRegion],
&RedactOptions) -> Result<RedactedPdf, Error>`, whose success means
requirements 1–5 hold for the returned bytes, plus a report of what
was removed and which other displays of the same data exist. Errors
reuse `Error::Unsupported` / `LimitExceeded`; no `Warning` may stand in
for a removal that did not happen.

## Effort (estimate)

| item | effort |
|---|---:|
| resource-usage analysis across pages, per-page copies of edited shared images, refusal when undecidable | 5–8 d |
| `/Link` handling (remove intersecting, keep the rest, verify internal destinations) | 3–5 d |
| image filter chains (ASCII85 / ASCIIHex in front of Flate / DCT) | 2 d |
| per-surface policy and tests for the remaining surfaces | 5+ d |
| C-compatible API and Apple builds (separate phase) | — |
