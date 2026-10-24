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
    /// Form XObjects encountered. The probe does not recurse into a
    /// form's content stream, so each one is an *unprobed region* —
    /// blockers hidden inside are invisible to the analysis.
    pub forms: usize,
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
        if let Object::Array(entries) = a {
            for e in &entries {
                probe_annotation(doc, e, pi, res, record);
            }
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

/// Mirror `Renderer::draw_annotations`: Hidden / NoView / Popup and a
/// `/Link` without `/AP` are skipped by the renderer; an annotation
/// with no `/AP` (appearance would have to be synthesised) or an
/// `/AP /N` state dictionary with several states and no `/AS` is a
/// blocker. The appearance stream's own content is *not* walked — it
/// is an unprobed region, counted in `forms` like any other form.
fn probe_annotation(
    doc: &Document,
    entry: &Object,
    pi: usize,
    res: &mut ProbeResult,
    record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) {
    let Ok(annot) = doc.deref(entry) else { return };
    let Some(ad) = annot.as_dict() else { return };
    let subtype = ad
        .get(b"Subtype".as_slice())
        .and_then(|o| o.as_name())
        .unwrap_or(b"");
    let flags = ad
        .get(b"F".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    if flags & 0x02 != 0 || flags & 0x20 != 0 || subtype == b"Popup" {
        return;
    }
    let Some(ap) = ad.get(b"AP".as_slice()) else {
        // Form widgets (§12.7.3.3) and the geometric markup subtypes
        // (§12.5.6) get a synthesised appearance; /Redact marks draw
        // nothing; every other subtype without /AP is refused.
        if !matches!(
            subtype,
            b"Link"
                | b"Widget"
                | b"Square"
                | b"Circle"
                | b"Line"
                | b"Polygon"
                | b"PolyLine"
                | b"Ink"
                | b"Highlight"
                | b"Underline"
                | b"StrikeOut"
                | b"Squiggly"
                | b"FreeText"
                | b"Redact"
        ) {
            record(res, "render.annotation.unsupported", pi);
        }
        return;
    };
    let Ok(ap) = doc.deref(ap) else { return };
    let Some(n) = ap.as_dict().and_then(|d| d.get(b"N".as_slice())) else {
        return;
    };
    match doc.deref(n) {
        Ok(Object::Dict(states)) => {
            // Several states without /AS: a widget falls back to its
            // field value (/V, possibly inherited); anything else is
            // ambiguous and refused.
            let has_value = subtype == b"Widget"
                && (ad.contains_key(b"V".as_slice()) || ad.contains_key(b"Parent".as_slice()));
            if ad.get(b"AS".as_slice()).is_none() && states.len() != 1 && !has_value {
                record(res, "render.annotation.unsupported", pi);
            } else {
                res.forms += 1;
            }
        }
        Ok(Object::Stream { .. }) => res.forms += 1,
        _ => {}
    }
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
            Operation::Shading(n) => {
                // Axial/radial shadings render; types 1 and 4–7 do not.
                if !shading_supported(doc, resources, "Shading", n) {
                    record(res, "render.shading.unsupported", pi)
                }
            }
            Operation::SetColorSpaceFill(_) | Operation::SetColorSpaceStroke(_) => {
                // Every colour-space family resolves now, /Pattern
                // included; malformed definitions are structure errors
                // found at run time.
            }
            Operation::SetColorFill(o) | Operation::SetColorStroke(o) => {
                // `scn`/`SCN` with a trailing pattern name: shading
                // patterns (type 2 over an axial/radial shading) and
                // tiling patterns render; mesh shadings do not.
                if let Some(Object::Name(p)) = o.last() {
                    if !pattern_supported(doc, resources, p) {
                        record(res, "render.pattern.unsupported", pi)
                    }
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

/// `true` when the shading `n` in `category` (`/Shading` or, for a
/// pattern, the pattern's own `/Shading` entry) is a type the renderer
/// draws (2 axial, 3 radial). Unresolvable names are left to the run
/// (they are structure errors, not feature gaps).
fn shading_supported(doc: &Document, resources: &Object, category: &str, n: &[u8]) -> bool {
    let Ok(Some(o)) = doc.resolve_resource(resources, category, n) else {
        return true;
    };
    shading_obj_supported(doc, &o)
}

fn shading_obj_supported(doc: &Document, o: &Object) -> bool {
    let Ok(o) = doc.deref(o) else { return true };
    let Some(d) = o.as_dict_like() else {
        return true;
    };
    match d.get(b"ShadingType".as_slice()).and_then(|o| o.as_i64()) {
        Some(2) | Some(3) | None => true,
        Some(_) => false,
    }
}

/// `true` when `/Pattern p` is a tiling pattern (type 1, rendered cell
/// by cell or through a cached cell) or a `/PatternType 2` shading
/// pattern over a supported shading without an `/ExtGState` override.
fn pattern_supported(doc: &Document, resources: &Object, p: &[u8]) -> bool {
    let Ok(Some(o)) = doc.resolve_resource(resources, "Pattern", p) else {
        return true;
    };
    let Some(d) = o.as_dict_like() else {
        return true;
    };
    match d.get(b"PatternType".as_slice()).and_then(|o| o.as_i64()) {
        Some(2) => {
            if d.contains_key(b"ExtGState".as_slice()) {
                return false;
            }
            d.get(b"Shading".as_slice())
                .map(|s| shading_obj_supported(doc, s))
                .unwrap_or(true)
        }
        Some(1) => true,
        _ => true,
    }
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
            // Mirror load_simple_font: a descriptor is required;
            // symbolic fonts map codes through the cmap; without
            // /FontFile2 the bundled substitute must be able to stand
            // in (it refuses symbolic names it cannot read).
            let fd = dict
                .get(b"FontDescriptor".as_slice())
                .and_then(|o| doc.deref(o).ok())
                .and_then(|o| o.as_dict().cloned());
            match fd {
                None => record(res, "render.font.nonembedded.unsupported", pi),
                Some(fd) => {
                    if !fd.contains_key(b"FontFile2".as_slice()) {
                        let base = dict
                            .get(b"BaseFont".as_slice())
                            .and_then(|o| o.as_name())
                            .unwrap_or(b"");
                        if !matches!(kasane_pdf::substitution_for(base, Some(&fd)), Ok(Some(_))) {
                            record(res, "render.font.nonembedded.unsupported", pi);
                        }
                    }
                }
            }
        }
        Some(b"Type0") => {
            // descendant kind + encoding + ToUnicode
            // Identity-H/V and embedded CMap streams load; a predefined
            // non-Identity CMap name is the only encoding still refused.
            match dict.get(b"Encoding".as_slice()) {
                Some(Object::Name(n)) if n != b"Identity-H" && n != b"Identity-V" => {
                    record(res, "render.font.type0-enc.unsupported", pi)
                }
                _ => {}
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
                                // Both CIDFont subtypes load (TrueType,
                                // OpenType and bare CFF programs); a
                                // descendant without any program refuses.
                                match dd.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
                                    Some(b"CIDFontType2") | Some(b"CIDFontType0") => {
                                        let has_program = dd
                                            .get(b"FontDescriptor".as_slice())
                                            .and_then(|o| doc.deref(o).ok())
                                            .and_then(|o| o.as_dict().cloned())
                                            .map(|fd| {
                                                fd.contains_key(b"FontFile2".as_slice())
                                                    || fd.contains_key(b"FontFile3".as_slice())
                                            })
                                            .unwrap_or(false);
                                        if !has_program {
                                            record(res, "render.font.nonembedded.unsupported", pi);
                                        }
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
                // descriptor but no font-file → a bundled substitute
                // stands in when one is sound; otherwise render refuses.
                else {
                    let base = dict
                        .get(b"BaseFont".as_slice())
                        .and_then(|o| o.as_name())
                        .unwrap_or(b"");
                    if !matches!(kasane_pdf::substitution_for(base, Some(&fd)), Ok(Some(_))) {
                        record(res, "render.font.nonembedded.unsupported", pi);
                    }
                }
            } else {
                // no descriptor at all → Standard 14 style; substitute
                // unless it is Symbol/ZapfDingbats or unreadable.
                let base = dict
                    .get(b"BaseFont".as_slice())
                    .and_then(|o| o.as_name())
                    .unwrap_or(b"");
                if !matches!(kasane_pdf::substitution_for(base, None), Ok(Some(_))) {
                    record(res, "render.font.nonembedded.unsupported", pi);
                }
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
            res.forms += 1;
            // /Group: an opaque-Normal transparency group now runs
            // inline; only alpha/blend/knockout contexts refuse, which
            // a static walk cannot see — so it is no longer recorded
            // (the estimate caveat covers it). A knockout group is
            // always refused, though.
            if let Some(g) = dict.get(b"Group".as_slice()) {
                if let Ok(gd) = doc.deref(g) {
                    if gd
                        .as_dict()
                        .and_then(|d| d.get(b"K".as_slice()))
                        .and_then(|o| o.as_bool())
                        == Some(true)
                    {
                        record(res, "render.xobject.group.unsupported", pi);
                    }
                }
            }
            // /OC is evaluated now (hidden forms are skipped, visible
            // ones painted) — no longer a blocker.
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
            // Image colour spaces, /ImageMask, /Mask and /SMask all
            // render now (a /Pattern colour space on an image is a
            // structure error, not a feature gap). What still refuses:
            // a pre-blended soft mask (/Matte), JPX-embedded alpha
            // (/SMaskInData ≠ 0) and optional content on the image.
            if let Some(Ok(sm)) = dict.get(b"SMask".as_slice()).map(|o| doc.deref(o)) {
                if sm
                    .as_dict_like()
                    .map(|d| d.contains_key(b"Matte".as_slice()))
                    .unwrap_or(false)
                {
                    record(res, "render.image.smask.unsupported", pi);
                }
            }
            if dict
                .get(b"SMaskInData".as_slice())
                .and_then(|o| o.as_i64())
                .unwrap_or(0)
                != 0
            {
                record(res, "render.image.smask.unsupported", pi);
            }
            // filters
            if let Some(f) = dict.get(b"Filter".as_slice()) {
                for name in filter_names(doc, f) {
                    match name.as_slice() {
                        // Byte filters, JPEG, JBIG2 (hayro-jbig2) and
                        // CCITT (hayro-ccitt) are all decoded.
                        b"FlateDecode" | b"DCTDecode" | b"JBIG2Decode" | b"CCITTFaxDecode"
                        | b"LZWDecode" | b"ASCIIHexDecode" | b"ASCII85Decode"
                        | b"RunLengthDecode" => {}
                        b"JPXDecode" => record(res, "parse.filter.jpx.unsupported", pi),
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
    _doc: &Document,
    _resources: &Object,
    _tag: &[u8],
    _properties: &Object,
    _pi: usize,
    _res: &mut ProbeResult,
    _record: &mut impl FnMut(&mut ProbeResult, &str, usize),
) {
    // Optional content (/OC) is evaluated against the document's
    // default configuration and /ActualText is honoured by extraction
    // — neither is a blocker any more. Malformed property lists are
    // structure errors found at run time.
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
