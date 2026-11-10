//! `redact` with `remove_annotations` on form fields (0.9): a widget
//! meeting a rect goes, and its field with it when no widget of the
//! field is left — the value, its description and its place in `/CO`
//! too. A field keeping a widget elsewhere keeps its value (shown there,
//! reported). XFA forms, signature fields and trees that cannot be
//! edited exactly are refused.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{redact, Document, RedactOptions};

const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);

fn doc(form: &str, annots: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R /AcroForm {form} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Annots [{annots}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"0 0 10 10 re f")),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

fn ap(v: &str) -> Vec<u8> {
    stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 40 10]",
        format!("% {v}\n0 0 40 10 re f").as_bytes(),
    )
}

fn run(pdf: &[u8], annotations: bool) -> Result<kasane_pdf::Redaction, kasane_pdf::Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_annotations(annotations),
    )
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// A field that is its own widget, inside the rect: the field leaves
/// `/Fields` and `/CO` with its value; two values redact alike.
#[test]
fn a_merged_field_widget_goes_with_its_value() {
    let make = |v: &str| {
        doc(
            "<< /Fields [10 0 R 12 0 R] /CO [10 0 R 12 0 R] >>",
            "10 0 R 12 0 R",
            vec![
                (
                    10,
                    format!(
                        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (secret) /V ({v}) /TU (tip {v}) \
                         /Rect [70 90 110 100] /AP << /N 11 0 R >> >>"
                    )
                    .into_bytes(),
                ),
                (11, ap(v)),
                (
                    12,
                    b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (kept) /V (PUBLICVALUE) \
                      /Rect [10 180 50 190] >>"
                        .to_vec(),
                ),
            ],
        )
    };
    let a = run(&make("SECRETVALUE1"), true).unwrap();
    let b = run(&make("SECRETVALUE2"), true).unwrap();
    assert!(
        a.output == b.output,
        "the removed field's value reached the output"
    );
    assert_eq!((a.annotations_removed, a.fields_removed), (1, 1));
    assert!(!contains(&a.output, b"SECRETVALUE"));
    assert_eq!(values(&a.output), vec!["PUBLICVALUE".to_string()]);
    let out = Document::from_bytes(&a.output).unwrap();
    let form = out
        .deref(out.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let form = form.as_dict().unwrap();
    for k in [b"Fields".as_slice(), b"CO"] {
        let l = out.deref(form.get(k).unwrap()).unwrap();
        assert_eq!(
            l.as_array().unwrap().len(),
            1,
            "/{}",
            String::from_utf8_lossy(k)
        );
    }
    // Without remove_annotations: refused, as before.
    let e = run(&make("SECRETVALUE1"), false).unwrap_err().to_string();
    assert!(e.contains("remove_annotations"), "{e}");
}

/// A field with two widgets, one inside the rect: the widget goes, the
/// field and its value stay (the other widget shows it) and the value
/// is reported elsewhere.
#[test]
fn a_field_keeping_a_widget_keeps_its_value() {
    let pdf = doc(
        "<< /Fields [10 0 R] >>",
        "11 0 R 12 0 R",
        vec![
            (
                10,
                b"<< /FT /Tx /T (name) /V (SHOWNVALUE) /Kids [11 0 R 12 0 R] >>".to_vec(),
            ),
            (
                11,
                b"<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [70 90 110 100] >>"
                    .to_vec(),
            ),
            (
                12,
                b"<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [10 180 50 190] >>"
                    .to_vec(),
            ),
        ],
    );
    let r = run(&pdf, true).unwrap();
    assert_eq!((r.annotations_removed, r.fields_removed), (1, 0));
    assert_eq!(values(&r.output), vec!["SHOWNVALUE".to_string()]);
    assert!(
        r.elsewhere
            .iter()
            .any(|e| matches!(&e.removed, kasane_pdf::Removed::Text(t) if t == "SHOWNVALUE")),
        "{:?}",
        r.elsewhere
    );
    let out = Document::from_bytes(&r.output).unwrap();
    let form = out
        .deref(out.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let fields = out
        .deref(form.as_dict().unwrap().get(b"Fields".as_slice()).unwrap())
        .unwrap();
    let field = out.deref(&fields.as_array().unwrap()[0]).unwrap();
    let kids = out
        .deref(field.as_dict().unwrap().get(b"Kids".as_slice()).unwrap())
        .unwrap();
    assert_eq!(kids.as_array().unwrap().len(), 1);
    // Both widgets inside: the field goes.
    let pdf = doc(
        "<< /Fields [10 0 R] >>",
        "11 0 R 12 0 R",
        vec![
            (
                10,
                b"<< /FT /Tx /T (name) /V (GONEVALUE) /Kids [11 0 R 12 0 R] >>".to_vec(),
            ),
            (
                11,
                b"<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [70 90 110 100] >>"
                    .to_vec(),
            ),
            (
                12,
                b"<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [70 110 110 120] >>"
                    .to_vec(),
            ),
        ],
    );
    let r = run(&pdf, true).unwrap();
    assert_eq!((r.annotations_removed, r.fields_removed), (2, 1));
    assert!(!contains(&r.output, b"GONEVALUE"));
    assert!(values(&r.output).is_empty());
}

/// Refused: an XFA form, a signature field (the type inherited), a
/// widget whose /Parent is not the field above it, a direct widget.
#[test]
fn forms_that_cannot_be_edited_exactly() {
    let widget = |extra: &str| {
        format!("<< /Type /Annot /Subtype /Widget /Rect [70 90 110 100] {extra} >>").into_bytes()
    };
    for (name, form, annots, extra) in [
        (
            "xfa",
            "<< /Fields [10 0 R] /XFA 11 0 R >>",
            "10 0 R",
            vec![
                (10, widget("/FT /Tx /T (a) /V (x)")),
                (11, stream_obj("", b"<xdp/>")),
            ],
        ),
        (
            "signature",
            "<< /Fields [10 0 R] >>",
            "11 0 R",
            vec![
                (10, b"<< /FT /Sig /T (s) /Kids [11 0 R] >>".to_vec()),
                (11, widget("/Parent 10 0 R")),
            ],
        ),
        (
            "parent",
            "<< /Fields [10 0 R] >>",
            "11 0 R",
            vec![
                (10, b"<< /FT /Tx /T (s) /Kids [11 0 R] >>".to_vec()),
                (11, widget("/Parent 12 0 R")),
                (12, b"<< /FT /Tx /T (o) >>".to_vec()),
            ],
        ),
        (
            "unreached",
            "<< /Fields [] >>",
            "11 0 R",
            vec![(11, widget("/FT /Tx /T (o) /V (x)"))],
        ),
    ] {
        let pdf = doc(form, annots, extra);
        assert!(run(&pdf, true).is_err(), "{name}: accepted");
    }
    let pdf = doc(
        "<< /Fields [] >>",
        "<< /Type /Annot /Subtype /Widget /Rect [70 90 110 100] >>",
        vec![],
    );
    let e = run(&pdf, true).unwrap_err().to_string();
    assert!(e.contains("direct widget"), "{e}");
}

/// Every string value of `/V` in the output's objects (decoded).
fn values(out: &[u8]) -> Vec<String> {
    let doc = Document::from_bytes(out).unwrap();
    let mut v = Vec::new();
    let form = doc
        .deref(doc.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let fields = doc
        .deref(form.as_dict().unwrap().get(b"Fields".as_slice()).unwrap())
        .unwrap();
    let mut stack: Vec<kasane_pdf::Object> = fields.as_array().unwrap().to_vec();
    while let Some(f) = stack.pop() {
        let f = doc.deref(&f).unwrap();
        let d = f.as_dict().unwrap();
        if let Some(kasane_pdf::Object::String(s)) = d.get(b"V".as_slice()) {
            v.push(String::from_utf8_lossy(s).into_owned());
        }
        if let Some(k) = d.get(b"Kids".as_slice()) {
            stack.extend(doc.deref(k).unwrap().as_array().unwrap().iter().cloned());
        }
    }
    v
}

/// A removed widget, field or markup annotation that something kept still
/// refers to is refused — also when `remove_javascript` edits it (its
/// content then no longer matches what was removed). (Review.)
#[test]
fn removed_objects_still_referred_to_are_refused() {
    let js = "/A << /S /JavaScript /JS (app.alert(1)) >>";
    let field = |extra: &str| {
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (s) /V (SECRETVALUE) \
             /Rect [70 90 110 100] {extra} >>"
        )
        .into_bytes()
    };
    let markup = format!(
        "<< /Type /Annot /Subtype /FreeText /Rect [70 90 110 100] /DA (/Helv 9 Tf) \
         /Contents (SECRETNOTE) {js} >>"
    );
    for (name, cat_extra, obj) in [
        ("field", "/XPriv 10 0 R", field("")),
        ("field-js", "/XPriv 10 0 R", field(js)),
        ("markup-js", "/XPriv 10 0 R", markup.into_bytes()),
    ] {
        let pdf = build_pdf(&[
            (
                1,
                format!(
                    "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] >> {cat_extra} >>"
                )
                .into_bytes(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                  /Annots [10 0 R] >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"0 0 10 10 re f")),
            (10, obj),
        ]);
        let doc = Document::from_bytes(&pdf).unwrap();
        for javascript in [false, true] {
            let r = redact(
                &doc,
                &[(0, vec![RECT])],
                &RedactOptions::new()
                    .remove_annotations(true)
                    .remove_javascript(javascript),
            );
            match r {
                Ok(r) => panic!(
                    "{name} (javascript {javascript}): accepted, secret kept: {}",
                    contains(&r.output, b"SECRET")
                ),
                Err(e) => assert!(e.to_string().contains("failed verification"), "{name}: {e}"),
            }
        }
    }
}

/// A field keeping widgets: refused when no kept widget is shown (on no
/// page, or Hidden) or when its button value is a removed widget's
/// state; a choice's array value is reported in `elsewhere`. (Review.)
#[test]
fn kept_fields_show_what_they_keep() {
    let two = |field: &str, kept: &str, gone: &str, annots: &str| {
        doc(
            "<< /Fields [10 0 R] >>",
            annots,
            vec![
                (10, format!("<< {field} /Kids [11 0 R 12 0 R] >>").into_bytes()),
                (
                    11,
                    format!("<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [70 90 110 100] {gone} >>")
                        .into_bytes(),
                ),
                (
                    12,
                    format!("<< /Type /Annot /Subtype /Widget /Parent 10 0 R /Rect [10 180 50 190] {kept} >>")
                        .into_bytes(),
                ),
            ],
        )
    };
    let radio = "/FT /Btn /Ff 49152 /T (r) /V /Secretopt";
    for (name, pdf) in [
        (
            "radio",
            two(radio, "/AS /Off", "/AS /Secretopt", "11 0 R 12 0 R"),
        ),
        (
            "orphan",
            two("/FT /Tx /T (t) /V (SECRETVALUE)", "", "", "11 0 R"),
        ),
        (
            "hidden",
            two(
                "/FT /Tx /T (t) /V (SECRETVALUE)",
                "/F 2",
                "",
                "11 0 R 12 0 R",
            ),
        ),
    ] {
        let e = run(&pdf, true).unwrap_err().to_string();
        assert!(e.contains("cannot be edited exactly"), "{name}: {e}");
    }
    // The kept widget shows the radio's state: accepted.
    assert!(run(
        &two(radio, "/AS /Secretopt", "/AS /Off", "11 0 R 12 0 R"),
        true
    )
    .is_ok());
    let choice = two(
        "/FT /Ch /Ff 2097152 /T (c) /V [(CHOSENONE) (CHOSENTWO)] /Opt [(CHOSENONE) (CHOSENTWO)]",
        "",
        "",
        "11 0 R 12 0 R",
    );
    let r = run(&choice, true).unwrap();
    assert!(
        r.elsewhere
            .iter()
            .any(|e| matches!(&e.removed, kasane_pdf::Removed::Text(t) if t == "CHOSENONE")),
        "{:?}",
        r.elsewhere
    );
}
