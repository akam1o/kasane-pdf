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
    let p = r.pixmap().pixel(x, y).expect("in bounds");
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
fn malformed_shading_types_and_patterns_are_classified() {
    // A one-input function on a function-based shading, and mesh types
    // given as dictionaries rather than streams, are structure errors.
    for t in [1, 4, 5, 6, 7] {
        let sh = format!(
            "/Sh << /ShadingType {t} /ColorSpace /DeviceRGB /Function << /FunctionType 2 \
             /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>"
        );
        let err = render(&page(b"/Sh sh", &sh, "", "", vec![])).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "type {t}: {err:?}");
    }
    let err = render(&page(
        b"/Sh sh",
        "/Sh << /ShadingType 8 /ColorSpace /DeviceRGB >>",
        "",
        "",
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // A tiling pattern that is a dictionary rather than a stream is
    // malformed (tiling itself is rendered — see
    // render_tiling_patterns.rs).
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
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // Stroking with a shading pattern paints the stroke outline with
    // the shading: grey along the diagonal, nothing elsewhere.
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceGray \
               /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> \
               /Extend [true true] >> >>";
    let r = render(&page(
        b"/Pattern CS /P SCN 10 w 10 10 m 90 90 l S",
        "",
        pat,
        "",
        vec![],
    ))
    .unwrap();
    let mid = px(&r, 50, 50);
    assert!(near(mid, (128, 128, 128), 10), "{mid:?}");
    assert_eq!(px(&r, 80, 80), WHITE, "off the stroke");
    assert_eq!(px(&r, 20, 20), WHITE, "off the stroke");
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

// ------------------------------------------------------------------
// function-based (type 1) and mesh (types 4–7) shadings
// ------------------------------------------------------------------

/// Stream shading object `10 0` with the given dictionary entries and
/// binary data; `/Sh` names it.
fn mesh_page(content: &[u8], dict: &str, data: &[u8], patterns: &str) -> Vec<u8> {
    page(
        content,
        "/Sh 10 0 R",
        patterns,
        "",
        vec![(10, stream_obj(dict, data))],
    )
}

/// 8-bit vertex: flag, x, y, then colour bytes.
fn v8(flag: u8, x: u8, y: u8, c: &[u8]) -> Vec<u8> {
    let mut v = vec![flag, x, y];
    v.extend_from_slice(c);
    v
}

#[test]
fn function_based_shading_samples_the_2d_function() {
    // gray = x over the unit domain, /Matrix maps it onto the page.
    let func = stream_obj("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1]", b"{ pop }");
    let sh = "/Sh << /ShadingType 1 /ColorSpace /DeviceGray /Domain [0 1 0 1] \
              /Matrix [100 0 0 100 0 0] /Function 11 0 R >>";
    let r = render(&page(b"/Sh sh", sh, "", "", vec![(11, func)])).unwrap();
    assert!(
        near(px(&r, 25, 50), (64, 64, 64), 4),
        "{:?}",
        px(&r, 25, 50)
    );
    assert!(
        near(px(&r, 75, 50), (191, 191, 191), 4),
        "{:?}",
        px(&r, 75, 50)
    );
    // Outside the domain nothing is painted; a pattern paints /Background there.
    let func = stream_obj("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1]", b"{ pop }");
    let sh = "/Sh << /ShadingType 1 /ColorSpace /DeviceGray /Matrix [50 0 0 50 25 25] \
              /Background [0.5] /Function 11 0 R >>";
    let pat = "/P << /PatternType 2 /Shading << /ShadingType 1 /ColorSpace /DeviceGray \
               /Matrix [50 0 0 50 25 25] /Background [0.5] /Function 11 0 R >> >>";
    let r = render(&page(
        b"/Sh sh /Pattern cs /P scn 0 0 100 10 re f",
        sh,
        pat,
        "",
        vec![(11, func)],
    ))
    .unwrap();
    assert_eq!(
        px(&r, 10, 50),
        WHITE,
        "outside the domain, sh paints nothing"
    );
    assert!(near(px(&r, 50, 50), (128, 128, 128), 4));
    assert!(
        near(px(&r, 10, 95), (128, 128, 128), 2),
        "pattern background"
    );
    // A one-input function is a structure error.
    let func = stream_obj("/FunctionType 4 /Domain [0 1] /Range [0 1]", b"{ }");
    let sh = "/Sh << /ShadingType 1 /ColorSpace /DeviceGray /Function 11 0 R >>";
    let err = render(&page(b"/Sh sh", sh, "", "", vec![(11, func)])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn free_form_triangle_mesh_with_edge_flags() {
    let mut data = Vec::new();
    data.extend(v8(0, 0, 0, &[255, 0, 0]));
    data.extend(v8(0, 100, 0, &[0, 255, 0]));
    data.extend(v8(0, 0, 100, &[0, 0, 255]));
    // f = 2: (va, vc, new) = red, blue, white.
    data.extend(v8(2, 100, 100, &[255, 255, 255]));
    let dict = "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]";
    let r = render(&mesh_page(b"/Sh sh", dict, &data, "")).unwrap();
    let c = px(&r, 5, 95); // user (5, 5): red corner
    assert!(c.0 > 200 && c.1 < 40 && c.2 < 40, "{c:?}");
    let c = px(&r, 95, 95); // user (95, 5): green corner
    assert!(c.1 > 200 && c.0 < 40 && c.2 < 40, "{c:?}");
    let c = px(&r, 5, 5); // user (5, 95): blue corner
    assert!(c.2 > 200 && c.0 < 40 && c.1 < 40, "{c:?}");
    let c = px(&r, 97, 3); // user (97, 97): white corner of the second triangle
    assert!(c.0 > 200 && c.1 > 200 && c.2 > 200, "{c:?}");
    // Centre of the first triangle: an even mix.
    let c = px(&r, 25, 75); // user (25, 25)
    assert!(near(c, (128, 64, 64), 12), "{c:?}");
    // Flag 1 before any triangle is malformed; flag 7 is not a flag.
    let bad = v8(1, 0, 0, &[0, 0, 0]);
    let err = render(&mesh_page(b"/Sh sh", dict, &bad, "")).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn lattice_mesh_and_triangle_limit() {
    // Two rows of two vertices: black at the bottom, white at the top.
    let mut data = Vec::new();
    for (x, y, g) in [(0u8, 0u8, 0u8), (100, 0, 0), (0, 100, 255), (100, 100, 255)] {
        data.extend_from_slice(&[x, y, g]);
    }
    let dict = "/ShadingType 5 /ColorSpace /DeviceGray /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /VerticesPerRow 2 /Decode [0 255 0 255 0 1]";
    let pdf = mesh_page(b"/Sh sh", dict, &data, "");
    let r = render(&pdf).unwrap();
    assert!(
        near(px(&r, 50, 25), (191, 191, 191), 4),
        "{:?}",
        px(&r, 50, 25)
    );
    assert!(
        near(px(&r, 50, 75), (64, 64, 64), 4),
        "{:?}",
        px(&r, 50, 75)
    );
    // The mesh has two triangles; a budget of one refuses it.
    let limits = kasane_pdf::Limits::default().with_max_shading_triangles(1);
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions::default().with_limits(limits);
    let err = render_page(&doc, &doc.pages()[0], &opts).unwrap_err();
    assert!(
        matches!(
            err,
            Error::LimitExceeded {
                limit: "max_shading_triangles",
                ..
            }
        ),
        "{err:?}"
    );
}

/// Straight-edged 10..90 square patch: boundary points in stream
/// order p11 p12 p13 p14 p24 p34 p44 p43 p42 p41 p31 p21.
fn square_patch_points() -> Vec<u8> {
    vec![
        10, 10, 10, 37, 10, 63, 10, 90, 37, 90, 63, 90, 90, 90, 90, 63, 90, 37, 90, 10, 63, 10, 37,
        10,
    ]
}

/// c1 black at p11 (10,10), c2 red at p14 (10,90), c3 white at p44
/// (90,90), c4 blue at p41 (90,10).
const SQUARE_COLOURS: [u8; 12] = [0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0, 255];

fn check_square_patch(r: &RenderedPage) {
    let c = px(r, 12, 12); // user (12, 88): near p14 → red
    assert!(c.0 > 200 && c.1 < 50 && c.2 < 50, "{c:?}");
    let c = px(r, 88, 88); // user (88, 12): near p41 → blue
    assert!(c.2 > 200 && c.0 < 50 && c.1 < 50, "{c:?}");
    let c = px(r, 50, 50); // centre: bilinear mix
    assert!(near(c, (128, 64, 128), 12), "{c:?}");
    assert_eq!(px(r, 5, 5), WHITE, "outside the patch");
}

#[test]
fn coons_patch_mesh_with_shared_edge() {
    let mut data = vec![0u8];
    data.extend(square_patch_points());
    data.extend_from_slice(&SQUARE_COLOURS);
    let dict = "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]";
    let r = render(&mesh_page(b"/Sh sh", dict, &data, "")).unwrap();
    check_square_patch(&r);
    assert_eq!(px(&r, 95, 50), WHITE, "outside the patch");
    // f = 2: the previous p44 p43 p42 p41 edge becomes p11..p14 of a
    // strip to the right (x 90..100), new corners p44 (100,10) and p41
    // (100,90) both green.
    data.push(2);
    data.extend_from_slice(&[
        93, 10, 97, 10, 100, 10, 100, 37, 100, 63, 100, 90, 97, 90, 93, 90,
    ]);
    data.extend_from_slice(&[0, 255, 0, 0, 255, 0]);
    let r = render(&mesh_page(b"/Sh sh", dict, &data, "")).unwrap();
    check_square_patch(&r);
    let c = px(&r, 98, 50); // user (98, 50): u ≈ 0.8 towards the green corners
    assert!(c.1 > 180 && c.0 < 80 && c.2 < 80, "{c:?}");
    // Flag 3 before any patch, and an unknown flag, are malformed.
    for bad in [vec![3u8; 23], vec![5u8]] {
        let err = render(&mesh_page(b"/Sh sh", dict, &bad, "")).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{err:?}");
    }
}

#[test]
fn tensor_patch_mesh_and_parametric_mesh() {
    let mut data = vec![0u8];
    data.extend(square_patch_points());
    // Interior p22 p23 p33 p32 at the bilinear positions.
    data.extend_from_slice(&[37, 37, 37, 63, 63, 63, 63, 37]);
    data.extend_from_slice(&SQUARE_COLOURS);
    let dict = "/ShadingType 7 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]";
    let r = render(&mesh_page(b"/Sh sh", dict, &data, "")).unwrap();
    check_square_patch(&r);
    // A mesh with /Function carries one parametric value per vertex,
    // interpolated before the function is applied.
    let mut data = Vec::new();
    data.extend(v8(0, 0, 0, &[0]));
    data.extend(v8(0, 100, 0, &[255]));
    data.extend(v8(0, 0, 100, &[255]));
    let dict = "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1] \
                /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >>";
    let r = render(&mesh_page(b"/Sh sh", dict, &data, "")).unwrap();
    let c = px(&r, 10, 90); // user (10, 10): t = 0.2
    assert!(near(c, (51, 51, 51), 4), "{c:?}");
    // As a pattern, /Background fills the rest of the shape; alpha applies.
    let dict = "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 \
                /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1] \
                /Background [1 1 0] \
                /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >>";
    let pat = "/P << /PatternType 2 /Shading 10 0 R >>";
    let r = render(&mesh_page(
        b"/Pattern cs /P scn 0 0 100 100 re f",
        dict,
        &data,
        pat,
    ))
    .unwrap();
    assert_eq!(
        px(&r, 90, 10),
        (255, 255, 0),
        "background outside the triangle"
    );
    assert!(near(px(&r, 10, 90), (51, 51, 51), 4));
}
