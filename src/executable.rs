//! Classification of executable content for [`rewrite_with`]
//! (`docs/plans/rewrite-policy.md` §3.1–§3.3), and the policy refusals.
//!
//! [`classify`] runs after the rewrite has written its output, on the
//! input objects the rewrite already read, so it never fails: a cycle in
//! an owner tree, an action chain past the walk depth or an object that
//! cannot be read becomes an [`UnclassifiedStructure`], never an error
//! and never "nothing there". [`enforce`] applies the policy.
//!
//! [`rewrite_with`]: crate::rewrite_with

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::rewrite_policy::{
    ActionClass, ActionKind, ActionOwner, ExecutableContentPolicy, ExecutableItem,
    MultimediaSubtype, ScriptSize, Trigger, UnclassifiedReason, UnclassifiedStructure,
};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// `Error::Unsupported` features of the policy refusals
/// ([`Error::refusal_reason`] classifies by them).
pub(crate) const REFUSED_SCRIPT: &str = "executable content: script";
pub(crate) const REFUSED_EXTERNAL: &str = "executable content: external action";
pub(crate) const REFUSED_UNKNOWN: &str = "executable content: unknown action";
pub(crate) const REFUSED_UNCLASSIFIED: &str = "executable content: unclassified structure";

/// `/AA` keys of ISO 32000-2 Tables 197–200.
const TRIGGERS: [&str; 20] = [
    "E", "X", "D", "U", "Fo", "Bl", "PO", "PC", "PV", "PI", "O", "C", "K", "F", "V", "WC", "WS",
    "DS", "WP", "DP",
];

fn trigger(key: &[u8]) -> Trigger {
    TRIGGERS
        .iter()
        .find(|t| t.as_bytes() == key)
        .map(|t| Trigger(t))
        .unwrap_or(Trigger::OTHER)
}

fn kind_of(s: Option<&[u8]>) -> ActionKind {
    let Some(s) = s else {
        return ActionKind::Missing;
    };
    match s {
        b"JavaScript" => ActionKind::JavaScript,
        b"Launch" => ActionKind::Launch,
        b"URI" => ActionKind::Uri,
        b"GoToR" => ActionKind::GoToR,
        b"GoToE" => ActionKind::GoToE,
        b"SubmitForm" => ActionKind::SubmitForm,
        b"ImportData" => ActionKind::ImportData,
        b"ResetForm" => ActionKind::ResetForm,
        b"Hide" => ActionKind::Hide,
        b"SetOCGState" => ActionKind::SetOcgState,
        b"Trans" => ActionKind::Trans,
        b"Named" => ActionKind::Named,
        b"GoTo" => ActionKind::GoTo,
        b"GoToDp" => ActionKind::GoToDp,
        b"Thread" => ActionKind::Thread,
        b"GoTo3DView" => ActionKind::GoTo3DView,
        b"Rendition" => ActionKind::Rendition,
        b"Movie" => ActionKind::Movie,
        b"Sound" => ActionKind::Sound,
        b"RichMediaExecute" => ActionKind::RichMediaExecute,
        _ => ActionKind::Other,
    }
}

/// The class of an action dictionary of kind `kind` (`d` for `/Named`'s
/// `/N`).
fn class_of(kind: ActionKind, d: &Dict) -> ActionClass {
    use ActionKind as K;
    match kind {
        K::JavaScript | K::XfaScript => ActionClass::Script,
        K::Launch | K::Uri | K::GoToR | K::GoToE | K::SubmitForm | K::ImportData => {
            ActionClass::External
        }
        K::ResetForm | K::Hide | K::SetOcgState | K::Trans => ActionClass::State,
        K::Named => match d.get(b"N".as_slice()).and_then(|n| n.as_name()) {
            Some(b"NextPage" | b"PrevPage" | b"FirstPage" | b"LastPage") => ActionClass::Navigation,
            _ => ActionClass::State,
        },
        K::GoTo | K::GoToDp | K::Thread | K::GoTo3DView => ActionClass::Navigation,
        K::Rendition | K::Movie | K::Sound | K::RichMediaExecute | K::MultimediaAnnotation(_) => {
            ActionClass::Multimedia
        }
        _ => ActionClass::Unknown,
    }
}

fn multimedia_subtype(d: &Dict) -> Option<MultimediaSubtype> {
    Some(match d.get(b"Subtype".as_slice())?.as_name()? {
        b"RichMedia" => MultimediaSubtype::RichMedia,
        b"Screen" => MultimediaSubtype::Screen,
        b"Movie" => MultimediaSubtype::Movie,
        b"Sound" => MultimediaSubtype::Sound,
        b"3D" => MultimediaSubtype::ThreeD,
        _ => return None,
    })
}

enum Root {
    Action(Object),
    Media(MultimediaSubtype, Option<IndirectRef>),
}

struct Classifier<'a> {
    doc: &'a Document,
    depth_limit: usize,
    output: &'a dyn Fn(IndirectRef) -> Option<IndirectRef>,
    items: Vec<ExecutableItem>,
    unclassified: Vec<UnclassifiedStructure>,
    /// Action object → its first item.
    first: HashMap<IndirectRef, usize>,
    /// Every annotation reference in a page's `/Annots`.
    annots: HashSet<IndirectRef>,
}

/// Every executable item of `doc`, and the structures that could not be
/// classified. `output` maps an input object to the rewrite's.
pub(crate) fn classify(
    doc: &Document,
    limits: &Limits,
    output: &dyn Fn(IndirectRef) -> Option<IndirectRef>,
) -> (Vec<ExecutableItem>, Vec<UnclassifiedStructure>) {
    let mut c = Classifier {
        doc,
        depth_limit: limits.max_rewrite_walk_depth,
        output,
        items: Vec::new(),
        unclassified: Vec::new(),
        first: HashMap::new(),
        annots: HashSet::new(),
    };
    let roots = c.roots();
    for (owner, root) in roots {
        match root {
            Root::Action(o) => c.chain(owner, o),
            Root::Media(subtype, r) => c.items.push(ExecutableItem {
                class: ActionClass::Multimedia,
                kind: ActionKind::MultimediaAnnotation(subtype),
                owner,
                chain_position: 0,
                input_object: r,
                output_object: r.and_then(output),
                script: None,
                shared_with: None,
            }),
        }
    }
    c.xfa();
    (c.items, c.unclassified)
}

/// Refuse what `policy` does not carry: under
/// `RefuseScriptsAndExternal` every script, external or unknown item and
/// any unclassified structure; under `Carry` unknown items and
/// unclassified structures. `Profile` refuses nothing here (its refusals
/// are `rewrite`'s, made before any output).
pub(crate) fn enforce(
    policy: ExecutableContentPolicy,
    items: &[ExecutableItem],
    unclassified: &[UnclassifiedStructure],
    doc: &Document,
    limits: &Limits,
) -> Result<(), Error> {
    let name = match policy {
        ExecutableContentPolicy::Profile => return Ok(()),
        ExecutableContentPolicy::RefuseScriptsAndExternal => "RefuseScriptsAndExternal",
        ExecutableContentPolicy::Carry => "Carry",
    };
    let strict = policy == ExecutableContentPolicy::RefuseScriptsAndExternal;
    for it in items {
        let feature = match it.class {
            ActionClass::Script if strict => REFUSED_SCRIPT,
            ActionClass::External if strict => REFUSED_EXTERNAL,
            ActionClass::Unknown => REFUSED_UNKNOWN,
            _ => continue,
        };
        return Err(Error::Unsupported {
            feature,
            detail: format!(
                "{} action {} at {} (chain position {}) — refused by the {name} policy",
                it.class.as_str(),
                it.kind.as_str(),
                it.owner,
                it.chain_position
            ),
        });
    }
    if let Some(u) = unclassified.first() {
        let at = u
            .owner
            .as_ref()
            .map(|o| format!(" at {o}"))
            .unwrap_or_default();
        return Err(Error::Unsupported {
            feature: REFUSED_UNCLASSIFIED,
            detail: format!(
                "the {}{at} could not be classified ({:?}) — refused by the {name} policy",
                u.structure, u.reason
            ),
        });
    }
    // Defence in depth: a script or external action anywhere in the
    // document, also where no viewer looks for one (a destination's
    // parameters, an unknown key), is refused by the strict policy —
    // the classification of owners is not the only gate.
    if strict {
        if let Some(what) = action_shaped_anywhere(doc, limits) {
            return Err(Error::Unsupported {
                feature: REFUSED_UNCLASSIFIED,
                detail: format!(
                    "{what} outside the action owners the classification follows — refused by the \
                     {name} policy"
                ),
            });
        }
    }
    Ok(())
}

/// Action types whose dictionary the strict policy refuses wherever it
/// is: scripts and external actions.
const SCRIPT_OR_EXTERNAL: [&[u8]; 7] = [
    b"JavaScript",
    b"Launch",
    b"URI",
    b"GoToR",
    b"GoToE",
    b"SubmitForm",
    b"ImportData",
];

/// A dictionary reachable from the trailer with a script or external
/// `/S`, or with a `/JS` entry: what it is. Every reference is followed
/// once; the walk is bounded by a multiple of the objects the rewrite
/// may write (`max_output_objects`) — past it, the answer is "too large
/// to search", which refuses too.
fn action_shaped_anywhere(doc: &Document, limits: &Limits) -> Option<&'static str> {
    let mut seen: HashSet<IndirectRef> = HashSet::new();
    let mut stack: Vec<Object> = Vec::new();
    for k in [b"Root".as_slice(), b"Info".as_slice()] {
        if let Some(v) = doc.trailer().get(k) {
            stack.push(v.clone());
        }
    }
    let mut nodes = 0usize;
    while let Some(o) = stack.pop() {
        nodes += 1;
        if nodes > limits.max_output_objects.saturating_mul(64) {
            return Some("a structure too large to search for actions");
        }
        let o = match o {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                match doc.resolve(r) {
                    Ok(v) => v,
                    Err(_) => return Some("an object that cannot be read"),
                }
            }
            o => o,
        };
        let d = match o {
            Object::Dict(d) => d,
            // A stream's `/Length` is not followed, as the rewrite does
            // not follow it (it writes the length directly).
            Object::Stream { mut dict, .. } => {
                dict.remove(b"Length".as_slice());
                dict
            }
            Object::Array(a) => {
                stack.extend(a);
                continue;
            }
            _ => continue,
        };
        if d.contains_key(b"JS".as_slice()) {
            return Some("a dictionary with a /JS script");
        }
        // `/S` directly or through a reference.
        let s = match d.get(b"S".as_slice()) {
            Some(Object::Name(n)) => Some(n.clone()),
            Some(r @ Object::Ref(_)) => match crate::rewrite::deref_chain(doc, r) {
                Ok(Object::Name(n)) => Some(n),
                Ok(_) => None,
                Err(_) => return Some("an object that cannot be read"),
            },
            _ => None,
        };
        if s.is_some_and(|s| SCRIPT_OR_EXTERNAL.contains(&s.as_slice())) {
            return Some("a script or external action");
        }
        stack.extend(d.into_values());
    }
    None
}

impl Classifier<'_> {
    fn get(&self, o: &Object) -> Result<Object, Error> {
        crate::rewrite::deref_chain(self.doc, o)
    }

    fn unclassified(
        &mut self,
        owner: Option<ActionOwner>,
        structure: &'static str,
        reason: UnclassifiedReason,
    ) {
        self.unclassified.push(UnclassifiedStructure {
            owner,
            structure,
            reason,
        });
    }

    /// A dictionary entry that is present and not null (directly or
    /// through a reference to an undefined object).
    fn entry(&mut self, d: &Dict, key: &[u8], what: &'static str) -> Option<Object> {
        let v = d.get(key)?;
        match self.get(v) {
            Ok(Object::Null) => None,
            Ok(_) => Some(v.clone()),
            Err(_) => {
                self.unclassified(None, what, UnclassifiedReason::Unreadable);
                None
            }
        }
    }

    fn dict(&mut self, o: &Object, what: &'static str) -> Option<Dict> {
        match self.get(o) {
            Ok(Object::Dict(d)) | Ok(Object::Stream { dict: d, .. }) => Some(d),
            Ok(_) => None,
            Err(_) => {
                self.unclassified(None, what, UnclassifiedReason::Unreadable);
                None
            }
        }
    }

    fn roots(&mut self) -> Vec<(ActionOwner, Root)> {
        let mut out = Vec::new();
        let cat = self.doc.catalog().clone();
        if let Some(oa) = self.entry(&cat, b"OpenAction", "catalog /OpenAction") {
            match self.get(&oa) {
                Ok(Object::Dict(_)) => {
                    out.push((ActionOwner::OpenAction { member: None }, Root::Action(oa)))
                }
                // A destination — its first element a page or a page
                // number — unless the first element is an action
                // dictionary (non-standard, seen): then each member is
                // one (a null member is no action, another value
                // malformed).
                Ok(Object::Array(a)) => {
                    let actions = a.first().is_some_and(|x| {
                        matches!(self.get(x), Ok(Object::Dict(d)) if d.contains_key(b"S".as_slice()))
                    });
                    if actions {
                        for (i, x) in a.into_iter().enumerate() {
                            out.push((
                                ActionOwner::OpenAction {
                                    member: Some(i as u32),
                                },
                                Root::Action(x),
                            ));
                        }
                    }
                }
                // A named destination.
                Ok(Object::Name(_) | Object::String(_) | Object::Null) => {}
                // Malformed: the chain lists it as such.
                _ => out.push((ActionOwner::OpenAction { member: None }, Root::Action(oa))),
            }
        }
        for (t, v) in self.aa(&cat, "catalog /AA") {
            out.push((ActionOwner::CatalogAa(t), Root::Action(v)));
        }
        let pages: Vec<Option<IndirectRef>> =
            self.doc.pages().iter().map(|p| p.object_ref()).collect();
        let mut page_annots = Vec::new();
        for pr in &pages {
            let pd = pr.and_then(|r| self.dict(&Object::Ref(r), "page"));
            let items = match pd
                .as_ref()
                .and_then(|pd| pd.get(b"Annots".as_slice()).cloned())
            {
                Some(a) => match self.get(&a) {
                    Ok(Object::Array(items)) => items,
                    Ok(_) => Vec::new(),
                    Err(_) => {
                        self.unclassified(None, "page /Annots", UnclassifiedReason::Unreadable);
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            for it in &items {
                if let Some(r) = it.as_ref() {
                    self.annots.insert(r);
                }
            }
            page_annots.push((pd, items));
        }
        for (i, (pd, items)) in page_annots.into_iter().enumerate() {
            let page = i as u32;
            if let Some(pd) = &pd {
                for (t, v) in self.aa(pd, "page /AA") {
                    out.push((ActionOwner::PageAa { page, trigger: t }, Root::Action(v)));
                }
            }
            for (k, it) in items.iter().enumerate() {
                let index = k as u32;
                let Some(ad) = self.dict(it, "annotation") else {
                    continue;
                };
                if let Some(st) = multimedia_subtype(&ad) {
                    out.push((
                        ActionOwner::Annotation { page, index },
                        Root::Media(st, it.as_ref()),
                    ));
                }
                if let Some(a) = self.entry(&ad, b"A", "annotation /A") {
                    out.push((ActionOwner::AnnotationA { page, index }, Root::Action(a)));
                }
                for (t, v) in self.aa(&ad, "annotation /AA") {
                    out.push((
                        ActionOwner::AnnotationAa {
                            page,
                            index,
                            trigger: t,
                        },
                        Root::Action(v),
                    ));
                }
            }
        }
        if let Some(af) = cat.get(b"AcroForm".as_slice()).cloned() {
            if let Some(afd) = self.dict(&af, "/AcroForm") {
                if let Some(f) = afd.get(b"Fields".as_slice()) {
                    if let Ok(Object::Array(fields)) = self.get(f) {
                        let mut seen = HashSet::new();
                        for (i, x) in fields.iter().enumerate() {
                            self.fields(x, vec![i as u32], &mut seen, &mut out);
                        }
                    }
                }
            }
        }
        if let Some(o) = cat.get(b"Outlines".as_slice()).cloned() {
            if let Some(od) = self.dict(&o, "/Outlines") {
                let mut seen = HashSet::new();
                self.outlines(&od, Vec::new(), &mut seen, &mut out);
            }
        }
        if let Some(n) = cat.get(b"Names".as_slice()).cloned() {
            if let Some(nd) = self.dict(&n, "/Names") {
                if let Some(t) = self.entry(&nd, b"JavaScript", "/Names /JavaScript") {
                    for (i, v) in self.tree(&t).into_iter().enumerate() {
                        out.push((ActionOwner::NameTree { index: i as u32 }, Root::Action(v)));
                    }
                }
            }
        }
        out
    }

    /// The triggers of `d`'s `/AA`, in key order.
    fn aa(&mut self, d: &Dict, what: &'static str) -> Vec<(Trigger, Object)> {
        let Some(aa) = self.entry(d, b"AA", what) else {
            return Vec::new();
        };
        match self.get(&aa) {
            Ok(Object::Dict(aa)) => aa.into_iter().map(|(k, v)| (trigger(&k), v)).collect(),
            _ => Vec::new(),
        }
    }

    fn fields(
        &mut self,
        o: &Object,
        path: Vec<u32>,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<(ActionOwner, Root)>,
    ) {
        if path.len() > self.depth_limit {
            return self.unclassified(None, "field tree", UnclassifiedReason::DepthLimit);
        }
        if let Some(r) = o.as_ref() {
            if !seen.insert(r) {
                return self.unclassified(None, "field tree", UnclassifiedReason::Cycle);
            }
        }
        let Some(d) = self.dict(o, "field") else {
            return;
        };
        let on_page = o.as_ref().is_some_and(|r| self.annots.contains(&r));
        if !on_page {
            for (t, v) in self.aa(&d, "field /AA") {
                out.push((
                    ActionOwner::FieldAa {
                        path: path.clone(),
                        trigger: t,
                    },
                    Root::Action(v),
                ));
            }
        }
        if let Some(k) = d.get(b"Kids".as_slice()) {
            if let Ok(Object::Array(kids)) = self.get(k) {
                for (i, k) in kids.iter().enumerate() {
                    let mut p = path.clone();
                    p.push(i as u32);
                    self.fields(k, p, seen, out);
                }
            }
        }
    }

    fn outlines(
        &mut self,
        parent: &Dict,
        at: Vec<u32>,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<(ActionOwner, Root)>,
    ) {
        if at.len() > self.depth_limit {
            return self.unclassified(None, "outline tree", UnclassifiedReason::DepthLimit);
        }
        let mut cur = parent.get(b"First".as_slice()).cloned();
        let mut i = 0u32;
        while let Some(o) = cur {
            if let Some(r) = o.as_ref() {
                if !seen.insert(r) {
                    return self.unclassified(None, "outline tree", UnclassifiedReason::Cycle);
                }
            }
            let Some(d) = self.dict(&o, "outline item") else {
                break;
            };
            let mut p = at.clone();
            p.push(i);
            if let Some(a) = self.entry(&d, b"A", "outline /A") {
                out.push((ActionOwner::Outline { path: p.clone() }, Root::Action(a)));
            }
            self.outlines(&d, p, seen, out);
            cur = d.get(b"Next".as_slice()).cloned();
            i += 1;
        }
    }

    /// The values of a name tree, in key order (leaves depth first).
    fn tree(&mut self, root: &Object) -> Vec<Object> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((node, depth)) = stack.pop() {
            if depth > self.depth_limit {
                self.unclassified(
                    None,
                    "/Names /JavaScript tree",
                    UnclassifiedReason::DepthLimit,
                );
                return out;
            }
            if let Some(r) = node.as_ref() {
                if !seen.insert(r) {
                    self.unclassified(None, "/Names /JavaScript tree", UnclassifiedReason::Cycle);
                    return out;
                }
            }
            let Some(d) = self.dict(&node, "/Names /JavaScript tree") else {
                self.unclassified(
                    None,
                    "/Names /JavaScript tree",
                    UnclassifiedReason::Unreadable,
                );
                return out;
            };
            if let Some(kv) = d.get(b"Names".as_slice()) {
                if let Ok(Object::Array(kv)) = self.get(kv) {
                    for pair in kv.chunks(2) {
                        if let [_, v] = pair {
                            out.push(v.clone());
                        }
                    }
                }
            }
            if let Some(kids) = d.get(b"Kids".as_slice()) {
                if let Ok(Object::Array(kids)) = self.get(kids) {
                    for k in kids.into_iter().rev() {
                        stack.push((k, depth + 1));
                    }
                }
            }
        }
        out
    }

    /// The action chain of `owner` from `o`: depth first through `/Next`
    /// (a dictionary or an array), a cycle ending the branch, a shared
    /// action listed with `shared_with` and not expanded again.
    fn chain(&mut self, owner: ActionOwner, o: Object) {
        let mut position = 0u32;
        let mut stack: Vec<(Object, usize, Rc<Vec<IndirectRef>>)> = vec![(o, 0, Rc::default())];
        while let Some((o, depth, path)) = stack.pop() {
            if depth > self.depth_limit {
                self.unclassified(
                    Some(owner.clone()),
                    "action chain",
                    UnclassifiedReason::DepthLimit,
                );
                return;
            }
            let r = o.as_ref();
            if r.is_some_and(|r| path.contains(&r)) {
                continue;
            }
            let d = match self.get(&o) {
                Ok(Object::Dict(d)) => d,
                // An undefined reference is null (§7.3.10): no action.
                Ok(Object::Null) => continue,
                Ok(_) => {
                    self.push(
                        owner.clone(),
                        ActionClass::Unknown,
                        ActionKind::Malformed,
                        position,
                        r,
                        None,
                        None,
                    );
                    position += 1;
                    continue;
                }
                Err(_) => {
                    self.unclassified(
                        Some(owner.clone()),
                        "action",
                        UnclassifiedReason::Unreadable,
                    );
                    position += 1;
                    continue;
                }
            };
            // `/S` directly or through a reference (ISO 32000 allows an
            // indirect value anywhere).
            let s = d.get(b"S".as_slice()).and_then(|s| match self.get(s) {
                Ok(Object::Name(n)) => Some(n),
                _ => None,
            });
            let kind = kind_of(s.as_deref());
            let shared = r.and_then(|r| self.first.get(&r).copied());
            let script = (kind == ActionKind::JavaScript)
                .then(|| d.get(b"JS".as_slice()).map(|js| self.script(js)))
                .flatten()
                .flatten();
            if let (Some(r), None) = (r, shared) {
                self.first.insert(r, self.items.len());
            }
            self.push(
                owner.clone(),
                class_of(kind, &d),
                kind,
                position,
                r,
                script,
                shared,
            );
            position += 1;
            if shared.is_some() {
                continue;
            }
            if let Some(n) = self.entry(&d, b"Next", "action /Next") {
                let next = match self.get(&n) {
                    Ok(Object::Array(a)) => a,
                    _ => vec![n],
                };
                let path = match r {
                    Some(r) => {
                        let mut p = (*path).clone();
                        p.push(r);
                        Rc::new(p)
                    }
                    None => path,
                };
                for x in next.into_iter().rev() {
                    stack.push((x, depth + 1, Rc::clone(&path)));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        owner: ActionOwner,
        class: ActionClass,
        kind: ActionKind,
        chain_position: u32,
        input: Option<IndirectRef>,
        script: Option<ScriptSize>,
        shared_with: Option<usize>,
    ) {
        self.items.push(ExecutableItem {
            class,
            kind,
            owner,
            chain_position,
            input_object: input,
            output_object: input.and_then(self.output),
            script,
            shared_with,
        });
    }

    /// The size of a `/JS` value: a string's length, a decoded stream's.
    fn script(&self, js: &Object) -> Option<ScriptSize> {
        match self.get(js).ok()? {
            Object::String(s) => Some(ScriptSize::Bytes(s.len() as u64)),
            Object::Stream { dict, data } => Some(match self.doc.decode_stream(&dict, &data) {
                Ok(b) => ScriptSize::Bytes(b.len() as u64),
                Err(_) => ScriptSize::Undecodable,
            }),
            _ => None,
        }
    }

    /// XFA scripts: one item per `script` element of each packet, and
    /// for those seen only across packet boundaries.
    fn xfa(&mut self) {
        let cat = self.doc.catalog();
        let Some(af) = cat.get(b"AcroForm".as_slice()) else {
            return;
        };
        let Ok(af) = self.get(af) else { return };
        let Some(xfa) = af.as_dict().and_then(|d| d.get(b"XFA".as_slice())).cloned() else {
            return;
        };
        match crate::rewrite::xfa_scripts(self.doc, &xfa) {
            Ok(found) => {
                for (owner, input, count) in found {
                    for n in 0..count {
                        self.push(
                            owner.clone(),
                            ActionClass::Script,
                            ActionKind::XfaScript,
                            n,
                            input,
                            None,
                            None,
                        );
                    }
                }
            }
            Err(_) => self.unclassified(None, "XFA packets", UnclassifiedReason::Unreadable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(page_aa: &str) -> Document {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /AA << /O {page_aa} >> >>"),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let x = out.len();
        out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
        );
        Document::from_bytes(&out).unwrap()
    }

    #[test]
    fn a_chain_past_the_walk_depth_is_unclassified_not_cut() {
        let mut chain = "<< /S /JavaScript /JS (x) >>".to_string();
        for _ in 0..10 {
            chain = format!("<< /S /GoTo /D [3 0 R /Fit] /Next {chain} >>");
        }
        let d = doc(&chain);
        let limits = Limits {
            max_rewrite_walk_depth: 4,
            ..Limits::default()
        };
        let (items, unclassified) = classify(&d, &limits, &|_| None);
        assert_eq!(items.len(), 5, "the steps within the depth");
        assert_eq!(unclassified.len(), 1);
        assert_eq!(unclassified[0].reason, UnclassifiedReason::DepthLimit);
        // Carry and the strict policy refuse it; Profile reports it.
        assert!(enforce(
            ExecutableContentPolicy::Carry,
            &items,
            &unclassified,
            &d,
            &Limits::default()
        )
        .is_err());
        assert!(enforce(
            ExecutableContentPolicy::Profile,
            &items,
            &unclassified,
            &d,
            &Limits::default()
        )
        .is_ok());
        let (items, unclassified) = classify(&d, &Limits::default(), &|_| None);
        assert_eq!((items.len(), unclassified.len()), (11, 0));
    }

    #[test]
    fn xfa_script_elements_by_local_name() {
        let n = crate::rewrite::count_xfa_scripts;
        assert_eq!(n(b"<script>"), 1);
        assert_eq!(n(b"<script/>"), 1);
        assert_eq!(n(b"<script\ncontentType='x'>"), 1);
        // Any ASCII case: an over-report (XML is case-sensitive), as the
        // XML-signature scan matches its namespace in any case.
        assert_eq!(n(b"<SCRIPT>"), 1);
        assert_eq!(n(b"<x:Script/>"), 1);
        assert_eq!(n(b"<xfa:script>"), 1);
        assert_eq!(n(b"<a.b-c_d:script >"), 1);
        assert_eq!(n(b"</script>"), 0);
        assert_eq!(n(b"<scripts>"), 0);
        assert_eq!(n(b"<myscript>"), 0);
        assert_eq!(n(b"<x:y:script>"), 0, "one prefix only");
        assert_eq!(n(b"<script"), 0, "not delimited");
        assert_eq!(n(b"<!-- <script> -->"), 1, "over-report");
    }
}
