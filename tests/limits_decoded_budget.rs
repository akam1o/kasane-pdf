//! `max_total_decoded_bytes` is a per-operation budget: every page
//! render (or text extraction, or rewrite) on one `Document` starts
//! with the whole budget, while a single operation that decodes more
//! than the budget still fails.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, render_page, rewrite_with_limits, Document, Error, Limits, RenderOptions,
};

/// Two pages whose content streams decode to `n` bytes each.
fn two_pages(n: usize) -> Vec<u8> {
    let mut content = b"0 0 1 rg 10 10 50 50 re f".to_vec();
    while content.len() < n {
        content.push(b' ');
    }
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("", &content)),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents [6 0 R 7 0 R] >>"
                .to_vec(),
        ),
        (6, stream_obj("", &content)),
        (7, stream_obj("", &content)),
    ])
}

fn limits(budget: u64) -> Limits {
    Limits {
        max_total_decoded_bytes: budget,
        ..Limits::default()
    }
}

#[test]
fn every_page_gets_the_whole_budget() {
    // Each page-1 render decodes 1000 bytes; the budget is 1500, so
    // rendering page 1 three times used to exhaust it on the second.
    let pdf = two_pages(1000);
    let doc = Document::from_bytes_with_limits(&pdf, limits(1500)).unwrap();
    for _ in 0..3 {
        render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    }
    extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    // Page 2 decodes 2000 bytes in one operation: over budget, refused.
    let err = render_page(&doc, &doc.pages()[1], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::LimitExceeded {
                limit: "max_total_decoded_bytes",
                ..
            }
        ),
        "{err:?}"
    );
    // The failed operation does not poison the next one.
    render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
}

#[test]
fn rewrite_is_one_operation() {
    // A rewrite touches both pages' streams (3000 bytes): fine with a
    // 4000-byte budget even after renders, not with 2500.
    let pdf = two_pages(1000);
    let doc = Document::from_bytes_with_limits(&pdf, limits(4000)).unwrap();
    render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    rewrite_with_limits(&doc, &Limits::default()).unwrap();
    rewrite_with_limits(&doc, &Limits::default()).unwrap();
    let doc = Document::from_bytes_with_limits(&pdf, limits(2500)).unwrap();
    // Streams travel encoded through a rewrite, so whether it decodes
    // them at all is an implementation detail; what must hold is that
    // the outcome does not depend on operations that came before.
    let a = rewrite_with_limits(&doc, &Limits::default()).is_ok();
    render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let b = rewrite_with_limits(&doc, &Limits::default()).is_ok();
    assert_eq!(a, b);
}
