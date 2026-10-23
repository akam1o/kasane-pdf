//! AcroForm widget appearance synthesis (ISO 32000-1 §12.7): widgets
//! without `/AP`, or variable-text fields under `/NeedAppearances`,
//! are drawn from the field value, `/DA`, `/MK` and `/Q`. 200×200 pt
//! page at 72 dpi: pixmap (x, y) = user (x, 200 − y).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

/// Page with widgets `annots` (object numbers), an `/AcroForm` body
/// and extra objects.
fn pdf(acroform: &str, annots: &[u32], objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let refs: String = annots.iter().map(|n| format!("{n} 0 R ")).collect();
    let fields: String = annots.iter().map(|n| format!("{n} 0 R ")).collect();
    let catalog =
        format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{fields}] {acroform} >> >>");
    let mut all = vec![
        (1, catalog.into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Annots [{refs}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"")),
    ];
    all.extend(objs);
    build_pdf(&all)
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

/// Dark pixels inside a pixmap rectangle (x0..x1, y0..y1).
fn ink(r: &RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = r.pixmap.pixel(x, y).unwrap();
            (p.red() as u32 + p.green() as u32 + p.blue() as u32) < 300
        })
        .count()
}

const WHITE: (u8, u8, u8) = (255, 255, 255);

#[test]
fn text_field_without_ap_draws_value_background_and_border() {
    // Rect 20..180 × 100..130 → pixmap x 20..180, y 70..100.
    let w = b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (Hello) \
              /Rect [20 100 180 130] /DA (/Helv 0 Tf 0 0 1 rg) \
              /MK << /BG [1 1 0.6] /BC [1 0 0] >> >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, w)])).unwrap();
    assert_eq!(px(&r, 170, 85), (255, 255, 153), "background");
    assert_eq!(px(&r, 20, 85), (255, 0, 0), "border");
    assert!(ink(&r, 22, 72, 100, 98) > 20, "text painted");
    assert_eq!(px(&r, 100, 150), WHITE);
    // Blue text colour from /DA.
    let blue = (22..100)
        .flat_map(|x| (72..98).map(move |y| (x, y)))
        .any(|(x, y)| {
            let p = r.pixmap.pixel(x, y).unwrap();
            p.blue() > 200 && p.red() < 120
        });
    assert!(blue);
    assert!(
        r.font_substitutions
            .iter()
            .any(|s| s.base_font.contains("Helvetica")),
        "the Helv fallback is reported as a substitution: {:?}",
        r.font_substitutions
    );
}

#[test]
fn quadding_aligns_the_value() {
    let make = |q: i32| {
        let w = format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (t) /V (ab) /Q {q} \
             /Rect [20 100 180 130] /DA (/Helv 12 Tf 0 g) >>"
        );
        render(&pdf("", &[10], vec![(10, w.into_bytes())])).unwrap()
    };
    let left = make(0);
    let centre = make(1);
    let right = make(2);
    assert!(ink(&left, 20, 70, 60, 100) > 0 && ink(&left, 140, 70, 180, 100) == 0);
    assert!(ink(&centre, 80, 70, 120, 100) > 0 && ink(&centre, 20, 70, 50, 100) == 0);
    assert!(ink(&right, 140, 70, 180, 100) > 0 && ink(&right, 20, 70, 60, 100) == 0);
}

#[test]
fn inherited_attributes_and_need_appearances() {
    // Value, /DA and /FT live on the parent field; the widget only has
    // /Rect. /NeedAppearances regenerates even though /AP exists.
    let parent = b"<< /FT /Tx /T (p) /V (X) /DA (/Helv 0 Tf 0 g) /Kids [10 0 R] >>".to_vec();
    let empty_ap = stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 160 30]", b"");
    let w = b"<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [20 100 180 130] \
              /AP << /N 12 0 R >> >>"
        .to_vec();
    let r = render(&pdf(
        "",
        &[10],
        vec![
            (10, w.clone()),
            (11, parent.clone()),
            (12, empty_ap.clone()),
        ],
    ))
    .unwrap();
    assert_eq!(
        ink(&r, 20, 70, 180, 100),
        0,
        "the empty /AP is used as given"
    );
    let r = render(&pdf(
        "/NeedAppearances true",
        &[10],
        vec![(10, w), (11, parent), (12, empty_ap)],
    ))
    .unwrap();
    assert!(ink(&r, 20, 70, 180, 100) > 10, "regenerated from /V");
}

#[test]
fn checkbox_and_radio_states() {
    let cb = |as_: &str, v: &str, ca: &str| {
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Btn /T (c) {v} {as_} \
             /Rect [50 50 100 100] /MK << /BC [0] {ca} >> >>"
        )
        .into_bytes()
    };
    let on = render(&pdf("", &[10], vec![(10, cb("/AS /Yes", "", ""))])).unwrap();
    let off = render(&pdf("", &[10], vec![(10, cb("/AS /Off", "", ""))])).unwrap();
    assert!(ink(&on, 60, 110, 90, 140) > 10, "check mark");
    assert_eq!(ink(&off, 60, 110, 90, 140), 0);
    assert_eq!(px(&off, 50, 125), (0, 0, 0), "border still drawn when off");
    // No /AS: the field value decides.
    let via_v = render(&pdf("", &[10], vec![(10, cb("", "/V /Yes", ""))])).unwrap();
    assert!(ink(&via_v, 60, 110, 90, 140) > 10);
    // Radio: filled dot in the middle.
    let radio = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 32768 /T (r) /AS /A \
         /Rect [50 50 100 100] /MK << /BC [0] >> >>"
    )
    .into_bytes();
    let r = render(&pdf("", &[10], vec![(10, radio)])).unwrap();
    assert_eq!(px(&r, 75, 125), (0, 0, 0), "dot centre");
    assert_eq!(px(&r, 56, 106), WHITE, "corner stays clear of the dot");
    // Cross style from /MK /CA.
    let cross = render(&pdf("", &[10], vec![(10, cb("/AS /Yes", "", "/CA (8)"))])).unwrap();
    assert!(ink(&cross, 60, 110, 90, 140) > 10);
}

#[test]
fn choice_fields() {
    // Combo shows the display text of the selected export value.
    let combo = b"<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (c) /V (b) \
                  /Opt [[(a) (Alpha)] [(b) (Beta)]] /Rect [20 100 180 130] \
                  /DA (/Helv 0 Tf 0 g) >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, combo)])).unwrap();
    assert!(ink(&r, 20, 70, 180, 100) > 10);
    // List: selected row highlighted, options listed from the top.
    let list = b"<< /Type /Annot /Subtype /Widget /FT /Ch /T (l) /V (two) \
                 /Opt [(one) (two) (three)] /Rect [20 40 180 160] /DA (/Helv 12 Tf 0 g) \
                 /MK << /BC [0] >> >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, list)])).unwrap();
    let highlighted = (30..170)
        .flat_map(|x| (40..160).map(move |y| (x, y)))
        .filter(|&(x, y)| px(&r, x, y) == (153, 191, 217))
        .count();
    assert!(highlighted > 200, "selection highlight: {highlighted}");
    assert!(ink(&r, 22, 42, 100, 60) > 0, "first option at the top");
}

#[test]
fn rotation_comb_and_multiline() {
    // /MK /R 90: the text runs upward along a tall rect.
    let rot = b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (t) /V (ROTATED) \
                /Rect [80 20 110 180] /DA (/Helv 0 Tf 0 g) /MK << /R 90 /BC [0] >> >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, rot)])).unwrap();
    assert!(ink(&r, 82, 22, 108, 178) > 20);
    // Comb: 4 cells, each with one character.
    let comb = b"<< /Type /Annot /Subtype /Widget /FT /Tx /Ff 16777216 /MaxLen 4 /T (t) \
                 /V (1234) /Rect [20 100 180 130] /DA (/Helv 0 Tf 0 g) >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, comb)])).unwrap();
    for cell in 0..4 {
        let x0 = 20 + cell * 40;
        assert!(ink(&r, x0 + 5, 70, x0 + 35, 100) > 0, "cell {cell}");
    }
    // Multiline: two lines of text stack downwards.
    let ml = b"<< /Type /Annot /Subtype /Widget /FT /Tx /Ff 4096 /T (t) \
               /V (first line\\nsecond) /Rect [20 60 180 160] /DA (/Helv 12 Tf 0 g) >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, ml)])).unwrap();
    assert!(ink(&r, 20, 42, 180, 58) > 0, "line 1");
    assert!(ink(&r, 20, 58, 180, 74) > 0, "line 2");
    assert_eq!(ink(&r, 20, 100, 180, 140), 0, "nothing below the text");
}

#[test]
fn edge_cases_and_refusals() {
    // Signature field without /AP draws nothing and does not refuse.
    let sig =
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [20 100 180 130] >>".to_vec();
    let r = render(&pdf("", &[10], vec![(10, sig)])).unwrap();
    assert_eq!(ink(&r, 20, 70, 180, 100), 0);
    // Empty value: only the border.
    let empty = b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (t) /V () /Rect [20 100 180 130] \
                  /MK << /BC [0] >> >>"
        .to_vec();
    let r = render(&pdf("", &[10], vec![(10, empty)])).unwrap();
    assert_eq!(px(&r, 20, 85), (0, 0, 0));
    assert_eq!(ink(&r, 24, 74, 176, 96), 0);
    // A CJK value cannot be drawn with the Latin fallback font.
    let cjk = "<< /Type /Annot /Subtype /Widget /FT /Tx /T (t) /V <FEFF65E5672C> \
               /Rect [20 100 180 130] /DA (/Helv 0 Tf 0 g) >>"
        .as_bytes()
        .to_vec();
    let err = render(&pdf("", &[10], vec![(10, cjk)])).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    // /Parent cycle is bounded.
    let a = b"<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [20 100 180 130] >>".to_vec();
    let b = b"<< /Parent 10 0 R /Kids [10 0 R] >>".to_vec();
    let err = render(&pdf("", &[10], vec![(10, a), (11, b)])).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
    // Non-widget annotations without /AP are still refused.
    let sq = b"<< /Type /Annot /Subtype /Square /Rect [20 100 180 130] >>".to_vec();
    let err = render(&pdf("", &[10], vec![(10, sq)])).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}
