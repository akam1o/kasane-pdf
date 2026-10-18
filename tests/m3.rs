//! M3 integration tests: page → pixmap rendering, PNG output, and the
//! error/limits surface for unsupported content.

use kasane_pdf::{pixmap_to_png, render_page, Document, Error, Limits, RenderOptions};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Build a 1-page PDF whose `/Contents` is `stream` (MediaBox
/// `media`, optional extra page-dict keys).
fn pdf_with_stream(stream: &[u8], media: &str, extra: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let page = format!("<< /Type /Page /Parent 2 0 R /MediaBox {media} {extra} /Contents 4 0 R >>");
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, page.into_bytes()),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn render(stream: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    render_with(stream, "[0 0 100 100]", "", 72.0)
}

fn render_with(
    stream: &[u8],
    media: &str,
    extra: &str,
    dpi: f64,
) -> Result<kasane_pdf::RenderedPage, Error> {
    let pdf = pdf_with_stream(stream, media, extra);
    let doc = Document::from_bytes(&pdf)?;
    render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions {
            dpi,
            ..Default::default()
        },
    )
}

/// Read one RGBA pixel.
fn px(page: &kasane_pdf::RenderedPage, x: u32, y: u32) -> [u8; 4] {
    let i = (y * page.width + x) as usize * 4;
    let d = page.pixmap.data();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

// -------------------------------------------------------------------
// Basic output
// -------------------------------------------------------------------

#[test]
fn blank_page_is_white() {
    let r = render(b"").unwrap();
    assert_eq!((r.width, r.height), (100, 100));
    assert_eq!(px(&r, 50, 50), [255, 255, 255, 255]);
}

#[test]
fn dpi_scales_dimensions() {
    let r = render_with(b"", "[0 0 100 100]", "", 144.0).unwrap();
    assert_eq!((r.width, r.height), (200, 200));
    let r = render_with(b"", "[0 0 100 50]", "", 72.0).unwrap();
    assert_eq!((r.width, r.height), (100, 50));
}

#[test]
fn png_output_is_decodable() {
    let r = render(b"0 0 0 rg 10 10 20 20 re f").unwrap();
    let png = pixmap_to_png(&r.pixmap).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // PNG dims must match the pixmap.
    let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    assert_eq!((w, h), (100, 100));
}

// -------------------------------------------------------------------
// Path painting
// -------------------------------------------------------------------

#[test]
fn fill_rect_paints_black_on_white() {
    let r = render(b"0 g 10 10 30 30 re f").unwrap();
    // PDF (10,10)-(40,40) in a 100-pt page at 72dpi → pixmap y is
    // flipped: top-left pixel (10..40, 60..90).
    assert_eq!(px(&r, 20, 75), [0, 0, 0, 255]);
    assert_eq!(px(&r, 5, 5), [255, 255, 255, 255]);
}

#[test]
fn stroke_rect_draws_outline() {
    let r = render(b"5 w 0 0 1 RG 10 10 30 30 re S").unwrap();
    // Stroke center on the rect edge; top edge of the rect (PDF y=40)
    // is pixmap y≈60. `0 0 1 RG` is blue.
    assert_eq!(px(&r, 25, 60), [0, 0, 255, 255]);
}

#[test]
fn fill_rules_differ() {
    // Two overlapping rects in one path: winding fills the overlap,
    // even-odd clears it.
    let path = b"0 g 10 10 60 60 re 20 20 60 60 re";
    let mut winding_src = path.to_vec();
    winding_src.extend_from_slice(b" f");
    let mut evenodd_src = path.to_vec();
    evenodd_src.extend_from_slice(b" f*");
    let winding = render(&winding_src).unwrap();
    let evenodd = render(&evenodd_src).unwrap();
    // A point inside the overlap (e.g. 40,40 user → x40,y60 pixmap):
    assert_eq!(px(&winding, 40, 60), [0, 0, 0, 255]);
    assert_eq!(px(&evenodd, 40, 60), [255, 255, 255, 255]);
}

#[test]
fn line_width_zero_is_hairline() {
    // A width-0 stroke should still render something (hairline).
    let r = render(b"0 w 0 0 0 RG 10 50 m 90 50 l S").unwrap();
    assert_ne!(px(&r, 50, 50), [255, 255, 255, 255]);
}

#[test]
fn q_restores_color_and_path_is_not_saved() {
    // After Q the colour reverts; the path survives (it is not gstate).
    let r = render(b"q 1 0 0 rg Q 0 1 0 rg 10 10 20 20 re f Q");
    // The extra Q at the end should be an error — Q without q.
    assert!(r.is_err());

    let r = render(b"q 1 0 0 rg Q 10 10 20 20 re f").unwrap();
    // Q restored fill to black, so the rect is black not red.
    assert_eq!(px(&r, 20, 80), [0, 0, 0, 255]);
}

#[test]
fn cm_mid_path_only_moves_later_elements() {
    // §8.4.3.1: a cm after a `m` does not move the existing point.
    // Path: m at (10,10), then cm translate +50, then l to (0,0) →
    // the line runs from pixmap (10,90) toward (50,100)-ish.
    let r = render(b"10 w 0 0 0 RG 10 10 m 1 0 0 1 50 0 cm 0 10 l S").unwrap();
    assert_ne!(px(&r, 15, 85), [255, 255, 255, 255]);
}

#[test]
fn n_drops_path_without_painting() {
    let r = render(b"0 0 0 rg 10 10 50 50 re n").unwrap();
    assert_eq!(px(&r, 30, 50), [255, 255, 255, 255]);
}

// -------------------------------------------------------------------
// Colour
// -------------------------------------------------------------------

#[test]
fn device_colors_render() {
    let r =
        render(b"0.5 g 10 10 10 10 re f 1 0 0 rg 30 10 10 10 re f 0 0 1 0.5 k 50 10 10 10 re f")
            .unwrap();
    assert_eq!(px(&r, 15, 85), [128, 128, 128, 255]);
    assert_eq!(px(&r, 35, 85), [255, 0, 0, 255]);
    // 0,0,1,0.5 CMYK → yellow-ish (1-min(1,0+0.5)=0.5? compute:
    // c=0,m=0,y=1,k=0.5 → r=1-0.5=0.5,g=0.5,b=0 → (128,128,0))
    assert_eq!(px(&r, 55, 85), [128, 128, 0, 255]);
}

#[test]
fn cs_and_sc_select_space() {
    let r = render(b"/DeviceRGB cs 0 1 0 sc 10 10 20 20 re f").unwrap();
    assert_eq!(px(&r, 20, 80), [0, 255, 0, 255]);
}

#[test]
fn default_color_space_maps_to_device() {
    let r = render(b"/DefaultGray cs 0.5 sc 10 10 20 20 re f").unwrap();
    assert_eq!(px(&r, 20, 80), [128, 128, 128, 255]);
}

#[test]
fn non_device_color_space_is_unsupported() {
    let err = render(b"/ICCBased cs 0.5 sc 10 10 20 20 re f").unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("colour space")),
        "got: {err:?}"
    );
}

#[test]
fn scn_pattern_name_is_unsupported() {
    let err = render(b"/Pattern cs /P1 scn 10 10 20 20 re f").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

// -------------------------------------------------------------------
// Clipping
// -------------------------------------------------------------------

#[test]
fn clip_restricts_painting() {
    // Clip to x≥50 (right half); filling whole page should only draw
    // there. PDF y-down: pixmap x ≥ 50.
    let r = render(b"50 0 50 100 re W n 0 0 0 rg 0 0 100 100 re f").unwrap();
    assert_eq!(px(&r, 75, 50), [0, 0, 0, 255]);
    assert_eq!(px(&r, 25, 50), [255, 255, 255, 255]);
}

#[test]
fn clip_is_restored_by_q() {
    let r = render(b"q 50 0 50 100 re W n 0 0 0 rg 0 0 100 100 re f Q 10 10 20 20 re f").unwrap();
    // After Q the clip is gone; the second rect paints on the left.
    assert_eq!(px(&r, 15, 80), [0, 0, 0, 255]);
}

// -------------------------------------------------------------------
// Unsupported features refuse the page
// -------------------------------------------------------------------

#[test]
fn text_page_is_refused() {
    let err = render(b"BT /F1 12 Tf 10 10 Td (x) Tj ET").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

#[test]
fn tf_alone_is_refused() {
    let err = render(b"BT /F1 12 Tf ET").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

#[test]
fn do_xobject_is_refused() {
    let err = render(b"/Im1 Do").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

#[test]
fn gs_extgstate_is_refused() {
    let err = render(b"/GS1 gs 10 10 10 10 re f").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

#[test]
fn shading_is_refused() {
    let err = render(b"/Sh1 sh").unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

#[test]
fn annotated_page_is_refused() {
    let doc = Document::from_bytes(&fixture("render_annot.pdf")).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("annotation")),
        "got: {err:?}"
    );
}

#[test]
fn marked_content_and_compat_are_noops() {
    let r = render(b"/Tag BMC 0 0 0 rg 10 10 20 20 re f EMC BX 1 2 UnknownOp EX").unwrap();
    assert_eq!(px(&r, 20, 80), [0, 0, 0, 255]);
}

// -------------------------------------------------------------------
// Page geometry
// -------------------------------------------------------------------

#[test]
fn rotate_90_swaps_dimensions() {
    let r = render_with(b"", "[0 0 200 100]", "/Rotate 90", 72.0).unwrap();
    assert_eq!((r.width, r.height), (100, 200));
}

#[test]
fn cropbox_offset_is_used() {
    // CropBox [50 50 150 150]: only that 100x100 region is rendered.
    let r = render_with(
        b"0 0 0 rg 0 0 200 200 re f",
        "[0 0 200 200]",
        "/CropBox [50 50 150 150]",
        72.0,
    )
    .unwrap();
    assert_eq!((r.width, r.height), (100, 100));
    // The whole visible area is inside the black fill.
    assert_eq!(px(&r, 50, 50), [0, 0, 0, 255]);
}

#[test]
fn nonzero_origin_page() {
    // MediaBox [100 100 300 300]: the page's user-space origin is at
    // PDF (100,100), which maps to pixmap (0,200) after the y-flip
    // (PDF bottom-left → pixmap bottom-left).
    let r = render_with(
        b"0 0 0 rg 100 100 20 20 re f",
        "[100 100 300 300]",
        "",
        72.0,
    )
    .unwrap();
    assert_eq!((r.width, r.height), (200, 200));
    // The rect PDF (100,100)-(120,120) → pixmap x 0–20, y 180–200.
    assert_eq!(px(&r, 10, 190), [0, 0, 0, 255]);
    // Outside the rect stays white.
    assert_eq!(px(&r, 50, 50), [255, 255, 255, 255]);
}

#[test]
fn userunit_other_than_one_is_unsupported() {
    let err = render_with(b"", "[0 0 100 100]", "/UserUnit 2", 72.0).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("UserUnit")),
        "got: {err:?}"
    );
}

#[test]
fn bad_dpi_is_rejected() {
    let pdf = pdf_with_stream(b"", "[0 0 100 100]", "");
    let doc = Document::from_bytes(&pdf).unwrap();
    for dpi in [0.0, -72.0, f64::NAN, f64::INFINITY] {
        let err = render_page(
            &doc,
            &doc.pages()[0],
            &RenderOptions {
                dpi,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "dpi {dpi}: {err:?}");
    }
}

// -------------------------------------------------------------------
// Limits
// -------------------------------------------------------------------

#[test]
fn render_pixel_limit() {
    let pdf = pdf_with_stream(b"", "[0 0 1000 1000]", "");
    let doc = Document::from_bytes(&pdf).unwrap();
    let err = render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions {
            dpi: 72.0,
            limits: Limits {
                max_render_pixels: 500,
                ..Default::default()
            },
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_render_pixels"),
        "got: {err:?}"
    );
}

#[test]
fn render_total_pixels_limit() {
    // 600x600 exceeds total 300_000 but neither dim exceeds 1000.
    let pdf = pdf_with_stream(b"", "[0 0 600 600]", "");
    let doc = Document::from_bytes(&pdf).unwrap();
    let err = render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions {
            dpi: 72.0,
            limits: Limits {
                max_render_total_pixels: 300_000,
                ..Default::default()
            },
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_render_total_pixels"),
        "got: {err:?}"
    );
}

#[test]
fn path_elements_limit() {
    let limits = Limits {
        max_path_elements: 3,
        ..Default::default()
    };
    let pdf = pdf_with_stream(b"0 0 m 1 1 l 2 2 l 3 3 l 4 4 l f", "[0 0 100 100]", "");
    let doc = Document::from_bytes(&pdf).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &RenderOptions { dpi: 72.0, limits }).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_path_elements"),
        "got: {err:?}"
    );
}

// -------------------------------------------------------------------
// No partial output
// -------------------------------------------------------------------

#[test]
fn render_rects_fixture_paints_expected_pixels() {
    // The fixture has a gray-filled+blue-stroked rect at (20,20)-(80,80)
    // and a green+red rect at (60,60)-(140,100).
    let doc = Document::from_bytes(&fixture("render_rects.pdf")).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    assert_eq!((r.width, r.height), (200, 200));
    // Inside first rect (PDF 40,40 → pixmap 40,160).
    assert_eq!(px(&r, 40, 160), [128, 128, 128, 255]);
    // The final clipped fill covers pixmap x ≥ 100 (PDF x ≥ 100 in a
    // 200-pt page at 72 dpi): red on the right, the earlier content on
    // the left.
    assert_eq!(px(&r, 150, 30), [255, 0, 0, 255]);
    // Second rect's green fill region (PDF 100,70 → pixmap 100,130):
    // left of the clip boundary it is green.
    assert_eq!(px(&r, 90, 130), [0, 255, 0, 255]);
}

#[test]
fn error_does_not_produce_output() {
    // On error there is no RenderedPage; the CLI must not write a file.
    assert!(render(b"BT /F1 12 Tf ET").is_err());
}
