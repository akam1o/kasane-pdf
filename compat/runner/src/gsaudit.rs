//! ExtGState usage audit — walks each corpus PDF and records every
//! `/ExtGState` resource dictionary's keys + values, split by whether
//! the gs is actually *referenced* by a `gs` operator on a page.
//!
//! Powers the Phase-split decision in the ExtGState implementation:
//! we implement the parameters that real corpus PDFs actually use,
//! not the whole ISO-32000 list.

use kasane_pdf::{Document, Object};
use std::collections::{BTreeMap, BTreeSet};

/// One ExtGState dict usage record.
#[derive(Debug)]
pub struct GsUse {
    /// the resource name (e.g. `/GS0`); also used to report which gs
    /// names were actually invoked vs merely defined.
    #[allow(dead_code)]
    pub name: String,
    /// keys present → their rendered value (for frequency counts)
    pub keys: BTreeMap<String, String>,
}

/// Per-file audit: which gs names exist, which are actually `Do`-style
/// referenced via `gs` ops, and the union of all keys used.
#[derive(Debug, Default)]
pub struct GsAudit {
    /// file id → list of (name, keys) — one per distinct /ExtGState entry.
    pub entries: Vec<GsUse>,
    /// gs names actually invoked by a `gs` op on some page.
    pub referenced: BTreeSet<String>,
    /// every page that invoked *any* gs (page indices).
    pub pages_using: BTreeSet<usize>,
}

/// Audit one document: enumerate the `/ExtGState` resource dicts and
/// which gs names are actually `gs`-invoked on each page.
pub fn audit(doc: &Document) -> GsAudit {
    let mut a = GsAudit::default();
    let mut seen_names: BTreeSet<Vec<u8>> = BTreeSet::new();

    for (pi, page) in doc.pages().iter().enumerate() {
        let ops = match doc.page_operations(page) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let resources = page.resources().cloned().unwrap_or(Object::Null);

        // Collect every gs name invoked on this page (top level + we
        // don't recurse into forms here — forms get their own audit
        // when their dict carries /Resources).
        let mut invoked = Vec::new();
        collect_gs_ops(&ops, &mut invoked);
        if !invoked.is_empty() {
            a.pages_using.insert(pi);
        }
        for name in &invoked {
            a.referenced
                .insert(String::from_utf8_lossy(name).into_owned());
            if seen_names.insert(name.clone()) {
                if let Ok(Some(obj)) = doc.resolve_resource(&resources, "ExtGState", name) {
                    if let Some(d) = obj.as_dict() {
                        a.entries.push(GsUse {
                            name: String::from_utf8_lossy(name).into_owned(),
                            keys: dump_keys(d),
                        });
                    }
                }
            }
        }
        // Also enumerate *all* /ExtGState entries present in resources
        // (even unreferenced) so we can report defined-but-unused.
        if let Ok(Some(Object::Dict(res))) = doc.deref(&resources).map(|r| match r {
            Object::Dict(d) => Some(Object::Dict(d)),
            _ => None,
        }) {
            if let Some(ext) = res.get(b"ExtGState".as_slice()) {
                if let Ok(Object::Dict(d)) = doc.deref(ext) {
                    for (name, entry) in d {
                        if seen_names.insert(name.clone()) {
                            if let Ok(eo) = doc.deref(&entry) {
                                if let Some(dd) = eo.as_dict() {
                                    a.entries.push(GsUse {
                                        name: String::from_utf8_lossy(&name).into_owned(),
                                        keys: dump_keys(dd),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    a
}

fn collect_gs_ops(ops: &[kasane_pdf::Operation], out: &mut Vec<Vec<u8>>) {
    for op in ops {
        if let kasane_pdf::Operation::SetExtGState(n) = op {
            out.push(n.clone());
        }
    }
}

/// Render every key in the gs dict to a short value string for
/// frequency bucketing. Values are truncated — we only need the
/// shape (number/name/array present), not full fidelity.
fn dump_keys(d: &kasane_pdf::Dict) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (k, v) in d {
        let key = String::from_utf8_lossy(k).into_owned();
        let val = match v {
            Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
            Object::Integer(i) => i.to_string(),
            Object::Real(f) => format!("{f}"),
            Object::Bool(b) => b.to_string(),
            Object::Array(a) => format!("[{} items]", a.len()),
            Object::Dict(_) => "<dict>".into(),
            Object::Ref(_) => "<ref>".into(),
            Object::Stream { .. } => "<stream>".into(),
            Object::String(_) => "<str>".into(),
            Object::Null => "null".into(),
        };
        m.insert(key, val);
    }
    m
}
