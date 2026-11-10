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
    /// The streams, in object order, with their digests.
    pub(crate) fn streams(&self) -> impl Iterator<Item = (IndirectRef, StreamDigests)> + '_ {
        self.objects
            .iter()
            .filter_map(|(r, _)| self.digests.get(r).map(|d| (*r, *d)))
    }

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
        // An empty or whitespace-only content stream carries nothing;
        // the same bytes as samples, a palette or a function do.
        if d.blank && roles.iter().all(|r| r.content_language()) {
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
            .filter_map(|(r, d)| {
                d.each()
                    .find(|x| wanted.contains_key(x))
                    .map(|x| (*r, x, d.blank))
            })
            .filter(|(_, x, blank)| !(*blank && wanted[x].iter().all(|u| u.content_language())))
            .map(|(r, x, _)| (r, x))
            .collect();
        if !held.is_empty() {
            let mut live: HashSet<[u8; 32]> = HashSet::new();
            // Excused only by a use that pictures or spells content too:
            // the same bytes as some font's `/ToUnicode` excuse nothing.
            let content = |m: HashMap<[u8; 32], BTreeSet<Use>>| {
                m.into_iter()
                    .filter(|(_, roles)| roles.iter().any(|r| r.content_bearing()))
                    .map(|(x, _)| x)
                    .collect::<Vec<_>>()
            };
            let after = usage(out, &is_edited, Bound::Under, limits)?;
            live.extend(content(used_digests(out, &after, Some(idx))?));
            let mut left: Vec<&(IndirectRef, [u8; 32])> =
                held.iter().filter(|(_, x)| !live.contains(x)).collect();
            if !left.is_empty() {
                // Page by page, until every copy is found used.
                for i in (0..out.pages().len()).filter(|i| !is_edited(*i)) {
                    let rest = usage(out, &|p| p == i, Bound::Under, limits)?;
                    live.extend(content(used_digests(out, &rest, Some(idx))?));
                    left.retain(|(_, x)| !live.contains(x));
                    if left.is_empty() {
                        break;
                    }
                }
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

/// Whether `key` of `d` is a back link the page signature leaves out:
/// a page tree node's, an annotation's or a field's `/Parent`, an
/// annotation's `/P`, `/Popup` and `/IRT`, a structure element's `/P`.
/// Decided by what `d` is, never by the key alone (a resource may be
/// named `/P`).
fn back_link(d: &crate::object::Dict, key: &[u8]) -> bool {
    let has = |k: &[u8]| d.contains_key(k);
    let is_type = |t: &[u8]| d.get(b"Type".as_slice()).and_then(|v| v.as_name()) == Some(t);
    let annotation = is_type(b"Annot") || (has(b"Subtype") && has(b"Rect"));
    let node = is_type(b"Page") || is_type(b"Pages");
    let field = has(b"FT") || (has(b"T") && has(b"Kids"));
    let element = is_type(b"StructElem") || (has(b"S") && has(b"P") && !annotation);
    match key {
        b"Parent" => node || annotation || field,
        b"P" => annotation || element,
        b"Popup" | b"IRT" => annotation,
        _ => false,
    }
}

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

    /// The digest of `o`: references are digested once (memoized) and
    /// written as their digest; direct values are written inline.
    fn value(&mut self, o: &Object, limits: &Limits) -> Result<Option<[u8; 32]>, Error> {
        if let Object::Ref(r) = o {
            if let Some(d) = self.memo.get(r) {
                return Ok(*d);
            }
            // A cycle, or too deep a chain: no signature (the page is
            // walked).
            if self.open.len() >= 256 || !self.open.insert(*r) {
                return Ok(None);
            }
            let t = self.doc.resolve(*r)?;
            let mut buf = Vec::new();
            let ok = self.write(&t, &mut buf, limits)?;
            let d = ok.then(|| sha256(&buf));
            self.open.remove(r);
            self.memo.insert(*r, d);
            return Ok(d);
        }
        let mut buf = Vec::new();
        Ok(self.write(o, &mut buf, limits)?.then(|| sha256(&buf)))
    }

    /// Write `o` canonically into `out`; `false` on a cycle or too much
    /// work.
    fn write(&mut self, o: &Object, out: &mut Vec<u8>, limits: &Limits) -> Result<bool, Error> {
        self.work += 1;
        if self.work > limits.max_audit_elements {
            return Ok(false);
        }
        match o {
            Object::Ref(_) => match self.value(o, limits)? {
                Some(d) => {
                    out.push(b'R');
                    out.extend_from_slice(&d);
                }
                None => return Ok(false),
            },
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                if let Object::Stream { data, .. } = o {
                    out.push(b'S');
                    out.extend_from_slice(&sha256(data));
                }
                out.push(b'<');
                let mut keys: Vec<&Vec<u8>> = d.keys().filter(|k| !back_link(d, k)).collect();
                keys.sort();
                for k in keys {
                    out.extend_from_slice(&(k.len() as u64).to_be_bytes());
                    out.extend_from_slice(k);
                    if !self.write(&d[k], out, limits)? {
                        return Ok(false);
                    }
                }
                out.push(b'>');
            }
            Object::Array(a) => {
                out.push(b'[');
                out.extend_from_slice(&(a.len() as u64).to_be_bytes());
                for v in a {
                    if !self.write(v, out, limits)? {
                        return Ok(false);
                    }
                }
            }
            Object::Integer(i) => {
                out.push(b'i');
                out.extend_from_slice(&i.to_be_bytes());
            }
            Object::Real(f) => {
                out.push(b'f');
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            Object::Name(n) => {
                out.push(b'n');
                out.extend_from_slice(&(n.len() as u64).to_be_bytes());
                out.extend_from_slice(n);
            }
            Object::String(t) => {
                out.push(b's');
                out.extend_from_slice(&(t.len() as u64).to_be_bytes());
                out.extend_from_slice(t);
            }
            other => {
                out.push(b'.');
                out.extend_from_slice(format!("{other:?}").as_bytes());
            }
        }
        Ok(true)
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
        b"FFilter",
        b"FDecodeParms",
    ] {
        d.remove(k);
    }
    // A stream's file specification, not an annotation's flags.
    if !matches!(d.get(b"F".as_slice()), Some(Object::Integer(_)) | None) {
        d.remove(b"F".as_slice());
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
    // What the output's pages still show is not removed, wherever an
    // identical annotation was (the same footer link on every page).
    let mut kept: HashSet<[u8; 32]> = HashSet::new();
    for i in 0..out.pages().len() {
        kept.extend(prints(out, i)?);
    }
    let mut removed: HashSet<[u8; 32]> = HashSet::new();
    for &i in edited {
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

/// The prints ([`crate::remove_region::annotation_print`]) of every
/// annotation the output's pages hold: an identical removed annotation's
/// print excuses nothing more than these show.
pub(crate) fn shown_annotation_prints(out: &Document) -> Result<HashSet<[u8; 32]>, Error> {
    let mut set = HashSet::new();
    for page in out.pages() {
        if let Some(a) = crate::redact_diag::page_dict_entry(out, page, b"Annots")? {
            for x in out.deref(&a)?.as_array().unwrap_or(&[]) {
                if let Some(d) = out.deref(x)?.as_dict() {
                    set.insert(crate::remove_region::annotation_print(d));
                }
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(objs: &[(u32, &[u8])]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = std::collections::BTreeMap::new();
        for (n, b) in objs {
            offs.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(b);
            out.extend_from_slice(b"\nendobj\n");
        }
        let size = offs.keys().max().unwrap() + 1;
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for i in 1..size {
            match offs.get(&i) {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
        );
        out
    }

    fn stream(data: &str) -> Vec<u8> {
        format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len()).into_bytes()
    }

    const PAGES: &[u8] = b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>";
    const P1: &[u8] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>";
    const P2: &[u8] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R \
        /Annots [7 0 R] >>";
    const NOTE: &[u8] = b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (note) >>";

    fn input() -> Vec<u8> {
        pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, PAGES),
            (3, P1),
            (4, &stream("0 0 10 10 re f 50 50 10 10 re f")),
            (5, P2),
            (6, &stream("20 20 5 5 re f")),
            (7, NOTE),
        ])
    }

    fn check(out: &[u8]) -> Result<(), Error> {
        let l = Limits::default();
        let a = Document::from_bytes(&input()).unwrap();
        let b = Document::from_bytes(out).unwrap();
        let idx = Index::build(&b, &l)?;
        let graph = RefGraph::build(&b, &idx, &l)?;
        check_reachable(&idx, &graph)?;
        let edited: BTreeSet<usize> = [0].into();
        check_use(&a, &b, &idx, &graph, &edited, &l)?;
        check_removed_annotations(&a, &b, &idx, &graph, &edited)
    }

    /// The invariants alone — no record of the edit — accept a clean
    /// output and fail each way removed data stayed in past versions.
    #[test]
    fn invariants_without_the_edits_record() {
        let new4 = stream("0 0 10 10 re f");
        let old4 = stream("0 0 10 10 re f 50 50 10 10 re f");
        let clean = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, PAGES),
            (3, P1),
            (4, &new4),
            (5, P2),
            (6, &stream("20 20 5 5 re f")),
            (7, NOTE),
        ]);
        check(&clean).unwrap();
        // The original content kept through a private catalog entry.
        let aliased = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /XPriv 8 0 R >>"),
            (2, PAGES),
            (3, P1),
            (4, &new4),
            (5, P2),
            (6, &stream("20 20 5 5 re f")),
            (7, NOTE),
            (8, &old4),
        ]);
        let e = check(&aliased).unwrap_err().to_string();
        assert!(
            e.contains("object 8 0 R") && e.contains("/Root/XPriv"),
            "{e}"
        );
        // A page not edited drawing differently.
        let changed = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, PAGES),
            (3, P1),
            (4, &new4),
            (5, P2),
            (6, &stream("20 20 6 6 re f")),
            (7, NOTE),
        ]);
        let e = check(&changed).unwrap_err().to_string();
        assert!(e.contains("page 2 (not edited)"), "{e}");
        // An object nothing refers to.
        let orphan = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, PAGES),
            (3, P1),
            (4, &new4),
            (5, P2),
            (6, &stream("20 20 5 5 re f")),
            (7, NOTE),
            (9, b"<< /Anything 1 >>"),
        ]);
        let e = check(&orphan).unwrap_err().to_string();
        assert!(e.contains("object 9 0 R"), "{e}");
    }

    /// An annotation the edited page lost, kept as a stream's dictionary
    /// (whatever its stream keys), fails.
    #[test]
    fn a_removed_annotation_as_a_stream_dictionary() {
        let annotated = |p1: &[u8], extra: &[(u32, &[u8])]| {
            let mut objs: Vec<(u32, &[u8])> = vec![
                (1, b"<< /Type /Catalog /Pages 2 0 R /XPriv 9 0 R >>"),
                (2, PAGES),
                (3, p1),
                (5, P2),
                (7, NOTE),
            ];
            objs.extend_from_slice(extra);
            pdf(&objs)
        };
        let l = Limits::default();
        let p1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [8 0 R] >>";
        let annot = b"<< /Type /Annot /Subtype /Text /Rect [5 5 9 9] /Contents (removed) >>";
        let a = annotated(
            p1,
            &[(8, annot), (9, b"<< >>"), (6, &stream("20 20 5 5 re f"))],
        );
        let copy = b"<< /Type /Annot /Subtype /Text /Rect [5 5 9 9] /Contents (removed) /Length 3 \
            /Filter /ASCIIHexDecode >>\nstream\n00>\nendstream";
        let b = annotated(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
            &[(9, copy), (6, &stream("20 20 5 5 re f"))],
        );
        let a = Document::from_bytes(&a).unwrap();
        let b = Document::from_bytes(&b).unwrap();
        let idx = Index::build(&b, &l).unwrap();
        let graph = RefGraph::build(&b, &idx, &l).unwrap();
        let e = check_removed_annotations(&a, &b, &idx, &graph, &[0].into())
            .unwrap_err()
            .to_string();
        assert!(e.contains("object 9 0 R"), "{e}");
    }
}
