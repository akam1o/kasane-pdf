//! Shadings (ISO 32000-1 §8.7.4): axial (type 2) and radial (type 3)
//! through `sh` and through `/PatternType 2` fills, with `/Extend`,
//! `/Domain`, `/BBox`, `/Background`, CTM, clipping, alpha and blend
//! modes. Unsupported types and malformed dictionaries are typed
//! errors. 100×100 pt page at 72 dpi: pixmap (x, y) = user (x, 100 − y).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

fn page(
    content: &[u8],
    shadings: &str,
    patterns: &str,
    extgstate: &str,
    objs: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /Shading << {shadings} >> /Pattern << {patterns} >> \
                 /ExtGState << {extgstate} >> /XObject << /Fm 9 0 R >> >> \
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

const WHITE: (u8, u8, u8) = (255, 255, 255);

/// Red → blue along x from 20 to 80, type 2 exponential function.
fn axial(extend: &str, extra: &str) -> String {
    format!(
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 80 0] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> \
         /Extend [{extend}] {extra} >>"
    )
}

#[test]
fn axial_sh_interpolates_and_stops_without_extend() {
    let r = render(&page(b"/Sh sh", &axial("false false", ""), "", "", vec![])).unwrap();
    assert!(near(px(&r, 20, 50), (255, 0, 0), 6), "{:?}", px(&r, 20, 50));
    assert!(
        near(px(&r, 50, 50), (128, 0, 128), 6),
        "{:?}",
        px(&r, 50, 50)
    );
    assert!(near(px(&r, 79, 50), (0, 0, 255), 8), "{:?}", px(&r, 79, 50));
    // Outside the axis without /Extend: untouched.
    assert_eq!(px(&r, 10, 50), WHITE);
    assert_eq!(px(&r, 90, 50), WHITE);
}

#[test]
fn axial_extend_fills_beyond_the_axis() {
    let r = render(&page(b"/Sh sh", &axial("true false", ""), "", "", vec![])).unwrap();
    assert_eq!(px(&r, 5, 50), (255, 0, 0));
    assert_eq!(px(&r, 95, 50), WHITE);
    let r = render(&page(b"/Sh sh", &axial("false true", ""), "", "", vec![])).unwrap();
    assert_eq!(px(&r, 5, 50), WHITE);
    assert_eq!(px(&r, 95, 50), (0, 0, 255));
}

#[test]
fn sh_is_limited_by_clip_and_bbox() {
    // Clip to the lower half; /BBox to x ∈ [30, 60].
    let r = render(&page(
        b"0 0 100 50 re W n /Sh sh",
        &axial("true true", "/BBox [30 0 60 100]"),
        "",
        "",
        vec![],
    ))
    .unwrap();
    assert_ne!(px(&r, 45, 75), WHITE, "inside clip and bbox");
    assert_eq!(px(&r, 45, 25), WHITE, "outside clip");
    assert_eq!(px(&r, 20, 75), WHITE, "outside bbox");
    assert_eq!(px(&r, 70, 75), WHITE, "outside bbox");
}

#[test]
fn sh_follows_the_ctm() {
    // Scale x by 0.5: the axis now runs 10..40 on the page.
    let r = render(&page(
        b"q 0.5 0 0 1 0 0 cm /Sh sh Q",
        &axial("false false", ""),
        "",
        "",
        vec![],
    ))
    .unwrap();
    assert!(
        near(px(&r, 11, 50), (255, 0, 0), 16),
        "{:?}",
        px(&r, 11, 50)
    );
    assert!(
        near(px(&r, 39, 50), (0, 0, 255), 16),
        "{:?}",
        px(&r, 39, 50)
    );
    assert_eq!(px(&r, 60, 50), WHITE);
}

#[test]
fn sh_honours_alpha_and_blend_mode() {
    let r = render(&page(
        b"/H gs /Sh sh",
        &axial("true true", ""),
        "",
        "/H << /ca 0.5 >>",
        vec![],
    ))
    .unwrap();
    assert!(
        near(px(&r, 5, 50), (255, 128, 128), 3),
        "{:?}",
        px(&r, 5, 50)
    );
    // Multiply over mid-grey: red × grey = (128, 0, 0).
    let r = render(&page(
        b"0.5 g 0 0 100 100 re f /M gs /Sh sh",
        &axial("true true", ""),
        "",
        "/M << /BM /Multiply >>",
        vec![],
    ))
    .unwrap();
    assert!(near(px(&r, 5, 50), (128, 0, 0), 3), "{:?}", px(&r, 5, 50));
}

#[test]
fn radial_sh_concentric_with_extend() {
    let sh = "/Sh << /ShadingType 3 /ColorSpace /DeviceGray /Coords [50 50 0 50 50 40] \
              /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>";
    let r = render(&page(b"/Sh sh", sh, "", "", vec![])).unwrap();
    assert!(near(px(&r, 50, 50), (0, 0, 0), 12), "{:?}", px(&r, 50, 50));
    assert!(
        near(px(&r, 70, 50), (128, 128, 128), 12),
        "{:?}",
        px(&r, 70, 50)
    );
    assert_eq!(px(&r, 2, 2), WHITE, "outside the outer circle");
    let sh = sh.replace(">> >>", ">> /Extend [false true] >>");
    let r = render(&page(b"/Sh sh", &sh, "", "", vec![])).unwrap();
    assert_eq!(
        px(&r, 2, 2),
        WHITE,
        "extend of circle 1 paints white (gray 1)"
    );
    assert!(near(px(&r, 70, 50), (128, 128, 128), 12));
}

#[test]
fn radial_domain_and_stitching_function() {
    // Domain [0 2], stitched red→green→blue; the mid radius is green.
    let sh = "/Sh << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 50 0 50 50 40] \
              /Domain [0 2] /Function << /FunctionType 3 /Domain [0 2] /Bounds [1] /Encode [0 1 0 1] \
              /Functions [ << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 1 0] /N 1 >> \
                           << /FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [0 0 1] /N 1 >> ] >> >>";
    let r = render(&page(b"/Sh sh", sh, "", "", vec![])).unwrap();
    assert!(
        near(px(&r, 70, 50), (0, 255, 0), 16),
        "{:?}",
        px(&r, 70, 50)
    );
    assert!(
        near(px(&r, 50, 50), (255, 0, 0), 20),
        "{:?}",
        px(&r, 50, 50)
    );
}

#[test]
fn shading_pattern_fills_a_path_with_matrix_and_background() {
    // Pattern space = page space; /Matrix shifts the axis right by 20.
    let pat = "/P << /PatternType 2 /Matrix [1 0 0 1 20 0] /Shading << /ShadingType 2 \
               /ColorSpace /DeviceRGB /Coords [0 0 40 0] /Background [0 1 0] \
               /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >>";
    let r = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        "",
        pat,
        "",
        vec![],
    ))
    .unwrap();
    assert!(
        near(px(&r, 21, 50), (255, 0, 0), 12),
        "{:?}",
        px(&r, 21, 50)
    );
    assert!(
        near(px(&r, 59, 50), (0, 0, 255), 12),
        "{:?}",
        px(&r, 59, 50)
    );
    // /Background covers the path where the axis does not reach.
    assert_eq!(px(&r, 5, 50), (0, 255, 0));
    assert_eq!(px(&r, 90, 50), (0, 255, 0));
}

#[test]
fn shading_pattern_is_clipped_to_the_path() {
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceGray \
               /Coords [0 0 100 0] /Extend [true true] \
               /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> >> >>";
    let r = render(&page(
        b"/Pattern cs /P scn 10 10 30 30 re f",
        "",
        pat,
        "",
        vec![],
    ))
    .unwrap();
    assert_eq!(px(&r, 20, 80), (0, 0, 0));
    assert_eq!(px(&r, 60, 80), WHITE);
    assert_eq!(px(&r, 20, 20), WHITE);
}

#[test]
fn pattern_inside_a_form_uses_the_form_space() {
    // Form matrix translates by 50: the pattern's axis (0..40 in form
    // space) lands at page x 50..90.
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB \
               /Coords [0 0 40 0] /Function << /FunctionType 2 /Domain [0 1] \
               /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >>";
    let form = stream_obj(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 50 0] \
             /Resources << /Pattern << {pat} >> >>"
        ),
        b"/Pattern cs /P scn 0 0 40 100 re f",
    );
    let r = render(&page(b"/Fm Do", "", "", "", vec![(9, form)])).unwrap();
    assert!(
        near(px(&r, 51, 50), (255, 0, 0), 12),
        "{:?}",
        px(&r, 51, 50)
    );
    assert!(
        near(px(&r, 89, 50), (0, 0, 255), 12),
        "{:?}",
        px(&r, 89, 50)
    );
    assert_eq!(px(&r, 30, 50), WHITE);
}

#[test]
fn stencil_mask_paints_the_fill_pattern() {
    // §8.9.6.2: an image mask paints the current fill colour; in
    // /Pattern space that is the shading. A 2×1 stencil (left sample
    // paints, right sample does not) over a red→blue axial pattern.
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceRGB \
               /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] \
               /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >> >>";
    let im = stream_obj(
        "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ImageMask true",
        b"\x40",
    );
    let make = |content: &[u8]| {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                     /Resources << /Pattern << {pat} >> /XObject << /Im 10 0 R >> >> \
                     /Contents 4 0 R >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", content)),
            (10, im.clone()),
        ])
    };
    let r = render(&make(b"/Pattern cs /P scn q 100 0 0 100 0 0 cm /Im Do Q")).unwrap();
    let l = px(&r, 25, 50);
    assert!(l.0 > 150 && l.2 < 110, "left half shows the pattern: {l:?}");
    assert_eq!(px(&r, 75, 50), WHITE, "unpainted sample stays white");
    // The pattern is sampled in pattern space, not image space: x = 45
    // is further along the axis than x = 5.
    let a = px(&r, 5, 50);
    let b = px(&r, 45, 50);
    assert!(
        b.2 > a.2 && b.0 < a.0,
        "gradient runs across the stencil: {a:?} {b:?}"
    );
    // /Pattern space with no pattern selected paints nothing.
    let r = render(&make(b"/Pattern cs q 100 0 0 100 0 0 cm /Im Do Q")).unwrap();
    assert_eq!(px(&r, 25, 50), WHITE);
}

#[test]
fn unsupported_shading_types_and_patterns_are_classified() {
    for t in [1, 4, 5, 6, 7] {
        let sh = format!(
            "/Sh << /ShadingType {t} /ColorSpace /DeviceRGB /Function << /FunctionType 2 \
             /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>"
        );
        let err = render(&page(b"/Sh sh", &sh, "", "", vec![])).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported { feature, .. } if feature.contains("shading")),
            "type {t}: {err:?}"
        );
    }
    let pat = "/P << /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] \
               /XStep 10 /YStep 10 /Resources << >> /Length 0 >>";
    let err = render(&page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        "",
        pat,
        "",
        vec![],
    ))
    .unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("tiling")),
        "{err:?}"
    );
    // Stroking and text in a pattern space are refused, not black.
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceGray \
               /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >> >>";
    let err = render(&page(
        b"/Pattern CS /P SCN 10 10 m 90 90 l S",
        "",
        pat,
        "",
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn malformed_shadings_are_structure_errors() {
    let cases = [
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>",
        "/Sh << /ShadingType 3 /ColorSpace /DeviceRGB /Coords [0 0 -1 0 0 5] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>",
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] >>",
        "/Sh << /ShadingType 2 /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /N 1 >> >>",
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Domain [1 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>",
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Extend [true] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>",
        "/Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>",
        "/Sh << /ShadingType 2 /ColorSpace [/Indexed /DeviceRGB 0 <000000>] /Coords [0 0 1 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> >>",
        "/Sh << /ShadingType 9 /ColorSpace /DeviceRGB >>",
        "/Sh 42",
    ];
    for c in cases {
        let err = render(&page(b"/Sh sh", c, "", "", vec![])).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{c}: {err:?}");
    }
}

#[test]
fn degenerate_axis_paints_nothing() {
    let sh =
        "/Sh << /ShadingType 2 /ColorSpace /DeviceGray /Coords [50 50 50 50] /Extend [true true] \
              /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [0] /N 1 >> >>";
    let r = render(&page(b"/Sh sh", sh, "", "", vec![])).unwrap();
    assert_eq!(px(&r, 50, 50), WHITE);
}
