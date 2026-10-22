# Experimental scoped region removal (text + image)

## Experimental scoped region removal (M20, `remove_region`, CLI `experimental-remove-region`)

**Not a redaction tool** — same guarantee boundary as `remove_text`
and `remove_image_region`. M20 performs a *single* integrated edit
pass over one page: removes character codes whose filled-ink bounds
hit a caller rect *and* blackens image samples whose painted
positions hit the same rects, then writes one new PDF.

What it does — API `remove_region(doc, page_index, rects, limits)`
returning `RegionRemoval { output, codes_removed,
invocations_edited, pixels_changed, sites, scope }`:

- One `DiagWalker::new` walk over the original content stream —
  every target is keyed by its *original* `op_index`. Text ops are
  rewritten by `remove_text::rewrite_ops_for` (same TJ/kern logic);
  image `Do`s are repointed to cloned image objects via `KasIm{n}`
  (same per-invocation cloning as M19). Both edits are applied to
  the *same* output ops list in one pass — a `Tj` that loses codes
  becomes a `TJ` at the same `op_index`, and a `Do` that hits a
  rect gets a new name at the same `op_index`. The page's
  `/Contents` is serialised once, and `rewrite` runs once.
- **M20.1** — the walker records the CTM at each `Image` `Do` call
  site into `TargetDetail::Image.ctm` (method A: no re-walk). This
  removes the O(images × ops) rescan that `ctm_at_op` used to
  perform — the budget `max_diag_exec_ops` now covers the *entire*
  CTM acquisition cost. `remove_image_region` benefits identically
  (it shares `DiagWalker`).
- **M21** — XObject names that were edited and are no longer
  referenced by any remaining `Do` are removed from the page's
  `/Resources /XObject` dict. The image object itself is left to
  the writer's reachability scan: if nothing else references it
  (another page, another `Do` via a different name, an inherited
  `/Resources`), it is not emitted. A name is removed only when
  the *edited* ops no longer use it — `new_ops` is scanned, not
  the original ops, so text expansion does not confuse the check.
  Names that were already unused before the edit are left alone.
- **M21.1** — verification is by *content*, not object number:
  re-parse the output and match image streams by their decoded
  sample bytes, not by original object number (the writer
  renumbers objects). A removed name does **not** imply the
  object was deleted — the writer's reachability scan decides
  that. An image can survive via an inherited `/Resources` even
  when the page's own `/XObject` no longer names it.
- **M22** — inherited `/Resources` materialisation: each page's
  effective `/Resources` (as resolved by the page-tree walk) is
  written explicitly into the page dict. A parent `/Pages` can
  then drop its `/Resources` — but only when *every* descendant
  page has been materialised. The parent keeps `/Type`, `/Kids`,
  `/Count`, `/MediaBox`, `/CropBox`, `/Rotate` — only `/Resources`
  is removed. The same `Edit`/overlay applies the materialisation,
  the text/image edits, and the `/Contents` rewrite in one pass.
- **M22.1** — the target page's dict is built once, not twice:
  `Contents` and the materialised `/Resources` coexist in one
  `page_dict`. A `/Pages` node *without* `/Resources` is still
  traversed — the check for whether to strip is applied after
  descendants are confirmed materialised, not before traversal.
- **M22.2** — the strip decision is *post-order*: a `/Pages`
  node's `all_materialised` is computed only after its children's
  results are known, and a child's failure propagates upward.
  Page refs are indexed by `HashMap`/`HashSet` so a leaf lookup
  is O(1), not O(n) per kid.
- **M22.3** — a node that finishes processing records its result
  in `node_results` whether it succeeded or failed. The
  `continue` guard is `next_kid_idx < kids.len() &&
  all_materialised` — a `false` result is *not* deferred, so the
  parent sees `false`, not `None`. This prevents infinite
  re-pushing of a failed child.
- **M22.4** — the traversal is extracted as
  `plan_resource_materialisation(doc, pages_with_resources) ->
  (pages_to_strip, node_results)` so the false-propagation is
  *asserted per node*, not merely inferred from the final strip
  list. The M22.3 tests previously accepted any `Err` from
  `remove_region`, which could fail *before* the traversal (a
  direct-kid target page has `object_ref() == None` → refused);
  they now call the planner directly. Termination is guaranteed by
  the parser's `walk_page_tree` — it rejects cycles
  (`CycleDetected`) and bounds the tree (`max_page_tree_depth`,
  `max_page_tree_nodes`) before this pass runs. The pass itself
  has no `visited` set: a node is re-pushed only while it is
  waiting on a child (`all_materialised` still `true`), and a
  recorded result is never recomputed — stack entries are bounded
  by nodes + edges. Verified end-to-end on a two-branch, two-page
  fixture (same name `/Im1` resolving to different images per
  branch): `scripts/verify-resources-materialize.sh` checks qpdf structure, both
  pages materialised, all `/Pages` stripped of `/Resources`,
  identical `pdftotext`, page-2 rendering byte-identical, page-1
  rendering differing only inside the edited region, and the
  branch-B image kept while the orphaned original is dropped.
- Text eligibility: `Tr 0` fill only, page-level (an out-of-rect
  `Tr 3`/`Tr 1` occurrence still refuses). `Tj`/`TJ`/`'`/`"` are
  rewritten with the same advance-preserving logic as `remove_text`.
- Image eligibility: 8-bit DeviceGray/DeviceRGB (post-resolution
  and `Default*` substitution), no `/Mask`/`SMask`/`SMaskInData`/
  `OC`/`ImageMask`/`Interpolate`, default `/Decode`, no filter or
  `/FlateDecode`, grid-aligned CTM (M28 — axis-aligned either sign,
  or axis-swapped; arbitrary rotation/shear/singular refused).
- **No Form `Do` in M20** — a `Do` to a `/Form` is refused even
  when it contains no text or images (M18's form handling is not
  inherited).
- Shared cumulative `max_edit_content_bytes` — the same budget
  bounds the cloned image bodies plus the new `/Contents` (not
  per-type). `max_image_bytes` is checked *before* each image
  decodes. `max_diag_exec_ops` bounds the walk (including the CTM
  recorded at each `Image` `Do` — M20.1). `max_operations`
  bounds the output op count. The final PDF is bounded by
  `max_output_bytes`.
- `pixels_changed` counts *pixels* (≥1 channel went non-0 → 0), not
  components — same definition as M19.1/M19.2.
- A rect that hits nothing produces an empty edit set — `Ok` with
  `codes_removed = 0`, `invocations_edited = 0`, `pixels_changed = 0`.
- Any failure — validation, unsupported construct, or limit — emits
  nothing; a partial rewrite is never returned as success.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, `/UserUnit` ≠ 1, any `Do` to a `/Form`, any
inline image, any `gs`/`sh`/Pattern/`ActualText`/OC, any
`blocker`/`uncertain` from the walk, non-`Tr 0` text, an image that
is not 8-bit DeviceGray/DeviceRGB (post-resolution), a mask/non-default
`/Decode`/non-Flate filter/non-grid-aligned CTM, or an `Image`
target at `form_path` depth ≥ 1.

A name removed from `/XObject` does **not** mean the object was
deleted — the writer's reachability scan decides that. If another
page, another `Do` under a different name, or an inherited
`/Resources` still references the object, it is emitted. The
`orphaned_names_removed` counter reports names removed from the
page's `/XObject`, not objects deleted.

Object numbers are *not* a reliable signal: the writer renumbers
objects, so a missing original number does not prove the object
is absent. Verification must re-parse the output and match image
streams by their decoded sample bytes (M21.1). `pdfimages` lists
only images that are actually `Do`-drawn on a page — an image
that survives via an inherited `/Resources` but is not drawn is
still emitted in the PDF but invisible to `pdfimages`.

An image *outside* every rect is never touched — its `hits` list is
empty so `to_edit_image` skips it entirely, and `validate_image` is
never reached. A JPEG outside the rect therefore leaves the call
`Ok` (the image is unedited); a JPEG *inside* the rect refuses the
whole call via `validate_image`. The same policy applies to text:
`Tr 3` outside the rect is a `blocker` (refused), `Tr 3` inside the
rect is a `hits`-non-empty `Target` (also refused).

Limitations — by design and explicitly *not* done:

- Form XObjects, inline images, JPEG, rotated/sheared images — all
  refused.
- The original image stream and `/Contents` stay in the file when
  another page or an unedited invocation shares them.
- Removed text can still exist elsewhere: `/Info`, XMP, structure
  tree, `ActualText`, embedded files, other pages, font internals.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-region <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`/`remove_text`/
`remove_image_region`; `--output` is required, input and output
must differ, and the output is written atomically — a refusal
leaves any existing output untouched.

External check: `qpdf --check` clean; `pdftotext` confirms removed
codes are absent and survivors keep positions; `pdfimages`
extraction confirms only selected pixels are zeroed and the
original image is preserved elsewhere in the file.
