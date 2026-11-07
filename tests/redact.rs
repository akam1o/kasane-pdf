//! `redact`: removal inside rects, no route back to edited originals,
//! verification of the written bytes, refusals.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, Document, ElsewhereLocation, Error, RedactOptions, TextOptions,
};

/// 16 × 16 RGB samples that occur nowhere else.
fn secret_samples() -> Vec<u8> {
    (0..16 * 16 * 3)
        .map(|i| ((i * 37 + 11) % 256) as u8)
        .collect()
}

fn image_obj() -> Vec<u8> {
    stream_obj(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8",
        &secret_samples(),
    )
}

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

/// One page with `content`, the font as /F1, and extra objects.
fn one_page(
    content: &str,
    page_extra: &str,
    catalog_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                 /Resources << /Font << /F1 20 0 R >> >> /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

const SECRET: &str = "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET";
const RECT: (f64, f64, f64, f64) = (45.0, 90.0, 160.0, 115.0);

fn run(pdf: &[u8], rects: Vec<(f64, f64, f64, f64)>) -> Result<kasane_pdf::Redaction, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(&doc, &[(0, rects)], &RedactOptions::new())
}

fn page_text(pdf: &[u8], page: usize) -> String {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[page], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .filter_map(|c| c.unicode.clone())
        .collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn unsupported(r: Result<kasane_pdf::Redaction, Error>) -> String {
    match r {
        Err(Error::Unsupported { feature, detail }) => format!("{feature}: {detail}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn text_inside_the_rect_is_removed_and_the_rest_kept() {
    let pdf = one_page(
        "BT /F1 12 Tf 10 100 Td (KEEP) Tj 50 0 Td (SECRETTEXT) Tj ET",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(55.0, 90.0, 190.0, 115.0)]).unwrap();
    assert_eq!(r.codes_removed, 10);
    let text = page_text(&r.output, 0);
    assert!(text.contains("KEEP"), "{text}");
    assert!(!text.contains("SECRET") && !text.contains("TEXT"), "{text}");
    assert!(!contains(&r.output, b"SECRETTEXT"));
}

/// The original of an edited image stays reachable through an
/// inherited `/Resources` on a page that never draws it (the region
/// editor's documented behaviour): `redact` removes that name.
#[test]
fn an_inherited_unused_original_is_not_carried() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> >>"
                .to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>".to_vec(),
        ),
        (
            4,
            b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".to_vec(),
        ),
        (5, image_obj()),
        (6, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (7, stream_obj("", b"")),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let edited = kasane_pdf::remove_regions(
        &doc,
        &[(0, vec![(0.0, 0.0, 200.0, 200.0)])],
        &kasane_pdf::Limits::default(),
    )
    .unwrap();
    assert!(
        contains(&edited.output, &secret_samples()),
        "the editor keeps it"
    );
    let r = run(&pdf, vec![(0.0, 0.0, 200.0, 200.0)]).unwrap();
    assert_eq!(r.pixels_changed, 256);
    assert_eq!(r.original_names_removed, 1);
    assert!(!contains(&r.output, &secret_samples()[..96]));
}

/// The same image drawn unredacted elsewhere keeps its samples in the
/// output: refused.
#[test]
fn an_image_also_drawn_elsewhere_is_refused() {
    let objs_pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                b"q 40 0 0 40 50 50 cm /Im1 Do Q q 40 0 0 40 150 150 cm /Im1 Do Q",
            ),
        ),
        (5, image_obj()),
    ]);
    let e = unsupported(run(&objs_pdf, vec![(40.0, 40.0, 100.0, 100.0)]));
    assert!(e.contains("also drawn unredacted"), "{e}");
}

/// A painted path inside a rect is removed; one crossing a rect's edge,
/// or one that also clips, is refused.
#[test]
fn paths() {
    let pdf = one_page(
        "0 0 1 rg 60 60 30 30 re f 1 0 0 rg 0 0 20 20 re f",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]).unwrap();
    assert_eq!(r.paths_removed, 1);
    let doc = Document::from_bytes(&r.output).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    let rects: Vec<_> = ops
        .iter()
        .filter(|o| matches!(o, kasane_pdf::Operation::Rectangle(..)))
        .collect();
    assert_eq!(rects.len(), 1, "the rect outside stays");

    let crossing = one_page("0 0 1 rg 30 60 60 30 re f", "", "", vec![]);
    let e = unsupported(run(&crossing, vec![(40.0, 40.0, 160.0, 160.0)]));
    assert!(e.contains("crossing a rect's edge"), "{e}");
    let clipping = one_page("q 60 60 30 30 re W f Q", "", "", vec![]);
    let e = unsupported(run(&clipping, vec![(40.0, 40.0, 160.0, 160.0)]));
    assert!(e.contains("also clips"), "{e}");
}

#[test]
fn document_metadata_is_removed() {
    let pdf = one_page(
        SECRET,
        "",
        "/Metadata 30 0 R",
        vec![(
            30,
            stream_obj("/Type /Metadata /Subtype /XML", b"<x>SECRETXMP</x>"),
        )],
    );
    let pdf = {
        let s = pdf;
        let i = s.windows(13).position(|w| w == b"/Root 1 0 R >").unwrap();
        let mut o = s[..i].to_vec();
        o.extend_from_slice(b"/Root 1 0 R /Info << /Title (SECRETINFO) >> >");
        o.extend_from_slice(&s[i + 13..]);
        o
    };
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(r.info_removed && r.metadata_removed);
    assert!(!contains(&r.output, b"SECRETINFO") && !contains(&r.output, b"SECRETXMP"));
}

/// Alternate text in the structure tree may repeat what is removed.
#[test]
fn structure_alternate_text() {
    let pdf = one_page(
        "/P <</MCID 0>> BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC",
        "/StructParents 0",
        "/StructTreeRoot 30 0 R /MarkInfo << /Marked true >>",
        vec![
            (30, b"<< /Type /StructTreeRoot /K 31 0 R >>".to_vec()),
            (
                31,
                b"<< /Type /StructElem /S /P /P 30 0 R /Pg 3 0 R /K 0 /ActualText (SECRETTEXT) >>"
                    .to_vec(),
            ),
        ],
    );
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("structure tree"), "{e}");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_structure_tree(true),
    )
    .unwrap();
    assert!(r.structure_tree_removed);
    assert!(!contains(&r.output, b"SECRETTEXT"));
}

/// The redaction is positional; the same text elsewhere is reported.
#[test]
fn removed_text_elsewhere_is_reported() {
    let pdf = one_page(
        &format!("{SECRET} BT /F1 12 Tf 50 20 Td (SECRETTEXT) Tj ET"),
        "",
        "/Outlines 30 0 R",
        vec![
            (
                30,
                b"<< /Type /Outlines /First 31 0 R /Last 31 0 R /Count 1 >>".to_vec(),
            ),
            (
                31,
                b"<< /Title (SECRETTEXT) /Parent 30 0 R /Dest [3 0 R /Fit] >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(r.codes_removed >= 10);
    let locs: Vec<_> = r.elsewhere.iter().map(|e| e.location.clone()).collect();
    assert!(locs.contains(&ElsewhereLocation::Page(0)), "{locs:?}");
    assert!(
        locs.iter()
            .any(|l| matches!(l, ElsewhereLocation::Object(_))),
        "{locs:?}"
    );
    assert!(r.elsewhere.iter().all(|e| e.text == "SECRETTEXT"));
    assert!(
        !format!("{r:?}").contains("SECRET"),
        "Debug carries no text"
    );
}

/// An original still reachable some other way (here an unused form's
/// own resources) fails verification: no output.
#[test]
fn verification_catches_a_remaining_original() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R /Fm 6 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (5, image_obj()),
        (
            6,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /I 5 0 R >> >>",
                b"",
            ),
        ),
    ]);
    let e = unsupported(run(&pdf, vec![(0.0, 0.0, 200.0, 200.0)]));
    assert!(
        e.contains("failed verification") && e.contains("original"),
        "{e}"
    );
}

/// What the editor refuses stays refused, with no output.
#[test]
fn refusals() {
    let annots = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Text /Rect [180 180 190 190] >>".to_vec(),
        )],
    );
    assert!(unsupported(run(&annots, vec![RECT])).contains("/Annots"));
    let invisible = one_page(
        "BT /F1 12 Tf 3 Tr 50 100 Td (SECRETTEXT) Tj ET",
        "",
        "",
        vec![],
    );
    assert!(unsupported(run(&invisible, vec![RECT])).contains("non-fill text"));
    let clip_text = one_page(
        "BT /F1 12 Tf 7 Tr 50 100 Td (SECRETTEXT) Tj ET",
        "",
        "",
        vec![],
    );
    let _ = unsupported(run(&clip_text, vec![RECT]));
    let pdf = one_page(SECRET, "", "", vec![]);
    assert!(matches!(
        run(&pdf, vec![(1.0, 1.0, 0.0, 5.0)]),
        Err(Error::Structure(_))
    ));
}

/// The output is a single revision that re-parses and renders.
#[test]
fn output_is_one_clean_revision() {
    let pdf = one_page(SECRET, "", "", vec![]);
    let r = run(&pdf, vec![RECT]).unwrap();
    let doc = Document::from_bytes(&r.output).unwrap();
    assert_eq!(r.residual.history.xref_sections, 1);
    kasane_pdf::render_page(&doc, &doc.pages()[0], &kasane_pdf::RenderOptions::default()).unwrap();
}
