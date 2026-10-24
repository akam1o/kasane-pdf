//! Type 3 fonts — glyphs as `/CharProcs` content streams (§9.6.4).
//!
//! A Type3 glyph is painted by *executing* its CharProc under
//! `ctm × trm × FontMatrix`, not by extracting an outline. Covers:
//!   * a `d1` CharProc painting a filled rect/triangle
//!   * `d0` (width-only) vs `d1` (width + bbox)
//!   * advance from `/Widths`
//!   * the `in_text` scope reset that lets a CharProc hold `BT … ET`
//!   * nested resource resolution (`/Resources` on the font)
//!
//! PDFs are built inline; CharProc bodies are ordinary content ops.

use kasane_pdf::{extract_text, render_page, Document, Error, RenderOptions, TextOptions};

/// Build a 1-page PDF with a `/Type3` font `/F0` whose CharProcs are
/// `[(name, body)]`. `extra` is appended to the page `/Resources`.
fn pdf_type3(stream: &[u8], charprocs: &[(&str, &str)], extra_objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    // CharProc objects start at 10; the /CharProcs dict (9) maps
    // names to them. /Encoding (8) maps codes 97.. to the names.
    let mut cp_entries = String::new();
    let mut diffs = String::new();
    for (i, (name, _)) in charprocs.iter().enumerate() {
        cp_entries.push_str(&format!("/{name} {} 0 R ", 10 + i));
        diffs.push_str(&format!("{} /{name} ", 97 + i));
    }
    let encoding = format!("<< /Type /Encoding /Differences [97 {diffs}] >>");
    let charprocs_dict = format!("<< {cp_entries} >>");
    let first = 97u32;
    let last = 96 + charprocs.len() as u32;
    let widths = vec!["600"; charprocs.len()].join(" ");
    let font = format!(
        "<< /Type /Font /Subtype /Type3 /Name /F0 \
           /FontBBox [0 0 750 750] /FontMatrix [0.001 0 0 0.001 0 0] \
           /Encoding 8 0 R /CharProcs 9 0 R \
           /FirstChar {first} /LastChar {last} /Widths [{widths}] >>"
    );

    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F0 4 0 R >> >> /Contents 5 0 R >>"
                .into(),
        ),
        (4, font.into_bytes()),
        (
            5,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (8, encoding.into_bytes()),
        (9, charprocs_dict.into_bytes()),
    ];
    for (i, (_, body)) in charprocs.iter().enumerate() {
        bodies.push((
            10 + i as u32,
            [
                format!("<< /Length {} >>\nstream\n", body.len()).into_bytes(),
                body.as_bytes().to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ));
    }
    bodies.extend(extra_objs.iter().map(|(n, b)| (*n, b.clone())));

    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = bodies.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    let mut lines = vec![b"0000000000 65535 f \n".to_vec()];
    let mut by_n: std::collections::BTreeMap<u32, usize> = offsets.iter().cloned().collect();
    for n in 1..size {
        let off = by_n.remove(&n).unwrap_or(0);
        lines.push(format!("{off:010} 00000 n \n").into_bytes());
    }
    for l in lines {
        out.extend_from_slice(&l);
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn render(pdf: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn inked(page: &kasane_pdf::RenderedPage) -> usize {
    let mut n = 0;
    for y in 0..page.pixmap().height() {
        for x in 0..page.pixmap().width() {
            let p = page.pixmap().pixel(x, y).unwrap();
            if (p.red(), p.green(), p.blue()) != (255, 255, 255) {
                n += 1;
            }
        }
    }
    n
}

// --------------------------------------------------------------------
// Basic CharProc rendering
// --------------------------------------------------------------------

#[test]
fn type3_rect_glyph_paints() {
    // code 97 → /square → d1 + a filled 750-unit rect. FontMatrix
    // 0.001 → 0.75 user units × Tfs.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (a) Tj ET",
        &[("square", "600 0 0 0 750 750 d1 0 0 750 750 re f")],
        &[],
    );
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 50, "Type3 d1 rect drew no ink");
}

#[test]
fn type3_d0_glyph_paints() {
    // d0 (width only, no bbox) — still paints.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (a) Tj ET",
        &[("sq", "600 0 d0 0 0 750 750 re f")],
        &[],
    );
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 50, "Type3 d0 rect drew no ink");
}

#[test]
fn type3_triangle_and_fill() {
    // Two glyphs — a filled rect and a stroked triangle.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (ab) Tj ET",
        &[
            ("square", "600 0 0 0 750 750 d1 0 0 750 750 re f"),
            ("tri", "600 0 0 0 750 750 d1 0 0 m 375 750 l 750 0 l f"),
        ],
        &[],
    );
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 100, "two Type3 glyphs should draw ink");
}

#[test]
fn type3_colored_glyph_uses_paint_ops() {
    // The CharProc sets its own colour — a red fill.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (a) Tj ET",
        &[("sq", "600 0 0 0 750 750 d1 1 0 0 rg 0 0 750 750 re f")],
        &[],
    );
    let r = render(&pdf).unwrap();
    // Sample the middle — should be red-tinged, not black.
    let mut red = false;
    for y in 0..r.pixmap().height() {
        for x in 0..r.pixmap().width() {
            let p = r.pixmap().pixel(x, y).unwrap();
            if p.red() > 150 && p.green() < 100 && p.blue() < 100 {
                red = true;
            }
        }
    }
    assert!(red, "CharProc colour not applied");
}

#[test]
fn type3_charproc_with_text_scope() {
    // A CharProc containing `BT … ET` (a text object inside a glyph)
    // must not trip the outer "BT inside text" error — in_text resets.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (a) Tj ET",
        &[(
            "sq",
            "600 0 0 0 750 750 d1 0 0 750 750 re f BT 10 10 Td (x) Tj ET",
        )],
        &[],
    );
    // The inner Tj would need a font — none resolvable → it errors on
    // the missing /Font, *not* on "BT inside text". Accept either a
    // paintable glyph or a font-resolution error.
    match render(&pdf) {
        Ok(r) => assert!(inked(&r) > 0),
        Err(Error::Structure(m)) => {
            assert!(!m.contains("BT inside"), "in_text leaked: {m}");
        }
        Err(_) => {}
    }
}

#[test]
fn type3_advances_by_widths() {
    // Two glyphs at /Widths 600 → second glyph's origin offset by
    // 600/1000 × Tfs in text space.
    let pdf = pdf_type3(
        b"BT /F0 10 Tf 50 100 Td (ab) Tj ET",
        &[
            ("a", "600 0 0 0 750 750 d1 0 0 750 750 re f"),
            ("b", "600 0 0 0 750 750 d1 0 0 750 750 re f"),
        ],
        &[],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    assert_eq!(t.chars.len(), 2);
    let dx = t.chars[1].origin.0 - t.chars[0].origin.0;
    // 600/1000 × 10pt = 6 user units advance.
    assert!((dx - 6.0).abs() < 0.01, "advance {dx} ≠ 6.0");
}

#[test]
fn type3_widths_pass_through_a_non_default_font_matrix() {
    // /FontMatrix [1 0 0 -1 0 0] (dvipdf bitmap fonts): /Widths are in
    // glyph space, so a width of 6 at Tfs 1 advances 6 user units —
    // not 6/1000.
    let pdf = pdf_type3(
        b"BT /F0 1 Tf 50 100 Td (ab) Tj ET",
        &[
            ("a", "6 0 0 0 5 5 d1 0 0 5 -5 re f"),
            ("b", "6 0 0 0 5 5 d1 0 0 5 -5 re f"),
        ],
        &[],
    )
    .replace(
        b"/FontMatrix [0.001 0 0 0.001 0 0]",
        b"/FontMatrix [1 0 0 -1 0 0]",
    )
    .replace(b"/Widths [600 600]", b"/Widths [6 6]");
    let doc = Document::from_bytes_recovering(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    assert_eq!(t.chars.len(), 2);
    let dx = t.chars[1].origin.0 - t.chars[0].origin.0;
    assert!((dx - 6.0).abs() < 0.01, "advance {dx} ≠ 6.0");
}

trait ReplaceBytes {
    fn replace(self, from: &[u8], to: &[u8]) -> Vec<u8>;
}
impl ReplaceBytes for Vec<u8> {
    fn replace(self, from: &[u8], to: &[u8]) -> Vec<u8> {
        let pos = self
            .windows(from.len())
            .position(|w| w == from)
            .expect("pattern present");
        let mut out = self[..pos].to_vec();
        out.extend_from_slice(to);
        out.extend_from_slice(&self[pos + from.len()..]);
        out
    }
}

#[test]
fn type3_no_charproc_errors() {
    // /CharProcs missing → Structure error at font load.
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    out.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F0 4 0 R >> >> /Contents 5 0 R >>\nendobj\n");
    out.extend_from_slice(b"4 0 obj\n<< /Type /Font /Subtype /Type3 /FontBBox [0 0 750 750] /FontMatrix [0.001 0 0 0.001 0 0] /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [600] >>\nendobj\n");
    let s = b"BT /F0 40 Tf 50 100 Td (a) Tj ET";
    out.extend_from_slice(format!("5 0 obj\n<< /Length {} >>\nstream\n", s.len()).as_bytes());
    out.extend_from_slice(s);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    // crude offsets — only need objects present; pad others as free
    let offs: Vec<usize> = {
        let text = String::from_utf8_lossy(&out);
        (1..=5)
            .map(|n| text.find(&format!("{n} 0 obj")).unwrap_or(0))
            .collect()
    };
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    match render(&out) {
        Err(Error::Structure(m)) => assert!(m.contains("CharProcs"), "got {m}"),
        other => panic!("missing /CharProcs should be Structure, got {other:?}"),
    }
}

/// The CharProc must land at the text position: glyph → text space
/// via `/FontMatrix` *first*, then `Trm`. Composing the other way
/// scaled the Tm translation by 0.001 and drew every Type3 glyph at
/// the page origin (the corpus file `simpletype3font` scored SSIM
/// 0.997 while being completely misplaced — blank pages hide it).
#[test]
fn type3_glyph_lands_at_text_position() {
    // 'a' = filled 750-unit square at Tfs 40 → 30×30 user units with
    // its origin at Td (50,100): page x 50..80, y 100..130.
    let pdf = pdf_type3(
        b"BT /F0 40 Tf 50 100 Td (a) Tj ET",
        &[("a", "600 0 0 0 750 750 d1 0 0 750 750 re f")],
        &[],
    );
    let r = render(&pdf).unwrap();
    let (mut minx, mut miny, mut maxx, mut maxy) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..r.pixmap().height() {
        for x in 0..r.pixmap().width() {
            let p = r.pixmap().pixel(x, y).unwrap();
            if (p.red(), p.green(), p.blue()) != (255, 255, 255) {
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
            }
        }
    }
    // pixmap y = 200 - page y → rows 70..100
    assert!((49..=51).contains(&minx), "minx {minx}");
    assert!((79..=81).contains(&maxx), "maxx {maxx}");
    assert!((69..=71).contains(&miny), "miny {miny}");
    assert!((99..=101).contains(&maxy), "maxy {maxy}");
}
