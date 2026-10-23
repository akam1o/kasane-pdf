//! `/ActualText` (ISO 32000-1 §14.9.4): replacement text for a
//! marked-content span. Extraction reports it; rendering ignores it.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, render_page, Document, Error, Limits, RenderOptions, ResolveKind};

fn pdf(content: &[u8]) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
               /Encoding /WinAnsiEncoding >> >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content)),
    ])
}

fn text(content: &[u8]) -> Result<String, Error> {
    let doc = Document::from_bytes(&pdf(content))?;
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default())?;
    Ok(t.chars.iter().filter_map(|c| c.unicode.clone()).collect())
}

#[test]
fn span_text_replaces_every_glyph_once() {
    let s =
        text(b"BT /F1 20 Tf 10 50 Td /Span << /ActualText (fi) >> BDC (f) Tj (i) Tj EMC (x) Tj ET")
            .unwrap();
    assert_eq!(s, "fix");
    let doc = Document::from_bytes(&pdf(
        b"BT /F1 20 Tf 10 50 Td /Span << /ActualText (fi) >> BDC (f) Tj (i) Tj EMC ET",
    ))
    .unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 2, "both glyphs are reported");
    assert_eq!(t.chars[1].unicode.as_deref(), Some(""));
    assert_eq!(t.chars[1].resolved_via, ResolveKind::ActualText);
    assert!(t.chars[1].advance.0 > 0.0, "advances are the glyphs' own");
}

#[test]
fn nested_spans_inner_wins_outer_not_repeated() {
    let s = text(
        b"BT /F1 20 Tf 10 50 Td /Span << /ActualText (OUT) >> BDC (a) Tj \
          /Span << /ActualText (IN) >> BDC (b) Tj EMC (c) Tj EMC ET",
    )
    .unwrap();
    assert_eq!(s, "OUTIN");
}

#[test]
fn empty_actual_text_hides_the_span() {
    let s =
        text(b"BT /F1 20 Tf 10 50 Td (A) Tj /Span << /ActualText () >> BDC (B) Tj EMC (C) Tj ET")
            .unwrap();
    assert_eq!(s, "AC");
}

#[test]
fn utf16_actual_text_and_non_span_tags() {
    // UTF-16BE with BOM, on a /P tag.
    let s = text(b"BT /F1 20 Tf 10 50 Td /P << /ActualText <FEFF65E5672C> >> BDC (A) Tj EMC ET")
        .unwrap();
    assert_eq!(s, "\u{65E5}\u{672C}");
}

#[test]
fn span_without_glyphs_still_yields_its_text() {
    let doc = Document::from_bytes(&pdf(
        b"BT /F1 20 Tf 10 50 Td (A) Tj /Span << /ActualText (-) >> BDC EMC (B) Tj ET",
    ))
    .unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    let s: String = t.chars.iter().filter_map(|c| c.unicode.clone()).collect();
    assert_eq!(s, "A-B");
    let synthetic = &t.chars[1];
    assert_eq!(synthetic.code, 0);
    assert_eq!(synthetic.advance, (0.0, 0.0));
    assert!(synthetic.bounds.is_none());
    assert!(
        (synthetic.origin.0 - t.chars[2].origin.0).abs() < 1e-6,
        "sits at the next glyph's origin"
    );
}

#[test]
fn invalid_actual_text_is_structure_error() {
    let err =
        text(b"BT /F1 20 Tf 10 50 Td /Span << /ActualText 42 >> BDC (A) Tj EMC ET").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn actual_text_does_not_affect_rendering() {
    let doc = Document::from_bytes(&pdf(
        b"BT /F1 20 Tf 10 50 Td /Span << /ActualText (Zzz) >> BDC (A) Tj EMC ET",
    ))
    .unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let ink = (0..r.width)
        .flat_map(|x| (0..r.height).map(move |y| (x, y)))
        .filter(|(x, y)| r.pixmap.pixel(*x, *y).unwrap().red() < 128)
        .count();
    assert!(ink > 0, "the glyph A is painted regardless of ActualText");
}

#[test]
fn deeply_nested_marked_content_is_bounded() {
    let mut c = b"BT /F1 20 Tf 10 50 Td ".to_vec();
    for _ in 0..600 {
        c.extend_from_slice(b"/Span << /ActualText (x) >> BDC ");
    }
    c.extend_from_slice(b"(A) Tj ");
    for _ in 0..600 {
        c.extend_from_slice(b"EMC ");
    }
    c.extend_from_slice(b"ET");
    let err = text(&c).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}
