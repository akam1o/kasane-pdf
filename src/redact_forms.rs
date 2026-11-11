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

/// How a reader uses a value of the edited document: what of it is
/// followed. `Any` follows every entry (content, resources below their
/// categories, metadata); the others only the entries ISO 32000-2
/// defines for what the value is.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Use {
    Any,
    /// The object itself (a reference chain to it), nothing it refers to.
    Mark,
    /// A dictionary of which only these keys are read (each whole).
    Keys(&'static [&'static [u8]]),
    /// The same, for a dictionary that has no other keys (a settings
    /// dictionary): one with others is not what the slot holds.
    Only(&'static [&'static [u8]]),
    /// A page's `/B`: thread beads.
    Beads,
    /// A navigation node (`/PresSteps`).
    NavNode,
    /// Numbers (and arrays of them): `/Border`.
    Numbers,
    /// `/Threads`: thread dictionaries.
    Threads,
    /// A metadata stream; a thumbnail image.
    Metadata,
    Thumb,
    /// The structure tree root, a structure element, and `/K` (kids:
    /// elements, marked-content and object references, integers).
    StructRoot,
    StructElem,
    StructKids,
    /// An array of objects used themselves.
    MarkList,
    Action,
    /// An action's `/Next`: an action or an array of them.
    Next,
    Dest,
    /// An additional-actions dictionary, with the triggers of its owner.
    Aa(Triggers),
    Annot,
    /// A markup annotation's `/Popup`: a popup annotation.
    Popup,
    AnnotList,
    Field,
    FieldList,
    Resources,
    /// A resource category: its named entries.
    Category,
    AcroForm,
    OutlineRoot,
    Outline,
    NamesDict,
    DestsDict,
    NameNode(NameUse),
    NameArray(NameUse),
    NameKids(NameUse),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Triggers {
    Annot,
    Widget,
    Field,
    Page,
    Catalog,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NameUse {
    Dest,
    Action,
    /// A page label dictionary (in the `/PageLabels` number tree).
    Label,
    /// Structure elements (`/IDTree`, `/ParentTree`).
    Struct,
    Any,
}

/// Whether `key` names a trigger event of an `/AA` dictionary owned by
/// `t` (ISO 32000-2 §12.6.3).
fn trigger(t: Triggers, key: &[u8]) -> bool {
    const ANNOT: &[&[u8]] = &[
        b"E", b"X", b"D", b"U", b"Fo", b"Bl", b"PO", b"PC", b"PV", b"PI",
    ];
    const FIELD: &[&[u8]] = &[b"K", b"F", b"V", b"C"];
    match t {
        Triggers::Annot => ANNOT.contains(&key),
        Triggers::Widget => ANNOT.contains(&key) || FIELD.contains(&key),
        Triggers::Field => FIELD.contains(&key),
        Triggers::Page => matches!(key, b"O" | b"C"),
        Triggers::Catalog => matches!(key, b"WC" | b"WS" | b"DS" | b"WP" | b"DP"),
    }
}

/// How a reader uses the value of `key` in a kept annotation of
/// `subtype` (ISO 32000-2 §12.5.2, §12.5.6): the common keys, the markup
/// keys on markup annotations, each subtype's own; `None` for a key the
/// subtype does not define, or one leading to the page.
fn annotation_use(subtype: &[u8], key: &[u8]) -> Option<Use> {
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
    ];
    const MARKUP: &[&[u8]] = &[
        b"T",
        b"RC",
        b"CreationDate",
        b"Subj",
        b"RT",
        b"IT",
        b"ExData",
    ];
    let markup = matches!(
        subtype,
        b"Text"
            | b"FreeText"
            | b"Line"
            | b"Square"
            | b"Circle"
            | b"Polygon"
            | b"PolyLine"
            | b"Highlight"
            | b"Underline"
            | b"Squiggly"
            | b"StrikeOut"
            | b"Caret"
            | b"Stamp"
            | b"Ink"
            | b"FileAttachment"
            | b"Sound"
            | b"Redact"
            | b"Projection"
    );
    let own: &[&[u8]] = match subtype {
        b"Text" => &[b"Open", b"Name", b"State", b"StateModel"],
        b"Link" => &[b"H", b"QuadPoints", b"BS"],
        b"FreeText" => &[b"DA", b"Q", b"DS", b"CL", b"BE", b"RD", b"BS", b"LE"],
        b"Line" => &[
            b"L", b"BS", b"LE", b"IC", b"LL", b"LLE", b"Cap", b"LLO", b"CP", b"Measure", b"CO",
        ],
        b"Square" | b"Circle" => &[b"BS", b"IC", b"BE", b"RD"],
        b"Polygon" | b"PolyLine" => &[b"Vertices", b"LE", b"BS", b"IC", b"BE", b"Measure", b"Path"],
        b"Highlight" | b"Underline" | b"Squiggly" | b"StrikeOut" => &[b"QuadPoints"],
        b"Caret" => &[b"RD", b"Sy"],
        b"Stamp" => &[b"Name"],
        b"Ink" => &[b"InkList", b"BS", b"Path"],
        b"Popup" => &[b"Open"],
        b"FileAttachment" => &[b"FS", b"Name"],
        b"Sound" => &[b"Sound", b"Name"],
        b"Movie" => &[b"T", b"Movie"],
        b"Screen" => &[b"T", b"MK"],
        // A merged field's keys: by the field (`Use::Field`).
        b"Widget" => &[b"H", b"MK", b"BS"],
        b"PrinterMark" => &[b"MN"],
        b"TrapNet" => &[b"LastModified", b"Version", b"AnnotStates", b"FontFauxing"],
        b"Watermark" => &[b"FixedPrint"],
        b"Redact" => &[
            b"QuadPoints",
            b"IC",
            b"RO",
            b"OverlayText",
            b"Repeat",
            b"DA",
            b"Q",
        ],
        b"3D" => &[b"3DD", b"3DV", b"3DA", b"3DI", b"3DB", b"3DU", b"GEO"],
        b"RichMedia" => &[b"RichMediaContent", b"RichMediaSettings", b"3DU", b"GEO"],
        _ => &[],
    };
    let acts = matches!(subtype, b"Link" | b"Widget" | b"Screen");
    Some(match key {
        b"P" => return None,
        b"Border" => Use::Numbers,
        b"Popup" if markup => Use::Popup,
        b"IRT" if markup => Use::Mark,
        b"Parent" if subtype == b"Widget" => Use::Field,
        b"Parent" if subtype == b"Popup" => Use::Mark,
        b"Kids" if subtype == b"Widget" => Use::FieldList,
        b"A" if acts => Use::Action,
        // A movie's activation dictionary, not an action.
        b"A" if subtype == b"Movie" => Use::Any,
        b"PA" if subtype == b"Link" => Use::Action,
        b"Dest" if subtype == b"Link" => Use::Dest,
        b"AA" if acts => Use::Aa(if subtype == b"Widget" {
            Triggers::Widget
        } else {
            Triggers::Annot
        }),
        k if COMMON.contains(&k) || (markup && MARKUP.contains(&k)) || own.contains(&k) => Use::Any,
        _ => return None,
    })
}

/// Whether `o` is what a slot of use `u` holds: an action is a
/// dictionary with `/S`, a destination an array or a dictionary without
/// one, and so on.
fn holds(u: Use, o: &Object) -> bool {
    let action = |o: &Object| matches!(o, Object::Dict(d) if d.get(b"S".as_slice()).and_then(|s| s.as_name()).is_some_and(|s| ACTION_TYPES.contains(&s)));
    match u {
        Use::Any | Use::Mark | Use::MarkList => true,
        Use::Action => action(o),
        Use::Next => action(o) || matches!(o, Object::Array(_)),
        Use::Dest => {
            matches!(o, Object::String(_) | Object::Name(_))
                || matches!(o, Object::Array(a)
                    if matches!(a.first(), Some(Object::Ref(_) | Object::Integer(_)))
                        && matches!(a.get(1), Some(Object::Name(_))))
                || matches!(o, Object::Dict(d) if !action(o) && d.contains_key(b"D".as_slice()))
        }
        Use::Only(keys) => {
            matches!(o, Object::Dict(d) if d.keys().all(|k| keys.contains(&k.as_slice())))
        }
        Use::Outline => matches!(o, Object::Dict(d) if d.contains_key(b"Title".as_slice())),
        Use::Beads => matches!(o, Object::Array(_)),
        Use::StructKids => matches!(o, Object::Array(_) | Object::Dict(_)),
        Use::StructRoot | Use::StructElem | Use::NavNode => matches!(o, Object::Dict(_)),
        Use::Numbers => {
            matches!(o, Object::Integer(_) | Object::Real(_) | Object::Array(_))
        }
        Use::Threads => matches!(o, Object::Array(_)),
        Use::Metadata => matches!(o, Object::Stream { dict, .. }
            if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"XML")),
        Use::Thumb => {
            matches!(o, Object::Stream { dict, .. } if dict.contains_key(b"Width".as_slice()))
        }
        Use::Aa(_) => matches!(o, Object::Dict(_)) && !action(o),
        Use::AnnotList | Use::FieldList | Use::NameArray(_) | Use::NameKids(_) => {
            matches!(o, Object::Array(_))
        }
        Use::Popup => {
            matches!(o, Object::Dict(d) if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Popup"))
        }
        Use::Annot
        | Use::Keys(_)
        | Use::Field
        | Use::Resources
        | Use::Category
        | Use::AcroForm
        | Use::OutlineRoot
        | Use::NamesDict
        | Use::DestsDict
        | Use::NameNode(_) => matches!(o, Object::Dict(_)),
    }
}

/// The slot through which an object of a removed annotation or field is
/// reached from it, and so what a use of it must be.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Held {
    /// Its own data (text, a link's target, a file, a value): kept by a
    /// slot defined for it, or inside data kept that way.
    Own,
    /// Its appearance and what that draws: kept by any use.
    Ap,
    /// An action: kept only by an action slot.
    Action,
    /// An `/AA` dictionary: kept only as one.
    Aa,
    /// A `/Next` array or action: kept only as one.
    Next,
    /// An annotation (its popup, or itself): kept only as an annotation.
    Annot,
    /// What an action names (field names, the annotations it hides):
    /// the names (strings, arrays of them) are its own data, the
    /// fields and annotations they name are not.
    Names,
}

/// The action types (ISO 32000-2 §12.6.4.1).
const ACTION_TYPES: &[&[u8]] = &[
    b"GoTo",
    b"GoToR",
    b"GoToE",
    b"GoToDp",
    b"Launch",
    b"Thread",
    b"URI",
    b"Sound",
    b"Movie",
    b"Hide",
    b"Named",
    b"SubmitForm",
    b"ResetForm",
    b"ImportData",
    b"SetOCGState",
    b"Rendition",
    b"Trans",
    b"GoTo3DView",
    b"JavaScript",
    b"RichMediaExecute",
];

/// How a reader uses the value of `key` in a kept action whose type is
/// `s` (ISO 32000-2 §12.6.4); `None` for a key that type does not define.
fn action_use(s: &[u8], key: &[u8]) -> Option<Use> {
    if key == b"Next" {
        return Some(Use::Next);
    }
    if matches!(key, b"Type" | b"S") {
        return Some(Use::Any);
    }
    let keys: &[&[u8]] = match s {
        b"GoTo" => &[b"D", b"SD"],
        b"GoToR" => &[b"F", b"D", b"SD", b"NewWindow"],
        b"GoToE" => &[b"F", b"D", b"NewWindow", b"T"],
        b"GoToDp" => &[b"Dp"],
        b"Launch" => &[b"F", b"Win", b"Mac", b"Unix", b"NewWindow"],
        b"Thread" => &[b"F", b"D", b"B"],
        b"URI" => &[b"URI", b"IsMap"],
        b"Sound" => &[b"Sound", b"Volume", b"Synchronous", b"Repeat", b"Mix"],
        b"Movie" => &[b"Annotation", b"T", b"Operation"],
        b"Hide" => &[b"T", b"H"],
        b"Named" => &[b"N"],
        b"SubmitForm" => &[b"F", b"Fields", b"Flags", b"CharSet"],
        b"ResetForm" => &[b"Fields", b"Flags"],
        b"ImportData" => &[b"F"],
        b"SetOCGState" => &[b"State", b"PreserveRB"],
        b"Rendition" => &[b"R", b"AN", b"OP", b"JS"],
        b"Trans" => &[b"Trans"],
        b"GoTo3DView" => &[b"TA", b"V"],
        b"JavaScript" => &[b"JS"],
        b"RichMediaExecute" => &[b"TA", b"TI", b"CMD"],
        _ => &[],
    };
    if !keys.contains(&key) {
        return None;
    }
    Some(match (s, key) {
        (b"GoTo" | b"GoToR" | b"GoToE", b"D") => Use::Dest,
        (b"Hide", b"T") | (_, b"Fields") => Use::MarkList,
        (_, b"Annotation" | b"AN" | b"TA") => Use::Mark,
        _ => Use::Any,
    })
}

/// Whether a form field of type `ft` (inherited; empty when unknown)
/// defines `key` (ISO 32000-2 §12.7.4).
fn field_key(ft: &[u8], key: &[u8]) -> bool {
    matches!(
        key,
        b"FT" | b"T" | b"TU" | b"TM" | b"Ff" | b"V" | b"DV" | b"DA" | b"Q" | b"DS" | b"RV"
    ) || match ft {
        b"Btn" => key == b"Opt",
        b"Tx" => key == b"MaxLen",
        b"Ch" => matches!(key, b"Opt" | b"TI" | b"I"),
        b"Sig" => matches!(key, b"Lock" | b"SV"),
        _ => false,
    }
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
/// entry, an annotation's key its subtype does not define, an `/AA`
/// entry that names no trigger, the outline root's own keys, an action
/// stored where a destination belongs. The use is walked by type ([`Use`])
/// from the catalog and the pages; an action, annotation or field of
/// theirs counts only where it is used as one. The first such object,
/// with the path it is reached by, and the full set (to check the
/// rewrite's output too).
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
    let input = |r: IndirectRef| doc.resolve_one_hop(r);
    let edited = |r: IndirectRef| current_hop(doc, overlay, r);
    // What the removed objects hold (as the input had them), each object
    // with the slot it is reached through: an action, an `/AA`
    // dictionary, a `/Next` array, a popup, the appearance, or its own
    // data. What an action only names (the fields it resets, the
    // annotation it hides, a structure destination) is not followed.
    // Each object with every class it is held as.
    let mut theirs: std::collections::HashMap<IndirectRef, Vec<Held>> = Default::default();
    let mut hwork: Vec<(Object, Held)> = Vec::new();
    for r in removed {
        if let Object::Dict(d) = doc.resolve(*r)? {
            hwork.push((Object::Dict(d), Held::Annot));
        }
    }
    let stop_theirs = |r: IndirectRef| pages.contains(&r) || removed.contains(&r);
    let mut hwalked: HashSet<(IndirectRef, Held)> = HashSet::new();
    while let Some((o, h)) = hwork.pop() {
        tick(1)?;
        let d = match o {
            Object::Ref(r) => {
                if stop_theirs(r) {
                    continue;
                }
                let body = match input(r) {
                    Ok(b) => b,
                    Err(Error::MissingReference(_)) => continue,
                    Err(e) => return Err(e),
                };
                // A field or annotation an action names is not its data.
                if h == Held::Names && matches!(body, Object::Dict(_) | Object::Stream { .. }) {
                    continue;
                }
                let class = if h == Held::Names { Held::Own } else { h };
                let classes = theirs.entry(r).or_default();
                if !classes.contains(&class) {
                    classes.push(class);
                }
                if hwalked.insert((r, h)) {
                    hwork.push((body, h));
                }
                continue;
            }
            Object::Array(a) => {
                let each = match h {
                    Held::Next => Held::Action,
                    Held::Action | Held::Aa | Held::Annot => Held::Own,
                    Held::Names => Held::Names,
                    h => h,
                };
                hwork.extend(a.into_iter().map(|v| (v, each)));
                continue;
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => d,
            _ => continue,
        };
        match h {
            Held::Own | Held::Ap => hwork.extend(d.into_values().map(|v| (v, h))),
            Held::Names => {}
            Held::Annot => {
                for (k, v) in d {
                    let h = match k.as_slice() {
                        b"P" | b"Parent" | b"IRT" => continue,
                        b"Popup" => Held::Annot,
                        b"A" | b"PA" => Held::Action,
                        b"AA" => Held::Aa,
                        // Its appearance, and what only presents it.
                        b"AP" | b"OC" | b"Measure" | b"BS" | b"BE" | b"MK" => Held::Ap,
                        _ => Held::Own,
                    };
                    hwork.push((v, h));
                }
            }
            Held::Aa => hwork.extend(d.into_values().map(|v| (v, Held::Action))),
            Held::Next => hwork.push((Object::Dict(d), Held::Action)),
            Held::Action => {
                let s = d
                    .get(b"S".as_slice())
                    .and_then(|s| s.as_name())
                    .unwrap_or(b"")
                    .to_vec();
                for (k, v) in d {
                    // What its type only names: the target, not its data.
                    let names = matches!(
                        (s.as_slice(), k.as_slice()),
                        (b"Hide", b"T")
                            | (b"SubmitForm" | b"ResetForm", b"Fields")
                            | (b"Movie", b"Annotation")
                            | (b"Rendition", b"AN")
                            | (b"GoTo3DView" | b"RichMediaExecute", b"TA")
                            | (b"GoTo" | b"GoToR", b"SD")
                    );
                    let h = if names {
                        Held::Names
                    } else if k.as_slice() == b"Next" {
                        Held::Next
                    } else {
                        Held::Own
                    };
                    hwork.push((v, h));
                }
            }
        }
    }
    if theirs.is_empty() {
        return Ok((None, HashSet::new()));
    }
    // What uses objects in the edited document, walked by what each
    // value is: from the catalog and the pages.
    // Each value with its use, whether it lies inside something read
    // whole (a dictionary's entries under `Use::Any`), and the object
    // that holds it.
    let mut work: Vec<(Object, Use, bool, Option<IndirectRef>)> = Vec::new();
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => Some(*r),
        _ => None,
    };
    let catalog = match root {
        Some(r) => current(doc, overlay, r)?,
        None => Object::Null,
    };
    if let Object::Dict(c) = &catalog {
        for (k, v) in c {
            let u = match k.as_slice() {
                // A destination array where an action belongs.
                b"OpenAction" => match deref_cur(doc, overlay, v)? {
                    Object::Array(_) => Use::Dest,
                    _ => Use::Action,
                },
                b"AA" => Use::Aa(Triggers::Catalog),
                b"Names" => Use::NamesDict,
                b"Dests" => Use::DestsDict,
                b"Outlines" => Use::OutlineRoot,
                b"AcroForm" => Use::AcroForm,
                b"ViewerPreferences" => Use::Only(&[
                    b"HideToolbar",
                    b"HideMenubar",
                    b"HideWindowUI",
                    b"FitWindow",
                    b"CenterWindow",
                    b"DisplayDocTitle",
                    b"NonFullScreenPageMode",
                    b"Direction",
                    b"ViewArea",
                    b"ViewClip",
                    b"PrintArea",
                    b"PrintClip",
                    b"PrintScaling",
                    b"Duplex",
                    b"PickTrayByPDFSize",
                    b"PrintPageRange",
                    b"NumCopies",
                    b"Enforce",
                ]),
                b"MarkInfo" => Use::Only(&[b"Marked", b"UserProperties", b"Suspects"]),
                b"URI" => Use::Only(&[b"Base"]),
                b"PageLabels" => Use::NameNode(NameUse::Label),
                b"OCProperties" => Use::Keys(&[b"OCGs", b"D", b"Configs"]),
                b"StructTreeRoot" => Use::StructRoot,
                b"Metadata" => Use::Metadata,
                b"Threads" => Use::Threads,
                b"Perms" => Use::Keys(&[b"DocMDP", b"UR3"]),
                // Values that are names, numbers or short strings: the
                // object only.
                b"Lang" | b"Version" | b"PageMode" | b"PageLayout" | b"NeedsRendering" => Use::Mark,
                b"AF" | b"OutputIntents" | b"Collection" | b"Legal" | b"Requirements"
                | b"Extensions" | b"DSS" | b"DPartRoot" => Use::Any,
                _ => continue,
            };
            work.push((v.clone(), u, false, root));
        }
    }
    let mut tree_nodes: HashSet<IndirectRef> = HashSet::new();
    for page in doc.pages() {
        tick(1)?;
        let Some(pr) = page.object_ref() else {
            continue;
        };
        let Object::Dict(pd) = current(doc, overlay, pr)? else {
            continue;
        };
        for (k, v) in &pd {
            let u = match k.as_slice() {
                b"Resources" => Use::Resources,
                b"AA" => Use::Aa(Triggers::Page),
                b"Annots" => Use::AnnotList,
                b"Group" => Use::Keys(&[b"Type", b"S", b"CS", b"I", b"K"]),
                b"B" => Use::Beads,
                b"PresSteps" => Use::NavNode,
                b"Thumb" => Use::Thumb,
                b"Metadata" => Use::Metadata,
                b"Trans" => Use::Keys(&[b"Type", b"S", b"D", b"Dm", b"M", b"Di", b"SS", b"B"]),
                b"Rotate" | b"UserUnit" | b"Dur" | b"LastModified" | b"Tabs" | b"StructParents" => {
                    Use::Mark
                }
                b"Contents"
                | b"MediaBox"
                | b"CropBox"
                | b"BleedBox"
                | b"TrimBox"
                | b"ArtBox"
                | b"BoxColorInfo"
                | b"VP"
                | b"TemplateInstantiated"
                | b"ID"
                | b"PZ"
                | b"SeparationInfo"
                | b"AF"
                | b"OutputIntents"
                | b"DPart" => Use::Any,
                _ => continue,
            };
            work.push((v.clone(), u, false, Some(pr)));
        }
        // Attributes the page inherits from its tree: each node once (a
        // node shared by many pages, and its ancestors, are read once).
        let mut up = pd.get(b"Parent".as_slice()).cloned();
        let mut depth = 0;
        while let Some(Object::Ref(nr)) = up {
            depth += 1;
            if depth > limits.max_page_tree_depth || pages.contains(&nr) || !tree_nodes.insert(nr) {
                break;
            }
            tick(1)?;
            let Object::Dict(nd) = current(doc, overlay, nr)? else {
                break;
            };
            for (k, u) in [
                (b"Resources".as_slice(), Use::Resources),
                (b"MediaBox", Use::Any),
                (b"CropBox", Use::Any),
                (b"Rotate", Use::Any),
            ] {
                if let Some(v) = nd.get(k) {
                    work.push((v.clone(), u, false, Some(nr)));
                }
            }
            up = nd.get(b"Parent".as_slice()).cloned();
        }
    }
    let stop_used = |r: IndirectRef| pages.contains(&r) || removed.contains(&r) || Some(r) == root;
    // Each object used: the use it is reached by, whether inside
    // something read whole, and what holds it.
    type Seen = (Use, bool, Option<IndirectRef>);
    let mut kept: std::collections::HashMap<IndirectRef, Vec<Seen>> = Default::default();
    let mut walked: HashSet<(IndirectRef, Use, bool)> = HashSet::new();
    // Each reference's chain, when its end fits the use.
    let mut chains: std::collections::HashMap<(IndirectRef, Use), Option<Vec<IndirectRef>>> =
        Default::default();
    while let Some((o, u, inside, holder)) = work.pop() {
        tick(1)?;
        // A value of a typed slot: its own use, not inside.
        let mut push = |v: Object, u: Use| work.push((v, u, false, holder));
        let d = match o {
            // A reference (chain): its links are used when what it ends
            // at is what the slot holds.
            Object::Ref(r) => {
                if stop_used(r) {
                    continue;
                }
                // Read once per use: a large object referred to from many
                // places is not parsed again for each.
                if let Some(known) = chains.get(&(r, u)) {
                    if let Some(links) = known {
                        for l in links {
                            kept.entry(*l).or_default().push((u, inside, holder));
                        }
                        if walked.contains(&(r, u, inside)) {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                let mut links = vec![r];
                let mut on_chain: HashSet<IndirectRef> = HashSet::from([r]);
                let body = loop {
                    tick(1)?;
                    match edited(*links.last().unwrap_or(&r)) {
                        Ok(Object::Ref(n)) => {
                            if stop_used(n) || !on_chain.insert(n) {
                                break None;
                            }
                            links.push(n);
                        }
                        Ok(b) => break Some(b),
                        Err(Error::MissingReference(_)) => break None,
                        Err(e) => return Err(e),
                    }
                };
                let fits = body.filter(|b| holds(u, b));
                let first = !chains.contains_key(&(r, u));
                chains.insert((r, u), fits.is_some().then(|| links.clone()));
                if let Some(b) = fits {
                    let at = links.last().copied();
                    if first {
                        for l in links {
                            kept.entry(l).or_default().push((u, inside, holder));
                        }
                    }
                    if walked.insert((r, u, inside)) {
                        work.push((b, u, inside, at));
                    }
                }
                continue;
            }
            Object::Array(a) => {
                let each = match u {
                    Use::Any => Use::Any,
                    Use::MarkList => Use::Mark,
                    Use::Beads => Use::Keys(&[b"Type", b"T", b"N", b"V", b"P", b"R"]),
                    Use::StructKids => Use::StructKids,
                    Use::Numbers => Use::Numbers,
                    Use::Threads => Use::Keys(&[b"Type", b"F", b"I", b"Metadata"]),
                    Use::Next => Use::Action,
                    Use::AnnotList => Use::Annot,
                    Use::FieldList => Use::Field,
                    Use::NameKids(n) => Use::NameNode(n),
                    Use::NameArray(n) => {
                        let value = match n {
                            NameUse::Dest => Use::Dest,
                            NameUse::Action => Use::Action,
                            NameUse::Label => Use::Keys(&[b"Type", b"S", b"P", b"St"]),
                            NameUse::Struct => Use::StructKids,
                            NameUse::Any => Use::Any,
                        };
                        for (i, v) in a.into_iter().enumerate() {
                            push(v, if i % 2 == 0 { Use::Mark } else { value });
                        }
                        continue;
                    }
                    // A destination: its page is a page; an operand given
                    // by reference is used when it is a number or null.
                    Use::Dest => {
                        for v in a {
                            let Object::Ref(mut at) = v else { continue };
                            let mut links = Vec::new();
                            let mut on_chain: HashSet<IndirectRef> = HashSet::new();
                            loop {
                                tick(1)?;
                                if pages.contains(&at) {
                                    break;
                                }
                                links.push(at);
                                on_chain.insert(at);
                                match edited(at) {
                                    Ok(Object::Ref(n)) if !on_chain.contains(&n) => at = n,
                                    Ok(Object::Integer(_) | Object::Real(_) | Object::Null) => {
                                        break
                                    }
                                    _ => {
                                        links.clear();
                                        break;
                                    }
                                }
                            }
                            for l in links {
                                kept.entry(l).or_default().push((Use::Dest, false, holder));
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                work.extend(a.into_iter().map(|v| (v, each, inside, holder)));
                continue;
            }
            Object::Dict(d) => d,
            Object::Stream { dict, .. } if u == Use::Any => dict,
            _ => continue,
        };
        let get_name = |k: &[u8]| d.get(k).and_then(|s| s.as_name()).unwrap_or(b"").to_vec();
        match u {
            Use::Any => work.extend(d.into_values().map(|v| (v, Use::Any, true, holder))),
            Use::Keys(keys) | Use::Only(keys) => {
                for (k, v) in d {
                    if keys.contains(&k.as_slice()) {
                        push(v, Use::Any);
                    }
                }
            }
            Use::Action => {
                let s = get_name(b"S");
                // An action of no known type does nothing (its /Next
                // neither).
                if !ACTION_TYPES.contains(&s.as_slice()) {
                    continue;
                }
                for (k, v) in d {
                    if let Some(u) = action_use(&s, &k) {
                        push(v, u);
                    }
                }
            }
            Use::Next => push(Object::Dict(d), Use::Action),
            // A destination dictionary (an action is not one).
            Use::Dest if !d.contains_key(b"S".as_slice()) => {
                if let Some(v) = d.get(b"D".as_slice()) {
                    push(v.clone(), Use::Dest);
                }
            }
            Use::Aa(t) if !d.contains_key(b"S".as_slice()) => {
                for (k, v) in d {
                    if trigger(t, &k) {
                        push(v, Use::Action);
                    }
                }
            }
            Use::Annot | Use::Popup => {
                let st = get_name(b"Subtype");
                // A widget merged with its field: the field's keys too.
                if st == b"Widget"
                    && (d.contains_key(b"FT".as_slice()) || d.contains_key(b"T".as_slice()))
                {
                    push(Object::Dict(d.clone()), Use::Field);
                }
                for (k, v) in d {
                    if let Some(u) = annotation_use(&st, &k) {
                        push(v, u);
                    }
                }
            }
            Use::Field => {
                // Its type, inherited from its ancestors.
                let mut ft = Vec::new();
                let mut at = Some(d.clone());
                for _ in 0..64 {
                    let Some(f) = at.take() else { break };
                    if let Some(t) = f.get(b"FT".as_slice()) {
                        ft = deref_cur(doc, overlay, t)?
                            .as_name()
                            .unwrap_or(b"")
                            .to_vec();
                        break;
                    }
                    if let Some(p) = f.get(b"Parent".as_slice()) {
                        tick(1)?;
                        at = deref_cur(doc, overlay, p)?.as_dict().cloned();
                    }
                }
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"Kids" => Use::FieldList,
                        b"Parent" => Use::Field,
                        b"AA" => Use::Aa(Triggers::Field),
                        k if field_key(&ft, k) => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            Use::Resources => {
                for (k, v) in d {
                    if matches!(
                        k.as_slice(),
                        b"ExtGState"
                            | b"ColorSpace"
                            | b"Pattern"
                            | b"Shading"
                            | b"XObject"
                            | b"Font"
                            | b"Properties"
                    ) {
                        push(v, Use::Category);
                    } else if k.as_slice() == b"ProcSet" {
                        push(v, Use::Any);
                    }
                }
            }
            Use::NavNode => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"NA" | b"PA" => Use::Action,
                        b"Next" | b"Prev" => Use::NavNode,
                        b"Type" | b"Dur" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            Use::StructRoot => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"K" => Use::StructKids,
                        b"IDTree" | b"ParentTree" => Use::NameNode(NameUse::Struct),
                        b"RoleMap"
                        | b"ClassMap"
                        | b"Namespaces"
                        | b"PronunciationLexicon"
                        | b"AF" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            // A kid: a marked-content or object reference, or an element.
            Use::StructKids => {
                let u = match d.get(b"Type".as_slice()).and_then(|t| t.as_name()) {
                    Some(b"MCR") => Use::Keys(&[b"Type", b"Pg", b"Stm", b"StmOwn", b"MCID"]),
                    Some(b"OBJR") => {
                        if let Some(v) = d.get(b"Obj".as_slice()) {
                            push(v.clone(), Use::Mark);
                        }
                        Use::Keys(&[b"Type", b"Pg"])
                    }
                    _ => Use::StructElem,
                };
                push(Object::Dict(d), u);
            }
            Use::StructElem => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"K" => Use::StructKids,
                        b"P" | b"Pg" => Use::Mark,
                        b"Ref" => Use::MarkList,
                        b"Type" | b"S" | b"ID" | b"A" | b"C" | b"R" | b"T" | b"Lang" | b"Alt"
                        | b"E" | b"ActualText" | b"AF" | b"NS" | b"PhoneticAlphabet"
                        | b"Phoneme" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            Use::Category => {
                for v in d.into_values() {
                    push(v, Use::Any);
                }
            }
            Use::AcroForm => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"Fields" => Use::FieldList,
                        b"CO" => Use::MarkList,
                        b"DR" | b"DA" | b"Q" | b"NeedAppearances" | b"SigFlags" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            Use::OutlineRoot => {
                if let Some(v) = d.get(b"First".as_slice()) {
                    push(v.clone(), Use::Outline);
                }
            }
            Use::Outline => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"A" => Use::Action,
                        b"Dest" => Use::Dest,
                        b"First" | b"Next" => Use::Outline,
                        b"Title" | b"SE" | b"C" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            Use::NamesDict => {
                for (k, v) in d {
                    let n = match k.as_slice() {
                        b"Dests" => NameUse::Dest,
                        b"JavaScript" => NameUse::Action,
                        b"AP"
                        | b"Pages"
                        | b"Templates"
                        | b"IDS"
                        | b"URLS"
                        | b"EmbeddedFiles"
                        | b"AlternatePresentations"
                        | b"Renditions" => NameUse::Any,
                        // A tree no reader looks up.
                        _ => continue,
                    };
                    push(v, Use::NameNode(n));
                }
            }
            Use::DestsDict => {
                for v in d.into_values() {
                    push(v, Use::Dest);
                }
            }
            Use::NameNode(n) => {
                for (k, v) in d {
                    let u = match k.as_slice() {
                        b"Names" | b"Nums" => Use::NameArray(n),
                        b"Kids" => Use::NameKids(n),
                        b"Limits" => Use::Any,
                        _ => continue,
                    };
                    push(v, u);
                }
            }
            _ => {}
        }
    }
    // What of theirs the edited document uses as what it is: an action
    // only by an action slot (an `/AA` dictionary, a `/Next` array or a
    // popup as one), its appearance by any use, its own data by a slot
    // defined for it — or inside data of theirs kept that way (a file
    // specification's embedded file), never inside something else read
    // whole (a private key of a font, a resource or a settings
    // dictionary).
    let mut excused: HashSet<IndirectRef> = HashSet::new();
    // Own data whose other classes are met but that is used only inside
    // something read whole: excused when that is excused.
    let mut held_in: std::collections::HashMap<IndirectRef, Vec<IndirectRef>> = Default::default();
    for (r, classes) in &theirs {
        tick(1)?;
        let Some(seen) = kept.get(r) else { continue };
        let met = |h: Held| {
            // A `/Next` that is one action (not an array) is an action.
            let one_action = h == Held::Next
                && matches!(doc.resolve(*r), Ok(Object::Dict(_) | Object::Stream { .. }));
            seen.iter().any(|(u, inside, _)| match h {
                Held::Action => matches!(u, Use::Action | Use::Next),
                Held::Next => *u == Use::Next || (one_action && *u == Use::Action),
                // Every trigger in it one of its user's.
                Held::Aa => match u {
                    Use::Aa(t) => match doc.resolve(*r) {
                        Ok(Object::Dict(d)) => d.keys().all(|k| trigger(*t, k)),
                        _ => false,
                    },
                    _ => false,
                },
                Held::Annot => matches!(u, Use::Annot | Use::Popup),
                Held::Ap => true,
                // As a value of an entry defined for one (not a slot that
                // only names an object, nor a structural one).
                Held::Own | Held::Names => {
                    !inside && matches!(u, Use::Any | Use::Dest | Use::Numbers)
                }
            })
        };
        if !classes.iter().filter(|h| **h != Held::Own).all(|h| met(*h)) {
            continue;
        }
        if !classes.contains(&Held::Own) || met(Held::Own) {
            excused.insert(*r);
            continue;
        }
        for (_, inside, by) in seen {
            if let (true, Some(b)) = (*inside, by) {
                tick(1)?;
                held_in.entry(*b).or_default().push(*r);
            }
        }
    }
    // (From a holder excused as own data: data inside the removed
    // annotation's appearance is not excused by the appearance.)
    let mut next: Vec<IndirectRef> = excused
        .iter()
        .filter(|r| theirs.get(*r).is_some_and(|c| c.contains(&Held::Own)))
        .copied()
        .collect();
    while let Some(b) = next.pop() {
        for r in held_in.remove(&b).unwrap_or_default() {
            tick(1)?;
            if excused.insert(r) {
                next.push(r);
            }
        }
    }
    let left: HashSet<IndirectRef> = theirs
        .keys()
        .filter(|r| !excused.contains(r))
        .copied()
        .collect();
    let first = removed_still_reached(doc, overlay, &left, limits)?;
    Ok((first, left))
}

/// Remove the AcroForm's `/XFA` (the XFA packets: the form's template,
/// scripts and a copy of its field values, `datasets`) from the edited
/// document, leaving the AcroForm every other reader uses. For
/// `remove_javascript` (XFA scripts are not edited) and when form fields
/// are edited (their values are in the datasets too). A form readers
/// must render from XFA alone (`/NeedsRendering true`) is refused. The
/// packets removed (to verify the output does not carry them), `None`
/// when there is no XFA.
pub(crate) fn remove_xfa(
    doc: &Document,
    overlay: &mut Overlay,
) -> Result<Option<HashSet<IndirectRef>>, Error> {
    let mut packets = HashSet::new();
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => *r,
        _ => return Ok(None),
    };
    let Object::Dict(mut catalog) = current(doc, overlay, root)? else {
        return Ok(None);
    };
    let (form_ref, mut form) = match catalog.get(b"AcroForm".as_slice()) {
        Some(Object::Ref(r)) => match current(doc, overlay, *r)? {
            Object::Dict(d) => (Some(*r), d),
            _ => return Ok(None),
        },
        Some(Object::Dict(d)) => (None, d.clone()),
        _ => return Ok(None),
    };
    let Some(xfa) = form.remove(b"XFA".as_slice()) else {
        return Ok(None);
    };
    if deref_cur(
        doc,
        overlay,
        catalog
            .get(b"NeedsRendering".as_slice())
            .unwrap_or(&Object::Null),
    )?
    .as_bool()
        == Some(true)
    {
        return Err(refuse(
            "a dynamic XFA form (/NeedsRendering): it has no AcroForm to keep",
        ));
    }
    // The packets: one stream, or an array of names and streams.
    let items = match &xfa {
        Object::Ref(_) => vec![xfa.clone()],
        Object::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    for i in items {
        if let Object::Ref(r) = i {
            packets.insert(r);
        }
    }
    if let Object::Ref(r) = xfa {
        // An array of packets given by reference.
        if !matches!(current(doc, overlay, r)?, Object::Stream { .. }) {
            if let Object::Array(a) = current(doc, overlay, r)? {
                for i in a {
                    if let Object::Ref(p) = i {
                        packets.insert(p);
                    }
                }
            }
        }
    }
    match form_ref {
        Some(r) => {
            overlay.insert((r.number, r.generation), Object::Dict(form));
        }
        None => {
            catalog.insert(b"AcroForm".to_vec(), Object::Dict(form));
            overlay.insert((root.number, root.generation), Object::Dict(catalog));
        }
    }
    Ok(Some(packets))
}

/// Whether the edited document's AcroForm still has an XFA form.
pub(crate) fn has_xfa(doc: &Document, overlay: &Overlay) -> Result<bool, Error> {
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => *r,
        _ => return Ok(false),
    };
    let Object::Dict(catalog) = current(doc, overlay, root)? else {
        return Ok(false);
    };
    let Some(f) = catalog.get(b"AcroForm".as_slice()) else {
        return Ok(false);
    };
    Ok(deref_cur(doc, overlay, f)?
        .as_dict()
        .is_some_and(|d| d.contains_key(b"XFA".as_slice())))
}
