//! Annotation and form-field text (`TextOptions::with_annotations`):
//! one record per annotation, sources kept apart (`/Contents`, `/RC`,
//! the field value, the appearance's characters), one prioritised
//! `text()`, nothing added to the page content's `chars`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, Document, Error, ExtractedText, Limits, TextOptions};

/// A page whose content shows "Body" and whose `/Annots` is `annots`
/// (object references or inline dictionaries); `objects` from 10.
fn pdf(annots: &str, objects: Vec<(u32, Vec<u8>)>, catalog_extra: &str) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
                 /Resources << /Font << /F 5 0 R >> >> /Annots [{annots}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"BT /F 12 Tf 10 280 Td (Body) Tj ET")),
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ];
    objs.extend(objects);
    build_pdf(&objs)
}

/// An appearance stream drawing `text` with Helvetica in a 100 × 20 box.
fn ap(text: &str) -> Vec<u8> {
    stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /H 5 0 R >> >>",
        format!("BT /H 10 Tf 2 5 Td ({text}) Tj ET").as_bytes(),
    )
}

fn extract(pdf: &[u8], on: bool) -> Result<ExtractedText, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(
        &doc,
        &doc.pages()[0],
        &TextOptions::new().with_annotations(on),
    )
}

fn chars(t: &[kasane_pdf::CharInfo]) -> String {
    t.iter().filter_map(|c| c.unicode.as_deref()).collect()
}

#[test]
fn off_by_default_and_page_text_never_changes() {
    let p = pdf(
        "10 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /FreeText /Rect [10 10 110 30] /Contents (Note) /AP << /N 11 0 R >> >>".to_vec()),
            (11, ap("Note")),
        ],
        "",
    );
    let off = extract(&p, false).unwrap();
    let on = extract(&p, true).unwrap();
    assert!(off.annotations.is_empty());
    assert_eq!(chars(&off.chars), "Body");
    assert_eq!(
        chars(&on.chars),
        "Body",
        "annotation text must not join the page text"
    );
    assert_eq!(on.annotations.len(), 1);
}

#[test]
fn free_text_contents_rich_text_and_appearance_stay_apart() {
    let p = pdf(
        "10 0 R",
        vec![
            (
                10,
                b"<< /Type /Annot /Subtype /FreeText /Rect [110 30 10 10] /Contents (Plain words) \
                  /RC (<body><p>Rich <b>words</b></p></body>) /AP << /N 11 0 R >> >>"
                    .to_vec(),
            ),
            (11, ap("Drawn words")),
        ],
        "",
    );
    let t = extract(&p, true).unwrap();
    let a = &t.annotations[0];
    assert_eq!(a.subtype, "FreeText");
    assert_eq!(
        (a.rect.llx, a.rect.lly, a.rect.urx, a.rect.ury),
        (10.0, 10.0, 110.0, 30.0)
    );
    assert_eq!(a.contents.as_deref(), Some("Plain words"));
    assert_eq!(
        a.rich_text.as_deref(),
        Some("<body><p>Rich <b>words</b></p></body>")
    );
    assert_eq!(chars(&a.appearance), "Drawn words");
    // Placed on the page inside the annotation rectangle.
    assert!(a
        .appearance
        .iter()
        .all(|c| (10.0..110.0).contains(&c.origin.0) && (10.0..30.0).contains(&c.origin.1)));
    assert_eq!(a.text().as_deref(), Some("Plain words"));
}

fn widget(n: u32, extra: &str, ap_ref: Option<u32>) -> (u32, Vec<u8>) {
    let ap = ap_ref
        .map(|r| format!("/AP << /N {r} 0 R >>"))
        .unwrap_or_default();
    (
        n,
        format!("<< /Type /Annot /Subtype /Widget /Rect [10 100 110 120] {ap} {extra} >>")
            .into_bytes(),
    )
}

#[test]
fn a_text_field_reports_its_value_not_a_stale_appearance() {
    let p = pdf(
        "10 0 R",
        vec![
            widget(
                10,
                "/FT /Tx /T (name) /V (New value) /DV (Default)",
                Some(11),
            ),
            (11, ap("Old value")),
        ],
        "/AcroForm << /Fields [10 0 R] >>",
    );
    let a = &extract(&p, true).unwrap().annotations[0];
    let f = a.field.as_ref().unwrap();
    assert_eq!((f.name.as_str(), f.field_type.as_str()), ("name", "Tx"));
    assert_eq!(f.value, vec!["New value"]);
    assert_eq!(f.default_value, vec!["Default"]);
    assert_eq!(chars(&a.appearance), "Old value");
    assert_eq!(a.text().as_deref(), Some("New value"));
}

#[test]
fn choice_fields_report_display_text() {
    let p = pdf(
        "10 0 R 12 0 R",
        vec![
            widget(10, "/FT /Ch /T (one) /Opt [[(e1) (Display one)] (Two)] /V (e1)", Some(11)),
            (11, ap("Display one")),
            widget(12, "/FT /Ch /Ff 2097152 /T (many) /Opt [[(e1) (D1)] [(e2) (D2)] (Three)] /V [(e2) (Three)]", Some(11)),
        ],
        "/AcroForm << /Fields [10 0 R 12 0 R] >>",
    );
    let t = extract(&p, true).unwrap();
    assert_eq!(
        t.annotations[0].field.as_ref().unwrap().value,
        vec!["Display one"]
    );
    assert_eq!(
        t.annotations[1].field.as_ref().unwrap().value,
        vec!["D2", "Three"]
    );
    assert_eq!(t.annotations[1].text().as_deref(), Some("D2\nThree"));
}

#[test]
fn empty_unicode_and_button_fields() {
    let p = pdf(
        "10 0 R 12 0 R 13 0 R",
        vec![
            widget(10, "/FT /Tx /T (empty)", Some(11)),
            (
                11,
                stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 100 20]", b""),
            ),
            // UTF-16BE "日本語"
            widget(12, "/FT /Tx /T (jp) /V <FEFF65E5672C8A9E>", Some(11)),
            widget(13, "/FT /Btn /T (check) /V /Yes /AS /Yes", Some(11)),
        ],
        "/AcroForm << /Fields [10 0 R 12 0 R 13 0 R] >>",
    );
    let t = extract(&p, true).unwrap();
    assert!(t.annotations[0].field.as_ref().unwrap().value.is_empty());
    assert_eq!(t.annotations[0].text(), None);
    assert_eq!(t.annotations[1].text().as_deref(), Some("日本語"));
    // A button's /V is a state name, not text.
    assert!(t.annotations[2].field.as_ref().unwrap().value.is_empty());
}

#[test]
fn hierarchical_names_and_one_value_per_field() {
    // Field "form.addr" (no widget of its own) with two widget kids;
    // the same widget is also listed twice in /Annots.
    let p = pdf(
        "11 0 R 12 0 R 11 0 R",
        vec![
            (10, b"<< /T (form) /Kids [14 0 R] >>".to_vec()),
            (
                14,
                b"<< /T (addr) /FT /Tx /V (1 Main St) /Parent 10 0 R /Kids [11 0 R 12 0 R] >>"
                    .to_vec(),
            ),
            widget(11, "/Parent 14 0 R", Some(13)),
            widget(12, "/Parent 14 0 R", Some(13)),
            (13, ap("1 Main St")),
        ],
        "/AcroForm << /Fields [10 0 R] >>",
    );
    let t = extract(&p, true).unwrap();
    assert_eq!(
        t.annotations.len(),
        2,
        "a widget listed twice is reported once"
    );
    let (a, b) = (
        t.annotations[0].field.as_ref().unwrap(),
        t.annotations[1].field.as_ref().unwrap(),
    );
    assert_eq!(
        (a.name.as_str(), b.name.as_str()),
        ("form.addr", "form.addr")
    );
    assert_eq!((a.first_widget, b.first_widget), (true, false));
    assert_eq!(a.value, vec!["1 Main St"]);
    assert!(b.value.is_empty());
    let texts: Vec<_> = t.annotations.iter().filter_map(|a| a.text()).collect();
    assert_eq!(texts, vec!["1 Main St"], "the value is counted once");
}

#[test]
fn a_widget_without_appearance_gets_the_synthesised_one() {
    let p = pdf(
        "10 0 R",
        vec![widget(
            10,
            "/FT /Tx /T (name) /V (Typed) /DA (/Helv 10 Tf 0 g)",
            None,
        )],
        "/AcroForm << /Fields [10 0 R] /DR << /Font << /Helv 5 0 R >> >> /DA (/Helv 10 Tf 0 g) >>",
    );
    let a = &extract(&p, true).unwrap().annotations[0];
    assert_eq!(chars(&a.appearance), "Typed");
    assert_eq!(a.text().as_deref(), Some("Typed"));
}

#[test]
fn hidden_annotations_report_values_but_no_text() {
    let p = pdf(
        "10 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /FreeText /F 2 /Rect [10 10 110 30] /Contents (Secret) /AP << /N 11 0 R >> >>".to_vec()),
            (11, ap("Secret")),
        ],
        "",
    );
    let a = &extract(&p, true).unwrap().annotations[0];
    assert!(a.hidden && a.appearance.is_empty());
    assert_eq!(a.contents.as_deref(), Some("Secret"));
    assert_eq!(a.text(), None);
}

#[test]
fn broken_references_and_cycles() {
    // An undefined /Parent ends the name chain.
    let p = pdf(
        "10 0 R",
        vec![
            widget(10, "/FT /Tx /T (leaf) /V (v) /Parent 99 0 R", Some(11)),
            (11, ap("v")),
        ],
        "",
    );
    assert_eq!(
        extract(&p, true).unwrap().annotations[0]
            .field
            .as_ref()
            .unwrap()
            .name,
        "leaf"
    );
    // A /Parent cycle is refused, not followed forever.
    let p = pdf(
        "10 0 R",
        vec![
            widget(10, "/FT /Tx /T (a) /V (v) /Parent 12 0 R", Some(11)),
            (11, ap("v")),
            (12, b"<< /T (b) /Parent 13 0 R >>".to_vec()),
            (13, b"<< /T (c) /Parent 12 0 R >>".to_vec()),
        ],
        "",
    );
    assert!(extract(&p, true).is_err());
    // …and only with the option: page text is unaffected.
    assert_eq!(chars(&extract(&p, false).unwrap().chars), "Body");
}

#[test]
fn many_fields_and_the_text_budget() {
    let n = 2000u32;
    let mut objs = vec![(11, ap("x"))];
    let mut refs = String::new();
    for i in 0..n {
        objs.push(widget(
            100 + i,
            &format!("/FT /Tx /T (f{i}) /V (value {i})"),
            Some(11),
        ));
        refs.push_str(&format!("{} 0 R ", 100 + i));
    }
    let p = pdf(&refs, objs, "");
    let t = extract(&p, true).unwrap();
    assert_eq!(t.annotations.len(), n as usize);
    assert_eq!(t.annotations[1999].text().as_deref(), Some("value 1999"));
    // The page's Unicode budget covers annotation text as well.
    let long = "y".repeat(5000);
    let p = pdf(
        "10 0 R",
        vec![(10, format!("<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents ({long}) /AP << /N 11 0 R >> >>").into_bytes()), (11, ap("x"))],
        "",
    );
    let doc = Document::from_bytes(&p).unwrap();
    let opts = TextOptions::new()
        .with_annotations(true)
        .with_limits(Limits::default().with_max_extracted_unicode_bytes(1000));
    match extract_text(&doc, &doc.pages()[0], &opts) {
        Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, "max_extracted_unicode_bytes"),
        other => panic!(
            "expected the text budget, got {:?}",
            other.map(|t| t.annotations.len())
        ),
    }
}

#[test]
fn a_note_without_appearance_reports_its_contents() {
    // A /Text note without /AP: no appearance to read (rendering still
    // refuses the page), its /Contents is its text.
    let p = pdf(
        "10 0 R",
        vec![(
            10,
            b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (Remember) >>".to_vec(),
        )],
        "",
    );
    let a = &extract(&p, true).unwrap().annotations[0];
    assert!(a.appearance.is_empty());
    assert_eq!(a.text().as_deref(), Some("Remember"));
    let doc = Document::from_bytes(&p).unwrap();
    assert!(
        kasane_pdf::render_page(&doc, &doc.pages()[0], &kasane_pdf::RenderOptions::new()).is_err()
    );
}
