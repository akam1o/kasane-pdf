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
/// (its source stream, the object stream holding it …) that something
/// kept still refers to.
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
    let snapshot = overlay.clone();
    let body = |r: IndirectRef| -> Result<Object, Error> {
        match snapshot.get(&(r.number, r.generation)) {
            Some(o) => Ok(o.clone()),
            None => doc.resolve(r),
        }
    };
    let mut refs: Vec<IndirectRef> = Vec::new();
    // Object number → the object stream holding it.
    let mut container: HashMap<u32, u32> = HashMap::new();
    for (r, e) in doc.xref_entries() {
        match e {
            crate::xref::XrefEntry::Free { .. } => continue,
            crate::xref::XrefEntry::Compressed { objstm, .. } => {
                container.insert(r.number, objstm);
            }
            _ => {}
        }
        refs.push(r);
    }
    for (n, g) in snapshot.keys() {
        let r = IndirectRef::new(*n, *g);
        if !refs.contains(&r) {
            refs.push(r);
        }
    }
    // The objects that are JavaScript actions.
    let mut scripts: HashSet<IndirectRef> = HashSet::new();
    for r in &refs {
        if body(*r)?.as_dict().is_some_and(is_javascript_action) {
            scripts.insert(*r);
        }
    }
    let root = match doc.trailer().get(b"Root".as_slice()) {
        Some(Object::Ref(r)) => Some(*r),
        _ => None,
    };
    let mut cx = Strip {
        scripts: &scripts,
        body: &body,
        limits,
        elements: 0,
        removed: Vec::new(),
    };
    let mut count = 0u64;
    let mut kept: Vec<IndirectRef> = Vec::new();
    for r in refs {
        if scripts.contains(&r) {
            continue;
        }
        kept.push(r);
        let mut obj = body(r)?;
        let mut n = cx.strip(&mut obj, 0)?;
        // The catalog's /Names /JavaScript, direct or by reference.
        if Some(r) == root {
            if let Object::Dict(cat) = &mut obj {
                match cat.get_mut(b"Names".as_slice()) {
                    Some(Object::Dict(names)) => {
                        if let Some(t) = names.remove(b"JavaScript".as_slice()) {
                            cx.removed.push(t);
                            n += 1;
                        }
                    }
                    Some(Object::Ref(nr)) => {
                        let nr = *nr;
                        let mut names = body(nr)?;
                        if let Object::Dict(d) = &mut names {
                            if let Some(t) = d.remove(b"JavaScript".as_slice()) {
                                cx.removed.push(t);
                                count += 1;
                                overlay.insert((nr.number, nr.generation), names);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if n > 0 {
            count += n;
            overlay.insert((r.number, r.generation), obj);
        }
    }
    // What the removed scripts use besides other scripts and their
    // successors: their sources, the object streams holding them, the
    // name tree's nodes. Nothing kept may still refer to it.
    let mut reached: HashSet<IndirectRef> = HashSet::new();
    let mut stack: Vec<IndirectRef> = Vec::new();
    let uses = |v: &Object, stack: &mut Vec<IndirectRef>| match v {
        Object::Dict(d) if is_javascript_action(d) => {
            for (k, x) in d.iter() {
                if k.as_slice() != b"Next" {
                    stack.extend(refs_in(x));
                }
            }
        }
        Object::Ref(_) => {}
        other => stack.extend(refs_in(other)),
    };
    for v in std::mem::take(&mut cx.removed) {
        if let Object::Ref(r) = v {
            stack.push(r);
        } else {
            uses(&v, &mut stack);
        }
    }
    for r in &scripts {
        stack.push(*r);
    }
    let mut walked: HashSet<IndirectRef> = HashSet::new();
    while let Some(r) = stack.pop() {
        if !walked.insert(r) {
            continue;
        }
        cx.charge(1)?;
        if let Some(c) = container.get(&r.number) {
            reached.insert(IndirectRef::new(*c, 0));
        }
        let o = body(r)?;
        if scripts.contains(&r) {
            uses(&o, &mut stack);
        } else {
            reached.insert(r);
            stack.extend(refs_in(&o));
        }
    }
    for r in kept {
        if reached.contains(&r) {
            continue;
        }
        let o = match overlay.get(&(r.number, r.generation)) {
            Some(o) => o.clone(),
            None => body(r)?,
        };
        if let Some(t) = refs_in(&o).into_iter().find(|t| reached.contains(t)) {
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
    Ok(count)
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

struct Strip<'a> {
    scripts: &'a HashSet<IndirectRef>,
    body: &'a dyn Fn(IndirectRef) -> Result<Object, Error>,
    limits: &'a Limits,
    /// Work done, charged against `max_audit_elements`.
    elements: u64,
    /// The script actions and trees removed (references or inline).
    removed: Vec<Object>,
}

impl Strip<'_> {
    fn charge(&mut self, n: u64) -> Result<(), Error> {
        self.elements = self.elements.saturating_add(n);
        if self.elements > self.limits.max_audit_elements {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: self.limits.max_audit_elements,
            });
        }
        Ok(())
    }

    fn is_script(&self, v: &Object) -> bool {
        match v {
            Object::Ref(r) => self.scripts.contains(r),
            Object::Dict(d) => is_javascript_action(d),
            _ => false,
        }
    }

    /// What stands for `v` once scripts are removed: `v` itself, or the
    /// actions after a script (its `/Next`: one action or an array, by
    /// reference or not), the scripts among them replaced in turn. Each
    /// action is listed once; a script met twice ends the chain there.
    fn successors(
        &mut self,
        v: &Object,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<Object>,
    ) -> Result<(), Error> {
        self.charge(1)?;
        if let Object::Ref(r) = v {
            if !seen.insert(*r) {
                return Ok(());
            }
        }
        if !self.is_script(v) {
            out.push(v.clone());
            return Ok(());
        }
        self.removed.push(v.clone());
        let dict = match v {
            Object::Ref(r) => (self.body)(*r)?,
            other => other.clone(),
        };
        let next = match dict.as_dict().and_then(|d| d.get(b"Next".as_slice())) {
            Some(n) => n.clone(),
            None => return Ok(()),
        };
        // A /Next by reference may be an array.
        let next = match &next {
            Object::Ref(r) if !self.scripts.contains(r) => match (self.body)(*r)? {
                a @ Object::Array(_) => a,
                _ => next,
            },
            _ => next,
        };
        match next {
            Object::Array(a) => {
                for x in &a {
                    self.successors(x, seen, out)?;
                }
            }
            x => self.successors(&x, seen, out)?,
        }
        Ok(())
    }

    /// Replace in `o` (not through references) every entry and element
    /// that is a JavaScript action by the actions after it.
    fn strip(&mut self, o: &mut Object, depth: u32) -> Result<u64, Error> {
        if depth > self.limits.max_nesting_depth {
            return Err(Error::LimitExceeded {
                limit: "max_nesting_depth (JavaScript removal)",
                max: self.limits.max_nesting_depth as u64,
            });
        }
        self.charge(1)?;
        let mut n = 0;
        match o {
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                let keys: Vec<Vec<u8>> = d
                    .iter()
                    .filter(|(_, v)| self.is_script(v))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in keys {
                    let v = d.remove(k.as_slice()).expect("listed above");
                    n += 1;
                    let mut next = Vec::new();
                    self.successors(&v, &mut HashSet::new(), &mut next)?;
                    match next.len() {
                        0 => {}
                        1 => {
                            d.insert(k, next.pop().expect("one"));
                        }
                        // Several actions fit only a /Next.
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
                    n += self.strip(v, depth + 1)?;
                }
            }
            Object::Array(a) => {
                if a.iter().any(|v| self.is_script(v)) {
                    let mut out = Vec::with_capacity(a.len());
                    for v in std::mem::take(a) {
                        if self.is_script(&v) {
                            n += 1;
                            self.successors(&v, &mut HashSet::new(), &mut out)?;
                        } else {
                            out.push(v);
                        }
                    }
                    *a = out;
                }
                for v in a.iter_mut() {
                    n += self.strip(v, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(n)
    }
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
