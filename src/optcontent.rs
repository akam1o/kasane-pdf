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
