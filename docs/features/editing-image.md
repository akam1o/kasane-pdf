# Experimental image-region removal

## Experimental image-region removal (`remove_image`, CLI `experimental-remove-image-region`)

**Not a redaction tool** — same guarantee boundary as `remove_text`.
`remove_image_region` *replaces* the image samples whose painted positions land inside
caller-supplied rectangles with opaque black (all-zero samples) in a
*new* image object; the original stream is never mutated, and only
the `Do` call sites that hit a rect are repointed to the clone. It
does not remove other copies of the same image data (metadata,
annotations, structure tree, other pages, thumbnails), does not
guarantee irreversibility, and does not reason about what the image
depicts.

What it does — API `remove_image_region(doc, page_index, rects,
limits)` returning `ImageRemoval { output, invocations_edited,
pixels_changed, sites, scope }`:

- Rects are un-rotated, un-cropped user space — same convention as
  `preflight-redaction`/`inspect`/`remove_text`.
- Reuses the diagnosis `DiagWalker` (`new_diag_only` — no text snapshots)
  so the graphics state and hit tests are the diagnosis's; any
  `blocker`/`uncertain` entry or a mid-stream walk failure is a
  typed refusal.
- **Exact pixel union, not a bounding box**: each rect
  produces its own `(x0,y0,x1,y1)` pixel range via inverse-CTM
  mapping. The union is computed by splitting into horizontal bands
  at every distinct y-boundary, then merging x-intervals within each
  band — a band is a maximal y-strip whose x-interval set is
  constant. Corner-touching rects and L-shaped overlaps do not fill
  the pixels between them; overlapping ranges are merged so no pixel
  is visited twice. Outward rounding (`floor`/`ceil`) and the y-flip
  (image rows top-down) are unchanged. The intermediate `Vec` is
  bounded by `rects.len()` (`max_diag_rects`), never by image height.
- **Grid-aligned image CTMs**: the `Do` CTM is classified
  before inversion — exact components only, no epsilon. Accepted:
  (A) *axis-aligned* `b==0, c==0` with non-zero `a`,`d` of either
  sign — covers horizontal/vertical reflection and 180° rotation;
  (B) *axis-swapped* `a==0, d==0` with non-zero `b`,`c` — covers
  90°/270° rotations and reflected rotations, with any non-uniform
  scale and finite translation. Each rect's two opposite corners are
  inverse-mapped into the image's unit square, min/max normalised
  (reflections swap the order), intersected with `[0,1]²`, then
  floor/ceil-mapped to pixel ranges as before. Arbitrary-angle
  rotation, shear, singular, or non-finite CTM → `Error::Unsupported`
  / `Error::Structure`; the input rect stays in un-rotated user
  space and the page's `/Rotate` is *not* applied to it.
- **Pixel count, not component count**: `pixels_changed`
  counts *pixels* — a pixel is one unit when at least one of its
  components changed **from non-0 to 0**. RGB `[r,g,b]` → `[0,0,0]`
  counts as 1; `[0,128,0]` → `[0,0,0]` also counts as 1; an
  already-all-0 pixel counts as 0. Gray follows the same definition.
- **Colour-space resolution + Default* substitution**:
  `/ColorSpace` names are resolved through the page's effective
  `/Resources /ColorSpace` dict — `/CS1` → `/DeviceGray` works.
  A *device* name (`/DeviceGray`, `/DeviceRGB`) is then checked
  against the `Default*` substitution table in the same resource
  dict: `/DefaultRGB` → `/DeviceRGB` is accepted, but `/DefaultRGB`
  → `/CalRGB` (or any non-device space) is refused. Indirect refs
  are dereferenced, then the same rules apply. A missing
  `/ColorSpace` resource entry is `Error::Structure`, not a guess.
  Substitution does not recurse — a `Default*` that is itself a
  device name is ignored.
- **Pre-decode budget check**: `max_image_bytes` and
  `max_edit_content_bytes` are both checked *before* decoding —
  an image whose expected `Width × Height × ncomps` size exceeds
  the remaining cumulative budget is refused without allocating
  its buffer. A non-intersecting image is skipped before decode
  entirely (no budget consumed). `charge_generated` runs before
  the decoded buffer is held, so the clone is never built when
  the budget is insufficient. The decoded `Vec` is then moved
  (not cloned) into the edited body — no extra copy.
- Per-invocation cloning: fresh `/XObject` names (`KasIm{n}`,
  deterministic, collision-checked), only the hit `Do` is repointed,
  the page's effective `/Resources` is materialised, shared objects
  are never mutated in place.
- The final PDF is still bounded by `max_output_bytes` at serialize
  time — `BoundedWriter` checks the remaining budget before every
  `push`/`extend_from_slice`.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, `/UserUnit` ≠ 1, any `Do` to a `/Form`, any
inline image, any `gs`/`sh`/Pattern/`ActualText`/OC, any
`blocker`/`uncertain` from the walk, an image that is not 8-bit
DeviceGray/DeviceRGB (post-resolution), a `/Mask`/`SMask`/
`SMaskInData`/`OC`/`ImageMask`/`Interpolate`, a non-default
`/Decode`, a `/Filter` other than none or `/FlateDecode`, or a
non-grid-aligned/singular/non-finite CTM (arbitrary-angle rotation
and shear are refused; grid-aligned forms are edited — see the grid-aligned CTM notes
above).

Limitations — by design and explicitly *not* done:

- Nested XObjects (a `Do` inside a form), inline images, and all
  non-device colour spaces beyond the resolved name are out of scope
  (refused).
- The original image stream stays in the file when another page or
  an unedited invocation shares it — the changed pixels exist only
  in the clone.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-image-region <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`; `--output` is
required, input and output must differ, and the output is written
atomically — a refusal leaves any existing output untouched.

External check: `qpdf --check` clean; `pdfimages` extraction
confirms disjoint rects leave middle pixels unchanged and only the
selected pixels' components are zeroed; `pixels_changed` counts
pixels (not components) and matches the CLI report.
