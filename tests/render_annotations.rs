//! Annotation appearance streams (§12.5.5): `/Annots` → `/AP` → `/N`,
//! `/AS` state selection, `/Rect` fitting with `/BBox` + `/Matrix`,
//! painting order, flags, and the refusals for appearances we would
//! have to invent.

use kasane_pdf::{extract_text, render_page, Document, Error, Limits, RenderOptions};

/// Assemble a PDF from `(number, body)` pairs; object 1 is the catalog.
fn pdf_objs(objs: &[(u32, String)]) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objs {
        offs.push((*n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.as_bytes().to_vec());
        parts.push(b"\nendobj\n".to_vec());
    }
    let size = objs.iter().map(|(n, _)| *n).max().unwrap() + 1;
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for n in 1..size {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").into_bytes());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

/// A 200×200 page whose content is `content`, with `/Annots` `annots`
/// (object refs) and extra objects.
fn page_pdf(content: &str, annots: &str, extra: &[(u32, String)]) -> Vec<u8> {
    let mut objs = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Resources << >> /Annots [{annots}] >>"
            ),
        ),
        (
            4,
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ),
    ];
    objs.extend(extra.iter().cloned());
    pdf_objs(&objs)
}

fn stream(dict: &str, body: &str) -> String {
    format!(
        "<< {dict} /Length {} >>\nstream\n{body}\nendstream",
        body.len()
    )
}

fn render(pdf: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

/// RGB at pixmap (x, y) — y grows downward; page y = 200 − y.
fn px(page: &kasane_pdf::RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = page.pixmap.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

const RED: (u8, u8, u8) = (255, 0, 0);
const GREEN: (u8, u8, u8) = (0, 255, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);
const WHITE: (u8, u8, u8) = (255, 255, 255);

fn square(rect: &str, ap: &str, extra: &str) -> String {
    format!("<< /Type /Annot /Subtype /Square /Rect [{rect}] {ap} {extra} >>")
}

#[test]
fn appearance_stream_is_fitted_to_rect() {
    // BBox 100×50 → Rect [50 50 150 100] (same size, scale 1).
    let extra = vec![
        (10, square("50 50 150 100", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 50]",
                "1 0 0 rg 0 0 100 50 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 125), RED, "inside the annotation rect");
    assert_eq!(px(&r, 25, 125), WHITE, "left of it");
    assert_eq!(px(&r, 100, 75), WHITE, "above it");
    assert_eq!(px(&r, 175, 125), WHITE, "right of it");
}

#[test]
fn bbox_is_scaled_to_rect_when_sizes_differ() {
    // BBox 10×10 fitted to a 100×50 Rect → anisotropic scale; a fill
    // of the lower-left quarter of the BBox covers the lower-left
    // quarter of the Rect.
    let extra = vec![
        (10, square("50 50 150 100", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                "0 0 1 rg 0 0 5 5 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(
        px(&r, 75, 140),
        BLUE,
        "lower-left quarter (page 50..100 × 50..75)"
    );
    assert_eq!(px(&r, 125, 140), WHITE, "lower-right quarter untouched");
    assert_eq!(px(&r, 75, 110), WHITE, "upper-left quarter untouched");
}

#[test]
fn matrix_is_applied_before_fitting() {
    // /Matrix rotates the 100×50 BBox by 90° → a 50×100 box, fitted to
    // a 50×100 Rect: the whole Rect is painted, nothing outside.
    let extra = vec![
        (10, square("50 50 100 150", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 50] /Matrix [0 1 -1 0 0 0]",
                "0 1 0 rg 0 0 100 50 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 75, 100), GREEN);
    assert_eq!(px(&r, 75, 60), GREEN, "near the top of the rect");
    assert_eq!(px(&r, 125, 100), WHITE, "outside the rect");
    assert_eq!(px(&r, 75, 40), WHITE);
}

#[test]
fn annotations_paint_over_content_in_array_order() {
    // Content paints red; annot A paints green over it; annot B (later
    // in /Annots) paints blue over the overlap.
    let extra = vec![
        (10, square("40 40 120 120", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 80 80]",
                "0 1 0 rg 0 0 80 80 re f",
            ),
        ),
        (12, square("80 80 160 160", "/AP << /N 13 0 R >>", "")),
        (
            13,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 80 80]",
                "0 0 1 rg 0 0 80 80 re f",
            ),
        ),
    ];
    let r = render(&page_pdf(
        "1 0 0 rg 0 0 200 200 re f",
        "10 0 R 12 0 R",
        &extra,
    ))
    .unwrap();
    assert_eq!(px(&r, 60, 140), GREEN, "A over content");
    assert_eq!(px(&r, 100, 100), BLUE, "B over A in the overlap");
    assert_eq!(px(&r, 140, 60), BLUE);
    assert_eq!(px(&r, 20, 20), RED, "content elsewhere");
}

#[test]
fn as_selects_the_appearance_state() {
    let states = "/AP << /N << /On 11 0 R /Off 12 0 R >> >>";
    let on = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "1 0 0 rg 0 0 10 10 re f",
    );
    let off = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "0 1 0 rg 0 0 10 10 re f",
    );
    for (as_, expect) in [("/AS /On", RED), ("/AS /Off", GREEN)] {
        let extra = vec![
            (
                10,
                format!("<< /Type /Annot /Subtype /Widget /Rect [50 50 150 150] {states} {as_} >>"),
            ),
            (11, on.clone()),
            (12, off.clone()),
        ];
        let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
        assert_eq!(px(&r, 100, 100), expect, "{as_}");
    }
    // /AS naming a state with no stream → draws nothing, not an error.
    let extra = vec![
        (
            10,
            format!(
                "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 150] {states} /AS /Maybe >>"
            ),
        ),
        (11, on.clone()),
        (12, off.clone()),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), WHITE);
    // Several states and no /AS → ambiguous → Unsupported.
    let extra = vec![
        (
            10,
            format!("<< /Type /Annot /Subtype /Widget /Rect [50 50 150 150] {states} >>"),
        ),
        (11, on.clone()),
        (12, off),
    ];
    match render(&page_pdf("", "10 0 R", &extra)) {
        Err(Error::Unsupported { feature, .. }) => assert!(feature.contains("/AS"), "{feature}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    // A single state without /AS is unambiguous.
    let extra = vec![
        (10, "<< /Type /Annot /Subtype /Widget /Rect [50 50 150 150] /AP << /N << /Only 11 0 R >> >> >>".to_string()),
        (11, on),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
}

#[test]
fn hidden_noview_popup_and_bare_link_are_skipped() {
    let red = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "1 0 0 rg 0 0 10 10 re f",
    );
    let extra = vec![
        (10, square("0 0 200 200", "/AP << /N 14 0 R >>", "/F 2")), // Hidden
        (11, square("0 0 200 200", "/AP << /N 14 0 R >>", "/F 32")), // NoView
        (
            12,
            "<< /Type /Annot /Subtype /Popup /Rect [0 0 200 200] /AP << /N 14 0 R >> /Open true >>"
                .to_string(),
        ),
        (
            13,
            "<< /Type /Annot /Subtype /Link /Rect [0 0 200 200] >>".to_string(),
        ),
        (14, red),
    ];
    let r = render(&page_pdf("", "10 0 R 11 0 R 12 0 R 13 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), WHITE, "none of them may paint");
    // The same Square *without* the flag does paint — the flag was
    // the reason, not a silent skip.
    let extra = vec![
        (10, square("0 0 200 200", "/AP << /N 14 0 R >>", "/F 4")), // Print only
        (
            14,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                "1 0 0 rg 0 0 10 10 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
}

#[test]
fn annotation_without_appearance_is_unsupported() {
    // A /Stamp has no normative look without /AP (geometric markups
    // are synthesised — see render_annot_synthesis.rs).
    let extra = vec![(
        10,
        "<< /Type /Annot /Subtype /Stamp /Rect [50 50 150 150] /C [1 0 0] >>".to_string(),
    )];
    match render(&page_pdf("", "10 0 R", &extra)) {
        Err(Error::Unsupported { feature, detail }) => {
            assert!(feature.contains("without appearance"), "{feature}");
            assert!(detail.contains("/Stamp"), "{detail}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    // Extraction is unaffected by the refusal — annotations are not
    // page text.
    let doc = Document::from_bytes(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert!(extract_text(&doc, &doc.pages()[0], &Limits::default()).is_ok());
}

/// A `null` entry (a reference to a free object resolves to null,
/// §7.3.9/§7.3.10) is skipped; any other non-dictionary is malformed.
#[test]
fn null_annots_entries_are_skipped() {
    let extra = vec![
        (10, square("50 50 150 100", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 50]",
                "1 0 0 rg 0 0 100 50 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "null 10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 125), RED);
    let err = render(&page_pdf("", "42 10 0 R", &extra)).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn malformed_rect_or_bbox_is_a_structure_error() {
    let ok_ap = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        "0 0 10 10 re f",
    );
    let extra = vec![
        (10, square("50 50 150", "/AP << /N 11 0 R >>", "")),
        (11, ok_ap),
    ];
    assert!(matches!(
        render(&page_pdf("", "10 0 R", &extra)),
        Err(Error::Structure(_))
    ));
    let no_bbox = stream("/Type /XObject /Subtype /Form", "0 0 10 10 re f");
    let extra = vec![
        (10, square("50 50 150 150", "/AP << /N 11 0 R >>", "")),
        (11, no_bbox),
    ];
    assert!(matches!(
        render(&page_pdf("", "10 0 R", &extra)),
        Err(Error::Structure(_))
    ));
    // Reversed Rect corners are legal (normalised), degenerate BBox
    // gets scale 1 rather than a division by zero.
    let flat = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 0]",
        "0 0 100 0 re f",
    );
    let extra = vec![
        (10, square("150 150 50 50", "/AP << /N 11 0 R >>", "")),
        (11, flat),
    ];
    assert!(render(&page_pdf("", "10 0 R", &extra)).is_ok());
}

#[test]
fn nested_form_inside_appearance_uses_its_own_resources() {
    let extra = vec![
        (10, square("50 50 150 150", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                 /Resources << /XObject << /Inner 12 0 R >> >>",
                "q 1 0 0 1 25 25 cm /Inner Do Q",
            ),
        ),
        (
            12,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 50 50]",
                "0 0 1 rg 0 0 50 50 re f",
            ),
        ),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), BLUE, "inner form centred in the rect");
    assert_eq!(px(&r, 60, 140), WHITE, "outside the inner form");
}

#[test]
fn opaque_transparency_group_appearance_runs_inline() {
    // Acrobat writes /Group /S /Transparency on most appearance
    // streams; under opaque Normal state it paints identically inline.
    let grp = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Group << /S /Transparency /CS /DeviceRGB /I true >>",
        "1 0 0 rg 0 0 10 10 re f",
    );
    let extra = vec![
        (10, square("50 50 150 150", "/AP << /N 11 0 R >>", "")),
        (11, grp),
    ];
    let r = render(&page_pdf("", "10 0 R", &extra)).unwrap();
    assert_eq!(px(&r, 100, 100), RED);
    // A knockout group needs real group compositing → refused.
    let ko = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Group << /S /Transparency /K true >>",
        "1 0 0 rg 0 0 10 10 re f",
    );
    let extra = vec![
        (10, square("50 50 150 150", "/AP << /N 11 0 R >>", "")),
        (11, ko),
    ];
    assert!(matches!(
        render(&page_pdf("", "10 0 R", &extra)),
        Err(Error::Unsupported { .. })
    ));
}

#[test]
fn annotation_state_does_not_leak_from_page_content() {
    // Page content leaves a non-identity CTM, a clip and a fill colour
    // behind (no Q); the appearance must start from a fresh state.
    let content = "2 0 0 2 100 100 cm 0 0 1 rg 0 0 10 10 re W n";
    let extra = vec![
        (10, square("20 20 60 60", "/AP << /N 11 0 R >>", "")),
        (
            11,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                "1 0 0 rg 0 0 10 10 re f",
            ),
        ),
    ];
    let r = render(&page_pdf(content, "10 0 R", &extra)).unwrap();
    assert_eq!(
        px(&r, 40, 160),
        RED,
        "drawn at its own Rect with its own colour"
    );
}

#[test]
fn page_without_annotations_is_unchanged() {
    let r = render(&page_pdf("0 0 1 rg 10 10 20 20 re f", "", &[])).unwrap();
    assert_eq!(px(&r, 20, 180), BLUE);
    assert!(r.font_substitutions.is_empty());
}
