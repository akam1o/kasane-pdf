//! Clip and soft masks held by the graphics state (v0.5.1, found while
//! measuring R2): a saved state shares its masks instead of copying
//! them, a form BBox or clip that cuts nothing keeps the mask, and the
//! masks alive at once are bounded by `max_render_buffer_bytes` (an
//! allowance of their own beside the page and its layers). Before, every
//! `q` and every form level copied the clip mask (one byte per page
//! pixel), outside any limit.

mod common;

use common::build_pdf;
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};

/// A 600 × 600 pt page (600 × 600 px at 72 dpi: a 1.44 MB pixmap, a
/// 0.36 MB mask) with `content`; `/X` is a chain of `forms` nested
/// forms, each clipped to its `/BBox`.
fn page(content: &str, forms: u32) -> Vec<u8> {
    chain(content, forms, false)
}

fn chain(content: &str, forms: u32, same_bbox: bool) -> Vec<u8> {
    let s = |d: &str, c: &str| {
        format!("<< {d} /Length {} >>\nstream\n{c}\nendstream", c.len()).into_bytes()
    };
    let mut o = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 600] /Resources << /XObject << /X 100 0 R >> >> /Contents 4 0 R >>".to_vec()),
        (4, s("", content)),
    ];
    for i in 0..forms {
        let (res, body) = if i + 1 == forms {
            (String::new(), "0 0 1 rg 0 0 10 10 re f")
        } else {
            (
                format!("/Resources << /XObject << /X {} 0 R >> >>", 101 + i),
                "/X Do",
            )
        };
        // Each BBox one unit smaller (every level a different clip), or
        // all the same.
        let (a, b) = if same_bbox { (0, 500) } else { (i, 500 - i) };
        o.push((
            100 + i,
            s(
                &format!("/Type /XObject /Subtype /Form /BBox [{a} {a} {b} {b}] {res}"),
                body,
            ),
        ));
    }
    build_pdf(&o)
}

fn render(b: &[u8], buffer: u64) -> Result<Vec<u8>, Error> {
    let d = Document::from_bytes(b).unwrap();
    let l = Limits::default().with_max_render_buffer_bytes(buffer);
    render_page(
        &d,
        &d.pages()[0],
        &RenderOptions::new().with_dpi(72.0).with_limits(l),
    )
    .map(|r| r.pixels().to_vec())
}

const MIB: u64 = 1 << 20;

/// 200 nested, distinct clips are 200 masks alive at once (72 MB):
/// refused under a 32 MiB budget, drawn under the default.
#[test]
fn distinct_live_clips_count_against_the_buffer_limit() {
    let content: String = (0..200)
        .map(|i| format!("q {i} {i} 400 400 re W n\n"))
        .collect::<String>()
        + "0 0 1 rg 0 0 600 600 re f";
    let b = page(&content, 1);
    match render(&b, 32 * MIB) {
        Err(Error::LimitExceeded {
            limit: "max_render_buffer_bytes",
            ..
        }) => {}
        other => panic!("{:?}", other.map(|p| p.len())),
    }
    assert!(render(&b, 256 * MIB).is_ok());
}

/// One clip saved by 1000 `q`s is one mask: a further clip inside
/// copies it once (the saved states keep the original), so the page
/// fits a 4 MiB budget and draws exactly what the unsaved clips draw.
#[test]
fn a_saved_clip_is_shared_not_copied() {
    let clips = "10 10 400 400 re W n";
    let inner = "20 20 300 300 re W n 0 0 1 rg 0 0 600 600 re f";
    let saved = format!("{clips} {} {inner}", "q ".repeat(1000));
    let a = render(&page(&saved, 1), 4 * MIB).expect("one shared mask");
    assert_eq!(
        a,
        render(&page(&format!("{clips} {inner}"), 1), 4 * MIB).unwrap()
    );
}

/// Nested forms clip to their BBox at every level: 48 distinct levels
/// are 48 masks (17 MB here) — inside the default budget, refused
/// under a budget below it.
#[test]
fn nested_form_clips_are_bounded() {
    let b = page("/X Do", 48);
    assert!(render(&b, 256 * MIB).is_ok());
    assert!(matches!(
        render(&b, 8 * MIB),
        Err(Error::LimitExceeded {
            limit: "max_render_buffer_bytes",
            ..
        })
    ));
}

/// A BBox that encloses the clip already in force changes nothing, so
/// 48 nested forms with one BBox keep one mask: they fit a 4 MiB budget
/// and draw exactly what a single form draws.
#[test]
fn enclosing_bboxes_keep_the_mask() {
    let deep = render(&chain("/X Do", 48, true), 4 * MIB).expect("one mask");
    assert_eq!(deep, render(&chain("/X Do", 1, true), 4 * MIB).unwrap());
}

/// A clip that cuts nothing away from the clip in force (the same
/// rectangle again, in each of 1000 nested `q`s) keeps the shared mask.
#[test]
fn a_clip_that_cuts_nothing_keeps_the_mask() {
    let repeated = "q 10 10 400 400 re W n ".repeat(1000) + "0 0 1 rg 0 0 600 600 re f";
    let once = "10 10 400 400 re W n 0 0 1 rg 0 0 600 600 re f";
    let a = render(&page(&repeated, 1), 4 * MIB).expect("one shared mask");
    assert_eq!(a, render(&page(once, 1), 4 * MIB).unwrap());
}
