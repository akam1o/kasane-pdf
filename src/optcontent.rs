//! Optional content (ISO 32000-1 §8.11): which optional content groups
//! are *visible* under the document's default configuration, and the
//! visibility of an `/OC` reference (an OCG or an OCMD) given that.
//!
//! What is evaluated:
//! * `/OCProperties /OCGs` — the set of groups;
//! * `/D` — the default configuration: `/BaseState` (`/ON` default,
//!   `/OFF`, `/Unchanged`), `/ON`, `/OFF`;
//! * `/D /AS` usage-application dictionaries whose `/Event` is `/View`
//!   — each listed group's `/Usage /View /ViewState` then overrides
//!   the ON/OFF arrays (that is what a viewer applies on open);
//! * OCMDs: `/VE` visibility expressions (`/And`, `/Or`, `/Not`, nested,
//!   depth- and node-bounded) when present, else `/OCGs` + `/P`
//!   (`/AnyOn` default, `/AllOn`, `/AnyOff`, `/AllOff`).
//!
//! What is *not* evaluated, deliberately: `/Order` and `/RBGroups` are
//! UI structure, not visibility; usage categories other than `/View`
//! (`/Print`, `/Export`, `/Zoom`, `/Language`, `/User`) describe other
//! consumers — a `/Print` state never changes what the page *shows*;
//! alternate configurations in `/Configs` are not the default.
//! Anything unreadable in `/OCProperties` is a structure error, not
//! a guess.
//!
//! OCGs are identified by indirect reference (§8.11.2.1 requires them
//! to be indirect objects). A direct OCG dictionary cannot be in any
//! configuration array and is treated as visible.

use std::collections::HashSet;

use crate::document::Document;
use crate::error::Error;
use crate::object::{Dict, IndirectRef, Object};

/// Nesting depth of `/VE` expressions accepted.
const MAX_VE_DEPTH: usize = 32;
/// Operand nodes evaluated per `/VE` expression.
const MAX_VE_NODES: usize = 1024;
/// Groups accepted in `/OCGs` (document-wide).
const MAX_OCGS: usize = 65_536;

/// Visibility state of every group under the default configuration.
#[derive(Debug, Default, Clone)]
pub(crate) struct OcConfig {
    /// Groups that are OFF; every other group is ON.
    hidden: HashSet<IndirectRef>,
    /// `true` when the document declares `/OCProperties` at all. A
    /// document without it has no optional content — every `/OC`
    /// reference then names an undeclared group (visible, by §8.11.4.3:
    /// "a group not in /OCGs … is treated as visible").
    declared: bool,
}

impl OcConfig {
    /// Read `/OCProperties` from the catalog.
    pub(crate) fn from_catalog(doc: &Document) -> Result<OcConfig, Error> {
        let Some(ocp) = doc.catalog().get(b"OCProperties".as_slice()) else {
            return Ok(OcConfig::default());
        };
        let ocp = doc.deref(ocp)?;
        let ocp = ocp
            .as_dict()
            .ok_or_else(|| Error::Structure("/OCProperties is not a dictionary".into()))?;
        let all: Vec<IndirectRef> = match ocp.get(b"OCGs".as_slice()) {
            None => Vec::new(),
            Some(o) => ref_list(doc, o, "/OCProperties /OCGs")?,
        };
        if all.len() > MAX_OCGS {
            return Err(Error::LimitExceeded {
                limit: "optional content groups",
                max: MAX_OCGS as u64,
            });
        }
        let mut cfg = OcConfig {
            hidden: HashSet::new(),
            declared: true,
        };
        let Some(d) = ocp.get(b"D".as_slice()) else {
            return Ok(cfg);
        };
        let d = doc.deref(d)?;
        let d = d
            .as_dict()
            .ok_or_else(|| Error::Structure("/OCProperties /D is not a dictionary".into()))?;
        match d.get(b"BaseState".as_slice()).and_then(|o| o.as_name()) {
            None | Some(b"ON") | Some(b"Unchanged") => {}
            Some(b"OFF") => cfg.hidden.extend(all.iter().copied()),
            Some(other) => {
                return Err(Error::Structure(format!(
                    "/BaseState /{} is not ON, OFF or Unchanged",
                    String::from_utf8_lossy(other)
                )))
            }
        }
        if let Some(on) = d.get(b"ON".as_slice()) {
            for r in ref_list(doc, on, "/D /ON")? {
                cfg.hidden.remove(&r);
            }
        }
        if let Some(off) = d.get(b"OFF".as_slice()) {
            for r in ref_list(doc, off, "/D /OFF")? {
                cfg.hidden.insert(r);
            }
        }
        // Usage application for the View event (§8.11.4.4): the
        // /Usage /View /ViewState of each listed group overrides.
        if let Some(as_obj) = d.get(b"AS".as_slice()) {
            let as_obj = doc.deref(as_obj)?;
            let arr = as_obj
                .as_array()
                .ok_or_else(|| Error::Structure("/D /AS is not an array".into()))?;
            for app in arr.iter().take(64) {
                let app = doc.deref(app)?;
                let Some(ad) = app.as_dict() else { continue };
                if ad.get(b"Event".as_slice()).and_then(|o| o.as_name()) != Some(b"View") {
                    continue;
                }
                let cats_view = match ad.get(b"Category".as_slice()).map(|o| doc.deref(o)) {
                    Some(Ok(Object::Array(c))) => c.iter().any(|n| n.as_name() == Some(b"View")),
                    _ => true,
                };
                if !cats_view {
                    continue;
                }
                let Some(groups) = ad.get(b"OCGs".as_slice()) else {
                    continue;
                };
                for g in ref_list(doc, groups, "/AS /OCGs")? {
                    let gd = doc.resolve(g)?;
                    let state = gd
                        .as_dict()
                        .and_then(|d| d.get(b"Usage".as_slice()))
                        .map(|u| doc.deref(u))
                        .transpose()?
                        .and_then(|u| u.as_dict().cloned())
                        .and_then(|u| u.get(b"View".as_slice()).cloned())
                        .map(|v| doc.deref(&v))
                        .transpose()?
                        .and_then(|v| v.as_dict().cloned())
                        .and_then(|v| {
                            v.get(b"ViewState".as_slice())
                                .and_then(|s| s.as_name())
                                .map(|s| s.to_vec())
                        });
                    match state.as_deref() {
                        Some(b"OFF") => {
                            cfg.hidden.insert(g);
                        }
                        Some(b"ON") => {
                            cfg.hidden.remove(&g);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(cfg)
    }

    /// Visibility of a group given by reference.
    fn group_visible(&self, r: IndirectRef) -> bool {
        !self.hidden.contains(&r)
    }

    /// Visibility of an `/OC` value: a reference to an OCG or OCMD, or
    /// (tolerated) a direct OCMD dictionary. `raw` must be the entry
    /// *as stored* so a reference can be recognised.
    pub(crate) fn visible(&self, doc: &Document, raw: &Object) -> Result<bool, Error> {
        if !self.declared {
            return Ok(true);
        }
        let (reference, dict) = match raw {
            Object::Ref(r) => {
                let o = doc.resolve(*r)?;
                let Some(d) = o.as_dict().cloned() else {
                    return Err(Error::Structure(format!(
                        "/OC {r} does not refer to a dictionary"
                    )));
                };
                (Some(*r), d)
            }
            Object::Dict(d) => (None, d.clone()),
            Object::Null => return Ok(true),
            _ => {
                return Err(Error::Structure(
                    "/OC is neither a reference nor a dictionary".into(),
                ))
            }
        };
        match dict.get(b"Type".as_slice()).and_then(|o| o.as_name()) {
            Some(b"OCMD") => self.ocmd_visible(doc, &dict),
            // An OCG (or an untyped dictionary — treated as a group).
            _ => Ok(reference.map(|r| self.group_visible(r)).unwrap_or(true)),
        }
    }

    fn ocmd_visible(&self, doc: &Document, d: &Dict) -> Result<bool, Error> {
        if let Some(ve) = d.get(b"VE".as_slice()) {
            let ve = doc.deref(ve)?;
            let mut nodes = 0usize;
            return self.eval_ve(doc, &ve, 0, &mut nodes);
        }
        let groups: Vec<Option<IndirectRef>> = match d.get(b"OCGs".as_slice()) {
            None => return Ok(true),
            Some(Object::Ref(r)) => vec![Some(*r)],
            Some(Object::Null) => return Ok(true),
            Some(Object::Dict(_)) => vec![None],
            Some(Object::Array(a)) => a
                .iter()
                .filter(|o| !matches!(o, Object::Null))
                .map(|o| match o {
                    Object::Ref(r) => Ok(Some(*r)),
                    Object::Dict(_) => Ok(None),
                    _ => Err(Error::Structure(
                        "/OCMD /OCGs entries must be references".into(),
                    )),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => {
                return Err(Error::Structure(
                    "/OCMD /OCGs is neither a reference nor an array".into(),
                ))
            }
        };
        if groups.is_empty() {
            return Ok(true);
        }
        let states: Vec<bool> = groups
            .iter()
            .map(|g| g.map(|r| self.group_visible(r)).unwrap_or(true))
            .collect();
        Ok(match d.get(b"P".as_slice()).and_then(|o| o.as_name()) {
            None | Some(b"AnyOn") => states.iter().any(|v| *v),
            Some(b"AllOn") => states.iter().all(|v| *v),
            Some(b"AnyOff") => states.iter().any(|v| !*v),
            Some(b"AllOff") => states.iter().all(|v| !*v),
            Some(other) => {
                return Err(Error::Structure(format!(
                    "/OCMD /P /{} is not a visibility policy",
                    String::from_utf8_lossy(other)
                )))
            }
        })
    }

    /// `/VE` expression: `[/And e1 e2 …]`, `[/Or …]`, `[/Not e]`, where
    /// each operand is a group reference or a nested array.
    fn eval_ve(
        &self,
        doc: &Document,
        ve: &Object,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<bool, Error> {
        if depth > MAX_VE_DEPTH {
            return Err(Error::LimitExceeded {
                limit: "optional content /VE depth",
                max: MAX_VE_DEPTH as u64,
            });
        }
        *nodes += 1;
        if *nodes > MAX_VE_NODES {
            return Err(Error::LimitExceeded {
                limit: "optional content /VE nodes",
                max: MAX_VE_NODES as u64,
            });
        }
        match ve {
            Object::Ref(r) => Ok(self.group_visible(*r)),
            Object::Array(a) => {
                let Some(op) = a.first().and_then(|o| o.as_name()) else {
                    return Err(Error::Structure(
                        "/VE array must start with /And, /Or or /Not".into(),
                    ));
                };
                let operands = &a[1..];
                match op {
                    b"Not" => {
                        if operands.len() != 1 {
                            return Err(Error::Structure("/VE /Not takes one operand".into()));
                        }
                        let v = self.eval_operand(doc, &operands[0], depth + 1, nodes)?;
                        Ok(!v)
                    }
                    b"And" | b"Or" => {
                        let mut acc = op == b"And";
                        for o in operands {
                            let v = self.eval_operand(doc, o, depth + 1, nodes)?;
                            acc = if op == b"And" { acc && v } else { acc || v };
                        }
                        Ok(acc)
                    }
                    other => Err(Error::Structure(format!(
                        "/VE operator /{} is not And, Or or Not",
                        String::from_utf8_lossy(other)
                    ))),
                }
            }
            Object::Dict(_) => Ok(true),
            _ => Err(Error::Structure(
                "/VE operand must be a group reference or array".into(),
            )),
        }
    }
}

impl OcConfig {
    /// One `/VE` operand: a reference is a group — unless it resolves
    /// to an array, which is a nested expression stored indirectly.
    fn eval_operand(
        &self,
        doc: &Document,
        o: &Object,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<bool, Error> {
        match o {
            Object::Ref(r) => match doc.resolve(*r)? {
                Object::Array(inner) => self.eval_ve(doc, &Object::Array(inner), depth, nodes),
                _ => {
                    *nodes += 1;
                    Ok(self.group_visible(*r))
                }
            },
            other => self.eval_ve(doc, other, depth, nodes),
        }
    }
}

/// An array (possibly indirect) of references; `null` entries skipped.
fn ref_list(doc: &Document, o: &Object, what: &str) -> Result<Vec<IndirectRef>, Error> {
    let o = doc.deref(o)?;
    let arr = match &o {
        Object::Array(a) => a,
        Object::Null => return Ok(Vec::new()),
        _ => return Err(Error::Structure(format!("{what} is not an array"))),
    };
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        match e {
            Object::Ref(r) => out.push(*r),
            Object::Null => {}
            _ => {
                return Err(Error::Structure(format!(
                    "{what} entries must be indirect references"
                )))
            }
        }
    }
    Ok(out)
}

/// Nesting depth of a configuration's `/Order` accepted by the rewriter.
const MAX_ORDER_DEPTH: usize = 32;
/// Entries walked across every `/Order`, `/RBGroups` and `/AS` array.
const MAX_CONFIG_NODES: usize = 262_144;

/// Check `/OCProperties` before the rewriter carries it (§8.11.4). The
/// rewriter copies the structure verbatim with renumbered references,
/// so what has to hold is that the structure means exactly one thing:
/// every type is the one the specification requires, every group a
/// configuration names is an OCG dictionary, `/Order` is a finite tree.
/// A violation is `Error::Structure` (or `LimitExceeded`): the rewrite
/// is refused rather than writing a structure whose meaning is unclear.
/// Content-level references (`/Properties`, XObject and annotation
/// `/OC`, OCMDs) are ordinary objects of the graph and are copied with
/// it.
pub(crate) fn validate_for_rewrite(doc: &Document) -> Result<(), Error> {
    let Some(ocp) = doc.catalog().get(b"OCProperties".as_slice()) else {
        return Ok(());
    };
    let ocp = doc.deref(ocp)?;
    let ocp = ocp
        .as_dict()
        .ok_or_else(|| Error::Structure("/OCProperties is not a dictionary".into()))?;
    let ocgs_obj = ocp
        .get(b"OCGs".as_slice())
        .ok_or_else(|| Error::Structure("/OCProperties has no /OCGs (required)".into()))?;
    let ocgs = strict_ref_list(doc, ocgs_obj, "/OCProperties /OCGs")?;
    if ocgs.len() > MAX_OCGS {
        return Err(Error::LimitExceeded {
            limit: "optional content groups",
            max: MAX_OCGS as u64,
        });
    }
    for r in &ocgs {
        check_group(doc, *r, "/OCProperties /OCGs")?;
    }
    let d = ocp
        .get(b"D".as_slice())
        .ok_or_else(|| Error::Structure("/OCProperties has no /D (required)".into()))?;
    let mut nodes = 0usize;
    check_config(doc, d, "/D", &mut nodes)?;
    if let Some(configs) = ocp.get(b"Configs".as_slice()) {
        let configs = doc.deref(configs)?;
        let arr = configs
            .as_array()
            .ok_or_else(|| Error::Structure("/OCProperties /Configs is not an array".into()))?;
        for (i, c) in arr.iter().enumerate() {
            check_config(doc, c, &format!("/Configs [{i}]"), &mut nodes)?;
        }
    }
    Ok(())
}

/// An array of references (no `null`, no direct objects).
fn strict_ref_list(doc: &Document, o: &Object, what: &str) -> Result<Vec<IndirectRef>, Error> {
    let o = doc.deref(o)?;
    let arr = o
        .as_array()
        .ok_or_else(|| Error::Structure(format!("{what} is not an array")))?;
    arr.iter()
        .map(|e| match e {
            Object::Ref(r) => Ok(*r),
            _ => Err(Error::Structure(format!(
                "{what} entries must be references to OCGs"
            ))),
        })
        .collect()
}

/// `r` is an optional content group dictionary (§8.11.2.1).
fn check_group(doc: &Document, r: IndirectRef, what: &str) -> Result<(), Error> {
    let o = doc.resolve(r)?;
    let d = o
        .as_dict()
        .ok_or_else(|| Error::Structure(format!("{what}: {r} is not an optional content group")))?;
    if d.get(b"Type".as_slice()).and_then(|t| t.as_name()) != Some(b"OCG") {
        return Err(Error::Structure(format!("{what}: {r} is not /Type /OCG")));
    }
    match d
        .get(b"Name".as_slice())
        .map(|n| doc.deref(n))
        .transpose()?
    {
        Some(Object::String(_)) => Ok(()),
        _ => Err(Error::Structure(format!(
            "{what}: {r} has no /Name text string"
        ))),
    }
}

fn names_in(o: &Object, allowed: &[&[u8]]) -> bool {
    o.as_name().map(|n| allowed.contains(&n)).unwrap_or(false)
}

/// An optional content configuration dictionary (§8.11.4.3).
fn check_config(doc: &Document, c: &Object, what: &str, nodes: &mut usize) -> Result<(), Error> {
    let c = doc.deref(c)?;
    let d = c
        .as_dict()
        .ok_or_else(|| Error::Structure(format!("{what} is not a configuration dictionary")))?;
    for key in [&b"Name"[..], b"Creator"] {
        if let Some(v) = d.get(key) {
            if !matches!(doc.deref(v)?, Object::String(_)) {
                return Err(Error::Structure(format!(
                    "{what} /{} is not a text string",
                    String::from_utf8_lossy(key)
                )));
            }
        }
    }
    if let Some(b) = d.get(b"BaseState".as_slice()) {
        if !names_in(b, &[b"ON", b"OFF", b"Unchanged"]) {
            return Err(Error::Structure(format!(
                "{what} /BaseState is not ON, OFF or Unchanged"
            )));
        }
    }
    if let Some(l) = d.get(b"ListMode".as_slice()) {
        if !names_in(l, &[b"AllPages", b"VisiblePages"]) {
            return Err(Error::Structure(format!(
                "{what} /ListMode is not AllPages or VisiblePages"
            )));
        }
    }
    if let Some(i) = d.get(b"Intent".as_slice()) {
        let ok = match doc.deref(i)? {
            Object::Name(_) => true,
            Object::Array(a) => a.iter().all(|n| n.as_name().is_some()),
            _ => false,
        };
        if !ok {
            return Err(Error::Structure(format!(
                "{what} /Intent is not a name or an array of names"
            )));
        }
    }
    for key in [&b"ON"[..], b"OFF", b"Locked"] {
        if let Some(list) = d.get(key) {
            let label = format!("{what} /{}", String::from_utf8_lossy(key));
            for r in strict_ref_list(doc, list, &label)? {
                count(nodes)?;
                check_group(doc, r, &label)?;
            }
        }
    }
    if let Some(rb) = d.get(b"RBGroups".as_slice()) {
        let rb = doc.deref(rb)?;
        let groups = rb
            .as_array()
            .ok_or_else(|| Error::Structure(format!("{what} /RBGroups is not an array")))?;
        for g in groups {
            for r in strict_ref_list(doc, g, &format!("{what} /RBGroups entry"))? {
                count(nodes)?;
                check_group(doc, r, &format!("{what} /RBGroups"))?;
            }
        }
    }
    if let Some(order) = d.get(b"Order".as_slice()) {
        let mut stack = Vec::new();
        check_order(doc, order, what, 0, &mut stack, nodes)?;
    }
    if let Some(apps) = d.get(b"AS".as_slice()) {
        let apps = doc.deref(apps)?;
        let apps = apps
            .as_array()
            .ok_or_else(|| Error::Structure(format!("{what} /AS is not an array")))?;
        for a in apps {
            count(nodes)?;
            let a = doc.deref(a)?;
            let ad = a
                .as_dict()
                .ok_or_else(|| Error::Structure(format!("{what} /AS entry is not a dictionary")))?;
            if !ad
                .get(b"Event".as_slice())
                .map(|e| names_in(e, &[b"View", b"Print", b"Export"]))
                .unwrap_or(false)
            {
                return Err(Error::Structure(format!(
                    "{what} /AS /Event is not View, Print or Export"
                )));
            }
            let groups = ad
                .get(b"OCGs".as_slice())
                .ok_or_else(|| Error::Structure(format!("{what} /AS entry has no /OCGs")))?;
            for r in strict_ref_list(doc, groups, &format!("{what} /AS /OCGs"))? {
                count(nodes)?;
                check_group(doc, r, &format!("{what} /AS /OCGs"))?;
            }
            match ad
                .get(b"Category".as_slice())
                .map(|c| doc.deref(c))
                .transpose()?
            {
                Some(Object::Array(cats)) if cats.iter().all(|c| c.as_name().is_some()) => {}
                _ => {
                    return Err(Error::Structure(format!(
                        "{what} /AS /Category is not an array of names"
                    )))
                }
            }
        }
    }
    Ok(())
}

fn count(nodes: &mut usize) -> Result<(), Error> {
    *nodes += 1;
    if *nodes > MAX_CONFIG_NODES {
        return Err(Error::LimitExceeded {
            limit: "optional content configuration entries",
            max: MAX_CONFIG_NODES as u64,
        });
    }
    Ok(())
}

/// `/Order`: a tree of arrays whose leaves are OCG references; a text
/// string may label a subarray. Indirect arrays are followed; one that
/// contains itself is a cycle.
fn check_order(
    doc: &Document,
    o: &Object,
    what: &str,
    depth: usize,
    stack: &mut Vec<IndirectRef>,
    nodes: &mut usize,
) -> Result<(), Error> {
    if depth > MAX_ORDER_DEPTH {
        return Err(Error::LimitExceeded {
            limit: "optional content /Order depth",
            max: MAX_ORDER_DEPTH as u64,
        });
    }
    count(nodes)?;
    let (reference, value) = match o {
        Object::Ref(r) => (Some(*r), doc.resolve(*r)?),
        other => (None, other.clone()),
    };
    match value {
        Object::Array(items) => {
            if let Some(r) = reference {
                if stack.contains(&r) {
                    return Err(Error::Structure(format!(
                        "{what} /Order contains itself ({r})"
                    )));
                }
                stack.push(r);
            }
            for it in &items {
                check_order(doc, it, what, depth + 1, stack, nodes)?;
            }
            if reference.is_some() {
                stack.pop();
            }
            Ok(())
        }
        Object::Dict(_) if reference.is_some() => {
            check_group(doc, reference.expect("checked"), &format!("{what} /Order"))
        }
        Object::String(_) if reference.is_none() => Ok(()),
        _ => Err(Error::Structure(format!(
            "{what} /Order entries must be OCG references, arrays or labels"
        ))),
    }
}
