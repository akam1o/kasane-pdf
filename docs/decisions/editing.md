# Decisions — experimental scoped editing (M17–M28)

## M17 — experimental scoped text removal

`src/remove_text.rs` implements `remove_text` + the
`experimental-remove-text` CLI — **not** a redaction tool, and the
name deliberately avoids `redact`/`sanitize`. It is an experiment in
character-level content editing: delete the shown character codes
whose filled-ink bounds hit a caller rect, rewrite the affected
strings so survivors keep their positions, emit a new
single-revision PDF.

Design choices:

- **Selection = M16's `DiagWalker`** — the same per-code targets the
  diagnosis produces (`filled` glyph outline bounds, edge-touching
  counts). One code = one removal unit even when it expands to
  several Unicode scalars. Any `blocker`/`uncertain` entry, or a
  mid-stream walk failure, is a typed refusal — the walker has
  already reasoned about `q`/`Q`, `cm`/`Tm`/`Td`/`TD`/`T*`, font
  width/space tables, `Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts`, and every
  construct the scope refuses. Walk errors keep their typed kind
  (`LimitExceeded`, `Structure`, `Parse`) — never flattened to
  `Unsupported`.
- **Op-time state snapshots (M17.1, extended M18)** — the walker
  records the text state each show op ran under (`Font` (Rc-cloned),
  `Tfs`, `Th`, `Tc`, `Tw`) into `show_states[(form_path, op_index)]`
  at the point `show_text` consumes it — after `"`'s `Tw`/`Tc`
  update, after `'`/`"`'s line move. The `form_path` is part of the
  key (`[]` at page level, `[frame]` inside a called form), so a
  form's inner ops under different invocations get distinct
  snapshots and never collide with page-level op indices. One
  snapshot per show op, bounded by `max_show_states` (M18.1). The
  map only records when the caller asks for it —
  `diagnose_redaction` walks with `record_show_states = false` and
  never fills it, so a diagnosis pays zero snapshot memory.
  Selection (`Target`) and rewrite read the *same* state, so a
  later `Tf`/`Tc`/`Tz`/`q`/`Q`/`BT` change can never rewrite an
  earlier show under the stream-end values — the M17.0 bug where
  `walker.gs.text` at end-of-stream was used for every op.
- **Page-level eligibility (M17.1, extended M18)** — a page is
  eligible only when *every* text occurrence on it is `Tr 0` fill,
  checked against all `walker.targets`, not just the hit ones —
  page-level and form-level alike. `Tr 4–7` and `w = 0` stroked
  text arrive via `blocker`/`uncertain` and refuse the same way; a
  `Tr` *set* but never used to show text is harmless.
- **One level of `/Form` (M18)** — a page-level `Do` targeting a
  `/Form` is edited *per invocation*: the call-site `form_path`
  (resource name + call-site `op_index`, not just the object ref)
  keys the removal scope, so two `Do`s of the same form or two
  names for the same object clone independently. The clone is a new
  stream object with the rewritten ops serialised uncompressed —
  `/Filter`/`/DecodeParms` are dropped because the body no longer
  carries the encoding they described, `/Length` is recomputed, and
  `/Resources`/`/Matrix`/`/BBox` plus every other key pass through
  verbatim. A deterministic `KasFm{n}` name (collision-checked
  against the page's effective `/XObject` dict) registers the clone
  in the page's materialised `/Resources`; only that `Do` is
  repointed. Nested `Do`s (`form_path` depth ≥ 2), `Image`/`Other`
  targets, and `/Group`/`/OC`/`/Ref` forms refuse — the last three
  via the walker's existing `blocker` path.
- **Rewrite = `Tj`→`TJ`, `TJ` element-split, `'`/`"` expansion** —
  a `Tj` losing codes becomes a `TJ` whose numeric elements restore
  the removed advance `-(w0 + (Tc+Tw)·1000/Tfs)` (`Tz` cancels — the
  TJ-number semantics is `w0 − n/1000` in glyph space before
  `Tfs·Th`); a `TJ` keeps its numeric elements and splits string
  elements around removed runs; `'`/`"` expand to `T*` + `Tj` (and
  `Tw`/`Tc` for `"`) so their state side effects survive an emptied
  string. `Tfs = 0` or `Tz = 0` is refused — the advance cannot be
  expressed as a TJ number.
- **Output = `Edit`/`rewrite_overlay`** — the new `/Contents` is a
  synthetic stream allocated through `Edit::alloc`, the page dict is
  overlaid, and `rewrite` produces a single revision. The input
  `Document` and its bytes are never modified; other pages sharing
  the original stream keep it untouched (the removed bytes still
  exist in the shared object — explicitly out of scope).
- **Cumulative budgets (M18.1)** — three limits the per-stream
  `max_operations` parse cap cannot express:
  - `max_diag_exec_ops` (200,000): every content-stream *execution*
    inside one `diagnose_redaction`/`remove_text` walk — page ops,
    ops inside a `Do`'d form, empty `Tj`s, state changes, unknown
    ops. A form invoked N times costs N× its op count.
  - `max_show_states` (200,000): entries in the `show_states`
    snapshot map; the map only records when `remove_text` asks,
    `diagnose_redaction` walks with snapshots off.
  - `max_edit_content_bytes` (64 MiB): cumulative bytes of
    *generated* stream bodies held in the `Edit` overlay — each
    cloned form body plus the new page `/Contents`. `serialize_ops`
    passes the cumulative count to a `BoundedWriter` whose
    `push`/`extend_from_slice` check the remaining budget *before*
    appending — a long `Tj` string or huge `TJ` array stops
    mid-write, never allocating past the bound. `charge_generated`
    counts each finished body; the writer's `max_output_bytes`
    still caps the final PDF independently.
- **Refusal list = diagnosis refusal list** — `/Annots`, `sh`/
  inline image/`gs`/Pattern/`ActualText`/OC, any `Do` inside a form,
  `Image`/non-Form XObject, any shown text mode other than `Tr 0`,
  `w = 0` hairlines, unsupported fonts, non-finite CTM, `/UserUnit`
  ≠ 1, vertical text — each is a typed `Error::Unsupported`, never
  a partial edit. `serialize_ops`/`write_op` reserialise every op in
  the stream (no verbatim copying); the match is exhaustive over
  `Operation`, so a future variant fails to compile rather than
  slipping through.
- **Audit trail** — `Removal { output, codes_removed, sites, scope }`
  reports which codes were removed (site identity from the
  diagnosis: `/Contents` ref + op index + code index) and the scope
  statement. `Removal`'s `Debug` impl is deliberately terse — output
  bytes and site contents are *not* printed so a stray `{:?}` never
  logs document text.

CLI: `kasane-pdf experimental-remove-text in.pdf 1 --rect llx lly urx
ury [--rect …] --output out.pdf` — `--output` is required, input and
output must differ (`refuse_same_file`), the write is atomic
(`write_atomic`), and a refusal leaves any existing output
untouched.

### M19.2 — image-region removal: exact union, Default* substitution, pre-decode budget

`src/remove_image.rs` implements `remove_image_region` +
`experimental-remove-image-region`. M19.2 fixes three issues without
widening scope:

- **Exact pixel union** — `image_pixel_ranges` splits rects into
  horizontal bands at every distinct y-boundary, then merges
  x-intervals within each band. A band is a maximal y-strip whose
  x-interval set is constant — the union is the exact set of covered
  pixels, not a bounding box. Corner-touching rects and L-shaped
  overlaps do not fill the pixels between them. The intermediate
  `Vec` is bounded by `rects.len()` (`max_diag_rects`), never by
  image height.
- **Default* substitution** — `validate_image` resolves the image's
  `/ColorSpace` name through the page's `/Resources /ColorSpace`
  dict, then applies the `Default*` substitution: a device name
  (`/DeviceGray`, `/DeviceRGB`) is checked against `/DefaultGray`/
  `/DefaultRGB` in the same dict. A non-device result is
  `Unsupported`; a missing entry is `Structure`. Substitution does
  not recurse — a `Default*` that is itself a device name is ignored.
- **Pre-decode budget** — `max_image_bytes` and `max_edit_content_bytes`
  are both checked *before* decoding. An image whose expected
  `Width×Height×ncomps` size exceeds the remaining cumulative budget
  is refused without allocating its buffer. A non-intersecting image
  is skipped before decode entirely. The decoded `Vec` is moved (not
  cloned) into the edited body — no extra copy.

CLI: `kasane-pdf experimental-remove-image-region in.pdf 1 --rect llx
lly urx ury [--rect …] --output out.pdf` — same atomic-write and
same-file-refusal rules as `experimental-remove-text`.

### M27 — JPEG (`/DCTDecode`) image editing

M27 extends the M19/M20 image-edit path to single-filter
sequential-DCT JPEGs — `DctSequential` covers baseline (SOF0) and
extended-sequential (SOF1) Huffman frames; differential and
arithmetic-coded sequential frames are refused by the decoder
itself as `Unsupported` inside `decode()`. Nothing about the
page-level eligibility rules, the rect-union algorithm, or the
clone-and-repoint mechanics changes; the only new logic is *how
the sample buffer is obtained*.

- **Where**: `decode_image` in `src/remove_image.rs` handles
  `/DCTDecode` by calling `dct_decode_for_edit`, which wraps
  `jpeg_decoder::Decoder` (the same crate the renderer already uses —
  no new dependency). `validate_image` admits a *single* `/DCTDecode`
  or `/DCT` filter; filter chains and every other codec remain
  refused. Both edit-side call sites (`remove_image_region`,
  `remove_region`'s page loop) pass the dict-declared `(w,h,ncomps)`
  into `decode_image` so the JPEG's own header is checked against the
  PDF before the pixel buffer is trusted.
- **Header check before decode**: `read_info` runs first — a corrupt
  or truncated JPEG fails fast with a `Parse` error, and `info()`'s
  `width`/`height`/`pixel_format` are compared to the dict. `CMYK32`,
  `L16`, and any other format are `Unsupported`; only `L8` (1 comp)
  and `RGB24` (3 comp) proceed.
- **Coding process check (M27.1)**: after `read_info` and before any
  pixel buffer is allocated, `info().coding_process` must be
  `DctSequential`. `DctProgressive` and `Lossless` are refused with
  `Error::Unsupported`, distinct from the `Parse` error on corrupt
  data. **Limit**: jpeg-decoder's public `CodingProcess` does not
  distinguish baseline (SOF0) from extended-sequential (SOF1) or
  differential/arithmetic sequential frames — those reach
  `decode()` and are refused there by the decoder itself
  (`Unsupported(Hierarchical)`/`Unsupported(ArithmeticEntropyCoding)`),
  so they are still `Unsupported`, never `Parse`.
- **Effective colour transform — decided explicitly (M27.2)**:
  ISO 32000-1 §7.4.8 (Table 13 `/ColorTransform`) + Adobe TN #5116.
  The precedence is resolved *by kasane*, not delegated to
  jpeg-decoder's `determine_color_transform` — the decoder ranks
  component IDs and JFIF *above* APP14, which contradicts the spec
  order. A minimal `scan_jpeg_markers` walk reads the APP14
  `transform` byte and each frame component's sampling factors;
  the effective transform is then:

  | APP14 | PDF `/ColorTransform` | effective transform (3-comp) |
  |---|---|---|
  | `0` | any | none — raw component values |
  | `1` | any | YCbCr→RGB |
  | `2` | any | `Unsupported` (YCCK = 4-comp) |
  | `>2` | any | `Unsupported` |
  | absent | `0` | none |
  | absent | `1` | YCbCr→RGB |
  | absent | absent | YCbCr→RGB (spec default for 3 comp) |

  The decoder setting is then chosen to *produce* that result:
  `YCbCr` forces the transform, `RGB` is used for "no transform"
  on a 3-component frame because it produces the same interleaved
  output as `None` without the panic that `color_no_convert`
  causes on subsampled data. "No transform" on a *subsampled*
  frame is still `Unsupported` — the per-plane layout cannot be
  represented as interleaved pixels. A 1-component frame always
  takes the decoder's grayscale path (`CT=1` is ignored per spec).
  Any other `DecodeParms` key is refused rather than ignored.
- **Budgets unchanged**: `max_image_dimension`/`total_samples` gate
  the dict geometry before decode; `max_image_bytes` is also set as
  the decoder's `max_decoding_buffer_size` so the JPEG cannot
  allocate past the cap; `max_edit_content_bytes` accounts for the
  decoded clone bytes cumulatively. A non-intersecting JPEG is never
  decoded — `image_pixel_ranges` returning empty still skips the
  whole edit before any decoder work.
- **Output**: the clone is written *uncompressed* — `/Filter` and
  `/DecodeParms` are dropped, `/Length` recomputed, and the dict's
  `Width`/`Height`/`ColorSpace`/`BitsPerComponent` are already
  consistent with the decoded samples (the header check guarantees
  it). JPEG-specific keys (`/K`…, `/ColorTransform` leftovers,
  EXIF data) are never copied; the original stream is untouched.
- **"Unaltered" defined on decoded samples**: the guarantee is that
  samples outside the rect union are byte-identical between the
  decoded original and the clone. JPEG is lossy — the clone is not
  promised to render pixel-identically to the original in every
  viewer, because decode/colour-transform/interpolation differences
  are inherent to re-encoding JPEG content.

### M28 — grid-aligned CTM image editing

`image_pixel_ranges` (`src/remove_image.rs`) no longer requires
axis-aligned *positive-scale* CTM. It first classifies the recorded
`Do` CTM into one of two *grid-aligned* classes — the components
are compared to `0.0` exactly, never epsilon-rounded, so a 1°
rotation or a hairline shear is refused, not folded into a class:

- **`CtmClass::AxisAligned`** — `b == 0, c == 0`, non-zero `a`,`d`
  of *either* sign. Negative `a`/`d` produce a horizontal/vertical
  reflection; `a<0, d<0` is a 180° rotation. Inverse:
  `(x,y) → ((x−e)/a, (y−f)/d)`.
- **`CtmClass::AxisSwapped`** — `a == 0, d == 0`, non-zero `b`,`c`.
  Corresponds to a 90°/270° rotation (and their reflections)
  composed with any non-uniform scale and finite translation.
  Inverse: `(x,y) → ((y−f)/b, (x−e)/c)` — the swap is written out
  explicitly, not hidden inside a generic matrix inversion.

The inverse map is computed *per class* by a `map_to_unit` closure.
Each rect's two opposite corners are mapped into the image's unit
square, the resulting `ux`/`uy` min/max are normalised (a reflection
or 90° swap reverses the order), intersected with `[0,1]²`, and
`floor`/`ceil`-mapped to pixel ranges — the outward-rounding, the
y-flip (unit `v=0` is the *bottom* pixel row), and the exact
band-union are all unchanged. Non-finite CTM input, or an inverse
that produces non-finite unit coordinates, returns
`Error::Structure`; every other non-grid-aligned shape is
`Error::Unsupported`. Because the two classes map axis-aligned user
rects to axis-aligned image rects, no polygon clipping or
arbitrary-transform rasteriser is needed.

The change is shared — `remove_image_region`, `remove_region`,
`remove_regions`, `remove_regions_and_document_metadata`, and the
CLI all call the same `image_pixel_ranges`; the DiagWalker's
recorded CTM (`TargetDetail::Image.ctm`) is still the single source,
no content-stream rescan. The input rect stays in un-rotated page
user space — a page `/Rotate` is display metadata and is *not*
applied to the selection. Positive-scale inputs are numerically
identical to before (the axis-aligned branch is the old code path).

### M20 — scoped region removal (text + image)

`src/remove_region.rs` implements `remove_region` +
`experimental-remove-region` — a single integrated edit pass over
one page, not a pipeline of `remove_text` → `remove_image_region`.

Design choices:

- **One walk, one rewrite** — `DiagWalker::new` runs once over the
  original content stream; every target is keyed by its original
  `op_index`. Text rewrites and image `Do` renames are applied to
  the same output ops list in one pass; the page's `/Contents` is
  serialised once, `Edit::rewrite()` runs once.
- **Planning against the original `Document`** — the walk never
  re-parses. `op_index` never drifts: a `Tj` that loses codes
  becomes a `TJ` at the same index; a `Do` that hits a rect gets a
  new `KasIm{n}` name at the same index. Text edits do not shift
  image `op_index` values and vice versa.
- **Shared internal helpers** — `remove_text::rewrite_ops_for` (TJ
  kern logic), `remove_text::serialize_ops`/`BoundedWriter`/
  `charge_generated`, `remove_image::validate_image`/`decode_image`/
  `image_pixel_ranges`/`fresh_xobject_name`/
  `is_default_decode`/`resolve_colorspace_name`/
  `apply_default_substitution` are all reused; nothing is
  duplicated.
- **M20.1 — CTM recorded at walk time** — `DiagWalker::do_image`
  stores the current `gs.ctm` into `TargetDetail::Image.ctm` at
  each `Image` `Do` call site. Both `remove_region` and
  `remove_image_region` read the recorded CTM instead of calling
  `ctm_at_op` (deleted). The walk's `max_diag_exec_ops` budget now
  covers the entire CTM acquisition cost — no O(images × ops)
  rescan. Two `Do`s of the same image at different CTMs each
  record their own matrix, and `q`/`Q`/`cm` are reflected
  correctly because the walker's graphics-state stack is the
  source of truth.
- **M20's own eligibility** — not a union of M17+M19 refusals:
  `Tr 0` text only (page-level), image eligibility per M19.2, and
  `Form` `Do` refused (M18's form handling is *not* inherited).
- **Shared cumulative budget** — `max_edit_content_bytes` bounds
  the cloned image bodies plus the new `/Contents` *together*;
  `charge_generated` runs before each image decodes and before
  the page stream is serialised. The budget is not reset per
  edit type.
- **Atomic failure** — any typed error aborts the whole call;
  no partial rewrite is returned. A rect hitting nothing produces
  `Ok` with zero counters.
- **M21 — orphaned XObject name removal** — after the edit pass,
  names that were edited (old `Do` names) and are no longer used
  by any remaining `Do` in `new_ops` are removed from the page's
  `/Resources /XObject` dict. The page's effective `/Resources`
  is cloned (not mutated in place), so a shared inherited
  `/Resources` is untouched. The image object itself is left to
  the writer's reachability scan — if another page or another
  name still references it, it is emitted. `orphaned_names_removed`
  reports names removed from `/XObject`, not objects deleted.
- **M21.1 — content-based verification** — re-parse the output
  and match image streams by decoded sample bytes, not by
  original object number (the writer renumbers). A removed name
  does not imply the object was deleted; an image can survive
  via an inherited `/Resources` even when the page's `/XObject`
  no longer names it.
- **M22 — inherited `/Resources` materialisation** — each page's
  effective `/Resources` (as resolved by the page-tree walk) is
  written explicitly into the page dict. A parent `/Pages` can
  then drop its `/Resources` — but only when *every* descendant
  page has been materialised. The parent keeps `/Type`, `/Kids`,
  `/Count`, `/MediaBox`, `/CropBox`, `/Rotate` — only `/Resources`
  is removed. The same `Edit`/overlay applies the materialisation,
  the text/image edits, and the `/Contents` rewrite in one pass.
  This is a *structural* cleanup, not a redaction guarantee: an
  image that survives via another page, another `Do` name, or an
  unmaterialised `/Pages` is still emitted.
- **M22.1 — traversal fix** — a `/Pages` node *without*
  `/Resources` is still traversed for descendants; the
  `has_resources` check is applied only when deciding whether to
  strip. The target page's dict is built once: `Contents` and the
  materialised `/Resources` coexist in one `page_dict`, not via
  two overlay inserts.
- **M22.2 — post-order strip decision** — a `/Pages` node's
  `all_materialised` is computed only after its children's
  results are known (`node_results: HashMap<IndirectRef, bool>`).
  A child's failure propagates upward — a parent never strips
  `/Resources` when a descendant is unmaterialised. Page refs are
  indexed by `HashMap`/`HashSet` (O(1) lookup) instead of a
  `Vec::contains` linear scan (O(n) per kid).
- **M22.3 — end-state fix** — a node that finishes processing
  records its result in `node_results` whether it succeeded or
  failed. The `continue` guard is `next_kid_idx < kids.len() &&
  all_materialised` — a `false` result is *not* deferred, so the
  parent sees `false`, not `None`. This prevents infinite
  re-pushing of a failed child. A `/Pages` node is pushed back
  to the stack only when a child is pushed (`all_materialised`
  is still `true`); a `false` break records the result
  immediately.
- **M22.4 — testability split** — the traversal is
  `plan_resource_materialisation(doc, pages_with_resources) ->
  (pages_to_strip, node_results)`. Returning `node_results` lets
  the tests assert each node's `false`/`true` — including that a
  3-level `false` propagates to the root and that a `false` node
  is never re-pushed — instead of only observing the final strip
  outcome through `remove_region`. (The earlier M22.3 tests went
  through `remove_region` and tolerated any `Err`, which masked
  the case where the target page itself was uneditable.)
  Termination relies on the parser: `walk_page_tree` rejects
  page-tree cycles (`CycleDetected`) and enforces
  `max_page_tree_depth`/`max_page_tree_nodes` before the
  materialisation plan ever runs, so this pass adds no `visited`
  set of its own. Complexity is O(nodes + edges) — each `/Pages`
  node is popped once per incoming edge at most, and each
  finished node records exactly one `node_results` entry.

CLI: `kasane-pdf experimental-remove-region in.pdf 1 --rect llx lly
urx ury [--rect …] --output out.pdf` — same atomic-write and
same-file-refusal rules as the other experimental commands.
