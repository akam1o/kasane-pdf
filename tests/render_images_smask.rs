//! Image soft masks (`/SMask`), stencil masks (`/ImageMask`, `/Mask`
//! streams), colour-key masks and sample depths other than 8 bits
//! (ISO 32000-1 §8.9.5–§8.9.6, §11.6.5.3).
//!
//! Every fixture is built inline: a 100×100 pt page at 72 dpi, so
//! pixmap (x, y) = user (x, 100 − y). Images are placed with `cm`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};

/// One page whose content is `content`; `xobjects` are `/Resources
/// /XObject` entries (`name` → object number), `extgstate` is the
/// raw `/ExtGState` dictionary body, `objs` the extra objects.
fn page(
    content: &[u8],
    xobjects: &[(&str, u32)],
    extgstate: &str,
    objs: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let xo: String = xobjects
        .iter()
        .map(|(n, o)| format!("/{n} {o} 0 R "))
        .collect();
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /XObject << {xo}>> /ExtGState << {extgstate} >> >> \
                 /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    all.extend(objs);
    build_pdf(&all)
}

fn render(pdf: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &kasane_pdf::RenderedPage, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).expect("in bounds");
    (p.red(), p.green(), p.blue(), p.alpha())
}

fn near(a: (u8, u8, u8, u8), b: (u8, u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol
        && (a.1 as i32 - b.1 as i32).abs() <= tol
        && (a.2 as i32 - b.2 as i32).abs() <= tol
        && a.3 == b.3
}

/// RGB image object `w×h` with samples `rgb`, plus `extra` dict keys.
fn rgb_image(w: u32, h: u32, rgb: &[u8], extra: &str) -> Vec<u8> {
    stream_obj(
        &format!(
            "/Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /BitsPerComponent 8 /ColorSpace /DeviceRGB {extra}"
        ),
        rgb,
    )
}

/// 8-bit DeviceGray soft-mask image object.
fn gray_mask(w: u32, h: u32, gray: &[u8], extra: &str) -> Vec<u8> {
    stream_obj(
        &format!(
            "/Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /BitsPerComponent 8 /ColorSpace /DeviceGray {extra}"
        ),
        gray,
    )
}

const WHITE: (u8, u8, u8, u8) = (255, 255, 255, 255);
const RED: (u8, u8, u8, u8) = (255, 0, 0, 255);

// --------------------------------------------------------------------
// /SMask
// --------------------------------------------------------------------

#[test]
fn smask_simple_left_opaque_right_transparent() {
    // 2×2 red image; mask columns 255 / 0 → left half painted only.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (
                5,
                rgb_image(
                    2,
                    2,
                    &[255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0],
                    "/SMask 6 0 R",
                ),
            ),
            (6, gray_mask(2, 2, &[255, 0, 255, 0], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 60), RED);
    assert_eq!(px(&r, 20, 80), RED);
    assert_eq!(px(&r, 40, 60), WHITE);
    assert_eq!(px(&r, 40, 80), WHITE);
    assert_eq!(px(&r, 5, 5), WHITE);
}

#[test]
fn smask_gradient_alpha_no_halo() {
    // 4×1 blue image under a 0/85/170/255 mask: each 10-pt column
    // blends blue over white by its alpha; no dark fringe anywhere.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (
                5,
                rgb_image(
                    4,
                    1,
                    &[0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255],
                    "/SMask 6 0 R",
                ),
            ),
            (6, gray_mask(4, 1, &[0, 85, 170, 255], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 15, 70), WHITE);
    assert!(
        near(px(&r, 25, 70), (170, 170, 255, 255), 2),
        "{:?}",
        px(&r, 25, 70)
    );
    assert!(
        near(px(&r, 35, 70), (85, 85, 255, 255), 2),
        "{:?}",
        px(&r, 35, 70)
    );
    assert_eq!(px(&r, 45, 70), (0, 0, 255, 255));
    // A transparent column never darkens: every channel ≥ the blend.
    for x in 11..49 {
        let (r_, g, b, _) = px(&r, x, 70);
        assert!(b == 255 && r_ == g, "halo at x={x}: {:?}", (r_, g, b));
    }
}

#[test]
fn smask_multiplies_with_ca() {
    // mask 128 × ca 0.5 ≈ alpha 0.25 → red over white = (255,191,191).
    let pdf = page(
        b"/H gs q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "/H << /ca 0.5 >>",
        vec![
            (5, rgb_image(1, 1, &[255, 0, 0], "/SMask 6 0 R")),
            (6, gray_mask(1, 1, &[128], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert!(
        near(px(&r, 30, 70), (255, 191, 191, 255), 3),
        "{:?}",
        px(&r, 30, 70)
    );
}

#[test]
fn smask_keeps_blend_mode() {
    // Grey backdrop, red image with Multiply: opaque column → red ×
    // grey = (128,0,0); transparent column → backdrop untouched.
    let pdf = page(
        b"0.5 g 0 0 100 100 re f /M gs q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "/M << /BM /Multiply >>",
        vec![
            (5, rgb_image(2, 1, &[255, 0, 0, 255, 0, 0], "/SMask 6 0 R")),
            (6, gray_mask(2, 1, &[255, 0], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert!(
        near(px(&r, 20, 70), (128, 0, 0, 255), 2),
        "{:?}",
        px(&r, 20, 70)
    );
    assert!(
        near(px(&r, 40, 70), (128, 128, 128, 255), 1),
        "{:?}",
        px(&r, 40, 70)
    );
}

#[test]
fn smask_follows_ctm_mirror() {
    // red|green under an opaque mask, drawn mirrored in x: green left.
    let pdf = page(
        b"q -40 0 0 40 50 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (5, rgb_image(2, 1, &[255, 0, 0, 0, 255, 0], "/SMask 6 0 R")),
            (6, gray_mask(2, 1, &[255, 255], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 255, 0, 255));
    assert_eq!(px(&r, 40, 70), RED);
}

#[test]
fn smask_respects_clip() {
    let pdf = page(
        b"0 0 30 100 re W n q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (5, rgb_image(1, 1, &[255, 0, 0], "/SMask 6 0 R")),
            (6, gray_mask(1, 1, &[255], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), RED);
    assert_eq!(px(&r, 40, 70), WHITE);
}

#[test]
fn smask_inside_form_xobject() {
    // Form translates by (10,10) and draws the image 40×40 at origin.
    let form = b"q 40 0 0 40 0 0 cm /Im Do Q";
    let pdf = page(
        b"/Fm Do",
        &[("Fm", 7)],
        "",
        vec![
            (5, rgb_image(1, 1, &[255, 0, 0], "/SMask 6 0 R")),
            (6, gray_mask(1, 1, &[128], "")),
            (
                7,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 10 10] \
                     /Resources << /XObject << /Im 5 0 R >> >>",
                    form,
                ),
            ),
        ],
    );
    let r = render(&pdf).unwrap();
    assert!(
        near(px(&r, 30, 70), (255, 127, 127, 255), 3),
        "{:?}",
        px(&r, 30, 70)
    );
    assert_eq!(px(&r, 5, 95), WHITE);
}

#[test]
fn smask_dimension_mismatch_is_resampled() {
    // 4×1 image, 2×1 mask → each mask sample covers two columns.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (
                5,
                rgb_image(
                    4,
                    1,
                    &[255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0],
                    "/SMask 6 0 R",
                ),
            ),
            (6, gray_mask(2, 1, &[255, 0], "")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 15, 70), RED);
    assert_eq!(px(&r, 25, 70), RED);
    assert_eq!(px(&r, 35, 70), WHITE);
    assert_eq!(px(&r, 45, 70), WHITE);
}

#[test]
fn smask_decode_inverts_alpha() {
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (5, rgb_image(1, 1, &[255, 0, 0], "/SMask 6 0 R")),
            (6, gray_mask(1, 1, &[255], "/Decode [1 0]")),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 30, 70), WHITE);
}

// --------------------------------------------------------------------
// Invalid / hostile soft masks — typed errors, never a panic or hang
// --------------------------------------------------------------------

fn smask_case(mask_obj: Vec<u8>) -> Vec<u8> {
    page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (5, rgb_image(1, 1, &[255, 0, 0], "/SMask 6 0 R")),
            (6, mask_obj),
        ],
    )
}

#[test]
fn smask_not_a_stream_is_structure_error() {
    let err = render(&smask_case(
        b"<< /Type /XObject /Subtype /Image >>".to_vec(),
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn smask_wrong_colour_space_is_structure_error() {
    let err = render(&smask_case(rgb_image(1, 1, &[1, 2, 3], ""))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn smask_bad_dimensions_are_refused() {
    let err = render(&smask_case(gray_mask(0, 1, &[], ""))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let err = render(&smask_case(stream_obj(
        "/Type /XObject /Subtype /Image /Width 3 /Height 2 /BitsPerComponent 8 /ColorSpace /DeviceGray",
        &[0, 0, 0],
    )))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "short data: {err:?}");
}

#[test]
fn smask_bad_decode_is_structure_error() {
    let err = render(&smask_case(gray_mask(1, 1, &[255], "/Decode [0 1 0]"))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn smask_self_reference_does_not_recurse() {
    // Image whose /SMask is itself: the mask decoder sees a 3-component
    // image and refuses — one level, no loop.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![(5, rgb_image(1, 1, &[255, 0, 0], "/SMask 5 0 R"))],
    );
    let err = render(&pdf).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn smask_missing_reference_is_typed() {
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![(5, rgb_image(1, 1, &[255, 0, 0], "/SMask 99 0 R"))],
    );
    let err = render(&pdf).unwrap_err();
    assert!(matches!(err, Error::MissingReference(_)), "{err:?}");
}

#[test]
fn smask_bad_flate_stream_is_parse_error() {
    let err = render(&smask_case(gray_mask(
        1,
        1,
        b"\xff\xfe\xfd",
        "/Filter /FlateDecode",
    )))
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

#[test]
fn smask_matte_is_unsupported() {
    let err = render(&smask_case(gray_mask(1, 1, &[255], "/Matte [0 0 0]"))).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("Matte")),
        "{err:?}"
    );
}

#[test]
fn smask_oversize_hits_limit() {
    let pdf = smask_case(gray_mask(5000, 5000, &[0], ""));
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions {
        limits: Limits {
            max_image_dimension: 1024,
            ..Limits::default()
        },
        ..RenderOptions::default()
    };
    let err = render_page(&doc, &doc.pages()[0], &opts).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

// --------------------------------------------------------------------
// Stencil masks, colour-key masks, other bit depths
// --------------------------------------------------------------------

#[test]
fn image_mask_paints_fill_colour_where_sample_is_zero() {
    // 8×1 bits 0b01010101: bit 0 (leftmost) is 0 → painted blue.
    let pdf = page(
        b"0 0 1 rg q 80 0 0 10 10 10 cm /ImM Do Q",
        &[("ImM", 5)],
        "",
        vec![(
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 8 /Height 1 /BitsPerComponent 1 /ImageMask true",
                &[0b0101_0101],
            ),
        )],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 15, 85), (0, 0, 255, 255));
    assert_eq!(px(&r, 25, 85), WHITE);
    assert_eq!(px(&r, 35, 85), (0, 0, 255, 255));
}

#[test]
fn image_mask_decode_one_zero_flips() {
    let pdf = page(
        b"0 0 1 rg q 80 0 0 10 10 10 cm /ImM Do Q",
        &[("ImM", 5)],
        "",
        vec![(
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 8 /Height 1 /BitsPerComponent 1 \
                 /ImageMask true /Decode [1 0]",
                &[0b0101_0101],
            ),
        )],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 15, 85), WHITE);
    assert_eq!(px(&r, 25, 85), (0, 0, 255, 255));
}

#[test]
fn stencil_mask_stream_hides_masked_samples() {
    // /Mask stream: 1 = masked out (§8.9.6.4). Right column hidden.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![
            (5, rgb_image(2, 1, &[255, 0, 0, 255, 0, 0], "/Mask 6 0 R")),
            (
                6,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 1 /ImageMask true",
                    &[0b0100_0000],
                ),
            ),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), RED);
    assert_eq!(px(&r, 40, 70), WHITE);
}

#[test]
fn colour_key_mask_hides_matching_samples() {
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![(5, gray_mask(2, 1, &[0, 255], "/Mask [250 255]"))],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 0, 0, 255));
    assert_eq!(px(&r, 40, 70), WHITE);
}

#[test]
fn four_and_sixteen_bit_gray_images() {
    // 4-bit: samples 0x0 and 0xF in one byte → black | white.
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q q 40 0 0 40 50 50 cm /Im16 Do Q",
        &[("Im", 5), ("Im16", 6)],
        "",
        vec![
            (
                5,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 4 /ColorSpace /DeviceGray",
                    &[0x0F],
                ),
            ),
            (
                6,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 16 /ColorSpace /DeviceGray",
                    &[0x80, 0x00],
                ),
            ),
        ],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 0, 0, 255));
    assert_eq!(px(&r, 40, 70), WHITE);
    assert!(
        near(px(&r, 70, 30), (128, 128, 128, 255), 1),
        "{:?}",
        px(&r, 70, 30)
    );
}

#[test]
fn cmyk_image_renders() {
    let pdf = page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        &[("Im", 5)],
        "",
        vec![(
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceCMYK",
                &[0, 0, 0, 255, 255, 0, 0, 0],
            ),
        )],
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 20, 70), (0, 0, 0, 255));
    assert_eq!(px(&r, 40, 70), (0, 255, 255, 255));
}
