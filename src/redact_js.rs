//! `redact` with [`RedactOptions::remove_javascript`]: every JavaScript
//! action leaves the document — the `/Names /JavaScript` tree, and each
//! dictionary entry or array element that is a JavaScript action
//! (`/OpenAction`, `/AA` triggers, an annotation's or field's `/A`, a
//! `/Next` in an action chain), directly or by reference. A script may
//! hold any text of the document; it cannot be redacted, only removed.
//!
//! [`RedactOptions::remove_javascript`]: crate::RedactOptions::remove_javascript

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use std::collections::{HashMap, HashSet};

/// An action dictionary running JavaScript.
pub(crate) fn is_javascript_action(d: &Dict) -> bool {
    d.get(b"S".as_slice()).and_then(|s| s.as_name()) == Some(b"JavaScript".as_slice())
}

/// Remove every JavaScript action and the `/Names /JavaScript` tree,
/// writing the edited objects into `overlay` (an object already there
/// is edited as it stands). Returns how many were removed.
pub(crate) fn strip_javascript(
    doc: &Document,
    overlay: &mut HashMap<(u32, u16), Object>,
    limits: &Limits,
) -> Result<u64, Error> {
    let body = |r: IndirectRef, overlay: &HashMap<(u32, u16), Object>| -> Result<Object, Error> {
        match overlay.get(&(r.number, r.generation)) {
            Some(o) => Ok(o.clone()),
            None => doc.resolve(r),
        }
    };
    let mut refs: Vec<IndirectRef> = doc
        .xref_entries()
        .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
        .map(|(r, _)| r)
        .collect();
    for (n, g) in overlay.keys() {
        let r = IndirectRef::new(*n, *g);
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    // The objects that are JavaScript actions.
    let mut scripts: HashSet<IndirectRef> = HashSet::new();
    for r in &refs {
        if let Some(d) = body(*r, overlay)?.as_dict() {
            if is_javascript_action(d) {
                scripts.insert(*r);
            }
        }
    }
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => Some(*r),
        _ => None,
    };
    let mut removed = 0u64;
    let mut elements = 0u64;
    for r in refs {
        if scripts.contains(&r) {
            continue;
        }
        let mut obj = body(r, overlay)?;
        let mut n = strip(&mut obj, &scripts, 0, limits, &mut elements)?;
        // The catalog's /Names /JavaScript, direct or by reference.
        if Some(r) == root {
            if let Object::Dict(cat) = &mut obj {
                match cat.get_mut(b"Names".as_slice()) {
                    Some(Object::Dict(names)) => {
                        n += u64::from(names.remove(b"JavaScript".as_slice()).is_some());
                    }
                    Some(Object::Ref(nr)) => {
                        let nr = *nr;
                        let mut names = body(nr, overlay)?;
                        if let Object::Dict(d) = &mut names {
                            if d.remove(b"JavaScript".as_slice()).is_some() {
                                removed += 1;
                                overlay.insert((nr.number, nr.generation), names);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if n > 0 {
            removed += n;
            overlay.insert((r.number, r.generation), obj);
        }
    }
    Ok(removed)
}

/// Drop from `o` (not through references) every entry and element that
/// is a JavaScript action, inline or one of `scripts`.
fn strip(
    o: &mut Object,
    scripts: &HashSet<IndirectRef>,
    depth: u32,
    limits: &Limits,
    elements: &mut u64,
) -> Result<u64, Error> {
    if depth > limits.max_nesting_depth {
        return Err(Error::LimitExceeded {
            limit: "max_nesting_depth (JavaScript removal)",
            max: limits.max_nesting_depth as u64,
        });
    }
    *elements += 1;
    if *elements > limits.max_audit_elements {
        return Err(Error::LimitExceeded {
            limit: "max_audit_elements",
            max: limits.max_audit_elements,
        });
    }
    let script = |v: &Object| match v {
        Object::Ref(r) => scripts.contains(r),
        Object::Dict(d) => is_javascript_action(d),
        _ => false,
    };
    let mut n = 0;
    match o {
        Object::Dict(d) | Object::Stream { dict: d, .. } => {
            let keys: Vec<Vec<u8>> = d
                .iter()
                .filter(|(_, v)| script(v))
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys {
                d.remove(k.as_slice());
                n += 1;
            }
            for v in d.values_mut() {
                n += strip(v, scripts, depth + 1, limits, elements)?;
            }
        }
        Object::Array(a) => {
            let before = a.len();
            a.retain(|v| !script(v));
            n += (before - a.len()) as u64;
            for v in a.iter_mut() {
                n += strip(v, scripts, depth + 1, limits, elements)?;
            }
        }
        _ => {}
    }
    Ok(n)
}

/// An object of `out` still holding JavaScript: an action, a `/JS`
/// entry or a `/Names /JavaScript` tree (the catalog's direct `/Names`
/// as object 0).
pub(crate) fn find_javascript(
    out: &Document,
    limits: &Limits,
) -> Result<Option<IndirectRef>, Error> {
    let mut elements = 0u64;
    let refs: Vec<IndirectRef> = out
        .xref_entries()
        .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
        .map(|(r, _)| r)
        .collect();
    for r in refs {
        let obj = out.resolve(r)?;
        let mut stack = vec![&obj];
        while let Some(o) = stack.pop() {
            elements += 1;
            if elements > limits.max_audit_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_audit_elements",
                    max: limits.max_audit_elements,
                });
            }
            match o {
                Object::Dict(d) | Object::Stream { dict: d, .. } => {
                    // A /JavaScript key is a name dictionary's script tree.
                    if is_javascript_action(d)
                        || d.contains_key(b"JS".as_slice())
                        || d.contains_key(b"JavaScript".as_slice())
                    {
                        return Ok(Some(r));
                    }
                    stack.extend(d.values());
                }
                Object::Array(a) => stack.extend(a.iter()),
                _ => {}
            }
        }
    }
    // The catalog's direct /Names.
    if let Some(Object::Dict(n)) = out.catalog().get(b"Names".as_slice()) {
        if n.contains_key(b"JavaScript".as_slice()) {
            return Ok(Some(IndirectRef::new(0, 0)));
        }
    }
    Ok(None)
}
