//! Automatic stroke adjustment (`/SA`, §10.7.5): off by default; with
//! `/SA true` axis-aligned rectangles and straight strokes get their
//! edges on whole device pixels, at least one pixel apart.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, RenderOptions};

fn pdf(content: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
              /Resources << /ExtGState << /On << /SA true >> /Off << /SA false >> >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ])
}

/// The grey levels of column `x` (rows 0..100) at 72 dpi, where device
/// y = 100 − user y.
fn column(content: &str, x: u32) -> Vec<u8> {
    let doc = Document::from_bytes(&pdf(content)).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0)).unwrap();
    (0..100)
        .map(|y| r.pixmap().pixel(x, y).unwrap().red())
        .collect()
}

fn grey_rows(c: &[u8]) -> usize {
    c.iter().filter(|&&v| v != 0 && v != 255).count()
}

/// A filled rectangle from y 40.3 to 40.9 (device rows 59.1–59.7):
/// exact, it is one grey row; adjusted, both edges round to 59 and are
/// pulled one pixel apart — row 59, black.
#[test]
fn rectangle_fill() {
    let exact = column("10 40.3 80 0.6 re f", 50);
    assert_eq!(grey_rows(&exact), 1);
    assert!(column("/Off gs 10 40.3 80 0.6 re f", 50) == exact);
    let adj = column("/On gs 10 40.3 80 0.6 re f", 50);
    assert_eq!(grey_rows(&adj), 0);
    assert_eq!(adj[59], 0);
    assert_eq!(adj.iter().filter(|&&v| v == 0).count(), 1);
}

/// A 0.5 pt horizontal line at y 40.5 covers device rows 59.25–59.75:
/// adjusted, a black row; a projecting cap extends it by half the width.
#[test]
fn straight_stroke() {
    let exact = column("0.5 w 10 40.5 m 90 40.5 l S", 50);
    assert_eq!(grey_rows(&exact), 1);
    let adj = column("/On gs 0.5 w 10 40.5 m 90 40.5 l S", 50);
    assert_eq!((grey_rows(&adj), adj[59]), (0, 0));
    // A rectangle outline: all four sides one black pixel wide.
    let frame = column("/On gs 0.5 w 10.2 20.2 79.6 59.6 re S", 50);
    assert_eq!(grey_rows(&frame), 0);
    assert_eq!(frame.iter().filter(|&&v| v == 0).count(), 2);
    // q/Q restores it like any graphics state parameter.
    assert!(column("q /On gs Q 0.5 w 10 40.5 m 90 40.5 l S", 50) == exact);
}

/// Shapes other than rectangles and straight segments stay exact.
#[test]
fn other_shapes_are_exact() {
    let tri = "10 40.3 m 90 40.3 l 50 40.9 l f";
    assert!(column(&format!("/On gs {tri}"), 50) == column(tri, 50));
    let dashed = "0.5 w [4 2] 0 d 10 40.5 m 90 40.5 l S";
    assert!(column(&format!("/On gs {dashed}"), 50) == column(dashed, 50));
}
