//! Document loading: header → startxref → xref chain → trailer →
//! catalog → page tree.
//!
//! [`Document::from_bytes`] is the single entry point. It either returns
//! a fully consistent [`Document`] or an error — partial parse results
//! are never exposed. All limits come from [`Limits`].

use crate::content::{self, Operation};
use crate::error::{Error, MissingReference};
use crate::filter;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::objstm::ObjectStream;
use crate::page::{Page, Rect};
use crate::parser::Parser;
use crate::xref::{self, XrefTable};
use std::collections::{HashMap, HashSet};

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
    /// `%PDF-M.m` header version from the input file.
    header_version: (u8, u8),
    /// Remaining indirect-reference resolution budget.
    ref_budget: std::cell::Cell<u64>,
    /// Remaining cumulative decoded-bytes budget (shared across all
    /// streams decoded via this document).
    decoded_budget: std::cell::Cell<u64>,
    /// Cache of decoded object streams, keyed by object-stream number.
    /// Bounded implicitly by the decoded-bytes budget.
    objstm_cache: std::cell::RefCell<HashMap<u32, ObjectStream>>,
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
        let header_version = parser.check_header()?;
        let startxref = parser.find_startxref()?;

        let xref = xref::read_xref_chain(data, &limits, startxref)?;

        // Refuse encrypted files entirely.
        if xref.trailer.contains_key(b"Encrypt".as_slice()) {
            return Err(Error::Encrypted);
        }

        let mut doc = Document {
            data: data.to_vec(),
            xref,
            catalog: Dict::new(),
            pages: Vec::new(),
            header_version,
            ref_budget: std::cell::Cell::new(limits.max_reference_depth),
            decoded_budget: std::cell::Cell::new(limits.max_total_decoded_bytes),
            objstm_cache: std::cell::RefCell::new(HashMap::new()),
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

            if let Some((objstm_num, index)) = self.xref.compressed_location(current) {
                // Type-2 entry: resolve via the object stream.
                return self.resolve_compressed(current, objstm_num, index);
            }

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

    /// Resolve an object stored inside an object stream (xref type 2).
    fn resolve_compressed(
        &self,
        want: IndirectRef,
        objstm_num: u32,
        index: u32,
    ) -> Result<Object, Error> {
        if !self.objstm_cache.borrow().contains_key(&objstm_num) {
            let objstm_ref = IndirectRef::new(objstm_num, 0);
            let objstm_obj = self.resolve_indirect_no_ref_follow(objstm_ref)?;
            let (dict, raw) = objstm_obj.as_stream().ok_or_else(|| {
                Error::Structure(format!("object stream {objstm_num} is not a stream"))
            })?;
            if dict.get(b"Type".as_slice()).and_then(|o| o.as_name()) != Some(b"ObjStm") {
                return Err(Error::Structure(format!(
                    "stream {objstm_num} is not /Type /ObjStm"
                )));
            }
            let decoded = self.decode_stream(dict, raw)?;
            let parsed = ObjectStream::parse(&decoded, dict, (want.number, index), &self.limits)?;
            self.objstm_cache.borrow_mut().insert(objstm_num, parsed);
        }
        let cache = self.objstm_cache.borrow();
        let os = cache.get(&objstm_num).unwrap();
        os.get(want.number).cloned().ok_or({
            Error::MissingReference(MissingReference {
                reference: want,
                context: "object stream member",
            })
        })
    }

    /// Resolve one level of indirection without following `Ref` chains
    /// (used for the object-stream container itself).
    fn resolve_indirect_no_ref_follow(&self, r: IndirectRef) -> Result<Object, Error> {
        let offset = self.xref.offset_of(r).ok_or({
            Error::MissingReference(MissingReference {
                reference: r,
                context: "object stream",
            })
        })?;
        let mut parser = Parser::new(&self.data, &self.limits);
        parser.seek(offset as usize);
        parser
            .parse_indirect_object()?
            .map(|io| io.object)
            .ok_or_else(|| {
                Error::Parse(format!(
                    "no indirect object at xref offset {offset} (for {r})"
                ))
            })
    }

    /// Decode a stream body according to its dictionary's `/Filter` and
    /// `/DecodeParms`, bounded by `max_stream_bytes` and the shared
    /// `decoded_budget`.
    pub(crate) fn decode_stream(&self, dict: &Dict, raw: &[u8]) -> Result<Vec<u8>, Error> {
        let out = filter::decode_stream(dict, raw, &self.limits)?;
        let cost = out.len() as u64;
        if cost > self.decoded_budget.get() {
            return Err(Error::LimitExceeded {
                limit: "max_total_decoded_bytes",
                max: self.limits.max_total_decoded_bytes,
            });
        }
        self.decoded_budget.set(self.decoded_budget.get() - cost);
        Ok(out)
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

    /// The `%PDF-M.m` header version from the input file.
    pub(crate) fn header_version(&self) -> (u8, u8) {
        self.header_version
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
    // Page contents (M1)
    // ---------------------------------------------------------------

    /// Fetch the decoded contents of `page` (the page's `/Contents`
    /// stream or array of streams).
    ///
    /// - `/Contents` absent → `Ok(vec![])` (empty content).
    /// - Single stream or an array of streams → decoded bodies in order.
    /// - Each entry must be a stream object (indirect or direct); other
    ///   shapes are `Error::Structure`/`Error::MissingReference`.
    /// - Unsupported filters or decode failures are reported when (and
    ///   only when) the contents are actually requested — a document
    ///   with e.g. an unsupported image filter still parses and reports
    ///   pages normally.
    pub fn page_contents(&self, page: &Page) -> Result<Vec<Vec<u8>>, Error> {
        let contents = match page.contents() {
            None => return Ok(Vec::new()),
            Some(c) => c.clone(),
        };
        let resolved = self.deref(&contents)?;
        let items: Vec<Object> = match resolved {
            Object::Array(a) => a,
            other => vec![other],
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let obj = self.deref(&item)?;
            let (dict, raw) = obj
                .as_stream()
                .ok_or_else(|| Error::Structure("page /Contents entry is not a stream".into()))?;
            out.push(self.decode_stream(dict, raw)?);
        }
        Ok(out)
    }

    /// Parse the page's `/Contents` into typed operations (M2).
    ///
    /// Calls [`page_contents`](Self::page_contents) then parses the
    /// concatenated streams as one logical sequence. Errors:
    /// * `Error::Unsupported` — inline images (`BI`/`ID`/`EI`)
    /// * `Error::Parse` — unknown operator, wrong arity/types
    /// * `Error::Structure` — unbalanced `q`/`Q`, `BT`/`ET`, `BX`/`EX`,
    ///   `BMC`/`BDC`/`EMC`
    /// * `Error::LimitExceeded` — too many operations or operands
    ///
    /// Parsing never returns a partial operation list: any error
    /// discards all operations parsed so far.
    pub fn page_operations(&self, page: &Page) -> Result<Vec<Operation>, Error> {
        let contents = self.page_contents(page)?;
        content::parse_content_stream(&contents, &self.limits)
    }

    // ---------------------------------------------------------------
    // Page resources (M2)
    // ---------------------------------------------------------------

    /// Resolve a named entry inside one of a page's resource
    /// categories (ISO 32000-1:2008, §7.8.3).
    ///
    /// `category` is a top-level `/Resources` key such as `"Font"`,
    /// `"XObject"`, `"ExtGState"`, `"ColorSpace"`, `"Pattern"`,
    /// `"Shading"` or `"Properties"`; `name` is the resource name
    /// without the leading `/` (e.g. `b"F1"` for `/F1`).
    ///
    /// The page's effective resources are used — `/Resources` is an
    /// inheritable attribute (Table 30), so a page that omits it uses
    /// the nearest ancestor's dictionary *as a whole*; there is no
    /// per-key merge. The category sub-dictionary is resolved one
    /// level, then `name` is looked up and the result resolved one
    /// level and returned as the raw object — fonts, XObjects and the
    /// like are **not** interpreted or executed.
    ///
    /// Returns:
    /// * `Ok(Some(obj))` — the entry's value (resolved if indirect).
    /// * `Ok(None)` — no resources, no such category, or no such
    ///   name (or the entry is `null`).
    /// * `Err(Error::Structure)` — the resources or the category is
    ///   not a dictionary.
    /// * `Err(Error::MissingReference)` — an indirect reference in the
    ///   path is undefined.
    pub fn page_resource(
        &self,
        page: &Page,
        category: &str,
        name: &[u8],
    ) -> Result<Option<Object>, Error> {
        let resources = match page.resources() {
            None => return Ok(None),
            Some(r) => r.clone(),
        };
        self.resolve_resource(&resources, category, name)
    }

    /// Resolve `name` in `category` of an arbitrary `/Resources`
    /// dictionary — the same lookup [`Self::page_resource`] performs, but
    /// against a supplied dictionary (a Form XObject's own resources).
    ///
    /// Per §7.8.3 a form without `/Resources` inherits the caller's
    /// resource dictionary (an obsolete-but-legal ≤1.1 convention);
    /// callers pass whichever dictionary is in effect.
    ///
    /// Returns the same three-state result as [`Self::page_resource`]:
    /// `Ok(None)` for absent resources/categories/names,
    /// `Err(Error::Structure)` for a non-dictionary resources or
    /// category, `Err(Error::MissingReference)` for unresolved refs.
    pub fn resolve_resource(
        &self,
        resources: &Object,
        category: &str,
        name: &[u8],
    ) -> Result<Option<Object>, Error> {
        let res = self.deref(resources)?;
        let res_dict = res
            .as_dict()
            .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?;
        let category_obj = match res_dict.get(category.as_bytes()) {
            None => return Ok(None),
            Some(c) => self.deref(c)?,
        };
        let cat_dict = category_obj.as_dict().ok_or_else(|| {
            Error::Structure(format!("/Resources /{category} is not a dictionary"))
        })?;
        match cat_dict.get(name) {
            None => Ok(None),
            Some(entry) => Ok(Some(self.deref(entry)?)),
        }
    }

    /// Like [`Self::resolve_resource`] but returns the *unresolved* entry —
    /// the raw `Object` stored in the category dictionary, which may
    /// still be an `Object::Ref`. Used by the renderer to track which
    /// indirect object a `Do` call refers to (for recursion detection)
    /// without a second lookup.
    pub fn resolve_resource_raw(
        &self,
        resources: &Object,
        category: &str,
        name: &[u8],
    ) -> Result<Option<Object>, Error> {
        let res = self.deref(resources)?;
        let res_dict = res
            .as_dict()
            .ok_or_else(|| Error::Structure("/Resources is not a dictionary".into()))?;
        let category_obj = match res_dict.get(category.as_bytes()) {
            None => return Ok(None),
            Some(c) => self.deref(c)?,
        };
        let cat_dict = category_obj.as_dict().ok_or_else(|| {
            Error::Structure(format!("/Resources /{category} is not a dictionary"))
        })?;
        match cat_dict.get(name) {
            None => Ok(None),
            Some(entry) => Ok(Some(entry.clone())),
        }
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
