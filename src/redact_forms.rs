//! The form fields of the widgets `redact` removes
//! (`RedactOptions::remove_annotations`).
//!
//! A widget is a field's annotation: removing it from its page leaves
//! the field — its value (`/V`, `/DV`, `/RV`) among it — in the
//! interactive form unless the field goes too. The field tree is read
//! from the AcroForm's `/Fields` (cycles, direct field dictionaries, a
//! widget whose `/Parent` is not the field the tree puts it under, and
//! too deep a tree are refused); every removed widget leaves its field's
//! `/Kids` (or `/Fields`), and a field all of whose kids are gone goes
//! with them, up the tree; `/CO` loses the fields gone. A field that
//! keeps a widget (on another page, or outside the rects) keeps its value:
//! it is shown there, and the value is searched for in `elsewhere`.
//! Refused: a signature field (`/FT /Sig`, inherited too), an XFA form
//! (its datasets hold the values as well), a direct widget.

use std::collections::{HashMap, HashSet};

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};

/// What the form edit removed.
#[derive(Default, Debug)]
pub(crate) struct FieldsEdit {
    /// Fields removed (merged field-widgets included).
    pub(crate) fields_removed: u64,
    /// Their values and descriptions, searched for in `elsewhere`.
    pub(crate) removed_text: Vec<String>,
    /// Prints of the removed field dictionaries (verified absent).
    pub(crate) prints: Vec<[u8; 32]>,
    /// The removed field objects (merged field-widgets included).
    pub(crate) gone: HashSet<IndirectRef>,
}

fn refuse(detail: impl Into<String>) -> Error {
    Error::Unsupported {
        feature: "redaction of a widget whose form field cannot be edited exactly",
        detail: detail.into(),
    }
}

type Overlay = HashMap<(u32, u16), Object>;

fn current(doc: &Document, overlay: &Overlay, r: IndirectRef) -> Result<Object, Error> {
    match overlay.get(&(r.number, r.generation)) {
        Some(o) => Ok(o.clone()),
        None => doc.resolve(r),
    }
}

fn deref_cur(doc: &Document, overlay: &Overlay, o: &Object) -> Result<Object, Error> {
    match o {
        Object::Ref(r) => current(doc, overlay, *r),
        o => Ok(o.clone()),
    }
}

/// Remove the fields of the widgets `removed` (see the module
/// documentation), editing `overlay`.
pub(crate) fn edit_fields(
    doc: &Document,
    overlay: &mut Overlay,
    removed: &HashSet<IndirectRef>,
    limits: &Limits,
) -> Result<FieldsEdit, Error> {
    let mut out = FieldsEdit::default();
    if removed.is_empty() {
        return Ok(out);
    }
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => *r,
        _ => return Err(refuse("the trailer /Root is not a reference")),
    };
    let catalog = match current(doc, overlay, root)? {
        Object::Dict(d) => d,
        _ => return Err(refuse("the catalog is not a dictionary")),
    };
    // Where the AcroForm dictionary is written: its own object, or the
    // catalog.
    let (form_ref, form) = match catalog.get(b"AcroForm".as_slice()) {
        None => return Ok(out),
        Some(Object::Ref(r)) => match current(doc, overlay, *r)? {
            Object::Dict(d) => (Some(*r), d),
            _ => return Err(refuse("/AcroForm is not a dictionary")),
        },
        Some(Object::Dict(d)) => (None, d.clone()),
        Some(_) => return Err(refuse("/AcroForm is not a dictionary")),
    };
    if form.contains_key(b"XFA".as_slice()) {
        return Err(refuse(
            "an XFA form (its datasets hold the field values too)",
        ));
    }
    let fields = match form.get(b"Fields".as_slice()) {
        Some(f) => deref_cur(doc, overlay, f)?,
        None => Object::Array(Vec::new()),
    };
    let fields = fields
        .as_array()
        .ok_or_else(|| refuse("/Fields is not an array"))?
        .to_vec();
    // The field tree: each node's parent (None: in /Fields) and kids.
    let mut parent: HashMap<IndirectRef, Option<IndirectRef>> = HashMap::new();
    let mut kids: HashMap<IndirectRef, Vec<IndirectRef>> = HashMap::new();
    let mut dicts: HashMap<IndirectRef, Dict> = HashMap::new();
    let mut stack: Vec<(Object, Option<IndirectRef>, u32)> =
        fields.iter().map(|f| (f.clone(), None, 0)).collect();
    while let Some((o, p, depth)) = stack.pop() {
        let Object::Ref(r) = o else {
            return Err(refuse("a direct field dictionary"));
        };
        if depth > 64 {
            return Err(refuse("a field tree deeper than 64"));
        }
        if parent.insert(r, p).is_some() {
            return Err(refuse(format!(
                "field {r} is reached twice (a cycle or a shared kid)"
            )));
        }
        if parent.len() as u64 > limits.max_audit_elements {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: limits.max_audit_elements,
            });
        }
        let d = match current(doc, overlay, r)? {
            Object::Dict(d) => d,
            _ => return Err(refuse(format!("field {r} is not a dictionary"))),
        };
        // A kid's /Parent names the field the tree puts it under.
        if let Some(pr) = p {
            if !matches!(d.get(b"Parent".as_slice()), Some(Object::Ref(x)) if *x == pr) {
                return Err(refuse(format!(
                    "field {r}'s /Parent is not the field above it"
                )));
            }
        }
        let ks: Vec<Object> = match d.get(b"Kids".as_slice()) {
            Some(k) => deref_cur(doc, overlay, k)?
                .as_array()
                .ok_or_else(|| refuse(format!("field {r}'s /Kids is not an array")))?
                .to_vec(),
            None => Vec::new(),
        };
        let mut refs = Vec::new();
        for k in ks {
            let Object::Ref(kr) = k else {
                return Err(refuse("a direct field dictionary"));
            };
            refs.push(kr);
            stack.push((Object::Ref(kr), Some(r), depth + 1));
        }
        kids.insert(r, refs);
        dicts.insert(r, d);
    }
    // Every removed widget is a node of the tree, or a widget of no
    // field (no /Parent, no field keys: nothing to edit).
    for w in removed {
        if parent.contains_key(w) {
            continue;
        }
        let d = match current(doc, overlay, *w)? {
            Object::Dict(d) => d,
            _ => continue,
        };
        if d.contains_key(b"Parent".as_slice())
            || d.contains_key(b"FT".as_slice())
            || d.contains_key(b"V".as_slice())
        {
            return Err(refuse(format!(
                "widget {w} belongs to a field that /Fields does not reach"
            )));
        }
    }
    // Signature fields (the field type is inheritable).
    for w in removed.iter().filter(|w| parent.contains_key(w)) {
        let mut cur = Some(*w);
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(ft) = dicts[&c].get(b"FT".as_slice()) {
                if deref_cur(doc, overlay, ft)?.as_name() == Some(b"Sig") {
                    return Err(refuse("a signature field"));
                }
                break;
            }
            cur = parent[&c];
            n += 1;
            if n > 64 {
                break;
            }
        }
    }
    // What a removed widget showed: its field's value, inherited or
    // its own — searched for in `elsewhere` (the field may keep it).
    for w in removed.iter().filter(|w| parent.contains_key(w)) {
        let mut cur = Some(*w);
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(v) = dicts[&c].get(b"V".as_slice()) {
                if let Object::String(t) = deref_cur(doc, overlay, v)? {
                    out.removed_text.push(crate::annot_text::text_string(&t));
                }
                break;
            }
            cur = parent[&c];
            n += 1;
            if n > 64 {
                break;
            }
        }
    }
    // Gone: the removed widgets, and fields all of whose kids are gone.
    let mut gone: HashSet<IndirectRef> = removed
        .iter()
        .filter(|w| parent.contains_key(w))
        .copied()
        .collect();
    if gone.is_empty() {
        return Ok(out);
    }
    // Each field's kids left, counted down as kids go (linear in the
    // tree, however many widgets go).
    let mut left: HashMap<IndirectRef, usize> = kids.iter().map(|(r, k)| (*r, k.len())).collect();
    let mut frontier: Vec<IndirectRef> = gone.iter().copied().collect();
    while let Some(c) = frontier.pop() {
        if let Some(Some(p)) = parent.get(&c) {
            if gone.contains(p) {
                continue;
            }
            let n = left.get_mut(p).expect("a parent's kids are counted");
            *n = n.saturating_sub(1);
            if *n == 0 {
                gone.insert(*p);
                frontier.push(*p);
            }
        }
    }
    // Rewrite the kids of the fields kept, and /Fields, /CO.
    let keep = |list: &[Object]| -> Vec<Object> {
        list.iter()
            .filter(|o| !matches!(o, Object::Ref(r) if gone.contains(r)))
            .cloned()
            .collect()
    };
    for (r, ks) in &kids {
        if gone.contains(r) || !ks.iter().any(|k| gone.contains(k)) {
            continue;
        }
        let mut d = dicts[r].clone();
        match d.get(b"Kids".as_slice()).cloned() {
            Some(Object::Ref(kr)) => {
                let a = current(doc, overlay, kr)?;
                let a = keep(a.as_array().unwrap_or(&[]));
                overlay.insert((kr.number, kr.generation), Object::Array(a));
            }
            Some(Object::Array(a)) => {
                d.insert(b"Kids".to_vec(), Object::Array(keep(&a)));
                overlay.insert((r.number, r.generation), Object::Dict(d));
            }
            _ => {}
        }
    }
    let mut form = form;
    match form.get(b"Fields".as_slice()).cloned() {
        Some(Object::Ref(fr)) => {
            overlay.insert((fr.number, fr.generation), Object::Array(keep(&fields)));
        }
        Some(_) => {
            form.insert(b"Fields".to_vec(), Object::Array(keep(&fields)));
        }
        None => {}
    }
    match form.get(b"CO".as_slice()).cloned() {
        Some(Object::Ref(cr)) => {
            let a = current(doc, overlay, cr)?;
            let a = keep(a.as_array().unwrap_or(&[]));
            overlay.insert((cr.number, cr.generation), Object::Array(a));
        }
        Some(Object::Array(a)) => {
            form.insert(b"CO".to_vec(), Object::Array(keep(&a)));
        }
        _ => {}
    }
    match form_ref {
        Some(fr) => {
            overlay.insert((fr.number, fr.generation), Object::Dict(form));
        }
        None => {
            let mut c = catalog;
            c.insert(b"AcroForm".to_vec(), Object::Dict(form));
            overlay.insert((root.number, root.generation), Object::Dict(c));
        }
    }
    out.gone = gone.clone();
    // What the removed fields held.
    for r in &gone {
        let d = &dicts[r];
        out.prints.push(crate::remove_region::annotation_print(d));
        for key in [b"V".as_slice(), b"DV", b"RV", b"TU"] {
            match d.get(key).map(|o| deref_cur(doc, overlay, o)).transpose()? {
                Some(Object::String(t)) => {
                    out.removed_text.push(crate::annot_text::text_string(&t))
                }
                Some(Object::Stream { dict, data }) => out.removed_text.push(
                    crate::annot_text::text_string(&doc.decode_stream(&dict, &data)?),
                ),
                Some(Object::Array(a)) => {
                    for x in a {
                        if let Object::String(t) = deref_cur(doc, overlay, &x)? {
                            out.removed_text.push(crate::annot_text::text_string(&t));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(mk) = d.get(b"MK".as_slice()) {
            if let Some(mk) = deref_cur(doc, overlay, mk)?.as_dict() {
                for key in [b"CA".as_slice(), b"RC", b"AC"] {
                    if let Some(Object::String(t)) = mk.get(key) {
                        out.removed_text.push(crate::annot_text::text_string(t));
                    }
                }
            }
        }
        // A widget is counted as an annotation; a field is counted here.
        if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) != Some(b"Widget")
            || d.contains_key(b"T".as_slice())
        {
            out.fields_removed += 1;
        }
    }
    Ok(out)
}

/// The first of `gone` (removed annotations and fields) that the
/// document as edited still reaches from its trailer — through any key,
/// a kept action's `/Fields`, a private entry, an object stream's member
/// — with the path, or `None`. The rewrite writes what the trailer
/// reaches, so such an object would be written whole, whatever later
/// passes did to its content. The trailer's `/Info` and the catalog's
/// `/Metadata` are left out: the rewrite drops them.
pub(crate) fn removed_still_reached(
    doc: &Document,
    overlay: &Overlay,
    gone: &HashSet<IndirectRef>,
    limits: &Limits,
) -> Result<Option<(IndirectRef, String)>, Error> {
    if gone.is_empty() {
        return Ok(None);
    }
    let mut seen: HashSet<IndirectRef> = HashSet::new();
    let mut queue: std::collections::VecDeque<(IndirectRef, String)> = Default::default();
    let mut budget: u64 = 0;
    // Direct values walked in place, references queued.
    let mut scan = |o: &Object,
                    path: &str,
                    top: Option<&[u8]>,
                    queue: &mut std::collections::VecDeque<(IndirectRef, String)>|
     -> Result<(), Error> {
        let mut stack: Vec<(&Object, String)> = vec![(o, path.to_string())];
        while let Some((o, p)) = stack.pop() {
            budget += 1;
            if budget > limits.max_audit_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_audit_elements",
                    max: limits.max_audit_elements,
                });
            }
            match o {
                Object::Ref(r) => queue.push_back((*r, p)),
                Object::Array(a) => {
                    for (i, v) in a.iter().enumerate() {
                        stack.push((v, format!("{p}[{i}]")));
                    }
                }
                Object::Dict(d) | Object::Stream { dict: d, .. } => {
                    for (k, v) in d {
                        if top.is_some_and(|t| t == k.as_slice()) && p == path {
                            continue;
                        }
                        stack.push((v, format!("{p}/{}", String::from_utf8_lossy(k))));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    };
    let trailer = Object::Dict(doc.trailer().clone());
    scan(&trailer, "", Some(b"Info"), &mut queue)?;
    while let Some((r, path)) = queue.pop_front() {
        if gone.contains(&r) {
            return Ok(Some((r, path)));
        }
        if !seen.insert(r) {
            continue;
        }
        let o = match current(doc, overlay, r) {
            Ok(o) => o,
            Err(Error::MissingReference(_)) => continue,
            Err(e) => return Err(e),
        };
        let skip: Option<&[u8]> = (path == "/Root").then_some(b"Metadata".as_slice());
        scan(&o, &path, skip, &mut queue)?;
    }
    Ok(None)
}
