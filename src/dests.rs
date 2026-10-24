//! Named destinations (§12.3.2.3) — enumeration and resolution.
//!
//! A named destination is a name or string that resolves, through the
//! catalog's `/Names` → `/Dests` name tree (PDF ≥ 1.2) or the legacy
//! catalog `/Dests` dictionary (PDF 1.1), to a destination: an array
//! `[page /XYZ left top zoom]` (page first, then the view) or a
//! dictionary whose `/D` holds that array.
//!
//! This module *reads* those structures. The rewrite path copies them
//! verbatim (object graph, renumbered refs), so what this module
//! returns for the input and for the rewritten output must agree —
//! that equality is the round-trip guarantee the tests assert.
//! Nothing here synthesises or repairs destinations: an entry whose
//! page reference does not resolve to a page of this document is
//! reported with `page: None`, never dropped or guessed.
//!
//! Bounds: the tree walk is limited by [`Limits::max_page_tree_depth`]
//! (depth) and [`Limits::max_page_tree_nodes`] (nodes visited), and a
//! node reached twice (a cyclic `/Kids` chain) is an error.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{IndirectRef, Object};
use std::collections::HashSet;

/// Where a named destination was found.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestSource {
    /// Catalog `/Names` → `/Dests` name tree.
    NameTree,
    /// Legacy catalog `/Dests` dictionary.
    LegacyDests,
}

/// One named destination, resolved as far as the document allows.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedDest {
    /// The key (a PDF string in the name tree, a name in the legacy
    /// dictionary) — raw bytes, no decoding.
    pub name: Vec<u8>,
    /// 0-based index of the target page, or `None` when the page
    /// entry is not a reference to one of this document's pages
    /// (a remote-document integer, a dangling ref, …).
    pub page: Option<usize>,
    /// The view part of the destination array — `/XYZ left top zoom`,
    /// `/Fit`, `/FitH top`, … — exactly as stored (direct objects).
    pub view: Vec<Object>,
    /// Where the name was found (the name tree or the legacy
    /// catalog `/Dests` dictionary).
    pub source: DestSource,
}

/// All named destinations of `doc`, in tree (sorted-key) order for
/// the name tree followed by dictionary order for the legacy `/Dests`.
pub fn named_destinations(doc: &Document, limits: &Limits) -> Result<Vec<NamedDest>, Error> {
    limits.validate()?;
    let mut out = Vec::new();
    let catalog = doc.catalog();

    if let Some(names_obj) = catalog.get(b"Names".as_slice()) {
        let names = doc.deref(names_obj)?;
        if let Some(nd) = names.as_dict() {
            if let Some(dests_obj) = nd.get(b"Dests".as_slice()) {
                let mut walker = TreeWalker {
                    doc,
                    limits,
                    seen: HashSet::new(),
                    nodes: 0,
                };
                walker.walk(dests_obj, 0, &mut out)?;
            }
        }
    }

    if let Some(dests_obj) = catalog.get(b"Dests".as_slice()) {
        let dests = doc.deref(dests_obj)?;
        let dd = dests
            .as_dict()
            .ok_or_else(|| Error::Structure("catalog /Dests is not a dictionary".into()))?;
        if dd.len() > limits.max_dict_entries {
            return Err(Error::LimitExceeded {
                limit: "max_dict_entries (/Dests)",
                max: limits.max_dict_entries as u64,
            });
        }
        for (k, v) in dd.iter() {
            let (page, view) = resolve_dest_value(doc, v)?;
            out.push(NamedDest {
                name: k.clone(),
                page,
                view,
                source: DestSource::LegacyDests,
            });
        }
    }
    Ok(out)
}

/// Resolve one named destination by key, searching the name tree
/// first, then the legacy dictionary. `Ok(None)` when absent.
pub fn resolve_named_destination(
    doc: &Document,
    name: &[u8],
    limits: &Limits,
) -> Result<Option<NamedDest>, Error> {
    limits.validate()?;
    Ok(named_destinations(doc, limits)?
        .into_iter()
        .find(|d| d.name == name))
}

struct TreeWalker<'a> {
    doc: &'a Document,
    limits: &'a Limits,
    seen: HashSet<IndirectRef>,
    nodes: u64,
}

impl TreeWalker<'_> {
    fn walk(
        &mut self,
        node_obj: &Object,
        depth: u32,
        out: &mut Vec<NamedDest>,
    ) -> Result<(), Error> {
        if depth > self.limits.max_page_tree_depth {
            return Err(Error::LimitExceeded {
                limit: "max_page_tree_depth (name tree)",
                max: self.limits.max_page_tree_depth as u64,
            });
        }
        self.nodes += 1;
        if self.nodes > self.limits.max_page_tree_nodes {
            return Err(Error::LimitExceeded {
                limit: "max_page_tree_nodes (name tree)",
                max: self.limits.max_page_tree_nodes,
            });
        }
        if let Some(r) = node_obj.as_ref() {
            if !self.seen.insert(r) {
                return Err(Error::CycleDetected {
                    context: "name tree /Kids",
                });
            }
        }
        let node = self.doc.deref(node_obj)?;
        let nd = node
            .as_dict()
            .ok_or_else(|| Error::Structure("name tree node is not a dictionary".into()))?;

        if let Some(kids_obj) = nd.get(b"Kids".as_slice()) {
            let kids = self.doc.deref(kids_obj)?;
            let kids = kids
                .as_array()
                .ok_or_else(|| Error::Structure("name tree /Kids is not an array".into()))?;
            for kid in kids {
                self.walk(kid, depth + 1, out)?;
            }
        }
        if let Some(names_obj) = nd.get(b"Names".as_slice()) {
            let names = self.doc.deref(names_obj)?;
            let names = names
                .as_array()
                .ok_or_else(|| Error::Structure("name tree /Names is not an array".into()))?;
            if names.len() % 2 != 0 {
                return Err(Error::Structure(
                    "name tree /Names must hold key/value pairs".into(),
                ));
            }
            for pair in names.chunks(2) {
                let key = self.doc.deref(&pair[0])?;
                let key = key
                    .as_string()
                    .map(|s| s.to_vec())
                    .or_else(|| key.as_name().map(|n| n.to_vec()))
                    .ok_or_else(|| Error::Structure("name tree key is not a string".into()))?;
                let (page, view) = resolve_dest_value(self.doc, &pair[1])?;
                out.push(NamedDest {
                    name: key,
                    page,
                    view,
                    source: DestSource::NameTree,
                });
            }
        }
        Ok(())
    }
}

/// A destination value: an array, or a dictionary with `/D`. Returns
/// the page index (when the first element references one of the
/// document's pages) and the view elements that follow it.
fn resolve_dest_value(doc: &Document, v: &Object) -> Result<(Option<usize>, Vec<Object>), Error> {
    let v = doc.deref(v)?;
    let arr: Vec<Object> = match &v {
        Object::Array(a) => a.clone(),
        Object::Dict(d) => {
            let d_obj = d
                .get(b"D".as_slice())
                .ok_or_else(|| Error::Structure("destination dictionary has no /D".into()))?;
            let d_val = doc.deref(d_obj)?;
            d_val
                .as_array()
                .map(|a| a.to_vec())
                .ok_or_else(|| Error::Structure("destination /D is not an array".into()))?
        }
        _ => {
            return Err(Error::Structure(
                "destination is neither an array nor a dictionary".into(),
            ))
        }
    };
    if arr.is_empty() {
        return Err(Error::Structure("destination array is empty".into()));
    }
    let page = match &arr[0] {
        Object::Ref(r) => doc.pages().iter().position(|p| p.object_ref() == Some(*r)),
        // A bare integer is a page *index* in a remote-GoTo
        // destination; inside this document it does not name one of
        // our page objects, so it stays unresolved rather than guessed.
        _ => None,
    };
    let mut view = Vec::with_capacity(arr.len() - 1);
    for o in &arr[1..] {
        view.push(doc.deref(o)?);
    }
    Ok((page, view))
}
