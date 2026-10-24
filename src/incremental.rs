//! Incremental save (ISO 32000-1 §7.5.6): the original file bytes are
//! kept verbatim and a new revision — the changed and new objects, a
//! cross-reference section for them and a trailer whose `/Prev` points
//! at the previous `startxref` — is appended. Nothing before the
//! original end of file is touched, which is what keeps earlier
//! revisions (and the bytes a signature covers) intact.
//!
//! What is written:
//! * every object the edit session replaced, under its *original*
//!   number and generation; every synthetic object under a fresh
//!   number starting at the trailer's `/Size` (references between them
//!   are translated at write time);
//! * an optional new `/Info` dictionary;
//! * a classic `xref` table + `trailer` when the file's newest section
//!   is a table, or a cross-reference *stream* (`/Type /XRef`, `/W
//!   [1 4 2]`, uncompressed) when it is a stream — §7.5.8.4 forbids
//!   mixing the two kinds in one file;
//! * `/Size`, `/Root`, `/Info`, `/ID` carried from the newest trailer,
//!   `/Prev` = the previous `startxref`; `/XRefStm` and other
//!   bookkeeping keys of the old trailer are not repeated.
//!
//! Refused (typed errors, nothing written): a document that was
//! *recovered* (its xref offsets are reconstructions, so `/Prev` would
//! point at damage), references to objects with a non-zero generation
//! (the serialiser writes generation 0), and — through the signature
//! policy applied by [`crate::edit::Edit::save_incremental`] — changes
//! a signature does not permit.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::serialize::write_object;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Append a revision carrying `overlay` (replaced originals and
/// synthetic objects, the latter listed in `synth`) and `info` (a
/// replacement `/Info` dictionary) to `doc`'s bytes.
pub(crate) fn write(
    doc: &Document,
    overlay: &HashMap<(u32, u16), Object>,
    synth: &HashSet<(u32, u16)>,
    info: Option<&Dict>,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    let _budget = doc.decoded_budget_scope();
    if doc.recovery().is_some() {
        return Err(Error::Unsupported {
            feature: "incremental save of a recovered document",
            detail: "the cross-reference table was rebuilt by scanning; an appended revision \
                     would point /Prev at damaged structure — use a full rewrite"
                .into(),
        });
    }
    let trailer = doc.latest_trailer();
    let merged = doc.trailer();
    if merged.contains_key(b"Encrypt".as_slice()) {
        return Err(Error::Encrypted);
    }
    let prev = doc.startxref().ok_or_else(|| {
        Error::Structure("document has no startxref to chain the new revision to".into())
    })?;
    let root = merged
        .get(b"Root".as_slice())
        .cloned()
        .ok_or_else(|| Error::Structure("trailer has no /Root".into()))?;
    if root.as_ref().is_none() {
        return Err(Error::Structure("trailer /Root is not a reference".into()));
    }
    let uses_xref_stream =
        trailer.get(b"Type".as_slice()).and_then(|o| o.as_name()) == Some(b"XRef");

    // ---- object numbers --------------------------------------------
    let old_size = merged
        .get(b"Size".as_slice())
        .and_then(|o| o.as_i64())
        .filter(|s| *s > 0 && *s <= u32::MAX as i64)
        .ok_or_else(|| Error::Structure("trailer /Size is missing or invalid".into()))?
        as u32;
    let mut next = old_size;
    let mut fresh = move |doc: &Document| -> Result<u32, Error> {
        while doc.number_reserved(next) {
            next = next.checked_add(1).ok_or(Error::LimitExceeded {
                limit: "object number",
                max: u32::MAX as u64,
            })?;
        }
        let n = next;
        next += 1;
        Ok(n)
    };
    let mut synth_sorted: Vec<(u32, u16)> = synth.iter().copied().collect();
    synth_sorted.sort_unstable();
    let mut map: HashMap<(u32, u16), u32> = HashMap::new();
    for key in &synth_sorted {
        if overlay.contains_key(key) {
            map.insert(*key, fresh(doc)?);
        }
    }
    let info_num = match info {
        Some(_) => Some(fresh(doc)?),
        None => None,
    };
    let total_objects = overlay.len() as u64 + info.is_some() as u64;
    if total_objects > limits.max_output_objects as u64 {
        return Err(Error::LimitExceeded {
            limit: "max_output_objects",
            max: limits.max_output_objects as u64,
        });
    }

    // ---- bodies, in ascending new number order ----------------------
    let mut bodies: BTreeMap<(u32, u16), &Object> = BTreeMap::new();
    for (key, obj) in overlay {
        let num = match map.get(key) {
            Some(n) => (*n, 0),
            None => {
                if !doc.number_reserved(key.0) {
                    return Err(Error::Structure(format!(
                        "overlay object {} {} is neither an input object nor synthetic",
                        key.0, key.1
                    )));
                }
                *key
            }
        };
        bodies.insert(num, obj);
    }
    for obj in bodies.values() {
        check_generations(obj, synth, 0)?;
    }
    let remap = |r: IndirectRef| -> Option<u32> {
        if let Some(n) = map.get(&(r.number, r.generation)) {
            return Some(*n);
        }
        if synth.contains(&(r.number, r.generation)) {
            return None; // allocated but never given a body
        }
        (r.generation == 0).then_some(r.number)
    };

    let mut out = doc.bytes().to_vec();
    if !matches!(out.last(), Some(b'\n') | Some(b'\r')) {
        out.push(b'\n');
    }
    let mut offsets: BTreeMap<u32, (u64, u16)> = BTreeMap::new();
    let mut emit = |out: &mut Vec<u8>, num: u32, gen: u16, obj: &Object| -> Result<(), Error> {
        offsets.insert(num, (out.len() as u64, gen));
        out.extend_from_slice(format!("{num} {gen} obj\n").as_bytes());
        write_object(obj, &remap, out, 0)?;
        out.extend_from_slice(b"\nendobj\n");
        if out.len() as u64 > limits.max_output_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_output_bytes",
                max: limits.max_output_bytes,
            });
        }
        Ok(())
    };
    for ((num, gen), obj) in &bodies {
        emit(&mut out, *num, *gen, obj)?;
    }
    let info_obj = info.map(|d| Object::Dict(d.clone()));
    if let (Some(n), Some(o)) = (info_num, &info_obj) {
        emit(&mut out, n, 0, o)?;
    }

    // ---- trailer keys ---------------------------------------------
    let highest = offsets.keys().next_back().copied().unwrap_or(0);
    let new_size = old_size.max(highest + 1).max(next_after(&map, info_num));
    let mut tdict = Dict::new();
    tdict.insert(b"Size".to_vec(), Object::Integer(new_size as i64));
    tdict.insert(b"Root".to_vec(), root);
    match (info_num, merged.get(b"Info".as_slice())) {
        (Some(n), _) => {
            tdict.insert(b"Info".to_vec(), Object::Ref(IndirectRef::new(n, 0)));
        }
        (None, Some(i)) => {
            tdict.insert(b"Info".to_vec(), i.clone());
        }
        (None, None) => {}
    }
    if let Some(id) = merged.get(b"ID".as_slice()) {
        tdict.insert(b"ID".to_vec(), id.clone());
    }
    tdict.insert(b"Prev".to_vec(), Object::Integer(prev as i64));

    // ---- cross-reference section ------------------------------------
    let xref_pos = out.len() as u64;
    if uses_xref_stream {
        // The stream describes itself too: one more entry at its own
        // offset, under a fresh number.
        let self_num = fresh(doc)?;
        offsets.insert(self_num, (xref_pos, 0));
        let size = new_size.max(self_num + 1);
        let mut index = Vec::new();
        let mut data = Vec::new();
        for run in runs(&offsets) {
            index.push(Object::Integer(run[0].0 as i64));
            index.push(Object::Integer(run.len() as i64));
            for (num, (off, gen)) in run {
                let _ = num;
                data.push(1u8);
                data.extend_from_slice(&(off as u32).to_be_bytes());
                data.extend_from_slice(&gen.to_be_bytes());
                if off > u32::MAX as u64 {
                    return Err(Error::LimitExceeded {
                        limit: "xref stream offset width (4 bytes)",
                        max: u32::MAX as u64,
                    });
                }
            }
        }
        let mut sd = tdict.clone();
        sd.insert(b"Type".to_vec(), Object::Name(b"XRef".to_vec()));
        sd.insert(b"Size".to_vec(), Object::Integer(size as i64));
        sd.insert(
            b"W".to_vec(),
            Object::Array(vec![
                Object::Integer(1),
                Object::Integer(4),
                Object::Integer(2),
            ]),
        );
        sd.insert(b"Index".to_vec(), Object::Array(index));
        out.extend_from_slice(format!("{self_num} 0 obj\n").as_bytes());
        write_object(&Object::Stream { dict: sd, data }, &remap, &mut out, 0)?;
        out.extend_from_slice(b"\nendobj\n");
    } else {
        out.extend_from_slice(b"xref\n");
        for run in runs(&offsets) {
            out.extend_from_slice(format!("{} {}\n", run[0].0, run.len()).as_bytes());
            for (_, (off, gen)) in run {
                if off > 9_999_999_999 {
                    return Err(Error::LimitExceeded {
                        limit: "classic xref offset width (10 digits)",
                        max: 9_999_999_999,
                    });
                }
                out.extend_from_slice(format!("{off:010} {gen:05} n \n").as_bytes());
            }
        }
        out.extend_from_slice(b"trailer\n");
        write_object(&Object::Dict(tdict), &remap, &mut out, 0)?;
        out.push(b'\n');
    }
    out.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());
    if out.len() as u64 > limits.max_output_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_output_bytes",
            max: limits.max_output_bytes,
        });
    }
    Ok(out)
}

/// One past the highest number handed out to a synthetic or the
/// `/Info` object.
fn next_after(map: &HashMap<(u32, u16), u32>, info: Option<u32>) -> u32 {
    map.values()
        .copied()
        .chain(info)
        .max()
        .map(|n| n + 1)
        .unwrap_or(0)
}

/// Split the sorted entries into runs of consecutive object numbers
/// (one xref subsection each).
fn runs(offsets: &BTreeMap<u32, (u64, u16)>) -> Vec<Vec<(u32, (u64, u16))>> {
    let mut out: Vec<Vec<(u32, (u64, u16))>> = Vec::new();
    for (num, v) in offsets {
        match out.last_mut() {
            Some(run) if run.last().map(|(n, _)| n + 1) == Some(*num) => run.push((*num, *v)),
            _ => out.push(vec![(*num, *v)]),
        }
    }
    out
}

/// References to objects with a non-zero generation cannot be written
/// (the serialiser emits generation 0); refuse rather than mis-link.
fn check_generations(obj: &Object, synth: &HashSet<(u32, u16)>, depth: usize) -> Result<(), Error> {
    if depth > 256 {
        return Ok(());
    }
    match obj {
        Object::Ref(r) if r.generation != 0 && !synth.contains(&(r.number, r.generation)) => {
            Err(Error::Unsupported {
                feature: "incremental save with non-zero generation references",
                detail: format!("{r} is referenced from an updated object"),
            })
        }
        Object::Array(a) => a
            .iter()
            .try_for_each(|o| check_generations(o, synth, depth + 1)),
        Object::Dict(d) | Object::Stream { dict: d, .. } => d
            .values()
            .try_for_each(|o| check_generations(o, synth, depth + 1)),
        _ => Ok(()),
    }
}
