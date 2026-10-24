//! Digital-signature *detection* and the edit policy derived from it
//! (ISO 32000-1 §12.8). Nothing here verifies a signature
//! cryptographically: the engine can tell that a document claims to
//! be signed, which bytes the claim covers, and what the signer
//! permitted (`/DocMDP` `/P`, `/FieldMDP`), and it uses that to decide
//! whether an edit may be *appended* as an incremental update without
//! invalidating the claim. It never reports a signature as valid.
//!
//! Policy (safe by default):
//! * a full rewrite of a signed document is always refused — every
//!   byte the signature covers would change;
//! * an incremental update keeps the signed bytes intact, so it is
//!   allowed only when the signed revision is intact in the file
//!   (`/ByteRange` covers the whole file up to the last signed
//!   revision, leaving only the `/Contents` hole) and the signer's
//!   permissions admit the change: `/DocMDP /P 1` admits nothing,
//!   `/P 2` form filling and signing, `/P 3` also annotation
//!   creation, deletion and modification; an approval signature
//!   without `/DocMDP` admits form filling and annotations (what
//!   viewers treat as a non-invalidating "document modified after
//!   signing"); metadata and any other change is refused whenever a
//!   signature exists;
//! * anything that cannot be classified — a `/ByteRange` that is not
//!   four non-negative integers within the file, a signature whose
//!   `/Contents` hole does not sit inside the range, a `/FieldMDP`
//!   lock (field-level locks are not evaluated) — is refused.

use crate::document::Document;
use crate::error::Error;
use crate::object::{Dict, IndirectRef, Object};
use std::collections::HashSet;

/// What an edit session changes, for the signature policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Field values (`/V`, `/AS`, `/NeedAppearances`).
    FormFill,
    /// Annotations added, removed or modified.
    Annotations,
    /// Document information / metadata.
    Metadata,
    /// Anything else (page content, structure).
    Other,
}

/// One signature found in the document.
#[derive(Debug, Clone)]
pub struct SignatureInfo {
    /// Field name (`/T`), or `"unnamed"`.
    pub field: String,
    /// `/ByteRange` as stored, when it is four integers.
    pub byte_range: Option<[u64; 4]>,
    /// `true` when `/ByteRange` covers the file from byte 0 to the end
    /// of some revision with only the `/Contents` string left out —
    /// i.e. the signed bytes are intact and still in the file.
    pub range_intact: bool,
    /// Byte offset the signed revision ends at (`range[2] + range[3]`).
    pub signed_end: Option<u64>,
    /// `/DocMDP` `/P` (1, 2 or 3) when this is a certification
    /// signature.
    pub docmdp_p: Option<i64>,
    /// The signature carries a `/FieldMDP` transform (field locks).
    pub field_mdp: bool,
    /// `/SubFilter` name, informational.
    pub sub_filter: Option<String>,
}

impl SignatureInfo {
    /// Certification (author) signature vs approval signature.
    pub fn is_certification(&self) -> bool {
        self.docmdp_p.is_some()
    }
}

/// Every signature value in `doc`: `/FT /Sig` fields with a `/V`
/// dictionary under `/AcroForm /Fields` (through `/Kids`, depth ≤ 32)
/// and signature widgets reachable only from page `/Annots`.
pub fn signatures(doc: &Document) -> Result<Vec<SignatureInfo>, Error> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let file_len = doc.bytes().len() as u64;
    // Certification permission from /Perms /DocMDP (a reference to the
    // signature dictionary); matched to the field by identity below.
    let perms_sig: Option<Object> = match doc.catalog().get(b"Perms".as_slice()) {
        Some(p) => doc
            .deref(p)?
            .as_dict()
            .and_then(|d| d.get(b"DocMDP".as_slice()).cloned()),
        None => None,
    };
    if let Some(af) = doc.catalog().get(b"AcroForm".as_slice()) {
        let af = doc.deref(af)?;
        if let Some(d) = af.as_dict() {
            if let Some(fields) = d.get(b"Fields".as_slice()) {
                let fields = doc.deref(fields)?;
                if let Some(arr) = fields.as_array() {
                    walk(
                        doc,
                        arr,
                        0,
                        &mut seen,
                        &mut out,
                        file_len,
                        perms_sig.as_ref(),
                    )?;
                }
            }
        }
    }
    for page in doc.pages() {
        let Some(pref) = page.object_ref() else {
            continue;
        };
        let Ok(pd) = doc.resolve(pref) else { continue };
        let Some(pd) = pd.as_dict() else { continue };
        let Some(annots) = pd.get(b"Annots".as_slice()) else {
            continue;
        };
        let annots = doc.deref(annots)?;
        if let Some(arr) = annots.as_array() {
            walk(
                doc,
                arr,
                0,
                &mut seen,
                &mut out,
                file_len,
                perms_sig.as_ref(),
            )?;
        }
    }
    Ok(out)
}

fn walk(
    doc: &Document,
    fields: &[Object],
    depth: usize,
    seen: &mut HashSet<IndirectRef>,
    out: &mut Vec<SignatureInfo>,
    file_len: u64,
    perms_sig: Option<&Object>,
) -> Result<(), Error> {
    if depth > 32 {
        return Ok(());
    }
    for f in fields {
        if let Some(r) = f.as_ref() {
            if !seen.insert(r) {
                continue;
            }
        }
        let fo = doc.deref(f)?;
        let Some(fd) = fo.as_dict() else { continue };
        let mut ft = fd
            .get(b"FT".as_slice())
            .and_then(|o| o.as_name())
            .map(|n| n.to_vec());
        if ft.is_none() {
            if let Some(p) = fd.get(b"Parent".as_slice()) {
                if let Ok(p) = doc.deref(p) {
                    ft = p
                        .as_dict()
                        .and_then(|pd| pd.get(b"FT".as_slice()))
                        .and_then(|o| o.as_name())
                        .map(|n| n.to_vec());
                }
            }
        }
        if ft.as_deref() == Some(b"Sig") {
            if let Some(v) = fd.get(b"V".as_slice()) {
                let vo = doc.deref(v)?;
                if let Some(vd) = vo.as_dict() {
                    let is_docmdp_target = match (perms_sig, v.as_ref()) {
                        (Some(Object::Ref(pr)), Some(vr)) => *pr == vr,
                        _ => false,
                    };
                    out.push(describe(doc, fd, vd, file_len, is_docmdp_target)?);
                }
            }
        }
        if let Some(kids) = fd.get(b"Kids".as_slice()) {
            let kids = doc.deref(kids)?;
            if let Some(k) = kids.as_array() {
                walk(doc, k, depth + 1, seen, out, file_len, perms_sig)?;
            }
        }
    }
    Ok(())
}

fn describe(
    doc: &Document,
    field: &Dict,
    sig: &Dict,
    file_len: u64,
    is_docmdp_target: bool,
) -> Result<SignatureInfo, Error> {
    let name = field
        .get(b"T".as_slice())
        .and_then(|o| o.as_string())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .unwrap_or_else(|| "unnamed".into());
    let sub_filter = sig
        .get(b"SubFilter".as_slice())
        .and_then(|o| o.as_name())
        .map(|n| String::from_utf8_lossy(n).into_owned());
    let byte_range: Option<[u64; 4]> = match sig.get(b"ByteRange".as_slice()) {
        Some(o) => {
            let o = doc.deref(o)?;
            o.as_array().and_then(|a| {
                if a.len() != 4 {
                    return None;
                }
                let mut r = [0u64; 4];
                for (i, x) in a.iter().enumerate() {
                    let v = x.as_i64()?;
                    if v < 0 {
                        return None;
                    }
                    r[i] = v as u64;
                }
                Some(r)
            })
        }
        None => None,
    };
    // Intact: [0, a) and [b, b+c) with a ≤ b, b + c ≤ file length, and
    // the gap [a, b) holding exactly the /Contents hex string. Without
    // the gap check a range could skip arbitrary bytes.
    let mut range_intact = false;
    let mut signed_end = None;
    if let Some([s0, a, b, c]) = byte_range {
        let end = b.saturating_add(c);
        if s0 == 0 && a <= b && end <= file_len && end > 0 {
            signed_end = Some(end);
            let gap = &doc.bytes()[a as usize..b as usize];
            let t = trim_ws(gap);
            range_intact = t.first() == Some(&b'<')
                && t.last() == Some(&b'>')
                && t[1..t.len() - 1]
                    .iter()
                    .all(|c| c.is_ascii_hexdigit() || c.is_ascii_whitespace());
        }
    }
    // /DocMDP and /FieldMDP live in /Reference [ << /TransformMethod … >> ].
    let mut docmdp_p = None;
    let mut field_mdp = false;
    if let Some(refs) = sig.get(b"Reference".as_slice()) {
        let refs = doc.deref(refs)?;
        if let Some(arr) = refs.as_array() {
            for r in arr {
                let r = doc.deref(r)?;
                let Some(rd) = r.as_dict() else { continue };
                match rd
                    .get(b"TransformMethod".as_slice())
                    .and_then(|o| o.as_name())
                {
                    Some(b"DocMDP") => {
                        let p = rd
                            .get(b"TransformParams".as_slice())
                            .map(|o| doc.deref(o))
                            .transpose()?
                            .and_then(|o| {
                                o.as_dict()
                                    .and_then(|d| d.get(b"P".as_slice()).and_then(|p| p.as_i64()))
                            })
                            .unwrap_or(2);
                        docmdp_p = Some(p);
                    }
                    Some(b"FieldMDP") => field_mdp = true,
                    _ => {}
                }
            }
        }
    }
    if is_docmdp_target && docmdp_p.is_none() {
        // /Perms names it as the certification signature even without a
        // readable /Reference: the default permission is 2.
        docmdp_p = Some(2);
    }
    Ok(SignatureInfo {
        field: name,
        byte_range,
        range_intact,
        signed_end,
        docmdp_p,
        field_mdp,
        sub_filter,
    })
}

fn trim_ws(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(s);
    &b[s..e.max(s)]
}

/// May `changes` be appended to `doc` as an incremental update
/// without contradicting its signatures? `Ok(())` when there is no
/// signature or every signature admits every change; otherwise the
/// first refusal.
pub(crate) fn incremental_policy(doc: &Document, changes: &[ChangeKind]) -> Result<(), Error> {
    let sigs = signatures(doc)?;
    if sigs.is_empty() {
        return Ok(());
    }
    let refuse = |detail: String| Error::Unsupported {
        feature: "edit of a signed document",
        detail,
    };
    for s in &sigs {
        if !s.range_intact {
            return Err(refuse(format!(
                "signature field {:?} has a /ByteRange that does not cover the signed revision \
                 intact ({:?}) — its validity cannot be reasoned about, so no change is appended",
                s.field, s.byte_range
            )));
        }
        if s.field_mdp {
            return Err(refuse(format!(
                "signature field {:?} carries a /FieldMDP lock, which is not evaluated",
                s.field
            )));
        }
        for c in changes {
            let ok = match (s.docmdp_p, c) {
                (Some(1), _) => false,
                (Some(2), ChangeKind::FormFill) => true,
                (Some(2), _) => false,
                (Some(3), ChangeKind::FormFill | ChangeKind::Annotations) => true,
                (Some(3), _) => false,
                (Some(_), _) => false,
                (None, ChangeKind::FormFill | ChangeKind::Annotations) => true,
                (None, _) => false,
            };
            if !ok {
                return Err(refuse(format!(
                    "signature field {:?} ({}) does not permit {:?} changes",
                    s.field,
                    match s.docmdp_p {
                        Some(p) => format!("certification, /DocMDP /P {p}"),
                        None => "approval signature".into(),
                    },
                    c
                )));
            }
        }
    }
    Ok(())
}
