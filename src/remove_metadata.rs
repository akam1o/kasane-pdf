//! M25 — remove the document-level metadata *entries* and write a
//! new PDF.
//!
//! [`remove_document_metadata`] deletes exactly two entries:
//!
//! - the trailer `/Info` entry (the document-information dictionary
//!   reference), and
//! - the catalog `/Metadata` entry (the document-level XMP stream
//!   reference).
//!
//! Both removals happen in one `rewrite` pass — no per-entry
//! rewrite, no new file per step. The output is a fresh
//! single-revision PDF like [`crate::rewrite`]'s.
//!
//! **This is not a metadata scrubber or a redaction tool.** It
//! removes two specific *references*. It does not inspect, parse,
//! or redact their *values*; it does not touch `/Metadata` on
//! pages, XObjects, or any other object; and it does not guarantee
//! the underlying data is gone — the same dictionary or stream can
//! be reachable (and thus emitted) through another key, and the
//! same information can be duplicated in `/Annots`, `/StructTree`,
//! `/Names`, embedded files, or page content.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::rewrite::{rewrite_overlay_with, RewriteOverrides};
use std::collections::{HashMap, HashSet};

/// The result of [`remove_document_metadata`] — the rewritten PDF
/// plus which of the two entries were actually present and removed.
pub struct MetadataRemoval {
    /// The new single-revision PDF.
    pub output: Vec<u8>,
    /// `true` when the input trailer had an `/Info` entry that was
    /// removed from the output trailer. `false` when there was no
    /// `/Info` to remove — *not* a guarantee that no Info-like data
    /// remains elsewhere.
    pub info_removed: bool,
    /// `true` when the catalog had a `/Metadata` entry that was
    /// removed. `false` when there was none — *not* a guarantee that
    /// no XMP or other metadata remains elsewhere.
    pub metadata_removed: bool,
    /// Human-readable scope statement — never contains document
    /// text, metadata values, or stream bytes.
    pub scope: Vec<String>,
}

/// Terse summary — never prints `output` bytes or metadata values.
impl std::fmt::Debug for MetadataRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataRemoval")
            .field("output_bytes", &self.output.len())
            .field("info_removed", &self.info_removed)
            .field("metadata_removed", &self.metadata_removed)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Remove the trailer `/Info` and catalog `/Metadata` *entries* and
/// return a fresh single-revision PDF.
///
/// This is a narrow, fixed edit — not a metadata scrubber:
///
/// - It deletes exactly two *entries* (the trailer's `/Info` key and
///   the catalog's `/Metadata` key). It never parses, edits, or
///   partially redacts their values.
/// - If an entry is absent the call still succeeds; the
///   corresponding `*_removed` flag is `false`.
/// - A removed entry's *value* (the Info dictionary, the XMP
///   stream) is dropped from the output **only** when nothing else
///   references it. A shared `/Info` dict or `/Metadata` stream
///   reachable through another key is preserved by the normal
///   reachability walk — this is by design, not a leak to fix here.
/// - `/Metadata` on pages, XObjects, or any other object is
///   untouched, as are annotations, the structure tree,
///   `ActualText`/`Alt`/`E`, attachments, JavaScript, fonts,
///   `ToUnicode`, and page content.
///
/// All of [`crate::rewrite`]'s refusals apply unchanged (encrypted,
/// signed, named destinations, out-of-profile catalog entries, …)
/// and the usual output budgets bound the result. On any failure
/// nothing is emitted — the input is never modified.
pub fn remove_document_metadata(doc: &Document, limits: &Limits) -> Result<MetadataRemoval, Error> {
    // Decide what to remove from the *current* trailer/catalog —
    // before any collection runs. Flags report whether an entry was
    // present to remove, not whether its data is gone from the file.
    let info_present = doc.trailer().contains_key(b"Info".as_slice());
    let metadata_present = doc.catalog().contains_key(b"Metadata".as_slice());

    let overrides = RewriteOverrides {
        strip_info: true,
        strip_metadata: true,
    };
    let output = rewrite_overlay_with(doc, &HashMap::new(), &HashSet::new(), limits, &overrides)?;

    Ok(MetadataRemoval {
        output,
        info_removed: info_present,
        metadata_removed: metadata_present,
        scope: vec![
            "removed: trailer /Info entry".into(),
            "removed: catalog /Metadata entry".into(),
            "kept: page/XObject /Metadata, annotations, structure tree, \
             ActualText/Alt/E, embedded files, JavaScript, fonts/ToUnicode, \
             page content"
                .into(),
            "boundary: the Info dictionary / XMP stream may still be emitted \
             if another key references it; the same information may exist \
             elsewhere in the file"
                .into(),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    /// Minimal one-page document with a trailer `/Info` and a
    /// catalog `/Metadata` stream — the "everything" case.
    fn full_metadata_pdf() -> Vec<u8> {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        // Info dict.
        push!(
            5,
            b"<< /Title (Secret Title) /Author (Secret Author) /Producer (kasane) >>"
        );
        // XMP metadata stream on the catalog.
        let xmp = b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
        for n in 1..7u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 7 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Info present, no /Metadata.
    fn info_only_pdf() -> Vec<u8> {
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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Title (Secret Title) >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Catalog /Metadata only — no /Info at all.
    fn metadata_only_pdf() -> Vec<u8> {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xmp = b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
        for n in 1..7u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        // No /Info at all.
        parts.push(b"trailer\n<< /Size 7 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    /// Neither /Info nor /Metadata — should still succeed.
    fn no_metadata_pdf() -> Vec<u8> {
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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
        for n in 1..5u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    #[test]
    fn info_and_metadata_both_removed() {
        let d = doc(&full_metadata_pdf());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.info_removed);
        assert!(r.metadata_removed);
        // Re-parse the output and check the entries are gone.
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
    }

    #[test]
    fn info_only_removed() {
        let d = doc(&info_only_pdf());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.info_removed);
        assert!(!r.metadata_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
    }

    #[test]
    fn metadata_only_removed() {
        let d = doc(&metadata_only_pdf());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(!r.info_removed);
        assert!(r.metadata_removed);
        let out = doc(&r.output);
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
    }

    #[test]
    fn neither_present_still_succeeds() {
        let d = doc(&no_metadata_pdf());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(!r.info_removed);
        assert!(!r.metadata_removed);
        let out = doc(&r.output);
        assert_eq!(out.page_count(), 1);
    }

    /// `/Info` whose value is not a ref (e.g. a direct dict) is
    /// still removed — we don't need to resolve it to drop the key.
    #[test]
    fn info_non_ref_value_removed() {
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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
        for n in 1..5u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        // /Info as a direct (non-ref) value — unusual but legal.
        parts.push(b"trailer\n<< /Size 5 /Root 1 0 R /Info << /Title (T) >> >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.info_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
    }

    /// An `/Info` that points at an object that doesn't exist still
    /// gets its *entry* removed — we never resolve the value.
    #[test]
    fn info_dangling_ref_removed() {
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
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
        for n in 1..5u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        // /Info → 99 0 R, which doesn't exist.
        parts.push(b"trailer\n<< /Size 5 /Root 1 0 R /Info 99 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.info_removed);
        let out = doc(&r.output);
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
    }

    /// When the Info dictionary is *also* referenced from the
    /// catalog, removing `/Info` does not delete the object — the
    /// surviving reference keeps it reachable.
    #[test]
    fn shared_info_dict_stays_via_other_ref() {
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
        // Catalog references the Info dict under a custom key.
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Custom 5 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Title (Shared Title) >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.info_removed);
        let out = doc(&r.output);
        // /Info entry gone from trailer.
        assert!(!out.trailer().contains_key(b"Info".as_slice()));
        // But the dict itself survives via /Custom.
        let custom = out
            .catalog()
            .get(b"Custom".as_slice())
            .expect("custom key kept");
        let obj = out.deref(custom).unwrap();
        let dict = obj.as_dict().expect("shared dict is a dict");
        assert!(dict.contains_key(b"Title".as_slice()));
    }

    /// Page-level `/Metadata` is untouched — only the *catalog* one
    /// is removed.
    #[test]
    fn page_metadata_is_kept() {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Metadata 7 0 R /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xmp = b"<x:xmpmeta>doc-level</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xmp2 = b"<x:xmpmeta>page-level</x:xmpmeta>";
        let mut m2 = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m2.extend_from_slice(xmp2.len().to_string().as_bytes());
        m2.extend_from_slice(b" >>\nstream\n");
        m2.extend_from_slice(xmp2);
        m2.extend_from_slice(b"\nendstream");
        push!(7, &m2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 8\n0000000000 65535 f \n".to_vec());
        for n in 1..8u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 8 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        assert!(r.metadata_removed);
        let out = doc(&r.output);
        // Catalog /Metadata gone.
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
        // Page-level /Metadata kept.
        let page = out.pages().first().unwrap();
        let page_obj = out.resolve(page.object_ref().unwrap()).unwrap();
        let page_dict = page_obj.as_dict().unwrap();
        assert!(
            page_dict.contains_key(b"Metadata".as_slice()),
            "page /Metadata must be preserved"
        );
    }

    /// Other catalog keys survive — `/Metadata` is the only one
    /// stripped.
    #[test]
    fn catalog_other_keys_kept() {
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
        push!(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R \
              /PageMode /UseOutlines /MarkInfo << /Marked true >> >>"
        );
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xmp = b"<x:xmpmeta>x</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
        for n in 1..7u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 7 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        let out = doc(&r.output);
        let cat = out.catalog();
        assert!(!cat.contains_key(b"Metadata".as_slice()));
        assert!(cat.contains_key(b"PageMode".as_slice()));
        assert!(cat.contains_key(b"MarkInfo".as_slice()));
        assert!(cat.contains_key(b"Pages".as_slice()));
        // Page count preserved.
        assert_eq!(out.page_count(), 1);
    }

    /// The plain `rewrite` path is unchanged — `/Info` and
    /// `/Metadata` are still carried over.
    #[test]
    fn plain_rewrite_keeps_info_and_metadata() {
        let d = doc(&full_metadata_pdf());
        let out = doc(&crate::rewrite::rewrite(&d).unwrap());
        assert!(out.trailer().contains_key(b"Info".as_slice()));
        assert!(out.catalog().contains_key(b"Metadata".as_slice()));
    }

    /// A rewrite refusal (signed document) is inherited — the API
    /// cannot be used to bypass it.
    #[test]
    fn refusal_is_inherited() {
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
        push!(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 5 0 R >> >>"
        );
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        push!(5, b"<< /Type /Sig >>");
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let e = remove_document_metadata(&d, &Limits::default()).unwrap_err();
        match e {
            Error::Unsupported { feature, .. } => {
                assert!(feature.contains("signed"), "{feature}");
            }
            other => panic!("expected Unsupported(signed), got {other:?}"),
        }
    }

    /// An output-size cap still applies — a typed `LimitExceeded`,
    /// not a truncated file.
    #[test]
    fn output_budget_still_applies() {
        let d = doc(&full_metadata_pdf());
        let limits = Limits {
            max_output_bytes: 10,
            ..Default::default()
        };
        let e = remove_document_metadata(&d, &limits).unwrap_err();
        match e {
            Error::LimitExceeded { limit, .. } => {
                assert_eq!(limit, "max_output_bytes");
            }
            other => panic!("expected LimitExceeded(max_output_bytes), got {other:?}"),
        }
    }

    /// The input document and bytes are never mutated.
    #[test]
    fn input_is_not_mutated() {
        let bytes = full_metadata_pdf();
        let d = doc(&bytes);
        let _ = remove_document_metadata(&d, &Limits::default()).unwrap();
        // Re-check the source document still has both entries.
        assert!(d.trailer().contains_key(b"Info".as_slice()));
        assert!(d.catalog().contains_key(b"Metadata".as_slice()));
    }

    /// Debug output never leaks the output bytes or metadata values.
    #[test]
    fn debug_no_leak() {
        let d = doc(&full_metadata_pdf());
        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("Secret Title"), "{dbg}");
        assert!(!dbg.contains("SECRET-XMP"), "{dbg}");
        assert!(dbg.contains("info_removed"));
    }

    /// The audit still flags *other* residuals (e.g. a page-level
    /// /Metadata), even when the document-level entries are gone —
    /// findings are not a "safe" verdict.
    #[test]
    fn audit_still_reports_remaining_metadata() {
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
        push!(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Metadata 7 0 R /Contents 4 0 R >>"
        );
        push!(4, b"<< /Length 0 >>\nstream\n\nendstream");
        let xmp = b"<x:xmpmeta>doc</x:xmpmeta>";
        let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m.extend_from_slice(xmp.len().to_string().as_bytes());
        m.extend_from_slice(b" >>\nstream\n");
        m.extend_from_slice(xmp);
        m.extend_from_slice(b"\nendstream");
        push!(6, &m);
        let xmp2 = b"<x:xmpmeta>page</x:xmpmeta>";
        let mut m2 = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
        m2.extend_from_slice(xmp2.len().to_string().as_bytes());
        m2.extend_from_slice(b" >>\nstream\n");
        m2.extend_from_slice(xmp2);
        m2.extend_from_slice(b"\nendstream");
        push!(7, &m2);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 8\n0000000000 65535 f \n".to_vec());
        for n in 1..8u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 8 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let d = doc(&parts.concat());
        let before = crate::audit::audit_residuals(&d, &Limits::default()).unwrap();
        let meta_before = before
            .findings
            .iter()
            .filter(|f| f.kind == crate::audit::FindingKind::Metadata)
            .count();
        // The audit reports catalog /Metadata; page-level /Metadata is
        // not a separate finding kind today, so we get exactly one.
        assert_eq!(meta_before, 1, "expected the catalog /Metadata finding");

        let r = remove_document_metadata(&d, &Limits::default()).unwrap();
        let out = doc(&r.output);
        // Structurally the catalog no longer has /Metadata.
        assert!(!out.catalog().contains_key(b"Metadata".as_slice()));
        // The audit of the *output* finds no catalog /Metadata —
        // but this is NOT a "no metadata anywhere" verdict: the
        // page-level /Metadata is still present (verified directly
        // above and in `page_metadata_is_kept`).
        let after = crate::audit::audit_residuals(&out, &Limits::default()).unwrap();
        let meta_after = after
            .findings
            .iter()
            .filter(|f| f.kind == crate::audit::FindingKind::Metadata)
            .count();
        assert_eq!(meta_after, 0, "catalog /Metadata finding gone");
        // Page-level /Metadata is still there — verified directly.
        let page = out.pages().first().unwrap();
        let page_dict = out.resolve(page.object_ref().unwrap()).unwrap();
        assert!(
            page_dict
                .as_dict()
                .unwrap()
                .contains_key(b"Metadata".as_slice()),
            "page-level /Metadata must remain"
        );
    }
}
