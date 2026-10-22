//! ExtGState (`gs` operator) rendering tests.
//!
//! `/ExtGState` resource dictionaries set parameters the `w`/`J`/`j`
//! `M`/`d` operators also set, plus transparency (`ca`/`CA`/`BM`).
//! These tests cover parameter application, `q`/`Q` save/restore,
//! alpha compositing, blend modes, and the strict-Unsupported path
//! for features we deliberately don't approximate.

use kasane_pdf::{render_page, Document, Error, RenderOptions};

/// Build a 1-page PDF whose page carries a `/Resources` dict holding
/// `/ExtGState << /GS0 <contents> >>`, and `/Contents` = `stream`.
fn pdf_with_gs(stream: &[u8], gs_dict: &str) -> Vec<u8> {
    let resources = format!("/Resources << /ExtGState << /GS0 {gs_dict} >> >>");
    pdf_with_resources(stream, &resources)
}

fn pdf_with_resources(stream: &[u8], resources: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let page = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] {resources} /Contents 4 0 R >>"
    );
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

fn render_gs(stream: &[u8], gs: &str) -> Result<kasane_pdf::RenderedPage, Error> {
    let pdf = pdf_with_gs(stream, gs);
    let doc = Document::from_bytes(&pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(page: &kasane_pdf::RenderedPage, x: u32, y: u32) -> [u8; 4] {
    let i = (y * page.width + x) as usize * 4;
    let d = page.pixmap.data();
    [d[i], d[i + 1], d[i + 2], d[i + 3]]
}

// -------------------------------------------------------------------
// Line parameters
// -------------------------------------------------------------------

#[test]
fn gs_line_width_applies() {
    // A /LW 20 gs then stroke a 100-pt vertical line at x=50.
    // The thick stroke covers x=50±10, so a pixel at x=45 is dark.
    let r = render_gs(b"/GS0 gs 1 0 0 RG 50 0 m 50 100 l S", "<< /LW 20 >>").unwrap();
    let p = px(&r, 45, 50);
    assert!(p[0] > 150, "expected mostly-red stroke, got {p:?}");
    assert!(p[1] < 60, "expected low green, got {p:?}");
}

#[test]
fn gs_dash_applies() {
    // Dashed vertical line: on for the first half, off for the second
    // (page y-flips, so pixmap bottom = user y=0). `/D` is an array
    // whose first element is the dash array and second the phase.
    let r = render_gs(
        b"/GS0 gs 0 0 0 RG 50 0 m 50 100 l S",
        "<< /LW 10 /D [[30 30] 0] >>",
    )
    .unwrap();
    let on = px(&r, 50, 15); // user y≈85 — inside a dash
    let off = px(&r, 50, 45); // user y≈55 — inside a gap
    assert!(on[0] < 100, "expected dark dash, got {on:?}");
    assert!(off[0] > 200, "expected white gap, got {off:?}");
}

#[test]
fn gs_q_q_restores_state() {
    // Set ca=0.5 via gs inside q/Q, then a second fill outside must
    // use the *outer* (opaque) state.
    let r = render_gs(
        b"q /GS0 gs 0 0 1 rg 10 10 40 40 re f Q 0 0 0 rg 10 60 40 30 re f",
        "<< /ca 0.5 >>",
    )
    .unwrap();
    // Blue rect (user y 10..50 → pixmap y 50..90) painted at ca .5.
    let inside = px(&r, 30, 78);
    assert!(
        inside[2] > 200 && inside[0] > 100 && inside[0] < 160,
        "half-blue, got {inside:?}"
    );
    // Black rect (user y 60..90 → pixmap y 10..40) — Q restored opaque.
    let outside = px(&r, 30, 20);
    assert_eq!(outside, [0, 0, 0, 255], "restored opaque, got {outside:?}");
}

// -------------------------------------------------------------------
// Alpha
// -------------------------------------------------------------------

#[test]
fn gs_fill_alpha_blends() {
    // Full-rect fill at ca 0.5 → greyish.
    let r = render_gs(b"/GS0 gs 0 g 0 0 100 100 re f", "<< /ca 0.5 >>").unwrap();
    let p = px(&r, 50, 50);
    // 50% black over white ≈ 127
    assert!(p[0] > 110 && p[0] < 145, "expected ~127 grey, got {p:?}");
}

#[test]
fn gs_stroke_alpha_applies() {
    let r = render_gs(b"/GS0 gs 0 g 20 w 50 0 m 50 100 l S", "<< /CA 0.5 >>").unwrap();
    let p = px(&r, 50, 50);
    assert!(p[0] > 110 && p[0] < 145, "half-alpha stroke, got {p:?}");
}

#[test]
fn gs_alpha_persists_across_color_change() {
    // ca set once stays in effect for later fills until Q (gs state).
    let r = render_gs(
        b"/GS0 gs 0 g 0 0 50 100 re f 1 0 0 rg 50 0 50 100 re f",
        "<< /ca 0.5 >>",
    )
    .unwrap();
    let left = px(&r, 25, 50); // black @ .5 → ~127 grey
    let right = px(&r, 75, 50); // red   @ .5 → [255,127,127]
    assert!(left[0] > 110 && left[0] < 145, "got {left:?}");
    assert!(
        right[0] > 200 && right[1] > 110 && right[1] < 145,
        "got {right:?}"
    );
}

// -------------------------------------------------------------------
// Blend modes
// -------------------------------------------------------------------

#[test]
fn gs_normal_blend_is_default() {
    let r = render_gs(b"/GS0 gs 0 g 0 0 100 100 re f", "<< /BM /Normal >>").unwrap();
    assert_eq!(px(&r, 50, 50), [0, 0, 0, 255]);
}

#[test]
fn gs_multiply_blend() {
    // Blue square under a Multiply red square → dark.
    let r = render_gs(
        b"0 0 1 rg 0 0 100 100 re f /GS0 gs 1 0 0 rg 0 0 100 100 re f",
        "<< /BM /Multiply >>",
    )
    .unwrap();
    let p = px(&r, 50, 50);
    // blue × red = black
    assert!(p[0] < 60 && p[2] < 60, "multiply → dark, got {p:?}");
}

#[test]
fn gs_screen_blend_brightens() {
    // Grey under Screen grey → lighter.
    let r = render_gs(
        b"0.5 g 0 0 100 100 re f /GS0 gs 0.5 g 0 0 100 100 re f",
        "<< /BM /Screen >>",
    )
    .unwrap();
    let p = px(&r, 50, 50);
    assert!(p[0] > 150, "screen → brighter, got {p:?}");
}

// -------------------------------------------------------------------
// Accepted no-ops / metadata
// -------------------------------------------------------------------

#[test]
fn gs_metadata_and_noops_accepted() {
    // /Type /ExtGState, overprint flags off, AIS/SA/TK, /SMask /None,
    // /Default TR — all accepted without changing output.
    let r = render_gs(
        b"/GS0 gs 0 g 0 0 100 100 re f",
        "<< /Type /ExtGState /OP false /op false /OPM 1 /AIS false /SA true /TK true /SMask /None /TR /Default >>",
    )
    .unwrap();
    assert_eq!(px(&r, 50, 50), [0, 0, 0, 255]);
}

#[test]
fn gs_ri_accepted() {
    let r = render_gs(b"/GS0 gs 0 g 0 0 100 100 re f", "<< /RI /Perceptual >>").unwrap();
    assert_eq!(px(&r, 50, 50), [0, 0, 0, 255]);
}

// -------------------------------------------------------------------
// Strict Unsupported
// -------------------------------------------------------------------

#[test]
fn gs_real_smask_is_unsupported() {
    // A luminosity soft mask — we refuse rather than mis-render.
    let r = render_gs(
        b"/GS0 gs 0 g 0 0 100 100 re f",
        "<< /SMask << /Type /Mask /S /Luminosity /G 9 0 R >> >>",
    );
    match r {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "soft mask (/SMask)");
        }
        other => panic!("expected Unsupported soft mask, got {other:?}"),
    }
}

#[test]
fn gs_unknown_key_is_unsupported() {
    let r = render_gs(b"/GS0 gs 0 g 0 0 100 100 re f", "<< /FooBar 7 >>");
    match r {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "ExtGState parameter");
        }
        other => panic!("expected Unsupported param, got {other:?}"),
    }
}

#[test]
fn gs_transfer_function_is_unsupported() {
    let r = render_gs(
        b"/GS0 gs 0 g 0 0 100 100 re f",
        "<< /TR << /FunctionType 0 >> >>",
    );
    match r {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "transfer/black-generation function");
        }
        other => panic!("expected Unsupported TR, got {other:?}"),
    }
}

// -------------------------------------------------------------------
// Structure errors
// -------------------------------------------------------------------

#[test]
fn gs_missing_resource_is_structure_error() {
    let pdf = pdf_with_resources(b"/GS0 gs 0 g 0 0 100 100 re f", "/Resources << >>");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default());
    assert!(matches!(r, Err(Error::Structure(_))), "got {r:?}");
}

#[test]
fn gs_bad_lw_type_is_structure_error() {
    let r = render_gs(b"/GS0 gs 0 g 0 0 100 100 re f", "<< /LW /Wide >>");
    assert!(matches!(r, Err(Error::Structure(_))), "got {r:?}");
}

// -------------------------------------------------------------------
// gs with no /Resources at all
// -------------------------------------------------------------------

#[test]
fn gs_no_resources_errors() {
    // gs but page has no /Resources → can't resolve.
    let pdf = pdf_with_resources(b"/GS0 gs 0 g 0 0 100 100 re f", "");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default());
    assert!(r.is_err(), "expected error, got {r:?}");
}

// -------------------------------------------------------------------
// Blend mode on an Image XObject — the mis-render guard.
// -------------------------------------------------------------------

/// Build a 1-page PDF: a grey image XObject painted under an ExtGState
/// with a chosen `/BM`. Object 5 = image, gs in page resources.
fn pdf_image_with_gs(bm: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    // image: 2x2 mid-grey RGB, 8bpc, raw (Length=12)
    let img_data = vec![128u8; 2 * 2 * 3];
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
        /Resources << /ExtGState << /GS0 << /BM /BMX >> >> \
        /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>"
        .replace("BMX", bm);
    let contents = b"/GS0 gs 100 0 0 100 0 0 cm /Im0 Do\n";
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, page.into_bytes()),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", contents.len()).into_bytes(),
                contents.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (
            5,
            [
                b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
                  /ColorSpace /DeviceRGB /BitsPerComponent 8 /Length 12 >>\nstream\n"
                    .to_vec(),
                img_data,
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
    out.extend_from_slice(b"xref\n0 6\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn image_multiply_blend_darkens() {
    // Grey image over white page under /Multiply → 128*255/255 ≈ 128
    // but multiplied over white stays ~128 (grey). Over a painted
    // black backdrop it darkens toward black.
    let pdf = pdf_image_with_gs("Multiply");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    // Image fills the whole page; multiply of mid-grey over white ≈ grey.
    let p = px(&r, 50, 50);
    assert!(p[0] > 100 && p[0] < 160, "multiply grey, got {p:?}");
}

#[test]
fn image_nonseparable_blend_refused() {
    // /Hue on an image: needs per-pixel colour conversion → refuse
    // rather than mis-render as Normal.
    let pdf = pdf_image_with_gs("Hue");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default());
    match r {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "non-separable blend mode on image");
        }
        other => panic!("expected non-separable refusal, got {other:?}"),
    }
}
