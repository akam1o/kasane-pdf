//! Annotation flags NoRotate (bit 5) and NoZoom (bit 4), ISO 32000-2
//! §12.5.3 Table 167.
//!
//! NoRotate: the appearance is not rotated with the page's `/Rotate`;
//! the upper-left corner of `/Rect` stays where the rotated page puts
//! it, and the appearance hangs from it upright. NoZoom: the appearance
//! is not scaled with the viewer's magnification. A render at a given
//! resolution is the page at magnification 1 (its physical size at that
//! resolution), so NoZoom leaves the placement unchanged at every dpi.
//!
//! Every expected box is computed here, independently of the renderer:
//! Algorithm 8.1 (`/BBox` through `/Matrix`, fitted to `/Rect`), then —
//! for NoRotate — the pivot about the upper-left corner, then the
//! displayed page (the CropBox rotated clockwise by `/Rotate`, origin at
//! the top left, one pixel per point × dpi / 72), clipped to the page.
//! The synthetic pages were also rendered with Poppler (`pdftoppm`),
//! which implements NoRotate the same way (`Gfx::drawAnnot`, pivot at
//! the upper-left corner): its ink boxes agree with these within a
//! pixel on every case of `flag_combinations_across_rotations` and
//! `appearance_bbox_matrix_and_inner_transforms`.

mod common;

use kasane_pdf::{render_page, Document, RenderOptions};

const W: f64 = 300.0;
const H: f64 = 200.0;

/// One annotation: `/Rect`, `/F` (None: omitted), the appearance
/// `/BBox`, optional `/Matrix`, and an optional `cm` inside the stream.
#[derive(Clone)]
struct Annot {
    rect: [f64; 4],
    flags: Option<i64>,
    bbox: [f64; 4],
    matrix: Option<[f64; 6]>,
    inner_cm: Option<[f64; 6]>,
}

impl Annot {
    fn new(rect: [f64; 4], flags: Option<i64>) -> Self {
        let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
        Annot {
            rect,
            flags,
            bbox: [0.0, 0.0, w, h],
            matrix: None,
            inner_cm: None,
        }
    }
    /// The appearance: the whole `/BBox` mid-grey, its top-left quarter
    /// (a quarter of the width, a third of the height) black — so the
    /// black box tells the orientation.
    fn marker(&self) -> [f64; 4] {
        let [x0, y0, x1, y1] = self.bbox;
        [x0, y1 - (y1 - y0) / 3.0, x0 + (x1 - x0) / 4.0, y1]
    }
    fn content(&self) -> String {
        let [x0, y0, x1, y1] = self.bbox;
        let [mx0, my0, mx1, my1] = self.marker();
        let cm = self
            .inner_cm
            .map(|m| format!("{} {} {} {} {} {} cm ", m[0], m[1], m[2], m[3], m[4], m[5]))
            .unwrap_or_default();
        format!(
            "{cm}0.5 g {x0} {y0} {} {} re f 0 g {mx0} {my0} {} {} re f",
            x1 - x0,
            y1 - y0,
            mx1 - mx0,
            my1 - my0
        )
    }
}

const NO_ROTATE: i64 = 1 << 4;
const NO_ZOOM: i64 = 1 << 3;
const PRINT: i64 = 1 << 2;

fn pdf(rotate: i64, annots: &[Annot]) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (4, common::stream_obj("", b"")),
    ];
    let mut refs = String::new();
    for (i, a) in annots.iter().enumerate() {
        let (an, ap) = (10 + 2 * i as u32, 11 + 2 * i as u32);
        refs.push_str(&format!("{an} 0 R "));
        let f = a.flags.map(|f| format!("/F {f} ")).unwrap_or_default();
        let [r0, r1, r2, r3] = a.rect;
        objs.push((
            an,
            format!("<< /Type /Annot /Subtype /Square /Rect [{r0} {r1} {r2} {r3}] {f}/AP << /N {ap} 0 R >> >>")
                .into_bytes(),
        ));
        let [b0, b1, b2, b3] = a.bbox;
        let m = a
            .matrix
            .map(|m| {
                format!(
                    "/Matrix [{} {} {} {} {} {}] ",
                    m[0], m[1], m[2], m[3], m[4], m[5]
                )
            })
            .unwrap_or_default();
        objs.push((
            ap,
            common::stream_obj(
                &format!("/Type /XObject /Subtype /Form /BBox [{b0} {b1} {b2} {b3}] {m}"),
                a.content().as_bytes(),
            ),
        ));
    }
    objs.push((
        3,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W} {H}] /Rotate {rotate} \
             /Resources << >> /Contents 4 0 R /Annots [{refs}] >>"
        )
        .into_bytes(),
    ));
    objs.sort_by_key(|o| o.0);
    common::build_pdf(&objs)
}

type Pt = (f64, f64);

fn apply(m: [f64; 6], (x, y): Pt) -> Pt {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// The displayed page: user point → pixel (top-left origin), page
/// rotated clockwise by `rotate`, `s` pixels per point.
fn display(rotate: i64, s: f64, (x, y): Pt) -> Pt {
    let (px, py) = match rotate {
        0 => (x, H - y),
        90 => (y, x),
        180 => (W - x, y),
        270 => (H - y, W - x),
        _ => unreachable!(),
    };
    (px * s, py * s)
}

/// Pixel box of the form-space region `region` of annotation `a`.
fn expected(rotate: i64, dpi: f64, a: &Annot, region: [f64; 4]) -> [f64; 4] {
    let s = dpi / 72.0;
    let m = a.matrix.unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let cm = a.inner_cm.unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    // Algorithm 8.1: the transformed BBox's bounds fitted to /Rect.
    let [b0, b1, b2, b3] = a.bbox;
    let tb: Vec<Pt> = [(b0, b1), (b2, b1), (b2, b3), (b0, b3)]
        .iter()
        .map(|&p| apply(m, p))
        .collect();
    let (tx0, tx1) = (
        tb.iter().map(|p| p.0).fold(f64::MAX, f64::min),
        tb.iter().map(|p| p.0).fold(f64::MIN, f64::max),
    );
    let (ty0, ty1) = (
        tb.iter().map(|p| p.1).fold(f64::MAX, f64::min),
        tb.iter().map(|p| p.1).fold(f64::MIN, f64::max),
    );
    let [r0, r1, r2, r3] = a.rect;
    let (sx, sy) = ((r2 - r0) / (tx1 - tx0), (r3 - r1) / (ty1 - ty0));
    let to_user = |p: Pt| {
        let (x, y) = apply(m, apply(cm, p));
        (r0 + (x - tx0) * sx, r1 + (y - ty0) * sy)
    };
    let no_rotate = a.flags.unwrap_or(0) & NO_ROTATE != 0;
    let to_px = |p: Pt| {
        let u = to_user(p);
        if no_rotate && rotate != 0 {
            // Upright from the upper-left corner's displayed position.
            let pivot = display(rotate, s, (r0, r3));
            (pivot.0 + (u.0 - r0) * s, pivot.1 + (r3 - u.1) * s)
        } else {
            display(rotate, s, u)
        }
    };
    let [x0, y0, x1, y1] = region;
    let pts: Vec<Pt> = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
        .iter()
        .map(|&p| to_px(p))
        .collect();
    // An upright appearance can reach past the displayed page: clipped.
    let (pw, ph) = match rotate {
        90 | 270 => (H * s, W * s),
        _ => (W * s, H * s),
    };
    [
        pts.iter().map(|p| p.0).fold(f64::MAX, f64::min).max(0.0),
        pts.iter().map(|p| p.1).fold(f64::MAX, f64::min).max(0.0),
        pts.iter().map(|p| p.0).fold(f64::MIN, f64::max).min(pw),
        pts.iter().map(|p| p.1).fold(f64::MIN, f64::max).min(ph),
    ]
}

/// Pixel boxes of grey-or-black ink and of black ink.
fn ink(data: &[u8], dpi: f64) -> ([f64; 4], [f64; 4]) {
    let doc = Document::from_bytes(data).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(dpi)).unwrap();
    let mut any = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut black = any;
    for y in 0..r.height() {
        for x in 0..r.width() {
            let v = r.pixels()[(y * r.stride() as u32 + x * 4) as usize];
            let (xf, yf) = (x as f64, y as f64);
            for (b, hit) in [(&mut any, v < 200), (&mut black, v < 60)] {
                if hit {
                    *b = [
                        b[0].min(xf),
                        b[1].min(yf),
                        b[2].max(xf + 1.0),
                        b[3].max(yf + 1.0),
                    ];
                }
            }
        }
    }
    (any, black)
}

fn assert_close(got: [f64; 4], want: [f64; 4], what: &str) {
    let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 1.01);
    assert!(ok, "{what}: ink {got:?}, expected {want:?}");
}

fn check(rotate: i64, dpi: f64, a: &Annot, what: &str) {
    let (any, black) = ink(&pdf(rotate, std::slice::from_ref(a)), dpi);
    assert_close(
        any,
        expected(rotate, dpi, a, a.bbox),
        &format!("{what}: annotation"),
    );
    assert_close(
        black,
        expected(rotate, dpi, a, a.marker()),
        &format!("{what}: marker"),
    );
}

/// Off-centre, non-square `/Rect` on a non-square page.
const RECT: [f64; 4] = [40.0, 120.0, 100.0, 150.0];

#[test]
fn flag_combinations_across_rotations() {
    for (rotate, flags) in [
        (0, 0),
        (90, 0),
        (90, NO_ROTATE),
        (90, NO_ZOOM),
        (90, NO_ROTATE | NO_ZOOM),
        (180, NO_ROTATE),
        (270, NO_ROTATE | NO_ZOOM),
        (0, NO_ROTATE | NO_ZOOM),
        (270, 0),
        (180, NO_ROTATE | PRINT),
    ] {
        let a = Annot::new(RECT, Some(flags));
        check(rotate, 72.0, &a, &format!("/Rotate {rotate} /F {flags}"));
    }
}

#[test]
fn no_rotate_keeps_the_upper_left_corner_and_the_size() {
    // /Rotate 90 displays the 300 × 200 page as 200 × 300: the corner
    // (40, 150) is at pixel (150, 40); the 60 × 30 appearance hangs
    // from it upright — not 30 × 60 as the rotated page would draw it —
    // and is clipped by the page's right edge at 200.
    let (any, black) = ink(&pdf(90, &[Annot::new(RECT, Some(NO_ROTATE))]), 72.0);
    assert_close(any, [150.0, 40.0, 200.0, 70.0], "NoRotate at /Rotate 90");
    assert_close(black, [150.0, 40.0, 165.0, 50.0], "marker stays top-left");
    let (any, _) = ink(&pdf(90, &[Annot::new(RECT, Some(0))]), 72.0);
    assert_close(any, [120.0, 40.0, 150.0, 100.0], "rotated with the page");
}

#[test]
fn no_zoom_is_magnification_one_at_every_resolution() {
    for dpi in [36.0, 72.0, 150.0] {
        for rotate in [0, 90] {
            for flags in [NO_ZOOM, NO_ROTATE | NO_ZOOM, NO_ROTATE] {
                let a = Annot::new(RECT, Some(flags));
                check(
                    rotate,
                    dpi,
                    &a,
                    &format!("{dpi} dpi /Rotate {rotate} /F {flags}"),
                );
                let plain = Annot {
                    flags: Some(flags & !NO_ZOOM),
                    ..a.clone()
                };
                assert_eq!(
                    ink(&pdf(rotate, std::slice::from_ref(&a)), dpi),
                    ink(&pdf(rotate, &[plain]), dpi),
                    "NoZoom changes nothing at {dpi} dpi"
                );
            }
        }
    }
}

#[test]
fn appearance_bbox_matrix_and_inner_transforms() {
    // /BBox larger than /Rect, offset origin: fitted (scaled) to /Rect.
    let big = Annot {
        bbox: [10.0, 20.0, 130.0, 80.0],
        ..Annot::new(RECT, Some(NO_ROTATE))
    };
    // A /Matrix rotating the form by 90°: Algorithm 8.1 fits the rotated
    // box, so the marker lands in another corner of /Rect.
    let turned = Annot {
        bbox: [0.0, 0.0, 30.0, 60.0],
        matrix: Some([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        ..Annot::new(RECT, Some(NO_ROTATE))
    };
    // A shear inside the stream moves the marker, not the fit.
    let sheared = Annot {
        inner_cm: Some([1.0, 0.0, 0.25, 1.0, 0.0, 0.0]),
        ..Annot::new([150.0, 20.0, 230.0, 60.0], Some(NO_ROTATE))
    };
    for rotate in [0, 90, 180, 270] {
        for (name, a) in [("bbox", &big), ("matrix", &turned)] {
            check(rotate, 72.0, a, &format!("{name} /Rotate {rotate}"));
        }
        let (_, black) = ink(&pdf(rotate, std::slice::from_ref(&sheared)), 72.0);
        assert_close(
            black,
            expected(rotate, 72.0, &sheared, sheared.marker()),
            &format!("shear /Rotate {rotate}"),
        );
    }
}

#[test]
fn omitted_flags_rotate_with_the_page() {
    for rotate in [0, 90, 180, 270] {
        check(
            rotate,
            72.0,
            &Annot::new(RECT, None),
            &format!("no /F, /Rotate {rotate}"),
        );
    }
}

#[test]
fn overlapping_annotations_keep_their_order_and_own_flags() {
    // A NoRotate annotation drawn over one that rotates: each placed by
    // its own flags; the later one paints over the earlier.
    let under = Annot::new([40.0, 100.0, 140.0, 150.0], Some(0));
    let over = Annot::new(RECT, Some(NO_ROTATE));
    let doc_bytes = pdf(90, &[under.clone(), over.clone()]);
    let doc = Document::from_bytes(&doc_bytes).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new()).unwrap();
    let [x0, y0, ..] = expected(90, 72.0, &over, over.marker());
    let v = r.pixels()[((y0 as u32 + 2) * r.stride() as u32 + (x0 as u32 + 2) * 4) as usize];
    assert!(v < 60, "the NoRotate marker is on top (luma {v})");
    let (any, _) = ink(&doc_bytes, 72.0);
    let a = expected(90, 72.0, &under, under.bbox);
    let b = expected(90, 72.0, &over, over.bbox);
    assert_close(
        any,
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ],
        "union of both",
    );
}

/// A one-annotation page: `page_extra` goes into the page dictionary
/// (e.g. a `/CropBox`), `annot` is the whole annotation dictionary.
fn raw_pdf(rotate: i64, page_extra: &str, annot: &str, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {W} {H}] {page_extra} /Rotate {rotate} \
                 /Resources << >> /Contents 4 0 R /Annots [10 0 R] >>"
            )
            .into_bytes(),
        ),
        (4, common::stream_obj("", b"")),
        (10, annot.as_bytes().to_vec()),
    ];
    objs.extend(extra.iter().cloned());
    objs.sort_by_key(|o| o.0);
    common::build_pdf(&objs)
}

#[test]
fn crop_box_offset_moves_the_pivot_with_the_page() {
    // CropBox [20 10 260 190] shown at /Rotate 90 is 180 × 240 px. The
    // corner (40, 130) of /Rect is (20, 120) from the CropBox origin, so
    // it is displayed at (120, 20); the 60 × 30 appearance hangs from it.
    let ap = common::stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 60 30]",
        b"0 g 0 0 60 30 re f",
    );
    let data = raw_pdf(
        90,
        "/CropBox [20 10 260 190]",
        "<< /Type /Annot /Subtype /Square /Rect [40 100 100 130] /F 16 /AP << /N 11 0 R >> >>",
        &[(11, ap)],
    );
    let (any, _) = ink(&data, 72.0);
    assert_close(any, [120.0, 20.0, 180.0, 50.0], "NoRotate under a CropBox");
}

#[test]
fn synthesised_appearance_pivots_on_rect_not_on_its_drawn_box() {
    // A Square without /AP is drawn inside /Rect shrunk by /RD 5; the
    // NoRotate pivot is still /Rect's corner (40, 90) → pixel (90, 40)
    // at /Rotate 90, so the 50 × 20 fill spans (95, 45)–(145, 65).
    let data = raw_pdf(
        90,
        "",
        "<< /Type /Annot /Subtype /Square /Rect [40 60 100 90] /F 16 /RD [5 5 5 5] \
         /IC [0 0 0] /Border [0 0 0] >>",
        &[],
    );
    let (any, _) = ink(&data, 72.0);
    assert_close(any, [95.0, 45.0, 145.0, 65.0], "pivot on /Rect");
}

#[test]
fn rotate_values_are_normalised_before_the_flag_applies() {
    // /Rotate -90 is 270 and 450 is 90 (§7.7.3.3: multiples of 90).
    let a = Annot::new(RECT, Some(NO_ROTATE));
    for (odd, same) in [(-90, 270), (450, 90), (-180, 180)] {
        assert_eq!(
            ink(&pdf(odd, std::slice::from_ref(&a)), 72.0),
            ink(&pdf(same, std::slice::from_ref(&a)), 72.0),
            "/Rotate {odd} behaves as {same}"
        );
    }
}
