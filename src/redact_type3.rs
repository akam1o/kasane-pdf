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

/// A Type 3 font by content: its whole dictionary with every reference
/// replaced by what it refers to (streams by their stored bytes'
/// digest) — CharProcs, encoding, widths, ToUnicode, resources — so the
/// same font is found again after rewriting renumbers it, and only
/// fonts identical in everything share a key.
type FontKey = [u8; 32];

#[derive(Default)]
struct Usage {
    /// Glyph names shown, per font key.
    shown: HashMap<FontKey, BTreeSet<Vec<u8>>>,
    /// What the walk reads content from: pages, page tree nodes, the
    /// forms and annotation appearances it entered.
    roots: HashSet<IndirectRef>,
    /// Resources and `/Font` dictionaries the walk resolved names in.
    intermediates: HashSet<IndirectRef>,
}

fn refuse(detail: String) -> Error {
    Error::Unsupported {
        feature: "Type 3 glyph definitions of removed text",
        detail,
    }
}

/// Digests of objects by reference, and the work spent, for
/// [`font_key`]: each object is digested once (shared subgraphs do not
/// multiply the work), and the total is bounded.
#[derive(Default)]
struct Canon {
    memo: HashMap<IndirectRef, [u8; 32]>,
    open: HashSet<IndirectRef>,
    work: u64,
    depth: u32,
}

/// The digest of `o` written canonically, each reference replaced by
/// its target's digest.
fn canonical(
    doc: &Document,
    o: &Object,
    c: &mut Canon,
    limits: &Limits,
) -> Result<[u8; 32], Error> {
    // References nest without bound in a file: the recursion is capped.
    if c.depth >= limits.max_nesting_depth {
        return Err(Error::LimitExceeded {
            limit: "max_nesting_depth (canonical digest)",
            max: limits.max_nesting_depth as u64,
        });
    }
    c.depth += 1;
    let r = canonical_inner(doc, o, c, limits);
    c.depth -= 1;
    r
}

fn canonical_inner(
    doc: &Document,
    o: &Object,
    c: &mut Canon,
    limits: &Limits,
) -> Result<[u8; 32], Error> {
    c.work += 1;
    if c.work > limits.max_audit_elements {
        return Err(Error::LimitExceeded {
            limit: "max_audit_elements",
            max: limits.max_audit_elements,
        });
    }
    let mut out = Vec::new();
    match o {
        Object::Ref(r) => {
            if let Some(d) = c.memo.get(r) {
                return Ok(*d);
            }
            if !c.open.insert(*r) {
                return Err(refuse("an object refers to itself".into()));
            }
            let d = canonical(doc, &doc.resolve(*r)?, c, limits)?;
            c.open.remove(r);
            c.memo.insert(*r, d);
            return Ok(d);
        }
        Object::Dict(d) => {
            out.push(b'<');
            let mut keys: Vec<&Vec<u8>> = d.keys().collect();
            keys.sort();
            for k in keys {
                out.extend_from_slice(&(k.len() as u64).to_be_bytes());
                out.extend_from_slice(k);
                out.extend_from_slice(&canonical(doc, &d[k], c, limits)?);
            }
        }
        Object::Array(a) => {
            out.push(b'[');
            for v in a {
                out.extend_from_slice(&canonical(doc, v, c, limits)?);
            }
        }
        Object::Stream { dict, data } => {
            out.push(b'S');
            out.extend_from_slice(&sha256(data));
            out.extend_from_slice(&canonical(doc, &Object::Dict(dict.clone()), c, limits)?);
        }
        other => {
            out.push(b'.');
            out.extend_from_slice(format!("{other:?}").as_bytes());
        }
    }
    Ok(sha256(&out))
}

/// The digest of `o` written canonically, references replaced by the
/// digests of what they refer to: the same in an output that renumbered
/// its objects.
pub(crate) fn content_digest(
    doc: &Document,
    o: &Object,
    limits: &Limits,
) -> Result<[u8; 32], Error> {
    canonical(doc, o, &mut Canon::default(), limits)
}

fn font_key(
    doc: &Document,
    dict: &Dict,
    c: &mut Canon,
    limits: &Limits,
) -> Result<Option<FontKey>, Error> {
    if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) != Some(b"Type3")
        || !dict.contains_key(b"CharProcs".as_slice())
    {
        return Ok(None);
    }
    Ok(Some(canonical(
        doc,
        &Object::Dict(dict.clone()),
        c,
        limits,
    )?))
}

/// A glyph name and its CharProc's digest (stored bytes).
type GlyphDigest = (Vec<u8>, [u8; 32]);

/// The digest of each glyph's CharProc.
fn charproc_digests(doc: &Document, dict: &Dict) -> Result<Vec<GlyphDigest>, Error> {
    let Some(cp) = dict.get(b"CharProcs".as_slice()) else {
        return Ok(Vec::new());
    };
    let cp = doc.deref(cp)?;
    let mut out = Vec::new();
    for (n, v) in cp.as_dict().into_iter().flatten() {
        if let Object::Stream { data, .. } = doc.deref(v)? {
            out.push((n.clone(), sha256(&data)));
        }
    }
    Ok(out)
}

/// The object directly holding the `/Font` mapping of resources `res`
/// found in `owner`'s body (`res` as written there).
fn record_holder(
    doc: &Document,
    res: &Object,
    out: &mut HashSet<IndirectRef>,
) -> Result<(), Error> {
    let body = match res {
        Object::Ref(r) => {
            out.insert(*r);
            doc.resolve(*r)?
        }
        other => other.clone(),
    };
    if let Some(Object::Ref(f)) = body.as_dict().and_then(|d| d.get(b"Font".as_slice())) {
        out.insert(*f);
    }
    Ok(())
}

/// What the output (or input) shows in Type 3 fonts: page content, the
/// forms it draws and annotation appearances, as `drawn_xobjects` walks
/// them, with each `Tf` resolved against the resources in force.
fn usage(doc: &Document, limits: &Limits) -> Result<Usage, Error> {
    type Current = Option<(FontKey, crate::font::Font)>;
    struct Frame {
        ops: Vec<Operation>,
        res: Object,
        depth: usize,
        /// The font in force where the stream is invoked (a form
        /// inherits the text state).
        font: Current,
    }
    let mut u = Usage::default();
    let mut canon = Canon::default();
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
        u.roots.insert(n);
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
        record_holder(doc, &res, &mut u.intermediates)?;
        if let Some(p) = page.object_ref() {
            u.roots.insert(p);
        }
        let mut stack = vec![Frame {
            ops: {
                let (ops, w) = doc.page_operations_warned(page)?;
                crate::redact_diag::refuse_raw_cr(&w)?;
                ops
            },
            res,
            depth: 0,
            font: None,
        }];
        let mut seen: HashSet<(IndirectRef, Vec<u8>)> = HashSet::new();
        let enter = |r: IndirectRef,
                     inherited: &Object,
                     depth: usize,
                     font: &Current,
                     u: &mut Usage,
                     seen: &mut HashSet<(IndirectRef, Vec<u8>)>|
         -> Result<Option<Frame>, Error> {
            let obj = doc.resolve(r)?;
            let Object::Stream { dict, data } = &obj else {
                return Ok(None);
            };
            if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Image") {
                return Ok(None);
            }
            u.roots.insert(r);
            let res = match dict.get(b"Resources".as_slice()) {
                Some(Object::Null) | None => inherited.clone(),
                Some(own) => {
                    record_holder(doc, own, &mut u.intermediates)?;
                    own.clone()
                }
            };
            let mut seen_key = format!("{res:?}").into_bytes();
            if let Some((k, _)) = font {
                seen_key.extend_from_slice(k);
            }
            if !seen.insert((r, seen_key)) {
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
            // The codes counted must be the codes every reader shows.
            crate::redact_diag::refuse_raw_cr(&warnings)?;
            Ok(Some(Frame {
                ops,
                res: match &res {
                    Object::Ref(rr) => doc.resolve(*rr)?,
                    o => o.clone(),
                },
                depth: depth + 1,
                font: font.clone(),
            }))
        };
        let empty = Object::Dict(Dict::new());
        for r in crate::redact::appearance_streams(doc, page)? {
            if let Some(f) = enter(r, &empty, 0, &None, &mut u, &mut seen)? {
                stack.push(f);
            }
        }
        while let Some(Frame {
            ops,
            res,
            depth,
            font: inherited_font,
        }) = stack.pop()
        {
            budget = budget.saturating_add(ops.len() as u64);
            if budget > limits.max_diag_exec_ops {
                return Err(Error::LimitExceeded {
                    limit: "max_diag_exec_ops",
                    max: limits.max_diag_exec_ops,
                });
            }
            // The current font (its key and dict, if Type 3) through
            // q/Q (the text state is part of the graphics state).
            let mut font: Current = inherited_font;
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
                                        Object::Dict(d) => {
                                            match font_key(doc, &d, &mut canon, limits)? {
                                                Some(k) => Some((
                                                    k,
                                                    crate::font::load_font_dict(
                                                        doc, &d, name, limits,
                                                    )?,
                                                )),
                                                None => None,
                                            }
                                        }
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
                            if let Some(f) = enter(r, &res, depth, &font, &mut u, &mut seen)? {
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

/// The XObject names a CharProc draws (`Do`).
fn drawn_names(doc: &Document, cp: &Object, limits: &Limits) -> Result<BTreeSet<Vec<u8>>, Error> {
    let Object::Stream { dict, data } = doc.deref(cp)? else {
        return Ok(BTreeSet::new());
    };
    let decoded = doc.decode_stream(&dict, &data)?;
    let mut w = Vec::new();
    let ops = crate::content::parse_content_stream(&[decoded], limits, false, &mut w)?;
    Ok(ops
        .into_iter()
        .filter_map(|o| match o {
            Operation::Do(n) => Some(n),
            _ => None,
        })
        .collect())
}

/// The font dictionary `dict` keeping only the glyphs `keep`: its
/// CharProcs, the codes naming them (encoding, widths, ToUnicode), the
/// images they draw. Digests of the images only removed glyphs drew
/// are added to `gone`.
fn pruned_font(
    doc: &Document,
    edit: &mut Edit,
    dict: &Dict,
    keep: &BTreeSet<Vec<u8>>,
    limits: &Limits,
    gone: &mut Vec<[u8; 32]>,
    procs: &mut Vec<[u8; 32]>,
    tounicodes: &mut Vec<[u8; 32]>,
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
        } else if let Object::Stream { dict, data } = doc.deref(v)? {
            // A removed glyph's procedure may not stay by another route.
            procs.extend([sha256(&doc.decode_stream(&dict, &data)?), sha256(&data)]);
        }
    }
    // The images: those the kept glyphs draw stay in the font's
    // resources; the others leave with their glyphs.
    let mut kept_names = BTreeSet::new();
    let mut gone_names = BTreeSet::new();
    for (n, v) in cp {
        let names = drawn_names(doc, v, limits)?;
        if keep.contains(n) {
            kept_names.extend(names);
        } else {
            gone_names.extend(names);
        }
    }
    gone_names.retain(|n| !kept_names.contains(n));
    if !gone_names.is_empty() {
        let Some(res) = dict.get(b"Resources".as_slice()) else {
            return Err(refuse(
                "a removed glyph draws an image from resources the font inherits".into(),
            ));
        };
        let res = doc.deref(res)?;
        let mut res = res
            .as_dict()
            .ok_or_else(|| refuse("the font's /Resources is not a dictionary".into()))?
            .clone();
        let xo = match res.get(b"XObject".as_slice()) {
            Some(x) => doc.deref(x)?,
            None => Object::Dict(Dict::new()),
        };
        let xo = xo
            .as_dict()
            .ok_or_else(|| refuse("the font's /XObject is not a dictionary".into()))?;
        let mut new_xo = Dict::new();
        for (n, v) in xo {
            if gone_names.contains(n) {
                if let Object::Stream { dict, data } = doc.deref(v)? {
                    gone.push(sha256(&data));
                    // Its masks hold its shape.
                    for key in [b"SMask".as_slice(), b"Mask"] {
                        if let Some(m @ Object::Ref(_)) = dict.get(key) {
                            if let Object::Stream { dict, data } = doc.deref(m)? {
                                gone.extend([
                                    sha256(&doc.decode_stream(&dict, &data)?),
                                    sha256(&data),
                                ]);
                            }
                        }
                    }
                }
            } else {
                new_xo.insert(n.clone(), v.clone());
            }
        }
        res.insert(b"XObject".to_vec(), Object::Dict(new_xo));
        out.insert(b"Resources".to_vec(), Object::Dict(res));
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
        // The original maps the removed codes too.
        tounicodes.extend([sha256(&decoded), sha256(&data)]);
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

/// What pruning did: the rewritten output, and the digests of the
/// images only removed glyphs drew (each must be gone from the output
/// or still drawn there).
pub(crate) struct Pruned {
    pub(crate) output: Vec<u8>,
    pub(crate) images: Vec<[u8; 32]>,
    /// Digests of the removed glyphs' procedures: each may stay only as
    /// a Type 3 font's CharProc (the same bytes still a glyph there).
    pub(crate) procs: Vec<[u8; 32]>,
    /// Digests of the replaced `/ToUnicode` streams: none may stay.
    pub(crate) tounicodes: Vec<[u8; 32]>,
}

/// Prune the Type 3 glyph definitions `output` no longer shows that
/// `input` showed (see the module documentation). `None`: nothing to
/// prune.
pub(crate) fn prune_type3(
    input: &Document,
    output: &[u8],
    limits: &Limits,
) -> Result<Option<Pruned>, Error> {
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
    let mut canon = Canon::default();
    // Every Type 3 font of the output: (object, nested in another
    // object's body, dict, key).
    let mut fonts: Vec<(IndirectRef, bool, Dict, FontKey)> = Vec::new();
    for (r, body) in &bodies {
        let mut stack: Vec<(&Object, bool)> = vec![(body, true)];
        while let Some((o, top)) = stack.pop() {
            match o {
                Object::Dict(d) => {
                    if let Some(key) = font_key(&out, d, &mut canon, limits)? {
                        fonts.push((*r, !top, d.clone(), key));
                    }
                    stack.extend(d.values().map(|v| (v, false)));
                }
                Object::Array(a) => stack.extend(a.iter().map(|v| (v, false))),
                Object::Stream { dict, .. } => stack.extend(dict.values().map(|v| (v, false))),
                _ => {}
            }
        }
    }
    // A font loses the glyphs it no longer shows when one it showed is
    // gone — and so does any font keeping, unshown, a CharProc such a
    // glyph was drawn by (until nothing changes).
    let digests: Vec<Vec<GlyphDigest>> = fonts
        .iter()
        .map(|(_, _, d, _)| charproc_digests(&out, d))
        .collect::<Result<_, _>>()?;
    let mut planned: Vec<bool> = vec![false; fonts.len()];
    let mut gone: HashSet<[u8; 32]> = HashSet::new();
    loop {
        let mut changed = false;
        for (i, (_, _, _, key)) in fonts.iter().enumerate() {
            if planned[i] {
                continue;
            }
            let shown_in = before.shown.get(key).unwrap_or(&empty);
            let shown_out = after.shown.get(key).unwrap_or(&empty);
            let digests = &digests[i];
            let lost = shown_in.iter().any(|n| !shown_out.contains(n))
                || digests
                    .iter()
                    .any(|(n, dg)| !shown_out.contains(n) && gone.contains(dg));
            if lost {
                planned[i] = true;
                changed = true;
                gone.extend(
                    digests
                        .iter()
                        .filter(|(n, _)| !shown_out.contains(n))
                        .map(|(_, dg)| *dg),
                );
            }
        }
        if !changed {
            break;
        }
    }
    let mut plans: Vec<(IndirectRef, Dict, BTreeSet<Vec<u8>>)> = Vec::new();
    for (i, (r, nested, d, key)) in fonts.iter().enumerate() {
        if !planned[i] {
            continue;
        }
        if *nested {
            return Err(refuse(format!(
                "a direct Type 3 font dictionary (in object {r}) loses glyphs"
            )));
        }
        plans.push((
            *r,
            d.clone(),
            after.shown.get(key).unwrap_or(&empty).clone(),
        ));
    }
    if plans.is_empty() {
        return Ok(None);
    }
    // Who refers to whom, once (bounded like the audit's graph).
    let mut referrers: HashMap<IndirectRef, Vec<IndirectRef>> = HashMap::new();
    let mut edges: u64 = 0;
    for (r, body) in &bodies {
        let mut stack = vec![body];
        while let Some(o) = stack.pop() {
            match o {
                Object::Ref(t) => {
                    edges += 1;
                    if edges > limits.max_audit_edges {
                        return Err(Error::LimitExceeded {
                            limit: "max_audit_edges",
                            max: limits.max_audit_edges,
                        });
                    }
                    referrers.entry(*t).or_default().push(*r);
                }
                Object::Dict(d) => stack.extend(d.values()),
                Object::Array(a) => stack.extend(a.iter()),
                Object::Stream { dict, .. } => stack.extend(dict.values()),
                _ => {}
            }
        }
    }
    // A font losing glyphs may be reached only through what the walk
    // read: up from the font, every referrer is a page, a page tree
    // node, a form or appearance the walk entered, or a resources or
    // /Font dictionary it resolved names in — whose referrers are held
    // to the same rule.
    for (f, _, _) in &plans {
        let mut stack = vec![*f];
        let mut seen = HashSet::new();
        while let Some(o) = stack.pop() {
            if !seen.insert(o) {
                continue;
            }
            for r in referrers.get(&o).into_iter().flatten() {
                if after.roots.contains(r) {
                    continue;
                }
                if after.intermediates.contains(r) {
                    stack.push(*r);
                    continue;
                }
                return Err(refuse(format!(
                    "the font (object {f}) is also reached from object {r}, where its glyphs are \
                     not evaluated"
                )));
            }
        }
    }
    let mut edit = Edit::new(&out, limits.clone())?;
    let mut images = Vec::new();
    let mut procs = Vec::new();
    let mut tounicodes = Vec::new();
    for (f, dict, keep) in &plans {
        let new = pruned_font(
            &out,
            &mut edit,
            dict,
            keep,
            limits,
            &mut images,
            &mut procs,
            &mut tounicodes,
        )?;
        edit.overlay
            .insert((f.number, f.generation), Object::Dict(new));
    }
    let output = edit.rewrite_with_overrides(&crate::rewrite::RewriteOverrides {
        strip_info: true,
        strip_metadata: true,
        ..Default::default()
    })?;
    Ok(Some(Pruned {
        output,
        images,
        procs,
        tounicodes,
    }))
}
