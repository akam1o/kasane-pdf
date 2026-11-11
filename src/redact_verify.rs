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
use crate::redact_usage::{usage, usage_of, Bound, Usage, Use};
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
                    let rest = usage_of(out, &[i], Bound::Under, limits)?;
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
    // Everything a reader may draw, both sides: every appearance state,
    // Hidden annotations, optional content (a narrower set would let an
    // edit of hidden content on another page through).
    let sel: Vec<usize> = walk.iter().copied().collect();
    let a = usage_of(input, &sel, Bound::Complete, limits)?;
    let b = usage_of(out, &sel, Bound::Complete, limits)?;
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
/// (references replaced by what they refer to; a page by its index; the
/// back links of each `/Annots` entry — its `/P`, `/Parent`, `/Popup`,
/// `/IRT` — left out there and nowhere else), so the same in an output
/// that renumbered its objects. `None` when the graph has a cycle or
/// is too large to digest.
struct Signatures<'a> {
    doc: &'a Document,
    /// Every page object, with its index.
    pages: HashMap<IndirectRef, usize>,
    memo: HashMap<IndirectRef, Option<[u8; 32]>>,
    open: HashSet<IndirectRef>,
    work: u64,
}

/// The keys of an annotation (an entry of a page's `/Annots`) that
/// link back to its page, field or other annotations; the signature
/// leaves them out there, and only there.
const ANNOT_BACK_LINKS: &[&[u8]] = &[b"P", b"Parent", b"Popup", b"IRT"];

impl<'a> Signatures<'a> {
    fn new(doc: &'a Document) -> Self {
        Signatures {
            doc,
            pages: doc
                .pages()
                .iter()
                .enumerate()
                .filter_map(|(i, p)| p.object_ref().map(|r| (r, i)))
                .collect(),
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
        // The annotations: each entry's own dictionary without its back
        // links, its values digested as anywhere else.
        parts.extend_from_slice(b"Annots");
        match pd.get(b"Annots".as_slice()) {
            None => parts.push(0),
            Some(a) => {
                let a = self.doc.deref(a)?;
                let Some(a) = a.as_array() else {
                    return Ok(None);
                };
                parts.extend_from_slice(&(a.len() as u64).to_be_bytes());
                for e in a {
                    let d = self.doc.deref(e)?;
                    let Some(d) = d.as_dict() else {
                        return Ok(None);
                    };
                    let mut keys: Vec<&Vec<u8>> = d
                        .keys()
                        .filter(|k| !ANNOT_BACK_LINKS.contains(&k.as_slice()))
                        .collect();
                    keys.sort();
                    parts.push(b'<');
                    for k in keys {
                        parts.extend_from_slice(&(k.len() as u64).to_be_bytes());
                        parts.extend_from_slice(k);
                        match self.value(&d[k.as_slice()], limits)? {
                            Some(x) => parts.extend_from_slice(&x),
                            None => return Ok(None),
                        }
                    }
                    parts.push(b'>');
                }
            }
        }
        for k in [b"Contents".as_slice(), b"Thumb", b"Group"] {
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
            // A page (a destination's, an annotation's `/P` deeper down)
            // by its place in the page tree, the same in both files.
            if let Some(i) = self.pages.get(r) {
                let mut b = b"page".to_vec();
                b.extend_from_slice(&(*i as u64).to_be_bytes());
                return Ok(Some(sha256(&b)));
            }
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
                let mut keys: Vec<&Vec<u8>> = d.keys().collect();
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

/// The annotations the output's pages hold, by identity: the
/// references in their `/Annots`, and the objects holding direct ones
/// (a page whose `/Annots` is a direct array, an `/Annots` array object).
pub(crate) struct AnnotEntries {
    refs: HashSet<IndirectRef>,
    holders: HashSet<IndirectRef>,
}

impl AnnotEntries {
    pub(crate) fn of(out: &Document) -> Result<AnnotEntries, Error> {
        let mut e = AnnotEntries {
            refs: HashSet::new(),
            holders: HashSet::new(),
        };
        for page in out.pages() {
            let Some(pr) = page.object_ref() else {
                continue;
            };
            let Object::Dict(pd) = out.resolve(pr)? else {
                continue;
            };
            let arr = match pd.get(b"Annots".as_slice()) {
                Some(Object::Ref(ar)) => {
                    e.holders.insert(*ar);
                    out.resolve(*ar)?
                }
                Some(a) => {
                    e.holders.insert(pr);
                    a.clone()
                }
                None => continue,
            };
            for x in arr.as_array().unwrap_or(&[]) {
                if let Object::Ref(r) = x {
                    e.refs.insert(*r);
                }
            }
        }
        Ok(e)
    }

    /// The first dictionary in object `r` (`o`), a stream's included, at
    /// any depth, for which `hit` holds — leaving out the dictionaries
    /// that are themselves entries of a page's `/Annots` (what a page
    /// still holds is not a copy, whatever its content), not their
    /// values.
    pub(crate) fn find(
        &self,
        r: IndirectRef,
        o: &Object,
        hit: &dyn Fn(&crate::object::Dict) -> bool,
    ) -> bool {
        // (value, whether it is an /Annots entry, whether it is the
        // /Annots array of the holder)
        let mut stack: Vec<(&Object, bool, bool)> =
            vec![(o, self.refs.contains(&r), self.holders.contains(&r))];
        let top = self.holders.contains(&r);
        let mut first = true;
        while let Some((o, entry, annots_array)) = stack.pop() {
            match o {
                Object::Array(a) => {
                    for v in a {
                        stack.push((v, annots_array, false));
                    }
                }
                Object::Dict(d) | Object::Stream { dict: d, .. } => {
                    if !entry && hit(d) {
                        return true;
                    }
                    // A page holding a direct /Annots array.
                    let page_holder = first && top && d.contains_key(b"Annots".as_slice());
                    for (k, v) in d {
                        let annots = page_holder && k.as_slice() == b"Annots";
                        stack.push((v, false, annots));
                    }
                }
                _ => {}
            }
            first = false;
        }
        false
    }
}

/// No annotation an edited page had in the input and does not have in
/// the output is still in the output, as any dictionary or a stream's
/// dictionary, whatever refers to it. The removed annotations are found
/// by comparing each edited page's annotations (prints, counted: of two
/// alike, one removed is removed), not from the edit's record; what the
/// output's pages hold is excused by identity only.
pub(crate) fn check_removed_annotations(
    input: &Document,
    out: &Document,
    idx: &Index,
    graph: &RefGraph,
    edited: &BTreeSet<usize>,
) -> Result<(), Error> {
    let prints = |doc: &Document, i: usize| -> Result<HashMap<[u8; 32], usize>, Error> {
        let mut set = HashMap::new();
        let Some(page) = doc.pages().get(i) else {
            return Ok(set);
        };
        if let Some(a) = crate::redact_diag::page_dict_entry(doc, page, b"Annots")? {
            for x in doc.deref(&a)?.as_array().unwrap_or(&[]) {
                if let Some(d) = doc.deref(x)?.as_dict() {
                    *set.entry(annotation_print(d)).or_insert(0) += 1;
                }
            }
        }
        Ok(set)
    };
    let mut removed: HashSet<[u8; 32]> = HashSet::new();
    for &i in edited {
        let kept = prints(out, i)?;
        for (p, n) in prints(input, i)? {
            if kept.get(&p).copied().unwrap_or(0) < n {
                removed.insert(p);
            }
        }
    }
    if removed.is_empty() {
        return Ok(());
    }
    let entries = AnnotEntries::of(out)?;
    for (r, o) in &idx.objects {
        if entries.find(*r, o, &|d| removed.contains(&annotation_print(d))) {
            return Err(fail(format!(
                "object {r} holds an annotation an edited page no longer has{}",
                graph
                    .path_to(*r)
                    .map(|p| format!(" (reached by {p})"))
                    .unwrap_or_default()
            )));
        }
    }
    Ok(())
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

    fn streamd(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    }

    fn check_pair(a: &[u8], b: &[u8]) -> Result<(), Error> {
        let l = Limits::default();
        let a = Document::from_bytes(a).unwrap();
        let b = Document::from_bytes(b).unwrap();
        let idx = Index::build(&b, &l)?;
        let graph = RefGraph::build(&b, &idx, &l)?;
        check_use(&a, &b, &idx, &graph, &[0].into(), &l)
    }

    /// Non-interference compares all a reader may draw on a page not
    /// edited: content under optional content, an XObject with `/OC`, a
    /// rollover appearance. (Review.)
    #[test]
    fn non_interference_compares_everything_drawn() {
        for kind in ["xobject", "bdc", "rollover"] {
            let make = |body: &[u8]| {
                let (content, oc, resources, annots): (&[u8], &str, &str, &str) = match kind {
                    "xobject" => (b"/P Do", "/OC 8 0 R", "/XObject << /P 7 0 R >>", ""),
                    "bdc" => (
                        b"/OC /L BDC /P Do EMC",
                        "",
                        "/XObject << /P 7 0 R >> /Properties << /L 8 0 R >>",
                        "",
                    ),
                    _ => (b"", "", "", "/Annots [9 0 R]"),
                };
                let p2 = format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << {resources} >> {annots} >>"
                );
                let fm = streamd(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << >> {oc}"
                    ),
                    body,
                );
                let mut objs: Vec<(u32, Vec<u8>)> = vec![
                    (1, b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [8 0 R] /D << /BaseState /ON >> >> >>".to_vec()),
                    (2, PAGES.to_vec()),
                    (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec()),
                    (4, stream("0 0 1 1 re f")),
                    (5, p2.into_bytes()),
                    (6, streamd("", content)),
                    (7, fm),
                    (8, b"<< /Type /OCG /Name (layer) >>".to_vec()),
                ];
                if kind == "rollover" {
                    objs.push((9, b"<< /Type /Annot /Subtype /Stamp /Rect [0 0 100 100] /AP << /N 10 0 R /R 7 0 R >> >>".to_vec()));
                    objs.push((
                        10,
                        streamd(
                            "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << >>",
                            b"",
                        ),
                    ));
                }
                let refs: Vec<(u32, &[u8])> =
                    objs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
                pdf(&refs)
            };
            let e = check_pair(&make(b"0 0 10 10 re f"), &make(b"50 50 10 10 re f"))
                .unwrap_err()
                .to_string();
            assert!(e.contains("page 2 (not edited)"), "{kind}: {e}");
        }
    }

    /// The page signature leaves out back links only in `/Annots`
    /// entries: resources named `/P` and `/S` are compared. (Review.)
    #[test]
    fn signature_back_links_by_context() {
        let make = |body: &[u8]| {
            let objs: Vec<(u32, Vec<u8>)> = vec![
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, PAGES.to_vec()),
                (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec()),
                (4, stream("0 0 1 1 re f")),
                (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 6 0 R /Resources << /XObject << /P 7 0 R /S 8 0 R >> >> >>".to_vec()),
                (6, stream("/P Do")),
                (7, streamd("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << >>", body)),
                (8, streamd("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << >>", b"")),
            ];
            let refs: Vec<(u32, &[u8])> = objs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
            pdf(&refs)
        };
        let e = check_pair(&make(b"0 0 10 10 re f"), &make(b"50 50 10 10 re f"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("page 2 (not edited)"), "{e}");
    }
}
