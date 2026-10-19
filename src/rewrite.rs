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
use std::collections::HashMap;

/// Maximum indirect objects emitted by one rewrite — bounds both the
/// reference map and the output buffer in memory.
const MAX_OUTPUT_OBJECTS: usize = 200_000;
/// Maximum total output bytes. Streams dominate file size; 2 GiB
/// covers realistic documents while still bounding a pathological
/// input that expands many small objects (a compressed stream is
/// carried as-is, so the cap applies to the *encoded* bytes we emit).
const MAX_OUTPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum DFS depth through the reference graph. Genuine indirect
/// cycles (page → /Parent → …) don't recurse — they hit the
/// already-numbered map and return; this bound catches pathological
/// *direct*-container nesting (arrays of arrays of refs).
const MAX_REF_WALK_DEPTH: usize = 512;

/// Header version — 1.7 covers every feature M8 can emit; a catalog
/// `/Version` above it is honoured instead (we never under-declare).
const BASE_VERSION: &str = "1.7";

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
    check_refusals(doc)?;

    // ---- Phase 1: collect reachable objects, assign new numbers ----
    let mut c = Collector {
        doc,
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
        c.walk(r, kind, 0)?;
    }

    // ---- Phase 2: emit, in new-number order ----
    let version = catalog_version(doc).unwrap_or(BASE_VERSION);
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
        if out.len() as u64 > MAX_OUTPUT_BYTES {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: MAX_OUTPUT_BYTES,
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
    fn walk(&mut self, old_ref: IndirectRef, kind: RootKind, depth: usize) -> Result<(), Error> {
        if depth > MAX_REF_WALK_DEPTH {
            return Err(Error::LimitExceeded {
                limit: "reference walk depth",
                max: MAX_REF_WALK_DEPTH as u64,
            });
        }
        if self.walked >= MAX_OUTPUT_OBJECTS {
            return Err(Error::LimitExceeded {
                limit: "max_output_objects",
                max: MAX_OUTPUT_OBJECTS as u64,
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

        let mut obj = self.doc.resolve(old_ref)?;
        self.scan_and_collect(&mut obj, depth)?;
        self.bodies.insert(new_num, obj);
        Ok(())
    }

    /// Walk every `Ref` inside `obj` so its referent gets a new
    /// number — the `Ref` itself is left untouched; the serializer's
    /// `remap` performs the old→new translation at write time.
    /// Doing the rewrite here *and* in `remap` would double-translate.
    fn scan_and_collect(&mut self, obj: &mut Object, depth: usize) -> Result<(), Error> {
        match obj {
            Object::Array(items) => {
                for item in items.iter_mut() {
                    self.scan_and_collect(item, depth + 1)?;
                }
            }
            Object::Dict(d) => {
                for v in d.values_mut() {
                    self.scan_and_collect(v, depth + 1)?;
                }
            }
            Object::Stream { dict, .. } => {
                for v in dict.values_mut() {
                    self.scan_and_collect(v, depth + 1)?;
                }
            }
            Object::Ref(r) => {
                self.walk(*r, RootKind::Ordinary, depth + 1)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Catalog `/Version` ≥ the header base is honoured (§7.5.2 note);
/// anything else falls back to `BASE_VERSION`.
fn catalog_version(doc: &Document) -> Option<&'static str> {
    doc.catalog()
        .get(b"Version".as_slice())
        .and_then(|o| o.as_name())
        .and_then(|n| match n {
            b"1.4" => Some("1.4"),
            b"1.5" => Some("1.5"),
            b"1.6" => Some("1.6"),
            b"1.7" => Some("1.7"),
            _ => None,
        })
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
