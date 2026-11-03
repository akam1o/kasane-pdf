//! Usage-rights signature removal (`UsageRightsPolicy::Remove`, D1;
//! `docs/plans/rewrite-policy.md` §5).
//!
//! [`scan`] walks the whole reachable graph — from the trailer's `/Root`
//! and `/Info` and every page, through every reference including `/P`
//! and `/Parent`, every link of a reference chain included (an
//! object-stream member that is itself a reference too) — and records
//! every *signature-like* dictionary (§5.3.2: `/ByteRange`; `/Type /Sig`
//! or `/DocTimeStamp`; `/Contents` with `/Filter` or `/SubFilter`;
//! `/Reference` with `/TransformMethod`; `/Cert` with `/Contents`), every
//! field lock (`/Lock`, wherever the dictionary is — a widget outside
//! `/AcroForm /Fields` included — except on a seed-value dictionary
//! itself, §5.3 rule 5), the seed-value dictionaries, and how many
//! references point at each object. An unreadable object or a walk past
//! its budget refuses: the walk is never cut short.
//!
//! [`assess`] decides, from that walk and the catalog alone, whether the
//! removal is allowed and what it edits ([`Plan`]); anything it cannot
//! establish refuses. Handler names never decide: only structure does.

// Phase A (C2): the walk and the assessment, wired into `rewrite_with`
// in phase B.
#![cfg_attr(not(test), allow(dead_code))]

use crate::document::Document;
use crate::error::{Error, SignatureRule};
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::rewrite::deref_chain;
use std::collections::{HashMap, HashSet};

/// Where a dictionary is: the nearest enclosing indirect object (none: a
/// direct value of the trailer) and the keys / indices from it (`[]` for
/// the object's own body).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Origin {
    pub obj: Option<IndirectRef>,
    pub path: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Step {
    Key(Vec<u8>),
    Index(usize),
}

/// Where [`Scan`] keeps the first hits; the counts are complete.
const KEPT: usize = 3;

/// What [`scan`] found. Content-free.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Signature-like dictionaries met (a dictionary reached in two
    /// roles, see [`scan`], counts twice).
    pub signature_like: usize,
    /// Where the first [`KEPT`] of them are.
    pub signature_like_at: Vec<Origin>,
    /// Signature-like dictionaries with a `/ByteRange` key.
    pub byte_ranges: usize,
    /// Dictionaries with a `/Lock` key (not counting a seed-value
    /// dictionary's own), and where the first [`KEPT`] are.
    pub locks: usize,
    pub lock_at: Vec<Origin>,
    /// Seed-value dictionaries (`/SV` values).
    pub seed_values: usize,
    /// References to each object met (every edge, back-pointers and
    /// chain links too; not the page-list roots).
    pub refs: HashMap<IndirectRef, u32>,
    /// Values examined.
    pub values: usize,
}

fn not_removable(detail: impl Into<String>) -> Error {
    Error::SignaturePolicy {
        rule: SignatureRule::UsageRightsNotRemovable,
        detail: detail.into(),
    }
}

/// A read failure while searching: the search is incomplete, so the
/// removal refuses (content-free: the error kind only).
fn unreadable(what: &str, e: &Error) -> Error {
    not_removable(format!(
        "{what} could not be read to search for signatures ({})",
        e.kind().as_str()
    ))
}

fn object_unreadable(r: IndirectRef, e: &Error) -> Error {
    unreadable(&format!("object {} {}", r.number, r.generation), e)
}

/// How far [`Markers::transform`] follows `/Reference` (nested arrays,
/// references); deeper is taken as signature-like.
const MAX_REFERENCE_DEPTH: usize = 8;

/// The signature-marker test (§5.3.2), reading values through their
/// references once per reference.
#[derive(Default)]
struct Markers {
    names: HashMap<IndirectRef, Option<Vec<u8>>>,
    transforms: HashMap<IndirectRef, bool>,
}

impl Markers {
    fn signature_like(&mut self, doc: &Document, d: &Dict) -> Result<bool, Error> {
        let has = |k: &[u8]| d.contains_key(k);
        if has(b"ByteRange")
            || (has(b"Contents") && (has(b"Filter") || has(b"SubFilter")))
            || (has(b"Cert") && has(b"Contents"))
        {
            return Ok(true);
        }
        if let Some(t) = d.get(b"Type".as_slice()) {
            if let Some(n) = self.name(doc, t)? {
                if n == b"Sig" || n == b"DocTimeStamp" {
                    return Ok(true);
                }
            }
        }
        match d.get(b"Reference".as_slice()) {
            Some(r) => self.transform(doc, r, 0),
            None => Ok(false),
        }
    }

    fn name(&mut self, doc: &Document, o: &Object) -> Result<Option<Vec<u8>>, Error> {
        match o {
            Object::Name(n) => Ok(Some(n.clone())),
            Object::Ref(r) => {
                if let Some(n) = self.names.get(r) {
                    return Ok(n.clone());
                }
                let n = match deref_chain(doc, o).map_err(|e| object_unreadable(*r, &e))? {
                    Object::Name(n) => Some(n),
                    _ => None,
                };
                self.names.insert(*r, n.clone());
                Ok(n)
            }
            _ => Ok(None),
        }
    }

    /// Whether a `/Reference` value holds a dictionary with
    /// `/TransformMethod` (an array of them, one alone, nested arrays).
    fn transform(&mut self, doc: &Document, o: &Object, depth: usize) -> Result<bool, Error> {
        if depth > MAX_REFERENCE_DEPTH {
            return Ok(true);
        }
        match o {
            Object::Ref(r) => {
                if let Some(t) = self.transforms.get(r) {
                    return Ok(*t);
                }
                let v = deref_chain(doc, o).map_err(|e| object_unreadable(*r, &e))?;
                let t = self.transform(doc, &v, depth + 1)?;
                self.transforms.insert(*r, t);
                Ok(t)
            }
            Object::Array(a) => {
                for e in a {
                    if self.transform(doc, e, depth + 1)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                Ok(d.contains_key(b"TransformMethod".as_slice()))
            }
            _ => Ok(false),
        }
    }
}

/// A node of the walk: its parent and how it is reached from it.
enum Link {
    Obj(IndirectRef),
    Step(Step),
}

struct Entry {
    value: Object,
    node: usize,
    /// The edge that led here counts as a reference (not a page-list
    /// root).
    counted: bool,
    /// The value of an `/SV` key.
    sv: bool,
}

fn origin(nodes: &[(Option<usize>, Link)], mut n: usize) -> Origin {
    let mut path = Vec::new();
    let obj = loop {
        let (parent, link) = &nodes[n];
        match link {
            Link::Obj(r) => break Some(*r),
            Link::Step(s) => path.push(s.clone()),
        }
        match parent {
            Some(p) => n = *p,
            None => break None,
        }
    };
    path.reverse();
    Origin { obj, path }
}

fn container(o: &Object) -> bool {
    matches!(
        o,
        Object::Ref(_) | Object::Array(_) | Object::Dict(_) | Object::Stream { .. }
    )
}

/// The walk budget: `max_output_objects` × 64 values.
fn budget(limits: &Limits) -> usize {
    limits.max_output_objects.saturating_mul(64)
}

fn over_budget(budget: usize) -> Error {
    not_removable(format!(
        "the document could not be searched for signatures completely (more than {budget} values)"
    ))
}

/// Walk `doc`'s reachable graph (module docs). Fails — refusing the
/// removal — on an unreadable object or past the walk budget.
///
/// An object is walked once per role: as a seed-value dictionary (the
/// value of `/SV`, whose own `/Lock` is carried) and otherwise. One
/// reached in both roles is walked twice, so what it holds is counted
/// twice — a reference count only errs high, towards refusing.
pub(crate) fn scan(doc: &Document, limits: &Limits) -> Result<Scan, Error> {
    let budget = budget(limits);
    let mut s = Scan::default();
    let mut markers = Markers::default();
    let mut seen: HashSet<(IndirectRef, bool)> = HashSet::new();
    let mut nodes: Vec<(Option<usize>, Link)> = Vec::new();
    let mut stack: Vec<Entry> = Vec::new();
    for k in [b"Root".as_slice(), b"Info".as_slice()] {
        if let Some(v) = doc.trailer().get(k) {
            nodes.push((None, Link::Step(Step::Key(k.to_vec()))));
            stack.push(Entry {
                value: v.clone(),
                node: nodes.len() - 1,
                counted: true,
                sv: false,
            });
        }
    }
    // Every page the document lists, should one not be reached from
    // `/Root`; these roots are not references, so not counted.
    for p in doc.pages() {
        if let Some(r) = p.object_ref() {
            nodes.push((None, Link::Step(Step::Key(b"Page".to_vec()))));
            stack.push(Entry {
                value: Object::Ref(r),
                node: nodes.len() - 1,
                counted: false,
                sv: false,
            });
        }
    }
    s.values = stack.len();
    while let Some(Entry {
        value,
        node,
        counted,
        sv,
    }) = stack.pop()
    {
        let (value, node) = match value {
            Object::Ref(r) => {
                if counted {
                    *s.refs.entry(r).or_default() += 1;
                }
                if !seen.insert((r, sv)) {
                    continue;
                }
                let v = doc
                    .resolve_one_hop(r)
                    .map_err(|e| object_unreadable(r, &e))?;
                nodes.push((Some(node), Link::Obj(r)));
                let node = nodes.len() - 1;
                if let Object::Ref(_) = v {
                    // A chain link (or an object-stream member that is a
                    // reference): the writers write it, then what it
                    // names — counted and walked as any other edge.
                    s.values += 1;
                    if s.values > budget {
                        return Err(over_budget(budget));
                    }
                    stack.push(Entry {
                        value: v,
                        node,
                        counted: true,
                        sv,
                    });
                    continue;
                }
                (v, node)
            }
            v => (v, node),
        };
        let d = match value {
            Object::Dict(d) => d,
            // A stream's `/Length` is storage; the rewrite does not
            // follow it either.
            Object::Stream { mut dict, .. } => {
                dict.remove(b"Length".as_slice());
                dict
            }
            Object::Array(a) => {
                s.values += a.len();
                if s.values > budget {
                    return Err(over_budget(budget));
                }
                for (i, x) in a.into_iter().enumerate().rev() {
                    if container(&x) {
                        nodes.push((Some(node), Link::Step(Step::Index(i))));
                        stack.push(Entry {
                            value: x,
                            node: nodes.len() - 1,
                            counted: true,
                            sv: false,
                        });
                    }
                }
                continue;
            }
            _ => continue,
        };
        if markers.signature_like(doc, &d)? {
            s.signature_like += 1;
            if d.contains_key(b"ByteRange".as_slice()) {
                s.byte_ranges += 1;
            }
            if s.signature_like_at.len() < KEPT {
                s.signature_like_at.push(origin(&nodes, node));
            }
        }
        if sv {
            s.seed_values += 1;
        } else if d.contains_key(b"Lock".as_slice()) {
            s.locks += 1;
            if s.lock_at.len() < KEPT {
                s.lock_at.push(origin(&nodes, node));
            }
        }
        s.values += d.len();
        if s.values > budget {
            return Err(over_budget(budget));
        }
        for (k, v) in d.into_iter().rev() {
            if container(&v) {
                let sv = k.as_slice() == b"SV";
                nodes.push((Some(node), Link::Step(Step::Key(k))));
                stack.push(Entry {
                    value: v,
                    node: nodes.len() - 1,
                    counted: true,
                    sv,
                });
            }
        }
    }
    Ok(s)
}

/// `/UR` or `/UR3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Ur,
    Ur3,
}

impl Key {
    pub fn name(self) -> &'static [u8] {
        match self {
            Key::Ur => b"UR",
            Key::Ur3 => b"UR3",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Key::Ur => "/UR",
            Key::Ur3 => "/UR3",
        }
    }
}

/// A removal target as read.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub key: Key,
    pub object: Option<IndirectRef>,
    pub dict: Dict,
}

/// Where `/Perms` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermsAt {
    /// A direct dictionary in the catalog.
    Catalog,
    /// An indirect object referenced only from the catalog.
    Object(IndirectRef),
}

/// Where `/SigFlags` is, for the bit-1 edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SigFlagsAt {
    /// In a direct `/AcroForm` dictionary of the catalog.
    CatalogAcroForm,
    /// In an `/AcroForm` object referenced only from the catalog.
    AcroFormObject(IndirectRef),
    /// An integer object referenced only from the `/AcroForm`.
    Object(IndirectRef),
}

/// What the removal edits.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub targets: Vec<Target>,
    pub perms: PermsAt,
    /// `/Perms` keys left after the removal (none: `/Perms` is dropped).
    pub perms_left: usize,
    /// `(before, after, where)` when bit 1 is set.
    pub sig_flags: Option<(i64, i64, SigFlagsAt)>,
    pub seed_values: usize,
}

/// The value `o` stands for and the reference it was read through, one
/// hop only. A chain (the object is itself a reference) refuses: the
/// rewrite would write the far end's body under each link's number, so
/// an edit to one link would leave unedited copies.
fn one_hop(doc: &Document, o: &Object, what: &str) -> Result<(Option<IndirectRef>, Object), Error> {
    match o {
        Object::Ref(r) => match doc.resolve_one_hop(*r).map_err(|e| unreadable(what, &e))? {
            Object::Ref(_) => Err(not_removable(format!(
                "{what} is a chain of references, so it cannot be edited"
            ))),
            v => Ok((Some(*r), v)),
        },
        v => Ok((None, v.clone())),
    }
}

/// Whether `/UR` and `/UR3` can be removed from `doc`, and how; `Ok(None)`
/// when `/Perms` has neither (and nothing else signature-like is
/// present). Every condition of `rewrite-policy.md` §5.3 that it cannot
/// establish refuses. Leaves `doc` as it found it (reference budget,
/// warnings, object-stream cache).
pub(crate) fn assess(doc: &Document, limits: &Limits) -> Result<Option<Plan>, Error> {
    doc.without_side_effects(|| assess_inner(doc, limits))
}

fn assess_inner(doc: &Document, limits: &Limits) -> Result<Option<Plan>, Error> {
    let root = doc
        .trailer()
        .get(b"Root".as_slice())
        .and_then(|o| o.as_ref())
        .ok_or_else(|| not_removable("the catalog is not an indirect object"))?;
    let cat = match one_hop(doc, &Object::Ref(root), "the catalog")?.1 {
        Object::Dict(d) => d,
        _ => return Err(not_removable("the catalog is not a dictionary")),
    };
    // Security stores (§5.6, option 1). One that cannot be read is
    // taken as present.
    for k in [b"DSS".as_slice(), b"Legal".as_slice()] {
        if let Some(v) = cat.get(k) {
            if !matches!(deref_chain(doc, v), Ok(Object::Null)) {
                return Err(Error::SignaturePolicy {
                    rule: SignatureRule::SecurityStore,
                    detail: format!(
                        "the catalog has /{} — a signature workflow the removal cannot reason about",
                        String::from_utf8_lossy(k)
                    ),
                });
            }
        }
    }
    let scan = scan(doc, limits)?;
    let refs = |r: &IndirectRef| scan.refs.get(r).copied().unwrap_or(0);

    // /Perms: where, and what it holds.
    let (perms_at, perms) = match cat.get(b"Perms".as_slice()) {
        None => (None, Dict::new()),
        Some(p) => match one_hop(doc, p, "/Perms")? {
            (_, Object::Null) => (None, Dict::new()),
            (Some(r), Object::Dict(d)) => (Some(PermsAt::Object(r)), d),
            (None, Object::Dict(d)) => (Some(PermsAt::Catalog), d),
            _ => return Err(not_removable("/Perms is not a dictionary")),
        },
    };
    let mut targets = Vec::new();
    for key in [Key::Ur, Key::Ur3] {
        let Some(v) = perms.get(key.name()) else {
            continue;
        };
        let what = format!("/Perms {}", key.label());
        let (object, dict) = match one_hop(doc, v, &what)? {
            (o, Object::Dict(d)) => (o, d),
            // A null (or undefined) entry names no signature, but the
            // key would stay in the output: refuse rather than guess.
            (_, Object::Null) => return Err(not_removable(format!("{what} is null"))),
            _ => return Err(not_removable(format!("{what} is not a dictionary"))),
        };
        check_target(doc, key, &dict)?;
        targets.push(Target { key, object, dict });
    }

    // Every signature-like dictionary must be a target itself.
    let target_origin = |t: &Target| -> Origin {
        match (t.object, perms_at) {
            (Some(r), _) => Origin {
                obj: Some(r),
                path: vec![],
            },
            (None, Some(PermsAt::Object(p))) => Origin {
                obj: Some(p),
                path: vec![Step::Key(t.key.name().to_vec())],
            },
            (None, _) => Origin {
                obj: Some(root),
                path: vec![
                    Step::Key(b"Perms".to_vec()),
                    Step::Key(t.key.name().to_vec()),
                ],
            },
        }
    };
    let allowed: Vec<Origin> = targets.iter().map(target_origin).collect();
    if scan.signature_like > allowed.len()
        || scan.signature_like_at.iter().any(|o| !allowed.contains(o))
    {
        let at = match scan.signature_like_at.iter().find(|o| !allowed.contains(o)) {
            Some(o) => describe(o),
            None => format!("{} places", scan.signature_like),
        };
        return Err(Error::SignaturePolicy {
            rule: SignatureRule::OtherSignaturePresent,
            detail: format!(
                "a signature-like dictionary at {at} — only /Perms /UR and /UR3 usage-rights \
                 signatures are removed"
            ),
        });
    }
    if let Some(o) = scan.lock_at.first() {
        return Err(Error::SignaturePolicy {
            rule: SignatureRule::OtherSignaturePresent,
            detail: format!("a field /Lock at {}", describe(o)),
        });
    }
    if targets.is_empty() {
        return Ok(None);
    }
    let perms_at = perms_at.expect("targets come from /Perms");

    // Sharing: each target, everything it holds, and /Perms itself are
    // reachable only from where the removal deletes them.
    for t in &targets {
        if let Some(r) = t.object {
            if refs(&r) != 1 {
                return Err(not_removable(format!(
                    "the {} signature is referenced {} times — it is shared with another \
                     structure",
                    t.key.label(),
                    refs(&r)
                )));
            }
        }
        for (r, n) in held_edges(doc, &t.dict, budget(limits))? {
            // Every reference to it is one from inside the target; any
            // other keeps it in the output.
            if refs(&r) != n {
                return Err(not_removable(format!(
                    "object {} {} inside the {} signature is shared with another structure",
                    r.number,
                    r.generation,
                    t.key.label()
                )));
            }
        }
    }
    if let PermsAt::Object(r) = perms_at {
        if refs(&r) != 1 {
            return Err(not_removable(
                "/Perms is shared with another structure, so it cannot be edited",
            ));
        }
    }
    let perms_left = perms.len() - targets.len();

    // /SigFlags: bit 1 (SignaturesExist) cleared when set — no signature
    // remains — every other bit kept.
    let mut sig_flags = None;
    if let Some(af) = cat.get(b"AcroForm".as_slice()) {
        let (af_at, afd) = match one_hop(doc, af, "/AcroForm")? {
            (_, Object::Null) => (None, None),
            (at, Object::Dict(d)) => (at, Some(d)),
            _ => return Err(not_removable("/AcroForm is not a dictionary")),
        };
        if let Some(f) = afd.as_ref().and_then(|d| d.get(b"SigFlags".as_slice())) {
            let (f_at, value) = match one_hop(doc, f, "/SigFlags")? {
                (at, Object::Integer(v)) => (at, v),
                (_, Object::Null) => (None, 0),
                _ => return Err(not_removable("/AcroForm /SigFlags is not an integer")),
            };
            if value & 1 == 1 {
                let at = match (f_at, af_at) {
                    (Some(r), _) => {
                        if refs(&r) != 1 {
                            return Err(not_removable(
                                "/SigFlags is shared with another structure",
                            ));
                        }
                        SigFlagsAt::Object(r)
                    }
                    (None, Some(r)) => {
                        if refs(&r) != 1 {
                            return Err(not_removable(
                                "/AcroForm is shared with another structure, so /SigFlags \
                                 cannot be edited",
                            ));
                        }
                        SigFlagsAt::AcroFormObject(r)
                    }
                    (None, None) => SigFlagsAt::CatalogAcroForm,
                };
                sig_flags = Some((value, value & !1, at));
            }
        }
    }
    Ok(Some(Plan {
        targets,
        perms: perms_at,
        perms_left,
        sig_flags,
        seed_values: scan.seed_values,
    }))
}

fn name_of(doc: &Document, o: Option<&Object>) -> Option<Vec<u8>> {
    match deref_chain(doc, o?).ok()? {
        Object::Name(n) => Some(n),
        _ => None,
    }
}

/// §5.3.1: a usage-rights signature dictionary — `/Type /Sig` or none,
/// `/Filter` and `/Contents`, and a non-empty `/Reference` array whose
/// every element has `/TransformMethod /UR3` (or `/UR` under `/Perms
/// /UR`). An element without `/TransformMethod` is `/DocMDP` (Table 257).
fn check_target(doc: &Document, key: Key, d: &Dict) -> Result<(), Error> {
    let k = key.label();
    let fail = |why: &str| not_removable(format!("/Perms {k}: {why}"));
    let read = |e: Error| unreadable(&format!("/Perms {k}"), &e);
    match name_of(doc, d.get(b"Type".as_slice())) {
        None if d.contains_key(b"Type".as_slice()) => return Err(fail("/Type is not a name")),
        Some(t) if t != b"Sig" => return Err(fail("/Type is not /Sig")),
        _ => {}
    }
    if !d.contains_key(b"Filter".as_slice()) || !d.contains_key(b"Contents".as_slice()) {
        return Err(fail(
            "not a signature dictionary (/Filter and /Contents required)",
        ));
    }
    let elems = match d.get(b"Reference".as_slice()).map(|r| deref_chain(doc, r)) {
        Some(Ok(Object::Array(a))) if !a.is_empty() => a,
        Some(Err(e)) => return Err(read(e)),
        _ => {
            return Err(fail(
                "no non-empty /Reference array: usage rights cannot be established",
            ))
        }
    };
    for e in &elems {
        let Object::Dict(ed) = deref_chain(doc, e).map_err(read)? else {
            return Err(fail("a /Reference element is not a dictionary"));
        };
        let ok = match name_of(doc, ed.get(b"TransformMethod".as_slice())).as_deref() {
            Some(b"UR3") => true,
            Some(b"UR") => key == Key::Ur,
            None if !ed.contains_key(b"TransformMethod".as_slice()) => {
                return Err(fail(
                    "a /Reference element without /TransformMethod (/DocMDP)",
                ))
            }
            _ => false,
        };
        if !ok {
            return Err(fail("a /Reference element is not a usage-rights transform"));
        }
    }
    Ok(())
}

/// The references inside `body`, through its references (each object
/// once, chain links included, a stream's `/Length` skipped — the edges
/// [`scan`] counts): how many there are to each object.
fn held_edges(
    doc: &Document,
    body: &Dict,
    budget: usize,
) -> Result<HashMap<IndirectRef, u32>, Error> {
    let mut edges: HashMap<IndirectRef, u32> = HashMap::new();
    let mut stack: Vec<Object> = body.values().cloned().collect();
    let mut values = stack.len();
    while let Some(o) = stack.pop() {
        let more: Vec<Object> = match o {
            Object::Ref(r) => {
                let n = edges.entry(r).or_default();
                *n += 1;
                if *n > 1 {
                    continue;
                }
                vec![doc
                    .resolve_one_hop(r)
                    .map_err(|e| object_unreadable(r, &e))?]
            }
            Object::Array(a) => a,
            Object::Dict(d) => d.into_values().collect(),
            Object::Stream { dict, .. } => dict
                .into_iter()
                .filter(|(k, _)| k.as_slice() != b"Length")
                .map(|(_, v)| v)
                .collect(),
            _ => continue,
        };
        values += more.len();
        if values > budget {
            return Err(over_budget(budget));
        }
        stack.extend(more.into_iter().filter(container));
    }
    Ok(edges)
}

/// Longest key [`describe`] shows, in bytes.
const MAX_DESCRIBED_KEY: usize = 32;
/// Most steps [`describe`] shows (the last ones).
const MAX_DESCRIBED_STEPS: usize = 8;

/// Content-free: the object and the key path (keys are structure),
/// bounded.
fn describe(o: &Origin) -> String {
    let mut s = match o.obj {
        Some(r) => format!("object {} {}", r.number, r.generation),
        None => "the trailer".to_string(),
    };
    let skip = o.path.len().saturating_sub(MAX_DESCRIBED_STEPS);
    if skip > 0 {
        s.push_str("/…");
    }
    for st in &o.path[skip..] {
        match st {
            Step::Key(k) => {
                s.push('/');
                let shown = &k[..k.len().min(MAX_DESCRIBED_KEY)];
                s.push_str(&String::from_utf8_lossy(shown));
                if k.len() > MAX_DESCRIBED_KEY {
                    s.push('…');
                }
            }
            Step::Index(i) => s.push_str(&format!("[{i}]")),
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const UR3: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
        /ByteRange [0 1 2 3] /Contents <0001> /Reference [<< /Type /SigRef /TransformMethod /UR3 \
        /TransformParams << /Type /TransformParams /Form [/FillIn] /V /2.2 >> >>] >>";

    /// Catalog entries `cat` (object 1), one page (3) with `page`, and
    /// `objs` from 10 on; a body `BROKEN` is unreadable.
    fn doc(cat: &str, page: &str, objs: &[(u32, &str)]) -> Document {
        doc_t(cat, page, objs, "")
    }

    /// [`doc`] with `trailer` added to the trailer dictionary.
    fn doc_t(cat: &str, page: &str, objs: &[(u32, &str)], trailer: &str) -> Document {
        let mut all: Vec<(u32, String)> = vec![
            (1, format!("<< /Type /Catalog /Pages 2 0 R {cat} >>")),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] {page} >>"),
            ),
        ];
        all.extend(objs.iter().map(|(n, b)| (*n, b.to_string())));
        let mut out = b"%PDF-1.7\n".to_vec();
        let size = all.iter().map(|o| o.0).max().unwrap() + 1;
        let mut offs = vec![None; size as usize];
        for (n, b) in &all {
            offs[*n as usize] = Some(out.len());
            if b == "BROKEN" {
                out.extend_from_slice(b"% not an object\n");
                continue;
            }
            out.extend_from_slice(format!("{n} 0 obj\n{b}\nendobj\n").as_bytes());
        }
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for o in &offs[1..] {
            match o {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R {trailer} >>\nstartxref\n{x}\n%%EOF\n")
                .as_bytes(),
        );
        Document::from_bytes(&out).unwrap()
    }

    fn plan(d: &Document) -> Result<Option<Plan>, Error> {
        assess(d, &Limits::default())
    }

    fn rule(r: Result<Option<Plan>, Error>) -> SignatureRule {
        match r {
            Err(Error::SignaturePolicy { rule, .. }) => rule,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    use SignatureRule::{
        OtherSignaturePresent as Other, SecurityStore, UsageRightsNotRemovable as NotRemovable,
    };

    #[test]
    fn the_removable_shapes() {
        // The corpus shape: /Perms with only an indirect /UR3, SigFlags 2.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags 2 >>",
            "",
            &[(10, UR3)],
        );
        let p = plan(&d).unwrap().unwrap();
        assert_eq!(
            (p.targets.len(), p.perms, p.perms_left),
            (1, PermsAt::Catalog, 0)
        );
        assert!(p.sig_flags.is_none(), "bit 1 not set: nothing to change");
        // A direct /UR3 in an indirect /Perms that keeps another key;
        // SigFlags 3 → 2 in a direct /AcroForm.
        let d = doc(
            "/Perms 20 0 R /AcroForm << /Fields [] /SigFlags 3 >>",
            "",
            &[(20, &format!("<< /UR3 {UR3} /X 1 >>"))],
        );
        let p = plan(&d).unwrap().unwrap();
        assert_eq!(
            (p.perms, p.perms_left),
            (PermsAt::Object(IndirectRef::new(20, 0)), 1)
        );
        assert_eq!(p.sig_flags, Some((3, 2, SigFlagsAt::CatalogAcroForm)));
        // SigFlags as an indirect integer; /AcroForm indirect.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R",
            "",
            &[
                (10, UR3),
                (30, "<< /Fields [] /SigFlags 31 0 R >>"),
                (31, "7"),
            ],
        );
        assert_eq!(
            plan(&d).unwrap().unwrap().sig_flags,
            Some((7, 6, SigFlagsAt::Object(IndirectRef::new(31, 0))))
        );
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R",
            "",
            &[(10, UR3), (30, "<< /Fields [] /SigFlags 5 >>")],
        );
        assert_eq!(
            plan(&d).unwrap().unwrap().sig_flags,
            Some((5, 4, SigFlagsAt::AcroFormObject(IndirectRef::new(30, 0))))
        );
        // /UR with /TransformMethod /UR (PDF 1.5) or /UR3.
        let ur = UR3.replace("/TransformMethod /UR3", "/TransformMethod /UR");
        let d = doc("/Perms << /UR 10 0 R >>", "", &[(10, &ur)]);
        assert_eq!(plan(&d).unwrap().unwrap().targets[0].key, Key::Ur);
        // Nothing to remove, nothing signature-like: None.
        assert!(plan(&doc("/AcroForm << /Fields [] >>", "", &[]))
            .unwrap()
            .is_none());
        // A /Parent cycle in a field tree ends; a seed value with a lock
        // inside is carried and counted.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [40 0 R] >>",
            "",
            &[
                (10, UR3),
                (40, "<< /T (a) /Kids [41 0 R] /SV 42 0 R >>"),
                (41, "<< /Parent 40 0 R /P 41 0 R >>"),
                (
                    42,
                    "<< /Type /SV /LockDocument true /Lock << /Action /All >> >>",
                ),
            ],
        );
        assert_eq!(plan(&d).unwrap().unwrap().seed_values, 1);
    }

    #[test]
    fn targets_that_are_not_usage_rights_signatures_refuse() {
        for (what, body) in [
            ("no /Reference", UR3.replace(" /Reference [", " /Ref [")),
            (
                "empty /Reference",
                "<< /Type /Sig /Filter /F /Contents <00> /Reference [] >>".into(),
            ),
            (
                "/Reference not an array",
                UR3.replace("/Reference [<<", "/Reference <<")
                    .replace(">>] >>", ">> >>"),
            ),
            (
                "element without /TransformMethod (DocMDP)",
                UR3.replace("/TransformMethod /UR3", ""),
            ),
            (
                "DocMDP",
                UR3.replace("/TransformMethod /UR3", "/TransformMethod /DocMDP"),
            ),
            (
                "FieldMDP",
                UR3.replace("/TransformMethod /UR3", "/TransformMethod /FieldMDP"),
            ),
            (
                "UR3 key with /UR transform",
                UR3.replace("/TransformMethod /UR3", "/TransformMethod /UR"),
            ),
            (
                "a timestamp",
                UR3.replace("/Type /Sig", "/Type /DocTimeStamp"),
            ),
            ("no /Filter", UR3.replace("/Filter /Adobe.PPKLite", "")),
            ("no /Contents", UR3.replace("/Contents <0001>", "")),
            ("not a dictionary", "42".into()),
        ] {
            let d = doc("/Perms << /UR3 10 0 R >>", "", &[(10, &body)]);
            assert_eq!(rule(plan(&d)), NotRemovable, "{what}");
        }
        // /Perms /DocMDP next to /UR3: another signature.
        let d = doc(
            "/Perms << /UR3 10 0 R /DocMDP 11 0 R >>",
            "",
            &[(10, UR3), (11, UR3)],
        );
        assert_eq!(rule(plan(&d)), Other);
        // /Perms not a dictionary.
        assert_eq!(rule(plan(&doc("/Perms 42", "", &[]))), NotRemovable);
    }

    #[test]
    fn sharing_refuses() {
        // The /UR3 dictionary is also a field's /V.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [40 0 R] >>",
            "",
            &[(10, UR3), (40, "<< /T (s) /V 10 0 R >>")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        // Something the /UR3 holds is referenced elsewhere.
        let held = UR3.replace(
            "/TransformParams << /Type /TransformParams /Form [/FillIn] /V /2.2 >>",
            "/TransformParams 12 0 R",
        );
        let d = doc(
            "/Perms << /UR3 10 0 R >> /X 12 0 R",
            "",
            &[(10, &held), (12, "<< /Type /TransformParams /V /2.2 >>")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        let d = doc(
            "/Perms << /UR3 10 0 R >>",
            "",
            &[(10, &held), (12, "<< /Type /TransformParams /V /2.2 >>")],
        );
        assert!(
            plan(&d).unwrap().is_some(),
            "held only by the target: removable"
        );
        // /Perms referenced from elsewhere too.
        let d = doc(
            "/Perms 20 0 R /Y 20 0 R",
            "",
            &[(10, UR3), (20, "<< /UR3 10 0 R >>")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        // /AcroForm shared when SigFlags must change.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R /Z 30 0 R",
            "",
            &[(10, UR3), (30, "<< /Fields [] /SigFlags 3 >>")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        // ... but not when it need not change.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R /Z 30 0 R",
            "",
            &[(10, UR3), (30, "<< /Fields [] /SigFlags 2 >>")],
        );
        assert!(plan(&d).unwrap().is_some());
        // SigFlags not an integer.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags /Three >>",
            "",
            &[(10, UR3)],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
    }

    #[test]
    fn any_other_signature_like_structure_refuses() {
        for (what, cat, page, objs) in [
            ("a /ByteRange in a field without /FT", "/AcroForm << /Fields [40 0 R] >>", "", vec![(40, "<< /T (f) /V << /ByteRange [0 1 2 3] >> >>")]),
            ("a timestamp reachable from an annotation", "", "/Annots [41 0 R]", vec![(41, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /X << /Type /DocTimeStamp >> >>")]),
            ("/Contents with a vendor /SubFilter", "/X << /Contents <00> /SubFilter /Vendor.Sign >>", "", vec![]),
            ("/Contents with a stray /Filter (conservative)", "", "/Annots [<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents (note) /Filter /F >>]", vec![]),
            ("/Cert with /Contents", "/X << /Cert <00> /Contents <00> >>", "", vec![]),
            ("/Reference with /TransformMethod", "/X << /Reference [<< /TransformMethod /FieldMDP >>] >>", "", vec![]),
            ("/Type through a reference", "/X << /Type 50 0 R >>", "", vec![(50, "/Sig")]),
            ("a signed widget outside /Fields", "/AcroForm << /Fields [] >>", "/Annots [42 0 R]", vec![(42, "<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /V << /Type /Sig /Filter /F /Contents <00> >> >>")]),
            // V4 P2: reachable only through /P (a page not in the tree).
            ("only through /P", "", "/Annots [43 0 R]", vec![(43, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /P 44 0 R >>"), (44, "<< /Type /Page /X << /ByteRange [0 1 2 3] >> >>")]),
            // ... and only through /Parent.
            ("only through /Parent", "/AcroForm << /Fields [45 0 R] >>", "", vec![(45, "<< /T (k) /Parent 46 0 R >>"), (46, "<< /Kids [45 0 R] /V << /Type /Sig >> >>")]),
            ("in /Info", "", "", vec![]),
        ] {
            let mut objs: Vec<(u32, &str)> = objs;
            objs.push((10, UR3));
            let cat = format!("/Perms << /UR3 10 0 R >> {cat}");
            let d = if what == "in /Info" {
                info_doc()
            } else {
                doc(&cat, page, &objs)
            };
            assert_eq!(rule(plan(&d)), Other, "{what}");
        }
        // Even without a usage-rights signature, Remove refuses a
        // document with another signature.
        let d = doc("/X << /ByteRange [0 1 2 3] >>", "", &[]);
        assert_eq!(rule(plan(&d)), Other);
    }

    fn info_doc() -> Document {
        // A trailer /Info holding a signature-like dictionary.
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 4 0 R >> >>".to_string(),
            "<< /Type /Pages /Kids [] /Count 0 >>".to_string(),
            "<< /Producer (p) /X << /ByteRange [0 1 2 3] >> >>".to_string(),
            UR3.to_string(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let x = out.len();
        out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R /Info 3 0 R >>\nstartxref\n{x}\n%%EOF\n")
                .as_bytes(),
        );
        Document::from_bytes(&out).unwrap()
    }

    #[test]
    fn locks_stores_unreadable_objects_and_limits_refuse() {
        // V4 P2: a /Lock on a widget that is in no /Fields.
        let d = doc("/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] >>", "/Annots [42 0 R]", &[(10, UR3), (42, "<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /Lock << /Action /All >> >>")]);
        assert_eq!(rule(plan(&d)), Other);
        // A field's own /Lock, any field.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [40 0 R] >>",
            "",
            &[
                (10, UR3),
                (40, "<< /FT /Tx /T (t) /Lock 41 0 R >>"),
                (41, "<< /Action /All >>"),
            ],
        );
        assert_eq!(rule(plan(&d)), Other);
        for store in ["/DSS << /Certs [] >>", "/Legal << /JavaScriptActions 0 >>"] {
            let d = doc(
                &format!("/Perms << /UR3 10 0 R >> {store}"),
                "",
                &[(10, UR3)],
            );
            assert_eq!(rule(plan(&d)), SecurityStore, "{store}");
        }
        // An unreadable object anywhere reachable.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /X 50 0 R",
            "",
            &[(10, UR3), (50, "BROKEN")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        // An undefined one is null: not a signature, not a refusal.
        let d = doc("/Perms << /UR3 10 0 R >> /X 99 0 R", "", &[(10, UR3)]);
        assert!(plan(&d).unwrap().is_some());
        // The walk budget (max_output_objects × 64 values): 200 values.
        let many = format!("/Perms << /UR3 10 0 R >> /X [{}]", "1 ".repeat(200));
        let d = doc(&many, "", &[(10, UR3)]);
        assert!(plan(&d).unwrap().is_some(), "within the default budget");
        let tiny = Limits {
            max_output_objects: 1,
            ..Limits::default()
        };
        assert_eq!(rule(assess(&d, &tiny)), NotRemovable);
    }

    /// Catalog `cat` (object 1), the page tree (2, 3), plain objects
    /// `plain`, and in object stream 40 the compressed objects `packed`;
    /// an xref stream (41).
    fn objstm_doc(cat: &str, plain: &[(u32, &str)], packed: &[(u32, &str)]) -> Document {
        let (mut head, mut body) = (String::new(), String::new());
        for (n, s) in packed {
            head.push_str(&format!("{n} {} ", body.len()));
            body.push_str(s);
            body.push(' ');
        }
        let os = format!("{head}{body}");
        let mut all: Vec<(u32, String)> = vec![
            (1, format!("<< /Type /Catalog /Pages 2 0 R {cat} >>")),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>".into(),
            ),
        ];
        all.extend(plain.iter().map(|(n, s)| (*n, s.to_string())));
        all.push((
            40,
            format!(
                "<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{os}\nendstream",
                packed.len(),
                head.len(),
                os.len()
            ),
        ));
        let mut b = b"%PDF-1.7\n".to_vec();
        let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
        for (n, s) in &all {
            rows.push((*n, 1, b.len() as u64, 0));
            b.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
        }
        for (i, (n, _)) in packed.iter().enumerate() {
            rows.push((*n, 2, 40, i as u64));
        }
        let x = b.len() as u64;
        rows.push((41, 1, x, 0));
        rows.sort();
        let (mut idx, mut data) = (String::new(), Vec::new());
        for (n, t, f2, f3) in &rows {
            idx.push_str(&format!("{n} 1 "));
            data.push(*t);
            data.extend_from_slice(&(*f2 as u32).to_be_bytes());
            data.extend_from_slice(&(*f3 as u16).to_be_bytes());
        }
        b.extend_from_slice(
            format!(
                "41 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 42 /Root 1 0 R /Length {} \
                 >>\nstream\n",
                data.len()
            )
            .as_bytes(),
        );
        b.extend_from_slice(&data);
        b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
        Document::from_bytes(&b).unwrap()
    }

    const TIMESTAMP: &str = "<< /Type /DocTimeStamp /ByteRange [0 1 2 3] /Contents <00> >>";

    /// Review 1 F1: an object-stream member whose body is a reference
    /// (an alias) is followed — the rewrite writes what it names.
    #[test]
    fn compressed_aliases_are_walked_and_counted() {
        let cat = "/Perms << /UR3 10 0 R >> /X 20 0 R";
        let d = objstm_doc(cat, &[(10, UR3), (21, TIMESTAMP)], &[(20, "21 0 R")]);
        assert_eq!(rule(plan(&d)), Other, "a timestamp behind an alias");
        let lock = "<< /FT /Tx /T (t) /Lock << /Action /All >> >>";
        let d = objstm_doc(cat, &[(10, UR3), (21, lock)], &[(20, "21 0 R")]);
        assert_eq!(rule(plan(&d)), Other, "a lock behind an alias");
        let d = objstm_doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R /X 20 0 R",
            &[(10, UR3), (30, "<< /Fields [] /SigFlags 3 >>")],
            &[(20, "30 0 R")],
        );
        assert_eq!(
            rule(plan(&d)),
            NotRemovable,
            "/AcroForm shared through an alias"
        );
        let d = objstm_doc(cat, &[(10, UR3)], &[(20, "10 0 R")]);
        assert_eq!(
            rule(plan(&d)),
            NotRemovable,
            "the target shared through an alias"
        );
        // Compressed targets, /Perms and /AcroForm are removable.
        let d = objstm_doc(
            "/Perms 20 0 R /AcroForm 30 0 R",
            &[],
            &[
                (10, UR3),
                (20, "<< /UR3 10 0 R >>"),
                (30, "<< /Fields [] /SigFlags 3 >>"),
            ],
        );
        let p = plan(&d).unwrap().unwrap();
        assert_eq!(p.perms, PermsAt::Object(IndirectRef::new(20, 0)));
        assert_eq!(
            p.sig_flags,
            Some((3, 2, SigFlagsAt::AcroFormObject(IndirectRef::new(30, 0))))
        );
        // ... and a timestamp packed next to them is found.
        let d = objstm_doc(
            "/Perms << /UR3 10 0 R >> /X 21 0 R",
            &[],
            &[(10, UR3), (21, TIMESTAMP)],
        );
        assert_eq!(rule(plan(&d)), Other);
    }

    /// Review 1 F3: every link of a plain chain is counted; an edited or
    /// removed structure reached through a chain refuses.
    #[test]
    fn reference_chains_are_counted_and_refused_where_edited() {
        let af = "<< /Fields [] /SigFlags 3 >>";
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R /X 5 0 R",
            "",
            &[(10, UR3), (5, "30 0 R"), (30, af)],
        );
        assert_eq!(
            rule(plan(&d)),
            NotRemovable,
            "/AcroForm shared through a chain"
        );
        for (what, cat, objs) in [
            (
                "/AcroForm",
                "/Perms << /UR3 10 0 R >> /AcroForm 5 0 R",
                vec![(5, "30 0 R"), (30, af)],
            ),
            (
                "/Perms",
                "/Perms 5 0 R",
                vec![(5, "20 0 R"), (20, "<< /UR3 10 0 R >>")],
            ),
            ("the target", "/Perms << /UR3 5 0 R >>", vec![]),
            (
                "/SigFlags",
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags 5 0 R >>",
                vec![(5, "6 0 R"), (6, "3")],
            ),
        ] {
            let mut objs = objs;
            objs.push((10, UR3));
            if what == "the target" {
                objs.push((5, "10 0 R"));
            }
            assert_eq!(rule(plan(&doc(cat, "", &objs))), NotRemovable, "{what}");
        }
    }

    /// Review 1 F4: only a seed-value dictionary's own /Lock is carried;
    /// an object also reached as something else is checked as that.
    #[test]
    fn a_seed_value_role_does_not_hide_a_field_lock() {
        let lock = "<< /FT /Tx /T (b) /Lock << /Action /All >> >>";
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [40 0 R 41 0 R] >>",
            "",
            &[
                (10, UR3),
                (40, "<< /FT /Sig /T (a) /SV 41 0 R >>"),
                (41, lock),
            ],
        );
        assert_eq!(rule(plan(&d)), Other, "a field that is also another's /SV");
        let d = doc_t(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [41 0 R] >>",
            "",
            &[(10, UR3), (41, lock), (50, "<< /SV 1 0 R >>")],
            "/Info 50 0 R",
        );
        assert!(
            matches!(plan(&d), Err(Error::SignaturePolicy { .. })),
            "/Info << /SV catalog >>"
        );
        let d = doc(
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [40 0 R] >>",
            "",
            &[
                (10, UR3),
                (
                    40,
                    "<< /FT /Sig /T (a) /SV << /X << /Lock << /Action /All >> >> >> >>",
                ),
            ],
        );
        assert_eq!(
            rule(plan(&d)),
            Other,
            "a lock below the seed-value dictionary"
        );
    }

    /// Review 1 F2: the walk keeps no path per pending value; a wide
    /// array under a long key is cheap.
    #[test]
    fn wide_structures_under_long_keys_are_cheap() {
        let key = "K".repeat(10_000);
        let wide = format!(
            "/Perms << /UR3 10 0 R >> /{key} << /{key} [{}] >>",
            "<< >> ".repeat(100_000)
        );
        let d = doc(&wide, "", &[(10, UR3)]);
        assert!(plan(&d).unwrap().is_some());
        // An error names a key at most MAX_DESCRIBED_KEY bytes long.
        let sig = format!("/{key} << /{key} [<< /ByteRange [0 1 2 3] >>] >>");
        let d = doc(&sig, "", &[]);
        match plan(&d) {
            Err(Error::SignaturePolicy {
                rule: Other,
                detail,
            }) => {
                assert!(detail.len() < 300, "{}", detail.len())
            }
            other => panic!("{other:?}"),
        }
    }

    /// Review 1 P3s.
    #[test]
    fn review_1_p3s() {
        // An indirect null or undefined /UR3 refuses (the key would
        // stay); a direct null is no key at all (§7.3.7).
        for v in ["5 0 R", "99 0 R"] {
            let d = doc(&format!("/Perms << /UR3 {v} >>"), "", &[(5, "null")]);
            assert_eq!(rule(plan(&d)), NotRemovable, "{v}");
        }
        assert!(plan(&doc("/Perms << /UR3 null >>", "", &[]))
            .unwrap()
            .is_none());
        // An object referenced twice from inside the target is held by it.
        let held = UR3.replace(
            "/TransformParams << /Type /TransformParams /Form [/FillIn] /V /2.2 >>",
            "/TransformParams 12 0 R /Data 12 0 R",
        );
        let d = doc(
            "/Perms << /UR3 10 0 R >>",
            "",
            &[(10, &held), (12, "<< /V /2.2 >>")],
        );
        assert!(plan(&d).unwrap().is_some(), "held twice by the target");
        // A target nested deeper than 64 is examined, not refused.
        let deep = format!("{}1 {}", "<< /A ".repeat(100), ">> ".repeat(100));
        let d = doc(
            "/Perms << /UR3 10 0 R >>",
            "",
            &[(
                10,
                &UR3.replace("/V /2.2", &format!("/V /2.2 /Deep {deep}")),
            )],
        );
        assert!(plan(&d).unwrap().is_some(), "deep target");
        // A nested /Reference array is a marker.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /X << /Reference [[<< /TransformMethod /DocMDP >>]] >>",
            "",
            &[(10, UR3)],
        );
        assert_eq!(rule(plan(&d)), Other);
        // An unreadable /Type target refuses as not removable.
        let d = doc(
            "/Perms << /UR3 10 0 R >> /X << /Type 50 0 R >>",
            "",
            &[(10, UR3), (50, "BROKEN")],
        );
        assert_eq!(rule(plan(&d)), NotRemovable);
        // The assessment leaves no warnings behind.
        let d = doc("/Perms << /UR3 10 0 R >> /X 99 0 R", "", &[(10, UR3)]);
        let before = d.warnings().len();
        assert!(plan(&d).unwrap().is_some());
        assert_eq!(d.warnings().len(), before);
    }
}
