//! D1 (v0.11): the outline edges of a render's dashed strokes are
//! bounded together. One stroke was bounded on its own, but a page could
//! repeat it without limit: segments off the pixmap took no row work,
//! yet each was still built, stroked and clipped away (a page of 512
//! million-unit dashed rectangles: 54 s for one render, 2026-11-16). The
//! budget is shared by everything one render draws — the page, its
//! forms, patterns, soft masks and annotation appearances — and is
//! applied in strict and recovery mode alike. Running out is an error,
//! never a partial drawing presented as a page.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, OpenOptions, RenderOptions};

/// `[3 1] 0 d`, width 5, butt caps: a rectangle `len` wide and 10 high
/// has `≈ (2 len + 20) / 4` dashes of 4 outline edges each.
const DASH: &str = "5 w [3 1] 0 d";

fn rect(len: u64) -> String {
    format!("10 5 {len} 10 re S")
}

/// A one-page document; `content` on the page, `extra` objects from 10,
/// `resources` added to the page's resources, `page_extra` to the page.
fn doc_with(
    content: &str,
    resources: &str,
    page_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 60] /Contents 4 0 R \
                 /Resources << {resources} >> {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

fn page(content: &str) -> Vec<u8> {
    doc_with(content, "", "", vec![])
}

fn render(pdf: &[u8], recovering: bool) -> Result<Vec<u8>, Error> {
    let d = Document::open(pdf, &OpenOptions::new().with_recovery(recovering)).unwrap();
    render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(36.0))
        .map(|r| r.pixels().to_vec())
}

/// Refused, in both modes, as a dashed stroke over the render's budget.
fn assert_refused(pdf: &[u8], what: &str) {
    for recovering in [false, true] {
        match render(pdf, recovering) {
            Err(
                e @ Error::Unsupported {
                    feature: "dashed stroke",
                    ..
                },
            ) => {
                assert!(
                    e.to_string().contains("dashed strokes together"),
                    "{what} (recovery {recovering}): {e}"
                );
            }
            other => panic!("{what} (recovery {recovering}): {:?}", other.map(|_| ())),
        }
    }
}

/// Drawn, and identically in both modes.
fn assert_drawn(pdf: &[u8], what: &str) -> Vec<u8> {
    let strict = render(pdf, false).unwrap_or_else(|e| panic!("{what}: {e}"));
    let recovery = render(pdf, true).unwrap_or_else(|e| panic!("{what} (recovery): {e}"));
    assert!(strict == recovery, "{what}: the modes differ");
    strict
}

#[test]
fn one_long_dashed_stroke_is_drawn_two_are_refused() {
    // ≈ 2 × 10⁶ edges: within the budget.
    assert_drawn(
        &page(&format!("{DASH} {}", rect(1_000_000))),
        "one rectangle",
    );
    // ≈ 4 × 10⁶ + 80: the known slow input (two such rectangles).
    assert_refused(
        &page(&format!("{DASH} {} {}", rect(1_000_000), rect(1_000_000))),
        "two rectangles",
    );
    // Far over it: refused as well, and before the dashing of the
    // strokes beyond the budget (512 of them took 54 s to draw).
    let many: String = (0..512)
        .map(|_| rect(1_000_000))
        .collect::<Vec<_>>()
        .join(" ");
    let t = std::time::Instant::now();
    assert_refused(&page(&format!("{DASH} {many}")), "512 rectangles");
    assert!(t.elapsed().as_secs() < 60, "took {:?}", t.elapsed());
}

#[test]
fn many_short_strokes_add_up() {
    // 40 020 edges each: 99 are within the budget, 101 are not.
    let n = |k: usize| (0..k).map(|_| rect(20_000)).collect::<Vec<_>>().join(" ");
    assert_drawn(&page(&format!("{DASH} {}", n(99))), "99 rectangles");
    assert_refused(&page(&format!("{DASH} {}", n(101))), "101 rectangles");
}

fn form(content: &str) -> Vec<u8> {
    stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 60 60] /Resources << /XObject << /In 11 0 R >> >>", content.as_bytes())
}

#[test]
fn the_budget_is_shared_with_forms_patterns_and_annotations() {
    let long = rect(1_000_000);
    // A nested form: the page draws one rectangle, a form inside a form
    // the other.
    assert_refused(
        &doc_with(
            &format!("{DASH} {long} /Fm Do"),
            "/XObject << /Fm 10 0 R >>",
            "",
            vec![(10, form("/In Do")), (11, form(&format!("{DASH} {long}")))],
        ),
        "a nested form",
    );
    // Each form alone is within it: drawn.
    assert_drawn(
        &doc_with(
            "/Fm Do",
            "/XObject << /Fm 10 0 R >>",
            "",
            vec![(10, form("/In Do")), (11, form(&format!("{DASH} {long}")))],
        ),
        "one rectangle in a nested form",
    );
    // A tiling pattern's cell, filled over the page: one tile (the step
    // is wider than the page), alone within the budget, over it with
    // the page's own rectangle.
    let tiling = |page_content: &str| {
        doc_with(
            &format!("{page_content} /Pattern cs /P scn 0 0 60 60 re f"),
            "/Pattern << /P 10 0 R >>",
            "",
            vec![(
                10,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 60 60] /XStep 1000 /YStep 1000 /Resources << >>",
                    format!("{DASH} {long}").as_bytes(),
                ),
            )],
        )
    };
    assert_drawn(&tiling(""), "a tiling pattern alone");
    assert_refused(&tiling(&format!("{DASH} {long}")), "a tiling pattern");
    // An annotation's appearance.
    assert_refused(
        &doc_with(
            &format!("{DASH} {long}"),
            "",
            "/Annots [10 0 R]",
            vec![
                (
                    10,
                    b"<< /Type /Annot /Subtype /Square /Rect [0 0 60 60] /AP << /N 11 0 R >> >>"
                        .to_vec(),
                ),
                (
                    11,
                    stream_obj(
                        "/Type /XObject /Subtype /Form /BBox [0 0 60 60]",
                        format!("{DASH} {long}").as_bytes(),
                    ),
                ),
            ],
        ),
        "an annotation appearance",
    );
}

#[test]
fn each_page_render_has_its_own_budget() {
    let content = stream_obj("", format!("{DASH} {}", rect(1_000_000)).as_bytes());
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 60] /Contents 4 0 R >>".to_vec(),
        ),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 60] /Contents 4 0 R >>".to_vec(),
        ),
        (4, content),
    ]);
    let d = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions::new().with_dpi(36.0);
    // Each page, rendered in turn and again, is within the budget.
    for _ in 0..2 {
        for p in d.pages() {
            render_page(&d, p, &opts).unwrap();
        }
    }
}

#[test]
fn ordinary_dashes_draw_as_before() {
    // Dashes of every kind well within the budget: drawn, identically
    // in both modes, and something is painted.
    for content in [
        "2 w [6 2] 0 d 5 5 m 55 55 l S",
        "1 w [1] 0 d 5 30 m 55 30 l S",
        "3 w [4 1 2] 2 d 5 5 50 50 re S",
        "2 w [] 0 d 5 5 m 55 5 l S",
        "2 w [0.5 0.5] 0 d 5 5 m 5 55 30 55 55 5 c S",
        "4 w 1 J [0 6] 0 d 5 10 m 55 10 l S",
        "2 w [5 3] 1.5 d q 0.5 0 0 0.5 10 10 cm 0 0 80 80 re S Q",
        "2 w [3 2] 0 d 0 0 30 30 re W n 5 5 m 55 55 l S",
    ] {
        let px = assert_drawn(&page(content), content);
        assert!(
            px.chunks(4).any(|p| p[..3] != [255, 255, 255]),
            "{content}: nothing painted"
        );
    }
}

/// More than a million dashes in one path: the dasher gives up (and the
/// rasteriser then draws nothing). Refused, in both modes, not drawn as
/// an empty stroke and reported as a page.
#[test]
fn a_stroke_of_more_than_a_million_dashes_is_refused() {
    let pdf = page("1 w [1 1] 0 d 0 30 m 2100000 30 l S");
    for recovering in [false, true] {
        match render(&pdf, recovering) {
            Err(
                e @ Error::Unsupported {
                    feature: "dashed stroke",
                    ..
                },
            ) => {
                assert!(e.to_string().contains("more than a million dashes"), "{e}")
            }
            other => panic!("recovery {recovering}: {:?}", other.map(|_| ())),
        }
    }
    // A path without extent dashes to nothing: drawn (as nothing).
    assert_drawn(&page("1 w [1 1] 0 d 30 30 m 30 30 l S"), "a point");
    // Dots of zero length ([0 2] with round caps) are dashes: drawn.
    let px = assert_drawn(&page("3 w 1 J [0 4] 0 d 5 30 m 55 30 l S"), "dots");
    assert!(px.chunks(4).any(|p| p[..3] != [255, 255, 255]));
}

/// Inputs at the edges of what a dash can be: each is drawn or refused
/// with an error, the same in both modes, and none runs long (each is
/// bounded before the dashing that would).
#[test]
fn extreme_dashes_end_in_a_drawing_or_an_error() {
    for content in [
        "1 w [0.0001 0.0001] 0 d 5 30 m 55 30 l S",
        "1 w [1e30 1] 0 d 5 30 m 55 30 l S",
        "1 w [1 1] 1000000000 d 5 30 m 55 30 l S",
        "1 w [-1 2] 0 d 5 30 m 55 30 l S",
        "1 w [0 0] 0 d 5 30 m 55 30 l S",
        "1 w [1 1] 0 d 5 30 m 5 30 l 5 30 l S",
        "1 w [1 1] 0 d q 1000000 0 0 1000000 0 0 cm 0 0 m 0.00005 0.00005 l S Q",
        "1 w [1 1] 0 d q 0.000001 0 0 0.000001 0 0 cm 0 0 m 1000000 1000000 l S Q",
        "1 w [3 1] 0 d 0 0 m 1000000 0 l 1000000 1000000 l 0 1000000 l h S",
        "1 w [1 1] 0 d 0 0 m 100000 100000 1000000 0 0 1000000 c S",
    ] {
        let t = std::time::Instant::now();
        let (a, b) = (render(&page(content), false), render(&page(content), true));
        match (&a, &b) {
            (Ok(x), Ok(y)) => assert!(x == y, "{content}: the modes differ"),
            (Err(x), Err(y)) => assert_eq!(x.to_string(), y.to_string(), "{content}"),
            _ => panic!(
                "{content}: {:?} / {:?}",
                a.as_ref().map(|_| ()),
                b.as_ref().map(|_| ())
            ),
        }
        assert!(t.elapsed().as_secs() < 30, "{content}: {:?}", t.elapsed());
    }
}

/// A stroke wide enough to cover the page draws one dash only, but every
/// dash was built and walked: each counts (as a butt-capped segment's
/// edges, 4), so repeating it runs out of the budget like any dashed
/// stroke. (Review.)
#[test]
fn covering_strokes_are_counted_in_full() {
    let line = "0 30 m 1000000 30 l S";
    let wide = "3000000 w 1 J [1 1] 0 d";
    // One: 500 000 dashes, 2 × 10⁶ units — drawn, covering the page.
    let px = assert_drawn(&page(&format!("{wide} {line}")), "one covering stroke");
    assert!(
        px.chunks(4).all(|p| p[..3] == [0, 0, 0]),
        "the page is covered"
    );
    // Two: 4 × 10⁶, the budget exactly — drawn. Three: over it.
    assert_drawn(
        &page(&format!("{wide} {line} {line}")),
        "two covering strokes",
    );
    assert_refused(
        &page(&format!("{wide} {line} {line} {line}")),
        "three covering strokes",
    );
    // 512: refused before the dashing of all of them.
    let many: String = (0..512).map(|_| line).collect::<Vec<_>>().join(" ");
    let t = std::time::Instant::now();
    assert_refused(&page(&format!("{wide} {many}")), "512 covering strokes");
    assert!(t.elapsed().as_secs() < 60, "took {:?}", t.elapsed());
}

/// A path that dashes to nothing — it lies in a gap, or it is several
/// points — is drawn as nothing, not refused as a stroke the dasher gave
/// up on. (Review.)
#[test]
fn a_path_in_the_gaps_is_drawn_as_nothing() {
    for content in [
        "1 w [3 100] 4 d 5 30 m 10 30 l S",
        "1 w [3 100] 4 d 5 30 m 5 50 l S",
        "1 w [1 1] 0 d 5 5 m 5 5 l 30 30 m 30 30 l S",
        "1 w [2 50] 3 d 5 30 m 20 30 25 30 30 30 c S",
    ] {
        let px = assert_drawn(&page(content), content);
        assert!(
            px.chunks(4).all(|p| p[..3] == [255, 255, 255]),
            "{content}: painted"
        );
    }
}
