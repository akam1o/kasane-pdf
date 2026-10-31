//! A CFF font program whose Top DICT selects the predefined ISOAdobe
//! charset (Adobe TN 5176 Appendix C: glyph *i* is standard string *i*)
//! carries no charset table. Its glyphs must still be found by name;
//! they used to resolve to `.notdef`, which was drawn instead of every
//! glyph (a visible box for a space) on pages counted as exact renders.
//! The fixture (`tests/fixtures/cff_charset/`, `make_cff_charset.py`):
//! `.notdef` is the square 100..200, `exclam` the rectangle
//! 50..550 × 0..700, `space` empty, all advancing 600.

mod common;

use kasane_pdf::{render_page, Document, Error, RenderOptions};

fn program() -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/cff_charset/isoadobe.bin",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// `( !)` at 100 pt from (20, 50) on a 300 × 200 page.
fn pdf(program: &[u8], encoding: &str) -> Vec<u8> {
    let content = b"BT /F0 100 Tf 20 50 Td ( !) Tj ET";
    common::build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, common::stream_obj("", content)),
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /Test /FirstChar 32 /LastChar 33 \
                 /Widths [600 600] {encoding} /FontDescriptor 6 0 R >>"
            )
            .into_bytes(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontBBox [0 0 600 700] \
              /ItalicAngle 0 /Ascent 700 /Descent 0 /CapHeight 700 /StemV 80 /FontFile3 7 0 R >>"
                .to_vec(),
        ),
        (7, common::stream_obj("/Subtype /Type1C", program)),
    ])
}

fn ink_bbox(data: &[u8]) -> Result<Option<(u32, u32, u32, u32)>, Error> {
    let doc = Document::from_bytes(data)?;
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new())?;
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in 0..r.height() {
        for x in 0..r.width() {
            if r.pixels()[(y * r.stride() as u32 + x * 4) as usize] < 128 {
                b = Some(match b {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    Ok(b)
}

#[test]
fn glyphs_of_an_isoadobe_charset_program_are_found_by_name() {
    // The space draws nothing; `exclam` is the 50 × 70 pt rectangle of
    // the second glyph, x 80 + 5 .. 80 + 55, page y 50..120 (rows 80..150).
    for encoding in ["/Encoding /WinAnsiEncoding", ""] {
        let b = ink_bbox(&pdf(&program(), encoding)).unwrap();
        assert_eq!(b, Some((85, 80, 135, 150)), "encoding {encoding:?}");
    }
}

#[test]
fn expert_charset_is_refused_not_drawn_as_notdef() {
    // Charset operand 1 (Expert): the standard-string names `space` and
    // `exclam` are not resolved through it; refuse rather than guess.
    let mut p = program();
    let at = p.windows(2).position(|w| w == [0x8b, 0x0f]).unwrap();
    p[at] = 0x8c;
    let e = ink_bbox(&pdf(&p, "/Encoding /WinAnsiEncoding")).unwrap_err();
    assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
}
