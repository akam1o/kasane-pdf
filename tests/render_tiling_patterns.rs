//! Tiling patterns (ISO 32000-1 §8.7.3): `/PatternType 1` cells placed
//! on the `/XStep`/`/YStep` grid in pattern space (`/Matrix` × the base
//! CTM), coloured (`/PaintType 1`) and uncoloured (`/PaintType 2`), as
//! fill, stroke and text colour, under alpha, clipping and nesting,
//! plus the limits and refusals. 100×100 pt page at 72 dpi: pixmap
//! (x, y) = user (x, 100 − y).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

/// A page whose `/Pattern` resources are `patterns` (name → object
/// number), with extra ExtGState entries and objects.
fn page(
    content: &[u8],
    patterns: &[(&str, u32)],
    extgstate: &str,
    objs: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let pats: String = patterns
        .iter()
        .map(|(n, o)| format!("/{n} {o} 0 R "))
        .collect();
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /Pattern << {pats}>> /ExtGState << {extgstate} >> \
                 /ColorSpace << /Cs [/Pattern /DeviceRGB] >> \
                 /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> \
                 /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    all.extend(objs);
    build_pdf(&all)
}

/// A tiling pattern stream.
fn pattern(dict: &str, content: &[u8]) -> Vec<u8> {
    stream_obj(&format!("/PatternType 1 {dict}"), content)
}

/// Coloured, 20 pt cells with a 10 pt red square in each lower-left corner.
fn red_squares() -> Vec<u8> {
    pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f",
    )
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn render_with(pdf: &[u8], limits: Limits) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes_with_limits(pdf, limits.clone())?;
    let opts = RenderOptions {
        limits,
        ..RenderOptions::default()
    };
    render_page(&doc, &doc.pages()[0], &opts)
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

fn count(r: &RenderedPage, pred: impl Fn((u8, u8, u8)) -> bool) -> usize {
    (0..100)
        .flat_map(|x| (0..100).map(move |y| (x, y)))
        .filter(|&(x, y)| pred(px(r, x, y)))
        .count()
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const RED: (u8, u8, u8) = (255, 0, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);

#[test]
fn coloured_cells_repeat_on_the_step_grid() {
    // 25 cells on the page: executed one by one.
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert_eq!(px(&r, 5, 95), RED, "cell (0,0)");
    assert_eq!(px(&r, 15, 85), WHITE, "gap in cell (0,0)");
    assert_eq!(px(&r, 25, 75), RED, "cell (1,1)");
    assert_eq!(px(&r, 85, 15), RED, "cell (4,4)");
    assert_eq!(px(&r, 15, 95), WHITE);
    // Exactly a quarter of the page is red.
    let red = count(&r, |p| p == RED);
    assert!((2400..=2600).contains(&red), "{red}");
}

#[test]
fn dense_tilings_use_the_cached_cell_with_the_same_result() {
    // 10 pt cells, 5 pt squares: 100 cells → cached path.
    let dense = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        b"1 0 0 rg 0 0 5 5 re f",
    );
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, dense)],
    ))
    .unwrap();
    assert!(near(px(&r, 2, 98), RED, 8), "{:?}", px(&r, 2, 98));
    assert!(near(px(&r, 7, 93), WHITE, 8), "{:?}", px(&r, 7, 93));
    assert!(near(px(&r, 12, 88), RED, 8), "{:?}", px(&r, 12, 88));
    assert!(near(px(&r, 97, 3), WHITE, 8), "{:?}", px(&r, 97, 3));
    let red = count(&r, |p| near(p, RED, 8));
    assert!((2300..=2700).contains(&red), "{red}");
    // Step larger than the cell: gaps between tiles.
    let gapped = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 5 5] /XStep 10 /YStep 10 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f",
    );
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, gapped)],
    ))
    .unwrap();
    assert!(near(px(&r, 2, 98), RED, 8), "inside the bbox");
    assert!(near(px(&r, 7, 98), WHITE, 8), "bbox clips the cell content");
    assert!(near(px(&r, 7, 93), WHITE, 8));
}

#[test]
fn uncoloured_cells_take_the_scn_colour_and_ignore_their_own() {
    let unc = pattern(
        "/PaintType 2 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f 0 g 10 10 10 10 re f",
    );
    let r = render(&page(
        b"/Cs cs 0 0 1 /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, unc.clone())],
    ))
    .unwrap();
    assert_eq!(px(&r, 5, 95), BLUE, "the scn colour, not the cell's rg");
    assert_eq!(px(&r, 15, 85), BLUE, "the cell's g is ignored too");
    assert_eq!(px(&r, 15, 95), WHITE);
    // Uncoloured without a base space, or with the wrong component
    // count, is refused.
    let err = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, unc.clone())],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let err = render(&page(
        b"/Cs cs 0.5 /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, unc)],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

#[test]
fn matrix_ctm_and_clip() {
    // /Matrix shifts pattern space by (5, 5): the squares move, the
    // CTM at fill time does not affect them (§8.7.3.1).
    let shifted = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 \
         /Matrix [1 0 0 1 5 5] /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f",
    );
    let r = render(&page(
        b"q 1 0 0 1 50 50 cm /Pattern cs /P scn -50 -50 100 100 re f Q",
        &[("P", 10)],
        "",
        vec![(10, shifted)],
    ))
    .unwrap();
    assert_eq!(px(&r, 2, 98), WHITE, "below the shifted square");
    assert_eq!(px(&r, 10, 90), RED, "user (10,10) is inside [5,15]²");
    assert_eq!(px(&r, 17, 83), WHITE);
    // The fill is clipped to the path and to the W clip.
    let r = render(&page(
        b"q 0 0 50 100 re W n /Pattern cs /P scn 20 20 60 60 re f Q",
        &[("P", 10)],
        "",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 75), RED, "inside path and clip");
    assert_eq!(px(&r, 5, 95), WHITE, "outside the path");
    assert_eq!(px(&r, 65, 35), WHITE, "outside the clip");
    // A rotated pattern matrix: squares become diamonds; nothing is
    // painted outside the path and the page keeps red ink.
    let rot = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 \
         /Matrix [0.7071 0.7071 -0.7071 0.7071 0 0] /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f",
    );
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, rot)],
    ))
    .unwrap();
    let red = count(&r, |p| near(p, RED, 40));
    assert!((2000..=3000).contains(&red), "{red}");
}

#[test]
fn stroke_and_text_with_a_pattern() {
    // A 10 pt line stroked with the pattern: red only within the band.
    let r = render(&page(
        b"/Pattern CS /P SCN 10 w 0 50 m 100 50 l S",
        &[("P", 10)],
        "",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert_eq!(px(&r, 5, 52), RED, "inside the band, inside a square");
    assert_eq!(px(&r, 15, 52), WHITE, "inside the band, between squares");
    assert_eq!(px(&r, 5, 30), WHITE, "outside the band");
    assert_eq!(
        count(&r, |p| p == (0, 0, 0)),
        0,
        "no black placeholder stroke"
    );
    // Text filled with the pattern.
    let r = render(&page(
        b"BT /F1 80 Tf /Pattern cs /P scn 10 20 Td (M) Tj ET",
        &[("P", 10)],
        "",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert!(count(&r, |p| p == RED) > 100, "glyph shows the pattern");
    assert_eq!(count(&r, |p| p == (0, 0, 0)), 0, "no black glyph");
}

#[test]
fn alpha_applies_once_and_blend_is_honoured() {
    let r = render(&page(
        b"/G gs /Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "/G << /ca 0.5 >>",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert!(
        near(px(&r, 5, 95), (255, 128, 128), 4),
        "{:?}",
        px(&r, 5, 95)
    );
    // Dense tiling under alpha goes through the cached cell: the same.
    let dense = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f",
    );
    let r = render(&page(
        b"/G gs /Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "/G << /ca 0.5 >>",
        vec![(10, dense)],
    ))
    .unwrap();
    assert!(
        near(px(&r, 50, 50), (255, 128, 128), 4),
        "{:?}",
        px(&r, 50, 50)
    );
}

#[test]
fn nested_patterns_cycles_and_depth() {
    // The cell of /P fills with /Q (blue squares inside the red cell).
    let inner = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 5 5] /XStep 5 /YStep 5 /Resources << >>",
        b"0 0 1 rg 0 0 2.5 2.5 re f",
    );
    let outer = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 \
         /Resources << /Pattern << /Q 11 0 R >> >>",
        b"1 0 0 rg 0 0 20 20 re f /Pattern cs /Q scn 0 0 10 10 re f",
    );
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, outer), (11, inner)],
    ))
    .unwrap();
    assert_eq!(px(&r, 15, 85), RED, "outer cell");
    assert!(count(&r, |p| near(p, BLUE, 8)) > 200, "inner cells painted");
    // A cell that fills with its own pattern is a cycle.
    let selfref = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 \
         /Resources << /Pattern << /P 10 0 R >> >>",
        b"/Pattern cs /P scn 0 0 20 20 re f",
    );
    let err = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, selfref)],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::CycleDetected { .. }), "{err:?}");
    // A chain of 10 distinct patterns exceeds the nesting depth.
    let mut objs = Vec::new();
    for k in 0..10u32 {
        let next = if k < 9 {
            format!("/Resources << /Pattern << /N {} 0 R >> >>", 11 + k)
        } else {
            "/Resources << >>".into()
        };
        let body: &[u8] = if k < 9 {
            b"/Pattern cs /N scn 0 0 20 20 re f"
        } else {
            b"1 0 0 rg 0 0 20 20 re f"
        };
        objs.push((
            10 + k,
            pattern(
                &format!("/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 {next}"),
                body,
            ),
        ));
    }
    let err = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        objs,
    ))
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

#[test]
fn overlapping_cells_are_executed_and_bounded() {
    // /BBox wider than the step: tiles overlap, so each is executed.
    let over = pattern(
        "/PaintType 1 /TilingType 1 /BBox [0 0 30 30] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 30 30 re f",
    );
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, over.clone())],
    ))
    .unwrap();
    assert_eq!(count(&r, |p| p == RED), 10_000, "overlap covers everything");
    let err = render_with(
        &page(
            b"/Pattern cs /P scn 0 0 100 100 re f",
            &[("P", 10)],
            "",
            vec![(10, over)],
        ),
        Limits {
            max_pattern_tiles: 8,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

#[test]
fn malformed_patterns_are_refused() {
    for dict in [
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 0 /YStep 20 /Resources << >>",
        "/PaintType 1 /TilingType 1 /BBox [0 0 20 20] /YStep 20 /Resources << >>",
        "/PaintType 3 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        "/PaintType 1 /TilingType 7 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        "/PaintType 1 /TilingType 1 /XStep 20 /YStep 20 /Resources << >>",
        "/PaintType 1 /TilingType 1 /BBox [0 0 20] /XStep 20 /YStep 20 /Resources << >>",
    ] {
        let err = render(&page(
            b"/Pattern cs /P scn 0 0 100 100 re f",
            &[("P", 10)],
            "",
            vec![(10, pattern(dict, b"0 0 10 10 re f"))],
        ))
        .unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{dict}: {err:?}");
    }
    // A pattern that is a dictionary, not a stream.
    let err = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(
            10,
            b"<< /PatternType 1 /PaintType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 >>".to_vec(),
        )],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // /Pattern space with no pattern selected paints nothing.
    let r = render(&page(
        b"/Pattern cs 0 0 100 100 re f",
        &[("P", 10)],
        "",
        vec![(10, red_squares())],
    ))
    .unwrap();
    assert_eq!(count(&r, |p| p != WHITE), 0);
}
