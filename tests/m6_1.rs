//! M6.1 — extraction hardening: ActualText/Optional Content refusal,
//! cumulative Unicode output budget, headless extraction (no
//! rasterising/image decode), and pinned-down coordinate semantics.

use kasane_pdf::{extract_text, render_page, Document, Error, Limits, RenderOptions, ResolveKind};

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

/// Minimal single-page PDF with an embedded TrueType font.
/// `page_extra` is appended to the page dict (e.g. `/Rotate 90`),
/// `page_resources` supplies the page `/Resources` body,
/// `extra_objs` carries extra objects (forms, property lists, …).
fn pdf(
    stream: &[u8],
    page_extra: &str,
    page_resources: &str,
    extra_objs: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let ttf = std::fs::read("tests/fixtures/KasaneTest.ttf").unwrap();
    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] {page_extra} \
                 /Resources << {page_resources} >> /Contents 4 0 R >>"
            )
            .into_bytes(),
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
    let mut font = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
        /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths ["
        .to_vec();
    for i in 0..224 {
        font.extend_from_slice(if i == 0 {
            b"250 ".as_slice()
        } else {
            b"600 ".as_slice()
        });
    }
    font.extend_from_slice(b"] /FontDescriptor 6 0 R >>");
    bodies.push((5, font));
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
    bodies.extend(extra_objs.iter().cloned());
    bodies.sort_by_key(|(n, _)| *n);

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = bodies.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    // Emit one xref entry per object number — gaps become free entries.
    let mut entries = vec!["0000000000 65535 f \n".to_string()];
    for n in 1..size {
        if let Some((_, off)) = offs.iter().find(|(on, _)| *on == n) {
            entries.push(format!("{off:010} 00000 n \n"));
        } else {
            entries.push("0000000000 00000 f \n".to_string());
        }
    }
    for e in &entries {
        out.extend_from_slice(e.as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn extract_pdf(bytes: &[u8], limits: &Limits) -> Result<kasane_pdf::ExtractedText, Error> {
    let doc = Document::from_bytes(bytes)?;
    extract_text(&doc, &doc.pages()[0], limits)
}

const FONT_RES: &str = "/Font << /F1 5 0 R >>";

// --------------------------------------------------------------------
// ActualText
// --------------------------------------------------------------------

#[test]
fn actual_text_direct_dict_refused() {
    let stream = b"BT /F1 20 Tf 10 80 Td /Span << /ActualText (Zzz) >> BDC (A) Tj EMC ET";
    let bytes = pdf(stream, "", FONT_RES, &[]);
    let err = extract_pdf(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn actual_text_via_properties_name_refused() {
    let stream = b"BT /F1 20 Tf 10 80 Td /Span /P1 BDC (A) Tj EMC ET";
    let bytes = pdf(
        stream,
        "",
        "/Font << /F1 5 0 R >> /Properties << /P1 << /ActualText (Zzz) >> >>",
        &[],
    );
    let err = extract_pdf(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn actual_text_inside_form_refused() {
    let form = b"BT /F1 20 Tf 0 0 Td /Span << /ActualText (Zzz) >> BDC (X) Tj EMC ET";
    let form_obj: Vec<u8> = [
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Length {} >>\nstream\n",
            form.len()
        )
        .into_bytes(),
        form.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat();
    let bytes = pdf(
        b"/Fm Do",
        "",
        "/Font << /F1 5 0 R >> /XObject << /Fm 8 0 R >>",
        &[(8, form_obj)],
    );
    let err = extract_pdf(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn plain_marked_content_still_extracts() {
    // A /Span BDC without ActualText is ordinary marked content.
    let stream =
        b"BT /F1 20 Tf 10 80 Td /Span << /Lang (en) >> BDC (A) Tj EMC /B2 BMC (B) Tj EMC ET";
    let bytes = pdf(stream, "", FONT_RES, &[]);
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 2);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[1].unicode.as_deref(), Some("B"));
}

#[test]
fn undefined_properties_ref_is_structure_error() {
    let stream = b"BT /F1 20 Tf 10 80 Td /Span /Missing BDC (A) Tj EMC ET";
    let bytes = pdf(stream, "", "/Font << /F1 5 0 R >> /Properties << >>", &[]);
    let err = extract_pdf(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

// --------------------------------------------------------------------
// Optional Content
// --------------------------------------------------------------------

fn ocg_pdf(ocg_type: &[u8]) -> Vec<u8> {
    let stream = b"BT /F1 20 Tf 10 80 Td /OC /G1 BDC (A) Tj EMC ET";
    pdf(
        stream,
        "",
        "/Font << /F1 5 0 R >> /Properties << /G1 8 0 R >>",
        &[(
            8,
            [
                b"<< /Type /".to_vec(),
                ocg_type.to_vec(),
                b" /Name (G) >>".to_vec(),
            ]
            .concat(),
        )],
    )
}

#[test]
fn optional_content_ocg_refused() {
    let err = extract_pdf(&ocg_pdf(b"OCG"), &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn optional_content_ocmd_refused() {
    let err = extract_pdf(&ocg_pdf(b"OCMD"), &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn oc_tag_without_membership_dict_is_plain_marked_content() {
    // Spec: marked content counts as optional content only when the
    // property list is a valid OCG/OCMD. A dict without /Type is not.
    let stream = b"BT /F1 20 Tf 10 80 Td /OC << /Name (x) >> BDC (A) Tj EMC ET";
    let bytes = pdf(stream, "", FONT_RES, &[]);
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 1);
}

#[test]
fn unused_ocg_resource_does_not_block() {
    // An OCG in /Properties that is never referenced by the stream is
    // irrelevant — extraction must not refuse it.
    let bytes = pdf(
        b"BT /F1 20 Tf 10 80 Td (A) Tj ET",
        "",
        "/Font << /F1 5 0 R >> /Properties << /Idle 8 0 R >>",
        &[(8, b"<< /Type /OCG /Name (idle) >>".to_vec())],
    );
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 1);
}

// --------------------------------------------------------------------
// Cumulative Unicode output budget
// --------------------------------------------------------------------

#[test]
fn unicode_budget_exact_boundary() {
    // 'A' = 1 byte per char; 3 chars = 3 bytes. Cap at 3 → ok.
    let t = extract("text_basic.pdf").unwrap();
    assert_eq!(t.chars.len(), 3);
    let pdf_bytes = fixture("text_basic.pdf");
    let doc = Document::from_bytes(&pdf_bytes).unwrap();
    let limits = Limits {
        max_extracted_unicode_bytes: 3,
        ..Limits::default()
    };
    let ok = extract_text(&doc, &doc.pages()[0], &limits).unwrap();
    assert_eq!(ok.chars.len(), 3);
    let under = Limits {
        max_extracted_unicode_bytes: 2,
        ..Limits::default()
    };
    let err = extract_text(&doc, &doc.pages()[0], &under).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit.contains("unicode")),
        "{err:?}"
    );
}

#[test]
fn unicode_budget_counts_repeated_mapping() {
    // A ToUnicode mapping for 'A' → 4 UTF-8 bytes; repeated 'AAAA'
    // counts 16 — cache/repetition does not evade the budget.
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        1 beginbfchar
        <41> <0066006600660066>
        endbfchar
        endcmap";
    let pdf = pdf_with_tounicode(b"BT /F1 20 Tf 10 80 Td (AAAA) Tj ET", cmap);
    let doc = Document::from_bytes(&pdf).unwrap();
    let ok = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 16,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(ok.chars.len(), 4);
    let err = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 15,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }));
}

#[test]
fn unicode_budget_form_repetition_counts() {
    // The same form called twice emits 'X' twice → 2 bytes, not 1.
    let form = b"BT /F1 20 Tf 0 0 Td (X) Tj ET";
    let form_obj: Vec<u8> = [
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
             /Resources << /Font << /F1 5 0 R >> >> \
             /Length {} >>\nstream\n",
            form.len()
        )
        .into_bytes(),
        form.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat();
    let bytes = pdf(
        b"/Fm Do /Fm Do",
        "",
        "/Font << /F1 5 0 R >> /XObject << /Fm 8 0 R >>",
        &[(8, form_obj)],
    );
    let doc = Document::from_bytes(&bytes).unwrap();
    let ok = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(ok.chars.len(), 2);
    let err = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }));
}

#[test]
fn unicode_budget_uses_utf8_bytes_not_utf16() {
    // α (U+03B1) is 2 UTF-8 bytes; cap of 3 allows 'Aα' (1+2) but
    // 2 does not.
    let cmap = b"begincmap
        1 begincodespacerange
        <00> <FF>
        endcodespacerange
        2 beginbfchar
        <41> <0041>
        <42> <03B1>
        endbfchar
        endcmap";
    let pdf = pdf_with_tounicode(b"BT /F1 20 Tf 10 80 Td (AB) Tj ET", cmap);
    let doc = Document::from_bytes(&pdf).unwrap();
    extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 3,
            ..Limits::default()
        },
    )
    .unwrap();
    let err = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }));
}

/// Same builder as tests/m6.rs `pdf_with_font`, but inlined here with
/// the CMap so this file is self-contained.
fn pdf_with_tounicode(stream: &[u8], tounicode: &[u8]) -> Vec<u8> {
    let tu_obj: Vec<u8> = [
        format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            flate(tounicode).len()
        )
        .into_bytes(),
        flate(tounicode),
        b"\nendstream".to_vec(),
    ]
    .concat();
    // Rebuild the font object with /ToUnicode 8 0 R — pdf() uses a
    // fixed font; build manually here.
    let ttf = std::fs::read("tests/fixtures/KasaneTest.ttf").unwrap();
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
    let mut font = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
        /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths ["
        .to_vec();
    for i in 0..224 {
        font.extend_from_slice(if i == 0 {
            b"250 ".as_slice()
        } else {
            b"600 ".as_slice()
        });
    }
    font.extend_from_slice(b"] /FontDescriptor 6 0 R /ToUnicode 8 0 R >>");
    bodies.push((5, font));
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
    bodies.push((8, tu_obj));
    bodies.sort_by_key(|(n, _)| *n);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = 9;
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
// Headless extraction — no rasterising / image decoding
// --------------------------------------------------------------------

#[test]
fn extraction_skips_broken_image_decode() {
    // An Image XObject whose data is corrupt still extracts — images
    // are never decoded in extraction mode.
    let img = [b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
        /ColorSpace /DeviceRGB /BitsPerComponent 8 \
        /Filter /DCTDecode /Length 4 >>\nstream\nxxxx\nendstream"
        .to_vec()];
    let bytes = pdf(
        b"BT /F1 20 Tf 10 80 Td (A) Tj ET /Im Do",
        "",
        "/Font << /F1 5 0 R >> /XObject << /Im 8 0 R >>",
        &[(8, img.concat())],
    );
    // Sanity: rendering this page fails on the broken JPEG.
    let doc = Document::from_bytes(&bytes).unwrap();
    assert!(render_page(&doc, &doc.pages()[0], &RenderOptions::default()).is_err());
    // Extraction still succeeds with just the text.
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 1);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
}

#[test]
fn extraction_does_not_allocate_page_sized_buffer() {
    // A page whose pixmap would exceed max_render_total_pixels still
    // extracts — extraction never sizes a raster buffer by page
    // dimensions. (max_render_pixels=64 keeps render refusal cheap;
    // a 10,000×10,000-pt page → ~1.4G px at 200dpi.)
    let bytes = pdf(
        b"BT /F1 20 Tf 10 80 Td (A) Tj ET",
        "/MediaBox [0 0 10000 10000]",
        FONT_RES,
        &[],
    );
    let doc = Document::from_bytes(&bytes).unwrap();
    let tiny = Limits {
        max_render_pixels: 64,
        max_render_total_pixels: 64 * 64,
        max_render_buffer_bytes: 64 * 64 * 4,
        ..Limits::default()
    };
    assert!(render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions {
            dpi: 200.0,
            limits: tiny.clone(),
        }
    )
    .is_err());
    let t = extract_text(&doc, &doc.pages()[0], &tiny).unwrap();
    assert_eq!(t.chars.len(), 1);
}

#[test]
fn extraction_skips_clip_mask_work() {
    // text_clip.pdf wraps its text in W n — extraction doesn't build
    // clip masks; chars are still reported (clipping is not a
    // visibility verdict for extraction).
    let t = extract("text_clip.pdf").unwrap();
    assert!(!t.chars.is_empty());
}

// --------------------------------------------------------------------
// Coordinate semantics
// --------------------------------------------------------------------

#[test]
fn coords_not_rebased_by_cropbox() {
    // Same text at the same content coords on a page whose CropBox
    // starts at (100,50) — coordinates are not rebased.
    let bytes = pdf(
        b"BT /F1 20 Tf 10 80 Td (A) Tj ET",
        "/CropBox [100 50 300 150]",
        FONT_RES,
        &[],
    );
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars[0].origin, (10.0, 80.0));
}

#[test]
fn coords_ignore_rotate() {
    for rot in [0, 90, 180, 270] {
        let bytes = pdf(
            b"BT /F1 20 Tf 10 80 Td (A) Tj ET",
            &format!("/Rotate {rot}"),
            FONT_RES,
            &[],
        );
        let t = extract_pdf(&bytes, &Limits::default()).unwrap();
        assert_eq!(t.chars[0].origin, (10.0, 80.0), "rotate {rot}");
        assert!((t.chars[0].advance.0 - 12.0).abs() < 0.01);
    }
}

#[test]
fn coords_nonzero_origin_mediabox() {
    let bytes = pdf(
        b"BT /F1 20 Tf 10 80 Td (A) Tj ET",
        "/MediaBox [50 25 250 125]",
        FONT_RES,
        &[],
    );
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars[0].origin, (10.0, 80.0));
}

#[test]
fn rotated_text_origin_and_advance() {
    // cm-rotated text: origin is the rotated point, advance follows
    // the rotation (y-up → +90° points "up" the page).
    let t = extract("text_rotated.pdf").unwrap();
    let c = &t.chars[0];
    // (0,0) under rot90 + translate(20,60) → (20,60).
    assert_eq!(c.origin, (20.0, 60.0));
    // Advance direction is the rotated +x → (0, +advance).
    assert!((c.advance.0).abs() < 0.01);
    assert!(c.advance.1 > 0.0);
    assert!(c.bounds.is_some());
}

#[test]
fn text_rise_shifts_origin() {
    // Ts moves the baseline up; origin.y reflects it.
    let bytes = pdf(b"BT /F1 20 Tf 5 Ts 10 80 Td (A) Tj ET", "", FONT_RES, &[]);
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert!((t.chars[0].origin.1 - 85.0).abs() < 0.01);
}

#[test]
fn space_has_no_bounds() {
    let bytes = pdf(b"BT /F1 20 Tf 10 80 Td (A B) Tj ET", "", FONT_RES, &[]);
    let t = extract_pdf(&bytes, &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 3);
    assert_eq!(t.chars[1].code, 0x20);
    assert_eq!(t.chars[1].bounds, None);
    assert!(t.chars[0].bounds.is_some());
}

#[test]
fn bounds_is_glyph_ink_not_advance_box() {
    // ' ' has no outline → no bounds; glyphs report their ink bbox
    // which differs from the advance box (KasaneTest glyphs are drawn
    // deliberately wider/narrower than 600 units in places — check
    // the two are not trivially identical either way).
    let t = extract("text_basic.pdf").unwrap();
    let c = &t.chars[0];
    let (minx, miny, maxx, maxy) = c.bounds.unwrap();
    let advance_rect = (c.origin.0, c.origin.1, c.origin.0 + c.advance.0, c.origin.1);
    assert!((minx, miny, maxx, maxy) != advance_rect);
    // bounds anchored at the glyph origin, extends upward (+y).
    assert!((minx - c.origin.0).abs() < 0.5);
    assert!(maxy > c.origin.1);
}

// --------------------------------------------------------------------
// Failure leaves no partial result
// --------------------------------------------------------------------

#[test]
fn actual_text_after_partial_text_returns_err_only() {
    // Text before the ActualText must not leak as a partial result:
    // the API returns Err, not ExtractedText.
    let stream = b"BT /F1 20 Tf 10 80 Td (A) Tj /Span << /ActualText (Z) >> BDC (B) Tj EMC ET";
    let bytes = pdf(stream, "", FONT_RES, &[]);
    let err = extract_pdf(&bytes, &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }));
}

// keep the import used
#[allow(dead_code)]
fn _use_resolve_kind(k: ResolveKind) -> ResolveKind {
    k
}
