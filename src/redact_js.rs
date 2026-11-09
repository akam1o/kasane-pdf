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

/// An action dictionary running JavaScript: `/S /JavaScript` with the
/// script (`/JS`, §12.6.4.17). A dictionary that only names the type
/// (some other object's private key) is not one.
pub(crate) fn is_javascript_action(d: &Dict) -> bool {
    d.get(b"S".as_slice()).and_then(|s| s.as_name()) == Some(b"JavaScript".as_slice())
        && d.contains_key(b"JS".as_slice())
}

fn refuse(detail: String) -> Error {
    Error::Unsupported {
        feature: "JavaScript removal",
        detail,
    }
}

/// Remove every JavaScript action and the `/Names /JavaScript` tree,
/// writing the edited objects into `overlay` (an object already there
/// is edited as it stands). A removed action's successors (`/Next`)
/// take its place. Returns how many were removed. Refused: an XFA form
/// (its scripts are not edited), and anything a removed script reaches
/// (its source stream …) that something kept still refers to.
pub(crate) fn strip_javascript(
    doc: &Document,
    overlay: &mut HashMap<(u32, u16), Object>,
    limits: &Limits,
) -> Result<u64, Error> {
    if let Some(af) = doc.catalog().get(b"AcroForm".as_slice()) {
        if doc
            .deref(af)?
            .as_dict()
            .is_some_and(|d| d.contains_key(b"XFA".as_slice()))
        {
            return Err(refuse("an XFA form: its scripts are not edited".into()));
        }
    }
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
    // The objects that are JavaScript actions, with their successors.
    let mut scripts: HashMap<IndirectRef, Option<Object>> = HashMap::new();
    for r in &refs {
        if let Some(d) = body(*r, overlay)?.as_dict() {
            if is_javascript_action(d) {
                scripts.insert(*r, d.get(b"Next".as_slice()).cloned());
            }
        }
    }
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => Some(*r),
        _ => None,
    };
    let mut removed = 0u64;
    let mut elements = 0u64;
    let mut kept: Vec<IndirectRef> = Vec::new();
    for r in refs {
        if scripts.contains_key(&r) {
            continue;
        }
        kept.push(r);
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
    // What the removed scripts reach (their source streams, …) besides
    // other actions: nothing kept may still refer to it.
    let mut reached: HashSet<IndirectRef> = HashSet::new();
    let mut stack: Vec<IndirectRef> = Vec::new();
    for r in scripts.keys() {
        if let Some(d) = body(*r, overlay)?.as_dict() {
            for (k, v) in d.iter() {
                if k.as_slice() != b"Next" {
                    stack.extend(refs_in(v));
                }
            }
        }
    }
    while let Some(r) = stack.pop() {
        if scripts.contains_key(&r) || !reached.insert(r) {
            continue;
        }
        elements += 1;
        if elements > limits.max_audit_elements {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: limits.max_audit_elements,
            });
        }
        stack.extend(refs_in(&body(r, overlay)?));
    }
    for r in kept {
        if reached.contains(&r) {
            continue;
        }
        if let Some(t) = refs_in(&body(r, overlay)?)
            .into_iter()
            .find(|t| reached.contains(t))
        {
            return Err(refuse(format!(
                "object {t}, which a removed script uses, is also referred to by object {r}"
            )));
        }
    }
    if let Some(t) = refs_in(&Object::Dict(doc.trailer().clone()))
        .into_iter()
        .find(|t| reached.contains(t))
    {
        return Err(refuse(format!(
            "object {t}, which a removed script uses, is referred to by the trailer"
        )));
    }
    Ok(removed)
}

/// The references written in `o` itself.
fn refs_in(o: &Object) -> Vec<IndirectRef> {
    let mut out = Vec::new();
    let mut stack = vec![o];
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => out.push(*r),
            Object::Dict(d) | Object::Stream { dict: d, .. } => stack.extend(d.values()),
            Object::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    out
}

/// What stands for `v` once scripts are removed: `v` itself, or the
/// successors of a script (its `/Next`, one action or an array), with
/// the scripts among them replaced in turn.
fn successors(
    v: &Object,
    scripts: &HashMap<IndirectRef, Option<Object>>,
    depth: u32,
    limits: &Limits,
) -> Result<Vec<Object>, Error> {
    if depth > limits.max_nesting_depth {
        return Err(Error::LimitExceeded {
            limit: "max_nesting_depth (JavaScript removal)",
            max: limits.max_nesting_depth as u64,
        });
    }
    let next = match v {
        Object::Ref(r) => match scripts.get(r) {
            Some(n) => n.clone(),
            None => return Ok(vec![v.clone()]),
        },
        Object::Dict(d) if is_javascript_action(d) => d.get(b"Next".as_slice()).cloned(),
        _ => return Ok(vec![v.clone()]),
    };
    let mut out = Vec::new();
    match next {
        None => {}
        Some(Object::Array(a)) => {
            for x in &a {
                out.extend(successors(x, scripts, depth + 1, limits)?);
            }
        }
        Some(x) => out.extend(successors(&x, scripts, depth + 1, limits)?),
    }
    Ok(out)
}

/// Replace in `o` (not through references) every entry and element that
/// is a JavaScript action, inline or one of `scripts`, by its successors.
fn strip(
    o: &mut Object,
    scripts: &HashMap<IndirectRef, Option<Object>>,
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
        Object::Ref(r) => scripts.contains_key(r),
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
                let v = d.remove(k.as_slice()).expect("listed above");
                n += 1;
                let mut next = successors(&v, scripts, 0, limits)?;
                match next.len() {
                    0 => {}
                    1 => {
                        d.insert(k, next.pop().expect("one"));
                    }
                    // Several successors fit only a /Next.
                    _ if k.as_slice() == b"Next" => {
                        d.insert(k, Object::Array(next));
                    }
                    _ => {
                        return Err(refuse(format!(
                            "the script at /{} is followed by several actions, which that \
                             entry cannot hold",
                            String::from_utf8_lossy(&k)
                        )))
                    }
                }
            }
            for v in d.values_mut() {
                n += strip(v, scripts, depth + 1, limits, elements)?;
            }
        }
        Object::Array(a) => {
            if a.iter().any(script) {
                let mut out = Vec::with_capacity(a.len());
                for v in a.drain(..) {
                    if script(&v) {
                        n += 1;
                        out.extend(successors(&v, scripts, 0, limits)?);
                    } else {
                        out.push(v);
                    }
                }
                *a = out;
            }
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
                    // A /JS entry is a script; a /JavaScript key is a
                    // name dictionary's script tree.
                    if d.contains_key(b"JS".as_slice()) || d.contains_key(b"JavaScript".as_slice())
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
