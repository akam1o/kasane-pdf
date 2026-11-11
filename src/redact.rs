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
//! Always: `/Info` and catalog XMP are removed. A structure tree is
//! kept and edited: the elements whose content the edit touched — and
//! their ancestors — lose their text (`/Alt`, `/ActualText`, `/E`,
//! `/T`, `/Phoneme`, attribute strings) and associated files, and object references to removed annotations or edited
//! originals leave it (`redact_struct`); or the caller asks for the
//! whole tree to be removed. Everything the editor refuses
//! stays refused: a structure that cannot be processed exactly is an
//! error, never a best-effort result, and a failed check returns no
//! PDF.

use crate::audit::{audit_residuals, ResidualAudit};
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::redact_diag::Site;
use crate::remove_region::{regions_edit_session_with, PageSpec, SessionMode};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Options for [`redact`].
#[derive(Debug, Clone, Default)]
pub struct RedactOptions {
    limits: Limits,
    remove_structure_tree: bool,
    refuse_copies_elsewhere: bool,
    remove_annotations: bool,
    embed_substitute_fonts: bool,
    remove_javascript: bool,
    refuse_uninspected: bool,
}

impl RedactOptions {
    /// Default limits; the structure tree is kept and edited.
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
    /// `/MarkInfo`) instead of editing it. The output is then untagged.
    /// By default the tree is kept: the elements whose content the
    /// edit touched, and their ancestors, lose their alternate text
    /// (see [`Redaction::structure_texts_removed`]).
    pub fn remove_structure_tree(mut self, remove: bool) -> Self {
        self.remove_structure_tree = remove;
        self
    }

    /// Remove every JavaScript action — `/Names /JavaScript`, an
    /// `/OpenAction`, `/AA` triggers, annotation and field `/A` actions,
    /// steps of an action chain — instead of refusing a document whose
    /// catalog runs scripts. A script may hold any of the document's
    /// text and is not redacted, so it is removed whole: the output's
    /// forms no longer calculate, validate or format, and nothing runs
    /// when it opens. Verification checks that no JavaScript is left.
    pub fn remove_javascript(mut self, remove: bool) -> Self {
        self.remove_javascript = remove;
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

    /// Refuse when the output carries data `redact` does not read
    /// ([`Redaction::uninspected`] lists any): attachments, XMP
    /// metadata other than the catalog's, thumbnails of pages not
    /// edited, scripts, XFA, rich text, multimedia, application data,
    /// forms nothing draws, streams under keys no modelled structure
    /// names, unknown entries of the catalog or a page — or when that
    /// inventory could not be completed. Images, fonts and other data
    /// in a known role are not refused (they are interpreted: not
    /// searched, and — but for forms and images — not checked to be
    /// used). This narrows a success to "every stream carried is
    /// reached in a role whose format `redact` reads or interprets"; it
    /// is still not a proof that no copy of the removed content remains.
    pub fn refuse_uninspected(mut self, refuse: bool) -> Self {
        self.refuse_uninspected = refuse;
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
    /// A widget meeting a rect is removed too, and its form field with
    /// it when no widget of the field is left (`Redaction::fields_removed`;
    /// a signature field, an XFA form or a field tree that cannot be
    /// edited exactly is refused). Other subtypes meeting a rect
    /// (signatures, media, file attachments …) stay refused. Its appearance streams may not
    /// stay in the output unless something still draws them (checked
    /// like an edited image's original), and its text is searched for
    /// in `elsewhere`. The structure tree's reference to it goes, and
    /// the element holding that reference loses its alternate text.
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
#[non_exhaustive]
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
    /// Shadings (`sh`) removed, each painting only inside a rect; the
    /// shading resources only they used are not carried.
    pub shadings_removed: u64,
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
    /// Text entries (`/Alt`, `/ActualText`, `/E`, `/T`, `/Phoneme`,
    /// attribute strings) removed from
    /// structure elements whose content the edit touched, or whose
    /// descendants' content it touched. The tree is otherwise kept.
    pub structure_texts_removed: u64,
    /// Content items removed from structure elements' `/K`: object
    /// references (`/OBJR`) to removed annotations and edited
    /// originals, marked-content references (`/MCR`) into edited
    /// forms' originals.
    pub structure_items_removed: u64,
    /// JavaScript actions and script trees removed
    /// ([`RedactOptions::remove_javascript`]).
    pub javascript_removed: u64,
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
    /// The output's data by what `redact` does with it: searched for
    /// copies of the removed text, interpreted, or carried uninspected
    /// (listed). See [`crate::uninspected_data`] and
    /// [`RedactOptions::refuse_uninspected`].
    pub uninspected: crate::redact_inventory::DataInventory,
    /// Thumbnails (`/Thumb`) removed from edited pages: they picture
    /// the page as it was.
    pub thumbnails_removed: u64,
    /// Form fields removed with the widgets `remove_annotations` removed
    /// (a field all of whose widgets are gone goes, its value with it).
    pub fields_removed: u64,
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
            .field("shadings_removed", &self.shadings_removed)
            .field("links_removed", &self.links_removed)
            .field("annotations_removed", &self.annotations_removed)
            .field("original_names_removed", &self.original_names_removed)
            .field("info_removed", &self.info_removed)
            .field("metadata_removed", &self.metadata_removed)
            .field("structure_tree_removed", &self.structure_tree_removed)
            .field("structure_texts_removed", &self.structure_texts_removed)
            .field("structure_items_removed", &self.structure_items_removed)
            .field("javascript_removed", &self.javascript_removed)
            .field("elsewhere", &self.elsewhere)
            .field("uninspected", &self.uninspected.uninspected)
            .field("thumbnails_removed", &self.thumbnails_removed)
            .field("fields_removed", &self.fields_removed)
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
        if let Some((bytes, n)) =
            crate::redact_fonts::embed_substitutes(doc, &options.limits, options.remove_javascript)?
        {
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
    let _fonts = doc.font_cache_scope();
    let info_present = doc.trailer().contains_key(b"Info".as_slice());
    let metadata_present = doc.catalog().contains_key(b"Metadata".as_slice());

    let tree = doc.catalog().contains_key(b"StructTreeRoot".as_slice());

    let session = crate::remove_region::SessionOptions {
        remove_annotations: options.remove_annotations,
    };
    let mut s = regions_edit_session_with(doc, specs, limits, SessionMode::Redact, session)?;
    let structure_tree_removed = tree && options.remove_structure_tree;
    let structure = if tree && !structure_tree_removed {
        let e = crate::redact_struct::edit_tree(doc, &mut s.edit.overlay, &s.touched, limits)?;
        // The removed texts may describe what the rects removed.
        s.removed_text.extend(e.removed.iter().cloned());
        e
    } else {
        crate::redact_struct::StructureEdit::default()
    };
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
    // The form fields of removed widgets.
    let forms = crate::redact_forms::edit_fields(
        doc,
        &mut s.edit.overlay,
        &s.touched.removed_widgets,
        limits,
    )?;
    s.removed_text.extend(forms.removed_text.iter().cloned());
    s.removed_annot_prints.extend(forms.prints.iter().copied());
    let javascript_removed = if options.remove_javascript {
        crate::redact_js::strip_javascript(doc, &mut s.edit.overlay, limits)?
    } else {
        0
    };
    // A removed annotation or field still reached by something kept
    // would be written whole (by identity: later passes, the JavaScript
    // one among them, may have changed its content, so its print would
    // no longer match).
    let mut gone: HashSet<IndirectRef> = s.touched.removed_annotations.clone();
    gone.extend(forms.gone.iter().copied());
    if let Some((r, path)) =
        crate::redact_forms::removed_still_reached(doc, &s.edit.overlay, &gone, limits)?
    {
        return Err(Error::Unsupported {
            feature: "redaction output failed verification",
            detail: format!(
                "object {r} is a removed annotation or form field that the document still \
                 refers to (by {path})"
            ),
        });
    }
    // What they held, kept only through entries no reader uses: their
    // own data (text, actions and their targets, resources …).
    let (first, held_left) =
        crate::redact_forms::removed_data_kept(doc, &s.edit.overlay, &gone, limits)?;
    if let Some((r, path)) = first {
        return Err(Error::Unsupported {
            feature: "redaction output failed verification",
            detail: format!(
                "object {r} is data of a removed annotation or form field that only entries \
                 no reader uses still refer to (by {path})"
            ),
        });
    }
    // Prints of what the edit left of them too (a copy edited the same
    // way matches these).
    for r in &gone {
        if let Some(Object::Dict(d)) = s.edit.overlay.get(&(r.number, r.generation)) {
            s.removed_annot_prints
                .push(crate::remove_region::annotation_print(d));
        }
    }
    let (output, written) =
        s.edit
            .rewrite_with_overrides_mapped(&crate::rewrite::RewriteOverrides {
                strip_info: true,
                strip_metadata: true,
                javascript_removed: options.remove_javascript,
            })?;
    // The same, on what the rewrite actually wrote (its object map is the
    // output's object set): no removed annotation or field among it.
    if let Some(r) = gone
        .iter()
        .chain(held_left.iter())
        .filter(|r| written.contains_key(&(r.number, r.generation)))
        .min()
    {
        return Err(Error::Unsupported {
            feature: "redaction output failed verification",
            detail: format!(
                "object {r} is a removed annotation or form field the output still writes"
            ),
        });
    }

    // Type 3 glyph definitions the output no longer shows go too.
    let (output, glyph_images, glyph_procs, glyph_tounicodes) =
        match crate::redact_type3::prune_type3(doc, &output, limits)? {
            Some(p) => (p.output, p.images, p.procs, p.tounicodes),
            None => (output, Vec::new(), Vec::new(), Vec::new()),
        };

    // Every object the output carries was read by now. A literal string
    // holding an unescaped CR is a line feed here and a CR to some
    // readers: written out it changes for them (a palette, a font
    // name, a field value), so the document is refused.
    if doc.raw_cr_object_strings() > 0 {
        return Err(Error::Unsupported {
            feature: "redaction of a document with an unescaped carriage return in a string",
            detail: format!(
                "{} literal string(s) — readers do not agree on their bytes",
                doc.raw_cr_object_strings()
            ),
        });
    }

    // An original may stay only as an object the output still draws
    // (from a page, through forms); each such display is reported.
    // The images only removed Type 3 glyphs drew are checked like
    // edited originals: gone, or still drawn (reported).
    let mut originals = s.original_digests.clone();
    originals.extend(glyph_images);
    let (out, residual, shown, idx) = verify(
        &output,
        specs,
        limits,
        &s.removed_shading_digests,
        &originals,
        &s.removed_annot_prints,
        session,
    )?;
    verify_original_contents(&out, &idx, &s.original_contents_digests)?;
    let mut gone_whole = s.removed_stream_digests.clone();
    gone_whole.extend(glyph_tounicodes);
    verify_removed_streams(&out, &idx, &gone_whole, &glyph_procs)?;
    crate::redact_struct::verify(&out, &structure, limits)?;
    // Invariants that compare the input with the output alone, whatever
    // the edit recorded (see `redact_verify`).
    {
        let graph = crate::redact_verify::RefGraph::build(&out, &idx, limits)?;
        crate::redact_verify::check_reachable(&idx, &graph)?;
        let edited: BTreeSet<usize> = specs.iter().map(|(p, _)| *p).collect();
        crate::redact_verify::check_use(doc, &out, &idx, &graph, &edited, limits)?;
        crate::redact_verify::check_removed_annotations(doc, &out, &idx, &graph, &edited)?;
    }
    if options.remove_javascript {
        if let Some(r) = crate::redact_js::find_javascript(&out, limits)? {
            return Err(Error::Unsupported {
                feature: "redaction output failed verification",
                detail: format!("object {r} still holds JavaScript"),
            });
        }
    }
    // No Type 3 glyph the input showed and the output does not is
    // still defined.
    if crate::redact_type3::prune_type3(doc, &output, limits)?.is_some() {
        return Err(Error::Unsupported {
            feature: "redaction output failed verification",
            detail: "a Type 3 font still defines a glyph the output no longer shows".into(),
        });
    }
    let uninspected = crate::redact_inventory::uninspected_data(&out, limits)?;
    if options.refuse_uninspected && (uninspected.uninspected > 0 || !uninspected.complete) {
        let first: Vec<String> = uninspected
            .uninspected_items()
            .take(5)
            .map(|i| {
                format!(
                    "{} {:?}",
                    i.object
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "(direct)".into()),
                    i.role
                )
            })
            .collect();
        return Err(Error::Unsupported {
            feature: "redaction: the output carries uninspected data",
            detail: if uninspected.complete {
                format!(
                    "{} item(s) redact does not read: {}",
                    uninspected.uninspected,
                    first.join(", ")
                )
            } else {
                "the inventory of the output's data stopped at a limit".into()
            },
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
        shadings_removed: s.shadings_removed_total,
        links_removed: s.links_removed_total,
        annotations_removed: s.annots_removed_total,
        original_names_removed: s.names_pruned_total,
        per_page: s.per_page,
        info_removed: info_present,
        metadata_removed: metadata_present,
        structure_tree_removed,
        structure_texts_removed: structure.texts_removed,
        structure_items_removed: structure.items_removed,
        javascript_removed,
        elsewhere,
        elsewhere_unsearched_pages,
        uninspected,
        thumbnails_removed: s.thumbnails_removed_total,
        fields_removed: forms.fields_removed,
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
    shadings: &[[u8; 32]],
    originals: &[[u8; 32]],
    removed_annots: &[[u8; 32]],
    session: crate::remove_region::SessionOptions,
) -> Result<(Document, ResidualAudit, Shown, crate::redact_verify::Index), Error> {
    let fail = |detail: String| Error::Unsupported {
        feature: "redaction output failed verification",
        detail,
    };
    let out = Document::from_bytes_with_limits(output, limits.clone())?;
    // Every object, resolved once, with its stream digests: the checks
    // below and the invariants after them read this, not the file again.
    let idx = crate::redact_verify::Index::build(&out, limits)?;
    if out.xref_section_offsets().len() != 1 {
        return Err(fail("the output has more than one revision".into()));
    }
    // The rewrite writes neither object streams nor cross-reference
    // streams: one in the output is an input's, carried because
    // something referred to it, and holds the input's objects as they
    // were — edited and removed ones included.
    for (r, o) in &idx.objects {
        if let Object::Stream { dict, .. } = o {
            let t = dict.get(b"Type".as_slice()).and_then(|t| t.as_name());
            if matches!(t, Some(b"ObjStm") | Some(b"XRef")) {
                return Err(fail(format!(
                    "object {r} is an input object stream or cross-reference stream (it holds \
                     the input's objects as they were)"
                )));
            }
        }
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
        // The removed annotations themselves are refused by identity
        // when the edit is written; this catches copies. What the
        // output's pages hold is excused by identity, never by content
        // (two annotations alike are not the same annotation).
        let prints: HashSet<[u8; 32]> = removed_annots.iter().copied().collect();
        let entries = crate::redact_verify::AnnotEntries::of(&out)?;
        for (r, o) in &idx.objects {
            if entries.find(*r, o, &|d| {
                prints.contains(&crate::remove_region::annotation_print(d))
            }) {
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
        for (r, obj) in &idx.objects {
            let Object::Stream { dict, .. } = obj else {
                continue;
            };
            let d = &idx.digests[r];
            if !(originals.contains(&d.stored) || d.decoded.is_some_and(|x| originals.contains(&x)))
            {
                continue;
            }
            let Some(pages) = drawn.get(r) else {
                return Err(fail(format!(
                    "object {r} holds an edited image's or form's original data"
                )));
            };
            let form = dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Form");
            shown.extend(pages.iter().map(|p| (*p, form)));
        }
    }
    // No removed shading is still in the file (by content: renumbering
    // does not hide it).
    if !shadings.is_empty() {
        let gone: HashSet<[u8; 32]> = shadings.iter().copied().collect();
        let resource = |d: &Dict| {
            d.contains_key(b"ShadingType".as_slice()) || d.contains_key(b"PatternType".as_slice())
        };
        let mut elements: u64 = 0;
        for (r, obj) in &idx.objects {
            // The object itself and every dictionary written inside it
            // (a copy may sit in a pattern, a resource dictionary …).
            let mut stack: Vec<&Object> = vec![obj];
            while let Some(o) = stack.pop() {
                elements += 1;
                if elements > limits.max_audit_elements {
                    return Err(Error::LimitExceeded {
                        limit: "max_audit_elements",
                        max: limits.max_audit_elements,
                    });
                }
                let (d, whole) = match o {
                    Object::Dict(d) => (d, o),
                    Object::Stream { dict, .. } => (dict, o),
                    Object::Array(a) => {
                        stack.extend(a.iter());
                        continue;
                    }
                    _ => continue,
                };
                if resource(d)
                    && gone.contains(&crate::redact_type3::content_digest(&out, whole, limits)?)
                {
                    return Err(fail(format!(
                        "object {r} holds a removed shading or tiling pattern"
                    )));
                }
                stack.extend(d.values());
            }
        }
    }
    let residual = audit_residuals(&out, limits)?;
    Ok((out, residual, shown, idx))
}

/// No stream of `out` holds an edited page's original content (stored or
/// decoded), unless a page of `out` draws it as its `/Contents` (a
/// content stream the edited page shared with another page).
fn verify_original_contents(
    out: &Document,
    idx: &crate::redact_verify::Index,
    digests: &[[u8; 32]],
) -> Result<(), Error> {
    if digests.is_empty() {
        return Ok(());
    }
    let gone: HashSet<[u8; 32]> = digests.iter().copied().collect();
    let mut contents: HashSet<IndirectRef> = HashSet::new();
    for page in out.pages() {
        let Some(r) = page.object_ref() else { continue };
        let obj = out.resolve(r)?;
        let Some(c) = obj.as_dict().and_then(|d| d.get(b"Contents".as_slice())) else {
            continue;
        };
        match c {
            Object::Ref(r) => match out.resolve(*r)? {
                Object::Array(a) => contents.extend(a.iter().filter_map(|o| o.as_ref())),
                _ => {
                    contents.insert(*r);
                }
            },
            Object::Array(a) => contents.extend(a.iter().filter_map(|o| o.as_ref())),
            _ => {}
        }
    }
    // In object order: the first failure is the same for every run.
    for (r, d) in idx.streams() {
        let held = gone.contains(&d.stored) || d.decoded.is_some_and(|x| gone.contains(&x));
        if held && !contents.contains(&r) {
            return Err(Error::Unsupported {
                feature: "redaction output failed verification",
                detail: format!(
                    "object {r} holds an edited page's original content (something refers to it)"
                ),
            });
        }
    }
    Ok(())
}

/// No stream of `out` is what was removed whole (`removed`, by digest: a
/// thumbnail — but another page's own `/Thumb`, the same picture — or a
/// replaced `/ToUnicode`), and none is a removed Type 3 glyph's procedure
/// (`procs`) except as a Type 3 font's CharProc (the same bytes still a
/// glyph there).
fn verify_removed_streams(
    out: &Document,
    idx: &crate::redact_verify::Index,
    removed: &[[u8; 32]],
    procs: &[[u8; 32]],
) -> Result<(), Error> {
    if removed.is_empty() && procs.is_empty() {
        return Ok(());
    }
    let removed: HashSet<[u8; 32]> = removed.iter().copied().collect();
    let procs: HashSet<[u8; 32]> = procs.iter().copied().collect();
    // Where such bytes may still be used.
    let mut in_use: HashSet<IndirectRef> = HashSet::new();
    let mut thumbs: HashSet<IndirectRef> = HashSet::new();
    for page in out.pages() {
        let Some(p) = page.object_ref() else { continue };
        if let Some(Object::Ref(t)) = out
            .resolve(p)?
            .as_dict()
            .and_then(|d| d.get(b"Thumb".as_slice()))
        {
            thumbs.insert(*t);
        }
    }
    for (_, o) in &idx.objects {
        let Object::Dict(d) = o else {
            continue;
        };
        if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Type3") {
            if let Some(cp) = d.get(b"CharProcs".as_slice()) {
                if let Some(cp) = out.deref(cp)?.as_dict() {
                    in_use.extend(cp.values().filter_map(|v| v.as_ref()));
                }
            }
        }
    }
    for (r, d) in idx.streams() {
        let digests = [Some(d.stored), d.decoded];
        let is = |set: &HashSet<[u8; 32]>| digests.iter().flatten().any(|d| set.contains(d));
        if is(&removed) && !thumbs.contains(&r) {
            return Err(Error::Unsupported {
                feature: "redaction output failed verification",
                detail: format!(
                    "object {r} holds what the redaction removed whole (a thumbnail, a \
                     replaced /ToUnicode)"
                ),
            });
        }
        if is(&procs) && !in_use.contains(&r) {
            return Err(Error::Unsupported {
                feature: "redaction output failed verification",
                detail: format!("object {r} holds a removed Type 3 glyph's procedure"),
            });
        }
    }
    Ok(())
}

/// Every XObject a page of `out` invokes — by a `Do` in its content or
/// in an annotation's appearance stream (`/AP`, every state), or
/// transitively in the forms those draw — with the pages invoking it.
/// Names resolve as a reader resolves them: against the form's own
/// `/Resources`, else the resources it inherits. An invocation counts
/// whether or not a clip or `/BBox` hides what it draws: this is what
/// the output still *uses*, not what is visible.
pub(crate) fn drawn_xobjects(
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
            // Its masks are drawn with it.
            for key in [b"SMask".as_slice(), b"Mask"] {
                if let Some(Object::Ref(m)) = dict.get(key) {
                    drawn.entry(*m).or_default().insert(page);
                }
            }
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
        // A page naming no XObject and with no annotations draws none:
        // its content need not be read.
        let names_xobjects = match out.deref(&res)? {
            Object::Dict(d) => match d.get(b"XObject".as_slice()) {
                Some(x) => out.deref(x)?.as_dict().is_none_or(|x| !x.is_empty()),
                None => false,
            },
            _ => true,
        };
        if !names_xobjects && crate::redact_diag::page_dict_entry(out, page, b"Annots")?.is_none() {
            continue;
        }
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
    // Line ends compared as one: a CR, LF or CRLF in the removed text
    // matches any of them in a copy.
    let eol = |s: &str| s.replace("\r\n", "\n").replace('\r', "\n");
    let wanted: Vec<String> = texts.iter().map(|t| eol(t)).collect();
    let mut found = Vec::new();
    let mut unsearched = Vec::new();
    if texts.is_empty() {
        return Ok((found, unsearched));
    }
    let _fonts = out.font_cache_scope();
    // Everything the page shows as text — optional content off by
    // default and annotation appearances too: a copy there is a copy.
    for (i, page) in out.pages().iter().enumerate() {
        // An appearance that cannot be read leaves the page's own text
        // searched and the page reported as not (wholly) searched.
        let extracted = match crate::text_extract::extract_text_for_search(out, page, limits, true)
        {
            Ok(t) => Ok(t),
            Err(_) => {
                unsearched.push(i);
                crate::text_extract::extract_text_for_search(out, page, limits, false)
            }
        };
        let shown: String = match extracted {
            Ok(t) => {
                let mut s: String = t
                    .chars
                    .iter()
                    .filter_map(|c| c.unicode.as_deref())
                    .collect();
                for a in &t.annotations {
                    s.push('\n');
                    s.extend(a.appearance.iter().filter_map(|c| c.unicode.as_deref()));
                }
                eol(&s)
            }
            Err(_) => {
                if unsearched.last() != Some(&i) {
                    unsearched.push(i);
                }
                continue;
            }
        };
        for (t, w) in texts.iter().zip(&wanted) {
            if shown.contains(w.as_str()) {
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
        let strings: Vec<String> = strings.iter().map(|s| eol(s)).collect();
        for (t, w) in texts.iter().zip(&wanted) {
            if strings.iter().any(|s| s.contains(w.as_str())) {
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
        if let Some((bytes, _)) = crate::redact_fonts::embed_substitutes(doc, limits, false)? {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(objs: &[&[u8]]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let x = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// Line ends are compared as one: removed text with a LF is found in
    /// a copy written with CR LF or CR, and the other way round.
    #[test]
    fn copies_match_across_line_ends() {
        for (copy, removed) in [
            (&b"(SECRET\\r\\nTEXT)"[..], "SECRET\nTEXT"),
            (b"(SECRET\\rTEXT)", "SECRET\nTEXT"),
            (b"(SECRET\\nTEXT)", "SECRET\r\nTEXT"),
        ] {
            let mut annot = b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents ".to_vec();
            annot.extend_from_slice(copy);
            annot.extend_from_slice(b" >>");
            let bytes = pdf(&[
                b"<< /Type /Catalog /Pages 2 0 R >>",
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Annots [4 0 R] >>",
                &annot,
            ]);
            let doc = Document::from_bytes(&bytes).unwrap();
            let (found, _) =
                find_elsewhere(&doc, &[removed.to_string()], &Limits::default()).unwrap();
            assert!(
                found
                    .iter()
                    .any(|e| e.location == ElsewhereLocation::Object(IndirectRef::new(4, 0))),
                "{:?} vs {removed:?}: {found:?}",
                String::from_utf8_lossy(copy)
            );
        }
    }
}
