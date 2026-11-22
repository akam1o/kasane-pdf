//! Dense tiling patterns drawn from the periodic texture
//! (`paint_tiles_texture`): what reaches the device is each pixel's
//! true coverage — thin lines keep their weight in cells of a pixel or
//! two, a line of width 0 is one device pixel, nothing is raised to a
//! minimum, tiles meet without seams, overlapping and translucent cells
//! composite as the tiles would, and the cells executed are counted
//! against `max_pattern_tiles`. Expected values are analytic, or the
//! same geometry drawn without a pattern. 200 × 200 pt page; the region
//! [10, 10, 190, 190] is filled.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

const REGION: &str = "10 10 180 180 re";

/// A page filling REGION with tiling pattern `/P` (`dict` and `cell`),
/// with ExtGState `ext` for the cell.
fn pattern_page(dict: &str, cell: &str, ext: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
              /Resources << /Pattern << /P 5 0 R >> >> >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj("", format!("/Pattern cs /P scn {REGION} f").as_bytes()),
        ),
        (
            5,
            stream_obj(
                &format!("/PatternType 1 /PaintType 1 /TilingType 1 {dict} /Resources << {ext} >>"),
                cell.as_bytes(),
            ),
        ),
    ])
}

/// A page drawing `content` directly (the same geometry, no pattern).
fn plain_page(content: &str, ext: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Resources << {ext} >> >>"
            )
            .into_bytes(),
        ),
        (
            4,
            stream_obj("", format!("q {REGION} W n {content} Q").as_bytes()),
        ),
    ])
}

fn render(pdf: &[u8], dpi: f64, limits: Limits) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    let opts = RenderOptions::new().with_dpi(dpi).with_limits(limits);
    render_page(&doc, &doc.pages()[0], &opts)
}

fn draw(pdf: &[u8], dpi: f64) -> RenderedPage {
    render(pdf, dpi, Limits::default()).unwrap()
}

/// Luma of pixel (x, y).
fn grey(r: &RenderedPage, x: u32, y: u32) -> f64 {
    let p = r.pixmap().pixel(x, y).unwrap();
    0.299 * p.red() as f64 + 0.587 * p.green() as f64 + 0.114 * p.blue() as f64
}

/// The inner part of REGION in pixels (2 px in from its edges).
fn inner(_: &RenderedPage, dpi: f64) -> (u32, u32, u32, u32) {
    let s = dpi / 72.0;
    (
        ((10.0 * s) as u32) + 2,
        ((10.0 * s) as u32) + 2,
        ((190.0 * s) as u32) - 2,
        ((190.0 * s) as u32) - 2,
    )
}

fn mean(r: &RenderedPage, dpi: f64) -> f64 {
    let (x0, y0, x1, y1) = inner(r, dpi);
    let mut sum = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            sum += grey(r, x, y);
        }
    }
    sum / ((x1 - x0) * (y1 - y0)) as f64
}

fn max_diff(a: &RenderedPage, b: &RenderedPage, dpi: f64) -> f64 {
    let (x0, y0, x1, y1) = inner(a, dpi);
    let mut worst: f64 = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            worst = worst.max((grey(a, x, y) - grey(b, x, y)).abs());
        }
    }
    worst
}

/// Mean grey of blue (luma 29) lines covering `coverage` of the area.
fn lines_expected(coverage: f64) -> f64 {
    255.0 - coverage.min(1.0) * (255.0 - 0.114 * 255.0)
}

/// A cell of one step `s` with a blue line of width `w` across its middle.
fn line_cell(s: f64, w: f64) -> Vec<u8> {
    let h = s / 2.0;
    pattern_page(
        &format!("/BBox [-{h} -{h} {h} {h}] /XStep {s} /YStep {s}"),
        &format!("0 0 1 RG {w} w -{h} 0 m {h} 0 l S"),
        "",
    )
}

#[test]
fn thin_lines_in_cells_of_a_pixel_or_two_keep_their_coverage() {
    // Cells of 1.2 to 4 device pixels; lines a tenth to two thirds of a
    // pixel wide. (Before: the cached cell drew nothing at all below
    // about 3 pixels.)
    for dpi in [48.0, 72.0, 96.0] {
        for (s, w) in [(0.9, 0.3985), (1.4, 0.3985), (1.4, 0.1), (2.98883, 0.3985)] {
            let r = draw(&line_cell(s, w), dpi);
            let (got, want) = (mean(&r, dpi), lines_expected(w / s));
            assert!(
                (got - want).abs() <= 3.0,
                "s {s} w {w} at {dpi} dpi: {got:.2}, want {want:.2}"
            );
        }
    }
}

#[test]
fn a_line_of_width_0_is_one_device_pixel() {
    // §8.4.3.2: the thinnest line the device draws — one pixel, at
    // every resolution, also inside a cell drawn supersampled.
    for (s, dpi) in [
        (6.0, 72.0),
        (2.98883, 96.0),
        (1.4, 150.0),
        (1.4, 72.0),
        (0.9, 96.0),
    ] {
        let r = draw(&line_cell(s, 0.0), dpi);
        let spacing = s * dpi / 72.0;
        let (got, want) = (mean(&r, dpi), lines_expected(1.0 / spacing));
        assert!(
            (got - want).abs() <= 3.0,
            "s {s} at {dpi} dpi: {got:.2}, want {want:.2}"
        );
    }
}

#[test]
fn coverage_is_not_raised_to_a_minimum() {
    // A line 0.002 pt wide every 2 pt covers 0.1 % of the area: what
    // reaches the device is that, not a visible pixel-wide line.
    let r = draw(&line_cell(2.0, 0.002), 96.0);
    assert!(mean(&r, 96.0) > 254.0, "{}", mean(&r, 96.0));
}

/// The spread of grey along the darkest device row (a line).
fn row_spread(r: &RenderedPage, dpi: f64) -> f64 {
    let (x0, y0, x1, y1) = inner(r, dpi);
    let row = |y| (x0..x1).map(|x| grey(r, x, y)).sum::<f64>();
    let darkest = (y0..y1)
        .min_by(|&a, &b| row(a).partial_cmp(&row(b)).unwrap())
        .unwrap();
    let g: Vec<f64> = (x0..x1).map(|x| grey(r, x, darkest)).collect();
    g.iter().cloned().fold(f64::MIN, f64::max) - g.iter().cloned().fold(f64::MAX, f64::min)
}

#[test]
fn tiles_meet_without_seams() {
    // Lines across the whole cell: along a line the device row is
    // uniform (the seams of executed tiles, or of a cell rounded to
    // whole pixels, would show as lighter columns) — also for the TikZ
    // cell whose /BBox exceeds the step by rounding (before: a lighter
    // column every cell).
    let dpi = 96.0;
    let near = pattern_page(
        "/BBox [-1.49442 -1.49442 1.49442 1.49442] /XStep 2.98883 /YStep 2.98883",
        "0 0 1 RG 0.3985 w -1.49442 0 m 1.49442 0 l S",
        "",
    );
    assert!(
        row_spread(&draw(&near, dpi), dpi) <= 2.0,
        "{}",
        row_spread(&draw(&near, dpi), dpi)
    );
    let r = draw(&line_cell(2.98883, 0.3985), dpi);
    assert!(row_spread(&r, dpi) <= 2.0, "{}", row_spread(&r, dpi));
}

#[test]
fn a_bbox_over_the_step_by_rounding_draws_as_one_step() {
    // TikZ writes a /BBox of 2 × 1.49442 for a step of 2.98883. Its
    // tiles overlap by 0.00001 pt: one step of the tiling is the cell
    // and three neighbours, 4 executions — not one per tile (before:
    // 3600 tiles, each executed, seams; over a larger area, refused at
    // max_pattern_tiles).
    let near = pattern_page(
        "/BBox [-1.49442 -1.49442 1.49442 1.49442] /XStep 2.98883 /YStep 2.98883",
        "0 0 1 RG 0.3985 w -1.49442 0 m 1.49442 0 l S",
        "",
    );
    let exact = line_cell(2.98883, 0.3985);
    for dpi in [72.0, 96.0, 150.0] {
        let (a, b) = (draw(&near, dpi), draw(&exact, dpi));
        assert!(
            max_diff(&a, &b, dpi) <= 2.0,
            "{dpi} dpi: {}",
            max_diff(&a, &b, dpi)
        );
    }
    assert!(render(&near, 96.0, Limits::default().with_max_pattern_tiles(4)).is_ok());
    assert!(matches!(
        render(&near, 96.0, Limits::default().with_max_pattern_tiles(3)),
        Err(Error::LimitExceeded {
            limit: "max_pattern_tiles",
            ..
        })
    ));
}

#[test]
fn paint_in_the_overlap_of_cells_under_a_pixel_is_kept() {
    // 0.1 pt cells whose only paint is in a 0.0038 pt excess of the
    // /BBox: a pixel spans many cells, so the excesses add up to 3.8 %
    // of the area — kept, as the same strip drawn inside the cell.
    // (Before: refused at max_pattern_tiles; the strip inside the cell
    // drew nothing.)
    let excess = pattern_page(
        "/BBox [0 0 0.1038 1] /XStep 0.1 /YStep 1",
        "0 0 0 rg 0.1 0 0.0038 1 re f",
        "",
    );
    let inside = pattern_page(
        "/BBox [0 0 0.1 1] /XStep 0.1 /YStep 1",
        "0 0 0 rg 0 0 0.0038 1 re f",
        "",
    );
    for dpi in [72.0, 150.0] {
        let (a, b) = (draw(&excess, dpi), draw(&inside, dpi));
        let want = 255.0 * (1.0 - 0.038);
        for (name, r) in [("excess", &a), ("inside", &b)] {
            assert!(
                (mean(r, dpi) - want).abs() <= 3.0,
                "{name} at {dpi} dpi: {:.2}",
                mean(r, dpi)
            );
        }
        assert!(max_diff(&a, &b, dpi) <= 3.0);
    }
}

#[test]
fn overlapping_translucent_cells_composite_as_the_tiles_do() {
    // 6 × 6 pt squares at half opacity every 4 pt: where squares overlap
    // the paint is composited twice or four times. Drawn square by
    // square without a pattern for comparison, in the tiles' order.
    let ext = "/ExtGState << /G << /ca 0.5 >> >>";
    let pat = pattern_page(
        "/BBox [0 0 6 6] /XStep 4 /YStep 4",
        "/G gs 1 0 0 rg 0 0 6 6 re f",
        ext,
    );
    // Each square its own fill at half opacity, as each tile is.
    let mut squares = String::new();
    for j in 0..=48 {
        for i in 0..=48 {
            squares.push_str(&format!("{} {} 6 6 re f ", i * 4, j * 4));
        }
    }
    let plain = plain_page(&format!("/G gs 1 0 0 rg {squares}"), ext);
    for dpi in [72.0, 96.0] {
        let (a, b) = (draw(&pat, dpi), draw(&plain, dpi));
        assert!(
            (mean(&a, dpi) - mean(&b, dpi)).abs() <= 1.0,
            "{dpi}: {} {}",
            mean(&a, dpi),
            mean(&b, dpi)
        );
        // Four squares overlap at (13, 13), one covers (15, 15): the
        // composition is kept, not flattened.
        let px = |pt: f64| (pt * dpi / 72.0) as u32;
        let single = grey(&a, px(15.0), px(200.0 - 15.0));
        let quad = grey(&a, px(13.0), px(200.0 - 13.0));
        assert!(
            quad < single - 20.0,
            "{dpi}: single {single}, overlap {quad}"
        );
    }
}

#[test]
fn transparent_cells_paint_nothing() {
    let r = draw(
        &pattern_page(
            "/BBox [0 0 2 2] /XStep 2 /YStep 2",
            "/G gs 1 0 0 rg 0 0 2 2 re f",
            "/ExtGState << /G << /ca 0 >> >>",
        ),
        96.0,
    );
    let (x0, y0, x1, y1) = inner(&r, 96.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = r.pixmap().pixel(x, y).unwrap();
            assert_eq!(
                (p.red(), p.green(), p.blue()),
                (255, 255, 255),
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn rotated_sheared_and_flipped_cells_keep_their_coverage() {
    // The 1.4 pt line cell under pattern matrices: the device lines are
    // 0.3 pt every 1.4 pt (21.4 % covered) whatever the orientation.
    let c = (30f64).to_radians().cos();
    let sn = (30f64).to_radians().sin();
    for m in [
        format!("{c} {sn} {} {c} 3.3 1.7", -sn),
        "1 0 0.6 1 0.37 0".to_string(),
        "1 0 0 -1 0.21 0.43".to_string(),
    ] {
        let pdf = pattern_page(
            &format!("/BBox [-0.7 -0.7 0.7 0.7] /XStep 1.4 /YStep 1.4 /Matrix [{m}]"),
            "0 0 1 RG 0.3 w -0.7 0 m 0.7 0 l S",
            "",
        );
        for dpi in [72.0, 150.0] {
            let r = draw(&pdf, dpi);
            let (got, want) = (mean(&r, dpi), lines_expected(0.3 / 1.4));
            assert!(
                (got - want).abs() <= 3.0,
                "[{m}] at {dpi} dpi: {got:.2}, want {want:.2}"
            );
        }
    }
}

#[test]
fn a_dense_pattern_inside_a_dense_cell_keeps_its_coverage() {
    // Outer 5 pt cells filled whole with the 1.4 pt line pattern (inner,
    // pattern space of the outer cell): the device sees the inner lines,
    // 21.4 % covered. The inner texture is not supersampled again.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
              /Resources << /Pattern << /P 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", format!("/Pattern cs /P scn {REGION} f").as_bytes())),
        (
            5,
            stream_obj(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 5.6 5.6] /XStep 5.6 /YStep 5.6 \
                 /Resources << /Pattern << /Q 6 0 R >> >>",
                b"/Pattern cs /Q scn 0 0 5.6 5.6 re f",
            ),
        ),
        (
            6,
            stream_obj(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [-0.7 -0.7 0.7 0.7] /XStep 1.4 /YStep 1.4 \
                 /Resources << >>",
                b"0 0 1 RG 0.3 w -0.7 0 m 0.7 0 l S",
            ),
        ),
    ]);
    for dpi in [72.0, 150.0] {
        let r = draw(&pdf, dpi);
        let (got, want) = (mean(&r, dpi), lines_expected(0.3 / 1.4));
        assert!(
            (got - want).abs() <= 3.0,
            "{dpi} dpi: {got:.2}, want {want:.2}"
        );
    }
}
