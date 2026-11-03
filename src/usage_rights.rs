//! Usage-rights signature removal (`UsageRightsPolicy::Remove`, D1;
//! `docs/plans/rewrite-policy.md` §5).
//!
//! [`scan`] walks the whole reachable graph — from the trailer's `/Root`
//! and `/Info` and every page, through every reference including `/P`
//! and `/Parent` — and records every *signature-like* dictionary (§5.3.2:
//! `/ByteRange`; `/Type /Sig` or `/DocTimeStamp`; `/Contents` with
//! `/Filter` or `/SubFilter`; `/Reference` with `/TransformMethod`;
//! `/Cert` with `/Contents`), every field lock (`/Lock`, wherever the
//! dictionary is — a widget outside `/AcroForm /Fields` included — except
//! inside a seed-value dictionary), the seed-value dictionaries, and how
//! many references point at each object. An unreadable object or a walk
//! past its budget refuses: the walk is never cut short.
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

/// Where a dictionary is: the nearest enclosing indirect object and the
/// keys / indices from it (`[]` for the object's own body).
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

/// What [`scan`] found. Content-free.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Every signature-like dictionary, by origin.
    pub signature_like: Vec<Origin>,
    /// Dictionaries with a `/ByteRange` key (a subset of the above).
    pub byte_ranges: usize,
    /// Dictionaries with a `/Lock` key outside a seed-value dictionary.
    pub locks: Vec<Origin>,
    /// Seed-value dictionaries (`/SV`).
    pub seed_values: usize,
    /// References to each object met (every edge, back-pointers too).
    pub refs: HashMap<IndirectRef, u32>,
    /// Values examined.
    pub nodes: usize,
}

fn name_of(doc: &Document, o: Option<&Object>) -> Option<Vec<u8>> {
    match deref_chain(doc, o?).ok()? {
        Object::Name(n) => Some(n),
        _ => None,
    }
}

/// Whether `d` is signature-like (§5.3.2). Values are read through their
/// references; one that cannot be read refuses (an error).
pub(crate) fn signature_like(doc: &Document, d: &Dict) -> Result<bool, Error> {
    let has = |k: &[u8]| d.contains_key(k);
    if has(b"ByteRange")
        || (has(b"Contents") && (has(b"Filter") || has(b"SubFilter")))
        || (has(b"Cert") && has(b"Contents"))
    {
        return Ok(true);
    }
    if let Some(t) = d.get(b"Type".as_slice()) {
        if let Object::Name(n) = deref_chain(doc, t)? {
            if n == b"Sig" || n == b"DocTimeStamp" {
                return Ok(true);
            }
        }
    }
    if let Some(r) = d.get(b"Reference".as_slice()) {
        let elems = match deref_chain(doc, r)? {
            Object::Array(a) => a,
            other => vec![other],
        };
        for e in &elems {
            if let Object::Dict(ed) = deref_chain(doc, e)? {
                if ed.contains_key(b"TransformMethod".as_slice()) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn not_removable(detail: impl Into<String>) -> Error {
    Error::SignaturePolicy {
        rule: SignatureRule::UsageRightsNotRemovable,
        detail: detail.into(),
    }
}

/// Walk `doc`'s reachable graph (module docs). Fails — refusing the
/// removal — on an unreadable object or past the walk budget
/// (`max_output_objects` × 64 values).
pub(crate) fn scan(doc: &Document, limits: &Limits) -> Result<Scan, Error> {
    let mut s = Scan::default();
    let mut seen: HashSet<IndirectRef> = HashSet::new();
    // (value, origin, under a seed-value dictionary, is the /SV value)
    let mut stack: Vec<(Object, Origin, bool, bool)> = Vec::new();
    let top = || Origin {
        obj: None,
        path: Vec::new(),
    };
    for k in [b"Root".as_slice(), b"Info".as_slice()] {
        if let Some(v) = doc.trailer().get(k) {
            stack.push((v.clone(), top(), false, false));
        }
    }
    for p in doc.pages() {
        if let Some(r) = p.object_ref() {
            stack.push((Object::Ref(r), top(), false, false));
        }
    }
    let budget = limits.max_output_objects.saturating_mul(64);
    while let Some((o, at, under_sv, is_sv)) = stack.pop() {
        s.nodes += 1;
        if s.nodes > budget {
            return Err(not_removable(format!(
                "the document could not be searched for signatures completely (more than {budget} \
                 values)"
            )));
        }
        let (o, at) = match o {
            Object::Ref(r) => {
                *s.refs.entry(r).or_default() += 1;
                if !seen.insert(r) {
                    continue;
                }
                let v = doc.resolve(r).map_err(|e| {
                    not_removable(format!(
                        "object {} {} could not be read to search for signatures ({})",
                        r.number,
                        r.generation,
                        e.kind().as_str()
                    ))
                })?;
                (
                    v,
                    Origin {
                        obj: Some(r),
                        path: Vec::new(),
                    },
                )
            }
            o => (o, at),
        };
        let mut d = match o {
            Object::Dict(d) => d,
            // A stream's `/Length` is storage; the rewrite does not
            // follow it either.
            Object::Stream { mut dict, .. } => {
                dict.remove(b"Length".as_slice());
                dict
            }
            Object::Array(a) => {
                for (i, x) in a.into_iter().enumerate().rev() {
                    let mut p = at.clone();
                    p.path.push(Step::Index(i));
                    stack.push((x, p, under_sv, false));
                }
                continue;
            }
            _ => continue,
        };
        if signature_like(doc, &d)? {
            if d.contains_key(b"ByteRange".as_slice()) {
                s.byte_ranges += 1;
            }
            s.signature_like.push(at.clone());
        }
        if d.contains_key(b"Lock".as_slice()) && !under_sv {
            s.locks.push(at.clone());
        }
        if is_sv {
            s.seed_values += 1;
        }
        for (k, v) in std::mem::take(&mut d).into_iter().rev() {
            let sv = k.as_slice() == b"SV";
            let mut p = at.clone();
            p.path.push(Step::Key(k));
            stack.push((v, p, under_sv || sv, sv));
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

/// Whether `/UR` and `/UR3` can be removed from `doc`, and how; `Ok(None)`
/// when `/Perms` has neither (and nothing else signature-like is
/// present). Every condition of `rewrite-policy.md` §5.3 that it cannot
/// establish refuses.
pub(crate) fn assess(doc: &Document, limits: &Limits) -> Result<Option<Plan>, Error> {
    let cat = doc.catalog();
    let root = doc
        .trailer()
        .get(b"Root".as_slice())
        .and_then(|o| o.as_ref())
        .ok_or_else(|| not_removable("the catalog is not an indirect object"))?;
    // Security stores (§5.6, option 1).
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
        Some(p) => {
            let at = match p.as_ref() {
                Some(r) => PermsAt::Object(r),
                None => PermsAt::Catalog,
            };
            match deref_chain(doc, p)? {
                Object::Null => (None, Dict::new()),
                Object::Dict(d) => (Some(at), d),
                _ => return Err(not_removable("/Perms is not a dictionary")),
            }
        }
    };
    let mut targets = Vec::new();
    for key in [Key::Ur, Key::Ur3] {
        let Some(v) = perms.get(key.name()) else {
            continue;
        };
        let object = v.as_ref();
        let dict = match deref_chain(doc, v)? {
            Object::Null => continue,
            Object::Dict(d) => d,
            _ => {
                return Err(not_removable(format!(
                    "/Perms /{} is not a dictionary",
                    String::from_utf8_lossy(key.name())
                )))
            }
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
    if let Some(o) = scan.signature_like.iter().find(|o| !allowed.contains(o)) {
        return Err(Error::SignaturePolicy {
            rule: SignatureRule::OtherSignaturePresent,
            detail: format!(
                "a signature-like dictionary at {} — only /Perms /UR and /UR3 usage-rights \
                 signatures are removed",
                describe(o)
            ),
        });
    }
    if let Some(o) = scan.locks.first() {
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
                    "the /{} signature is referenced {} times — it is shared with another \
                     structure",
                    String::from_utf8_lossy(t.key.name()),
                    refs(&r)
                )));
            }
        }
        let mut inner = HashSet::new();
        held_refs(doc, &Object::Dict(t.dict.clone()), &mut inner, 0)?;
        for r in inner {
            // Counted once from inside the target; any other reference
            // keeps it in the output.
            if refs(&r) != 1 {
                return Err(not_removable(format!(
                    "object {} {} inside the /{} signature is shared with another structure",
                    r.number,
                    r.generation,
                    String::from_utf8_lossy(t.key.name())
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
        let af_at = af.as_ref();
        if let Object::Dict(afd) = deref_chain(doc, af)? {
            if let Some(f) = afd.get(b"SigFlags".as_slice()) {
                let value = match deref_chain(doc, f)? {
                    Object::Integer(v) => v,
                    Object::Null => 0,
                    _ => return Err(not_removable("/AcroForm /SigFlags is not an integer")),
                };
                if value & 1 == 1 {
                    let at = match (f.as_ref(), af_at) {
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
    }
    Ok(Some(Plan {
        targets,
        perms: perms_at,
        perms_left,
        sig_flags,
        seed_values: scan.seed_values,
    }))
}

/// §5.3.1: a usage-rights signature dictionary — `/Type /Sig` or none,
/// `/Filter` and `/Contents`, and a non-empty `/Reference` array whose
/// every element has `/TransformMethod /UR3` (or `/UR` under `/Perms
/// /UR`). An element without `/TransformMethod` is `/DocMDP` (Table 257).
fn check_target(doc: &Document, key: Key, d: &Dict) -> Result<(), Error> {
    let k = String::from_utf8_lossy(key.name());
    let fail = |why: &str| not_removable(format!("/Perms /{k}: {why}"));
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
        Some(Err(e)) => return Err(e),
        _ => {
            return Err(fail(
                "no non-empty /Reference array: usage rights cannot be established",
            ))
        }
    };
    for e in &elems {
        let Object::Dict(ed) = deref_chain(doc, e)? else {
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

/// The indirect objects `o` holds, through its references (bounded).
fn held_refs(
    doc: &Document,
    o: &Object,
    out: &mut HashSet<IndirectRef>,
    depth: usize,
) -> Result<(), Error> {
    if depth > 64 {
        return Err(not_removable("a usage-rights signature nested too deeply"));
    }
    match o {
        Object::Ref(r) => {
            if out.insert(*r) {
                let v = doc.resolve(*r).map_err(|_| {
                    not_removable("an object inside a usage-rights signature cannot be read")
                })?;
                held_refs(doc, &v, out, depth + 1)?;
            }
        }
        Object::Array(a) => {
            for x in a {
                held_refs(doc, x, out, depth + 1)?;
            }
        }
        Object::Dict(d) | Object::Stream { dict: d, .. } => {
            for (k, x) in d {
                if matches!(o, Object::Stream { .. }) && k.as_slice() == b"Length" {
                    continue;
                }
                held_refs(doc, x, out, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Content-free: the object and the key path (keys are structure).
fn describe(o: &Origin) -> String {
    let mut s = match o.obj {
        Some(r) => format!("object {} {}", r.number, r.generation),
        None => "the trailer".to_string(),
    };
    for st in &o.path {
        match st {
            Step::Key(k) => {
                s.push('/');
                s.push_str(&String::from_utf8_lossy(k));
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
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
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
}
