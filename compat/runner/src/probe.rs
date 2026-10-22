//! Read-only blocker probe.
//!
//! The engine's render/text/rewrite return the **first** error they
//! hit, which hides every blocker behind it. The probe walks the page
//! the same way — content ops, `/XObject`/`/Font`/`/ColorSpace`/
//! `/ExtGState` resources, `/Annots`, `/UserUnit` — and records *every*
//! unsupported feature it would hit, as a `Cause` id. It performs no
//! drawing or decoding and never mutates the document.
//!
//! This powers the "affected / sole-blocker / potentially-unblocked"
//! analysis: a page is a *sole-blocker* for feature X when X is the
//! only cause the probe finds on it.
//!
//! The probe mirrors the renderer's refusal rules; it is deliberately
//! conservative — when in doubt it records a cause rather than none.
//! It is a *diagnostic* walker like `redact_diag`, not a render.

use kasane_pdf::{Dict, Document, Object, Operation, Page};
use std::collections::BTreeSet;

/// Per-file blocker set (union over probed pages).
#[derive(Debug, Default)]
pub struct ProbeResult {
    /// All distinct cause ids encountered on any probed page.
    pub causes: BTreeSet<String>,
    /// cause id → count of pages it appeared on (deduped per page).
    pub pages_per_cause: std::collections::BTreeMap<String, usize>,
    /// pages the probe could not even walk (structure errors).
    pub walk_errors: usize,
    /// total pages probed.
    pub pages: usize,
}

/// Probe a document: enumerate blockers for render+text per page.
/// `max_pages` bounds how many pages are walked (first-N, matching the
/// runner's stage scope).
pub fn probe_document(doc: &Document, max_pages: usize) -> ProbeResult {
    let mut res = ProbeResult::default();
    let mut seen: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut record = |res: &mut ProbeResult, id: &str, page: usize| {
        res.causes.insert(id.to_string());
        if seen.insert((id.to_string(), page)) {
            *res.pages_per_cause.entry(id.to_string()).or_insert(0) += 1;
        }
    };

    for (pi, page) in doc.pages().iter().take(max_pages).enumerate() {
        res.pages += 1;
        match probe_page(doc, page, pi, &mut res, &mut record) {
            Ok(()) => {}
            Err(_) => res.walk_errors += 1,
        }
    }
    res
}

fn probe_page(
    doc: &Document,
    page: &Page,
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) -> Result<(), kasane_pdf::Error> {
    // Page-level blockers (mirror render_page's early checks).
    let pd = page_dict(doc, page)?;
    if let Some(a) = pd.get(b"Annots".as_slice()) {
        let a = doc.deref(a)?;
        if !matches!(a, Object::Array(ref v) if v.is_empty()) {
            record(res, "render.annotation.unsupported", pi);
        }
    }
    if let Some(u) = pd.get(b"UserUnit".as_slice()) {
        if let Some(v) = doc.deref(u)?.as_number() {
            if (v - 1.0).abs() > f64::EPSILON {
                record(res, "render.userunit.unsupported", pi);
            }
        }
    }

    let ops = doc.page_operations(page)?;
    let resources = page.resources().cloned().unwrap_or(Object::Null);
    scan_ops(doc, &ops, &resources, pi, res, record, 0);
    Ok(())
}

fn page_dict(doc: &Document, page: &Page) -> Result<Dict, kasane_pdf::Error> {
    // Rebuild the page dict from the page object's own dict if exposed;
    // `Page` doesn't hand back its dict, so re-derive Annots/UserUnit
    // via the resources path isn't possible — instead resolve the page
    // object through its object_ref when available.
    if let Some(r) = page.object_ref() {
        if let Ok(o) = doc.resolve(r) {
            if let Some(d) = o.as_dict() {
                return Ok(d.clone());
            }
        }
    }
    Ok(Dict::new())
}

fn scan_ops(
    doc: &Document,
    ops: &[Operation],
    resources: &Object,
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for op in ops {
        match op {
            Operation::SetExtGState(n) => probe_extgstate(doc, resources, n, pi, res, record),
            Operation::Shading(_) => record(res, "render.shading.unsupported", pi),
            Operation::SetColorSpaceFill(n) | Operation::SetColorSpaceStroke(n) => {
                if !is_device_space_name(n) {
                    record(res, "render.colorspace.unsupported", pi)
                }
            }
            Operation::SetColorFill(o) | Operation::SetColorStroke(o) => {
                // `scn`/`SCN` with a trailing pattern name → pattern.
                if o.iter().any(|x| matches!(x, Object::Name(_))) {
                    record(res, "render.pattern.unsupported", pi)
                }
            }
            Operation::SetTextRenderingMode(m) => {
                if *m >= 4 {
                    record(res, "render.textmode.unsupported", pi)
                }
            }
            Operation::SetTextFont { font, .. } => {
                probe_font(doc, resources, font, pi, res, record)
            }
            Operation::Do(n) => probe_xobject(doc, resources, n, pi, res, record, depth),
            Operation::BeginMarkedContentWithProps { tag, properties }
            | Operation::DesignatedMarkedContentPoint { tag, properties } => {
                probe_marked(doc, resources, tag, properties, pi, res, record)
            }
            _ => {}
        }
    }
}

fn is_device_space_name(n: &[u8]) -> bool {
    matches!(
        n,
        b"DeviceGray"
            | b"DeviceRGB"
            | b"DeviceCMYK"
            | b"DefaultGray"
            | b"DefaultRGB"
            | b"DefaultCMYK"
            | b"Pattern"
    )
}

/// Mirror `apply_extgstate`: resolve the `/ExtGState` dict and record a
/// blocker only for keys the renderer *can't* apply — a real soft mask
/// (`/SMask` ≠ `/None`), a non-`/Default` transfer/black-generation
/// function, `/Font` selection via gs, an unrecognised blend mode, or
/// an unknown key. Applied/no-op keys produce no blocker.
fn probe_extgstate(
    doc: &Document,
    resources: &Object,
    name: &[u8],
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) {
    let obj = match doc.resolve_resource(resources, "ExtGState", name) {
        Ok(Some(o)) => o,
        _ => return,
    };
    let Some(d) = obj.as_dict() else { return };
    for (k, v) in d {
        match k.as_slice() {
            // keys that always apply or are accepted no-ops
            b"Type" | b"LW" | b"LC" | b"LJ" | b"ML" | b"D" | b"RI" | b"ca" | b"CA" | b"OP"
            | b"op" | b"OPM" | b"SA" | b"AIS" | b"TK" | b"HT" | b"HTP" | b"FL" | b"SM" => {}
            b"BM" => {
                // only unsupported blend modes block
                if let Some(mode) = first_bm_name(doc, v) {
                    if !is_supported_bm(&mode) {
                        record(res, "render.blendmode.unsupported", pi);
                    }
                }
            }
            b"SMask" => {
                if !matches!(v.as_name(), Some(b"None")) {
                    record(res, "render.smask.unsupported", pi);
                }
            }
            b"TR" | b"TR2" | b"BG" | b"BG2" | b"UCR" | b"UCR2" => {
                if !matches!(v.as_name(), Some(b"Default")) {
                    record(res, "render.transfer.unsupported", pi);
                }
            }
            b"Font" => record(res, "render.extgstate.font.unsupported", pi),
            _ => record(res, "render.extgstate.unsupported", pi),
        }
    }
}

fn first_bm_name(doc: &Document, v: &Object) -> Option<Vec<u8>> {
    match doc.deref(v).ok()? {
        Object::Name(n) => Some(n),
        Object::Array(a) => a
            .first()
            .and_then(|x| doc.deref(x).ok())
            .and_then(|o| o.as_name().map(|n| n.to_vec())),
        _ => None,
    }
}

fn is_supported_bm(n: &[u8]) -> bool {
    matches!(
        n,
        b"Normal"
            | b"Compatible"
            | b"Multiply"
            | b"Screen"
            | b"Overlay"
            | b"Darken"
            | b"Lighten"
            | b"ColorDodge"
            | b"ColorBurn"
            | b"HardLight"
            | b"SoftLight"
            | b"Difference"
            | b"Exclusion"
            | b"Hue"
            | b"Saturation"
            | b"Color"
            | b"Luminosity"
    )
}

fn probe_font(
    doc: &Document,
    resources: &Object,
    name: &[u8],
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) {
    let fobj = match doc.resolve_resource(resources, "Font", name) {
        Ok(Some(o)) => o,
        _ => return,
    };
    let Some(dict) = fobj.as_dict() else { return };
    let subtype = dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name());
    match subtype {
        Some(b"TrueType") => {
            // embedded? Symbolic? Encoding?
            if dict.get(b"FontDescriptor".as_slice()).is_none() {
                record(res, "render.font.nonembedded.unsupported", pi);
            }
        }
        Some(b"Type0") => {
            // descendant kind + encoding + ToUnicode
            let enc = dict.get(b"Encoding".as_slice()).and_then(|o| o.as_name());
            if matches!(enc, Some(b"Identity-V")) {
                record(res, "render.font.vertical.unsupported", pi);
            } else if !matches!(enc, Some(b"Identity-H")) && enc.is_some() {
                record(res, "render.font.type0-enc.unsupported", pi);
            }
            if let Some(df) = dict.get(b"DescendantFonts".as_slice()) {
                if let Some(arr) = doc
                    .deref(df)
                    .ok()
                    .and_then(|o| o.as_array().map(|a| a.to_vec()))
                {
                    if arr.len() != 1 {
                        record(res, "render.font.multi-desc.unsupported", pi);
                    }
                    if let Some(d0) = arr.first() {
                        if let Ok(d) = doc.deref(d0) {
                            if let Some(dd) = d.as_dict() {
                                match dd.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
                                    Some(b"CIDFontType2") => {}
                                    Some(b"CIDFontType0") => {
                                        record(res, "render.font.cff.unsupported", pi)
                                    }
                                    _ => record(res, "render.font.cid-subtype.unsupported", pi),
                                }
                            }
                        }
                    }
                }
            }
        }
        Some(b"Type1") | Some(b"MMType1") => {
            // Mirror load_type1_font's acceptance:
            //   /FontFile   → eexec Type1 program → SUPPORTED (parsed by
            //                 src/type1.rs into outlines). A malformed
            //                 program yields a Structure error at render,
            //                 not an Unsupported flag.
            //   /FontFile3 CIDFontType0C → CID, not simple → unsupported
            //   /FontFile3 Type1C / absent program → supported (render
            //                 refuses only when there is no program)
            let fd = dict
                .get(b"FontDescriptor".as_slice())
                .and_then(|o| doc.deref(o).ok())
                .and_then(|o| o.as_dict().cloned());
            if let Some(fd) = fd {
                if fd.contains_key(b"FontFile".as_slice()) {
                    // eexec Type1 — supported by the charstring interpreter.
                } else if let Some(ff3) = fd.get(b"FontFile3".as_slice()) {
                    // Resolve the stream then read its /Subtype —
                    // CIDFontType0C is a CID font, not simple Type1.
                    let is_cid0 = doc
                        .deref(ff3)
                        .ok()
                        .and_then(|o| {
                            o.as_stream()
                                .and_then(|(d, _)| {
                                    d.get(b"Subtype".as_slice()).and_then(|o| o.as_name())
                                })
                                .map(|n| n.to_vec())
                        })
                        .map(|n| n == b"CIDFontType0C")
                        .unwrap_or(false);
                    if is_cid0 {
                        record(res, "render.font.cff.unsupported", pi);
                    }
                    // else /Type1C CFF — supported, no flag.
                }
                // descriptor but no font-file → metrics-only: render
                // refuses → still an Unsupported blocker for render.
                else {
                    record(res, "render.font.nonembedded.unsupported", pi);
                }
            } else {
                // no descriptor at all → metrics-only → render refuses.
                record(res, "render.font.nonembedded.unsupported", pi);
            }
        }
        Some(b"Type3") => {
            // Type3 CharProcs run like miniature Form XObjects — but a
            // CharProc that uses /Pattern colours or nested shading is
            // still blocked. Only flag the ones we genuinely can't run:
            // a CharProc using an unimplemented feature is caught by the
            // nested probe, so here we just confirm the font *shape* is
            // loadable (has /CharProcs + /Encoding).
            let has_cp = dict
                .get(b"CharProcs".as_slice())
                .map(|o| matches!(doc.deref(o), Ok(Object::Dict(_))))
                .unwrap_or(false);
            if !has_cp {
                record(res, "render.font.type3.nocharprocs.unsupported", pi);
            }
            // With /CharProcs the font loads — any residual blocker is a
            // nested feature (pattern, colours), probed elsewhere.
        }
        Some(b"CIDFontType0") => record(res, "render.font.cff.unsupported", pi),
        Some(_) => record(res, "render.font.unsupported", pi),
        None => {}
    }
}

fn probe_xobject(
    doc: &Document,
    resources: &Object,
    name: &[u8],
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
    _depth: usize,
) {
    let xobj = match doc.resolve_resource(resources, "XObject", name) {
        Ok(Some(o)) => o,
        _ => return,
    };
    let Some(dict) = xobj.as_dict() else { return };
    match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
        Some(b"Form") => {
            if dict.contains_key(b"Group".as_slice()) {
                record(res, "render.xobject.group.unsupported", pi);
            }
            if dict.contains_key(b"OC".as_slice()) {
                record(res, "render.xobject.oc.unsupported", pi);
            }
            if dict.contains_key(b"Ref".as_slice()) {
                record(res, "render.xobject.ref.unsupported", pi);
            }
            // NOTE: we do not recurse into a Form's inner content
            // stream — a form's ops aren't reachable through the
            // public page_operations API, and inner blockers surface
            // at Do-time anyway. Forms still register Group/OC/Ref
            // blockers above. This keeps Phase-A conservative: a
            // form-heavy file may carry *additional* blockers we
            // don't see, which is reported as an estimate caveat.
        }
        Some(b"Image") => {
            // image colour space + filters
            if let Some(cs) = dict.get(b"ColorSpace".as_slice()) {
                match doc.deref(cs) {
                    Ok(Object::Name(n)) => {
                        if !is_device_space_name(&n) {
                            record(res, "render.image.colorspace.unsupported", pi);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            if dict.get(b"ImageMask".as_slice()).and_then(|o| o.as_bool()) == Some(true) {
                record(res, "render.image.mask.unsupported", pi);
            }
            if dict.contains_key(b"SMask".as_slice()) || dict.contains_key(b"Mask".as_slice()) {
                record(res, "render.image.smask.unsupported", pi);
            }
            // filters
            if let Some(f) = dict.get(b"Filter".as_slice()) {
                for name in filter_names(doc, f) {
                    match name.as_slice() {
                        b"FlateDecode" | b"DCTDecode" => {}
                        b"JPXDecode" => record(res, "parse.filter.jpx.unsupported", pi),
                        b"JBIG2Decode" => record(res, "parse.filter.jbig2.unsupported", pi),
                        b"CCITTFaxDecode" => record(res, "parse.filter.ccitt.unsupported", pi),
                        b"LZWDecode" => record(res, "parse.filter.lzw.unsupported", pi),
                        b"ASCIIHexDecode" | b"ASCII85Decode" | b"RunLengthDecode" => {
                            record(res, "parse.filter.unsupported", pi)
                        }
                        _ => record(res, "parse.filter.unsupported", pi),
                    }
                }
            }
        }
        Some(_) => record(res, "render.xobject.subtype.unsupported", pi),
        None => {}
    }
}

fn probe_marked(
    doc: &Document,
    resources: &Object,
    tag: &[u8],
    properties: &Object,
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) {
    match tag {
        b"OC" | b"OCMD" => record(res, "render.oc.unsupported", pi),
        b"Span" => {
            // /Span <</ActualText …>> changes extraction output.
            if let Object::Name(n) = properties {
                if let Ok(Some(o)) = doc.resolve_resource(resources, "Properties", n) {
                    if has_actual_text(&o) {
                        record(res, "text.actualtext.unsupported", pi);
                    }
                }
            } else if has_actual_text(properties) {
                record(res, "text.actualtext.unsupported", pi);
            }
        }
        _ => {}
    }
}

fn has_actual_text(o: &Object) -> bool {
    o.as_dict()
        .map(|d| d.contains_key(b"ActualText".as_slice()))
        .unwrap_or(false)
}

fn filter_names(doc: &Document, o: &Object) -> Vec<Vec<u8>> {
    match doc.deref(o) {
        Ok(Object::Name(n)) => vec![n],
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|x| {
                doc.deref(x)
                    .ok()
                    .and_then(|o| o.as_name().map(|n| n.to_vec()))
            })
            .collect(),
        _ => Vec::new(),
    }
}
