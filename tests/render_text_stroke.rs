//! Stroked text (`Tr` 1, 2, 5, 6): the glyph outline is stroked like a
//! path in user space (§9.3.6, §8.5.3.2) — the line width, caps, joins
//! and dashes of the graphics state are user units, independent of the
//! font size and the text matrix that scale the outline itself.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, RenderOptions};

fn pdf(content: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 120] /Contents 4 0 R \
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

fn ink(content: &str) -> usize {
    let doc = Document::from_bytes(&pdf(content)).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0)).unwrap();
    r.pixmap().pixels().iter().filter(|p| p.red() < 128).count()
}

#[test]
fn the_text_stroke_width_is_in_user_space() {
    // The same 100-pt "Hi" outlined with a 4-unit line, the size coming
    // from Tfs, from Tm, or from the CTM (with the line width scaled by
    // the CTM like every other user-space length).
    let by_size = ink("BT 1 Tr 4 w /F1 100 Tf 10 20 Td (Hi) Tj ET");
    let by_tm = ink("BT 1 Tr 4 w /F1 10 Tf 10 0 0 10 10 20 Tm (Hi) Tj ET");
    let by_ctm = ink("q 10 0 0 10 10 20 cm BT 1 Tr 0.4 w /F1 10 Tf (Hi) Tj ET Q");
    let close = |a: usize, b: usize| (a as f64 - b as f64).abs() <= 0.03 * a.max(b) as f64;
    assert!(
        close(by_size, by_tm) && close(by_size, by_ctm),
        "{by_size} {by_tm} {by_ctm}"
    );
    // A 4-unit outline of a glyph ~70 units tall is far from both a
    // hairline and a solid blob: thicker lines ink more.
    let thin = ink("BT 1 Tr 1 w /F1 100 Tf 10 20 Td (Hi) Tj ET");
    let filled = ink("BT 0 Tr /F1 100 Tf 10 20 Td (Hi) Tj ET");
    assert!(
        thin < by_size && by_size < filled * 2,
        "thin {thin} 4w {by_size} filled {filled}"
    );
}

#[test]
fn fill_then_stroke_keeps_both_colours() {
    // Tr 2: red fill, blue 3-unit outline — both visible.
    let doc = Document::from_bytes(&pdf(
        "BT 2 Tr 3 w 1 0 0 rg 0 0 1 RG /F1 100 Tf 10 20 Td (H) Tj ET",
    ))
    .unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0)).unwrap();
    let px = r.pixmap().pixels();
    let red = px.iter().filter(|p| p.red() > 200 && p.blue() < 60).count();
    let blue = px.iter().filter(|p| p.blue() > 200 && p.red() < 60).count();
    assert!(red > 500 && blue > 300, "red {red} blue {blue}");
}
