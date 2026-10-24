//! Read-only **residual-information audit**.
//!
//! [`audit_residuals`] enumerates places a PDF can still carry extra
//! information — document metadata, attachments, annotations, form
//! values, structure/actual text, actions, optional content, resource
//! objects, superseded revisions and unreachable objects. It is a
//! *structural* inventory: it reports that data exists, where it is,
//! and how it is referenced — it does **not** decide whether that
//! data is sensitive, and a clean report is **not** a "safe" verdict.
//!
//! Nothing here mutates the document or the input bytes. Body bytes
//! (Info strings, field values, JavaScript, attachments, XMP) are
//! never copied into the result — findings carry object references,
//! key paths, stream sizes and kind labels only.
//!
//! ## Reachability classes
//!
//! Every finding is classified by how its object is reachable:
//!
//! - [`ReachClass::Reachable`] — reachable from the audit root set:
//!   trailer `/Root` **and** `/Info` (Info is a residual-information
//!   root in its own right), plus every page object.
//! - [`ReachClass::UnreachableLive`] — present in the merged xref as
//!   an in-use or compressed object and **confirmed** unreachable
//!   after the walk drained its queue (e.g. an orphaned image).
//! - [`ReachClass::PreviousRevision`] — a live entry whose *offset*
//!   lies before the newest xref section; the body at that offset is
//!   the current view — earlier bodies at other offsets are
//!   superseded (see [`HistoryReport`]).
//! - [`ReachClass::Unevaluated`] — the audit did not confirm or
//!   exclude reachability: the walk was cut short, a referrer was
//!   missing, or the region was never scanned. An unvisited object
//!   is **never** reported `UnreachableLive` while the walk is
//!   incomplete.
//!
//! ## Counters
//!
//! - `objects_examined` — objects the audit successfully *resolved*
//!   during the pass-1 walk (a missing or broken object does not
//!   count).
//! - `edges_examined` — reference edges *found* while scanning
//!   resolved objects (the target does not have to resolve).
//! - `reachable` — objects confirmed reachable: the root set plus
//!   every object whose resolution succeeded during the walk.
//! - `unreachable_live` — live objects *confirmed* unreachable
//!   (walk finished). `unevaluated_live` — live objects whose
//!   reachability could not be decided.
//! - `missing_references` — edges whose target is not in the xref.
//!   Each is also reported as a `MissingReference` finding carrying
//!   the referrer + path. Missing refs count against the audit's
//!   confidence (they mark the walk incomplete) but are *not*
//!   findings "about" real objects.
//!
//! ## Limits
//!
//! The audit reuses the document's [`Limits`]: object resolution is
//! charged to `max_reference_depth`, decoded streams to
//! `max_total_decoded_bytes`, page-tree/node bounds were already
//! enforced at load. Audit-specific caps: `max_audit_findings`,
//! `max_audit_edges` (reference edges followed), `max_audit_paths`
//! (referrers recorded per object), `max_audit_walk_depth` (nesting
//! depth inside one resolved object), `max_audit_elements`
//! (container elements expanded across all traversals — name-tree
//! nodes, array items, dict entries), and `max_audit_content_ops`
//! (per-page content ops scanned for `BDC`/`DP`/`BMC` marked
//! content). Exceeding one records an [`AuditIncomplete`] — it never
//! silently truncates into "absent".

use crate::content::Operation;
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::xref::XrefEntry;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One step in a reference path — how a referrer points at an object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathStep {
    /// Through a dictionary key (`/Key`).
    Key(Vec<u8>),
    /// Through an array element (`[i]`).
    Index(u32),
    /// Through a name-tree entry (`/Names` leaf string or `/Kids`).
    /// Carries the decoded name bytes so binary-safe escaping can be
    /// applied at print time.
    NameTreeEntry(Vec<u8>),
    /// Through a page's `/Resources` name (`/XObject /Im1`).
    ResourceName {
        /// Resource category (`XObject`, `Font`, …).
        category: Vec<u8>,
        /// The registered name (`Im1`, `F1`, …).
        name: Vec<u8>,
    },
    /// The trailer root (`/Root`, `/Info`, …) — first step only.
    TrailerKey(Vec<u8>),
}

/// How a finding's object is reachable from the audit roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachClass {
    /// Reachable from `/Root`, `/Info` or a page object.
    Reachable,
    /// In the merged xref (in-use or compressed) and confirmed
    /// unreachable — reported only when the pass-1 walk finished.
    UnreachableLive,
    /// Live entry whose offset precedes the newest xref section —
    /// the current body sits in an earlier revision region. (The
    /// *current* value is what resolves; superseded bodies are
    /// tracked under `HistoryReport`.)
    PreviousRevision,
    /// The audit could not decide reachability: the walk was cut
    /// short (edges/depth/element cap), a referrer was missing, or
    /// the object lives outside the walked graph.
    Unevaluated,
}

/// Broad category of one audit finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    /// Trailer `/Info` dictionary present.
    DocumentInfo,
    /// Catalog or object `/Metadata` (XMP stream).
    Metadata,
    /// Page `/Thumb` thumbnail.
    PageThumbnail,
    /// `/Names /EmbeddedFiles` name tree present.
    EmbeddedFiles,
    /// A `/Filespec` dict or an `/EF` embedded-file stream.
    FileSpec,
    /// Catalog `/AF` associated-files array.
    AssociatedFiles,
    /// A `/Subtype /FileAttachment` annotation.
    FileAttachmentAnnot,
    /// Annotation text-bearing keys (`/Contents`, `/RC`, `/T`, …).
    AnnotText,
    /// Annotation `/AP` appearance stream.
    AnnotAppearance,
    /// Catalog `/AcroForm` present.
    AcroForm,
    /// A form field value/default (`/V`, `/DV`) on a field dict.
    FormFieldValue,
    /// A `/Subtype /Widget` annotation (form widget).
    WidgetAnnot,
    /// `/AcroForm /XFA` present.
    Xfa,
    /// Catalog `/StructTreeRoot` present.
    StructTree,
    /// `/ActualText`, `/Alt`, `/E` on a structure element, marked
    /// content property list, or elsewhere in a checked dict.
    AlternateText,
    /// Catalog `/OpenAction` or `/AA`.
    OpenAction,
    /// An action dictionary (`/S` = `/JavaScript`, `/URI`, `/GoTo`,…).
    Action,
    /// `/Names /JavaScript` name tree or a `/S /JavaScript` action.
    JavaScript,
    /// Catalog `/OCProperties` present.
    OptionalContent,
    /// An `/OCG` or `/OCMD` dictionary.
    Ocg,
    /// An `/XObject /Subtype /Image` stream.
    Image,
    /// A font dict with `/FontFile*` (embedded font program).
    EmbeddedFont,
    /// A `/ToUnicode` CMap stream.
    ToUnicode,
    /// An `/XObject /Subtype /Form` stream.
    FormXObject,
    /// A page (or inherited) `/Resources` dictionary.
    Resources,
    /// A `/Resources` entry not referenced by any `Do`/`Tf`/… in the
    /// audited content — "usage unexamined", *not* "unused".
    ResourceUsageUnexamined,
    /// A free xref entry (object number is free in the latest view).
    FreeObject,
    /// An in-use xref object unreachable from the roots.
    UnreachableObject,
    /// A superseded object body (same number, earlier revision).
    SupersededObject,
    /// A reference edge whose target is absent from the xref —
    /// the referrer and path are recorded; the target is not an
    /// object and is never counted as one.
    MissingReference,
}

/// One detected residual-information item.
#[derive(Debug, Clone)]
pub struct AuditFinding {
    /// What kind of residual data this is.
    pub kind: FindingKind,
    /// The indirect object holding it, when the finding is anchored
    /// to one (a direct dict entry reports `None` and relies on
    /// `via`/`location`).
    pub object: Option<IndirectRef>,
    /// Reachability of `object` from the audit roots.
    pub reach: ReachClass,
    /// Representative path from a root to this finding — a bounded
    /// sample, *not* an exhaustive path list.
    pub via: Vec<PathStep>,
    /// Direct referrers that point at `object` (bounded —
    /// `max_audit_paths`); empty for direct/dict-only findings.
    pub referrers: Vec<PathStep>,
    /// Byte length when the finding is a stream (`Some((encoded,
    /// declared_length))`) or a fixed-size fact (`None`).
    pub bytes: Option<(u64, Option<u64>)>,
    /// Short human-readable detail — keys/subtypes/counts only,
    /// never Info values, form values or stream bodies.
    pub note: String,
}

/// A region or construct the audit could not finish or chose not to
/// evaluate — kept out of `findings` so "not evaluated" is never
/// reported as "absent".
#[derive(Debug, Clone)]
pub struct AuditIncomplete {
    /// What was being audited when it stopped.
    pub scope: String,
    /// Why it stopped (limit name, unsupported construct, parse
    /// failure).
    pub reason: String,
    /// `true` when the audit's own limit stopped the scan; `false`
    /// when the input itself was outside scope.
    pub limit: bool,
}

/// Counts describing the reachability walk.
#[derive(Debug, Clone, Default)]
pub struct ReachabilityReport {
    /// Objects confirmed reachable (roots + resolved during walk).
    pub reachable: u64,
    /// Live xref objects (in-use + compressed).
    pub live_objects: u64,
    /// Reachable live objects.
    pub reachable_live: u64,
    /// Live objects *confirmed* unreachable — only counted when the
    /// walk completed (`walk_complete`).
    pub unreachable_live: u64,
    /// Live objects whose reachability could not be decided —
    /// either the walk was cut short or they were never visited.
    /// Never merged into `unreachable_live`.
    pub unevaluated_live: u64,
    /// Reference edges followed during the walk.
    pub edges_followed: u64,
    /// Edges whose target was not in the xref (missing references).
    /// Each is also reported as a `MissingReference` finding.
    pub missing_references: u64,
    /// `true` when the pass-1 walk drained its queue — only then is
    /// `unreachable_live` a complete count. `false` means the walk
    /// stopped early (edges cap, element cap, or a depth stop on any
    /// branch) and unvisited objects are `unevaluated_live`, *not*
    /// unreachable.
    pub walk_complete: bool,
}

/// What the audit learned about saved revisions / physical bytes.
#[derive(Debug, Clone, Default)]
pub struct HistoryReport {
    /// Number of xref sections read (1 = no incremental history).
    pub xref_sections: u64,
    /// Number of free (`f`) entries in the merged xref.
    pub free_entries: u64,
    /// Number of type-2 (compressed) entries.
    pub compressed_entries: u64,
    /// Live objects whose offset lies before the newest xref section
    /// — they were written by an earlier revision but remain current.
    pub objects_in_old_region: u64,
    /// Whether physical regions outside every xref-covered object
    /// were examined (`false` — the audit does not byte-scan slack).
    pub physical_slack_examined: bool,
    /// `Some(n)` when the file carries incremental history (more than
    /// one section); `None` when it does not. "No history" here means
    /// *one xref section and no `/Prev`* — it says nothing about
    /// physical bytes the xref does not reference.
    pub incremental_history: Option<u64>,
}

/// The full audit result.
#[derive(Debug, Default)]
pub struct ResidualAudit {
    /// Detected items, in a deterministic order (kind, then object).
    pub findings: Vec<AuditFinding>,
    /// Reachability counts.
    pub reachability: ReachabilityReport,
    /// Revision / physical-data summary.
    pub history: HistoryReport,
    /// Scopes that could not be evaluated at all.
    pub unexamined: Vec<String>,
    /// Scopes that started but stopped early (limit or error).
    pub incomplete: Vec<AuditIncomplete>,
    /// Distinct objects resolved during the audit.
    pub objects_examined: u64,
    /// Reference edges examined during the reachability walk.
    pub edges_examined: u64,
}

// ---------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------

/// Audit `doc` for residual information. `limits` caps audit-specific
/// effort; the document's own `Limits` still govern object
/// resolution and stream decoding. Never fails on content it cannot
/// inspect — that becomes `incomplete`/`unexamined`. Fails only when
/// the audit itself cannot run (e.g. the catalog is unreadable).
pub fn audit_residuals(doc: &Document, limits: &Limits) -> Result<ResidualAudit, Error> {
    limits.validate()?;
    let mut a = Audit {
        doc,
        limits,
        out: ResidualAudit::default(),
        edges: 0,
        elements: 0,
        referrers: HashMap::new(),
        reachable: HashSet::new(),
        walk_complete: false,
        findings_seen: 0,
        findings_capped: false,
        #[cfg(test)]
        max_walk_stack: 0,
    };

    // ---- pass 1: reachability walk --------------------------------
    a.reachability_walk()?;

    // ---- pass 2: structure-keyed findings --------------------------
    a.audit_trailer_and_catalog();
    a.audit_pages();
    a.audit_annotations_and_form();
    a.audit_structure_tree();
    a.audit_name_trees();
    a.audit_oc_properties();
    a.audit_resources();
    a.audit_stream_kinds();

    // ---- pass 3: xref/history/physical -----------------------------
    a.audit_xref_and_history();

    // deterministic ordering: by kind, then object, then note
    a.out.findings.sort_by(|x, y| {
        x.kind
            .cmp(&y.kind)
            .then(x.object.cmp(&y.object))
            .then(x.note.cmp(&y.note))
    });

    Ok(a.out)
}

// ---------------------------------------------------------------------
// Audit state
// ---------------------------------------------------------------------

struct Audit<'d> {
    doc: &'d Document,
    limits: &'d Limits,
    out: ResidualAudit,
    /// Reference edges followed during the reachability walk.
    edges: u64,
    /// Container elements expanded across all traversals
    /// (`max_audit_elements` budget).
    elements: u64,
    /// `object -> its direct referrers` (bounded per object).
    referrers: HashMap<IndirectRef, Vec<PathStep>>,
    /// Objects confirmed reachable — roots plus every object that
    /// resolved successfully during the walk.
    reachable: HashSet<IndirectRef>,
    /// `true` when the pass-1 BFS drained its queue without hitting
    /// `max_audit_edges`. `false` means unvisited live objects are
    /// `Unevaluated`, never `UnreachableLive`.
    walk_complete: bool,
    /// Number of findings emitted (cap: `max_audit_findings`).
    findings_seen: u64,
    /// `true` once `max_audit_findings` dropped a finding. Kept
    /// separate from `walk_complete` — output truncation is not a
    /// reachability statement.
    findings_capped: bool,
    /// Test-only instrumentation: the deepest the per-object
    /// container frame stack ever grew during `walk_object_refs`.
    /// Lets tests prove the stack tracks nesting depth, not
    /// breadth, without duplicating the production walker.
    #[cfg(test)]
    max_walk_stack: usize,
}

impl<'d> Audit<'d> {
    fn add_finding(&mut self, mut f: AuditFinding) {
        // Fill `referrers` from the reachability map when the finding
        // is anchored to an indirect object — the caller may leave it
        // empty and let the walk's records populate it.
        if f.referrers.is_empty() {
            if let Some(r) = f.object {
                if let Some(rs) = self.referrers.get(&r) {
                    f.referrers = rs.clone();
                }
            }
        }
        if self.findings_seen >= self.limits.max_audit_findings {
            if !self.findings_capped {
                self.out.incomplete.push(AuditIncomplete {
                    scope: "findings".into(),
                    reason: format!(
                        "max_audit_findings ({}) reached",
                        self.limits.max_audit_findings
                    ),
                    limit: true,
                });
                self.findings_capped = true;
            }
            return;
        }
        self.findings_seen += 1;
        self.out.findings.push(f);
    }

    /// Consume one unit of the shared `max_audit_elements` budget.
    /// Returns `false` once the budget is exhausted — the caller
    /// stops the current traversal and records `incomplete`.
    fn consume_element(&mut self, scope: &str) -> bool {
        self.elements += 1;
        if self.elements > self.limits.max_audit_elements {
            self.out.incomplete.push(AuditIncomplete {
                scope: scope.into(),
                reason: format!(
                    "max_audit_elements ({}) reached",
                    self.limits.max_audit_elements
                ),
                limit: true,
            });
            return false;
        }
        true
    }

    /// Classify `r`'s reachability for a finding.
    fn reach_of(&self, r: IndirectRef) -> ReachClass {
        if self.reachable.contains(&r) {
            return ReachClass::Reachable;
        }
        match self.doc.xref_entry_kind(r) {
            Some(XrefEntry::InUse { offset }) => {
                // Is the body in a superseded region? The newest
                // section offset is the smallest offset that is part
                // of the *current* revision's structural data; an
                // object written before it belongs to an older
                // revision region.
                let newest = self
                    .doc
                    .xref_section_offsets()
                    .first()
                    .copied()
                    .unwrap_or(0);
                if offset < newest && self.doc.xref_section_offsets().len() > 1 {
                    ReachClass::PreviousRevision
                } else if self.walk_complete {
                    ReachClass::UnreachableLive
                } else {
                    ReachClass::Unevaluated
                }
            }
            Some(XrefEntry::Compressed { .. }) => {
                if self.walk_complete {
                    ReachClass::UnreachableLive
                } else {
                    ReachClass::Unevaluated
                }
            }
            _ => {
                if self.walk_complete {
                    ReachClass::UnreachableLive
                } else {
                    ReachClass::Unevaluated
                }
            }
        }
    }
}

/// Render a `PathStep` list as a compact "obj/key[i]/name" string —
/// used to record a referrer's access path without a nested
/// structure.
fn render_path(p: &[PathStep]) -> String {
    let mut s = String::new();
    for step in p {
        match step {
            PathStep::TrailerKey(k) | PathStep::Key(k) => {
                s.push('/');
                s.push_str(&String::from_utf8_lossy(k));
            }
            PathStep::Index(i) => {
                s.push_str(&format!("[{i}]"));
            }
            PathStep::NameTreeEntry(n) => {
                s.push_str(&format!(":{}", String::from_utf8_lossy(n)));
            }
            PathStep::ResourceName { category, name } => {
                s.push('/');
                s.push_str(&String::from_utf8_lossy(category));
                s.push('/');
                s.push_str(&String::from_utf8_lossy(name));
            }
        }
    }
    s
}

// ---------------------------------------------------------------------
// pass 1 — reachability walk
// ---------------------------------------------------------------------

impl<'d> Audit<'d> {
    /// BFS over indirect refs from the audit roots: trailer `/Root`,
    /// trailer `/Info` (a residual-data root itself), and every page
    /// object (pages are reachable through `/Root` already, but
    /// listing them makes the root set explicit and robust to a
    /// catalog that loses `/Pages`). `walk_complete` is set only
    /// when the queue drains without an edges-cap stop — a
    /// depth-stopped branch still leaves the walk incomplete.
    fn reachability_walk(&mut self) -> Result<(), Error> {
        // Root set: trailer keys + page objects. A root ref that
        // does not resolve is a missing root, not a reachable one.
        let mut queue: Vec<IndirectRef> = Vec::new();
        let mut visited: HashSet<IndirectRef> = HashSet::new();
        for key in [b"Root".as_slice(), b"Info".as_slice()] {
            if let Some(Object::Ref(r)) = self.doc.trailer().get(key) {
                queue.push(*r);
            }
        }
        for p in self.doc.pages() {
            if let Some(r) = p.object_ref() {
                queue.push(r);
            }
        }

        while let Some(r) = queue.pop() {
            if !visited.insert(r) {
                continue;
            }
            let obj = match self.doc.resolve_defined(r) {
                Ok(o) => o,
                Err(Error::MissingReference(_)) => {
                    // A queued ref that turned out to be absent is a
                    // missing reference — it was never a real object,
                    // so it is neither examined nor reachable. The
                    // walk stays complete: the queue still drains.
                    self.out.reachability.missing_references += 1;
                    // Report it with the root that pointed at it —
                    // the ref may have come from a trailer key or a
                    // queued edge, both recorded via `referrers`.
                    let mut via = vec![];
                    if let Some(rs) = self.referrers.get(&r) {
                        via = rs.clone();
                    }
                    self.add_finding(AuditFinding {
                        kind: FindingKind::MissingReference,
                        object: Some(r),
                        reach: ReachClass::Unevaluated,
                        via,
                        referrers: vec![],
                        bytes: None,
                        note: format!(
                            "reference to {r} has no xref entry \
                             (dangling reference)"
                        ),
                    });
                    continue;
                }
                Err(e) => return Err(e),
            };
            // Only now is the object confirmed real: count it as
            // examined *and* reachable (it was reached via a root or
            // a queued edge).
            self.out.objects_examined += 1;
            self.reachable.insert(r);
            let finished = self.walk_object_refs(r, &obj, &mut queue)?;
            if !finished {
                // Edges cap — stop the whole walk; unvisited objects
                // stay Unevaluated.
                break;
            }
        }
        // `walk_complete` is true only when every queued ref was
        // popped *and* every object scan finished — a depth stop or
        // an element stop leaves the walk incomplete.
        self.walk_complete = queue.is_empty()
            && self.edges <= self.limits.max_audit_edges
            && !self
                .out
                .incomplete
                .iter()
                .any(|i| i.scope.contains("reachability walk"))
            && self.elements <= self.limits.max_audit_elements;
        let rep = &mut self.out.reachability;
        rep.edges_followed = self.edges;
        rep.walk_complete = self.walk_complete;
        Ok(())
    }

    /// Follow every `Ref` inside `obj`, recording referrers and
    /// queueing unvisited targets. Returns `false` when the walk
    /// must stop entirely (`max_audit_edges`); `true` otherwise —
    /// including when a single branch exceeded `max_audit_walk_depth`
    /// (that branch is dropped, siblings continue).
    ///
    /// The traversal is iterative — each frame holds a container
    /// iterator plus the position it has reached, so a budget stop
    /// costs the frames on the stack (≈ nesting depth), never the
    /// breadth of any container. A path (`Vec<PathStep>`) is cloned
    /// only when a *ref* is found, not for every element visited.
    fn walk_object_refs(
        &mut self,
        from: IndirectRef,
        obj: &Object,
        queue: &mut Vec<IndirectRef>,
    ) -> Result<bool, Error> {
        // One frame per open container: the container's iterator plus
        // the path that led to it. Stack height ≈ nesting depth —
        // a wide array is one frame that yields its elements lazily;
        // a path is *extended* only when the yielded element turns
        // out to be a container we descend into.
        enum Frame<'a> {
            /// The top-level object — path is empty.
            Leaf(&'a Object),
            /// Iterating an array — yields (`item`, `index`).
            Array {
                it: std::slice::Iter<'a, Object>,
                idx: u32,
                /// Path to this array (no trailing index — added per
                /// yielded element).
                path: Vec<PathStep>,
            },
            /// Iterating a dict (or a stream's dict) — yields
            /// (`value`, `key`).
            Dict {
                it: std::collections::btree_map::Iter<'a, Vec<u8>, Object>,
                /// Path to this dict.
                path: Vec<PathStep>,
            },
        }
        // Stack entries are (frame, depth-of-children). Depth is the
        // same for every element a frame yields, so it is kept on
        // the stack, not cloned into each path.
        let mut stack: Vec<(Frame, u32)> = vec![(Frame::Leaf(obj), 0)];
        // Test-only: track the stack's high-water mark so tests can
        // assert it stays proportional to depth, not breadth.
        #[cfg(test)]
        macro_rules! push_frame {
            ($f:expr) => {{
                stack.push($f);
                self.max_walk_stack = self.max_walk_stack.max(stack.len());
            }};
        }
        #[cfg(not(test))]
        macro_rules! push_frame {
            ($f:expr) => {
                stack.push($f)
            };
        }
        let mut depth_reported = false;
        while let Some((mut frame, depth)) = stack.pop() {
            // Advance the top frame: yield its next element. A Leaf
            // yields once and is *not* pushed back; container frames
            // push themselves back so the next element is visited.
            let (o, step, keep) = match &mut frame {
                Frame::Leaf(o) => (*o, None, false),
                Frame::Array { it, idx, .. } => match it.next() {
                    Some(o) => {
                        let s = PathStep::Index(*idx);
                        *idx += 1;
                        (o, Some(s), true)
                    }
                    None => continue,
                },
                Frame::Dict { it, .. } => match it.next() {
                    Some((k, o)) => (o, Some(PathStep::Key(k.clone())), true),
                    None => continue,
                },
            };
            if !self.consume_element("reachability walk") {
                // Element budget exhausted — stop the whole walk;
                // everything still queued is Unevaluated.
                return Ok(false);
            }
            match o {
                Object::Ref(to) => {
                    self.edges += 1;
                    self.out.edges_examined = self.edges;
                    if self.edges > self.limits.max_audit_edges {
                        self.out.incomplete.push(AuditIncomplete {
                            scope: "reachability walk".into(),
                            reason: format!(
                                "max_audit_edges ({}) reached",
                                self.limits.max_audit_edges
                            ),
                            limit: true,
                        });
                        return Ok(false); // hard stop
                    }
                    // The path to *this* element = frame path + yielded
                    // step — built only for refs/containers, not for
                    // the leaves that dominate a wide container.
                    let frame_path = match &frame {
                        Frame::Leaf(_) => Vec::new(),
                        Frame::Array { path, .. } | Frame::Dict { path, .. } => {
                            let mut p = path.clone();
                            if let Some(s) = step.clone() {
                                p.push(s);
                            }
                            p
                        }
                    };
                    let rs = self.referrers.entry(*to).or_default();
                    if rs.len() < self.limits.max_audit_paths as usize {
                        let mut p = vec![PathStep::Key(format!("{from}").into_bytes())];
                        p.extend(frame_path.clone());
                        rs.push(PathStep::Key(render_path(&p).into_bytes()));
                    }
                    // A ref whose target is not in the merged xref at
                    // all is missing — report it with its referrer
                    // and path, and do not queue it.
                    if self.doc.xref_entry_kind(*to).is_none() {
                        self.out.reachability.missing_references += 1;
                        let mut via = vec![PathStep::Key(format!("{from}").into_bytes())];
                        via.extend(frame_path);
                        self.add_finding(AuditFinding {
                            kind: FindingKind::MissingReference,
                            object: Some(*to),
                            reach: ReachClass::Unevaluated,
                            via,
                            referrers: vec![],
                            bytes: None,
                            note: format!(
                                "reference to {to} has no xref entry \
                                 (dangling reference)"
                            ),
                        });
                        if keep {
                            push_frame!((frame, depth));
                        }
                        continue;
                    }
                    queue.push(*to);
                    if keep {
                        push_frame!((frame, depth));
                    }
                    continue;
                }
                Object::Array(items) => {
                    if depth + 1 > self.limits.max_audit_walk_depth as u32 {
                        if !depth_reported {
                            self.out.incomplete.push(AuditIncomplete {
                                scope: format!("reachability walk from {from}"),
                                reason: format!(
                                    "max_audit_walk_depth ({}) reached",
                                    self.limits.max_audit_walk_depth
                                ),
                                limit: true,
                            });
                            depth_reported = true;
                        }
                        // Drop this branch only — siblings continue.
                        if keep {
                            push_frame!((frame, depth));
                        }
                        continue;
                    }
                    // Descend: build the child's path from the frame
                    // path + the yielded index step.
                    let child_path = match frame {
                        Frame::Leaf(_) => Vec::new(),
                        Frame::Array { ref path, .. } | Frame::Dict { ref path, .. } => {
                            let mut p = path.clone();
                            if let Some(s) = step.clone() {
                                p.push(s);
                            }
                            p
                        }
                    };
                    push_frame!((
                        Frame::Array {
                            it: items.iter(),
                            idx: 0,
                            path: child_path,
                        },
                        depth + 1,
                    ));
                    if keep {
                        push_frame!((frame, depth));
                    }
                    continue;
                }
                Object::Dict(d) => {
                    if depth + 1 > self.limits.max_audit_walk_depth as u32 {
                        if !depth_reported {
                            self.out.incomplete.push(AuditIncomplete {
                                scope: format!("reachability walk from {from}"),
                                reason: format!(
                                    "max_audit_walk_depth ({}) reached",
                                    self.limits.max_audit_walk_depth
                                ),
                                limit: true,
                            });
                            depth_reported = true;
                        }
                        if keep {
                            push_frame!((frame, depth));
                        }
                        continue;
                    }
                    let child_path = match frame {
                        Frame::Leaf(_) => Vec::new(),
                        Frame::Array { ref path, .. } | Frame::Dict { ref path, .. } => {
                            let mut p = path.clone();
                            if let Some(s) = step.clone() {
                                p.push(s);
                            }
                            p
                        }
                    };
                    push_frame!((
                        Frame::Dict {
                            it: d.iter(),
                            path: child_path,
                        },
                        depth + 1,
                    ));
                    if keep {
                        push_frame!((frame, depth));
                    }
                    continue;
                }
                Object::Stream { dict, .. } => {
                    if depth + 1 > self.limits.max_audit_walk_depth as u32 {
                        if !depth_reported {
                            self.out.incomplete.push(AuditIncomplete {
                                scope: format!("reachability walk from {from}"),
                                reason: format!(
                                    "max_audit_walk_depth ({}) reached",
                                    self.limits.max_audit_walk_depth
                                ),
                                limit: true,
                            });
                            depth_reported = true;
                        }
                        if keep {
                            push_frame!((frame, depth));
                        }
                        continue;
                    }
                    let child_path = match frame {
                        Frame::Leaf(_) => Vec::new(),
                        Frame::Array { ref path, .. } | Frame::Dict { ref path, .. } => {
                            let mut p = path.clone();
                            if let Some(s) = step.clone() {
                                p.push(s);
                            }
                            p
                        }
                    };
                    push_frame!((
                        Frame::Dict {
                            it: dict.iter(),
                            path: child_path,
                        },
                        depth + 1,
                    ));
                    if keep {
                        push_frame!((frame, depth));
                    }
                    continue;
                }
                _ => {}
            }
            // Non-container leaf — a container frame may have more
            // elements; a Leaf frame is done. `keep` is false for
            // Leaf, true for exhausted-or-live containers.
            if keep {
                push_frame!((frame, depth));
            }
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------
// pass 2 — structure-keyed findings
// ---------------------------------------------------------------------

impl<'d> Audit<'d> {
    /// trailer `/Info` — the document-information dictionary. Any
    /// `/Info` at all is reported; its string *values* are not read.
    /// The merged trailer can carry a stale `/Info` from an older
    /// section — compare `latest_trailer` to distinguish.
    fn audit_trailer_and_catalog(&mut self) {
        // /Info presence + key names only. A merged-trailer /Info that
        // is absent from the *latest* trailer was inherited from an
        // older revision — still reported, but marked PreviousRevision
        // rather than silently treated as current.
        if let Some(info_obj) = self.doc.trailer().get(b"Info".as_slice()) {
            let (obj, reach) = match info_obj {
                Object::Ref(r) => (Some(*r), self.reach_of(*r)),
                _ => (None, ReachClass::Reachable),
            };
            let in_latest = self.doc.latest_trailer().contains_key(b"Info".as_slice());
            let keys: Vec<String> = match self.doc.deref_defined(info_obj) {
                Ok(o) => o
                    .as_dict()
                    .map(|d| {
                        d.keys()
                            .map(|k| format!("/{}", String::from_utf8_lossy(k)))
                            .collect()
                    })
                    .unwrap_or_default(),
                Err(_) => vec!["<unreadable>".into()],
            };
            let note = if in_latest {
                format!("keys: {}", keys.join(" "))
            } else {
                format!(
                    "keys: {} (via merged trailer — the newest trailer has no /Info)",
                    keys.join(" ")
                )
            };
            self.add_finding(AuditFinding {
                kind: FindingKind::DocumentInfo,
                object: obj,
                reach: if in_latest {
                    reach
                } else {
                    ReachClass::PreviousRevision
                },
                via: vec![PathStep::TrailerKey(b"Info".to_vec())],
                referrers: vec![],
                bytes: None,
                note,
            });
        }
        // Catalog keys that carry residual information.
        let cat = self.doc.catalog().clone();
        self.audit_catalog_keys(&cat);
    }

    fn audit_catalog_keys(&mut self, cat: &Dict) {
        // /Metadata (XMP stream on the catalog).
        if let Some(m) = cat.get(b"Metadata".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::Metadata,
                object: m.as_ref(),
                reach: m
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::TrailerKey(b"Root".to_vec()),
                    PathStep::Key(b"Metadata".to_vec()),
                ],
                referrers: vec![],
                bytes: self.stream_bytes(m),
                note: "catalog /Metadata (XMP)".into(),
            });
        }
        // /OpenAction, /AA.
        for key in [b"OpenAction".as_slice(), b"AA".as_slice()] {
            if let Some(o) = cat.get(key) {
                self.add_finding(AuditFinding {
                    kind: FindingKind::OpenAction,
                    object: o.as_ref(),
                    reach: o
                        .as_ref()
                        .map(|r| self.reach_of(r))
                        .unwrap_or(ReachClass::Reachable),
                    via: vec![
                        PathStep::TrailerKey(b"Root".to_vec()),
                        PathStep::Key(key.to_vec()),
                    ],
                    referrers: vec![],
                    bytes: None,
                    note: format!("catalog /{}", String::from_utf8_lossy(key)),
                });
            }
        }
        // /AcroForm.
        if let Some(o) = cat.get(b"AcroForm".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::AcroForm,
                object: o.as_ref(),
                reach: o
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::TrailerKey(b"Root".to_vec()),
                    PathStep::Key(b"AcroForm".to_vec()),
                ],
                referrers: vec![],
                bytes: None,
                note: "catalog /AcroForm".into(),
            });
            // /XFA inside it.
            if let Ok(af) = self.doc.deref_defined(o) {
                if let Some(d) = af.as_dict() {
                    if d.contains_key(b"XFA".as_slice()) {
                        self.add_finding(AuditFinding {
                            kind: FindingKind::Xfa,
                            object: o.as_ref(),
                            reach: ReachClass::Reachable,
                            via: vec![
                                PathStep::TrailerKey(b"Root".to_vec()),
                                PathStep::Key(b"AcroForm".to_vec()),
                                PathStep::Key(b"XFA".to_vec()),
                            ],
                            referrers: vec![],
                            bytes: None,
                            note: "AcroForm /XFA".into(),
                        });
                    }
                }
            }
        }
        // /StructTreeRoot.
        if let Some(o) = cat.get(b"StructTreeRoot".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::StructTree,
                object: o.as_ref(),
                reach: o
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::TrailerKey(b"Root".to_vec()),
                    PathStep::Key(b"StructTreeRoot".to_vec()),
                ],
                referrers: vec![],
                bytes: None,
                note: "catalog /StructTreeRoot".into(),
            });
        }
        // /OCProperties.
        if let Some(o) = cat.get(b"OCProperties".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::OptionalContent,
                object: o.as_ref(),
                reach: o
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::TrailerKey(b"Root".to_vec()),
                    PathStep::Key(b"OCProperties".to_vec()),
                ],
                referrers: vec![],
                bytes: None,
                note: "catalog /OCProperties".into(),
            });
        }
        // /AF (associated files).
        if let Some(o) = cat.get(b"AF".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::AssociatedFiles,
                object: o.as_ref(),
                reach: o
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::TrailerKey(b"Root".to_vec()),
                    PathStep::Key(b"AF".to_vec()),
                ],
                referrers: vec![],
                bytes: None,
                note: "catalog /AF".into(),
            });
        }
    }

    fn audit_pages(&mut self) {
        let pages: Vec<(usize, Option<IndirectRef>, Option<Object>)> = self
            .doc
            .pages()
            .iter()
            .map(|p| (p.index(), p.object_ref(), p.resources().cloned()))
            .collect();
        for (index, pref, resources) in pages {
            // Page dict itself — Annots/Thumb live here.
            if let Some(pr) = pref {
                if let Ok(pd) = self.doc.resolve_defined(pr).and_then(|o| {
                    o.as_dict()
                        .cloned()
                        .ok_or_else(|| Error::Structure("page is not a dict".into()))
                }) {
                    self.audit_page_dict(index + 1, pr, &pd);
                }
            }
            // /Resources (effective) presence.
            if let Some(res) = resources {
                let (obj, reach) = match &res {
                    Object::Ref(r) => (Some(*r), self.reach_of(*r)),
                    _ => (None, ReachClass::Reachable),
                };
                self.add_finding(AuditFinding {
                    kind: FindingKind::Resources,
                    object: obj,
                    reach,
                    via: vec![
                        PathStep::Key(b"Page".to_vec()),
                        PathStep::Index(index as u32),
                        PathStep::Key(b"Resources".to_vec()),
                    ],
                    referrers: vec![],
                    bytes: None,
                    note: format!("page {} effective /Resources", index + 1),
                });
            }
        }
    }

    fn audit_page_dict(&mut self, page_no: usize, pref: IndirectRef, pd: &Dict) {
        if let Some(t) = pd.get(b"Thumb".as_slice()) {
            self.add_finding(AuditFinding {
                kind: FindingKind::PageThumbnail,
                object: t.as_ref(),
                reach: t
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: vec![
                    PathStep::Key(b"Page".to_vec()),
                    PathStep::Index(page_no as u32 - 1),
                    PathStep::Key(b"Thumb".to_vec()),
                ],
                referrers: vec![],
                bytes: self.stream_bytes(t),
                note: format!("page {page_no} /Thumb"),
            });
        }
        // Annots are audited in `audit_annotations_and_form`.
        let _ = (pref, pd.get(b"Annots".as_slice()));
    }

    /// Walk every page's `/Annots`: text-bearing keys, `/AP`,
    /// `/FileAttachment`, `/Widget` (also `/Subtype /Widget`).
    fn audit_annotations_and_form(&mut self) {
        for p in self.doc.pages() {
            let Some(pr) = p.object_ref() else { continue };
            let Ok(pd) = self.doc.resolve_defined(pr).and_then(|o| {
                o.as_dict()
                    .cloned()
                    .ok_or_else(|| Error::Structure("page".into()))
            }) else {
                continue;
            };
            let Some(annots) = pd.get(b"Annots".as_slice()) else {
                continue;
            };
            let Ok(resolved) = self.doc.deref_defined(annots) else {
                continue;
            };
            let items = match resolved.as_array() {
                Some(a) => a,
                None => continue,
            };
            for (i, entry) in items.iter().enumerate() {
                let (aobj, aref) = match entry {
                    Object::Ref(r) => match self.doc.resolve_defined(*r) {
                        Ok(o) => (o, Some(*r)),
                        Err(_) => continue,
                    },
                    o => (o.clone(), None),
                };
                let Some(ad) = aobj.as_dict() else { continue };
                let subtype = ad
                    .get(b"Subtype".as_slice())
                    .and_then(|o| o.as_name())
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_else(|| "?".into());
                let base_via = vec![
                    PathStep::Key(b"Page".to_vec()),
                    PathStep::Index(p.index() as u32),
                    PathStep::Key(b"Annots".to_vec()),
                    PathStep::Index(i as u32),
                ];
                let reach = aref
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable);
                // Text-bearing keys — presence only, values not read.
                for key in [
                    b"Contents".as_slice(),
                    b"RC".as_slice(),
                    b"T".as_slice(),
                    b"Subj".as_slice(),
                    b"Title".as_slice(),
                ] {
                    if ad.contains_key(key) {
                        self.add_finding(AuditFinding {
                            kind: FindingKind::AnnotText,
                            object: aref,
                            reach,
                            via: {
                                let mut v = base_via.clone();
                                v.push(PathStep::Key(key.to_vec()));
                                v
                            },
                            referrers: vec![],
                            bytes: None,
                            note: format!(
                                "/{} annot /{} present",
                                subtype,
                                String::from_utf8_lossy(key)
                            ),
                        });
                    }
                }
                if ad.contains_key(b"AP".as_slice()) {
                    self.add_finding(AuditFinding {
                        kind: FindingKind::AnnotAppearance,
                        object: aref,
                        reach,
                        via: {
                            let mut v = base_via.clone();
                            v.push(PathStep::Key(b"AP".to_vec()));
                            v
                        },
                        referrers: vec![],
                        bytes: None,
                        note: format!("/{subtype} annot /AP"),
                    });
                }
                if ad.contains_key(b"A".as_slice()) || ad.contains_key(b"AA".as_slice()) {
                    self.add_finding(AuditFinding {
                        kind: FindingKind::Action,
                        object: aref,
                        reach,
                        via: base_via.clone(),
                        referrers: vec![],
                        bytes: None,
                        note: format!("/{subtype} annot /A or /AA"),
                    });
                }
                match subtype.as_str() {
                    "FileAttachment" => self.add_finding(AuditFinding {
                        kind: FindingKind::FileAttachmentAnnot,
                        object: aref,
                        reach,
                        via: base_via.clone(),
                        referrers: vec![],
                        bytes: None,
                        note: "/FileAttachment annot".into(),
                    }),
                    "Widget" => {
                        self.add_finding(AuditFinding {
                            kind: FindingKind::WidgetAnnot,
                            object: aref,
                            reach,
                            via: base_via.clone(),
                            referrers: vec![],
                            bytes: None,
                            note: "/Widget annot (form field)".into(),
                        });
                        // Field value keys on the widget (merged field).
                        for key in [b"V".as_slice(), b"DV".as_slice()] {
                            if ad.contains_key(key) {
                                self.add_finding(AuditFinding {
                                    kind: FindingKind::FormFieldValue,
                                    object: aref,
                                    reach,
                                    via: {
                                        let mut v = base_via.clone();
                                        v.push(PathStep::Key(key.to_vec()));
                                        v
                                    },
                                    referrers: vec![],
                                    bytes: None,
                                    note: format!(
                                        "widget /{} present",
                                        String::from_utf8_lossy(key)
                                    ),
                                });
                            }
                        }
                    }
                    _ => {}
                }
                // A FileAttachment's /FS FileSpec is a Filespec finding.
                if let Some(fs) = ad.get(b"FS".as_slice()) {
                    self.add_finding(AuditFinding {
                        kind: FindingKind::FileSpec,
                        object: fs.as_ref(),
                        reach: fs
                            .as_ref()
                            .map(|r| self.reach_of(r))
                            .unwrap_or(ReachClass::Reachable),
                        via: {
                            let mut v = base_via.clone();
                            v.push(PathStep::Key(b"FS".to_vec()));
                            v
                        },
                        referrers: vec![],
                        bytes: self.stream_bytes(fs),
                        note: "annotation /FS (file specification)".into(),
                    });
                }
            }
        }
    }

    /// StructTreeRoot: walk `/K` elements looking for `/ActualText`,
    /// `/Alt`, `/E` — bounded by `max_audit_elements`.
    fn audit_structure_tree(&mut self) {
        let Some(root) = self.doc.catalog().get(b"StructTreeRoot".as_slice()) else {
            return;
        };
        let root_obj = match self.doc.deref_defined(root) {
            Ok(o) => o,
            Err(e) => {
                self.out.incomplete.push(AuditIncomplete {
                    scope: "structure tree".into(),
                    reason: format!("StructTreeRoot does not resolve: {}", e.kind()),
                    limit: matches!(e, Error::LimitExceeded { .. }),
                });
                return;
            }
        };
        let mut stack: Vec<(Object, Vec<PathStep>)> = vec![(
            root_obj,
            vec![
                PathStep::TrailerKey(b"Root".to_vec()),
                PathStep::Key(b"StructTreeRoot".to_vec()),
            ],
        )];
        while let Some((obj, via)) = stack.pop() {
            if !self.consume_element("structure tree") {
                return;
            }
            let Some(d) = obj.as_dict() else { continue };
            for key in [b"ActualText".as_slice(), b"Alt".as_slice(), b"E".as_slice()] {
                if d.contains_key(key) {
                    let mut v = via.clone();
                    v.push(PathStep::Key(key.to_vec()));
                    self.add_finding(AuditFinding {
                        kind: FindingKind::AlternateText,
                        object: None,
                        reach: ReachClass::Reachable,
                        via: v,
                        referrers: vec![],
                        bytes: None,
                        note: format!("struct element /{}", String::from_utf8_lossy(key)),
                    });
                }
            }
            // Recurse into /K (kids). Elements may be Refs — resolve
            // each before pushing so `as_dict` sees the element dict,
            // not the ref. Parent links (/P) are *not* followed —
            // they create cycles the step cap would only discover
            // after the fact.
            if let Some(k) = d.get(b"K".as_slice()) {
                let k2 = match self.doc.deref_defined(k) {
                    Ok(o) => o,
                    Err(e) => {
                        self.out.incomplete.push(AuditIncomplete {
                            scope: "structure tree".into(),
                            reason: format!("/K does not resolve: {}", e.kind()),
                            limit: matches!(e, Error::LimitExceeded { .. }),
                        });
                        continue;
                    }
                };
                match k2 {
                    Object::Array(items) => {
                        for (i, it) in items.iter().enumerate() {
                            let resolved = match self.doc.deref_defined(it) {
                                Ok(o) => o,
                                Err(e) => {
                                    self.out.incomplete.push(AuditIncomplete {
                                        scope: "structure tree".into(),
                                        reason: format!("/K[{i}] does not resolve: {}", e.kind()),
                                        limit: matches!(e, Error::LimitExceeded { .. }),
                                    });
                                    continue;
                                }
                            };
                            let mut v = via.clone();
                            v.push(PathStep::Key(b"K".to_vec()));
                            v.push(PathStep::Index(i as u32));
                            stack.push((resolved, v));
                        }
                    }
                    o => {
                        let mut v = via.clone();
                        v.push(PathStep::Key(b"K".to_vec()));
                        stack.push((o, v));
                    }
                }
            }
        }
    }

    /// Name trees under catalog `/Names`: EmbeddedFiles, JavaScript.
    /// Walks `/Names`/`/Kids` arrays only — name strings are used as
    /// path components, values are not read. Entry-point failures
    /// (`/Names` itself or each tree's root) are recorded — a
    /// missing `/Names` key is *not* the same as "no attachments".
    fn audit_name_trees(&mut self) {
        let Some(names) = self.doc.catalog().get(b"Names".as_slice()) else {
            return;
        };
        let names_obj = match self.doc.deref_defined(names) {
            Ok(o) => o,
            Err(e) => {
                self.out.incomplete.push(AuditIncomplete {
                    scope: "catalog /Names".into(),
                    reason: format!("does not resolve: {}", e.kind()),
                    limit: matches!(e, Error::LimitExceeded { .. }),
                });
                return;
            }
        };
        let Some(nd) = names_obj.as_dict() else {
            self.out.incomplete.push(AuditIncomplete {
                scope: "catalog /Names".into(),
                reason: "resolves to a non-dictionary object".into(),
                limit: false,
            });
            return;
        };
        for (tree_key, kind) in [
            (b"EmbeddedFiles".as_slice(), FindingKind::EmbeddedFiles),
            (b"JavaScript".as_slice(), FindingKind::JavaScript),
        ] {
            let Some(tree) = nd.get(tree_key) else {
                continue;
            };
            let tree_obj = match self.doc.deref_defined(tree) {
                Ok(o) => o,
                Err(e) => {
                    self.out.incomplete.push(AuditIncomplete {
                        scope: format!("name tree /{}", String::from_utf8_lossy(tree_key)),
                        reason: format!("root does not resolve: {}", e.kind()),
                        limit: matches!(e, Error::LimitExceeded { .. }),
                    });
                    continue; // the other tree is independent — audit it
                }
            };
            let base_via = vec![
                PathStep::TrailerKey(b"Root".to_vec()),
                PathStep::Key(b"Names".to_vec()),
                PathStep::Key(tree_key.to_vec()),
            ];
            self.add_finding(AuditFinding {
                kind,
                object: tree.as_ref(),
                reach: tree
                    .as_ref()
                    .map(|r| self.reach_of(r))
                    .unwrap_or(ReachClass::Reachable),
                via: base_via.clone(),
                referrers: vec![],
                bytes: None,
                note: format!("name tree /{}", String::from_utf8_lossy(tree_key)),
            });
            // Walk the name-tree pairs; each leaf value is a FileSpec
            // (embedded) or action (JavaScript) — presence only.
            self.walk_name_tree(tree_key, &tree_obj, base_via, kind);
        }
    }

    /// Walk one name tree iteratively. Two distinct guards:
    /// `visiting` — the node refs on the current traversal stack
    /// (catches true cycles) — and `seen` — every node already fully
    /// expanded (shared kids are re-visited exactly once). Bounded by
    /// `max_audit_elements` (per node + per `/Names` pair + per
    /// `/Kids` entry) and `max_audit_walk_depth` (tree depth). A
    /// failed `deref` or a skipped cycle is *recorded*, not dropped.
    fn walk_name_tree(
        &mut self,
        tree_key: &[u8],
        root: &Object,
        via: Vec<PathStep>,
        kind: FindingKind,
    ) {
        // Iterative DFS — no call-stack growth. Each stack entry is
        // (node-object, path, depth, node-ref-or-none). The ref is
        // kept separately so two different objects sharing one
        // subtree are still walked once each.
        struct Frame {
            obj: Object,
            via: Vec<PathStep>,
            depth: u32,
            node_ref: Option<IndirectRef>,
            /// Refs on the path from the tree root to this node —
            /// a kid whose ref is here is a cycle, not a share.
            path_refs: HashSet<IndirectRef>,
        }
        let mut root_path = HashSet::new();
        if let Some(rr) = root.as_ref() {
            root_path.insert(rr);
        }
        let mut stack: Vec<Frame> = vec![Frame {
            obj: root.clone(),
            via,
            depth: 0,
            node_ref: root.as_ref(),
            path_refs: root_path,
        }];
        // `seen` — nodes already fully expanded (a second visit is
        // a shared node, walked exactly once). Cycle detection uses
        // the *path* (`via`): a kid whose ref already appears in the
        // current `via` is re-entering its own ancestor — a cycle,
        // not a share.
        let mut seen: HashSet<IndirectRef> = HashSet::new();
        while let Some(f) = stack.pop() {
            let Frame {
                obj,
                via,
                depth,
                node_ref,
                path_refs,
            } = f;
            if !self.consume_element("name-tree walk") {
                break;
            }
            if let Some(nr) = node_ref {
                if !seen.insert(nr) {
                    self.out.incomplete.push(AuditIncomplete {
                        scope: format!("name tree /{}", String::from_utf8_lossy(tree_key)),
                        reason: format!("shared node {nr} already walked"),
                        limit: false,
                    });
                    continue;
                }
            }
            let Some(d) = obj.as_dict() else {
                continue;
            };
            // /Names pairs — each pair consumes one element unit.
            if let Some(Object::Array(pairs)) = d.get(b"Names".as_slice()) {
                let mut it = pairs.iter();
                while let (Some(name), Some(value)) = (it.next(), it.next()) {
                    if !self.consume_element("name-tree walk") {
                        return; // element budget exhausted — stop the tree walk
                    }
                    let mut v = via.clone();
                    if let Object::String(s) = name {
                        v.push(PathStep::NameTreeEntry(s.clone()));
                    }
                    let sub = match kind {
                        FindingKind::EmbeddedFiles => FindingKind::FileSpec,
                        other => other,
                    };
                    self.add_finding(AuditFinding {
                        kind: sub,
                        object: value.as_ref(),
                        reach: value
                            .as_ref()
                            .map(|r| self.reach_of(r))
                            .unwrap_or(ReachClass::Reachable),
                        via: v,
                        referrers: vec![],
                        bytes: self.stream_bytes(value),
                        note: "name-tree leaf".into(),
                    });
                }
            }
            // /Kids — recurse via the explicit stack. Depth is
            // checked per node, so a deep branch stops without
            // cutting off siblings at shallower levels.
            if let Some(Object::Array(kids)) = d.get(b"Kids".as_slice()) {
                for (i, k) in kids.iter().enumerate() {
                    if !self.consume_element("name-tree walk") {
                        return; // element budget exhausted
                    }
                    let mut v = via.clone();
                    v.push(PathStep::Key(b"Kids".to_vec()));
                    v.push(PathStep::Index(i as u32));
                    if depth + 1 > self.limits.max_audit_walk_depth as u32 {
                        self.out.incomplete.push(AuditIncomplete {
                            scope: format!("name tree /{}", String::from_utf8_lossy(tree_key)),
                            reason: format!(
                                "max_audit_walk_depth ({}) reached at /Kids[{}]",
                                self.limits.max_audit_walk_depth, i
                            ),
                            limit: true,
                        });
                        continue; // drop this kid, keep siblings
                    }
                    match self.doc.deref_defined(k) {
                        Ok(ko) => {
                            // Cycle check: if this kid's ref already
                            // sits on the current path, walking it
                            // would re-enter an ancestor — report and
                            // skip, don't recurse forever.
                            if let Some(kr) = k.as_ref() {
                                if path_refs.contains(&kr) {
                                    self.out.incomplete.push(AuditIncomplete {
                                        scope: format!(
                                            "name tree /{}",
                                            String::from_utf8_lossy(tree_key)
                                        ),
                                        reason: format!("cycle: node {kr} re-enters its own path"),
                                        limit: false,
                                    });
                                    continue;
                                }
                            }
                            let mut child_path = path_refs.clone();
                            if let Some(kr) = k.as_ref() {
                                child_path.insert(kr);
                            }
                            stack.push(Frame {
                                obj: ko,
                                via: v,
                                depth: depth + 1,
                                node_ref: k.as_ref(),
                                path_refs: child_path,
                            });
                        }
                        Err(e) => {
                            self.out.incomplete.push(AuditIncomplete {
                                scope: format!("name tree /{}", String::from_utf8_lossy(tree_key)),
                                reason: format!("/Kids[{}] does not resolve: {}", i, e.kind()),
                                limit: false,
                            });
                        }
                    }
                }
            }
            let _ = node_ref; // node_ref is used only for seen/path checks
        }
    }

    /// OCProperties: catalog-level finding already recorded; here the
    /// `/OCGs` array members are enumerated as Ocg findings.
    fn audit_oc_properties(&mut self) {
        let Some(ocp) = self.doc.catalog().get(b"OCProperties".as_slice()) else {
            return;
        };
        let Ok(o) = self.doc.deref_defined(ocp) else {
            return;
        };
        let Some(d) = o.as_dict() else { return };
        if let Some(ocgs) = d.get(b"OCGs".as_slice()) {
            if let Ok(arr) = self.doc.deref_defined(ocgs) {
                if let Some(items) = arr.as_array() {
                    for (i, it) in items.iter().enumerate() {
                        if let Object::Ref(r) = it {
                            self.add_finding(AuditFinding {
                                kind: FindingKind::Ocg,
                                object: Some(*r),
                                reach: self.reach_of(*r),
                                via: vec![
                                    PathStep::TrailerKey(b"Root".to_vec()),
                                    PathStep::Key(b"OCProperties".to_vec()),
                                    PathStep::Key(b"OCGs".to_vec()),
                                    PathStep::Index(i as u32),
                                ],
                                referrers: vec![],
                                bytes: None,
                                note: "optional-content group".into(),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Enumerate `/Resources` entries per page — each Font/XObject
    /// name is a finding. Whether the name is *used* by a `Do`/`Tf`
    /// is reported separately as ResourceUsageUnexamined (the audit
    /// does not run the full content interpreter for usage checks).
    fn audit_resources(&mut self) {
        for p in self.doc.pages() {
            let Some(res) = p.resources() else { continue };
            let Ok(r) = self.doc.deref_defined(res) else {
                continue;
            };
            let Some(rd) = r.as_dict() else { continue };
            for cat in [b"XObject".as_slice(), b"Font".as_slice()] {
                let Some(c) = rd.get(cat) else { continue };
                let Ok(co) = self.doc.deref_defined(c) else {
                    continue;
                };
                let Some(cd) = co.as_dict() else { continue };
                for (name, val) in cd {
                    let obj = val.as_ref();
                    self.add_finding(AuditFinding {
                        kind: FindingKind::ResourceUsageUnexamined,
                        object: obj,
                        reach: obj
                            .map(|r| self.reach_of(r))
                            .unwrap_or(ReachClass::Reachable),
                        via: vec![
                            PathStep::Key(b"Page".to_vec()),
                            PathStep::Index(p.index() as u32),
                            PathStep::ResourceName {
                                category: cat.to_vec(),
                                name: name.clone(),
                            },
                        ],
                        referrers: vec![],
                        bytes: self.stream_bytes(val),
                        note: format!(
                            "page {} /{}/{}",
                            p.index() + 1,
                            String::from_utf8_lossy(cat),
                            String::from_utf8_lossy(name)
                        ),
                    });
                }
            }
        }
    }

    /// Per-object stream classification: images, embedded fonts,
    /// ToUnicode, forms — found by walking every *live* xref object
    /// (so unreachable images are reported too, classified
    /// UnreachableLive). Type is read from the stream dict only;
    /// bodies are not decoded.
    fn audit_stream_kinds(&mut self) {
        for (r, entry) in self.doc.xref_entries() {
            if !matches!(entry, XrefEntry::InUse { .. }) {
                continue; // compressed objects are resolved lazily
            }
            let obj = match self.doc.resolve_defined(r) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let reach = self.reach_of(r);
            match &obj {
                Object::Stream { dict, .. } => {
                    let subtype = dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name());
                    let type_ = dict.get(b"Type".as_slice()).and_then(|o| o.as_name());
                    let kind = match (type_, subtype) {
                        (_, Some(b"Image")) => Some(FindingKind::Image),
                        (_, Some(b"Form")) => Some(FindingKind::FormXObject),
                        (Some(b"ObjStm"), _) => None,
                        _ => {
                            if dict.contains_key(b"ToUnicode".as_slice())
                                || type_ == Some(b"ToUnicode")
                            {
                                Some(FindingKind::ToUnicode)
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(kind) = kind {
                        self.add_finding(AuditFinding {
                            kind,
                            object: Some(r),
                            reach,
                            via: vec![],
                            referrers: vec![],
                            bytes: self.stream_bytes(&obj),
                            note: format!(
                                "{} stream {}{}",
                                String::from_utf8_lossy(subtype.or(type_).unwrap_or(b"?")),
                                r,
                                self.stream_filter_label(&obj)
                            ),
                        });
                    }
                }
                Object::Dict(d) => {
                    // A font dict carrying /ToUnicode references a
                    // ToUnicode CMap stream — report the *stream*
                    // object (the finding's object), not the font.
                    if let Some(tu) = d.get(b"ToUnicode".as_slice()) {
                        self.add_finding(AuditFinding {
                            kind: FindingKind::ToUnicode,
                            object: tu.as_ref().or(Some(r)),
                            reach: tu.as_ref().map(|x| self.reach_of(x)).unwrap_or(reach),
                            via: vec![PathStep::Key(b"ToUnicode".to_vec())],
                            referrers: vec![],
                            bytes: self.stream_bytes(tu),
                            note: format!("font {} carries /ToUnicode", r),
                        });
                    }
                    // Embedded font program: /FontFile/FontFile2/FontFile3
                    // inside a FontDescriptor. Detect the descriptor by
                    // /Type /FontDescriptor.
                    if d.get(b"Type".as_slice()).and_then(|o| o.as_name())
                        == Some(b"FontDescriptor")
                    {
                        for k in [
                            b"FontFile".as_slice(),
                            b"FontFile2".as_slice(),
                            b"FontFile3".as_slice(),
                        ] {
                            if let Some(ff) = d.get(k) {
                                self.add_finding(AuditFinding {
                                    kind: FindingKind::EmbeddedFont,
                                    object: ff.as_ref().or(Some(r)),
                                    reach: ff.as_ref().map(|x| self.reach_of(x)).unwrap_or(reach),
                                    via: vec![PathStep::Key(k.to_vec())],
                                    referrers: vec![],
                                    bytes: self.stream_bytes(ff),
                                    note: format!(
                                        "font program /{} in {}",
                                        String::from_utf8_lossy(k),
                                        r
                                    ),
                                });
                            }
                        }
                    }
                    // Action dictionaries: a dict with /S is an action.
                    if let Some(s) = d.get(b"S".as_slice()).and_then(|o| o.as_name()) {
                        let kind = if s == b"JavaScript" {
                            FindingKind::JavaScript
                        } else {
                            FindingKind::Action
                        };
                        self.add_finding(AuditFinding {
                            kind,
                            object: Some(r),
                            reach,
                            via: vec![],
                            referrers: vec![],
                            bytes: None,
                            note: format!("action /S = /{} in {}", String::from_utf8_lossy(s), r),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Encoded + declared length of a stream object, plus whether a
    /// `/Filter` is present (so the caller can tell "encoded bytes"
    /// from "raw bytes"). `None` for non-streams. `declared` is the
    /// `/Length` value as written — it can disagree with the encoded
    /// bytes actually present. The *decoded* length is not computed:
    /// decoding is unexamined work the audit avoids.
    fn stream_bytes(&self, o: &Object) -> Option<(u64, Option<u64>)> {
        let resolved = match self.doc.deref_defined(o) {
            Ok(r) => r,
            Err(_) => return None,
        };
        match resolved {
            Object::Stream { dict, data } => {
                let declared = dict
                    .get(b"Length".as_slice())
                    .and_then(|l| self.doc.deref_defined(l).ok())
                    .and_then(|l| l.as_i64())
                    .map(|n| n as u64);
                Some((data.len() as u64, declared))
            }
            _ => None,
        }
    }

    /// Whether a stream's `/Filter` is absent (raw bytes) or present
    /// (encoded) — returned for note text so "encoded size" is never
    /// mistaken for "decoded size".
    fn stream_filter_label(&self, o: &Object) -> &'static str {
        match self.doc.deref_defined(o) {
            Ok(Object::Stream { dict, .. }) => {
                if dict.contains_key(b"Filter".as_slice()) {
                    " (filtered)"
                } else {
                    " (raw)"
                }
            }
            _ => "",
        }
    }
}

// ---------------------------------------------------------------------
// pass 3 — xref / history / physical data
// ---------------------------------------------------------------------

impl<'d> Audit<'d> {
    fn audit_xref_and_history(&mut self) {
        let sections = self.doc.xref_section_offsets();
        self.out.history.xref_sections = sections.len() as u64;
        if sections.len() > 1 {
            self.out.history.incremental_history = Some(sections.len() as u64 - 1);
        }
        let mut live = 0u64;
        let mut free = 0u64;
        let mut compressed = 0u64;
        let mut unreachable_live = 0u64;
        let mut unevaluated_live = 0u64;
        // Collect the merged live (number, generation) -> offset so a
        // *superseded* body for the same number can be told from a
        // live one.
        let mut live_offsets: HashMap<u32, u64> = HashMap::new();
        for (r, e) in self.doc.xref_entries() {
            match e {
                XrefEntry::Free { .. } => {
                    free += 1;
                    self.add_finding(AuditFinding {
                        kind: FindingKind::FreeObject,
                        object: Some(r),
                        reach: ReachClass::UnreachableLive,
                        via: vec![],
                        referrers: vec![],
                        bytes: None,
                        note: "free xref entry".into(),
                    });
                }
                XrefEntry::Compressed { .. } => {
                    compressed += 1;
                    live += 1;
                    if !self.reachable.contains(&r) {
                        if self.walk_complete {
                            unreachable_live += 1;
                            self.add_finding(AuditFinding {
                                kind: FindingKind::UnreachableObject,
                                object: Some(r),
                                reach: ReachClass::UnreachableLive,
                                via: vec![],
                                referrers: vec![],
                                bytes: None,
                                note: "compressed object unreachable from roots".into(),
                            });
                        } else {
                            unevaluated_live += 1;
                            self.add_finding(AuditFinding {
                                kind: FindingKind::UnreachableObject,
                                object: Some(r),
                                reach: ReachClass::Unevaluated,
                                via: vec![],
                                referrers: vec![],
                                bytes: None,
                                note: "compressed object — reachability \
                                       unevaluated (walk incomplete)"
                                    .into(),
                            });
                        }
                    }
                }
                XrefEntry::InUse { offset } => {
                    live += 1;
                    live_offsets.insert(r.number, offset);
                    if !self.reachable.contains(&r) {
                        // Distinguish a structural object the roots
                        // never reference (ObjStm, XRef, Info-less
                        // trailer machinery) from a genuinely orphaned
                        // content object — and, when the walk was
                        // incomplete, say *unevaluated*, not
                        // unreachable.
                        let obj = self.doc.resolve_defined(r).ok();
                        let kind_label = obj
                            .as_ref()
                            .and_then(|o| o.as_dict_like())
                            .and_then(|d| d.get(b"Type".as_slice()))
                            .and_then(|o| o.as_name())
                            .map(|n| String::from_utf8_lossy(n).into_owned())
                            .unwrap_or_else(|| "?".into());
                        if self.walk_complete {
                            unreachable_live += 1;
                            self.add_finding(AuditFinding {
                                kind: FindingKind::UnreachableObject,
                                object: Some(r),
                                reach: ReachClass::UnreachableLive,
                                via: vec![],
                                referrers: vec![],
                                bytes: obj.as_ref().and_then(|o| self.stream_bytes(o)),
                                note: format!(
                                    "in-use object unreachable from roots (type /{kind_label})"
                                ),
                            });
                        } else {
                            unevaluated_live += 1;
                            self.add_finding(AuditFinding {
                                kind: FindingKind::UnreachableObject,
                                object: Some(r),
                                reach: ReachClass::Unevaluated,
                                via: vec![],
                                referrers: vec![],
                                bytes: obj.as_ref().and_then(|o| self.stream_bytes(o)),
                                note: format!(
                                    "in-use object — reachability unevaluated \
                                     (walk incomplete; type /{kind_label})"
                                ),
                            });
                        }
                    }
                }
            }
        }
        // Walk every *earlier* xref section and collect its in-use
        // entries: an object whose number appears at a different
        // offset than its live entry has a superseded body still
        // present in the file (the merged xref shadowed it).
        let mut superseded: BTreeMap<u32, u64> = BTreeMap::new();
        for (i, &sec_off) in sections.iter().enumerate() {
            if i == 0 {
                continue; // the newest section defines the live view
            }
            let mut sec_entries: Vec<((u32, u16), XrefEntry)> = Vec::new();
            if let Err(e) = crate::xref::read_one_section(
                self.doc.raw_bytes(),
                self.limits,
                sec_off,
                &mut |key, entry| sec_entries.push((key, entry)),
                &mut Vec::new(),
            ) {
                self.out.incomplete.push(AuditIncomplete {
                    scope: format!("old xref section at {sec_off}"),
                    reason: format!("could not re-read: {}", e.kind()),
                    limit: false,
                });
                continue;
            }
            for ((num, _gen), entry) in sec_entries {
                if let XrefEntry::InUse { offset } = entry {
                    if live_offsets.get(&num) != Some(&offset) {
                        superseded.insert(num, offset);
                    }
                }
            }
        }
        for (num, off) in superseded {
            self.out.history.objects_in_old_region += 1;
            self.add_finding(AuditFinding {
                kind: FindingKind::SupersededObject,
                object: Some(IndirectRef::new(num, 0)),
                reach: ReachClass::PreviousRevision,
                via: vec![],
                referrers: vec![],
                bytes: None,
                note: format!(
                    "object {num} has a superseded body at offset {off} \
                     (a different body is live)"
                ),
            });
        }
        self.out.history.free_entries = free;
        self.out.history.compressed_entries = compressed;
        self.out.reachability.live_objects = live;
        self.out.reachability.reachable = self.reachable.len() as u64;
        self.out.reachability.reachable_live = self
            .reachable
            .iter()
            .filter(|r| {
                matches!(
                    self.doc.xref_entry_kind(**r),
                    Some(XrefEntry::InUse { .. }) | Some(XrefEntry::Compressed { .. })
                )
            })
            .count() as u64;
        self.out.reachability.unreachable_live = unreachable_live;
        self.out.reachability.unevaluated_live = unevaluated_live;
        self.out.history.physical_slack_examined = false;
        if !self.out.history.physical_slack_examined {
            self.out.unexamined.push(
                "physical byte regions outside xref-covered objects \
                 (slack between objects, bytes after %%EOF, bodies in \
                 superseded revisions other than the still-live objects \
                 reported as SupersededObject)"
                    .into(),
            );
        }
        if sections.len() <= 1 {
            // "No history" means exactly one xref section and no
            // /Prev — it says nothing about physical slack bytes.
            self.out.unexamined.push(
                "absence of incremental history (single xref section) \
                 does not prove the file carries no old bytes — \
                 physical slack was not scanned"
                    .into(),
            );
        }
        // Content-scan for marked-content ActualText — bounded by
        // max_audit_content_ops per page; a page that cannot be
        // parsed is Unexamined, not "no ActualText".
        self.audit_content_actualtext();
    }

    /// Scan each page's content ops for marked-content property lists
    /// carrying `/ActualText`, `/Alt` or `/E` — the audit's own
    /// bounded pass, not the full diagnosis walker.
    fn audit_content_actualtext(&mut self) {
        for p in self.doc.pages() {
            // A /Contents reference to an undefined object is null for
            // a reader (§7.3.10) — an empty page — but for an audit it
            // is content that could not be examined.
            if let Some(c) = p.contents() {
                let items: Vec<Object> = match self.doc.deref_defined(c) {
                    Ok(Object::Array(a)) => a,
                    Ok(_) => Vec::new(),
                    Err(e) => {
                        self.out.incomplete.push(AuditIncomplete {
                            scope: format!("page {} content", p.index() + 1),
                            reason: format!("/Contents does not resolve: {}", e.kind()),
                            limit: false,
                        });
                        continue;
                    }
                };
                if let Some(e) = items.iter().find_map(|i| self.doc.deref_defined(i).err()) {
                    self.out.incomplete.push(AuditIncomplete {
                        scope: format!("page {} content", p.index() + 1),
                        reason: format!("/Contents element does not resolve: {}", e.kind()),
                        limit: false,
                    });
                    continue;
                }
            }
            let ops = match self.doc.page_operations(p) {
                Ok(o) => o,
                Err(e) => {
                    self.out.incomplete.push(AuditIncomplete {
                        scope: format!("page {} content", p.index() + 1),
                        reason: format!("content parse failed: {}", e.kind()),
                        limit: false,
                    });
                    continue;
                }
            };
            let mut n = 0u64;
            for (i, op) in ops.iter().enumerate() {
                n += 1;
                if n > self.limits.max_audit_content_ops {
                    self.out.incomplete.push(AuditIncomplete {
                        scope: format!("page {} content", p.index() + 1),
                        reason: format!(
                            "max_audit_content_ops ({}) reached",
                            self.limits.max_audit_content_ops
                        ),
                        limit: true,
                    });
                    break;
                }
                let dict_obj = match op {
                    Operation::BeginMarkedContentWithProps { properties, .. }
                    | Operation::DesignatedMarkedContentPoint { properties, .. } => {
                        Some(properties.clone())
                    }
                    _ => None,
                };
                let Some(props) = dict_obj else { continue };
                // Properties may be a name into /Properties resources —
                // resolve it; unresolvable or non-dict → skip (not
                // "no ActualText").
                let resolved = match props {
                    Object::Name(n) => match p.resources() {
                        Some(res) => match self.doc.resolve_resource(res, "Properties", &n) {
                            Ok(Some(o)) => o,
                            _ => continue,
                        },
                        None => continue,
                    },
                    o => o,
                };
                let Some(d) = resolved.as_dict() else {
                    continue;
                };
                for key in [b"ActualText".as_slice(), b"Alt".as_slice(), b"E".as_slice()] {
                    if d.contains_key(key) {
                        self.add_finding(AuditFinding {
                            kind: FindingKind::AlternateText,
                            object: None,
                            reach: ReachClass::Reachable,
                            via: vec![
                                PathStep::Key(b"Page".to_vec()),
                                PathStep::Index(p.index() as u32),
                                PathStep::Key(b"Contents".to_vec()),
                                PathStep::Index(i as u32),
                            ],
                            referrers: vec![],
                            bytes: None,
                            note: format!(
                                "marked-content /{} at op #{}",
                                String::from_utf8_lossy(key),
                                i
                            ),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a PDF from `(obj_num, body)` pairs; xref + trailer are
    /// generated automatically. `extra_trailer` is appended to the
    /// trailer dict (e.g. `/Info 9 0 R`).
    fn assemble(objs: &[(u32, &[u8])], extra_trailer: &[u8]) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        for (n, body) in objs {
            offs.push((*n, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} 0 obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        }
        let size = objs.iter().map(|(n, _)| n).max().unwrap_or(&0) + 1;
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
        for n in 1..size {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        let mut trailer = format!("trailer\n<< /Size {size} /Root 1 0 R ").into_bytes();
        trailer.extend_from_slice(extra_trailer);
        trailer.extend_from_slice(b" >>\n");
        parts.push(trailer);
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    fn has_kind(a: &ResidualAudit, k: FindingKind) -> bool {
        a.findings.iter().any(|f| f.kind == k)
    }

    fn find_kind(a: &ResidualAudit, k: FindingKind) -> Vec<&AuditFinding> {
        a.findings.iter().filter(|f| f.kind == k).collect()
    }

    /// Minimal one-page document: catalog + pages + page + contents.
    fn minimal_pdf() -> Vec<u8> {
        assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        )
    }

    // ---- presence detection -------------------------------------

    #[test]
    fn detects_info() {
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /Title (secret title) /Author (someone) >>"),
            ],
            b"/Info 5 0 R",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        let f = find_kind(&a, FindingKind::DocumentInfo);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].object, Some(IndirectRef::new(5, 0)));
        assert_eq!(f[0].reach, ReachClass::Reachable);
        assert!(f[0].note.contains("/Title"));
        assert!(f[0].note.contains("/Author"));
        // The *values* must never appear in the note.
        assert!(!f[0].note.contains("secret"));
        assert!(!f[0].note.contains("someone"));
    }

    #[test]
    fn detects_catalog_metadata_and_openaction() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R \
                      /OpenAction 6 0 R >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (
                    5,
                    b"<< /Type /Metadata /Subtype /XML /Length 10 >>\nstream\n<x:xmpmeta>\nendstream",
                ),
                (6, b"<< /S /JavaScript /JS (app.alert()) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::Metadata));
        assert!(has_kind(&a, FindingKind::OpenAction));
        // The action dict /S /JavaScript is a JavaScript finding.
        assert!(has_kind(&a, FindingKind::JavaScript));
    }

    #[test]
    fn detects_embedded_files_and_filespec() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 5 0 R >> \
                      /AF [6 0 R] >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // Name tree root: one pair (name) -> filespec.
                (5, b"<< /Names [(report.txt) 6 0 R] >>"),
                // FileSpec with /EF embedded-file stream.
                (
                    6,
                    b"<< /Type /Filespec /F (report.txt) \
                      /EF << /F 7 0 R >> >>",
                ),
                (
                    7,
                    b"<< /Type /EmbeddedFile /Length 4 >>\nstream\nDATA\nendstream",
                ),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::EmbeddedFiles));
        assert!(has_kind(&a, FindingKind::FileSpec));
        assert!(has_kind(&a, FindingKind::AssociatedFiles));
    }

    #[test]
    fn detects_annots_text_ap_fileattachment_widget() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R /Annots [5 0 R 6 0 R 8 0 R] >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // Text annot with Contents + RC + AP.
                (
                    5,
                    b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] \
                      /Contents (confidential note) /RC (<x>rich</x>) \
                      /AP << /N 7 0 R >> >>",
                ),
                // FileAttachment annot with /FS.
                (
                    6,
                    b"<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] \
                      /FS 9 0 R >>",
                ),
                (7, b"<< /Length 2 >>\nstream\nxx\nendstream"),
                // Widget annot (form field) with /V.
                (
                    8,
                    b"<< /Type /Annot /Subtype /Widget /Rect [0 0 50 12] \
                      /FT /Tx /T (field1) /V (user input) >>",
                ),
                (9, b"<< /Type /Filespec /F (a.bin) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::AnnotText));
        assert!(has_kind(&a, FindingKind::AnnotAppearance));
        assert!(has_kind(&a, FindingKind::FileAttachmentAnnot));
        assert!(has_kind(&a, FindingKind::FileSpec));
        assert!(has_kind(&a, FindingKind::WidgetAnnot));
        assert!(has_kind(&a, FindingKind::FormFieldValue));
        assert!(has_kind(&a, FindingKind::AcroForm));
        // The annot /Contents value must not leak into the note.
        for f in &a.findings {
            assert!(!f.note.contains("confidential"), "{}", f.note);
            assert!(!f.note.contains("user input"), "{}", f.note);
        }
    }

    #[test]
    fn detects_struct_tree_alt_and_actualtext() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /StructTreeRoot 5 0 R >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // Structure tree: root with one kid carrying /Alt +
                // /ActualText + /E.
                (5, b"<< /Type /StructTreeRoot /K [6 0 R] >>"),
                (
                    6,
                    b"<< /Type /StructElem /S /Figure /Alt (a figure) \
                      /ActualText (actual) /E (expansion) >>",
                ),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::StructTree));
        let alt = find_kind(&a, FindingKind::AlternateText);
        assert_eq!(alt.len(), 3, "expected /Alt + /ActualText + /E");
        for f in &alt {
            assert!(!f.note.contains("a figure"));
            assert!(!f.note.contains("actual"));
        }
    }

    #[test]
    fn detects_actualtext_in_content() {
        // /Span << /ActualText (… ) >> BDC … EMC in the page stream.
        let body = b"/Span << /ActualText (hidden) >> BDC (x) Tj EMC\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, &sb),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        let alt = find_kind(&a, FindingKind::AlternateText);
        assert_eq!(alt.len(), 1);
        assert!(alt[0].note.contains("/ActualText"));
        assert!(!alt[0].note.contains("hidden"));
    }

    #[test]
    fn detects_ocg_and_ocproperties() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /OCProperties << /OCGs [5 0 R] /D << /Order [5 0 R] >> >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /Type /OCG /Name (Layer 1) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::OptionalContent));
        let ocg = find_kind(&a, FindingKind::Ocg);
        assert_eq!(ocg.len(), 1);
        assert_eq!(ocg[0].object, Some(IndirectRef::new(5, 0)));
    }

    #[test]
    fn detects_images_fonts_tounicode_forms() {
        // Reuse the region-removal fixture: font + FontFile2 + ToUnicode +
        // image + form — all should be found.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut fdesc = b"<< /Type /FontDescriptor /FontName /KasaneTest \
              /Flags 32 /FontBBox [0 0 1 1] /ItalicAngle 0 /Ascent 1 \
              /Descent 0 /CapHeight 1 /StemV 1 /FontFile2 8 0 R >>"
            .to_vec();
        let _ = &mut fdesc;
        let mut ff2 = format!("<< /Length {} >>\nstream\n", font_bytes.len()).into_bytes();
        ff2.extend_from_slice(&font_bytes);
        ff2.extend_from_slice(b"\nendstream");
        let img = [0x11u8, 0x22, 0x33, 0x44];
        let mut is = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 4 >>\nstream\n"
            .to_vec();
        is.extend_from_slice(&img);
        is.extend_from_slice(b"\nendstream");
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Resources << /Font << /F1 4 0 R >> \
                      /XObject << /Im1 6 0 R /Fm1 9 0 R >> >> \
                      /Contents 5 0 R >>",
                ),
                (
                    4,
                    b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
                      /FirstChar 32 /LastChar 255 /Widths [600] \
                      /Encoding /WinAnsiEncoding /ToUnicode 7 0 R \
                      /FontDescriptor 10 0 R >>",
                ),
                (5, b"<< /Length 0 >>\nstream\n\nendstream"),
                (6, &is),
                (7, b"<< /Length 4 >>\nstream\nCMAP\nendstream"),
                (8, &ff2),
                (
                    9,
                    b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                      /Length 0 >>\nstream\n\nendstream",
                ),
                (10, &fdesc),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::Image));
        assert!(has_kind(&a, FindingKind::FormXObject));
        assert!(has_kind(&a, FindingKind::ToUnicode));
        assert!(has_kind(&a, FindingKind::EmbeddedFont));
    }

    // ---- reachability / references ------------------------------

    #[test]
    fn unreachable_and_shared_and_cyclic() {
        // Object 5 is *not* referenced by anything — unreachable live.
        // Objects 6 and 7 reference each other — a cycle that must
        // terminate.
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Extra 6 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /Orphaned true >>"),
                (6, b"<< /Other 7 0 R >>"),
                (7, b"<< /Back 6 0 R >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        let un = find_kind(&a, FindingKind::UnreachableObject);
        // Object 5 is unreachable; 6 and 7 are reachable via
        // /Extra → 6 → 7 → 6 (cycle terminates via `visited`).
        assert_eq!(un.len(), 1);
        assert_eq!(un[0].object, Some(IndirectRef::new(5, 0)));
        assert!(a.reachability.edges_followed > 0);
        assert!(a.reachability.walk_complete);
    }

    #[test]
    fn shared_object_records_multiple_referrers() {
        // Object 6 is referenced from *both* the page dict (/Foo) and
        // the catalog (/Bar) — both referrers must be recorded.
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Bar 6 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R /Foo 6 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (
                    5,
                    b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
                      /ColorSpace /DeviceGray /BitsPerComponent 8 \
                      /Length 1 >>\nstream\n\xAA\nendstream",
                ),
                (
                    6,
                    b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 \
                      /ColorSpace /DeviceGray /BitsPerComponent 8 \
                      /Length 1 >>\nstream\n\xBB\nendstream",
                ),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // Find the Image finding for object 6 — its referrers list
        // should name both the page dict and the catalog.
        let img6 = a
            .findings
            .iter()
            .find(|f| f.object == Some(IndirectRef::new(6, 0)));
        let img6 = img6.expect("object 6 should be audited");
        assert!(
            img6.referrers.len() >= 2,
            "expected >=2 referrers for a shared object, got {:?}",
            img6.referrers
        );
    }

    #[test]
    fn dangling_reference_is_missing_not_unexamined() {
        // Page /Contents points at a non-existent object — the audit
        // must still finish and the missing ref shows up as a
        // resolution failure in the content scan (incomplete) or be
        // absent, but the audit must not claim "no ActualText".
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 99 0 R >>",
                ),
            ],
            b"",
        );
        let d = doc(&pdf);
        let a = audit_residuals(&d, &Limits::default()).unwrap();
        // The content scan records an incomplete entry, not silence.
        assert!(
            a.incomplete
                .iter()
                .any(|i| i.scope.contains("page 1 content")),
            "expected an incomplete entry for the unreadable page content"
        );
    }

    // ---- history --------------------------------------------------

    #[test]
    fn incremental_update_reports_history_and_superseded() {
        // Two revisions: object 5 (/Info) originally has /Title (old);
        // the update redefines 5 0 with a new body and a new xref
        // section. The audit must report 2 sections and the still-live
        // object whose body predates the newest section.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Title (old title) /Author (someone) >>");
        let xref1: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref1}\n%%EOF\n").into_bytes());
        // --- incremental update: redefine object 5 ---------------
        let off5b: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"5 0 obj\n".to_vec());
        parts.push(b"<< /Title (new title) >>".to_vec());
        parts.push(b"\nendobj\n".to_vec());
        let xref2: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n5 1\n".to_vec());
        parts.push(format!("{off5b:010} 00000 n \n").into_bytes());
        parts.push(format!("trailer\n<< /Size 6 /Root 1 0 R /Prev {xref1} >>\n").into_bytes());
        parts.push(format!("startxref\n{xref2}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let a = audit_residuals(&d, &Limits::default()).unwrap();
        assert_eq!(a.history.xref_sections, 2);
        assert_eq!(a.history.incremental_history, Some(1));
        // The newest trailer has no /Info — merged-trailer /Info is
        // flagged PreviousRevision.
        let info = find_kind(&a, FindingKind::DocumentInfo);
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].reach, ReachClass::PreviousRevision);
        // Only object 5 has a *superseded* body — its old definition
        // ("old title") sits at the offset recorded by the earlier
        // xref section, while the live entry points at the newer
        // body. Objects 1-4 share one body across revisions, so they
        // are not superseded.
        let sup = find_kind(&a, FindingKind::SupersededObject);
        assert_eq!(sup.len(), 1);
        assert_eq!(sup[0].object, Some(IndirectRef::new(5, 0)));
        assert_eq!(sup[0].reach, ReachClass::PreviousRevision);
    }

    #[test]
    fn no_history_is_one_section_not_absent_evidence() {
        let a = audit_residuals(&doc(&minimal_pdf()), &Limits::default()).unwrap();
        assert_eq!(a.history.xref_sections, 1);
        assert_eq!(a.history.incremental_history, None);
        // "No history" never means "no old bytes" — the audit says so.
        assert!(a.unexamined.iter().any(|u| u.contains("physical slack")));
    }

    // ---- limits / leakage ----------------------------------------

    #[test]
    fn findings_cap_marks_incomplete() {
        let limits = Limits {
            max_audit_findings: 2,
            ..Default::default()
        };
        // hello-style document has >=3 findings (Resources, Image,
        // ResourceUsageUnexamined, FreeObject…).
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Resources << /XObject << /Im1 5 0 R >> >> \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (
                    5,
                    b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
                      /ColorSpace /DeviceGray /BitsPerComponent 8 \
                      /Length 4 >>\nstream\n\x11\x22\x33\x44\nendstream",
                ),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        assert_eq!(a.findings.len(), 2);
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope == "findings" && i.limit));
    }

    /// An `experimental-remove-region` output still lists residual
    /// information distinctly from the edited content — the audit
    /// must tell the rewritten contents apart from the untouched
    /// Info/annot/etc., not conflate them.
    #[test]
    fn audit_distinguishes_edited_from_residual() {
        // Build a doc with /Info AND an annot AND an image; run
        // remove_region on the image rect, then audit the output.
        // The Info dict and the annot must still be found — the edit
        // never touched them.
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut ff2 = format!("<< /Length {} >>\nstream\n", font_bytes.len()).into_bytes();
        ff2.extend_from_slice(&font_bytes);
        ff2.extend_from_slice(b"\nendstream");
        let img = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut is = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 4 >>\nstream\n"
            .to_vec();
        is.extend_from_slice(&img);
        is.extend_from_slice(b"\nendstream");
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Resources << /Font << /F1 4 0 R >> \
                      /XObject << /Im1 6 0 R >> >> \
                      /Annots [7 0 R] /Contents 5 0 R >>",
                ),
                (
                    4,
                    b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
                      /FirstChar 32 /LastChar 255 /Widths [600] \
                      /Encoding /WinAnsiEncoding >>",
                ),
                (
                    5,
                    b"<< /Length 40 >>\nstream\n\
                      BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n\
                      40 0 0 40 20 60 cm\n/Im1 Do\nendstream",
                ),
                (6, &is),
                (
                    7,
                    b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] \
                      /Contents (keep me) >>",
                ),
                (8, &ff2),
                (9, b"<< /Title (doc title) >>"),
            ],
            b"/Info 9 0 R",
        );
        let d = doc(&pdf);
        // /Annots on the target page refuses remove_region — the
        // point is that the audit reports annot presence even when
        // the content edit cannot run.
        let r = crate::remove_region::remove_region(
            &d,
            0,
            &[(20.0, 80.0, 40.0, 100.0)],
            &Limits::default(),
        );
        assert!(r.is_err(), "Annots page is refused");
        // Audit the ORIGINAL — edit refused, nothing rewritten.
        let a = audit_residuals(&d, &Limits::default()).unwrap();
        assert!(has_kind(&a, FindingKind::DocumentInfo));
        assert!(has_kind(&a, FindingKind::AnnotText));
        assert!(has_kind(&a, FindingKind::Image));
        assert!(has_kind(&a, FindingKind::ResourceUsageUnexamined));
        assert!(has_kind(&a, FindingKind::Resources));
    }

    // ---- classification & traversal limits ----------------

    /// A tiny `max_audit_edges` cuts the walk before it reaches the
    /// orphaned object — the orphan must be Unevaluated, never
    /// reported as unreachable.
    #[test]
    fn truncated_walk_reports_unevaluated_not_unreachable() {
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Extra 5 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /Other 6 0 R >>"),
                (6, b"<< /Orphaned true >>"),
                (7, b"<< /Also 8 0 R >>"),
                (8, b"<< /Deep true >>"),
            ],
            b"",
        );
        // Only 3 edges — the walk reaches 5 (edge) then stops: 6
        // stays queued, 7/8 never seen.
        let limits = Limits {
            max_audit_edges: 3,
            ..Default::default()
        };
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        assert!(!a.reachability.walk_complete);
        // Object 6 was queued but never visited → Unevaluated, not
        // unreachable.
        let u6 = a
            .findings
            .iter()
            .find(|f| f.object == Some(IndirectRef::new(6, 0)))
            .expect("finding for object 6");
        assert_eq!(u6.reach, ReachClass::Unevaluated);
        // Objects 5..8 are all unvisited — 5 was queued but never
        // popped, 6/7/8 never even queued. All Unevaluated.
        for n in [5u32, 6, 7, 8] {
            let f = a
                .findings
                .iter()
                .find(|f| f.object == Some(IndirectRef::new(n, 0)))
                .expect("finding");
            assert_eq!(f.reach, ReachClass::Unevaluated);
        }
        assert_eq!(a.reachability.unreachable_live, 0);
        assert_eq!(a.reachability.unevaluated_live, 4);
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.reason.contains("max_audit_edges") && i.limit));
    }

    /// Findings cap is a separate condition from walk completeness.
    #[test]
    fn findings_cap_does_not_mean_walk_incomplete() {
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Resources << /Font << /F1 5 0 R >> >> \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
            ],
            b"",
        );
        let limits = Limits {
            max_audit_findings: 1,
            ..Default::default()
        };
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        // Walk completed — only the findings report was truncated.
        assert!(a.reachability.walk_complete);
        assert_eq!(a.findings.len(), 1);
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope == "findings" && i.limit));
    }

    /// Name-tree cycles terminate: a node's /Kids pointing at itself
    /// and two nodes pointing at each other are both cycles, not
    /// hangs.
    #[test]
    fn name_tree_cycle_and_shared_node() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 5 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // Root: /Names leaf + /Kids [5 0 R 6 0 R 7 0 R].
                (
                    5,
                    b"<< /Names [(a.txt) 9 0 R] \
                      /Kids [5 0 R 6 0 R 7 0 R] >>",
                ),
                // Node 6 ↔ node 7 (mutual cycle). Node 7 also /Kids → 6.
                (6, b"<< /Names [(b.txt) 9 0 R] /Kids [7 0 R] >>"),
                (7, b"<< /Names [(c.txt) 9 0 R] /Kids [6 0 R] >>"),
                (8, b"<< /Names [(d.txt) 9 0 R] >>"),
                (9, b"<< /Type /Filespec /F (x) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // Tree root + one leaf found.
        assert!(has_kind(&a, FindingKind::EmbeddedFiles));
        // The cycles are reported as incomplete, not hung on — a
        // node re-entering the stack is a cycle; a finished node
        // revisited via two parents is a shared node.
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.reason.contains("cycle") || i.reason.contains("shared node")));
        // All four leaves found despite the cycle.
        let leaves = find_kind(&a, FindingKind::FileSpec);
        assert_eq!(leaves.len(), 4);
    }

    /// Wide (shallow) containers are not killed by the depth cap —
    /// depth is *nesting*, not element count.
    #[test]
    fn walk_depth_is_nesting_not_breadth() {
        // One page dict with a 100-element /Annots array (refs).
        let mut items = String::from("[");
        for n in 10..110u32 {
            items.push_str(&format!("{n} 0 R "));
        }
        items.push(']');
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Annots {items} >>"
        );
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        // 100 small annots.
        let annot = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] >>";
        let annots: Vec<(u32, &[u8])> = (10..110u32).map(|n| (n, annot.as_ref())).collect();
        objs.extend_from_slice(&annots);
        let pdf = assemble(&objs, b"");
        let limits = Limits {
            max_audit_walk_depth: 4,
            ..Default::default()
        };
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        // Walk completed — the 100-element array did not trip the
        // depth cap.
        assert!(a.reachability.walk_complete);
        assert_eq!(a.reachability.unevaluated_live, 0);
    }

    /// A genuinely deep container (nested arrays) hits the depth cap
    /// and the walk is reported incomplete — while siblings at
    /// shallower depth are still visited.
    #[test]
    fn walk_depth_caps_nesting_and_reports_incomplete() {
        // /Deep = [[[[...]]]] nested 80 deep, sibling /Shallow = 5 0 R.
        let mut deep = String::new();
        for _ in 0..80 {
            deep.push('[');
        }
        deep.push_str("5 0 R");
        for _ in 0..80 {
            deep.push(']');
        }
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Deep {deep} /Shallow 6 0 R >>"
        );
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, page.as_bytes()),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (5, b"<< /DeepLeaf true >>"),
                (6, b"<< /ShallowLeaf true >>"),
            ],
            b"",
        );
        let limits = Limits {
            max_audit_walk_depth: 8,
            ..Default::default()
        };
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        // Depth stop → walk incomplete.
        assert!(!a.reachability.walk_complete);
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.reason.contains("max_audit_walk_depth")));
        // The shallow sibling was still reached — its edge was
        // followed (the deep branch stopped, not the whole walk).
        assert!(a.reachability.edges_followed >= 3);
        // …and it resolves fine — reachability was real, not a guess.
        let f6 = a
            .findings
            .iter()
            .find(|f| f.object == Some(IndirectRef::new(6, 0)));
        // Object 6 is reachable but its /Type-less dict produces no
        // stream finding — check reachability via the finding it
        // *does* generate when classified, or via referrers.
        assert!(f6.is_none() || f6.unwrap().reach == ReachClass::Reachable);
        // The deep leaf (5) is inside the over-depth branch —
        // Unevaluated, not unreachable.
        let f5 = a
            .findings
            .iter()
            .find(|f| f.object == Some(IndirectRef::new(5, 0)))
            .expect("finding for 5");
        assert_eq!(f5.reach, ReachClass::Unevaluated);
    }

    /// A reference whose target has no xref entry is reported as
    /// `MissingReference` with its referrer path — it is *not*
    /// counted as examined or reachable, and never as unreachable.
    #[test]
    fn missing_reference_is_reported_not_counted() {
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Dangling 99 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // 99 0 R does not exist — reported MissingReference.
        let miss = find_kind(&a, FindingKind::MissingReference);
        assert_eq!(miss.len(), 1);
        assert_eq!(miss[0].object, Some(IndirectRef::new(99, 0)));
        // The referrer path names the containing object and key.
        let rendered: Vec<String> = miss[0]
            .via
            .iter()
            .map(|s| render_path(std::slice::from_ref(s)))
            .collect();
        assert!(
            rendered.iter().any(|s| s.contains("/Dangling")),
            "referrer path should name /Dangling: {:?}",
            miss[0].via
        );
        // Counter semantics: 99 0 R was never examined or reachable.
        assert_eq!(a.reachability.missing_references, 1);
        assert!(a.reachability.walk_complete);
    }

    /// objects_examined counts resolved objects only — a missing
    /// root does not inflate it.
    #[test]
    fn objects_examined_counts_resolved_only() {
        // trailer /Info points at a missing object; /Root is fine.
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"/Info 99 0 R",
        );
        let d = doc(&pdf);
        // The trailer really carries /Info → the ref must resolve to
        // MissingReference (not some other error).
        let info_obj = d.trailer().get(b"Info".as_slice());
        assert!(matches!(info_obj, Some(Object::Ref(_))), "{info_obj:?}");
        let r = info_obj.unwrap().as_ref().unwrap();
        assert!(matches!(
            d.resolve_defined(r),
            Err(Error::MissingReference(_))
        ));
        assert!(
            matches!(d.resolve(r), Ok(Object::Null)),
            "§7.3.10 for readers"
        );
        let a = audit_residuals(&d, &Limits::default()).unwrap();
        // 4 real objects resolved (1,2,3,4); 99 0 R never examined.
        assert_eq!(a.objects_examined, 4);
        assert_eq!(a.reachability.missing_references, 1);
        // A MissingReference finding names the trailer key.
        let miss = find_kind(&a, FindingKind::MissingReference);
        assert_eq!(miss.len(), 1);
        // Info finding is still reported — the trailer *has* /Info
        // even though its target is gone.
        assert!(has_kind(&a, FindingKind::DocumentInfo));
    }

    /// Element budget stops wide traversals separately from the
    /// depth budget.
    #[test]
    fn element_budget_stops_wide_walk() {
        let mut items = String::from("[");
        for n in 10..210u32 {
            items.push_str(&format!("{n} 0 R "));
        }
        items.push(']');
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Annots {items} >>"
        );
        let annot = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] >>";
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let annots: Vec<(u32, &[u8])> = (10..210u32).map(|n| (n, annot.as_ref())).collect();
        objs.extend_from_slice(&annots);
        let pdf = assemble(&objs, b"");
        // 50 elements only — the 200-item array trips the cap.
        let limits = Limits {
            max_audit_elements: 50,
            ..Default::default()
        };
        let a = audit_residuals(&doc(&pdf), &limits).unwrap();
        assert!(!a.reachability.walk_complete);
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.reason.contains("max_audit_elements")));
    }

    /// A page-content parse failure is Incomplete — never reported
    /// as "no ActualText".
    #[test]
    fn unparseable_page_content_is_incomplete() {
        // A /Contents stream whose body does not parse (unbalanced
        // operator) — the audit must record incomplete.
        let body = b"BT /F1 24 Tf (oops\n"; // unterminated string
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, &sb),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("page 1 content")));
    }

    /// A malformed name-tree /Kids ref is recorded, not silently
    /// skipped.
    #[test]
    fn name_tree_missing_kid_is_incomplete() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 5 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // /Kids points at a non-existent 99 0 R.
                (5, b"<< /Names [(a) 9 0 R] /Kids [99 0 R 8 0 R] >>"),
                (8, b"<< /Names [(b) 9 0 R] >>"),
                (9, b"<< /Type /Filespec /F (x) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // The dangling /Kids[0] is reported, not skipped.
        assert!(a.incomplete.iter().any(|i| i.reason.contains("Kids[0]")));
        // The valid leaf is still found.
        assert!(!find_kind(&a, FindingKind::FileSpec).is_empty());
    }

    // ---- incremental scanning & entry failures ------------

    /// Build an `Audit` for a single-object walk — the same state
    /// `audit_residuals` would construct, exposed to tests so the
    /// stack-height probe reads the *production* `walk_object_refs`.
    fn test_audit<'d>(doc: &'d Document, limits: &'d Limits) -> Audit<'d> {
        Audit {
            doc,
            limits,
            out: ResidualAudit::default(),
            edges: 0,
            elements: 0,
            referrers: HashMap::new(),
            reachable: HashSet::new(),
            walk_complete: false,
            findings_seen: 0,
            findings_capped: false,
            max_walk_stack: 0,
        }
    }

    /// A wide container does not expand all its children into frames
    /// before the element budget can stop the walk — the production
    /// walker's stack stays proportional to *depth*, not breadth.
    /// An earlier walker that pushed one frame per child would show
    /// `max_walk_stack` climbing toward the container width.
    #[test]
    fn wide_input_does_not_preallocate_frames() {
        // 200-item array of refs under /Annots.
        let mut items = String::from("[");
        for n in 10..210u32 {
            items.push_str(&format!("{n} 0 R "));
        }
        items.push(']');
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Annots {items} >>"
        );
        let annot = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] >>";
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let annots: Vec<(u32, &[u8])> = (10..210u32).map(|n| (n, annot.as_ref())).collect();
        objs.extend_from_slice(&annots);
        let pdf = assemble(&objs, b"");
        let d = doc(&pdf);
        let limits = Limits::default();
        let mut a = test_audit(&d, &limits);
        let page_obj = d.resolve(IndirectRef::new(3, 0)).unwrap();
        let mut queue = Vec::new();
        a.walk_object_refs(IndirectRef::new(3, 0), &page_obj, &mut queue)
            .unwrap();
        // 202 ref edges queued (200 annots + /Contents + /Parent), but
        // the frame stack never held them all: leaf + array frame
        // ~ a handful, not ~200.
        assert_eq!(queue.len(), 202, "queued refs");
        assert!(
            a.max_walk_stack <= 4,
            "stack grew to {} — frames tracked breadth, not depth",
            a.max_walk_stack
        );
    }

    /// A wide *dictionary* is likewise one lazy frame — children are
    /// not expanded into stack slots up front.
    #[test]
    fn wide_dict_does_not_preallocate_frames() {
        let mut entries = String::new();
        for n in 10..210u32 {
            entries.push_str(&format!("/K{n} {n} 0 R "));
        }
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R {entries} >>"
        );
        let leaf = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] >>";
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let leaves: Vec<(u32, &[u8])> = (10..210u32).map(|n| (n, leaf.as_ref())).collect();
        objs.extend_from_slice(&leaves);
        let pdf = assemble(&objs, b"");
        let d = doc(&pdf);
        let limits = Limits::default();
        let mut a = test_audit(&d, &limits);
        let page_obj = d.resolve(IndirectRef::new(3, 0)).unwrap();
        let mut queue = Vec::new();
        a.walk_object_refs(IndirectRef::new(3, 0), &page_obj, &mut queue)
            .unwrap();
        assert!(
            a.max_walk_stack <= 4,
            "dict walk held {} frames",
            a.max_walk_stack
        );
    }

    /// A deep nest does not blow the stack up to the *element count*:
    /// ~40 elements spread across 20 levels keeps the live frame
    /// count near the open-container count, never ~40 frames at once.
    #[test]
    fn deep_walk_stack_stays_bounded() {
        // [ 8 0 R [8 0 R [8 0 R [...]]]] — ~2 elements per level,
        // 20 levels deep.
        let mut deep = String::from("[8 0 R ");
        for _ in 0..20 {
            deep.push_str("[8 0 R ");
        }
        deep.push_str("8 0 R");
        for _ in 0..20 {
            deep.push(']');
        }
        deep.push(']');
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Deep {deep} >>"
        );
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (3, page.as_bytes()),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (8, b"<< /DeepLeaf true >>"),
            ],
            b"",
        );
        let d = doc(&pdf);
        let limits = Limits::default();
        let mut a = test_audit(&d, &limits);
        let page_obj = d.resolve(IndirectRef::new(3, 0)).unwrap();
        let mut queue = Vec::new();
        a.walk_object_refs(IndirectRef::new(3, 0), &page_obj, &mut queue)
            .unwrap();
        // The walk completed: every nested level was visited.
        assert!(!queue.is_empty(), "inner refs were queued");
        // The stack never held ~40 frames — it stays near the
        // simultaneously-open container count.
        assert!(
            a.max_walk_stack <= 10,
            "deep walk held {} frames — expected ≤ open containers",
            a.max_walk_stack
        );
    }

    /// A small `max_audit_elements` stops the walk mid-container —
    /// the stack never expands the remaining siblings, the audit
    /// reports the walk as incomplete, and unvisited live objects
    /// are Unevaluated, not UnreachableLive.
    #[test]
    fn element_budget_stop_keeps_stack_small_and_incomplete() {
        let mut items = String::from("[");
        for n in 10..110u32 {
            items.push_str(&format!("{n} 0 R "));
        }
        items.push(']');
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R /Annots {items} >>"
        );
        let annot = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] >>";
        let mut objs: Vec<(u32, &[u8])> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, page.as_bytes()),
            (4, b"<< /Length 0 >>\nstream\n\nendstream"),
        ];
        let annots: Vec<(u32, &[u8])> = (10..110u32).map(|n| (n, annot.as_ref())).collect();
        objs.extend_from_slice(&annots);
        let pdf = assemble(&objs, b"");
        let d = doc(&pdf);
        let limits = Limits {
            max_audit_elements: 20,
            ..Default::default()
        };
        let a = audit_residuals(&d, &limits).unwrap();
        assert!(!a.reachability.walk_complete);
        // Unvisited live objects are Unevaluated, never Unreachable.
        assert!(a.reachability.unevaluated_live > 0);
        assert_eq!(a.reachability.unreachable_live, 0);
        // The element cap was reported as a limit incomplete.
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("reachability walk")
                && i.reason.contains("max_audit_elements")));
    }

    /// The name-tree entry points record resolution failures.
    #[test]
    fn name_tree_entry_failures_are_incomplete() {
        // /Names itself is a missing ref; each tree root is missing
        // too. Both must be reported, not silently absent.
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names 99 0 R >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // /Names is a missing ref — recorded with its scope.
        assert!(a.incomplete.iter().any(|i| i.scope == "catalog /Names"));
        // No EmbeddedFiles finding was reported — "not evaluated"
        // is not "none present".
        assert!(!has_kind(&a, FindingKind::EmbeddedFiles));
    }

    /// A tree-root ref that is missing is reported under its own
    /// name, and a *good* sibling tree still reports its leaves.
    #[test]
    fn name_tree_bad_root_does_not_kill_sibling() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 99 0 R \
                               /JavaScript 5 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // JavaScript tree is fine — its leaf must still be
                // found even though EmbeddedFiles' root is missing.
                (5, b"<< /Names [(app.js) 6 0 R] >>"),
                (6, b"<< /Type /Action /S /JavaScript /JS (x) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // EmbeddedFiles root missing → recorded.
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("EmbeddedFiles") && i.reason.contains("does not resolve")));
        // JavaScript tree still produced its finding.
        assert!(has_kind(&a, FindingKind::JavaScript));
    }

    /// `/Names` resolving to a non-dictionary is recorded, not
    /// silently skipped.
    #[test]
    fn names_not_a_dict_is_incomplete() {
        let pdf = assemble(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R /Names [1 2 3] >>"),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope == "catalog /Names" && i.reason.contains("non-dictionary")));
    }

    /// A `/Kids` resolution error carries the key, index and error
    /// kind — not an Unsupported flattening.
    #[test]
    fn name_tree_kid_error_keeps_error_kind() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 5 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                // /Kids [99 0 R] — the ref is dangling, so the walk
                // reports it rather than dropping it.
                (5, b"<< /Names [(a) 9 0 R] /Kids [99 0 R] >>"),
                (9, b"<< /Type /Filespec /F (x) >>"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        // The /Kids[0] dangling ref is a non-limit incomplete.
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("EmbeddedFiles")
                && i.reason.contains("Kids[0]")
                && !i.limit));
        // The valid leaf is still found.
        assert!(!find_kind(&a, FindingKind::FileSpec).is_empty());
    }

    /// `limit` is `true` only when the failure is actually a
    /// `LimitExceeded` — a missing ref is `limit: false`.
    #[test]
    fn name_tree_error_limit_flag() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 99 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        let inc = a
            .incomplete
            .iter()
            .find(|i| i.scope.contains("EmbeddedFiles"))
            .expect("EmbeddedFiles incomplete");
        assert!(!inc.limit, "missing ref is not a limit");
        // No EmbeddedFiles findings were reported — absent is not
        // "no attachments".
        assert!(!has_kind(&a, FindingKind::EmbeddedFiles));
    }

    /// No name trees at all → no name-tree incomplete entries.
    #[test]
    fn absent_name_tree_is_not_incomplete() {
        let a = audit_residuals(&doc(&minimal_pdf()), &Limits::default()).unwrap();
        assert!(!a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("Names") || i.scope.contains("name tree")));
    }

    /// A broken name-tree root produces an `incomplete` record at
    /// the audit level — the CLI text/JSON surface is covered by
    /// `cli_audit_remove_multi.rs::cli_audit_incomplete_*` tests that run the real
    /// binary against the same kind of input.
    #[test]
    fn audit_incomplete_for_broken_name_tree() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 99 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        assert!(a
            .incomplete
            .iter()
            .any(|i| i.scope.contains("EmbeddedFiles")));
    }

    /// The audit must never leak the values it inventories — the
    /// name-tree entry failure messages carry keys and error kinds
    /// only, never the file's text content.
    #[test]
    fn incomplete_reports_do_not_leak_body_text() {
        let pdf = assemble(
            &[
                (
                    1,
                    b"<< /Type /Catalog /Pages 2 0 R \
                      /Names << /EmbeddedFiles 99 0 R >> >>",
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                (
                    3,
                    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                      /Contents 4 0 R /Annots [5 0 R] >>",
                ),
                (4, b"<< /Length 0 >>\nstream\n\nendstream"),
                (
                    5,
                    b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] \
                      /Contents (SECRET-INCOMPLETE-CHECK) >>",
                ),
            ],
            b"",
        );
        let a = audit_residuals(&doc(&pdf), &Limits::default()).unwrap();
        for i in &a.incomplete {
            assert!(
                !i.reason.contains("SECRET-INCOMPLETE-CHECK"),
                "incomplete reason leaked body: {}",
                i.reason
            );
            assert!(!i.scope.contains("SECRET-INCOMPLETE-CHECK"));
        }
        // And the annot's Contents is still reported as *present*,
        // never as its value.
        assert!(has_kind(&a, FindingKind::AnnotText));
    }

    #[test]
    fn input_bytes_and_document_unchanged() {
        let bytes = minimal_pdf();
        let before = bytes.clone();
        let d = doc(&bytes);
        let _ = audit_residuals(&d, &Limits::default()).unwrap();
        assert_eq!(bytes, before, "input bytes must not change");
        // A second audit on the same Document returns identically.
        let a2 = audit_residuals(&d, &Limits::default()).unwrap();
        let a1 = audit_residuals(&d, &Limits::default()).unwrap();
        assert_eq!(a1.findings.len(), a2.findings.len());
    }
}
