//! A Type 1 font program's own `FontMatrix` (§9.6.2) maps its glyph
//! space to text space — for eexec (`/FontFile`) programs it is in the
//! cleartext font dictionary, for CFF (`/FontFile3 /Type1C`) in the Top
//! DICT. The fixtures (`tests/fixtures/fontmatrix/`, written by
//! `make_fontmatrix.py`) all draw glyph `A` as the rectangle
//! 0..500 × 0..700 of a 1000-unit em with advance 600: the 2048-unit
//! programs state it in 2048 units with a 1/2048 matrix and must
//! render identically; the oblique and wide ones differ by their matrix.

use kasane_pdf::{extract_text, render_page, Document, Error, RenderOptions, TextOptions};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/fontmatrix/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// A 300 × 200 pt page showing `AA` at 100 pt from (20, 50) in a
/// Type 1 font with the given program (`eexec`: `/FontFile`, else
/// `/FontFile3 /Type1C`).
fn pdf(program: &[u8], eexec: bool) -> Vec<u8> {
    let content = b"BT /F0 100 Tf 20 50 Td (AA) Tj ET";
    let file = if eexec {
        format!("<< /Length {} /Length1 {} >>", program.len(), program.len())
    } else {
        format!("<< /Length {} /Subtype /Type1C >>", program.len())
    };
    let key = if eexec { "FontFile" } else { "FontFile3" };
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
           /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(),
            content.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Test /FirstChar 65 /LastChar 65 \
           /Widths [600] /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>"
            .to_vec(),
        format!(
            "<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontBBox [0 0 1000 1000] \
             /ItalicAngle 0 /Ascent 700 /Descent 0 /CapHeight 700 /StemV 80 /{key} 7 0 R >>"
        )
        .into_bytes(),
        [
            format!("{file}\nstream\n").into_bytes(),
            program.to_vec(),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Inked pixel bounds `(x0, y0, x1, y1)` (top-left origin, exclusive
/// end) at 72 dpi — one pixel per point.
fn ink_bbox(data: &[u8]) -> Result<(u32, u32, u32, u32), Error> {
    let doc = Document::from_bytes(data)?;
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0))?;
    let (w, h) = (r.width(), r.height());
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let i = (y * r.stride() as u32 + x * 4) as usize;
            if r.pixels()[i] < 128 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    Ok((x0, y0, x1, y1))
}

fn close(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0.abs_diff(b.0) <= 1
        && a.1.abs_diff(b.1) <= 1
        && a.2.abs_diff(b.2) <= 1
        && a.3.abs_diff(b.3) <= 1
}

/// Two `A`s: x 20..70 and 80..130 (advance 60 pt from /Widths), y
/// 50..120 pt → rows 80..150 on the 200-pt page.
const UPRIGHT: (u32, u32, u32, u32) = (20, 80, 130, 150);

#[test]
fn eexec_and_cff_scale_by_their_font_matrix() {
    for (name, eexec) in [
        ("t1_1000.bin", true),
        ("t1_2048.bin", true),
        ("t1_2048_private.bin", true),
        ("t1_default.bin", true),
        ("cff_1000.bin", false),
        ("cff_2048.bin", false),
    ] {
        let b = ink_bbox(&pdf(&fixture(name), eexec)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(close(b, UPRIGHT), "{name}: ink {b:?}, expected {UPRIGHT:?}");
    }
}

#[test]
fn oblique_and_non_uniform_matrices_are_applied_in_full() {
    // [0.001 0 0.0005 0.001]: x' = x + 0.5 y — the top edge (70 pt up)
    // moves 35 pt right; the second glyph ends at 130 + 35.
    for (name, eexec) in [("t1_oblique.bin", true), ("cff_oblique.bin", false)] {
        let b = ink_bbox(&pdf(&fixture(name), eexec)).unwrap();
        assert!(close(b, (20, 80, 165, 150)), "{name}: {b:?}");
    }
    // [0.002 0 0 0.001]: each glyph 100 pt wide, still advancing 60 pt.
    let b = ink_bbox(&pdf(&fixture("t1_wide.bin"), true)).unwrap();
    assert!(close(b, (20, 80, 180, 150)), "wide: {b:?}");
}

#[test]
fn singular_font_matrix_is_a_structure_error() {
    let e = ink_bbox(&pdf(&fixture("t1_singular.bin"), true)).unwrap_err();
    assert!(matches!(e, Error::Structure(_)), "{e:?}");
}

#[test]
fn text_positions_follow_widths_and_bounds_follow_the_matrix() {
    for (name, eexec) in [
        ("t1_1000.bin", true),
        ("t1_2048.bin", true),
        ("cff_2048.bin", false),
    ] {
        let data = pdf(&fixture(name), eexec);
        let doc = Document::from_bytes(&data).unwrap();
        let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
        assert_eq!(t.chars.len(), 2, "{name}");
        let (c0, c1) = (&t.chars[0], &t.chars[1]);
        assert_eq!(c0.unicode.as_deref(), Some("A"), "{name}");
        // Advance comes from /Widths (600/1000 em), not the program.
        assert!(
            (c1.origin.0 - c0.origin.0 - 60.0).abs() < 1e-6,
            "{name}: {:?}",
            c1.origin
        );
        assert!((c0.advance.0 - 60.0).abs() < 1e-6, "{name}");
        let (x0, y0, x1, y1) = c0.bounds.expect("outline bounds");
        for (got, want) in [(x0, 20.0), (y0, 50.0), (x1, 70.0), (y1, 120.0)] {
            assert!((got - want).abs() < 0.05, "{name}: bounds {:?}", c0.bounds);
        }
    }
}
