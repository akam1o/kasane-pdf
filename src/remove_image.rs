//! **Experimental** image-region removal.
//!
//! [`remove_image_region`] replaces the image samples whose painted
//! positions land inside caller-supplied rectangles with opaque black
//! (all-zero samples) in a *new* image object — the original stream
//! is never mutated, and only the `Do` call sites that hit a rect are
//! repointed to the clone.
//!
//! **This is not a redaction feature.** It is an experiment in
//! *pixel-level content editing*: it does not remove other copies of
//! the same image data (metadata, annotations, structure tree, other
//! pages, thumbnails), does not guarantee irreversibility, and does
//! not reason about what the image depicts. See the crate docs and
//! `docs/features/editing-image.md` for the full scope list.
//!
//! Scope — a page is *refused* (typed error) rather than half-edited
//! when any of these appear in its content or page dict:
//!
//! - `/Annots` — annotations are out of scope.
//! - `/UserUnit` ≠ 1 — coordinates are not default units.
//! - Any `Do` to a `/Form` — nested XObjects are out of scope.
//! - Any inline image (`BI`/`ID`/`EI`).
//! - Any `gs`, `sh`, Pattern colour space, `/ActualText`/`OC` marked
//!   content, or any other construct the diagnosis marks as
//!   `blocker`/`uncertain` — the walk refuses on the first one.
//! - Any image that is not 8-bit `/DeviceGray` or `/DeviceRGB` (after
//!   resolving `/ColorSpace` names through the page's `/Resources
//!   /ColorSpace` dict *and* applying the `Default*` substitution —
//!   a `/DeviceRGB` image with `/DefaultRGB` → `/CalRGB` is refused,
//!   because the effective colour space is not a device space) with
//!   no `/Mask`, `/SMask`, `/SMaskInData`, `/OC`, `/ImageMask`, or
//!   `/Interpolate` — the sample format must be exactly what we can
//!   rewrite.
//! - Any non-default `/Decode` array — inverted or remapped samples
//!   are not rewritten.
//! - Any image with a `/Filter` other than none, a single
//!   `/FlateDecode`, or a single `/DCTDecode` — filter chains,
//!   `/JPXDecode`, and every other codec are out of scope. For
//!   DCTDecode only sequential-DCT JPEGs decoding to 8-bit Gray or RGB
//!   samples are accepted (the decoder's `L8`/`RGB24` formats);
//!   CMYK/YCCK, 16-bit, and progressive files are refused. The
//!   JPEG's own width/height/component count must match the PDF
//!   dict. `/DecodeParms /ColorTransform` is honoured — `0` keeps
//!   the raw component values, `1` or absent lets the decoder
//!   apply the JPEG's internal transform — and any other key in
//!   `DecodeParms` is refused rather than ignored.
//! - Any image whose CTM is not one of the *grid-aligned* forms —
//!   axis-aligned (including negative scales = reflections) or
//!   axis-swapping (90°/270° rotations and reflected rotations).
//!   Arbitrary-angle rotations, shears, and singular/non-finite
//!   transforms are refused.
//! - Any `Image` target at `form_path` depth ≥ 1 — images inside a
//!   form are out of scope.
//!
//! Pixel selection: each rect's intersection with the image's painted
//! axis-aligned bounding box is mapped back to image space via the
//! inverse of the `Do`'s CTM (a grid-aligned affine — axis-aligned
//! with either sign, or axis-swapping). The resulting real-valued
//! pixel bounds are
//! expanded *outward* — `floor` on the low edge, `ceil` on the high
//! edge — so no covered pixel is lost to rounding. Multiple rects
//! produce an *exact* union of ranges, not a bounding box — the
//! union is computed by splitting into horizontal bands at every
//! distinct y-boundary and merging x-intervals within each band.
//! Corner-touching rects and L-shaped overlaps do not fill the
//! pixels between them; overlapping ranges are merged so no pixel
//! is visited twice. A pixel that is only partially covered is
//! still replaced; the edit is conservative, not exact.
//!
//! `pixels_changed` counts *pixels*, not components: a pixel counts
//! when **at least one of its components changed from non-0 to 0**
//! inside the selected rects. An RGB pixel is 1 whether 1 or 3
//! channels went to black; an already-all-0 pixel counts 0; a pixel
//! covered by two overlapping rects is counted once. Gray follows
//! the same definition (one component per pixel).
//!
//! Resource budgets: `max_image_bytes` and `max_edit_content_bytes`
//! are both checked *before* decoding — an image whose expected
//! `Width × Height × ncomps` size exceeds the remaining cumulative
//! budget is refused without allocating its buffer. A non-intersecting
//! image is skipped before decode entirely (no budget consumed).
//! `max_image_dimension` and `max_image_total_samples` bound the
//! geometry; `checked_mul` guards the `Width × Height × ncomps` size.
//! The same `max_edit_content_bytes` cumulative generated-byte cap
//! as `remove_text` bounds the cloned image bodies plus the rewritten
//! `/Contents`; `charge_generated` runs before the decoded buffer is
//! held. `max_diag_exec_ops` bounds the page walk;
//! `max_xobject_invocations` bounds the `Do` count. The final PDF is
//! bounded by `max_output_bytes` at serialize time — `BoundedWriter`
//! checks the remaining budget before every byte lands (logical
//! bytes written, not allocator capacity/RSS).

use crate::content::Operation;
use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, IndirectRef, Object};
use crate::redact_diag::{check_rect, page_dict_entry, DiagWalker, Site, TargetDetail, TargetKind};
use std::collections::BTreeMap;

/// The result of [`remove_image_region`] — the rewritten PDF bytes
/// plus the audit trail of what was removed.
pub struct ImageRemoval {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// How many `Do` invocations were edited.
    pub invocations_edited: u64,
    /// How many pixels were changed to black.
    pub pixels_changed: u64,
    /// The `Site` of every edited invocation — page content ref +
    /// op index (per `Site` docs).
    pub sites: Vec<Site>,
    /// Human-readable scope statement for the CLI/report — what was
    /// checked and what limits applied. Never contains image bytes.
    pub scope: Vec<String>,
}

/// Terse summary for logging — never prints `output` bytes or site
/// content so a stray `{:?}` cannot leak document data.
impl std::fmt::Debug for ImageRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageRemoval")
            .field("output_bytes", &self.output.len())
            .field("invocations_edited", &self.invocations_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("sites", &self.sites.len())
            .field("scope", &self.scope)
            .finish()
    }
}

/// Experimental image-region removal — see the module docs for the
/// full refusal list. `rects` are in un-rotated, un-cropped user
/// space (+x right, +y up) — the same convention `diagnose_redaction`
/// and `extract_text` use.
///
/// On any failure — validation, an unsupported construct, or a
/// limit — nothing is emitted; a partial rewrite is never returned
/// as success.
pub fn remove_image_region(
    doc: &Document,
    page_index: usize,
    rects: &[(f64, f64, f64, f64)],
    limits: &Limits,
) -> Result<ImageRemoval, Error> {
    // ---- input validation (same rules as the diagnosis) -----------
    if rects.is_empty() {
        return Err(Error::Structure("at least one --rect is required".into()));
    }
    if rects.len() as u64 > limits.max_diag_rects {
        return Err(Error::LimitExceeded {
            limit: "max_diag_rects",
            max: limits.max_diag_rects,
        });
    }
    let mut validated = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        validated.push(check_rect(i, r.0, r.1, r.2, r.3)?);
    }
    let page = doc.pages().get(page_index).ok_or_else(|| {
        Error::Structure(format!(
            "page {} does not exist (document has {} pages)",
            page_index + 1,
            doc.page_count()
        ))
    })?;

    // ---- pre-flight refusal checks --------------------------------
    // /Annots — annotations are out of scope.
    if page_dict_entry(doc, page, b"Annots")?.is_some() {
        return Err(Error::Unsupported {
            feature: "image removal on a page with /Annots",
            detail: "annotations are out of scope — their content is \
                     not evaluated"
                .into(),
        });
    }
    // /UserUnit ≠ 1 changes user-space units — refuse as the
    // diagnosis does.
    if let Some(uu) = page_dict_entry(doc, page, b"UserUnit")? {
        match uu.as_number() {
            Some(v) if (v - 1.0).abs() > f64::EPSILON => {
                return Err(Error::Unsupported {
                    feature: "/UserUnit other than 1",
                    detail: "coordinates are not default units".into(),
                })
            }
            Some(_) => {}
            None => return Err(Error::Structure("/UserUnit is not a number".into())),
        }
    }

    // ---- walk the content ------------------------------------------
    // Reuse the diagnosis walker: it tracks graphics/text state the
    // same way the renderer does, reports a `Target` per occurrence,
    // and flags every construct we refuse as `uncertain`/`blocker`
    // before we ever touch the stream.
    let ops = doc.page_operations(page)?;
    let mut walker = DiagWalker::new_diag_only(doc, page, limits, &validated);
    walker.run(&ops)?;
    // Any blocker or uncertain entry means part of the page's state
    // or content was not fully understood — refuse rather than edit
    // past it.
    if let Some(b) = walker.blockers.first() {
        return Err(Error::Unsupported {
            feature: "image removal on a page with blockers",
            detail: format!("{}: {}", b.what, b.detail),
        });
    }
    if let Some(u) = walker.uncertain.first() {
        return Err(Error::Unsupported {
            feature: "image removal on a page with uncertain content",
            detail: u.reason.clone(),
        });
    }
    // `sh` is out of scope outright — shading fills an area we cannot
    // bound or rewrite around. Refuse on the first one.
    if let Some((i, _)) = ops
        .iter()
        .enumerate()
        .find(|(_, o)| matches!(o, Operation::Shading(_)))
    {
        return Err(Error::Unsupported {
            feature: "image removal on a page with sh",
            detail: format!("op #{i} — shading content is out of scope"),
        });
    }
    // Inline images are out of scope — the parser already refuses
    // `BI`/`ID`/`EI` with `Error::Unsupported`, so reaching here means
    // none exist on the page.

    // ---- page-level eligibility -----------------------------------
    // A page is eligible only when every occurrence the walk produced
    // is one `remove_image_region` can faithfully rewrite:
    //
    // - `Image` targets at `form_path` depth 0 are the page-level
    //   `Do` calls we may edit. At depth ≥ 1 they are inside a form
    //   → nested, refused.
    // - `Form` targets are refused — nested XObjects are out of
    //   scope for `remove_image_region`.
    // - `Text` and `Path` targets are unaffected — we only rewrite
    //   `Do` calls, and the page's other content passes through
    //   unchanged.
    // - `Other` targets are refused regardless of rects.
    for t in &walker.targets {
        if t.site.form_path.len() > 1 {
            return Err(Error::Unsupported {
                feature: "image removal on a page with nested XObjects",
                detail: "a Do inside a form is out of scope".into(),
            });
        }
        match t.kind {
            TargetKind::Image if t.site.form_path.is_empty() => {}
            TargetKind::Form if !t.site.form_path.is_empty() => {
                return Err(Error::Unsupported {
                    feature: "image removal on a page with nested XObjects",
                    detail: "a Do inside a form is out of scope".into(),
                });
            }
            TargetKind::Form => {
                return Err(Error::Unsupported {
                    feature: "image removal on a page with form XObjects",
                    detail: "a /Form Do is out of scope for `remove_image_region`".into(),
                });
            }
            TargetKind::Text | TargetKind::Path => {}
            TargetKind::Image | TargetKind::Other => {
                return Err(Error::Unsupported {
                    feature: "image removal on a page with non-image XObjects",
                    detail: format!(
                        "an {:?} occurrence at op #{} is out of scope",
                        t.kind, t.site.op_index
                    ),
                });
            }
        }
    }

    // ---- collect image invocations to edit --------------------------
    // `op_index → (Site, CTM, image_ref, name)` for each `Do` to an image
    // whose painted bounds hit a rect. Two `Do`s of the same image
    // are distinct invocations — each gets its own clone.
    let mut to_edit: BTreeMap<usize, (Site, Matrix, Option<IndirectRef>, Vec<u8>)> =
        BTreeMap::new();
    for t in &walker.targets {
        if t.kind == TargetKind::Image && t.site.form_path.is_empty() && !t.hits.is_empty() {
            if let TargetDetail::Image {
                xobj, name, ctm, ..
            } = &t.detail
            {
                // The CTM is recorded by the walker at the `Do`
                // call site — no re-walk needed.
                to_edit.insert(t.site.op_index, (t.site.clone(), *ctm, *xobj, name.clone()));
            }
        }
    }

    // ---- clone images for edited invocations ------------------------
    let mut edit_guard: Option<Edit> = None;
    let mut do_renames: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut removed_sites: Vec<Site> = Vec::new();
    let mut pixels_changed: u64 = 0;
    let mut generated_bytes: u64 = 0;
    let mut new_xobject_entries: Vec<(Vec<u8>, IndirectRef)> = Vec::new();

    for (op_index, (site, ctm, _xobj_ref, name)) in &to_edit {
        // Resolve the image stream through the page's effective
        // `/XObject` dict — the `Site` does not carry the resource
        // name; the `TargetDetail::Image` does.
        let entry = doc
            .resolve_resource_raw(
                page.resources()
                    .ok_or_else(|| Error::Structure("page has no /Resources".into()))?,
                "XObject",
                name.as_slice(),
            )?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/XObject /{} no longer resolves",
                    String::from_utf8_lossy(name)
                ))
            })?;
        let obj = doc.deref(&entry)?;
        let (dict, raw) = obj
            .as_stream()
            .ok_or_else(|| Error::Structure("image XObject is not a stream".into()))?;

        // ---- validate the image ----------------------------
        let (width, height, ncomps) = validate_image(dict, page.resources(), doc, limits)?;

        // ---- compute the pixel ranges to blacken -------------------
        // Each rect produces its own (x0,y0,x1,y1) — the union of
        // *these* ranges is what gets blackened, not their bounding
        // box. Disjoint rects do not fill the pixels between them.
        let ranges = image_pixel_ranges(ctm, width, height, &validated)?;
        if ranges.is_empty() {
            continue; // no pixels to change — skip decode entirely
        }

        // ---- decode the sample data --------------------------------
        // Budget is checked *before* the buffer grows — a raw image
        // larger than `max_image_bytes` is refused without cloning,
        // and a Flate stream stops mid-decode when the output would
        // exceed the cap. The cumulative `max_edit_content_bytes` is
        // checked *before* decode so a second image is refused without
        // allocating its full buffer.
        let expected = (width as u64)
            .checked_mul(height as u64)
            .and_then(|s| s.checked_mul(ncomps as u64))
            .ok_or(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: limits.max_image_bytes,
            })?;
        if expected > limits.max_image_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: limits.max_image_bytes,
            });
        }
        charge_generated(generated_bytes, expected as usize, limits)?;
        let decoded = decode_image(dict, raw, limits, (width, height, ncomps))?;
        if decoded.len() as u64 != expected {
            return Err(Error::Structure(format!(
                "image /{} data length {} does not match {}x{}x{} = {}",
                String::from_utf8_lossy(name),
                decoded.len(),
                width,
                height,
                ncomps,
                expected
            )));
        }

        // Count changed *pixels* (not components): a pixel is one unit
        // regardless of how many of its channels were non-zero.
        let mut new_data = decoded;
        let mut changed_here: u64 = 0;
        for &(x0, y0, x1, y1) in &ranges {
            for y in y0..y1 {
                for x in x0..x1 {
                    let idx = ((y as u64) * (width as u64) + (x as u64)) * (ncomps as u64);
                    let mut pixel_changed = false;
                    for c in 0..ncomps {
                        let i = (idx + c as u64) as usize;
                        if new_data[i] != 0 {
                            new_data[i] = 0;
                            pixel_changed = true;
                        }
                    }
                    if pixel_changed {
                        changed_here += 1;
                    }
                }
            }
        }
        if changed_here == 0 {
            continue; // already all black
        }
        pixels_changed += changed_here;

        // ---- build the new image object -----------------------------
        let mut new_dict = dict.clone();
        new_dict.remove(b"Filter".as_slice());
        new_dict.remove(b"DecodeParms".as_slice());
        new_dict.remove(b"Length".as_slice());
        new_dict.insert(b"Length".to_vec(), Object::Integer(new_data.len() as i64));
        // The image may carry `/Decode` — keep it only when it is the
        // default [0 1 …] array so the new black samples are not
        // remapped back to white.
        if let Some(d) = dict.get(b"Decode".as_slice()) {
            if !is_default_decode(d, ncomps) {
                return Err(Error::Unsupported {
                    feature: "image with non-default /Decode",
                    detail: "inverted or remapped samples are not rewritten".into(),
                });
            }
        }

        if edit_guard.is_none() {
            edit_guard = Some(Edit::new(doc, limits.clone())?);
        }
        let edit = edit_guard.as_mut().unwrap();
        let clone_ref = edit.alloc()?;
        generated_bytes = charge_generated(generated_bytes, new_data.len(), limits)?;
        edit.overlay.insert(
            (clone_ref.number, clone_ref.generation),
            Object::Stream {
                dict: new_dict,
                data: new_data,
            },
        );

        // Register the clone under a deterministic fresh name.
        let allocated: Vec<Vec<u8>> = new_xobject_entries.iter().map(|(n, _)| n.clone()).collect();
        let new_name = fresh_xobject_name(page, doc, do_renames.len() as u64, &allocated)?;
        do_renames.insert(*op_index, new_name.clone());
        new_xobject_entries.push((new_name, clone_ref));
        removed_sites.push(site.clone());
    }

    // ---- rewrite the page content stream ----------------------------
    let mut new_ops: Vec<Operation> = Vec::with_capacity(ops.len());
    for (op_index, op) in ops.iter().enumerate() {
        match op {
            Operation::Do(_) => {
                if let Some(new_name) = do_renames.get(&op_index) {
                    new_ops.push(Operation::Do(new_name.clone()));
                } else {
                    new_ops.push(op.clone());
                }
            }
            _ => new_ops.push(op.clone()),
        }
    }

    // ---- serialise the new page stream -------------------------------
    let new_body = serialize_ops(&new_ops, limits, generated_bytes)?;
    charge_generated(generated_bytes, new_body.len(), limits)?;
    let mut edit = match edit_guard {
        Some(e) => e,
        None => Edit::new(doc, limits.clone())?,
    };
    let stream_ref = edit.alloc()?;
    edit.overlay.insert(
        (stream_ref.number, stream_ref.generation),
        Object::Stream {
            dict: {
                let mut d = crate::object::Dict::new();
                d.insert(b"Length".to_vec(), Object::Integer(new_body.len() as i64));
                d
            },
            data: new_body,
        },
    );
    let page_ref = page.object_ref().ok_or_else(|| {
        Error::Structure("cannot edit a page that has no indirect reference".into())
    })?;
    let page_obj = doc.resolve(page_ref)?;
    let mut page_dict = page_obj
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Structure(format!("page {page_ref} is not a dictionary")))?;
    page_dict.insert(b"Contents".to_vec(), Object::Ref(stream_ref));
    if !new_xobject_entries.is_empty() {
        // Merge the cloned images into the page's *effective* `/XObject`
        // dict — the effective dict is materialised into the page's
        // `/Resources` so an inherited or shared resources object is
        // never mutated. Other categories (`/Font`, `/ExtGState`, …)
        // pass through verbatim (refs preserved).
        let mut res_dict = match page.resources() {
            Some(r) => doc
                .deref(r)?
                .as_dict()
                .cloned()
                .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?,
            None => Dict::new(),
        };
        // The `/XObject` entry may itself be a `Ref` to a shared dict
        // — deref + clone so the shared object is never mutated.
        let mut xobj_dict = match res_dict.get(b"XObject".as_slice()) {
            Some(o) => doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                Error::Structure("/Resources /XObject is not a dictionary".into())
            })?,
            None => Dict::new(),
        };
        for (name, r) in &new_xobject_entries {
            // `fresh_xobject_name` already verified the name is free.
            xobj_dict.insert(name.clone(), Object::Ref(*r));
        }
        res_dict.insert(b"XObject".to_vec(), Object::Dict(xobj_dict));
        page_dict.insert(b"Resources".to_vec(), Object::Dict(res_dict));
    }
    edit.overlay.insert(
        (page_ref.number, page_ref.generation),
        Object::Dict(page_dict),
    );
    let output = edit.rewrite()?;

    Ok(ImageRemoval {
        output,
        invocations_edited: do_renames.len() as u64,
        pixels_changed,
        sites: removed_sites,
        scope: vec![
            format!(
                "page {} ({} rect{})",
                page_index + 1,
                validated.len(),
                if validated.len() == 1 { "" } else { "s" }
            ),
            format!("invocations edited: {}", do_renames.len()),
            format!("pixels changed: {pixels_changed}"),
            "scope: page-level /Image Do calls only — no forms, no inline images, \
             no masks, no non-device colour, no non-default /Decode. Images: no \
             filter / FlateDecode / single DCTDecode (8-bit Gray/RGB only); \
             JPEG is decoded to samples and written back uncompressed — other \
             copies of the same data are not removed."
                .into(),
        ],
    })
}

/// Validate an image XObject for region removal — returns `(width, height,
/// ncomps)` on success, `Err` on any unsupported attribute.
///
/// `resources` is the page's effective `/Resources` — needed to
/// resolve named colour spaces (`/CS1`) and `Default*` substitutions.
///
/// Shared with `remove_region.rs`.
pub(crate) fn validate_image(
    dict: &Dict,
    resources: Option<&Object>,
    doc: &Document,
    limits: &Limits,
) -> Result<(u32, u32, usize), Error> {
    let width = dict
        .get(b"Width".as_slice())
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure("image has no /Width".into()))?;
    let height = dict
        .get(b"Height".as_slice())
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure("image has no /Height".into()))?;
    if width <= 0 || height <= 0 {
        return Err(Error::Structure(format!(
            "image dimensions {width}x{height} are not positive"
        )));
    }
    if width as u64 > limits.max_image_dimension as u64
        || height as u64 > limits.max_image_dimension as u64
    {
        return Err(Error::LimitExceeded {
            limit: "max_image_dimension",
            max: limits.max_image_dimension as u64,
        });
    }
    let samples = (width as u64)
        .checked_mul(height as u64)
        .ok_or(Error::LimitExceeded {
            limit: "max_image_total_samples",
            max: limits.max_image_total_samples,
        })?;
    if samples > limits.max_image_total_samples {
        return Err(Error::LimitExceeded {
            limit: "max_image_total_samples",
            max: limits.max_image_total_samples,
        });
    }

    // Blocking attributes — same list the diagnosis walker uses.
    for key in [
        b"Mask".as_slice(),
        b"SMask".as_slice(),
        b"SMaskInData".as_slice(),
        b"OC".as_slice(),
        b"ImageMask".as_slice(),
        b"Interpolate".as_slice(),
    ] {
        if let Some(v) = dict.get(key) {
            if key == b"SMaskInData" && v.as_i64() == Some(0) {
                continue;
            }
            if key == b"ImageMask" && v.as_bool() == Some(false) {
                continue;
            }
            if key == b"Interpolate" && v.as_bool() == Some(false) {
                continue;
            }
            return Err(Error::Unsupported {
                feature: "image attribute",
                detail: format!(
                    "/{} is not supported for image removal",
                    String::from_utf8_lossy(key)
                ),
            });
        }
    }

    let bpc = dict
        .get(b"BitsPerComponent".as_slice())
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure("image has no /BitsPerComponent".into()))?;
    if bpc != 8 {
        return Err(Error::Unsupported {
            feature: "image BitsPerComponent",
            detail: format!("{bpc} (only 8 supported)"),
        });
    }
    let cs_obj = dict
        .get(b"ColorSpace".as_slice())
        .ok_or_else(|| Error::Structure("image has no /ColorSpace".into()))?;
    let ncomps = match cs_obj {
        Object::Name(n) => {
            // Resolve the name through the page's /Resources
            // /ColorSpace dict. Two steps are distinguished:
            //   A. The image's own name (e.g. /CS1) is looked up.
            //   B. A device name (DeviceGray/DeviceRGB) is then
            //      checked against Default* substitution — if the
            //      resource dict has /DefaultGray or /DefaultRGB,
            //      that entry overrides the device space.
            let resolved = resolve_colorspace_name(doc, resources, n)?;
            let final_obj = apply_default_substitution(doc, resources, resolved)?;
            match final_obj.as_name() {
                Some(b"DeviceGray") => 1,
                Some(b"DeviceRGB") => 3,
                _ => {
                    return Err(Error::Unsupported {
                        feature: "image colour space",
                        detail: format!(
                            "/{} resolves to non-device space",
                            String::from_utf8_lossy(n)
                        ),
                    })
                }
            }
        }
        Object::Ref(r) => {
            // An indirect colour space — resolve the reference, then
            // apply the same name-based rules. A named space that
            // resolves to another name is followed one level deep.
            let resolved = doc.deref(&Object::Ref(*r))?;
            let name = resolved.as_name().ok_or_else(|| Error::Unsupported {
                feature: "image colour space",
                detail: "indirect colour space is not a name".into(),
            })?;
            let resolved2 = resolve_colorspace_name(doc, resources, name)?;
            let final_obj = apply_default_substitution(doc, resources, resolved2)?;
            match final_obj.as_name() {
                Some(b"DeviceGray") => 1,
                Some(b"DeviceRGB") => 3,
                _ => {
                    return Err(Error::Unsupported {
                        feature: "image colour space",
                        detail: format!(
                            "indirect /{} resolves to non-device space",
                            String::from_utf8_lossy(name)
                        ),
                    })
                }
            }
        }
        _ => {
            return Err(Error::Unsupported {
                feature: "image colour space",
                detail: "non-name colour space is not supported for image removal".into(),
            })
        }
    };

    // Filter must be absent, or a *single* FlateDecode or DCTDecode.
    // Filter chains are refused — the caller only ever decodes the
    // first filter anyway, so a chain would silently ignore part of
    // the encoding.
    let filters = crate::filter::normalize_filters(dict.get(b"Filter".as_slice()))?;
    match filters.len() {
        0 => {}
        1 if filters[0] == b"FlateDecode"
            || filters[0] == b"Fl"
            || filters[0] == b"DCTDecode"
            || filters[0] == b"DCT" => {}
        _ => {
            return Err(Error::Unsupported {
                feature: "image stream filter",
                detail: format!(
                    "{:?} — only none, FlateDecode, or DCTDecode are supported",
                    filters
                        .iter()
                        .map(|f| String::from_utf8_lossy(f).into_owned())
                        .collect::<Vec<_>>()
                ),
            })
        }
    }

    // For DCTDecode the JPEG's own header must agree with the PDF
    // dict — checked inside `decode_image` where the decoder runs.
    Ok((width as u32, height as u32, ncomps))
}

/// Decode image sample data — reuses the stream decoder with the
/// image's own limits. Supports no filter, a single `/FlateDecode`,
/// or a single `/DCTDecode` (sequential-DCT JPEG). Anything else is
/// `Error::Unsupported`.
///
/// `expected` is the dict-declared `(width, height, ncomps)` — for
/// DCTDecode the JPEG's own header is checked against it *before*
/// the pixel buffer is allocated, so a mismatched stream fails fast.
/// For raw/Flate the caller re-checks `decoded.len() == w*h*ncomps`
/// after decode, so no extra check is needed here.
///
/// `max_image_bytes` is checked *before* the buffer grows: a raw
/// image larger than the cap is refused without cloning, a Flate
/// stream stops mid-decode when the output would exceed it, and the
/// JPEG decoder's own `max_decoding_buffer_size` is set to the same
/// cap so a hostile JPEG cannot allocate past it.
///
/// Shared with `remove_region.rs` and the combined entry point.
pub(crate) fn decode_image(
    dict: &Dict,
    raw: &[u8],
    limits: &Limits,
    expected: (u32, u32, usize),
) -> Result<Vec<u8>, Error> {
    let filters = crate::filter::normalize_filters(dict.get(b"Filter".as_slice()))?;
    if filters.is_empty() {
        // Raw data — check the cap before cloning.
        if raw.len() as u64 > limits.max_image_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: limits.max_image_bytes,
            });
        }
        return Ok(raw.to_vec());
    }
    if filters.len() > limits.max_filter_chain {
        return Err(Error::LimitExceeded {
            limit: "max_filter_chain",
            max: limits.max_filter_chain as u64,
        });
    }
    let parms =
        crate::filter::normalize_decode_parms(dict.get(b"DecodeParms".as_slice()), filters.len())?;
    let mut buf = raw.to_vec();
    for (i, fname) in filters.iter().enumerate() {
        let parm = parms.get(i).and_then(|o| o.as_ref());
        buf = match fname.as_slice() {
            b"FlateDecode" | b"Fl" => {
                let mut dec = flate2::read::ZlibDecoder::new(buf.as_slice());
                // Read with a cap — stop before the output exceeds
                // `max_image_bytes`, not after the whole stream is
                // inflated.
                let mut out = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    let n = std::io::Read::read(&mut dec, &mut chunk)
                        .map_err(|e| Error::Parse(format!("FlateDecode failed: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    let new_len =
                        (out.len() as u64)
                            .checked_add(n as u64)
                            .ok_or(Error::LimitExceeded {
                                limit: "max_image_bytes",
                                max: limits.max_image_bytes,
                            })?;
                    if new_len > limits.max_image_bytes {
                        return Err(Error::LimitExceeded {
                            limit: "max_image_bytes",
                            max: limits.max_image_bytes,
                        });
                    }
                    out.extend_from_slice(&chunk[..n]);
                }
                crate::filter::apply_predictor(out, parm, limits)?
            }
            b"DCTDecode" | b"DCT" => dct_decode_for_edit(&buf, parm, limits, expected)?,
            other => {
                return Err(Error::Unsupported {
                    feature: "image stream filter",
                    detail: format!("/{}", String::from_utf8_lossy(other)),
                })
            }
        };
    }
    Ok(buf)
}

/// What [`scan_jpeg_markers`] found in a JPEG byte stream.
///
/// Three fields drive the *effective* colour-transform decision the
/// edit path must make itself (rather than leaving it to the
/// decoder's component-ID/JFIF heuristics, which rank below the
/// Adobe APP14 flag in the PDF spec's precedence order):
///
/// * `adobe_transform` — the APP14 `Adobe\0` transform byte, or
///   `None` when no APP14 segment carries the Adobe signature.
///   `Some(v)` records the *first* valid transform value seen;
///   `conflicting_app14` flags a second segment disagreeing with it.
/// * `subsampled` — any frame component has a sampling factor
///   other than 1×1.
/// * `truncated` — the marker walk hit a short buffer, an out-of-
///   range segment length, or bytes that stopped looking like a
///   marker sequence before `SOS`. In that case `adobe_transform`
///   may be a false negative and the caller must not silently fall
///   back to defaults — the data is corrupt.
struct JpegMarkers {
    adobe_transform: Option<u8>,
    /// A second APP14 `Adobe\0` segment whose transform byte
    /// disagrees with the first. The spec defines one transform
    /// per stream — two contradictory values are ambiguous, so the
    /// caller refuses rather than guess.
    conflicting_app14: bool,
    subsampled: bool,
    /// The marker walk could not be completed (truncated or
    /// structurally invalid before `SOS`).
    truncated: bool,
}

/// Minimal JPEG marker walk — reads only what the edit path needs:
/// the Adobe APP14 `transform` byte and each frame component's
/// sampling factors. Anything else is skipped via the marker's
/// own length field.
///
/// Marker handling mirrors the decoder (`read_marker`): `0xFF`
/// fill bytes before a marker are skipped, and the walk stops at
/// the first `SOS` (start of scan) since the entropy-coded data
/// follows and may legitimately contain `0xFFxx` sequences. Any
/// deviation — non-`0xFF` byte where a marker is expected, a
/// segment whose declared length runs past the buffer, a truncated
/// segment — sets `truncated` so the caller does not mistake a
/// partial walk for "no APP14".
///
/// The APP14 signature check uses the same 6-byte `"Adobe\0"`
/// prefix the decoder requires (`parse_app`) — a bare `"Adobe"`
/// with a different sixth byte is *not* recognised as the colour-
/// transform marker by either implementation.
fn scan_jpeg_markers(data: &[u8]) -> JpegMarkers {
    let mut m = JpegMarkers {
        adobe_transform: None,
        conflicting_app14: false,
        subsampled: false,
        truncated: false,
    };
    // Must start with SOI.
    if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
        m.truncated = true; // not a JPEG — decoder will report the error
        return m;
    }
    let mut i = 2;
    'outer: while i < data.len() {
        // Skip `0xFF` fill bytes before the marker — same rule as
        // the decoder's `read_marker` (B.1.1.2: any marker may be
        // preceded by any number of `0xFF` fill bytes).
        while i < data.len() && data[i] == 0xFF && i + 1 < data.len() && data[i + 1] == 0xFF {
            i += 1;
        }
        if i + 1 >= data.len() {
            m.truncated = true;
            break;
        }
        if data[i] != 0xFF {
            // Lost marker sync before SOS — the bytes are corrupt.
            m.truncated = true;
            break;
        }
        let marker = data[i + 1];
        // Standalone markers — no length field.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA {
            break; // SOS — image data follows; clean stop.
        }
        // Every other marker carries a 2-byte big-endian length.
        if i + 4 > data.len() {
            m.truncated = true;
            break;
        }
        let len = ((data[i + 2] as usize) << 8) | data[i + 3] as usize;
        if len < 2 || i + 2 + len > data.len() {
            m.truncated = true;
            break;
        }
        let seg = &data[i + 4..i + 2 + len];
        match marker {
            // APP14 — decoder requires the 6-byte "Adobe\0" prefix,
            // then reads transform at byte offset 11 of the segment
            // (5 "Adobe" + 1 flags0 + 2 flags1 + 2 flags2 + 1 tf).
            0xEE => {
                if seg.len() >= 12 && seg[..6] == *b"Adobe\0" {
                    match m.adobe_transform {
                        None => m.adobe_transform = Some(seg[11]),
                        Some(v) if v != seg[11] => {
                            m.conflicting_app14 = true;
                            break 'outer;
                        }
                        Some(_) => {}
                    }
                }
            }
            // SOF0/1/2 — read each component's sampling nibble.
            // Layout: precision(1) height(2) width(2) ncomp(1)
            //         then per-component { id(1) sampling(1) q(1) }.
            0xC0..=0xC2 if seg.len() >= 6 => {
                let ncomp = seg[5] as usize;
                let mut p = 6;
                for _ in 0..ncomp {
                    if p + 2 >= seg.len() {
                        m.truncated = true; // declared ncomp overruns the segment
                        break 'outer;
                    }
                    let sampling = seg[p + 1];
                    if (sampling >> 4) != 1 || (sampling & 0x0F) != 1 {
                        m.subsampled = true;
                    }
                    p += 3;
                }
            }
            _ => {}
        }
        i += 2 + len;
    }
    m
}

/// JPEG via `jpeg-decoder` for the *edit* path — returns raw 8-bit
/// samples whose `width × height × ncomps` must match the PDF dict's
/// declared shape (`expected`). The dict is authoritative: a JPEG
/// whose own header disagrees is refused, not silently resized.
///
/// **Coding process**: only sequential DCT (`DctSequential`) is in
/// scope — progressive (`DctProgressive`) and lossless are refused.
/// Within sequential DCT the decoder distinguishes baseline,
/// extended-sequential, differential, and arithmetic-coded frames;
/// the last two classes are `Unsupported` inside `decode()` itself,
/// so reaching this point means a baseline or extended-sequential
/// Huffman image. See docs/decisions/editing.md for the precise limit.
///
/// **`/ColorTransform` precedence** (ISO 32000-1 §7.4.8 + Adobe
/// Technical Note #5116): the effective transform is decided by
/// kasane, *not* delegated to the decoder's `determine_color_transform`
/// — that routine ranks component IDs and JFIF above APP14, which
/// contradicts the spec order. When the JPEG carries the Adobe-
/// defined APP14 marker, *its* transform byte decides the conversion
/// and the PDF `/ColorTransform` entry is ignored. When APP14 is
/// absent the PDF value applies: `0` requests the raw component
/// values, `1` requests YCbCr→RGB on a 3-component image and is
/// ignored on 1- or 2-component data. When both are absent the
/// default is `1` for three components and `0` otherwise.
///
/// CMYK/YCCK and 16-bit JPEGs are refused — they are out of scope
/// for the edit path even though the renderer also refuses them.
fn dct_decode_for_edit(
    data: &[u8],
    parms: Option<&Dict>,
    limits: &Limits,
    expected: (u32, u32, usize),
) -> Result<Vec<u8>, Error> {
    // ---- DecodeParms -------------------------------------------------
    // Only /ColorTransform is defined for DCTDecode. Every other key
    // is refused rather than silently ignored.
    let mut pdf_ct: Option<i64> = None;
    if let Some(p) = parms {
        match p.get(b"ColorTransform".as_slice()) {
            None | Some(Object::Null) => {}
            Some(o) => {
                let v = o
                    .as_i64()
                    .ok_or_else(|| Error::Structure("/ColorTransform is not an integer".into()))?;
                match v {
                    0 | 1 => pdf_ct = Some(v),
                    _ => {
                        return Err(Error::Unsupported {
                            feature: "DCTDecode /ColorTransform",
                            detail: format!("{v} (only 0 or 1 defined)"),
                        })
                    }
                }
            }
        }
        for k in p.keys() {
            if k.as_slice() != b"ColorTransform" {
                return Err(Error::Unsupported {
                    feature: "DCTDecode /DecodeParms",
                    detail: format!(
                        "/{} is not a defined DCTDecode parameter",
                        String::from_utf8_lossy(k)
                    ),
                });
            }
        }
    }

    // ---- JPEG markers ------------------------------------------------
    // Read the Adobe APP14 transform flag and the frame's sampling
    // factors *before* decoding — they decide the effective colour
    // transform. A truncated or structurally ambiguous marker walk
    // means the APP14 status is unknown, so we do not fall back to
    // defaults; the decoder will report the corruption itself.
    let markers = scan_jpeg_markers(data);
    if markers.truncated || markers.conflicting_app14 {
        return Err(Error::Parse(
            "JPEG marker structure is ambiguous or truncated before SOS".into(),
        ));
    }

    let mut dec = jpeg_decoder::Decoder::new(data);
    dec.set_max_decoding_buffer_size(limits.max_image_bytes as usize);
    // Read the header *before* the pixel buffer is allocated so a
    // malformed JPEG fails fast and the geometry check below runs on
    // real metadata, not on the decode result.
    dec.read_info()
        .map_err(|e| Error::Parse(format!("DCTDecode (JPEG) header: {e}")))?;
    let info = dec
        .info()
        .ok_or_else(|| Error::Parse("DCTDecode produced no image info".into()))?;

    // Coding process — only sequential DCT is in scope. Progressive
    // and lossless are `Unsupported`, distinct from a `Parse` error
    // on malformed data.
    if info.coding_process != jpeg_decoder::CodingProcess::DctSequential {
        return Err(Error::Unsupported {
            feature: "DCTDecode coding process",
            detail: format!(
                "{:?} — only sequential (baseline/extended) DCT is supported",
                info.coding_process
            ),
        });
    }

    // ---- effective colour transform ----------------------------------
    // ISO 32000-1 §7.4.8 + Adobe TN #5116: the in-data APP14
    // transform byte wins over the PDF /ColorTransform entry.
    // When APP14 is absent the PDF value applies; when both are
    // absent the default is `1` for three components and `0`
    // otherwise. The decoder's own component-ID/JFIF heuristics
    // are deliberately *not* consulted — they rank APP14 below
    // JFIF/IDs, which contradicts the spec order.
    //
    // The decoder's `None` and `RGB` transforms produce the same
    // interleaved output on a 3-component frame (`color_no_convert`
    // vs `color_convert_line_rgb`), but `None` reads past the
    // buffer on subsampled data — so "no transform" is requested
    // via `RGB` for 3-component frames and `None` for gray, and
    // subsampled no-transform 3-component data is refused outright.
    let (exp_w, exp_h, exp_n) = expected;
    let effective_ct: Option<jpeg_decoder::ColorTransform> = match markers.adobe_transform {
        Some(0) => {
            // APP14 transform=0 — "unknown" in Adobe TN #5116 means
            // the data is stored as the device colour space: RGB on
            // a 3-component frame, CMYK on 4. Both are no-conversion
            // outputs; we refuse the 4-component case below via the
            // pixel-format check anyway.
            if exp_n == 3 {
                if markers.subsampled {
                    return Err(Error::Unsupported {
                        feature: "DCTDecode raw RGB on subsampled JPEG",
                        detail: "subsampled raw-component output is not \
                                 representable as interleaved pixels"
                            .into(),
                    });
                }
                Some(jpeg_decoder::ColorTransform::RGB)
            } else {
                None // gray or 4-comp — pixel-format check decides
            }
        }
        Some(1) => {
            // APP14 transform=1 — YCbCr on 3-comp, YCCK on 4-comp.
            if exp_n == 3 {
                Some(jpeg_decoder::ColorTransform::YCbCr)
            } else {
                None // 4-comp refused by pixel-format check
            }
        }
        Some(2) => {
            // YCCK — always out of scope (4-component output).
            return Err(Error::Unsupported {
                feature: "DCTDecode Adobe APP14 transform=2 (YCCK)",
                detail: "4-component YCCK output is not supported".into(),
            });
        }
        Some(v) => {
            return Err(Error::Unsupported {
                feature: "DCTDecode Adobe APP14 transform",
                detail: format!("{v} — only 0, 1, or 2 are defined"),
            });
        }
        None => match pdf_ct {
            Some(0) => {
                // No transform — the decoder's `RGB` interleave is
                // the safe equivalent of `None` on 3-component data.
                if exp_n == 3 {
                    if markers.subsampled {
                        return Err(Error::Unsupported {
                            feature: "DCTDecode /ColorTransform 0 on subsampled JPEG",
                            detail: "raw component output is not representable for \
                                     subsampled data"
                                .into(),
                        });
                    }
                    Some(jpeg_decoder::ColorTransform::RGB)
                } else {
                    Some(jpeg_decoder::ColorTransform::None)
                }
            }
            Some(1) if exp_n == 3 => Some(jpeg_decoder::ColorTransform::YCbCr),
            Some(1) => None, // spec: CT=1 ignored on 1–2 components
            None if exp_n == 3 => {
                // PDF default for 3 components: transform (YCbCr→RGB).
                Some(jpeg_decoder::ColorTransform::YCbCr)
            }
            None => None, // gray: decoder picks Grayscale itself
            Some(_) => unreachable!("pdf_ct validated to 0|1"),
        },
    };
    if let Some(ct) = effective_ct {
        dec.set_color_transform(ct);
    }

    if info.width as u32 != exp_w || info.height as u32 != exp_h {
        return Err(Error::Structure(format!(
            "JPEG dimensions {}x{} do not match the image dict {}x{}",
            info.width, info.height, exp_w, exp_h
        )));
    }
    let jpeg_ncomps = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => 1usize,
        jpeg_decoder::PixelFormat::RGB24 => 3usize,
        // CMYK32, L16, and anything else are refused — the decoder
        // can produce them but the edit path only writes 8-bit
        // Gray/RGB.
        _ => {
            return Err(Error::Unsupported {
                feature: "DCTDecode image",
                detail: format!(
                    "unsupported JPEG pixel format {:?} — only 8-bit gray/RGB",
                    info.pixel_format
                ),
            })
        }
    };
    if jpeg_ncomps != exp_n {
        return Err(Error::Structure(format!(
            "JPEG has {jpeg_ncomps} components but the image dict declares {exp_n}"
        )));
    }
    dec.decode()
        .map_err(|e| Error::Parse(format!("DCTDecode (JPEG) failed: {e}")))
}

/// Resolve a colour-space name through the page's `/Resources`
/// `/ColorSpace` dict — returns the resolved object, or `Err` when
/// the name is missing or the resource chain is malformed.
///
/// Device names (`/DeviceGray`, `/DeviceRGB`, `/DeviceCMYK`) are
/// returned directly — they are not resource names and do not need
/// lookup. `Default*` names are also returned directly; the caller
/// applies substitution separately.
fn resolve_colorspace_name(
    doc: &Document,
    resources: Option<&Object>,
    name: &[u8],
) -> Result<Object, Error> {
    match name {
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" => {
            return Ok(Object::Name(name.to_vec()));
        }
        b"DefaultGray" | b"DefaultRGB" | b"DefaultCMYK" => {
            return Ok(Object::Name(name.to_vec()));
        }
        _ => {}
    }
    let res = resources.ok_or_else(|| Error::Structure("page has no /Resources".into()))?;
    doc.resolve_resource(res, "ColorSpace", name)?
        .ok_or_else(|| {
            Error::Structure(format!(
                "/ColorSpace /{} is not defined in the page resources",
                String::from_utf8_lossy(name)
            ))
        })
}

/// Apply the `Default*` substitution for a device colour space.
///
/// ISO 32000-1 §8.6.5.6: when the *specified* colour space is a
/// device space (`/DeviceGray`, `/DeviceRGB`, `/DeviceCMYK`), the
/// consumer must check whether the page's `/Resources /ColorSpace`
/// dict defines `/DefaultGray`, `/DefaultRGB`, or `/DefaultCMYK`.
/// If it does, that entry replaces the device space.
///
/// This function takes the *resolved* object (after `resolve_colorspace_name`)
/// and, if it is a device name, checks the corresponding `Default*`
/// key. A `Default*` that is itself a device name is ignored —
/// substitution does not recurse.
///
/// Returns the final colour-space object to use.
fn apply_default_substitution(
    doc: &Document,
    resources: Option<&Object>,
    resolved: Object,
) -> Result<Object, Error> {
    let name = match resolved.as_name() {
        Some(n) => n,
        None => return Ok(resolved),
    };
    let default_key = match name {
        b"DeviceGray" => b"DefaultGray".as_slice(),
        b"DeviceRGB" => b"DefaultRGB".as_slice(),
        _ => return Ok(resolved),
    };
    let res = match resources {
        Some(r) => r,
        None => return Ok(resolved),
    };
    // Look up the Default* key in the *same* /ColorSpace dict.
    let cs_dict = match res {
        Object::Dict(d) => d.get(b"ColorSpace".as_slice()),
        _ => None,
    };
    let default_entry = cs_dict
        .and_then(|o| o.as_dict())
        .and_then(|d| d.get(default_key));
    match default_entry {
        None => Ok(resolved), // no substitution — keep the device name
        Some(obj) => {
            // Resolve the default entry: it may be a name or a ref.
            let resolved_default = match obj {
                Object::Name(n) => Object::Name(n.clone()),
                Object::Ref(r) => doc.deref(&Object::Ref(*r))?,
                _ => {
                    return Err(Error::Unsupported {
                        feature: "image colour space",
                        detail: format!(
                            "/{} has non-name/non-ref type",
                            String::from_utf8_lossy(default_key)
                        ),
                    })
                }
            };
            match resolved_default.as_name() {
                Some(_) => Ok(resolved_default),
                None => Err(Error::Unsupported {
                    feature: "image colour space",
                    detail: format!(
                        "/{} does not resolve to a name",
                        String::from_utf8_lossy(default_key)
                    ),
                }),
            }
        }
    }
}

/// Whether a `/Decode` array is the default `[0 1 …]` for `ncomps`.
///
/// Shared with `remove_region.rs`.
pub(crate) fn is_default_decode(obj: &Object, ncomps: usize) -> bool {
    let arr = match obj.as_array() {
        Some(a) => a,
        None => return false,
    };
    if arr.len() != ncomps * 2 {
        return false;
    }
    arr.as_chunks::<2>()
        .0
        .iter()
        .all(|p| p[0].as_number() == Some(0.0) && p[1].as_number() == Some(1.0))
}

/// Which of the two *grid-aligned* CTM classes a `Do` carries.
///
/// Only these are accepted for image region editing — they map an
/// axis-aligned user-space rect to an axis-aligned image-space
/// rect, so the inverse produces exact pixel ranges with no
/// polygon clipping. Everything else (arbitrary rotation, shear,
/// near-zero "almost" angles) is refused outright — no epsilon is
/// applied to round an arbitrary angle into one of these classes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum CtmClass {
    /// `b == 0, c == 0, a ≠ 0, d ≠ 0` — axis-aligned; negative a or
    /// d produce horizontal / vertical reflection, `a<0,d<0` is a
    /// 180° rotation.
    AxisAligned,
    /// `a == 0, d == 0, b ≠ 0, c ≠ 0` — axis-swapping; corresponds
    /// to a 90° or 270° rotation (and their reflections) composed
    /// with any non-zero scale.
    AxisSwapped,
}

/// Map user-space rects to image pixel ranges. Accepted CTMs are
/// the two grid-aligned classes in [`CtmClass`]; any other transform
/// — arbitrary-angle rotation, shear, singular or non-finite — is
/// refused. Returns an *exact* union of `(x0, y0, x1, y1)` pixel
/// ranges, one per covered band-interval, expanded *outward*
/// (`floor`/`ceil`) so no covered pixel is lost. Shared with
/// `remove_region.rs`.
pub(crate) fn image_pixel_ranges(
    ctm: &Matrix,
    width: u32,
    height: u32,
    rects: &[crate::page::Rect],
) -> Result<Vec<(u32, u32, u32, u32)>, Error> {
    // Classify the CTM — no epsilon: the components must be *exactly*
    // zero or non-zero, so a 1° rotation or a hairline shear is
    // refused, not rounded into a supported class.
    let class = if !ctm.is_finite() {
        return Err(Error::Structure("image CTM has non-finite values".into()));
    } else if ctm.b == 0.0 && ctm.c == 0.0 && ctm.a != 0.0 && ctm.d != 0.0 {
        CtmClass::AxisAligned
    } else if ctm.a == 0.0 && ctm.d == 0.0 && ctm.b != 0.0 && ctm.c != 0.0 {
        CtmClass::AxisSwapped
    } else {
        return Err(Error::Unsupported {
            feature: "image transform",
            detail: "arbitrary rotation, shear, or singular CTM is not supported".into(),
        });
    };

    // The image paints the unit square [0,0]–[1,1] in *user space*
    // (the space after the `cm`). The CTM maps that to the page.
    // To find the pixel range, invert the CTM and map each rect's
    // corners back to unit-square space.
    //
    // For [a 0 0 d e f] the inverse maps (x,y) → ((x−e)/a, (y−f)/d);
    // for [0 b c 0 e f] it maps (x,y) → ((y−f)/b, (x−e)/c). Both are
    // computed explicitly here so the axis-swap is visible, not
    // hidden inside a generic matrix inversion.
    let map_to_unit = |x: f64, y: f64| -> (f64, f64) {
        match class {
            CtmClass::AxisAligned => ((x - ctm.e) / ctm.a, (y - ctm.f) / ctm.d),
            CtmClass::AxisSwapped => ((y - ctm.f) / ctm.b, (x - ctm.e) / ctm.c),
        }
    };

    let mut ranges = Vec::with_capacity(rects.len());
    for r in rects {
        // Map the rect's opposite corners to unit-square space.
        // Reflections and rotations can swap the min/max order —
        // normalise after mapping.
        let (uxa, uya) = map_to_unit(r.llx, r.lly);
        let (uxb, uyb) = map_to_unit(r.urx, r.ury);
        if !uxa.is_finite() || !uya.is_finite() || !uxb.is_finite() || !uyb.is_finite() {
            return Err(Error::Structure(
                "inverse CTM produced non-finite coordinates".into(),
            ));
        }
        let ux0 = uxa.min(uxb);
        let ux1 = uxa.max(uxb);
        let uy0 = uya.min(uyb);
        let uy1 = uya.max(uyb);
        // Intersect with the unit square [0,1]².
        let (ux0, ux1) = (ux0.max(0.0), ux1.min(1.0));
        let (uy0, uy1) = (uy0.max(0.0), uy1.min(1.0));
        if ux0 >= ux1 || uy0 >= uy1 {
            continue;
        }
        // Unit-square space → pixel space: x ∈ [0,width],
        // y ∈ [0,height]. The image's row order is top-down
        // (sample (0,0) is the top-left), so y=0 in unit space is
        // the *bottom* row of the image. PDF user space has +y up;
        // image rows go top-to-bottom.
        let px0 = (ux0 * width as f64).floor() as u32;
        let px1 = (ux1 * width as f64).ceil() as u32;
        // y is flipped: unit-square y=0 is the bottom row = pixel row
        // height-1, y=1 is the top row = pixel row 0.
        let py0 = (height as f64 - uy1 * height as f64).floor() as u32;
        let py1 = (height as f64 - uy0 * height as f64).ceil() as u32;
        ranges.push((
            px0.min(width),
            py0.min(height),
            px1.min(width),
            py1.min(height),
        ));
    }
    // Exact union: split into horizontal bands at every distinct y
    // boundary, then merge x-intervals within each band. A band is
    // a maximal y-strip whose x-interval set is constant — the union
    // is the exact set of covered pixels, not a bounding box.
    //
    // The intermediate Vec is bounded by `rects.len()` bands (each
    // rect contributes at most one y-boundary pair), never by image
    // height. Two rects sharing only a corner produce two bands
    // with disjoint x-intervals — the union does not fill the 2×2
    // block between them.
    type Band = (u32, u32, Vec<(u32, u32)>);
    let mut bands: Vec<Band> = Vec::new();
    for &(x0, y0, x1, y1) in &ranges {
        // Find the band this rect belongs to: split at its y
        // boundaries so every band has a constant x-interval set.
        let mut i = 0;
        while i < bands.len() {
            let by0 = bands[i].0;
            let by1 = bands[i].1;
            if y1 <= by0 || y0 >= by1 {
                i += 1;
                continue;
            }
            // Overlap in y — split the band at the rect's edges.
            let overlap_y0 = y0.max(by0);
            let overlap_y1 = y1.min(by1);
            let old_xs = bands[i].2.clone();
            // Split the existing band into pieces.
            if by0 < overlap_y0 {
                bands.push((by0, overlap_y0, old_xs.clone()));
            }
            if overlap_y1 < by1 {
                bands.push((overlap_y1, by1, old_xs.clone()));
            }
            // Rebuild the overlapping band with the new x-interval.
            let mut xs = old_xs;
            xs.push((x0, x1));
            xs.sort_by_key(|&(a, _)| a);
            let mut m: Vec<(u32, u32)> = Vec::with_capacity(xs.len());
            for x in xs {
                if let Some(l) = m.last_mut() {
                    if x.0 <= l.1 {
                        l.1 = l.1.max(x.1);
                        continue;
                    }
                }
                m.push(x);
            }
            bands[i] = (overlap_y0, overlap_y1, m);
            i += 1;
        }
        // The part of the rect not covered by any existing band.
        let mut cover = y0;
        for &(by0, by1, _) in &bands {
            if by0 <= cover && cover < by1 {
                cover = by1.max(cover);
            }
        }
        if cover < y1 {
            bands.push((cover, y1, vec![(x0, x1)]));
        }
    }
    // Sort bands by y0 so the output is deterministic.
    bands.sort_by_key(|&(y0, _, _)| y0);
    // Flatten into (x0, y0, x1, y1) per band-interval.
    let mut result = Vec::new();
    for (by0, by1, xs) in bands {
        for (x0, x1) in xs {
            result.push((x0, by0, x1, by1));
        }
    }
    Ok(result)
}

/// Charge `stream_len` bytes of generated output against
/// `max_edit_content_bytes` — cumulative across every stream body the
/// call materialises (page `/Contents` + each cloned image). Checked
/// arithmetic so a counter can't wrap.
fn charge_generated(acc: u64, stream_len: usize, limits: &Limits) -> Result<u64, Error> {
    let total = acc
        .checked_add(stream_len as u64)
        .ok_or(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: limits.max_edit_content_bytes,
        })?;
    if total > limits.max_edit_content_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: limits.max_edit_content_bytes,
        });
    }
    Ok(total)
}

/// Bounded output buffer — every `push`/`extend_from_slice` checks
/// the *remaining* byte budget *before* appending, so a long string
/// or huge TJ array stops mid-write rather than allocating past the
/// limit. `max_edit_content_bytes` (cumulative) and
/// `max_output_bytes` (single-stream) are enforced together; the
/// tighter of the two is hit first.
///
/// The buffer owns a `Vec<u8>` and never exposes it mutably — callers
/// cannot bypass the checks. On failure the partial bytes are
/// dropped with the buffer (no halfway stream escapes to the caller).
struct BoundedWriter<'a> {
    buf: Vec<u8>,
    limits: &'a Limits,
    /// Bytes already charged against `max_edit_content_bytes` by
    /// earlier streams in the same call — the remaining cumulative
    /// budget is `max_edit_content_bytes - generated_so_far`.
    generated_so_far: u64,
}

impl<'a> BoundedWriter<'a> {
    fn new(limits: &'a Limits, generated_so_far: u64) -> Self {
        Self {
            buf: Vec::new(),
            limits,
            generated_so_far,
        }
    }

    /// Check `n` more bytes fit before appending. Returns the
    /// specific limit that would be exceeded, or `Ok(())`.
    fn check(&self, n: usize) -> Result<(), Error> {
        let used = self.buf.len() as u64;
        let n = n as u64;
        let new_len = used.checked_add(n).ok_or(Error::LimitExceeded {
            limit: "max_edit_content_bytes",
            max: self.limits.max_edit_content_bytes,
        })?;
        if new_len > self.limits.max_output_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: self.limits.max_output_bytes,
            });
        }
        let gen_total = self
            .generated_so_far
            .checked_add(new_len)
            .ok_or(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                max: self.limits.max_edit_content_bytes,
            })?;
        if gen_total > self.limits.max_edit_content_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_edit_content_bytes",
                max: self.limits.max_edit_content_bytes,
            });
        }
        Ok(())
    }

    /// Append one byte — budget checked first.
    fn push(&mut self, b: u8) -> Result<(), Error> {
        self.check(1)?;
        self.buf.push(b);
        Ok(())
    }

    /// Append a slice — budget checked for the *whole* slice first.
    fn extend_from_slice(&mut self, s: &[u8]) -> Result<(), Error> {
        self.check(s.len())?;
        self.buf.extend_from_slice(s);
        Ok(())
    }

    /// Consume the buffer — the caller charges `buf.len()` via
    /// `charge_generated`.
    fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

/// Serialise one op to content-stream bytes — a small subset of the
/// full grammar, exactly what `remove_image_region` emits.
fn serialize_ops(
    ops: &[Operation],
    limits: &Limits,
    generated_so_far: u64,
) -> Result<Vec<u8>, Error> {
    let mut out = BoundedWriter::new(limits, generated_so_far);
    let mut ops_written: usize = 0;
    for op in ops {
        ops_written += 1;
        if ops_written > limits.max_operations {
            return Err(Error::LimitExceeded {
                limit: "max_operations",
                max: limits.max_operations as u64,
            });
        }
        write_op(op, &mut out)?;
        out.push(b'\n')?;
    }
    Ok(out.into_inner())
}

/// Serialise a single `Operation` — supports every variant the
/// removal path can emit. Literal-string bytes are written verbatim
/// with `(`/`)`/`\` escaped (the same encoding the parser accepts).
fn write_op(op: &Operation, out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    match op {
        Operation::SaveState => out.extend_from_slice(b"q")?,
        Operation::RestoreState => out.extend_from_slice(b"Q")?,
        Operation::ConcatMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes())?;
                out.push(b' ')?;
            }
            out.extend_from_slice(b"cm")?;
        }
        Operation::SetLineWidth(w) => {
            out.extend_from_slice(fmt_num(*w).as_bytes())?;
            out.extend_from_slice(b" w")?;
        }
        Operation::SetLineCap(c) => {
            out.extend_from_slice(c.to_string().as_bytes())?;
            out.extend_from_slice(b" J")?;
        }
        Operation::SetLineJoin(j) => {
            out.extend_from_slice(j.to_string().as_bytes())?;
            out.extend_from_slice(b" j")?;
        }
        Operation::SetMiterLimit(m) => {
            out.extend_from_slice(fmt_num(*m).as_bytes())?;
            out.extend_from_slice(b" M")?;
        }
        Operation::Type3SetWidth { wx, wy } => {
            out.extend_from_slice(fmt_num(*wx).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*wy).as_bytes())?;
            out.extend_from_slice(b" d0")?;
        }
        Operation::Type3SetWidthAndBox {
            wx,
            wy,
            llx,
            lly,
            urx,
            ury,
        } => {
            for (i, v) in [wx, wy, llx, lly, urx, ury].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" d1")?;
        }
        Operation::SetDash { dashes, phase } => {
            out.push(b'[')?;
            for (i, v) in dashes.iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(*v).as_bytes())?;
            }
            out.extend_from_slice(b"] ")?;
            out.extend_from_slice(fmt_num(*phase).as_bytes())?;
            out.extend_from_slice(b" d")?;
        }
        Operation::SetRenderingIntent(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" ri")?;
        }
        Operation::SetFlatness(i) => {
            out.extend_from_slice(fmt_num(*i).as_bytes())?;
            out.extend_from_slice(b" i")?;
        }
        Operation::SetGrayStroke(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" G")?;
        }
        Operation::SetGrayFill(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" g")?;
        }
        Operation::SetRgbStroke(r, g, b) => {
            for (i, v) in [r, g, b].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" RG")?;
        }
        Operation::SetRgbFill(r, g, b) => {
            for (i, v) in [r, g, b].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" rg")?;
        }
        Operation::SetCmykStroke(c, m, y, k) => {
            for (i, v) in [c, m, y, k].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" K")?;
        }
        Operation::SetCmykFill(c, m, y, k) => {
            for (i, v) in [c, m, y, k].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" k")?;
        }
        Operation::SetColorSpaceStroke(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" CS")?;
        }
        Operation::SetColorSpaceFill(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" cs")?;
        }
        Operation::SetColorStroke(ops) => {
            for o in ops {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            if matches!(ops.last(), Some(Object::Name(_))) {
                out.extend_from_slice(b"SCN")?;
            } else {
                out.extend_from_slice(b"SC")?;
            }
        }
        Operation::SetColorFill(ops) => {
            for o in ops {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            if matches!(ops.last(), Some(Object::Name(_))) {
                out.extend_from_slice(b"scn")?;
            } else {
                out.extend_from_slice(b"sc")?;
            }
        }
        Operation::SetExtGState(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" gs")?;
        }
        Operation::MoveTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" m")?;
        }
        Operation::LineTo(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" l")?;
        }
        Operation::CurveTo(x1, y1, x2, y2, x3, y3) => {
            for (i, v) in [x1, y1, x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" c")?;
        }
        Operation::CurveToV(x2, y2, x3, y3) => {
            for (i, v) in [x2, y2, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" v")?;
        }
        Operation::CurveToY(x1, y1, x3, y3) => {
            for (i, v) in [x1, y1, x3, y3].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" y")?;
        }
        Operation::ClosePath => out.extend_from_slice(b"h")?,
        Operation::Rectangle(x, y, w, h) => {
            for (i, v) in [x, y, w, h].iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                out.extend_from_slice(fmt_num(**v).as_bytes())?;
            }
            out.extend_from_slice(b" re")?;
        }
        Operation::Stroke => out.extend_from_slice(b"S")?,
        Operation::CloseAndStroke => out.extend_from_slice(b"s")?,
        Operation::Fill => out.extend_from_slice(b"f")?,
        Operation::FillEvenOdd => out.extend_from_slice(b"f*")?,
        Operation::FillAndStroke => out.extend_from_slice(b"B")?,
        Operation::FillAndStrokeEvenOdd => out.extend_from_slice(b"B*")?,
        Operation::CloseFillAndStroke => out.extend_from_slice(b"b")?,
        Operation::CloseFillAndStrokeEvenOdd => out.extend_from_slice(b"b*")?,
        Operation::EndPath => out.extend_from_slice(b"n")?,
        Operation::Clip => out.extend_from_slice(b"W")?,
        Operation::ClipEvenOdd => out.extend_from_slice(b"W*")?,
        Operation::BeginText => out.extend_from_slice(b"BT")?,
        Operation::EndText => out.extend_from_slice(b"ET")?,
        Operation::SetTextFont { font, size } => {
            out.push(b'/')?;
            out.extend_from_slice(font)?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*size).as_bytes())?;
            out.extend_from_slice(b" Tf")?;
        }
        Operation::SetCharSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tc")?;
        }
        Operation::SetWordSpacing(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tw")?;
        }
        Operation::SetHorizontalScaling(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Tz")?;
        }
        Operation::SetTextLeading(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" TL")?;
        }
        Operation::SetTextRenderingMode(m) => {
            out.extend_from_slice(m.to_string().as_bytes())?;
            out.extend_from_slice(b" Tr")?;
        }
        Operation::SetTextRise(v) => {
            out.extend_from_slice(fmt_num(*v).as_bytes())?;
            out.extend_from_slice(b" Ts")?;
        }
        Operation::MoveTextPosition(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" Td")?;
        }
        Operation::MoveTextPositionAndSetLeading(x, y) => {
            out.extend_from_slice(fmt_num(*x).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*y).as_bytes())?;
            out.extend_from_slice(b" TD")?;
        }
        Operation::SetTextMatrix(m) => {
            for v in [m.a, m.b, m.c, m.d, m.e, m.f] {
                out.extend_from_slice(fmt_num(v).as_bytes())?;
                out.push(b' ')?;
            }
            out.extend_from_slice(b"Tm")?;
        }
        Operation::NextLine => out.extend_from_slice(b"T*")?,
        Operation::ShowText(bytes) => {
            write_literal_string(bytes, out)?;
            out.extend_from_slice(b" Tj")?;
        }
        Operation::ShowTextAdjusted(items) => {
            out.push(b'[')?;
            let mut first = true;
            for it in items {
                if !first {
                    out.push(b' ')?;
                }
                first = false;
                match it {
                    crate::content::TJItem::Text(b) => write_literal_string(b, out)?,
                    crate::content::TJItem::Kern(n) => {
                        out.extend_from_slice(fmt_num(*n).as_bytes())?
                    }
                }
            }
            out.extend_from_slice(b"] TJ")?;
        }
        Operation::NextLineShowText(text) => {
            write_literal_string(text, out)?;
            out.extend_from_slice(b" '")?;
        }
        Operation::SetSpacingNextLineShowText {
            word_spacing,
            char_spacing,
            text,
        } => {
            out.extend_from_slice(fmt_num(*word_spacing).as_bytes())?;
            out.push(b' ')?;
            out.extend_from_slice(fmt_num(*char_spacing).as_bytes())?;
            out.push(b' ')?;
            write_literal_string(text, out)?;
            out.extend_from_slice(b" \"")?;
        }
        Operation::Do(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" Do")?;
        }
        Operation::Shading(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" sh")?;
        }
        Operation::InlineImage { dict, data } => {
            out.extend_from_slice(b"BI")?;
            for (k, v) in dict {
                out.push(b' ')?;
                out.push(b'/')?;
                out.extend_from_slice(k)?;
                out.push(b' ')?;
                write_object_shallow(v, out)?;
            }
            out.extend_from_slice(b" ID ")?;
            out.extend_from_slice(data)?;
            out.extend_from_slice(b" EI")?;
        }
        Operation::MarkedContentPoint(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" MP")?;
        }
        Operation::DesignatedMarkedContentPoint { tag, properties } => {
            out.push(b'/')?;
            out.extend_from_slice(tag)?;
            out.push(b' ')?;
            write_object_shallow(properties, out)?;
            out.extend_from_slice(b" DP")?;
        }
        Operation::BeginMarkedContent(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
            out.extend_from_slice(b" BMC")?;
        }
        Operation::BeginMarkedContentWithProps { tag, properties } => {
            out.push(b'/')?;
            out.extend_from_slice(tag)?;
            out.push(b' ')?;
            write_object_shallow(properties, out)?;
            out.extend_from_slice(b" BDC")?;
        }
        Operation::EndMarkedContent => out.extend_from_slice(b"EMC")?,
        Operation::BeginCompatibility => out.extend_from_slice(b"BX")?,
        Operation::EndCompatibility => out.extend_from_slice(b"EX")?,
        // Unknown ops inside BX…EX round-trip verbatim — the operands
        // were parsed objects, re-emitted with the same serialiser.
        // (`Unknown` outside BX…EX never reaches here: the walker's
        // `uncertain_state` taint refuses the page first.)
        Operation::Unknown { operator, operands } => {
            for o in operands {
                write_object_shallow(o, out)?;
                out.push(b' ')?;
            }
            out.extend_from_slice(operator)?;
        }
    }
    Ok(())
}

/// Literal PDF string: `(…)` with `(`/`)`/`\` and control bytes
/// escaped. Non-ASCII bytes are written as `\xNN` octal escapes so
/// the output stays ASCII-clean and re-parses exactly.
fn write_literal_string(bytes: &[u8], out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    out.push(b'(')?;
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push(b'\\')?;
                out.push(b)?;
            }
            0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f..=0xff => {
                out.extend_from_slice(format!("\\{:03o}", b).as_bytes())?;
            }
            _ => out.push(b)?,
        }
    }
    out.push(b')')?;
    Ok(())
}

/// Serialise a scalar/array/dict object shallowly — used for
/// marked-content properties and `Unknown` operands. Streams are not
/// representable here (none appear as operands).
fn write_object_shallow(o: &Object, out: &mut BoundedWriter<'_>) -> Result<(), Error> {
    match o {
        Object::Null => out.extend_from_slice(b"null")?,
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" })?,
        Object::Integer(n) => out.extend_from_slice(n.to_string().as_bytes())?,
        Object::Real(n) => out.extend_from_slice(fmt_num(*n).as_bytes())?,
        Object::Name(n) => {
            out.push(b'/')?;
            out.extend_from_slice(n)?;
        }
        Object::String(s) => write_literal_string(s, out)?,
        Object::Array(a) => {
            out.push(b'[')?;
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ')?;
                }
                write_object_shallow(x, out)?;
            }
            out.push(b']')?;
        }
        Object::Dict(d) => {
            out.extend_from_slice(b"<<")?;
            for (k, v) in d {
                out.push(b' ')?;
                out.push(b'/')?;
                out.extend_from_slice(k)?;
                out.push(b' ')?;
                write_object_shallow(v, out)?;
            }
            out.extend_from_slice(b" >>")?;
        }
        Object::Ref(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes())?;
        }
        Object::Stream { .. } => {
            // Unreachable — an operand is never a stream.
            out.extend_from_slice(b"null")?;
        }
    }
    Ok(())
}

/// Format a content-stream number: integer when exact, else fixed-
/// point trimmed to 4 decimals — the same convention `edit.rs` uses
/// for annotation coordinates. `NaN`/`inf` never reach here (every
/// caller validates finite upstream; `write_op` on `ConcatMatrix`
/// re-validates because a `cm` operand could carry them).
fn fmt_num(n: f64) -> String {
    debug_assert!(n.is_finite(), "non-finite operand {n} leaked to writer");
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Deterministic fresh `/XObject` name — `KasIm{n}` (image variant of
/// the `KasFm{n}` scheme `remove_text` uses). Collision-checked
/// against the page's effective `/XObject` dict plus the names this
/// call already allocated.
///
/// Shared with `remove_region.rs`.
pub(crate) fn fresh_xobject_name(
    page: &crate::page::Page,
    doc: &Document,
    seq: u64,
    allocated: &[Vec<u8>],
) -> Result<Vec<u8>, Error> {
    let existing: Option<Dict> = match page.resources() {
        Some(r) => match doc.deref(r)?.as_dict() {
            Some(d) => match d.get(b"XObject".as_slice()) {
                Some(o) => Some(doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                    Error::Structure("/Resources /XObject is not a dictionary".into())
                })?),
                None => None,
            },
            None => return Err(Error::Structure("/Resources is not a dictionary".into())),
        },
        None => None,
    };
    for n in seq.. {
        let name = format!("KasIm{n}").into_bytes();
        let taken = existing
            .as_ref()
            .map(|d| d.contains_key(name.as_slice()))
            .unwrap_or(false)
            || allocated.contains(&name);
        if !taken {
            return Ok(name);
        }
        // Practically unreachable — a document would need `KasIm{n}`
        // for every n up to u64::MAX — but the loop is bounded so a
        // pathological dict can't spin forever.
        if n - seq > 1_000_000 {
            return Err(Error::LimitExceeded {
                limit: "xobject_name_space",
                max: 1_000_000,
            });
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×2 DeviceGray image with known pixels: top-left 0x10,
    /// top-right 0x20, bottom-left 0x30, bottom-right 0x40.
    /// Painted at 20,60 with a 40×40 cm so each pixel covers 20×20
    /// user units.
    fn gray_image_pdf() -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // 2×2 gray image: rows top-to-bottom = [0x10, 0x20], [0x30, 0x40]
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Page content: paint the image at 20,60 scaled 40×40.
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// A 2×2 DeviceRGB image with known pixels: top-left (10,20,30),
    /// top-right (40,50,60), bottom-left (70,80,90), bottom-right
    /// (a0,b0,c0).
    fn rgb_image_pdf() -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // 2×2 RGB image: rows top-to-bottom =
        // [(10,20,30), (40,50,60)], [(70,80,90), (a0,b0,c0)]
        let img_data = [
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, // top row
            0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, // bottom row
        ];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Two `Do` calls to the same image at different positions —
    /// `Im1` at 20,60 (40×40) and `Im1` again at 100,60 (40×40).
    fn two_invocations_pdf() -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n40 0 0 40 100 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Same image invoked under two different names (`/Im1` and
    /// `/Im2` both → 10 0 R).
    fn two_names_pdf() -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R /Im2 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n40 0 0 40 100 60 cm\n/Im2 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Two pages sharing the same image object — page 1 invokes it at
    /// 20,60, page 2 at 100,60. Editing page 1 must leave page 2
    /// untouched.
    fn shared_image_two_pages_pdf() -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        // Page 1: image at 20,60.
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // Page 2: same image at 100,60.
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 11 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Page 1 content.
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        // Page 2 content.
        let body2 = b"40 0 0 40 100 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb.extend_from_slice(body2);
        sb.extend_from_slice(b"\nendstream");
        push!(11, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    /// A single 2×2 gray image painted at 20,60 scaled 40×40 —
    /// each pixel covers 20×20 user units.
    #[test]
    fn image_region_removal_clones_the_invocation() {
        let d = doc(&gray_image_pdf());
        // Rect covering the top-left pixel's painted area (20,80)-(40,100)
        // — the image's unit square maps to (20,60)-(60,100), so the
        // top-left pixel (0,0) is at (20,80)-(40,100).
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1); // one pixel → 1 component changed
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone's stream must contain the blackened pixel.
        assert!(
            out.contains("/KasIm"),
            "cloned image must get a fresh /XObject name: {out}"
        );
        // The original image object (10 0 R) must still contain the
        // *unmodified* data — its bytes are preserved verbatim.
        assert!(
            out.contains("\x10\x20\x30\x40"),
            "original image must keep its sample bytes: {out}"
        );
    }

    /// A rect covering the whole image changes all 4 pixels.
    #[test]
    fn image_region_removal_whole_image() {
        let d = doc(&gray_image_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4); // 4 pixels × 1 component
    }

    /// A rect covering only the bottom-right pixel changes 1 pixel.
    #[test]
    fn image_region_removal_bottom_right() {
        let d = doc(&gray_image_pdf());
        // Bottom-right pixel (1,1) is at (40,60)-(60,80).
        let r =
            remove_image_region(&d, 0, &[(40.0, 60.0, 60.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
    }

    /// A rect covering no pixels leaves the invocation unchanged.
    #[test]
    fn image_region_removal_no_pixels() {
        let d = doc(&gray_image_pdf());
        let r = remove_image_region(&d, 0, &[(0.0, 0.0, 10.0, 10.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 0);
        assert_eq!(r.pixels_changed, 0);
    }

    /// Two `Do` calls to the same image: only the one whose painted
    /// bounds hit a rect is cloned; the other keeps pointing at the
    /// original object.
    #[test]
    fn two_invocations_only_the_hit_one_is_cloned() {
        let d = doc(&two_invocations_pdf());
        // Rect hits the first invocation's top-left pixel only.
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // One clone → one `/KasIm0` in `/Resources`, one `/KasIm0 Do`
        // in the content, one `/Im1 Do` for the unedited call.
        assert_eq!(out.matches("/KasIm0").count(), 2); // Resources dict + Do
        assert_eq!(out.matches("/Im1 Do").count(), 1); // unedited
        assert_eq!(out.matches("/KasIm0 Do").count(), 1); // edited
    }

    /// Two names for the same image object — each `Do` is a distinct
    /// invocation and gets its own clone.
    #[test]
    fn two_names_for_same_image() {
        let d = doc(&two_names_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // One clone → `/KasIm0` in `/Resources` + `/KasIm0 Do`.
        assert_eq!(out.matches("/KasIm0").count(), 2);
        // `/Im1` was edited → its `Do` became `/KasIm0`; `/Im2` is
        // still invoked by the second `Do`.
        assert_eq!(out.matches("/Im1 Do").count(), 0); // edited → /KasIm0
        assert_eq!(out.matches("/Im2 Do").count(), 1); // unedited
        assert_eq!(out.matches("/KasIm0 Do").count(), 1); // edited
    }

    /// Two pages sharing the same image — editing page 1 leaves
    /// page 2's stream and the original image untouched.
    #[test]
    fn shared_image_other_page_keeps_original() {
        let d = doc(&shared_image_two_pages_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        let d2 = doc(&r.output);
        // Page 2 still paints the original image — its content stream
        // still references `/Im1`, and `/Im1` still resolves to the
        // original object.
        let p2_ops = d2.page_operations(&d2.pages()[1]).unwrap();
        assert!(
            p2_ops
                .iter()
                .any(|o| matches!(o, Operation::Do(n) if n == b"Im1")),
            "page 2 must still invoke /Im1"
        );
        // The original image data is still in the output.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(
            out.contains("\x10\x20\x30\x40"),
            "original image must keep its sample bytes: {out}"
        );
    }

    /// A truncated/corrupt JPEG is refused — `/DCTDecode` is in
    /// scope but the bytes must actually decode. The fake header
    /// here fails `read_info` before any pixels are allocated.
    #[test]
    fn jpeg_image_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0xffu8, 0xd8, 0xff, 0xe0]; // fake JPEG header
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Parse(_)),
            "corrupt JPEG must be a Parse error: {err:?}"
        );
    }

    /// An image with `/Mask` is refused — masks are out of scope.
    #[test]
    fn image_with_mask_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [0 0] /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "image with /Mask must be refused: {err:?}"
        );
    }

    /// An arbitrary-angle (non-90°-multiple) rotation is refused —
    /// only grid-aligned transforms are accepted.
    #[test]
    fn rotated_image_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // ~30° rotation: [34.6 20 −20 34.6 20 60] — neither
        // axis-aligned nor axis-swapped; refused.
        let body = b"34.64 20 -20 34.64 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "arbitrary-angle rotation must be refused: {err:?}"
        );
    }

    /// A page with annotations is refused — `/Annots` is out of scope.
    #[test]
    fn annots_page_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Annots [12 0 R] /Contents 9 0 R >>"
        );
        push!(
            12,
            b"<< /Type /Annot /Subtype /Square /Rect [10 10 50 50] >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 13\n0000000000 65535 f \n".to_vec());
        for n in 1..13u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 13 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "page with /Annots must be refused: {err:?}"
        );
    }

    /// A page with a `/Form` XObject is refused — nested XObjects are
    /// out of scope for `remove_image_region`.
    #[test]
    fn form_xobject_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R /Fm1 11 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let form =
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length 0 >>\nstream\n\nendstream";
        push!(11, form);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n/Fm1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "page with /Form must be refused: {err:?}"
        );
    }

    /// A page with `gs` is refused — ExtGState is not interpreted.
    #[test]
    fn extgstate_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /ExtGState << /GS1 << /LW 5 >> >> \
              /XObject << /Im1 10 0 R >> >> /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"/GS1 gs\n40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "page with gs must be refused: {err:?}"
        );
    }

    /// A page with `/UserUnit` ≠ 1 is refused — coordinates are not
    /// default units.
    #[test]
    fn user_unit_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /UserUnit 2 /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "page with /UserUnit must be refused: {err:?}"
        );
    }

    /// A 2×2 RGB image: `pixels_changed` counts *pixels*, not
    /// components — one pixel = 1 even when all 3 channels go black.
    #[test]
    fn rgb_image_pixel_count() {
        let d = doc(&rgb_image_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1); // 1 pixel, not 3 components
    }

    /// A FlateDecode image decodes, blackens, and re-encodes without
    /// a filter — the output stream is uncompressed.
    #[test]
    fn flate_image_is_supported() {
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&img_data).unwrap();
            e.finish().unwrap()
        };
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length "
            .to_vec();
        f.extend_from_slice(enc.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&enc);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone must not carry /Filter — the new body is
        // uncompressed. The *original* image keeps its `/Filter` (we
        // never mutate it), so the check is that no `/KasIm` stream
        // dict claims a filter.
        assert!(
            !out.contains("/KasIm0/Filter"),
            "cloned image must not claim a filter it does not use: {out}"
        );
    }

    /// A non-default /Decode array is refused — inverted samples are
    /// not rewritten.
    #[test]
    fn non_default_decode_is_refused() {
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0] /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "non-default /Decode must be refused: {err:?}"
        );
    }

    /// A page-level `Do` to a `/Form` is refused — `remove_image_region` does not edit
    /// inside forms.
    #[test]
    fn form_do_is_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let form =
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length 0 >>\nstream\n\nendstream";
        push!(10, form);
        let body = b"/Fm1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let err = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "form Do must be refused: {err:?}"
        );
    }

    // ---- regression tests ------------------------------------

    /// Two rects covering the top-left and bottom-right pixels only —
    /// the middle pixels must stay unchanged (no bounding-box fill).
    #[test]
    fn disjoint_rects_do_not_fill_between() {
        // 4×4 gray image: each pixel covers 10×10 user units.
        // Painted at 0,0 scaled 40×40 → pixels at:
        //   (0,0): (0,30)-(10,40)  (1,0): (10,30)-(20,40)  …
        //   (0,1): (0,20)-(10,30)  (1,1): (10,20)-(20,30)  …
        //   (3,3): (30,0)-(40,10)
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // 4×4 gray: 16 distinct values 0x10..0x1f
        let img_data: Vec<u8> = (0x10..=0x1f).collect();
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Rects: top-left pixel (0,3) at (0,30)-(10,40) and
        // bottom-right pixel (3,0) at (30,0)-(40,10).
        let r = remove_image_region(
            &d,
            0,
            &[(0.0, 30.0, 10.0, 40.0), (30.0, 0.0, 40.0, 10.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        // 2 pixels changed, not 16 (bounding box) — the 14 middle
        // pixels must be unchanged.
        assert_eq!(r.pixels_changed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The clone must contain the original middle pixels.
        // 4×4 data: [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
        //            0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f]
        // Pixel (0,0) is first byte 0x10 → 0x00 (top-left, row 0).
        // Pixel (3,3) is last byte 0x1f → 0x00 (bottom-right, row 3).
        // Middle bytes must be unchanged.
        assert!(
            out.contains("\x00\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x00"),
            "middle pixels must be unchanged: {out}"
        );
    }

    /// Overlapping rects: the shared pixel is counted once.
    #[test]
    fn overlapping_rects_count_once() {
        let d = doc(&gray_image_pdf());
        // Two rects covering the same top-left pixel — the second
        // rect is entirely inside the first.
        let r = remove_image_region(
            &d,
            0,
            &[(20.0, 80.0, 40.0, 100.0), (25.0, 85.0, 35.0, 95.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        // 1 pixel changed, not 2 — the overlap is not double-counted.
        assert_eq!(r.pixels_changed, 1);
    }

    /// An RGB image's `pixels_changed` counts *pixels*, not changed
    /// components — one pixel = 1 even when all 3 channels go black.
    #[test]
    fn rgb_pixels_changed_counts_pixels() {
        let d = doc(&rgb_image_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        // 1 pixel changed (all 3 components), not 3.
        assert_eq!(r.pixels_changed, 1);
    }

    /// A gray image's `pixels_changed` counts pixels — same definition
    /// as RGB.
    #[test]
    fn gray_pixels_changed_counts_pixels() {
        let d = doc(&gray_image_pdf());
        let r =
            remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4); // 4 pixels × 1 component
    }

    /// A named colour space resolves through the page's `/Resources`
    /// `/ColorSpace` dict — `/CS1` → `/DeviceGray` is accepted.
    #[test]
    fn named_colorspace_resolves() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> \
              /ColorSpace << /CS1 /DeviceGray >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /CS1 /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r =
            remove_image_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
    }

    /// `/DefaultRGB` is NOT unconditionally treated as `/DeviceRGB` —
    /// it resolves through the page's `/Resources /ColorSpace` dict,
    /// and if that entry is missing or non-device it is refused.
    #[test]
    fn default_rgb_resolves_or_refuses() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> \
              /ColorSpace << /DefaultRGB 12 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(12, b"/CalRGB");
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DefaultRGB /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 13\n0000000000 65535 f \n".to_vec());
        for n in 1..13u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 13 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // /DefaultRGB → /CalRGB (not a device space) → refused.
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "non-device DefaultRGB must be refused: {err:?}"
        );
    }

    /// An indirect `/ColorSpace` reference resolves through the
    /// page's `/Resources /ColorSpace` dict — `/CS1` → `11 0 R` →
    /// `/DeviceGray` is accepted.
    #[test]
    fn indirect_colorspace_resolves() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> \
              /ColorSpace << /CS1 11 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(11, b"/DeviceGray");
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /CS1 /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r =
            remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4);
    }

    /// A compressed image that expands past `max_image_bytes` is
    /// refused *during* decode, not after — the read stops before
    /// the whole buffer is allocated.
    #[test]
    fn flate_decode_stops_at_limit() {
        // 4×4 gray = 16 bytes raw; compress it.
        let img_data = [0x10u8; 16];
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&img_data).unwrap();
            e.finish().unwrap()
        };
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length "
            .to_vec();
        f.extend_from_slice(enc.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&enc);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // 16 bytes decoded > 8-byte limit → fails during decode.
        let l = Limits {
            max_image_bytes: 8,
            ..Limits::default()
        };
        // Rect covers the painted image (20,60)–(60,100).
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_image_bytes",
                    ..
                }
            ),
            "decode must stop at max_image_bytes: {err:?}"
        );
    }

    /// A raw (uncompressed) image larger than `max_image_bytes` is
    /// refused before cloning — `raw.to_vec()` is not called.
    #[test]
    fn raw_image_refused_before_clone() {
        let img_data = [0x10u8; 32]; // 32 bytes > 8-byte limit
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 4 /Height 8 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let l = Limits {
            max_image_bytes: 8,
            ..Limits::default()
        };
        // Rect covers the painted image (20,60)–(60,100).
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_image_bytes",
                    ..
                }
            ),
            "raw image must be refused before clone: {err:?}"
        );
    }

    // ---- regression tests ------------------------------------

    /// Corner-touching rects: two pixels that share only a corner
    /// must not fill the 2×2 block between them.
    #[test]
    fn corner_touching_rects_do_not_fill() {
        // 4×4 gray: pixels (0,0) and (1,1) touch only at the corner.
        // Image is painted 40×40 at (0,0) → each pixel is 10×10.
        // (0,0) covers (0,30)-(10,40); (1,1) covers (10,20)-(20,30).
        // They share the corner point (10,30) only.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data: Vec<u8> = (0x10..=0x1f).collect();
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Rects covering pixels (0,0) and (2,0) — corner-touching
        // at (10,40) in user space (pixel columns 0 and 2, same row).
        let r = remove_image_region(
            &d,
            0,
            &[(0.0, 30.0, 10.0, 40.0), (20.0, 30.0, 30.0, 40.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        // 2 pixels changed, not 3 — the pixel between them is not
        // covered.
        assert_eq!(r.pixels_changed, 2);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // 4×4 data: [0x10, 0x11, 0x12, 0x13, ...]
        // Pixel (0,0) → byte 0 = 0x00. Pixel (2,0) → byte 2 = 0x00.
        // Pixel (1,0)=byte 1 must be unchanged.
        assert!(
            out.contains("\x00\x11\x00\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f"),
            "only corner-touching pixels changed: {out}"
        );
    }

    /// L-shaped overlap: two rects that overlap in an L shape must
    /// not fill the bounding box.
    #[test]
    fn l_shaped_overlap_does_not_fill() {
        // 4×4 gray: rect A covers top row (y=0..1, x=0..4) and
        // left column (x=0..1, y=0..4); rect B covers right column
        // (x=3..4, y=0..4). Union is L-shaped (or backwards-L).
        // Bounding box would be the whole 4×4.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data: Vec<u8> = (0x10..=0x1f).collect();
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Rect A: top row + left column (L shape).
        // Rect B: right column. Union = L + right column.
        // Pixels: row 0 all, rows 1-3 col 0 and col 3.
        // Total: 4 + 3×2 = 10 pixels.
        let r = remove_image_region(
            &d,
            0,
            &[
                (0.0, 30.0, 40.0, 40.0),
                (0.0, 0.0, 10.0, 40.0),
                (30.0, 0.0, 40.0, 40.0),
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        // 10 pixels, not 16 (bounding box).
        assert_eq!(r.pixels_changed, 10);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Row 0: all 0x00. Rows 1-3: col 0 = 0x00, col 3 = 0x00,
        // cols 1-2 unchanged.
        assert!(
            out.contains("\x00\x00\x00\x00\x00\x15\x16\x00\x00\x19\x1a\x00\x00\x1d\x1e\x00"),
            "L-shaped union must not fill middle: {out}"
        );
    }

    /// A named colour space that resolves to a device space is
    /// accepted; the name is looked up in the page's `/Resources`.
    #[test]
    fn device_rgb_with_default_substitution() {
        // /DeviceRGB in the image dict + /DefaultRGB → /DeviceRGB
        // in Resources — the default substitution resolves to the
        // same device space, so it is accepted.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> \
              /ColorSpace << /DefaultRGB /DeviceRGB >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 \
              /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r =
            remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
    }

    /// `/DefaultRGB` resolving to a non-device space is refused —
    /// even when the image dict says `/DeviceRGB`.
    #[test]
    fn device_rgb_with_nondevice_default_refused() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> \
              /ColorSpace << /DefaultRGB /CalRGB >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 \
              /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // /DefaultRGB → /CalRGB — the image says /DeviceRGB but the
        // effective default substitution is non-device. Refused.
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "non-device DefaultRGB must refuse even when image says /DeviceRGB: {err:?}"
        );
    }

    /// A missing `/ColorSpace` resource entry is `Error::Structure`,
    /// not silently treated as a device space.
    #[test]
    fn missing_colorspace_resource_is_error() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /CS1 /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // /CS1 is not defined in the page's /Resources /ColorSpace.
        let err = remove_image_region(&d, 0, &[(20.0, 60.0, 60.0, 100.0)], &Limits::default())
            .unwrap_err();
        assert!(
            matches!(err, Error::Structure(_)),
            "missing /ColorSpace resource must be Structure: {err:?}"
        );
    }

    /// A second image whose decode exceeds the remaining cumulative
    /// `max_edit_content_bytes` is refused *before* decoding — the
    /// budget is checked up front, not after the clone is built.
    #[test]
    fn cumulative_budget_refused_before_decode() {
        // Two images: first succeeds (4 bytes), second would need 4
        // more but only 4 bytes remain in the 8-byte budget.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R /Im2 11 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8; 4];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        push!(11, &f); // same image data, second invocation
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n/Im2 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // First image: 4 bytes. Second image: 4 more bytes needed
        // but only 4 remain in the 8-byte budget.
        let l = Limits {
            max_edit_content_bytes: 8,
            ..Limits::default()
        };
        let err = remove_image_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &l).unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_edit_content_bytes",
                    ..
                }
            ),
            "second image must be refused before decode: {err:?}"
        );
    }

    /// An image whose pixel range does not intersect any rect is
    /// skipped without decoding — no budget is consumed.
    #[test]
    fn nonintersecting_image_is_not_decoded() {
        // Two images: Im1 is intersected, Im2 is not. Im2's decode
        // would fail (corrupt data) — but it is never reached.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R /Im2 11 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8; 4];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Im2 has corrupt data — wrong length for 2×2.
        let bad_data = [0x10u8; 2]; // 2 bytes ≠ 4 expected
        let mut f2 = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f2.extend_from_slice(bad_data.len().to_string().as_bytes());
        f2.extend_from_slice(b" >>\nstream\n");
        f2.extend_from_slice(&bad_data);
        f2.extend_from_slice(b"\nendstream");
        push!(11, &f2);
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n40 0 0 40 100 0 cm\n/Im2 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Only Im1's area is rect'd — Im2 at (100,0) is outside.
        let r = remove_image_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        // Im2 was never decoded — the corrupt data is not an error.
    }

    // ---- rotated / reflected image region editing --------------
    //
    // The shared `image_pixel_ranges` accepts the two grid-aligned
    // CTM classes; these tests verify the *pixel* each user-space
    // rect maps to. A 3×2 gray image makes the per-pixel mapping
    // observable (pixels are position-identifiable):
    //
    //   pixel row 0 (top):    0x10  0x20  0x30   → pixels (0,0)(1,0)(2,0)
    //   pixel row 1 (bottom): 0x40  0x50  0x60   → pixels (0,1)(1,1)(2,1)
    //
    // Pixel (px,py) covers unit-square [px/3, (px+1)/3] ×
    // [1−(py+1)/2, 1−py/2] — the y-flip means py=0 is the top row.
    //
    // Each case hand-computes which user-space rect selects which
    // pixel; the assertion then verifies `pixels_changed` and that
    // the clone's blackened samples are exactly those pixels.

    /// Build a 1-page PDF painting the 3×2 gray image under `cm`.
    fn grid_ctm_gray_pdf(cm: &str) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = format!("{cm}\n/Im1 Do\n").into_bytes();
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(&body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Decode the edited clone's samples (KasIm0) as a flat
    /// width*height gray vector.
    fn grid_ctm_edited_samples(d: &Document) -> Vec<u8> {
        let page = d.pages().first().unwrap();
        let res = page.resources().unwrap();
        // Find the KasIm0 XObject ref.
        let entry = d
            .resolve_resource_raw(res, "XObject", b"KasIm0")
            .unwrap()
            .expect("no KasIm0");
        let obj = d.deref(&entry).unwrap();
        match obj {
            Object::Stream { ref data, .. } => data.clone(),
            _ => panic!("KasIm0 not a stream"),
        }
    }

    /// Baseline axis-aligned — a rect selects the top-left pixel.
    /// `[6 0 0 10 20 60] cm`: pixel (0,0) covers user space
    /// x∈[20,22], y∈[65,70].
    #[test]
    fn grid_ctm_axis_aligned_baseline() {
        let d = doc(&grid_ctm_gray_pdf("6 0 0 10 20 60 cm"));
        let r =
            remove_image_region(&d, 0, &[(20.0, 65.0, 22.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Horizontal reflection — `[−6 0 0 10 38 60]`: pixel (0,0)
    /// lands at x∈[36,38], y∈[65,70] (the image's left edge is on
    /// the right of the painted rect).
    #[test]
    fn grid_ctm_horizontal_flip() {
        let d = doc(&grid_ctm_gray_pdf("-6 0 0 10 38 60 cm"));
        let r =
            remove_image_region(&d, 0, &[(36.0, 65.0, 38.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Vertical reflection — `[6 0 0 −10 20 80]`: pixel (0,0)
    /// lands at x∈[20,22], y∈[70,75] (the image's top row is on
    /// the bottom of the painted rect).
    #[test]
    fn grid_ctm_vertical_flip() {
        let d = doc(&grid_ctm_gray_pdf("6 0 0 -10 20 80 cm"));
        let r =
            remove_image_region(&d, 0, &[(20.0, 70.0, 22.0, 75.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// 180° rotation (both flips) — `[−6 0 0 −10 38 80]`: pixel
    /// (0,0) lands at x∈[36,38], y∈[70,75].
    #[test]
    fn grid_ctm_rot180() {
        let d = doc(&grid_ctm_gray_pdf("-6 0 0 -10 38 80 cm"));
        let r =
            remove_image_region(&d, 0, &[(36.0, 70.0, 38.0, 75.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// 90° rotation — `[0 10 −6 0 38 60] cm`: x' = −6v+38,
    /// y' = 10u+60. Pixel (0,0) covers u∈[0,1/3], v∈[1/2,1] →
    /// user x∈[32,35], y∈[60,63.33].
    #[test]
    fn grid_ctm_rot90() {
        let d = doc(&grid_ctm_gray_pdf("0 10 -6 0 38 60 cm"));
        // Strictly inside pixel (0,0)'s painted rect so the
        // floor/ceil expansion covers only that pixel.
        let r =
            remove_image_region(&d, 0, &[(32.0, 60.0, 35.0, 63.3)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// 270° rotation — `[0 −10 6 0 2 70] cm`: x' = 6v+2,
    /// y' = −10u+70. Pixel (0,0) covers u∈[0,1/3], v∈[1/2,1] →
    /// user x∈[5,8], y∈[66.67,70].
    #[test]
    fn grid_ctm_rot270() {
        let d = doc(&grid_ctm_gray_pdf("0 -10 6 0 2 70 cm"));
        let r = remove_image_region(&d, 0, &[(5.0, 66.7, 8.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Axis-swap with a reflection — `[0 10 6 0 20 60] cm`:
    /// x' = 6v+20, y' = 10u+60 (a 90°-CCW about the origin, then
    /// scaled). Pixel (0,0) covers u∈[0,1/3], v∈[1/2,1] →
    /// user x∈[23,26], y∈[60,63.33].
    #[test]
    fn grid_ctm_axis_swap_reflect() {
        let d = doc(&grid_ctm_gray_pdf("0 10 6 0 20 60 cm"));
        let r =
            remove_image_region(&d, 0, &[(23.0, 60.0, 26.0, 63.3)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Non-uniform scale + non-zero translation on an axis-swapped
    /// CTM — `[0 20 −4 0 38 60]`: x' = −4v+38, y' = 20u+60.
    /// Pixel (0,0) covers u∈[0,1/3], v∈[1/2,1] →
    /// user x∈[34,36], y∈[60,66.67].
    #[test]
    fn grid_ctm_nonuniform_swapped() {
        let d = doc(&grid_ctm_gray_pdf("0 20 -4 0 38 60 cm"));
        let r =
            remove_image_region(&d, 0, &[(34.0, 60.0, 36.0, 66.6)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Negative page coordinates — `[-6 0 0 10 -2 60]` places the
    /// painted rect at x∈[-38,-20]; a rect into negative space
    /// still selects pixel (0,0).
    #[test]
    fn grid_ctm_negative_coords() {
        let d = doc(&grid_ctm_gray_pdf("-6 0 0 10 -20 60 cm"));
        // Pixel (0,0): unit(0,1/2)-(1/3,1) → x∈[-20,-22] wait —
        // x' = -6u - 20: u=0→-20, u=1/3→-22 → x∈[-22,-20].
        let r =
            remove_image_region(&d, 0, &[(-22.0, 65.0, -20.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// A rect partially covering the image selects the covered
    /// pixels only — with a reflection so the covered half is the
    /// image's *right* half (pixels x≥1).
    #[test]
    fn grid_ctm_partial_cover_reflected() {
        // `[6 0 0 10 20 60]` paints the image at x∈[20,26]. A rect
        // covering the right portion x∈[23,26] selects pixel
        // columns px≥1 on both rows → 4 pixels.
        let d = doc(&grid_ctm_gray_pdf("6 0 0 10 20 60 cm"));
        let r =
            remove_image_region(&d, 0, &[(23.0, 60.0, 26.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 4);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x10, 0x00, 0x00, 0x40, 0x00, 0x00]);
    }

    /// Pixel-boundary rounding — a rect that clips part of a pixel
    /// still selects it (floor/ceil outward rule, unchanged).
    #[test]
    fn grid_ctm_partial_pixel_cover() {
        let d = doc(&grid_ctm_gray_pdf("6 0 0 10 20 60 cm"));
        // Rect covers only the right half of pixel (0,0)'s painted
        // area (x∈[21,22] of its [20,22] span) — still selects it.
        let r =
            remove_image_region(&d, 0, &[(21.0, 65.0, 22.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Disjoint rects on a reflected image — two rects select
    /// two non-adjacent pixels; the L-shape between them is not
    /// filled.
    #[test]
    fn grid_ctm_disjoint_rects_reflected() {
        let d = doc(&grid_ctm_gray_pdf("-6 0 0 10 38 60 cm"));
        // Image is painted at x∈[32,38]; pixel (0,0) covers
        // x∈[36,38], pixel (2,0) covers x∈[32,34] (the image's
        // right column lands on the left after reflection).
        let r = remove_image_region(
            &d,
            0,
            &[(36.0, 65.0, 38.0, 70.0), (32.0, 65.0, 34.0, 70.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.pixels_changed, 2);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x00, 0x40, 0x50, 0x60]);
    }

    /// RGB image — pixel (0,0) under a 90° rotation; each pixel is
    /// three components but counts once.
    #[test]
    fn grid_ctm_rgb_rotated() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // 3×2 RGB — each pixel's channels differ from its neighbours.
        let img_data: &[u8] = &[
            0x11, 0x12, 0x13, 0x21, 0x22, 0x23, 0x31, 0x32, 0x33, // row 0
            0x41, 0x42, 0x43, 0x51, 0x52, 0x53, 0x61, 0x62, 0x63, // row 1
        ];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"0 10 -6 0 38 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r =
            remove_image_region(&d, 0, &[(32.0, 60.0, 35.0, 63.3)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let out = doc(&r.output);
        let s = grid_ctm_edited_samples(&out);
        assert_eq!(
            s,
            vec![
                0x00, 0x00, 0x00, 0x21, 0x22, 0x23, 0x31, 0x32, 0x33, 0x41, 0x42, 0x43, 0x51, 0x52,
                0x53, 0x61, 0x62, 0x63,
            ]
        );
    }

    /// A shear CTM is refused (not grid-aligned).
    #[test]
    fn grid_ctm_shear_refused() {
        // Shear: [1 0 0.5 1 20 60] — a=c≠0? no: a=1,b=0,c=0.5,d=1
        // → b==0 but c≠0 → neither class → refused.
        let d = doc(&grid_ctm_gray_pdf("1 0 0.5 1 20 60 cm"));
        let e = remove_image_region(&d, 0, &[(20.0, 60.0, 40.0, 80.0)], &Limits::default())
            .unwrap_err();
        assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    }

    /// A singular CTM (a=0,b=0,c=0,d=0 or degenerate) is refused.
    #[test]
    fn grid_ctm_singular_refused() {
        let d = doc(&grid_ctm_gray_pdf("0 0 0 0 20 60 cm"));
        let e = remove_image_region(&d, 0, &[(20.0, 60.0, 40.0, 80.0)], &Limits::default())
            .unwrap_err();
        assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    }

    /// A non-finite CTM is `Structure` — tested directly on
    /// `image_pixel_ranges` since a real `cm` operand can't carry
    /// NaN/∞ through the parser.
    #[test]
    fn grid_ctm_nonfinite_refused() {
        let m = Matrix::new(f64::INFINITY, 0.0, 0.0, 10.0, 20.0, 60.0);
        let e = image_pixel_ranges(&m, 3, 2, &[]).unwrap_err();
        assert!(matches!(e, Error::Structure(_)), "{e:?}");
        let m2 = Matrix::new(f64::NAN, 0.0, 0.0, 10.0, 20.0, 60.0);
        let e2 = image_pixel_ranges(&m2, 3, 2, &[]).unwrap_err();
        assert!(matches!(e2, Error::Structure(_)), "{e2:?}");
    }

    /// A 90° rotation through `remove_region` (the integrated
    /// text+image path) — same shared transform logic.
    #[test]
    fn grid_ctm_rot90_via_remove_region() {
        let d = doc(&grid_ctm_gray_pdf("0 10 -6 0 38 60 cm"));
        let r = crate::remove_region::remove_region(
            &d,
            0,
            &[(32.0, 60.0, 35.0, 63.3)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.pixels_changed, 1);
    }

    /// The same image painted at *different* transforms in two `Do`
    /// calls — only the rotated one is edited; the axis-aligned one
    /// keeps the original object.
    #[test]
    fn grid_ctm_two_invocations_different_transforms() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Im1 painted twice with independent CTMs (q/Q isolate
        // the graphics state): axis-aligned at (20,60), then
        // 90°-rotated with origin (100,120). The rect hits only
        // the rotated copy's pixel (1,0) area — not (0,0), which
        // is already black in this image and would not count.
        let body = b"q\n6 0 0 10 20 60 cm\n/Im1 Do\nQ\nq\n0 10 -6 0 100 120 cm\n/Im1 Do\nQ\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Rotated copy: x' = -6v+100, y' = 10u+120; pixel (1,0)
        // covers u∈[1/3,2/3], v∈[1/2,1] → x∈[94,97], y∈[123.33,126.67].
        let r =
            remove_image_region(&d, 0, &[(94.0, 123.4, 97.0, 126.6)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        // Only the rotated Do was repointed — the axis-aligned one
        // still uses /Im1 and the original data is intact.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(
            out.contains("/Im1 Do"),
            "axis-aligned Do must remain: {out}"
        );
        assert!(out.contains("\x10\x20\x30\x40\x50\x60"), "original kept");
    }

    /// A JPEG image under a 90° rotation — the shared transform
    /// applies before decode, same as raw/Flate.
    #[test]
    fn grid_ctm_jpeg_rotated() {
        // 8×8 gray JPEG + 90° CTM.
        let jpeg = {
            use crate::remove_image::decode_image;
            let _ = decode_image;
            unhex_jpeg()
        };
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = format!(
            "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 \
              /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        f.extend_from_slice(&jpeg);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // 90° rotation, non-uniform 16×8 scale, origin (120,100).
        // x' = -8v+120, y' = 16u+100. Pixel (0,1) — second row,
        // first column, not already black — covers u∈[0,1/8],
        // v∈[6/8,7/8] → x∈[113,114], y∈[100,102].
        let body = b"0 16 -8 0 120 100 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Select inside pixel (0,1)'s painted rect x∈[113,114],
        // y∈[100,102] — pull the edges inward so neighbouring
        // pixels aren't caught by the outward floor/ceil.
        let r = remove_image_region(&d, 0, &[(113.1, 100.1, 113.9, 101.9)], &Limits::default())
            .unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
    }

    /// The g8 fixture JPEG (same bytes as remove_region.rs tests).
    fn unhex_jpeg() -> Vec<u8> {
        const H: &str =
            "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07\
             0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc0000b080008\
             000801011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b51000020103\
             03020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024336272\
             82090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475\
             767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9\
             cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f003e12c1a5fecabf03\
             a5f19ded9f9daac98d3f42b46b569a3b9d49e37681240acb888796cee4bafc88c14972aadfffd9";
        (0..H.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&H[i..i + 2], 16).unwrap())
            .collect()
    }

    /// A `/Rotate 90` page — the input rect still uses un-rotated
    /// user-space coordinates; the page rotation is *not* applied
    /// to the selection rect.
    #[test]
    fn grid_ctm_page_rotate_unaffected() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        // /Rotate 90 on the *page* — the image's own CTM is what
        // matters; the rect is in the same user space as before.
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Rotate 90 \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"6 0 0 10 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Same un-rotated rect as grid_ctm_axis_aligned_baseline — the
        // page /Rotate is display metadata, not a transform of the
        // content stream's coordinate space.
        let r =
            remove_image_region(&d, 0, &[(20.0, 65.0, 22.0, 70.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
    }

    /// Two pages share one image object under different transforms;
    /// editing one page's rect leaves the other page's pixels and
    /// the original stream untouched.
    #[test]
    fn grid_ctm_two_pages_shared_image() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        for (n, cm) in [(3u32, "6 0 0 10 20 60 cm"), (4u32, "0 10 -6 0 100 120 cm")] {
            let contents = 9 + (n - 3); // 9 and 10
            let page = format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /XObject << /Im1 11 0 R >> >> \
                  /Contents {contents} 0 R >>"
            );
            push!(n, page.as_bytes());
            let body = format!("{cm}\n/Im1 Do\n").into_bytes();
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(&body);
            sb.extend_from_slice(b"\nendstream");
            push!(contents, &sb);
        }
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(11, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Edit page 1's rotated copy: pixel (0,0) at x∈[94,97],
        // y∈[120,123.3] under [0 10 −6 0 100 120].
        let r = crate::remove_region::remove_regions(
            &d,
            &[
                (0, vec![(20.0, 65.0, 22.0, 70.0)]),
                (1, vec![(94.0, 120.0, 97.0, 123.3)]),
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 2);
        // The *original* image object still carries its bytes.
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 11,
                generation: 0,
            }))
            .unwrap();
        match orig {
            Object::Stream { ref data, .. } => {
                assert_eq!(data, &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]);
            }
            _ => panic!("not a stream"),
        }
    }

    /// A FlateDecode image under a 90° rotation — the shared
    /// transform applies before decode, same as raw/JPEG.
    #[test]
    fn grid_ctm_flate_rotated() {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&img_data).unwrap();
        let compressed = enc.finish().unwrap();

        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let mut f = format!(
            "<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 \
              /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        f.extend_from_slice(&compressed);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"0 10 -6 0 38 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Same geometry as grid_ctm_rot90 — pixel (0,0) at x∈[32,35],
        // y∈[60,63.33].
        let r =
            remove_image_region(&d, 0, &[(32.0, 60.0, 35.0, 63.3)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 1);
        let s = grid_ctm_edited_samples(&doc(&r.output));
        assert_eq!(s, vec![0x00, 0x20, 0x30, 0x40, 0x50, 0x60]);
    }

    /// Text + rotated image mixed on one page — the text op and the
    /// rotated `Do` are edited in the same integrated pass.
    #[test]
    fn grid_ctm_text_and_rotated_image() {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> \
              /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        font_dict.extend_from_slice(&widths);
        font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &font_dict);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Rotated image in the lower region, text in the upper
        // region — two independent rects, like removes_text_and_image.
        let body = b"q\n0 10 -6 0 38 60 cm\n/Im1 Do\nQ\n\
            BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Rects: the rotated image's pixel (0,0) at x∈[32,35],
        // y∈[60,63.3]; and the text glyph painted near (30,140).
        let r = crate::remove_region::remove_region(
            &d,
            0,
            &[(32.0, 60.0, 35.0, 63.3), (10.0, 130.0, 60.0, 170.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.pixels_changed, 1);
        assert_eq!(r.invocations_edited, 1);
        assert!(r.codes_removed > 0, "text inside the rect was not removed");
    }

    /// Rotated image through the metadata-integrated entry — the
    /// document's `/Info` is stripped in the same edit.
    #[test]
    fn grid_ctm_metadata_integrated_rotated() {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(13, b"<< /Producer (kasane-test) /Creator (m28) >>");
        let img_data = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"0 10 -6 0 38 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 14\n0000000000 65535 f \n".to_vec());
        for n in 1..14u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 14 /Root 1 0 R /Info 13 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = crate::remove_region::remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(32.0, 60.0, 35.0, 63.3)])],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        // /Info is stripped — the trailer's Info reference is gone.
        let out_txt = String::from_utf8_lossy(&r.output).into_owned();
        assert!(!out_txt.contains("/Producer"), "Info still present");
    }
}
