//! Optional content (ISO 32000-1 §8.11): the default configuration
//! decides what is painted and extracted; hidden content still
//! clips; OCMD policies and `/VE` expressions; bounds; the
//! include-hidden extraction mode.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, extract_text_all, render_page, Document, Error, Limits, RenderOptions,
    RenderedPage,
};

/// Page with `content`; `ocprops` is the catalog's `/OCProperties`
/// body (or "" for none); objects 10/11 are OCGs G1/G2; `props` is the
/// `/Properties` resource body; `extra` objects.
fn pdf(content: &[u8], ocprops: &str, props: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let catalog = if ocprops.is_empty() {
        "<< /Type /Catalog /Pages 2 0 R >>".to_string()
    } else {
        format!("<< /Type /Catalog /Pages 2 0 R /OCProperties {ocprops} >>")
    };
    let mut objs = vec![
        (1, catalog.into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >> >> /Properties << {props} >> \
                 /XObject << /Fm 20 0 R /Im 21 0 R >> >> /Contents 4 0 R /Annots [22 0 R] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
        (10, b"<< /Type /OCG /Name (G1) >>".to_vec()),
        (11, b"<< /Type /OCG /Name (G2) >>".to_vec()),
        (
            20,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /OC 10 0 R",
                b"0 0 1 rg 50 50 40 40 re f",
            ),
        ),
        (
            21,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
                 /ColorSpace /DeviceRGB /OC 10 0 R",
                &[0, 255, 0],
            ),
        ),
        (
            22,
            b"<< /Type /Annot /Subtype /Square /Rect [0 80 20 100] /OC 10 0 R /AP << /N 23 0 R >> >>"
                .to_vec(),
        ),
        (
            23,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
                b"1 0 0 rg 0 0 20 20 re f",
            ),
        ),
    ];
    // `extra` replaces same-numbered defaults (e.g. an OCG with /Usage).
    objs.retain(|(n, _)| !extra.iter().any(|(m, _)| m == n));
    objs.extend(extra);
    objs.sort_by_key(|(n, _)| *n);
    build_pdf(&objs)
}

const OFF_G1: &str = "<< /OCGs [10 0 R 11 0 R] /D << /OFF [10 0 R] >> >>";
const ALL_ON: &str = "<< /OCGs [10 0 R 11 0 R] /D << >> >>";

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const BLACK: (u8, u8, u8) = (0, 0, 0);

#[test]
fn hidden_group_hides_marked_content_and_visible_group_paints() {
    let content = b"/OC /P1 BDC 0 g 10 10 30 30 re f EMC /OC /P2 BDC 0 g 60 10 30 30 re f EMC";
    let r = render(&pdf(content, OFF_G1, "/P1 10 0 R /P2 11 0 R", vec![])).unwrap();
    assert_eq!(px(&r, 20, 80), WHITE, "G1 is OFF");
    assert_eq!(px(&r, 70, 80), BLACK, "G2 is ON");
    let r = render(&pdf(content, ALL_ON, "/P1 10 0 R /P2 11 0 R", vec![])).unwrap();
    assert_eq!(px(&r, 20, 80), BLACK);
}

#[test]
fn base_state_off_with_on_list() {
    let content = b"/OC /P1 BDC 0 g 10 10 30 30 re f EMC /OC /P2 BDC 0 g 60 10 30 30 re f EMC";
    let oc = "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /OFF /ON [11 0 R] >> >>";
    let r = render(&pdf(content, oc, "/P1 10 0 R /P2 11 0 R", vec![])).unwrap();
    assert_eq!(px(&r, 20, 80), WHITE);
    assert_eq!(px(&r, 70, 80), BLACK);
}

#[test]
fn hidden_xobjects_and_annotations_are_skipped() {
    let content = b"/Fm Do q 40 0 0 40 10 50 cm /Im Do Q";
    let r = render(&pdf(content, OFF_G1, "", vec![])).unwrap();
    assert_eq!(px(&r, 70, 30), WHITE, "form with /OC G1");
    assert_eq!(px(&r, 30, 30), WHITE, "image with /OC G1");
    assert_eq!(px(&r, 10, 10), WHITE, "annotation with /OC G1");
    let r = render(&pdf(content, ALL_ON, "", vec![])).unwrap();
    assert_eq!(px(&r, 70, 30), (0, 0, 255));
    assert_eq!(px(&r, 30, 30), (0, 255, 0));
    assert_eq!(px(&r, 10, 10), (255, 0, 0));
}

#[test]
fn hidden_content_still_clips_and_keeps_state() {
    // A clip set inside the hidden span restricts later visible paint;
    // a colour set inside it stays in effect.
    let content = b"/OC /P1 BDC 0 0 50 100 re W n 1 0 0 rg 0 0 100 100 re f EMC 0 0 100 100 re f";
    let r = render(&pdf(content, OFF_G1, "/P1 10 0 R", vec![])).unwrap();
    assert_eq!(
        px(&r, 25, 50),
        (255, 0, 0),
        "clip from the hidden span applies, colour too"
    );
    assert_eq!(px(&r, 75, 50), WHITE);
}

#[test]
fn nested_hidden_spans_and_text() {
    let content = b"/OC /P2 BDC /OC /P1 BDC 0 g 10 10 30 30 re f EMC 0 g 60 10 30 30 re f EMC \
                    BT /F1 20 Tf 10 60 Td /OC /P1 BDC (hidden) Tj EMC (shown) Tj ET";
    let p = pdf(content, OFF_G1, "/P1 10 0 R /P2 11 0 R", vec![]);
    let r = render(&p).unwrap();
    assert_eq!(px(&r, 20, 80), WHITE);
    assert_eq!(px(&r, 70, 80), BLACK);
    let doc = Document::from_bytes(&p).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    let s: String = t.chars.iter().filter_map(|c| c.unicode.clone()).collect();
    assert_eq!(s, "shown");
    assert!(
        t.chars[0].origin.0 > 10.0 + 40.0,
        "hidden text still advanced the position"
    );
    let t = extract_text_all(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    let s: String = t.chars.iter().filter_map(|c| c.unicode.clone()).collect();
    assert_eq!(s, "hiddenshown");
}

#[test]
fn ocmd_policies() {
    let content = b"/OC /M BDC 0 g 10 10 30 30 re f EMC";
    for (p, expect) in [
        ("/AnyOn", BLACK),
        ("/AllOn", WHITE),
        ("/AnyOff", BLACK),
        ("/AllOff", WHITE),
    ] {
        let props = format!("/M << /Type /OCMD /OCGs [10 0 R 11 0 R] /P {p} >>");
        let r = render(&pdf(content, OFF_G1, &props, vec![])).unwrap();
        assert_eq!(px(&r, 20, 80), expect, "{p}");
    }
    // Single reference, default AnyOn.
    let r = render(&pdf(
        content,
        OFF_G1,
        "/M << /Type /OCMD /OCGs 10 0 R >>",
        vec![],
    ))
    .unwrap();
    assert_eq!(px(&r, 20, 80), WHITE);
}

#[test]
fn ocmd_visibility_expressions() {
    let content = b"/OC /M BDC 0 g 10 10 30 30 re f EMC";
    for (ve, expect) in [
        ("[/Not 10 0 R]", BLACK),
        ("[/And 10 0 R 11 0 R]", WHITE),
        ("[/Or 10 0 R 11 0 R]", BLACK),
        ("[/And [/Not 10 0 R] 11 0 R]", BLACK),
        ("[/Or [/Not 11 0 R] 10 0 R]", WHITE),
    ] {
        let props = format!("/M << /Type /OCMD /VE {ve} >>");
        let r = render(&pdf(content, OFF_G1, &props, vec![])).unwrap();
        assert_eq!(px(&r, 20, 80), expect, "{ve}");
    }
    let err = render(&pdf(
        content,
        OFF_G1,
        "/M << /Type /OCMD /VE [/Xor 10 0 R] >>",
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    let mut deep = String::new();
    for _ in 0..40 {
        deep.push_str("[/Not ");
    }
    deep.push_str("10 0 R");
    for _ in 0..40 {
        deep.push(']');
    }
    let err = render(&pdf(
        content,
        OFF_G1,
        &format!("/M << /Type /OCMD /VE {deep} >>"),
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

#[test]
fn view_usage_application_overrides() {
    let oc = "<< /OCGs [10 0 R 11 0 R] /D << /AS [ << /Event /View /Category [/View] /OCGs [10 0 R] >> ] >> >>";
    let content = b"/OC /P1 BDC 0 g 10 10 30 30 re f EMC";
    let extra = vec![(
        10,
        b"<< /Type /OCG /Name (G1) /Usage << /View << /ViewState /OFF >> >> >>".to_vec(),
    )];
    let r = render(&pdf(content, oc, "/P1 10 0 R", extra)).unwrap();
    assert_eq!(
        px(&r, 20, 80),
        WHITE,
        "/Usage /View /ViewState /OFF applied through /AS"
    );
    // A /Print usage never affects what is shown.
    let oc = "<< /OCGs [10 0 R] /D << /AS [ << /Event /Print /Category [/Print] /OCGs [10 0 R] >> ] >> >>";
    let extra = vec![(
        10,
        b"<< /Type /OCG /Name (G1) /Usage << /Print << /PrintState /OFF >> >> >>".to_vec(),
    )];
    let r = render(&pdf(content, oc, "/P1 10 0 R", extra)).unwrap();
    assert_eq!(px(&r, 20, 80), BLACK);
}

#[test]
fn malformed_ocproperties_are_structure_errors() {
    let content = b"0 g 10 10 30 30 re f";
    for oc in [
        "42",
        "<< /OCGs 7 >>",
        "<< /OCGs [10 0 R] /D << /BaseState /Maybe >> >>",
        "<< /OCGs [10 0 R] /D << /OFF [(x)] >> >>",
    ] {
        let err = render(&pdf(content, oc, "", vec![])).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{oc}: {err:?}");
    }
    let err = render(&pdf(
        b"/OC /M BDC EMC",
        OFF_G1,
        "/M << /Type /OCMD /P /Sometimes /OCGs [10 0 R] >>",
        vec![],
    ))
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}
