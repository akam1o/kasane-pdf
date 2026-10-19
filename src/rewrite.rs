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
//! keys are preserved, never silently dropped.

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
    check_refusals(doc)?;

    // ---- Phase 1: collect reachable objects, assign new numbers ----
    let mut c = Collector {
        doc,
        overlay,
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
    if let Some(r) = trailer.get(b"Info".as_slice()).and_then(|o| o.as_ref()) {
        root_refs.push((r, RootKind::Info));
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
    let cat = catalog_version_num(doc)?;
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

/// Catalog `/Version` as `(major, minor)`.
/// `Ok(None)` when `/Version` is absent.
/// `Err(Unsupported)` when `/Version` is present but malformed — the
/// value is a real dictionary entry we cannot silently drop, and an
/// unparseable version cannot be compared to the header.
fn catalog_version_num(doc: &Document) -> Result<Option<(u8, u8)>, Error> {
    let name = match doc.catalog().get(b"Version".as_slice()) {
        Some(o) => o,
        None => return Ok(None),
    };
    // Spec §7.5.2: /Version is a name, e.g. /1.7. Anything else is
    // malformed input we must not pretend is absent.
    let raw = name.as_name().ok_or_else(|| Error::Unsupported {
        feature: "PDF version",
        detail: "catalog /Version is not a name object".to_string(),
    })?;
    let s = std::str::from_utf8(raw).map_err(|_| Error::Unsupported {
        feature: "PDF version",
        detail: "catalog /Version is not valid UTF-8".to_string(),
    })?;
    let (maj, min) = s.split_once('.').ok_or_else(|| Error::Unsupported {
        feature: "PDF version",
        detail: format!("catalog /Version /{s} is not M.m form"),
    })?;
    let maj: u8 = maj.parse().map_err(|_| Error::Unsupported {
        feature: "PDF version",
        detail: format!("catalog /Version /{s} has non-numeric major"),
    })?;
    let min: u8 = min.parse().map_err(|_| Error::Unsupported {
        feature: "PDF version",
        detail: format!("catalog /Version /{s} has non-numeric minor"),
    })?;
    Ok(Some((maj, min)))
}

/// Scan every reachable `/Annots` entry and `/OpenAction`/`/GoTo`
/// destination operands for a *named* destination — a name or
/// string instead of the required destination array. Refuses the
/// document before any output when one is found.
fn check_named_dests(doc: &Document) -> Result<(), Error> {
    // /OpenAction /D as a name/string is a named dest.
    if let Some(oa) = doc.catalog().get(b"OpenAction".as_slice()) {
        let oa = doc.deref(oa)?;
        if let Some(d) = oa.as_dict() {
            if let Some(dest) = d.get(b"D".as_slice()) {
                check_dest_operand(doc, dest, "/OpenAction /D")?;
            }
        } else {
            check_dest_operand(doc, &oa, "/OpenAction")?;
        }
    }
    for page in doc.pages() {
        if let Some(annots_obj) = page
            .object_ref()
            .and_then(|r| doc.resolve(r).ok())
            .and_then(|o| o.as_dict().cloned())
            .and_then(|d| d.get(b"Annots".as_slice()).cloned())
        {
            let annots = doc.deref(&annots_obj)?;
            if let Some(arr) = annots.as_array() {
                for a in arr {
                    let a = doc.deref(a)?;
                    if let Some(ad) = a.as_dict() {
                        if let Some(dest) = ad.get(b"Dest".as_slice()) {
                            check_dest_operand(doc, dest, "/Annots /Dest")?;
                        }
                        if let Some(action) = ad.get(b"A".as_slice()) {
                            let action = doc.deref(action)?;
                            if let Some(act_d) = action.as_dict() {
                                let s = act_d.get(b"S".as_slice()).and_then(|o| o.as_name());
                                if s == Some(b"GoTo") {
                                    if let Some(dest) = act_d.get(b"D".as_slice()) {
                                        check_dest_operand(doc, dest, "/Annots /A /GoTo /D")?;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// A destination operand must be an array (`[page /XYZ …]` or
/// `[page /Fit]`) — a `Name` or `String` means "look me up in the
/// name tree", which M8 does not implement.
fn check_dest_operand(doc: &Document, obj: &Object, context: &str) -> Result<(), Error> {
    match obj {
        Object::Name(n) | Object::String(n) => {
            return Err(Error::Unsupported {
                feature: "named destination",
                detail: format!(
                    "{context} contains named destination /{} — name trees are not resolved",
                    String::from_utf8_lossy(n)
                ),
            });
        }
        Object::Ref(_) => {
            // Resolve the ref once and re-check — an indirect /Dest
            // could still hold a name/string inside an array's slot.
            let resolved = doc.deref(obj)?;
            return check_dest_operand(doc, &resolved, context);
        }
        Object::Array(arr) => {
            // First element is the page ref — not a destination by
            // itself; subsequent elements are positional params.
            // A bare /Name or /String as the *first* element of the
            // array is still wrong (dest arrays start with a page).
            if let Some(Object::Name(n) | Object::String(n)) = arr.first() {
                return Err(Error::Unsupported {
                    feature: "named destination",
                    detail: format!(
                        "{context} destination array starts with /{} — not a page ref",
                        String::from_utf8_lossy(n)
                    ),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

/// Refusals — checked before any collection or output begins.
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
                if let Some(fields) = fields_obj.as_array() {
                    for f in fields {
                        let f = doc.deref(f)?;
                        let is_sig = f
                            .as_dict()
                            .and_then(|fd| fd.get(b"FT".as_slice()))
                            .and_then(|o| o.as_name())
                            == Some(b"Sig");
                        let filled = f
                            .as_dict()
                            .map(|fd| fd.contains_key(b"V".as_slice()))
                            .unwrap_or(false);
                        if is_sig && filled {
                            return Err(Error::Unsupported {
                                feature: "signed signature field",
                                detail: "/AcroForm contains a filled /Sig field — cannot rewrite"
                                    .into(),
                            });
                        }
                    }
                }
            }
        }
    }

    // ---- Named destinations: explicit refusal (M8.1 decision B) ----
    //
    // A named destination is a *name or string* that resolves through
    // a name tree (/Dests in /Names, or the legacy /Dests dict) — we
    // do not resolve or preserve that indirection. Direct destination
    // *arrays* (e.g. `/D [5 0 R /XYZ 0 200 null]`) are preserved.
    //
    // Structures checked (any reachable):
    //   * catalog /Dests dict          → refuse
    //   * catalog /Names /Dests tree   → refuse
    //   * link annot /Dest name/string → refuse
    //   * GoTo action /D name/string   → refuse
    //   * OpenAction /D name/string    → refuse
    // The name-tree Kids chain is also walked so a /Dests hidden
    // behind intermediate /Kids nodes is still caught.

    if catalog.contains_key(b"Dests".as_slice()) {
        return Err(Error::Unsupported {
            feature: "named destination",
            detail: "/Dests name tree is outside the M8 rewrite profile".into(),
        });
    }
    if let Some(names_obj) = catalog.get(b"Names".as_slice()) {
        let names = doc.deref(names_obj)?;
        if let Some(d) = names.as_dict() {
            if d.contains_key(b"Dests".as_slice()) {
                return Err(Error::Unsupported {
                    feature: "named destination",
                    detail: "/Names /Dests is outside the M8 rewrite profile".into(),
                });
            }
        }
    }

    // Walk /Annots arrays on every page for link-annotation named
    // destinations, and check GoTo/OpenAction /D operands — a name
    // or string where a destination array is required is a named
    // destination we cannot honour.
    check_named_dests(doc)?;

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
