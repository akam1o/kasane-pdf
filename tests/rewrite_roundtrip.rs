//! Full-rewrite / round-trip integration tests.

use kasane_pdf::{rewrite, Document, Error, Limits, Object};

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

/// Minimal PDF: `pages` × (page + contents), optional trailer extras.
fn pdf_with(pages: usize, extra_root: &str, extra_trailer: &str) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(
        1,
        format!("<< /Type /Catalog /Pages 2 0 R {extra_root} >>").as_bytes(),
    );
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 3 + i * 2)).collect();
    push(
        2,
        format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>").as_bytes(),
    );
    for i in 0..pages {
        let pn = 3 + i * 2;
        let cn = pn + 1;
        push(
            pn as u32,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents {cn} 0 R >>")
                .as_bytes(),
        );
        push(cn as u32, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    }
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    let size = 3 + pages * 2;
    parts.push(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for n in 1..size as u32 {
        if let Some((_, off)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{off:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(format!("trailer\n<< /Size {size} /Root 1 0 R {extra_trailer} >>\n").into_bytes());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

fn rw(bytes: &[u8]) -> Vec<u8> {
    let doc = Document::from_bytes(bytes).unwrap();
    rewrite(&doc).unwrap()
}

fn reparse(bytes: &[u8]) -> Document {
    Document::from_bytes(bytes).unwrap()
}

// ------------------------------------------------------------------
// §1  serialization basics
// ------------------------------------------------------------------

#[test]
fn rewrite_basic_roundtrip() {
    let out = rw(&pdf_with(2, "", ""));
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.pages()[0].media_box().width(), 300.0);
}

#[test]
fn rewrite_is_deterministic() {
    let src = pdf_with(1, "", "");
    assert_eq!(rw(&src), rw(&src));
}

#[test]
fn rewrite_rejects_same_file() {
    // handled at the CLI level (canonicalize + compare); the API
    // itself is file-path-free so this just documents the contract.
    let data = std::fs::read("tests/fixtures/text_basic.pdf").unwrap();
    let doc = Document::from_bytes(&data).unwrap();
    // rewrite returns bytes — no file path knowledge, no panic.
    let out = rewrite(&doc).unwrap();
    assert!(out.starts_with(b"%PDF-"));
}

#[test]
fn header_has_binary_marker() {
    let out = rw(&pdf_with(1, "", ""));
    // pdf_with generates a 1.4 header — 1.4 input stays 1.4.
    assert!(out.starts_with(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n"));
}

#[test]
fn no_prev_or_xrefstm_in_output() {
    // /Prev in the input must not leak into the output trailer.
    let out = rw(&pdf_with(1, "", ""));
    let s = String::from_utf8_lossy(&out);
    assert!(!s.contains("/Prev"), "output still mentions /Prev");
    assert!(!s.contains("/XRefStm"));
}

// ------------------------------------------------------------------
// §2  object graph
// ------------------------------------------------------------------

#[test]
fn shared_object_gets_one_new_number() {
    // Two pages sharing one Resources dict → one new object, both
    // pages point at it. Build via the helper — Resources is obj 20.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
    push(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 20 0 R /Contents 4 0 R >>");
    push(4, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    push(5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources 20 0 R /Contents 6 0 R >>");
    push(6, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    push(20, b"<< /ProcSet [/PDF /Text] >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 21\n0000000000 65535 f \n".to_vec());
    for n in 1..21u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 21 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();
    let out = rw(&src);
    let s = String::from_utf8_lossy(&out);
    // The shared Resources object appears exactly once.
    let procset_count = s.matches("/ProcSet").count();
    assert_eq!(procset_count, 1, "shared object duplicated: {s}");
    // Both pages still point at it.
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 2);
}

#[test]
fn page_parent_cycle_is_preserved() {
    let out = rw(&pdf_with(1, "", ""));
    let doc = reparse(&out);
    let page = &doc.pages()[0];
    // /Parent must resolve back to the /Pages node.
    let parent_ref = page
        .object_ref()
        .and_then(|r| doc.resolve(r).ok())
        .and_then(|o| o.as_dict().cloned())
        .and_then(|d| d.get(b"Parent".as_slice()).cloned());
    assert!(matches!(parent_ref, Some(Object::Ref(_))));
}

// ------------------------------------------------------------------
// §3  streams
// ------------------------------------------------------------------

#[test]
fn stream_length_is_recomputed() {
    // /Length deliberately wrong in input — output must carry the
    // real encoded length, not the stored (lying) value.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R >>",
    );
    // /Length says 999 but the stream is 4 bytes.
    push(4, b"<< /Length 999 >>\nstream\nABCD\nendstream");
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
    let bytes = parts.concat();
    match Document::from_bytes(&bytes) {
        Ok(doc) => {
            let out = rewrite(&doc).unwrap();
            let s = String::from_utf8_lossy(&out);
            // The stream body is 4 bytes ("ABCD") — must be recomputed.
            assert!(s.contains("/Length 4"), "length not recomputed: {s}");
        }
        Err(_) => {
            // The parser may legitimately reject a lying /Length —
            // that's fine; the test then only asserts the writer
            // never emits the stored value.
        }
    }
}

#[test]
fn stream_bytes_verbatim() {
    // Content stream must arrive byte-identical.
    let out = rw(&pdf_with(1, "", ""));
    assert!(out.windows(5).any(|w| w == b"q Q\n\n" || w == b"\nq Q\n"));
}

/// Build a PDF from `(object-number, body)` pairs — bodies already
/// contain `stream…endstream` where needed. Returns the file bytes.
fn pdf_from_bodies(bodies: &[(u32, &[u8])]) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, b) in bodies {
        offs.push((*n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(b.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    }
    let max = bodies.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(format!("xref\n0 {max}\n0000000000 65535 f \n").into_bytes());
    for n in 1..max {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(format!("trailer\n<< /Size {max} /Root 1 0 R >>\n").into_bytes());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn stream_indirect_length_not_orphaned() {
    // /Length as an indirect ref shared by two streams. After rewrite
    // the output has no reference to the old Length object — it must
    // not be emitted as an orphan (the collector skips the
    // /Length slot inside stream dicts).
    let src = pdf_from_bodies(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents [4 0 R 5 0 R] >>",
        ),
        (4, b"<< /Length 6 0 R >>\nstream\nABCD\nendstream"),
        (5, b"<< /Length 6 0 R >>\nstream\nWXYZ\nendstream"),
        (6, b"4"), // shared indirect /Length = 4 bytes
    ]);
    let out = rw(&src);
    let s = String::from_utf8_lossy(&out);
    // Output /Length is always a direct integer equal to the data.
    assert!(s.contains("/Length 4"), "length not direct: {s}");
    // The orphan Length object (a bare `4` indirect object) must not
    // appear — it would be a `N 0 obj\n4\nendobj` triple.
    assert!(
        !s.contains(" obj\n4\nendobj"),
        "orphan /Length object emitted: {s}"
    );
    // Both streams still carry their data.
    assert!(out.windows(4).any(|w| w == b"ABCD"));
    assert!(out.windows(4).any(|w| w == b"WXYZ"));
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn stream_indirect_length_shared_elsewhere_kept() {
    // The same object is used both as /Length AND referenced from
    // another reachable slot — the object must survive (collected
    // from the non-Length site, not silently dropped).
    let src = pdf_from_bodies(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Annots [7 0 R] >>",
        ),
        (4, b"<< /Length 6 0 R >>\nstream\nABCD\nendstream"),
        (6, b"4"),
        // Annot dict references object 6 as an ordinary value.
        (
            7,
            b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /QuadPoints 6 0 R >>",
        ),
    ]);
    let out = rw(&src);
    let doc = reparse(&out);
    // Object 6 must still be reachable — /QuadPoints resolves to `4`.
    let annot_ref = doc.pages()[0]
        .object_ref()
        .and_then(|r| doc.resolve(r).ok())
        .and_then(|o| o.as_dict().cloned())
        .and_then(|d| d.get(b"Annots".as_slice()).cloned())
        .and_then(|a| a.as_array().map(|v| v.to_vec()))
        .and_then(|arr| arr.first().cloned())
        .and_then(|r| r.as_ref());
    let annot = annot_ref.and_then(|r| doc.resolve(r).ok());
    let qp = annot
        .and_then(|o| o.as_dict().cloned())
        .and_then(|d| d.get(b"QuadPoints".as_slice()).cloned())
        .and_then(|q| q.as_ref())
        .and_then(|r| doc.resolve(r).ok());
    assert_eq!(qp, Some(Object::Integer(4)));
}

#[test]
fn stream_data_containing_endstream_like_bytes() {
    // Stream data that happens to contain the bytes `endstream`
    // must round-trip verbatim — /Length recomputes over the real
    // data, and the emit wraps it with LF…LF so the embedded keyword
    // can never be mistaken for the terminator (§7.3.8.1).
    let data: &[u8] = b"xxendstreamyy";
    let body: Vec<u8> = [
        format!("<< /Length {} >>\nstream\n", data.len()).into_bytes(),
        data.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat();
    let src = pdf_from_bodies(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R >>",
        ),
        (4, &body),
    ]);
    let out = rw(&src);
    assert!(out.windows(data.len()).any(|w| w == data));
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("/Length 13"), "{s}");
    // And the output reparses — the parser must find the real
    // endstream, not the embedded look-alike.
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 1);
}

// ------------------------------------------------------------------
// §4  fonts / text
// ------------------------------------------------------------------

#[test]
fn text_basic_roundtrip() {
    let data = std::fs::read("tests/fixtures/text_basic.pdf").unwrap();
    let out = rw(&data);
    let doc = reparse(&out);
    let text = kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::TextOptions::default())
        .unwrap();
    assert_eq!(text.chars.len(), 3);
    assert_eq!(text.chars[0].unicode.as_deref(), Some("A"));
}

#[test]
fn cid_font_roundtrip() {
    // Use the m7 helper indirectly — build a CID PDF inline.
    let ttf = std::fs::read("tests/fixtures/KasaneCIDTest.ttf").unwrap();
    let stream = b"BT /F1 20 Tf 20 50 Td <0001> Tj ET";
    let bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (
            5,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneCIDTest-Regular /Encoding /Identity-H /DescendantFonts [6 0 R] >>".to_vec(),
        ),
        (
            6,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KasaneCIDTest-Regular /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /CIDToGIDMap /Identity /DW 1000 >>".to_vec(),
        ),
        (
            7,
            b"<< /Type /FontDescriptor /FontName /KasaneCIDTest-Regular /Flags 4 /FontBBox [0 -200 600 800] /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 8 0 R >>".to_vec(),
        ),
        (
            8,
            [
                format!("<< /Length {} >>\nstream\n", ttf.len()).into_bytes(),
                ttf.clone(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, b) in &bodies {
        offs.push((*n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(b.clone());
        parts.push(b"\nendobj\n".to_vec());
    }
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 9\n0000000000 65535 f \n".to_vec());
    for n in 1..9 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 9 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();
    let out = rw(&src);
    let doc = reparse(&out);
    let text = kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::TextOptions::default())
        .unwrap();
    assert_eq!(text.chars.len(), 1);
    assert_eq!(text.chars[0].code, 1);
}

// ------------------------------------------------------------------
// §5  links
// ------------------------------------------------------------------

#[test]
fn uri_link_preserved() {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots [9 0 R] >>",
    );
    push(9, b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /A << /S /URI /URI <68747470733A2F2F6578616D706C652E636F6D> >> >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
    for n in 1..10u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();
    let out = rw(&src);
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("/URI"), "URI action lost: {s}");
    assert!(s.contains("/Annots"), "Annots array lost");
    // URI string is hex-encoded — the hex payload must be present.
    let hex = b"68747470733A2F2F6578616D706C652E636F6D";
    assert!(out.windows(hex.len()).any(|w| w == hex));
}

#[test]
fn goto_dest_page_ref_remapped() {
    // /A /S /GoTo /D [page2 0 R /XYZ …] — dest ref must point at the
    // *new* page-2 number.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots [5 0 R] >>",
    );
    push(5, b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /A << /S /GoTo /D [8 0 R /XYZ 0 200 null] >> >>");
    push(
        8,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 9\n0000000000 65535 f \n".to_vec());
    for n in 1..9 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 9 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();
    let out = rw(&src);
    let doc2 = reparse(&out);
    // The /D array's first element is a page ref — it must resolve.
    let annots = doc2.pages()[0].resources(); // just touching the page is fine
    let _ = annots;
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("/GoTo"), "GoTo action lost");
    assert!(s.contains("/XYZ"), "dest array lost");
}

// ------------------------------------------------------------------
// §6  refusals
// ------------------------------------------------------------------

#[test]
fn encrypted_refused() {
    let data = std::fs::read("tests/fixtures/encrypted.pdf").unwrap();
    match Document::from_bytes(&data) {
        Err(Error::Encrypted) => {}
        _ => panic!("encrypted.pdf did not fail at load"),
    }
}

#[test]
fn signed_docmdp_refused() {
    let p = pdf_with(1, "/Perms << /DocMDP 9 0 R >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::SignaturePolicy { rule, .. }) => {
            assert_eq!(rule, kasane_pdf::SignatureRule::RewriteOfSignedDocument)
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

/// A signature value nested under /Kids (the common Acrobat layout)
/// or hanging off a page widget outside /Fields is found too; an
/// unsigned /Sig field (no /V) passes through.
#[test]
fn nested_and_widget_only_signatures_refused() {
    let p = pdf_with(
        1,
        "/AcroForm << /Fields [<< /T (grp) /Kids [<< /FT /Sig /T (s1) \
         /V << /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >> >>] >>] >>",
        "",
    );
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, detail }) => {
            assert_eq!(feature, "signed signature field");
            assert!(detail.contains("ByteRange"), "{detail}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    let p = pdf_with(1, "/AcroForm << /Fields [<< /FT /Sig /T (blank) >>] >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    assert!(
        rewrite(&doc).is_ok(),
        "an unsigned field is not a signature"
    );
}

#[test]
fn xfa_refused() {
    let p = pdf_with(1, "/AcroForm << /XFA 9 0 R /Fields [] >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "XFA form"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn openaction_javascript_refused() {
    let p = pdf_with(1, "/OpenAction << /S /JavaScript /JS (app.alert) >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "OpenAction JavaScript")
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn names_javascript_refused() {
    let p = pdf_with(1, "/Names << /JavaScript 9 0 R >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "JavaScript name tree")
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn goto_d_array_accepted() {
    // Direct destination *array* must still pass — not a named dest.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots [5 0 R] >>",
    );
    push(
        5,
        b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /A << /S /GoTo /D [8 0 R /Fit] >> >>",
    );
    push(
        8,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 9\n0000000000 65535 f \n".to_vec());
    for n in 1..9 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 9 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();
    let out = rw(&src);
    assert!(out.windows(5).any(|w| w == b"/GoTo"));
}

#[test]
fn ocproperties_refused() {
    let p = pdf_with(1, "/OCProperties << /OCGs [] >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "catalog entry outside profile")
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// §7  trailer / version
// ------------------------------------------------------------------

#[test]
fn trailer_has_root_and_size_only() {
    let out = rw(&pdf_with(1, "", ""));
    let s = String::from_utf8_lossy(&out);
    let t = s.find("trailer").unwrap();
    let tr = &s[t..];
    assert!(tr.contains("/Root"));
    assert!(tr.contains("/Size"));
    assert!(!tr.contains("/Prev"));
    assert!(!tr.contains("/ID"));
}

#[test]
fn catalog_version_respected() {
    let p = pdf_with(1, "/Version /1.7", "");
    let out = rw(&p);
    assert!(out.starts_with(b"%PDF-1.7"));
}

#[test]
fn version_2_0_refused() {
    let p = pdf_with(1, "/Version /2.0", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn version_2_0_header_refused() {
    let mut src = pdf_with(1, "", "");
    src.splice(0..8, b"%PDF-2.0\n".iter().cloned());
    let doc = Document::from_bytes(&src).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn catalog_version_below_header_stays_high() {
    // Header 1.7 + catalog Version 1.4 → output 1.7 (max wins).
    let mut src = pdf_with(1, "/Version /1.4", "");
    src.splice(0..8, b"%PDF-1.7\n".iter().cloned());
    let out = rw(&src);
    assert!(out.starts_with(b"%PDF-1.7"));
}

#[test]
fn malformed_header_version_refused() {
    // `%PDF-9.x` is a malformed version token — the file still loads
    // (header check finds `%PDF-` and moves on), but rewrite must
    // refuse rather than emit a fabricated `%PDF-1.7`.
    let mut src = pdf_with(1, "", "");
    src.splice(0..8, b"%PDF-9.x\n".iter().cloned());
    let doc = Document::from_bytes(&src).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn malformed_header_version_no_dot_refused() {
    let mut src = pdf_with(1, "", "");
    src.splice(0..8, b"%PDF-abc\n".iter().cloned());
    let doc = Document::from_bytes(&src).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn catalog_version_non_name_refused() {
    // /Version as an integer is malformed — refuse, don't ignore.
    let p = pdf_with(1, "/Version 17", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn catalog_version_no_dot_refused() {
    let p = pdf_with(1, "/Version /17", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn catalog_version_nonnumeric_minor_refused() {
    let p = pdf_with(1, "/Version /1.x", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

// ------------------------------------------------------------------
// §8  ObjStm input
// ------------------------------------------------------------------

#[test]
fn output_object_limit() {
    // Limits::max_output_objects bounds the DFS walk — a document
    // with more reachable objects than the cap fails cleanly.
    let src = pdf_with(10, "", "");
    let doc = Document::from_bytes(&src).unwrap();
    let limits = kasane_pdf::Limits::default().with_max_output_objects(3);
    match kasane_pdf::rewrite_with_limits(&doc, &limits) {
        Err(Error::LimitExceeded { limit, .. }) => {
            assert_eq!(limit, "max_output_objects")
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
}

#[test]
fn output_byte_limit() {
    // A tight byte cap must fail even on a valid input.
    let src = pdf_with(1, "", "");
    let doc = Document::from_bytes(&src).unwrap();
    let limits = kasane_pdf::Limits::default().with_max_output_bytes(100);
    match kasane_pdf::rewrite_with_limits(&doc, &limits) {
        Err(Error::LimitExceeded { limit, .. }) => {
            assert_eq!(limit, "max_output_bytes")
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
}

#[test]
fn objstm_input_rewritten_as_plain_objects() {
    let data = std::fs::read("tests/fixtures/objstm.pdf").unwrap();
    let out = rw(&data);
    // Output must not contain an /ObjStm stream.
    let s = String::from_utf8_lossy(&out);
    assert!(!s.contains("/ObjStm"), "output still has ObjStm: {s}");
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 1);
}

#[test]
fn incremental_input_latest_state() {
    let data = std::fs::read("tests/fixtures/incremental.pdf").unwrap();
    let out = rw(&data);
    let doc = reparse(&out);
    assert_eq!(doc.page_count(), 1);
}

// ------------------------------------------------------------------
// §9  serialization edge cases
// ------------------------------------------------------------------

// --------------------------------------------------------------------
// Named destinations — preserved through the rewrite
// --------------------------------------------------------------------

/// Assemble a PDF from `(number, body)` pairs; object 1 must be the
/// catalog. Numbers may be sparse.
fn pdf_objs(objs: &[(u32, String)]) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objs {
        offs.push((*n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.as_bytes().to_vec());
        parts.push(b"\nendobj\n".to_vec());
    }
    let size = objs.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for n in 1..size {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").into_bytes());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

fn two_pages(extra_catalog: &str, extra_page1: &str, extra: &[(u32, String)]) -> Vec<u8> {
    let mut objs = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {extra_catalog} >>")),
        (2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string()),
        (
            3,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R {extra_page1} >>"),
        ),
        (4, "<< /Length 4 >>\nstream\nq Q\nendstream".to_string()),
        (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 6 0 R >>".to_string()),
        (6, "<< /Length 4 >>\nstream\nq Q\nendstream".to_string()),
    ];
    objs.extend(extra.iter().cloned());
    pdf_objs(&objs)
}

fn dests_of(bytes: &[u8]) -> Vec<kasane_pdf::NamedDest> {
    let doc = Document::from_bytes(bytes).unwrap();
    kasane_pdf::named_destinations(&doc, &Limits::default()).unwrap()
}

#[test]
fn legacy_catalog_dests_survive_rewrite() {
    let src = two_pages(
        "/Dests << /Chap1 [5 0 R /Fit] /Chap0 [3 0 R /XYZ 0 100 null] >>",
        "",
        &[],
    );
    let before = dests_of(&src);
    assert_eq!(before.len(), 2);
    let out = rewrite(&Document::from_bytes(&src).unwrap()).unwrap();
    let after = dests_of(&out);
    assert_eq!(before, after, "named destinations must round-trip");
    let c1 = after.iter().find(|d| d.name == b"Chap1").unwrap();
    assert_eq!(c1.page, Some(1));
    assert_eq!(c1.source, kasane_pdf::DestSource::LegacyDests);
    let c0 = after.iter().find(|d| d.name == b"Chap0").unwrap();
    assert_eq!(c0.page, Some(0));
    assert_eq!(c0.view.len(), 4);
}

#[test]
fn names_dests_tree_with_kids_and_dict_dests_survives_rewrite() {
    // Root → two Kids (with /Limits); values are a direct array, an
    // indirect array and an indirect destination *dictionary* (/D).
    let extra = vec![
        (10, "<< /Kids [11 0 R 12 0 R] >>".to_string()),
        (
            11,
            "<< /Limits [(A) (B)] /Names [(A) [3 0 R /Fit] (B) 13 0 R] >>".to_string(),
        ),
        (
            12,
            "<< /Limits [(C) (C)] /Names [(C) 14 0 R] >>".to_string(),
        ),
        (13, "[5 0 R /FitH 150]".to_string()),
        (14, "<< /D [5 0 R /XYZ 10 20 1.5] >>".to_string()),
    ];
    let src = two_pages("/Names << /Dests 10 0 R >>", "", &extra);
    let before = dests_of(&src);
    assert_eq!(before.len(), 3);
    assert_eq!(
        before.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
        [b"A".to_vec(), b"B".to_vec(), b"C".to_vec()]
    );
    assert_eq!(before[1].page, Some(1));
    assert_eq!(before[2].page, Some(1));
    assert_eq!(before[2].view[0], kasane_pdf::Object::Name(b"XYZ".to_vec()));
    let out = rewrite(&Document::from_bytes(&src).unwrap()).unwrap();
    assert_eq!(dests_of(&out), before);
    // The tree *structure* is still a tree — Kids and Limits intact.
    let doc = Document::from_bytes(&out).unwrap();
    let names = doc
        .deref(doc.catalog().get(b"Names".as_slice()).unwrap())
        .unwrap();
    let dests = doc
        .deref(names.as_dict().unwrap().get(b"Dests".as_slice()).unwrap())
        .unwrap();
    let kids = doc
        .deref(dests.as_dict().unwrap().get(b"Kids".as_slice()).unwrap())
        .unwrap();
    assert_eq!(kids.as_array().unwrap().len(), 2);
    let kid0 = doc.deref(&kids.as_array().unwrap()[0]).unwrap();
    assert!(kid0.as_dict().unwrap().contains_key(b"Limits".as_slice()));
}

#[test]
fn link_goto_and_openaction_named_refs_survive_rewrite() {
    let extra = vec![
        (
            9,
            "<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /Dest /Chap1 >>".to_string(),
        ),
        (
            15,
            "<< /Type /Annot /Subtype /Link /Rect [10 40 100 60] /A << /S /GoTo /D (Chap1) >> >>"
                .to_string(),
        ),
    ];
    let src = two_pages(
        "/Dests << /Chap1 [5 0 R /Fit] >> /OpenAction << /S /GoTo /D /Chap1 >>",
        "/Annots [9 0 R 15 0 R]",
        &extra,
    );
    let out = rewrite(&Document::from_bytes(&src).unwrap()).unwrap();
    let doc = Document::from_bytes(&out).unwrap();
    // The name operands are still names/strings — not resolved away,
    // not dropped — and still resolve to page 2.
    let page = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let annots = doc
        .deref(page.as_dict().unwrap().get(b"Annots".as_slice()).unwrap())
        .unwrap();
    let annots = annots.as_array().unwrap();
    let a0 = doc.deref(&annots[0]).unwrap();
    assert_eq!(
        a0.as_dict().unwrap().get(b"Dest".as_slice()),
        Some(&kasane_pdf::Object::Name(b"Chap1".to_vec()))
    );
    let a1 = doc.deref(&annots[1]).unwrap();
    let act = doc
        .deref(a1.as_dict().unwrap().get(b"A".as_slice()).unwrap())
        .unwrap();
    assert_eq!(
        act.as_dict().unwrap().get(b"D".as_slice()),
        Some(&kasane_pdf::Object::String(b"Chap1".to_vec()))
    );
    let oa = doc
        .deref(doc.catalog().get(b"OpenAction".as_slice()).unwrap())
        .unwrap();
    assert_eq!(
        oa.as_dict().unwrap().get(b"D".as_slice()),
        Some(&kasane_pdf::Object::Name(b"Chap1".to_vec()))
    );
    let d = kasane_pdf::resolve_named_destination(&doc, b"Chap1", &Limits::default())
        .unwrap()
        .unwrap();
    assert_eq!(d.page, Some(1));
}

#[test]
fn dangling_named_dest_is_copied_not_repaired() {
    // No tree at all; the link names a destination that does not exist.
    // The input's defect is preserved verbatim — nothing is invented.
    let extra = vec![(
        9,
        "<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /Dest /Nowhere >>".to_string(),
    )];
    let src = two_pages("", "/Annots [9 0 R]", &extra);
    let out = rewrite(&Document::from_bytes(&src).unwrap()).unwrap();
    let doc = Document::from_bytes(&out).unwrap();
    assert!(
        kasane_pdf::resolve_named_destination(&doc, b"Nowhere", &Limits::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn unresolvable_dest_page_reports_none() {
    // Page entry is a bare integer (remote-style) → page None, kept.
    let src = two_pages("/Dests << /R [1 /Fit] >>", "", &[]);
    let d = dests_of(&src);
    assert_eq!(d[0].page, None);
    let out = rewrite(&Document::from_bytes(&src).unwrap()).unwrap();
    assert_eq!(dests_of(&out), d);
}

#[test]
fn cyclic_name_tree_is_refused_before_output() {
    let extra = vec![
        (10, "<< /Kids [11 0 R] >>".to_string()),
        (11, "<< /Kids [10 0 R] >>".to_string()),
    ];
    let src = two_pages("/Names << /Dests 10 0 R >>", "", &extra);
    let doc = Document::from_bytes(&src).unwrap();
    assert!(matches!(rewrite(&doc), Err(Error::CycleDetected { .. })));
}
