//! The content walker shared by `diagnose_redaction` and the region
//! editors composes a nested `cm`, and a form's `/Matrix`, as
//! M × CTM (ISO 32000-2 §8.4.4, §8.10.1): content is located where it
//! is drawn.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{diagnose_redaction, remove_region, Document, Limits, TargetKind};

fn page(content: &[u8], xobjects: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                 /Resources << /XObject << {xobjects} >> >> /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

fn image() -> Vec<u8> {
    let samples: Vec<u8> = (0..16 * 16 * 3).map(|i| (i * 37 % 251 + 1) as u8).collect();
    stream_obj(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8",
        &samples,
    )
}

/// An image under two `cm`: drawn at (0, 50)–(100, 80).
fn nested_cm_image() -> Vec<u8> {
    page(
        b"q 1 0 0 1 0 50 cm 100 0 0 30 0 0 cm /Im Do Q",
        "/Im 5 0 R",
        vec![(5, image())],
    )
}

#[test]
fn an_image_under_nested_cm_is_located_and_removed() {
    let doc = Document::from_bytes(&nested_cm_image()).unwrap();
    let rect = [(0.0, 50.0, 100.0, 80.0)];
    let d = diagnose_redaction(&doc, 0, &rect, &Limits::default()).unwrap();
    let img = d
        .targets
        .iter()
        .find(|t| t.kind == TargetKind::Image)
        .expect("the image is a target");
    let (x0, y0, x1, y1) = img.bounds;
    assert!(
        (x0, y0, x1, y1) == (0.0, 50.0, 100.0, 80.0),
        "bounds {:?}",
        img.bounds
    );
    assert!(!img.hits.is_empty(), "the rect that contains it is a hit");
    let r = remove_region(&doc, 0, &rect, &Limits::default()).unwrap();
    assert_eq!(r.pixels_changed, 256);
}

/// A form's /Matrix applies before the CTM: a form scaled 2× under a
/// translation by (50, 0) draws `10 10 10 10 re` at (70, 20)–(90, 40).
#[test]
fn a_form_matrix_under_cm_is_located() {
    let pdf = page(
        b"q 1 0 0 1 50 0 cm /Fm Do Q",
        "/Fm 5 0 R",
        vec![(
            5,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Matrix [2 0 0 2 0 0] \
                 /Resources << >>",
                b"10 10 10 10 re f",
            ),
        )],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let d = diagnose_redaction(&doc, 0, &[(65.0, 15.0, 95.0, 45.0)], &Limits::default()).unwrap();
    let path = d
        .targets
        .iter()
        .find(|t| t.kind == TargetKind::Path)
        .expect("the path is a target");
    let (x0, y0, x1, y1) = path.bounds;
    assert!(
        (x0 - 70.0).abs() < 1e-9
            && (y0 - 20.0).abs() < 1e-9
            && (x1 - 90.0).abs() < 1e-9
            && (y1 - 40.0).abs() < 1e-9,
        "bounds {:?}",
        path.bounds
    );
    assert!(!path.hits.is_empty());
}
