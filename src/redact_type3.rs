//! `redact`: Type 3 glyph definitions of removed text.
//!
//! A Type 3 glyph is drawn by its CharProc, a content stream that may
//! hold anything a page can — a picture, a signature, a word. Removing
//! the codes inside the rects leaves those definitions in the font
//! unless they are pruned. After the edit, each Type 3 font is compared
//! with the input: a glyph shown in the input and shown nowhere in the
//! output (the same font — by content — on any page, form or annotation
//! appearance) loses its definition: its `/CharProcs` entry, the codes
//! naming it in the encoding, their widths and their `/ToUnicode`
//! entries. Glyphs never shown in the input go with them, and glyphs
//! the output still shows keep their definitions (the output draws
//! them there).
//!
//! Where a font is used is known only for what the walk reads: page
//! content, forms they draw and annotation appearances. A font that
//! loses glyphs and that something else refers to (a tiling pattern, a
//! soft mask, a form field's `/DR`, another Type 3 font's resources …)
//! may still be used for those glyphs there: refused. So is a direct
//! (unreferenced) Type 3 font dictionary that loses glyphs.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::content::{Operation, TJItem};
use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::remove_region::sha256;

/// A Type 3 font by content: its FontMatrix and its CharProcs (names
/// and stored bytes), so the same font is found again after rewriting
/// renumbers it.
type FontKey = [u8; 32];

#[derive(Default)]
struct Usage {
    /// Glyph names shown, per font key.
    shown: HashMap<FontKey, BTreeSet<Vec<u8>>>,
    /// Objects whose body maps a font resource name the walk resolved
    /// (a `/Font` dictionary, a resources dictionary, a form, a page
    /// tree node).
    holders: HashSet<IndirectRef>,
}

fn refuse(detail: String) -> Error {
    Error::Unsupported {
        feature: "Type 3 glyph definitions of removed text",
        detail,
    }
}

fn font_key(doc: &Document, dict: &Dict) -> Result<Option<FontKey>, Error> {
    if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) != Some(b"Type3") {
        return Ok(None);
    }
    let mut k = Vec::new();
    if let Some(m) = dict.get(b"FontMatrix".as_slice()) {
        k.extend_from_slice(format!("{:?}", doc.deref(m)?).as_bytes());
    }
    let Some(cp) = dict.get(b"CharProcs".as_slice()) else {
        return Ok(None);
    };
    let cp = doc.deref(cp)?;
    let Some(cp) = cp.as_dict() else {
        return Ok(None);
    };
    let mut names: Vec<&Vec<u8>> = cp.keys().collect();
    names.sort();
    for n in names {
        k.extend_from_slice(&(n.len() as u64).to_be_bytes());
        k.extend_from_slice(n);
        let digest = match doc.deref(&cp[n])? {
            Object::Stream { data, .. } => sha256(&data),
            _ => [0; 32],
        };
        k.extend_from_slice(&digest);
    }
    Ok(Some(sha256(&k)))
}

/// The object directly holding the `/Font` mapping of resources `res`
/// found in `owner`'s body (`res` as written there).
fn record_holder(
    doc: &Document,
    res: &Object,
    owner: Option<IndirectRef>,
    out: &mut HashSet<IndirectRef>,
) -> Result<(), Error> {
    let (body, holder) = match res {
        Object::Ref(r) => (doc.resolve(*r)?, Some(*r)),
        other => (other.clone(), owner),
    };
    match body.as_dict().and_then(|d| d.get(b"Font".as_slice())) {
        Some(Object::Ref(f)) => {
            out.insert(*f);
        }
        Some(_) => {
            if let Some(h) = holder {
                out.insert(h);
            }
        }
        None => {}
    }
    Ok(())
}

/// What the output (or input) shows in Type 3 fonts: page content, the
/// forms it draws and annotation appearances, as `drawn_xobjects` walks
/// them, with each `Tf` resolved against the resources in force.
fn usage(doc: &Document, limits: &Limits) -> Result<Usage, Error> {
    struct Frame {
        ops: Vec<Operation>,
        res: Object,
        depth: usize,
    }
    let mut u = Usage::default();
    let mut keys: HashMap<Vec<u8>, Option<(FontKey, crate::font::Font)>> = HashMap::new();
    let mut budget: u64 = 0;
    // Page tree nodes hold inherited resources.
    let mut nodes: Vec<IndirectRef> = Vec::new();
    if let Some(Object::Ref(r)) = doc.catalog().get(b"Pages".as_slice()) {
        nodes.push(*r);
    }
    let mut seen_nodes = HashSet::new();
    while let Some(n) = nodes.pop() {
        if !seen_nodes.insert(n) || seen_nodes.len() > limits.max_page_tree_nodes as usize {
            continue;
        }
        u.holders.insert(n);
        if let Some(kids) = doc
            .resolve(n)?
            .as_dict()
            .and_then(|d| d.get(b"Kids".as_slice()))
        {
            for k in doc.deref(kids)?.as_array().unwrap_or(&[]) {
                if let Object::Ref(r) = k {
                    nodes.push(*r);
                }
            }
        }
    }
    for page in doc.pages() {
        let res = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Dict::new()));
        record_holder(doc, &res, page.object_ref(), &mut u.holders)?;
        let mut stack = vec![Frame {
            ops: doc.page_operations(page)?,
            res,
            depth: 0,
        }];
        let mut seen: HashSet<(IndirectRef, Vec<u8>)> = HashSet::new();
        let enter = |r: IndirectRef,
                     inherited: &Object,
                     depth: usize,
                     holders: &mut HashSet<IndirectRef>,
                     seen: &mut HashSet<(IndirectRef, Vec<u8>)>|
         -> Result<Option<Frame>, Error> {
            let obj = doc.resolve(r)?;
            let Object::Stream { dict, data } = &obj else {
                return Ok(None);
            };
            if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Image") {
                return Ok(None);
            }
            let res = match dict.get(b"Resources".as_slice()) {
                Some(Object::Null) | None => inherited.clone(),
                Some(own) => {
                    record_holder(doc, own, Some(r), holders)?;
                    own.clone()
                }
            };
            if !seen.insert((r, format!("{res:?}").into_bytes())) {
                return Ok(None);
            }
            if depth + 1 > limits.max_xobject_depth as usize {
                return Err(Error::LimitExceeded {
                    limit: "max_xobject_depth",
                    max: limits.max_xobject_depth as u64,
                });
            }
            let decoded = doc.decode_stream(dict, data)?;
            let mut warnings = Vec::new();
            let ops = crate::content::parse_content_stream(
                &[decoded],
                limits,
                doc.recovery_mode(),
                &mut warnings,
            )?;
            Ok(Some(Frame {
                ops,
                res: match &res {
                    Object::Ref(rr) => doc.resolve(*rr)?,
                    o => o.clone(),
                },
                depth: depth + 1,
            }))
        };
        let empty = Object::Dict(Dict::new());
        for r in crate::redact::appearance_streams(doc, page)? {
            if let Some(f) = enter(r, &empty, 0, &mut u.holders, &mut seen)? {
                stack.push(f);
            }
        }
        while let Some(Frame { ops, res, depth }) = stack.pop() {
            budget = budget.saturating_add(ops.len() as u64);
            if budget > limits.max_diag_exec_ops {
                return Err(Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    max: limits.max_diag_exec_ops,
                });
            }
            // The current font (its key and dict, if Type 3) through
            // q/Q (the text state is part of the graphics state).
            let mut font: Option<(FontKey, crate::font::Font)> = None;
            let mut saved: Vec<Option<(FontKey, crate::font::Font)>> = Vec::new();
            let show = |font: &Option<(FontKey, crate::font::Font)>,
                        bytes: &[u8],
                        u: &mut Usage|
             -> Result<(), Error> {
                let Some((key, loaded)) = font else {
                    return Ok(());
                };
                let names = u.shown.entry(*key).or_default();
                let Some(t3) = loaded.as_type3() else {
                    return Ok(());
                };
                for b in bytes {
                    if let Some(n) = t3.glyph_name(*b) {
                        names.insert(n.to_vec());
                    }
                }
                Ok(())
            };
            for op in &ops {
                match op {
                    Operation::SaveState => saved.push(font.clone()),
                    Operation::RestoreState => font = saved.pop().unwrap_or(None),
                    Operation::SetTextFont { font: name, .. } => {
                        let raw = doc.resolve_resource_raw(&res, "Font", name)?;
                        let cache_key = format!("{raw:?}").into_bytes();
                        font = match keys.get(&cache_key) {
                            Some(f) => f.clone(),
                            None => {
                                let f = match &raw {
                                    Some(o) => match doc.deref(o)? {
                                        Object::Dict(d) => match font_key(doc, &d)? {
                                            Some(k) => Some((
                                                k,
                                                crate::font::load_font_dict(doc, &d, name, limits)?,
                                            )),
                                            None => None,
                                        },
                                        _ => None,
                                    },
                                    None => None,
                                };
                                keys.insert(cache_key, f.clone());
                                f
                            }
                        };
                    }
                    Operation::ShowText(b) | Operation::NextLineShowText(b) => {
                        show(&font, b, &mut u)?
                    }
                    Operation::SetSpacingNextLineShowText { text, .. } => {
                        show(&font, text, &mut u)?
                    }
                    Operation::ShowTextAdjusted(items) => {
                        for i in items {
                            if let TJItem::Text(b) = i {
                                show(&font, b, &mut u)?;
                            }
                        }
                    }
                    Operation::Do(name) => {
                        if let Some(Object::Ref(r)) =
                            doc.resolve_resource_raw(&res, "XObject", name)?
                        {
                            if let Some(f) = enter(r, &res, depth, &mut u.holders, &mut seen)? {
                                stack.push(f);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(u)
}

/// Whether `o` (not following references) refers to `target`.
fn refers_to(o: &Object, target: IndirectRef) -> bool {
    let mut stack = vec![o];
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) if *r == target => return true,
            Object::Dict(d) => stack.extend(d.values()),
            Object::Array(a) => stack.extend(a.iter()),
            Object::Stream { dict, .. } => stack.extend(dict.values()),
            _ => {}
        }
    }
    false
}

/// A ToUnicode CMap mapping each of `entries` (single-byte codes).
fn tounicode_cmap(entries: &[(u8, Vec<u16>)]) -> Vec<u8> {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    for chunk in entries.chunks(100) {
        s.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (c, u) in chunk {
            let hex: String = u.iter().map(|w| format!("{w:04X}")).collect();
            s.push_str(&format!("<{c:02X}> <{hex}>\n"));
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

/// The font dictionary `dict` keeping only the glyphs `keep`: its
/// CharProcs, the codes naming them (encoding, widths, ToUnicode).
fn pruned_font(
    doc: &Document,
    edit: &mut Edit,
    dict: &Dict,
    keep: &BTreeSet<Vec<u8>>,
    limits: &Limits,
) -> Result<Dict, Error> {
    let loaded = crate::font::load_font_dict(doc, dict, b"T3", limits)?;
    let t3 = loaded
        .as_type3()
        .ok_or_else(|| refuse("not a Type 3 font".into()))?;
    let mut out = dict.clone();
    let cp = dict
        .get(b"CharProcs".as_slice())
        .ok_or_else(|| refuse("no /CharProcs".into()))?;
    let cp = doc.deref(cp)?;
    let cp = cp
        .as_dict()
        .ok_or_else(|| refuse("/CharProcs is not a dictionary".into()))?;
    let mut new_cp = Dict::new();
    for (n, v) in cp {
        if keep.contains(n) {
            new_cp.insert(n.clone(), v.clone());
        }
    }
    out.insert(b"CharProcs".to_vec(), Object::Dict(new_cp));
    // Codes whose glyph is kept, with their names, written out in full
    // (a base encoding's names included), so nothing names a removed
    // glyph.
    let kept_codes: Vec<(u8, Vec<u8>)> = (0..=255u8)
        .filter_map(|c| t3.glyph_name(c).map(|n| (c, n.to_vec())))
        .filter(|(_, n)| keep.contains(n))
        .collect();
    let mut diffs = Vec::new();
    for (c, n) in &kept_codes {
        diffs.push(Object::Integer(*c as i64));
        diffs.push(Object::Name(n.clone()));
    }
    let mut enc = Dict::new();
    enc.insert(b"Type".to_vec(), Object::Name(b"Encoding".to_vec()));
    enc.insert(b"Differences".to_vec(), Object::Array(diffs));
    out.insert(b"Encoding".to_vec(), Object::Dict(enc));
    let kept: HashSet<u8> = kept_codes.iter().map(|(c, _)| *c).collect();
    if let Some(w) = dict.get(b"Widths".as_slice()) {
        let first = dict
            .get(b"FirstChar".as_slice())
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        let w = doc.deref(w)?;
        let arr = w
            .as_array()
            .ok_or_else(|| refuse("/Widths is not an array".into()))?;
        let new: Vec<Object> = arr
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let code = first.checked_add(i as i64);
                match code {
                    Some(c @ 0..=255) if kept.contains(&(c as u8)) => v.clone(),
                    _ => Object::Integer(0),
                }
            })
            .collect();
        out.insert(b"Widths".to_vec(), Object::Array(new));
    }
    if let Some(tu) = dict.get(b"ToUnicode".as_slice()) {
        let Object::Stream { dict: td, data } = doc.deref(tu)? else {
            return Err(refuse("/ToUnicode is not a stream".into()));
        };
        let decoded = doc.decode_stream(&td, &data)?;
        let cmap = crate::tounicode::ToUnicode::parse(&decoded, limits)
            .map_err(|e| refuse(format!("its /ToUnicode is not rewritten: {e}")))?;
        if cmap.code_len() != 1 {
            return Err(refuse("its /ToUnicode does not use one-byte codes".into()));
        }
        let entries: Vec<(u8, Vec<u16>)> = cmap
            .entries()
            .filter(|(c, _)| *c <= 255 && kept.contains(&(*c as u8)))
            .map(|(c, u)| (c as u8, u.to_vec()))
            .collect();
        let data = tounicode_cmap(&entries);
        let r = edit.alloc()?;
        let mut sd = Dict::new();
        sd.insert(b"Length".to_vec(), Object::Integer(data.len() as i64));
        edit.overlay
            .insert((r.number, r.generation), Object::Stream { dict: sd, data });
        out.insert(b"ToUnicode".to_vec(), Object::Ref(r));
    }
    Ok(out)
}

/// Prune the Type 3 glyph definitions `output` no longer shows that
/// `input` showed (see the module documentation). `None`: nothing to
/// prune.
pub(crate) fn prune_type3(
    input: &Document,
    output: &[u8],
    limits: &Limits,
) -> Result<Option<Vec<u8>>, Error> {
    // Cheap exit: no Type 3 font in the input.
    let any = input.xref_entries().any(|(r, _)| {
        matches!(input.resolve(r), Ok(Object::Dict(d))
            if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Type3"))
    });
    if !any {
        return Ok(None);
    }
    let before = usage(input, limits)?;
    let out = Document::from_bytes_with_limits(output, limits.clone())?;
    let after = usage(&out, limits)?;
    let empty = BTreeSet::new();
    let refs: Vec<IndirectRef> = out
        .xref_entries()
        .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
        .map(|(r, _)| r)
        .collect();
    let mut bodies: Vec<(IndirectRef, Object)> = Vec::with_capacity(refs.len());
    for r in &refs {
        bodies.push((*r, out.resolve(*r)?));
    }
    let mut plans: Vec<(IndirectRef, Dict, BTreeSet<Vec<u8>>)> = Vec::new();
    for (r, body) in &bodies {
        // Direct Type 3 fonts, nested in another object's body.
        let mut stack: Vec<(&Object, bool)> = vec![(body, true)];
        while let Some((o, top)) = stack.pop() {
            match o {
                Object::Dict(d) => {
                    if let Some(key) = font_key(&out, d)? {
                        let shown_in = before.shown.get(&key).unwrap_or(&empty);
                        let shown_out = after.shown.get(&key).unwrap_or(&empty);
                        if shown_in.iter().any(|n| !shown_out.contains(n)) {
                            if !top {
                                return Err(refuse(format!(
                                    "a direct Type 3 font dictionary (in object {r}) loses glyphs"
                                )));
                            }
                            plans.push((*r, d.clone(), shown_out.clone()));
                        }
                    }
                    stack.extend(d.values().map(|v| (v, false)));
                }
                Object::Array(a) => stack.extend(a.iter().map(|v| (v, false))),
                Object::Stream { dict, .. } => stack.extend(dict.values().map(|v| (v, false))),
                _ => {}
            }
        }
    }
    if plans.is_empty() {
        return Ok(None);
    }
    for (f, _, _) in &plans {
        for (r, body) in &bodies {
            if r != f && refers_to(body, *f) && !after.holders.contains(r) {
                return Err(refuse(format!(
                    "the font (object {f}) is also used from object {r}, where its glyphs are not \
                     evaluated"
                )));
            }
        }
    }
    let mut edit = Edit::new(&out, limits.clone())?;
    for (f, dict, keep) in &plans {
        let new = pruned_font(&out, &mut edit, dict, keep, limits)?;
        edit.overlay
            .insert((f.number, f.generation), Object::Dict(new));
    }
    Ok(Some(edit.rewrite_with_overrides(
        &crate::rewrite::RewriteOverrides {
            strip_info: true,
            strip_metadata: true,
        },
    )?))
}
