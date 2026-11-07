//! Glyph placement on the device grid: unrotated, unskewed text filled
//! with a colour sits on a whole device row (the glyph origin's device y
//! floored, as Poppler's Splash places the glyphs it caches), so a line
//! drawn a fraction of a pixel lower renders exactly like the line at
//! the row above it; x keeps its exact position. Stroked, clipping,
//! pattern-filled and rotated text is drawn where it falls. Text
//! extraction coordinates are not snapped.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, render_page, Document, RenderOptions, TextOptions};

fn pdf(content: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
              /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ])
}

fn pixels(content: &str) -> Vec<u8> {
    let doc = Document::from_bytes(&pdf(content)).unwrap();
    render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0))
        .unwrap()
        .pixels()
        .to_vec()
}

/// At 72 dpi device y = 100 − user y: baselines at user y 40.2–40.9
/// have device y 59.1–59.8, all floored to row 59 — the device position
/// of user y 41.
#[test]
fn horizontal_text_snaps_to_the_row_above() {
    let at = |y: &str| pixels(&format!("BT /F1 24 Tf 20 {y} Td (Hxg) Tj ET"));
    let row = at("41");
    for y in ["40.2", "40.5", "40.9"] {
        assert!(at(y) == row, "baseline {y} differs from baseline 41");
    }
    // x is not snapped.
    assert!(pixels("BT /F1 24 Tf 20.5 41 Td (Hxg) Tj ET") != row);
    // Stroked, clipping and pattern-filled text are exact paths, as in
    // Splash: not snapped.
    let stroked = |y: &str| pixels(&format!("BT /F1 24 Tf 1 Tr 20 {y} Td (Hxg) Tj ET"));
    assert!(stroked("40.5") != stroked("41"));
    let clipped = |y: &str| {
        pixels(&format!(
            "BT /F1 24 Tf 7 Tr 20 {y} Td (Hxg) Tj ET 0 0 1 rg 0 0 200 100 re f"
        ))
    };
    assert!(clipped("40.5") != clipped("41"));
}

/// Rotated text keeps its exact position.
#[test]
fn rotated_text_is_not_snapped() {
    let at = |y: &str| {
        pixels(&format!(
            "BT /F1 24 Tf 0.9659 0.2588 -0.2588 0.9659 20 {y} Tm (Hxg) Tj ET"
        ))
    };
    assert!(at("40.5") != at("41"));
}

/// Extraction reports the exact baseline.
#[test]
fn extraction_is_not_snapped() {
    let b = pdf("BT /F1 24 Tf 20 40.5 Td (H) Tj ET");
    let doc = Document::from_bytes(&b).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    let c = &t.chars[0];
    assert!((c.origin.1 - 40.5).abs() < 1e-6, "{:?}", c.origin);
}
