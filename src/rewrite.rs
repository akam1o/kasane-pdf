//! Rebuild a loaded [`Document`] as a fresh, single-revision PDF —
//! the "full rewrite / round-trip" writer.
//!
//! The traversal starts from the trailer's `/Root` (plus `/Info` when
//! present) and follows every indirect reference, assigning dense new
//! object numbers starting at 1. No original byte ranges, xref
//! structure, `/Prev` history, object-stream containers or trailer
//! bookkeeping survive — the output is a clean classic-xref file.
//!
//! What this is **not**: editing, redaction or sanitizing. Referenced
//! metadata (including `/Info` and anything reachable from the
//! catalog) is carried over verbatim; a full rewrite does *not*
//! guarantee removal of sensitive data and must not be presented as
//! such.
//!
//! Refusals (before any byte is written):
//! * encrypted input — already rejected at [`Document::from_bytes`].
//! * `/Perms` `/DocMDP`/`/UR`/`/UR3` signature lock dictionaries and
//!   `/AcroForm` signature fields that actually hold a `/V` — digital
//!   signatures cannot survive a rewrite, so writing a file that
//!   still *claims* one is refused outright (blank unsigned fields
//!   pass through like any other object).
//! * an `/AcroForm` `/XFA` packet that holds an XML signature (the
//!   XML-DSig namespace anywhere in the packets' decoded content,
//!   character references expanded, UTF-8 or UTF-16), or that cannot
//!   be read completely to check (filter, limits, external data, an
//!   encoding that is not ASCII-compatible, a DTD) — the signature
//!   policy applied to XFA. An XFA form without one is
//!   carried verbatim (below).
//! * catalog `/RichMedia`, `/AA` — outside profile.
//! * a catalog `/OCProperties` that is not well formed (types, required
//!   entries, `/Order` cycles — `crate::optcontent::validate_for_rewrite`).
//!   A well-formed one is carried verbatim, like every other reachable
//!   structure: OCGs, configurations and every content-level reference
//!   (`/Properties`, XObject and annotation `/OC`, OCMDs) keep their
//!   targets through the renumbering.
//!
//! References to undefined objects (§7.3.10: no in-use or compressed
//! cross-reference entry for that number *and* generation — absent,
//! free, or another generation) are references to the null object; the
//! rewrite keeps each such reference in place and points it at an
//! indirect object whose body is `null`, so array positions, dictionary
//! keys and shared targets are unchanged and the output has no dangling
//! reference. Each is recorded on the document as
//! [`crate::Warning::UndefinedReference`]. An object that is defined
//! but cannot be read (damaged body, a compressed entry whose object
//! stream lacks it, a reference cycle, a limit) is never turned into
//! null: the rewrite fails with that error.
//!
//! Everything else reachable is copied verbatim — unknown dictionary
//! keys are preserved, never silently dropped. That includes XFA
//! (§12.7.8): the `/XFA` stream or packet array — packet names, order,
//! duplicates, every packet's stream bytes — and the catalog's
//! `/NeedsRendering` are ordinary reachable objects, copied with
//! renumbered references; nothing is parsed, regenerated or reordered.
//! Named destinations travel the same way: the `/Names` → `/Dests`
//! name tree, the legacy catalog `/Dests` dictionary and every `/Dest`, `/GoTo` `/D` or
//! `/OpenAction` operand that names into them travel as ordinary
//! objects with renumbered references, so the name → page binding
//! is identical before and after (see [`crate::dests`]).

use crate::document::Document;
use crate::error::Error;
use crate::object::{IndirectRef, Object};
use crate::serialize::write_indirect_object;
use std::collections::{HashMap, HashSet};

use crate::limits::Limits;

/// The latest version the rewriter declares. The output — classic xref,
/// no object streams, every object carried as it is — is valid for the
/// version the input declares, up to ISO 32000-2 (PDF 2.0): a 2.0
/// document is written as 2.0 (never translated down), with the trailer
/// `/ID` 2.0 requires. A later version is refused: its rules are not
/// known, so the output could not be vouched for.
const MAX_VERSION: (u8, u8) = (2, 0);

/// A PDF version, `(major, minor)`.
type Version = (u8, u8);

/// Which trailer slot a discovered object came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootKind {
    Root,
    Info,
    /// Below a root — not a trailer entry.
    Ordinary,
}

/// Rewrite `doc` as a new single-revision PDF.
///
/// Returns the complete file bytes; nothing is written to disk.
/// Deterministic: same input → same output.
pub fn rewrite(doc: &Document) -> Result<Vec<u8>, Error> {
    rewrite_with_limits(doc, &Limits::default())
}

/// Like [`rewrite`] but with explicit output limits — the traversal
/// budget, emitted-byte cap and walk depth come from `limits`.
pub fn rewrite_with_limits(doc: &Document, limits: &Limits) -> Result<Vec<u8>, Error> {
    limits.validate()?;
    rewrite_overlay(doc, &HashMap::new(), &HashSet::new(), limits)
}

/// Optional overrides for [`rewrite_overlay`] — an internal knob
/// used by `remove_document_metadata`; not a public trailer-edit
/// API. All fields default to off.
#[derive(Default)]
pub(crate) struct RewriteOverrides {
    /// Drop the trailer `/Info` entry — both as a collection root
    /// and from the output trailer.
    pub strip_info: bool,
    /// Drop the catalog `/Metadata` entry — the catalog object is
    /// overlaid with a copy that lacks `/Metadata`.
    pub strip_metadata: bool,
}

/// Rewrite with an *overlay*: `(old obj num, gen) → replacement body`.
/// When the collector reaches a reference present in `overlay`, the
/// overlay body is used instead of `doc.resolve` — this is how the
/// editor injects new or modified objects without touching the
/// original bytes. Overlay bodies are walked like any other, so refs
/// inside them get renumbered too. Refusal checks run against the
/// *original* document plus overlay-independent checks.
///
/// `synth_numbers` is the set of `(num, gen)` the edit session
/// allocated as *new* synthetic objects. A reference that originates
/// from an *input* body is never allowed to resolve to a synthetic —
/// a dangling input ref resolves through the document (to null, if the
/// object is undefined), never to an edit's new object.
/// Refs inside overlay bodies *are* edit refs and may resolve to
/// synthetics (the new `/Annots` array points at them).
pub(crate) fn rewrite_overlay(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    synth_numbers: &HashSet<(u32, u16)>,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    rewrite_overlay_with(
        doc,
        overlay,
        synth_numbers,
        limits,
        &RewriteOverrides::default(),
    )
}

/// [`rewrite_overlay`] plus internal-only `overrides` — see
/// [`RewriteOverrides`] for what each flag does.
pub(crate) fn rewrite_overlay_with(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    synth_numbers: &HashSet<(u32, u16)>,
    limits: &Limits,
    overrides: &RewriteOverrides,
) -> Result<Vec<u8>, Error> {
    rewrite_core(doc, overlay, synth_numbers, limits, overrides, false).map(|(out, ..)| out)
}

/// `(old number, old generation) → new number` of a rewrite.
pub(crate) type ObjectMap = HashMap<(u32, u16), u32>;

/// The rewrite of [`rewrite_with`](crate::rewrite_with): [`rewrite`]'s
/// pipeline, also returning the object map and the version written.
/// `carry` lifts the document-level executable-content refusals
/// ([`ExecutableContentPolicy::Carry`](crate::ExecutableContentPolicy::Carry));
/// with `false` it is exactly [`rewrite_with_limits`].
pub(crate) fn rewrite_mapped(
    doc: &Document,
    limits: &Limits,
    carry: bool,
) -> Result<(Vec<u8>, ObjectMap, crate::rewrite_policy::VersionChange), Error> {
    rewrite_core(
        doc,
        &HashMap::new(),
        &HashSet::new(),
        limits,
        &RewriteOverrides::default(),
        carry,
    )
}

fn rewrite_core(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    synth_numbers: &HashSet<(u32, u16)>,
    limits: &Limits,
    overrides: &RewriteOverrides,
    carry: bool,
) -> Result<(Vec<u8>, ObjectMap, crate::rewrite_policy::VersionChange), Error> {
    let _budget = doc.decoded_budget_scope();
    if doc.is_encrypted() {
        return Err(crate::incremental::encrypted_write_refused());
    }
    check_refusals(doc, carry)?;

    // The catalog's `/Metadata` is removed by overlaying the catalog
    // object itself — but if the caller already overlaid the catalog
    // (e.g. an `Edit` that changed it), we remove `/Metadata` from
    // *that* body instead of re-inserting a fresh clone, so earlier
    // edits are not rolled back.
    let mut owned_overlay: HashMap<(u32, u16), Object> = HashMap::new();
    if overrides.strip_metadata {
        if let Some(root_ref) = doc
            .trailer()
            .get(b"Root".as_slice())
            .and_then(|o| o.as_ref())
        {
            let key = (root_ref.number, root_ref.generation);
            // Start from the *existing* overlay body when present —
            // it may carry earlier edits — otherwise from the
            // document's catalog.
            let mut cat = match overlay.get(&key) {
                Some(Object::Dict(d)) => d.clone(),
                Some(_) => {
                    return Err(Error::Structure(
                        "catalog overlay is not a dictionary".into(),
                    ))
                }
                None => doc.catalog().clone(),
            };
            cat.remove(b"Metadata".as_slice());
            owned_overlay.insert(key, Object::Dict(cat));
        }
    }
    let effective_overlay = if owned_overlay.is_empty() {
        overlay
    } else {
        // Caller-supplied overlay entries win for every object
        // *except* the catalog we just rewrote (ours is the merged
        // catalog-minus-Metadata, which must be the final body).
        for (k, v) in overlay.iter() {
            owned_overlay.entry(*k).or_insert_with(|| v.clone());
        }
        &owned_overlay
    };

    // ---- Phase 1: collect reachable objects, assign new numbers ----
    let mut c = Collector {
        doc,
        overlay: effective_overlay,
        synth_numbers,
        limits,
        map: HashMap::new(),
        bodies: HashMap::new(),
        kinds: HashMap::new(),
        next_number: 1,
        walked: 0,
    };

    let trailer = doc.trailer();
    let mut root_refs: Vec<(IndirectRef, RootKind)> = Vec::new();
    if let Some(r) = trailer.get(b"Root".as_slice()).and_then(|o| o.as_ref()) {
        root_refs.push((r, RootKind::Root));
    }
    if !overrides.strip_info {
        if let Some(r) = trailer.get(b"Info".as_slice()).and_then(|o| o.as_ref()) {
            root_refs.push((r, RootKind::Info));
        }
    }
    for (r, kind) in root_refs {
        c.walk(r, kind, 0, false)?;
    }

    // ---- Phase 2: emit, in new-number order ----
    let (declared, version) = effective_version(doc)?;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(format!("%PDF-{}.{}", version.0, version.1).as_bytes());
    // Binary marker comment (§7.5.2 recommends bytes >127 so
    // transport layers never mistake the file for text).
    out.extend_from_slice(b"\n%\xE2\xE3\xCF\xD3\n");

    let mut sorted: Vec<(u32, &Object)> = c.bodies.iter().map(|(n, obj)| (*n, obj)).collect();
    sorted.sort_by_key(|(n, _)| *n);

    let remap = |old: IndirectRef| c.map.get(&(old.number, old.generation)).copied();

    let mut offsets: Vec<u64> = Vec::with_capacity(sorted.len() + 1);
    offsets.push(0); // object 0 is the free head
    for (new_num, obj) in &sorted {
        offsets.push(out.len() as u64);
        write_indirect_object(*new_num, obj, &remap, &mut out)?;
        if out.len() as u64 > limits.max_output_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: limits.max_output_bytes,
            });
        }
    }

    // xref — dense numbering gives exactly one subsection 0..=N.
    let size = sorted.len() as u32 + 1;
    let xref_pos = out.len() as u64;
    out.extend_from_slice(b"xref\n");
    out.extend_from_slice(format!("0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (i, _) in sorted.iter().enumerate() {
        let off = offsets[i + 1];
        if off >= 10_000_000_000 {
            return Err(Error::LimitExceeded {
                limit: "classic xref offset width (10 digits)",
                max: 10_000_000_000,
            });
        }
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }

    // trailer — /Size and only the root references we renumbered.
    // /Prev, /XRefStm and other save-history keys are not carried into
    // a single-revision file; /ID is written for 2.0 only (required
    // there), so 1.x output is unchanged.
    let id = (version >= (2, 0))
        .then(|| trailer_id(doc, &out[..xref_pos as usize], !overrides.strip_info));
    out.extend_from_slice(b"trailer\n<<\n");
    out.extend_from_slice(format!("/Size {size}\n").as_bytes());
    let root_new = c
        .kinds
        .iter()
        .find(|(_, k)| **k == RootKind::Root)
        .map(|(n, _)| *n);
    if let Some(r) = root_new {
        out.extend_from_slice(format!("/Root {r} 0 R\n").as_bytes());
    }
    let info_new = c
        .kinds
        .iter()
        .find(|(_, k)| **k == RootKind::Info)
        .map(|(n, _)| *n);
    if let Some(i) = info_new {
        out.extend_from_slice(format!("/Info {i} 0 R\n").as_bytes());
    }
    if let Some((second, first)) = id {
        let hex = |b: &[u8]| b.iter().map(|c| format!("{c:02X}")).collect::<String>();
        out.extend_from_slice(format!("/ID [<{}> <{}>]\n", hex(&first), hex(&second)).as_bytes());
    }
    out.extend_from_slice(b">>\n");
    out.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());
    // The cap covers the whole file: a carried trailer /ID can be larger
    // than the objects.
    if out.len() as u64 > limits.max_output_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_output_bytes",
            max: limits.max_output_bytes,
        });
    }
    let change = crate::rewrite_policy::VersionChange {
        declared,
        written: version,
        reason: (version != declared)
            .then_some(crate::rewrite_policy::VersionReason::OptionalContent),
    };
    Ok((out, c.map, change))
}

struct Collector<'a> {
    doc: &'a Document,
    /// `(obj, gen) → replacement body` consulted before `doc.resolve`.
    overlay: &'a HashMap<(u32, u16), Object>,
    /// `(num, gen)` the edit session allocated as *new* synthetic
    /// objects. An *input* reference to one of these is a dangling
    /// ref, not an edit ref — it must not resolve to a synthetic body.
    synth_numbers: &'a HashSet<(u32, u16)>,
    limits: &'a Limits,
    /// `(old number, old gen) → new number`.
    map: HashMap<(u32, u16), u32>,
    /// `new number → resolved + ref-rewritten body`.
    bodies: HashMap<u32, Object>,
    /// `new number → trailer slot` (root-level only).
    kinds: HashMap<u32, RootKind>,
    next_number: u32,
    walked: usize,
}

impl<'a> Collector<'a> {
    /// Assign `old_ref` a new number (if unseen), resolve its body,
    /// recursively renumber every ref inside the body, then store it.
    /// Shared refs and genuine indirect cycles both hit the `map`
    /// early-return — no stack needed for correctness.
    ///
    /// `ref_from_overlay` is true when the reference was found inside
    /// an overlay (edit-created) body — those are edit refs and may
    /// resolve to synthetic objects. A reference found in an *input*
    /// body that happens to land on a synthetic number is a dangling
    /// input ref, not an edit ref: it goes through `doc.resolve` (null
    /// if undefined, §7.3.10) and never resolves to a synthetic body.
    fn walk(
        &mut self,
        old_ref: IndirectRef,
        kind: RootKind,
        depth: usize,
        ref_from_overlay: bool,
    ) -> Result<(), Error> {
        if depth > self.limits.max_rewrite_walk_depth {
            return Err(Error::LimitExceeded {
                limit: "max_rewrite_walk_depth",
                max: self.limits.max_rewrite_walk_depth as u64,
            });
        }
        if self.walked >= self.limits.max_output_objects {
            return Err(Error::LimitExceeded {
                limit: "max_output_objects",
                max: self.limits.max_output_objects as u64,
            });
        }
        self.walked += 1;

        let key = (old_ref.number, old_ref.generation);
        if self.map.contains_key(&key) {
            return Ok(());
        }
        let new_num = self.next_number;
        self.next_number += 1;
        self.map.insert(key, new_num);
        self.kinds.insert(new_num, kind);

        // Guard: an input-side ref to a synthetic number is a dangling
        // input reference — resolve via the document only (null if
        // undefined), never via the overlay.
        let overlay_body = if ref_from_overlay {
            self.overlay.get(&key)
        } else if self.synth_numbers.contains(&key) {
            None // dangling input ref — do not repair via overlay
        } else {
            self.overlay.get(&key)
        };
        let (mut obj, from_overlay) = match overlay_body {
            // Overlay wins — the editor's synthetic/replaced body.
            Some(o) => (o.clone(), true),
            // §7.3.10: an undefined object is the null object (written
            // as `n 0 obj null`, recorded as a warning); a defined one
            // that cannot be read stays an error.
            None => (self.doc.resolve(old_ref)?, false),
        };
        self.scan_and_collect(&mut obj, depth, from_overlay)?;
        self.bodies.insert(new_num, obj);
        Ok(())
    }

    /// Walk every `Ref` inside `obj` so its referent gets a new
    /// number — the `Ref` itself is left untouched; the serializer's
    /// `remap` performs the old→new translation at write time.
    /// Doing the rewrite here *and* in `remap` would double-translate.
    ///
    /// `from_overlay` is true while scanning an overlay (edit-created)
    /// body: refs inside it are edit refs and may resolve to
    /// synthetics. Refs inside an *input* body are input refs — one
    /// landing on a synthetic number is a dangling input ref.
    fn scan_and_collect(
        &mut self,
        obj: &mut Object,
        depth: usize,
        from_overlay: bool,
    ) -> Result<(), Error> {
        match obj {
            Object::Array(items) => {
                for item in items.iter_mut() {
                    self.scan_and_collect(item, depth + 1, from_overlay)?;
                }
            }
            Object::Dict(d) => {
                for v in d.values_mut() {
                    self.scan_and_collect(v, depth + 1, from_overlay)?;
                }
            }
            Object::Stream { dict, .. } => {
                for (k, v) in dict.iter_mut() {
                    // /Length is recomputed as a direct integer at
                    // write time — an indirect /Length ref collected
                    // here would emit an orphan object nothing in the
                    // output references. If the same object is also
                    // referenced elsewhere it is still collected from
                    // that site (map dedupe keeps it reachable); only
                    // the stream-dict slot is skipped.
                    if k.as_slice() == b"Length" {
                        continue;
                    }
                    self.scan_and_collect(v, depth + 1, from_overlay)?;
                }
            }
            Object::Ref(r) => {
                self.walk(*r, RootKind::Ordinary, depth + 1, from_overlay)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// The effective input version: `max(header_version, catalog_version)`.
///
/// Guarantees we can write: the file's declared version is only
/// emitted when we actually know it. `None` on the header (malformed
/// `%PDF-M.m`) is a **refusal**, not a silent `1.7` — the writer
/// must not fabricate a version for a file whose own declaration it
/// could not read. A catalog `/Version` that is present but not a
/// valid `M.m` name is likewise refused rather than ignored.
/// Above [`MAX_VERSION`] is refused.
/// Returns the declared version and the one to write (raised to 1.5 for
/// optional content, the only reason the two differ).
fn effective_version(doc: &Document) -> Result<(Version, Version), Error> {
    let hdr = doc.header_version().ok_or_else(|| Error::Unsupported {
        feature: "PDF version",
        detail: "header %PDF- token is malformed — cannot determine ".to_string()
            + "the version to declare on output",
    })?;

    // Catalog /Version: absent → use header; present but not a valid
    // name token → refuse (a malformed version is not "no version").
    let cat = doc.catalog_version()?;
    let declared = match cat {
        Some(c) if c > hdr => c,
        _ => hdr,
    };
    let mut effective = declared;
    // Optional content is a PDF 1.5 feature (§8.11): a file that
    // carries `/OCProperties` declares at least 1.5, whatever the input
    // claimed, so the output never declares less than it contains.
    if doc.catalog().contains_key(b"OCProperties".as_slice()) && effective < (1, 5) {
        effective = (1, 5);
    }
    if effective > MAX_VERSION {
        return Err(Error::Unsupported {
            feature: "PDF version",
            detail: format!(
                "document declares PDF {}.{} — the rewriter writes up to {}.{}",
                effective.0, effective.1, MAX_VERSION.0, MAX_VERSION.1
            ),
        });
    }
    Ok((declared, effective))
}

/// The trailer `/ID` of a PDF 2.0 output (required, ISO 32000-2
/// Table 15): the input's permanent first identifier when it has one
/// (§14.4: it is set when the file is created and kept), the second —
/// which changes with every modification — the MD5 of the written body,
/// so the same input gives the same output. When `/Info` is stripped
/// (metadata removal) the input's identifier is not carried either: both
/// are the digest.
fn trailer_id(doc: &Document, body: &[u8], carry: bool) -> ([u8; 16], Vec<u8>) {
    let digest = crate::crypt::md5(&[body]);
    let first = carry
        .then(|| doc.trailer().get(b"ID".as_slice()))
        .flatten()
        .and_then(|id| deref_chain(doc, id).ok())
        .and_then(|id| match id {
            Object::Array(a) if a.len() == 2 => match deref_chain(doc, &a[0]).ok()? {
                Object::String(s) if !s.is_empty() => Some(s),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| digest.to_vec());
    (digest, first)
}

/// Refusals — checked before any collection or output begins.
/// Walk a field/widget array (and `/Kids`, depth ≤ 32) for a
/// signature field that actually holds a signature: `/FT /Sig`
/// (possibly inherited) with a `/V` that is a dictionary (normally
/// carrying `/ByteRange` and `/Contents`). Returns a short description
/// of the first one found.
fn signed_field_in(
    doc: &Document,
    fields: &[Object],
    depth: usize,
    seen: &mut std::collections::HashSet<crate::object::IndirectRef>,
) -> Result<Option<String>, Error> {
    if depth > 32 {
        return Ok(None);
    }
    for f in fields {
        if let Some(r) = f.as_ref() {
            if !seen.insert(r) {
                continue;
            }
        }
        let f = deref_chain(doc, f)?;
        let Some(fd) = f.as_dict() else { continue };
        // /FT may be inherited from the parent field; a Kids-only
        // node carries none and is just walked.
        let mut ft = fd
            .get(b"FT".as_slice())
            .and_then(|o| o.as_name())
            .map(|n| n.to_vec());
        if ft.is_none() {
            if let Some(p) = fd.get(b"Parent".as_slice()) {
                if let Ok(p) = deref_chain(doc, p) {
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
                let v = deref_chain(doc, v)?;
                if let Some(vd) = v.as_dict() {
                    let name = fd
                        .get(b"T".as_slice())
                        .and_then(|o| o.as_string())
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .unwrap_or_else(|| "unnamed".into());
                    let what = if vd.contains_key(b"ByteRange".as_slice()) {
                        format!("field {name:?} with /ByteRange")
                    } else {
                        format!("field {name:?} with a signature dictionary")
                    };
                    return Ok(Some(what));
                }
            }
        }
        if let Some(kids) = fd.get(b"Kids".as_slice()) {
            let kids = deref_chain(doc, kids)?;
            if let Some(k) = kids.as_array() {
                if let Some(w) = signed_field_in(doc, k, depth + 1, seen)? {
                    return Ok(Some(w));
                }
            }
        }
    }
    Ok(None)
}

/// `obj` with every reference followed. [`Document::deref`] follows a
/// chain of indirect objects, but a compressed object whose value is a
/// reference comes back as that reference; the refusal checks must see
/// the object a reader would, so they follow it here too.
pub(crate) fn deref_chain(doc: &Document, obj: &Object) -> Result<Object, Error> {
    let mut o = doc.deref(obj)?;
    for _ in 0..MAX_REF_CHAIN {
        match o {
            Object::Ref(r) => o = doc.resolve(r)?,
            other => return Ok(other),
        }
    }
    Err(Error::CycleDetected {
        context: "reference chain through object streams",
    })
}

/// Hops [`deref_chain`] follows before it calls the chain a cycle.
const MAX_REF_CHAIN: usize = 64;

/// What XML Signature (XML-DSig 1.0, 1.1, 2.0) namespaces have in
/// common. An element is an XML signature only in that namespace, and
/// a namespace can only be bound by writing its URI in an `xmlns`
/// attribute — where character references may spell it, so they are
/// expanded first. Matched case-insensitively anywhere, comments and
/// CDATA included: a mention outside markup is refused too.
const XMLDSIG_MARK: &[u8] = b"xmldsig";

/// Encodings (lower case; a trailing `*` matches any suffix) in which
/// every ASCII character is the ASCII byte, so markup and the namespace
/// URI are visible as bytes.
const ASCII_COMPATIBLE: &[&str] = &[
    "utf-8",
    "utf8",
    "us-ascii",
    "ascii",
    "iso-8859-*",
    "latin*",
    "windows-125*",
    "cp125*",
    "shift_jis",
    "shift-jis",
    "sjis",
    "windows-31j",
    "cp932",
    "euc-*",
    "gb2312",
    "gbk",
    "gb18030",
    "big5*",
    "iso-2022-*",
    "koi8-*",
];

/// An XFA packet's decoded bytes as ASCII-compatible text for the
/// signature scan, and whether they were UTF-16: UTF-16 (byte order
/// mark, or `<` as the first non-blank code unit) narrowed to one byte
/// per code unit (non-ASCII → 0xFF), a UTF-8 byte order mark removed.
/// UTF-32, EBCDIC and NUL bytes in no known encoding cannot be read:
/// the reason is returned.
fn xfa_text(raw: &[u8]) -> Result<(std::borrow::Cow<'_, [u8]>, bool), String> {
    use std::borrow::Cow;
    if raw.starts_with(&[0, 0, 0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE, 0, 0]) {
        return Err("UTF-32 content".into());
    }
    if raw.starts_with(&[0x4C, 0x6F, 0xA7, 0x94]) {
        return Err("EBCDIC content".into());
    }
    let first_unit = |le: bool| {
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|&[a, b]| if le { (a, b) } else { (b, a) })
            .find(|&(lo, hi)| !(hi == 0 && matches!(lo, b' ' | b'\t' | b'\r' | b'\n')))
    };
    let utf16 = if raw.starts_with(&[0xFF, 0xFE]) {
        Some((true, 2))
    } else if raw.starts_with(&[0xFE, 0xFF]) {
        Some((false, 2))
    } else if raw.contains(&0) {
        // XML text holds no U+0000: NUL bytes mean a wide encoding.
        if first_unit(true) == Some((b'<', 0)) {
            Some((true, 0))
        } else if first_unit(false) == Some((b'<', 0)) {
            Some((false, 0))
        } else {
            return Err("NUL bytes in no recognised encoding".into());
        }
    } else {
        None
    };
    let text: Cow<'_, [u8]> = match utf16 {
        Some((le, bom)) => {
            let narrowed: Vec<u8> = raw[bom..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[a, b]| {
                    let (lo, hi) = if le { (a, b) } else { (b, a) };
                    if hi == 0 && lo < 0x80 {
                        lo
                    } else {
                        0xFF
                    }
                })
                .collect();
            if narrowed.contains(&0) {
                return Err("U+0000 in UTF-16 content (UTF-32?)".into());
            }
            Cow::Owned(narrowed)
        }
        None => Cow::Borrowed(raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw)),
    };
    Ok((text, utf16.is_some()))
}

/// Whether text from [`xfa_text`] can be checked completely: every XML
/// declaration names an encoding the bytes are in — UTF-16 only if
/// `wide` (the bytes were UTF-16), [`ASCII_COMPATIBLE`] only if
/// `narrow` (they were not; both for the packets together) — and there
/// is no DTD (its entities can build markup and bind namespaces).
/// Otherwise the reason.
fn xfa_checkable(text: &[u8], wide: bool, narrow: bool) -> Result<(), String> {
    let mut at = 0;
    while let Some(i) = find(&text[at..], b"<?xml") {
        // A declaration is short: search its end within 512 bytes only
        // (linear in the number of declarations).
        let decl = &text[at + i..];
        let decl = &decl[..decl.len().min(512)];
        let decl = &decl[..find(decl, b"?>").unwrap_or(decl.len())];
        if let Some(enc) = declared_encoding(decl) {
            let enc = enc.to_ascii_lowercase();
            let utf16_name = enc.starts_with("utf-16") || enc.starts_with("ucs-2");
            let ascii = ASCII_COMPATIBLE.iter().any(|p| match p.strip_suffix('*') {
                Some(prefix) => enc.starts_with(prefix),
                None => enc == *p,
            });
            let ok = (wide && utf16_name) || (narrow && ascii);
            if !ok {
                return Err(format!("declared encoding {enc:?}"));
            }
        }
        at += i + 5;
    }
    if find_ci(text, b"<!DOCTYPE").is_some() || find_ci(text, b"<!ENTITY").is_some() {
        return Err("a DTD".into());
    }
    Ok(())
}

/// The `encoding="…"` value of an XML declaration.
fn declared_encoding(decl: &[u8]) -> Option<String> {
    let i = find(decl, b"encoding")? + b"encoding".len();
    let rest = decl[i..]
        .trim_ascii_start()
        .strip_prefix(b"=")?
        .trim_ascii_start();
    let quote = *rest.first().filter(|q| matches!(q, b'"' | b'\''))?;
    let end = rest[1..].iter().position(|c| *c == quote)?;
    Some(String::from_utf8_lossy(&rest[1..1 + end]).into_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// `text` with numeric character references (`&#…;`, `&#x…;`)
/// expanded: ASCII ones to their byte, others to 0xFF.
fn expand_char_refs(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'&' && text.get(i + 1) == Some(&b'#') {
            let (radix, start) = match text.get(i + 2) {
                Some(b'x' | b'X') => (16, i + 3),
                _ => (10, i + 2),
            };
            // Any number of digits: leading zeros are valid XML.
            let digits = text[start..]
                .iter()
                .take_while(|c| (**c as char).is_digit(radix))
                .count();
            if digits > 0 && text.get(start + digits) == Some(&b';') {
                let v = text[start..start + digits].iter().fold(0u32, |v, c| {
                    let d = (*c as char).to_digit(radix).unwrap_or(0);
                    v.saturating_mul(radix).saturating_add(d)
                });
                out.push(if v < 0x80 { v as u8 } else { 0xFF });
                i = start + digits + 1;
                continue;
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// An XFA packet (or the single XFA stream) whose content holds an XML
/// signature: a description of the first one. Each packet is checked
/// alone and then all of them as the one XML document they form — a
/// character reference, a DTD, a declaration or the namespace can
/// straddle a boundary — by the same steps: [`xfa_checkable`],
/// character references expanded, the namespace searched. A packet
/// that cannot be decoded or read completely ([`xfa_text`]), whose data
/// is in an external file, or a document that fails [`xfa_checkable`]
/// cannot be checked, so it is an error rather than silently treated as
/// unsigned.
fn xfa_xml_signature(doc: &Document, xfa: &Object) -> Result<Option<String>, Error> {
    let unsupported = |what: &str, why: String| Error::Unsupported {
        feature: "XFA packet",
        detail: format!("{what} could not be checked for an XML signature ({why})"),
    };
    // Checked text (not yet expanded) and whether it was UTF-16.
    let text = |o: &Object, what: &str| -> Result<Option<(Vec<u8>, bool)>, Error> {
        let Object::Stream { dict, data } = o else {
            return Ok(None);
        };
        if dict.contains_key(b"F".as_slice()) {
            return Err(unsupported(what, "stream data in an external file".into()));
        }
        let decoded = doc
            .decode_stream(dict, data)
            .map_err(|e| unsupported(what, e.to_string()))?;
        let (t, wide) = xfa_text(&decoded).map_err(|why| unsupported(what, why))?;
        xfa_checkable(&t, wide, !wide).map_err(|why| unsupported(what, why))?;
        Ok(Some((t.into_owned(), wide)))
    };
    let signed = |t: &[u8]| find_ci(&expand_char_refs(t), XMLDSIG_MARK).is_some();
    match deref_chain(doc, xfa)? {
        Object::Array(items) => {
            let mut name = String::from("(unnamed)");
            let (mut whole, mut any_wide, mut any_narrow) = (Vec::new(), false, false);
            for it in &items {
                match deref_chain(doc, it)? {
                    Object::String(s) => name = String::from_utf8_lossy(&s).into_owned(),
                    stream @ Object::Stream { .. } => {
                        let what = format!("packet {name:?}");
                        if let Some((t, wide)) = text(&stream, &what)? {
                            if signed(&t) {
                                return Ok(Some(what));
                            }
                            whole.extend_from_slice(&t);
                            any_wide |= wide;
                            any_narrow |= !wide;
                        }
                    }
                    _ => {}
                }
            }
            let what = "the packets together";
            xfa_checkable(&whole, any_wide, any_narrow).map_err(|why| unsupported(what, why))?;
            Ok(signed(&whole).then(|| "packets (across a packet boundary)".to_string()))
        }
        stream @ Object::Stream { .. } => {
            let t = text(&stream, "stream")?.map(|t| t.0).unwrap_or_default();
            Ok(signed(&t).then(|| "stream".to_string()))
        }
        _ => Ok(None),
    }
}

/// The number of `script` elements in checked XFA text: `<`, an optional
/// `NCName:` prefix, the local name `script`, then white space, `>` or
/// `/` (`docs/plans/rewrite-policy.md` §3.1). Any match counts — a
/// `script`-shaped string the rule cannot delimit (in a comment, an
/// attribute) is reported as a script: an over-report, never an
/// under-report.
pub(crate) fn count_xfa_scripts(text: &[u8]) -> u32 {
    let name_start = |c: u8| c.is_ascii_alphabetic() || c == b'_' || c >= 0x80;
    let name_char = |c: u8| name_start(c) || c.is_ascii_digit() || c == b'.' || c == b'-';
    let mut n = 0u32;
    let mut i = 0;
    while let Some(at) = text[i..].iter().position(|c| *c == b'<') {
        let mut j = i + at + 1;
        let start = j;
        while j < text.len() && name_char(text[j]) {
            j += 1;
        }
        let mut local = &text[start..j];
        if j < text.len() && text[j] == b':' && !local.is_empty() && name_start(local[0]) {
            let s2 = j + 1;
            j = s2;
            while j < text.len() && name_char(text[j]) {
                j += 1;
            }
            local = &text[s2..j];
        }
        let delimited = matches!(
            text.get(j),
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        );
        if local == b"script" && delimited {
            n = n.saturating_add(1);
        }
        i = i + at + 1;
    }
    n
}

/// XFA scripts (`docs/plans/rewrite-policy.md` §3.1): for each packet
/// stream (by position among the streams; `Xfa { packet: None }` for a
/// single stream) its object and number of `script` elements, then those
/// seen only when the packets are read together. The text is the one the
/// signature check reads ([`xfa_text`], character references expanded).
pub(crate) fn xfa_scripts(
    doc: &Document,
    xfa: &Object,
) -> Result<Vec<(crate::rewrite_policy::ActionOwner, Option<IndirectRef>, u32)>, Error> {
    use crate::rewrite_policy::ActionOwner;
    let text = |o: &Object| -> Result<Option<Vec<u8>>, Error> {
        let Object::Stream { dict, data } = o else {
            return Ok(None);
        };
        let decoded = doc.decode_stream(dict, data)?;
        let (t, _) = xfa_text(&decoded).map_err(|why| Error::Unsupported {
            feature: "XFA packet",
            detail: why,
        })?;
        Ok(Some(expand_char_refs(&t)))
    };
    let mut out = Vec::new();
    match deref_chain(doc, xfa)? {
        Object::Array(items) => {
            let (mut whole, mut sum, mut packet) = (Vec::new(), 0u32, 0u32);
            for it in &items {
                let v = deref_chain(doc, it)?;
                if let Some(t) = text(&v)? {
                    let n = count_xfa_scripts(&t);
                    sum = sum.saturating_add(n);
                    out.push((
                        ActionOwner::Xfa {
                            packet: Some(packet),
                        },
                        it.as_ref(),
                        n,
                    ));
                    whole.extend_from_slice(&t);
                    packet += 1;
                }
            }
            let across = count_xfa_scripts(&whole).saturating_sub(sum);
            out.push((ActionOwner::XfaAcross, None, across));
        }
        stream @ Object::Stream { .. } => {
            let n = text(&stream)?.map(|t| count_xfa_scripts(&t)).unwrap_or(0);
            out.push((ActionOwner::Xfa { packet: None }, xfa.as_ref(), n));
        }
        _ => {}
    }
    Ok(out)
}

/// Every reference is followed with the §7.3.10 rule (an undefined
/// object reads as null, i.e. as absent); an object that is defined but
/// unreadable fails the check rather than being skipped.
///
/// `carry` (`ExecutableContentPolicy::Carry`) lifts the three
/// document-level executable-content refusals (catalog `/AA`, a
/// JavaScript `/OpenAction`, `/Names /JavaScript`) and nothing else: the
/// catalog `/RichMedia` refusal is `MultimediaPolicy::Refuse`, and the
/// signature refusals apply under every policy.
fn check_refusals(doc: &Document, carry: bool) -> Result<(), Error> {
    let catalog = doc.catalog();

    // Signed documents: a signature/lock dictionary in /Perms claims
    // an integrity check a rewrite cannot preserve.
    if let Some(perms_obj) = catalog.get(b"Perms".as_slice()) {
        let perms = deref_chain(doc, perms_obj)?;
        if let Some(d) = perms.as_dict() {
            for key in [&b"DocMDP"[..], &b"UR"[..], &b"UR3"[..]] {
                if d.contains_key(key) {
                    return Err(Error::SignaturePolicy {
                        rule: crate::error::SignatureRule::RewriteOfSignedDocument,
                        detail: format!(
                            "/Perms /{} present — rewrite would invalidate the signature",
                            String::from_utf8_lossy(key)
                        ),
                    });
                }
            }
        }
    }

    // AcroForm: a filled /Sig field (FT=/Sig with a /V entry) means
    // "actually signed" — refuse. Blank unsigned fields are ordinary
    // widgets and pass through. XFA is carried verbatim unless a packet
    // holds an XML signature.
    if let Some(af_obj) = catalog.get(b"AcroForm".as_slice()) {
        let af = deref_chain(doc, af_obj)?;
        if let Some(d) = af.as_dict() {
            if let Some(xfa) = d.get(b"XFA".as_slice()) {
                if let Some(what) = xfa_xml_signature(doc, xfa)? {
                    return Err(Error::SignaturePolicy {
                        rule: crate::error::SignatureRule::RewriteOfSignedDocument,
                        detail: format!(
                            "XFA {what} holds an XML signature (the XML-DSig namespace) — \
                             what it signs is not evaluated, so the document is treated as \
                             signed"
                        ),
                    });
                }
            }
            if let Some(fields_obj) = d.get(b"Fields".as_slice()) {
                let fields = deref_chain(doc, fields_obj)?;
                if let Some(fields) = fields.as_array() {
                    let mut seen = std::collections::HashSet::new();
                    if let Some(what) = signed_field_in(doc, fields, 0, &mut seen)? {
                        return Err(Error::Unsupported {
                            feature: "signed signature field",
                            detail: format!(
                                "/AcroForm contains a signed /Sig field ({what}) — a rewrite \
                                 would invalidate the signature"
                            ),
                        });
                    }
                }
            }
        }
    }
    // A signature value can also hang off a widget that is not in
    // /AcroForm /Fields (broken forms, or signatures added by tools
    // that only touched /Annots): any page annotation with /FT /Sig
    // and a /V carrying /ByteRange counts.
    for page in doc.pages() {
        let Some(pref) = page.object_ref() else {
            continue;
        };
        let Ok(pd) = doc.resolve(pref) else {
            continue;
        };
        let Some(pd) = pd.as_dict() else { continue };
        let Some(annots) = pd.get(b"Annots".as_slice()) else {
            continue;
        };
        let annots = deref_chain(doc, annots)?;
        if let Some(arr) = annots.as_array() {
            let mut seen = std::collections::HashSet::new();
            if let Some(what) = signed_field_in(doc, arr, 0, &mut seen)? {
                return Err(Error::Unsupported {
                    feature: "signed signature field",
                    detail: format!(
                        "page {} carries a signed /Sig widget ({what}) — a rewrite would \
                         invalidate the signature",
                        page.index() + 1
                    ),
                });
            }
        }
    }

    // ---- Named destinations: preserved --
    //
    // The catalog `/Names` → `/Dests` name tree, the legacy catalog
    // `/Dests` dictionary, and every name/string operand that refers
    // into them (link `/Dest`, `/GoTo` `/D`, `/OpenAction` `/D`) are
    // ordinary reachable objects: the collector copies the whole tree
    // (`/Kids`, `/Names`, `/Limits`, destination arrays and dicts)
    // verbatim with renumbered references, so the name → page binding
    // survives unchanged. `crate::dests::named_destinations` reads the
    // structure before and after a rewrite and the round-trip tests
    // assert the two agree — that is the integrity check, not a
    // refusal. A dangling name (no entry in the tree) is the input's
    // own defect and is copied as-is, never repaired or dropped.
    //
    // Only the tree's *shape* is validated here, bounded by the page
    // tree limits, so a cyclic or oversized name tree is refused
    // before any output rather than looping in the collector.
    crate::dests::named_destinations(doc, &Limits::default())?;

    // Optional content: carried verbatim once its structure is known to
    // be well formed.
    crate::optcontent::validate_for_rewrite(doc)?;

    // Active/executable structures outside the rewrite profile.
    for key in [&b"RichMedia"[..], &b"AA"[..]] {
        if carry && key == b"AA" {
            continue;
        }
        if catalog.contains_key(key) {
            return Err(Error::Unsupported {
                feature: "catalog entry outside profile",
                detail: format!(
                    "/{} is outside the rewrite profile",
                    String::from_utf8_lossy(key)
                ),
            });
        }
    }

    // Open-action / Names JavaScript: refuse executable payloads that
    // a rewrite could silently retain.
    if carry {
        return Ok(());
    }
    if let Some(oa) = catalog.get(b"OpenAction".as_slice()) {
        let oa = deref_chain(doc, oa)?;
        if let Some(d) = oa.as_dict() {
            let s = d.get(b"S".as_slice()).and_then(|o| o.as_name());
            if s == Some(b"JavaScript") {
                return Err(Error::Unsupported {
                    feature: "OpenAction JavaScript",
                    detail: "/OpenAction /S /JavaScript is outside the rewrite profile".into(),
                });
            }
        }
    }
    if let Some(names_obj) = catalog.get(b"Names".as_slice()) {
        let names = deref_chain(doc, names_obj)?;
        if let Some(d) = names.as_dict() {
            if d.contains_key(b"JavaScript".as_slice()) {
                return Err(Error::Unsupported {
                    feature: "JavaScript name tree",
                    detail: "/Names /JavaScript is outside the rewrite profile".into(),
                });
            }
        }
    }
    Ok(())
}
