//! Images whose stored size exceeds the image limits while the size they
//! are drawn at does not: a JPEG is decoded with a reduced inverse DCT
//! (reported on `RenderedPage::image_approximations`), a plain soft mask
//! is sampled directly at the compositing grid. Anything else stays a
//! `LimitExceeded`.

mod common;

use common::{build_pdf, fixture, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

fn page(content: &str, image: Vec<u8>, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
               /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (5, image),
    ];
    objs.extend(extra.iter().cloned());
    build_pdf(&objs)
}

fn jpeg_image() -> Vec<u8> {
    let jpg = std::fs::read(fixture("dct_64x64_red_blue.jpg")).unwrap();
    stream_obj(
        "/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8 /Filter /DCTDecode",
        &jpg,
    )
}

fn render(pdf: &[u8], limits: Limits) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions::default().with_limits(limits),
    )
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let i = y as usize * r.stride() + x as usize * 4;
    let p = &r.pixels()[i..i + 4];
    (p[0], p[1], p[2])
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8)) -> bool {
    [(a.0, b.0), (a.1, b.1), (a.2, b.2)]
        .iter()
        .all(|(x, y)| (*x as i32 - *y as i32).abs() <= 24)
}

#[test]
fn oversized_jpeg_is_decoded_at_reduced_size_when_drawn_small() {
    // 64 × 64 JPEG drawn at 10 × 10 pt (10 px at 72 dpi) under a
    // 16-sample dimension limit: 1/8 (8 px) is too coarse, 1/4 (16 px)
    // keeps one sample per pixel and fits.
    let pdf = page("q 10 0 0 10 20 20 cm /Im Do Q", jpeg_image(), &[]);
    let limits = Limits::default().with_max_image_dimension(16);
    let r = render(&pdf, limits).unwrap();
    // Pixel row = 100 − y. Left half red, right half blue.
    assert!(near(px(&r, 22, 75), (255, 0, 0)), "{:?}", px(&r, 22, 75));
    assert!(near(px(&r, 28, 75), (0, 0, 255)), "{:?}", px(&r, 28, 75));
    let notes = r.image_approximations();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("64x64") && notes[0].contains("16x16"),
        "{notes:?}"
    );
    // At full size the same image needs no note.
    let r = render(&pdf, Limits::default()).unwrap();
    assert!(r.image_approximations().is_empty());
}

#[test]
fn oversized_jpeg_drawn_large_uses_the_finest_fitting_scale() {
    // Drawn at 90 × 90 px: no reduced scale both fits the 16-sample limit
    // and keeps the drawn resolution, so the finest that fits (1/4, 16 px)
    // is used and the note says it is below the drawn resolution.
    let pdf = page("q 90 0 0 90 5 5 cm /Im Do Q", jpeg_image(), &[]);
    let r = render(&pdf, Limits::default().with_max_image_dimension(16)).unwrap();
    let notes = r.image_approximations();
    assert!(
        notes.len() == 1 && notes[0].contains("below the drawn resolution"),
        "{notes:?}"
    );
    // When even 1/8 (8 px) exceeds the limit, the limit stands.
    let e = render(&pdf, Limits::default().with_max_image_dimension(7)).unwrap_err();
    assert!(
        matches!(
            e,
            Error::LimitExceeded {
                limit: "max_image_dimension",
                ..
            }
        ),
        "{e:?}"
    );
}

/// A 200 × 200 one-bit soft mask, left half opaque, right half
/// transparent, Flate-encoded.
fn half_mask() -> Vec<u8> {
    let mut rows = Vec::new();
    for _ in 0..200 {
        let mut row = vec![0u8; 25];
        for b in row.iter_mut().take(12) {
            *b = 0xFF;
        }
        row[12] = 0xF0; // bits 96..99 set: x < 100 opaque
        rows.extend_from_slice(&row);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut z, &rows).unwrap();
    stream_obj(
        "/Type /XObject /Subtype /Image /Width 200 /Height 200 /ColorSpace /DeviceGray \
         /BitsPerComponent 1 /Filter /FlateDecode",
        &z.finish().unwrap(),
    )
}

fn red_2x2_with_mask() -> Vec<u8> {
    stream_obj(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8 /SMask 6 0 R",
        &[255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0],
    )
}

#[test]
fn finer_soft_mask_is_composited_at_its_resolution() {
    // The 2 × 2 image is drawn at 80 × 80 px; its 200 × 200 mask cuts
    // it in half. Compositing on the image's 2 × 2 grid would put the
    // edge at a sample boundary by luck only; at the mask's resolution
    // (capped at 80 × 80) the edge is where the mask says.
    let pdf = page(
        "q 80 0 0 80 10 10 cm /Im Do Q",
        red_2x2_with_mask(),
        &[(6, half_mask())],
    );
    let r = render(&pdf, Limits::default()).unwrap();
    assert!(near(px(&r, 30, 50), (255, 0, 0)), "{:?}", px(&r, 30, 50));
    assert!(
        near(px(&r, 70, 50), (255, 255, 255)),
        "{:?}",
        px(&r, 70, 50)
    );
    assert!(r.image_approximations().is_empty());
}

#[test]
fn oversized_plain_soft_mask_is_sampled_directly() {
    // The mask exceeds a 100-sample dimension limit; the compositing
    // grid (80 × 80) does not, so the mask is sampled directly — same
    // result as without the limit.
    let pdf = page(
        "q 80 0 0 80 10 10 cm /Im Do Q",
        red_2x2_with_mask(),
        &[(6, half_mask())],
    );
    let limited = render(&pdf, Limits::default().with_max_image_dimension(100)).unwrap();
    let full = render(&pdf, Limits::default()).unwrap();
    assert_eq!(limited.pixels(), full.pixels());
}

#[test]
fn jpeg_frame_must_match_the_dictionary_before_decoding() {
    // The dictionary claims 8 × 8, the frame is 64 × 64: refused before
    // the decoder allocates anything for the larger frame.
    let jpg = std::fs::read(fixture("dct_64x64_red_blue.jpg")).unwrap();
    let image = stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8 /Filter /DCTDecode",
        &jpg,
    );
    let pdf = page("q 10 0 0 10 20 20 cm /Im Do Q", image, &[]);
    let e = render(&pdf, Limits::default()).unwrap_err();
    assert!(
        matches!(&e, Error::Structure(m) if m.contains("JPEG frame is 64x64")),
        "{e:?}"
    );
}

#[test]
fn progressive_jpeg_coefficients_are_bounded_by_max_image_bytes() {
    // A progressive frame holds 2 bytes per DCT coefficient of the full
    // image: 64 blocks × 64 × 2 × 3 = 24 576 bytes here, whatever the
    // output size. Under a smaller max_image_bytes it is refused before
    // decoding; with room it renders.
    let jpg = std::fs::read(fixture("dct_64x64_red_blue_progressive.jpg")).unwrap();
    let image = stream_obj(
        "/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8 /Filter /DCTDecode",
        &jpg,
    );
    let pdf = page("q 10 0 0 10 20 20 cm /Im Do Q", image, &[]);
    let e = render(&pdf, Limits::default().with_max_image_bytes(20_000)).unwrap_err();
    assert!(
        matches!(
            e,
            Error::LimitExceeded {
                limit: "max_image_bytes",
                ..
            }
        ),
        "{e:?}"
    );
    let r = render(&pdf, Limits::default()).unwrap();
    assert!(near(px(&r, 22, 75), (255, 0, 0)), "{:?}", px(&r, 22, 75));
}

#[test]
fn colour_key_masked_jpeg_is_not_decoded_at_reduced_size() {
    // A colour key matches exact sample values, which a reduced inverse
    // DCT does not preserve: the oversized image stays refused.
    let jpg = std::fs::read(fixture("dct_64x64_red_blue.jpg")).unwrap();
    let image = stream_obj(
        "/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8 /Filter /DCTDecode /Mask [250 255 0 5 0 5]",
        &jpg,
    );
    let pdf = page("q 10 0 0 10 20 20 cm /Im Do Q", image, &[]);
    let e = render(&pdf, Limits::default().with_max_image_dimension(16)).unwrap_err();
    assert!(
        matches!(
            e,
            Error::LimitExceeded {
                limit: "max_image_dimension",
                ..
            }
        ),
        "{e:?}"
    );
}
