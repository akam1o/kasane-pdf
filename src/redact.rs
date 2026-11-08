//! Redaction: remove what is drawn inside page rectangles, and check
//! on the written bytes that it is gone.
//!
//! [`redact`] builds on the region editor (`remove_regions`). Its
//! result makes three separate statements — do not read one as
//! another:
//!
//! 1. **The rects (verified).** Text codes, image samples and painted
//!    paths inside the rects are removed (paths crossing a rect's edge
//!    are cut there), never painted over. Checked on the output: the
//!    same rects find nothing left to remove.
//! 2. **The edited originals (verified, with stated exceptions).** No
//!    live object of the output — every stream, stored or decoded —
//!    equals an edited image's or form's original, except an object
//!    the output still invokes — a `Do` on a page, in an annotation's
//!    appearance, or transitively in a form those draw (another page,
//!    the same page outside the rects): those invocations, visible or
//!    hidden by a clip, are listed in [`Redaction::elsewhere`].
//!    Names of an original on pages that do not draw it are removed.
//!    The output is a single revision (no incremental history, no
//!    unreachable objects carried). Equal digests are the test: the
//!    same picture re-encoded differently is *not* detected here.
//! 3. **The rest of the document (reported, not guaranteed).**
//!    [`Redaction::elsewhere`] lists where removed text (runs of four
//!    or more characters) is found again — other pages' extracted
//!    text, objects' strings — and originals still drawn;
//!    [`Redaction::residual`] audits what else the output carries
//!    (attachments, annotations, form values, actions …). Not finding
//!    text by extraction does not mean it is absent (text in images or
//!    fonts, other encodings). [`RedactOptions::refuse_copies_elsewhere`]
//!    turns any finding into a refusal.
//!
//! Always: `/Info` and catalog XMP are removed; a structure tree with
//! alternate text (`/Alt`, `/ActualText`, `/E`) is refused unless the
//! caller asks for it to be removed. Everything the editor refuses
//! stays refused: a structure that cannot be processed exactly is an
//! error, never a best-effort result, and a failed check returns no
//! PDF.

use crate::audit::{audit_residuals, ResidualAudit};
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::redact_diag::Site;
use crate::remove_region::{regions_edit_session_with, sha256, PageSpec, SessionMode};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Options for [`redact`].
#[derive(Debug, Clone, Default)]
pub struct RedactOptions {
    limits: Limits,
    remove_structure_tree: bool,
    refuse_copies_elsewhere: bool,
    remove_annotations: bool,
    embed_substitute_fonts: bool,
}

impl RedactOptions {
    /// Default limits; a structure tree with alternate text refuses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resource limits for the whole call (editing, writing and
    /// verification).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Remove the document's structure tree (`/StructTreeRoot`,
    /// `/MarkInfo`) instead of refusing one that carries alternate
    /// text. The output is then untagged.
    pub fn remove_structure_tree(mut self, remove: bool) -> Self {
        self.remove_structure_tree = remove;
        self
    }

    /// Refuse instead of reporting when the removed content is found
    /// again in the output ([`Redaction::elsewhere`] not empty) or a
    /// page could not be searched for it. This narrows the result to
    /// "not found elsewhere by these searches" — still not a proof of
    /// erasure: text inside images, fonts or differently encoded is
    /// not searched.
    pub fn refuse_copies_elsewhere(mut self, refuse: bool) -> Self {
        self.refuse_copies_elsewhere = refuse;
        self
    }

    /// Remove a markup annotation (Text, FreeText, Line, Square,
    /// Circle, Polygon, PolyLine, Highlight, Underline, Squiggly,
    /// StrikeOut, Caret, Stamp, Ink, Popup) whose `/Rect` meets a rect
    /// instead of refusing it. The annotation is removed **whole** —
    /// its appearance and text (`/Contents`, `/T`, `/RC`, `/Subj`) go
    /// even where they lie outside the rect — together with what
    /// belongs to it on the page: its popup, the annotation a removed
    /// popup belongs to, and replies to it (`/IRT`), transitively.
    /// Other subtypes meeting a rect (widgets, signatures, media,
    /// file attachments …) stay refused. Its appearance streams may not
    /// stay in the output unless something still draws them (checked
    /// like an edited image's original), and its text is searched for
    /// in `elsewhere`. One the structure tree refers to is refused
    /// unless the tree is removed.
    pub fn remove_annotations(mut self, remove: bool) -> Self {
        self.remove_annotations = remove;
        self
    }

    /// Before redacting, replace each non-embedded Latin Standard 14
    /// font (Times, Helvetica, Courier and their metric-compatible
    /// aliases such as Arial) — document-wide — by a TrueType font
    /// embedding the bundled substitute (Liberation), with every code's
    /// glyph name and advance written out, so that every reader draws
    /// exactly the embedded glyphs at the same positions. The glyphs'
    /// extent is then known and such text is redacted like any embedded
    /// font's; without this option it is refused, because a reader
    /// draws it with a font of its own choosing.
    ///
    /// The output then shows Liberation's glyph shapes wherever those
    /// fonts are used (metric-compatible: the same advances, slightly
    /// different shapes) and is larger by the embedded programs (about
    /// 0.3–0.4 MB per face). Symbol, ZapfDingbats, other non-embedded
    /// fonts and CID fonts are not embedded and stay refused.
    /// [`Redaction::fonts_embedded`] counts the fonts replaced.
    pub fn embed_substitute_fonts(mut self, embed: bool) -> Self {
        self.embed_substitute_fonts = embed;
        self
    }
}

/// The result of [`redact`]: a verified single-revision PDF.
pub struct Redaction {
    /// The redacted PDF.
    pub output: Vec<u8>,
    /// Non-embedded fonts replaced by their embedded substitutes before
    /// redacting ([`RedactOptions::embed_substitute_fonts`]).
    pub fonts_embedded: u64,
    /// Character codes removed.
    pub codes_removed: u64,
    /// Image invocations whose samples inside the rects were zeroed.
    pub images_edited: u64,
    /// Form invocations whose text or paths inside the rects were
    /// removed — each now draws its own edited copy of the form.
    pub forms_edited: u64,
    /// Image pixels set to black.
    pub pixels_changed: u64,
    /// Painted paths removed (each wholly inside a rect).
    pub paths_removed: u64,
    /// Painted paths crossing a rect's edge, cut there: what they
    /// painted inside the rect is gone, the rest is kept.
    pub paths_cut: u64,
    /// `/Link` annotations removed because their `/Rect` met a rect
    /// (with their action or destination); links outside are kept.
    pub links_removed: u64,
    /// Annotations other than links removed whole
    /// ([`RedactOptions::remove_annotations`]), with the popups and
    /// replies tied to them.
    pub annotations_removed: u64,
    /// `/XObject` names of edited images' originals removed from
    /// pages that did not draw them.
    pub original_names_removed: u64,
    /// `(page_index, sites)` — what was edited on each page.
    pub per_page: Vec<(usize, Vec<Site>)>,
    /// The input had a trailer `/Info` (removed).
    pub info_removed: bool,
    /// The input had a catalog `/Metadata` (removed).
    pub metadata_removed: bool,
    /// The input's structure tree was removed (only with
    /// [`RedactOptions::remove_structure_tree`]).
    pub structure_tree_removed: bool,
    /// What was removed from the rects that the output still shows or
    /// stores elsewhere: removed text (runs of at least four
    /// non-whitespace characters) in another page's text or an
    /// object's strings (an outline title, an annotation's contents …),
    /// and edited images whose original another page — or the same
    /// page outside the rects — still draws. The redaction is
    /// positional; these are for the caller to decide on, or to refuse
    /// with [`RedactOptions::refuse_copies_elsewhere`].
    pub elsewhere: Vec<Elsewhere>,
    /// Pages whose text could not be extracted for that search (an
    /// unsupported construct or a limit): not searched, not clean.
    pub elsewhere_unsearched_pages: Vec<usize>,
    /// The residual-information audit of the *output*: annotations,
    /// attachments, form values, actions, outlines' targets … that
    /// the positional redaction does not touch. Review before
    /// distributing.
    pub residual: ResidualAudit,
}

/// Where removed text was found again in the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElsewhereLocation {
    /// In the extracted text of a page (0-based index).
    Page(usize),
    /// In a string of a (non-stream) object: an outline title, an
    /// annotation's `/Contents`, a field value …
    Object(crate::object::IndirectRef),
}

/// What of the removed content a finding of [`Redaction::elsewhere`]
/// is about.
#[derive(Clone, PartialEq, Eq)]
pub enum Removed {
    /// Removed text, found again.
    Text(String),
    /// An image edited in the rects whose original is still drawn
    /// unredacted at the location ([`ElsewhereLocation::Page`]).
    Image,
    /// A form XObject edited in the rects (its text or paths) whose
    /// original is still drawn unredacted at the location.
    Form,
}

impl std::fmt::Debug for Removed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Removed::Text(t) => write!(f, "Text({} chars)", t.chars().count()),
            Removed::Image => f.write_str("Image"),
            Removed::Form => f.write_str("Form"),
        }
    }
}

/// One finding of [`Redaction::elsewhere`].
#[derive(Debug, Clone)]
pub struct Elsewhere {
    /// What was found again.
    pub removed: Removed,
    /// Where.
    pub location: ElsewhereLocation,
}

impl std::fmt::Debug for Redaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redaction")
            .field("output_bytes", &self.output.len())
            .field("fonts_embedded", &self.fonts_embedded)
            .field("codes_removed", &self.codes_removed)
            .field("images_edited", &self.images_edited)
            .field("forms_edited", &self.forms_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("paths_removed", &self.paths_removed)
            .field("paths_cut", &self.paths_cut)
            .field("links_removed", &self.links_removed)
            .field("annotations_removed", &self.annotations_removed)
            .field("original_names_removed", &self.original_names_removed)
            .field("info_removed", &self.info_removed)
            .field("metadata_removed", &self.metadata_removed)
            .field("structure_tree_removed", &self.structure_tree_removed)
            .field("elsewhere", &self.elsewhere)
            .field("residual_findings", &self.residual.findings.len())
            .finish()
    }
}

/// Redact the rects of each `(page_index, rects)` spec (0-based pages,
/// rects in the page's default user space, `(llx, lly, urx, ury)`) and
/// return the verified output. See the module documentation for what
/// is removed, what is refused and what is checked.
///
/// On any refusal, limit or failed check the call returns an error and
/// no PDF.
pub fn redact(
    doc: &Document,
    specs: &[PageSpec],
    options: &RedactOptions,
) -> Result<Redaction, Error> {
    options.limits.validate()?;
    if options.embed_substitute_fonts {
        if let Some((bytes, n)) = crate::redact_fonts::embed_substitutes(doc, &options.limits)? {
            let normalized = Document::from_bytes_with_limits(&bytes, options.limits.clone())?;
            return redact_doc(&normalized, specs, options, n);
        }
    }
    redact_doc(doc, specs, options, 0)
}

fn redact_doc(
    doc: &Document,
    specs: &[PageSpec],
    options: &RedactOptions,
    fonts_embedded: u64,
) -> Result<Redaction, Error> {
    let limits = &options.limits;
    let info_present = doc.trailer().contains_key(b"Info".as_slice());
    let metadata_present = doc.catalog().contains_key(b"Metadata".as_slice());

    let tree = doc.catalog().contains_key(b"StructTreeRoot".as_slice());
    if tree && !options.remove_structure_tree {
        if let Some(key) = structure_alternate_text(doc, limits)? {
            return Err(Error::Unsupported {
                feature: "redaction of a document whose structure tree has alternate text",
                detail: format!(
                    "a structure element carries /{key}, which may repeat redacted \
                     content; RedactOptions::remove_structure_tree removes the tree"
                ),
            });
        }
    }

    let session = crate::remove_region::SessionOptions {
        remove_annotations: options.remove_annotations,
    };
    let mut s = regions_edit_session_with(doc, specs, limits, SessionMode::Redact, session)?;
    let structure_tree_removed = tree && options.remove_structure_tree;
    if tree && !structure_tree_removed && s.tagged_links_removed_total > 0 {
        return Err(Error::Unsupported {
            feature: "redaction of an annotation the structure tree refers to",
            detail: "the tree's /OBJR would keep the removed link or annotation \
                     reachable; RedactOptions::remove_structure_tree removes the tree"
                .into(),
        });
    }
    if structure_tree_removed {
        let root = match doc.trailer().get(b"Root".as_slice()) {
            Some(Object::Ref(r)) => *r,
            _ => {
                return Err(Error::Unsupported {
                    feature: "structure tree removal with a direct catalog",
                    detail: "the trailer /Root is not a reference".into(),
                })
            }
        };
        let mut catalog: Dict = match s.edit.overlay.get(&(root.number, root.generation)) {
            Some(Object::Dict(d)) => d.clone(),
            _ => doc.catalog().clone(),
        };
        catalog.remove(b"StructTreeRoot".as_slice());
        catalog.remove(b"MarkInfo".as_slice());
        s.edit
            .overlay
            .insert((root.number, root.generation), Object::Dict(catalog));
    }
    let output = s
        .edit
        .rewrite_with_overrides(&crate::rewrite::RewriteOverrides {
            strip_info: true,
            strip_metadata: true,
        })?;

    // Type 3 glyph definitions the output no longer shows go too.
    let (output, glyph_images) = match crate::redact_type3::prune_type3(doc, &output, limits)? {
        Some(p) => (p.output, p.images),
        None => (output, Vec::new()),
    };

    // An original may stay only as an object the output still draws
    // (from a page, through forms); each such display is reported.
    // The images only removed Type 3 glyphs drew are checked like
    // edited originals: gone, or still drawn (reported).
    let mut originals = s.original_digests.clone();
    originals.extend(glyph_images);
    let (out, residual, shown) = verify(
        &output,
        specs,
        limits,
        &originals,
        &s.removed_annot_prints,
        session,
    )?;
    // No Type 3 glyph the input showed and the output does not is
    // still defined.
    if crate::redact_type3::prune_type3(doc, &output, limits)?.is_some() {
        return Err(Error::Unsupported {
            feature: "redaction output failed verification",
            detail: "a Type 3 font still defines a glyph the output no longer shows".into(),
        });
    }
    let (mut elsewhere, elsewhere_unsearched_pages) =
        find_elsewhere(&out, &s.removed_text, limits)?;
    elsewhere.extend(shown.into_iter().map(|(p, form)| Elsewhere {
        removed: if form { Removed::Form } else { Removed::Image },
        location: ElsewhereLocation::Page(p),
    }));
    if options.refuse_copies_elsewhere
        && (!elsewhere.is_empty() || !elsewhere_unsearched_pages.is_empty())
    {
        return Err(Error::Unsupported {
            feature: "redaction: removed content remains elsewhere",
            detail: format!(
                "{} finding(s) of removed text or images elsewhere, {} page(s) not searched",
                elsewhere.len(),
                elsewhere_unsearched_pages.len()
            ),
        });
    }

    Ok(Redaction {
        output,
        fonts_embedded,
        codes_removed: s.codes_removed_total,
        images_edited: s.invocations_edited_total,
        forms_edited: s.forms_edited_total,
        pixels_changed: s.pixels_changed_total,
        paths_removed: s.paths_removed_total,
        paths_cut: s.paths_cut_total,
        links_removed: s.links_removed_total,
        annotations_removed: s.annots_removed_total,
        original_names_removed: s.names_pruned_total,
        per_page: s.per_page,
        info_removed: info_present,
        metadata_removed: metadata_present,
        structure_tree_removed,
        elsewhere,
        elsewhere_unsearched_pages,
        residual,
    })
}

/// Check the written bytes; on success return the output's residual
/// audit.
/// Pages still drawing an original, and whether it is a form.
type Shown = BTreeSet<(usize, bool)>;

fn verify(
    output: &[u8],
    specs: &[PageSpec],
    limits: &Limits,
    originals: &[[u8; 32]],
    removed_annots: &[[u8; 32]],
    session: crate::remove_region::SessionOptions,
) -> Result<(Document, ResidualAudit, Shown), Error> {
    let fail = |detail: String| Error::Unsupported {
        feature: "redaction output failed verification",
        detail,
    };
    let out = Document::from_bytes_with_limits(output, limits.clone())?;
    if out.xref_section_offsets().len() != 1 {
        return Err(fail("the output has more than one revision".into()));
    }
    // The same rects find nothing left to remove.
    let again = regions_edit_session_with(&out, specs, limits, SessionMode::Redact, session)?;
    if again.codes_removed_total
        + again.pixels_changed_total
        + again.paths_removed_total
        + again.paths_cut_total
        + again.links_removed_total
        + again.annots_removed_total
        != 0
    {
        return Err(fail(format!(
            "the rects still hit {} codes, {} nonzero pixels, {} paths, {} paths to cut, {} links, \
             {} annotations",
            again.codes_removed_total,
            again.pixels_changed_total,
            again.paths_removed_total,
            again.paths_cut_total,
            again.links_removed_total,
            again.annots_removed_total
        )));
    }
    drop(again);
    // No removed link or annotation is still in the output, whatever
    // refers to it (an action, another annotation, the structure tree,
    // an outline …): every dictionary written is compared.
    if !removed_annots.is_empty() {
        let prints: HashSet<[u8; 32]> = removed_annots.iter().copied().collect();
        // Every nested dictionary, at any depth (iteratively).
        fn find(o: &Object, prints: &HashSet<[u8; 32]>) -> bool {
            let mut stack = vec![o];
            while let Some(o) = stack.pop() {
                match o {
                    Object::Dict(d) => {
                        if prints.contains(&crate::remove_region::annotation_print(d)) {
                            return true;
                        }
                        stack.extend(d.values());
                    }
                    Object::Array(a) => stack.extend(a.iter()),
                    Object::Stream { dict, .. } => stack.extend(dict.values()),
                    _ => {}
                }
            }
            false
        }
        let refs: Vec<_> = out
            .xref_entries()
            .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
            .map(|(r, _)| r)
            .collect();
        for r in refs {
            if find(&out.resolve(r)?, &prints) {
                return Err(fail(format!(
                    "object {r} still holds a removed link or annotation (something refers to it)"
                )));
            }
        }
    }
    // No stream equals an original, stored or decoded — except an
    // object the output still draws: a display elsewhere, reported.
    let originals: HashSet<[u8; 32]> = originals.iter().copied().collect();
    let mut shown: BTreeSet<(usize, bool)> = BTreeSet::new();
    if !originals.is_empty() {
        let drawn = drawn_xobjects(&out, limits)?;
        let refs: Vec<_> = out
            .xref_entries()
            .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
            .map(|(r, _)| r)
            .collect();
        for r in refs {
            let obj = out.resolve(r)?;
            let Object::Stream { dict, data } = &obj else {
                continue;
            };
            let stored = sha256(data);
            let decoded = out.decode_stream(dict, data).ok().map(|d| sha256(&d));
            if !(originals.contains(&stored) || decoded.is_some_and(|d| originals.contains(&d))) {
                continue;
            }
            let Some(pages) = drawn.get(&r) else {
                return Err(fail(format!(
                    "object {r} holds an edited image's or form's original data"
                )));
            };
            let form = dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Form");
            shown.extend(pages.iter().map(|p| (*p, form)));
        }
    }
    let residual = audit_residuals(&out, limits)?;
    Ok((out, residual, shown))
}

/// Every XObject a page of `out` invokes — by a `Do` in its content or
/// in an annotation's appearance stream (`/AP`, every state), or
/// transitively in the forms those draw — with the pages invoking it.
/// Names resolve as a reader resolves them: against the form's own
/// `/Resources`, else the resources it inherits. An invocation counts
/// whether or not a clip or `/BBox` hides what it draws: this is what
/// the output still *uses*, not what is visible.
fn drawn_xobjects(
    out: &Document,
    limits: &Limits,
) -> Result<HashMap<IndirectRef, BTreeSet<usize>>, Error> {
    type Frame = (
        Vec<crate::content::Operation>,
        Object,
        Option<IndirectRef>,
        usize,
    );
    // Enter form or image `r`, invoked under `res` (owned by `owner`,
    // None for the page) at `depth`: record it, and return a form's
    // content to walk — once per resources it resolves against.
    let enter = |r: IndirectRef,
                 res: &Object,
                 owner: Option<IndirectRef>,
                 depth: usize,
                 page: usize,
                 drawn: &mut HashMap<IndirectRef, BTreeSet<usize>>,
                 seen: &mut HashSet<(IndirectRef, Option<IndirectRef>)>|
     -> Result<Option<Frame>, Error> {
        drawn.entry(r).or_default().insert(page);
        let obj = out.resolve(r)?;
        let Object::Stream { dict, data } = &obj else {
            return Ok(None);
        };
        if dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Image") {
            return Ok(None);
        }
        let own = dict
            .get(b"Resources".as_slice())
            .filter(|o| !matches!(o, Object::Null))
            .cloned();
        if !seen.insert((r, if own.is_some() { None } else { owner })) {
            return Ok(None);
        }
        if depth + 1 > limits.max_xobject_depth as usize {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                max: limits.max_xobject_depth as u64,
            });
        }
        let decoded = out.decode_stream(dict, data)?;
        let mut warnings = Vec::new();
        let ops = crate::content::parse_content_stream(
            &[decoded],
            limits,
            out.recovery_mode(),
            &mut warnings,
        )?;
        Ok(Some(match own {
            Some(own) => (ops, own, Some(r), depth + 1),
            None => (ops, res.clone(), owner, depth + 1),
        }))
    };
    let mut drawn: HashMap<IndirectRef, BTreeSet<usize>> = HashMap::new();
    for (i, page) in out.pages().iter().enumerate() {
        let res = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Dict::new()));
        let mut seen: HashSet<(IndirectRef, Option<IndirectRef>)> = HashSet::new();
        let mut stack: Vec<Frame> = vec![(out.page_operations(page)?, res.clone(), None, 0)];
        // Annotation appearances: their own resources, else none.
        for r in appearance_streams(out, page)? {
            let empty = Object::Dict(Dict::new());
            if let Some(f) = enter(r, &empty, Some(r), 0, i, &mut drawn, &mut seen)? {
                stack.push(f);
            }
        }
        while let Some((ops, res, owner, depth)) = stack.pop() {
            for op in &ops {
                let crate::content::Operation::Do(name) = op else {
                    continue;
                };
                let Some(Object::Ref(r)) = out.resolve_resource_raw(&res, "XObject", name)? else {
                    continue;
                };
                if let Some(f) = enter(r, &res, owner, depth, i, &mut drawn, &mut seen)? {
                    stack.push(f);
                }
            }
        }
    }
    Ok(drawn)
}

/// The appearance streams (`/AP` `/N`, `/R`, `/D`, each a stream or a
/// dictionary of states) of a page's annotations.
pub(crate) fn appearance_streams(
    out: &Document,
    page: &crate::page::Page,
) -> Result<Vec<IndirectRef>, Error> {
    let mut refs = Vec::new();
    let Some(annots) = crate::redact_diag::page_dict_entry(out, page, b"Annots")? else {
        return Ok(refs);
    };
    let annots = out.deref(&annots)?;
    for a in annots.as_array().unwrap_or(&[]) {
        let a = out.deref(a)?;
        let Some(ap) = a.as_dict().and_then(|d| d.get(b"AP".as_slice())) else {
            continue;
        };
        let ap = out.deref(ap)?;
        let Some(ap) = ap.as_dict() else { continue };
        // Only the defined appearances; readers ignore other keys.
        for v in [b"N".as_slice(), b"R", b"D"]
            .iter()
            .filter_map(|k| ap.get(*k))
        {
            match v {
                Object::Ref(r) => match out.resolve(*r)? {
                    Object::Dict(states) => refs.extend(states.values().filter_map(|s| match s {
                        Object::Ref(r) => Some(*r),
                        _ => None,
                    })),
                    _ => refs.push(*r),
                },
                Object::Dict(states) => refs.extend(states.values().filter_map(|s| match s {
                    Object::Ref(r) => Some(*r),
                    _ => None,
                })),
                _ => {}
            }
        }
    }
    Ok(refs)
}

/// Where the removed `texts` (runs of four or more non-whitespace
/// characters) appear in `out`: each page's extracted text, and the
/// text strings of every live non-stream object.
fn find_elsewhere(
    out: &Document,
    texts: &[String],
    limits: &Limits,
) -> Result<(Vec<Elsewhere>, Vec<usize>), Error> {
    let mut texts: Vec<&str> = texts
        .iter()
        .map(|t| t.trim())
        .filter(|t| t.chars().filter(|c| !c.is_whitespace()).count() >= 4)
        .collect();
    texts.sort_unstable();
    texts.dedup();
    let mut found = Vec::new();
    let mut unsearched = Vec::new();
    if texts.is_empty() {
        return Ok((found, unsearched));
    }
    let opts = crate::text_extract::TextOptions::default().with_limits(limits.clone());
    for (i, page) in out.pages().iter().enumerate() {
        let shown: String = match crate::text_extract::extract_text(out, page, &opts) {
            Ok(t) => t
                .chars
                .iter()
                .filter_map(|c| c.unicode.as_deref())
                .collect(),
            Err(_) => {
                unsearched.push(i);
                continue;
            }
        };
        for t in &texts {
            if shown.contains(t) {
                found.push(Elsewhere {
                    removed: Removed::Text(t.to_string()),
                    location: ElsewhereLocation::Page(i),
                });
            }
        }
    }
    let refs: Vec<_> = out
        .xref_entries()
        .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
        .map(|(r, _)| r)
        .collect();
    for r in refs {
        let obj = out.resolve(r)?;
        if matches!(obj, Object::Stream { .. }) {
            continue;
        }
        let mut strings = Vec::new();
        collect_strings(&obj, &mut strings, 0);
        for t in &texts {
            if strings.iter().any(|s| s.contains(t)) {
                found.push(Elsewhere {
                    removed: Removed::Text(t.to_string()),
                    location: ElsewhereLocation::Object(r),
                });
            }
        }
    }
    Ok((found, unsearched))
}

/// Decoded text strings in `o` (direct values only; references are
/// separate objects), down to a fixed depth.
fn collect_strings(o: &Object, out: &mut Vec<String>, depth: u32) {
    if depth > 64 {
        return;
    }
    match o {
        Object::String(b) => out.push(crate::annot_text::text_string(b)),
        Object::Array(a) => a.iter().for_each(|v| collect_strings(v, out, depth + 1)),
        Object::Dict(d) => d.values().for_each(|v| collect_strings(v, out, depth + 1)),
        _ => {}
    }
}

/// The first alternate-text key (`Alt`, `ActualText`, `E`) found in the
/// structure tree, walking at most `max_audit_elements` containers.
fn structure_alternate_text(
    doc: &Document,
    limits: &Limits,
) -> Result<Option<&'static str>, Error> {
    let Some(root) = doc.catalog().get(b"StructTreeRoot".as_slice()) else {
        return Ok(None);
    };
    let mut stack = vec![root.clone()];
    let mut seen = HashSet::new();
    let mut budget = limits.max_audit_elements;
    while let Some(o) = stack.pop() {
        if budget == 0 {
            return Err(Error::LimitExceeded {
                limit: "max_audit_elements",
                max: limits.max_audit_elements,
            });
        }
        budget -= 1;
        let o = match o {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                doc.resolve(r)?
            }
            o => o,
        };
        match o {
            Object::Dict(d) => {
                for key in ["Alt", "ActualText", "E"] {
                    if d.contains_key(key.as_bytes()) {
                        return Ok(Some(key));
                    }
                }
                // Descend through the tree's own links only.
                for k in [b"K".as_slice(), b"ParentTree", b"Nums", b"Kids"] {
                    if let Some(v) = d.get(k) {
                        stack.push(v.clone());
                    }
                }
            }
            Object::Array(a) => stack.extend(a),
            _ => {}
        }
    }
    Ok(None)
}

/// Corpus analysis aid, not a verdict: every blocker and uncertain item
/// the redaction-mode walk finds on `page_index` for `rects` (as
/// `(kind, what)` pairs, `kind` "blocker" or "uncertain"), where
/// [`redact`] refuses on the first. Other refusals (annotations,
/// images, the structure tree …) are not listed. With `embed` the
/// substitutes are embedded first, as with
/// [`RedactOptions::embed_substitute_fonts`].
#[doc(hidden)]
pub fn redaction_walk_findings(
    doc: &Document,
    page_index: usize,
    rects: &[(f64, f64, f64, f64)],
    limits: &Limits,
    embed: bool,
) -> Result<Vec<(&'static str, String)>, Error> {
    if embed {
        if let Some((bytes, _)) = crate::redact_fonts::embed_substitutes(doc, limits)? {
            let normalized = Document::from_bytes_with_limits(&bytes, limits.clone())?;
            return redaction_walk_findings(&normalized, page_index, rects, limits, false);
        }
    }
    let pages = doc.pages();
    let page = pages
        .get(page_index)
        .ok_or_else(|| Error::Structure(format!("no page {page_index}")))?;
    let mut validated = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        validated.push(crate::redact_diag::check_rect(i, r.0, r.1, r.2, r.3)?);
    }
    let ops = doc.page_operations(page)?;
    let mut walker = crate::redact_diag::DiagWalker::new(doc, page, limits, &validated);
    walker.redaction_mode = true;
    walker.run(&ops)?;
    let mut out: Vec<(&'static str, String)> = walker
        .blockers
        .iter()
        .map(|b| ("blocker", format!("{}: {}", b.what, b.detail)))
        .collect();
    out.extend(
        walker
            .uncertain
            .iter()
            .map(|u| ("uncertain", u.reason.clone())),
    );
    Ok(out)
}
