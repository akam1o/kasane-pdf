//! Extreme line widths (found by the `render_content` fuzz target,
//! v0.5.1 R4): a stroke whose width is far outside the page must render
//! or refuse with a typed error, never panic in the rasteriser.

mod common;

use common::build_pdf;
use kasane_pdf::{render_page, Document, RenderOptions};

fn page(content: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 60] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec()),
        (4, format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes()),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
    ])
}

#[test]
fn huge_line_widths_never_panic() {
    for w in ["99999999999999999999", "1e30", "3.4e38", "100000000"] {
        for body in [
            format!("{w} w BT /F1 8 Tf 1 20 Td 2 Tr (Hi) Tj ET"),
            format!("{w} w BT /F1 8 Tf 1 20 Td 1 Tr [(a) -120 (b)] TJ ET"),
            format!("{w} w 0 0 m 60 60 l S"),
            format!("{w} w 10 10 40 40 re B"),
        ] {
            let b = page(&body);
            let r = std::panic::catch_unwind(move || {
                let d = Document::from_bytes(&b).unwrap();
                render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(36.0)).map(|_| ())
            });
            assert!(r.is_ok(), "panicked: {body}");
        }
    }
}

fn pixels(body: &str) -> Result<Vec<u8>, kasane_pdf::Error> {
    let b = page(body);
    let d = Document::from_bytes(&b).unwrap();
    render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(36.0))
        .map(|r| r.pixels().to_vec())
}

/// A stroke that covers the whole page is narrowed without a pixel of
/// difference: 10^20 draws what 10^9 (below the bound) draws.
#[test]
fn a_covering_stroke_is_narrowed_exactly() {
    for shape in [
        "10 10 40 40 re S",
        "1 j 0 0 m 60 0 l 0 60 l S",
        "BT /F1 8 Tf 1 20 Td 1 Tr (Hi) Tj ET",
    ] {
        let wide = pixels(&format!("0 0 1 RG 99999999999999999999 w {shape}")).unwrap();
        let below = pixels(&format!("0 0 1 RG 1000000000 w {shape}")).unwrap();
        assert_eq!(wide, below, "{shape}");
        assert_eq!(
            pixels(&format!("0 0 1 RG 1000000000000 w {shape}")).unwrap(),
            below,
            "{shape}"
        );
    }
}

/// One that cannot be narrowed — a near-degenerate transform makes the
/// narrow direction need a width the rasteriser cannot take — is
/// refused with a typed error.
#[test]
fn an_unnarrowable_stroke_is_refused() {
    match pixels("0.0000001 0 0 1000 0 0 cm 99999999999999999999 w 0 0 1 1 re S") {
        Err(kasane_pdf::Error::Unsupported {
            feature: "stroke width",
            ..
        }) => {}
        other => panic!("{:?}", other.map(|p| p.len())),
    }
}
