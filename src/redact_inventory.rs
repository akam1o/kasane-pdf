//! What of a document the redaction reads, and what it carries
//! without reading it: [`uninspected_data`].
//!
//! Every object reachable from the trailer is reached by its role — a
//! page's `/Contents`, a font's `/FontFile2`, an annotation's `/AP`, a
//! file specification's `/EF` — and each stream gets one of three
//! statuses:
//!
//! - [`DataStatus::Searched`]: content whose text `redact` extracts and
//!   searches for copies of what it removed (`Redaction::elsewhere`):
//!   page content, and the forms pages and annotation appearances draw.
//! - [`DataStatus::Interpreted`]: data of a known format in a known role
//!   — images, font programs, CMaps, functions, shading and pattern
//!   streams, ICC profiles, Type 3 glyph procedures — that a reader
//!   shows only through the pages. Its format is understood; it is not
//!   searched (text in an image's pixels or a font program is not
//!   found).
//! - [`DataStatus::Uninspected`]: carried as it is and never
//!   interpreted — attachments, XMP metadata, thumbnails, scripts, XFA,
//!   rich text, multimedia, private application data, forms nothing
//!   draws, any stream under a key no structure modelled here names,
//!   and any entry of the catalog or a page that none names. It may hold
//!   anything, a copy of the redacted content included.
//!
//! Non-stream objects are not classified: their strings are searched as
//! text for `elsewhere`. Unknown entries of the catalog, the page tree
//! and pages are the exception — they are listed, whatever they hold.
//! Objects nothing refers to are [`DataStatus::NotCarried`]: `redact`
//! writes only what is reachable (object and cross-reference streams
//! are the file's structure, counted apart).
//!
//! "Uninspected" means only that: not read. Nothing here searches the
//! bytes of such data — compression, encryption, encodings and
//! application formats make a byte search no evidence of absence.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use std::collections::{HashMap, HashSet, VecDeque};

/// How [`redact`](crate::redact) treats one piece of data.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataStatus {
    /// Its text is extracted and searched for copies of the removed
    /// content.
    Searched,
    /// A known format in a known role (its use is not checked, but for
    /// forms and images);
    /// not searched.
    Interpreted,
    /// Carried as it is; never read.
    Uninspected,
    /// Nothing refers to it: `redact` does not write it.
    NotCarried,
}

/// The role of an item of [`DataInventory::items`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataRole {
    /// An embedded file (`/EF`, `/RF`): an attachment, an associated
    /// file.
    EmbeddedFile,
    /// An XMP metadata stream (`/Metadata`).
    Metadata,
    /// A page or file thumbnail image (`/Thumb`).
    Thumbnail,
    /// JavaScript source (`/JS` as a stream).
    JavaScript,
    /// An XFA form packet.
    Xfa,
    /// Rich text (`/RC`, `/RV`, a field value as a stream).
    RichText,
    /// Sound, movie, rendition, rich media or 3D data.
    Multimedia,
    /// Application data (`/PieceInfo`).
    PrivateData,
    /// A PostScript XObject or an OPI proxy.
    PostScript,
    /// A halftone.
    Halftone,
    /// A template page (`/Names /Templates`, `/Names /Pages`).
    Template,
    /// An image or form XObject nothing draws (a named appearance, an
    /// icon, an unused resource): not shown, its content not searched.
    Undrawn,
    /// A stream under a key no modelled structure names.
    UnknownStream,
    /// An entry of the catalog, the page tree or a page that no
    /// modelled structure names (its value, whatever it is).
    UnknownEntry,
    /// An object nothing refers to.
    Unreachable,
}

impl DataRole {
    /// What to do about it, in a few words.
    pub fn recommendation(self) -> &'static str {
        match self {
            DataRole::EmbeddedFile => "review the attachment, or remove it before redacting",
            DataRole::Metadata => "remove the metadata (remove_document_metadata) or review it",
            DataRole::Thumbnail => "remove the thumbnail: it pictures the page as it was",
            DataRole::JavaScript => "remove the scripts (RedactOptions::remove_javascript)",
            DataRole::Xfa => "flatten or remove the XFA form",
            DataRole::RichText => "review the annotation or field text",
            DataRole::Multimedia => "review or remove the media",
            DataRole::PrivateData => "remove the application data (/PieceInfo)",
            DataRole::PostScript => "remove the PostScript or OPI data",
            DataRole::Halftone => "review the halftone",
            DataRole::Template => "review or remove the template pages",
            DataRole::Undrawn => "remove the unused image or form, or review it",
            DataRole::UnknownStream | DataRole::UnknownEntry => "review the data or remove it",
            DataRole::Unreachable => "none: redact does not write it",
        }
    }
}

/// One item of [`DataInventory`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DataItem {
    /// The object; `None` for a direct value (see `via`).
    pub object: Option<IndirectRef>,
    /// What it is.
    pub role: DataRole,
    /// How `redact` treats it.
    pub status: DataStatus,
    /// The first path found to it from the trailer (`/Root/Names/…`).
    pub via: String,
    /// Stored (encoded) length of a stream.
    pub stream_bytes: Option<u64>,
    /// A stream's filters, in order.
    pub filters: Vec<String>,
}

/// Every stream of a document by status, with the data `redact` does not
/// read listed: see the module documentation.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct DataInventory {
    /// Items that are [`DataStatus::Uninspected`] or
    /// [`DataStatus::NotCarried`], in object order (at most
    /// `max_audit_findings`).
    pub items: Vec<DataItem>,
    /// Streams whose text is searched.
    pub searched: u64,
    /// Streams interpreted, not searched.
    pub interpreted: u64,
    /// Uninspected items (streams and unknown entries).
    pub uninspected: u64,
    /// Objects nothing refers to (`redact` does not write them).
    pub not_carried: u64,
    /// Object streams and cross-reference streams (the file's own
    /// structure; never written by `redact`).
    pub structural: u64,
    /// `false` when a limit stopped the walk: then what was not reached
    /// is not classified, and nothing may be read into its absence.
    pub complete: bool,
}

impl DataInventory {
    /// The uninspected items.
    pub fn uninspected_items(&self) -> impl Iterator<Item = &DataItem> {
        self.items
            .iter()
            .filter(|i| i.status == DataStatus::Uninspected)
    }
}

/// The role an object is reached in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Ctx {
    Catalog,
    Pages,
    Page,
    Contents,
    Resources,
    /// Resources of a pattern, a Type 3 font, a soft mask or a form's
    /// default resources: what they draw is not followed by the content
    /// walk that decides what is drawn.
    ResourcesOther,
    XObjectOther,
    FormOther,
    /// A resource dictionary of `Ctx` values (`/Font`, `/XObject` …).
    Map(Sub),
    Font,
    FontDescriptor,
    FontProgram,
    Cmap,
    CharProcs,
    XObject,
    Image,
    Form,
    Pattern,
    Shading,
    Function,
    ExtGState,
    SoftMask,
    ColorSpace,
    Icc,
    Annots,
    /// A transparency group dictionary.
    Group,
    Annot,
    Appearance,
    /// An appearance: a stream, or a dictionary of states.
    ApState,
    Action,
    Aa,
    Names,
    NameTree(Sub),
    /// A name tree's `/Kids`.
    NameKids(Sub),
    FileSpec,
    Ef,
    AcroForm,
    Outline,
    OutputIntents,
    /// Data of an uninspected role, whatever is below it.
    Opaque(DataRole),
    /// Under a key no modelled structure names: streams are unknown.
    Generic,
    /// Under an unknown entry of the catalog, page tree or a page.
    Unknown,
}

/// What a map or name tree holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Sub {
    Font,
    XObject,
    XObjectOther,
    ExtGState,
    ColorSpace,
    Pattern,
    Shading,
    Generic,
    FileSpec,
    Action,
    Undrawn,
    Template,
    Multimedia,
}

impl Sub {
    fn ctx(self) -> Ctx {
        match self {
            Sub::Font => Ctx::Font,
            Sub::XObject => Ctx::XObject,
            Sub::XObjectOther => Ctx::XObjectOther,
            Sub::ExtGState => Ctx::ExtGState,
            Sub::ColorSpace => Ctx::ColorSpace,
            Sub::Pattern => Ctx::Pattern,
            Sub::Shading => Ctx::Shading,
            Sub::Generic => Ctx::Generic,
            Sub::FileSpec => Ctx::FileSpec,
            Sub::Action => Ctx::Action,
            Sub::Undrawn => Ctx::Opaque(DataRole::Undrawn),
            Sub::Template => Ctx::Opaque(DataRole::Template),
            Sub::Multimedia => Ctx::Opaque(DataRole::Multimedia),
        }
    }
}

const CATALOG_KEYS: &[&[u8]] = &[
    b"Type",
    b"Version",
    b"Extensions",
    b"PageLabels",
    b"Dests",
    b"ViewerPreferences",
    b"PageLayout",
    b"PageMode",
    b"Threads",
    b"URI",
    b"StructTreeRoot",
    b"MarkInfo",
    b"Lang",
    b"SpiderInfo",
    b"OCProperties",
    b"Perms",
    b"Legal",
    b"Requirements",
    b"Collection",
    b"NeedsRendering",
    b"DPartRoot",
    b"DSS",
    // A modification date (defined for pages; some writers put it here).
    b"LastModified",
];

const PAGES_KEYS: &[&[u8]] = &[
    b"Type",
    b"Count",
    b"MediaBox",
    b"CropBox",
    b"Rotate",
    b"ProcSet",
];

const PAGE_KEYS: &[&[u8]] = &[
    b"Type",
    b"LastModified",
    b"MediaBox",
    b"CropBox",
    b"BleedBox",
    b"TrimBox",
    b"ArtBox",
    b"BoxColorInfo",
    b"Rotate",
    b"Group",
    b"B",
    b"Dur",
    b"Trans",
    b"StructParents",
    b"ID",
    b"PZ",
    b"SeparationInfo",
    b"Tabs",
    b"TemplateInstantiated",
    b"PresSteps",
    b"UserUnit",
    b"VP",
    b"DPart",
    // Belongs in resources; a list of standard names wherever it is.
    b"ProcSet",
];

/// Classify every stream of `doc` by what `redact` does with it, and
/// list the data it does not read. See the module documentation.
pub fn uninspected_data(doc: &Document, limits: &Limits) -> Result<DataInventory, Error> {
    limits.validate()?;
    let drawn = crate::redact::drawn_xobjects(doc, limits).ok();
    let mut w = Walk {
        doc,
        seen: HashSet::new(),
        visited: HashSet::new(),
        known: HashSet::new(),
        streams: HashMap::new(),
        direct_unknown: Vec::new(),
        paths: Vec::new(),
        budget: limits.max_audit_elements,
        complete: true,
    };
    // Breadth first: the path recorded is a shortest one.
    let mut queue: VecDeque<(Object, Ctx, usize)> = VecDeque::new();
    if let Some(r) = doc.trailer().get(b"Root".as_slice()) {
        let p = w.child(usize::MAX, "/Root".into());
        queue.push_back((r.clone(), Ctx::Catalog, p));
    }
    if let Some(i) = doc.trailer().get(b"Info".as_slice()) {
        let p = w.child(usize::MAX, "/Info".into());
        queue.push_back((i.clone(), Ctx::Generic, p));
    }
    while let Some((o, ctx, via)) = queue.pop_front() {
        if !w.visit(o, ctx, via, &drawn, &mut queue)? {
            break;
        }
    }

    let mut inv = DataInventory {
        complete: w.complete,
        ..Default::default()
    };
    let mut items: Vec<DataItem> = Vec::new();
    for (r, s) in &w.streams {
        if s.role == DataRole::UnknownEntry && w.known.contains(r) {
            continue;
        }
        let status = s.status();
        match status {
            DataStatus::Searched => inv.searched += 1,
            DataStatus::Interpreted => inv.interpreted += 1,
            _ => {}
        }
        if status == DataStatus::Uninspected {
            inv.uninspected += 1;
            items.push(item(doc, Some(*r), s.role, status, w.render(s.via)));
        }
    }
    for (holder, via) in &w.direct_unknown {
        let via = w.render(*via);
        inv.uninspected += 1;
        items.push(DataItem {
            object: *holder,
            role: DataRole::UnknownEntry,
            status: DataStatus::Uninspected,
            via,
            stream_bytes: None,
            filters: Vec::new(),
        });
    }
    // What nothing refers to; the file's own structure apart.
    if w.complete {
        let live: Vec<IndirectRef> = doc
            .xref_entries()
            .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
            .map(|(r, _)| r)
            .collect();
        for r in live {
            if w.seen.contains(&r) || w.streams.contains_key(&r) {
                continue;
            }
            let o = match doc.resolve(r) {
                Ok(o) => o,
                Err(_) => continue,
            };
            if let Object::Stream { dict, .. } = &o {
                let t = dict.get(b"Type".as_slice()).and_then(|t| t.as_name());
                if matches!(t, Some(b"ObjStm") | Some(b"XRef")) {
                    inv.structural += 1;
                    continue;
                }
            }
            inv.not_carried += 1;
            items.push(item(
                doc,
                Some(r),
                DataRole::Unreachable,
                DataStatus::NotCarried,
                String::new(),
            ));
        }
    }
    items.sort_by_key(|i| (i.object, i.status, i.role, i.via.clone()));
    items.truncate(limits.max_audit_findings as usize);
    inv.items = items;
    Ok(inv)
}

fn item(
    doc: &Document,
    r: Option<IndirectRef>,
    role: DataRole,
    status: DataStatus,
    via: String,
) -> DataItem {
    let (bytes, filters) = match r.map(|r| doc.resolve(r)) {
        Some(Ok(Object::Stream { dict, data })) => {
            let filters = match dict.get(b"Filter".as_slice()) {
                Some(Object::Name(n)) => vec![String::from_utf8_lossy(n).into_owned()],
                Some(Object::Array(a)) => a
                    .iter()
                    .filter_map(|f| f.as_name())
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .collect(),
                _ => Vec::new(),
            };
            (Some(data.len() as u64), filters)
        }
        _ => (None, Vec::new()),
    };
    DataItem {
        object: r,
        role,
        status,
        via,
        stream_bytes: bytes,
        filters,
    }
}

/// What one stream was reached as.
struct StreamUse {
    /// The best status among its roles, and the role it was found
    /// uninspected in.
    best: DataStatus,
    role: DataRole,
    via: usize,
    /// Reached in a role whose bytes a reader hands out as they are
    /// (an attachment, metadata …): uninspected whatever else it is.
    sticky: bool,
}

impl StreamUse {
    fn status(&self) -> DataStatus {
        self.best
    }
}

struct Walk<'d> {
    doc: &'d Document,
    /// Objects reached at all.
    seen: HashSet<IndirectRef>,
    /// `(object, role)` pairs visited.
    visited: HashSet<(IndirectRef, Ctx)>,
    /// Non-stream objects reached in a known role (not listed as
    /// unknown entries).
    known: HashSet<IndirectRef>,
    streams: HashMap<IndirectRef, StreamUse>,
    /// Unknown entries holding a direct value: the holder and the path.
    direct_unknown: Vec<(Option<IndirectRef>, usize)>,
    /// Paths, each a step from its parent (`usize::MAX` for none):
    /// shared, so their size grows with the steps, not their depth.
    paths: Vec<(usize, String)>,
    budget: u64,
    complete: bool,
}

impl Walk<'_> {
    /// Visit `o` reached as `ctx`; `false` when the budget ran out.
    fn visit(
        &mut self,
        o: Object,
        ctx: Ctx,
        via: usize,
        drawn: &Option<HashMap<IndirectRef, std::collections::BTreeSet<usize>>>,
        queue: &mut VecDeque<(Object, Ctx, usize)>,
    ) -> Result<bool, Error> {
        if self.budget == 0 {
            self.complete = false;
            return Ok(false);
        }
        self.budget -= 1;
        let (r, body) = match &o {
            Object::Ref(r) => {
                let first = self.visited.insert((*r, ctx));
                self.seen.insert(*r);
                if !first {
                    return Ok(true);
                }
                match self.doc.resolve(*r) {
                    Ok(b) => (Some(*r), b),
                    Err(Error::MissingReference(_)) => return Ok(true),
                    Err(e) => return Err(e),
                }
            }
            o => (None, o.clone()),
        };
        // A page tree node is a page or a node by its type.
        let ctx = match (ctx, &body) {
            (Ctx::Pages, Object::Dict(d))
                if d.get(b"Type".as_slice()).and_then(|t| t.as_name()) == Some(b"Page") =>
            {
                Ctx::Page
            }
            // An appearance: a form, or a dictionary of states.
            (Ctx::ApState, Object::Stream { .. }) => Ctx::Form,
            _ => ctx,
        };
        if let (Some(r), false) = (r, matches!(body, Object::Stream { .. })) {
            if ctx != Ctx::Unknown && ctx != Ctx::Generic {
                self.known.insert(r);
            }
        }
        if ctx == Ctx::Unknown {
            // Listed as itself (a non-stream unless also reached in a
            // known role); a number, boolean or null holds nothing.
            if is_scalar(&body) {
                return Ok(true);
            }
            if let Some(r) = r {
                self.note_stream(r, DataStatus::Uninspected, DataRole::UnknownEntry, via);
            }
            // Its contents are unknown too, but listed through it.
            self.push_children(&body, Ctx::Generic, via, queue);
            return Ok(true);
        }
        if let (Some(r), Object::Stream { dict, .. }) = (r, &body) {
            let (status, role) = stream_status(ctx, dict, r, drawn);
            self.note_stream(r, status, role, via);
        }
        let d = match &body {
            Object::Dict(d) => Some(d),
            Object::Stream { dict, .. } => Some(dict),
            _ => None,
        };
        match (ctx, &body) {
            (Ctx::Pages, Object::Array(a)) | (Ctx::NameKids(_), Object::Array(a)) => {
                let each = match ctx {
                    Ctx::NameKids(sub) => Ctx::NameTree(sub),
                    c => c,
                };
                for (i, x) in a.iter().enumerate() {
                    self.push(queue, x, each, via, format!("[{i}]"));
                }
            }
            (Ctx::Contents, Object::Array(a)) => {
                for (i, x) in a.iter().enumerate() {
                    self.push(queue, x, Ctx::Contents, via, format!("[{i}]"));
                }
            }
            (Ctx::Annots, Object::Array(a)) => {
                for (i, x) in a.iter().enumerate() {
                    self.push(queue, x, Ctx::Annot, via, format!("[{i}]"));
                }
            }
            (Ctx::ColorSpace, Object::Array(a)) => self.color_space(a, via, queue),
            (Ctx::Function, Object::Array(a))
            | (Ctx::Font, Object::Array(a))
            | (Ctx::FileSpec, Object::Array(a))
            | (Ctx::Action, Object::Array(a))
            | (Ctx::Annot, Object::Array(a))
            | (Ctx::OutputIntents, Object::Array(a))
            | (Ctx::Opaque(_), Object::Array(a))
            | (Ctx::Generic, Object::Array(a)) => {
                for (i, x) in a.iter().enumerate() {
                    self.push(queue, x, ctx, via, format!("[{i}]"));
                }
            }
            (Ctx::NameTree(sub), Object::Array(a)) => {
                // A `/Names` array: keys and values alternate.
                for (i, x) in a.iter().enumerate() {
                    if i % 2 == 1 {
                        self.push(queue, x, sub.ctx(), via, format!("[{i}]"));
                    }
                }
            }
            _ => {
                if let Some(d) = d {
                    self.dict(ctx, d, r, via, queue);
                }
            }
        }
        Ok(true)
    }

    /// A path one step below `parent`.
    fn child(&mut self, parent: usize, step: String) -> usize {
        self.paths.push((parent, step));
        self.paths.len() - 1
    }

    /// A path written out (its last 32 steps).
    fn render(&self, mut at: usize) -> String {
        let mut steps = Vec::new();
        while at != usize::MAX {
            if steps.len() == 32 {
                steps.push("…");
                break;
            }
            let (parent, step) = &self.paths[at];
            steps.push(step.as_str());
            at = *parent;
        }
        steps.reverse();
        steps.concat()
    }

    fn note_stream(&mut self, r: IndirectRef, status: DataStatus, role: DataRole, via: usize) {
        let sticky = status == DataStatus::Uninspected
            && !matches!(
                role,
                DataRole::UnknownStream | DataRole::UnknownEntry | DataRole::Undrawn
            );
        match self.streams.get_mut(&r) {
            Some(u) if u.sticky => {}
            Some(u) if sticky => {
                u.best = status;
                u.role = role;
                u.sticky = true;
            }
            Some(u) => {
                let vaguer = matches!(u.role, DataRole::UnknownStream | DataRole::UnknownEntry);
                if status < u.best || (status == u.best && vaguer) {
                    u.best = status;
                    u.role = role;
                }
            }
            None => {
                self.streams.insert(
                    r,
                    StreamUse {
                        best: status,
                        role,
                        via,
                        sticky,
                    },
                );
            }
        }
    }

    /// Queue `o` as `ctx`, one step below `via` — only what can lead to
    /// a stream (a reference or a container): strings, names and numbers
    /// are not walked (strings are searched as text; unknown entries
    /// holding direct values are listed by `dict`).
    fn push(
        &mut self,
        queue: &mut VecDeque<(Object, Ctx, usize)>,
        o: &Object,
        ctx: Ctx,
        via: usize,
        step: String,
    ) {
        if !matches!(
            o,
            Object::Ref(_) | Object::Dict(_) | Object::Array(_) | Object::Stream { .. }
        ) {
            return;
        }
        let p = self.child(via, step);
        queue.push_back((o.clone(), ctx, p));
    }

    fn push_children(
        &mut self,
        o: &Object,
        ctx: Ctx,
        via: usize,
        queue: &mut VecDeque<(Object, Ctx, usize)>,
    ) {
        match o {
            Object::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    self.push(queue, x, ctx, via, format!("[{i}]"));
                }
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                for (k, v) in d {
                    self.push(queue, v, ctx, via, format!("/{}", name(k)));
                }
            }
            _ => {}
        }
    }

    fn color_space(
        &mut self,
        a: &[Object],
        via: usize,
        queue: &mut VecDeque<(Object, Ctx, usize)>,
    ) {
        let family = a.first().and_then(|f| f.as_name());
        for (i, x) in a.iter().enumerate().skip(1) {
            let c = match (family, i) {
                (Some(b"ICCBased"), 1) => Ctx::Icc,
                (Some(b"Indexed") | Some(b"I"), 1) => Ctx::ColorSpace,
                // The lookup table: a string or a stream of samples.
                (Some(b"Indexed") | Some(b"I"), 3) => Ctx::Function,
                (Some(b"Separation"), 2) | (Some(b"DeviceN"), 2) => Ctx::ColorSpace,
                (Some(b"Separation"), 3) | (Some(b"DeviceN"), 3) => Ctx::Function,
                // DeviceN attributes: /Colorants, /Process /ColorSpace.
                (Some(b"DeviceN"), 4) => Ctx::ColorSpace,
                (Some(b"Pattern"), 1) => Ctx::ColorSpace,
                _ => Ctx::Generic,
            };
            self.push(queue, x, c, via, format!("[{i}]"));
        }
    }

    fn dict(
        &mut self,
        ctx: Ctx,
        d: &Dict,
        holder: Option<IndirectRef>,
        via: usize,
        queue: &mut VecDeque<(Object, Ctx, usize)>,
    ) {
        for (k, v) in d {
            let key = k.as_slice();
            let child = match child_ctx(ctx, d, key) {
                Some(c) => c,
                None => continue,
            };
            if child == Ctx::Unknown && !matches!(v, Object::Ref(_)) && !is_scalar(v) {
                // A direct value under an unknown entry: listed at its
                // holder.
                let path = self.child(via, format!("/{}", name(k)));
                self.direct_unknown.push((holder, path));
                self.push_children(v, Ctx::Generic, path, queue);
                continue;
            }
            self.push(queue, v, child, via, format!("/{}", name(k)));
        }
    }
}

/// A value that carries no data of its own.
fn is_scalar(o: &Object) -> bool {
    matches!(
        o,
        Object::Integer(_) | Object::Real(_) | Object::Bool(_) | Object::Null
    )
}

fn name(k: &[u8]) -> String {
    String::from_utf8_lossy(k).into_owned()
}

/// The role of `d`'s entry `key` when `d` is reached as `ctx` (a back
/// edge — `/Parent`, `/P` — is followed with no role: what only it
/// reaches is still classified, conservatively). Keys naming
/// uninspected data wherever they appear (`/Metadata`, `/PieceInfo`,
/// `/Thumb`, `/AF`) are taken so under any structure.
fn child_ctx(ctx: Ctx, d: &Dict, key: &[u8]) -> Option<Ctx> {
    Some(match child_ctx_in(ctx, d, key)? {
        Ctx::Generic => match key {
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            b"PieceInfo" => Ctx::Opaque(DataRole::PrivateData),
            b"Thumb" => Ctx::Opaque(DataRole::Thumbnail),
            // Associated files (PDF 2.0) hang off any object: structure
            // elements, annotations, forms, marked content.
            b"AF" => Ctx::FileSpec,
            _ => Ctx::Generic,
        },
        c => c,
    })
}

fn child_ctx_in(ctx: Ctx, d: &Dict, key: &[u8]) -> Option<Ctx> {
    let subtype = d.get(b"Subtype".as_slice()).and_then(|s| s.as_name());
    Some(match ctx {
        Ctx::Catalog => match key {
            b"Pages" => Ctx::Pages,
            b"Names" => Ctx::Names,
            b"Outlines" => Ctx::Outline,
            b"OpenAction" => Ctx::Action,
            b"AA" => Ctx::Aa,
            b"AcroForm" => Ctx::AcroForm,
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            b"OutputIntents" => Ctx::OutputIntents,
            b"PieceInfo" => Ctx::Opaque(DataRole::PrivateData),
            b"AF" => Ctx::FileSpec,
            k if CATALOG_KEYS.contains(&k) => Ctx::Generic,
            _ => Ctx::Unknown,
        },
        Ctx::Pages => match key {
            b"Parent" => Ctx::Generic,
            b"Kids" => Ctx::Pages,
            b"Resources" => Ctx::Resources,
            k if PAGES_KEYS.contains(&k) => Ctx::Generic,
            _ => Ctx::Unknown,
        },
        Ctx::Page => match key {
            b"Parent" => Ctx::Generic,
            b"Contents" => Ctx::Contents,
            b"Resources" => Ctx::Resources,
            b"Annots" => Ctx::Annots,
            b"Group" => Ctx::Group,
            b"Thumb" => Ctx::Opaque(DataRole::Thumbnail),
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            b"PieceInfo" => Ctx::Opaque(DataRole::PrivateData),
            b"AA" => Ctx::Aa,
            b"AF" => Ctx::FileSpec,
            b"OutputIntents" => Ctx::OutputIntents,
            k if PAGE_KEYS.contains(&k) => Ctx::Generic,
            _ => Ctx::Unknown,
        },
        Ctx::ResourcesOther => match key {
            b"XObject" => Ctx::Map(Sub::XObjectOther),
            _ => return child_ctx_in(Ctx::Resources, d, key),
        },
        Ctx::XObjectOther => match subtype {
            Some(b"Image") => return child_ctx_in(Ctx::Image, d, key),
            Some(b"Form") => return child_ctx_in(Ctx::FormOther, d, key),
            _ => Ctx::Generic,
        },
        Ctx::FormOther => match key {
            b"Resources" => Ctx::ResourcesOther,
            _ => return child_ctx_in(Ctx::Form, d, key),
        },
        Ctx::Resources => match key {
            b"Font" => Ctx::Map(Sub::Font),
            b"XObject" => Ctx::Map(Sub::XObject),
            b"ExtGState" => Ctx::Map(Sub::ExtGState),
            b"ColorSpace" => Ctx::Map(Sub::ColorSpace),
            b"Pattern" => Ctx::Map(Sub::Pattern),
            b"Shading" => Ctx::Map(Sub::Shading),
            _ => Ctx::Generic,
        },
        Ctx::Map(sub) => sub.ctx(),
        Ctx::Font => match key {
            b"FontDescriptor" => Ctx::FontDescriptor,
            b"ToUnicode" | b"Encoding" | b"CIDToGIDMap" => Ctx::Cmap,
            b"DescendantFonts" => Ctx::Font,
            b"CharProcs" => Ctx::CharProcs,
            b"Resources" => Ctx::ResourcesOther,
            _ => Ctx::Generic,
        },
        Ctx::FontDescriptor => match key {
            b"FontFile" | b"FontFile2" | b"FontFile3" | b"CIDSet" => Ctx::FontProgram,
            b"FD" => Ctx::Map(Sub::Generic),
            _ => Ctx::Generic,
        },
        Ctx::Cmap => match key {
            b"UseCMap" => Ctx::Cmap,
            _ => Ctx::Generic,
        },
        Ctx::CharProcs => Ctx::FontProgram,
        Ctx::XObject => match subtype {
            Some(b"Image") => return child_ctx_in(Ctx::Image, d, key),
            Some(b"Form") => return child_ctx_in(Ctx::Form, d, key),
            _ => Ctx::Generic,
        },
        Ctx::Image => match key {
            b"SMask" | b"Mask" => Ctx::Image,
            b"Alternates" => Ctx::Opaque(DataRole::UnknownStream),
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            b"OPI" => Ctx::Opaque(DataRole::PostScript),
            b"ColorSpace" => Ctx::ColorSpace,
            _ => Ctx::Generic,
        },
        Ctx::Form => match key {
            b"Resources" => Ctx::Resources,
            b"Group" => Ctx::Group,
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            b"PieceInfo" => Ctx::Opaque(DataRole::PrivateData),
            b"OPI" | b"PS" => Ctx::Opaque(DataRole::PostScript),
            b"Ref" => Ctx::FileSpec,
            _ => Ctx::Generic,
        },
        Ctx::Pattern => match key {
            b"Resources" => Ctx::ResourcesOther,
            b"Shading" => Ctx::Shading,
            b"ExtGState" => Ctx::ExtGState,
            _ => Ctx::Generic,
        },
        Ctx::Shading => match key {
            b"Function" => Ctx::Function,
            b"ColorSpace" => Ctx::ColorSpace,
            _ => Ctx::Generic,
        },
        Ctx::Function => match key {
            b"Functions" => Ctx::Function,
            _ => Ctx::Generic,
        },
        Ctx::ExtGState => match key {
            b"SMask" => Ctx::SoftMask,
            b"TR" | b"TR2" | b"BG" | b"BG2" | b"UCR" | b"UCR2" => Ctx::Function,
            b"HT" => Ctx::Opaque(DataRole::Halftone),
            b"Font" => Ctx::Font,
            _ => Ctx::Generic,
        },
        Ctx::SoftMask => match key {
            b"G" => Ctx::FormOther,
            b"TR" => Ctx::Function,
            _ => Ctx::Generic,
        },
        Ctx::ColorSpace => match key {
            // A colour space dictionary (CalRGB, Lab) or DeviceN
            // attributes: colourants are colour spaces.
            b"Colorants" => Ctx::Map(Sub::ColorSpace),
            b"Process" | b"ColorSpace" => Ctx::ColorSpace,
            _ => Ctx::Generic,
        },
        Ctx::Group => match key {
            b"CS" => Ctx::ColorSpace,
            _ => Ctx::Generic,
        },
        Ctx::Icc => match key {
            b"Alternate" => Ctx::ColorSpace,
            b"Metadata" => Ctx::Opaque(DataRole::Metadata),
            _ => Ctx::Generic,
        },
        Ctx::Annot => match key {
            b"P" | b"Parent" => Ctx::Generic,
            b"AP" => Ctx::Appearance,
            b"A" => Ctx::Action,
            b"AA" => Ctx::Aa,
            b"Popup" | b"IRT" | b"Kids" => Ctx::Annot,
            b"FS" => Ctx::FileSpec,
            b"RC" | b"RV" | b"V" | b"DV" => Ctx::Opaque(DataRole::RichText),
            b"Sound" | b"Movie" | b"RichMediaContent" | b"RichMediaSettings" | b"3DD" | b"3DA"
            | b"3DV" => Ctx::Opaque(DataRole::Multimedia),
            b"MK" => Ctx::Map(Sub::Undrawn),
            b"DR" => Ctx::ResourcesOther,
            _ => Ctx::Generic,
        },
        Ctx::Appearance => match key {
            b"N" | b"R" | b"D" => Ctx::ApState,
            _ => Ctx::Generic,
        },
        Ctx::Action => match key {
            b"Next" => Ctx::Action,
            b"JS" => Ctx::Opaque(DataRole::JavaScript),
            b"F" | b"Win" | b"Mac" | b"Unix" => Ctx::FileSpec,
            b"Sound" | b"Movie" | b"R" | b"Rendition" => Ctx::Opaque(DataRole::Multimedia),
            b"AN" | b"Annotation" => Ctx::Annot,
            _ => Ctx::Generic,
        },
        Ctx::Aa => Ctx::Action,
        Ctx::Names => match key {
            b"EmbeddedFiles" => Ctx::NameTree(Sub::FileSpec),
            b"JavaScript" => Ctx::NameTree(Sub::Action),
            b"AP" => Ctx::NameTree(Sub::Undrawn),
            b"Pages" | b"Templates" => Ctx::NameTree(Sub::Template),
            b"AlternatePresentations" | b"Renditions" => Ctx::NameTree(Sub::Multimedia),
            b"Dests" | b"IDS" | b"URLS" => Ctx::NameTree(Sub::Generic),
            _ => Ctx::Unknown,
        },
        Ctx::NameTree(sub) => match key {
            b"Kids" => Ctx::NameKids(sub),
            b"Names" => Ctx::NameTree(sub),
            _ => Ctx::Generic,
        },
        Ctx::FileSpec => match key {
            b"EF" | b"RF" => Ctx::Ef,
            b"Thumb" => Ctx::Opaque(DataRole::Thumbnail),
            _ => Ctx::Generic,
        },
        Ctx::Ef => Ctx::Opaque(DataRole::EmbeddedFile),
        Ctx::AcroForm => match key {
            b"Fields" => Ctx::Annot,
            b"DR" => Ctx::ResourcesOther,
            b"XFA" => Ctx::Opaque(DataRole::Xfa),
            _ => Ctx::Generic,
        },
        Ctx::Outline => match key {
            b"Parent" | b"Prev" => Ctx::Generic,
            b"First" | b"Next" | b"Last" => Ctx::Outline,
            b"A" => Ctx::Action,
            _ => Ctx::Generic,
        },
        Ctx::OutputIntents => match key {
            b"DestOutputProfile" => Ctx::Icc,
            _ => Ctx::Generic,
        },
        Ctx::ApState => Ctx::Form,
        Ctx::Contents | Ctx::FontProgram => Ctx::Generic,
        Ctx::Opaque(role) => Ctx::Opaque(role),
        Ctx::Generic | Ctx::Annots | Ctx::NameKids(_) => Ctx::Generic,
        Ctx::Unknown => Ctx::Generic,
    })
}

/// A stream reached as `ctx`: its status and, when uninspected, its
/// role.
fn stream_status(
    ctx: Ctx,
    dict: &Dict,
    r: IndirectRef,
    drawn: &Option<HashMap<IndirectRef, std::collections::BTreeSet<usize>>>,
) -> (DataStatus, DataRole) {
    let subtype = dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name());
    let shown = drawn.as_ref().is_some_and(|d| d.contains_key(&r));
    let form = |_: IndirectRef| {
        if shown {
            (DataStatus::Searched, DataRole::Undrawn)
        } else {
            (DataStatus::Uninspected, DataRole::Undrawn)
        }
    };
    match ctx {
        Ctx::Contents => (DataStatus::Searched, DataRole::UnknownStream),
        Ctx::XObjectOther | Ctx::FormOther => match subtype {
            Some(b"Image") | Some(b"Form") => (DataStatus::Interpreted, DataRole::UnknownStream),
            Some(b"PS") => (DataStatus::Uninspected, DataRole::PostScript),
            _ => (DataStatus::Uninspected, DataRole::UnknownStream),
        },
        Ctx::XObject => match subtype {
            Some(b"Image") if shown => (DataStatus::Interpreted, DataRole::UnknownStream),
            Some(b"Image") => (DataStatus::Uninspected, DataRole::Undrawn),
            Some(b"Form") => form(r),
            Some(b"PS") => (DataStatus::Uninspected, DataRole::PostScript),
            _ => (DataStatus::Uninspected, DataRole::UnknownStream),
        },
        Ctx::Form | Ctx::ApState => form(r),
        Ctx::Image
        | Ctx::FontProgram
        | Ctx::Cmap
        | Ctx::Function
        | Ctx::Shading
        | Ctx::Pattern
        | Ctx::Icc => (DataStatus::Interpreted, DataRole::UnknownStream),
        Ctx::Opaque(DataRole::Undrawn) => form(r),
        Ctx::Opaque(role) => (DataStatus::Uninspected, role),
        _ => (DataStatus::Uninspected, DataRole::UnknownStream),
    }
}
