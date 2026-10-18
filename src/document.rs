//! Document loading: header → startxref → xref chain → trailer →
//! catalog → page tree.
//!
//! [`Document::from_bytes`] is the single entry point. It either returns
//! a fully consistent [`Document`] or an error — partial parse results
//! are never exposed. All limits come from [`Limits`].

use crate::error::{Error, MissingReference};
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::page::{Page, Rect};
use crate::parser::Parser;
use crate::xref::{self, XrefTable};
use std::collections::HashSet;

/// Inheritable page attributes accumulated down the page tree
/// (ISO 32000-1:2008, Table 30).
#[derive(Debug, Default, Clone)]
struct InheritedAttrs {
    media_box: Option<Rect>,
    crop_box: Option<Rect>,
    rotate: Option<i64>,
    resources: Option<Object>,
}

/// A fully parsed PDF document.
///
/// Owns the input bytes plus the xref table and trailer. Object
/// resolution is lazy through [`Document::resolve`]; the page tree is
/// walked eagerly at load time (it is small and security-sensitive, so
/// validating it up front is deliberate).
#[derive(Debug)]
pub struct Document {
    data: Vec<u8>,
    xref: XrefTable,
    catalog: Dict,
    pages: Vec<Page>,
    limits: Limits,
    /// Remaining indirect-reference resolution budget.
    ref_budget: std::cell::Cell<u64>,
}

impl Document {
    /// Parse `data` with default [`Limits`].
    pub fn from_bytes(data: &[u8]) -> Result<Document, Error> {
        Self::from_bytes_with_limits(data, Limits::default())
    }

    /// Parse `data` with explicit [`Limits`].
    ///
    /// Errors:
    /// * [`Error::NotPdf`] — no `%PDF-` header in the first 1024 bytes.
    /// * [`Error::Parse`]/[`Error::UnexpectedEof`]/[`Error::Structure`] —
    ///   malformed or truncated input.
    /// * [`Error::Encrypted`] — trailer has `/Encrypt`.
    /// * [`Error::Unsupported`] — xref streams, object streams, and other
    ///   features outside M0.
    /// * [`Error::MissingReference`] — a required indirect object is
    ///   absent from the xref table.
    /// * [`Error::CycleDetected`]/[`Error::LimitExceeded`] — hostile
    ///   structure (reference loops, `/Prev` loops, oversized trees).
    pub fn from_bytes_with_limits(data: &[u8], limits: Limits) -> Result<Document, Error> {
        if data.len() > limits.max_input_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_input_bytes",
                max: limits.max_input_bytes as u64,
            });
        }

        let mut parser = Parser::new(data, &limits);
        parser.check_header()?;
        let startxref = parser.find_startxref()?;

        let xref = xref::read_xref_chain(data, &limits, startxref)?;

        // Refuse encrypted files entirely.
        if xref.trailer.contains_key(b"Encrypt".as_slice()) {
            return Err(Error::Encrypted);
        }
        // Hybrid-reference files also carry an xref stream we would
        // silently ignore; reject them instead of reading a stale view.
        if xref.trailer.contains_key(b"XRefStm".as_slice()) {
            return Err(Error::Unsupported {
                feature: "hybrid-reference file",
                detail: "trailer contains /XRefStm".to_string(),
            });
        }

        let mut doc = Document {
            data: data.to_vec(),
            xref,
            catalog: Dict::new(),
            pages: Vec::new(),
            ref_budget: std::cell::Cell::new(limits.max_reference_depth),
            limits,
        };

        doc.load_catalog()?;
        doc.load_pages()?;
        Ok(doc)
    }

    // ---------------------------------------------------------------
    // Object resolution
    // ---------------------------------------------------------------

    /// Resolve one indirect reference to a clone of its definition.
    ///
    /// Chained references (`5 0 obj` containing `7 0 R`) are followed.
    /// Bounded by `max_reference_depth` total resolutions and by cycle
    /// detection on the chain itself. Free entries and absent objects
    /// produce [`Error::MissingReference`].
    pub fn resolve(&self, r: IndirectRef) -> Result<Object, Error> {
        let mut seen = HashSet::new();
        let mut current = r;
        loop {
            if !seen.insert(current) {
                return Err(Error::CycleDetected {
                    context: "indirect reference chain",
                });
            }
            if self.ref_budget.get() == 0 {
                return Err(Error::LimitExceeded {
                    limit: "max_reference_depth",
                    max: self.limits.max_reference_depth,
                });
            }
            self.ref_budget.set(self.ref_budget.get() - 1);

            let offset = self.xref.offset_of(current).ok_or({
                Error::MissingReference(MissingReference {
                    reference: current,
                    context: "indirect object",
                })
            })?;

            let mut parser = Parser::new(&self.data, &self.limits);
            parser.seek(offset as usize);
            let obj = parser.parse_indirect_object()?.ok_or_else(|| {
                Error::Parse(format!(
                    "no indirect object at xref offset {offset} (for {})",
                    current
                ))
            })?;
            if obj.object_ref != current {
                // The offset must point at `n g obj` for the same n and g
                // (spec). Tolerate same-number/different-generation since
                // some writers are loose; mismatching number is corrupt.
                if obj.object_ref.number != current.number {
                    return Err(Error::Parse(format!(
                        "xref offset {offset} points at {} but xref entry was for {}",
                        obj.object_ref, current
                    )));
                }
            }
            match obj.object {
                Object::Ref(next) => current = next,
                other => return Ok(other),
            }
        }
    }

    /// Resolve `obj` if it is a reference; pass other values through.
    pub fn deref(&self, obj: &Object) -> Result<Object, Error> {
        match obj {
            Object::Ref(r) => self.resolve(*r),
            other => Ok(other.clone()),
        }
    }

    /// The merged trailer dictionary (newest section, older fills).
    pub fn trailer(&self) -> &Dict {
        &self.xref.trailer
    }

    /// The document catalog dictionary (already resolved).
    pub fn catalog(&self) -> &Dict {
        &self.catalog
    }

    /// Pages in document order, with inherited attributes applied.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// Number of pages in the document.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    // ---------------------------------------------------------------
    // Catalog and page tree
    // ---------------------------------------------------------------

    fn load_catalog(&mut self) -> Result<(), Error> {
        let root_obj = self
            .xref
            .trailer
            .get(b"Root".as_slice())
            .ok_or_else(|| Error::Structure("trailer has no /Root".into()))?
            .clone();
        let catalog = self.deref(&root_obj)?;
        let dict = catalog
            .as_dict()
            .ok_or_else(|| Error::Structure("/Root is not a dictionary".into()))?
            .clone();

        // If /Type is present it must be /Catalog (many writers omit it).
        if let Some(t) = dict.get(b"Type".as_slice()) {
            if t.as_name() != Some(b"Catalog") {
                return Err(Error::Structure(format!(
                    "/Root /Type is /{}, expected /Catalog",
                    t.as_name()
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_else(|| "<non-name>".into())
                )));
            }
        }
        self.catalog = dict;
        Ok(())
    }

    fn load_pages(&mut self) -> Result<(), Error> {
        let pages_obj = self
            .catalog
            .get(b"Pages".as_slice())
            .ok_or_else(|| Error::Structure("catalog has no /Pages".into()))?
            .clone();
        let pages_dict = self.deref(&pages_obj)?;
        let dict = pages_dict
            .as_dict()
            .ok_or_else(|| Error::Structure("/Pages is not a dictionary".into()))?
            .clone();
        let root_ref = pages_obj.as_ref();

        let mut visited: HashSet<IndirectRef> = HashSet::new();
        let mut nodes: u64 = 0;
        let mut pages = Vec::new();
        self.walk_page_tree(
            &dict,
            root_ref,
            &InheritedAttrs::default(),
            0,
            &mut visited,
            &mut nodes,
            &mut pages,
        )?;

        // Verify /Count consistency on the root node (best-effort check;
        // intermediate counts are not re-validated).
        if let Some(count) = dict.get(b"Count".as_slice()).and_then(|o| o.as_i64()) {
            if count >= 0 && count as usize != pages.len() {
                return Err(Error::Structure(format!(
                    "page tree /Count {count} but {} leaf pages found",
                    pages.len()
                )));
            }
        }
        self.pages = pages;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_page_tree(
        &self,
        node: &Dict,
        node_ref: Option<IndirectRef>,
        inherited: &InheritedAttrs,
        depth: u32,
        visited: &mut HashSet<IndirectRef>,
        nodes: &mut u64,
        pages: &mut Vec<Page>,
    ) -> Result<(), Error> {
        if depth > self.limits.max_page_tree_depth {
            return Err(Error::LimitExceeded {
                limit: "max_page_tree_depth",
                max: self.limits.max_page_tree_depth as u64,
            });
        }
        *nodes += 1;
        if *nodes > self.limits.max_page_tree_nodes {
            return Err(Error::LimitExceeded {
                limit: "max_page_tree_nodes",
                max: self.limits.max_page_tree_nodes,
            });
        }
        if let Some(r) = node_ref {
            if !visited.insert(r) {
                return Err(Error::CycleDetected {
                    context: "page tree",
                });
            }
        }

        // Merge inheritable attributes (this node wins over ancestors).
        let mut attrs = inherited.clone();
        if let Some(obj) = node.get(b"MediaBox".as_slice()) {
            let v = self.deref(obj)?;
            attrs.media_box =
                Some(Rect::from_object(&v).ok_or_else(|| {
                    Error::Structure("/MediaBox is not a 4-number rectangle".into())
                })?);
        }
        if let Some(obj) = node.get(b"CropBox".as_slice()) {
            let v = self.deref(obj)?;
            attrs.crop_box =
                Some(Rect::from_object(&v).ok_or_else(|| {
                    Error::Structure("/CropBox is not a 4-number rectangle".into())
                })?);
        }
        if let Some(obj) = node.get(b"Rotate".as_slice()) {
            let v = self.deref(obj)?;
            let rot = v
                .as_i64()
                .ok_or_else(|| Error::Structure("/Rotate is not an integer".into()))?;
            if rot % 90 != 0 {
                return Err(Error::Structure(format!(
                    "/Rotate {rot} is not a multiple of 90"
                )));
            }
            attrs.rotate = Some(rot);
        }
        if let Some(obj) = node.get(b"Resources".as_slice()) {
            attrs.resources = Some(self.deref(obj)?);
        }

        // Determine node kind. A node with a /Kids array is intermediate
        // even if /Type is missing or wrongly says /Page; otherwise it is
        // a leaf page. Any other /Type is a structural error.
        let has_kids = node.contains_key(b"Kids".as_slice());
        let type_name = node.get(b"Type".as_slice()).and_then(|o| o.as_name());
        let is_intermediate = match type_name {
            Some(t) if t == b"Pages" => true,
            Some(t) if t == b"Page" => has_kids,
            None => has_kids,
            Some(other) => {
                return Err(Error::Structure(format!(
                    "unexpected /Type /{} in page tree",
                    String::from_utf8_lossy(other)
                )))
            }
        };

        if is_intermediate {
            let kids_obj = node
                .get(b"Kids".as_slice())
                .ok_or_else(|| Error::Structure("page tree node has no /Kids".into()))?;
            let kids = self.deref(kids_obj)?;
            let kids = kids
                .as_array()
                .ok_or_else(|| Error::Structure("/Kids is not an array".into()))?;
            for kid in kids {
                let kid_obj = self.deref(kid)?;
                let kid_dict = kid_obj
                    .as_dict()
                    .ok_or_else(|| Error::Structure("/Kids entry is not a dictionary".into()))?;
                self.walk_page_tree(
                    kid_dict,
                    kid.as_ref(),
                    &attrs,
                    depth + 1,
                    visited,
                    nodes,
                    pages,
                )?;
            }
            return Ok(());
        }

        let media_box = attrs.media_box.ok_or_else(|| {
            Error::Structure(format!(
                "page {} has no MediaBox (not set on page or any ancestor)",
                pages.len() + 1
            ))
        })?;
        let crop_box = attrs.crop_box.unwrap_or(media_box);
        let rotate = attrs.rotate.unwrap_or(0);
        let contents = node.get(b"Contents".as_slice()).cloned();
        pages.push(Page::new(
            pages.len(),
            node_ref,
            media_box,
            crop_box,
            rotate,
            attrs.resources.clone(),
            contents,
        ));
        Ok(())
    }
}
