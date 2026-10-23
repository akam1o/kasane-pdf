//! Inline images (ISO 32000-1 §8.9.7): `BI … ID … EI` through the
//! image XObject pipeline — abbreviated keys and filters, `/Decode`,
//! `/ImageMask`, CTM, clipping, several images per stream, and the
//! data-end rules that keep an `EI` byte pair inside the data from
//! ending it early. 100×100 pt page at 72 dpi.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

fn page(content: &[u8], colorspaces: &str) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /ColorSpace << {colorspaces} >> >> /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ])
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).expect("in bounds");
    (p.red(), p.green(), p.blue())
}

const WHITE: (u8, u8, u8) = (255, 255, 255);

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn inline_gray_image_with_abbreviated_keys() {
    // 2×1 gray: black | white, scaled 40×40 at (10,10).
    let c = cat(&[b"q 40 0 0 40 10 10 cm BI /W 2 /H 1 /CS /G /BPC 8 ID \x00\xff EI Q"]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 0, 0));
    assert_eq!(px(&r, 40, 70), WHITE);
    assert_eq!(px(&r, 5, 5), WHITE);
}

#[test]
fn inline_rgb_with_decode_and_full_key_names() {
    let c = cat(&[
        b"q 40 0 0 40 10 10 cm BI /Width 1 /Height 1 /ColorSpace /DeviceRGB \
          /BitsPerComponent 8 /Decode [1 0 1 0 1 0] ID \x00\xff\xff EI Q",
    ]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 30, 70), (255, 0, 0));
}

#[test]
fn inline_flate_filter_abbreviation() {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &[0u8, 0, 255]).unwrap();
    let enc = enc.finish().unwrap();
    let c = cat(&[
        b"q 40 0 0 40 10 10 cm BI /W 1 /H 1 /CS /RGB /BPC 8 /F /Fl ID ",
        &enc,
        b" EI Q 0 1 0 rg 60 60 20 20 re f",
    ]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 30, 70), (0, 0, 255));
    assert_eq!(px(&r, 70, 30), (0, 255, 0), "content after EI still runs");
}

#[test]
fn inline_image_mask_paints_fill_colour() {
    // 8×1 bits 0b0101_0101: bit 0 painted (blue), bit 1 not.
    let c = cat(&[b"0 0 1 rg q 80 0 0 10 10 10 cm BI /W 8 /H 1 /IM true ID \x55 EI Q"]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 15, 85), (0, 0, 255));
    assert_eq!(px(&r, 25, 85), WHITE);
}

#[test]
fn inline_image_named_colour_space_resource() {
    let c = cat(&[b"q 40 0 0 40 10 10 cm BI /W 1 /H 1 /CS /CS0 /BPC 8 ID \x01 EI Q"]);
    let r = render(&page(&c, "/CS0 [/Indexed /DeviceRGB 1 <FF000000FF00>]")).unwrap();
    assert_eq!(px(&r, 30, 70), (0, 255, 0));
}

#[test]
fn inline_image_respects_clip_and_ctm() {
    let c =
        cat(&[b"0 0 30 100 re W n q 40 0 0 40 10 10 cm BI /W 1 /H 1 /CS /G /BPC 8 ID \x00 EI Q"]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 0, 0));
    assert_eq!(px(&r, 40, 70), WHITE);
}

#[test]
fn several_inline_images_in_one_stream() {
    let c = cat(&[
        b"q 20 0 0 20 10 10 cm BI /W 1 /H 1 /CS /RGB /BPC 8 ID \xff\x00\x00 EI Q \
          q 20 0 0 20 60 60 cm BI /W 1 /H 1 /CS /RGB /BPC 8 ID \x00\x00\xff EI Q",
    ]);
    let r = render(&page(&c, "")).unwrap();
    assert_eq!(px(&r, 20, 80), (255, 0, 0));
    assert_eq!(px(&r, 70, 30), (0, 0, 255));
}

#[test]
fn data_spelling_ei_is_not_cut_short() {
    // 4×1 gray whose bytes are " EI " — the computed length wins.
    let c = cat(&[b"q 40 0 0 40 10 10 cm BI /W 4 /H 1 /CS /G /BPC 8 ID  EI  EI Q"]);
    let r = render(&page(&c, "")).unwrap();
    // Sample values 0x20 'E' 'I' 0x20 → dark greys, not white.
    assert_eq!(px(&r, 15, 70), (32, 32, 32));
    assert_eq!(px(&r, 25, 70), (69, 69, 69));
}

#[test]
fn invalid_inline_images_are_typed_errors() {
    // Truncated data.
    let err = render(&page(b"BI /W 4 /H 4 /CS /RGB /BPC 8 ID \x00\x00", "")).unwrap_err();
    assert!(
        matches!(err, Error::Parse(_) | Error::UnexpectedEof { .. }),
        "{err:?}"
    );
    // Missing EI after the computed length.
    let err = render(&page(b"BI /W 1 /H 1 /CS /G /BPC 8 ID \x00 0 g", "")).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
    // Bad dimensions / depth.
    let err = render(&page(b"BI /W 0 /H 1 /CS /G /BPC 8 ID  EI", "")).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let err = render(&page(b"BI /W 1 /H 1 /CS /G /BPC 3 ID \x00 EI", "")).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // Unknown colour-space resource reaches the renderer as Structure.
    let err = render(&page(
        b"q 10 0 0 10 0 0 cm BI /W 1 /H 1 /CS /Nope /BPC 8 ID \x00 EI Q",
        "",
    ))
    .unwrap_err();
    assert!(
        matches!(err, Error::Structure(_) | Error::Parse(_)),
        "{err:?}"
    );
    // Corrupt flate data is a parse error, not a panic.
    let err = render(&page(
        b"q 10 0 0 10 0 0 cm BI /W 1 /H 1 /CS /G /BPC 8 /F /Fl ID \xff\xfe EI Q",
        "",
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}
