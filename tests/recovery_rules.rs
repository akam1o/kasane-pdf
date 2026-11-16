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

/// The rule recorded for an operator with the wrong number of operands
/// (R1).
fn wrong_count(rules: &[RecoveryRule]) -> usize {
    rules
        .iter()
        .filter(|r| **r == RecoveryRule::IgnoredWrongOperandCount)
        .count()
}

/// R1: a fixed-arity operator that does not open or close a block,
/// given too few or too many operands, is an error in strict mode. In
/// recovery mode it is kept, uninterpreted, as `Operation::Unknown`
/// with all its operands — Poppler uses the last operands and MuPDF the
/// first (or zeros for missing ones), so no reading is shared — and the
/// renderer skips it.
#[test]
fn wrong_operand_count_is_kept_uninterpreted_only_in_recovery() {
    for (bad, op, n) in [
        ("0 0 10 re", "re", 3),
        ("1 2 3 m", "m", 3),
        ("2 0 0 2 0 cm", "cm", 5),
        ("9 1 0 0 1 0 0 cm", "cm", 7),
        ("1 f", "f", 1),
        ("0 0 rg", "rg", 2),
        ("1 1 1 1 1 k", "k", 5),
        ("/GS1 /GS2 gs", "gs", 2),
        ("[3] d", "d", 1),
        ("1 2 w", "w", 2),
        ("1 W n", "W", 1),
        ("BT 12 Tf ET", "Tf", 1),
        ("BT 7 60 100 Td ET", "Td", 3),
        ("BT 1 (x) Tj ET", "Tj", 2),
        ("BT 1 (x) ' ET", "'", 2),
        ("BT 1 1 1 1 0 0 0 Tm ET", "Tm", 7),
        ("Do", "Do", 0),
        ("/A /B sh", "sh", 2),
        ("MP", "MP", 0),
        ("/T DP", "DP", 1),
        ("1 2 3 d0", "d0", 3),
    ] {
        let pdf = page(
            format!("{bad} 1 0 0 rg 0 0 100 100 re f").as_bytes(),
            "",
            &[],
        );
        let strict = Document::from_bytes(&pdf).unwrap();
        let e = strict.page_operations(&strict.pages()[0]).unwrap_err();
        assert!(
            e.to_string().contains("expects") && e.to_string().contains(&format!("found {n}")),
            "{bad}: {e}"
        );
        let rec = Document::from_bytes_recovering(&pdf).unwrap();
        let ops = rec
            .page_operations(&rec.pages()[0])
            .unwrap_or_else(|e| panic!("{bad}: {e}"));
        let kept: Vec<_> = ops
            .iter()
            .filter_map(|o| match o {
                kasane_pdf::Operation::Unknown { operator, operands } => {
                    Some((operator.clone(), operands.len()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            kept,
            vec![(op.as_bytes().to_vec(), n)],
            "{bad}: kept uninterpreted with its operands"
        );
        let _ = rec.take_warnings();
        let r = render(&rec).unwrap_or_else(|e| panic!("{bad}: {e}"));
        assert_eq!(px(&r, 50, 50), (255, 0, 0), "{bad}: the rest is drawn");
        assert_eq!(
            wrong_count(&recovery_rules(&rec)),
            1,
            "{bad}: recorded once"
        );
    }
}

/// Operators that open or close a block (`q Q BT ET BMC BDC EMC BX EX`)
/// are not recovered: dropping one would change the nesting of all
/// that follows. Neither are operands of the wrong type, or operands
/// left before an inline image or at the end of the stream.
#[test]
fn what_the_operand_count_rule_does_not_recover() {
    for bad in [
        "1 q 0 0 1 1 re f Q",
        "q 1 Q",
        "1 BT ET",
        "BT 1 ET",
        "1 /T BMC EMC",
        "/T BMC 1 EMC",
        "/T /P <<>> BDC EMC",
        "1 BX EX",
        "(a) 2 m",
        "/N 1 2 3 re",
        "BT /F1 (x) Tf ET",
        "1 2",
        "1 BI /W 1 /H 1 /BPC 8 /CS /G ID \u{0} EI",
    ] {
        let pdf = page(bad.as_bytes(), "", &[]);
        for recovering in [false, true] {
            let doc = if recovering {
                Document::from_bytes_recovering(&pdf)
            } else {
                Document::from_bytes(&pdf)
            }
            .unwrap();
            assert!(
                doc.page_operations(&doc.pages()[0]).is_err(),
                "{bad} (recovery {recovering}): parsed"
            );
            assert_eq!(wrong_count(&recovery_rules(&doc)), 0, "{bad}");
        }
    }
}

/// Inside `BX … EX` the same: strict mode still counts operands of a
/// known operator; recovery keeps the wrong one uninterpreted.
#[test]
fn wrong_operand_count_inside_a_compatibility_section() {
    let pdf = page(b"BX 0 0 10 re EX 1 0 0 rg 0 0 100 100 re f", "", &[]);
    let strict = Document::from_bytes(&pdf).unwrap();
    assert!(strict.page_operations(&strict.pages()[0]).is_err());
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert_eq!(px(&r, 50, 50), (255, 0, 0));
    assert_eq!(wrong_count(&recovery_rules(&rec)), 1);
}

/// A page whose resources name form 10 `/Fm` (and 11 `/In`).
fn form_page(content: &[u8], forms: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /XObject << /Fm 10 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(forms.iter().cloned());
    build_pdf(&objs)
}

fn unclipped(rules: &[RecoveryRule]) -> usize {
    rules
        .iter()
        .filter(|r| format!("{r:?}") == "UnclippedFormWithoutBBox")
        .count()
}

/// R2: a form XObject without the required `/BBox` (Table 93) is an
/// error in strict mode. In recovery mode it is drawn unclipped (as
/// MuPDF does; Poppler draws nothing), recorded; a `/BBox` that is
/// present but not a rectangle stays an error in both modes.
#[test]
fn form_without_bbox_is_drawn_unclipped_only_in_recovery() {
    // The form paints the whole page blue; with any /BBox it would be
    // clipped to it. /Matrix still applies: the second form is moved
    // half a page right and paints only there.
    let whole = "/Type /XObject /Subtype /Form";
    for (name, dict, body, at, want) in [
        (
            "absent",
            whole.to_string(),
            "0 0 1 rg -1000 -1000 3000 3000 re f",
            (10, 50),
            (0, 0, 255),
        ),
        (
            "null",
            format!("{whole} /BBox null"),
            "0 0 1 rg -1000 -1000 3000 3000 re f",
            (10, 50),
            (0, 0, 255),
        ),
        (
            "with /Matrix",
            format!("{whole} /Matrix [1 0 0 1 50 0]"),
            "0 0 1 rg 0 0 50 100 re f",
            (75, 50),
            (0, 0, 255),
        ),
        (
            "with /Matrix, left half",
            format!("{whole} /Matrix [1 0 0 1 50 0]"),
            "0 0 1 rg 0 0 50 100 re f",
            (25, 50),
            (255, 255, 255),
        ),
    ] {
        let pdf = form_page(b"/Fm Do", &[(10, stream_obj(&dict, body.as_bytes()))]);
        let strict = Document::from_bytes(&pdf).unwrap();
        let e = render(&strict).unwrap_err();
        assert!(e.to_string().contains("no /BBox"), "{name}: {e}");
        let rec = Document::from_bytes_recovering(&pdf).unwrap();
        let r = render(&rec).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(px(&r, at.0, at.1), want, "{name}");
        assert_eq!(unclipped(&recovery_rules(&rec)), 1, "{name}: recorded once");
    }
    // Present but not a rectangle: an error in both modes (MuPDF does
    // not draw it either).
    for bad in ["[0 0 50]", "[0 0 (a) 50]", "/Box", "[0 0 1e400 50]"] {
        let pdf = form_page(
            b"/Fm Do",
            &[(
                10,
                stream_obj(
                    &format!("{whole} /BBox {bad}"),
                    b"0 0 1 rg 0 0 100 100 re f",
                ),
            )],
        );
        for recovering in [false, true] {
            let d = if recovering {
                Document::from_bytes_recovering(&pdf)
            } else {
                Document::from_bytes(&pdf)
            }
            .unwrap();
            assert!(
                render(&d).is_err(),
                "{bad} (recovery {recovering}) rendered"
            );
            assert_eq!(unclipped(&recovery_rules(&d)), 0, "{bad}");
        }
    }
    // Nested: a form without /BBox inside one with a /BBox is clipped
    // by the outer one.
    let pdf = form_page(
        b"/Fm Do",
        &[
            (
                10,
                stream_obj(
                    &format!(
                        "{whole} /BBox [0 0 50 100] /Resources << /XObject << /In 11 0 R >> >>"
                    ),
                    b"/In Do",
                ),
            ),
            (
                11,
                stream_obj(whole, b"0 0 1 rg -1000 -1000 3000 3000 re f"),
            ),
        ],
    );
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert_eq!(
        (px(&r, 25, 50), px(&r, 75, 50)),
        ((0, 0, 255), (255, 255, 255))
    );
    assert_eq!(unclipped(&recovery_rules(&rec)), 1);
}

/// A page painting shading 10 (`/Sh`) over the whole page.
fn shading_page(shading: &str, more: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /Shading << /Sh 10 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"/Sh sh")),
        (10, shading.as_bytes().to_vec()),
    ];
    objs.extend(more.iter().cloned());
    build_pdf(&objs)
}

fn rule_count(rules: &[RecoveryRule], name: &str) -> usize {
    rules.iter().filter(|r| format!("{r:?}") == name).count()
}

/// R3: a type 2 or 3 function takes one input (§7.10.3, §7.10.4); one
/// whose /Domain gives more is an error in strict mode. In recovery
/// mode the first input is used (as MuPDF does; Poppler draws nothing),
/// recorded.
#[test]
fn function_with_extra_inputs_uses_the_first_only_in_recovery() {
    let red_to_blue = "/C0 [1 0 0] /C1 [0 0 1] /N 1";
    for (name, func) in [
        ("type 2", format!("<< /FunctionType 2 /Domain [0 1 0 1] {red_to_blue} >>")),
        (
            "type 3",
            format!("<< /FunctionType 3 /Domain [0 1 0 1] /Functions [<< /FunctionType 2 /Domain [0 1] {red_to_blue} >>] /Bounds [] /Encode [0 1] >>"),
        ),
    ] {
        let pdf = shading_page(
            &format!("<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function {func} /Extend [true true] >>"),
            &[],
        );
        let strict = Document::from_bytes(&pdf).unwrap();
        let e = render(&strict).unwrap_err();
        assert!(e.to_string().contains("takes exactly one input"), "{name}: {e}");
        let rec = Document::from_bytes_recovering(&pdf).unwrap();
        let r = render(&rec).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (left, right) = (px(&r, 2, 50), px(&r, 97, 50));
        assert!(left.0 > 200 && left.2 < 40 && right.2 > 200 && right.0 < 40, "{name}: {left:?} {right:?}");
        assert_eq!(rule_count(&recovery_rules(&rec), "FunctionExtraInputsIgnored"), 1, "{name}");
    }
}

/// R3: a shading whose function yields more outputs than its colour
/// space has components is an error in strict mode; recovery mode uses
/// the first ones (as MuPDF does). Fewer outputs stay an error in both
/// modes: no value is defined for the missing components (MuPDF fills
/// them with each function type's own defaults).
#[test]
fn shading_with_extra_outputs_is_truncated_only_in_recovery() {
    let sh = |cs: &str, c0: &str, c1: &str| {
        shading_page(
            &format!("<< /ShadingType 2 /ColorSpace /{cs} /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 {c0} /C1 {c1} /N 1 >> /Extend [true true] >>"),
            &[],
        )
    };
    // Gray from an RGB-sized function: the first output, 1 → 0.
    let pdf = sh("DeviceGray", "[1 0 0]", "[0 0 1]");
    let strict = Document::from_bytes(&pdf).unwrap();
    assert!(render(&strict)
        .unwrap_err()
        .to_string()
        .contains("outputs, colour space has 1"));
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap();
    assert!(
        px(&r, 2, 50).0 > 240 && px(&r, 97, 50).0 < 15,
        "{:?} {:?}",
        px(&r, 2, 50),
        px(&r, 97, 50)
    );
    assert_eq!(
        rule_count(&recovery_rules(&rec), "ShadingOutputsTruncated"),
        1
    );
    // Fewer outputs than components: an error in both modes.
    for (cs, c0, c1) in [
        ("DeviceRGB", "[1]", "[0]"),
        ("DeviceCMYK", "[1 0]", "[0 1]"),
    ] {
        let pdf = sh(cs, c0, c1);
        for recovering in [false, true] {
            let d = if recovering {
                Document::from_bytes_recovering(&pdf)
            } else {
                Document::from_bytes(&pdf)
            }
            .unwrap();
            assert!(
                render(&d).is_err(),
                "{cs} {c0} (recovery {recovering}) rendered"
            );
            assert_eq!(
                rule_count(&recovery_rules(&d), "ShadingOutputsTruncated"),
                0
            );
        }
    }
}

/// R3: an array of one-output functions longer than the colour space's
/// components keeps the first members, and the rest are never
/// evaluated. The third member here fails when evaluated (it pops more
/// than its one input). (Review: evaluating every member made the
/// array an amplifier.)
#[test]
fn extra_functions_of_an_array_are_not_evaluated() {
    let one = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>";
    let fails = "11 0 R";
    let pdf = shading_page(
        &format!(
            "<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Function [{one} {one} {fails}] \
             /Extend [true true] >>"
        ),
        &[(11, stream_obj("/FunctionType 4 /Domain [0 1] /Range [0 1]", b"{ pop pop 0 }"))],
    );
    let strict = Document::from_bytes(&pdf).unwrap();
    assert!(render(&strict).is_err());
    let rec = Document::from_bytes_recovering(&pdf).unwrap();
    let r = render(&rec).unwrap_or_else(|e| panic!("{e}"));
    assert!(px(&r, 2, 50).0 > 240 && px(&r, 97, 50).0 < 15);
    assert_eq!(
        rule_count(&recovery_rules(&rec), "ShadingOutputsTruncated"),
        1
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
