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
//!    equals an edited image's original, except where another page
//!    (or the same page outside the rects) still draws that original:
//!    those displays are kept and listed in [`Redaction::elsewhere`].
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
use crate::object::{Dict, Object};
use crate::redact_diag::Site;
use crate::remove_region::{regions_edit_session, sha256, PageSpec, SessionMode};
use std::collections::HashSet;

/// Options for [`redact`].
#[derive(Debug, Clone, Default)]
pub struct RedactOptions {
    limits: Limits,
    remove_structure_tree: bool,
    refuse_copies_elsewhere: bool,
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
}

/// The result of [`redact`]: a verified single-revision PDF.
pub struct Redaction {
    /// The redacted PDF.
    pub output: Vec<u8>,
    /// Character codes removed.
    pub codes_removed: u64,
    /// Image invocations whose samples inside the rects were zeroed.
    pub images_edited: u64,
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
}

impl std::fmt::Debug for Removed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Removed::Text(t) => write!(f, "Text({} chars)", t.chars().count()),
            Removed::Image => f.write_str("Image"),
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
            .field("codes_removed", &self.codes_removed)
            .field("images_edited", &self.images_edited)
            .field("pixels_changed", &self.pixels_changed)
            .field("paths_removed", &self.paths_removed)
            .field("paths_cut", &self.paths_cut)
            .field("links_removed", &self.links_removed)
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
    let limits = &options.limits;
    limits.validate()?;
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

    let mut s = regions_edit_session(doc, specs, limits, SessionMode::Redact)?;
    let structure_tree_removed = tree && options.remove_structure_tree;
    if tree && !structure_tree_removed && s.tagged_links_removed_total > 0 {
        return Err(Error::Unsupported {
            feature: "redaction of a link the structure tree refers to",
            detail: "the tree's /OBJR would keep the removed link (and its action) \
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

    // Originals another page still draws stay in the file by design;
    // every other copy must be gone.
    let shown: HashSet<[u8; 32]> = s.originals_shown.iter().flat_map(|(_, d)| *d).collect();
    let must_be_gone: Vec<[u8; 32]> = s
        .original_digests
        .iter()
        .copied()
        .filter(|d| !shown.contains(d))
        .collect();
    let (out, residual) = verify(&output, specs, limits, &must_be_gone)?;
    let (mut elsewhere, elsewhere_unsearched_pages) =
        find_elsewhere(&out, &s.removed_text, limits)?;
    let mut pages: Vec<usize> = s.originals_shown.iter().map(|(p, _)| *p).collect();
    pages.sort_unstable();
    pages.dedup();
    elsewhere.extend(pages.into_iter().map(|p| Elsewhere {
        removed: Removed::Image,
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
        codes_removed: s.codes_removed_total,
        images_edited: s.invocations_edited_total,
        pixels_changed: s.pixels_changed_total,
        paths_removed: s.paths_removed_total,
        paths_cut: s.paths_cut_total,
        links_removed: s.links_removed_total,
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
fn verify(
    output: &[u8],
    specs: &[PageSpec],
    limits: &Limits,
    originals: &[[u8; 32]],
) -> Result<(Document, ResidualAudit), Error> {
    let fail = |detail: String| Error::Unsupported {
        feature: "redaction output failed verification",
        detail,
    };
    let out = Document::from_bytes_with_limits(output, limits.clone())?;
    if out.xref_section_offsets().len() != 1 {
        return Err(fail("the output has more than one revision".into()));
    }
    // The same rects find nothing left to remove.
    let again = regions_edit_session(&out, specs, limits, SessionMode::Redact)?;
    if again.codes_removed_total
        + again.pixels_changed_total
        + again.paths_removed_total
        + again.paths_cut_total
        + again.links_removed_total
        != 0
    {
        return Err(fail(format!(
            "the rects still hit {} codes, {} nonzero pixels, {} paths, {} paths to cut, {} links",
            again.codes_removed_total,
            again.pixels_changed_total,
            again.paths_removed_total,
            again.paths_cut_total,
            again.links_removed_total
        )));
    }
    drop(again);
    // No stream equals an original, stored or decoded.
    let originals: HashSet<[u8; 32]> = originals.iter().copied().collect();
    if !originals.is_empty() {
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
            let decoded = out.decode_stream(dict, data).ok();
            if originals.contains(&sha256(data))
                || decoded.is_some_and(|d| originals.contains(&sha256(&d)))
            {
                return Err(fail(format!(
                    "object {r} holds an edited image's original data"
                )));
            }
        }
    }
    let residual = audit_residuals(&out, limits)?;
    Ok((out, residual))
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
