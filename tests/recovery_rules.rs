//! Strict mode versus recovery mode on malformed input: what §7.3.10
//! makes exact in both (undefined references), what is closed with a
//! warning in both (open blocks at the end of a content stream), and
//! what only `Document::from_bytes_recovering` reinterprets, each time
//! recorded as `Warning::RecoveryApplied`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    render_page, Document, Error, RecoveryRule, RenderOptions, RenderedPage, Warning,
};

fn page(content: &[u8], page_extra: &str, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> \
                 /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(extra.iter().cloned());
    build_pdf(&objs)
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let i = y as usize * r.stride() + x as usize * 4;
    let p = &r.pixels()[i..i + 4];
    (p[0], p[1], p[2])
}

fn render(doc: &Document) -> Result<RenderedPage, Error> {
    render_page(doc, &doc.pages()[0], &RenderOptions::default())
}

fn recovery_rules(doc: &Document) -> Vec<RecoveryRule> {
    doc.take_warnings()
        .into_iter()
        .filter_map(|w| match w {
            Warning::RecoveryApplied { rule, .. } => Some(rule),
            _ => None,
        })
        .collect()
}

#[test]
fn undefined_reference_is_null_in_strict_mode_and_recorded_once() {
    // /Annots [9 0 R 9 0 R]: object 9 does not exist. §7.3.10: null,
    // i.e. no annotation — the page renders; the reference is recorded
    // once however often it is resolved.
    let pdf = page(b"0 0 1 rg 0 0 100 100 re f", "/Annots [9 0 R 9 0 R]", &[]);
    let doc = Document::from_bytes(&pdf).unwrap();
    assert!(!doc.recovery_mode());
    let r = render(&doc).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 255));
    let w = doc.take_warnings();
    assert_eq!(
        w.iter()
            .filter(
                |w| matches!(w, Warning::UndefinedReference { reference } if reference.number == 9)
            )
            .count(),
        1,
        "{w:?}"
    );
}

#[test]
fn reference_to_a_freed_object_is_undefined() {
    // The same object number at a different generation is a different
    // object: (5, 1) is undefined when only (5, 0) exists.
    let pdf = page(
        b"",
        "/Annots [5 1 R]",
        &[(
            5,
            b"<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] >>".to_vec(),
        )],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    render(&doc).unwrap();
    assert!(doc.take_warnings().iter().any(
        |w| matches!(w, Warning::UndefinedReference { reference } if reference.generation == 1)
    ));
}

#[test]
fn stray_restore_is_an_error_in_strict_mode_and_ignored_in_recovery() {
    let pdf = page(b"Q 1 0 0 rg 0 0 100 100 re f", "", &[]);
    let strict = Document::from_bytes(&pdf).unwrap();
    let e = strict.page_operations(&strict.pages()[0]).unwrap_err();
    assert!(matches!(e, Error::Structure(_)), "{e:?}");
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    assert!(rec.recovery_mode());
    assert!(
        rec.recovery().is_none(),
        "the xref was fine; only the content rules apply"
    );
    let r = render(&rec).unwrap();
    assert_eq!(px(&r, 50, 50), (255, 0, 0));
    assert_eq!(
        recovery_rules(&rec),
        vec![RecoveryRule::IgnoredRestoreWithoutSave]
    );
}

#[test]
fn unknown_operator_is_an_error_in_strict_mode_and_dropped_in_recovery() {
    let pdf = page(b"1 2 frobnicate 0 1 0 rg 0 0 100 100 re f", "", &[]);
    let strict = Document::from_bytes(&pdf).unwrap();
    assert!(matches!(
        strict.page_operations(&strict.pages()[0]),
        Err(Error::Parse(_))
    ));
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 255, 0));
    assert_eq!(
        recovery_rules(&rec),
        vec![RecoveryRule::IgnoredUnknownOperator]
    );
}

#[test]
fn operator_inside_a_path_object_is_applied_only_in_recovery() {
    // `rg` between `re` and `f`: §8.2 Figure 9 forbids it; every major
    // reader applies it before painting. Strict refuses; recovery
    // paints the rectangle blue and records the rule.
    let pdf = page(b"1 0 0 rg 0 0 100 100 re 0 0 1 rg f", "", &[]);
    let strict = Document::from_bytes(&pdf).unwrap();
    let e = render(&strict).unwrap_err();
    assert!(
        matches!(e, Error::Structure(ref m) if m.contains("path object")),
        "{e:?}"
    );
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 255));
    assert_eq!(
        recovery_rules(&rec),
        vec![RecoveryRule::OperatorInsidePathObject]
    );
}

#[test]
fn missing_mediabox_is_refused_strictly_and_defaulted_in_recovery() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("", b"0 0 1 rg 0 0 612 792 re f")),
    ]);
    let e = Document::from_bytes(&pdf).unwrap_err();
    assert!(
        matches!(e, Error::Structure(ref m) if m.contains("MediaBox")),
        "{e:?}"
    );
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let mb = rec.pages()[0].media_box();
    assert_eq!((mb.urx, mb.ury), (612.0, 792.0));
    assert_eq!(recovery_rules(&rec), vec![RecoveryRule::MediaBoxDefault]);
    let r = render(&rec).unwrap();
    assert_eq!((r.width(), r.height()), (612, 792));
}

#[test]
fn invalid_annotation_entries_are_skipped_only_in_recovery() {
    let pdf = page(b"0 0 1 rg 0 0 100 100 re f", "/Annots [42]", &[]);
    let strict = Document::from_bytes(&pdf).unwrap();
    assert!(matches!(render(&strict), Err(Error::Structure(_))));
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 255));
    assert_eq!(
        recovery_rules(&rec),
        vec![RecoveryRule::IgnoredInvalidAnnotation]
    );
}

#[test]
fn unknown_rendering_intent_is_a_warning_in_both_modes() {
    let pdf = page(b"/Custom ri 0 0 1 rg 0 0 100 100 re f", "", &[]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render(&doc).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 255));
    let w = doc.take_warnings();
    assert!(
        matches!(&w[..], [Warning::UnknownRenderingIntent { name }] if name == "Custom"),
        "{w:?}"
    );
}

#[test]
fn recovery_mode_adds_nothing_to_a_conforming_file() {
    let pdf = page(b"q 0 0 1 rg 0 0 100 100 re f Q", "", &[]);
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    render(&rec).unwrap();
    assert!(rec.take_warnings().is_empty());
    assert!(rec.recovery().is_none());
}

#[test]
fn degenerate_page_boxes_are_replaced_only_in_recovery() {
    // A zero-area /MediaBox, a zero-area /CropBox and a /CropBox outside
    // the media box: recovery mode falls back to the media box or US
    // Letter and records each decision; strict mode does not.
    for (boxes, expect, strict_renders) in [
        ("/MediaBox [0 0 0 0]", (612, 792), false),
        (
            "/MediaBox [0 0 300 200] /CropBox [0 0 0 0]",
            (300, 200),
            false,
        ),
        // Strict mode draws the crop box as written (a 100 × 100 page
        // showing nothing of the media box); recovery falls back.
        (
            "/MediaBox [0 0 300 200] /CropBox [400 400 500 500]",
            (300, 200),
            true,
        ),
    ] {
        let pdf = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!("<< /Type /Page /Parent 2 0 R {boxes} /Contents 4 0 R >>").into_bytes(),
            ),
            (4, stream_obj("", b"0 0 1 rg 0 0 50 50 re f")),
        ]);
        let strict = Document::from_bytes(&pdf).unwrap();
        assert_eq!(render(&strict).is_ok(), strict_renders, "{boxes}");
        let rec = Document::from_bytes_recovering(&pdf).unwrap();
        let r = render(&rec).unwrap();
        assert_eq!((r.width(), r.height()), expect, "{boxes}");
        assert_eq!(
            recovery_rules(&rec),
            vec![RecoveryRule::MediaBoxDefault],
            "{boxes}"
        );
    }
}
