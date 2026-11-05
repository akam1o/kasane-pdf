//! Dashed strokes whose rasterisation work explodes (found by
//! `fuzz-nightly`, v0.5.1 R4; present since 0.1): round or square caps
//! on a dash pattern along a long path made a few kilobytes of content
//! cost minutes. Such a stroke now renders exactly — the dash segments
//! that cannot reach the page are dropped, and when every segment covers
//! the page one is enough — or is refused (`Unsupported`, "dashed
//! stroke"), never slowly and never approximately.

mod common;

use common::build_pdf;
use kasane_pdf::{render_page, Document, Error, RenderOptions};
use std::time::{Duration, Instant};

fn render(content: &str, size: u32) -> (Result<Vec<u8>, Error>, Duration) {
    let b = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {size} {size}] /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (
            4,
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
            .into_bytes(),
        ),
    ]);
    let d = Document::from_bytes(&b).unwrap();
    let t = Instant::now();
    let r = render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(72.0))
        .map(|p| p.pixels().to_vec());
    (r, t.elapsed())
}

/// The bound is generous for a debug build; before the fix each of these
/// took from tens of seconds to minutes in a release build.
const QUICK: Duration = Duration::from_secs(20);

/// 250 000 dash segments with round caps, all on the page (a back and
/// forth line, 15 KB of content): refused or drawn, quickly.
#[test]
fn many_overlapping_round_capped_dashes_are_bounded() {
    let zigzag: String = (0..1666)
        .map(|i| if i % 2 == 0 { "600 300 l " } else { "0 300 l " })
        .collect();
    for w in [10, 100, 600] {
        let (r, t) = render(&format!("1 J [3 1] 0 d {w} w 0 300 m {zigzag}S"), 600);
        assert!(t < QUICK, "width {w}: {t:?}");
        match r {
            Ok(_) => {}
            Err(Error::Unsupported {
                feature: "dashed stroke",
                ..
            }) => {}
            Err(e) => panic!("width {w}: {e:?}"),
        }
    }
}

/// The fuzz input's pattern: square caps, a dash, a width that covers
/// the page from every point of a very long path. Every segment covers
/// the page, so it is drawn exactly — the page fully painted — and fast.
#[test]
fn page_covering_dashes_draw_exactly_and_fast() {
    for cap in [1, 2] {
        let (r, t) = render(
            &format!("{cap} J [3 1] 5 d 99999999999999999999 w -20 5 1000000 -20 re S"),
            600,
        );
        assert!(t < QUICK, "cap {cap}: {t:?}");
        let px = r.unwrap_or_else(|e| panic!("cap {cap}: {e:?}"));
        assert!(
            px.chunks(4).all(|p| p[..3] == [0, 0, 0]),
            "cap {cap}: not fully painted"
        );
    }
}

/// Ordinary dashed drawing is unaffected: a dashed grid over a whole
/// page at 300 dpi and a dashed border in every cap style render.
#[test]
fn ordinary_dashed_drawings_render() {
    let grid: String = (0..60)
        .map(|i| {
            format!(
                "0 {y} m 600 {y} l {x} 0 m {x} 600 l ",
                y = i * 10,
                x = i * 10
            )
        })
        .collect();
    for cap in [0, 1, 2] {
        let content =
            format!("{cap} J [4 2] 0 d 1 w {grid}S 0 0 1 RG 5 w [12 6] 3 d 20 20 560 560 re S");
        let b = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 600] /Contents 4 0 R >>".to_vec(),
            ),
            (
                4,
                format!(
                    "<< /Length {} >>\nstream\n{content}\nendstream",
                    content.len()
                )
                .into_bytes(),
            ),
        ]);
        let d = Document::from_bytes(&b).unwrap();
        let t = Instant::now();
        let px = render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(300.0))
            .unwrap_or_else(|e| panic!("cap {cap}: {e:?}"));
        assert!(t.elapsed() < QUICK, "cap {cap}: {:?}", t.elapsed());
        assert!(
            px.pixels().chunks(4).any(|p| p[..3] != [255, 255, 255]),
            "cap {cap}: blank"
        );
    }
}
