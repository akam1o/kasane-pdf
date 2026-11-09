//! Redaction of a document's structure tree (§14.7): the text the tree
//! keeps about what the rects remove goes, the rest of the tree stays.
//!
//! The edit records which content it touched — the marked-content
//! sequences (`MCID`) holding an edited op, on a page or in a form; the
//! images and forms whose originals were edited; the annotations removed
//! ([`Touched`]). A structure element is touched when one of its
//! content items is (an `MCID` integer or `/MCR`, an `/OBJR`), or one
//! of its descendants is. Each touched element loses its text —
//! `/Alt`, `/ActualText`, `/E` and `/T` — whole: which part of it
//! describes the removed content is not known, so none of it is kept.
//! Its ancestors are touched too (their text covers it). An `/OBJR` to
//! a removed annotation or an edited original, and an `/MCR` into an
//! edited form's original, leave the element's `/K` (they would keep
//! those objects reachable), and the `/ParentTree` entry of a removed
//! annotation goes. Untouched elements keep everything.
//!
//! Content items are matched as conservatively as the tree allows: an
//! `MCID` whose page is not known (no `/Pg` on it or its element) is
//! touched when that number is touched anywhere; a page `MCID` is also
//! touched when an edited form drawn on the page without
//! `/StructParents` (so without MCIDs of its own) uses that number; a
//! sequence whose `MCID` cannot be read (named properties that do not
//! resolve) touches every `MCID` of its stream. Elements are found from
//! the root's `/K` and from `/ParentTree`, and ancestors through both
//! the tree and `/P`.
//!
//! Refused: a cycle among elements, a kid that is not an element,
//! `MCID`, `/MCR`, `/OBJR` or null, an `/MCR` without an integer
//! `/MCID`, an `/OBJR` whose `/Obj` is not a reference, and an element
//! that needs editing but is written directly inside another object.
//! Verification re-reads the output's tree: no element may carry a text
//! that was removed, unless an untouched element of the input carried
//! the same text (reported in `elsewhere`, like any copy).

use crate::content::Operation;
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The keys of a structure element holding text about its content:
/// alternate description, replacement text, expansion, title.
const TEXT_KEYS: [&[u8]; 4] = [b"Alt", b"ActualText", b"E", b"T"];

/// Number trees deeper than this are refused.
const MAX_NUMBER_TREE_DEPTH: usize = 64;

/// What a redaction edit touched, for the structure tree.
#[derive(Debug, Default, Clone)]
pub(crate) struct Touched {
    /// Per page: the `MCID`s of page-content sequences holding an edit.
    pub(crate) page_mcids: HashMap<IndirectRef, BTreeSet<i64>>,
    /// Per form XObject (the original): the `MCID`s of its sequences
    /// holding an edit.
    pub(crate) form_mcids: HashMap<IndirectRef, BTreeSet<i64>>,
    /// Per page: the `MCID`s touched in the forms it draws that have no
    /// `/StructParents`.
    pub(crate) page_form_mcids: HashMap<IndirectRef, BTreeSet<i64>>,
    /// Pages and forms with an edited sequence whose `MCID` could not
    /// be read: every `MCID` of theirs is touched.
    pub(crate) all_mcids: HashSet<IndirectRef>,
    /// Edited image and form originals and removed annotations.
    pub(crate) objects: HashSet<IndirectRef>,
    /// Of those, the removed annotations (their `/ParentTree` entries go).
    pub(crate) removed_annotations: HashSet<IndirectRef>,
}

impl Touched {
    /// Record the sequences of `ops` (a page's content or a form's,
    /// `owner`) that hold an op `edited` reports.
    pub(crate) fn record(
        &mut self,
        doc: &Document,
        ops: &[Operation],
        edited: &dyn Fn(usize) -> bool,
        res: Option<&Object>,
        owner: Owner,
    ) -> Result<(), Error> {
        let (mcids, unreadable) = touched_mcids(doc, ops, edited, res)?;
        if mcids.is_empty() && !unreadable {
            return Ok(());
        }
        let (r, set) = match owner {
            Owner::Page(p) => (p, self.page_mcids.entry(p).or_default()),
            Owner::Form { form, page, tagged } => {
                // A form without `/StructParents` has no MCIDs of its
                // own: a tree may name them as the page's.
                if !tagged {
                    self.page_form_mcids
                        .entry(page)
                        .or_default()
                        .extend(mcids.iter().copied());
                    if unreadable {
                        self.all_mcids.insert(page);
                    }
                }
                (form, self.form_mcids.entry(form).or_default())
            }
        };
        set.extend(mcids);
        if unreadable {
            self.all_mcids.insert(r);
        }
        Ok(())
    }

    fn any_mcid(&self, m: i64) -> bool {
        !self.all_mcids.is_empty()
            || self.page_mcids.values().any(|s| s.contains(&m))
            || self.form_mcids.values().any(|s| s.contains(&m))
    }

    fn page_mcid(&self, page: IndirectRef, m: i64) -> bool {
        self.all_mcids.contains(&page)
            || self.page_mcids.get(&page).is_some_and(|s| s.contains(&m))
            || self
                .page_form_mcids
                .get(&page)
                .is_some_and(|s| s.contains(&m))
    }

    fn form_mcid(&self, form: IndirectRef, m: i64) -> bool {
        self.all_mcids.contains(&form)
            || self.objects.contains(&form)
            || self.form_mcids.get(&form).is_some_and(|s| s.contains(&m))
    }
}

/// The content stream a [`Touched::record`] call is about.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Owner {
    Page(IndirectRef),
    /// A form's original, drawn on `page`; `tagged` when it has
    /// `/StructParents`.
    Form {
        form: IndirectRef,
        page: IndirectRef,
        tagged: bool,
    },
}

/// The `MCID`s of the marked-content sequences of `ops` holding an op
/// `edited` reports (nested sequences: every enclosing one), and whether
/// one such sequence's `MCID` could not be read.
fn touched_mcids(
    doc: &Document,
    ops: &[Operation],
    edited: &dyn Fn(usize) -> bool,
    res: Option<&Object>,
) -> Result<(BTreeSet<i64>, bool), Error> {
    #[derive(Clone, Copy)]
    enum Id {
        None,
        Mcid(i64),
        Unreadable,
    }
    let mcid_of = |d: &Dict| match d.get(b"MCID".as_slice()) {
        None => Id::None,
        Some(Object::Integer(m)) => Id::Mcid(*m),
        Some(_) => Id::Unreadable,
    };
    let mut out = BTreeSet::new();
    let mut unreadable = false;
    let mut open: Vec<(Id, bool)> = Vec::new();
    let mut close = |e: (Id, bool), out: &mut BTreeSet<i64>| match e {
        (Id::Mcid(m), true) => {
            out.insert(m);
        }
        (Id::Unreadable, true) => unreadable = true,
        _ => {}
    };
    for (i, op) in ops.iter().enumerate() {
        match op {
            Operation::BeginMarkedContentWithProps { properties, .. } => {
                let id = match properties {
                    Object::Dict(d) => mcid_of(d),
                    Object::Name(n) => {
                        let resolved = match res {
                            Some(r) => doc
                                .resolve_resource_raw(r, "Properties", n)
                                .ok()
                                .flatten()
                                .and_then(|o| doc.deref(&o).ok()),
                            None => None,
                        };
                        match resolved.as_ref().and_then(|o| o.as_dict()) {
                            Some(d) => mcid_of(d),
                            None => Id::Unreadable,
                        }
                    }
                    _ => Id::Unreadable,
                };
                open.push((id, false));
            }
            Operation::BeginMarkedContent(_) => open.push((Id::None, false)),
            Operation::EndMarkedContent => {
                if let Some(e) = open.pop() {
                    close(e, &mut out);
                }
            }
            _ if edited(i) => open.iter_mut().for_each(|e| e.1 = true),
            _ => {}
        }
    }
    while let Some(e) = open.pop() {
        close(e, &mut out);
    }
    Ok((out, unreadable))
}

/// What [`edit_tree`] did.
#[derive(Debug, Default)]
pub(crate) struct StructureEdit {
    /// Text entries removed from elements.
    pub(crate) texts_removed: u64,
    /// Content items (`/OBJR`, `/MCR`) removed from elements' `/K`.
    pub(crate) items_removed: u64,
    /// `/ParentTree` entries removed.
    pub(crate) parent_entries_removed: u64,
    /// The removed texts (searched for elsewhere; never kept in
    /// `Debug` output by the caller).
    pub(crate) removed: Vec<String>,
    /// The texts untouched elements keep.
    pub(crate) kept: HashSet<String>,
}

/// A content item of an element.
enum Item {
    /// A marked-content sequence: its page (when known), the form
    /// holding it (an `/MCR /Stm`), its `MCID`.
    Mcid {
        page: Option<IndirectRef>,
        stm: Option<IndirectRef>,
        mcid: i64,
    },
    /// An object (`/OBJR`).
    Obj(IndirectRef),
}

/// One element found in the tree.
struct Element {
    /// `None` for an element written directly inside another object.
    r: Option<IndirectRef>,
    dict: Dict,
    /// Its content items, by their index in its `/K` list.
    items: Vec<(usize, Item)>,
    /// Child elements (indices into the element list).
    children: Vec<usize>,
}

/// The current body of `r`: the overlay's, else the document's. An
/// absent object is null.
fn current(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    r: IndirectRef,
) -> Result<Object, Error> {
    if let Some(o) = overlay.get(&(r.number, r.generation)) {
        return Ok(o.clone());
    }
    match doc.resolve(r) {
        Ok(o) => Ok(o),
        Err(Error::MissingReference(_)) => Ok(Object::Null),
        Err(e) => Err(e),
    }
}

fn deref_cur(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    o: &Object,
) -> Result<Object, Error> {
    match o {
        Object::Ref(r) => current(doc, overlay, *r),
        o => Ok(o.clone()),
    }
}

fn refuse(detail: impl Into<String>) -> Error {
    Error::Unsupported {
        feature: "redaction of a document whose structure tree cannot be edited exactly",
        detail: detail.into(),
    }
}

/// The shared `max_audit_elements` budget of one tree operation.
struct Budget {
    left: u64,
    max: u64,
}

impl Budget {
    fn new(limits: &Limits) -> Self {
        Budget {
            left: limits.max_audit_elements,
            max: limits.max_audit_elements,
        }
    }

    fn charge(&mut self) -> Result<(), Error> {
        if self.left == 0 {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: self.max,
            });
        }
        self.left -= 1;
        Ok(())
    }
}

/// The `/K` entries of an element, as a list.
fn kid_list(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    d: &Dict,
) -> Result<Vec<Object>, Error> {
    let Some(k) = d.get(b"K".as_slice()) else {
        return Ok(Vec::new());
    };
    // `/K` may be a reference to an array — or to a single kid.
    if let Object::Ref(r) = k {
        if let Object::Array(a) = current(doc, overlay, *r)? {
            return Ok(a);
        }
    }
    Ok(match k {
        Object::Array(a) => a.clone(),
        k => vec![k.clone()],
    })
}

fn page_ref(o: Option<&Object>) -> Option<IndirectRef> {
    match o {
        Some(Object::Ref(r)) => Some(*r),
        _ => None,
    }
}

/// The structure tree as read: elements (from the root's `/K`, and any
/// found later), `/ParentTree` entries.
struct Tree {
    elements: Vec<Element>,
    by_ref: HashMap<IndirectRef, usize>,
    /// `/ParentTree` key → its value(s).
    parent_tree: BTreeMap<i64, Vec<Object>>,
    /// The root, when indirect: a `/P` to it ends a walk up.
    root: Option<IndirectRef>,
}

impl Tree {
    /// Add the element `kid` (with `parent`) and every element below
    /// it; its index (`None` for null). A known element only gains the
    /// parent.
    fn add(
        &mut self,
        doc: &Document,
        overlay: &HashMap<(u32, u16), Object>,
        kid: Object,
        parent: Option<usize>,
        budget: &mut Budget,
    ) -> Result<Option<usize>, Error> {
        let mut first = None;
        let mut queue: Vec<(Object, Option<usize>)> = vec![(kid, parent)];
        let mut q = 0;
        while q < queue.len() {
            let (kid, parent) = queue[q].clone();
            let top = q == 0;
            q += 1;
            budget.charge()?;
            let r = match &kid {
                Object::Ref(r) => Some(*r),
                _ => None,
            };
            if let Some(&i) = r.as_ref().and_then(|r| self.by_ref.get(r)) {
                if let Some(p) = parent {
                    self.elements[p].children.push(i);
                }
                if top {
                    first = Some(i);
                }
                continue;
            }
            let d = match deref_cur(doc, overlay, &kid)? {
                Object::Dict(d) => d,
                Object::Null => continue,
                _ => return Err(refuse("a structure element is not a dictionary")),
            };
            let i = self.elements.len();
            if top {
                first = Some(i);
            }
            if let Some(r) = r {
                self.by_ref.insert(r, i);
            }
            if let Some(p) = parent {
                self.elements[p].children.push(i);
            }
            let pg = page_ref(d.get(b"Pg".as_slice()));
            let mut items = Vec::new();
            for (k, kid) in kid_list(doc, overlay, &d)?.into_iter().enumerate() {
                budget.charge()?;
                match &kid {
                    Object::Integer(m) => items.push((
                        k,
                        Item::Mcid {
                            page: pg,
                            stm: None,
                            mcid: *m,
                        },
                    )),
                    Object::Null => {}
                    Object::Dict(_) | Object::Ref(_) => {
                        let body = deref_cur(doc, overlay, &kid)?;
                        let Some(kd) = body.as_dict() else {
                            if matches!(body, Object::Null) {
                                continue;
                            }
                            return Err(refuse("a structure element kid is not a dictionary"));
                        };
                        let ty = kd.get(b"Type".as_slice()).and_then(|t| t.as_name());
                        let element = kd.contains_key(b"S".as_slice());
                        if ty == Some(b"MCR") || (!element && kd.contains_key(b"MCID".as_slice())) {
                            let Some(Object::Integer(m)) = kd.get(b"MCID".as_slice()) else {
                                return Err(refuse("a marked-content reference without an MCID"));
                            };
                            items.push((
                                k,
                                Item::Mcid {
                                    page: page_ref(kd.get(b"Pg".as_slice())).or(pg),
                                    stm: page_ref(kd.get(b"Stm".as_slice())),
                                    mcid: *m,
                                },
                            ));
                        } else if ty == Some(b"OBJR")
                            || (!element && kd.contains_key(b"Obj".as_slice()))
                        {
                            match kd.get(b"Obj".as_slice()) {
                                Some(Object::Ref(o)) => items.push((k, Item::Obj(*o))),
                                _ => {
                                    return Err(refuse(
                                        "an object reference whose /Obj is not a reference",
                                    ))
                                }
                            }
                        } else {
                            queue.push((kid.clone(), Some(i)));
                        }
                    }
                    _ => return Err(refuse("a structure element kid of an unknown kind")),
                }
            }
            self.elements.push(Element {
                r,
                dict: d,
                items,
                children: Vec::new(),
            });
        }
        Ok(first)
    }

    /// Refuse a cycle among the elements (Kahn's algorithm over the
    /// child edges).
    fn check_acyclic(&self) -> Result<(), Error> {
        let n = self.elements.len();
        let mut indegree = vec![0usize; n];
        for e in &self.elements {
            for &c in &e.children {
                indegree[c] += 1;
            }
        }
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut done = 0;
        while let Some(i) = ready.pop() {
            done += 1;
            for &c in &self.elements[i].children {
                indegree[c] -= 1;
                if indegree[c] == 0 {
                    ready.push(c);
                }
            }
        }
        if done != n {
            return Err(Error::CycleDetected {
                context: "structure tree",
            });
        }
        Ok(())
    }
}

fn read_tree(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    root_obj: &Object,
    budget: &mut Budget,
) -> Result<Tree, Error> {
    let root = deref_cur(doc, overlay, root_obj)?;
    let Some(root) = root.as_dict() else {
        return Err(refuse("/StructTreeRoot is not a dictionary"));
    };
    let mut tree = Tree {
        elements: Vec::new(),
        by_ref: HashMap::new(),
        parent_tree: BTreeMap::new(),
        root: page_ref(Some(root_obj)),
    };
    for kid in kid_list(doc, overlay, root)? {
        tree.add(doc, overlay, kid, None, budget)?;
    }
    // `/ParentTree`, a number tree.
    if let Some(pt) = root.get(b"ParentTree".as_slice()) {
        let mut seen = HashSet::new();
        let mut stack = vec![(pt.clone(), 0usize)];
        while let Some((node, depth)) = stack.pop() {
            budget.charge()?;
            if depth > MAX_NUMBER_TREE_DEPTH {
                return Err(refuse("/ParentTree is nested too deeply"));
            }
            if let Object::Ref(r) = &node {
                if !seen.insert(*r) {
                    return Err(Error::CycleDetected {
                        context: "structure tree /ParentTree",
                    });
                }
            }
            let node = deref_cur(doc, overlay, &node)?;
            let Some(nd) = node.as_dict() else {
                if matches!(node, Object::Null) {
                    continue;
                }
                return Err(refuse("a /ParentTree node is not a dictionary"));
            };
            if let Some(nums) = nd.get(b"Nums".as_slice()) {
                let nums = deref_cur(doc, overlay, nums)?;
                let nums = nums
                    .as_array()
                    .ok_or_else(|| refuse("/ParentTree /Nums is not an array"))?;
                for pair in nums.chunks(2) {
                    budget.charge()?;
                    let [Object::Integer(key), value] = pair else {
                        return Err(refuse("/ParentTree /Nums is not key-value pairs"));
                    };
                    tree.parent_tree
                        .entry(*key)
                        .or_default()
                        .push(value.clone());
                }
            }
            if let Some(kids) = nd.get(b"Kids".as_slice()) {
                let kids = deref_cur(doc, overlay, kids)?;
                let kids = kids
                    .as_array()
                    .ok_or_else(|| refuse("/ParentTree /Kids is not an array"))?;
                stack.extend(kids.iter().map(|k| (k.clone(), depth + 1)));
            }
        }
    }
    Ok(tree)
}

/// The values `/ParentTree` holds for `key`, flattened: the element of
/// a `/StructParent`, the elements of a `/StructParents` (by `MCID`,
/// `None` keeping all of them, else those of `mcids`).
fn parent_tree_elements(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    tree: &Tree,
    key: &Object,
    mcids: Option<&BTreeSet<i64>>,
    budget: &mut Budget,
) -> Result<Vec<Object>, Error> {
    let Object::Integer(k) = key else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for v in tree.parent_tree.get(k).into_iter().flatten() {
        match deref_cur(doc, overlay, v)? {
            Object::Array(arr) => {
                for (m, e) in arr.iter().enumerate() {
                    budget.charge()?;
                    if mcids.is_none_or(|s| s.contains(&(m as i64))) {
                        out.push(e.clone());
                    }
                }
            }
            _ => out.push(v.clone()),
        }
    }
    Ok(out)
}

/// Edit the structure tree of `doc` in `overlay` for what `touched`
/// reports: see the module documentation.
pub(crate) fn edit_tree(
    doc: &Document,
    overlay: &mut HashMap<(u32, u16), Object>,
    touched: &Touched,
    limits: &Limits,
) -> Result<StructureEdit, Error> {
    let mut out = StructureEdit::default();
    let Some(root_obj) = doc.catalog().get(b"StructTreeRoot".as_slice()).cloned() else {
        return Ok(out);
    };
    let mut budget = Budget::new(limits);
    let mut tree = read_tree(doc, overlay, &root_obj, &mut budget)?;

    // Seeds through `/ParentTree`: the elements of the touched MCIDs of
    // each page and form, and of each touched object. Elements reached
    // only this way are read too (the tree refers to them, so they are
    // written).
    let mut seeds: Vec<Object> = Vec::new();
    let mut removed_keys: BTreeSet<i64> = BTreeSet::new();
    let mut owners: Vec<(IndirectRef, Option<BTreeSet<i64>>)> = Vec::new();
    for (&p, mcids) in &touched.page_mcids {
        owners.push((p, Some(mcids.clone())));
    }
    for (&p, mcids) in &touched.page_form_mcids {
        owners.push((p, Some(mcids.clone())));
    }
    for (&f, mcids) in &touched.form_mcids {
        owners.push((f, Some(mcids.clone())));
    }
    for &o in &touched.all_mcids {
        owners.push((o, None));
    }
    for (o, mcids) in owners {
        let body = current(doc, overlay, o)?;
        let Some(d) = body.as_dict_like() else {
            continue;
        };
        if let Some(key) = d.get(b"StructParents".as_slice()) {
            let mcids = if touched.all_mcids.contains(&o) {
                None
            } else {
                mcids.as_ref()
            };
            seeds.extend(parent_tree_elements(
                doc,
                overlay,
                &tree,
                key,
                mcids,
                &mut budget,
            )?);
        }
    }
    for &o in &touched.objects {
        let body = current(doc, overlay, o)?;
        let Some(d) = body.as_dict_like() else {
            continue;
        };
        let Some(key) = d.get(b"StructParent".as_slice()) else {
            continue;
        };
        seeds.extend(parent_tree_elements(
            doc,
            overlay,
            &tree,
            key,
            None,
            &mut budget,
        )?);
        if touched.removed_annotations.contains(&o) {
            if let Object::Integer(k) = key {
                removed_keys.insert(*k);
            }
        }
    }
    let mut hit: HashSet<usize> = HashSet::new();
    for e in seeds {
        if let Some(i) = tree.add(doc, overlay, e, None, &mut budget)? {
            hit.insert(i);
        }
    }

    // Items, then ancestors (through the tree and `/P`), until no new
    // element is found.
    let mut drop_items: HashMap<usize, BTreeSet<usize>> = HashMap::new();
    let mut evaluated = 0;
    let mut climbed: HashSet<usize> = HashSet::new();
    loop {
        let n = tree.elements.len();
        for i in evaluated..n {
            for (k, item) in &tree.elements[i].items {
                budget.charge()?;
                let (touches, drop) = match item {
                    Item::Mcid {
                        stm: Some(f), mcid, ..
                    } => (touched.form_mcid(*f, *mcid), touched.objects.contains(f)),
                    Item::Mcid {
                        page: Some(p),
                        stm: None,
                        mcid,
                    } => (touched.page_mcid(*p, *mcid), false),
                    Item::Mcid {
                        page: None, mcid, ..
                    } => (touched.any_mcid(*mcid), false),
                    Item::Obj(o) => {
                        let gone = touched.objects.contains(o);
                        (gone, gone)
                    }
                };
                if touches {
                    hit.insert(i);
                }
                if drop {
                    drop_items.entry(i).or_default().insert(*k);
                }
            }
        }
        evaluated = n;
        let mut parents: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, e) in tree.elements.iter().enumerate() {
            for &c in &e.children {
                parents.entry(c).or_default().push(i);
            }
        }
        let mut stack: Vec<usize> = hit.iter().copied().collect();
        while let Some(i) = stack.pop() {
            budget.charge()?;
            let mut up = parents.get(&i).cloned().unwrap_or_default();
            if climbed.insert(i) {
                if let Some(p) = tree.elements[i].dict.get(b"P".as_slice()).cloned() {
                    let to_root = matches!((&p, tree.root), (Object::Ref(a), Some(b)) if *a == b);
                    if !to_root {
                        if let Some(j) = tree.add(doc, overlay, p, None, &mut budget)? {
                            up.push(j);
                        }
                    }
                }
            }
            for j in up {
                if hit.insert(j) {
                    stack.push(j);
                }
            }
        }
        if tree.elements.len() == evaluated {
            break;
        }
    }
    tree.check_acyclic()?;

    // Edit the touched elements.
    for (i, e) in tree.elements.iter().enumerate() {
        let strip = hit.contains(&i) && TEXT_KEYS.iter().any(|k| e.dict.contains_key(*k));
        let drops = drop_items.get(&i);
        if !strip && drops.is_none() {
            for k in TEXT_KEYS {
                if let Some(t) = text_of(doc, overlay, e.dict.get(k))? {
                    out.kept.insert(t);
                }
            }
            continue;
        }
        let Some(r) = e.r else {
            return Err(refuse(
                "a structure element to edit is written directly inside another object",
            ));
        };
        let mut d = e.dict.clone();
        if strip {
            for k in TEXT_KEYS {
                if let Some(v) = d.remove(k) {
                    out.texts_removed += 1;
                    if let Some(t) = text_of(doc, overlay, Some(&v))? {
                        out.removed.push(t);
                    }
                }
            }
        }
        if let Some(drops) = drops {
            let kids: Vec<Object> = kid_list(doc, overlay, &e.dict)?
                .into_iter()
                .enumerate()
                .filter(|(k, _)| !drops.contains(k))
                .map(|(_, o)| o)
                .collect();
            out.items_removed += drops.len() as u64;
            if kids.is_empty() {
                d.remove(b"K".as_slice());
            } else {
                d.insert(b"K".to_vec(), Object::Array(kids));
            }
        }
        overlay.insert((r.number, r.generation), Object::Dict(d));
    }

    if !removed_keys.is_empty() {
        out.parent_entries_removed = prune_parent_tree(doc, overlay, &root_obj, &removed_keys)?;
    }
    Ok(out)
}

/// A text string's text (an indirect one resolved).
fn text_of(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    o: Option<&Object>,
) -> Result<Option<String>, Error> {
    let Some(o) = o else { return Ok(None) };
    Ok(match deref_cur(doc, overlay, o)? {
        Object::String(s) => Some(crate::annot_text::text_string(&s)),
        _ => None,
    })
}

/// Remove `keys` from the root's `/ParentTree`, rewriting the nodes
/// holding them; the count removed.
fn prune_parent_tree(
    doc: &Document,
    overlay: &mut HashMap<(u32, u16), Object>,
    root_obj: &Object,
    keys: &BTreeSet<i64>,
) -> Result<u64, Error> {
    // A node's new body when it changed: written to the overlay for an
    // indirect node, returned for a direct one (its holder rewrites).
    fn prune(
        doc: &Document,
        overlay: &mut HashMap<(u32, u16), Object>,
        node: &Object,
        keys: &BTreeSet<i64>,
        depth: usize,
        removed: &mut u64,
    ) -> Result<Option<Object>, Error> {
        if depth > MAX_NUMBER_TREE_DEPTH {
            return Err(refuse("/ParentTree is nested too deeply"));
        }
        let body = deref_cur(doc, overlay, node)?;
        let Object::Dict(mut d) = body else {
            return Ok(None);
        };
        let mut changed = false;
        if let Some(nums) = d.get(b"Nums".as_slice()).cloned() {
            let arr = deref_cur(doc, overlay, &nums)?;
            if let Some(arr) = arr.as_array() {
                let mut kept = Vec::with_capacity(arr.len());
                for pair in arr.chunks(2) {
                    match pair {
                        [Object::Integer(k), _] if keys.contains(k) => {
                            *removed += 1;
                            changed = true;
                        }
                        _ => kept.extend(pair.iter().cloned()),
                    }
                }
                if changed {
                    d.insert(b"Nums".to_vec(), Object::Array(kept));
                }
            }
        }
        if let Some(kids) = d.get(b"Kids".as_slice()).cloned() {
            let arr = deref_cur(doc, overlay, &kids)?;
            if let Some(arr) = arr.as_array() {
                let mut new = arr.to_vec();
                let mut kids_changed = false;
                for k in new.iter_mut() {
                    if let Some(n) = prune(doc, overlay, k, keys, depth + 1, removed)? {
                        *k = n;
                        kids_changed = true;
                    }
                }
                if kids_changed {
                    d.insert(b"Kids".to_vec(), Object::Array(new));
                    changed = true;
                }
            }
        }
        if !changed {
            return Ok(None);
        }
        match node {
            Object::Ref(r) => {
                overlay.insert((r.number, r.generation), Object::Dict(d));
                Ok(None)
            }
            _ => Ok(Some(Object::Dict(d))),
        }
    }
    let mut removed = 0;
    let root = deref_cur(doc, overlay, root_obj)?;
    let Some(mut rd) = root.as_dict().cloned() else {
        return Ok(0);
    };
    let Some(pt) = rd.get(b"ParentTree".as_slice()).cloned() else {
        return Ok(0);
    };
    if let Some(new) = prune(doc, overlay, &pt, keys, 0, &mut removed)? {
        let Object::Ref(r) = root_obj else {
            return Err(refuse("a direct /StructTreeRoot whose /ParentTree changes"));
        };
        rd.insert(b"ParentTree".to_vec(), new);
        overlay.insert((r.number, r.generation), Object::Dict(rd));
    }
    Ok(removed)
}

/// Check the output's structure tree: it reads (no cycle, every kid of
/// a known kind) and no element carries a text that was removed and
/// that no untouched element kept.
pub(crate) fn verify(out: &Document, edit: &StructureEdit, limits: &Limits) -> Result<(), Error> {
    let Some(root) = out.catalog().get(b"StructTreeRoot".as_slice()).cloned() else {
        return Ok(());
    };
    let overlay = HashMap::new();
    let mut budget = Budget::new(limits);
    let mut tree = read_tree(out, &overlay, &root, &mut budget)?;
    tree.check_acyclic()?;
    let gone: HashSet<&String> = edit
        .removed
        .iter()
        .filter(|t| !edit.kept.contains(*t))
        .collect();
    if gone.is_empty() {
        return Ok(());
    }
    // Elements reached through `/ParentTree` too.
    let values: Vec<Object> = std::mem::take(&mut tree.parent_tree)
        .into_values()
        .flatten()
        .collect();
    let mut dicts: Vec<Dict> = tree.elements.into_iter().map(|e| e.dict).collect();
    for v in values {
        let v = out.deref(&v)?;
        let list = match &v {
            Object::Array(a) => a.clone(),
            o => vec![o.clone()],
        };
        for e in list {
            budget.charge()?;
            if let Object::Dict(d) = out.deref(&e)? {
                dicts.push(d);
            }
        }
    }
    for d in dicts {
        for k in TEXT_KEYS {
            if let Some(t) = text_of(out, &overlay, d.get(k))? {
                if gone.contains(&t) {
                    return Err(Error::Unsupported {
                        feature: "redaction output failed verification",
                        detail: format!(
                            "a structure element still carries a removed /{}",
                            String::from_utf8_lossy(k)
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}
