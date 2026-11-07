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
    assert!(unsupported(run(&annots, vec![RECT])).contains("other than links"));
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

/// A Type 3 glyph may paint anywhere: refused, not estimated.
#[test]
fn type3_text_is_refused() {
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
    let e = unsupported(run(&pdf, vec![(90.0, 100.0, 110.0, 120.0)]));
    assert!(e.contains("Type 3"), "{e}");
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

/// Text clipped away from the rect shows only outside it: kept.
#[test]
fn text_clipped_away_from_the_rect_is_kept() {
    let pdf = one_page(
        "q 65 0 100 200 re W n BT /F1 40 Tf 50 100 Td (M) Tj ET Q",
        "",
        "",
        vec![],
    );
    let r = run(&pdf, vec![(50.0, 100.0, 60.0, 140.0)]).unwrap();
    assert_eq!(r.codes_removed, 0);
    assert_eq!(page_text(&r.output, 0), "M");
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

/// A clip that is not a rectangle: its bounds meeting a rect does not
/// mean anything shows there. Refused rather than guessed.
#[test]
fn non_rectangular_clip_is_refused() {
    let pdf = one_page(
        "q 0 0 200 200 re 50 100 15 40 re W* n BT /F1 40 Tf 50 100 Td (M) Tj ET Q",
        "",
        "",
        vec![],
    );
    let e = unsupported(run(&pdf, vec![(50.0, 100.0, 60.0, 140.0)]));
    assert!(e.contains("not a rectangle"), "{e}");
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
    let dashed = one_page("[4 2] 0 d 2 w 10 100 m 190 100 l S", "", "", vec![]);
    let e = unsupported(run(&dashed, vec![(50.0, 90.0, 100.0, 110.0)]));
    assert!(e.contains("dashed"), "{e}");
    // A right-angle corner at (100, 100) whose miter tip, 2.83 out,
    // points at a rect 2.5 away: the segments clear it, the join may
    // not.
    let sharp = one_page("4 w 10 M 50 100 m 100 100 l 100 50 l S", "", "", vec![]);
    let e = unsupported(run(&sharp, vec![(102.5, 102.5, 150.0, 150.0)]));
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
