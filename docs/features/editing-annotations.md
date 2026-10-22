# `/Square` annotation editing

## Annotation edit — `/Square` (M9, ISO 32000-1 §12.5.6.9, §8.4, §12.5.2)

`Document::edit()` returns an [`Edit`] session that accumulates
`/Subtype /Square` annotations as **synthetic objects** in a reserved
indirect-number space (`u32::MAX − 1 000 000` upward). The source
bytes are never modified; `Edit::rewrite()` feeds the session's
overlay plus the live document through the normal rewrite path.

| Feature | Status | Notes |
|---|---|---|
| `/Subtype /Square` on a page | ✅ | `add_square(page, rect, rgb, width)` |
| Rect meaning | ✅ | **the annotation's outer bounding box** — the stroke's outer edge, not the path centreline (§12.5.5 + "Border Styles": border drawn *inside* the Rect) |
| Rect space | ✅ | un-rotated PDF user space (+x right, +y up); CropBox/Rotate not applied — same space M6.1 reports |
| Stroke colour | ✅ | `/C` `[r g b]`, each channel validated finite `0..=1` — out-of-range is an error, never clamped |
| Line width | ✅ | `/BS << /S /S /W w >>`; finite and `> 0`, and `< min(rect w,h)` so the stroke can't invert |
| Fill | — | interior is unfilled (`S` stroke only); no `/IC` emitted |
| Appearance stream | ✅ | generated `/AP << /N >>` Form XObject — `BBox [0 0 w h]`, `Matrix [1 0 0 1 llx lly]`, body `q RG w re S Q` stroking the *inner* rect `[w/2 w/2 w-w/2 h-w/2]`; transformed BBox coincides with Rect so the annotation mapping (`AA = Matrix × A`) is a pure translate and the stroke width is never rescaled or clipped |
| Flags | ✅ | `/F 4` (Print) — visible on screen and on paper |
| Page link | ✅ | `/P` set to the page's indirect ref |
| Multiple annots | ✅ | per page and across pages; each page's `/Annots` array is materialised once |
| `/Annots` absent / direct / indirect | ✅ | a fresh array object is always created — a shared `/Annots` is never mutated in place, so annotating one page can't alter another |
| Existing annots / links | ✅ | preserved verbatim in the new array |
| `/UserUnit` ≠ 1 | ❌ | `Error::Unsupported` — the rect is in default user units and a scaled page would misplace it |
| Refusals | ✅ | all M8 rewrite refusals still apply (encryption, signatures, version, named dests, malformed structure, limits) |
| Synthetic object numbers | ✅ | allocated downward from `u32::MAX` skipping any number *reserved* by the input xref at **any generation or entry type** (in-use, free, compressed), any already-allocated synthetic, or any overlay key — a synthetic ref can never alias a real, reserved, or prior synthetic number; exhaustion and `max_edit_objects` are typed errors |
| Input-ref integrity | ✅ | a reference found in an *input* body that lands on a synthetic number is treated as a dangling input ref and resolved via the document only — never silently resolved to a synthetic body; overlay (edit) refs may resolve to synthetics |
| Out-of-range page / non-finite rect / bad colour / bad width | ❌ | `Error::Structure`, no partial state |
| Reversed rect (llx>urx or lly>ury) | ❌ | refused — not silently normalized |

Not in scope: fill/IC, dashes, transparency, other annotation
subtypes, editing or deleting existing annotations, page edits,
flattening, redaction, a general-purpose edit API, or kasane's own
renderer painting the annotation (it still refuses pages with
`/Annots` — that is M6 behaviour, unchanged).

External check: `qpdf --check` clean; `pdftoppm -hide-annotations`
output is byte-identical to the un-annotated input (only the
annotation differs); the change is confined to the Rect bounds.
