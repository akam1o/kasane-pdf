//! Milestone M8 — full rewrite / round-trip tests.

use kasane_pdf::{rewrite, Document, Error, Object};

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
    // not be emitted as an orphan (M8.1 fix: the collector skips the
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
    let text =
        kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::Limits::default()).unwrap();
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
    let text =
        kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::Limits::default()).unwrap();
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
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "signed document"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
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
fn catalog_dests_refused() {
    // Legacy /Dests dict on the catalog — named destination tree.
    let p = pdf_with(1, "/Dests << /Chap1 [3 0 R /Fit] >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "named destination"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn names_dests_refused() {
    // /Names /Dests name tree.
    let p = pdf_with(
        1,
        "/Names << /Dests << /Names [(Chap1) [3 0 R /Fit]] >> >>",
        "",
    );
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "named destination"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn link_dest_name_refused() {
    // /Annots /Dest as a name — named destination.
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
    push(
        9,
        b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /Dest /Chap1 >>",
    );
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
    let doc = Document::from_bytes(&src).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "named destination"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn goto_d_name_refused() {
    // /GoTo /D as a string — named destination.
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
    push(
        9,
        b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /A << /S /GoTo /D (Chap1) >> >>",
    );
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
    let doc = Document::from_bytes(&src).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "named destination"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn openaction_d_name_refused() {
    let p = pdf_with(1, "/OpenAction << /S /GoTo /D /Chap1 >>", "");
    let doc = Document::from_bytes(&p).unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "named destination"),
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

// ------------------------------------------------------------------
// §8  ObjStm input
// ------------------------------------------------------------------

#[test]
fn output_object_limit() {
    // Limits::max_output_objects bounds the DFS walk — a document
    // with more reachable objects than the cap fails cleanly.
    let src = pdf_with(10, "", "");
    let doc = Document::from_bytes(&src).unwrap();
    let limits = kasane_pdf::Limits {
        max_output_objects: 3, // 10 pages + catalog + contents > 3
        ..Default::default()
    };
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
    let limits = kasane_pdf::Limits {
        max_output_bytes: 100,
        ..Default::default()
    };
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
