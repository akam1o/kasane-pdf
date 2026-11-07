//! Redaction: remove what is drawn inside page rectangles, and prove
//! on the written bytes that it is gone.
//!
//! [`redact`] builds on the region editor (`remove_regions`) and adds
//! what makes the result fit for distribution:
//!
//! - **Removal, not covering.** Text codes and image samples inside
//!   the rects are removed or zeroed as by the editor; painted paths
//!   inside a rect are removed too. Nothing is painted over anything.
//! - **No route back to the original.** An edited image's original
//!   may not stay reachable: names of it on pages that do not draw it
//!   are removed, and a page that still draws it unredacted is
//!   refused. The trailer `/Info` and catalog `/Metadata` (XMP) are
//!   removed. A structure tree carrying alternate text (`/Alt`,
//!   `/ActualText`, `/E`), which may repeat the removed text, is
//!   refused unless the caller asks for the tree to be removed.
//! - **Verified output.** The written PDF is parsed again and checked
//!   before it is returned: one revision only; the same rects find no
//!   text, path or nonzero image sample to remove; and no stream —
//!   every live object, decoded — equals an edited image's original
//!   samples or stored bytes. A failed check is an error, never a
//!   returned PDF.
//!
//! Everything the editor refuses stays refused (annotations on an
//! edited page, forms, masks, non-fill text, …): a structure that
//! cannot be processed exactly is an error, not a best-effort result.
//!
//! The redaction is **positional**: what is drawn inside the rects is
//! removed. The same information elsewhere — another occurrence of the
//! text on another page, an outline title, an attachment, a form
//! value — is not searched for; [`Redaction::residual`] lists the
//! surfaces the output still carries for the caller to review.

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
    /// Text removed from the rects (runs of at least four
    /// non-whitespace characters) that the output still shows or
    /// stores elsewhere — another page's content, an outline title, an
    /// annotation's contents … The redaction is positional; these are
    /// for the caller to decide on.
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

/// One finding of [`Redaction::elsewhere`].
#[derive(Clone)]
pub struct Elsewhere {
    /// The removed text that was found.
    pub text: String,
    /// Where.
    pub location: ElsewhereLocation,
}

impl std::fmt::Debug for Elsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Elsewhere")
            .field("text_chars", &self.text.chars().count())
            .field("location", &self.location)
            .finish()
    }
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

    let (out, residual) = verify(&output, specs, limits, &s.original_digests)?;
    let (elsewhere, elsewhere_unsearched_pages) = find_elsewhere(&out, &s.removed_text, limits)?;

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
                    text: t.to_string(),
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
                    text: t.to_string(),
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
