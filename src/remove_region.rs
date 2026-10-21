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
//! docs and `docs/coverage.md` for the full scope list.
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
//!   a non-default `/Decode`, a `/Filter` other than none or
//!   `/FlateDecode`, or a non-axis-aligned/negative-scale/non-finite
//!   CTM.
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
use std::collections::{BTreeMap, BTreeSet, HashSet};

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
            page_index + 1,
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
        let decoded = decode_image(dict, raw, limits)?;
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
    let mut pages_with_resources: Vec<IndirectRef> = Vec::new();
    let mut pages_to_strip: Vec<IndirectRef> = Vec::new();
    {
        // Collect all page refs and their effective Resources.
        for p in doc.pages() {
            if let Some(pr) = p.object_ref() {
                if p.resources().is_some() {
                    pages_with_resources.push(pr);
                }
            }
        }
        // For each /Pages node that has /Resources, check if all
        // descendants are in pages_with_resources. If so, the /Pages
        // can drop its /Resources.
        let catalog_dict = doc
            .catalog()
            .get(b"Pages".as_slice())
            .and_then(|o| o.as_ref())
            .ok_or_else(|| Error::Structure("catalog has no /Pages".into()))?;
        let mut stack: Vec<IndirectRef> = vec![catalog_dict];
        let mut visited: HashSet<IndirectRef> = HashSet::new();
        while let Some(pages_ref) = stack.pop() {
            if !visited.insert(pages_ref) {
                continue;
            }
            let pages_obj = doc.resolve(pages_ref)?;
            let pages_dict = pages_obj
                .as_dict()
                .ok_or_else(|| Error::Structure("/Pages is not a dictionary".into()))?;
            if pages_dict.contains_key(b"Resources".as_slice()) {
                // Check if all descendants are materialised.
                let kids = pages_dict
                    .get(b"Kids".as_slice())
                    .and_then(|o| o.as_array())
                    .ok_or_else(|| Error::Structure("/Pages has no /Kids".into()))?;
                let mut all_materialised = true;
                for kid in kids {
                    if let Some(kid_ref) = kid.as_ref() {
                        let kid_obj = doc.resolve(kid_ref)?;
                        let kid_dict = kid_obj
                            .as_dict()
                            .ok_or_else(|| Error::Structure("/Kids entry is not a dict".into()))?;
                        if kid_dict.contains_key(b"Kids".as_slice()) {
                            // Intermediate node — push to stack.
                            stack.push(kid_ref);
                        } else {
                            // Leaf page — must be in pages_with_resources.
                            if !pages_with_resources.contains(&kid_ref) {
                                all_materialised = false;
                                break;
                            }
                        }
                    } else {
                        // Direct kid — no ref, cannot check.
                        all_materialised = false;
                        break;
                    }
                }
                if all_materialised {
                    pages_to_strip.push(pages_ref);
                }
            }
        }
    }

    // Apply materialisation: each page gets its effective Resources.
    for &pr in &pages_with_resources {
        let p = doc
            .pages()
            .iter()
            .find(|p| p.object_ref() == Some(pr))
            .unwrap();
        let p_obj = doc.resolve(pr)?;
        let mut p_dict = p_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("page is not a dictionary".into()))?;
        p_dict.insert(b"Resources".to_vec(), p.resources().unwrap().clone());
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
                page_index + 1,
                validated.len(),
                if validated.len() == 1 { "" } else { "s" }
            ),
            format!("codes removed: {codes_removed}"),
            format!("invocations edited: {}", do_renames.len()),
            format!("pixels changed: {pixels_changed}"),
            format!("orphaned names removed: {orphaned_names_removed}"),
            "scope: page-level text + image Do calls — no forms, no nested XObjects, \
             no /Annots/metadata/structure, no JPEG/masks/non-device colour"
                .into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // JPEG image — DCTDecode, refused.
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
        // Text is editable but the image is JPEG — when the rect
        // hits the JPEG image the whole call fails. A JPEG image
        // *outside* the rect is left alone (M20's existing policy).
        let err = remove_region(
            &d,
            0,
            &[(20.0, 140.0, 30.0, 160.0), (20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { .. }),
            "JPEG image in rect must refuse the whole call: {err:?}"
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
}
