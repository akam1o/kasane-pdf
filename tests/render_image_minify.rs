//! Minified images are area-averaged: a device pixel is the mean of the
//! source samples its footprint covers, so a fine checkerboard reduced
//! to one pixel is grey, not whichever squares a sampling grid hits.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, RenderOptions};

/// 8 × 8 one-sample squares, alternating 0 and 255.
fn checker_gray() -> Vec<u8> {
    (0..64)
        .map(|i| if (i % 8 + i / 8) % 2 == 0 { 0 } else { 255 })
        .collect()
}

/// The same checkerboard as 1-bit rows (one byte per row).
fn checker_bits() -> Vec<u8> {
    (0..8)
        .map(|r| if r % 2 == 0 { 0x55 } else { 0xAA })
        .collect()
}

fn pixel(content: &str, image: (u32, Vec<u8>)) -> [u8; 3] {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
              /Resources << /XObject << /Im 5 0 R >> /Pattern << /P 6 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        image,
        (
            6,
            b"<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB \
              /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] \
              /C0 [1 0 0] /C1 [1 0 0] /N 1 >> >> >>"
                .to_vec(),
        ),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0)).unwrap();
    let p = r.pixmap().pixel(50, 49).unwrap();
    [p.red(), p.green(), p.blue()]
}

fn image(dict: &str, data: Vec<u8>) -> (u32, Vec<u8>) {
    let mut s = format!(
        "<< /Type /XObject /Subtype /Image /Width 8 /Height 8 {dict} /Length {} >>\nstream\n",
        data.len()
    )
    .into_bytes();
    s.extend_from_slice(&data);
    s.extend_from_slice(b"\nendstream");
    (5, s)
}

fn near(v: u8, want: u8) -> bool {
    v.abs_diff(want) <= 2
}

/// The 8 × 8 image drawn into the one device pixel (50, 49).
const ONE_PIXEL: &str = "q 1 0 0 1 50 50 cm /Im Do Q";

#[test]
fn colour_image() {
    let p = pixel(
        ONE_PIXEL,
        image(
            "/ColorSpace /DeviceGray /BitsPerComponent 8",
            checker_gray(),
        ),
    );
    assert!(near(p[0], 128), "{p:?}");
}

#[test]
fn stencil_mask() {
    let p = pixel(
        &format!("0 g {ONE_PIXEL}"),
        image("/ImageMask true", checker_bits()),
    );
    assert!(near(p[0], 128), "{p:?}");
    // Painted with a pattern, the stencil is a coverage mask: half red.
    let p = pixel(
        &format!("/Pattern cs /P scn {ONE_PIXEL}"),
        image("/ImageMask true", checker_bits()),
    );
    assert!(near(p[0], 255) && near(p[1], 128), "{p:?}");
}
