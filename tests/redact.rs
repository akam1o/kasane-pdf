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

/// Alternate text in the structure tree may repeat what is removed: the
/// touched element loses it, the tree stays; or the tree is removed.
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
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(!r.structure_tree_removed);
    assert_eq!(r.structure_texts_removed, 1);
    assert!(!contains(&r.output, b"SECRETTEXT"));
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
/// outside is kept. A link the structure tree refers to leaves the
/// tree with it.
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
    let r = run(&tagged, vec![RECT]).unwrap();
    assert_eq!(r.structure_items_removed, 1);
    assert!(!contains(&r.output, b"SECRETLINK"));
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
    for g in ["Masked", "F"] {
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
fn widgets_go_with_remove_annotations_and_tagged_annotations_leave_the_tree() {
    // A widget of no interactive form: removed whole, its value with it
    // (0.9; refused before). Form fields: tests/redact_forms.rs.
    let widget = annot_page(
        "30 0 R",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /Rect [60 95 70 105] /V (WIDGETVALUE) >>"
                .to_vec(),
        )],
    );
    let r = annots_run(&widget).unwrap();
    assert_eq!(r.annotations_removed, 1);
    assert!(!contains(&r.output, b"WIDGETVALUE"));
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
            (
                40,
                b"<< /Type /StructTreeRoot /K 41 0 R /ParentTree << /Nums [0 41 0 R] >> >>"
                    .to_vec(),
            ),
            (
                41,
                b"<< /Type /StructElem /S /Annot /P 40 0 R /Alt (SECRETNOTE) \
                  /K << /Type /OBJR /Obj 30 0 R >> >>"
                    .to_vec(),
            ),
        ],
    );
    let r = annots_run(&tagged).unwrap();
    assert_eq!(r.annotations_removed, 1);
    assert_eq!(r.structure_items_removed, 1);
    assert_eq!(r.structure_texts_removed, 1);
    assert!(!contains(&r.output, b"SECRETNOTE"));
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

/// An annotation without an appearance stream is drawn from its
/// geometry, inside its /Rect or not: its extent is both. Geometry
/// meeting a rect makes it a hit (removed on request, else refused);
/// geometry clear of every rect keeps it.
#[test]
fn appearance_less_geometry_outside_rect_counts_where_it_is() {
    let ink = |list: &str| {
        annot_page(
            "30 0 R",
            vec![(
                30,
                format!(
                    "<< /Type /Annot /Subtype /Ink /Rect [150 150 190 180] /C [1 0 0] \
                     /InkList [[{list}]] /BS << /W 2 >> >>"
                )
                .into_bytes(),
            )],
        )
    };
    let r = annots_run(&ink("60 100 100 100")).unwrap();
    assert_eq!(r.annotations_removed, 1);
    assert!(unsupported(run(&ink("60 100 100 100"), vec![RECT])).contains("other than a link"));
    let r = annots_run(&ink("10 10 30 30")).unwrap();
    assert_eq!(r.annotations_removed, 0, "clear of the rect: kept");
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

/// Text-bearing marked-content properties (/ActualText on any tag,
/// /Alt, /E) written in the content: where anything in the sequence is
/// edited they are dropped (other keys stay); an untouched sequence
/// keeps them. Named properties (the text would stay in the resources)
/// and marked-content points stay refused.
#[test]
fn replacement_text_of_edited_sequences_is_dropped() {
    for props in [
        "/P << /ActualText (SECRETALT) /MCID 3 >>",
        "/Figure << /Alt (SECRETALT) /MCID 3 >>",
        "/Span << /E (SECRETALT) /MCID 3 >>",
    ] {
        // Nested: the outer sequence holds the edited text too.
        let pdf = one_page(
            &format!(
                "{props} BDC /Span << /ActualText (INNERALT) >> BDC {SECRET} EMC EMC \
                 /Span << /ActualText (KEPTALT) >> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC"
            ),
            "",
            "",
            vec![],
        );
        let r = run(&pdf, vec![RECT]).unwrap();
        assert_eq!(r.codes_removed, 10, "{props}");
        assert!(
            !contains(&r.output, b"SECRETALT") && !contains(&r.output, b"INNERALT"),
            "{props}"
        );
        assert!(
            contains(&r.output, b"KEPTALT"),
            "the untouched sequence keeps its text"
        );
        let doc = Document::from_bytes(&r.output).unwrap();
        let ops = doc.page_operations(&doc.pages()[0]).unwrap();
        assert!(
            ops.iter().any(
                |o| matches!(o, kasane_pdf::Operation::BeginMarkedContentWithProps {
                properties: kasane_pdf::Object::Dict(d), .. } if d.contains_key(b"MCID".as_slice()))
            ),
            "/MCID stays"
        );
    }
    // A sequence clear of the rects keeps its text, the page is accepted.
    let pdf = one_page(
        &format!("/Span << /ActualText (KEPTALT) >> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC {SECRET}"),
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(contains(&r.output, b"KEPTALT"));
    // Named properties and points stay refused.
    let named = doc_with(
        "/Font << /F1 20 0 R >> /Properties << /P0 << /ActualText (SECRETALT) >> >>",
        &[format!("/Span /P0 BDC {SECRET} EMC").as_bytes()],
        font_objs(),
    );
    let point = one_page(
        &format!("/Span << /ActualText (SECRETALT) >> DP {SECRET}"),
        "",
        "",
        vec![],
    );
    for pdf in [named, point] {
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("ActualText"), "{e}");
    }
}

/// Dropped replacement text found again elsewhere is reported.
#[test]
fn dropped_replacement_text_elsewhere_is_reported() {
    let pdf = doc_with(
        "/Font << /F1 20 0 R >>",
        &[
            format!("/Span << /ActualText (CONFIDENTIAL) >> BDC {SECRET} EMC").as_bytes(),
            b"BT /F1 12 Tf 10 20 Td (CONFIDENTIAL) Tj ET",
        ],
        font_objs(),
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(
        r.elsewhere.iter().any(
            |e| matches!(&e.removed, kasane_pdf::Removed::Text(t) if t.contains("CONFIDENTIAL"))
        ),
        "{:?}",
        r.elsewhere
    );
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
    // An object in `extra` replaces the one of the same number.
    objs.retain(|(n, _)| !extra.iter().any(|(m, _)| m == n));
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
        "1000 0 d0 BT /T3 1 Tf (A) Tj ET",
        "1000 0 d0 /G gs 0 0 10 10 re f",
        "1000 0 d0 /Pattern cs /P scn 0 0 10 10 re f",
        "1000 0 d0 /Sh sh",
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

/// One 16×16 image drawn at (50, 50)–(150, 150) with `img_extra` in its
/// dictionary (its `/ColorSpace` among them), plus `objs`.
fn image_page(img_extra: &str, samples: &[u8], objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut o = vec![
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
                &format!("/Type /XObject /Subtype /Image /Width 16 /Height 16 /BitsPerComponent 8 {img_extra}"),
                samples,
            ),
        ),
    ];
    o.extend(objs);
    build_pdf(&o)
}

/// A palette whose red channel is the index.
fn ramp_palette() -> (u32, Vec<u8>) {
    (
        41,
        stream_obj(
            "",
            &(0..768)
                .map(|i| if i % 3 == 0 { (i / 3) as u8 } else { 0 })
                .collect::<Vec<_>>(),
        ),
    )
}

/// An Indexed image with its default /Decode [0 255] is edited: indices
/// inside the rect zeroed, every index outside kept exactly.
#[test]
fn indexed_default_decode_is_edited() {
    let samples: Vec<u8> = (0..256).map(|i| (i * 7 % 250 + 3) as u8).collect();
    let pdf = image_page(
        "/ColorSpace [/Indexed /DeviceRGB 255 41 0 R] /Decode [0 255]",
        &samples,
        vec![ramp_palette()],
    );
    let r = run(&pdf, vec![(50.0, 50.0, 100.0, 150.0)]).unwrap();
    assert_eq!(r.pixels_changed, 128);
    let (a, b) = (reds(&pdf), reds(&r.output));
    for y in 50..150u32 {
        for x in 50..150u32 {
            let i = (y * 200 + x) as usize;
            if x < 100 {
                assert_eq!(b[i], 0, "inside ({x}, {y})");
            } else {
                assert_eq!(a[i], b[i], "outside ({x}, {y})");
            }
        }
    }
    // Remapping decodes stay refused, Indexed or not.
    for extra in [
        "/ColorSpace [/Indexed /DeviceRGB 255 41 0 R] /Decode [255 0]",
        "/ColorSpace [/Indexed /DeviceRGB 255 41 0 R] /Decode [0 127]",
        "/ColorSpace /DeviceGray /Decode [0 255]",
    ] {
        let pdf = image_page(extra, &samples, vec![ramp_palette()]);
        let e = unsupported(run(&pdf, vec![(50.0, 50.0, 100.0, 150.0)]));
        assert!(e.contains("Decode"), "{extra}: {e}");
    }
}

/// A /ColorSpace reached through a reference to an array or a device
/// name (its parameters indirect too) is taken like a direct one; a
/// reference cycle or chain, a malformed array and an Indexed JPEG
/// behind a reference are refused.
#[test]
fn indirect_colour_spaces_are_resolved() {
    let samples: Vec<u8> = (0..256 * 4).map(|i| (i * 13 % 251 + 1) as u8).collect();
    let palette = ramp_palette();
    type Objs = Vec<(u32, Vec<u8>)>;
    let ok: [(Objs, usize); 4] = [
        (
            vec![
                (43, b"[/Indexed /DeviceRGB 255 41 0 R]".to_vec()),
                palette.clone(),
            ],
            1,
        ),
        (
            vec![
                (43, b"[/ICCBased 45 0 R]".to_vec()),
                (45, stream_obj("/N 3 /Alternate 46 0 R", b"profile")),
                (46, b"/DeviceRGB".to_vec()),
            ],
            3,
        ),
        (vec![(43, b"/DeviceCMYK".to_vec())], 4),
        (vec![(43, b"/DeviceGray".to_vec())], 1),
    ];
    for (objs, n) in ok {
        let s = &samples[..256 * n];
        let pdf = image_page("/ColorSpace 43 0 R", s, objs);
        let r = run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).unwrap();
        assert_eq!(r.pixels_changed, 256, "{n}");
        assert!(!contains(&r.output, &s[..64]));
    }
    let refused: [Objs; 5] = [
        vec![(43, b"44 0 R".to_vec()), (44, b"43 0 R".to_vec())],
        // A chain of references (Poppler does not follow it): refused.
        vec![
            (43, b"44 0 R".to_vec()),
            (44, b"[/Indexed /DeviceRGB 255 41 0 R]".to_vec()),
            palette.clone(),
        ],
        vec![
            (43, b"[/Indexed /DeviceRGB 255 44 0 R]".to_vec()),
            (44, b"41 0 R".to_vec()),
            palette.clone(),
        ],
        vec![(43, b"[/Indexed /DeviceRGB 255]".to_vec())],
        vec![(43, b"[/Separation /Spot /DeviceGray 46 0 R]".to_vec())],
    ];
    for objs in refused {
        let pdf = image_page("/ColorSpace 43 0 R", &samples[..256], objs.clone());
        assert!(
            run(&pdf, vec![(50.0, 50.0, 150.0, 150.0)]).is_err(),
            "{:?}",
            String::from_utf8_lossy(&objs[0].1)
        );
    }
    let jpeg = image_page(
        "/ColorSpace 43 0 R /Filter /DCTDecode",
        &samples[..256],
        vec![(43, b"[/Indexed /DeviceRGB 255 41 0 R]".to_vec()), palette],
    );
    let e = unsupported(run(&jpeg, vec![(50.0, 50.0, 150.0, 150.0)]));
    assert!(e.contains("Indexed DCTDecode"), "{e}");
}

/// A CharProc strokes with the line state it is shown with: an inherited
/// wide line is taken at its reach.
#[test]
fn type3_strokes_inherit_the_line_state() {
    let proc = "1000 0 d0 2000 0 m 2000 500 l S";
    let pdf = type3_page(
        "1000 w 1 j BT /T3 20 Tf 50 100 Td (A) Tj ET",
        proc,
        proc,
        "",
        vec![],
    );
    let r = run(&pdf, vec![(95.0, 100.0, 98.0, 105.0)]).unwrap();
    assert_eq!(
        r.codes_removed, 1,
        "the inherited 1000-unit line reaches the rect"
    );
    // The same CharProc under a thin line stays clear of it.
    let pdf = type3_page(
        "1 w 1 j BT /T3 20 Tf 50 100 Td (A) Tj ET",
        proc,
        proc,
        "",
        vec![],
    );
    assert_eq!(
        run(&pdf, vec![(95.0, 100.0, 98.0, 105.0)])
            .unwrap()
            .codes_removed,
        0
    );
}

/// Stroke text modes do not stroke a Type 3 glyph (its CharProc decides
/// what is painted): no inflation by the line width.
#[test]
fn type3_glyphs_are_not_inflated_by_stroke_modes() {
    let proc = "1000 0 d0 0 0 400 500 re f";
    for tr in [0, 1, 2] {
        let pdf = type3_page(
            &format!("10 w BT /T3 20 Tf {tr} Tr 50 100 Td (A) Tj ET"),
            proc,
            proc,
            "",
            vec![],
        );
        let r = run(&pdf, vec![(120.0, 100.0, 125.0, 105.0)]).unwrap();
        assert_eq!(r.codes_removed, 0, "Tr {tr}");
    }
}

/// CharProc ops count against the execution budget.
#[test]
fn type3_charprocs_count_against_the_execution_budget() {
    let proc = format!("1000 0 d0 {} 0 0 10 10 re f", "q Q ".repeat(2000));
    let pdf = type3_page(T3_TEXT, &proc, T3_FILL, "", vec![]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RedactOptions::new()
        .with_limits(kasane_pdf::Limits::default().with_max_diag_exec_ops(1000));
    let e = redact(&doc, &[(0, vec![(41.5, 99.0, 55.0, 108.0)])], &opts).unwrap_err();
    assert!(e.to_string().contains("max_diag_exec_ops"), "{e}");
}

/// Stroke adjustment (/SA true) and hairlines (0 w) move a stroke's ink
/// at device-pixel scale only: their geometry is redacted in user space
/// like any stroke's — removed inside a rect, cut at its edge, kept
/// clear of it — and no path point stays inside a rect. Fills cut under
/// /SA, stroked text under /SA and Type 3 CharProc hairlines likewise.
#[test]
fn stroke_adjustment_and_hairlines_are_redacted() {
    for (gs, w) in [("/Adjust gs", "1"), ("", "0"), ("/Adjust gs", "0")] {
        let c = format!(
            "{gs} {SECRET} {w} w 70 100 m 80 100 l S 10 102 m 190 102 l S \
             10 150 m 40 170 l S 30 95 50 10 re f"
        );
        let pdf = doc_with(GS_RES, &[c.as_bytes()], vec![]);
        let r = run(&pdf, vec![RECT]).unwrap_or_else(|e| panic!("{c}: {e}"));
        assert_eq!(r.codes_removed, 10, "{c}");
        assert_eq!((r.paths_removed, r.paths_cut), (1, 2), "{c}");
        assert!(!path_points_inside(&r.output, RECT), "{c}");
        assert!(page_text(&r.output, 0).is_empty());
    }
    // Stroked text under /SA, and a hairline stroked glyph.
    for c in [
        format!("/Adjust gs {SECRET} BT /F1 12 Tf 1 Tr 10 20 Td (PUBLICWORD) Tj ET"),
        "0 w BT /F1 12 Tf 1 Tr 50 100 Td (SECRETTEXT) Tj 10 -80 Td (PUBLICWORD) Tj ET".to_string(),
    ] {
        let pdf = doc_with(GS_RES, &[c.as_bytes()], vec![]);
        let r = run(&pdf, vec![RECT]).unwrap_or_else(|e| panic!("{c}: {e}"));
        assert_eq!(r.codes_removed, 10, "{c}");
        assert_eq!(page_text(&r.output, 0), "PUBLICWORD", "{c}");
    }
    // A Type 3 glyph drawn by a hairline under /SA.
    let page = (
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
          /Font << /T3 60 0 R >> /ExtGState << /Adjust << /SA true >> >> >> /Contents 4 0 R >>"
            .to_vec(),
    );
    let proc = "1000 0 d0 0 w 0 0 m 500 500 l S";
    let pdf = type3_page(
        "/Adjust gs BT /T3 10 Tf 20 100 Td (AAAAAA) Tj ET",
        proc,
        proc,
        "",
        vec![page],
    );
    assert_eq!(
        run(&pdf, vec![(41.5, 99.0, 55.0, 108.0)])
            .unwrap()
            .codes_removed,
        2
    );
}

/// Chains of references are refused wherever a colour space is read in
/// redaction: inside an Indexed space's ICC base, and as a named
/// /ColorSpace resource.
#[test]
fn colour_space_chains_are_refused_everywhere() {
    let samples = [9u8; 256];
    let pdf = image_page(
        "/ColorSpace [/Indexed [/ICCBased 44 0 R] 255 41 0 R]",
        &samples,
        vec![
            ramp_palette(),
            (44, b"45 0 R".to_vec()),
            (45, stream_obj("/N 3 /Alternate /DeviceRGB", b"profile")),
        ],
    );
    let e = unsupported(run(&pdf, vec![(50.0, 50.0, 100.0, 150.0)]));
    assert!(e.contains("chain of references"), "{e}");
    // The same base without the chain is accepted.
    let pdf = image_page(
        "/ColorSpace [/Indexed [/ICCBased 45 0 R] 255 41 0 R]",
        &samples,
        vec![
            ramp_palette(),
            (45, stream_obj("/N 3 /Alternate /DeviceRGB", b"profile")),
        ],
    );
    run(&pdf, vec![(50.0, 50.0, 100.0, 150.0)]).unwrap();
    // A named resource reached through a chain.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
              /XObject << /Im1 5 0 R >> /ColorSpace << /CS0 43 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"q 100 0 0 100 50 50 cm /Im1 Do Q")),
        (
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 16 /Height 16 /BitsPerComponent 8 /ColorSpace /CS0",
                &samples,
            ),
        ),
        (43, b"44 0 R".to_vec()),
        (44, b"[/Indexed /DeviceRGB 255 41 0 R]".to_vec()),
        ramp_palette(),
    ]);
    let e = unsupported(run(&pdf, vec![(50.0, 50.0, 100.0, 150.0)]));
    assert!(e.contains("chain of references"), "{e}");
}

/// A refused CharProc's ops count against the budget too.
#[test]
fn refused_type3_charprocs_count_against_the_execution_budget() {
    let proc = format!("1000 0 d0 {} /X Do", "q Q ".repeat(2000));
    let pdf = type3_page(T3_TEXT, &proc, T3_FILL, "", vec![]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RedactOptions::new()
        .with_limits(kasane_pdf::Limits::default().with_max_diag_exec_ops(1000));
    let e = redact(&doc, &[(0, vec![(41.5, 99.0, 55.0, 108.0)])], &opts).unwrap_err();
    assert!(e.to_string().contains("max_diag_exec_ops"), "{e}");
}

// Type 3 glyph definitions of removed text: a CharProc may hold any
// drawing, so a glyph the output no longer shows loses its definition.

/// A ToUnicode CMap mapping 65 → A, 66 → B.
fn t3_tounicode() -> (u32, Vec<u8>) {
    (
        63,
        stream_obj(
            "",
            b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /T def \
              1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <0041> \
              <42> <0042> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end",
        ),
    )
}

/// The Type 3 font dictionary of a one-font output.
fn t3_font(pdf: &[u8]) -> kasane_pdf::Dict {
    let doc = Document::from_bytes(pdf).unwrap();
    let page = &doc.pages()[0];
    let res = doc.deref(page.resources().unwrap()).unwrap();
    let fonts = doc
        .deref(res.as_dict().unwrap().get(b"Font".as_slice()).unwrap())
        .unwrap();
    let f = fonts
        .as_dict()
        .unwrap()
        .get(b"T3".as_slice())
        .unwrap()
        .clone();
    doc.deref(&f).unwrap().as_dict().unwrap().clone()
}

const SECRET_GLYPH: &str = "1000 0 d0 111 0 333 777 re f";

#[test]
fn a_type3_glyph_shown_only_in_the_rect_loses_its_definition() {
    let pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "/ToUnicode 63 0 R",
        vec![t3_tounicode()],
    );
    let r = run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(!contains(&r.output, b"333 777"), "the B CharProc is gone");
    let doc = Document::from_bytes(&r.output).unwrap();
    let f = t3_font(&r.output);
    let get = |k: &[u8]| doc.deref(f.get(k).unwrap()).unwrap();
    let cp = get(b"CharProcs");
    let names: Vec<&Vec<u8>> = cp.as_dict().unwrap().keys().collect();
    assert_eq!(names, vec![&b"A".to_vec()]);
    let enc = format!("{:?}", get(b"Encoding"));
    assert!(
        enc.contains("Name([65])") && !enc.contains("Name([66])"),
        "{enc}"
    );
    assert_eq!(
        format!("{:?}", get(b"Widths")),
        "Array([Integer(1000), Integer(0)])"
    );
    let kasane_pdf::Object::Stream { data, .. } = get(b"ToUnicode") else {
        panic!("ToUnicode")
    };
    let cmap = String::from_utf8(data).unwrap();
    assert!(
        cmap.contains("<41> <0041>") && !cmap.contains("<42>"),
        "{cmap}"
    );
    // The kept A still shows and extracts.
    assert_eq!(page_text(&r.output, 0), "A");
    assert_ne!(at(&r.output, 25, 103), 255);
}

#[test]
fn a_type3_glyph_also_shown_outside_keeps_its_definition() {
    let pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj 0 -50 Td (B) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "/ToUnicode 63 0 R",
        vec![t3_tounicode()],
    );
    let r = run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(contains(&r.output, b"333 777"), "B is still shown below");
    assert_eq!(page_text(&r.output, 0).replace(['\n', ' '], ""), "AB");
    // Nothing removed: the font is untouched.
    let r = run(&pdf, vec![(150.0, 150.0, 160.0, 160.0)]).unwrap();
    assert_eq!(r.codes_removed, 0);
    let strip = |pdf: &[u8]| {
        t3_font(pdf)
            .iter()
            .filter(|(k, _)| !matches!(k.as_slice(), b"CharProcs" | b"ToUnicode"))
            .map(|(k, v)| format!("{k:?}{v:?}"))
            .collect::<String>()
    };
    assert_eq!(strip(&r.output), strip(&pdf));
    assert!(contains(&r.output, b"333 777"));
}

/// A font losing glyphs that something the walk does not read also
/// uses (a form field's /DR, a tiling pattern) is refused.
#[test]
fn a_type3_font_used_where_its_glyphs_are_not_evaluated_is_refused() {
    let mut pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "",
        vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] /DR << /Font << /T3 60 0 R >> >> >> >>".to_vec()),
        ],
    );
    let e = unsupported(run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]));
    assert!(e.contains("Type 3 glyph definitions"), "{e}");
    pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "",
        vec![
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                  /Font << /T3 60 0 R >> /Pattern << /P 70 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (
                70,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 \
                     /Resources << /Font << /F 60 0 R >> >>",
                    b"BT /F 1 Tf (B) Tj ET",
                ),
            ),
        ],
    );
    let e = unsupported(run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]));
    assert!(e.contains("Type 3 glyph definitions"), "{e}");
}

/// A Type 3 font with `n` glyphs (codes 0…n−1, names g0…), glyph i
/// drawn by `proc(i)`, shown by `text` on one page.
fn type3_many(n: usize, proc: impl Fn(usize) -> Vec<u8>, text: &[u8]) -> Vec<u8> {
    let names: String = (0..n).map(|i| format!("/g{i} ")).collect();
    let cps: String = (0..n).map(|i| format!("/g{i} {} 0 R ", 1000 + i)).collect();
    let widths: String = (0..n).map(|_| "1000 ").collect();
    let hex: String = text.iter().map(|b| format!("{b:02x}")).collect();
    let content = format!("BT /T3 10 Tf 20 100 Td <{hex}> Tj ET").into_bytes();
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /T3 60 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", &content)),
        (
            60,
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {cps}>> \
                 /Encoding << /Type /Encoding /Differences [0 {names}] >> /FirstChar 0 \
                 /LastChar {} /Widths [{widths}] /Resources << >> >>",
                n - 1
            )
            .into_bytes(),
        ),
    ];
    for i in 0..n {
        objs.push((1000 + i as u32, proc(i)));
    }
    build_pdf(&objs)
}

fn limit_hit(e: &Error) -> bool {
    matches!(e, Error::LimitExceeded { limit, .. } if *limit == "max_diag_exec_ops")
}

/// CharProc parsing is bounded whatever its outcome: a parse failing
/// late, one huge CharProc, many distinct failing ones, a compressed
/// one that inflates hugely — each stops at a limit, quickly.
#[test]
fn type3_charproc_parsing_is_bounded() {
    let small = |n| {
        RedactOptions::new().with_limits(kasane_pdf::Limits::default().with_max_diag_exec_ops(n))
    };
    let go = |pdf: &[u8], o: &RedactOptions| {
        let doc = Document::from_bytes(pdf).unwrap();
        let t = std::time::Instant::now();
        let r = redact(&doc, &[(0, vec![(19.0, 99.0, 200.0, 108.0)])], o);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            t.elapsed()
        );
        r
    };
    // A parse failing after 2,000 ops (an operand left without its
    // operator), under a budget of 1,000.
    let late = type3_many(
        1,
        |_| {
            stream_obj(
                "",
                format!("1000 0 d0 {} 1 2", "q Q ".repeat(2000)).as_bytes(),
            )
        },
        b"\x00",
    );
    let e = go(&late, &small(1000)).unwrap_err();
    assert!(limit_hit(&e), "{e}");
    // One huge CharProc under the default budget.
    let huge = type3_many(
        1,
        |_| {
            stream_obj(
                "",
                format!("1000 0 d0 {}", "q Q ".repeat(300_000)).as_bytes(),
            )
        },
        b"\x00",
    );
    let e = go(&huge, &RedactOptions::new()).unwrap_err();
    assert!(limit_hit(&e), "{e}");
    // 200 distinct CharProcs, each failing after 1,000 ops.
    let text: Vec<u8> = (0..200u8).collect();
    let many = type3_many(
        200,
        |i| {
            stream_obj(
                "",
                format!("1000 0 d0 % {i}\n{} 1 2", "q Q ".repeat(1000)).as_bytes(),
            )
        },
        &text,
    );
    let e = go(&many, &small(50_000)).unwrap_err();
    assert!(limit_hit(&e), "{e}");
    // A Flate CharProc inflating to 64 MB of `q Q`.
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut z, b"1000 0 d0 ").unwrap();
    for _ in 0..(64 << 20) / 4096 {
        std::io::Write::write_all(&mut z, "q Q ".repeat(1024).as_bytes()).unwrap();
    }
    let bomb = z.finish().unwrap();
    let pdf = type3_many(1, |_| stream_obj("/Filter /FlateDecode", &bomb), b"\x00");
    assert!(go(&pdf, &RedactOptions::new()).is_err());
}

// Type 3 glyphs drawn by images (Chrome's bitmap glyphs, TeX's bitmap
// fonts): an image paints its unit square; a removed glyph's image
// leaves with its definition.

/// 4 × 4 RGB samples, distinct per `seed`, with a soft mask.
fn glyph_image(seed: u8, smask: u32) -> Vec<u8> {
    let px: Vec<u8> = (0..48u8)
        .map(|i| i.wrapping_mul(29).wrapping_add(seed))
        .collect();
    stream_obj(
        &format!("/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask {smask} 0 R"),
        &px,
    )
}

fn glyph_mask(seed: u8) -> Vec<u8> {
    let px: Vec<u8> = (0..16u8)
        .map(|i| i.wrapping_mul(53).wrapping_add(seed))
        .collect();
    stream_obj("/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 8", &px)
}

/// Glyph A draws image /Ia, B draws /Ib (each with a soft mask).
fn image_glyph_font(content: &str, font_extra: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (70, glyph_image(1, 71)),
        (71, glyph_mask(2)),
        (72, glyph_image(101, 73)),
        (73, glyph_mask(102)),
    ];
    objs.extend(extra);
    type3_page(
        content,
        "1000 0 d0 800 0 0 800 100 0 cm /Ia Do",
        "1000 0 d0 800 0 0 800 100 0 cm /Ib Do",
        &format!("/Resources << /XObject << /Ia 70 0 R /Ib 72 0 R >> >> {font_extra}"),
        objs,
    )
}

fn samples_of(seed: u8) -> Vec<u8> {
    (0..48u8)
        .map(|i| i.wrapping_mul(29).wrapping_add(seed))
        .collect()
}

#[test]
fn a_removed_image_glyph_takes_its_image_with_it() {
    let pdf = image_glyph_font("BT /T3 10 Tf 20 100 Td (AB) Tj ET", "", vec![]);
    // B at x 31…39.
    let r = run(&pdf, vec![(32.0, 101.0, 38.0, 107.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(!contains(&r.output, &samples_of(101)), "B's image is gone");
    let mask: Vec<u8> = (0..16u8)
        .map(|i| i.wrapping_mul(53).wrapping_add(102))
        .collect();
    assert!(!contains(&r.output, &mask), "and its soft mask");
    assert!(contains(&r.output, &samples_of(1)), "A's image stays");
    assert!(r.elsewhere.is_empty());
    // B also shown outside: its image stays, drawn there.
    let pdf = image_glyph_font(
        "BT /T3 10 Tf 20 100 Td (AB) Tj 0 -50 Td (B) Tj ET",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(32.0, 101.0, 38.0, 107.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(contains(&r.output, &samples_of(101)));
}

/// An inline-image glyph goes with its CharProc; a CharProc drawing a
/// form, or an image from resources the font inherits, is refused; an
/// image a removed glyph drew that the page also draws stays, reported.
#[test]
fn image_glyph_edge_cases() {
    let inline = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        "1000 0 d0 800 0 0 800 100 0 cm BI /W 2 /H 1 /CS /G /BPC 8 ID \u{7}\u{9} EI",
        "",
        vec![],
    );
    let r = run(&inline, vec![(32.0, 101.0, 38.0, 107.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(!contains(&r.output, b"/W 2 /H 1"));
    let form = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        "1000 0 d0 /Fm Do",
        "/Resources << /XObject << /Fm 74 0 R >> >>",
        vec![(
            74,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 1 1]",
                b"0 0 1 1 re f",
            ),
        )],
    );
    let e = unsupported(run(&form, vec![(32.0, 101.0, 38.0, 107.0)]));
    assert!(e.contains("not an image"), "{e}");
    // The font has no /Resources: its images come from the page's.
    let inherited = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        "1000 0 d0 800 0 0 800 100 0 cm /Ib Do",
        "",
        vec![
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                  /Font << /T3 60 0 R >> /XObject << /Ib 72 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (
                60,
                b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                  /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 61 0 R /B 62 0 R >> \
                  /Encoding << /Type /Encoding /Differences [65 /A /B] >> /FirstChar 65 \
                  /LastChar 66 /Widths [1000 1000] >>"
                    .to_vec(),
            ),
            (72, glyph_image(101, 73)),
            (73, glyph_mask(102)),
        ],
    );
    let e = unsupported(run(&inherited, vec![(32.0, 101.0, 38.0, 107.0)]));
    assert!(e.contains("inherits"), "{e}");
    // The page draws B's image itself too, far from the rect.
    let shared = image_glyph_font(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET q 20 0 0 20 150 20 cm /Pb Do Q",
        "",
        vec![(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
              /Font << /T3 60 0 R >> /XObject << /Pb 72 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        )],
    );
    let r = run(&shared, vec![(32.0, 101.0, 38.0, 107.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(contains(&r.output, &samples_of(101)));
    assert!(
        r.elsewhere
            .iter()
            .any(|e| matches!(e.removed, kasane_pdf::Removed::Image)),
        "{:?}",
        r.elsewhere
    );
}

/// Review of the pruning: a form inheriting the font; a /Font dict also
/// reached from a pattern; fonts differing only in ToUnicode; a CharProc
/// shared with a font that does not show it.
#[test]
fn type3_pruning_follows_every_use() {
    let page = |res: &str| {
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {res} >> \
                 /Contents 4 0 R >>"
            )
            .into_bytes(),
        )
    };
    // The form shows with the font set before its invocation: B (in the
    // rect) loses its definition, A (outside) keeps its own.
    let pdf = type3_page(
        "BT /T3 10 Tf ET /T3 10 Tf /Fm Do",
        T3_FILL,
        SECRET_GLYPH,
        "",
        vec![
            page("/Font << /T3 60 0 R >> /XObject << /Fm 75 0 R >>"),
            (
                75,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200]",
                    b"BT 20 100 Td (AB) Tj ET",
                ),
            ),
        ],
    );
    let r = run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]).unwrap();
    assert_eq!(r.codes_removed, 1);
    assert!(!contains(&r.output, b"333 777"));
    assert!(
        contains(&r.output, b"100 0 600 700 re f"),
        "A, shown in the form, keeps its glyph"
    );
    // A /Font dictionary shared with a pattern (on another page): refused.
    let mut pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "",
        vec![
            page("/Font 76 0 R"),
            (76, b"<< /T3 60 0 R >>".to_vec()),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 77 0 R] /Count 2 >>".to_vec(),
            ),
            (
                77,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                  /Pattern << /P 78 0 R >> >> /Contents 79 0 R >>"
                    .to_vec(),
            ),
            (
                78,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 50 50] /XStep 50 \
                     /YStep 50 /Resources << /Font 76 0 R >>",
                    b"BT /T3 10 Tf 0 0 Td (B) Tj ET",
                ),
            ),
            (79, stream_obj("", b"/Pattern cs /P scn 0 0 200 200 re f")),
        ],
    );
    let e = unsupported(run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]));
    assert!(e.contains("Type 3 glyph definitions"), "{e}");
    // Two fonts equal but for their ToUnicode (and a third with another
    // matrix sharing B's CharProc, shown nowhere): the output does not
    // depend on what only the removed glyph carried.
    let outputs: Vec<Vec<u8>> = ["SECRETONE", "SECRETTWO"]
        .iter()
        .map(|secret| {
            let tu = |s: &str| {
                let hex: String = s.encode_utf16().map(|u| format!("{u:04X}")).collect();
                stream_obj(
                    "",
                    format!(
                        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /T def \
                         1 begincodespacerange <00> <FF> endcodespacerange 2 beginbfchar <41> <0041> \
                         <42> <{hex}> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end"
                    )
                    .as_bytes(),
                )
            };
            pdf = type3_page(
                "BT /T3 10 Tf 20 100 Td (B) Tj /U 10 Tf 20 20 Td (A) Tj ET",
                T3_FILL,
                SECRET_GLYPH,
                "/ToUnicode 63 0 R",
                vec![
                    page("/Font << /T3 60 0 R /U 64 0 R /V 66 0 R >>"),
                    (63, tu(secret)),
                    (
                        64,
                        b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                          /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 61 0 R /B 62 0 R >> \
                          /Encoding << /Type /Encoding /Differences [65 /A /B] >> /FirstChar 65 \
                          /LastChar 66 /Widths [1000 1000] /Resources << >> /ToUnicode 65 0 R >>"
                            .to_vec(),
                    ),
                    (65, tu("PUBLIC")),
                    (
                        66,
                        b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                          /FontMatrix [0.002 0 0 0.002 0 0] /CharProcs << /Z 62 0 R >> \
                          /Encoding << /Type /Encoding /Differences [90 /Z] >> /FirstChar 90 \
                          /LastChar 90 /Widths [1000] /Resources << >> >>"
                            .to_vec(),
                    ),
                ],
            );
            let r = run(&pdf, vec![(15.0, 95.0, 45.0, 115.0)]).unwrap();
            assert_eq!(r.codes_removed, 1);
            assert!(!contains(&r.output, secret.as_bytes()));
            assert!(!contains(&r.output, b"333 777"), "B's CharProc is gone from every font");
            r.output
        })
        .collect();
    assert!(outputs[0] == outputs[1], "the outputs differ");
}

/// Comparing Type 3 fonts by content digests each shared object once:
/// a 4-way shared graph 24 levels deep under the font costs 24 objects,
/// not 4^24 paths.
#[test]
fn type3_font_keys_are_linear_in_shared_graphs() {
    let mut extra: Vec<(u32, Vec<u8>)> = (0..24u32)
        .map(|i| {
            (
                80 + i,
                format!("[{n} 0 R {n} 0 R {n} 0 R {n} 0 R]", n = 81 + i).into_bytes(),
            )
        })
        .collect();
    extra.push((104, b"(leaf)".to_vec()));
    let pdf = type3_page(
        "BT /T3 10 Tf 20 100 Td (AB) Tj ET",
        T3_FILL,
        SECRET_GLYPH,
        "/Junk 80 0 R",
        extra,
    );
    let t = std::time::Instant::now();
    let r = run(&pdf, vec![(31.0, 99.0, 39.0, 108.0)]).unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );
    assert!(!contains(&r.output, b"333 777"));
}

/// Kept characters with their origins (rounded to 1e-6 pt).
fn kept_chars(pdf: &[u8]) -> Vec<(String, i64, i64)> {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .map(|c| {
            (
                c.unicode.clone().unwrap_or_default(),
                (c.origin.0 * 1e6).round() as i64,
                (c.origin.1 * 1e6).round() as i64,
            )
        })
        .collect()
}

/// The removed text's positioning does not stay: two inputs differing
/// only in where (and how wide) the removed glyphs were give the same
/// output, and the kept text stays exactly where it was — whatever the
/// producer's positioning (per-glyph Td, TJ kerns, absolute Tm, T*, ').
#[test]
fn removed_text_leaves_no_positions() {
    // Removed glyphs' offsets and kerns vary with `v`; the kept text
    // ("Name:", "here", "next", "below") stays where it is.
    let layouts = |v: u32| -> Vec<String> {
        let o = |k: u32| ((v * 7 + k * 3) % 11) as i64; // varies with v
        let (a, b, c) = (5 + o(1), 6 + o(2), 4 + o(3));
        let k: Vec<i64> = (1..=4).map(|i| -(o(i) * 37) - 100).collect();
        vec![
            format!(
                "BT /F1 12 Tf 20 100 Td (Name:) Tj 52 0 Td (S) Tj {a} 0 Td (E) Tj {b} 0 Td (CRET) Tj \
                 {c} 0 Td (W) Tj {} 0 Td (here) Tj 0 -14 Td (next) Tj ET",
                90 - a - b - c
            ),
            format!(
                "BT /F1 12 Tf 20 100 Td [(Name: ) {} (S) {} (E) {} (CRET) {} (W) {} (ORD) -900 (here)] TJ ET",
                k[0],
                k[1],
                k[2],
                k[3],
                -5000 - k.iter().sum::<i64>()
            ),
            format!(
                "BT /F1 12 Tf 1 0 0 1 20 100 Tm (Name:) Tj 1 0 0 1 {} 100 Tm (SEC) Tj 1 0 0 1 {} 100 Tm \
                 (RET) Tj 1 0 0 1 162 100 Tm (here) Tj ET",
                70 + o(1),
                100 + o(2)
            ),
            format!(
                "BT /F1 12 Tf 14 TL 20 100 Td (Name:) Tj {} 0 Td (SECRET) Tj T* ({}) ' 30 0 Td (W) Tj ET \
                 BT /F1 12 Tf 20 50 Td (below) Tj ET",
                52 + o(1),
                ["XX", "QQQQ", "W"][(v % 3) as usize]
            ),
            format!(
                "BT /F1 12 Tf 100 Tz 20 100 Td (Name:) Tj {} 0 Td (SEC) Tj {} 0 Td (here) Tj ET",
                52 + a,
                90 - a
            ),
            format!(
                "BT /F1 12 Tf 20 100 Td (Name:) Tj 60 -{a} TD (SEC) Tj 0 -{b} TD (RET) Tj \
                 -60 -{} TD (below) Tj ET",
                45 - a - b + 20
            ),
            // The number of removed glyphs, words (spaces under Tw) and
            // coloured spans differs.
            format!(
                "BT /F1 12 Tf 2 Tw 20 100 Td (Name:) Tj 52 0 Td {} 0 g 90 0 Td (here) Tj ET",
                [
                    "(AB CD) Tj 0 0 1 rg (EF) Tj",
                    "(A B C) Tj 0 0 1 rg (D) Tj 0 g 0 0 1 rg (E) Tj 1 0 0 rg (F) Tj",
                    "(ABCDEF) Tj",
                ][(v % 3) as usize]
            ),
            // Removed shows each in q/Q; removed-only text objects with
            // their own styling; colours by cs/scn.
            format!(
                "BT /F1 12 Tf 20 100 Td (Name:) Tj ET {} {} \
                 BT /F1 12 Tf 0 Ts 0 g 162 100 Td (here) Tj ET",
                "q BT /F1 12 Tf 80 100 Td (S) Tj ET Q ".repeat(v as usize),
                format!(
                    "BT /F1 1{v} Tf {v} Ts /DeviceRGB cs 0.{v} 0 1 scn 90 95 Td (E{v}) Tj ET"
                )
            ),
            // A translation set for the removed show and undone after
            // it; a stroke colour and line width only removed stroked
            // text used.
            format!(
                "1 0 0 1 {v} 0 cm BT /F1 12 Tf 80 100 Td (SEC) Tj ET 1 0 0 1 -{v} 0 cm \
                 BT /F1 12 Tf 0.{v} 0 0 RG {v} w 2 Tr 90 95 Td (RET) Tj 0 0 0 RG 0 Tr \
                 20 100 Td (Name:) Tj ET BT /F1 12 Tf 162 100 Td (here) Tj ET"
            ),
        ]
    };
    let rect = vec![(60.0, 60.0, 155.0, 115.0)];
    for i in 0..9 {
        let (a, b) = (layouts(1)[i].clone(), layouts(2)[i].clone());
        assert_ne!(a, b);
        let (pa, pb) = (one_page(&a, "", "", vec![]), one_page(&b, "", "", vec![]));
        let (ra, rb) = (
            run(&pa, rect.clone()).unwrap(),
            run(&pb, rect.clone()).unwrap(),
        );
        assert!(ra.codes_removed > 0, "{a}");
        let text = page_text(&ra.output, 0);
        let after = [
            "here", "here", "here", "below", "here", "below", "here", "here", "here",
        ][i];
        assert!(
            text.contains("Name") && text.contains(after),
            "layout {i}: {text}"
        );
        let ops = |pdf: &[u8]| {
            let d = Document::from_bytes(pdf).unwrap();
            let p = &d.pages()[0];
            d.page_operations(p)
                .unwrap()
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            ops(&ra.output),
            ops(&rb.output),
            "layout {i}: the content differs"
        );
        assert!(ra.output == rb.output, "layout {i}: the outputs differ");
        // Kept characters stay where they were.
        let kept: Vec<_> = kept_chars(&pa)
            .into_iter()
            .filter(|c| kept_chars(&ra.output).contains(c))
            .collect();
        assert_eq!(
            kept,
            kept_chars(&ra.output),
            "layout {i}: a kept character moved"
        );
    }
}

/// ExtGState set around each removed show: how many there were does
/// not stay.
#[test]
fn removed_shows_leave_no_ext_gstate_trail() {
    let outputs: Vec<Vec<u8>> = [1usize, 3]
        .iter()
        .map(|n| {
            let removed = "/A gs BT /F1 12 Tf 80 100 Td (S) Tj ET /B gs ".repeat(*n);
            let pdf = doc_with(
                "/Font << /F1 20 0 R >> /ExtGState << /A << /ca 0.5 >> /B << /ca 1 >> >>",
                &[format!("{removed}BT /F1 12 Tf 20 20 Td (PUBLICWORD) Tj ET").as_bytes()],
                font_objs(),
            );
            let r = run(&pdf, vec![(60.0, 60.0, 155.0, 115.0)]).unwrap();
            assert_eq!(r.codes_removed as usize, *n);
            r.output
        })
        .collect();
    assert!(outputs[0] == outputs[1], "the outputs differ");
    // A stroke opacity only removed stroked text used: the content
    // does not select it (its definition stays in the page's resources,
    // which are kept as they are).
    let contents: Vec<String> = ["0.25", "0.75"]
        .iter()
        .map(|ca| {
            let pdf = doc_with(
                &format!("/Font << /F1 20 0 R >> /ExtGState << /S << /CA {ca} >> >>"),
                &[b"/S gs BT /F1 12 Tf 1 Tr 80 100 Td (S) Tj 0 Tr 20 20 Td (PUBLICWORD) Tj ET"],
                font_objs(),
            );
            let out = run(&pdf, vec![(60.0, 60.0, 155.0, 115.0)]).unwrap().output;
            let d = Document::from_bytes(&out).unwrap();
            let ops = d.page_operations(&d.pages()[0]).unwrap();
            ops.iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert_eq!(contents[0], contents[1]);
    assert!(!contents[0].contains("gs"), "{}", contents[0]);
}

// Shadings (`sh`): one painting only inside a rect is removed with the
// resource only it used; one crossing a rect is refused (its function
// and geometry are data a clip would only hide).

/// An axial shading whose function carries `c0` (distinctive values).
fn axial(c0: &str, extra: &str) -> Vec<u8> {
    format!(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Extend [true true] {extra} \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [{c0}] /C1 [1 1 1] /N 1 >> >>"
    )
    .into_bytes()
}

#[test]
fn shadings_inside_a_rect_are_removed() {
    let page_res = "/Font << /F1 20 0 R >> /Shading << /S1 30 0 R /S2 31 0 R >>";
    let mut objs = font_objs();
    objs.push((30, axial("0.123 0.456 0.789", "")));
    objs.push((31, axial("0.9 0.1 0.2", "")));
    // S1 clipped to a box inside the rect; S2 clipped clear of it.
    let pdf = doc_with(
        page_res,
        &[format!(
            "{SECRET} q 60 95 20 10 re W n /S1 sh Q q 10 10 20 20 re W n /S2 sh Q \
             BT /F1 12 Tf 10 50 Td (PUBLICWORD) Tj ET"
        )
        .as_bytes()],
        objs.clone(),
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!((r.codes_removed, r.shadings_removed), (10, 1));
    assert!(!contains(&r.output, b"0.123 0.456 0.789") && !raw_contains(&r.output, b"0.456"));
    assert!(raw_contains(&r.output, b"0.9 0.1 0.2") || contains(&r.output, b"0.9"));
    let doc = Document::from_bytes(&r.output).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    let sh: Vec<String> = ops
        .iter()
        .filter(|o| matches!(o, kasane_pdf::Operation::Shading(_)))
        .map(|o| o.to_string())
        .collect();
    assert_eq!(sh, vec!["/S2 sh".to_string()]);
    // A /BBox inside the rect bounds it without a clip.
    let mut o2 = font_objs();
    o2.push((30, axial("0.123 0.456 0.789", "/BBox [0 0 10 10]")));
    o2.push((31, axial("0.9 0.1 0.2", "")));
    let pdf = doc_with(
        page_res,
        &[format!("{SECRET} q 1 0 0 1 60 95 cm /S1 sh Q").as_bytes()],
        o2,
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.shadings_removed, 1);
    assert!(!raw_contains(&r.output, b"0.456"));
}

/// A shading crossing a rect, unbounded, shared with another page,
/// drawn in a form or also painted elsewhere on the page is refused.
#[test]
fn shadings_that_cannot_be_removed_alone() {
    let page_res = "/Font << /F1 20 0 R >> /Shading << /S1 30 0 R >>";
    let mut objs = font_objs();
    objs.push((30, axial("0.123 0.456 0.789", "")));
    for c in [
        format!("{SECRET} q 30 95 50 10 re W n /S1 sh Q"),
        format!("{SECRET} /S1 sh"),
    ] {
        let pdf = doc_with(page_res, &[c.as_bytes()], objs.clone());
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("shading"), "{c}: {e}");
    }
    // Page 2 uses the same shading object.
    let pdf = doc_with(
        page_res,
        &[
            format!("{SECRET} q 60 95 20 10 re W n /S1 sh Q").as_bytes(),
            b"q 0 0 5 5 re W n /S1 sh Q",
        ],
        objs.clone(),
    );
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(
        e.contains("shading used elsewhere") || e.contains("shared"),
        "{e}"
    );
    // In a form.
    let mut o2 = objs.clone();
    o2.push((
        40,
        stream_obj(
            "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Shading << /S1 30 0 R >> >>",
            b"q 60 95 20 10 re W n /S1 sh Q",
        ),
    ));
    let pdf = doc_with(
        "/Font << /F1 20 0 R >> /XObject << /Fm 40 0 R >>",
        &[format!("{SECRET} /Fm Do").as_bytes()],
        o2,
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("shading"));
    // Still painted elsewhere on the page: refused — the kept use would
    // keep the function over the part painted inside.
    let pdf = doc_with(
        page_res,
        &[format!("{SECRET} q 60 95 20 10 re W n /S1 sh Q q 0 0 5 5 re W n /S1 sh Q").as_bytes()],
        objs,
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("also outside"));
}

// Tiling patterns: what a pattern paints is decided where it is used —
// a fill clear of the rects stays, one wholly inside a rect goes with
// the pattern only it used, one crossing a rect is refused (the cell is
// content a cut would only hide), and so is text painted with one that
// meets a rect.

fn tiling() -> Vec<u8> {
    stream_obj(
        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 4 4] /XStep 4 /YStep 4 /Resources << >>",
        b"0.321 0.654 0.987 rg 0 0 2 2 re f",
    )
}

#[test]
fn tiling_patterns_follow_their_uses() {
    let res = "/Font << /F1 20 0 R >> /Pattern << /P1 30 0 R >>";
    let mut objs = font_objs();
    objs.push((30, tiling()));
    // Clear of the rect: kept (the page was refused whole before).
    let pdf = doc_with(
        res,
        &[format!("{SECRET} /Pattern cs /P1 scn 10 10 20 20 re f").as_bytes()],
        objs.clone(),
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    assert!(contains(&r.output, b"0.321 0.654 0.987"));
    // Wholly inside: removed with the cell.
    let pdf = doc_with(
        res,
        &[format!("{SECRET} /Pattern cs /P1 scn 60 95 20 10 re f").as_bytes()],
        objs.clone(),
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!((r.codes_removed, r.paths_removed), (10, 1));
    assert!(
        !contains(&r.output, b"0.321 0.654 0.987"),
        "the cell is gone"
    );
    // Nor is it still selected by name (kept content follows).
    let pdf = doc_with(
        res,
        &[b"/Pattern cs /P1 scn 60 95 20 10 re f 0 g 10 10 20 20 re f"],
        objs.clone(),
    );
    let r = run(&pdf, vec![RECT]).unwrap();
    let doc = Document::from_bytes(&r.output).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    let shown: Vec<String> = ops.iter().map(|o| o.to_string()).collect();
    assert!(!shown.iter().any(|o| o.contains("/P1")), "{shown:?}");
    // Crossing, and text painted with it meeting the rect: refused.
    for c in [
        format!("{SECRET} /Pattern cs /P1 scn 30 95 50 10 re f"),
        "/Pattern cs /P1 scn BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET".to_string(),
    ] {
        let pdf = doc_with(res, &[c.as_bytes()], objs.clone());
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("tiling pattern"), "{c}: {e}");
    }
    // A colour set after the pattern ends it.
    let pdf = doc_with(
        res,
        &[b"/Pattern cs /P1 scn 0 g BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET"],
        objs.clone(),
    );
    assert_eq!(run(&pdf, vec![RECT]).unwrap().codes_removed, 10);
    // The pattern also used by another page: refused.
    let pdf = doc_with(
        res,
        &[
            format!("{SECRET} /Pattern cs /P1 scn 60 95 20 10 re f").as_bytes(),
            b"/Pattern cs /P1 scn 0 0 5 5 re f",
        ],
        objs,
    );
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(
        e.contains("tiling pattern used elsewhere") || e.contains("shared"),
        "{e}"
    );
}

// Carriage returns in strings. §7.3.4.2 reads an unescaped CR in a
// literal string — alone or as CRLF — as one LF; Poppler and pdf.js keep
// the bytes. Kept codes holding 0x0D are written escaped, so every
// reader reads them alike; input holding a raw CR is refused, the codes
// shown (and where) depending on the reader.

#[test]
fn kept_carriage_return_codes_are_written_escaped() {
    let c = format!("{SECRET} BT /F1 12 Tf 10 20 Td <410d0a42> Tj ET");
    let pdf = doc_with("/Font << /F1 20 0 R >>", &[c.as_bytes()], font_objs());
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.codes_removed, 10);
    let doc = Document::from_bytes(&r.output).unwrap();
    let page = &doc.pages()[0];
    let raw: Vec<u8> = doc.page_contents(page).unwrap().concat();
    assert!(
        !raw.contains(&b'\r'),
        "a raw CR in the content: {}",
        String::from_utf8_lossy(&raw)
    );
    let shown: Vec<Vec<u8>> = doc
        .page_operations(page)
        .unwrap()
        .into_iter()
        .filter_map(|o| match o {
            kasane_pdf::Operation::ShowText(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![b"A\r\nB".to_vec()]);
}

#[test]
fn raw_carriage_returns_in_strings_are_refused() {
    let page_res = "/Font << /F1 20 0 R >> /XObject << /Fm 40 0 R >>";
    for c in [
        format!("{SECRET} BT /F1 12 Tf 10 20 Td (A\rB) Tj ET"),
        "BT /F1 12 Tf 50 100 Td (SECRET\r\nTEXT) Tj ET".to_string(),
        format!("{SECRET} /Fm Do"),
    ] {
        let mut objs = font_objs();
        objs.push((
            40,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 20 0 R >> >>",
                b"BT /F1 12 Tf 10 20 Td (A\rB) Tj ET",
            ),
        ));
        let pdf = doc_with(page_res, &[c.as_bytes()], objs);
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("carriage return"), "{c:?}: {e}");
    }
    // Escaped, it is unambiguous.
    let c = format!("{SECRET} BT /F1 12 Tf 10 20 Td (A\\rB\\015) Tj ET");
    let pdf = doc_with("/Font << /F1 20 0 R >>", &[c.as_bytes()], font_objs());
    assert_eq!(run(&pdf, vec![RECT]).unwrap().codes_removed, 10);
}

/// What a removed shading or tiling pattern uses must go with it: data
/// it shares with anything else, a shading or pattern also painted
/// outside the rects, and patterns read only by images carrying their
/// own colours (review of 0.7.0).
#[test]
fn removed_shadings_and_patterns_take_their_data() {
    let page_res = "/Font << /F1 20 0 R >> /Shading << /S1 30 0 R >>";
    let inside = format!("{SECRET} q 60 95 20 10 re W n /S1 sh Q 0 g 10 10 20 20 re f");
    let shading = |f: &str| {
        format!(
            "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Extend [true true] \
             /Function {f} >>"
        )
        .into_bytes()
    };
    let func = b"<< /FunctionType 2 /Domain [0 1] /C0 [0.123 0.456 0.789] /C1 [1 1 1] /N 1 >>";
    // Its function an object of its own: removed with it.
    let mut objs = font_objs();
    objs.push((30, shading("31 0 R")));
    objs.push((31, func.to_vec()));
    let pdf = doc_with(page_res, &[inside.as_bytes()], objs.clone());
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.shadings_removed, 1);
    assert!(!raw_contains(&r.output, b"0.456") && !contains(&r.output, b"0.456"));
    // The function also held by another object (here an unused
    // annotation-free dictionary the page refers to): refused.
    let mut o2 = objs.clone();
    o2.push((32, b"<< /Cache 31 0 R >>".to_vec()));
    let page_res2 = format!("{page_res} /Properties << /X 32 0 R >>");
    let pdf = doc_with(&page_res2, &[inside.as_bytes()], o2);
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("also referred to"), "{e}");
    // Painted inside and also outside: refused.
    let c = format!("{inside} q 0 0 5 5 re W n /S1 sh Q");
    let pdf = doc_with(page_res, &[c.as_bytes()], objs.clone());
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("also outside"), "{e}");
    // Two names for one shading, one of them kept: refused.
    let pdf = doc_with(
        "/Font << /F1 20 0 R >> /Shading << /S1 30 0 R /S2 30 0 R >>",
        &[format!("{inside} q 0 0 5 5 re W n /S2 sh Q").as_bytes()],
        objs,
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("shading"));

    // A tiling pattern followed by an RGB image: the image carries its
    // own colours, so the cell goes; an image mask paints with it.
    let res = "/Font << /F1 20 0 R >> /Pattern << /P1 30 0 R >> /XObject << /Im 40 0 R >>";
    let mut objs = font_objs();
    objs.push((30, tiling()));
    for (mask, gone) in [(false, true), (true, false)] {
        let mut o = objs.clone();
        o.push((
            40,
            if mask {
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask true \
                     /BitsPerComponent 1",
                    &[0],
                )
            } else {
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB \
                     /BitsPerComponent 8",
                    &[1, 2, 3],
                )
            },
        ));
        let c = "/Pattern cs /P1 scn 60 95 20 10 re f q 10 0 0 10 10 10 cm /Im Do Q";
        let r = run(&doc_with(res, &[c.as_bytes()], o), vec![RECT]);
        if gone {
            let r = r.unwrap();
            assert!(
                !contains(&r.output, b"0.321 0.654 0.987"),
                "the cell is gone"
            );
        } else {
            assert!(unsupported(r).contains("also outside"));
        }
    }
}

/// A removed shading reaching a long chain of references: a typed
/// limit, not a stack overflow (review of 0.7.0).
#[test]
fn deep_references_from_a_removed_shading_are_bounded() {
    let mut objs = font_objs();
    objs.push((
        30,
        b"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Function << /FunctionType 2 \
          /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> /Extra 1000 0 R >>"
            .to_vec(),
    ));
    for i in 0..3000u32 {
        let body = if i + 1 < 3000 {
            format!("[{} 0 R]", 1001 + i)
        } else {
            "0".into()
        };
        objs.push((1000 + i, body.into_bytes()));
    }
    let pdf = doc_with(
        "/Font << /F1 20 0 R >> /Shading << /S1 30 0 R >>",
        &[format!("{SECRET} q 60 95 20 10 re W n /S1 sh Q").as_bytes()],
        objs,
    );
    let e = run(&pdf, vec![RECT]).unwrap_err();
    assert!(matches!(e, Error::LimitExceeded { .. }), "{e}");
}

/// Second review of 0.7.0: a copy of a removed shading written inside
/// another object fails verification; an image mask flagged through a
/// reference is a use of the pattern; resources given by reference
/// with a direct /Shading are not taken as shared.
#[test]
fn removed_shading_copies_masks_and_holders() {
    let sh = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] /Extend [true true] \
              /Function << /FunctionType 2 /Domain [0 1] /C0 [0.123 0.456 0.789] /C1 [1 1 1] /N 1 >> >>";
    let inside = format!("{SECRET} q 60 95 20 10 re W n /S1 sh Q 0 g 10 10 20 20 re f");
    // An unused shading pattern holding an identical shading dictionary.
    let res = format!("/Font << /F1 20 0 R >> /Shading << /S1 {sh} >> /Pattern << /P 33 0 R >>");
    let mut objs = font_objs();
    objs.push((
        33,
        format!("<< /PatternType 2 /Shading {sh} >>").into_bytes(),
    ));
    let e = unsupported(run(&doc_with(&res, &[inside.as_bytes()], objs), vec![RECT]));
    assert!(e.contains("failed verification"), "{e}");
    // /Resources by reference, /Shading direct in it: accepted.
    let mut objs = font_objs();
    objs.push((
        34,
        format!("<< /Font << /F1 20 0 R >> /Shading << /S1 {sh} >> >>").into_bytes(),
    ));
    let pdf = doc_with_resources_ref(34, &[inside.as_bytes()], objs);
    let r = run(&pdf, vec![RECT]).unwrap();
    assert_eq!(r.shadings_removed, 1);
    assert!(!raw_contains(&r.output, b"0.456") && !contains(&r.output, b"0.456"));
    // An image mask whose /ImageMask is a reference paints with the
    // pattern: a kept use, so removing the pattern fill is refused.
    let res = "/Font << /F1 20 0 R >> /Pattern << /P1 30 0 R >> /XObject << /Im 40 0 R >>";
    let mut objs = font_objs();
    objs.push((30, tiling()));
    objs.push((
        40,
        stream_obj(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ImageMask 41 0 R /BitsPerComponent 1",
            &[0],
        ),
    ));
    objs.push((41, b"true".to_vec()));
    let c = "/Pattern cs /P1 scn 60 95 20 10 re f q 10 0 0 10 10 10 cm /Im Do Q";
    let e = unsupported(run(&doc_with(res, &[c.as_bytes()], objs), vec![RECT]));
    assert!(e.contains("also outside"), "{e}");
}

/// [`doc_with`] with the page's /Resources given by reference.
fn doc_with_resources_ref(res: u32, pages: &[&[u8]], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = Vec::new();
    objs.push((1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()));
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 100 + 2 * i))
        .collect();
    objs.push((
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        )
        .into_bytes(),
    ));
    for (i, c) in pages.iter().enumerate() {
        let n = 100 + 2 * i as u32;
        objs.push((
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {res} 0 R \
                 /Contents {} 0 R >>",
                n + 1
            )
            .into_bytes(),
        ));
        objs.push((n + 1, stream_obj("", c)));
    }
    objs.extend(extra);
    build_pdf(&objs)
}

/// A kept annotation appearance holding a raw CR, in a Type 3 font
/// whose glyphs are pruned: which code it shows decides which glyph
/// must stay, and readers differ on it — refused (second review).
#[test]
fn raw_carriage_returns_in_type3_usage_are_refused() {
    let widths = vec!["600"; 57].join(" ");
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /T 5 0 R >> >> \
              /Contents 4 0 R /Annots [40 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /T 10 Tf 80 100 Td <0A> Tj ET")),
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 600] /FontMatrix [.001 0 0 .001 0 0] \
                 /CharProcs << /LF 6 0 R /CR 7 0 R /A 8 0 R /B 9 0 R >> /Encoding << /Differences \
                 [10 /LF 13 /CR 65 /A /B] >> /FirstChar 10 /LastChar 66 /Widths [{widths}] >>"
            )
            .into_bytes(),
        ),
        (6, stream_obj("", b"600 0 d0 1 0 0 rg 0 0 600 600 re f")),
        (7, stream_obj("", b"600 0 d0 0 0 1 rg 0 0 600 600 re f")),
        (8, stream_obj("", b"600 0 d0 0 g 0 0 400 600 re f")),
        (9, stream_obj("", b"600 0 d0 0 g 100 0 400 600 re f")),
        (
            40,
            b"<< /Type /Annot /Subtype /Square /Rect [10 10 70 40] /AP << /N 41 0 R >> >>".to_vec(),
        ),
        (
            41,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 60 30] /Resources << /Font << /T 5 0 R >> >>",
                b"BT /T 10 Tf 0 10 Td (A\rB) Tj ET",
            ),
        ),
    ]);
    let e = unsupported(run(&pdf, vec![(60.0, 60.0, 140.0, 140.0)]));
    assert!(e.contains("carriage return"), "{e}");
}

/// remove_javascript: every JavaScript action and script tree goes — the
/// name tree, /OpenAction, /AA triggers, an annotation's /A, a step of
/// an action chain, one script shared by several — and other actions
/// stay; without the option the document is refused.
#[test]
fn javascript_is_removed_on_request() {
    let objs = |cat_extra: &str| {
        let mut o = font_objs();
        o.extend([
            (
                50,
                b"<< /S /JavaScript /JS (var leak = 'SCRIPTBODY1';) >>".to_vec(),
            ),
            (51, b"<< /S /JavaScript /JS 52 0 R >>".to_vec()),
            (52, stream_obj("", b"app.alert('SCRIPTBODY2');")),
            (
                53,
                b"<< /S /URI /URI (https://example.com/kept) /Next [50 0 R 54 0 R] >>".to_vec(),
            ),
            (54, b"<< /S /JavaScript /JS (SCRIPTBODY3) >>".to_vec()),
            (55, b"<< /Names [(init) 50 0 R (more) 51 0 R] >>".to_vec()),
            (
                56,
                b"<< /Type /Annot /Subtype /Link /Rect [10 150 60 170] /A 53 0 R \
                  /AA << /E 51 0 R /X << /S /JavaScript /JS (SCRIPTBODY4) >> >> >>"
                    .to_vec(),
            ),
            (
                1,
                format!(
                    "<< /Type /Catalog /Pages 2 0 R /OpenAction 50 0 R \
                     /Names << /JavaScript 55 0 R >> {cat_extra} >>"
                )
                .into_bytes(),
            ),
        ]);
        o
    };
    let page = format!("{SECRET} BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET");
    let pdf = doc_with_annots(
        "/Font << /F1 20 0 R >>",
        page.as_bytes(),
        "/Annots [56 0 R]",
        objs(""),
    );
    assert!(unsupported(run(&pdf, vec![RECT])).contains("JavaScript"));
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_javascript(true),
    )
    .unwrap();
    assert_eq!(r.codes_removed, 10);
    assert!(r.javascript_removed >= 5, "{}", r.javascript_removed);
    for s in [&b"SCRIPTBODY"[..], b"/JavaScript", b"/JS"] {
        assert!(
            !raw_contains(&r.output, s) && !contains(&r.output, s),
            "{}",
            String::from_utf8_lossy(s)
        );
    }
    assert!(
        contains(&r.output, b"https://example.com/kept")
            || raw_contains(&r.output, b"example.com/kept")
    );
    assert_eq!(page_text(&r.output, 0), "PUBLICWORD");
}

/// [`doc_with`] for one page with `page_extra` in its dictionary; an
/// object 1 in `extra` replaces the catalog.
fn doc_with_annots(
    page_res: &str,
    content: &[u8],
    page_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (2, b"<< /Type /Pages /Kids [100 0 R] /Count 1 >>".to_vec()),
        (
            100,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {page_res} >> \
                 /Contents 101 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (101, stream_obj("", content)),
    ];
    if !extra.iter().any(|(n, _)| *n == 1) {
        objs.push((1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()));
    }
    objs.extend(extra);
    objs.sort_by_key(|(n, _)| *n);
    build_pdf(&objs)
}

/// remove_javascript, second review: a script's source also referred to
/// elsewhere, and an XFA form, are refused; a dictionary that only names
/// /S /JavaScript (no /JS) is not a script; a script in an action chain
/// gives its place to the actions after it.
#[test]
fn javascript_removal_keeps_what_is_not_a_script() {
    let o = RedactOptions::new().remove_javascript(true);
    let page = format!("{SECRET} BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET");
    let go = |cat: &str, extra: Vec<(u32, Vec<u8>)>| {
        let mut objs = font_objs();
        objs.push((
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
        ));
        objs.extend(extra);
        let pdf = doc_with_annots("/Font << /F1 20 0 R >>", page.as_bytes(), "", objs);
        let doc = Document::from_bytes(&pdf).unwrap();
        redact(&doc, &[(0, vec![RECT])], &o)
    };
    let script = || (50, b"<< /S /JavaScript /JS 51 0 R >>".to_vec());
    let source = || (51, stream_obj("", b"var s = 'SCRIPTSOURCE';"));
    let e = go("/OpenAction 50 0 R /Cache 51 0 R", vec![script(), source()]).unwrap_err();
    assert!(e.to_string().contains("also referred to"), "{e}");
    let e = go(
        "/AcroForm << /Fields [] /XFA 52 0 R >>",
        vec![(52, stream_obj("", b"<xdp:xdp/>"))],
    )
    .unwrap_err();
    assert!(e.to_string().contains("XFA"), "{e}");
    // Not an action: kept, and the document is not taken for scripted.
    let r = go(
        "/Extra 53 0 R",
        vec![(53, b"<< /S /JavaScript /Keep (KEPTVALUE) >>".to_vec())],
    )
    .unwrap();
    assert!(contains(&r.output, b"KEPTVALUE") || raw_contains(&r.output, b"KEPTVALUE"));
    // An inline script whose source is also referred to elsewhere.
    let e = go(
        "/OpenAction << /S /JavaScript /JS 51 0 R >> /Cache 51 0 R",
        vec![source()],
    )
    .unwrap_err();
    assert!(e.to_string().contains("also referred to"), "{e}");
    // A script followed (through a referenced array) by two actions,
    // in an entry holding one action: refused.
    let e = go(
        "/OpenAction 50 0 R",
        vec![
            (50, b"<< /S /JavaScript /JS (x) /Next 56 0 R >>".to_vec()),
            (56, b"[57 0 R 58 0 R]".to_vec()),
            (57, b"<< /S /URI /URI (https://example.com/a) >>".to_vec()),
            (58, b"<< /S /URI /URI (https://example.com/b) >>".to_vec()),
        ],
    )
    .unwrap_err();
    assert!(e.to_string().contains("several actions"), "{e}");
    // Two scripts following each other in a cycle: both go.
    let r = go(
        "/OpenAction 50 0 R",
        vec![
            (
                50,
                b"<< /S /JavaScript /JS (SCRIPTSOURCE) /Next 59 0 R >>".to_vec(),
            ),
            (
                59,
                b"<< /S /JavaScript /JS (SCRIPTSOURCE) /Next 50 0 R >>".to_vec(),
            ),
        ],
    )
    .unwrap();
    assert!(!raw_contains(&r.output, b"SCRIPTSOURCE") && !contains(&r.output, b"SCRIPTSOURCE"));
    // URI -> script -> URI: the last one follows the first.
    let r = go(
        "/OpenAction 54 0 R",
        vec![
            (
                54,
                b"<< /S /URI /URI (https://example.com/first) /Next 50 0 R >>".to_vec(),
            ),
            (
                50,
                b"<< /S /JavaScript /JS (SCRIPTSOURCE) /Next 55 0 R >>".to_vec(),
            ),
            (
                55,
                b"<< /S /URI /URI (https://example.com/after) >>".to_vec(),
            ),
        ],
    )
    .unwrap();
    assert!(!raw_contains(&r.output, b"SCRIPTSOURCE") && !contains(&r.output, b"SCRIPTSOURCE"));
    for u in [&b"example.com/first"[..], b"example.com/after"] {
        assert!(
            contains(&r.output, u) || raw_contains(&r.output, u),
            "{}",
            String::from_utf8_lossy(u)
        );
    }
}

/// An image's soft mask is redacted with it: its samples inside the
/// rect made opaque in a copy, the original not carried — or, when the
/// image is still drawn elsewhere, kept with it and reported.
#[test]
fn soft_masks() {
    let mask: Vec<u8> = (0..256).map(|i| (i * 7 % 200 + 20) as u8).collect();
    let make = |content: &[u8]| {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", content)),
            (
                5,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 16 /Height 16 \
                     /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R",
                    &secret_samples(),
                ),
            ),
            (
                6,
                stream_obj(
                    "/Type /XObject /Subtype /Image /Width 16 /Height 16 \
                     /ColorSpace /DeviceGray /BitsPerComponent 8",
                    &mask,
                ),
            ),
        ])
    };
    let pdf = make(b"q 100 0 0 100 50 50 cm /Im1 Do Q");
    let r = run(&pdf, vec![(40.0, 40.0, 100.0, 100.0)]).unwrap();
    assert_eq!(r.images_edited, 1);
    assert!(
        !contains(&r.output, &mask),
        "the original mask is not carried"
    );
    let doc = Document::from_bytes(&r.output).unwrap();
    let res = doc.pages()[0].resources().cloned().unwrap();
    let img = doc.resolve_resource(&res, "XObject", b"Im1").unwrap();
    assert!(img.is_none(), "the original's name is gone");
    // Still drawn elsewhere: the original and its mask stay, reported.
    let pdf = make(b"q 40 0 0 40 50 50 cm /Im1 Do Q q 40 0 0 40 150 150 cm /Im1 Do Q");
    let r = run(&pdf, vec![(40.0, 40.0, 100.0, 100.0)]).unwrap();
    assert!(contains(&r.output, &mask));
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.removed == kasane_pdf::Removed::Image));
}

/// An appearance-less line's endings reach past its geometry — up to
/// 9.5 × the border width in Poppler and MuPDF: they make it a hit.
/// A caption or a cloudy border is drawn to no known extent: refused.
/// (Independent review, P0.)
#[test]
fn appearance_less_line_endings_and_decorations() {
    let line = |extra: &str| {
        annot_page(
            "30 0 R",
            vec![(
                30,
                format!(
                    "<< /Type /Annot /Subtype /Line /Rect [170 170 190 190] \
                     /L [80 80 110 80] /C [0 0 0] /IC [0 0 0] /BS << /W 2 >> {extra} >>"
                )
                .into_bytes(),
            )],
        )
    };
    // Without endings the line (y 80 ± 2) is clear of the rect (y ≥ 90).
    assert_eq!(annots_run(&line("")).unwrap().annotations_removed, 0);
    let r = annots_run(&line("/LE [/ClosedArrow /None]")).unwrap();
    assert_eq!(r.annotations_removed, 1, "the arrowhead reaches the rect");
    assert!(
        unsupported(run(&line("/LE [/ClosedArrow /None]"), vec![RECT]))
            .contains("other than a link")
    );
    assert_eq!(
        annots_run(&line("/LL 15")).unwrap().annotations_removed,
        1,
        "a leader line"
    );
    assert!(unsupported(annots_run(&line("/Cap true"))).contains("no known extent"));
    assert!(unsupported(annots_run(&line("/BE << /S /C /I 1 >>"))).contains("no known extent"));
}

/// An appearance-less highlight's rounded ends reach past its
/// quadrilaterals (0.19 × their height in Poppler and MuPDF): 2 pt from
/// a rect, a 20 pt high highlight meets it. (Release review: the
/// extent was the quadrilaterals grown by the border width.)
#[test]
fn appearance_less_highlight_ends() {
    let pdf = annot_page(
        "30 0 R",
        vec![(
            30,
            b"<< /Type /Annot /Subtype /Highlight /Rect [10 150 40 170] \
              /QuadPoints [10 170 40 170 10 150 40 150] /C [1 1 0] >>"
                .to_vec(),
        )],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let rects = vec![RECT, (42.0, 150.0, 60.0, 170.0)];
    let r = redact(
        &doc,
        &[(0, rects.clone())],
        &RedactOptions::new().remove_annotations(true),
    )
    .unwrap();
    assert_eq!(r.annotations_removed, 1);
    let far = vec![RECT, (55.0, 150.0, 60.0, 170.0)];
    let r = redact(
        &doc,
        &[(0, far)],
        &RedactOptions::new().remove_annotations(true),
    )
    .unwrap();
    assert_eq!(r.annotations_removed, 0, "beyond half its height: kept");
}

/// The diagnosis lists a soft mask among an image's blockers only when
/// redaction cannot edit it.
#[test]
fn diagnosis_soft_mask_blockers() {
    let make = |mask_extra: &str| {
        build_pdf(&[
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
                    "/Type /XObject /Subtype /Image /Width 16 /Height 16 \
                     /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask 6 0 R",
                    &secret_samples(),
                ),
            ),
            (
                6,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Image /Width 16 /Height 16 \
                         /ColorSpace /DeviceGray /BitsPerComponent 8 {mask_extra}"
                    ),
                    &[7u8; 256],
                ),
            ),
        ])
    };
    let blockers = |pdf: &[u8]| {
        let doc = Document::from_bytes(pdf).unwrap();
        let d = kasane_pdf::diagnose_redaction(
            &doc,
            0,
            &[(40.0, 40.0, 100.0, 100.0)],
            &kasane_pdf::Limits::default(),
        )
        .unwrap();
        d.targets
            .iter()
            .filter_map(|t| match &t.detail {
                kasane_pdf::TargetDetail::Image { blockers, .. } => Some(blockers.join(" ")),
                _ => None,
            })
            .collect::<String>()
    };
    assert!(!blockers(&make("")).contains("SMask"));
    assert!(blockers(&make("/Matte [0 0 0]")).contains("/SMask"));
}

/// An edited page's original content stream may not stay in the output
/// by any route — a private catalog entry, application data, an
/// attachment: verification fails, nothing is written. A content stream
/// another page still draws stays, its text reported. (Release review.)
#[test]
fn an_edited_pages_original_content_does_not_stay() {
    for (cat, page_extra) in [
        ("/XYZPrivate 4 0 R", ""),
        ("/PieceInfo << /App << /Private 4 0 R >> >>", ""),
        (
            "/Names << /EmbeddedFiles << /Names [(x) << /Type /Filespec /F (x) \
             /EF << /F 4 0 R >> >>] >> >>",
            "",
        ),
        ("", "/PieceInfo << /App << /Private 4 0 R >> >>"),
    ] {
        let pdf = one_page(SECRET, page_extra, cat, vec![]);
        let e = unsupported(run(&pdf, vec![RECT]));
        assert!(e.contains("original content"), "{cat}{page_extra}: {e}");
    }
    // Shared with a page that is not edited: kept there, reported.
    let pdf = build_pdf(&{
        let mut o = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (
                2,
                b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>".to_vec(),
            ),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /Font << /F1 20 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", SECRET.as_bytes())),
            (
                8,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /Font << /F1 20 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
        ];
        o.extend(font_objs());
        o
    });
    let r = run(&pdf, vec![RECT]).unwrap();
    assert!(r
        .elsewhere
        .iter()
        .any(|e| e.location == ElsewhereLocation::Page(1)));
}

/// Decorations given indirectly count too: an indirect line ending, a
/// highlight whose quadrilateral coordinates are references.
/// (Release review.)
#[test]
fn indirect_annotation_decorations() {
    let pdf = annot_page(
        "30 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Line /Rect [170 170 190 190] \
                  /L [80 80 110 80] /LE [31 0 R /None] /C [0 0 0] /BS << /W 2 >> >>"
                    .to_vec(),
            ),
            (31, b"/ClosedArrow".to_vec()),
        ],
    );
    assert_eq!(annots_run(&pdf).unwrap().annotations_removed, 1);
    let pdf = annot_page(
        "30 0 R",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Highlight /Rect [10 150 40 170] \
                  /QuadPoints [10 31 0 R 40 31 0 R 10 150 40 150] /C [1 1 0] >>"
                    .to_vec(),
            ),
            (31, b"170".to_vec()),
        ],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT, (42.0, 150.0, 60.0, 170.0)])],
        &RedactOptions::new().remove_annotations(true),
    )
    .unwrap();
    assert_eq!(r.annotations_removed, 1);
}

/// What `redact` removes may not stay through another reference (here a
/// private catalog entry): a removed link's or annotation's appearance,
/// the images and masks it draws, a copy of the annotation dictionary
/// as a stream's dictionary, a removed Type 3 glyph's procedure, the
/// font's former `/ToUnicode` and the masks of images only removed
/// glyphs drew, an edited page's thumbnail. Verification fails; nothing
/// is written. (Release review.)
#[test]
fn removed_objects_do_not_stay_through_other_references() {
    let image = "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8";
    let mask: Vec<u8> = vec![
        10, 230, 40, 180, 70, 110, 50, 240, 120, 45, 90, 210, 30, 160, 80, 220,
    ];
    let page =
        |content: &str, res: &str, page_extra: &str, cat: &str, extra: Vec<(u32, Vec<u8>)>| {
            let mut objs = vec![
                (
                    1,
                    format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
                ),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (
                    3,
                    format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {res} >> \
                     /Contents 4 0 R {page_extra} >>"
                )
                    .into_bytes(),
                ),
                (4, stream_obj("", content.as_bytes())),
            ];
            objs.extend(extra);
            build_pdf(&objs)
        };
    let opts = RedactOptions::new().remove_annotations(true);
    let refused = |pdf: &[u8], what: &str| {
        let doc = Document::from_bytes(pdf).unwrap();
        let e = unsupported(redact(&doc, &[(0, vec![RECT])], &opts));
        assert!(e.contains("failed verification"), "{what}: {e}");
    };
    // Appearances of a removed link and stamp: the form, its image, the mask.
    for subtype in ["Link", "Stamp"] {
        for alias in [41, 42, 43] {
            let pdf = page(
                "",
                "",
                "/Annots [40 0 R]",
                &format!("/Alias {alias} 0 R"),
                vec![
                    (
                        40,
                        format!(
                            "<< /Type /Annot /Subtype /{subtype} /Rect [50 100 70 110] \
                             /AP << /N 41 0 R >> >>"
                        )
                        .into_bytes(),
                    ),
                    (
                        41,
                        stream_obj(
                            "/Type /XObject /Subtype /Form /BBox [0 0 20 10] \
                             /Resources << /XObject << /Im 42 0 R >> >>",
                            b"q 20 0 0 10 0 0 cm /Im Do Q",
                        ),
                    ),
                    (
                        42,
                        stream_obj(&format!("{image} /SMask 43 0 R"), &[0u8; 16]),
                    ),
                    (43, stream_obj(image, &mask)),
                ],
            );
            refused(&pdf, &format!("{subtype} {alias}"));
        }
    }
    // A removed annotation's dictionary kept as a stream's dictionary.
    let annot = "/Type /Annot /Subtype /Text /Rect [50 100 70 110] /Contents (SECRETANNOT)";
    let pdf = page(
        "",
        "",
        "/Annots [40 0 R]",
        "/Alias 41 0 R",
        vec![
            (40, format!("<< {annot} /Length 0 >>").into_bytes()),
            (41, stream_obj(annot, b"")),
        ],
    );
    refused(&pdf, "annotation dictionary as a stream's");
    // Type 3: the removed glyph's procedure, the font's former
    // ToUnicode, the mask of the image only the removed glyph drew.
    let t3 = |procs: &str, extra: Vec<(u32, Vec<u8>)>, alias: u32| {
        let mut objs = vec![(
            20,
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 700] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs {procs} \
                 /Encoding << /Differences [65 /A /B] >> /FirstChar 65 /LastChar 66 \
                 /Widths [600 600] /Resources << /XObject << /Im 44 0 R >> >> /ToUnicode 46 0 R >>"
            )
            .into_bytes(),
        )];
        objs.extend(extra);
        page(
            "BT /F 12 Tf 50 100 Td (A) Tj ET BT /F 12 Tf 10 20 Td (B) Tj ET",
            "/Font << /F 20 0 R >>",
            "",
            &format!("/Alias {alias} 0 R"),
            objs,
        )
    };
    let common = || {
        vec![
            (40, b"<< /A 41 0 R /B 42 0 R >>".to_vec()),
            (
                41,
                stream_obj("", b"600 0 d0 q 500 0 0 700 0 0 cm /Im Do Q"),
            ),
            (42, stream_obj("", b"600 0 d0 0 0 100 700 re f")),
            (
                44,
                stream_obj(&format!("{image} /SMask 45 0 R"), &[0u8; 16]),
            ),
            (45, stream_obj(image, &mask)),
            (
                46,
                stream_obj(
                    "",
                    b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                      /CMapName /TU def 1 begincodespacerange <00> <FF> endcodespacerange \
                      2 beginbfchar <41> <0053> <42> <0042> endbfchar endcmap \
                      CMapName currentdict /CMap defineresource pop end end",
                ),
            ),
        ]
    };
    for (what, alias) in [
        ("procedure", 41),
        ("procedures", 40),
        ("mask", 45),
        ("ToUnicode", 46),
    ] {
        refused(&t3("40 0 R", common(), alias), what);
    }
    // An edited page's thumbnail.
    let pdf = page(
        "q 40 0 0 40 50 90 cm /Im Do Q",
        "/XObject << /Im 42 0 R >>",
        "/Thumb 40 0 R",
        "/Alias 40 0 R",
        vec![
            (
                40,
                stream_obj(
                    "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                    &[1, 2, 3, 4],
                ),
            ),
            (42, stream_obj(image, &(0..16).collect::<Vec<u8>>())),
        ],
    );
    refused(&pdf, "thumbnail");
}

/// A removed annotation's appearance drawing through a pattern, a soft
/// mask group or a Type 3 font, or naming resources it does not have, is
/// refused: what those hold would not be checked. (Release review.)
#[test]
fn removed_appearances_through_shared_resources_are_refused() {
    let ap = |res: &str, body: &[u8]| {
        annot_page(
            "30 0 R",
            vec![
                (
                    30,
                    b"<< /Type /Annot /Subtype /Stamp /Rect [60 95 100 105] /AP << /N 31 0 R >> >>"
                        .to_vec(),
                ),
                (
                    31,
                    stream_obj(
                        &format!("/Type /XObject /Subtype /Form /BBox [0 0 40 10] {res}"),
                        body,
                    ),
                ),
                (
                    32,
                    stream_obj(
                        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 \
                         /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << >>",
                        b"0 0 1 1 re f",
                    ),
                ),
            ],
        )
    };
    for (res, body) in [
        (
            "/Resources << /Pattern << /P 32 0 R >> >>",
            &b"/Pattern cs /P scn 0 0 40 10 re f"[..],
        ),
        (
            "/Resources << /ExtGState << /G << /SMask << /S /Luminosity /G 32 0 R >> >> >> >>",
            b"/G gs 0 0 40 10 re f",
        ),
        ("", b"/Pattern cs /P scn 0 0 40 10 re f"),
        ("", b"/G gs 0 0 40 10 re f"),
        // Resources of its own that lack the name: resolved elsewhere.
        (
            "/Resources << /ProcSet [/PDF] >>",
            b"/Pattern cs /P scn 0 0 40 10 re f",
        ),
        ("/Resources << /Pattern << >> >>", b"/P1 gs 0 0 40 10 re f"),
        ("/Resources 5", b"0 0 40 10 re f"),
        (
            "/Resources << /ExtGState << /G << /Font [20 0 R 12] >> >> >>",
            b"/G gs BT (x) Tj ET",
        ),
        // Entries that are there but unusable: Poppler resolves the name
        // in the resources it shares.
        ("/Resources << /XObject << /X1 99 0 R >> >>", b"/X1 Do"),
        ("/Resources << /XObject << /X1 5 >> >>", b"/X1 Do"),
        (
            "/Resources << /Font << /T3 99 0 R >> >>",
            b"BT /T3 12 Tf (x) Tj ET",
        ),
        (
            "/Resources << /Font << /T3 5 >> >>",
            b"BT /T3 12 Tf (x) Tj ET",
        ),
        // A stream is no font dictionary (Poppler passes it over).
        (
            "/Resources << /Font << /T3 32 0 R >> >>",
            b"BT /T3 12 Tf (x) Tj ET",
        ),
        (
            "/Resources << /Font << /T3 /None >> >>",
            b"BT /T3 12 Tf (x) Tj ET",
        ),
    ] {
        let e = unsupported(annots_run(&ap(res, body)));
        assert!(e.contains("resources it shares"), "{res}: {e}");
    }
    // Plain drawing in an appearance without resources is removed.
    assert_eq!(
        annots_run(&ap("", b"0 0 40 10 re f"))
            .unwrap()
            .annotations_removed,
        1
    );
}

/// A removed Type 3 glyph's procedure kept as some dictionary's
/// `/ToUnicode` is not a font's glyph: verification fails.
/// (Release review.)
#[test]
fn a_removed_glyph_procedure_is_not_excused_as_a_tounicode() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
              /Font << /F 20 0 R /G 21 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                b"BT /F 12 Tf 50 100 Td (A) Tj ET BT /F 12 Tf 10 20 Td (B) Tj ET",
            ),
        ),
        (
            20,
            b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 700] \
              /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 41 0 R /B 42 0 R >> \
              /Encoding << /Differences [65 /A /B] >> /FirstChar 65 /LastChar 66 \
              /Widths [600 600] /Resources << >> >>"
                .to_vec(),
        ),
        (
            21,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 41 0 R >>".to_vec(),
        ),
        (
            41,
            stream_obj("", b"600 0 d0 0 0 500 700 re f % SECRETGLYPH"),
        ),
        (42, stream_obj("", b"600 0 d0 0 0 100 700 re f")),
    ]);
    let e = unsupported(run(&pdf, vec![RECT]));
    assert!(e.contains("glyph's procedure"), "{e}");
}

/// A thumbnail an edited page shares with another page leaves both.
#[test]
fn a_shared_thumbnail_leaves_every_page() {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 20 0 R >> >> /Contents 4 0 R /Thumb 30 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", SECRET.as_bytes())),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R \
              /Thumb 30 0 R >>"
                .to_vec(),
        ),
        (6, stream_obj("", b"")),
        (
            30,
            stream_obj(
                "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[1, 2, 3, 4],
            ),
        ),
    ];
    objs.extend(font_objs());
    let r = run(&build_pdf(&objs), vec![RECT]).unwrap();
    assert_eq!(r.thumbnails_removed, 2);
    let doc = Document::from_bytes(&r.output).unwrap();
    for p in doc.pages() {
        let d = doc.resolve(p.object_ref().unwrap()).unwrap();
        assert!(!d.as_dict().unwrap().contains_key(b"Thumb".as_slice()));
    }
}

/// What a removed link's appearance paints with — an Indexed palette (its
/// bytes whitespace or not), a palette a form uses under an inherited
/// colour, a transfer function a later `/SMask /None` does not end, a
/// `/DefaultGray` space's tint function — may not stay through another
/// reference, nor be excused as some font's `/ToUnicode`. (Review.)
#[test]
fn removed_appearance_colour_data_does_not_stay() {
    let func = |data: &[u8]| {
        stream_obj(
            "/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2] /BitsPerSample 8",
            data,
        )
    };
    let link = |res: &str, body: &[u8], extra: Vec<(u32, Vec<u8>)>, cat: &str| {
        let mut objs = vec![
            (
                1,
                format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                  /Annots [30 0 R] >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"0 0 5 5 re f")),
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [70 90 110 100] /AP << /N 31 0 R >> >>"
                    .to_vec(),
            ),
            (
                31,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 40 10] /Resources << {res} >>"
                    ),
                    body,
                ),
            ),
        ];
        objs.extend(extra);
        build_pdf(&objs)
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "whitespace palette",
            link(
                "/ColorSpace << /C [/Indexed /DeviceRGB 0 40 0 R] >>",
                b"/C cs 0 sc 0 0 10 10 re f",
                vec![(40, stream_obj("", b" \t\n"))],
                "/Private 40 0 R",
            ),
        ),
        (
            "inherited palette",
            link(
                "/ColorSpace << /C1 [/Indexed /DeviceRGB 0 40 0 R] /C2 [/Indexed /DeviceRGB 0 41 0 R] >> \
                 /XObject << /Fm 42 0 R >>",
                b"/C1 cs 0 sc /Fm Do /C2 cs 0 sc /Fm Do",
                vec![
                    (40, stream_obj("", b"\x10\x20\x30")),
                    (41, stream_obj("", b"\x51\x62\x73")),
                    (42, stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"0 0 10 10 re f")),
                ],
                "/Private 41 0 R",
            ),
        ),
        (
            "transfer function past /SMask /None",
            link(
                "/ExtGState << /G << /TR 40 0 R >> /Clear << /SMask /None >> >>",
                b"/G gs /Clear gs 0 0 10 10 re f",
                vec![(40, func(b"\x00\x10\x20\xf0\xe0\xd0"))],
                "/Private 40 0 R",
            ),
        ),
        (
            "default grey",
            link(
                "/ColorSpace << /DefaultGray [/Separation /S /DeviceRGB 40 0 R] >>",
                b"0 g 0 0 10 10 re f",
                vec![(40, func(b"\xff\x00\x00\x00\x00\xff"))],
                "/Private 40 0 R",
            ),
        ),
    ];
    for (name, pdf) in cases {
        let doc = Document::from_bytes(&pdf).unwrap();
        let e = unsupported(redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()));
        assert!(e.contains("failed verification"), "{name}: {e}");
    }
}

/// A removed link's appearance may stay only where a reader draws it:
/// not as a Hidden annotation's appearance, another annotation's down
/// appearance, an unselected state, or a form under optional content.
/// (Review.)
#[test]
fn removed_appearance_is_not_excused_by_what_is_not_drawn() {
    let page_with = |annot2: &str, content: &str, res: &str| {
        build_pdf(&[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [40 0 R] /D << /OFF [40 0 R] >> >> >>"
                    .to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                     /Resources << {res} >> /Annots [30 0 R 32 0 R] >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", content.as_bytes())),
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [70 90 110 100] /AP << /N 31 0 R >> >>".to_vec(),
            ),
            (
                31,
                stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 40 10]", b"0 0 40 10 re f 1 1 3 3 re f"),
            ),
            (32, annot2.as_bytes().to_vec()),
            (
                33,
                stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 40 40]", b"0 0 9 9 re f"),
            ),
            (40, b"<< /Type /OCG /Name (off) >>".to_vec()),
        ])
    };
    let other =
        |extra: &str| format!("<< /Type /Annot /Subtype /Link /Rect [150 150 190 190] {extra} >>");
    for (name, pdf) in [
        (
            "hidden",
            page_with(&other("/F 2 /AP << /N 31 0 R >>"), "0 0 5 5 re f", ""),
        ),
        (
            "down",
            page_with(&other("/AP << /N 33 0 R /D 31 0 R >>"), "0 0 5 5 re f", ""),
        ),
        (
            "state",
            page_with(
                &other("/AS /Off /AP << /N << /Off 33 0 R /On 31 0 R >> >>"),
                "0 0 5 5 re f",
                "",
            ),
        ),
        (
            "optional content",
            page_with(
                &other(""),
                "/OC /L BDC q 1 0 0 1 150 150 cm /Fm Do Q EMC",
                "/XObject << /Fm 31 0 R >> /Properties << /L 40 0 R >>",
            ),
        ),
    ] {
        let doc = Document::from_bytes(&pdf).unwrap();
        let e = unsupported(redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()));
        assert!(e.contains("failed verification"), "{name}: {e}");
    }
}

/// An image shared by every page of a long document, edited on one: the
/// pages not edited are walked within the operation bound each, not all
/// together. (Review: 0.9 refused this for its length.)
#[test]
fn a_long_document_sharing_an_edited_image() {
    let pages = 120;
    let mut filler = String::new();
    for _ in 0..1000 {
        filler.push_str("0 0 1 1 re n ");
    }
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {pages} >>",
                (0..pages)
                    .map(|i| format!("{} 0 R", 100 + 2 * i))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes(),
        ),
        (
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8",
                &[11, 22, 33, 44],
            ),
        ),
    ];
    for i in 0..pages {
        objs.push((
            100 + 2 * i,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} 0 R \
                 /Resources << /XObject << /Im 5 0 R >> >> >>",
                101 + 2 * i
            )
            .into_bytes(),
        ));
        objs.push((
            101 + 2 * i,
            stream_obj(
                "",
                format!("{filler} q 40 0 0 40 40 80 cm /Im Do Q").as_bytes(),
            ),
        ));
    }
    let pdf = build_pdf(&objs);
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()).unwrap();
    assert_eq!(r.images_edited, 1);
}

/// The same link on every page: the one meeting the rect goes, the
/// identical ones on other pages stay (refused before). (Review.)
#[test]
fn identical_links_on_other_pages_stay() {
    let link = b"<< /Type /Annot /Subtype /Link /Rect [70 90 110 100] /A << /S /URI /URI (https://example.com/) >> >>";
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [10 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"0 0 5 5 re f")),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [11 0 R] >>"
                .to_vec(),
        ),
        (10, link.to_vec()),
        (11, link.to_vec()),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()).unwrap();
    assert_eq!(r.links_removed, 1);
    let out = Document::from_bytes(&r.output).unwrap();
    let annots = |i: usize| {
        out.deref(
            out.resolve(out.pages()[i].object_ref().unwrap())
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Annots".as_slice())
                .unwrap_or(&kasane_pdf::Object::Null),
        )
        .unwrap()
        .as_array()
        .map_or(0, |a| a.len())
    };
    assert_eq!((annots(0), annots(1)), (0, 1));
}
