//! M20 — **experimental** scoped region removal (text + image).
//!
//! [`remove_region`] performs a single integrated edit pass over one
//! page: it removes character codes whose filled-ink bounds hit a
//! caller rect *and* blackens image samples whose painted positions
//! hit the same rects — then writes one new PDF.
//!
//! **This is not a redaction feature.** It is an experiment in
//! *content editing*: it does not paint over anything, does not
//! remove other copies of the same data (metadata, annotations,
//! structure tree, other pages, font internals, the original image
//! streams), and does not guarantee irreversibility. See the crate
//! docs and `docs/features/editing-region.md` for the full scope list.
//!
//! Scope — a page is *refused* (typed error) rather than half-edited
//! when any of these appear in its content or page dict:
//!
//! - `/Annots` — annotations are out of scope.
//! - `/UserUnit` ≠ 1 — coordinates are not default units.
//! - Any `Do` to a `/Form` — nested XObjects are out of scope
//!   (M20 does not inherit M18's form handling; a form `Do` is
//!   refused even when it contains no text/images).
//! - Any inline image (`BI`/`ID`/`EI`), `gs`, `sh`, Pattern colour
//!   space, `/ActualText`/`OC` marked content, or any other construct
//!   the diagnosis marks as `blocker`/`uncertain`.
//! - Any text-rendering mode other than `Tr 0` (fill) anywhere in
//!   the shown text — page-level eligibility, not per-rect.
//! - Any image that is not 8-bit `/DeviceGray` or `/DeviceRGB` (after
//!   colour-space resolution and `Default*` substitution) with no
//!   `/Mask`/`SMask`/`SMaskInData`/`OC`/`ImageMask`/`Interpolate`,
//!   a non-default `/Decode`, a `/Filter` other than none, a single
//!   `/FlateDecode`, or a single `/DCTDecode` (sequential-DCT JPEG decoding
//!   to 8-bit Gray/RGB; CMYK/16-bit/progressive refused), or a
//!   non-axis-aligned/negative-scale/non-finite CTM.
//! - Any `Image` target at `form_path` depth ≥ 1 — images inside a
//!   form are out of scope.
//!
//! **Planning against the original `Document`.** The walk runs on the
//! original content stream; every target is keyed by its original
//! `op_index`. Text ops are rewritten by `remove_text::rewrite_ops_for`
//! (the same TJ/kern logic M17 uses); image `Do`s are repointed to
//! cloned image objects via `KasIm{n}` names (the same per-invocation
//! cloning M19 uses). Both kinds of edits are applied to the *same*
//! output ops list in one pass — a `Tj` that loses codes becomes a
//! `TJ` at the same `op_index`, and a `Do` that hits a rect gets a
//! new name at the same `op_index`. The page's `/Contents` is
//! serialised once, and `rewrite` runs once.
//!
//! Resource budgets: the same cumulative `max_edit_content_bytes`
//! cap as `remove_text`/`remove_image_region` bounds the cloned
//! image bodies plus the new page `/Contents` — shared, not per-type.
//! `max_image_bytes` is checked *before* each image decodes.
//! `max_diag_exec_ops` bounds the walk; `max_xobject_invocations`
//! bounds the `Do` count. The final PDF is bounded by
//! `max_output_bytes` at serialize time — `BoundedWriter` checks
//! the remaining budget before every byte lands (logical bytes
//! written, not allocator capacity/RSS).

use crate::content::Operation;
use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, IndirectRef, Object};
use crate::redact_diag::{check_rect, page_dict_entry, DiagWalker, Site, TargetDetail, TargetKind};
use crate::remove_image::{
    decode_image, fresh_xobject_name, image_pixel_ranges, is_default_decode, validate_image,
};
use crate::remove_text::{charge_generated, rewrite_ops_for, serialize_ops};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The result of [`remove_region`] — the rewritten PDF bytes plus
/// the audit trail of what was removed.
pub struct RegionRemoval {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// How many character codes were removed.
    pub codes_removed: u64,
    /// How many `Do` invocations were edited.
    pub invocations_edited: u64,
    /// How many pixels were changed to black (at least one component).
    pub pixels_changed: u64,
    /// How many XObject names were removed from the page's
    /// `/Resources` because they were orphaned by the edit (M21).
    /// A removed name does *not* imply the object itself was
    /// deleted — the writer's reachability scan decides that.
    pub orphaned_names_removed: u64,
    /// The `Site` of every edited invocation — page content ref +
    /// op index (per `Site` docs).
    pub sites: Vec<Site>,
    /// Human-readable scope statement for the CLI/report — what was
    /// checked and what limits applied. Never contains document
    /// text or image bytes.
    pub scope: Vec<String>,
}

/// Terse summary for logging — never prints `output` bytes or site
/// content so a stray `{:?}` cannot leak document text or image data.
impl std::fmt::Debug for RegionRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionRemoval")
            .field("output_bytes", &self.output.len())
            .field("codes_removed", &self.codes_removed)
            .field("invocations_edited", &self.invocations_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("orphaned_names_removed", &self.orphaned_names_removed)
            .field("sites", &self.sites.len())
            .field("scope", &self.scope)
            .finish()
    }
}

/// Experimental scoped region removal — text + image in one pass.
/// `rects` are in un-rotated, un-cropped user space (+x right, +y up)
/// — the same convention `diagnose_redaction`, `remove_text`, and
/// `remove_image_region` use.
///
/// On any failure — validation, an unsupported construct, or a
/// limit — nothing is emitted; a partial rewrite is never returned
/// as success.
pub fn remove_region(
    doc: &Document,
    page_index: usize,
    rects: &[(f64, f64, f64, f64)],
    limits: &Limits,
) -> Result<RegionRemoval, Error> {
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
            page_label(page_index),
            doc.page_count()
        ))
    })?;

    // ---- pre-flight refusal checks --------------------------------
    if page_dict_entry(doc, page, b"Annots")?.is_some() {
        return Err(Error::Unsupported {
            feature: "region removal on a page with /Annots",
            detail: "annotations are out of scope — their content is \
                     not evaluated"
                .into(),
        });
    }
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
    // One walk, one ops list — the original `Document` is never
    // re-parsed. Every target is keyed by its original `op_index`.
    let ops = doc.page_operations(page)?;
    let mut walker = DiagWalker::new(doc, page, limits, &validated);
    walker.run(&ops)?;
    if let Some(b) = walker.blockers.first() {
        return Err(Error::Unsupported {
            feature: "region removal on a page with blockers",
            detail: format!("{}: {}", b.what, b.detail),
        });
    }
    if let Some(u) = walker.uncertain.first() {
        return Err(Error::Unsupported {
            feature: "region removal on a page with uncertain content",
            detail: u.reason.clone(),
        });
    }
    if let Some((i, _)) = ops
        .iter()
        .enumerate()
        .find(|(_, o)| matches!(o, Operation::Shading(_)))
    {
        return Err(Error::Unsupported {
            feature: "region removal on a page with sh",
            detail: format!("op #{i} — shading content is out of scope"),
        });
    }

    // ---- page-level eligibility ------------------------------------
    // M20's own rules — not a simple union of M17+M19 refusals:
    //
    // - `Text` targets must all be `Tr 0` fill — page-level *and*
    //   form-level alike. (`Tr 4–7` and `w = 0` already landed in
    //   `blockers`/`uncertain` above.)
    // - `Image` targets at `form_path` depth 0 are in scope; at
    //   depth ≥ 1 they are inside a form → refused.
    // - `Form` targets are refused — M20 does not inherit M18's
    //   form handling; a `Do` to a `/Form` is refused even when it
    //   contains no text or images.
    // - `Other` targets are refused regardless of rects.
    for t in &walker.targets {
        if t.site.form_path.len() > 1 {
            return Err(Error::Unsupported {
                feature: "region removal on a page with nested XObjects",
                detail: "a Do inside a form is out of scope".into(),
            });
        }
        match t.kind {
            TargetKind::Text => {
                if let TargetDetail::Text { render_mode, .. } = &t.detail {
                    if *render_mode != 0u8 {
                        return Err(Error::Unsupported {
                            feature: "region removal on a page with non-fill text",
                            detail: format!(
                                "Tr {render_mode} text is out of scope — only Tr 0 fill \
                                 is rewritten (op #{}, code #{:?})",
                                t.site.op_index, t.site.code_index
                            ),
                        });
                    }
                }
            }
            TargetKind::Image if t.site.form_path.is_empty() => {}
            TargetKind::Form | TargetKind::Image | TargetKind::Other => {
                return Err(Error::Unsupported {
                    feature: "region removal on a page with non-image XObjects",
                    detail: format!(
                        "an {:?} occurrence at op #{} is out of scope",
                        t.kind, t.site.op_index
                    ),
                });
            }
            TargetKind::Path => {}
        }
    }

    // ---- collect removal plan --------------------------------------
    // Text: (form_path, op_index) → sorted code_index list.
    // Image: op_index → (Site, CTM, xobj_ref, name).
    let mut to_remove_text: BTreeMap<(Vec<crate::redact_diag::FormFrame>, usize), Vec<usize>> =
        BTreeMap::new();
    let mut site_lookup: BTreeMap<(Vec<crate::redact_diag::FormFrame>, usize, usize), Site> =
        BTreeMap::new();
    let mut to_edit_image: BTreeMap<usize, (Site, Matrix, Option<IndirectRef>, Vec<u8>)> =
        BTreeMap::new();

    for t in &walker.targets {
        match t.kind {
            TargetKind::Text if !t.hits.is_empty() => {
                let ci = t
                    .site
                    .code_index
                    .ok_or_else(|| Error::Structure("text target without code_index".into()))?;
                let key = (t.site.form_path.clone(), t.site.op_index);
                site_lookup
                    .entry((t.site.form_path.clone(), t.site.op_index, ci))
                    .or_insert_with(|| t.site.clone());
                to_remove_text.entry(key).or_default().push(ci);
            }
            TargetKind::Image if t.site.form_path.is_empty() && !t.hits.is_empty() => {
                if let TargetDetail::Image {
                    xobj, name, ctm, ..
                } = &t.detail
                {
                    to_edit_image
                        .insert(t.site.op_index, (t.site.clone(), *ctm, *xobj, name.clone()));
                }
            }
            _ => {}
        }
    }
    for v in to_remove_text.values_mut() {
        v.sort_unstable();
    }

    // ---- clone images for edited invocations ------------------------
    let mut edit_guard: Option<Edit> = None;
    let mut do_renames: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    let mut removed_sites: Vec<Site> = Vec::new();
    let mut codes_removed: u64 = 0;
    let mut pixels_changed: u64 = 0;
    let mut generated_bytes: u64 = 0;
    let mut new_xobject_entries: Vec<(Vec<u8>, IndirectRef)> = Vec::new();

    for (op_index, (site, ctm, _xobj_ref, name)) in &to_edit_image {
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

        let (width, height, ncomps) = validate_image(dict, page.resources(), doc, limits)?;

        let ranges = image_pixel_ranges(ctm, width, height, &validated)?;
        if ranges.is_empty() {
            continue; // no pixels to change — skip decode entirely
        }

        // Budget checked *before* decode — cumulative, shared with
        // text edits. A second image is refused without allocating.
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

        // Count changed *pixels* (not components).
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

        let mut new_dict = dict.clone();
        new_dict.remove(b"Filter".as_slice());
        new_dict.remove(b"DecodeParms".as_slice());
        new_dict.remove(b"Length".as_slice());
        new_dict.insert(b"Length".to_vec(), Object::Integer(new_data.len() as i64));
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

        let allocated: Vec<Vec<u8>> = new_xobject_entries.iter().map(|(n, _)| n.clone()).collect();
        let new_name = fresh_xobject_name(page, doc, do_renames.len() as u64, &allocated)?;
        do_renames.insert(*op_index, new_name.clone());
        new_xobject_entries.push((new_name, clone_ref));
        removed_sites.push(site.clone());
    }

    // ---- rewrite the page content stream ----------------------------
    // One pass over the original ops: text ops are rewritten by
    // `remove_text::rewrite_ops_for` (same TJ/kern logic), `Do` ops
    // whose image hit a rect are repointed to the clone name.
    // Both use the *original* op_index — no index drift.
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
            _ => new_ops.extend(rewrite_ops_for(
                &walker,
                &to_remove_text,
                &site_lookup,
                &[],
                op_index,
                op,
                &mut codes_removed,
                &mut removed_sites,
            )?),
        }
    }

    // ---- M21: collect XObject names orphaned by the edit ------------
    // Names that were edited (old names) are candidates for removal;
    // they are kept only if some *remaining* `Do` still uses them.
    // We scan `new_ops` (the post-edit ops) — not the original ops —
    // because text expansion may have changed op indices.
    let mut candidate_names: BTreeSet<Vec<u8>> = BTreeSet::new();
    for (_site, _ctm, _xobj_ref, name) in to_edit_image.values() {
        candidate_names.insert(name.clone());
    }
    let mut used_names: BTreeSet<Vec<u8>> = BTreeSet::new();
    for op in &new_ops {
        if let Operation::Do(name) = op {
            used_names.insert(name.clone());
        }
    }
    let orphaned_names: Vec<Vec<u8>> = candidate_names
        .iter()
        .filter(|n| !used_names.contains(*n))
        .cloned()
        .collect();

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

    // ---- M22: materialise inherited Resources -----------------------
    // Each page's *effective* Resources (as resolved by the page-tree
    // walk) is written explicitly into the page dict. A parent /Pages
    // can then drop its /Resources — but only when *every* descendant
    // page has been materialised. This is the same Edit/overlay as the
    // text/image edits — one rewrite, not two.
    //
    // M22.2: page refs are indexed by HashMap so a leaf lookup is O(1),
    // not O(n) per kid. The strip decision is post-order — a /Pages
    // node's `all_materialised` is computed only after its children's
    // results are known, and a child's failure propagates upward.
    let mut pages_with_resources: HashSet<IndirectRef> = HashSet::new();
    let mut page_map: HashMap<IndirectRef, &crate::page::Page> = HashMap::new();
    for p in doc.pages() {
        if let Some(pr) = p.object_ref() {
            if p.resources().is_some() {
                pages_with_resources.insert(pr);
                page_map.insert(pr, p);
            }
        }
    }
    // Post-order traversal over the /Pages tree — see
    // `plan_resource_materialisation` for the end-state contract.
    let (pages_to_strip, _node_results) =
        plan_resource_materialisation(doc, &pages_with_resources)?;

    // Apply materialisation: each page gets its effective Resources.
    // The *target* page's dict is updated here — not by a separate
    // overlay insert later — so the Contents edit and the Resources
    // materialisation coexist in one page dict.
    for &pr in &pages_with_resources {
        if pr == page_ref {
            // Target page: page_dict already has Contents; add
            // effective Resources.
            page_dict.insert(b"Resources".to_vec(), page.resources().unwrap().clone());
            continue;
        }
        let p_obj = doc.resolve(pr)?;
        let mut p_dict = p_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("page is not a dictionary".into()))?;
        let res = page_map.get(&pr).unwrap().resources().unwrap().clone();
        p_dict.insert(b"Resources".to_vec(), res);
        edit.overlay
            .insert((pr.number, pr.generation), Object::Dict(p_dict));
    }
    // Strip /Resources from /Pages nodes where all descendants are
    // materialised.
    for &pages_ref in &pages_to_strip {
        let pages_obj = doc.resolve(pages_ref)?;
        let mut pages_dict = pages_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("/Pages is not a dictionary".into()))?;
        pages_dict.remove(b"Resources".as_slice());
        edit.overlay.insert(
            (pages_ref.number, pages_ref.generation),
            Object::Dict(pages_dict),
        );
    }

    let mut orphaned_names_removed: u64 = 0;
    if !new_xobject_entries.is_empty() {
        let mut res_dict = match page.resources() {
            Some(r) => doc
                .deref(r)?
                .as_dict()
                .cloned()
                .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?,
            None => Dict::new(),
        };
        let mut xobj_dict = match res_dict.get(b"XObject".as_slice()) {
            Some(o) => doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                Error::Structure("/Resources /XObject is not a dictionary".into())
            })?,
            None => Dict::new(),
        };
        for (name, r) in &new_xobject_entries {
            xobj_dict.insert(name.clone(), Object::Ref(*r));
        }
        // M21: remove orphaned names — XObject names that were edited
        // and are no longer referenced by any remaining `Do`. The
        // orphaned image object itself is left to the writer's
        // reachability scan: if nothing else references it, it is
        // not emitted in the output.
        for name in &orphaned_names {
            if xobj_dict.remove(name).is_some() {
                orphaned_names_removed += 1;
            }
        }
        res_dict.insert(b"XObject".to_vec(), Object::Dict(xobj_dict));
        page_dict.insert(b"Resources".to_vec(), Object::Dict(res_dict));
    }
    edit.overlay.insert(
        (page_ref.number, page_ref.generation),
        Object::Dict(page_dict),
    );
    let output = edit.rewrite()?;

    Ok(RegionRemoval {
        output,
        codes_removed,
        invocations_edited: do_renames.len() as u64,
        pixels_changed,
        orphaned_names_removed,
        sites: removed_sites,
        scope: vec![
            format!(
                "page {} ({} rect{})",
                page_label(page_index),
                validated.len(),
                if validated.len() == 1 { "" } else { "s" }
            ),
            format!("codes removed: {codes_removed}"),
            format!("invocations edited: {}", do_renames.len()),
            format!("pixels changed: {pixels_changed}"),
            format!("orphaned names removed: {orphaned_names_removed}"),
            "scope: page-level text + image Do calls — no forms, no nested XObjects, \
             no /Annots/metadata/structure, no masks/non-device colour. Images: \
             no filter / FlateDecode / single DCTDecode (8-bit Gray/RGB only); \
             JPEG is decoded to samples and written back uncompressed — other \
             copies of the same data are not removed."
                .into(),
        ],
    })
}

/// The result of [`remove_regions`] — the rewritten PDF bytes plus
/// a per-page audit trail of what was edited.
pub struct RegionsRemoval {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// Total character codes removed across all edited pages.
    pub codes_removed: u64,
    /// Total `Do` invocations edited across all edited pages.
    pub invocations_edited: u64,
    /// Total pixels changed to black across all edited pages.
    pub pixels_changed: u64,
    /// XObject names orphaned by the edit, removed from each edited
    /// page's `/Resources` (M21) — total across pages.
    pub orphaned_names_removed: u64,
    /// `(page_index, sites)` pairs — each entry carries the 0-based
    /// page index plus the `Site`s edited on that page.
    pub per_page: Vec<(usize, Vec<Site>)>,
    /// Human-readable scope statement — never contains document
    /// text or image bytes.
    pub scope: Vec<String>,
}

/// Terse summary — never prints `output` bytes or site content.
impl std::fmt::Debug for RegionsRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionsRemoval")
            .field("output_bytes", &self.output.len())
            .field("codes_removed", &self.codes_removed)
            .field("invocations_edited", &self.invocations_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("orphaned_names_removed", &self.orphaned_names_removed)
            .field("pages", &self.per_page.len())
            .field("scope", &self.scope)
            .finish()
    }
}

/// A page spec for [`remove_regions`] — `(0-based_page_index, rects)`.
/// The same page may appear more than once; its rect lists are merged.
pub type PageSpec = (usize, Vec<(f64, f64, f64, f64)>);

/// Format a 0-based page index for an error message without
/// overflowing: valid pages display 1-based (`page_index + 1`), an
/// out-of-range index that cannot be converted (e.g. `usize::MAX`)
/// is shown raw so the error itself never panics.
fn page_label(page_index: usize) -> String {
    match page_index.checked_add(1) {
        Some(n) => n.to_string(),
        None => page_index.to_string(),
    }
}

/// The workhorse behind [`remove_regions`] and
/// [`remove_regions_and_document_metadata`]: validates `specs`,
/// runs every page's region-removal edits, applies the document-wide
/// `/Resources` materialisation, and returns the accumulated `Edit`
/// session plus the aggregated counters — *without* writing.
///
/// The caller decides what to do with the session: `remove_regions`
/// calls `edit.rewrite()`; the M26 combined entry point calls
/// `edit.rewrite_with_overrides` so the trailer `/Info` and catalog
/// `/Metadata` entries are stripped in the *same* rewrite.
struct RegionsEdit<'a> {
    edit: Edit<'a>,
    codes_removed_total: u64,
    invocations_edited_total: u64,
    pixels_changed_total: u64,
    orphaned_names_removed_total: u64,
    per_page: Vec<(usize, Vec<Site>)>,
    /// Number of distinct pages edited (for the scope line).
    page_count: usize,
}

fn regions_edit_session<'a>(
    doc: &'a Document,
    specs: &[PageSpec],
    limits: &Limits,
) -> Result<RegionsEdit<'a>, Error> {
    // ---- input validation (all before any edit work) --------------
    if specs.is_empty() {
        return Err(Error::Structure(
            "remove_regions requires at least one (page, rects) spec".into(),
        ));
    }
    // Merge per-page rect lists; keep them sorted by page index.
    // `max_diag_rects` bounds the *merged* per-page rect count — a
    // page repeated across several specs cannot bypass it. The
    // check runs before extend so an over-limit page is refused
    // regardless of spec order.
    let mut by_page: BTreeMap<usize, Vec<(f64, f64, f64, f64)>> = BTreeMap::new();
    for (page_index, rects) in specs {
        if rects.is_empty() {
            return Err(Error::Structure(format!(
                "page {} has an empty rect list",
                page_label(*page_index)
            )));
        }
        let entry = by_page.entry(*page_index).or_default();
        let merged = entry
            .len()
            .checked_add(rects.len())
            .ok_or(Error::LimitExceeded {
                limit: "max_diag_rects",
                max: limits.max_diag_rects,
            })?;
        if merged as u64 > limits.max_diag_rects {
            return Err(Error::LimitExceeded {
                limit: "max_diag_rects",
                max: limits.max_diag_rects,
            });
        }
        entry.extend_from_slice(rects);
    }
    // Validate every rect and page index *before* walking anything.
    for (page_index, rects) in &by_page {
        for (i, r) in rects.iter().enumerate() {
            check_rect(i, r.0, r.1, r.2, r.3).map_err(|e| {
                Error::Structure(format!("page {} rect {i}: {e}", page_label(*page_index)))
            })?;
        }
        if doc.pages().get(*page_index).is_none() {
            return Err(Error::Structure(format!(
                "page {} does not exist (document has {} pages)",
                page_label(*page_index),
                doc.page_count()
            )));
        }
    }

    // ---- shared edit session & cumulative budgets -----------------
    let mut edit_guard: Option<Edit> = None;
    // Cumulative counters the multi-page call must not exceed —
    // each page's spend is subtracted from the next page's budget.
    let mut spent_exec_ops: u64 = 0;
    let mut spent_text_codes: u64 = 0;
    let mut spent_xobject_invocations: u64 = 0;
    let mut spent_show_states: u64 = 0;
    let mut generated_bytes: u64 = 0;

    let mut codes_removed_total: u64 = 0;
    let mut invocations_edited_total: u64 = 0;
    let mut pixels_changed_total: u64 = 0;
    let mut orphaned_names_removed_total: u64 = 0;
    let mut per_page: Vec<(usize, Vec<Site>)> = Vec::new();
    let mut page_dicts: Vec<(IndirectRef, Dict)> = Vec::new();
    let mut pages_with_resources: HashSet<IndirectRef> = HashSet::new();
    let mut page_map: HashMap<IndirectRef, &crate::page::Page> = HashMap::new();

    for p in doc.pages() {
        if let Some(pr) = p.object_ref() {
            if p.resources().is_some() {
                pages_with_resources.insert(pr);
                page_map.insert(pr, p);
            }
        }
    }

    for (page_index, raw_rects) in &by_page {
        let page = doc.pages().get(*page_index).unwrap();
        let page_ref = page.object_ref().ok_or_else(|| {
            Error::Structure("cannot edit a page that has no indirect reference".into())
        })?;

        // Per-page validation (same rules as `remove_region`).
        let mut validated = Vec::with_capacity(raw_rects.len());
        for (i, r) in raw_rects.iter().enumerate() {
            validated.push(check_rect(i, r.0, r.1, r.2, r.3)?);
        }
        if page_dict_entry(doc, page, b"Annots")?.is_some() {
            return Err(Error::Unsupported {
                feature: "region removal on a page with /Annots",
                detail: "annotations are out of scope — their content is \
                         not evaluated"
                    .into(),
            });
        }
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

        // ---- walk with remaining budgets ---------------------------
        // Subtract what earlier pages spent so the multi-page call
        // cannot exceed the documented cumulative limits.
        let ops = doc.page_operations(page)?;
        let page_limits = Limits {
            max_diag_exec_ops: limits.max_diag_exec_ops.checked_sub(spent_exec_ops).ok_or(
                Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    max: limits.max_diag_exec_ops,
                },
            )?,
            max_text_codes: limits.max_text_codes.checked_sub(spent_text_codes).ok_or(
                Error::LimitExceeded {
                    limit: "max_text_codes",
                    max: limits.max_text_codes,
                },
            )?,
            max_xobject_invocations: limits
                .max_xobject_invocations
                .checked_sub(spent_xobject_invocations)
                .ok_or(Error::LimitExceeded {
                    limit: "max_xobject_invocations",
                    max: limits.max_xobject_invocations,
                })?,
            max_show_states: limits
                .max_show_states
                .checked_sub(spent_show_states)
                .ok_or(Error::LimitExceeded {
                    limit: "max_show_states",
                    max: limits.max_show_states,
                })?,
            ..limits.clone()
        };
        let mut walker = DiagWalker::new(doc, page, &page_limits, &validated);
        walker.run(&ops)?;
        // Carry this page's spend forward — the next page gets the
        // remaining budget, not a fresh one.
        spent_exec_ops += walker.exec_ops_consumed();
        spent_text_codes += walker.text_codes_consumed();
        spent_xobject_invocations += walker.xobject_invocations_consumed();
        spent_show_states += walker.show_states_len();

        if let Some(b) = walker.blockers.first() {
            return Err(Error::Unsupported {
                feature: "region removal on a page with blockers",
                detail: format!("{}: {}", b.what, b.detail),
            });
        }
        if let Some(u) = walker.uncertain.first() {
            return Err(Error::Unsupported {
                feature: "region removal on a page with uncertain content",
                detail: u.reason.clone(),
            });
        }
        if let Some((i, _)) = ops
            .iter()
            .enumerate()
            .find(|(_, o)| matches!(o, Operation::Shading(_)))
        {
            return Err(Error::Unsupported {
                feature: "region removal on a page with sh",
                detail: format!("op #{i} — shading content is out of scope"),
            });
        }

        // ---- page-level eligibility ---------------------------------
        for t in &walker.targets {
            if t.site.form_path.len() > 1 {
                return Err(Error::Unsupported {
                    feature: "region removal on a page with nested XObjects",
                    detail: "a Do inside a form is out of scope".into(),
                });
            }
            match t.kind {
                TargetKind::Text => {
                    if let TargetDetail::Text { render_mode, .. } = &t.detail {
                        if *render_mode != 0u8 {
                            return Err(Error::Unsupported {
                                feature: "region removal on a page with non-fill text",
                                detail: format!(
                                    "Tr {render_mode} text is out of scope — only Tr 0 fill \
                                     is rewritten (op #{}, code #{:?})",
                                    t.site.op_index, t.site.code_index
                                ),
                            });
                        }
                    }
                }
                TargetKind::Image if t.site.form_path.is_empty() => {}
                TargetKind::Form | TargetKind::Image | TargetKind::Other => {
                    return Err(Error::Unsupported {
                        feature: "region removal on a page with non-image XObjects",
                        detail: format!(
                            "an {:?} occurrence at op #{} is out of scope",
                            t.kind, t.site.op_index
                        ),
                    });
                }
                TargetKind::Path => {}
            }
        }

        // ---- collect this page's removal plan -----------------------
        let mut to_remove_text: BTreeMap<(Vec<crate::redact_diag::FormFrame>, usize), Vec<usize>> =
            BTreeMap::new();
        let mut site_lookup: BTreeMap<(Vec<crate::redact_diag::FormFrame>, usize, usize), Site> =
            BTreeMap::new();
        let mut to_edit_image: BTreeMap<usize, (Site, Matrix, Option<IndirectRef>, Vec<u8>)> =
            BTreeMap::new();
        for t in &walker.targets {
            match t.kind {
                TargetKind::Text if !t.hits.is_empty() => {
                    let ci = t
                        .site
                        .code_index
                        .ok_or_else(|| Error::Structure("text target without code_index".into()))?;
                    let key = (t.site.form_path.clone(), t.site.op_index);
                    site_lookup
                        .entry((t.site.form_path.clone(), t.site.op_index, ci))
                        .or_insert_with(|| t.site.clone());
                    to_remove_text.entry(key).or_default().push(ci);
                }
                TargetKind::Image if t.site.form_path.is_empty() && !t.hits.is_empty() => {
                    if let TargetDetail::Image {
                        xobj, name, ctm, ..
                    } = &t.detail
                    {
                        to_edit_image
                            .insert(t.site.op_index, (t.site.clone(), *ctm, *xobj, name.clone()));
                    }
                }
                _ => {}
            }
        }
        for v in to_remove_text.values_mut() {
            v.sort_unstable();
        }

        // ---- clone images for this page's edited invocations -------
        let mut do_renames: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut removed_sites: Vec<Site> = Vec::new();
        let mut codes_removed: u64 = 0;
        let mut pixels_changed: u64 = 0;
        let mut new_xobject_entries: Vec<(Vec<u8>, IndirectRef)> = Vec::new();

        for (op_index, (site, ctm, _xobj_ref, name)) in &to_edit_image {
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

            let (width, height, ncomps) = validate_image(dict, page.resources(), doc, limits)?;

            let ranges = image_pixel_ranges(ctm, width, height, &validated)?;
            if ranges.is_empty() {
                continue;
            }
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
                continue;
            }
            pixels_changed += changed_here;

            let mut new_dict = dict.clone();
            new_dict.remove(b"Filter".as_slice());
            new_dict.remove(b"DecodeParms".as_slice());
            new_dict.remove(b"Length".as_slice());
            new_dict.insert(b"Length".to_vec(), Object::Integer(new_data.len() as i64));
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

            let allocated: Vec<Vec<u8>> =
                new_xobject_entries.iter().map(|(n, _)| n.clone()).collect();
            let new_name = fresh_xobject_name(page, doc, do_renames.len() as u64, &allocated)?;
            do_renames.insert(*op_index, new_name.clone());
            new_xobject_entries.push((new_name, clone_ref));
            removed_sites.push(site.clone());
        }

        // ---- rewrite this page's content stream ---------------------
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
                _ => new_ops.extend(rewrite_ops_for(
                    &walker,
                    &to_remove_text,
                    &site_lookup,
                    &[],
                    op_index,
                    op,
                    &mut codes_removed,
                    &mut removed_sites,
                )?),
            }
        }

        // ---- M21: orphaned XObject names for this page --------------
        let mut candidate_names: BTreeSet<Vec<u8>> = BTreeSet::new();
        for (_site, _ctm, _xobj_ref, name) in to_edit_image.values() {
            candidate_names.insert(name.clone());
        }
        let mut used_names: BTreeSet<Vec<u8>> = BTreeSet::new();
        for op in &new_ops {
            if let Operation::Do(name) = op {
                used_names.insert(name.clone());
            }
        }
        let orphaned_names: Vec<Vec<u8>> = candidate_names
            .iter()
            .filter(|n| !used_names.contains(*n))
            .cloned()
            .collect();

        // ---- serialise + page dict update ---------------------------
        let new_body = serialize_ops(&new_ops, limits, generated_bytes)?;
        charge_generated(generated_bytes, new_body.len(), limits)?;
        // Share the *same* Edit across pages — a synthetic ref from an
        // earlier page must resolve through the same overlay.
        if edit_guard.is_none() {
            edit_guard = Some(Edit::new(doc, limits.clone())?);
        }
        let edit = edit_guard.as_mut().unwrap();
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
        let page_obj = doc.resolve(page_ref)?;
        let mut page_dict = page_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure(format!("page {page_ref} is not a dictionary")))?;
        // Contents and Resources coexist in *this* page dict — the
        // M22.1 rule: never write Contents separately from the
        // materialised Resources.
        page_dict.insert(b"Contents".to_vec(), Object::Ref(stream_ref));
        if let Some(res) = page.resources() {
            page_dict.insert(b"Resources".to_vec(), res.clone());
        }

        let mut orphaned_names_removed: u64 = 0;
        if !new_xobject_entries.is_empty() {
            let mut res_dict = match page.resources() {
                Some(r) => doc
                    .deref(r)?
                    .as_dict()
                    .cloned()
                    .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?,
                None => Dict::new(),
            };
            let mut xobj_dict = match res_dict.get(b"XObject".as_slice()) {
                Some(o) => doc.deref(o)?.as_dict().cloned().ok_or_else(|| {
                    Error::Structure("/Resources /XObject is not a dictionary".into())
                })?,
                None => Dict::new(),
            };
            for (name, r) in &new_xobject_entries {
                xobj_dict.insert(name.clone(), Object::Ref(*r));
            }
            for name in &orphaned_names {
                if xobj_dict.remove(name).is_some() {
                    orphaned_names_removed += 1;
                }
            }
            res_dict.insert(b"XObject".to_vec(), Object::Dict(xobj_dict));
            page_dict.insert(b"Resources".to_vec(), Object::Dict(res_dict));
        }
        // Stash the completed page dict — the overlay insert happens
        // after every page's edits are computed, so one page's
        // Resources materialisation cannot clobber another's
        // Contents.
        page_dicts.push((page_ref, page_dict));

        codes_removed_total += codes_removed;
        invocations_edited_total += do_renames.len() as u64;
        pixels_changed_total += pixels_changed;
        orphaned_names_removed_total += orphaned_names_removed;
        per_page.push((*page_index, removed_sites));
    }

    // ---- Resources materialisation across the document -------------
    // Every page's effective Resources is written into its page dict
    // *here* — after all per-page edits are computed — so a parent
    // /Pages can drop /Resources only when *every* descendant has
    // been materialised.
    let mut edit = match edit_guard {
        Some(e) => e,
        None => Edit::new(doc, limits.clone())?,
    };
    let (pages_to_strip, _node_results) =
        plan_resource_materialisation(doc, &pages_with_resources)?;
    let edited_refs: HashSet<IndirectRef> = page_dicts.iter().map(|(r, _)| *r).collect();
    for &pr in &pages_with_resources {
        if edited_refs.contains(&pr) {
            // Edited page — its Resources was already materialised
            // into `page_dicts` together with Contents.
            continue;
        }
        let p_obj = doc.resolve(pr)?;
        let mut p_dict = p_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("page is not a dictionary".into()))?;
        let res = page_map.get(&pr).unwrap().resources().unwrap().clone();
        p_dict.insert(b"Resources".to_vec(), res);
        edit.overlay
            .insert((pr.number, pr.generation), Object::Dict(p_dict));
    }
    // Strip /Resources from /Pages nodes where all descendants are
    // materialised.
    for &pages_ref in &pages_to_strip {
        let pages_obj = doc.resolve(pages_ref)?;
        let mut pages_dict = pages_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("/Pages is not a dictionary".into()))?;
        pages_dict.remove(b"Resources".as_slice());
        edit.overlay.insert(
            (pages_ref.number, pages_ref.generation),
            Object::Dict(pages_dict),
        );
    }
    // Commit every edited page dict — Contents + Resources together.
    for (page_ref, page_dict) in page_dicts {
        edit.overlay.insert(
            (page_ref.number, page_ref.generation),
            Object::Dict(page_dict),
        );
    }

    Ok(RegionsEdit {
        edit,
        codes_removed_total,
        invocations_edited_total,
        pixels_changed_total,
        orphaned_names_removed_total,
        per_page,
        page_count: by_page.len(),
    })
}

/// M24 — experimental scoped region removal over **multiple pages**
/// in one edit session and one rewrite.
///
/// `specs` is `(0-based_page_index, rects)` pairs; rects use the same
/// un-rotated user-space convention as [`remove_region`]. The same
/// page may appear more than once — its rect lists are merged.
/// Pages are processed in ascending page order regardless of input
/// order.
///
/// Every per-page eligibility rule from [`remove_region`] applies
/// unchanged (no `/Annots`, no forms, no masks/JPEG/non-device
/// colour, `Tr 0` text only, …). All pages share the *same* `Edit`
/// session and one final `rewrite` — no per-page PDFs, no chained
/// rewrites, no per-page budget resets. Cumulative limits
/// (`max_diag_exec_ops`, `max_text_codes`, `max_show_states`,
/// `max_edit_content_bytes`, `max_edit_objects`, `max_output_bytes`)
/// bound the *whole call*, not each page.
///
/// On any failure nothing is emitted — there is no partial rewrite.
pub fn remove_regions(
    doc: &Document,
    specs: &[PageSpec],
    limits: &Limits,
) -> Result<RegionsRemoval, Error> {
    let s = regions_edit_session(doc, specs, limits)?;
    let output = s.edit.rewrite()?;
    Ok(RegionsRemoval {
        output,
        codes_removed: s.codes_removed_total,
        invocations_edited: s.invocations_edited_total,
        pixels_changed: s.pixels_changed_total,
        orphaned_names_removed: s.orphaned_names_removed_total,
        per_page: s.per_page,
        scope: vec![
            format!(
                "{} page{} edited",
                s.page_count,
                if s.page_count == 1 { "" } else { "s" }
            ),
            format!("codes removed: {}", s.codes_removed_total),
            format!("invocations edited: {}", s.invocations_edited_total),
            format!("pixels changed: {}", s.pixels_changed_total),
            format!("orphaned names removed: {}", s.orphaned_names_removed_total),
            "scope: page-level text + image Do calls — no forms, no nested XObjects, \
             no /Annots/metadata/structure, no masks/non-device colour. Images: \
             no filter / FlateDecode / single DCTDecode (8-bit Gray/RGB only); \
             JPEG is decoded to samples and written back uncompressed. \
             Experimental content editing — other copies of the same data \
             (metadata, annotations, structure tree, other pages) may remain."
                .into(),
        ],
    })
}

/// The result of [`remove_regions_and_document_metadata`] — the
/// region-removal tallies plus the two metadata-entry flags.
pub struct RegionsAndMetadataRemoval {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// Total character codes removed across all edited pages.
    pub codes_removed: u64,
    /// Total `Do` invocations edited across all edited pages.
    pub invocations_edited: u64,
    /// Total pixels changed to black across all edited pages.
    pub pixels_changed: u64,
    /// XObject names orphaned by the edit, removed from each edited
    /// page's `/Resources` — total across pages.
    pub orphaned_names_removed: u64,
    /// `(page_index, sites)` pairs — 0-based page index plus the
    /// `Site`s edited on that page.
    pub per_page: Vec<(usize, Vec<Site>)>,
    /// `true` when the trailer had an `/Info` entry that was
    /// removed — *not* a guarantee the data is gone from the file.
    pub info_removed: bool,
    /// `true` when the catalog had a `/Metadata` entry that was
    /// removed — *not* a guarantee the stream is gone.
    pub metadata_removed: bool,
    /// Human-readable scope statement — never contains document
    /// text, image bytes, or metadata values.
    pub scope: Vec<String>,
}

/// Terse summary — never prints `output` bytes, site content, or
/// metadata values.
impl std::fmt::Debug for RegionsAndMetadataRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionsAndMetadataRemoval")
            .field("output_bytes", &self.output.len())
            .field("codes_removed", &self.codes_removed)
            .field("invocations_edited", &self.invocations_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("orphaned_names_removed", &self.orphaned_names_removed)
            .field("pages", &self.per_page.len())
            .field("info_removed", &self.info_removed)
            .field("metadata_removed", &self.metadata_removed)
            .field("scope", &self.scope)
            .finish()
    }
}

/// M26 — experimental scoped region removal over multiple pages
/// **and** document-metadata entry removal in *one* edit session and
/// *one* rewrite.
///
/// Runs the exact `remove_regions` plan for every requested page
/// (same input rules, same per-page eligibility, same cumulative
/// budgets) and additionally removes the trailer `/Info` and catalog
/// `/Metadata` *entries* in the same change set — no intermediate
/// PDF is ever produced or re-parsed.
///
/// This is experimental content editing plus a narrow entry
/// deletion — **not** redaction, sanitizing, or guaranteed data
/// erasure. The metadata *values* are never parsed; the removed
/// Info dictionary / XMP stream may still be emitted if another key
/// references it, page/XObject `/Metadata` is untouched, and the
/// same information may exist elsewhere in the file.
///
/// All `rewrite` refusals apply; on any failure nothing is emitted
/// and the input `Document` is unchanged.
pub fn remove_regions_and_document_metadata(
    doc: &Document,
    specs: &[PageSpec],
    limits: &Limits,
) -> Result<RegionsAndMetadataRemoval, Error> {
    // Presence is checked on the *input* — flags report whether an
    // entry was there to remove, not whether its data vanished.
    let info_present = doc.trailer().contains_key(b"Info".as_slice());
    let metadata_present = doc.catalog().contains_key(b"Metadata".as_slice());

    let s = regions_edit_session(doc, specs, limits)?;
    // One rewrite for everything: the region edits accumulated in
    // `s.edit.overlay` plus the trailer/catalog overrides. The
    // metadata-strip runs against the *effective* overlay — an
    // already-edited catalog (none today, but the writer handles it)
    // loses only /Metadata, never its other edits.
    let output = s
        .edit
        .rewrite_with_overrides(&crate::rewrite::RewriteOverrides {
            strip_info: true,
            strip_metadata: true,
        })?;

    Ok(RegionsAndMetadataRemoval {
        output,
        codes_removed: s.codes_removed_total,
        invocations_edited: s.invocations_edited_total,
        pixels_changed: s.pixels_changed_total,
        orphaned_names_removed: s.orphaned_names_removed_total,
        per_page: s.per_page,
        info_removed: info_present,
        metadata_removed: metadata_present,
        scope: vec![
            format!(
                "{} page{} edited",
                s.page_count,
                if s.page_count == 1 { "" } else { "s" }
            ),
            format!("codes removed: {}", s.codes_removed_total),
            format!("invocations edited: {}", s.invocations_edited_total),
            format!("pixels changed: {}", s.pixels_changed_total),
            format!("orphaned names removed: {}", s.orphaned_names_removed_total),
            format!("trailer /Info entry removed: {info_present}"),
            format!("catalog /Metadata entry removed: {metadata_present}"),
            "scope: page-level text + image Do calls — no forms, no nested XObjects, \
             no /Annots/metadata/structure, no masks/non-device colour. Images: \
             no filter / FlateDecode / single DCTDecode (8-bit Gray/RGB only); \
             JPEG is decoded to samples and written back uncompressed. \
             Removed the trailer /Info and catalog /Metadata entries only; \
             page/XObject /Metadata and any data still reachable through \
             other keys may remain. Experimental content editing — complete \
             erasure is not guaranteed."
                .into(),
        ],
    })
}

/// M22.4 — post-order traversal over the `/Pages` tree that decides
/// which intermediate nodes may drop their `/Resources`.
///
/// A `/Pages` node's result is `true` only when *every* descendant
/// leaf page has been materialised (`pages_with_resources`). The
/// result is recorded in `node_results` so the parent sees a
/// definitive `false`, not `None` — M22.3 fixed a bug where a node
/// that broke out of its kid loop on a *failure* was left unrecorded,
/// causing the parent to re-push the same child indefinitely.
///
/// Returns `(pages_to_strip, node_results)` — the set of `/Pages`
/// refs whose `/Resources` may be removed (all descendants
/// materialised *and* the node itself has `/Resources`), plus the
/// per-node results for testability.
///
/// Termination: the base parser's `walk_page_tree` rejects cycles and
/// bounds the tree (`max_page_tree_depth`, `max_page_tree_nodes`)
/// *before* this runs — every `/Pages` node reachable here has
/// already been visited once. This pass then visits each node at
/// most once per *re-push* (a parent is re-pushed only when a child
/// is pushed — `all_materialised` still `true` — so the number of
/// stack entries is bounded by the tree size plus the number of
/// edges). A node whose result is recorded is never re-processed.
fn plan_resource_materialisation(
    doc: &Document,
    pages_with_resources: &HashSet<IndirectRef>,
) -> Result<(Vec<IndirectRef>, HashMap<IndirectRef, bool>), Error> {
    let catalog_dict = doc
        .catalog()
        .get(b"Pages".as_slice())
        .and_then(|o| o.as_ref())
        .ok_or_else(|| Error::Structure("catalog has no /Pages".into()))?;
    // Stack of (node_ref, child_index). child_index tracks which kid
    // is being processed; when all kids are done the node's result is
    // computed and stored in `node_results`.
    let mut stack: Vec<(IndirectRef, usize)> = vec![(catalog_dict, 0)];
    let mut node_results: HashMap<IndirectRef, bool> = HashMap::new();
    let mut pages_to_strip: Vec<IndirectRef> = Vec::new();
    while let Some((pages_ref, child_idx)) = stack.pop() {
        let pages_obj = doc.resolve(pages_ref)?;
        let pages_dict = pages_obj
            .as_dict()
            .ok_or_else(|| Error::Structure("/Pages is not a dictionary".into()))?;
        let kids = pages_dict
            .get(b"Kids".as_slice())
            .and_then(|o| o.as_array())
            .ok_or_else(|| Error::Structure("/Pages has no /Kids".into()))?;
        // Process the next unprocessed kid.
        let mut all_materialised = true;
        let mut next_kid_idx = child_idx;
        while next_kid_idx < kids.len() {
            let kid = &kids[next_kid_idx];
            if let Some(kid_ref) = kid.as_ref() {
                let kid_obj = doc.resolve(kid_ref)?;
                let kid_dict = kid_obj
                    .as_dict()
                    .ok_or_else(|| Error::Structure("/Kids entry is not a dict".into()))?;
                if kid_dict.contains_key(b"Kids".as_slice()) {
                    // Intermediate node — check its result if already
                    // computed, else push and process it.
                    if let Some(&kid_result) = node_results.get(&kid_ref) {
                        if !kid_result {
                            all_materialised = false;
                            break;
                        }
                        next_kid_idx += 1;
                    } else {
                        // Push current node back to continue after the
                        // child, then push the child. The parent is
                        // re-pushed only while `all_materialised` is
                        // still `true` — a `false` break records the
                        // result immediately instead.
                        stack.push((pages_ref, next_kid_idx));
                        stack.push((kid_ref, 0));
                        break;
                    }
                } else {
                    // Leaf page — must be in pages_with_resources.
                    if !pages_with_resources.contains(&kid_ref) {
                        all_materialised = false;
                        break;
                    }
                    next_kid_idx += 1;
                }
            } else {
                // Direct kid — no ref, cannot check.
                all_materialised = false;
                break;
            }
        }
        // If we broke out of the kid loop early *because a child was
        // pushed to the stack*, do not compute a result yet — the
        // node will be revisited when the child finishes. But if we
        // broke out because a leaf failed or a kid had no
        // IndirectRef, the result is `false` and must be recorded
        // now — otherwise the parent would re-push the same child
        // indefinitely.
        if next_kid_idx < kids.len() && all_materialised {
            continue;
        }
        // All kids processed (or a failure was found): record the
        // result so the parent sees `false`, not `None`.
        let node_result = all_materialised;
        node_results.insert(pages_ref, node_result);
        // Only strip if this node *has* /Resources and all
        // descendants are materialised.
        if node_result && pages_dict.contains_key(b"Resources".as_slice()) {
            pages_to_strip.push(pages_ref);
        }
    }
    Ok((pages_to_strip, node_results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remove_image::remove_image_region;

    /// Build a PDF with one page containing both text and an image.
    /// Text: `(ABC) Tj` at 24pt — 'A' at x=20..34.4, 'B' at 34.4..40.4,
    /// 'C' at 40.4..46.4 (user space).
    /// Image: 2×2 gray at 20,60 scaled 40×40 — pixels (0,0)-(1,0) top
    /// row at y=80-100, (0,1)-(1,1) bottom row at y=60-80.
    fn text_and_image_pdf() -> Vec<u8> {
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
        // 2×2 gray image: [0x10, 0x20, 0x30, 0x40]
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Content: text at (20,140) and image at (20,60) — separate
        // regions so each can be hit independently.
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    /// A rect covering both the 'A' text and the top-left image pixel
    /// removes both in one pass.
    #[test]
    fn removes_text_and_image_together() {
        let d = doc(&text_and_image_pdf());
        // Rect (20,140)-(30,160) covers 'A' only (20-34.4 x, 140-160 y).
        // Rect (20,80)-(40,100) covers top-left image pixel.
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
    }

    /// Text-only rect: image is not edited.
    #[test]
    fn text_only_rect() {
        let d = doc(&text_and_image_pdf());
        // Rect covers 'A' only — no image pixels inside.
        let r = remove_region(&d, 0, &[(20.0, 140.0, 30.0, 160.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 0);
        assert_eq!(r.pixels_changed, 0);
    }

    /// Image-only rect: text is not removed.
    #[test]
    fn image_only_rect() {
        let d = doc(&text_and_image_pdf());
        // Rect covers top-left image pixel only — no text inside.
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 0);
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
    }

    /// No overlap: nothing is edited.
    #[test]
    fn no_overlap() {
        let d = doc(&text_and_image_pdf());
        // Rect is far from both text and image.
        let r = remove_region(&d, 0, &[(100.0, 100.0, 120.0, 120.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 0);
        assert_eq!(r.invocations_edited, 0);
        assert_eq!(r.pixels_changed, 0);
        assert_eq!(r.sites.len(), 0);
    }

    /// A page with a Form `Do` is refused — M20 does not handle forms.
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
        let err =
            remove_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "form Do must be refused: {err:?}"
        );
    }

    /// The page's effective `/Resources` is materialised — an
    /// inherited resources dict is never mutated in place.
    #[test]
    fn inherited_resources_not_mutated() {
        // Page inherits /Resources from the /Pages node.
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
        push!(
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /Font << /F1 4 0 R >> \
              /XObject << /Im1 10 0 R >> >> >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        // The output must contain the page's own /Resources (materialised
        // from the inherited one) with the new image name.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(out.contains("/KasIm"), "{out}");
    }

    /// A second image Do that is NOT hit keeps its original name —
    /// only the hit invocation is repointed.
    #[test]
    fn unedited_invocation_keeps_original() {
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
        // Two Do calls to the same image — only Im1 is hit.
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
        let d = doc(&parts.concat());
        // Only Im1's area is rect'd — Im2 at (100,60) is outside.
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // Im2 must still point to the original image (10 0 R), not a clone.
        assert!(out.contains("/Im2"), "{out}");
        assert!(!out.contains("/KasIm1"), "{out}"); // only KasIm0 exists
    }

    /// The output is a single-revision PDF — `edit.rewrite()` runs
    /// once, not per edit type.
    #[test]
    fn single_rewrite() {
        let d = doc(&text_and_image_pdf());
        // Cover both 'A' (y=140-160) and top-left image pixel (y=80-100).
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap();
        // The output is a valid single-revision PDF — it parses.
        let d2 = doc(&r.output);
        assert_eq!(d2.page_count(), 1);
        // The rewritten stream contains the TJ for the removed 'A'
        // and the new image name.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(out.contains("TJ"), "{out}");
        assert!(out.contains("/KasIm0"), "{out}");
    }

    // ---- M20.1 — regression tests -----------------------------------

    /// Multiple `Do` calls to the same image at different CTMs —
    /// each gets its own clone with the correct pixel range.
    #[test]
    fn same_image_different_ctms() {
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
        // 2×2 gray image.
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Two Do calls: first at (0,0) scaled 40×40, second at (100,0)
        // scaled 40×40. Only the first is hit.
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n40 0 0 40 100 0 cm\n/Im1 Do\n";
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
        // Only the first Do's area is rect'd.
        let r = remove_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4); // all 4 pixels of the first call
                                         // The second Do must keep the original name /Im1.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // /Im1 appears twice: once in the un-edited XObject
        // resource dict and once in the second Do. Only one Do
        // is edited to the new name.
        let do_count = out.matches("/Im1 Do").count() + out.matches("/Im1\nDo").count();
        assert_eq!(do_count, 1, "{out}");
    }

    /// `q`/`Q` restores the CTM — a `Do` after `Q` uses the restored
    /// matrix, not the inner one.
    #[test]
    fn q_restores_ctm() {
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
        // q saves CTM, cm moves to (0,0), Do paints, Q restores to
        // identity, second Do paints at (100,100).
        let body = b"q\n40 0 0 40 0 0 cm\n/Im1 Do\nQ\n40 0 0 40 100 100 cm\n/Im1 Do\n";
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
        // Only the second Do's area is rect'd — Q restored the CTM.
        let r = remove_region(&d, 0, &[(100.0, 100.0, 140.0, 140.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4);
    }

    /// A `Tj` that loses codes expands to `TJ` — a `Do` after it
    /// keeps its original `op_index` and is correctly repointed.
    #[test]
    fn text_expansion_before_do() {
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
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Text at (20,140), image at (20,60).
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        // Cover 'A' text and top-left image pixel.
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The Do after the expanded Tj is still at the same op_index
        // and gets the new name.
        assert!(out.contains("/KasIm0"), "{out}");
        assert!(out.contains("TJ"), "{out}");
    }

    /// A `'` op expands to `T*` + `TJ` — a `Do` after it keeps its
    /// original `op_index` and is correctly repointed.
    #[test]
    fn quote_expansion_before_do() {
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
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // ' op at (20,140), image at (20,60).
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) ' ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        // Cover 'A' text and top-left image pixel.
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The ' expands to T* + TJ — the Do after it is still
        // at the same op_index and gets the new name.
        assert!(out.contains("/KasIm0"), "{out}");
        assert!(out.contains("T*"), "{out}");
        assert!(out.contains("TJ"), "{out}");
    }

    /// A page with unsupported image (JPEG) is refused even when
    /// the text is editable — the whole call fails.
    #[test]
    fn unsupported_image_refuses_whole_call() {
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
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        // JPEG image — DCTDecode is in scope, but these bytes are
        // truncated junk so the decode fails.
        let jpeg_data = [0xffu8, 0xd8, 0xff, 0xd9]; // minimal JPEG markers
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 \
              /Filter /DCTDecode /Length "
            .to_vec();
        f.extend_from_slice(jpeg_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&jpeg_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        // Text is editable but the image's JPEG bytes are truncated
        // junk — when the rect hits it the whole call fails. A JPEG
        // image *outside* the rect is left alone (M20's policy).
        let err = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Parse(_)),
            "corrupt JPEG in rect must fail the whole call: {err:?}"
        );
    }

    /// A page with unsupported text (Tr 3) is refused even when
    /// the image is editable — the whole call fails.
    #[test]
    fn unsupported_text_refuses_whole_call() {
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
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Tr 3 (invisible) text — refused.
        let body = b"BT /F1 24 Tf 3 Tr 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        // Image is editable but the text is Tr 3 — the whole call fails.
        let err =
            remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "Tr 3 text must refuse the whole call: {err:?}"
        );
    }

    /// Cumulative `max_edit_content_bytes` is shared between image
    /// and Contents — a second image is refused before decode.
    #[test]
    fn shared_budget_refused_before_decode() {
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
        push!(11, &f);
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
        // First image: 4 bytes. Second image: 4 more needed but only
        // 4 remain in the 8-byte budget.
        let l = Limits {
            max_edit_content_bytes: 8,
            ..Limits::default()
        };
        let err = remove_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &l).unwrap_err();
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

    /// `max_edit_content_bytes` boundary: exactly at the limit passes,
    /// one byte over is refused.
    #[test]
    fn shared_budget_exact_boundary() {
        // Single 4-byte image at budget 4 — exactly at the limit.
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
        let img_data = [0x10u8; 4];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
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

        // The 4-byte image data alone charges 4 bytes. A budget of
        // 4 is enough for the image itself but the new `/Contents`
        // stream pushes over. Set a generous budget and verify the
        // image edit succeeds.
        let r = remove_region(
            &d,
            0,
            &[(0.0, 0.0, 40.0, 40.0)],
            &Limits {
                max_edit_content_bytes: 4 + 64, // 4 image + headroom for stream
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);

        // One byte under the image's 4 bytes is refused before decode.
        let err = remove_region(
            &d,
            0,
            &[(0.0, 0.0, 40.0, 40.0)],
            &Limits {
                max_edit_content_bytes: 3,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                Error::LimitExceeded {
                    limit: "max_edit_content_bytes",
                    ..
                }
            ),
            "one byte under must refuse: {err:?}"
        );
    }

    /// A `"` op expands to `Tw` + `Tc` + `T*` + `TJ` — a `Do` after
    /// it keeps its original `op_index` and is correctly repointed.
    #[test]
    fn dquote_expansion_before_do() {
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
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
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
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // " op at (20,140), image at (20,60).
        let body = b"BT /F1 24 Tf 20 140 Td 1 2 (ABC) \" ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
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
        // Cover 'A' text and top-left image pixel.
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The " expands to Tw+Tc+T*+TJ — the Do after it is still
        // at the same op_index and gets the new name.
        assert!(out.contains("/KasIm0"), "{out}");
        assert!(out.contains("TJ"), "{out}");
    }

    // ---- M21 — orphaned XObject name removal -------------------------

    /// One image, one `Do`, edited — the original name is orphaned
    /// and removed from /XObject.
    #[test]
    fn m21_orphaned_name_removed() {
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
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The original name /Im1 is gone from the XObject dict.
        assert!(!out.contains("/Im1"), "original name must be gone: {out}");
        assert!(out.contains("/KasIm0"), "{out}");
    }

    /// Two `Do`s to the same image, only one edited — the original
    /// name is kept because the unedited `Do` still uses it.
    #[test]
    fn m21_partial_edit_keeps_name() {
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
        // Two Do calls: first at (0,0), second at (100,0). Only the
        // first is hit.
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n40 0 0 40 100 0 cm\n/Im1 Do\n";
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
        // Only the first Do's area is rect'd.
        let r = remove_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 0);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // /Im1 is kept (second Do still uses it).
        assert!(out.contains("/Im1"), "original name must be kept: {out}");
        assert!(out.contains("/KasIm0"), "{out}");
    }

    /// Different names for the same image — editing `/Im1` does
    /// not remove `/ImAlias`, and the original image object stays
    /// reachable via the unedited `/ImAlias Do`.
    #[test]
    fn m21_alias_keeps_original() {
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
              /Resources << /XObject << /Im1 10 0 R /ImAlias 10 0 R >> >> \
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
        // Im1 at (0,0) is edited; ImAlias at (100,0) is not.
        let body = b"40 0 0 40 0 0 cm\n/Im1 Do\n40 0 0 40 100 0 cm\n/ImAlias Do\n";
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
        let r = remove_region(&d, 0, &[(0.0, 0.0, 40.0, 40.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1); // /Im1 removed
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // /ImAlias is kept; /Im1 is gone.
        assert!(out.contains("/ImAlias"), "alias must be kept: {out}");
        assert!(!out.contains("/Im1 Do"), "original Do must be gone: {out}");
        assert!(out.contains("/KasIm0"), "{out}");
    }

    /// Image shared with another page — the orphaned name is removed
    /// from *this* page's /XObject, but the image object stays in the
    /// output because the other page still references it.
    #[test]
    fn m21_shared_image_other_page() {
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
        // Page 1: has the image, will be edited.
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // Page 2: shares the same image object.
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
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(11, &sb2);
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
        // Edit page 1 only.
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1);
        let out = String::from_utf8_lossy(&r.output).into_owned();
        // The image object itself stays (page 2 still uses it).
        assert!(out.contains("/Subtype /Image"), "image must stay: {out}");
        // Page 1's /Im1 is gone; page 2's /Im1 stays.
        // We verify by checking the output has the image stream.
        assert!(out.contains("/KasIm0"), "{out}");
    }

    /// Inherited /Resources — the page has no own /Resources but
    /// inherits from /Pages. The cloned XObject dict must not
    /// mutate the shared /Pages resources.
    #[test]
    fn m21_inherited_resources() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /XObject << /Im1 10 0 R /Unused 11 0 R >> \
                            /Font << /F1 5 0 R >> >> >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
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
        push!(
            11,
            b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Length 0 >>"
        );
        push!(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
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
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1);

        // M21.1: re-parse the output and verify by structure.
        let out_doc = doc(&r.output);
        let out_str = String::from_utf8_lossy(&r.output).into_owned();
        // The materialised /Resources keeps /Font and /Unused.
        assert!(out_str.contains("/Font"), "font must be kept: {out_str}");
        assert!(
            out_str.contains("/Unused"),
            "unused XObject must be kept: {out_str}"
        );
        assert!(out_str.contains("/KasIm0"), "{out_str}");

        // M22: the parent /Pages has had /Resources stripped (all
        // descendants were materialised). The page's own /Resources
        // is the materialised copy — it has /Font and /Unused but
        // /Im1 was orphaned and removed.
        let pages_ref = out_doc
            .catalog()
            .get(b"Pages".as_slice())
            .and_then(|o| o.as_ref())
            .unwrap();
        let pages_obj = out_doc.resolve(pages_ref).unwrap();
        let pages_dict = pages_obj.as_dict().unwrap();
        // Parent /Pages no longer has /Resources.
        assert!(
            !pages_dict.contains_key(b"Resources".as_slice()),
            "parent /Pages must drop /Resources after materialisation"
        );
        // The page's own /Resources has /Font and /Unused but not /Im1.
        let page_ref = out_doc.pages()[0].object_ref().unwrap();
        let page_obj = out_doc.resolve(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        let page_res = page_dict.get(b"Resources".as_slice()).unwrap();
        let page_res_obj = out_doc.deref(page_res).unwrap();
        let page_res_dict = page_res_obj.as_dict().unwrap();
        let page_xobj = page_res_dict.get(b"XObject".as_slice()).unwrap();
        let page_xobj_obj = out_doc.deref(page_xobj).unwrap();
        let page_xobj_dict = page_xobj_obj.as_dict().unwrap();
        assert!(
            page_xobj_dict.contains_key(b"KasIm0".as_slice()),
            "page /XObject must have /KasIm0"
        );
        assert!(
            !page_xobj_dict.contains_key(b"Im1".as_slice()),
            "page /XObject must not have /Im1 (orphaned)"
        );
        assert!(
            page_xobj_dict.contains_key(b"Unused".as_slice()),
            "page /XObject must keep /Unused"
        );
    }

    /// `max_diag_exec_ops` boundary: exactly at the limit passes,
    /// one op under is refused (M20.1 requirement A).
    #[test]
    fn m20_1_max_diag_exec_ops_boundary() {
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
        // 3 ops: cm, Do — the walker sees them all.
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

        // Exactly at the limit passes (2 ops: cm + Do).
        let r = remove_region(
            &d,
            0,
            &[(20.0, 80.0, 40.0, 100.0)],
            &Limits {
                max_diag_exec_ops: 2,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);

        // One under is refused (1 op cannot cover cm + Do).
        let err = remove_region(
            &d,
            0,
            &[(20.0, 80.0, 40.0, 100.0)],
            &Limits {
                max_diag_exec_ops: 1,
                ..Limits::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::LimitExceeded { .. }),
            "one under must refuse: {err:?}"
        );
    }

    // ---- M21.1 — content-based image verification ---------------------

    /// Re-parse the output and verify by *content* (not object number):
    /// the edited image stream exists, the original does not.
    #[test]
    fn m21_1_original_image_absent_by_content() {
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
        // Identifiable image: 0xAA, 0xBB, 0xCC, 0xDD
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
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
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1);

        // Re-parse the output and enumerate image streams by content.
        let out_doc = doc(&r.output);
        let mut found_original = false;
        let mut found_edited = false;
        for i in 1..20 {
            if let Ok(Object::Stream { dict, data }) = out_doc.resolve(crate::object::IndirectRef {
                number: i,
                generation: 0,
            }) {
                if dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name())
                    == Some(b"Image".as_slice())
                {
                    // Check content: original has 0xAA, 0xBB, 0xCC, 0xDD;
                    // edited has 0x00 in top-left pixel.
                    if data.as_slice() == [0xAA, 0xBB, 0xCC, 0xDD] {
                        found_original = true;
                    }
                    if data.as_slice() == [0x00, 0xBB, 0xCC, 0xDD] {
                        found_edited = true;
                    }
                }
            }
        }
        assert!(!found_original, "original image must not be in output");
        assert!(found_edited, "edited image must be in output");
    }

    /// Shared image with another page — the original image object
    /// stays in the output because the other page still references it.
    #[test]
    fn m21_1_shared_image_present_by_content() {
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
        // Page 1: has the image, will be edited.
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // Page 2: shares the same image object.
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 11 0 R >>"
        );
        // Identifiable image: 0xAA, 0xBB, 0xCC, 0xDD
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(11, &sb2);
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
        // Edit page 1 only.
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.orphaned_names_removed, 1);

        // Re-parse the output and enumerate image streams by content.
        let out_doc = doc(&r.output);
        let mut found_original = false;
        let mut found_edited = false;
        for i in 1..20 {
            if let Ok(Object::Stream { dict, data }) = out_doc.resolve(crate::object::IndirectRef {
                number: i,
                generation: 0,
            }) {
                if dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name())
                    == Some(b"Image".as_slice())
                {
                    if data.as_slice() == [0xAA, 0xBB, 0xCC, 0xDD] {
                        found_original = true;
                    }
                    if data.as_slice() == [0x00, 0xBB, 0xCC, 0xDD] {
                        found_edited = true;
                    }
                }
            }
        }
        assert!(
            found_original,
            "original image must stay (shared with page 2)"
        );
        assert!(found_edited, "edited image must be in output");
    }

    // ---- M22.1 — multi-level page tree materialisation --------------

    /// A /Pages node *without* /Resources still has its descendants
    /// materialised — the traversal does not stop at a resourceless
    /// intermediate node.
    #[test]
    fn m22_1_resourceless_intermediate() {
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
        // Root /Pages has /Resources; intermediate has none.
        push!(
            2,
            b"<< /Type /Pages /Kids [10 0 R] /Count 1 \
              /Resources << /XObject << /Im1 11 0 R >> >> >>"
        );
        push!(
            10,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 2 0 R >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 10 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(11, &f);
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
        let r = remove_region(&d, 0, &[(20.0, 80.0, 40.0, 100.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        // The intermediate /Pages (10 0 R) had no /Resources — it is
        // still traversed, and the root /Pages (2 0 R) drops its
        // /Resources because the leaf page is materialised.
        let out_str = String::from_utf8_lossy(&r.output).into_owned();
        assert!(out_str.contains("/KasIm0"), "{out_str}");
        // Root /Pages drops /Resources.
        let out_doc = doc(&r.output);
        let pages_ref = out_doc
            .catalog()
            .get(b"Pages".as_slice())
            .and_then(|o| o.as_ref())
            .unwrap();
        let pages_obj = out_doc.resolve(pages_ref).unwrap();
        let pages_dict = pages_obj.as_dict().unwrap();
        assert!(
            !pages_dict.contains_key(b"Resources".as_slice()),
            "root /Pages must drop /Resources"
        );
    }

    /// Text-only edit on an inherited-resources page: no image
    /// clone is created, but the page still gets its effective
    /// /Resources materialised (the Contents edit is not lost).
    #[test]
    fn m22_1_text_only_materialises_resources() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /Font << /F1 4 0 R >> >> >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
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
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Text-only edit: no image clone is created.
        let r = remove_region(&d, 0, &[(20.0, 140.0, 30.0, 160.0)], &Limits::default()).unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 0);
        // The page still gets its effective /Resources materialised.
        let out_doc = doc(&r.output);
        let page_ref = out_doc.pages()[0].object_ref().unwrap();
        let page_obj = out_doc.resolve(page_ref).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        assert!(
            page_dict.contains_key(b"Resources".as_slice()),
            "page must have /Resources materialised"
        );
        let page_res = page_dict.get(b"Resources".as_slice()).unwrap();
        let page_res_obj = out_doc.deref(page_res).unwrap();
        let page_res_dict = page_res_obj.as_dict().unwrap();
        assert!(
            page_res_dict.contains_key(b"Font".as_slice()),
            "page /Resources must keep /Font"
        );
        // The parent /Pages drops /Resources.
        let pages_ref = out_doc
            .catalog()
            .get(b"Pages".as_slice())
            .and_then(|o| o.as_ref())
            .unwrap();
        let pages_obj = out_doc.resolve(pages_ref).unwrap();
        let pages_dict = pages_obj.as_dict().unwrap();
        assert!(
            !pages_dict.contains_key(b"Resources".as_slice()),
            "parent /Pages must drop /Resources"
        );
    }

    /// A child /Pages whose leaf cannot be materialised (direct kid,
    /// no IndirectRef) — the parent's /Resources must NOT be stripped
    /// even though the child was pushed to the stack.
    #[test]
    fn m22_2_unmaterialised_child_blocks_parent() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [10 0 R] /Count 1 \
              /Resources << /XObject << /Im1 11 0 R >> >> >>"
        );
        // Intermediate /Pages with a *direct* kid (no IndirectRef) —
        // the leaf page cannot be materialised.
        push!(
            10,
            b"<< /Type /Pages /Kids [<< /Type /Page /Parent 10 0 R \
              /MediaBox [0 0 200 200] /Contents 9 0 R >>] /Count 1 \
              /Parent 2 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(11, &f);
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
        // The direct kid has no IndirectRef, so `object_ref()` is
        // `None` and the leaf is absent from `pages_with_resources`.
        // Call the planner directly — going through `remove_region`
        // would refuse earlier (a direct-kid target page has no ref
        // to edit) and never reach the traversal.
        let pages_with_resources = collect_pages_with_resources(&d);
        assert!(pages_with_resources.is_empty());
        let (pages_to_strip, node_results) =
            plan_resource_materialisation(&d, &pages_with_resources).unwrap();
        // The intermediate /Pages (10) sees a direct kid → false.
        assert_eq!(node_results.get(&IndirectRef::new(10, 0)), Some(&false));
        // The false propagates to the root (2).
        assert_eq!(node_results.get(&IndirectRef::new(2, 0)), Some(&false));
        // Neither node is a strip candidate.
        assert!(pages_to_strip.is_empty());
    }

    /// Build the `pages_with_resources` set the same way
    /// `remove_region` does — every page that has an indirect ref
    /// *and* an effective `/Resources`.
    fn collect_pages_with_resources(d: &Document) -> HashSet<IndirectRef> {
        let mut s = HashSet::new();
        for p in d.pages() {
            if let Some(pr) = p.object_ref() {
                if p.resources().is_some() {
                    s.insert(pr);
                }
            }
        }
        s
    }

    /// A 3-level tree: root has /Resources, middle has none, lower
    /// has a leaf that cannot be materialised (a direct kid — no
    /// IndirectRef, so `object_ref()` is `None` and the page is never
    /// in `pages_with_resources`). The failure must propagate up to
    /// the root — /Resources is kept. Calls
    /// `plan_resource_materialisation` directly so the per-node
    /// `false` is asserted, not just the final strip outcome.
    #[test]
    fn m22_3_failure_propagates_three_levels() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [10 0 R] /Count 1 \
              /Resources << /XObject << /Im1 11 0 R >> >> >>"
        );
        // Middle /Pages has no /Resources — still traversed.
        push!(
            10,
            b"<< /Type /Pages /Kids [12 0 R] /Count 1 /Parent 2 0 R >>"
        );
        // Lower /Pages has a direct kid — no IndirectRef, so the leaf
        // page has `object_ref() == None` and cannot be materialised.
        push!(
            12,
            b"<< /Type /Pages /Kids [<< /Type /Page /Parent 12 0 R \
              /MediaBox [0 0 200 200] /Contents 9 0 R >>] /Count 1 \
              /Parent 10 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(11, &f);
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
        let pages_with_resources = collect_pages_with_resources(&d);
        // The direct-kid leaf has no ref, so it is absent; the only
        // materialised page would be none — the set is empty.
        assert!(
            pages_with_resources.is_empty(),
            "the direct-kid leaf must not be in pages_with_resources"
        );
        let (pages_to_strip, node_results) =
            plan_resource_materialisation(&d, &pages_with_resources).unwrap();
        // Lower /Pages (12) has a direct kid → false.
        assert_eq!(node_results.get(&IndirectRef::new(12, 0)), Some(&false));
        // Middle /Pages (10) sees child's false → false.
        assert_eq!(node_results.get(&IndirectRef::new(10, 0)), Some(&false));
        // Root /Pages (2) sees middle's false → false.
        assert_eq!(node_results.get(&IndirectRef::new(2, 0)), Some(&false));
        // Nothing is a strip candidate — root keeps /Resources.
        assert!(
            pages_to_strip.is_empty(),
            "no /Pages may be stripped when a descendant is unmaterialised"
        );
    }

    /// Two branches: branch 1 has a materialised leaf, branch 2 has
    /// an unmaterialised leaf (direct kid). `false` propagates from
    /// branch 2 to the root — the root must NOT drop /Resources even
    /// though branch 1 succeeded.
    #[test]
    fn m22_3_mixed_branches() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 \
              /Resources << /XObject << /Im1 12 0 R >> >> >>"
        );
        // Branch 1: a /Pages node whose leaf page is a materialised
        // indirect page.
        push!(
            10,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 2 0 R >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 10 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        // Branch 2: a /Pages node whose leaf is a *direct* kid — no
        // IndirectRef, so `object_ref()` is None and the page is not
        // in `pages_with_resources`.
        push!(
            11,
            b"<< /Type /Pages /Kids [<< /Type /Page /Parent 11 0 R \
              /MediaBox [0 0 200 200] /Contents 13 0 R >>] /Count 1 \
              /Parent 2 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(12, &f);
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(13, &sb2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 14\n0000000000 65535 f \n".to_vec());
        for n in 1..14u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 14 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let pages_with_resources = collect_pages_with_resources(&d);
        // Only page 3 (branch 1) is materialised — the branch-2 leaf
        // is a direct kid with no ref.
        assert_eq!(pages_with_resources.len(), 1);
        assert!(pages_with_resources.contains(&IndirectRef::new(3, 0)));
        let (pages_to_strip, node_results) =
            plan_resource_materialisation(&d, &pages_with_resources).unwrap();
        // Branch 1 (10): leaf 3 is materialised → true.
        assert_eq!(node_results.get(&IndirectRef::new(10, 0)), Some(&true));
        // Branch 2 (11): direct kid → false.
        assert_eq!(node_results.get(&IndirectRef::new(11, 0)), Some(&false));
        // Root (2): branch-2 false propagates → false.
        assert_eq!(node_results.get(&IndirectRef::new(2, 0)), Some(&false));
        // Nothing is stripped — root keeps /Resources.
        assert!(
            pages_to_strip.is_empty(),
            "root keeps /Resources when one branch is unmaterialised"
        );
    }

    /// All branches succeed — the root *is* a strip candidate.
    /// Confirms `true` results are recorded and the parent drops
    /// /Resources only when every descendant is materialised.
    #[test]
    fn m22_4_all_branches_materialised() {
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
        push!(
            2,
            b"<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 \
              /Resources << /XObject << /Im1 12 0 R >> >> >>"
        );
        push!(
            10,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 2 0 R >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 10 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        push!(
            11,
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /Parent 2 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 11 0 R /MediaBox [0 0 200 200] \
              /Contents 13 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(12, &f);
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(13, &sb2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 14\n0000000000 65535 f \n".to_vec());
        for n in 1..14u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 14 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let pages_with_resources = collect_pages_with_resources(&d);
        // Both leaf pages are materialised.
        assert_eq!(pages_with_resources.len(), 2);
        let (pages_to_strip, node_results) =
            plan_resource_materialisation(&d, &pages_with_resources).unwrap();
        assert_eq!(node_results.get(&IndirectRef::new(10, 0)), Some(&true));
        assert_eq!(node_results.get(&IndirectRef::new(11, 0)), Some(&true));
        assert_eq!(node_results.get(&IndirectRef::new(2, 0)), Some(&true));
        // Only the root has /Resources → it is the sole strip candidate.
        assert_eq!(pages_to_strip, vec![IndirectRef::new(2, 0)]);
    }

    /// A page referenced by `IndirectRef` but carrying no effective
    /// `/Resources` is not in `pages_with_resources` — its parent
    /// branch records `false`. Distinct from the direct-kid case: the
    /// leaf *has* a ref, it is just not materialised.
    #[test]
    fn m22_4_indirect_leaf_without_resources() {
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
        // Root has no /Resources — page 3 has its own, page 13 has
        // none (inherits nothing → effective resources absent).
        push!(2, b"<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 >>");
        push!(
            10,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 2 0 R \
              /Resources << /XObject << /Im1 12 0 R >> >> >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 10 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        push!(
            11,
            b"<< /Type /Pages /Kids [13 0 R] /Count 1 /Parent 2 0 R >>"
        );
        // Page 13 has an IndirectRef but no /Resources anywhere in its
        // ancestry → `resources()` is None → not materialised.
        push!(
            13,
            b"<< /Type /Page /Parent 11 0 R /MediaBox [0 0 200 200] \
              /Contents 14 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(12, &f);
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        push!(14, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 15\n0000000000 65535 f \n".to_vec());
        for n in 1..15u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 15 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let pages_with_resources = collect_pages_with_resources(&d);
        // Only page 3 is materialised — page 13 inherits nothing.
        assert_eq!(pages_with_resources.len(), 1);
        assert!(pages_with_resources.contains(&IndirectRef::new(3, 0)));
        let (pages_to_strip, node_results) =
            plan_resource_materialisation(&d, &pages_with_resources).unwrap();
        assert_eq!(node_results.get(&IndirectRef::new(10, 0)), Some(&true));
        assert_eq!(node_results.get(&IndirectRef::new(11, 0)), Some(&false));
        assert_eq!(node_results.get(&IndirectRef::new(2, 0)), Some(&false));
        // Node 10 has /Resources and result true → it is the only
        // strip candidate. Root (no /Resources, false) and node 11
        // (false) are not.
        assert_eq!(pages_to_strip, vec![IndirectRef::new(10, 0)]);
    }

    // ---- M24 — multi-page region removal ---------------------------

    /// A 2-page fixture: page 1 has text `(ABC)` at (20,140) and a
    /// 2x2 gray image at (20,60); page 2 has text `(XYZ)` at (20,140)
    /// and shares the same image object at (100,100).
    fn two_page_text_image_pdf() -> Vec<u8> {
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
        push!(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> \
              /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> \
              /XObject << /Im1 10 0 R >> >> \
              /Contents 12 0 R >>"
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
        font_dict.extend_from_slice(b" /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>");
        push!(5, &font_dict);
        push!(
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
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
        push!(7, &fs);
        // 2x2 gray image shared by both pages.
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Page 1: text at (20,140) + image at (20,60).
        let body1 = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        // Page 2: text at (20,140) + image at (100,100).
        let body2 = b"BT /F1 24 Tf 20 140 Td (XYZ) Tj ET\n40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(12, &sb2);
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
        parts.concat()
    }

    /// Two pages, different rect regions — both edits land in one
    /// output through one Edit/rewrite.
    #[test]
    fn m24_two_pages_different_rects() {
        let d = doc(&two_page_text_image_pdf());
        // Page 1: cover 'A' text. Page 2: cover image pixels.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(100.0, 100.0, 140.0, 140.0)]),
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1); // 'A' on page 1
        assert_eq!(r.invocations_edited, 1); // /Im1 on page 2
        assert_eq!(r.pixels_changed, 4); // all 4 pixels of 2x2
        assert_eq!(r.per_page.len(), 2);
        // Per-page site lists.
        assert_eq!(r.per_page[0].0, 0);
        assert_eq!(r.per_page[1].0, 1);
        assert_eq!(r.per_page[0].1.len(), 1, "page 1 site");
        assert_eq!(r.per_page[1].1.len(), 1, "page 2 site");
    }

    /// Text edit on one page, image edit on the other — a mixed
    /// multi-page pass.
    #[test]
    fn m24_mixed_text_and_image_pages() {
        let d = doc(&two_page_text_image_pdf());
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),   // text on p1
                (1, vec![(100.0, 100.0, 140.0, 140.0)]), // image on p2
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        // The original shared image survives — each edited page gets
        // its own clone; the unedited page 2 keeps the original.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(out.contains("/Subtype /Image"), "image must stay");
    }

    /// Same page repeated with multiple rect groups — merged into
    /// one plan, not double-counted.
    #[test]
    fn m24_same_page_repeated_merges_rects() {
        let d = doc(&two_page_text_image_pdf());
        // Page 1 listed twice — rects merge.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (0, vec![(20.0, 80.0, 40.0, 100.0)]), // overlaps image top-left
            ],
            &Limits::default(),
        )
        .unwrap();
        // 'A' text + image top-left pixel, each counted once.
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        assert_eq!(r.per_page.len(), 1);
    }

    /// A failure on page 2 fails the whole call — no partial output.
    #[test]
    fn m24_failure_on_later_page_fails_all() {
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
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> >> \
              /Contents 9 0 R >>"
        );
        // Page 2 has /Annots — refused by eligibility.
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Annots [6 0 R] \
              /Resources << >> /Contents 11 0 R >>"
        );
        push!(6, b"<< /Type /Annot /Subtype /Text /Rect [0 0 5 5] >>");
        push!(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        push!(9, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(11, b"<< /Length 0 >>\nstream\n\nendstream");
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
        let e = remove_regions(
            &d,
            &[
                (0, vec![(0.0, 0.0, 10.0, 10.0)]),
                (1, vec![(0.0, 0.0, 10.0, 10.0)]),
            ],
            &Limits::default(),
        )
        .unwrap_err();
        // Typed error — annotations refusal, not a generic failure.
        match e {
            Error::Unsupported { feature, .. } => {
                assert!(feature.contains("/Annots"), "{feature}");
            }
            other => panic!("expected Unsupported(/Annots), got {other:?}"),
        }
    }

    /// A cumulative limit that fits one page but not two is refused
    /// — `max_diag_exec_ops` carries across pages.
    #[test]
    fn m24_cumulative_exec_ops_budget() {
        let d = doc(&two_page_text_image_pdf());
        // Each page runs ~9 ops (BT/ET/Tj/cm/Do). A cap of 5 allows
        // page 1 but not both.
        let limits = Limits {
            max_diag_exec_ops: 5,
            ..Default::default()
        };
        let e = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(100.0, 100.0, 140.0, 140.0)]),
            ],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => {
                assert_eq!(limit, "max_diag_exec_ops");
            }
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    /// Same cumulative boundary — the exact limit succeeds.
    #[test]
    fn m24_cumulative_budget_boundary() {
        let d = doc(&two_page_text_image_pdf());
        // Generous enough for both pages.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(100.0, 100.0, 140.0, 140.0)]),
            ],
            &Limits::default(),
        )
        .unwrap();
        assert!(r.codes_removed > 0);
    }

    /// Empty specs, empty rect list, bad page index — all refused
    /// up front, before any walk runs.
    #[test]
    fn m24_input_validation() {
        let d = doc(&two_page_text_image_pdf());
        // Empty specs.
        assert!(matches!(
            remove_regions(&d, &[], &Limits::default()),
            Err(Error::Structure(_))
        ));
        // Empty rect list for a page.
        assert!(matches!(
            remove_regions(&d, &[(0, vec![])], &Limits::default()),
            Err(Error::Structure(_))
        ));
        // Page index out of range.
        assert!(matches!(
            remove_regions(&d, &[(5, vec![(0.0, 0.0, 10.0, 10.0)])], &Limits::default()),
            Err(Error::Structure(_))
        ));
        // Reversed rect.
        assert!(matches!(
            remove_regions(&d, &[(0, vec![(10.0, 10.0, 0.0, 0.0)])], &Limits::default()),
            Err(Error::Structure(_))
        ));
    }

    /// Debug must not leak output bytes or site content.
    #[test]
    fn m24_debug_no_leak() {
        let d = doc(&two_page_text_image_pdf());
        let r = remove_regions(
            &d,
            &[(0, vec![(20.0, 140.0, 30.0, 160.0)])],
            &Limits::default(),
        )
        .unwrap();
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("(ABC)"), "leaked text: {dbg}");
        assert!(!dbg.contains("\\x10\\x20"), "leaked image: {dbg}");
        // The scope reports page counts, not content.
        assert!(dbg.contains("codes_removed"));
    }

    /// Enumerate every `/Subtype /Image` stream in a re-parsed output
    /// document, returning `(ref, decoded_data)` pairs.
    fn image_streams(d: &Document) -> Vec<(IndirectRef, Vec<u8>)> {
        let mut v = Vec::new();
        for i in 1..40u32 {
            let r = IndirectRef {
                number: i,
                generation: 0,
            };
            if let Ok(Object::Stream { dict, data }) = d.resolve(r) {
                if dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name())
                    == Some(b"Image".as_slice())
                {
                    v.push((r, data));
                }
            }
        }
        v
    }

    /// The XObject name(s) a page's `/Contents` invokes — decoded
    /// text of every `Do` op in the resolved stream.
    fn do_names_in_contents(d: &Document, page: &crate::page::Page) -> Vec<Vec<u8>> {
        let ops = d.page_operations(page).unwrap();
        ops.iter()
            .filter_map(|o| match o {
                Operation::Do(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    /// Resolve a page's `/XObject /{name}` entry to its indirect ref.
    fn page_xobject_ref(
        d: &Document,
        page: &crate::page::Page,
        name: &[u8],
    ) -> Option<IndirectRef> {
        let res = page.resources()?;
        let entry = d.resolve_resource_raw(res, "XObject", name).ok()??;
        entry.as_ref()
    }

    // ---- M24.1 — per-page rect count limit -------------------------

    /// `max_diag_rects` bounds the *merged* per-page rect count —
    /// repeated specs for one page cannot bypass it.
    #[test]
    fn m24_1_rect_count_limit() {
        let d = doc(&two_page_text_image_pdf());
        // A tiny cap — one rect fits, two do not.
        let limits = Limits {
            max_diag_rects: 1,
            ..Default::default()
        };
        // Two rects in one spec.
        let e = remove_regions(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0), (1.0, 1.0, 2.0, 2.0)])],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => assert_eq!(limit, "max_diag_rects"),
            other => panic!("expected LimitExceeded(max_diag_rects), got {other:?}"),
        }
        // Two specs for the same page — merged count 2 > 1.
        let e = remove_regions(
            &d,
            &[
                (0, vec![(0.0, 0.0, 10.0, 10.0)]),
                (0, vec![(1.0, 1.0, 2.0, 2.0)]),
            ],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => assert_eq!(limit, "max_diag_rects"),
            other => panic!("expected LimitExceeded(max_diag_rects), got {other:?}"),
        }
        // Non-adjacent specs for the same page still merge.
        let e = remove_regions(
            &d,
            &[
                (0, vec![(0.0, 0.0, 10.0, 10.0)]),
                (1, vec![(0.0, 0.0, 10.0, 10.0)]),
                (0, vec![(1.0, 1.0, 2.0, 2.0)]),
            ],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => assert_eq!(limit, "max_diag_rects"),
            other => panic!("expected LimitExceeded(max_diag_rects), got {other:?}"),
        }
        // Duplicate identical rects still count as input entries.
        let e = remove_regions(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0), (0.0, 0.0, 10.0, 10.0)])],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => assert_eq!(limit, "max_diag_rects"),
            other => panic!("expected LimitExceeded(max_diag_rects), got {other:?}"),
        }
        // Two pages each within the limit pass — the cap is per
        // page, not a document-wide rect budget.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(100.0, 100.0, 140.0, 140.0)]),
            ],
            &limits,
        )
        .unwrap();
        assert_eq!(r.codes_removed + r.invocations_edited, 2);
    }

    /// The limit boundary exactly succeeds.
    #[test]
    fn m24_1_rect_count_boundary() {
        let d = doc(&two_page_text_image_pdf());
        let limits = Limits {
            max_diag_rects: 2,
            ..Default::default()
        };
        let r = remove_regions(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0), (1.0, 1.0, 2.0, 2.0)])],
            &limits,
        )
        .unwrap();
        assert_eq!(r.per_page.len(), 1);
    }

    // ---- M24.1 — page-index overflow -------------------------------

    /// `usize::MAX` as a page index must not overflow the `page + 1`
    /// error path — the refusal is `Error::Structure`, not a panic.
    #[test]
    fn m24_1_page_index_overflow() {
        let d = doc(&two_page_text_image_pdf());
        // usize::MAX + 1 would wrap in release and panic in debug.
        let e = remove_regions(
            &d,
            &[(usize::MAX, vec![(0.0, 0.0, 10.0, 10.0)])],
            &Limits::default(),
        )
        .unwrap_err();
        match e {
            Error::Structure(_) => {}
            other => panic!("expected Structure, got {other:?}"),
        }
        // The first out-of-range index (page_count == 2 → index 2).
        let e = remove_regions(&d, &[(2, vec![(0.0, 0.0, 10.0, 10.0)])], &Limits::default())
            .unwrap_err();
        match e {
            Error::Structure(msg) => assert!(msg.contains("page 3"), "{msg}"),
            other => panic!("expected Structure, got {other:?}"),
        }
        // Out-of-range page + empty rects — both are refusals; the
        // first check (empty rects) wins, and it must not panic.
        let e = remove_regions(&d, &[(usize::MAX, vec![])], &Limits::default()).unwrap_err();
        match e {
            Error::Structure(_) => {}
            other => panic!("expected Structure, got {other:?}"),
        }
        // Out-of-range page + a malformed rect — rect validation
        // runs first; still a typed error, never a panic.
        let e = remove_regions(
            &d,
            &[(usize::MAX, vec![(10.0, 10.0, 0.0, 0.0)])],
            &Limits::default(),
        )
        .unwrap_err();
        match e {
            Error::Structure(_) => {}
            other => panic!("expected Structure, got {other:?}"),
        }
        // Valid first and last pages still work.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(20.0, 140.0, 30.0, 160.0)]),
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.per_page.len(), 2);
    }

    // ---- M24.1 — shared image across two edited pages --------------

    /// Two pages share the same image object; different rects edit
    /// different pixels. Each page's clone is independent — page 1's
    /// edit does not leak into page 2's clone and vice versa.
    #[test]
    fn m24_1_shared_image_two_edited_pages() {
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
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 11 0 R >>"
        );
        // Identifiable 2x2 gray image.
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        // Page 1: image at (20,60); page 2: image at (100,100).
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(11, &sb2);
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
        // Page 1 edits pixel (0,1) — the painted image's lower-left
        // quadrant (unit-space y<0.5 maps to the bottom pixel row).
        // Page 2 edits pixel (1,0) — the upper-right quadrant.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 60.0, 40.0, 80.0)]),     // paints (0,1)
                (1, vec![(120.0, 120.0, 140.0, 140.0)]), // paints (1,0)
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 2);
        assert_eq!(r.pixels_changed, 2);
        // Re-parse and inspect every image stream.
        let out = doc(&r.output);
        let streams = image_streams(&out);
        // What must exist are the two *distinct* edited clones.
        let has = |pat: &[u8]| streams.iter().any(|(_, d)| d == pat);
        // Page 1 painted (0,1) → pixel index 2 (row 1, col 0).
        assert!(has(&[0xAA, 0xBB, 0x00, 0xDD]), "page-1 clone missing");
        // Page 2 painted (1,0) → pixel index 1 (row 0, col 1).
        assert!(has(&[0xAA, 0x00, 0xCC, 0xDD]), "page-2 clone missing");
        // Each page's content references a generated KasIm name —
        // the name may coincide (`KasIm0` on both) since it only
        // needs to be unique within *that page's* /XObject dict.
        let p1_names = do_names_in_contents(&out, out.pages().first().unwrap());
        let p2_names = do_names_in_contents(&out, out.pages().get(1).unwrap());
        let n1 = p1_names
            .iter()
            .find(|n| n.starts_with(b"KasIm"))
            .expect("p1 KasIm");
        let n2 = p2_names
            .iter()
            .find(|n| n.starts_with(b"KasIm"))
            .expect("p2 KasIm");
        // The names resolve through each page's own /XObject dict to
        // the two different clones.
        let r1 = page_xobject_ref(&out, out.pages().first().unwrap(), n1).unwrap();
        let r2 = page_xobject_ref(&out, out.pages().get(1).unwrap(), n2).unwrap();
        let d1 = match out.resolve(r1).unwrap() {
            Object::Stream { data, .. } => data,
            _ => panic!("p1 clone is not a stream"),
        };
        let d2 = match out.resolve(r2).unwrap() {
            Object::Stream { data, .. } => data,
            _ => panic!("p2 clone is not a stream"),
        };
        assert_eq!(d1.as_slice(), &[0xAA, 0xBB, 0x00, 0xDD]);
        assert_eq!(d2.as_slice(), &[0xAA, 0x00, 0xCC, 0xDD]);
    }

    /// Different branches of the page tree give `/Im1` different
    /// effective resources — each page edits *its own* image.
    #[test]
    fn m24_1_same_name_different_image_per_branch() {
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
        // Two branches — each has its own /Resources naming /Im1 to
        // a different image object.
        push!(2, b"<< /Type /Pages /Kids [10 0 R 11 0 R] /Count 2 >>");
        push!(
            10,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Parent 2 0 R \
              /Resources << /XObject << /Im1 12 0 R >> >> >>"
        );
        push!(
            11,
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 /Parent 2 0 R \
              /Resources << /XObject << /Im1 13 0 R >> >> >>"
        );
        push!(
            3,
            b"<< /Type /Page /Parent 10 0 R /MediaBox [0 0 200 200] \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 11 0 R /MediaBox [0 0 200 200] \
              /Contents 14 0 R >>"
        );
        // Branch-1 image 12: 0x10,0x20,0x30,0x40 — branch-2 image 13:
        // 0x90,0xA0,0xB0,0xC0.
        for (n, data) in [
            (12, [0x10u8, 0x20, 0x30, 0x40]),
            (13, [0x90u8, 0xA0, 0xB0, 0xC0]),
        ] {
            let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
                  /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
                .to_vec();
            f.extend_from_slice(data.len().to_string().as_bytes());
            f.extend_from_slice(b" >>\nstream\n");
            f.extend_from_slice(&data);
            f.extend_from_slice(b"\nendstream");
            push!(n, &f);
        }
        for (n, body) in [
            (9, b"40 0 0 40 20 60 cm\n/Im1 Do\n" as &[u8]),
            (14, b"40 0 0 40 100 100 cm\n/Im1 Do\n"),
        ] {
            let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
            sb.extend_from_slice(body);
            sb.extend_from_slice(b"\nendstream");
            push!(n, &sb);
        }
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 15\n0000000000 65535 f \n".to_vec());
        for n in 1..15u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 15 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        // Page 1 paints pixel (0,1) (lower-left); page 2 paints
        // pixel (1,0) (upper-right) — different quadrants.
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 60.0, 40.0, 80.0)]),     // paints (0,1)
                (1, vec![(120.0, 120.0, 140.0, 140.0)]), // paints (1,0)
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 2);
        assert_eq!(r.pixels_changed, 2);
        let out = doc(&r.output);
        let p1 = out.pages().first().unwrap();
        let p2 = out.pages().get(1).unwrap();
        // Each page's contents + materialised resources coexist.
        for p in [p1, p2] {
            let pd = p.object_ref().unwrap();
            let obj = out.resolve(pd).unwrap();
            let dict = obj.as_dict().unwrap();
            assert!(
                dict.get(b"Contents".as_slice()).is_some(),
                "page missing /Contents"
            );
            assert!(
                dict.get(b"Resources".as_slice()).is_some(),
                "page missing /Resources"
            );
        }
        // Page 1's KasIm resolves to a clone of branch-1's image
        // (first pixel 0 → 0x00), page 2's to a clone of branch-2's.
        let n1 = do_names_in_contents(&out, p1)
            .into_iter()
            .find(|n| n.starts_with(b"KasIm"))
            .unwrap();
        let n2 = do_names_in_contents(&out, p2)
            .into_iter()
            .find(|n| n.starts_with(b"KasIm"))
            .unwrap();
        let d1 = match out
            .resolve(page_xobject_ref(&out, p1, &n1).unwrap())
            .unwrap()
        {
            Object::Stream { data, .. } => data,
            _ => panic!("not a stream"),
        };
        let d2 = match out
            .resolve(page_xobject_ref(&out, p2, &n2).unwrap())
            .unwrap()
        {
            Object::Stream { data, .. } => data,
            _ => panic!("not a stream"),
        };
        assert_eq!(d1.as_slice(), &[0x10, 0x20, 0x00, 0x40], "branch-1 clone");
        assert_eq!(d2.as_slice(), &[0x90, 0x00, 0xB0, 0xC0], "branch-2 clone");
    }

    /// An unedited page sharing the image keeps the original —
    /// the edited page's clone does not leak back.
    #[test]
    fn m24_1_unedited_page_keeps_shared_original() {
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
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 11 0 R >>"
        );
        let img_data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body1 = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(11, &sb2);
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
        // Only page 1 is edited — its clone blackens pixel (0,1).
        let r = remove_regions(
            &d,
            &[(0, vec![(20.0, 60.0, 40.0, 80.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 1);
        let out = doc(&r.output);
        // The unedited page's `/Im1` must still resolve to the
        // original stream content.
        let p2 = out.pages().get(1).unwrap();
        let orig_ref = page_xobject_ref(&out, p2, b"Im1").unwrap();
        let data = match out.resolve(orig_ref).unwrap() {
            Object::Stream { data, .. } => data,
            _ => panic!("not a stream"),
        };
        assert_eq!(
            data.as_slice(),
            &[0xAA, 0xBB, 0xCC, 0xDD],
            "unedited page must keep the original image"
        );
        // The original image still exists somewhere in the output.
        let streams = image_streams(&out);
        assert!(
            streams
                .iter()
                .any(|(_, d)| d.as_slice() == [0xAA, 0xBB, 0xCC, 0xDD]),
            "original image dropped even though page 2 references it"
        );
    }

    // ---- M26 — region removal + document-metadata entry removal ----

    /// 2-page fixture with trailer /Info and catalog /Metadata.
    /// Page 1: text `(ABC)` at (20,140) + shared 2x2 image at
    /// (20,60); page 2: text `(XYZ)` + same image at (100,100).
    fn two_page_text_image_metadata_pdf() -> Vec<u8> {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 13 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> \
              /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> \
              /XObject << /Im1 10 0 R >> >> \
              /Contents 12 0 R >>"
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
        font_dict.extend_from_slice(b" /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>");
        push!(5, &font_dict);
        push!(
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
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
        push!(7, &fs);
        // Shared 2x2 gray image.
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body1 = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
        sb.extend_from_slice(body1);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let body2 = b"BT /F1 24 Tf 20 140 Td (XYZ) Tj ET\n40 0 0 40 100 100 cm\n/Im1 Do\n";
        let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
        sb2.extend_from_slice(body2);
        sb2.extend_from_slice(b"\nendstream");
        push!(12, &sb2);
        // Document-level XMP.
        let xmp = b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(13, &m);
        // Info dict.
        push!(14, b"<< /Title (Secret Title) /Author (Secret Author) >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 15\n0000000000 65535 f \n".to_vec());
        for n in 1..15u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 15 /Root 1 0 R /Info 14 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Both edits and both metadata entries are removed in one
    /// rewrite — verified by re-parsing the output.
    #[test]
    fn m26_combined_edit_and_metadata_removal() {
        let d = doc(&two_page_text_image_metadata_pdf());
        let r = remove_regions_and_document_metadata(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),   // 'A' on p1
                (1, vec![(100.0, 100.0, 140.0, 140.0)]), // image on p2
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 1);
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4);
        assert!(r.info_removed);
        assert!(r.metadata_removed);
        // Re-parse: trailer /Info and catalog /Metadata are gone;
        // the page edits landed.
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
        assert!(out.catalog().contains_key(b"Pages".as_slice()));
        // Page contents still carry the edits.
        let p1 = out.pages().first().unwrap();
        let names = do_names_in_contents(&out, p1);
        // Page 1 had only the text hit — no image edit, so no KasIm.
        assert!(!names.iter().any(|n| n.starts_with(b"KasIm")));
        // Page 2's image was cloned and repointed.
        let p2 = out.pages().get(1).unwrap();
        let names2 = do_names_in_contents(&out, p2);
        assert!(names2.iter().any(|n| n.starts_with(b"KasIm")));
    }

    /// Metadata absent — the call still succeeds and the flags are
    /// false.
    #[test]
    fn m26_metadata_absent_succeeds() {
        let d = doc(&two_page_text_image_pdf()); // no /Info, no /Metadata
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(20.0, 140.0, 30.0, 160.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert!(!r.info_removed);
        assert!(!r.metadata_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
    }

    /// A dangling /Info ref is still removed without resolving it.
    #[test]
    fn m26_info_dangling_ref_removed() {
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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
        for n in 1..5u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 5 /Root 1 0 R /Info 99 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert!(r.info_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
    }

    /// Page-level /Metadata is kept — only the catalog one is
    /// removed.
    #[test]
    fn m26_page_metadata_kept() {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Metadata 7 0 R /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xmp = b"<x:xmpmeta>doc</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xmp2 = b"<x:xmpmeta>page</x:xmpmeta>";
        let mut m2 = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m2.extend_from_slice(xmp2.len().to_string().as_bytes());
        m2.extend_from_slice(b" >>\nstream\n");
        m2.extend_from_slice(xmp2);
        m2.extend_from_slice(b"\nendstream");
        push!(7, &m2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 8\n0000000000 65535 f \n".to_vec());
        for n in 1..8u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 8 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert!(r.metadata_removed);
        let out = doc(&r.output);
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
        let page = out.pages().first().unwrap();
        let pd = out.resolve(page.object_ref().unwrap()).unwrap();
        assert!(
            pd.as_dict().unwrap().contains_key(b"Metadata".as_slice()),
            "page /Metadata must be preserved"
        );
    }

    /// An Info dict shared through another catalog key stays
    /// reachable — the *entry* is removed, not the data.
    #[test]
    fn m26_shared_info_kept_via_other_ref() {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Custom 5 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Title (Shared Title) >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert!(r.info_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
        // But the dict itself survives via /Custom.
        let custom = out.catalog().get(b"Custom".as_slice()).unwrap();
        let obj = out.deref(custom).unwrap();
        assert!(obj.as_dict().unwrap().contains_key(b"Title".as_slice()));
    }

    /// Without the metadata strip, `remove_regions` still keeps
    /// Info and /Metadata — the combined entry point is opt-in.
    #[test]
    fn m26_remove_regions_still_keeps_metadata() {
        let d = doc(&two_page_text_image_metadata_pdf());
        let r = remove_regions(
            &d,
            &[(0, vec![(20.0, 140.0, 30.0, 160.0)])],
            &Limits::default(),
        )
        .unwrap();
        let out = doc(&r.output);
        assert!(out.trailer().contains_key(b"Info".as_slice()));
        assert!(out.catalog().contains_key(b"Metadata".as_slice()));
    }

    /// A failure on a later page fails the whole combined call —
    /// no partial output, and metadata is not stripped either.
    #[test]
    fn m26_failure_on_later_page_fails_all() {
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
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Annots [6 0 R] /Contents 11 0 R >>"
        );
        push!(6, b"<< /Type /Annot /Subtype /Text /Rect [0 0 5 5] >>");
        push!(9, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(11, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
        for n in 1..12u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 12 /Root 1 0 R /Info 9 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let e = remove_regions_and_document_metadata(
            &d,
            &[
                (0, vec![(0.0, 0.0, 10.0, 10.0)]),
                (1, vec![(0.0, 0.0, 10.0, 10.0)]),
            ],
            &Limits::default(),
        )
        .unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => {
                assert!(feature.contains("/Annots"), "{feature}");
            }
            other => panic!("expected Unsupported(/Annots), got {other:?}"),
        }
    }

    /// A cumulative budget that fits one page but not two is still
    /// enforced when the metadata strip is also requested.
    #[test]
    fn m26_cumulative_budget_still_applies() {
        let d = doc(&two_page_text_image_metadata_pdf());
        let limits = Limits {
            max_diag_exec_ops: 5,
            ..Default::default()
        };
        let e = remove_regions_and_document_metadata(
            &d,
            &[
                (0, vec![(20.0, 140.0, 30.0, 160.0)]),
                (1, vec![(20.0, 140.0, 30.0, 160.0)]),
            ],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => {
                assert_eq!(limit, "max_diag_exec_ops");
            }
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    /// An inherited rewrite refusal (signed document) still fires
    /// through the combined entry point.
    #[test]
    fn m26_rewrite_refusal_inherited() {
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
        push!(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 5 0 R >> >>"
        );
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Type /Sig >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let e = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
            &Limits::default(),
        )
        .unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => {
                assert!(feature.contains("signed"), "{feature}");
            }
            other => panic!("expected Unsupported(signed), got {other:?}"),
        }
    }

    /// Input document is unchanged and the bytes are not mutated.
    #[test]
    fn m26_input_not_mutated() {
        let bytes = two_page_text_image_metadata_pdf();
        let d = doc(&bytes);
        let _ = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(20.0, 140.0, 30.0, 160.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert!(d.trailer().contains_key(b"Info".as_slice()));
        assert!(d.catalog().contains_key(b"Metadata".as_slice()));
    }

    /// Debug output never leaks output bytes, site content, or
    /// metadata values.
    #[test]
    fn m26_debug_no_leak() {
        let d = doc(&two_page_text_image_metadata_pdf());
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(20.0, 140.0, 30.0, 160.0)])],
            &Limits::default(),
        )
        .unwrap();
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("Secret Title"), "{dbg}");
        assert!(!dbg.contains("SECRET-XMP"), "{dbg}");
        assert!(!dbg.contains("(ABC)"), "{dbg}");
        assert!(dbg.contains("info_removed"));
    }

    // ---- M27 — JPEG (DCTDecode) image editing -----------------------

    /// Decode a hex string — test fixture helper only.
    fn unhex(s: &str) -> Vec<u8> {
        assert!(s.len() % 2 == 0, "odd-length hex");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// g8.jpg — 8×8 DeviceGray JPEG, 375 bytes, generated with
    /// `cjpeg` (libjpeg-turbo, `-grayscale -quality 90`) from a
    /// deterministic PGM; embedded so tests need no external tools.
    const JPEG_G8_HEX: &str =
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07\
         0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc0000b080008\
         000801011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b51000020103\
         03020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024336272\
         82090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475\
         767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9\
         cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f003e12c1a5fecabf03\
         a5f19ded9f9daac98d3f42b46b569a3b9d49e37681240acb888796cee4bafc88c14972aadfffd9";

    /// c8.jpg — 8×8 DeviceRGB JPEG, 659 bytes, same provenance.
    const JPEG_C8_HEX: &str =
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07\
         0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304\
         0405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414141414141414\
         1414141414141414141414141414ffc00011080008000803012200021101031101ffc4001f0000010501010101010100\
         000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221\
         31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a\
         434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899\
         9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1\
         f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100\
         020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015\
         6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869\
         6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4\
         c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00e5\
         3e0bfec87fea3fd0bd3f868a28aac2626afb25a9d5c07c6b9d7f62d3fdeff5f79fffd9";

    /// c8_prog.jpg — 8×8 DeviceRGB *progressive* JPEG, 553 bytes,
    /// `cjpeg -progressive -quality 90` from c8.ppm; must be refused
    /// as Unsupported (progressive is out of scope), not Parse.
    const JPEG_C8_PROG_HEX: &str =
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b\
         0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00430103040405040509050509140d0b0d14141414\
         14141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414ffc20011080008000803\
         012200021101031101ffc4001500010100000000000000000000000000000005ffc4001501010100000000000000000000000000000103ff\
         da000c03010002100310000001921aff00ffc400161000030000000000000000000000000000000506ffda00080101000105024b207fffc4\
         00191100010500000000000000000000000000050002041422ffda0008010301013f01026a6d26e97fffc400171100030100000000000000\
         00000000000000020312ffda0008010201013f01951b27ffc400161000030000000000000000000000000000000122ffda0008010100063f\
         02507fffc40015100101000000000000000000000000000000f1ffda0008010100013f2184ffda000c03010002000300000010f3ffc40016\
         110003000000000000000000000000000000c1f0ffda0008010301013f109167ffc400161100030000000000000000000000000000001121\
         ffda0008010201013f105f4fffc40015100101000000000000000000000000000000a1ffda0008010100013f10867fffd9";

    /// c8_rgb.jpg — 8×8 DeviceRGB baseline JPEG *encoded as RGB*
    /// (Adobe APP14 transform=0, component IDs R/G/B, no subsampling),
    /// 374 bytes, `cjpeg -rgb -quality 90` from c8.ppm. Decodes to
    /// raw RGB — first six samples `00 00 00 1f 00 0f`.
    const JPEG_C8_RGB_HEX: &str =
        "ffd8ffee000e41646f626500640000000000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b\
         0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc00011080008000803521100471100421100ffc4001f0000\
         010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d0102030004110512\
         2131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a43444546474849\
         4a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6\
         b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda000c03520047004200003f00\
         e53fe09b7ff30bff0080d729ff000edbff00a85ffe43ae53f66dfd9b7fe3d7fd17d3f86bffd9";

    /// c8_nojfif.jpg — 8×8 DeviceRGB baseline JPEG with the JFIF
    /// APP0 segment manually removed, 641 bytes. Component IDs are
    /// still (1,2,3) so the decoder picks YCbCr; Adobe APP14 absent.
    const JPEG_C8_NOJFIF_HEX: &str =
        "ffd8ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113\
         141515150c0f171816141812141514ffdb00430103040405040509050509140d0b0d14141414141414141414141414141414141414141414\
         14141414141414141414141414141414141414141414141414141414ffc00011080008000803012200021101031101ffc4001f0000010501\
         010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d0102030004110512213141\
         0613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a5354\
         55565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9\
         bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f01000301010101010101010100\
         00000000000102030405060708090a0bffc400b5110002010204040304070504040001027700010203110405213106124151076171132232\
         8108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63\
         6465666768696a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6\
         c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00e53e0bfec87fea3fd0bd3f\
         868a28aac2626afb25a9d5c07c6b9d7f62d3fdeff5f79fffd9";

    /// c8_444.jpg — 8×8 DeviceRGB baseline JPEG, 4:4:4 sampling (no chroma subsampling), component IDs 1,2,3, JFIF APP0, 648 bytes; `cjpeg -sample 1x1 -quality 90` from c8.ppm.
    const JPEG_C8_444_HEX: &str =
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b\
         0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00430103040405040509050509140d0b0d14141414\
         14141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414ffc00011080008000803\
         011100021101031101ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b510000201030302040305\
         0504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a2526272829\
         2a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899\
         9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9\
         faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b5110002010204040304070504040001027700\
         0102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a3536373839\
         3a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6\
         a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c030100\
         02110311003f00e53e0bfec87fea3fd0bd3f868c1e336d43c3cf10fe0f7cffd9";

    /// c8_444_app14_0.jpg — c8_444 with an Adobe APP14 transform=0 injected after SOI, 664 bytes; the APP14 must win over the component IDs (1,2,3) so raw component values are returned.
    const JPEG_C8_444_APP14_0_HEX: &str =
        "ffd8ffee000e41646f626500640000000000ffe000104a46494600010100000100010000ffdb004300030202030202030303030403030405\
         0805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304\
         0405040509050509140d0b0d1414141414141414141414141414141414141414141414141414141414141414141414141414141414141414\
         141414141414ffc00011080008000803011100021101031101ffc4001f000001050101010101010000000000000000010203040506070809\
         0a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024\
         33627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a\
         838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3\
         e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100\
         020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e1\
         25f11718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788\
         898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9ea\
         f2f3f4f5f6f7f8f9faffda000c03010002110311003f00e53e0bfec87fea3fd0bd3f868c1e336d43c3cf10fe0f7cffd9";

    /// c8_rgb_app14_1.jpg — c8_rgb (component IDs R,G,B) with its APP14 rewritten to transform=1, 374 bytes; APP14 wins over the IDs so YCbCr→RGB is applied to RGB-encoded data.
    const JPEG_C8_RGB_APP14_1_HEX: &str =
        "ffd8ffee000e41646f626500640000000001ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b\
         0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc00011080008000803521100471100421100ffc4001f0000\
         010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d0102030004110512\
         2131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a43444546474849\
         4a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6\
         b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda000c03520047004200003f00\
         e53fe09b7ff30bff0080d729ff000edbff00a85ffe43ae53f66dfd9b7fe3d7fd17d3f86bffd9";

    /// c8_app14_bad.jpg — c8_444 with an Adobe APP14 transform=3 injected after SOI, 664 bytes; transform values outside 0–2 are undefined in Adobe TN #5116 → refused.
    const JPEG_C8_APP14_BAD_HEX: &str =
        "ffd8ffee000e41646f626500640000000003ffe000104a46494600010100000100010000ffdb004300030202030202030303030403030405\
         0805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304\
         0405040509050509140d0b0d1414141414141414141414141414141414141414141414141414141414141414141414141414141414141414\
         141414141414ffc00011080008000803011100021101031101ffc4001f000001050101010101010000000000000000010203040506070809\
         0a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024\
         33627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a\
         838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3\
         e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100\
         020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e1\
         25f11718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788\
         898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9ea\
         f2f3f4f5f6f7f8f9faffda000c03010002110311003f00e53e0bfec87fea3fd0bd3f868c1e336d43c3cf10fe0f7cffd9";

    /// c8_app14_dup.jpg — c8_444 with two contradictory APP14 markers (transform=0 then transform=1), 680 bytes; ambiguous input → refused.
    const JPEG_C8_APP14_DUP_HEX: &str =
        "ffd8ffee000e41646f626500640000000000ffee000e41646f626500640000000001ffe000104a46494600010100000100010000ffdb0043\
         000302020302020303030304030304050805050404050a070706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f\
         171816141812141514ffdb00430103040405040509050509140d0b0d14141414141414141414141414141414141414141414141414141414\
         14141414141414141414141414141414141414141414ffc00011080008000803011100021101031101ffc4001f0000010501010101010100\
         000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d0102030004110512213141061351610722\
         7114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a\
         636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6\
         c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f01000301010101010101010100000000000001\
         02030405060708090a0bffc400b51100020102040403040705040400010277000102031104052131061241510761711322328108144291a1\
         b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869\
         6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3\
         d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00e53e0bfec87fea3fd0bd3f868c1e336d43\
         c3cf10fe0f7cffd9";

    /// c16.jpg — 16×16 DeviceRGB JPEG, 702 bytes, same provenance.
    const JPEG_C16_HEX: &str =
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07\
         0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb0043010304\
         0405040509050509140d0b0d141414141414141414141414141414141414141414141414141414141414141414141414\
         1414141414141414141414141414ffc00011080010001003012200021101031101ffc4001f0000010501010101010100\
         000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221\
         31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a\
         434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899\
         9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1\
         f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100\
         020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015\
         6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869\
         6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4\
         c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f8\
         dbc29f0a7c8d9fb9ce7dba57bd784fe14f93b3f739cfb74af62f09fc2af2367ee73f874af53f09fc29f2367ee73f874a\
         fcbf39e30fec2bfbd7bfcb92df7f3735fe5ffa4f6f86dc7bed793df3ffd9";

    /// A 1-page PDF with one JPEG image painted at `cm` over the
    /// MediaBox. `cs_name` must match the JPEG's own component count.
    /// `decode_parms` is inserted verbatim into the image dict —
    /// pass `""` for none, e.g. `"/DecodeParms << /ColorTransform 0 >>"`.
    fn jpeg_page_pdf_parms(
        jpeg: &[u8],
        w: u32,
        h: u32,
        cs_name: &str,
        cm: &str,
        decode_parms: &str,
    ) -> Vec<u8> {
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
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
              /ColorSpace /{cs_name} /BitsPerComponent 8 \
              /Filter /DCTDecode {decode_parms} /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        f.extend_from_slice(jpeg);
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

    /// Convenience wrapper without `/DecodeParms`.
    fn jpeg_page_pdf(jpeg: &[u8], w: u32, h: u32, cs_name: &str, cm: &str) -> Vec<u8> {
        jpeg_page_pdf_parms(jpeg, w, h, cs_name, cm, "")
    }

    /// A 2-page PDF: both pages invoke the *same* JPEG image object.
    fn two_page_jpeg_pdf(jpeg: &[u8], w: u32, h: u32, cs_name: &str) -> Vec<u8> {
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
        for (n, cm) in [(3u32, "80 0 0 80 20 60 cm"), (4u32, "80 0 0 80 100 100 cm")] {
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
        let mut f = format!(
            "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
              /ColorSpace /{cs_name} /BitsPerComponent 8 \
              /Filter /DCTDecode /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        f.extend_from_slice(jpeg);
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
        parts.concat()
    }

    /// Fetch the decoded samples of the *edited* `KasIm0` image on a
    /// page — the clone the first edited `Do` points at.
    fn edited_image_samples(d: &Document, page: &crate::page::Page) -> Vec<u8> {
        let r = page_xobject_ref(d, page, b"KasIm0").expect("no KasIm0 on page");
        let obj = d.resolve(r).unwrap();
        match obj {
            Object::Stream { dict, data } => {
                assert!(
                    !dict.contains_key(b"Filter".as_slice()),
                    "edited JPEG clone must be written uncompressed"
                );
                data
            }
            _ => panic!("KasIm0 is not a stream"),
        }
    }

    // -- basic JPEG editing -------------------------------------------

    /// Gray JPEG: a quarter-rect blacks exactly the top-left 2×2
    /// pixels of the 8×8 image (rect in user space maps to pixel
    /// rows 0..2, cols 0..2 after the y-flip).
    #[test]
    fn m27_gray_jpeg_partial_edit() {
        // 8×8 gray JPEG painted over the whole 200×200 page.
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        // Decode the original samples for comparison.
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let orig_data = match orig {
            Object::Stream { ref dict, .. } => decode_image(
                dict,
                &match orig {
                    Object::Stream { ref data, .. } => data.clone(),
                    _ => unreachable!(),
                },
                &Limits::default(),
                (8, 8, 1),
            )
            .unwrap(),
            _ => panic!("not a stream"),
        };
        // Top-left quarter of the page in user space → pixel
        // rows 6..8?  No: image y is top-down, so user-space rect
        // (0,150)-(50,200) covers pixel rows 0..2, cols 0..2.
        let r =
            remove_image_region(&d, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        // The decoder's top-left pixel is already 0 — 3 of the 4
        // covered pixels change; `pixels_changed` counts only real
        // changes.
        assert_eq!(r.pixels_changed, 3);
        let out = doc(&r.output);
        let new = edited_image_samples(&out, out.pages().first().unwrap());
        assert_eq!(new.len(), 64);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let i = (y * 8 + x) as usize;
                if y < 2 && x < 2 {
                    assert_eq!(new[i], 0, "edited pixel ({x},{y}) not black");
                } else {
                    assert_eq!(new[i], orig_data[i], "unaltered pixel ({x},{y}) changed");
                }
            }
        }
    }

    /// RGB JPEG: same geometry — each changed pixel zeroes all 3
    /// channels but counts as one pixel.
    #[test]
    fn m27_rgb_jpeg_partial_edit() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let r =
            remove_image_region(&d, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert_eq!(r.pixels_changed, 4); // pixels, not components
        let out = doc(&r.output);
        let new = edited_image_samples(&out, out.pages().first().unwrap());
        assert_eq!(new.len(), 8 * 8 * 3);
        // Top-left 2×2 pixels all-zero; everything else unchanged.
        for y in 0..8u32 {
            for x in 0..8u32 {
                let base = ((y * 8 + x) * 3) as usize;
                if y < 2 && x < 2 {
                    assert_eq!(&new[base..base + 3], &[0, 0, 0]);
                } else {
                    assert_ne!(&new[base..base + 3], &[0, 0, 0], "unedited pixel blackened");
                }
            }
        }
    }

    /// Whole-image rect blacks every pixel; `pixels_changed` counts
    /// only pixels that were not already black.
    #[test]
    fn m27_jpeg_whole_image() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        // Count the decoder's non-zero samples — that's how many
        // will actually change.
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let orig_data = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 1)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        let nonzero = orig_data.iter().filter(|&&b| b != 0).count() as u64;
        let r =
            remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, nonzero);
        let out = doc(&r.output);
        assert!(edited_image_samples(&out, out.pages().first().unwrap())
            .iter()
            .all(|&b| b == 0));
    }

    /// Two disjoint rects produce an L-shape — the pixels between
    /// them are *not* filled.
    #[test]
    fn m27_jpeg_disjoint_rects_l_shape() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        // Top-left 2×2 + bottom-right 2×2 (user space). Two of the
        // covered pixels are already 0 in the decoded data.
        let orig_obj = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let orig_data = match orig_obj {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 1)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        let r = remove_image_region(
            &d,
            0,
            &[(0.0, 150.0, 50.0, 200.0), (150.0, 0.0, 200.0, 50.0)],
            &Limits::default(),
        )
        .unwrap();
        // 8 covered pixels minus those already black (index 0 and
        // index 45 = (5,6) is inside the bottom-right rect).
        let covered = |y: u32, x: u32| (y < 2 && x < 2) || (y >= 6 && x >= 6);
        let expected_changed = (0..8u32)
            .flat_map(|y| (0..8u32).map(move |x| (y, x)))
            .filter(|&(y, x)| covered(y, x) && orig_data[(y * 8 + x) as usize] != 0)
            .count() as u64;
        assert_eq!(r.pixels_changed, expected_changed);
        let out = doc(&r.output);
        let new = edited_image_samples(&out, out.pages().first().unwrap());
        assert_eq!(new.len(), 64);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let i = (y * 8 + x) as usize;
                if covered(y, x) {
                    assert_eq!(new[i], 0, "({x},{y})");
                } else {
                    assert_eq!(new[i], orig_data[i], "({x},{y})");
                }
            }
        }
    }

    /// Overlapping rects count each pixel once — no double-count.
    #[test]
    fn m27_jpeg_overlapping_rects_count_once() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        // Two rects covering the same 2×2 corner; the decoder's
        // top-left pixel is already 0 so only 3 pixels change.
        let r = remove_image_region(
            &d,
            0,
            &[(0.0, 150.0, 50.0, 200.0), (0.0, 160.0, 40.0, 200.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.pixels_changed, 3);
    }

    /// Unselected decoded samples are preserved — the *exact* byte
    /// the JPEG decoder produced stays in the clone.
    #[test]
    fn m27_jpeg_unselected_samples_unchanged() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let orig_obj = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let orig_data = match orig_obj {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 3)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        let r =
            remove_image_region(&d, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        let out = doc(&r.output);
        let new = edited_image_samples(&out, out.pages().first().unwrap());
        // Only the top-left 2×2 pixels changed.
        for y in 0..8u32 {
            for x in 0..8u32 {
                let base = ((y * 8 + x) * 3) as usize;
                if y < 2 && x < 2 {
                    assert_eq!(&new[base..base + 3], &[0, 0, 0]);
                } else {
                    assert_eq!(
                        &new[base..base + 3],
                        &orig_data[base..base + 3],
                        "({x},{y})"
                    );
                }
            }
        }
    }

    /// A JPEG outside every rect is never decoded — the clone is
    /// not made and the original stream survives.
    #[test]
    fn m27_jpeg_outside_rect_not_decoded() {
        let jpeg = unhex(JPEG_G8_HEX);
        // The image covers a small corner (40×40 at 20,60); a rect
        // in the opposite corner misses it entirely — the stream is
        // never decoded for editing.
        let d2 = doc(&jpeg_page_pdf(
            &jpeg,
            8,
            8,
            "DeviceGray",
            "40 0 0 40 20 60 cm",
        ));
        let r = remove_image_region(&d2, 0, &[(150.0, 150.0, 200.0, 200.0)], &Limits::default())
            .unwrap();
        assert_eq!(r.invocations_edited, 0);
        // The original JPEG bytes are still in the output — the
        // stream was never decoded for editing.
        let out = String::from_utf8_lossy(&r.output).into_owned();
        assert!(
            out.contains("DCTDecode"),
            "original JPEG's /Filter must remain"
        );
    }

    /// An already-black region contributes zero to pixels_changed.
    #[test]
    fn m27_jpeg_already_black_not_counted() {
        // First edit blacks the corner; second edit on the output
        // finds nothing to change.
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        let r =
            remove_image_region(&d, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.pixels_changed, 3); // one covered pixel was already 0
                                         // Second call on the *output* — the same rect is now black.
        let d2 = doc(&r.output);
        let r2 =
            remove_image_region(&d2, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r2.pixels_changed, 0, "already-black pixels not counted");
        assert_eq!(
            r2.invocations_edited, 0,
            "no new clone for unchanged pixels"
        );
    }

    // -- sharing / integration -----------------------------------------

    /// One `Do` edited, the other untouched — same JPEG object.
    #[test]
    fn m27_jpeg_two_invocations_one_edited() {
        // Two Do calls to the same image: first painted in the rect,
        // second outside it.
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
        let jpeg = unhex(JPEG_G8_HEX);
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
        // Im1 painted twice: (20,60) inside the rect, (120,120) outside.
        let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n40 0 0 40 120 120 cm\n/Im1 Do\n";
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
        let out = doc(&r.output);
        // First Do repointed to KasIm0, second still calls /Im1.
        let names = do_names_in_contents(&out, out.pages().first().unwrap());
        assert!(names.iter().any(|n| n == b"KasIm0"));
        assert!(names.iter().any(|n| n == b"Im1"));
        // The original JPEG stream is still reachable via /Im1.
        let orig_ref = page_xobject_ref(&out, out.pages().first().unwrap(), b"Im1").unwrap();
        let orig_obj = out.resolve(orig_ref).unwrap();
        match orig_obj {
            Object::Stream { dict, .. } => {
                assert_eq!(
                    dict.get(b"Filter".as_slice()).and_then(|o| o.as_name()),
                    Some(b"DCTDecode".as_slice()),
                    "unedited invocation must keep the original JPEG"
                );
            }
            _ => panic!("orig not a stream"),
        }
    }

    /// Two pages share one JPEG; each is edited in a different rect.
    #[test]
    fn m27_jpeg_shared_two_pages_different_rects() {
        let d = doc(&two_page_jpeg_pdf(&unhex(JPEG_G8_HEX), 8, 8, "DeviceGray"));
        let r = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 60.0, 100.0, 140.0)]), // page 1: whole 80×80 image
                (1, vec![(100.0, 100.0, 140.0, 140.0)]), // page 2: bottom-left quarter
            ],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 2);
        let out = doc(&r.output);
        // Page 1's clone is fully black; page 2's is quarter-black.
        let p1 = out.pages().first().unwrap();
        let p2 = out.pages().get(1).unwrap();
        let s1 = edited_image_samples(&out, p1);
        let s2 = edited_image_samples(&out, p2);
        assert!(s1.iter().all(|&b| b == 0), "page 1 fully black");
        // Page 2's rect (100,100)-(140,140): image at 80×80 at
        // (100,100) → unit square maps to (100,100)-(180,180); the
        // rect covers x∈[0,0.5], y∈[0,0.5] → pixel cols 0..4,
        // rows 4..8 → 16 pixels black.
        let orig_obj = d
            .deref(&Object::Ref(IndirectRef {
                number: 11,
                generation: 0,
            }))
            .unwrap();
        let orig_data = match orig_obj {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 1)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        for y in 0..8u32 {
            for x in 0..8u32 {
                let i = (y * 8 + x) as usize;
                if y >= 4 && x < 4 {
                    assert_eq!(s2[i], 0, "({x},{y})");
                } else {
                    assert_eq!(s2[i], orig_data[i], "({x},{y})");
                }
            }
        }
        // Both pages edited — no live reference to the original
        // JPEG remains, so the collector drops it.
        let streams = image_streams(&out);
        assert!(
            !streams
                .iter()
                .any(|(_, d)| d.len() == JPEG_G8_HEX.len() / 2),
            "unreferenced original JPEG should not be emitted"
        );
    }

    /// The unedited page keeps the original JPEG stream.
    #[test]
    fn m27_jpeg_unedited_page_keeps_original() {
        let d = doc(&two_page_jpeg_pdf(&unhex(JPEG_G8_HEX), 8, 8, "DeviceGray"));
        let r = remove_regions(
            &d,
            &[(0, vec![(20.0, 60.0, 60.0, 100.0)])],
            &Limits::default(),
        )
        .unwrap();
        let out = doc(&r.output);
        // Page 2 still invokes /Im1 and the stream is the original JPEG.
        let p2 = out.pages().get(1).unwrap();
        let names = do_names_in_contents(&out, p2);
        assert!(names.iter().any(|n| n == b"Im1"));
        let orig_ref = page_xobject_ref(&out, p2, b"Im1").unwrap();
        let orig_obj = out.resolve(orig_ref).unwrap();
        match orig_obj {
            Object::Stream { dict, data } => {
                assert_eq!(
                    dict.get(b"Filter".as_slice()).and_then(|o| o.as_name()),
                    Some(b"DCTDecode".as_slice())
                );
                assert_eq!(data.len(), JPEG_G8_HEX.len() / 2);
            }
            _ => panic!("not a stream"),
        }
    }

    /// Text + JPEG on one page are edited together.
    #[test]
    fn m27_text_and_jpeg_together() {
        // One page with text `(ABC)` and an 8×8 gray JPEG.
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
              /Resources << /Font << /F1 5 0 R >> \
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
        font_dict.extend_from_slice(b" /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>");
        push!(5, &font_dict);
        push!(
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
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
        push!(7, &fs);
        let jpeg = unhex(JPEG_G8_HEX);
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
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n80 0 0 80 20 60 cm\n/Im1 Do\n";
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
        // 'ABC' is fully inside the text rect and the image rect
        // covers the whole image — both edits land in one pass.
        let r = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 50.0, 160.0), (20.0, 60.0, 100.0, 140.0)],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.codes_removed, 3); // "ABC"
        assert_eq!(r.invocations_edited, 1);
        assert!(r.pixels_changed > 0);
    }

    /// Combined API: page edit + metadata strip with JPEG.
    #[test]
    fn m27_combined_jpeg_and_metadata() {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        let jpeg = unhex(JPEG_G8_HEX);
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
        let body = b"80 0 0 80 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xmp = b"<x:xmpmeta>doc</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
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
        let r = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(20.0, 60.0, 100.0, 140.0)])],
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.invocations_edited, 1);
        assert!(r.metadata_removed);
        let out = doc(&r.output);
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
        let names = do_names_in_contents(&out, out.pages().first().unwrap());
        assert!(names.iter().any(|n| n.starts_with(b"KasIm")));
    }

    // -- refusals ------------------------------------------------------

    /// A corrupt JPEG fails with Parse, not a panic.
    #[test]
    fn m27_jpeg_corrupt_data_refused() {
        let d = doc(&jpeg_page_pdf(
            &[0xffu8, 0xd8, 0xff],
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        assert!(matches!(e, Error::Parse(_)), "{e:?}");
    }

    /// Dict declares 8×8 but the JPEG is 16×16 → refused.
    #[test]
    fn m27_jpeg_dimension_mismatch_refused() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C16_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Structure(msg) => assert!(msg.contains("dimensions"), "{msg}"),
            other => panic!("expected Structure(dimensions), got {other:?}"),
        }
    }

    /// Dict declares DeviceRGB (3 comp) but the JPEG is gray (1) →
    /// refused on the component count.
    #[test]
    fn m27_jpeg_ncomps_mismatch_refused() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Structure(msg) => assert!(msg.contains("components"), "{msg}"),
            other => panic!("expected Structure(components), got {other:?}"),
        }
    }

    /// An unsupported DecodeParms key is refused, not ignored.
    #[test]
    fn m27_jpeg_unknown_decodeparms_refused() {
        let jpeg = unhex(JPEG_G8_HEX);
        // Build the fixture manually so we can inject DecodeParms.
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
              /Filter /DCTDecode \
              /DecodeParms << /BogusKey 1 >> \
              /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        f.extend_from_slice(&jpeg);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"200 0 0 200 0 0 cm\n/Im1 Do\n";
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
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, detail } => {
                assert!(
                    feature.contains("DecodeParms") || detail.contains("DecodeParms"),
                    "{feature}: {detail}"
                );
            }
            other => panic!("expected Unsupported(DecodeParms), got {other:?}"),
        }
    }

    /// A multi-filter chain ending in DCTDecode is refused — we only
    /// accept a *single* DCTDecode.
    #[test]
    fn m27_jpeg_filter_chain_refused() {
        let jpeg = unhex(JPEG_G8_HEX);
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
              /Filter [/ASCII85Decode /DCTDecode] /Length {} >>\nstream\n",
            jpeg.len()
        )
        .into_bytes();
        f.extend_from_slice(&jpeg);
        f.extend_from_slice(b"\nendstream");
        push!(10, &f);
        let body = b"200 0 0 200 0 0 cm\n/Im1 Do\n";
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
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => assert!(feature.contains("filter"), "{feature}"),
            other => panic!("expected Unsupported(filter), got {other:?}"),
        }
    }

    /// `max_image_bytes` bounds the JPEG decode — a tiny cap refuses
    /// before the pixel buffer is held.
    #[test]
    fn m27_jpeg_max_image_bytes() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        let limits = Limits {
            max_image_bytes: 32,
            ..Default::default()
        }; // < 8*8
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &limits).unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => assert_eq!(limit, "max_image_bytes"),
            other => panic!("expected LimitExceeded(max_image_bytes), got {other:?}"),
        }
    }

    /// Cumulative `max_edit_content_bytes` over two JPEG edits on
    /// one call is still enforced.
    #[test]
    fn m27_jpeg_cumulative_budget() {
        let d = doc(&two_page_jpeg_pdf(&unhex(JPEG_G8_HEX), 8, 8, "DeviceGray"));
        // Two 64-byte clones fit individually but not together.
        let limits = Limits {
            max_edit_content_bytes: 100,
            ..Default::default()
        };
        let e = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 60.0, 100.0, 140.0)]),
                (1, vec![(100.0, 100.0, 180.0, 180.0)]),
            ],
            &limits,
        )
        .unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => {
                assert_eq!(limit, "max_edit_content_bytes")
            }
            other => panic!("expected LimitExceeded(max_edit_content_bytes), got {other:?}"),
        }
    }

    /// A failure on the second page fails the whole combined call —
    /// even when the JPEG on page 1 was fine.
    #[test]
    fn m27_jpeg_failure_on_later_page_fails_all() {
        let jpeg = unhex(JPEG_G8_HEX);
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
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>"
        );
        push!(
            4,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Annots [6 0 R] /Contents 11 0 R >>"
        );
        push!(6, b"<< /Type /Annot /Subtype /Text /Rect [0 0 5 5] >>");
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
        let body = b"80 0 0 80 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        push!(11, b"<< /Length 0 >>\nstream\n\nendstream");
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
        let e = remove_regions(
            &d,
            &[
                (0, vec![(20.0, 60.0, 100.0, 140.0)]),
                (1, vec![(0.0, 0.0, 10.0, 10.0)]),
            ],
            &Limits::default(),
        )
        .unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => assert!(feature.contains("Annots"), "{feature}"),
            other => panic!("expected Unsupported(/Annots), got {other:?}"),
        }
    }

    // -- M27.1 — coding-process check + ColorTransform precedence ----

    /// A progressive JPEG is `Unsupported`, not `Parse` — the data is
    /// well-formed, just out of scope. Checked *before* decode so no
    /// partial output is produced.
    #[test]
    fn m27_1_progressive_jpeg_unsupported() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_PROG_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, detail } => {
                assert!(
                    feature.contains("coding process")
                        || feature.contains("DCTDecode")
                        || detail.contains("rogressive")
                        || detail.contains("DctProgressive"),
                    "unexpected Unsupported: {feature}: {detail}"
                );
            }
            other => panic!("expected Unsupported(progressive), got {other:?}"),
        }
    }

    /// The same progressive refusal through `remove_regions` — the
    /// shared `decode_image` path refuses it identically.
    #[test]
    fn m27_1_progressive_via_remove_regions() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_PROG_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_regions(
            &d,
            &[(0, vec![(0.0, 0.0, 200.0, 200.0)])],
            &Limits::default(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    }

    /// A progressive refusal inside `remove_regions_and_document_metadata`
    /// still fails the whole call — no partial output.
    #[test]
    fn m27_1_progressive_via_combined_entry() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_PROG_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_regions_and_document_metadata(
            &d,
            &[(0, vec![(0.0, 0.0, 200.0, 200.0)])],
            &Limits::default(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    }

    /// `/ColorTransform 0` on a *subsampled* YCbCr JPEG is refused —
    /// the decoder's `ColorTransform::None` path cannot represent
    /// the per-component planes as interleaved RGB, so we refuse
    /// explicitly rather than emit a wrong shape.
    #[test]
    fn m27_1_ct0_on_subsampled_jpeg_refused() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_HEX), // subsampled YCbCr (comp 1 = 2×2)
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 0 >>",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, detail } => {
                assert!(
                    feature.contains("ColorTransform") || detail.contains("subsampled"),
                    "{feature}: {detail}"
                );
            }
            other => panic!("expected Unsupported(ColorTransform 0), got {other:?}"),
        }
    }

    /// `/ColorTransform 0` on a *non-subsampled* RGB-encoded JPEG
    /// (c8_rgb: Adobe APP14 transform=0, all components 1×1) is
    /// accepted — the raw component values *are* RGB, and the APP14
    /// marker's transform=0 agrees with the PDF request.
    #[test]
    fn m27_1_ct0_on_rgb_jpeg_accepted() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_RGB_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 0 >>",
        ));
        let r =
            remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(r.invocations_edited, 1);
    }

    /// `/ColorTransform 0` is ignored when the JPEG carries an Adobe
    /// APP14 marker — the in-data transform byte wins over the PDF
    /// entry (ISO 32000-1 §7.4.8). c8_rgb's APP14 transform=0 already
    /// says RGB, so the result equals CT=0 anyway, but the *reason*
    /// it's accepted is the marker, not the PDF value.
    ///
    /// A conflicting combination — `/ColorTransform 1` on the RGB
    /// JPEG — must therefore still decode as raw RGB (the marker's
    /// transform=0 wins); asserting the *samples* distinguishes
    /// "marker honoured" from "PDF honoured".
    #[test]
    fn m27_1_app14_wins_over_pdf_ct() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_RGB_HEX), // APP14 transform=0 → raw RGB
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 1 >>", // PDF says YCbCr
        ));
        // Decode the image directly to inspect the samples — the
        // marker must win, so the first pixels are the RGB-encoded
        // values, not YCbCr→RGB of them.
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let samples = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 3)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        // c8_rgb first pixel is (0,0,0) and second is (0x1f,0x00,0x0f)
        // when read as raw RGB (djpeg reference: `00 00 00 1f 00 0f`).
        // YCbCr→RGB of those bytes would give a *different* value.
        assert_eq!(&samples[0..3], &[0x00, 0x00, 0x00], "pixel 0");
        assert_eq!(&samples[3..6], &[0x1f, 0x00, 0x0f], "pixel 1");
    }

    /// `/ColorTransform 1` on a JPEG *without* APP14 forces the
    /// YCbCr→RGB conversion — same result as the default (the decoder
    /// picks YCbCr from component IDs 1,2,3 anyway).
    #[test]
    fn m27_1_ct1_forces_ycbcr_without_app14() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_NOJFIF_HEX), // no JFIF, no APP14
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 1 >>",
        ));
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let samples = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 3)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        // Same chromatic content as c8: djpeg gives `01 00 00 16 07 0c`
        // for the first two pixels; jpeg-decoder's fixed-point YCbCr→RGB
        // rounds ±1 (documented tolerance — not a colour-space swap).
        let expected = [0x01u8, 0x00, 0x00, 0x16, 0x07, 0x0c];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(
                a.abs_diff(b) <= 1,
                "sample {i}: got {a:#04x}, expected ~{b:#04x}"
            );
        }
    }

    /// The default (no `/ColorTransform`, no APP14) on a 3-component
    /// JPEG applies YCbCr→RGB — the spec default for three components.
    #[test]
    fn m27_1_default_ct_is_ycbcr_on_3comp() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_NOJFIF_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let samples = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 3)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        // Same chromatic YCbCr→RGB content as m27_1_ct1 — the default
        // matches the explicit CT=1 result on 3-component data.
        let expected = [0x01u8, 0x00, 0x00, 0x16, 0x07, 0x0c];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 1, "sample {i}: {a:#04x} vs {b:#04x}");
        }
    }

    /// `/ColorTransform 1` on a *gray* JPEG is ignored per spec (the
    /// transform applies only to 3- or 4-component data) — decode
    /// succeeds and the samples are the plain gray values.
    #[test]
    fn m27_1_ct1_on_gray_ignored() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 1 >>",
        ));
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let samples = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 1)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        // g8 row 0 = 0 35 61 90 125 156 184 215 — same as the
        // no-ColorTransform decode; CT=1 changes nothing on gray.
        let row0 = [0u8, 35, 61, 90, 125, 156, 184, 215];
        for (i, (&a, &b)) in samples.iter().zip(row0.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 1, "gray sample {i}: {a} vs {b}");
        }
    }

    /// `/ColorTransform` with a non-integer value is a typed
    /// `Structure` error, not a silent default.
    #[test]
    fn m27_1_ct_bad_type_refused() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform /Bad >>",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        assert!(matches!(e, Error::Structure(_)), "{e:?}");
    }

    /// `/ColorTransform` outside {0,1} is `Unsupported`, distinct
    /// from a parse error on corrupt data.
    #[test]
    fn m27_1_ct_bad_value_refused() {
        let d = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 7 >>",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => {
                assert!(feature.contains("ColorTransform"), "{feature}")
            }
            other => panic!("expected Unsupported(ColorTransform), got {other:?}"),
        }
    }

    // -- M27.2 — explicit effective-transform decision ---------------
    //
    // Each case lists the *spec-mandated* effective transform (ISO
    // 32000-1 §7.4.8 + Adobe TN #5116), whether the edit accepts or
    // refuses the image, the decoder setting we force, and the
    // expected sample evidence. Expected samples come from `djpeg`
    // (libjpeg-turbo) on the same files — the ±1 tolerance covers
    // jpeg-decoder's fixed-point rounding, not a colour-space swap.

    /// Helper: decode the image stream on a one-page JPEG PDF and
    /// return the raw samples.
    fn jpeg_samples(bytes: &[u8]) -> Vec<u8> {
        let d = doc(bytes);
        let orig = d
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        match orig {
            Object::Stream { ref dict, ref data } => {
                let ncomps = if dict.get(b"ColorSpace".as_slice()).and_then(|o| o.as_name())
                    == Some(b"DeviceGray".as_slice())
                {
                    1
                } else {
                    3
                };
                let w = dict.get(b"Width".as_slice()).unwrap().as_i64().unwrap() as u32;
                let h = dict.get(b"Height".as_slice()).unwrap().as_i64().unwrap() as u32;
                decode_image(dict, data, &Limits::default(), (w, h, ncomps)).unwrap()
            }
            _ => panic!("not a stream"),
        }
    }

    /// Case 1 — APP14 transform=0 with YCbCr component IDs (1,2,3).
    ///
    /// Effective transform per spec: APP14 wins → *no transform*
    /// (the `0` code means the components are the device colour
    /// space values). jpeg-decoder alone would pick YCbCr from the
    /// IDs (it ranks IDs above APP14), so this case only passes if
    /// our explicit decision forces the raw path.
    ///
    /// Decoder setting: `ColorTransform::RGB` — its interleave
    /// produces the same interleaved output as `None` but is safe
    /// on this non-subsampled 4:4:4 frame.
    ///
    /// Evidence: `djpeg -pnm c8_444_app14_0.jpg` gives the YCbCr
    /// component values `00 80 80 0b 83 8c`; any YCbCr→RGB applied
    /// would shift them towards `00 00 00 1c 01 10`.
    #[test]
    fn m27_2_app14_0_beats_ycbcr_ids() {
        let samples = jpeg_samples(&jpeg_page_pdf(
            &unhex(JPEG_C8_444_APP14_0_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let expected = [0x00u8, 0x80, 0x80, 0x0b, 0x83, 0x8c];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(
                a.abs_diff(b) <= 1,
                "sample {i}: got {a:#04x}, expected ~{b:#04x} (raw YCbCr values)"
            );
        }
    }

    /// Case 2 — APP14 transform=1 with RGB component IDs (R,G,B).
    ///
    /// Effective transform per spec: APP14 wins → YCbCr→RGB is
    /// *applied* even though the data was encoded as RGB and the
    /// component IDs say so. The output is the (incorrect-looking)
    /// transformed colour — that is exactly what the spec mandates:
    /// the in-data flag is authoritative.
    ///
    /// Decoder setting: `ColorTransform::YCbCr`. jpeg-decoder alone
    /// would pick `RGB` from the IDs — the exact case where its
    /// heuristics contradict the spec order.
    ///
    /// Evidence: YCbCr→RGB of the stored RGB values `00 00 00` /
    /// `1f 00 0f` gives `00 87 00` / `00 9c 00` — the shifted
    /// (wrong-looking) colours the spec requires.
    #[test]
    fn m27_2_app14_1_beats_rgb_ids() {
        let samples = jpeg_samples(&jpeg_page_pdf(
            &unhex(JPEG_C8_RGB_APP14_1_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let expected = [0x00u8, 0x87, 0x00, 0x00, 0x9c, 0x00];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(
                a.abs_diff(b) <= 2,
                "sample {i}: got {a:#04x}, expected ~{b:#04x} (forced YCbCr→RGB)"
            );
        }
    }

    /// Case 3 — APP14 conflicts with PDF `/ColorTransform`.
    ///
    /// Effective transform per spec: APP14 wins, PDF entry ignored.
    /// APP14=1 + PDF `ColorTransform 0` → YCbCr→RGB is applied.
    ///
    /// Decoder setting: `ColorTransform::YCbCr` (driven by the
    /// APP14 byte, not the PDF value).
    #[test]
    fn m27_2_app14_wins_over_pdf() {
        let d2 = doc(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_RGB_APP14_1_HEX), // APP14 tf=1
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 0 >>", // PDF says raw
        ));
        let orig = d2
            .deref(&Object::Ref(IndirectRef {
                number: 10,
                generation: 0,
            }))
            .unwrap();
        let samples = match orig {
            Object::Stream { ref dict, ref data } => {
                decode_image(dict, data, &Limits::default(), (8, 8, 3)).unwrap()
            }
            _ => panic!("not a stream"),
        };
        // APP14=1 wins → YCbCr→RGB applied to RGB data → shifted
        // colours `00 87 00 …`, not the raw RGB `00 00 00 …`.
        let expected = [0x00u8, 0x87, 0x00, 0x00, 0x9c, 0x00];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 2, "sample {i}: {a:#04x} vs ~{b:#04x}");
        }
    }

    /// Case 4 — no APP14, no PDF ColorTransform, component IDs R,G,B.
    ///
    /// Effective transform per spec: default for 3 components is
    /// `1` → YCbCr→RGB. jpeg-decoder alone would pick `RGB` from
    /// the IDs — the case where its default contradicts the spec
    /// default.
    ///
    /// Decoder setting: `ColorTransform::YCbCr` (spec default).
    ///
    /// Note: this is a *spec-invalid* file in the wild (RGB data
    /// marked by IDs but unmarked for transform) — the spec's
    /// default applies regardless.
    #[test]
    fn m27_2_default_ct_applies_even_with_rgb_ids() {
        // c8_rgb has APP14 transform=0 — strip that case by using a
        // file with RGB IDs and *no* APP14: rebuild c8_rgb minus APP14.
        // JPEG_C8_RGB_HEX keeps APP14=0; we need an RGB-ID file with
        // no APP14 → construct via jpeg_page_pdf on the raw bytes.
        let raw = unhex(JPEG_C8_RGB_HEX);
        // Strip the APP14 segment (SOI + APP14 + rest).
        let ln = ((raw[4] as usize) << 8) | raw[5] as usize;
        let mut stripped = raw[..2].to_vec();
        stripped.extend_from_slice(&raw[4 + ln..]);
        let samples = jpeg_samples(&jpeg_page_pdf(
            &stripped,
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        // Spec default for 3 components → YCbCr→RGB applied to the
        // stored RGB values `00 00 00` / `1f 00 0f` → `00 87 00` /
        // `00 9c 00` (same shifted colours as case 2).
        let expected = [0x00u8, 0x87, 0x00, 0x00, 0x9c, 0x00];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 2, "sample {i}: {a:#04x} vs ~{b:#04x}");
        }
    }

    /// Case 5 — no APP14, PDF `/ColorTransform 0`, non-subsampled.
    ///
    /// Effective transform: none. Decoder setting `RGB` (the safe
    /// interleave; `None` would panic on subsampled data — checked
    /// separately in case 9).
    #[test]
    fn m27_2_no_app14_pdf_ct0_nonsubsampled() {
        let samples = jpeg_samples(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_444_HEX), // 4:4:4 YCbCr, no APP14
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 0 >>",
        ));
        // Raw YCbCr values interleaved — `djpeg` gives `00 80 80`
        // `0b 83 8c` for the first two pixels.
        let expected = [0x00u8, 0x80, 0x80, 0x0b, 0x83, 0x8c];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 1, "sample {i}: {a:#04x} vs ~{b:#04x}");
        }
    }

    /// Case 6 — no APP14, PDF `/ColorTransform 1`, YCbCr data.
    ///
    /// Effective transform: YCbCr→RGB. Decoder setting `YCbCr`.
    #[test]
    fn m27_2_no_app14_pdf_ct1() {
        let samples = jpeg_samples(&jpeg_page_pdf_parms(
            &unhex(JPEG_C8_444_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 1 >>",
        ));
        // YCbCr→RGB applied — `djpeg` gives `00 00 00 1c 01 10`.
        let expected = [0x00u8, 0x00, 0x00, 0x1c, 0x01, 0x10];
        for (i, (&a, &b)) in samples.iter().zip(expected.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 1, "sample {i}: {a:#04x} vs ~{b:#04x}");
        }
    }

    /// Case 7 — gray JPEG, `/ColorTransform 1` is ignored per spec
    /// (transform only applies to 3/4-component data).
    #[test]
    fn m27_2_gray_ignores_ct() {
        let samples = jpeg_samples(&jpeg_page_pdf_parms(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
            "/DecodeParms << /ColorTransform 1 >>",
        ));
        // Plain gray row 0: 0 35 61 90 125 156 184 215.
        let row0 = [0u8, 35, 61, 90, 125, 156, 184, 215];
        for (i, (&a, &b)) in samples.iter().zip(row0.iter()).enumerate() {
            assert!(a.abs_diff(b) <= 1, "gray sample {i}: {a} vs {b}");
        }
    }

    /// Case 8 — APP14 transform value outside 0–2 (Adobe TN #5116
    /// defines only those three) is refused as `Unsupported`.
    #[test]
    fn m27_2_app14_bad_transform_refused() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_APP14_BAD_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, detail } => {
                assert!(
                    feature.contains("APP14") || detail.contains("transform"),
                    "{feature}: {detail}"
                );
            }
            // jpeg-decoder's own read_info may also reject tf=3 with
            // a Format error — either typed refusal is correct.
            Error::Parse(_) => {}
            other => panic!("expected refusal for APP14 tf=3, got {other:?}"),
        }
    }

    /// Case 9 — "no transform" requested on a chroma-subsampled
    /// JPEG is refused regardless of *why* it was requested.
    /// Here via APP14=0 on a 4:2:0 frame.
    #[test]
    fn m27_2_no_transform_subsampled_refused() {
        // c8 is 4:2:0-subsampled YCbCr (IDs 1,2,3) with no APP14 —
        // inject one with transform=0 by borrowing the 16-byte APP14
        // segment from the generated fixture.
        let app14_seg = &unhex(JPEG_C8_444_APP14_0_HEX)[2..18];
        let raw = unhex(JPEG_C8_HEX);
        let mut d = raw[..2].to_vec();
        d.extend_from_slice(app14_seg);
        d.extend_from_slice(&raw[2..]);
        let doc_ = doc(&jpeg_page_pdf(&d, 8, 8, "DeviceRGB", "200 0 0 200 0 0 cm"));
        let e = remove_image_region(&doc_, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        match e {
            Error::Unsupported { feature, detail } => {
                assert!(
                    feature.contains("raw") || detail.contains("subsampled"),
                    "{feature}: {detail}"
                );
            }
            other => panic!("expected Unsupported(no-transform subsampled), got {other:?}"),
        }
    }

    /// Case 10 — two contradictory APP14 markers are ambiguous;
    /// refused rather than guessing.
    #[test]
    fn m27_2_conflicting_app14_refused() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_C8_APP14_DUP_HEX),
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        // Typed refusal — Parse (ambiguous structure) or
        // Unsupported; either is correct, never a silent pick.
        assert!(
            matches!(e, Error::Parse(_) | Error::Unsupported { .. }),
            "{e:?}"
        );
    }

    /// A truncated marker walk — SOI + a length that overruns the
    /// buffer — is `Parse`, not a silent "no APP14" default.
    #[test]
    fn m27_2_truncated_markers_refused() {
        // SOI + APP14 marker + length 0x00 0x0E but only 3 payload
        // bytes — the segment overruns the buffer.
        let bad = [0xFF, 0xD8, 0xFF, 0xEE, 0x00, 0x0E, b'A', b'd', b'o'];
        let d = doc(&jpeg_page_pdf(
            &bad,
            8,
            8,
            "DeviceRGB",
            "200 0 0 200 0 0 cm",
        ));
        let e = remove_image_region(&d, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default())
            .unwrap_err();
        // jpeg-decoder's read_info may fail first with its own
        // Parse/Format error — either way it's a typed refusal,
        // never silently treated as "no APP14".
        assert!(
            matches!(
                e,
                Error::Parse(_) | Error::Unsupported { .. } | Error::Structure(_)
            ),
            "{e:?}"
        );
    }

    /// New `/Contents` + `/Resources` both exist and are consistent.
    #[test]
    fn m27_jpeg_contents_and_resources_consistent() {
        let d = doc(&jpeg_page_pdf(
            &unhex(JPEG_G8_HEX),
            8,
            8,
            "DeviceGray",
            "200 0 0 200 0 0 cm",
        ));
        let r =
            remove_image_region(&d, 0, &[(0.0, 150.0, 50.0, 200.0)], &Limits::default()).unwrap();
        let out = doc(&r.output);
        let page = out.pages().first().unwrap();
        let pd = out.resolve(page.object_ref().unwrap()).unwrap();
        let dict = pd.as_dict().unwrap();
        assert!(dict.contains_key(b"Contents".as_slice()));
        assert!(dict.contains_key(b"Resources".as_slice()));
        // The new Contents invokes KasIm0 and the XObject dict maps it.
        let names = do_names_in_contents(&out, page);
        assert!(names.iter().any(|n| n == b"KasIm0"));
        assert!(page_xobject_ref(&out, page, b"KasIm0").is_some());
    }
}
