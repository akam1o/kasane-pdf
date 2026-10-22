//! Non-device colour spaces (ISO 32000-1 §8.6): CalGray, CalRGB, Lab,
//! ICCBased (exact matrix/TRC profiles vs. recorded approximations),
//! Indexed, Separation, DeviceN, `/Default*` remapping, and malformed
//! definitions. Fill colour is asserted at one pixel of a 100×100 page.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

/// Page with `content`, a `/ColorSpace` resource dictionary body and
/// extra objects (numbered from 5).
fn page(content: &[u8], colorspaces: &str, objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /ColorSpace << {colorspaces} >> /XObject << /Im 9 0 R >> >> \
                 /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    all.extend(objs);
    build_pdf(&all)
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).expect("in bounds");
    (p.red(), p.green(), p.blue())
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol
        && (a.1 as i32 - b.1 as i32).abs() <= tol
        && (a.2 as i32 - b.2 as i32).abs() <= tol
}

/// Fill the whole page with `/CS cs <comps> sc` and return the centre.
fn fill_with(
    colorspaces: &str,
    set: &str,
    objs: Vec<(u32, Vec<u8>)>,
) -> Result<(u8, u8, u8), Error> {
    let content = format!("/CS cs {set} 0 0 100 100 re f");
    let r = render(&page(content.as_bytes(), colorspaces, objs))?;
    Ok(px(&r, 50, 50))
}

/// A minimal ICC v2 RGB matrix/TRC profile: sRGB-like colorants
/// (D50-adapted), gamma 2.2 curves. `space` overrides the header's
/// data colour space (e.g. `b"CMYK"` to make it unsupported).
fn icc_profile(space: &[u8; 4]) -> Vec<u8> {
    fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for v in [x, y, z] {
            t.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
        }
        t
    }
    let mut curv = b"curv\0\0\0\0".to_vec();
    curv.extend_from_slice(&1u32.to_be_bytes());
    curv.extend_from_slice(&((2.2f64 * 256.0) as u16).to_be_bytes());
    curv.extend_from_slice(&[0, 0]);
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
        (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
        (b"bXYZ", xyz(0.1431, 0.0606, 0.7139)),
        (b"rTRC", curv.clone()),
        (b"gTRC", curv.clone()),
        (b"bTRC", curv),
    ];
    let mut data = vec![0u8; 128];
    data[16..20].copy_from_slice(space);
    data[20..24].copy_from_slice(b"XYZ ");
    data[36..40].copy_from_slice(b"acsp");
    data.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    let mut off = 132 + tags.len() * 12;
    let mut bodies = Vec::new();
    for (sig, body) in &tags {
        data.extend_from_slice(*sig);
        data.extend_from_slice(&(off as u32).to_be_bytes());
        data.extend_from_slice(&(body.len() as u32).to_be_bytes());
        off += body.len();
        bodies.extend_from_slice(body);
    }
    data.extend_from_slice(&bodies);
    let len = data.len() as u32;
    data[0..4].copy_from_slice(&len.to_be_bytes());
    data
}

const D65: &str = "/WhitePoint [0.9505 1 1.089]";
const SRGB_MATRIX: &str =
    "/Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505]";

// --------------------------------------------------------------------
// CIE-based
// --------------------------------------------------------------------

#[test]
fn calgray_gamma_one_is_linear_light() {
    let cs = format!("/CS [/CalGray << {D65} /Gamma 1 >>]");
    // Linear 0.5 → sRGB-encoded ≈ 0.735 → 187, not 128.
    let c = fill_with(&cs, "0.5 sc", vec![]).unwrap();
    assert!(near(c, (187, 187, 187), 3), "{c:?}");
    assert!(near(
        fill_with(&cs, "1 sc", vec![]).unwrap(),
        (255, 255, 255),
        1
    ));
    assert_eq!(fill_with(&cs, "0 sc", vec![]).unwrap(), (0, 0, 0));
}

#[test]
fn calrgb_srgb_primaries_round_trip() {
    let cs = format!("/CS [/CalRGB << {D65} /Gamma [2.2 2.2 2.2] {SRGB_MATRIX} >>]");
    assert!(near(
        fill_with(&cs, "1 0 0 sc", vec![]).unwrap(),
        (255, 0, 0),
        12
    ));
    assert!(near(
        fill_with(&cs, "0 0 1 sc", vec![]).unwrap(),
        (0, 0, 255),
        12
    ));
    assert!(near(
        fill_with(&cs, "0.5 0.5 0.5 sc", vec![]).unwrap(),
        (128, 128, 128),
        8
    ));
    assert!(near(
        fill_with(&cs, "1 1 1 sc", vec![]).unwrap(),
        (255, 255, 255),
        3
    ));
}

#[test]
fn calrgb_initial_colour_is_black() {
    let cs = format!("/CS [/CalRGB << {D65} >>]");
    let r = render(&page(b"/CS cs 0 0 100 100 re f", &cs, vec![])).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 0));
}

#[test]
fn lab_white_black_and_hue() {
    let cs = "/CS [/Lab << /WhitePoint [0.9643 1 0.8251] /Range [-100 100 -100 100] >>]";
    assert!(near(
        fill_with(cs, "100 0 0 sc", vec![]).unwrap(),
        (255, 255, 255),
        3
    ));
    assert!(near(
        fill_with(cs, "0 0 0 sc", vec![]).unwrap(),
        (0, 0, 0),
        3
    ));
    let (r, g, b) = fill_with(cs, "50 70 50 sc", vec![]).unwrap();
    assert!(r > g + 60 && r > b + 60, "{:?}", (r, g, b));
}

// --------------------------------------------------------------------
// ICCBased
// --------------------------------------------------------------------

#[test]
fn iccbased_matrix_trc_profile_is_evaluated_exactly() {
    let objs = vec![(
        5,
        stream_obj("/N 3 /Alternate /DeviceRGB", &icc_profile(b"RGB ")),
    )];
    let pdf = page(
        b"/CS cs 1 0 0 sc 0 0 100 100 re f",
        "/CS [/ICCBased 5 0 R]",
        objs,
    );
    let r = render(&pdf).unwrap();
    assert!(
        near(px(&r, 50, 50), (255, 0, 0), 15),
        "{:?}",
        px(&r, 50, 50)
    );
    assert!(
        r.color_approximations.is_empty(),
        "{:?}",
        r.color_approximations
    );
}

#[test]
fn iccbased_unsupported_profile_uses_alternate_and_records_it() {
    // CMYK header → LUT-class profile we do not evaluate: the colour
    // comes from /Alternate DeviceCMYK and the page says so.
    let objs = vec![(
        5,
        stream_obj("/N 4 /Alternate /DeviceCMYK", &icc_profile(b"CMYK")),
    )];
    let pdf = page(
        b"/CS cs 0 0 0 1 sc 0 0 100 100 re f",
        "/CS [/ICCBased 5 0 R]",
        objs,
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 0));
    assert_eq!(r.color_approximations.len(), 1);
    assert!(
        r.color_approximations[0].contains("CMYK"),
        "{:?}",
        r.color_approximations
    );
}

#[test]
fn iccbased_garbage_stream_falls_back_with_note() {
    let objs = vec![(5, stream_obj("/N 3", b"definitely not an ICC profile"))];
    let pdf = page(
        b"/CS cs 0 1 0 sc 0 0 100 100 re f",
        "/CS [/ICCBased 5 0 R]",
        objs,
    );
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 255, 0));
    assert_eq!(r.color_approximations.len(), 1);
}

#[test]
fn iccbased_n_mismatch_is_structure_error() {
    let objs = vec![(
        5,
        stream_obj("/N 3 /Alternate /DeviceCMYK", &icc_profile(b"RGB ")),
    )];
    let err = fill_with("/CS [/ICCBased 5 0 R]", "1 0 0 sc", objs).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let objs = vec![(5, stream_obj("/N 5", &icc_profile(b"RGB ")))];
    let err = fill_with("/CS [/ICCBased 5 0 R]", "1 0 0 sc", objs).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let err = fill_with("/CS [/ICCBased << /N 3 >>]", "1 0 0 sc", vec![]).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

// --------------------------------------------------------------------
// Indexed / Separation / DeviceN
// --------------------------------------------------------------------

#[test]
fn indexed_lookup_selects_base_colour() {
    let cs = "/CS [/Indexed /DeviceRGB 1 <FF000000FF00>]";
    assert_eq!(fill_with(cs, "0 sc", vec![]).unwrap(), (255, 0, 0));
    assert_eq!(fill_with(cs, "1 sc", vec![]).unwrap(), (0, 255, 0));
    // Out-of-range index clips to hival (§8.6.6.3).
    assert_eq!(fill_with(cs, "9 sc", vec![]).unwrap(), (0, 255, 0));
    // Lookup table may be a stream; short tables pad with zeros.
    let objs = vec![(5, stream_obj("", &[0, 0, 255]))];
    assert_eq!(
        fill_with("/CS [/Indexed /DeviceRGB 1 5 0 R]", "0 sc", objs).unwrap(),
        (0, 0, 255)
    );
    let objs = vec![(5, stream_obj("", &[0, 0, 255]))];
    assert_eq!(
        fill_with("/CS [/Indexed /DeviceRGB 1 5 0 R]", "1 sc", objs).unwrap(),
        (0, 0, 0)
    );
}

#[test]
fn indexed_one_bit_image() {
    // 2×1 image, bits 0b01 → index 0 (red), index 1 (green).
    let objs = vec![(
        9,
        stream_obj(
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 1 \
             /ColorSpace [/Indexed /DeviceRGB 1 <FF000000FF00>]",
            &[0b0100_0000],
        ),
    )];
    let r = render(&page(b"q 40 0 0 40 10 10 cm /Im Do Q", "", objs)).unwrap();
    assert_eq!(px(&r, 20, 70), (255, 0, 0));
    assert_eq!(px(&r, 40, 70), (0, 255, 0));
}

#[test]
fn separation_tint_transform_and_initial_colour() {
    // Tint 1 → red, 0 → white (type 2 function).
    let cs = "/CS [/Separation /Spot /DeviceRGB \
              << /FunctionType 2 /Domain [0 1] /C0 [1 1 1] /C1 [1 0 0] /N 1 >>]";
    assert_eq!(fill_with(cs, "1 sc", vec![]).unwrap(), (255, 0, 0));
    assert_eq!(fill_with(cs, "0 sc", vec![]).unwrap(), (255, 255, 255));
    assert!(near(
        fill_with(cs, "0.5 sc", vec![]).unwrap(),
        (255, 128, 128),
        2
    ));
    // §8.6.6.4: the initial colour of a Separation is tint 1.
    let r = render(&page(b"/CS cs 0 0 100 100 re f", cs, vec![])).unwrap();
    assert_eq!(px(&r, 50, 50), (255, 0, 0));
}

#[test]
fn separation_none_paints_nothing() {
    let cs = "/CS [/Separation /None /DeviceGray << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >>]";
    assert_eq!(fill_with(cs, "1 sc", vec![]).unwrap(), (255, 255, 255));
}

#[test]
fn devicen_postscript_tint_transform() {
    // Two colorants → RGB via a type 4 function `{ 0 }` (a b → a b 0).
    let objs = vec![(
        5,
        stream_obj(
            "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
            b"{ 0 }",
        ),
    )];
    let cs = "/CS [/DeviceN [/A /B] /DeviceRGB 5 0 R]";
    assert_eq!(fill_with(cs, "1 0 sc", objs).unwrap(), (255, 0, 0));
    let objs = vec![(
        5,
        stream_obj(
            "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
            b"{ 0 }",
        ),
    )];
    assert_eq!(fill_with(cs, "0 1 sc", objs).unwrap(), (0, 255, 0));
}

#[test]
fn separation_sampled_function() {
    // Type 0: 2 samples, 3 outputs, 8-bit: white → blue.
    let objs = vec![(
        5,
        stream_obj(
            "/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2] /BitsPerSample 8",
            &[255, 255, 255, 0, 0, 255],
        ),
    )];
    let cs = "/CS [/Separation /Blue /DeviceRGB 5 0 R]";
    assert!(near(
        fill_with(cs, "0.5 sc", objs).unwrap(),
        (128, 128, 255),
        2
    ));
}

// --------------------------------------------------------------------
// /Default* remapping and images through non-device spaces
// --------------------------------------------------------------------

#[test]
fn default_gray_remaps_the_g_operator() {
    let cs = format!("/DefaultGray [/CalGray << {D65} /Gamma 1 >>]");
    let r = render(&page(b"0.5 g 0 0 100 100 re f", &cs, vec![])).unwrap();
    assert!(
        near(px(&r, 50, 50), (187, 187, 187), 3),
        "{:?}",
        px(&r, 50, 50)
    );
    // A DefaultGray with the wrong component count is malformed.
    let err = render(&page(
        b"0.5 g 0 0 100 100 re f",
        "/DefaultGray /DeviceRGB",
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn calrgb_image_with_decode() {
    let objs = vec![(
        9,
        stream_obj(
            &format!(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
                 /ColorSpace [/CalRGB << {D65} /Gamma [2.2 2.2 2.2] {SRGB_MATRIX} >>] \
                 /Decode [1 0 1 0 1 0]"
            ),
            &[0, 255, 255],
        ),
    )];
    let r = render(&page(b"q 40 0 0 40 10 10 cm /Im Do Q", "", objs)).unwrap();
    assert!(
        near(px(&r, 30, 70), (255, 0, 0), 12),
        "{:?}",
        px(&r, 30, 70)
    );
}

#[test]
fn image_colour_space_may_be_a_named_resource() {
    let objs = vec![(
        9,
        stream_obj(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /CS",
            &[1],
        ),
    )];
    let r = render(&page(
        b"q 40 0 0 40 10 10 cm /Im Do Q",
        "/CS [/Indexed /DeviceRGB 1 <FF000000FF00>]",
        objs,
    ))
    .unwrap();
    assert_eq!(px(&r, 30, 70), (0, 255, 0));
}

// --------------------------------------------------------------------
// Malformed definitions
// --------------------------------------------------------------------

#[test]
fn malformed_colour_spaces_are_structure_errors() {
    let cases: &[&str] = &[
        "/CS [/CalRGB]",
        "/CS [/CalGray << /Gamma 1 >>]",
        "/CS [/CalRGB << /WhitePoint [1 2 3] >>]",
        "/CS [/Indexed /DeviceRGB 300 <00>]",
        "/CS [/Indexed [/Indexed /DeviceRGB 0 <000000>] 0 <00>]",
        "/CS [/Indexed /DeviceRGB 1 /NotALookup]",
        "/CS [/Separation /Spot /DeviceRGB << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>]",
        "/CS [/Separation /Spot /DeviceRGB 42]",
        "/CS [/DeviceN [] /DeviceRGB << /FunctionType 2 /Domain [0 1] /N 1 >>]",
        "/CS [/Lab << /WhitePoint [0.96 1 0.82] /Range [1 0 0 1] >>]",
        "/CS [42]",
        "/CS []",
        "/CS /CS",
    ];
    for c in cases {
        let err = fill_with(c, "0 sc", vec![]).unwrap_err();
        assert!(
            matches!(err, Error::Structure(_) | Error::LimitExceeded { .. }),
            "{c}: {err:?}"
        );
    }
}

#[test]
fn wrong_component_count_is_parse_error() {
    let err = fill_with(
        "/CS [/CalRGB << /WhitePoint [0.95 1 1.09] >>]",
        "0.5 sc",
        vec![],
    )
    .unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

#[test]
fn unknown_family_and_pattern_are_unsupported() {
    let err = fill_with("/CS [/Pattern /DeviceRGB]", "0 sc", vec![]).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    let err = fill_with("/CS [/Frobnicate 1 2]", "0 sc", vec![]).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn hostile_postscript_function_is_bounded() {
    // Stack overflow through `dup` is a limit error, not a crash.
    let prog = format!("{{ {} }}", "dup ".repeat(300));
    let objs = vec![(
        5,
        stream_obj(
            "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
            prog.as_bytes(),
        ),
    )];
    let err = fill_with("/CS [/Separation /S /DeviceRGB 5 0 R]", "0.5 sc", objs).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}
