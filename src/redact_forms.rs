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
    check_kept_fields(
        doc, overlay, removed, &gone, &parent, &kids, &dicts, &mut out,
    )?;
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
/// passes did to its content. It walks what `redact`'s rewrite writes:
/// the trailer's `/Root` and what it reaches, but the catalog's
/// `/Metadata` (dropped, like the trailer's `/Info` and every other
/// trailer key). `redact` also checks the rewrite's object map itself.
pub(crate) fn removed_still_reached(
    doc: &Document,
    overlay: &Overlay,
    gone: &HashSet<IndirectRef>,
    limits: &Limits,
) -> Result<Option<(IndirectRef, String)>, Error> {
    if gone.is_empty() {
        return Ok(None);
    }
    // Breadth first; each reference queued with the step from the object
    // that holds it (its index in `steps`), the path built only when
    // reported (a long chain would otherwise copy ever longer paths).
    let mut steps: Vec<(usize, String)> = Vec::new();
    let mut seen: HashSet<IndirectRef> = HashSet::new();
    let mut queue: std::collections::VecDeque<(IndirectRef, usize)> = Default::default();
    let mut budget: u64 = 0;
    let path_of = |steps: &Vec<(usize, String)>, mut at: usize| {
        let mut parts = Vec::new();
        while at != usize::MAX {
            let (up, step) = &steps[at];
            parts.push(step.clone());
            at = *up;
        }
        parts.reverse();
        parts.concat()
    };
    if let Some(Object::Ref(root)) = doc.trailer().get(b"Root".as_slice()) {
        steps.push((usize::MAX, "/Root".into()));
        queue.push_back((*root, 0));
    }
    while let Some((r, at)) = queue.pop_front() {
        if gone.contains(&r) {
            return Ok(Some((r, path_of(&steps, at))));
        }
        if !seen.insert(r) {
            continue;
        }
        // One hop: a chain's links are objects the writer writes.
        let o = match current_hop(doc, overlay, r) {
            Ok(o) => o,
            Err(Error::MissingReference(_)) => continue,
            Err(e) => return Err(e),
        };
        // The catalog's /Metadata is not written.
        let catalog = at == 0;
        // Direct values walked in place (with their key path inside the
        // object), references queued.
        let mut stack: Vec<(&Object, String)> = vec![(&o, String::new())];
        while let Some((v, p)) = stack.pop() {
            budget += 1;
            if budget > limits.max_audit_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_audit_elements",
                    max: limits.max_audit_elements,
                });
            }
            match v {
                Object::Ref(x) => {
                    steps.push((at, p));
                    queue.push_back((*x, steps.len() - 1));
                }
                Object::Array(a) => {
                    for (i, v) in a.iter().enumerate() {
                        stack.push((v, format!("{p}[{i}]")));
                    }
                }
                Object::Dict(d) | Object::Stream { dict: d, .. } => {
                    for (k, v) in d {
                        if catalog && p.is_empty() && k.as_slice() == b"Metadata" {
                            continue;
                        }
                        stack.push((v, format!("{p}/{}", String::from_utf8_lossy(k))));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(None)
}

/// The fields that keep some widget while losing others: each must still
/// show a widget (on a page's `/Annots`, not Hidden or NoView), and its
/// value must be one a kept widget shows — a button group whose on-state
/// was a removed widget's is refused; a choice's array and a stream value
/// are reported in `elsewhere` like a string; any other value is refused.
#[allow(clippy::too_many_arguments)]
fn check_kept_fields(
    doc: &Document,
    overlay: &Overlay,
    removed: &HashSet<IndirectRef>,
    gone: &HashSet<IndirectRef>,
    parent: &HashMap<IndirectRef, Option<IndirectRef>>,
    kids: &HashMap<IndirectRef, Vec<IndirectRef>>,
    dicts: &HashMap<IndirectRef, Dict>,
    out: &mut FieldsEdit,
) -> Result<(), Error> {
    // The kept fields nearest each removed widget.
    let mut affected: HashSet<IndirectRef> = HashSet::new();
    for w in removed.iter().filter(|w| parent.contains_key(w)) {
        let mut cur = parent[w];
        while let Some(c) = cur {
            if !gone.contains(&c) {
                affected.insert(c);
                break;
            }
            cur = parent[&c];
        }
    }
    if affected.is_empty() {
        return Ok(());
    }
    // Widgets a reader shows: on a page's /Annots, not Hidden or NoView.
    let mut on_pages: HashSet<IndirectRef> = HashSet::new();
    for page in doc.pages() {
        let Some(pr) = page.object_ref() else {
            continue;
        };
        let Object::Dict(pd) = current(doc, overlay, pr)? else {
            continue;
        };
        if let Some(a) = pd.get(b"Annots".as_slice()) {
            for x in deref_cur(doc, overlay, a)?.as_array().unwrap_or(&[]) {
                if let Object::Ref(r) = x {
                    on_pages.insert(*r);
                }
            }
        }
    }
    let shown = |r: IndirectRef| -> Result<bool, Error> {
        if !on_pages.contains(&r) {
            return Ok(false);
        }
        // `/F` an integer without Hidden or NoView (a real is not
        // allowed, and readers read it differently), and no `/OC`.
        let d = &dicts[&r];
        let flags = match d
            .get(b"F".as_slice())
            .map(|f| deref_cur(doc, overlay, f))
            .transpose()?
        {
            None => 0,
            Some(Object::Integer(i)) => i,
            Some(_) => return Ok(false),
        };
        Ok(flags & (2 | 32) == 0 && !d.contains_key(b"OC".as_slice()))
    };
    // The value of `key` in effect at `r`, and the field holding it.
    let inherited_at = |r: IndirectRef, key: &[u8]| -> Option<(IndirectRef, Object)> {
        let mut cur = Some(r);
        let mut n = 0;
        while let Some(c) = cur {
            if let Some(v) = dicts[&c].get(key) {
                return Some((c, v.clone()));
            }
            cur = parent[&c];
            n += 1;
            if n > 64 {
                return None;
            }
        }
        None
    };
    let inherited = |r: IndirectRef, key: &[u8]| inherited_at(r, key).map(|(_, v)| v);
    // The widgets left under field `h` (leaves of its subtree not gone).
    let leaves_left = |h: IndirectRef| -> Vec<IndirectRef> {
        let mut out = Vec::new();
        let mut stack = vec![h];
        while let Some(c) = stack.pop() {
            if kids[&c].is_empty() {
                if !gone.contains(&c) {
                    out.push(c);
                }
            } else {
                stack.extend(kids[&c].iter().copied());
            }
        }
        out
    };
    let mut affected: Vec<IndirectRef> = affected.into_iter().collect();
    affected.sort();
    for f in affected {
        // Its widgets left and removed (leaves of its subtree).
        let (mut left, mut lost) = (Vec::new(), Vec::new());
        let mut stack = vec![f];
        while let Some(c) = stack.pop() {
            if kids[&c].is_empty() {
                if gone.contains(&c) {
                    lost.push(c);
                } else {
                    left.push(c);
                }
            } else {
                stack.extend(kids[&c].iter().copied());
            }
        }
        let mut visible = false;
        for w in &left {
            visible |= shown(*w)?;
        }
        if !visible {
            return Err(refuse(format!(
                "field {f} keeps only widgets no page shows (hidden, or on no page's /Annots)"
            )));
        }
        for key in [b"V".as_slice(), b"DV", b"RV"] {
            let Some((holder, v)) = inherited_at(f, key) else {
                continue;
            };
            match deref_cur(doc, overlay, &v)? {
                Object::String(_) | Object::Null => {}
                Object::Array(a) => {
                    for x in a {
                        if let Object::String(t) = deref_cur(doc, overlay, &x)? {
                            out.removed_text.push(crate::annot_text::text_string(&t));
                        }
                    }
                }
                Object::Stream { dict, data } => out.removed_text.push(
                    crate::annot_text::text_string(&doc.decode_stream(&dict, &data)?),
                ),
                // A button's state: a widget the output shows must show it
                // (`/V`: in that state now, its `/AS`; `/DV`: having it in
                // its normal appearance). Whatever a removed or unshown
                // widget had excuses nothing; undecidable is refused.
                Object::Name(n) if n.as_slice() != b"Off" => {
                    let mut shows = false;
                    for w in leaves_left(holder) {
                        if !shown(w)? {
                            continue;
                        }
                        let d = &dicts[&w];
                        let state = match d.get(b"AS".as_slice()) {
                            Some(a) => deref_cur(doc, overlay, a)?.as_name().map(|x| x.to_vec()),
                            None => None,
                        };
                        let normal = match d.get(b"AP".as_slice()) {
                            Some(ap) => match deref_cur(doc, overlay, ap)?
                                .as_dict()
                                .and_then(|a| a.get(b"N".as_slice()).cloned())
                            {
                                // The state's appearance: a stream (anything
                                // else draws nothing).
                                Some(nm) => match deref_cur(doc, overlay, &nm)?
                                    .as_dict()
                                    .and_then(|a| a.get(n.as_slice()).cloned())
                                {
                                    Some(st) => matches!(
                                        deref_cur(doc, overlay, &st)?,
                                        Object::Stream { .. }
                                    ),
                                    None => false,
                                },
                                None => false,
                            },
                            None => false,
                        };
                        shows |= if key == b"V" {
                            state.as_deref() == Some(n.as_slice()) && normal
                        } else {
                            normal
                        };
                    }
                    if !shows {
                        return Err(refuse(format!(
                            "field {holder}'s /{} /{} is a state no widget the output shows has",
                            String::from_utf8_lossy(key),
                            String::from_utf8_lossy(&n)
                        )));
                    }
                }
                Object::Name(_) => {}
                _ => {
                    return Err(refuse(format!(
                        "field {f} keeps a value that is neither a string, a name nor an array"
                    )))
                }
            }
        }
        // Choice options a removed widget may have shown.
        for key in [b"Opt".as_slice()] {
            if let Some(v) = inherited(f, key) {
                for x in deref_cur(doc, overlay, &v)?.as_array().unwrap_or(&[]) {
                    let x = deref_cur(doc, overlay, x)?;
                    let items: Vec<Object> = match x {
                        Object::Array(a) => a,
                        o => vec![o],
                    };
                    for i in items {
                        if let Object::String(t) = deref_cur(doc, overlay, &i)? {
                            out.removed_text.push(crate::annot_text::text_string(&t));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// One hop of the edited document: the body of `r` as written (a
/// reference chain's links are objects of their own, as the writer
/// writes them).
fn current_hop(doc: &Document, overlay: &Overlay, r: IndirectRef) -> Result<Object, Error> {
    match overlay.get(&(r.number, r.generation)) {
        Some(o) => Ok(o.clone()),
        None => doc.resolve_one_hop(r),
    }
}

/// Keys of an annotation that lead to other annotations, its page or its
/// field: what they refer to is not the annotation's own data.
const LINKS_OUT: &[&[u8]] = &[b"P", b"Parent", b"Popup", b"IRT"];

/// The keys of a kept annotation whose values a reader uses (ISO 32000-2
/// §12.5.2, §12.5.6): the common and markup keys and those of the
/// subtypes; `/A`, `/AA`, `/Dest` and `/PA` only on the subtypes that act
/// (links, widgets, screens).
fn annotation_key_used(subtype: &[u8], key: &[u8]) -> bool {
    const COMMON: &[&[u8]] = &[
        b"Type",
        b"Subtype",
        b"Rect",
        b"Contents",
        b"NM",
        b"M",
        b"F",
        b"AP",
        b"AS",
        b"Border",
        b"C",
        b"StructParent",
        b"OC",
        b"AF",
        b"ca",
        b"CA",
        b"BM",
        b"Lang",
        b"T",
        b"RC",
        b"CreationDate",
        b"Subj",
        b"RT",
        b"IT",
        b"ExData",
        b"BS",
        b"BE",
        b"L",
        b"LE",
        b"IC",
        b"LL",
        b"LLE",
        b"Cap",
        b"LLO",
        b"CP",
        b"Measure",
        b"CO",
        b"QuadPoints",
        b"InkList",
        b"Vertices",
        b"Path",
        b"DA",
        b"Q",
        b"DS",
        b"RD",
        b"Name",
        b"FS",
        b"Sound",
        b"Open",
        b"State",
        b"StateModel",
        b"MK",
        b"H",
        b"FT",
        b"V",
        b"DV",
        b"Opt",
        b"TU",
        b"TM",
        b"Ff",
        b"MaxLen",
        b"I",
        b"TI",
        b"RV",
    ];
    if COMMON.contains(&key) {
        return true;
    }
    matches!(key, b"A" | b"AA" | b"Dest" | b"PA")
        && matches!(subtype, b"Link" | b"Widget" | b"Screen")
}

/// The first object [`removed_data_kept`] finds, with its path, and the
/// whole set it found.
pub(crate) type HeldData = (Option<(IndirectRef, String)>, HashSet<IndirectRef>);

/// What a removed annotation or form field holds that the edited
/// document still writes but nothing that uses it keeps: every object
/// reachable from the removed dictionaries (its text, actions and their
/// targets, appearance resources, rich text, attachments …; not its page,
/// field, popup or reply links, not pages, not other removed objects)
/// that is reached only through entries no reader uses — a private
/// entry, an annotation's key its subtype does not define, the outline
/// root's own keys, an action stored where a destination belongs. What a
/// kept page draws, a kept annotation or form field uses, an outline item
/// or a destination names, the catalog acts on or a name tree holds is
/// theirs too and stays. The first such object, with the path it is
/// reached by, and the full set (to check the rewrite's output too).
pub(crate) fn removed_data_kept(
    doc: &Document,
    overlay: &Overlay,
    removed: &HashSet<IndirectRef>,
    limits: &Limits,
) -> Result<HeldData, Error> {
    if removed.is_empty() {
        return Ok((None, HashSet::new()));
    }
    let pages: HashSet<IndirectRef> = doc.pages().iter().filter_map(|p| p.object_ref()).collect();
    let mut budget: u64 = 0;
    let mut tick = |n: u64| -> Result<(), Error> {
        budget += n;
        if budget > limits.max_audit_elements.saturating_mul(2) {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: limits.max_audit_elements,
            });
        }
        Ok(())
    };
    // The closure of `start` (values, not entered when pages or removed
    // objects), one hop at a time, in the input or the edited document.
    // Dictionary keys in `skip` are not followed at the top of a removed
    // object.
    fn closure(
        start: Vec<Object>,
        body: &dyn Fn(IndirectRef) -> Result<Object, Error>,
        stop: &dyn Fn(IndirectRef) -> bool,
        tick: &mut dyn FnMut(u64) -> Result<(), Error>,
    ) -> Result<HashSet<IndirectRef>, Error> {
        let mut set = HashSet::new();
        let mut stack = start;
        while let Some(o) = stack.pop() {
            tick(1)?;
            match o {
                Object::Ref(r) => {
                    if stop(r) || !set.insert(r) {
                        continue;
                    }
                    match body(r) {
                        Ok(o) => stack.push(o),
                        Err(Error::MissingReference(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                Object::Array(a) => stack.extend(a),
                Object::Dict(d) | Object::Stream { dict: d, .. } => stack.extend(d.into_values()),
                _ => {}
            }
        }
        Ok(set)
    }
    let input = |r: IndirectRef| doc.resolve_one_hop(r);
    let edited = |r: IndirectRef| current_hop(doc, overlay, r);
    // What the removed objects hold (as the input had them).
    let mut start = Vec::new();
    for r in removed {
        if let Object::Dict(d) = doc.resolve(*r)? {
            for (k, v) in d {
                // Its popup is its own (a listed one is removed with it).
                if k.as_slice() == b"Popup" || !LINKS_OUT.contains(&k.as_slice()) {
                    start.push(v);
                }
            }
        }
    }
    let stop_theirs = |r: IndirectRef| pages.contains(&r) || removed.contains(&r);
    let theirs = closure(start, &input, &stop_theirs, &mut tick)?;
    if theirs.is_empty() {
        return Ok((None, HashSet::new()));
    }
    // What uses objects in the edited document: values to close over.
    let mut used: Vec<Object> = Vec::new();
    // Objects used themselves, not what they refer to (an /Annots array,
    // a name tree node, a destination array by reference).
    let mut marks: Vec<IndirectRef> = Vec::new();
    let catalog = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => current(doc, overlay, *r)?,
        _ => Object::Null,
    };
    let catalog = catalog.as_dict().cloned().unwrap_or_default();
    // A destination: an array (its page is a page; nothing else in it is
    // read) or a dictionary's `/D`.
    let dest =
        |o: &Object, used: &mut Vec<Object>, marks: &mut Vec<IndirectRef>| -> Result<(), Error> {
            match deref_cur(doc, overlay, o)? {
                // A destination dictionary (`/D`); an action is not one.
                Object::Dict(d) if !d.contains_key(b"S".as_slice()) => {
                    if let Some(v) = d.get(b"D".as_slice()) {
                        used.push(v.clone());
                    }
                }
                Object::Array(_) | Object::String(_) | Object::Name(_) => {}
                _ => return Ok(()),
            }
            // The destination object itself (an array by reference) is used.
            if let Object::Ref(r) = o {
                marks.push(*r);
            }
            Ok(())
        };
    // An action (closed over whole, `/Next` included); a destination array
    // where an action belongs (`/OpenAction`) is a destination.
    let action =
        |o: &Object, used: &mut Vec<Object>, marks: &mut Vec<IndirectRef>| -> Result<(), Error> {
            match deref_cur(doc, overlay, o)? {
                Object::Dict(_) => used.push(o.clone()),
                _ => dest(o, used, marks)?,
            }
            Ok(())
        };
    // `/AA` (by reference or direct): its trigger values are actions.
    let additional =
        |o: &Object, used: &mut Vec<Object>, marks: &mut Vec<IndirectRef>| -> Result<(), Error> {
            // A trigger dictionary (an action where one belongs is not).
            if let Object::Dict(aa) = deref_cur(doc, overlay, o)? {
                if aa.contains_key(b"S".as_slice()) {
                    return Ok(());
                }
                if let Object::Ref(r) = o {
                    marks.push(*r);
                }
                for v in aa.values() {
                    action(v, used, marks)?;
                }
            }
            Ok(())
        };
    if let Some(v) = catalog.get(b"OpenAction".as_slice()) {
        action(v, &mut used, &mut marks)?;
    }
    if let Some(v) = catalog.get(b"AA".as_slice()) {
        additional(v, &mut used, &mut marks)?;
    }
    for k in [
        b"OCProperties".as_slice(),
        b"AF",
        b"URI",
        b"Lang",
        b"ViewerPreferences",
    ] {
        if let Some(v) = catalog.get(k) {
            used.push(v.clone());
        }
    }
    // Pages: what they draw and act on, and their annotations.
    let mut kept_annots: Vec<IndirectRef> = Vec::new();
    for page in doc.pages() {
        tick(1)?;
        let Some(pr) = page.object_ref() else {
            continue;
        };
        let Object::Dict(pd) = current(doc, overlay, pr)? else {
            continue;
        };
        for k in [
            b"Contents".as_slice(),
            b"Group",
            b"Thumb",
            b"B",
            b"Metadata",
        ] {
            if let Some(v) = pd.get(k) {
                used.push(v.clone());
            }
        }
        if let Some(v) = page.resources() {
            used.push(v.clone());
        }
        if let Some(v) = pd.get(b"AA".as_slice()) {
            additional(v, &mut used, &mut marks)?;
        }
        if let Some(a) = pd.get(b"Annots".as_slice()) {
            if let Object::Ref(ar) = a {
                marks.push(*ar);
            }
            for x in deref_cur(doc, overlay, a)?.as_array().unwrap_or(&[]) {
                tick(1)?;
                let Some(xr) = x.as_ref() else {
                    // A direct annotation: its values.
                    if let Object::Dict(d) = x {
                        kept_annotation(d, &mut used, &mut marks, &action, &dest, &additional)?;
                    }
                    continue;
                };
                if removed.contains(&xr) {
                    continue;
                }
                kept_annots.push(xr);
                marks.push(xr);
            }
        }
    }
    type Slot<'a> =
        &'a dyn Fn(&Object, &mut Vec<Object>, &mut Vec<IndirectRef>) -> Result<(), Error>;
    fn kept_annotation(
        d: &Dict,
        used: &mut Vec<Object>,
        marks: &mut Vec<IndirectRef>,
        action: Slot<'_>,
        dest: Slot<'_>,
        additional: Slot<'_>,
    ) -> Result<(), Error> {
        let st = d
            .get(b"Subtype".as_slice())
            .and_then(|s| s.as_name())
            .unwrap_or(b"");
        for (k, v) in d {
            let k = k.as_slice();
            if LINKS_OUT.contains(&k) || !annotation_key_used(st, k) {
                continue;
            }
            match k {
                b"A" | b"PA" => action(v, used, marks)?,
                b"Dest" => dest(v, used, marks)?,
                b"AA" => additional(v, used, marks)?,
                _ => used.push(v.clone()),
            }
        }
        Ok(())
    }
    for r in &kept_annots {
        if let Object::Dict(d) = current(doc, overlay, *r)? {
            kept_annotation(&d, &mut used, &mut marks, &action, &dest, &additional)?;
        }
    }
    // Outline items (below the root), form fields, name trees, /Dests.
    let mut stack: Vec<(Object, u8)> = Vec::new();
    if let Some(o) = catalog.get(b"Outlines".as_slice()) {
        if let Object::Dict(root) = deref_cur(doc, overlay, o)? {
            if let Some(f) = root.get(b"First".as_slice()) {
                stack.push((f.clone(), b'o'));
            }
        }
    }
    if let Some(f) = catalog.get(b"AcroForm".as_slice()) {
        if let Object::Dict(f) = deref_cur(doc, overlay, f)? {
            for (k, v) in &f {
                match k.as_slice() {
                    b"Fields" => stack.push((v.clone(), b'f')),
                    b"CO" | b"XFA" => {}
                    _ => used.push(v.clone()),
                }
            }
        }
    }
    if let Some(n) = catalog.get(b"Names".as_slice()) {
        if let Object::Dict(n) = deref_cur(doc, overlay, n)? {
            for (k, t) in &n {
                stack.push((
                    t.clone(),
                    if k.as_slice() == b"Dests" { b'd' } else { b'n' },
                ));
            }
        }
    }
    if let Some(d) = catalog.get(b"Dests".as_slice()) {
        if let Object::Dict(d) = deref_cur(doc, overlay, d)? {
            for v in d.values() {
                dest(v, &mut used, &mut marks)?;
            }
        }
    }
    let mut seen: HashSet<IndirectRef> = HashSet::new();
    while let Some((o, kind)) = stack.pop() {
        tick(1)?;
        if let Object::Ref(r) = &o {
            if !seen.insert(*r) || removed.contains(r) {
                continue;
            }
        }
        match deref_cur(doc, overlay, &o)? {
            Object::Array(a) if kind == b'f' => stack.extend(a.into_iter().map(|x| (x, kind))),
            Object::Dict(d) => match kind {
                b'o' => {
                    if let Some(v) = d.get(b"A".as_slice()) {
                        action(v, &mut used, &mut marks)?;
                    }
                    if let Some(v) = d.get(b"Dest".as_slice()) {
                        dest(v, &mut used, &mut marks)?;
                    }
                    for k in [b"First".as_slice(), b"Next"] {
                        if let Some(v) = d.get(k) {
                            stack.push((v.clone(), kind));
                        }
                    }
                }
                b'f' => {
                    for (k, v) in &d {
                        match k.as_slice() {
                            b"Kids" => stack.push((v.clone(), kind)),
                            b"Parent" | b"P" | b"Popup" | b"IRT" => {}
                            b"AA" => additional(v, &mut used, &mut marks)?,
                            b"A" => action(v, &mut used, &mut marks)?,
                            _ => used.push(v.clone()),
                        }
                    }
                }
                _ => {
                    // A name tree node: its values (destinations in
                    // /Dests), its kids.
                    if let Some(Object::Array(names)) = d
                        .get(b"Names".as_slice())
                        .map(|n| deref_cur(doc, overlay, n))
                        .transpose()?
                    {
                        for v in names.into_iter().skip(1).step_by(2) {
                            if kind == b'd' {
                                dest(&v, &mut used, &mut marks)?;
                            } else {
                                used.push(v);
                            }
                        }
                    }
                    if let Some(k) = d.get(b"Kids".as_slice()) {
                        if let Object::Ref(r) = k {
                            marks.push(*r);
                        }
                        if let Object::Array(kids) = deref_cur(doc, overlay, k)? {
                            stack.extend(kids.into_iter().map(|x| (x, kind)));
                        }
                    }
                    if let Object::Ref(r) = &o {
                        marks.push(*r);
                    }
                }
            },
            _ => {}
        }
    }
    let stop_used = |r: IndirectRef| pages.contains(&r) || removed.contains(&r);
    let mut kept = closure(used, &edited, &stop_used, &mut tick)?;
    kept.extend(marks);
    let left: HashSet<IndirectRef> = theirs.difference(&kept).copied().collect();
    let first = removed_still_reached(doc, overlay, &left, limits)?;
    Ok((first, left))
}
