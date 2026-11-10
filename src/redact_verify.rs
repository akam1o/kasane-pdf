//! Verification invariants of `redact` that do not rely on what the
//! redaction recorded about its own edits: each compares the input with
//! the written output, re-parsed.
//!
//! - **Use** ([`check_use`]): every stream an edited page *used* in the
//!   input to picture or spell content (as [`crate::redact_usage`] walks
//!   it, undecidable points counted as used) is, in the output, either
//!   used again or gone. A copy the output carries but does not use —
//!   kept through some other reference, under any role, at any object
//!   number — fails: that is how removed data has stayed in past
//!   versions (an aliased content stream, an annotation appearance, a
//!   glyph procedure, a mask, a thumbnail). Streams are compared by the
//!   SHA-256 of their stored and decoded bytes; empty ones carry nothing
//!   and are not compared.
//! - **Non-interference** (also [`check_use`]): every page not edited
//!   draws, in the output, from the same content-bearing streams as in
//!   the input (by digest). An edit of a shared object must not change
//!   another page's drawing. Exceptions: a thumbnail, removed from every
//!   page sharing an edited page's; a font's code data and program (a
//!   Type 3 font that loses glyphs no page shows gets a new
//!   `/ToUnicode` on every page using it).
//! - **Removed annotations** ([`check_removed_annotations`]): an
//!   annotation an edited page had and no longer has is in no dictionary
//!   of the output, a stream's included.
//! - **Reachability** ([`check_reachable`]): every object of the output
//!   is reachable from the trailer (the rewrite writes nothing else; an
//!   unreachable object is data any parser still reads).
//!
//! What these do not establish — that no *re-encoded* copy of removed
//! data remains, or that interpreted data (font programs, ICC
//! profiles) holds nothing about the removed content — is stated in
//! `docs/redaction-safety-model.md`.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{IndirectRef, Object};
use crate::redact_usage::{usage, Bound, Usage, Use};
use crate::remove_region::sha256;

fn fail(detail: String) -> Error {
    Error::Unsupported {
        feature: "redaction output failed verification",
        detail,
    }
}

/// Every live object of a document, resolved once, with its stream
/// digests (stored bytes; decoded bytes when they decode).
pub(crate) struct Index {
    pub(crate) objects: Vec<(IndirectRef, Object)>,
    /// Per stream: (stored, decoded) digests; `decoded` is `None` when
    /// the stream does not decode, and the digests are both `None` when
    /// it holds nothing but whitespace.
    pub(crate) digests: HashMap<IndirectRef, StreamDigests>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamDigests {
    pub(crate) stored: [u8; 32],
    pub(crate) decoded: Option<[u8; 32]>,
    /// The decoded bytes are empty or whitespace only.
    pub(crate) blank: bool,
}

impl StreamDigests {
    fn of(doc: &Document, dict: &crate::object::Dict, data: &[u8]) -> StreamDigests {
        let decoded = doc.decode_stream(dict, data).ok();
        let blank = match &decoded {
            Some(d) => d.iter().all(|b| b.is_ascii_whitespace()),
            None => data.iter().all(|b| b.is_ascii_whitespace()),
        };
        StreamDigests {
            stored: sha256(data),
            decoded: decoded.map(|d| sha256(&d)),
            blank,
        }
    }

    fn each(&self) -> impl Iterator<Item = [u8; 32]> {
        std::iter::once(self.stored).chain(self.decoded)
    }
}

impl Index {
    pub(crate) fn build(doc: &Document, limits: &Limits) -> Result<Index, Error> {
        let refs: Vec<IndirectRef> = doc
            .xref_entries()
            .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
            .map(|(r, _)| r)
            .collect();
        if refs.len() as u64 > limits.max_audit_elements {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: limits.max_audit_elements,
            });
        }
        let mut objects = Vec::with_capacity(refs.len());
        let mut digests = HashMap::new();
        for r in refs {
            let o = doc.resolve(r)?;
            if let Object::Stream { dict, data } = &o {
                digests.insert(r, StreamDigests::of(doc, dict, data));
            }
            objects.push((r, o));
        }
        Ok(Index { objects, digests })
    }
}

/// The reference graph of a document: who refers to whom, under which
/// key, and what the trailer reaches.
pub(crate) struct RefGraph {
    /// Per object: the objects referring to it, with the key path from
    /// the referrer's top level.
    pub(crate) parents: HashMap<IndirectRef, Vec<(Option<IndirectRef>, String)>>,
    pub(crate) reachable: HashSet<IndirectRef>,
}

impl RefGraph {
    pub(crate) fn build(doc: &Document, idx: &Index, limits: &Limits) -> Result<RefGraph, Error> {
        let mut parents: HashMap<IndirectRef, Vec<(Option<IndirectRef>, String)>> = HashMap::new();
        let mut edges: HashMap<IndirectRef, Vec<IndirectRef>> = HashMap::new();
        let mut budget: u64 = 0;
        let mut out_edges =
            |from: Option<IndirectRef>,
             o: &Object,
             parents: &mut HashMap<IndirectRef, Vec<(Option<IndirectRef>, String)>>|
             -> Result<Vec<IndirectRef>, Error> {
                let mut found = Vec::new();
                let mut stack: Vec<(&Object, String)> = vec![(o, String::new())];
                while let Some((o, path)) = stack.pop() {
                    budget += 1;
                    if budget > limits.max_audit_elements.saturating_mul(4) {
                        return Err(Error::LimitExceeded {
                            limit: "max_audit_elements",
                            max: limits.max_audit_elements,
                        });
                    }
                    match o {
                        Object::Ref(r) => {
                            let e = parents.entry(*r).or_default();
                            if e.len() < 8 {
                                e.push((from, path));
                            }
                            found.push(*r);
                        }
                        Object::Array(a) => {
                            for (i, v) in a.iter().enumerate() {
                                stack.push((v, format!("{path}[{i}]")));
                            }
                        }
                        Object::Dict(d) | Object::Stream { dict: d, .. } => {
                            for (k, v) in d {
                                stack.push((v, format!("{path}/{}", String::from_utf8_lossy(k))));
                            }
                        }
                        _ => {}
                    }
                }
                Ok(found)
            };
        let trailer = Object::Dict(doc.trailer().clone());
        let roots = out_edges(None, &trailer, &mut parents)?;
        for (r, o) in &idx.objects {
            let e = out_edges(Some(*r), o, &mut parents)?;
            edges.insert(*r, e);
        }
        let mut reachable: HashSet<IndirectRef> = HashSet::new();
        let mut queue: VecDeque<IndirectRef> = roots.into_iter().collect();
        while let Some(r) = queue.pop_front() {
            if !reachable.insert(r) {
                continue;
            }
            if let Some(next) = edges.get(&r) {
                queue.extend(next.iter().copied().filter(|n| !reachable.contains(n)));
            }
        }
        Ok(RefGraph { parents, reachable })
    }

    /// A chain of referrers from the trailer to `r`, as key paths, for
    /// messages (`/Root/PieceInfo/App/Private`); `None` if none is found.
    pub(crate) fn path_to(&self, r: IndirectRef) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = r;
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(cur) || parts.len() > 32 {
                return None;
            }
            let ps = self.parents.get(&cur)?;
            // Prefer the trailer, then a reachable referrer.
            let (from, path) = ps.iter().find(|(f, _)| f.is_none()).or_else(|| {
                ps.iter()
                    .find(|(f, _)| f.is_some_and(|f| self.reachable.contains(&f)))
            })?;
            parts.push(path.clone());
            match from {
                None => break,
                Some(f) => cur = *f,
            }
        }
        parts.reverse();
        Some(parts.concat())
    }
}

/// Every object of the output is reachable from its trailer.
pub(crate) fn check_reachable(idx: &Index, graph: &RefGraph) -> Result<(), Error> {
    for (r, _) in &idx.objects {
        if !graph.reachable.contains(r) {
            return Err(fail(format!(
                "object {r} is in the output but nothing reachable refers to it"
            )));
        }
    }
    Ok(())
}

/// Digests of the streams `u` lists, with the content-bearing roles they
/// are used in (blank streams left out).
fn used_digests(
    doc: &Document,
    u: &Usage,
    idx: Option<&Index>,
) -> Result<HashMap<[u8; 32], BTreeSet<Use>>, Error> {
    let mut out: HashMap<[u8; 32], BTreeSet<Use>> = HashMap::new();
    for (r, (roles, _)) in &u.used {
        let d = match idx.and_then(|i| i.digests.get(r)) {
            Some(d) => *d,
            None => match doc.resolve(*r)? {
                Object::Stream { dict, data } => StreamDigests::of(doc, &dict, &data),
                _ => continue,
            },
        };
        if d.blank {
            continue;
        }
        for x in d.each() {
            out.entry(x).or_default().extend(roles.iter().copied());
        }
    }
    Ok(out)
}

/// The use invariant and non-interference (see the module
/// documentation): `input` the document redacted (substitutes embedded,
/// if asked), `out` the re-parsed output, `edited` the pages redacted.
pub(crate) fn check_use(
    input: &Document,
    out: &Document,
    idx: &Index,
    graph: &RefGraph,
    edited: &BTreeSet<usize>,
    limits: &Limits,
) -> Result<(), Error> {
    let is_edited = |i: usize| edited.contains(&i);
    // What the edited pages used, every undecidable point counted.
    let before = usage(input, &is_edited, Bound::Over, limits)?;
    let wanted: HashMap<[u8; 32], BTreeSet<Use>> = used_digests(input, &before, None)?
        .into_iter()
        .filter(|(_, roles)| roles.iter().any(|r| r.content_bearing()))
        .collect();
    if !wanted.is_empty() {
        // The output streams holding such data.
        let held: Vec<(IndirectRef, [u8; 32])> = idx
            .digests
            .iter()
            .filter(|(_, d)| !d.blank)
            .filter_map(|(r, d)| d.each().find(|x| wanted.contains_key(x)).map(|x| (*r, x)))
            .collect();
        if !held.is_empty() {
            let mut live: HashSet<[u8; 32]> = HashSet::new();
            let after = usage(out, &is_edited, Bound::Under, limits)?;
            live.extend(used_digests(out, &after, Some(idx))?.into_keys());
            let mut left: Vec<&(IndirectRef, [u8; 32])> =
                held.iter().filter(|(_, x)| !live.contains(x)).collect();
            if !left.is_empty() {
                let rest = usage(out, &|i| !is_edited(i), Bound::Under, limits)?;
                live.extend(used_digests(out, &rest, Some(idx))?.into_keys());
                left.retain(|(_, x)| !live.contains(x));
            }
            if let Some((r, x)) = left.into_iter().min_by_key(|(r, _)| *r) {
                let roles: Vec<String> = wanted[x].iter().map(|u| format!("{u:?}")).collect();
                return Err(fail(format!(
                    "object {r} holds data an edited page used ({}) that the output carries without \
                     using it{}",
                    roles.join(", "),
                    graph
                        .path_to(*r)
                        .map(|p| format!(" (reached by {p})"))
                        .unwrap_or_default()
                )));
            }
        }
    }
    non_interference(input, out, idx, edited, limits)
}

/// Every page not edited uses the same streams in the output as in the
/// input, by digest (a thumbnail may be gone).
fn non_interference(
    input: &Document,
    out: &Document,
    idx: &Index,
    edited: &BTreeSet<usize>,
    limits: &Limits,
) -> Result<(), Error> {
    let n = input.pages().len();
    if out.pages().len() != n {
        return Err(fail(format!(
            "the output has {} pages, the input {n}",
            out.pages().len()
        )));
    }
    let others: Vec<usize> = (0..n).filter(|i| !edited.contains(i)).collect();
    if others.is_empty() {
        return Ok(());
    }
    // Pages whose drawing is the same graph in both need no walk.
    let mut sig_in = Signatures::new(input);
    let mut sig_out = Signatures::new(out);
    let mut walk: BTreeSet<usize> = BTreeSet::new();
    for &i in &others {
        let a = sig_in.page(&input.pages()[i], limits)?;
        let b = sig_out.page(&out.pages()[i], limits)?;
        if a.is_none() || a != b {
            walk.insert(i);
        }
    }
    if walk.is_empty() {
        return Ok(());
    }
    let sel = |i: usize| walk.contains(&i);
    let a = usage(input, &sel, Bound::Under, limits)?;
    let b = usage(out, &sel, Bound::Under, limits)?;
    for &i in &walk {
        let set = |doc: &Document,
                   u: &Usage,
                   idx: Option<&Index>|
         -> Result<BTreeSet<(Use, [u8; 32])>, Error> {
            let mut s = BTreeSet::new();
            for (r, roles) in u.on_page(i) {
                let d = match idx.and_then(|x| x.digests.get(&r)) {
                    Some(d) => *d,
                    None => match doc.resolve(r)? {
                        Object::Stream { dict, data } => StreamDigests::of(doc, &dict, &data),
                        _ => continue,
                    },
                };
                // What is drawn: a font's code data may be rewritten (a
                // Type 3 font losing glyphs no page shows gets a new
                // `/ToUnicode` for every page using it).
                for role in roles.iter().filter(|r| r.content_bearing()) {
                    s.insert((*role, d.stored));
                }
            }
            Ok(s)
        };
        let before = set(input, &a, None)?;
        let after = set(out, &b, Some(idx))?;
        let lost: Vec<&(Use, [u8; 32])> = before
            .difference(&after)
            .filter(|(role, _)| *role != Use::Thumbnail)
            .collect();
        let gained: Vec<&(Use, [u8; 32])> = after.difference(&before).collect();
        if let Some((role, _)) = lost.first().or(gained.first()) {
            return Err(fail(format!(
                "page {} (not edited) draws differently in the output ({} {role:?} stream(s) \
                 changed): an edit of a shared object reached it",
                i + 1,
                lost.len() + gained.len()
            )));
        }
    }
    Ok(())
}

/// A digest of what a page draws from — its content streams, its
/// resources in force, annotations and thumbnail — as a graph
/// (references replaced by what they refer to; back links to the page
/// tree and between annotations left out), so the same in an output
/// that renumbered its objects. `None` when the graph has a cycle or
/// is too large to digest.
struct Signatures<'a> {
    doc: &'a Document,
    memo: HashMap<IndirectRef, Option<[u8; 32]>>,
    open: HashSet<IndirectRef>,
    work: u64,
}

const BACK_LINKS: &[&[u8]] = &[
    b"Parent",
    b"P",
    b"Popup",
    b"IRT",
    b"StructParent",
    b"StructParents",
];

impl<'a> Signatures<'a> {
    fn new(doc: &'a Document) -> Self {
        Signatures {
            doc,
            memo: HashMap::new(),
            open: HashSet::new(),
            work: 0,
        }
    }

    fn page(
        &mut self,
        page: &crate::page::Page,
        limits: &Limits,
    ) -> Result<Option<[u8; 32]>, Error> {
        let Some(pr) = page.object_ref() else {
            return Ok(None);
        };
        let pd = match self.doc.resolve(pr)? {
            Object::Dict(d) => d,
            _ => return Ok(None),
        };
        let mut parts = Vec::new();
        for k in [b"Contents".as_slice(), b"Annots", b"Thumb", b"Group"] {
            parts.extend_from_slice(k);
            match pd.get(k) {
                Some(v) => match self.value(v, limits)? {
                    Some(d) => parts.extend_from_slice(&d),
                    None => return Ok(None),
                },
                None => parts.push(0),
            }
        }
        parts.extend_from_slice(b"Resources");
        match page.resources() {
            Some(v) => match self.value(v, limits)? {
                Some(d) => parts.extend_from_slice(&d),
                None => return Ok(None),
            },
            None => parts.push(0),
        }
        Ok(Some(sha256(&parts)))
    }

    fn value(&mut self, o: &Object, limits: &Limits) -> Result<Option<[u8; 32]>, Error> {
        self.work += 1;
        if self.work > limits.max_audit_elements {
            return Ok(None);
        }
        let mut out = Vec::new();
        match o {
            Object::Ref(r) => {
                if let Some(d) = self.memo.get(r) {
                    return Ok(*d);
                }
                if !self.open.insert(*r) || self.open.len() > 256 {
                    return Ok(None);
                }
                let t = self.doc.resolve(*r)?;
                let d = self.value(&t, limits)?;
                self.open.remove(r);
                self.memo.insert(*r, d);
                return Ok(d);
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                if let Object::Stream { data, .. } = o {
                    out.push(b'S');
                    out.extend_from_slice(&sha256(data));
                }
                out.push(b'<');
                let mut keys: Vec<&Vec<u8>> = d
                    .keys()
                    .filter(|k| !BACK_LINKS.contains(&k.as_slice()))
                    .collect();
                keys.sort();
                for k in keys {
                    out.extend_from_slice(&(k.len() as u64).to_be_bytes());
                    out.extend_from_slice(k);
                    match self.value(&d[k], limits)? {
                        Some(x) => out.extend_from_slice(&x),
                        None => return Ok(None),
                    }
                }
            }
            Object::Array(a) => {
                out.push(b'[');
                for v in a {
                    match self.value(v, limits)? {
                        Some(x) => out.extend_from_slice(&x),
                        None => return Ok(None),
                    }
                }
            }
            other => {
                out.push(b'.');
                out.extend_from_slice(format!("{other:?}").as_bytes());
            }
        }
        Ok(Some(sha256(&out)))
    }
}

/// The print of a dictionary for [`check_removed_annotations`]: that of
/// [`crate::remove_region::annotation_print`] without the keys a stream
/// adds (a copy written as a stream's dictionary has them).
fn annotation_print(d: &crate::object::Dict) -> [u8; 32] {
    let mut d = d.clone();
    for k in [
        b"Length".as_slice(),
        b"Filter",
        b"DecodeParms",
        b"DL",
        b"F",
        b"FFilter",
        b"FDecodeParms",
    ] {
        d.remove(k);
    }
    crate::remove_region::annotation_print(&d)
}

/// No annotation an edited page had in the input and does not have in
/// the output is still in the output, as any dictionary or a stream's
/// dictionary, whatever refers to it (the removed annotations are found
/// by comparing the pages, not from the edit's record).
pub(crate) fn check_removed_annotations(
    input: &Document,
    out: &Document,
    idx: &Index,
    graph: &RefGraph,
    edited: &BTreeSet<usize>,
) -> Result<(), Error> {
    let prints = |doc: &Document, i: usize| -> Result<HashSet<[u8; 32]>, Error> {
        let mut set = HashSet::new();
        let Some(page) = doc.pages().get(i) else {
            return Ok(set);
        };
        if let Some(a) = crate::redact_diag::page_dict_entry(doc, page, b"Annots")? {
            for x in doc.deref(&a)?.as_array().unwrap_or(&[]) {
                if let Some(d) = doc.deref(x)?.as_dict() {
                    set.insert(annotation_print(d));
                }
            }
        }
        Ok(set)
    };
    let mut removed: HashSet<[u8; 32]> = HashSet::new();
    for &i in edited {
        let kept = prints(out, i)?;
        removed.extend(prints(input, i)?.into_iter().filter(|p| !kept.contains(p)));
    }
    if removed.is_empty() {
        return Ok(());
    }
    for (r, o) in &idx.objects {
        let mut stack: Vec<&Object> = vec![o];
        while let Some(o) = stack.pop() {
            let d = match o {
                Object::Dict(d) | Object::Stream { dict: d, .. } => d,
                Object::Array(a) => {
                    stack.extend(a.iter());
                    continue;
                }
                _ => continue,
            };
            if removed.contains(&annotation_print(d)) {
                return Err(fail(format!(
                    "object {r} holds an annotation an edited page no longer has{}",
                    graph
                        .path_to(*r)
                        .map(|p| format!(" (reached by {p})"))
                        .unwrap_or_default()
                )));
            }
            stack.extend(d.values());
        }
    }
    Ok(())
}
