//! `redact` on pages that set a soft mask with `gs`: the mask's group is
//! content (ISO 32000-2 §11.6.5.2). A mask whose group draws nothing that
//! meets a rect is kept whole and the page redacted; one whose group
//! draws inside a rect — or whose group's `/BBox` meets one, or that
//! hides content inside a rect behind its `/BBox` — is refused, as is
//! anything in it that cannot be bounded.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, redact, Document, Error, RedactOptions, TextOptions};

/// LiberationSans as an embedded simple TrueType font, objects 20–22.
fn font_objs() -> Vec<(u32, Vec<u8>)> {
    let ff = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let widths = vec!["600"; 95].join(" ");
    vec![
        (
            20,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
                 /LastChar 126 /Widths [{widths}] /Encoding /WinAnsiEncoding \
                 /FontDescriptor 21 0 R >>"
            )
            .into_bytes(),
        ),
        (
            21,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 22 0 R >>"
                .to_vec(),
        ),
        (22, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
    ]
}

const SECRET: &str = "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET";
const RECT: (f64, f64, f64, f64) = (45.0, 90.0, 160.0, 115.0);

/// A page drawing `content`, with ExtGState /GM setting a luminosity
/// soft mask whose group (object 30) has `/BBox [bbox]` and draws
/// `group`; object 31 is a form the group may call as /Fx.
fn page(content: &str, bbox: &str, group: &str) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 20 0 R >> \
              /ExtGState << /GM << /SMask << /S /Luminosity /G 30 0 R >> >> \
              /GN << /SMask /None >> >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            30,
            stream_obj(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [{bbox}] \
                     /Group << /S /Transparency /CS /DeviceGray >> \
                     /Resources << /Font << /F1 20 0 R >> /XObject << /Fx 31 0 R >> \
                     /Shading << /S0 32 0 R >> /Pattern << /P0 33 0 R >> >>"
                ),
                group.as_bytes(),
            ),
        ),
        (
            31,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                b"1 g 55 95 20 10 re f",
            ),
        ),
        // An axial shading whose axis lies inside the rect.
        (
            32,
            b"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [60 100 150 100] \
              /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>"
                .to_vec(),
        ),
        (33, b"<< /PatternType 2 /Shading 32 0 R >>".to_vec()),
    ];
    objs.extend(font_objs());
    build_pdf(&objs)
}

fn run(pdf: &[u8]) -> Result<Vec<u8>, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()).map(|r| r.output)
}

fn text(pdf: &[u8]) -> String {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .filter_map(|c| c.unicode.clone())
        .collect()
}

/// Whether the output's page still sets ExtGState /GM.
fn sets_the_mask(pdf: &[u8]) -> bool {
    let doc = Document::from_bytes(pdf).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    ops.iter()
        .any(|o| matches!(o, kasane_pdf::Operation::SetExtGState(n) if n.as_slice() == b"GM"))
}

fn refused_for_the_mask(r: Result<Vec<u8>, Error>) {
    match r {
        Err(Error::Unsupported { feature, detail }) => {
            assert!(feature.contains("blockers"), "{feature}: {detail}");
            assert!(
                detail.contains("soft mask whose group draws inside a rect"),
                "{detail}"
            );
        }
        other => panic!("not refused for the mask: {:?}", other.map(|b| b.len())),
    }
}

const FAR: &str = "1 g 5 5 30 30 re f";

#[test]
fn a_mask_drawing_away_from_the_rects_is_kept_and_the_page_redacted() {
    for content in [
        // Text beside a masked fill, under the mask, and with the mask
        // set inside the text object (the text state survives the walk
        // of the mask's group).
        format!("q /GM gs 0 0 1 rg 0 0 40 40 re f Q {SECRET}"),
        format!("q /GM gs 0 0 1 rg 0 0 40 40 re f {SECRET} Q"),
        "q 0 0 1 rg 0 0 40 40 re f BT /F1 12 Tf 50 100 Td /GM gs (SECRETTEXT) Tj \
         0 -60 Td (KEPT) Tj /GN gs ET Q"
            .to_string(),
    ] {
        let out =
            run(&page(&content, "0 0 40 40", FAR)).unwrap_or_else(|e| panic!("{content}: {e}"));
        let t = text(&out);
        assert!(!t.contains("SECRET"), "{content}: {t}");
        assert!(
            content.contains("KEPT") == t.contains("KEPT"),
            "{content}: {t}"
        );
        assert!(sets_the_mask(&out), "the mask is kept: {content}");
    }
    // The mask's own text, away from the rects, is kept.
    let out = run(&page(
        &format!("q /GM gs 0 0 40 40 re f Q {SECRET}"),
        "0 0 40 40",
        "BT /F1 6 Tf 2 10 Td (MASKTEXT) Tj ET",
    ))
    .unwrap();
    let doc = Document::from_bytes(&out).unwrap();
    assert!(!text(&out).contains("SECRET"));
    assert_eq!(doc.pages().len(), 1);
}

#[test]
fn a_mask_left_with_nothing_painted_under_it_is_refused() {
    // Everything painted under the mask is removed: the output would
    // carry the mask's group without using it — data the page used,
    // which the verification of the output refuses.
    match run(&page(&format!("q /GM gs {SECRET} Q"), "0 0 40 40", FAR)) {
        Err(Error::Unsupported { feature, .. }) => {
            assert_eq!(feature, "redaction output failed verification")
        }
        other => panic!("{:?}", other.map(|b| b.len())),
    }
}

#[test]
fn a_mask_whose_group_meets_a_rect_is_refused() {
    let page_content = format!("q /GM gs 0 0 40 40 re f Q {SECRET}");
    for (bbox, group) in [
        // Draws inside the rect.
        ("0 0 200 200", "1 g 55 95 20 10 re f"),
        // Text inside the rect, in the mask.
        ("0 0 200 200", "BT /F1 12 Tf 50 100 Td (MASKTEXT) Tj ET"),
        // Drawn inside the rect, hidden by the group's /BBox: data still.
        ("0 0 40 40", "1 g 55 95 20 10 re f"),
        // The group's /BBox meets the rect (what it bounds is its own).
        ("0 0 200 200", FAR),
        // A form the group calls draws inside the rect.
        ("0 0 40 40", "/Fx Do"),
    ] {
        refused_for_the_mask(run(&page(&page_content, bbox, group)));
    }
}

#[test]
fn a_mask_painting_a_shading_or_a_pattern_is_refused() {
    // Their data (here an axis inside the rect) reach past what the
    // group's /BBox lets them paint.
    for group in ["/S0 sh", "/Pattern cs /P0 scn 0 0 10 10 re f"] {
        refused_for_the_mask(run(&page(
            &format!("q /GM gs 0 0 40 40 re f Q {SECRET}"),
            "0 0 40 40",
            group,
        )));
    }
}
