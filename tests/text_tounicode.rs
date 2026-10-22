//! M6 — ToUnicode CMap parsing and positioned text extraction.
//!
//! Covers the M6 scope: `/ToUnicode` CMap (bfchar, bfrange
//! sequential + array, surrogate pairs, multi-scalar mappings),
//! `extract_text` API, char-by-char page-space positions, and the
//! refusal paths for malformed CMaps and unsupported constructs.

use kasane_pdf::{extract_text, render_page, Document, Error, Limits, RenderOptions};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

fn extract(name: &str) -> Result<kasane_pdf::ExtractedText, Error> {
    let pdf = fixture(name);
    let doc = Document::from_bytes(&pdf)?;
    extract_text(&doc, &doc.pages()[0], &Limits::default())
}

fn flate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn pdf_with_font(stream: &[u8], tounicode: Option<&[u8]>) -> Vec<u8> {
    let ttf = std::fs::read("tests/fixtures/KasaneTest.ttf").unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
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
    ];
    let mut font_obj = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
        /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 \
        /Widths ["
        .to_vec();
    for i in 0..224 {
        font_obj.extend_from_slice(if i == 0 {
            b"250 ".as_slice()
        } else {
            b"600 ".as_slice()
        });
    }
    font_obj.extend_from_slice(b"] /FontDescriptor 6 0 R".as_slice());
    if tounicode.is_some() {
        font_obj.extend_from_slice(b" /ToUnicode 8 0 R".as_slice());
    }
    font_obj.extend_from_slice(b" >>".as_slice());
    bodies.push((5, font_obj));
    bodies.push((
        6,
        b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular \
           /Flags 32 /FontBBox [0 -200 600 800] /ItalicAngle 0 \
           /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 \
           /FontFile2 7 0 R >>"
            .to_vec(),
    ));
    bodies.push((
        7,
        [
            format!(
                "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
                flate(&ttf).len(),
                ttf.len()
            )
            .into_bytes(),
            flate(&ttf),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ));
    if let Some(tu) = tounicode {
        bodies.push((
            8,
            [
                b"<< /Length ".as_slice(),
                flate(tu).len().to_string().as_bytes(),
                b" /Filter /FlateDecode >>\nstream\n",
                &flate(tu),
                b"\nendstream",
            ]
            .concat(),
        ));
    }
    // Sort by object number so xref entries land at the right offsets.
    bodies.sort_by_key(|(n, _)| *n);
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = bodies.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offs {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

// --------------------------------------------------------------------
// ToUnicode parsing
// --------------------------------------------------------------------

#[test]
fn tounicode_bfchar() {
    let cmap = b"/CIDInit /ProcSet findresource begin
        12 dict begin
        begincmap
        /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
        /CMapName /Test def
        /CMapType 2 def
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        2 beginbfchar
        <41> <0041>
        <42> <03B1>
        endbfchar
        endcmap
        CMapName currentdict /CMap defineresource pop
        end
        end";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (AB) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 2);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[1].unicode.as_deref(), Some("α")); // U+03B1
    assert_eq!(t.chars[0].resolved_via, kasane_pdf::ResolveKind::ToUnicode);
}

#[test]
fn tounicode_bfrange_sequential() {
    // <41> <43> <0041> → A,B,C → A,B,C
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfrange
        <41> <43> <0041>
        endbfrange
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (ABC) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 3);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[1].unicode.as_deref(), Some("B"));
    assert_eq!(t.chars[2].unicode.as_deref(), Some("C"));
}

/// A sequential bfrange whose destination sits above U+00FF — the
/// common case for symbol/math ToUnicode maps (`<2212>` …). Only the
/// *low byte* of the last code unit increments (§9.10.3); the high
/// byte must never trip the overflow check.
#[test]
fn tounicode_bfrange_sequential_above_latin1() {
    // <41> <43> <2212> → −, ∓(2213), ∔(2214)
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfrange
        <41> <43> <2212>
        endbfrange
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (ABC) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default())
        .expect("bfrange above U+00FF must parse");
    assert_eq!(t.chars.len(), 3);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("\u{2212}"));
    assert_eq!(t.chars[1].unicode.as_deref(), Some("\u{2213}"));
    assert_eq!(t.chars[2].unicode.as_deref(), Some("\u{2214}"));
    // The real overflow (low byte would pass 0xFF) is still refused.
    let bad = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfrange
        <41> <43> <22FE>
        endbfrange
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (ABC) Tj ET", Some(bad));
    let doc = Document::from_bytes(&pdf).unwrap();
    let err = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got {err:?}");
}

#[test]
fn tounicode_bfrange_array() {
    // <41> <43> [<0041><03B1><03B2>] → A→A, B→α, C→β
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfrange
        <41> <43> [<0041><03B1><03B2>]
        endbfrange
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (ABC) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars[1].unicode.as_deref(), Some("α"));
}

#[test]
fn tounicode_surrogate_pair() {
    // <3A51> → U+2003E via surrogate pair <D840DC3E>.
    // Our font doesn't have 0x51's glyph — but the code is mapped.
    // Use a code that exists: map <41> to a surrogate pair.
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfchar
        <41> <D840DC3E>
        endbfchar
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (A) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("\u{2003E}"));
}

#[test]
fn tounicode_multi_scalar() {
    // One code → multiple Unicode scalars (ligature-style).
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfchar
        <41> <00660066>
        endbfchar
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (A) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 1);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("ff")); // two f's
}

#[test]
fn tounicode_missing_entry_unresolved() {
    // <41> mapped, <42> not — second char is unresolved.
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfchar
        <41> <0041>
        endbfchar
        endcmap";
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (AB) Tj ET", Some(cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[1].unicode, None);
    assert_eq!(t.chars[1].resolved_via, kasane_pdf::ResolveKind::Unresolved);
}

#[test]
fn tounicode_usecmap_refused() {
    // /UseCMap in the ToUnicode stream dict → Unsupported.
    // Build the font dict with a ToUnicode stream that carries
    // /UseCMap in its stream dictionary.
    let ttf = std::fs::read("tests/fixtures/KasaneTest.ttf").unwrap();
    let cmap = b"begincmap 1 begincodespacerange <00> <FF> endcodespacerange endcmap";
    let mut out = Vec::new();
    out.extend_from_slice(
        b"%PDF-1.4
",
    );
    let mut offs = Vec::new();
    let stream = b"BT /F1 20 Tf 10 80 Td (A) Tj ET";
    let widths: Vec<&str> = (0..224)
        .map(|i| if i == 0 { "250" } else { "600" })
        .collect();
    let font = format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
         /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 \
         /Widths [{}] /FontDescriptor 6 0 R /ToUnicode 8 0 R >>",
        widths.join(" ")
    );
    let bodies = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
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
        (5, font.into_bytes()),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular \
               /Flags 32 /FontBBox [0 -200 600 800] /ItalicAngle 0 \
               /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 \
               /FontFile2 7 0 R >>"
                .to_vec(),
        ),
        (
            7,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
                    flate(&ttf).len(),
                    ttf.len()
                )
                .into_bytes(),
                flate(&ttf),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        // ToUnicode stream carrying /UseCMap in its dict.
        (
            8,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode /UseCMap 9 0 R >>\nstream\n",
                    flate(cmap).len()
                )
                .into_bytes(),
                flate(cmap),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        // Dummy stream for the UseCMap reference to point at.
        (9, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ];
    let mut bodies = bodies;
    bodies.sort_by_key(|(n, _)| *n);
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 10\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offs {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 10 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    let doc = Document::from_bytes(&out).unwrap();
    let err = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

// --------------------------------------------------------------------
// Fallback and unresolved
// --------------------------------------------------------------------

#[test]
fn no_tounicode_uses_encoding_fallback() {
    let r = extract("text_basic.pdf").unwrap();
    // All three chars resolve via WinAnsiEncoding → AGL → Unicode.
    assert_eq!(r.chars.len(), 3);
    assert_eq!(r.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(
        r.chars[0].resolved_via,
        kasane_pdf::ResolveKind::EncodingFallback
    );
}

#[test]
fn unresolved_code_is_none_not_fffd() {
    // 0x01 has no WinAnsi entry → glyph is .notdef, unicode unresolved.
    let r = extract("text_notdef.pdf").unwrap();
    assert!(r.chars.iter().any(|c| c.code == 0x01));
    let c = r.chars.iter().find(|c| c.code == 0x01).unwrap();
    assert_eq!(c.unicode, None);
    assert_eq!(c.resolved_via, kasane_pdf::ResolveKind::Unresolved);
}

#[test]
fn unicode_not_guessed() {
    // A code that maps to nothing returns None — never an invented
    // character.
    let r = extract("text_notdef.pdf").unwrap();
    assert!(!r
        .chars
        .iter()
        .any(|c| c.unicode.as_deref() == Some("\u{FFFD}")
            && c.resolved_via == kasane_pdf::ResolveKind::Unresolved));
}

// --------------------------------------------------------------------
// Position / geometry
// --------------------------------------------------------------------

#[test]
fn char_origin_is_page_space() {
    let r = extract("text_basic.pdf").unwrap();
    // First char A at Td(20,60), font 24pt.
    let c = &r.chars[0];
    assert_eq!(c.origin, (20.0, 60.0));
    // Advance = 600/1000 × 24 = 14.4 px horizontally.
    assert!((c.advance.0 - 14.4).abs() < 0.01);
    assert_eq!(c.advance.1, 0.0);
}

#[test]
fn char_advances_stack() {
    let r = extract("text_basic.pdf").unwrap();
    let (a, b, c) = (&r.chars[0], &r.chars[1], &r.chars[2]);
    // Each origin + advance = next origin.
    assert!((a.origin.0 + a.advance.0 - b.origin.0).abs() < 0.01);
    assert!((b.origin.0 + b.advance.0 - c.origin.0).abs() < 0.01);
}

#[test]
fn empty_string_produces_no_chars() {
    let r = extract("text_empty_string.pdf").unwrap();
    assert_eq!(r.chars.len(), 1);
    assert_eq!(r.chars[0].code, 0x41);
}

#[test]
fn tj_numbers_do_not_create_chars() {
    let r = extract("text_tj_consecutive_numbers.pdf").unwrap();
    // "A" + number + "B" — only 2 chars, number is not a code.
    assert_eq!(r.chars.len(), 2);
    assert_eq!(r.chars[0].code, 0x41);
    assert_eq!(r.chars[1].code, 0x42);
}

#[test]
fn invisible_text_extracted() {
    // Mode 3 chars are in the stream — they extract with
    // render_mode 3.
    let pdf = {
        let stream = b"BT /F1 20 Tf 10 80 Td 3 Tr (A) Tj 0 Tr (B) Tj ET";
        pdf_with_font(stream, None)
    };
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 2);
    assert_eq!(t.chars[0].render_mode, 3);
    assert_eq!(t.chars[1].render_mode, 0);
}

#[test]
fn form_text_extracted_at_form_position() {
    let r = extract("text_in_form.pdf").unwrap();
    // Form at Matrix [1 0 0 1 50 20] — "XY" at form-internal (0,0)
    // → page space (50, 20).
    assert_eq!(r.chars.len(), 2);
    assert_eq!(r.chars[0].origin, (50.0, 20.0));
}

#[test]
fn same_font_name_resolves_per_resources() {
    let r = extract("text_same_name_two_resources.pdf").unwrap();
    // Page's "Pg" at x=10; form's "Fm" at x=100.
    assert!(r.chars.len() >= 4);
    let page_char = &r.chars[0];
    let form_char = r.chars.iter().find(|c| c.origin.0 > 50.0).unwrap();
    assert_eq!(page_char.code, 0x50); // P
    assert_eq!(form_char.code, 0x46); // F
}

// --------------------------------------------------------------------
// Rendering unaffected
// --------------------------------------------------------------------

#[test]
fn tounicode_does_not_change_rendering() {
    // A PDF with ToUnicode renders identically to one without —
    // extraction is read-only.
    let plain = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (A) Tj ET", None);
    let doc1 = Document::from_bytes(&plain).unwrap();
    let r1 = render_page(&doc1, &doc1.pages()[0], &RenderOptions::default()).unwrap();
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfchar
        <41> <0391>
        endbfchar
        endcmap";
    let with_tu = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (A) Tj ET", Some(cmap));
    let doc2 = Document::from_bytes(&with_tu).unwrap();
    let r2 = render_page(&doc2, &doc2.pages()[0], &RenderOptions::default()).unwrap();
    assert_eq!(r1.pixmap.data(), r2.pixmap.data());
}

// --------------------------------------------------------------------
// Limits
// --------------------------------------------------------------------

#[test]
fn tounicode_size_limit() {
    // CMap bigger than max_stream_bytes → LimitExceeded.
    let big_cmap = vec![b'%'; 200]; // comment-filled — still a stream
    let pdf = pdf_with_font(b"BT /F1 20 Tf 10 80 Td (A) Tj ET", Some(&big_cmap));
    let doc = Document::from_bytes(&pdf).unwrap();
    let limits = Limits {
        max_stream_bytes: 10,
        ..Limits::default()
    };
    let err = extract_text(&doc, &doc.pages()[0], &limits).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}
