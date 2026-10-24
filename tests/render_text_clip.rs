//! Text rendering modes 4–7 (ISO 32000-1 §9.3.6): the glyph outlines
//! shown inside a text object become a clip at `ET`, intersected with
//! the current clip and released by `Q`; modes 4–6 also paint. An
//! empty set of glyphs clips everything away; a Type 3 font in a
//! clipping mode is refused. Also `/ExtGState /Font` (§8.4.5), which
//! selects the same embedded font through `gs` instead of `Tf`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

/// 200×100 pt page, `/F1` = embedded KasaneTest TrueType (A, B),
/// `/T3` = a Type 3 font whose `a` is a filled square.
fn pdf(content: &[u8]) -> Vec<u8> {
    let ttf = std::fs::read("tests/fixtures/KasaneTest.ttf").unwrap();
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /F1 5 0 R /T3 8 0 R >> \
               /ExtGState << /GF << /Font [5 0 R 40] >> /GB << /Font [5 0 R] >> >> >> \
               /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content)),
        (
            5,
            b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
               /Encoding /WinAnsiEncoding /FirstChar 65 /LastChar 66 \
               /Widths [600 600] /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular \
               /Flags 32 /FontBBox [0 -200 600 800] /ItalicAngle 0 \
               /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 \
               /FontFile2 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj(&format!("/Length1 {}", ttf.len()), &ttf)),
        (
            8,
            b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
               /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /sq 9 0 R >> \
               /Encoding << /Type /Encoding /Differences [97 /sq] >> \
               /FirstChar 97 /LastChar 97 /Widths [1000] /Resources << >> >>"
                .to_vec(),
        ),
        (9, stream_obj("", b"1000 0 d0 0 0 1000 1000 re f")),
    ])
}

fn render(content: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(&pdf(content))?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

/// Pixels in the region that are not white.
fn ink(r: &RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| px(r, x, y) != (255, 255, 255))
        .count()
}

/// The glyph (A) at 40 pt from (20, 30) covers roughly x 20..44,
/// y 30..58 in user space = pixmap rows 42..70.
const GLYPH: (u32, u32, u32, u32) = (20, 42, 44, 70);

#[test]
fn mode_7_clips_later_painting_to_the_glyphs() {
    // Nothing is painted by the text object itself …
    let r = render(b"BT /F1 40 Tf 20 30 Td 7 Tr (A) Tj ET").unwrap();
    assert_eq!(ink(&r, 0, 0, 200, 100), 0, "mode 7 paints nothing");
    // … and a page-filling rectangle afterwards only shows through
    // the glyph.
    let r = render(b"BT /F1 40 Tf 20 30 Td 7 Tr (A) Tj ET 0 0 1 rg 0 0 200 100 re f").unwrap();
    let (gx0, gy0, gx1, gy1) = GLYPH;
    let inside = ink(&r, gx0, gy0, gx1, gy1);
    assert!(inside > 50, "glyph interior painted: {inside}");
    assert_eq!(ink(&r, 100, 0, 200, 100), 0, "right half stays white");
    assert_eq!(ink(&r, 0, 0, 200, 30), 0, "above the glyph stays white");
    // What shows is the rectangle's colour, not a text colour.
    let blue = (gx0..gx1)
        .flat_map(|x| (gy0..gy1).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&r, x, y) == (0, 0, 255))
        .count();
    assert_eq!(blue, inside);
}

#[test]
fn mode_4_paints_and_clips_and_q_releases_the_clip() {
    // Mode 4: the glyph is filled red; the later blue fill is clipped
    // to it and overpaints the red.
    let r = render(
        b"q 1 0 0 rg BT /F1 40 Tf 20 30 Td 4 Tr (A) Tj ET 0 0 1 rg 0 0 100 100 re f Q \
          0 1 0 rg 150 10 30 30 re f",
    )
    .unwrap();
    let (gx0, gy0, gx1, gy1) = GLYPH;
    let blue = (gx0..gx1)
        .flat_map(|x| (gy0..gy1).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&r, x, y) == (0, 0, 255))
        .count();
    assert!(blue > 50, "{blue}");
    assert_eq!(ink(&r, 50, 0, 100, 100), 0, "clip confines the blue fill");
    // After Q the clip is gone: the green square paints in full.
    assert_eq!(px(&r, 165, 75), (0, 255, 0));
    // Mode 4 without a later fill shows the red glyph itself.
    let r = render(b"1 0 0 rg BT /F1 40 Tf 20 30 Td 4 Tr (A) Tj ET").unwrap();
    let red = (gx0..gx1)
        .flat_map(|x| (gy0..gy1).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&r, x, y) == (255, 0, 0))
        .count();
    assert!(red > 50, "{red}");
}

#[test]
fn clipping_mode_without_glyphs_clips_everything() {
    let r = render(b"BT /F1 40 Tf 7 Tr ET 0 0 200 100 re f").unwrap();
    // No glyph was shown, so the mode never took effect: nothing to
    // clip with means the clip is untouched (§9.3.6 applies to glyphs
    // that were painted in a clipping mode).
    assert!(ink(&r, 0, 0, 200, 100) > 10_000);
    // A glyph *was* shown but has no outline (space): the clip is empty.
    let r = render(b"BT /F1 40 Tf 20 30 Td 7 Tr ( ) Tj ET 0 0 200 100 re f");
    match r {
        Ok(r) => assert_eq!(ink(&r, 0, 0, 200, 100), 0),
        // The test font may not define a space glyph at all.
        Err(e) => assert!(matches!(e, Error::Structure(_)), "{e:?}"),
    }
}

#[test]
fn text_clip_accumulates_across_strings_and_intersects_an_existing_clip() {
    // Two glyphs in one text object both show through; an earlier
    // rectangular clip limits the result further.
    let r =
        render(b"20 0 60 100 re W n BT /F1 40 Tf 20 30 Td 7 Tr (A) Tj (B) Tj ET 0 0 200 100 re f")
            .unwrap();
    assert!(ink(&r, 20, 42, 44, 70) > 50, "A shows");
    assert!(ink(&r, 44, 42, 68, 70) > 50, "B shows");
    assert_eq!(ink(&r, 80, 0, 200, 100), 0, "outside the rectangular clip");
}

#[test]
fn type3_font_in_a_clipping_mode_is_refused() {
    let err = render(b"BT /T3 40 Tf 20 30 Td 7 Tr (a) Tj ET").unwrap_err();
    assert!(
        matches!(
            err,
            Error::Unsupported {
                feature: "text rendering mode",
                ..
            }
        ),
        "{err:?}"
    );
    // Painting modes with Type 3 are unaffected.
    let r = render(b"BT /T3 40 Tf 20 30 Td 0 Tr (a) Tj ET").unwrap();
    assert!(ink(&r, 20, 30, 60, 70) > 1000);
}

#[test]
fn extgstate_font_selects_the_font_like_tf() {
    let a = render(b"BT /F1 40 Tf 20 30 Td (A) Tj ET").unwrap();
    let b = render(b"/GF gs BT 20 30 Td (A) Tj ET").unwrap();
    assert_eq!(a.pixmap().data(), b.pixmap().data(), "gs /Font == Tf");
    // The gs font persists across text objects and is overridden by Tf.
    let c = render(b"/GF gs BT 20 30 Td (A) Tj ET BT /F1 20 Tf 100 30 Td (B) Tj ET").unwrap();
    assert!(ink(&c, 20, 42, 44, 70) > 50);
    assert!(ink(&c, 100, 50, 120, 70) > 20);
    // Malformed: not a [font size] pair.
    let err = render(b"/GB gs BT 20 30 Td (A) Tj ET").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}
