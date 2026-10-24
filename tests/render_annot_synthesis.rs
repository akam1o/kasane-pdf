//! Markup annotations without `/AP` (ISO 32000-1 §12.5.6): Square,
//! Circle, Line, Polygon/PolyLine, Ink, Highlight/Underline/StrikeOut/
//! Squiggly and FreeText get a synthesised appearance; other subtypes
//! stay refused. 200×200 pt page at 72 dpi: pixmap (x, y) = user
//! (x, 200 − y).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

fn pdf(content: &[u8], annots: &[&str]) -> Vec<u8> {
    let refs: String = (0..annots.len())
        .map(|i| format!("{} 0 R ", 10 + i))
        .collect();
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Annots [{refs}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    for (i, a) in annots.iter().enumerate() {
        all.push((10 + i as u32, a.as_bytes().to_vec()));
    }
    build_pdf(&all)
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol
        && (a.1 as i32 - b.1 as i32).abs() <= tol
        && (a.2 as i32 - b.2 as i32).abs() <= tol
}

/// Dark-ish pixels in a pixmap rectangle.
fn ink(r: &RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = r.pixmap.pixel(x, y).unwrap();
            (p.red() as u32 + p.green() as u32 + p.blue() as u32) < 600
        })
        .count()
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const RED: (u8, u8, u8) = (255, 0, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);

#[test]
fn square_border_interior_and_width() {
    // Rect 50..150 × 50..150, border 10 pt red, interior blue.
    let a = "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /IC [0 0 1] \
             /BS << /W 10 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), BLUE, "interior");
    assert_eq!(px(&r, 54, 100), RED, "left border");
    assert_eq!(px(&r, 100, 54), RED, "top border");
    assert_eq!(px(&r, 40, 100), WHITE, "outside");
    assert_eq!(px(&r, 100, 160), WHITE, "outside");
    // /Border [0 0 4] is the older spelling of the width; no /IC →
    // the inside stays white.
    let a = "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /Border [0 0 4] >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), WHITE);
    assert_eq!(px(&r, 52, 100), RED);
    assert_eq!(px(&r, 58, 100), WHITE, "border is 4 wide, not 10");
    // Transparent stroke (empty /C) and no /IC: nothing is drawn, but
    // the annotation is handled (no error).
    let a = "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [] >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(ink(&r, 0, 0, 200, 200), 0);
}

#[test]
fn circle_is_inscribed() {
    let a = "<< /Type /Annot /Subtype /Circle /Rect [50 50 150 150] /C [0 0 1] /IC [1 0 0] \
             /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), RED, "centre filled");
    assert_eq!(
        px(&r, 55, 55),
        WHITE,
        "corner of the rect is outside the ellipse"
    );
    assert!(
        near(px(&r, 100, 51), BLUE, 60),
        "top of the ellipse is the stroke: {:?}",
        px(&r, 100, 51)
    );
}

#[test]
fn line_with_endings_leader_lines_and_empty_rect() {
    // Horizontal line y = 100 from x 40 to 160, 4 pt wide, closed
    // arrow at the end, filled with /IC.
    let a = "<< /Type /Annot /Subtype /Line /Rect [30 80 170 120] /L [40 100 160 100] \
             /C [1 0 0] /IC [1 0 0] /BS << /W 4 >> /LE [/None /ClosedArrow] >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), RED, "on the line");
    assert_eq!(px(&r, 100, 90), WHITE, "above it");
    // Arrow head widens the stroke near the end point.
    assert!(
        ink(&r, 148, 90, 160, 110) > ink(&r, 100, 90, 112, 110),
        "arrow head"
    );
    // An empty /Rect (pdf.js corpus): the line is still drawn, within
    // its own bounds.
    let a = "<< /Type /Annot /Subtype /Line /Rect [0 0 0 0] /L [20 150 180 150] /C [1 0 0] \
             /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 50), RED);
    assert_eq!(px(&r, 100, 60), WHITE);
    // Leader lines: /LL 20 shifts the line 20 pt to the left of its
    // direction (upwards for a left-to-right line) and draws the two
    // perpendicular leaders.
    let a = "<< /Type /Annot /Subtype /Line /Rect [0 0 200 200] /L [40 100 160 100] /C [1 0 0] \
             /LL 20 /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 80), RED, "line moved up by 20");
    assert_eq!(px(&r, 100, 100), WHITE, "nothing at the original y");
    assert_eq!(px(&r, 40, 90), RED, "leader at the start point");
    // Clipping: the appearance never leaves /Rect.
    let a = "<< /Type /Annot /Subtype /Line /Rect [60 90 140 110] /L [0 100 200 100] /C [1 0 0] \
             /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
    assert_eq!(px(&r, 30, 100), WHITE, "clipped to /Rect");
}

#[test]
fn polygon_polyline_and_ink() {
    let a = "<< /Type /Annot /Subtype /Polygon /Rect [0 0 200 200] \
             /Vertices [50 50 150 50 100 150] /C [0 0 1] /IC [1 0 0] /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 120), RED, "inside the triangle");
    assert_eq!(px(&r, 60, 70), WHITE, "outside");
    assert!(
        near(px(&r, 100, 149), BLUE, 60),
        "bottom edge stroked: {:?}",
        px(&r, 100, 149)
    );
    let a = "<< /Type /Annot /Subtype /PolyLine /Rect [0 0 200 200] \
             /Vertices [50 50 150 50 100 150] /C [0 0 1] /IC [1 0 0] /BS << /W 2 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 120), WHITE, "a polyline is not filled");
    assert!(near(px(&r, 100, 149), BLUE, 60));
    // Ink: two strokes, round caps; a point stroke draws a dot.
    let a = "<< /Type /Annot /Subtype /Ink /Rect [0 0 200 200] \
             /InkList [[20 100 180 100] [100 20 100 60] [150 150]] /C [1 0 0] /BS << /W 6 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
    assert_eq!(px(&r, 100, 160), RED);
    assert!(
        ink(&r, 146, 46, 154, 54) > 0,
        "single-point stroke is a dot"
    );
    assert_eq!(px(&r, 50, 50), WHITE);
}

#[test]
fn text_markups_follow_quadpoints() {
    // Text at y 100..120 (quad upper-left (50,120), upper-right (150,120),
    // lower-left (50,100), lower-right (150,100)).
    let quad = "/QuadPoints [50 120 150 120 50 100 150 100]";
    let black_bar = b"0 g 50 105 100 10 re f";
    let hl =
        format!("<< /Type /Annot /Subtype /Highlight /Rect [50 100 150 120] {quad} /C [1 1 0] >>");
    let r = render(&pdf(black_bar, &[&hl])).unwrap();
    assert_eq!(px(&r, 100, 82), (255, 255, 0), "highlight over white");
    assert!(
        near(px(&r, 100, 90), (0, 0, 0), 2),
        "multiply keeps black text black"
    );
    assert_eq!(px(&r, 30, 90), WHITE);
    let ul =
        format!("<< /Type /Annot /Subtype /Underline /Rect [50 100 150 120] {quad} /C [1 0 0] >>");
    let r = render(&pdf(b"", &[&ul])).unwrap();
    assert!(ink(&r, 60, 97, 140, 100) > 50, "line along the bottom");
    assert_eq!(ink(&r, 60, 80, 140, 95), 0, "nothing higher up");
    let so =
        format!("<< /Type /Annot /Subtype /StrikeOut /Rect [50 100 150 120] {quad} /C [1 0 0] >>");
    let r = render(&pdf(b"", &[&so])).unwrap();
    assert!(ink(&r, 60, 88, 140, 92) > 50, "line through the middle");
    assert_eq!(ink(&r, 60, 96, 140, 100), 0);
    let sq =
        format!("<< /Type /Annot /Subtype /Squiggly /Rect [50 100 150 120] {quad} /C [0 0 1] >>");
    let r = render(&pdf(b"", &[&sq])).unwrap();
    assert!(ink(&r, 60, 93, 140, 100) > 30, "zigzag along the bottom");
    assert_eq!(ink(&r, 60, 80, 140, 92), 0);
    // A /QuadPoints count that is not a multiple of 8 is malformed.
    let bad = "<< /Type /Annot /Subtype /Highlight /Rect [50 100 150 120] \
               /QuadPoints [1 2 3] /C [1 1 0] >>";
    let err = render(&pdf(b"", &[bad])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn constant_alpha_composites_the_annotation_once() {
    // Two overlapping ink strokes at /CA 0.5: the overlap is as light
    // as the rest (group alpha), not darker (per-stroke alpha).
    let a = "<< /Type /Annot /Subtype /Ink /Rect [0 0 200 200] \
             /InkList [[20 100 180 100] [100 20 100 180]] /C [0 0 0] /CA 0.5 /BS << /W 8 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    let on_h = px(&r, 60, 100);
    let on_cross = px(&r, 100, 100);
    assert!(near(on_h, (128, 128, 128), 6), "{on_h:?}");
    assert!(near(on_cross, on_h, 6), "overlap {on_cross:?} vs {on_h:?}");
    assert_eq!(px(&r, 60, 60), WHITE);
}

#[test]
fn free_text_draws_background_border_and_contents() {
    let a = "<< /Type /Annot /Subtype /FreeText /Rect [20 100 180 140] /C [1 0 0] \
             /DA (/Helv 12 Tf 0 0 1 rg) /Contents (Hello) /BS << /W 1 >> >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert_eq!(px(&r, 170, 85), RED, "background from /C");
    let blue = (24..100)
        .flat_map(|x| (62..98).map(move |y| (x, y)))
        .any(|(x, y)| {
            let p = r.pixmap.pixel(x, y).unwrap();
            p.blue() > 200 && p.red() < 120
        });
    assert!(blue, "text in the /DA colour");
    assert!(
        near(px(&r, 20, 85), BLUE, 60),
        "border in the text colour: {:?}",
        px(&r, 20, 85)
    );
    // Without /DA (pdfium corpus) the Helvetica fallback at 12 pt is used.
    let a =
        "<< /Type /Annot /Subtype /FreeText /Rect [20 100 180 140] /C [1 0 0] /Contents (Hi) >>";
    let r = render(&pdf(b"", &[a])).unwrap();
    assert!(ink(&r, 22, 62, 100, 98) > 10);
    assert!(r
        .font_substitutions
        .iter()
        .any(|s| s.base_font.contains("Helvetica")));
    // Contents outside the Latin range are refused, not mangled.
    let a = "<< /Type /Annot /Subtype /FreeText /Rect [20 100 180 140] /Contents <FEFF65E5672C> >>";
    let err = render(&pdf(b"", &[a])).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn existing_appearance_wins_and_other_subtypes_are_refused() {
    // A Square with an /AP paints the stream (green), not a synthesised red border.
    let ap = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100]",
        b"0 1 0 rg 0 0 100 100 re f",
    );
    let refs = "10 0 R";
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [{refs}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"")),
        (
            10,
            b"<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /AP << /N 11 0 R >> >>"
                .to_vec(),
        ),
        (11, ap),
    ]);
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 100, 100), (0, 255, 0));
    assert_eq!(px(&r, 52, 100), (0, 255, 0));
    // Hidden flag skips the synthesis too.
    let a = "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /F 2 >>";
    let r = render(&self::pdf(b"", &[a])).unwrap();
    assert_eq!(ink(&r, 0, 0, 200, 200), 0);
    // /Redact draws nothing; /Text, /Stamp, /FileAttachment are refused.
    let a = "<< /Type /Annot /Subtype /Redact /Rect [50 50 150 150] /C [1 0 0] >>";
    let r = render(&self::pdf(b"", &[a])).unwrap();
    assert_eq!(ink(&r, 0, 0, 200, 200), 0);
    for st in ["Text", "Stamp", "FileAttachment", "Caret"] {
        let a = format!("<< /Type /Annot /Subtype /{st} /Rect [50 50 150 150] /C [1 0 0] >>");
        let err = render(&self::pdf(b"", &[&a])).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{st}: {err:?}");
    }
    // Painting order: annotations go over the page content.
    let a = "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /IC [1 0 0] /C [] >>";
    let r = render(&self::pdf(b"0 0 1 rg 0 0 200 200 re f", &[a])).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
    assert_eq!(px(&r, 20, 20), BLUE);
}

#[test]
fn malformed_geometry_is_a_structure_error_and_counts_are_bounded() {
    for a in [
        "<< /Type /Annot /Subtype /Line /Rect [0 0 200 200] /C [1 0 0] >>",
        "<< /Type /Annot /Subtype /Line /Rect [0 0 200 200] /L [1 2 3] /C [1 0 0] >>",
        "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0] >>",
        "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /BS << /W -1 >> >>",
        "<< /Type /Annot /Subtype /Square /Rect [50 50 150 150] /C [1 0 0] /RD [60 0 60 0] >>",
        "<< /Type /Annot /Subtype /Ink /Rect [0 0 200 200] /InkList 5 /C [1 0 0] >>",
    ] {
        let err = render(&pdf(b"", &[a])).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{a}: {err:?}");
    }
    let many: String = (0..70_000)
        .map(|i| format!("{} {} ", i % 200, (i / 7) % 200))
        .collect();
    let a = format!(
        "<< /Type /Annot /Subtype /Ink /Rect [0 0 200 200] /InkList [[{many}]] /C [1 0 0] >>"
    );
    let err = render(&pdf(b"", &[&a])).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}
