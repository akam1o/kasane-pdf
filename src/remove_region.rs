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
    ctm_at_op, decode_image, fresh_xobject_name, image_pixel_ranges, is_default_decode,
    validate_image,
};
use crate::remove_text::{charge_generated, rewrite_ops_for, serialize_ops};
use std::collections::BTreeMap;

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
                if let TargetDetail::Image { xobj, name, .. } = &t.detail {
                    let ctm = ctm_at_op(&ops, t.site.op_index)?;
                    to_edit_image
                        .insert(t.site.op_index, (t.site.clone(), ctm, *xobj, name.clone()));
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
}
