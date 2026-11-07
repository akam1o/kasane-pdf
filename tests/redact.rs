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

fn raw_contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether `needle` occurs in the output once parsed: in any string of
/// any object (object-stream members included), any stream's stored
/// bytes, or any page's decoded content. Byte search over the file
/// alone misses compressed object streams.
fn contains(pdf: &[u8], needle: &[u8]) -> bool {
    fn walk(o: &kasane_pdf::Object, needle: &[u8]) -> bool {
        use kasane_pdf::Object as O;
        match o {
            O::String(b) => raw_contains(b, needle),
            O::Array(a) => a.iter().any(|v| walk(v, needle)),
            O::Dict(d) => d.values().any(|v| walk(v, needle)),
            O::Stream { dict, data } => {
                raw_contains(data, needle) || dict.values().any(|v| walk(v, needle))
            }
            _ => false,
        }
    }
    if raw_contains(pdf, needle) {
        return true;
    }
    let doc = Document::from_bytes(pdf).unwrap();
    let size = doc
        .trailer()
        .get(b"Size".as_slice())
        .and_then(|s| s.as_i64())
        .unwrap();
    for n in 1..size as u32 {
        let r = kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        };
        if let Ok(o) = doc.resolve(r) {
            if walk(&o, needle) {
                return true;
            }
        }
    }
    doc.pages().iter().any(|p| {
        doc.page_contents(p)
            .unwrap()
            .iter()
            .any(|c| raw_contains(c, needle))
    })
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

/// The same image drawn unredacted elsewhere — here the same page,
/// outside the rect — keeps its original in the file: the redaction of
/// the rect succeeds, the display is reported, and refused on request.
#[test]
fn an_image_also_drawn_elsewhere_is_reported() {
    let pdf = build_pdf(&[
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
    let r = run(&pdf, vec![(40.0, 40.0, 100.0, 100.0)]).unwrap();
    assert_eq!(r.images_edited, 1);
    assert!(r.elsewhere.iter().any(
        |e| e.removed == kasane_pdf::Removed::Image && e.location == ElsewhereLocation::Page(0)
    ));
    assert!(
        contains(&r.output, &secret_samples()),
        "the other display keeps it"
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = unsupported(redact(
        &doc,
        &[(0, vec![(40.0, 40.0, 100.0, 100.0)])],
        &RedactOptions::new().refuse_copies_elsewhere(true),
    ));
    assert!(e.contains("remains elsewhere"), "{e}");
}

/// An image shared by two pages: the redacted page draws an edited
/// copy, the other page keeps the original (reported).
#[test]
fn a_shared_image_is_redacted_on_one_page_only() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (5, image_obj()),
        (
            6,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
    ]);
    let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
    assert_eq!(r.pixels_changed, 256);
    assert_eq!(pixel(&r.output, 100, 100), 0, "page 1: blackened");
    assert!(r.elsewhere.iter().any(
        |e| e.removed == kasane_pdf::Removed::Image && e.location == ElsewhereLocation::Page(1)
    ));
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

    // A curve crossing a rect's edge is refused.
    let crossing = one_page("0 0 1 rg 30 60 m 90 60 90 90 30 90 c f", "", "", vec![]);
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
    assert!(r
        .elsewhere
        .iter()
        .all(|e| e.removed == kasane_pdf::Removed::Text("SECRETTEXT".into())));
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
            b"<< /Type /Annot /Subtype /Text /Rect [100 100 110 110] >>".to_vec(),
        )],
    );
    assert!(unsupported(run(&annots, vec![RECT])).contains("other than a link meeting a rect"));
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

/// A link whose `/Rect` meets a rect is removed with its action; one
/// outside is kept. A link the structure tree refers to would stay
/// reachable through it: refused unless the tree is removed.
#[test]
fn links() {
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R 31 0 R]",
        "",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [50 95 120 110] \
                  /A << /S /URI /URI (https://example.com/SECRETLINK) >> >>"
                    .to_vec(),
            ),
            (
                31,
                b"<< /Type /Annot /Subtype /Link /Rect [10 10 40 20] \
                  /A << /S /URI /URI (https://example.com/public) >> >>"
                    .to_vec(),
            ),
        ],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.links_removed, 1);
    assert!(!contains(&r.output, b"SECRETLINK"));
    assert!(
        contains(&r.output, b"example.com/public"),
        "the search sees object streams"
    );
    let doc = Document::from_bytes(&r.output).unwrap();
    let page = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let annots = page.as_dict().unwrap().get(b"Annots".as_slice()).cloned();
    let annots = doc
        .deref(&annots.expect("the outside link is kept"))
        .unwrap();
    assert_eq!(annots.as_array().unwrap().len(), 1);

    let tagged = one_page(
        SECRET,
        "/Annots [30 0 R] /StructParents 0",
        "/StructTreeRoot 32 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [50 95 120 110] /StructParent 1 \
                  /A << /S /URI /URI (https://example.com/SECRETLINK) >> >>"
                    .to_vec(),
            ),
            (32, b"<< /Type /StructTreeRoot /K 33 0 R >>".to_vec()),
            (
                33,
                b"<< /Type /StructElem /S /Link /P 32 0 R /K << /Type /OBJR /Obj 30 0 R >> >>"
                    .to_vec(),
            ),
        ],
    );
    let e = unsupported(run(&tagged, vec![RECT]));
    assert!(e.contains("structure tree refers to"), "{e}");
    let doc = Document::from_bytes(&tagged).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_structure_tree(true),
    )
    .unwrap();
    assert!(!contains(&r.output, b"SECRETLINK"));
}

// Independent review (P0s, all fixed): content the walker placed
// wrongly was kept while verification, using the same walker, agreed.

/// A rectangle under a rotated CTM is bounded by all four corners.
#[test]
fn rotated_rectangle_path_is_located() {
    let pdf = one_page("q 1 1 -1 1 100 20 cm 0 0 40 40 re f Q", "", "", vec![]);
    let e = unsupported(run(&pdf, vec![(70.0, 50.0, 80.0, 60.0)]));
    assert!(e.contains("crossing a rect's edge"), "{e}");
    let r = run(&pdf, vec![(40.0, 0.0, 160.0, 120.0)]).unwrap();
    assert_eq!(r.paths_removed, 1);
}

/// A Type 3 glyph painting far from its cell (40 pt to the right) is
/// taken where its CharProc paints, not estimated from the cell.
#[test]
fn type3_ink_far_from_the_cell_is_found() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /F0 20 Tf 50 100 Td (a) Tj ET")),
        (
            5,
            b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 3000 1000] \
              /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 6 0 R >> \
              /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 \
              /Widths [600] /Resources << >> >>"
                .to_vec(),
        ),
        (6, stream_obj("", b"600 0 d0 2000 0 1000 1000 re f")),
    ]);
    let r = run(&pdf, vec![(90.0, 100.0, 110.0, 120.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
}

/// An image's `/Alternates` hold the same picture: refused.
#[test]
fn image_alternates_are_refused() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 /Alternates [<< /Image 6 0 R >>]",
                &secret_samples(),
            ),
        ),
        (6, image_obj()),
    ]);
    let e = unsupported(run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]));
    assert!(e.contains("Alternates"), "{e}");
}

// Second independent review (P0/P1, fixed).

fn pixel(pdf: &[u8], x: u32, y: u32) -> u8 {
    let doc = Document::from_bytes(pdf).unwrap();
    let p = kasane_pdf::render_page(&doc, &doc.pages()[0], &kasane_pdf::RenderOptions::default())
        .unwrap();
    p.pixmap().pixel(x, y).unwrap().red()
}

/// A NoRotate link's appearance is not placed by its /Rect: refused.
#[test]
fn norotate_links_are_refused() {
    let pdf =
        one_page(
            "",
            "/Rotate 90 /Annots [30 0 R]",
            "",
            vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [10 10 100 30] /F 16 /AP << /N 31 0 R >> >>"
                    .to_vec(),
            ),
            (
                31,
                stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 90 20]", b"0 0 90 20 re f"),
            ),
        ],
        );
    let e = unsupported(run(&pdf, vec![(15.0, 50.0, 25.0, 60.0)]));
    assert!(e.contains("NoRotate"), "{e}");
}

/// A clip hides ink, not data: a code or path standing in the rect is
/// removed even when clipped away there (extraction and readers of the
/// file still find it).
#[test]
fn clipped_content_in_the_rect_is_removed() {
    let pdf = one_page(
        "q 65 0 100 200 re W n BT /F1 40 Tf 50 100 Td (M) Tj ET 50 100 10 30 re f Q",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(50.0, 100.0, 60.0, 140.0)]).unwrap();
    assert_eq!((r.codes_removed, r.paths_removed), (1, 1));
    assert_eq!(page_text(&r.output, 0), "");
}

/// A `cm` inside a path object is placed differently by different
/// readers: refused (§8.2).
#[test]
fn an_op_inside_a_path_object_is_refused() {
    let pdf = one_page("0 0 20 20 re 1 0 0 1 100 100 cm f", "", "", vec![]);
    assert!(matches!(
        run(&pdf, vec![(100.0, 100.0, 120.0, 120.0)]),
        Err(Error::Structure(m)) if m.contains("inside a path object")
    ));
}

/// Names are written escaped: `/Im#2331` must not become `/Im1`.
#[test]
fn escaped_names_survive_the_rewrite() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im#2331 5 0 R /Im1 6 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 50 0 0 50 100 100 cm /Im#2331 Do Q")),
        (
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                &[0, 0, 0],
            ),
        ),
        (
            6,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8",
                &[255, 255, 255],
            ),
        ),
    ]);
    let r = run(&pdf, vec![(0.0, 0.0, 10.0, 10.0)]).unwrap();
    assert_eq!(pixel(&pdf, 125, 75), 0);
    assert_eq!(pixel(&r.output, 125, 75), 0);
}

/// Numbers are written losslessly: content outside keeps its geometry.
#[test]
fn numbers_survive_the_rewrite() {
    let pdf = one_page(
        "q .00014 0 0 .00014 100 100 cm 0 0 400000 400000 re f Q",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(0.0, 0.0, 10.0, 10.0)]).unwrap();
    assert_eq!(pixel(&pdf, 145, 80), 0);
    assert_eq!(pixel(&r.output, 145, 80), 0);
}

// Third independent review (P0, fixed).

/// Clips decide nothing in redaction: whatever meets a rect is removed
/// or cut, under any clip.
#[test]
fn clips_do_not_decide_hits() {
    let pdf = one_page(
        "q 0 0 m 200 0 l 50 150 l h W n 52 102 30 30 re f Q",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(50.0, 100.0, 60.0, 140.0)]).unwrap();
    assert_eq!(r.paths_cut, 1);
    // Rectangular clips, restored by Q and set under changed CTMs,
    // stay exact.
    let ok = one_page(
        "q 2 0 0 2 0 0 cm 0 0 20 20 re W n Q q 1 0 0 1 50 50 cm 0 0 100 100 re W n \
         0 0 50 50 re W n BT /F1 12 Tf 10 10 Td (SECRET) Tj ET Q",
        "",
        "",
        vec![],
    );
    assert_eq!(
        run(&ok, vec![(50.0, 50.0, 100.0, 100.0)])
            .unwrap()
            .codes_removed,
        6
    );
}

/// In a CMap with one- and two-byte codes, `Tw` applies to the
/// one-byte code 32 only, not to `<0020>` (§9.3.3): the second glyph is
/// placed without word spacing — where the renderer draws it.
#[test]
fn word_spacing_skips_two_byte_space_codes() {
    let cmap = b"begincmap 2 begincodespacerange <20> <7F> <0000> <00FF> endcodespacerange \
                 2 begincidchar <0020> 32 <0021> 33 endcidchar endcmap";
    let ff = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/KasaneCIDTest.ttf"
    ))
    .unwrap();
    let mut map = vec![0u8; 68];
    map[65] = 1;
    map[67] = 2;
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj("", b"BT /F1 20 Tf 10 Tw 50 100 Td <00200021> Tj ET"),
        ),
        (
            5,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneCIDTest /Encoding 8 0 R \
              /DescendantFonts [6 0 R] >>"
                .to_vec(),
        ),
        (
            6,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KasaneCIDTest \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /FontDescriptor 7 0 R /DW 600 /CIDToGIDMap 9 0 R >>"
                .to_vec(),
        ),
        (
            7,
            b"<< /Type /FontDescriptor /FontName /KasaneCIDTest /Flags 4 \
              /FontBBox [0 -200 600 800] /ItalicAngle 0 /Ascent 800 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 10 0 R >>"
                .to_vec(),
        ),
        (
            8,
            stream_obj(
                "/Type /CMap /CMapName /Mixed /CIDSystemInfo << /Registry (Adobe) \
                 /Ordering (Identity) /Supplement 0 >>",
                cmap,
            ),
        ),
        (9, stream_obj("", &map)),
        (10, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
    ]);
    let r = run(&pdf, vec![(61.0, 99.0, 70.0, 116.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
}

// Paths crossing a rect's edge are cut there.

/// Gray level at page point (x, y) at 72 dpi on a 200 pt page.
fn at(pdf: &[u8], x: u32, y: u32) -> u8 {
    pixel(pdf, x, 200 - y)
}

/// Whether any path point of page 1 lies strictly inside `r`.
fn path_points_inside(pdf: &[u8], r: (f64, f64, f64, f64)) -> bool {
    use kasane_pdf::Operation as O;
    let doc = Document::from_bytes(pdf).unwrap();
    let inside = |x: f64, y: f64| x > r.0 && x < r.2 && y > r.1 && y < r.3;
    doc.page_operations(&doc.pages()[0])
        .unwrap()
        .iter()
        .any(|o| match o {
            O::MoveTo(x, y) | O::LineTo(x, y) => inside(*x, *y),
            O::Rectangle(x, y, w, h) => {
                inside(*x, *y)
                    || inside(*x + *w, *y + *h)
                    || inside(*x + *w, *y)
                    || inside(*x, *y + *h)
            }
            _ => false,
        })
}

#[test]
fn a_filled_rectangle_crossing_a_rect_is_cut() {
    let pdf = one_page("0 0 1 rg 30 60 60 30 re f", "", "", vec![]);
    let rect = (40.0, 40.0, 160.0, 160.0);
    let r = run(&pdf, vec![rect]).unwrap();
    assert_eq!((r.paths_cut, r.paths_removed), (1, 0));
    assert_eq!(at(&r.output, 50, 75), 255, "inside: gone");
    assert_eq!(at(&r.output, 35, 75), 0, "outside: kept (blue has no red)");
    assert!(!path_points_inside(&r.output, rect));
}

/// A table rule through a rect: the two outer parts stay, butt-ended
/// half a line width clear of the rect.
#[test]
fn a_stroked_line_crossing_a_rect_is_cut() {
    let pdf = one_page("2 w 10 100 m 190 100 l S", "", "", vec![]);
    let rect = (50.0, 90.0, 100.0, 110.0);
    let r = run(&pdf, vec![rect]).unwrap();
    assert_eq!(r.paths_cut, 1);
    assert_eq!(at(&r.output, 75, 100), 255);
    assert_eq!(
        at(&r.output, 50, 100),
        255,
        "the band stops at the rect's edge"
    );
    assert_eq!(at(&r.output, 30, 100), 0);
    assert_eq!(at(&r.output, 150, 100), 0);
    assert!(!path_points_inside(&r.output, rect));
}

/// A rectangle's outline crossing a rect keeps its far corners (with
/// their joins) and loses the part inside.
#[test]
fn a_stroked_outline_crossing_a_rect_is_cut() {
    let pdf = one_page("4 w 20 20 160 160 re S", "", "", vec![]);
    let rect = (60.0, 170.0, 100.0, 190.0);
    let r = run(&pdf, vec![rect]).unwrap();
    assert_eq!(r.paths_cut, 1);
    assert_eq!(at(&r.output, 80, 180), 255);
    for (x, y) in [(40, 180), (150, 180), (20, 100), (180, 100), (100, 20)] {
        assert_eq!(at(&r.output, x, y), 0, "({x}, {y}) kept");
    }
    // The bottom-left and top-right corners keep their miter joins.
    assert_eq!(at(&r.output, 19, 19), 0);
    assert!(!path_points_inside(&r.output, rect));
}

/// Fill and stroke together: both are cut.
#[test]
fn fill_and_stroke_crossing_a_rect_are_cut() {
    let pdf = one_page("1 0 0 rg 2 w 30 60 60 30 re B", "", "", vec![]);
    let r = run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]).unwrap();
    assert_eq!(r.paths_cut, 1);
    assert_eq!(at(&r.output, 60, 75), 255);
    assert_eq!(
        at(&r.output, 35, 75),
        255,
        "red fill (red channel 255) kept"
    );
    assert_eq!(at(&r.output, 30, 75), 0, "black stroke kept");
}

/// What cannot be cut exactly is refused: a dashed stroke, a corner
/// whose miter reaches into the rect.
#[test]
fn uncuttable_paths_are_refused() {
    let dashed = one_page("[0 0] 0 d 2 w 10 100 m 190 100 l S", "", "", vec![]);
    let e = unsupported(run(&dashed, vec![(50.0, 90.0, 100.0, 110.0)]));
    assert!(e.contains("degenerate pattern"), "{e}");
    // A right-angle corner at (100, 100) whose miter tip, at (102, 102),
    // lies inside a rect from (101.5, 101.5): the segments clear it (2
    // is half the width, 1.5 the gap), the join does not.
    let sharp = one_page("4 w 10 M 50 100 m 100 100 l 100 50 l S", "", "", vec![]);
    let e = unsupported(run(&sharp, vec![(101.5, 101.5, 150.0, 150.0)]));
    assert!(e.contains("join"), "{e}");
}

/// A path whose conservative bounds meet a rect but whose paint keeps
/// clear of it is left exactly as it was.
#[test]
fn a_path_clear_of_the_rect_is_unchanged() {
    // Bounds are inflated by the miter limit (10 × 1): they reach the
    // rect, the straight line itself stays 1 unit away.
    let pdf = one_page("2 w 10 100 m 190 100 l S", "", "", vec![]);
    let r = run(&pdf, vec![(50.0, 102.0, 100.0, 110.0)]).unwrap();
    assert_eq!((r.paths_cut, r.paths_removed), (0, 0));
    assert_eq!(at(&r.output, 75, 100), 0);
}

/// Cuts survive the round trip through a scaled, flipped CTM (as
/// Chrome writes): verification re-runs the cut and finds nothing.
#[test]
fn cuts_under_a_flipped_scaled_ctm() {
    let pdf = one_page(
        "q .23999999 0 0 -.23999999 0 200 cm 4.1666 w 41.6 416.7 m 791.7 416.7 l S \
         0 0 1 rg 125 375 333.3 83.3 re f Q",
        "",
        "",
        vec![],
    );
    let rect = (50.0, 90.0, 100.0, 110.0);
    let r = run(&pdf, vec![rect]).unwrap();
    assert_eq!(r.paths_cut, 2);
    assert_eq!(at(&r.output, 75, 100), 255);
    assert_eq!(at(&r.output, 30, 100), 0);
}

// Review of the path cut (P0s, fixed): joins decided in user space,
// repeated points normalised, the cut exact at the stroke's edge.

/// Under an anisotropic CTM a join that is a bevel in page-space
/// angles is a miter in user space, where the stroke is drawn.
#[test]
fn joins_are_decided_in_user_space() {
    let pdf = one_page(
        "q 10 0 0 1 100 100 cm 10 w 0 J 0 j 2 M -10 -10 m 0 0 l -10 10 l S Q",
        "",
        "",
        vec![],
    );
    let e = unsupported(run(&pdf, vec![(160.0, 99.5, 165.0, 100.5)]));
    assert!(e.contains("join"), "{e}");
}

/// A repeated point does not hide the join of the segments around it.
#[test]
fn repeated_points_do_not_hide_joins() {
    for c in [
        "10 w 0 J 0 j 20 M 0 90 m 100 100 l 100 100 l 0 110 l S",
        "10 w 0 J 0 j 20 M 100 100 m 0 110 l 0 90 l 100 100 l h S",
    ] {
        let pdf = one_page(c, "", "", vec![]);
        let e = unsupported(run(&pdf, vec![(130.0, 99.5, 140.0, 100.5)]));
        assert!(e.contains("join"), "{c}: {e}");
    }
}

/// A butt-capped stroke is cut exactly where its band meets the rect:
/// nothing outside is lost.
#[test]
fn a_butt_cut_keeps_everything_outside() {
    let pdf = one_page("10 w 0 J 20 100 m 200 100 l S", "", "", vec![]);
    let r = run(&pdf, vec![(90.0, 90.0, 110.0, 110.0)]).unwrap();
    assert_eq!(r.paths_cut, 1);
    for x in [86, 88, 89, 111, 112] {
        assert_eq!(at(&r.output, x, 100), 0, "x = {x} is outside and kept");
    }
    assert_eq!(at(&r.output, 100, 100), 255);
}

// Form XObjects drawn by the page: edited in a per-invocation copy.

/// A page drawing `/Fm` (object 30) with `content`, plus extra objects.
fn form_page(
    page_content: &str,
    form_dict: &str,
    form_content: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 20 0 R >> /XObject << /Fm 30 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", page_content.as_bytes())),
        (
            30,
            stream_obj(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200] \
                     /Resources << /Font << /F1 20 0 R >> >> {form_dict}"
                ),
                form_content.as_bytes(),
            ),
        ),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

#[test]
fn text_and_paths_in_a_form_are_redacted() {
    let pdf = form_page(
        "/Fm Do",
        "",
        "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET \
         2 w 10 103 m 190 103 l S",
        vec![],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!((r.forms_edited, r.codes_removed, r.paths_cut), (1, 10, 1));
    let text = page_text(&r.output, 0);
    assert!(
        text.contains("PUBLICWORD") && !text.contains("SECRET"),
        "{text}"
    );
    assert!(
        !contains(&r.output, b"SECRETTEXT"),
        "the original form is not carried"
    );
    assert_eq!(at(&r.output, 100, 103), 255);
    assert_eq!(at(&r.output, 20, 103), 0);
}

/// A transparency group form is edited like any other.
#[test]
fn a_transparency_group_form_is_redacted() {
    let pdf = form_page(
        "/Fm Do",
        "/Group << /S /Transparency >>",
        "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET",
        vec![],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
}

/// The same form drawn again outside the rect keeps its original
/// there: reported, the text found again.
#[test]
fn a_form_drawn_again_elsewhere_is_reported() {
    let pdf = form_page(
        "/Fm Do q 1 0 0 1 0 -80 cm /Fm Do Q",
        "",
        "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET",
        vec![],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.forms_edited, 1);
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Form));
    assert!(r
        .elsewhere
        .iter()
        .any(|e| matches!(e.removed, kasane_pdf::Removed::Text(_))));
}

/// A form drawn by a form, and images inside forms, are edited in
/// copies along the whole invocation chain.
#[test]
fn nested_forms_and_form_images_are_redacted() {
    let nested = build_pdf(&{
        let mut o = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /XObject << /Fm 30 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"q 1 0 0 1 0 10 cm /Fm Do Q")),
            (
                30,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200] \
                     /Resources << /XObject << /In 31 0 R >> >>",
                    b"q 1 0 0 1 0 -10 cm /In Do Q",
                ),
            ),
            (
                31,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 20 0 R >> >>",
                    b"BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET",
                ),
            ),
        ];
        o.extend(font_objs());
        o
    });
    let r = run(&nested, vec![RECT]).unwrap();
    assert_eq!((r.codes_removed, r.forms_edited), (10, 2));
    let text = page_text(&r.output, 0);
    assert!(
        text.contains("PUBLICWORD") && !text.contains("SECRET"),
        "{text}"
    );
    assert!(!contains(&r.output, b"SECRETTEXT"));

    // An image inside a form, half inside the rect.
    let image = form_page(
        "/Fm Do",
        "/Resources << /XObject << /I 31 0 R >> >>",
        "q 100 0 0 100 0 50 cm /I Do Q",
        vec![(31, image_obj())],
    );
    let r = run(&image, vec![(50.0, 0.0, 200.0, 200.0)]).unwrap();
    assert_eq!((r.images_edited, r.forms_edited), (1, 1));
    assert_eq!(r.pixels_changed, 128);
    assert_eq!(at(&r.output, 75, 100), 0, "redacted half: black");
    assert!(
        !contains(&r.output, &secret_samples()[..96]),
        "the original is not carried"
    );
}

/// An inline image away from the rects is kept as it is; one meeting a
/// rect is refused (its samples are not edited).
#[test]
fn inline_images() {
    let outside = one_page(
        &format!("{SECRET} q 20 0 0 20 10 10 cm BI /W 1 /H 1 /CS /G /BPC 8 ID \u{0} EI Q"),
        "",
        "",
        vec![],
    );
    let r = run(&outside, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    assert_eq!(at(&r.output, 20, 20), 0, "the inline image is kept");
    let inside = one_page(
        &format!("{SECRET} q 20 0 0 20 60 95 cm BI /W 1 /H 1 /CS /G /BPC 8 ID \u{0} EI Q"),
        "",
        "",
        vec![],
    );
    let e = unsupported(run(&inside, vec![RECT]));
    assert!(e.contains("inline image"), "{e}");
}

// Review of forms and shared images (P0/P1, fixed).

/// Nested transforms compose as CTM' = M × CTM (cm and a form's
/// /Matrix): content is located where it is drawn.
#[test]
fn nested_transforms_locate_content() {
    // Drawn at (70, 20)–(90, 40): 1 0 0 1 50 0 cm, then a form scaled 2×.
    let pdf = form_page(
        "q 1 0 0 1 50 0 cm /Fm Do Q",
        "/Matrix [2 0 0 2 0 0]",
        "10 10 10 10 re f",
        vec![],
    );
    let r = run(&pdf, vec![(65.0, 15.0, 95.0, 45.0)]).unwrap();
    assert_eq!(r.paths_removed, 1);
    assert_eq!(at(&r.output, 80, 30), 255);
    // An empty area where the reversed order would place it.
    let r = run(&pdf, vec![(119.0, 19.0, 141.0, 41.0)]).unwrap();
    assert_eq!(r.paths_removed + r.paths_cut, 0);
    assert_eq!(at(&r.output, 80, 30), 0, "outside the rect: kept");
    // Two cm on the page.
    let page = one_page(
        "q 1 0 0 1 50 0 cm 2 0 0 2 0 0 cm 10 10 10 10 re f Q",
        "",
        "",
        vec![],
    );
    let r = run(&page, vec![(65.0, 15.0, 95.0, 45.0)]).unwrap();
    assert_eq!(r.paths_removed, 1);
}

/// An original kept for its display on another page may stay once —
/// not as a second, undisplayed copy.
#[test]
fn a_second_copy_of_a_kept_original_is_caught() {
    let pdf = build_pdf(&[
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Extra 8 0 R >>".to_vec(),
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (5, image_obj()),
        (
            6,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (8, image_obj()),
    ]);
    let e = unsupported(run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]));
    assert!(e.contains("original data"), "{e}");
}

// Final review (P0/P2, fixed).

/// An inline image whose sample bytes spell `EI` and whose length the
/// dictionary does not fix (a named colour space) may have been cut
/// short by the parser: refused, not rewritten.
#[test]
fn an_inline_image_of_uncertain_length_is_refused() {
    let content =
        "q 20 0 0 20 10 10 cm BI /W 19 /H 1 /CS /MyGray /BPC 8 ID \u{0} EI 2 4 1 1 re f % EI Q";
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 20 0 R >> /ColorSpace << /MyGray /DeviceGray >> >> \
              /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", format!("{SECRET} {content}").as_bytes())),
    ];
    objs.extend(font_objs());
    let pdf = build_pdf(&objs);
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("inline image"), "{e}");
}

/// Two distinct originals with equal bytes, both still drawn on page
/// 2: each may stay once.
#[test]
fn equal_originals_shown_elsewhere_each_stay_once() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /A 5 0 R /B 8 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                b"q 50 0 0 50 50 50 cm /A Do Q q 50 0 0 50 100 100 cm /B Do Q",
            ),
        ),
        (5, image_obj()),
        (
            6,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /A 5 0 R /B 8 0 R >> >> /Contents 7 0 R >>"
                .to_vec(),
        ),
        (
            7,
            stream_obj(
                "",
                b"q 50 0 0 50 0 0 cm /A Do Q q 50 0 0 50 60 0 cm /B Do Q",
            ),
        ),
        (8, image_obj()),
    ]);
    let r = run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]).unwrap();
    assert_eq!(r.images_edited, 2);
}

// Forms: nesting, sharing, inheritance, cycles.

fn doc_with(page_res: &str, pages: &[&[u8]], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 100 + 2 * i))
        .collect();
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            )
            .into_bytes(),
        ),
    ];
    for (i, c) in pages.iter().enumerate() {
        let n = 100 + 2 * i as u32;
        objs.push((
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {page_res} >> \
                 /Contents {} 0 R >>",
                n + 1
            )
            .into_bytes(),
        ));
        objs.push((n + 1, stream_obj("", c)));
    }
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

const FORM: &str = "/Type /XObject /Subtype /Form /BBox [0 0 200 200]";

#[test]
fn an_image_in_a_nested_form_is_redacted() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >>",
        &[b"/Fm Do"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /In 31 0 R >> >>"),
                    b"/In Do",
                ),
            ),
            (
                31,
                stream_obj(
                    &format!(
                        "{FORM} /Matrix [1 0 0 1 0 50] /Resources << /XObject << /I 32 0 R >> >>"
                    ),
                    b"q 100 0 0 100 0 0 cm /I Do Q",
                ),
            ),
            (32, image_obj()),
        ],
    );
    let r = run(&pdf, vec![(50.0, 0.0, 200.0, 200.0)]).unwrap();
    assert_eq!(
        (r.images_edited, r.forms_edited, r.pixels_changed),
        (1, 2, 128)
    );
    assert_eq!(at(&r.output, 75, 100), 0);
    assert_ne!(at(&r.output, 25, 100), 0, "the half outside is kept");
    assert!(!contains(&r.output, &secret_samples()[..96]));
}

/// A form (with an image inside) shared by two pages: page 1 draws
/// edited copies, page 2 the originals — kept once and reported.
#[test]
fn a_shared_form_with_an_image_is_redacted_on_one_page() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >>",
        &[b"/Fm Do", b"/Fm Do"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /I 32 0 R >> >>"),
                    b"q 100 0 0 100 50 50 cm /I Do Q",
                ),
            ),
            (32, image_obj()),
        ],
    );
    let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
    assert_eq!(
        (r.images_edited, r.forms_edited, r.pixels_changed),
        (1, 1, 256)
    );
    assert_eq!(pixel(&r.output, 100, 100), 0);
    assert!(r.elsewhere.iter().any(
        |e| e.removed == kasane_pdf::Removed::Form && e.location == ElsewhereLocation::Page(1)
    ));
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = unsupported(redact(
        &doc,
        &[(0, vec![(50.0, 50.0, 150.0, 150.0)])],
        &RedactOptions::new().refuse_copies_elsewhere(true),
    ));
    assert!(e.contains("remains elsewhere"), "{e}");
}

/// A form without /Resources draws an image named by the page.
#[test]
fn an_image_in_a_form_inheriting_page_resources_is_redacted() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R /I 32 0 R >>",
        &[b"/Fm Do"],
        vec![
            (30, stream_obj(FORM, b"q 100 0 0 100 50 50 cm /I Do Q")),
            (32, image_obj()),
        ],
    );
    let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
    assert_eq!((r.images_edited, r.forms_edited), (1, 1));
    assert!(!contains(&r.output, &secret_samples()[..96]));
}

/// One of two invocations of a form meets the rect: only it is edited.
#[test]
fn only_the_invocation_in_the_rect_is_edited() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >>",
        &[b"q 1 0 0 1 0 50 cm /Fm Do Q q 1 0 0 1 0 -40 cm /Fm Do Q"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /I 32 0 R >> >>"),
                    b"q 40 0 0 40 60 50 cm /I Do Q",
                ),
            ),
            (32, image_obj()),
        ],
    );
    let r = run(&pdf, vec![(50.0, 90.0, 110.0, 150.0)]).unwrap();
    assert_eq!((r.images_edited, r.forms_edited), (1, 1));
    assert_eq!(at(&r.output, 80, 120), 0, "edited: black");
    assert_ne!(
        at(&r.output, 80, 30),
        0,
        "the other invocation keeps the original"
    );
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Form));
}

/// A form that draws itself is refused, not followed.
#[test]
fn a_form_cycle_is_refused() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >> /Font << /F1 20 0 R >>",
        &[b"/Fm Do"],
        vec![(
            30,
            stream_obj(
                &format!(
                    "{FORM} /Resources << /XObject << /Fm 30 0 R >> /Font << /F1 20 0 R >> >>"
                ),
                format!("{SECRET} /Fm Do").as_bytes(),
            ),
        )],
    );
    assert!(run(&pdf, vec![RECT]).is_err());
}

// Review of nested form editing (fixed): what may stay is decided from
// what the output draws; trimmed resources keep inherited names.

/// An original only *named* by a form still drawn — not drawn by it —
/// is not a display: it may not stay.
#[test]
fn an_original_only_named_by_a_kept_form_does_not_stay() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R /I 31 0 R >>",
        &[b"/Fm Do q 1 0 0 1 0 -80 cm /Fm Do Q q 40 0 0 20 50 90 cm /I Do Q"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /Unused 31 0 R >> >>"),
                    b"50 90 30 20 re f",
                ),
            ),
            (31, image_obj()),
        ],
    );
    match run(&pdf, vec![RECT]) {
        Ok(r) => {
            assert!(!contains(&r.output, &secret_samples()));
            assert!(!r
                .elsewhere
                .iter()
                .any(|e| e.removed == kasane_pdf::Removed::Image));
        }
        Err(e) => assert!(e.to_string().contains("original data"), "{e}"),
    }
}

/// A child drawn twice by its parent, once in the rect: the other
/// drawing keeps the original child.
#[test]
fn a_child_drawn_twice_by_its_parent() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >>",
        &[b"/Fm Do"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /In 31 0 R >> >>"),
                    b"/In Do q 1 0 0 1 0 -80 cm /In Do Q",
                ),
            ),
            (
                31,
                stream_obj(&format!("{FORM} /Resources << >>"), b"50 90 30 20 re f"),
            ),
        ],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(at(&r.output, 60, 100), 255);
    assert_eq!(at(&r.output, 60, 20), 0, "the other drawing is kept");
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Form));
}

/// Two parents share a child; only one parent's drawing meets the rect.
#[test]
fn two_parents_share_a_child() {
    let pdf = doc_with(
        "/XObject << /A 30 0 R /B 32 0 R >>",
        &[b"/A Do q 1 0 0 1 0 -80 cm /B Do Q"],
        vec![
            (
                30,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /In 31 0 R >> >>"),
                    b"/In Do",
                ),
            ),
            (
                32,
                stream_obj(
                    &format!("{FORM} /Resources << /XObject << /In 31 0 R >> >>"),
                    b"q /In Do Q",
                ),
            ),
            (
                31,
                stream_obj(&format!("{FORM} /Resources << >>"), b"50 90 30 20 re f"),
            ),
        ],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(at(&r.output, 60, 100), 255);
    assert_eq!(at(&r.output, 60, 20), 0);
}

/// A copy's trimmed resources keep what an untouched child without
/// /Resources draws through them.
#[test]
fn trimmed_resources_keep_names_an_inheriting_child_uses() {
    let pdf = doc_with(
        "/XObject << /Fm 30 0 R >>",
        &[b"/Fm Do"],
        vec![
            (
                30,
                stream_obj(
                    &format!(
                        "{FORM} /Resources << /XObject << /I 31 0 R /Child 32 0 R /Other 33 0 R >> >>"
                    ),
                    b"q 40 0 0 40 50 80 cm /I Do Q /Child Do",
                ),
            ),
            (31, image_obj()),
            (32, stream_obj(FORM, b"q 20 0 0 20 10 10 cm /Other Do Q")),
            (
                33,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
                     /BitsPerComponent 8",
                    &[50],
                ),
            ),
        ],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(
        at(&r.output, 20, 20),
        50,
        "the inheriting child still draws /Other"
    );
}

// ExtGState in redaction: stroke parameters applied, colour and
// compositing passed through, soft masks and fonts refused.

const GS_RES: &str = "/Font << /F1 20 0 R >> /ExtGState << /Wide << /LW 10 /LC 0 >> \
    /Alpha << /Type /ExtGState /ca 0.5 /CA 0.5 /BM /Multiply /SMask /None /OP false /SA false >> \
    /Adjust << /SA true >> \
    /Masked << /SMask << /S /Luminosity /G 40 0 R >> >> /F << /Font [20 0 R 12] >> >>";

/// A line width set by `gs` decides where the stroke reaches.
#[test]
fn a_gs_line_width_is_applied() {
    let pdf = doc_with(GS_RES, &[b"/Wide gs 10 118 m 190 118 l S"], vec![]);
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.paths_cut, 1, "the 10-wide band reaches into the rect");
    assert_eq!(at(&r.output, 100, 114), 255);
    assert_eq!(at(&r.output, 20, 118), 0);
}

/// Alpha, blend mode and other colour parameters do not stop redaction.
#[test]
fn compositing_gs_is_passed_through() {
    let pdf = doc_with(GS_RES, &[format!("/Alpha gs {SECRET}").as_bytes()], vec![]);
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    assert!(!page_text(&r.output, 0).contains("SECRET"));
}

#[test]
fn a_gs_soft_mask_or_font_is_refused() {
    let mask = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Group << /S /Transparency /CS /DeviceGray >>",
        b"1 g 0 0 200 200 re f",
    );
    for g in ["Masked", "F", "Adjust"] {
        let pdf = doc_with(
            GS_RES,
            &[format!("/{g} gs {SECRET}").as_bytes()],
            vec![(40, mask.clone())],
        );
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("ExtGState"), "{g}: {e}");
    }
}

/// Annotations clear of every rect are kept as they are; NoZoom and
/// NoRotate ones (drawn away from /Rect) are refused anywhere.
#[test]
fn annotations_clear_of_the_rects_are_kept() {
    let ap = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        b"0 g 0 0 10 10 re f",
    );
    let annot = |f: u32| {
        format!(
            "<< /Type /Annot /Subtype /Square /Rect [170 170 180 180] /F {f} /Contents (Note) \
             /AP << /N 31 0 R >> >>"
        )
        .into_bytes()
    };
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "",
        vec![(30, annot(4)), (31, ap.clone())],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    let doc = Document::from_bytes(&r.output).unwrap();
    let page = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let annots = page.as_dict().unwrap().get(b"Annots".as_slice()).cloned();
    let annots = doc.deref(&annots.expect("the square is kept")).unwrap();
    assert_eq!(annots.as_array().unwrap().len(), 1);
    assert_eq!(at(&r.output, 175, 175), 0, "and still drawn");
    for f in [4 | 8, 4 | 16] {
        let pdf = one_page(
            SECRET,
            "/Annots [30 0 R]",
            "",
            vec![(30, annot(f)), (31, ap.clone())],
        );
        assert!(unsupported(run(&pdf, vec![RECT])).contains("NoZoom or NoRotate"));
    }
}

// Patterns: a shading pattern fill is cut like any fill; a tiling
// pattern (its cell is content) is refused.

const PATTERN_RES: &str = "/Pattern << /Sh << /PatternType 2 /Shading << /ShadingType 2 \
    /ColorSpace /DeviceRGB /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] \
    /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >> /Ti 40 0 R >>";

#[test]
fn a_shading_pattern_fill_is_cut() {
    let pdf = doc_with(
        PATTERN_RES,
        &[b"/Pattern cs /Sh scn 30 60 60 30 re f"],
        vec![],
    );
    let r = run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]).unwrap();
    assert_eq!(r.paths_cut, 1);
    assert_eq!(at(&r.output, 50, 75), 255);
    assert_ne!(at(&r.output, 35, 75), 255, "outside: kept");
}

#[test]
fn a_tiling_pattern_is_refused() {
    let tile = stream_obj(
        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 \
         /Resources << >>",
        b"0 0 5 5 re f",
    );
    let pdf = doc_with(
        PATTERN_RES,
        &[b"/Pattern cs /Ti scn 30 60 60 30 re f"],
        vec![(40, tile)],
    );
    let e = unsupported(run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]));
    assert!(e.contains("tiling pattern"), "{e}");
}

/// A watermark's /FixedPrint moves it away from /Rect when printed.
#[test]
fn a_fixed_print_watermark_is_refused() {
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Watermark /Rect [10 10 30 30] /F 4 \
              /FixedPrint << /Type /FixedPrint /Matrix [1 0 0 1 40 80] >> >>"
                .to_vec(),
        )],
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("FixedPrint"));
}

/// An original an annotation's appearance still draws is a display:
/// kept, reported.
#[test]
fn an_original_drawn_by_an_annotation_appearance_is_reported() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R /Annots [6 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 40 0 0 40 50 50 cm /Im1 Do Q")),
        (5, image_obj()),
        (
            6,
            b"<< /Type /Annot /Subtype /Square /Rect [150 150 190 190] /AP << /N 7 0 R >> >>"
                .to_vec(),
        ),
        (
            7,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 40 40] /Resources << /XObject << /I 5 0 R >> >>",
                b"q 40 0 0 40 0 0 cm /I Do Q",
            ),
        ),
    ]);
    let r = run(&pdf, vec![(40.0, 40.0, 100.0, 100.0)]).unwrap();
    assert_eq!(r.images_edited, 1);
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Image));
}

/// A dashed stroke is cut, each piece keeping the pattern where it
/// stood: outside the rect the pixels are those of the original.
#[test]
fn a_dashed_stroke_is_cut_keeping_its_pattern() {
    for c in [
        "[6 4] 3 d 2 w 10 100 m 190 100 l S",
        "[7 3 1 3] 0 d 2 w 1 J 20 20 m 20 180 l 180 180 l 180 20 l h S",
        "q 0.5 0 0 2 0 0 cm [8 4] 0 d 2 w 20 50 m 380 50 l S Q",
    ] {
        let pdf = one_page(c, "", "", vec![]);
        let rect = (50.0, 90.0, 100.0, 190.0);
        let r = run(&pdf, vec![rect]).unwrap();
        assert!(r.paths_cut >= 1, "{c}");
        let (a, b) = (reds(&pdf), reds(&r.output));
        let at = |img: &Vec<u8>, x: u32, y: u32| img[((200 - y.max(1)) * 200 + x) as usize];
        for y in (1..200).step_by(2) {
            for x in (0..200).step_by(1) {
                let (xf, yf) = (x as f64, y as f64);
                let clear = xf < rect.0 - 4.0
                    || xf > rect.2 + 4.0
                    || yf < rect.1 - 4.0
                    || yf > rect.3 + 4.0;
                if clear {
                    assert_eq!(at(&b, x, y), at(&a, x, y), "{c}: ({x}, {y})");
                } else if xf > rect.0 + 1.0
                    && xf < rect.2 - 1.0
                    && yf > rect.1 + 1.0
                    && yf < rect.3 - 1.0
                {
                    assert_eq!(at(&b, x, y), 255, "{c}: inside ({x}, {y})");
                }
            }
        }
    }
}

/// The red channel of page 1 at 72 dpi, row-major from the top.
fn reds(pdf: &[u8]) -> Vec<u8> {
    let doc = Document::from_bytes(pdf).unwrap();
    let p = kasane_pdf::render_page(&doc, &doc.pages()[0], &kasane_pdf::RenderOptions::default())
        .unwrap();
    let m = p.pixmap();
    let mut v = Vec::new();
    for y in 0..m.height() {
        for x in 0..m.width() {
            v.push(m.pixel(x, y).unwrap().red());
        }
    }
    v
}

// Review of the dash cut and patterns (fixed).

/// Pieces of a translucent dashed stroke would each be painted: where
/// they overlap the paint would show twice. Refused.
#[test]
fn a_translucent_dashed_stroke_is_not_cut_into_pieces() {
    let pdf = doc_with(
        "/ExtGState << /T << /CA 0.5 >> >>",
        &[b"/T gs [1000 1] 0 d 10 w 10 50 m 190 50 l 50 10 m 50 190 l S"],
        vec![],
    );
    let e = unsupported(run(&pdf, vec![(140.0, 40.0, 160.0, 60.0)]));
    assert!(e.contains("not opaque"), "{e}");
}

/// An odd dash array is written out in full, its phase reduced into
/// one period, so every reader places the dashes alike.
#[test]
fn an_odd_dash_array_is_written_in_full() {
    use kasane_pdf::Operation as O;
    let pdf = one_page("[7 3 2] 35 d 2 w 10 100 m 190 100 l S", "", "", vec![]);
    let r = run(&pdf, vec![(50.0, 90.0, 100.0, 110.0)]).unwrap();
    let doc = Document::from_bytes(&r.output).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    let dashes: Vec<_> = ops
        .iter()
        .filter_map(|o| match o {
            O::SetDash { dashes, phase } => Some((dashes.clone(), *phase)),
            _ => None,
        })
        .collect();
    assert!(dashes.len() >= 2);
    for (d, p) in dashes.iter().skip(1) {
        assert_eq!(d, &vec![7.0, 3.0, 2.0, 7.0, 3.0, 2.0]);
        assert!((0.0..24.0).contains(p), "{p}");
    }
}

/// A shading pattern with its own ExtGState (which may carry a soft
/// mask with content) is refused.
#[test]
fn a_shading_pattern_with_its_own_gstate_is_refused() {
    let pdf = doc_with(
        "/Pattern << /Sh << /PatternType 2 /ExtGState << /CA 1 >> /Shading << /ShadingType 2 \
         /ColorSpace /DeviceRGB /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] \
         /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >> >>",
        &[b"/Pattern cs /Sh scn 30 60 60 30 re f"],
        vec![],
    );
    let e = unsupported(run(&pdf, vec![(40.0, 40.0, 160.0, 160.0)]));
    assert!(e.contains("tiling pattern") || e.contains("pattern"), "{e}");
}

/// An /AP key other than /N, /R, /D is not an appearance: an original
/// it names may not stay.
#[test]
fn an_unknown_appearance_key_is_not_a_display() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R /Annots [6 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 40 0 0 40 50 50 cm /Im1 Do Q")),
        (5, image_obj()),
        (
            6,
            b"<< /Type /Annot /Subtype /Square /Rect [150 150 190 190] /AP << /N 7 0 R /Unused 5 0 R >> >>"
                .to_vec(),
        ),
        (7, stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 40 40]", b"0 0 10 10 re f")),
    ]);
    match run(&pdf, vec![(40.0, 40.0, 100.0, 100.0)]) {
        Ok(r) => assert!(!contains(&r.output, &secret_samples())),
        Err(e) => assert!(e.to_string().contains("original data"), "{e}"),
    }
}

// Non-embedded fonts: a reader draws them with a font of its choosing,
// so their ink extent is unknown and they are refused — except under a
// clip that is exactly a rectangle meeting no rect.

const HELV: &str = "/Font << /F1 20 0 R /H 50 0 R >>";

/// Non-embedded Helvetica with its AFM widths for codes 32 … 90.
fn helv() -> (u32, Vec<u8>) {
    let w = "278 278 355 556 556 889 667 191 333 333 389 584 278 333 278 278 556 556 556 556 556 \
             556 556 556 556 556 278 278 584 584 584 556 1015 667 667 722 722 667 611 778 722 278 \
             500 667 556 833 722 778 667 778 722 667 611 722 667 944 667 667 611";
    (
        50,
        format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 90 \
             /Widths [{w}] >>"
        )
        .into_bytes(),
    )
}

/// Even with every width given, a non-embedded font's glyphs may reach
/// anywhere: refused, wherever on the page they are.
#[test]
fn non_embedded_text_is_refused() {
    for content in [
        "BT /H 12 Tf 20 100 Td (PUBLIC SECRETWORD MORE) Tj ET",
        "BT /H 12 Tf 20 20 Td (FARAWAY) Tj ET",
    ] {
        let mut objs = font_objs();
        objs.push(helv());
        let pdf = doc_with(HELV, &[format!("{SECRET} {content}").as_bytes()], objs);
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("uncertain") && e.contains("non-embedded"), "{e}");
    }
}

/// A clip hides a non-embedded glyph's ink, not its code: still
/// refused (the code could stand in a rect).
#[test]
fn non_embedded_text_is_refused_even_clipped_away() {
    let mut objs = font_objs();
    objs.push(helv());
    let pdf = doc_with(
        HELV,
        &[
            format!("{SECRET} q 0 0 200 60 re W n BT /H 12 Tf 20 20 Td (FARAWAY) Tj ET Q")
                .as_bytes(),
        ],
        objs,
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("non-embedded"));
}

/// A non-embedded Standard 14 font without /Widths is laid out with
/// the Adobe AFM widths (Helvetica `A` 667, Times space 250, Courier
/// 600), as readers lay it out.
#[test]
fn standard_14_without_widths_uses_the_afm_widths() {
    let cases = [
        ("Helvetica", "AAAAX", 4.0 * 667.0),
        ("Times-Roman", "    X", 4.0 * 250.0),
        ("Courier-Bold", "iiiiX", 4.0 * 600.0),
        ("Arial,Bold", "WWWWX", 4.0 * 944.0),
    ];
    for (name, text, before) in cases {
        let pdf = doc_with(
            "/Font << /H 50 0 R >>",
            &[format!("BT /H 10 Tf 0 100 Td ({text}) Tj ET").as_bytes()],
            vec![(
                50,
                format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} >>").into_bytes(),
            )],
        );
        let doc = Document::from_bytes(&pdf).unwrap();
        let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
        let x = t.chars[4].origin.0;
        assert!((x - before / 100.0).abs() < 1e-9, "{name}: X at {x}");
    }
}

/// A Text annotation's icon is drawn away from /Rect at other zooms:
/// kept anywhere on an edited page it is refused; removed, it is not.
#[test]
fn a_text_annotation_icon_is_never_kept() {
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Text /Rect [40 100 41 101] /F 4 /Name /Note >>".to_vec(),
        )],
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("NoZoom"));
}

// Annotations meeting a rect: refused by default; with
// remove_annotations, markup annotations removed whole with their
// popups and replies.

fn annots_run(pdf: &[u8]) -> Result<kasane_pdf::Redaction, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_annotations(true),
    )
}

fn annot_page(annots: &str, objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    one_page(SECRET, &format!("/Annots [{annots}]"), "", objs)
}

#[test]
fn a_markup_annotation_meeting_a_rect_is_removed_whole_on_request() {
    let pdf = annot_page(
        "30 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 190 140] /Contents (SECRETNOTE) \
                  /DA (/Helv 10 Tf 0 g) /AP << /N 31 0 R >> >>"
                    .to_vec(),
            ),
            (
                31,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 90 40]",
                    b"0 0 1 rg 0 0 90 40 re f % APPEARANCEMARK",
                ),
            ),
        ],
    );
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("remove_annotations"), "{e}");
    let r = annots_run(&pdf).unwrap();
    assert_eq!(r.annotations_removed, 1);
    assert!(!contains(&r.output, b"SECRETNOTE"));
    assert!(!contains(&r.output, b"APPEARANCEMARK"));
    assert_eq!(
        at(&r.output, 180, 130),
        255,
        "its part outside the rect goes too"
    );
}

#[test]
fn popups_and_replies_go_with_a_removed_annotation() {
    let pdf = annot_page(
        "30 0 R 31 0 R 32 0 R 33 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Text /Rect [60 95 70 105] /Contents (SECRETNOTE) /Popup 31 0 R >>"
                    .to_vec(),
            ),
            (31, b"<< /Type /Annot /Subtype /Popup /Rect [150 150 190 190] /Parent 30 0 R >>".to_vec()),
            (
                32,
                b"<< /Type /Annot /Subtype /Text /Rect [170 10 180 20] /Contents (REPLYNOTE) /IRT 30 0 R >>"
                    .to_vec(),
            ),
            (33, b"<< /Type /Annot /Subtype /Square /Rect [10 170 20 180] /Contents (OTHERNOTE) >>".to_vec()),
        ],
    );
    let r = annots_run(&pdf).unwrap();
    assert_eq!(r.annotations_removed, 3);
    assert!(!contains(&r.output, b"SECRETNOTE") && !contains(&r.output, b"REPLYNOTE"));
    assert!(contains(&r.output, b"OTHERNOTE"));
}

#[test]
fn widgets_and_tagged_annotations_stay_refused() {
    let widget = annot_page(
        "30 0 R",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /Rect [60 95 70 105] /V (SECRET) >>"
                .to_vec(),
        )],
    );
    assert!(unsupported(annots_run(&widget)).contains("other than a link"));
    let tagged = one_page(
        SECRET,
        "/Annots [30 0 R] /StructParents 1",
        "/StructTreeRoot 40 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Square /Rect [60 95 70 105] /StructParent 0 >>"
                    .to_vec(),
            ),
            (40, b"<< /Type /StructTreeRoot >>".to_vec()),
        ],
    );
    assert!(unsupported(annots_run(&tagged)).contains("structure tree"));
}

// Review of annotation removal (fixed).

/// Something else referring to a removed annotation (an action, a
/// kept annotation's popup) would carry it into the output: refused.
#[test]
fn a_removed_annotation_referred_to_elsewhere_is_refused() {
    let open_action = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "/OpenAction << /S /Hide /T 30 0 R >>",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Square /Rect [60 95 70 105] /Contents (PIN) >>".to_vec(),
        )],
    );
    assert!(unsupported(annots_run(&open_action)).contains("refers to"));
    let popup = annot_page(
        "30 0 R 31 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Popup /Rect [60 95 70 105] >>".to_vec(),
            ),
            (
                31,
                b"<< /Type /Annot /Subtype /Square /Rect [150 150 160 160] /Popup 30 0 R >>"
                    .to_vec(),
            ),
        ],
    );
    assert!(unsupported(annots_run(&popup)).contains("refers to"));
}

/// What a removed annotation's appearance draws is an original too.
#[test]
fn a_removed_appearance_keeps_no_image() {
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R] /Resources << /Font << /F1 20 0 R >> /XObject << /Unused 32 0 R >> >>",
        "",
        vec![
            (30, b"<< /Type /Annot /Subtype /Stamp /Rect [60 95 100 105] /AP << /N 31 0 R >> >>".to_vec()),
            (
                31,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 40 10] /Resources << /XObject << /I 32 0 R >> >>",
                    b"q 40 0 0 10 0 0 cm /I Do Q",
                ),
            ),
            (32, image_obj()),
        ],
    );
    match annots_run(&pdf) {
        Ok(r) => assert!(!contains(&r.output, &secret_samples())),
        Err(e) => assert!(e.to_string().contains("original data"), "{e}"),
    }
}

/// An annotation without an appearance stream whose geometry lies
/// outside its /Rect is drawn there by readers: refused.
#[test]
fn appearance_less_geometry_outside_rect_is_refused() {
    let pdf = annot_page(
        "30 0 R",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Ink /Rect [150 150 190 180] /C [1 0 0] \
              /InkList [[60 100 100 100]] /BS << /W 2 >> >>"
                .to_vec(),
        )],
    );
    assert!(unsupported(annots_run(&pdf)).contains("outside its /Rect"));
}

/// A rich-text /RC stream is searched like a string.
#[test]
fn a_removed_rich_text_stream_is_searched() {
    let pdf = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "/Outlines 40 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /FreeText /Rect [60 95 100 105] /DA (/Helv 10 Tf) /RC 31 0 R >>"
                    .to_vec(),
            ),
            (31, stream_obj("", b"RICHSECRETNOTE")),
            (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
            (41, b"<< /Title (RICHSECRETNOTE) /Parent 40 0 R >>".to_vec()),
        ],
    );
    let r = annots_run(&pdf).unwrap();
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Text("RICHSECRETNOTE".into())));
}

/// Whatever keeps a removed annotation reachable — the page's own /AA,
/// a kept direct annotation, an /Annots array referred to elsewhere,
/// a structure element an outline keeps — fails verification; an
/// unreachable object referring to it does not matter.
#[test]
fn a_removed_annotation_kept_reachable_is_refused() {
    let square = || {
        (
            30,
            b"<< /Type /Annot /Subtype /Square /Rect [60 95 70 105] /Contents (PIN) >>".to_vec(),
        )
    };
    let cases = [
        one_page(SECRET, "/Annots [30 0 R] /AA << /O << /S /Hide /T 30 0 R >> >>", "", vec![square()]),
        one_page(
            SECRET,
            "/Annots [30 0 R << /Type /Annot /Subtype /Square /Rect [150 150 160 160] /Popup 30 0 R >>]",
            "",
            vec![square()],
        ),
        one_page(
            SECRET,
            "/Annots 35 0 R",
            "/OpenAction << /S /Hide /T 35 0 R >>",
            vec![square(), (35, b"[30 0 R]".to_vec())],
        ),
    ];
    for pdf in &cases {
        let e = unsupported(annots_run(pdf));
        assert!(e.contains("refers to"), "{e}");
    }
    let outline_se = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "/StructTreeRoot 40 0 R /Outlines 42 0 R",
        vec![
            square(),
            (40, b"<< /Type /StructTreeRoot /K 41 0 R >>".to_vec()),
            (
                41,
                b"<< /Type /StructElem /S /Annot /P 40 0 R /K << /Type /OBJR /Obj 30 0 R >> >>"
                    .to_vec(),
            ),
            (
                42,
                b"<< /Type /Outlines /First 43 0 R /Last 43 0 R /Count 1 >>".to_vec(),
            ),
            (43, b"<< /Title (x) /Parent 42 0 R /SE 41 0 R >>".to_vec()),
        ],
    );
    let doc = Document::from_bytes(&outline_se).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new()
            .remove_annotations(true)
            .remove_structure_tree(true),
    );
    match r {
        Ok(r) => assert!(!contains(&r.output, b"(PIN)")),
        Err(e) => assert!(e.to_string().contains("refers to"), "{e}"),
    }
    // Unreachable: object 36 refers to the annotation but nothing
    // refers to object 36.
    let unreachable = one_page(
        SECRET,
        "/Annots [30 0 R]",
        "",
        vec![square(), (36, b"<< /S /Hide /T 30 0 R >>".to_vec())],
    );
    let r = annots_run(&unreachable).unwrap();
    assert!(!contains(&r.output, b"(PIN)"));
}

/// A removed annotation whose numbers the writer normalises (−0, a
/// tiny real), or kept deep inside arrays, is still found.
#[test]
fn removed_annotation_prints_survive_normalisation_and_depth() {
    let square = |r: &str| {
        (
            30,
            format!("<< /Type /Annot /Subtype /Square /Rect [{r} 95 70 105] /Contents (PIN) >>")
                .into_bytes(),
        )
    };
    for r in ["-0.0", "0.00000000001"] {
        let pdf = one_page(
            SECRET,
            "/Annots [30 0 R] /AA << /O << /S /Hide /T 30 0 R >> >>",
            "",
            vec![square(r)],
        );
        assert!(unsupported(annots_run(&pdf)).contains("refers to"), "{r}");
    }
    let annot = "<< /Type /Annot /Subtype /Square /Rect [60 95 70 105] /Contents (PIN) >>";
    let deep = format!("{}{annot}{}", "[".repeat(70), "]".repeat(70));
    let pdf = one_page(
        SECRET,
        &format!("/Annots [{annot}]"),
        &format!("/Deep {deep}"),
        vec![],
    );
    assert!(unsupported(annots_run(&pdf)).contains("refers to"));
}

// Optional content: redaction takes it as drawn — what meets a rect is
// removed whether a viewer shows it or not.

#[test]
fn optional_content_is_taken_as_drawn() {
    let ocg = (40, b"<< /Type /OCG /Name (Hidden layer) >>".to_vec());
    let pdf = one_page(
        "/OC /L BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC \
         /OC /L BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC",
        "/Resources << /Font << /F1 20 0 R >> /Properties << /L 40 0 R >> >>",
        "/OCProperties << /OCGs [40 0 R] /D << /OFF [40 0 R] >> >>",
        vec![ocg.clone()],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(
        r.codes_removed, 10,
        "hidden content in the rect is removed too"
    );
    assert!(!contains(&r.output, b"SECRETTEXT"));
    assert!(contains(&r.output, b"PUBLICWORD"));
    // An optional content form is edited like any form.
    let form = form_page("/Fm Do", "/OC 40 0 R", SECRET, vec![ocg]);
    let r = run(&form, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
}

/// A shading fills the clip: accepted under a rectangular clip clear of
/// the rects, refused otherwise.
#[test]
fn a_shading_clipped_away_from_the_rects_is_kept() {
    let sh = "/Shading << /Sh << /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 200 0] \
              /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >>";
    let mut objs = font_objs();
    objs.push((99, b"<< >>".to_vec()));
    let ok = doc_with(
        &format!("/Font << /F1 20 0 R >> {sh}"),
        &[format!("{SECRET} q 0 0 200 40 re W n /Sh sh Q").as_bytes()],
        objs.clone(),
    );
    let r = run(&ok, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    assert_ne!(at(&r.output, 100, 20), 255, "the shading is kept");
    let refused = doc_with(
        &format!("/Font << /F1 20 0 R >> {sh}"),
        &[format!("{SECRET} q 0 0 200 100 re W n /Sh sh Q").as_bytes()],
        objs,
    );
    assert!(unsupported(run(&refused, vec![RECT])).contains("shading"));
}

// Pre-release review (P0s, fixed).

/// /ActualText on any tag replaces the shown text: refused, as on
/// /Span. Alternate descriptions and expansions likewise.
#[test]
fn replacement_text_on_any_tag_is_refused() {
    for props in [
        "/P << /ActualText (SECRETTEXT) >>",
        "/Figure << /Alt (SECRETTEXT) >>",
        "/Span << /E (SECRETTEXT) >>",
    ] {
        let pdf = one_page(&format!("{props} BDC {SECRET} EMC"), "", "", vec![]);
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("blockers"), "{props}: {e}");
    }
}

/// A code without a /Widths entry takes /MissingWidth, so what follows
/// it is placed where readers place it.
#[test]
fn missing_width_places_what_follows() {
    let pdf = doc_with(
        "/Font << /F1 20 0 R /H 50 0 R >>",
        &[b"BT /H 12 Tf 10 100 Td q 0 0 20 20 re W n (BBBB) Tj Q /F1 12 Tf (W) Tj ET"],
        {
            let mut o = font_objs();
            o.push((
                50,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 65 /LastChar 65 \
                  /Widths [500] /FontDescriptor << /Type /FontDescriptor /FontName /Helvetica \
                  /Flags 32 /MissingWidth 1000 >> >>"
                    .to_vec(),
            ));
            o
        },
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    assert!(
        (t.chars[4].origin.0 - 58.0).abs() < 1e-9,
        "{}",
        t.chars[4].origin.0
    );
    let r = embed_run(&pdf, vec![(55.0, 90.0, 80.0, 115.0)]).unwrap();
    assert_eq!(r.codes_removed, 1, "the W in the rect is removed");
}

/// Metrics follow the font's name, not its descriptor's flags:
/// Helvetica with ForceBold is still Helvetica's A (667).
#[test]
fn afm_widths_follow_the_name() {
    let pdf = doc_with(
        "/Font << /H 50 0 R >>",
        &[b"BT /H 10 Tf 0 100 Td (AAAAX) Tj ET"],
        vec![(
            50,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FontDescriptor << \
              /Type /FontDescriptor /FontName /Helvetica /Flags 262176 >> >>"
                .to_vec(),
        )],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    assert!(
        (t.chars[4].origin.0 - 26.68).abs() < 1e-9,
        "{}",
        t.chars[4].origin.0
    );
}

// Non-embedded Standard 14 fonts, with embed_substitute_fonts: the
// substitute is embedded first, so every reader draws exactly its
// glyphs and redaction decides by their ink.

fn embed_run(pdf: &[u8], rects: Vec<(f64, f64, f64, f64)>) -> Result<kasane_pdf::Redaction, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(
        &doc,
        &[(0, rects)],
        &RedactOptions::new().embed_substitute_fonts(true),
    )
}

#[test]
fn non_embedded_standard_14_text_is_redacted_after_embedding() {
    for base in ["Helvetica", "Times-Roman", "Courier-Bold", "Arial,Italic"] {
        let pdf = doc_with(
            "/Font << /H 50 0 R >>",
            &[b"BT /H 12 Tf 20 100 Td (PUBLIC SECRETWORD MORE) Tj ET BT /H 12 Tf 20 40 Td [(KEEP) -250 (LINE)] TJ ET"],
            vec![(
                50,
                format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>")
                    .into_bytes(),
            )],
        );
        assert!(
            unsupported(run(&pdf, vec![RECT])).contains("non-embedded"),
            "{base}"
        );
        // Locate SECRETWORD from the input's extracted positions.
        let doc = Document::from_bytes(&pdf).unwrap();
        let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
        let x0 = t.chars[7].origin.0;
        let x1 = t.chars[17].origin.0;
        let rect = (x0 + 0.5, 95.0, x1 - 0.5, 112.0);
        let r = embed_run(&pdf, vec![rect]).unwrap();
        assert_eq!(r.fonts_embedded, 1, "{base}");
        // SECRETWORD, and a space whose (empty) cell meets the rect.
        assert!(
            (10..=12).contains(&r.codes_removed),
            "{base}: {}",
            r.codes_removed
        );
        let text = page_text(&r.output, 0);
        assert!(
            !text.contains("SECRET") && text.contains("PUBLIC") && text.contains("MORE"),
            "{base}: {text}"
        );
        assert!(!contains(&r.output, b"SECRETWORD"));
        // Outside the rect the page draws exactly what this crate drew
        // for the non-embedded font (same glyphs, same positions).
        let (a, b) = (reds(&pdf), reds(&r.output));
        for y in 0..200u32 {
            for x in 0..200u32 {
                let (xf, yf) = (x as f64 + 0.5, 199.5 - y as f64);
                if xf < rect.0 - 2.0 || xf > rect.2 + 2.0 || yf < rect.1 - 2.0 || yf > rect.3 + 2.0
                {
                    let i = (y * 200 + x) as usize;
                    assert_eq!(a[i], b[i], "{base}: ({x}, {y})");
                }
            }
        }
    }
}

/// Fonts this mode does not embed stay refused.
#[test]
fn fonts_not_embedded_by_the_option_stay_refused() {
    let pdf = doc_with(
        "/Font << /H 50 0 R >>",
        &[b"BT /H 12 Tf 50 100 Td (SECRETTEXT) Tj ET"],
        vec![(
            50,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>".to_vec(),
        )],
    );
    assert!(embed_run(&pdf, vec![RECT]).is_err());
}

// Caps at a cut: the band of a perpendicular crossing ends exactly at
// the rect. Square caps (pulled back by their extent) and round-capped
// pieces whose both ends are cuts (stroked with square caps) lose
// nothing outside; a round-capped piece keeping an original end keeps
// its round cap and loses the cap's corners at the cut.

/// Pixels that differ between two renders away from `rect` (1 px
/// margin); asserts nothing is drawn well inside it.
fn lost_outside(a: &[u8], b: &[u8], rect: (f64, f64, f64, f64)) -> usize {
    let mut lost = 0;
    for y in 0..200u32 {
        for x in 0..200u32 {
            let (xf, yf) = (x as f64 + 0.5, 199.5 - y as f64);
            let i = (y * 200 + x) as usize;
            if xf < rect.0 - 1.0 || xf > rect.2 + 1.0 || yf < rect.1 - 1.0 || yf > rect.3 + 1.0 {
                lost += usize::from(a[i] != b[i]);
            } else if xf > rect.0 + 1.0
                && xf < rect.2 - 1.0
                && yf > rect.1 + 1.0
                && yf < rect.3 - 1.0
            {
                assert_eq!(b[i], 255, "drawn inside at ({x}, {y})");
            }
        }
    }
    lost
}

#[test]
fn square_caps_and_round_pieces_between_cuts_lose_nothing() {
    // Square caps, horizontal and under a non-uniform CTM.
    for c in [
        "10 w 2 J 20 100 m 180 100 l S",
        "q 2 0 0 0.5 0 0 cm 6 w 2 J 10 200 m 90 200 l S Q",
    ] {
        let pdf = one_page(c, "", "", vec![]);
        let rect = (90.0, 90.0, 110.0, 160.0);
        let r = run(&pdf, vec![rect]).unwrap();
        assert_eq!(lost_outside(&reds(&pdf), &reds(&r.output), rect), 0, "{c}");
    }
    // Round caps: the middle piece between two rects is square-capped.
    let pdf = one_page("10 w 1 J 20 100 m 180 100 l S", "", "", vec![]);
    let rects = vec![(60.0, 90.0, 70.0, 110.0), (130.0, 90.0, 140.0, 110.0)];
    let r = run(&pdf, rects.clone()).unwrap();
    let (a, b) = (reds(&pdf), reds(&r.output));
    // Between the rects (x 72…128) nothing differs.
    for y in 90..111u32 {
        for x in 72..128u32 {
            let i = ((200 - y) * 200 + x) as usize;
            assert_eq!(a[i], b[i], "({x}, {y})");
        }
    }
    // A closed round-capped outline cut once: its one piece has cuts at
    // both ends.
    let pdf = one_page(
        "6 w 1 J 30 30 m 30 170 l 170 170 l 170 30 l h S",
        "",
        "",
        vec![],
    );
    let rect = (90.0, 160.0, 110.0, 180.0);
    let r = run(&pdf, vec![rect]).unwrap();
    assert_eq!(lost_outside(&reds(&pdf), &reds(&r.output), rect), 0);
}

/// A round-capped piece that keeps an original end loses only its
/// cut cap's corners; an oblique crossing also loses the sliver beyond
/// the perpendicular cut.
#[test]
fn round_caps_with_an_original_end_lose_little() {
    for (c, bound) in [
        ("10 w 1 J 20 100 m 180 100 l S", 30),
        ("8 w 1 J 1 j 20 40 m 100 150 l 180 40 l S", 150),
    ] {
        let pdf = one_page(c, "", "", vec![]);
        let rect = (90.0, 90.0, 110.0, 160.0);
        let r = run(&pdf, vec![rect]).unwrap();
        let lost = lost_outside(&reds(&pdf), &reds(&r.output), rect);
        assert!(lost <= bound, "{c}: {lost} pixels lost outside");
    }
}

/// Glyph-name aliases readers resolve differently (`sfthyphen`,
/// `nbspace`) are written as canonical `uniXXXX` names: this crate and
/// readers then draw the same glyph, and what is drawn in the rect goes.
#[test]
fn embedded_substitutes_use_canonical_glyph_names() {
    let pdf = doc_with(
        "/Font << /F 50 0 R >>",
        &[b"BT /F 80 Tf 50 50 Td <49> Tj ET BT /F 12 Tf 20 150 Td (KEEP) Tj ET"],
        vec![(
            50,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << \
              /BaseEncoding /WinAnsiEncoding /Differences [73 /sfthyphen] >> >>"
                .to_vec(),
        )],
    );
    let r = embed_run(&pdf, vec![(50.0, 40.0, 100.0, 100.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(page_text(&r.output, 0).contains("KEEP"));
    // The written font names code 73 `uni00AD`, not the alias.
    let doc = Document::from_bytes(&r.output).unwrap();
    let page = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let res = doc
        .deref(
            page.as_dict()
                .unwrap()
                .get(b"Resources".as_slice())
                .unwrap(),
        )
        .unwrap();
    let fonts = doc
        .deref(res.as_dict().unwrap().get(b"Font".as_slice()).unwrap())
        .unwrap();
    let font = doc
        .deref(fonts.as_dict().unwrap().get(b"F".as_slice()).unwrap())
        .unwrap();
    let enc = doc
        .deref(font.as_dict().unwrap().get(b"Encoding".as_slice()).unwrap())
        .unwrap();
    let diffs = enc
        .as_dict()
        .unwrap()
        .get(b"Differences".as_slice())
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert_eq!(diffs[1 + 73].as_name(), Some(b"uni00AD".as_slice()));
}

// Curves crossing a rect: stroked curves are cut by subdivision into
// exact sub-curves; fills and dashed curves stay refused.

#[test]
fn stroked_curves_crossing_a_rect_are_cut() {
    let circle = "100 160 m 133.1 160 160 133.1 160 100 c 160 66.9 133.1 40 100 40 c \
                  66.9 40 40 66.9 40 100 c 40 133.1 66.9 160 100 160 c h";
    for c in [
        "4 w 20 100 m 60 180 140 20 180 100 c S".to_string(),
        format!("3 w 1 J {circle} S"),
        "q 1.5 0.5 -0.5 1 0 0 cm 3 w 10 60 m 40 120 80 0 110 60 c S Q".to_string(),
        "2 w 10 10 m 60 60 l 90 150 120 150 150 60 c 190 10 l S".to_string(),
    ] {
        let pdf = one_page(&c, "", "", vec![]);
        let rect = (85.0, 80.0, 115.0, 175.0);
        let r = run(&pdf, vec![rect]).unwrap();
        assert!(r.paths_cut >= 1, "{c}");
        let lost = lost_outside(&reds(&pdf), &reds(&r.output), rect);
        // The band within the stroke's reach (√2 × half width, plus the
        // subdivision's last step) of the rect may go; far away nothing.
        let (a, b) = (reds(&pdf), reds(&r.output));
        for y in 0..200u32 {
            for x in 0..200u32 {
                let (xf, yf) = (x as f64 + 0.5, 199.5 - y as f64);
                if xf < rect.0 - 8.0 || xf > rect.2 + 8.0 || yf < rect.1 - 8.0 || yf > rect.3 + 8.0
                {
                    // The kept sub-curves are the original geometry; a
                    // rasteriser flattens them a little differently, so
                    // edge anti-aliasing may shift — but no ink is lost.
                    let i = (y * 200 + x) as usize;
                    assert!(
                        !(a[i] < 64 && b[i] > 192),
                        "{c}: far outside ({x}, {y}) {} → {}",
                        a[i],
                        b[i]
                    );
                }
            }
        }
        assert!(lost < 200, "{c}: {lost} pixels lost next to the rect");
    }
}

#[test]
fn curved_fills_and_dashed_curves_are_refused() {
    let fill = one_page("0 0 1 rg 30 60 m 90 60 90 90 30 90 c f", "", "", vec![]);
    assert!(unsupported(run(&fill, vec![(40.0, 40.0, 160.0, 160.0)]))
        .contains("crossing a rect's edge"));
    let dashed = one_page(
        "[4 2] 0 d 2 w 20 100 m 60 180 140 20 180 100 c S",
        "",
        "",
        vec![],
    );
    assert!(unsupported(run(&dashed, vec![(85.0, 80.0, 115.0, 175.0)])).contains("dashed curve"));
}

/// Invisible (OCR) and stroked text are removed by where they stand —
/// invisible codes by a generous box around their advance, stroked
/// ones by their outline grown by the stroke — the rest kept.
#[test]
fn invisible_and_stroked_text_are_redacted() {
    for mode in [3, 1, 2] {
        let pdf = one_page(
            &format!(
                "BT /F1 12 Tf {mode} Tr 50 100 Td (SECRETTEXT) Tj ET \
                 BT /F1 12 Tf {mode} Tr 10 20 Td (PUBLICWORD) Tj ET"
            ),
            "",
            "",
            vec![],
        );
        let r = run(&pdf, vec![RECT]).unwrap();
        assert_eq!(r.codes_removed, 10, "Tr {mode}");
        let doc = Document::from_bytes(&r.output).unwrap();
        let t = extract_text(
            &doc,
            &doc.pages()[0],
            &TextOptions::default().with_hidden_content(true),
        )
        .unwrap();
        let text: String = t.chars.iter().filter_map(|c| c.unicode.clone()).collect();
        assert!(
            !text.contains("SECRET") && text.contains("PUBLICWORD"),
            "Tr {mode}: {text}"
        );
    }
}

// Review of the cap and curve cuts (fixed).

/// A zero-length segment does not hide the join of the segments around
/// it.
#[test]
fn zero_length_curve_segments_do_not_hide_joins() {
    for c in [
        "10 w 0 j 20 M 50 50 m 100 50 l 100 50 100 50 100 50 c 50 60 l S",
        "10 w 0 j 20 M 50 50 m 100 50 l 100 50 l 50 60 0 0 50 60 c S",
    ] {
        let pdf = one_page(c, "", "", vec![]);
        let e = unsupported(run(&pdf, vec![(125.0, 44.0, 130.0, 46.0)]));
        assert!(e.contains("join"), "{c}: {e}");
    }
}

/// A piece starting at an original corner keeps round caps: a square
/// cap there would paint where the original had a join.
#[test]
fn square_caps_only_between_cuts_inside_segments() {
    let pdf = one_page(
        "10 w 1 J 1 j 50 50 m 100 50 l 150 100 l 250 100 l S",
        "",
        "",
        vec![],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(
            0,
            vec![(104.0, 45.0, 105.0, 46.0), (180.0, 90.0, 190.0, 110.0)],
        )],
        &RedactOptions::new(),
    )
    .unwrap();
    let ops = Document::from_bytes(&r.output).unwrap();
    let ops = ops.page_operations(&ops.pages()[0]).unwrap();
    assert!(
        !ops.iter()
            .any(|o| matches!(o, kasane_pdf::Operation::SetLineCap(2))),
        "no square caps"
    );
}

/// Smoothing (`/Interpolate true`) and optional content do not change
/// which samples are drawn inside a rect: such images are edited.
#[test]
fn interpolated_and_optional_images_are_edited() {
    for extra in ["/Interpolate true", "/OC 40 0 R"] {
        let img = stream_obj(
            &format!(
                "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB \
                 /BitsPerComponent 8 {extra}"
            ),
            &secret_samples(),
        );
        let pdf = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
            (5, img),
            (40, b"<< /Type /OCG /Name (L) >>".to_vec()),
        ]);
        let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
        assert_eq!(r.pixels_changed, 256, "{extra}");
        assert!(!contains(&r.output, &secret_samples()), "{extra}");
    }
}

/// Under a strongly anisotropic text matrix readers disagree where a
/// glyph is drawn: refused.
#[test]
fn strongly_anisotropic_text_is_refused() {
    let pdf = one_page(
        "BT /F1 20 Tf .1 0 0 10 100 100 Tm (W) Tj ET",
        "",
        "",
        vec![],
    );
    let e = unsupported(run(&pdf, vec![(150.0, 150.0, 160.0, 160.0)]));
    assert!(e.contains("anisotropic"), "{e}");
    // A CTM cancelling the text matrix's stretch does not hide it.
    let cancelled = one_page(
        "10 0 0 .1 0 0 cm BT /F1 20 Tf .1 0 0 10 20 2000 Tm (W) Tj ET",
        "",
        "",
        vec![],
    );
    let e = unsupported(run(&cancelled, vec![(150.0, 150.0, 160.0, 160.0)]));
    assert!(e.contains("anisotropic"), "{e}");
    let ok = one_page(
        "BT /F1 20 Tf .25 0 0 4 100 100 Tm (W) Tj ET",
        "",
        "",
        vec![],
    );
    run(&ok, vec![(150.0, 150.0, 160.0, 160.0)]).unwrap();
}

/// Invisible text clipped away from a rect is still removed.
#[test]
fn clipped_invisible_text_in_the_rect_is_removed() {
    let pdf = one_page(
        "q 0 0 10 10 re W n BT /F1 12 Tf 3 Tr 50 100 Td (SECRETTEXT) Tj ET Q",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
}

/// /UserUnit sets the physical size of a unit; redaction rects are in
/// the page's user space, so it changes nothing.
#[test]
fn user_unit_pages_are_redacted_in_user_space() {
    let pdf = one_page(SECRET_AND_PUBLIC, "/UserUnit 3", "", vec![]);
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    let text = page_text(&r.output, 0);
    assert!(
        text.contains("PUBLICWORD") && !text.contains("SECRET"),
        "{text}"
    );
}

const SECRET_AND_PUBLIC: &str =
    "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET";

/// CMYK, Indexed and ICCBased images are edited by zeroing their
/// samples (a zeroed index selects the palette's first colour; zeroed
/// samples carry nothing whatever colour they show).
#[test]
fn cmyk_indexed_and_icc_images_are_edited() {
    type Case = (&'static str, usize, Vec<(u32, Vec<u8>)>);
    let cases: [Case; 3] = [
        ("/DeviceCMYK", 4, vec![]),
        (
            "[/Indexed /DeviceRGB 255 41 0 R]",
            1,
            vec![(41, stream_obj("", &[7u8; 768]))],
        ),
        (
            "[/ICCBased 42 0 R]",
            3,
            vec![(42, stream_obj("/N 3 /Alternate /DeviceRGB", b"profile"))],
        ),
    ];
    for (cs, n, extra) in cases {
        let samples: Vec<u8> = (0..16 * 16 * n)
            .map(|i| ((i * 37 + 11) % 251 + 1) as u8)
            .collect();
        let img = stream_obj(
            &format!("/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace {cs} /BitsPerComponent 8"),
            &samples,
        );
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
            (5, img),
        ];
        objs.extend(extra);
        let pdf = build_pdf(&objs);
        let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
        assert_eq!(r.pixels_changed, 256, "{cs}");
        assert!(!contains(&r.output, &samples[..64]), "{cs}");
    }
}

/// A JPEG of palette indices is not rewritten (a decoder's rounding by
/// one changes an index — any colour — outside the rects); an Indexed
/// space with an invalid hival or an Indexed base is refused.
#[test]
fn indexed_jpegs_and_invalid_indexed_spaces_are_refused() {
    for (cs, filter, res) in [
        ("[/Indexed /DeviceRGB 255 41 0 R]", "/Filter /DCTDecode", ""),
        (
            "/CS0",
            "/Filter /DCTDecode",
            "/ColorSpace << /CS0 [/Indexed /DeviceRGB 255 41 0 R] >>",
        ),
        ("[/Indexed /DeviceRGB -1 41 0 R]", "", ""),
        (
            "[/Indexed [/Indexed /DeviceRGB 255 41 0 R] 255 41 0 R]",
            "",
            "",
        ),
    ] {
        let img = stream_obj(
            &format!(
                "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace {cs} \
                 /BitsPerComponent 8 {filter}"
            ),
            &[1u8; 256],
        );
        let pdf = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                     /Resources << /XObject << /Im1 5 0 R >> {res} >> /Contents 4 0 R >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
            (5, img),
            (41, stream_obj("", &[7u8; 768])),
        ]);
        assert!(
            run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).is_err(),
            "{cs} {filter}"
        );
    }
}

// Type 3 fonts: CharProcs made of paths are evaluated (their ink, under
// the FontMatrix, united with the advance cell); anything else in a
// CharProc is refused.

/// A page showing `text` (codes 65…) in a Type 3 font whose glyph `A`
/// is `proc_a` and `B` is `proc_b`, with `font_extra` in the font dict.
fn type3_page(
    content: &str,
    proc_a: &str,
    proc_b: &str,
    font_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /T3 60 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            60,
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 61 0 R /B 62 0 R >> \
                 /Encoding << /Type /Encoding /Differences [65 /A /B] >> /FirstChar 65 \
                 /LastChar 66 /Widths [1000 1000] /Resources << >> {font_extra} >>"
            )
            .into_bytes(),
        ),
        (61, stream_obj("", proc_a.as_bytes())),
        (62, stream_obj("", proc_b.as_bytes())),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

const T3_FILL: &str = "1000 0 0 0 800 800 d1 100 0 600 700 re f";

/// Code positions: 10 pt glyphs at x = 20, 30, 40, … on y = 100.
const T3_TEXT: &str = "BT /T3 10 Tf 20 100 Td (AAAAAA) Tj ET";

#[test]
fn type3_fill_glyphs_are_redacted() {
    let pdf = type3_page(T3_TEXT, T3_FILL, T3_FILL, "", vec![]);
    // The third and fourth glyphs (x 40…50, 50…60).
    let r = run(&pdf, vec![(41.5, 99.0, 55.0, 108.0)]).unwrap();
    assert_eq!(r.codes_removed, 2);
    let (a, b) = (reds(&pdf), reds(&r.output));
    for y in 0..200u32 {
        for x in 0..200u32 {
            let (xf, yf) = (x as f64 + 0.5, 199.5 - y as f64);
            let i = (y * 200 + x) as usize;
            if (39.0..=61.0).contains(&xf) && (95.0..=110.0).contains(&yf) {
                if (42.0..55.0).contains(&xf) && (100.0..106.0).contains(&yf) {
                    assert_eq!(b[i], 255, "inside ({x}, {y})");
                }
            } else {
                assert_eq!(a[i], b[i], "outside ({x}, {y})");
            }
        }
    }
}

/// Strokes are grown by their reach; the FontMatrix (rotated, sheared)
/// places the ink; a glyph painting beyond /FontBBox and beyond its
/// cell is found where it paints; a clip in the CharProc decides
/// nothing.
#[test]
fn type3_ink_is_taken_where_it_is_painted() {
    // A stroke 400 glyph units wide reaching 200 below the baseline.
    let stroked = type3_page(
        T3_TEXT,
        "1000 0 d0 400 w 500 0 m 500 0 l 0 J S",
        T3_FILL,
        "",
        vec![],
    );
    let r = run(&stroked, vec![(20.0, 97.0, 200.0, 98.5)]).unwrap();
    assert_eq!(
        r.codes_removed, 6,
        "the stroke reaches 2 pt below the baseline"
    );
    // Ink 3000 glyph units to the right of the glyph (30 pt), outside
    // /FontBBox and the cell.
    let far = type3_page(
        "BT /T3 10 Tf 20 100 Td (A) Tj ET",
        "1000 0 d0 3000 0 400 400 re f",
        T3_FILL,
        "",
        vec![],
    );
    let r = run(&far, vec![(49.0, 99.0, 55.0, 105.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    // A sheared, rotated FontMatrix.
    let pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (A) Tj ET",
        T3_FILL,
        T3_FILL,
        "/FontMatrix [0 0.001 -0.001 0.0005 0 0]",
        vec![],
    );
    let r = run(&pdf, vec![(10.0, 101.0, 19.0, 107.0)]).unwrap();
    assert_eq!(
        r.codes_removed, 1,
        "the rotated glyph lies left of its origin"
    );
    // A clip in the CharProc hides the ink, not the glyph.
    let clipped = type3_page(
        T3_TEXT,
        "1000 0 d0 0 0 1 1 re W n 100 0 600 700 re f",
        T3_FILL,
        "",
        vec![],
    );
    let r = run(&clipped, vec![(41.5, 99.0, 45.0, 106.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
}

/// The same glyph inside and outside the rect: only the one inside goes.
#[test]
fn type3_same_glyph_inside_and_outside() {
    let pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj 0 -50 Td (AB) Tj ET",
        T3_FILL,
        T3_FILL,
        "",
        vec![],
    );
    let r = run(&pdf, vec![(21.0, 99.0, 29.0, 108.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert_ne!(at(&r.output, 25, 53), 255, "the same glyph below is kept");
}

/// CharProcs with images, XObjects, text, ExtGState, patterns, shadings
/// or hairlines are refused; so are non-finite coordinates.
#[test]
fn type3_charprocs_beyond_paths_are_refused() {
    for proc in [
        "1000 0 d0 q 100 0 0 100 0 0 cm /Im Do Q",
        "1000 0 d0 BI /W 1 /H 1 /CS /G /BPC 8 ID \u{0} EI",
        "1000 0 d0 BT /T3 1 Tf (A) Tj ET",
        "1000 0 d0 /G gs 0 0 10 10 re f",
        "1000 0 d0 /Pattern cs /P scn 0 0 10 10 re f",
        "1000 0 d0 /Sh sh",
        "1000 0 d0 0 w 0 0 m 10 10 l S",
        "1000 0 d0 /P << /ActualText (X) >> BDC 0 0 10 10 re f EMC",
    ] {
        let pdf = type3_page(T3_TEXT, proc, T3_FILL, "", vec![]);
        let e = unsupported(run(&pdf, vec![(41.5, 99.0, 55.0, 108.0)]));
        assert!(e.contains("Type 3"), "{proc}: {e}");
    }
    let huge = type3_page(
        T3_TEXT,
        "1000 0 d0 1e308 1e308 m -1e308 -1e308 l 1e308 w S",
        T3_FILL,
        "",
        vec![],
    );
    assert!(run(&huge, vec![(41.5, 99.0, 55.0, 108.0)]).is_err());
}

/// An ICCBased space whose /Alternate is not a device space (a reader
/// may fall back to it — an Indexed one makes samples palette indices)
/// is refused.
#[test]
fn icc_with_a_non_device_alternate_is_refused() {
    let img = stream_obj(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace [/ICCBased 42 0 R] \
         /BitsPerComponent 8",
        &[1u8; 256],
    );
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (5, img),
        (
            42,
            stream_obj(
                "/N 1 /Alternate [/Indexed /DeviceRGB 255 41 0 R]",
                b"profile",
            ),
        ),
        (41, stream_obj("", &[7u8; 768])),
    ]);
    assert!(run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).is_err());
}

/// A CharProc reached through a reference cycle, a singular or
/// strongly anisotropic FontMatrix, and malformed CharProcs (an
/// unbalanced Q, a non-stream) are refused.
#[test]
fn type3_cycles_and_degenerate_fonts_are_refused() {
    let cyclic = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /T3 60 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", T3_TEXT.as_bytes())),
        (
            60,
            b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
              /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 61 0 R >> \
              /Encoding << /Differences [65 /A] >> /FirstChar 65 /LastChar 65 \
              /Widths [1000] /Resources << >> >>"
                .to_vec(),
        ),
        (61, b"62 0 R".to_vec()),
        (62, b"61 0 R".to_vec()),
    ]);
    assert!(run(&cyclic, vec![(41.5, 99.0, 55.0, 108.0)]).is_err());
    for fm in ["[0 0 0 0 0 0]", "[0.001 0 0 0.00001 0 0]"] {
        let pdf = type3_page(
            T3_TEXT,
            T3_FILL,
            T3_FILL,
            &format!("/FontMatrix {fm}"),
            vec![],
        );
        let e = unsupported(run(&pdf, vec![(41.5, 99.0, 55.0, 108.0)]));
        assert!(e.contains("anisotropic"), "{fm}: {e}");
    }
    let unbalanced = type3_page(T3_TEXT, "1000 0 d0 Q 0 0 10 10 re f", T3_FILL, "", vec![]);
    assert!(run(&unbalanced, vec![(41.5, 99.0, 55.0, 108.0)]).is_err());
}
