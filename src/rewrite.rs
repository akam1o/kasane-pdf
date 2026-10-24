//! Rebuild a loaded [`Document`] as a fresh, single-revision PDF —
//! milestone **M8** "full rewrite / round-trip".
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
//! * `/AcroForm` `/XFA` — outside the M8 preservation profile.
//! * catalog `/OCProperties`, `/RichMedia`, `/AA` — outside profile.
//!
//! Everything else reachable is copied verbatim — unknown dictionary
//! keys are preserved, never silently dropped. That includes named
//! destinations: the `/Names` → `/Dests` name tree, the legacy
//! catalog `/Dests` dictionary and every `/Dest`, `/GoTo` `/D` or
//! `/OpenAction` operand that names into them travel as ordinary
//! objects with renumbered references, so the name → page binding
//! is identical before and after (see [`crate::dests`]).

use crate::document::Document;
use crate::error::Error;
use crate::object::{IndirectRef, Object};
use crate::serialize::write_indirect_object;
use std::collections::{HashMap, HashSet};

use crate::limits::Limits;

/// Header version — M8 writes classic xref + ObjStm-free output, so
/// 1.7 covers every feature it can emit. A catalog `/Version` of 1.4
/// through 1.7 is honoured; 1.3 or below means the catalog claims
/// less than the header — we still output 1.7 so the file's declared
/// version never falls below the structures it contains.
/// A document that declares 2.0 (header or catalog) is refused: M8
/// does not translate 2.0 content down to 1.7.
const BASE_VERSION: (u8, u8) = (1, 7);

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
/// that would let a malformed/dangling input ref silently succeed.
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
    let _budget = doc.decoded_budget_scope();
    check_refusals(doc)?;

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
    let version = effective_version(doc)?;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-");
    out.extend_from_slice(version.as_bytes());
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
    // /ID, /Prev, /XRefStm and other save-history keys are not
    // carried into a single-revision file.
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
    out.extend_from_slice(b">>\n");
    out.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());
    Ok(out)
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
    /// input ref, not an edit ref: it must go through `doc.resolve`
    /// and fail rather than silently resolve to a synthetic body.
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
        // input reference — resolve via the document only (→ the
        // MissingReference it should be), never via the overlay.
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
/// `2.0` and above are refused: M8 does not translate 2.0-only
/// features down, so claiming `1.7` would silently break them.
fn effective_version(doc: &Document) -> Result<String, Error> {
    let hdr = doc.header_version().ok_or_else(|| Error::Unsupported {
        feature: "PDF version",
        detail: "header %PDF- token is malformed — cannot determine ".to_string()
            + "the version to declare on output",
    })?;

    // Catalog /Version: absent → use header; present but not a valid
    // name token → refuse (a malformed version is not "no version").
    let cat = doc.catalog_version()?;
    let effective = match cat {
        Some(c) if c > hdr => c,
        _ => hdr,
    };
    // Guaranteed-write ceiling: 1.7. Anything above is Unsupported.
    if effective > BASE_VERSION {
        return Err(Error::Unsupported {
            feature: "PDF version",
            detail: format!(
                "document declares PDF {}.{} — cannot rewrite as ≤{}.{}",
                effective.0, effective.1, BASE_VERSION.0, BASE_VERSION.1
            ),
        });
    }
    Ok(format!("{}.{}", effective.0, effective.1))
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
        let f = doc.deref(f)?;
        let Some(fd) = f.as_dict() else { continue };
        // /FT may be inherited from the parent field; a Kids-only
        // node carries none and is just walked.
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
                let v = doc.deref(v)?;
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
            let kids = doc.deref(kids)?;
            if let Some(k) = kids.as_array() {
                if let Some(w) = signed_field_in(doc, k, depth + 1, seen)? {
                    return Ok(Some(w));
                }
            }
        }
    }
    Ok(None)
}

fn check_refusals(doc: &Document) -> Result<(), Error> {
    let catalog = doc.catalog();

    // Signed documents: a signature/lock dictionary in /Perms claims
    // an integrity check a rewrite cannot preserve.
    if let Some(perms_obj) = catalog.get(b"Perms".as_slice()) {
        let perms = doc.deref(perms_obj)?;
        if let Some(d) = perms.as_dict() {
            for key in [&b"DocMDP"[..], &b"UR"[..], &b"UR3"[..]] {
                if d.contains_key(key) {
                    return Err(Error::Unsupported {
                        feature: "signed document",
                        detail: format!(
                            "/Perms /{} present — rewrite would invalidate the signature",
                            String::from_utf8_lossy(key)
                        ),
                    });
                }
            }
        }
    }

    // AcroForm: /XFA is outside the profile; a filled /Sig field
    // (FT=/Sig with a /V entry) means "actually signed" — refuse.
    // Blank unsigned fields are ordinary widgets and pass through.
    if let Some(af_obj) = catalog.get(b"AcroForm".as_slice()) {
        let af = doc.deref(af_obj)?;
        if let Some(d) = af.as_dict() {
            if d.contains_key(b"XFA".as_slice()) {
                return Err(Error::Unsupported {
                    feature: "XFA form",
                    detail: "/AcroForm /XFA is outside the rewrite profile".into(),
                });
            }
            if let Some(fields_obj) = d.get(b"Fields".as_slice()) {
                let fields = doc.deref(fields_obj)?;
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
        let Ok(pd) = doc.resolve(pref) else { continue };
        let Some(pd) = pd.as_dict() else { continue };
        let Some(annots) = pd.get(b"Annots".as_slice()) else {
            continue;
        };
        let annots = doc.deref(annots)?;
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

    // ---- Named destinations: preserved (M8.1 decision B revisited) --
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

    // Active/executable structures outside the M8 profile.
    for key in [&b"OCProperties"[..], &b"RichMedia"[..], &b"AA"[..]] {
        if catalog.contains_key(key) {
            return Err(Error::Unsupported {
                feature: "catalog entry outside profile",
                detail: format!(
                    "/{} is outside the M8 rewrite profile",
                    String::from_utf8_lossy(key)
                ),
            });
        }
    }

    // Open-action / Names JavaScript: refuse executable payloads that
    // a rewrite could silently retain.
    if let Some(oa) = catalog.get(b"OpenAction".as_slice()) {
        let oa = doc.deref(oa)?;
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
        let names = doc.deref(names_obj)?;
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
