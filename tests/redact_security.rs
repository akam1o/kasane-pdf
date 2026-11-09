//! Security regression suite for `redact`: non-interference.
//!
//! Each case builds two inputs that differ only in what the rects
//! remove — text, its positions and advances, glyph programs, image
//! samples, path coordinates, shading functions, pattern cells,
//! replacement text, annotation appearances — and requires that the
//! two redactions give byte-identical outputs: then nothing removed can
//! reach a reader by any route (an operand, a resource, a residue of
//! interpolation, a reference). Where a structure is refused, both
//! inputs must be refused for the same reason. The equivalence is the
//! whole file, so it also covers resources, fonts and metadata.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{redact, Document, RedactOptions};

const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);

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

/// A one-page document: `content`, `/Resources << res >>` (the font is
/// /F1), `page_extra` in the page dictionary, and `extra` objects.
fn page(content: &str, res: &str, page_extra: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                 /Font << /F1 20 0 R >> {res} >> /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

/// Text kept outside the rect, so every output has something to keep.
const KEPT: &str = "BT /F1 10 Tf 10 20 Td (PUBLICWORD) Tj ET";

/// Redact `make(1)` and `make(2)` (which must differ) with `opts`:
/// both outputs byte-identical, or both refused alike.
fn independent(name: &str, make: impl Fn(u32) -> Vec<u8>, opts: &RedactOptions) {
    let (a, b) = (make(1), make(2));
    assert_ne!(a, b, "{name}: the inputs do not differ");
    let run = |pdf: &[u8]| {
        let doc = Document::from_bytes(pdf).unwrap();
        redact(&doc, &[(0, vec![RECT])], opts)
    };
    match (run(&a), run(&b)) {
        (Ok(x), Ok(y)) => {
            if x.output != y.output {
                for o in [&x.output, &y.output] {
                    let d = Document::from_bytes(o).unwrap();
                    let ops = d.page_operations(&d.pages()[0]).unwrap();
                    eprintln!(
                        "{name}: {}",
                        ops.iter()
                            .map(|o| o.to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
            assert!(x.output == y.output, "{name}: the outputs differ");
            let doc = Document::from_bytes(&x.output).unwrap();
            let ops = doc.page_operations(&doc.pages()[0]).unwrap();
            assert!(!ops.is_empty(), "{name}: nothing kept");
        }
        // The message names the first glyph refused: compare the rest.
        (Err(x), Err(y)) => {
            let mask = |e: &kasane_pdf::Error| {
                let s = e.to_string();
                match s.find("glyph 0x") {
                    Some(i) => {
                        let j = s[i + 8..].find(' ').map_or(s.len(), |k| i + 8 + k);
                        format!("{}glyph{}", &s[..i], &s[j..])
                    }
                    None => s,
                }
            };
            assert_eq!(mask(&x), mask(&y), "{name}: refused unlike")
        }
        (x, y) => panic!(
            "{name}: one redacted, one refused: {:?} / {:?}",
            x.map(|r| r.codes_removed),
            y.map(|r| r.codes_removed)
        ),
    }
}

fn accepted(name: &str, make: impl Fn(u32) -> Vec<u8>, opts: &RedactOptions) {
    let doc = Document::from_bytes(&make(1)).unwrap();
    redact(&doc, &[(0, vec![RECT])], opts).unwrap_or_else(|e| panic!("{name}: {e}"));
    independent(name, make, opts);
}

fn refused(name: &str, make: impl Fn(u32) -> Vec<u8>, opts: &RedactOptions) {
    let doc = Document::from_bytes(&make(1)).unwrap();
    assert!(
        redact(&doc, &[(0, vec![RECT])], opts).is_err(),
        "{name}: accepted"
    );
    independent(name, make, opts);
}

/// A secret per variant: different text, different length — at most
/// 7 glyphs (42 pt at 10 pt), so from x 80 it stays inside the rect.
fn secret(v: u32) -> &'static str {
    ["", "ALPHA", "BRAVOXY"][v as usize]
}

fn image(w: u32, h: u32, cs: &str, n: usize, v: u32, extra: &str) -> Vec<u8> {
    let data: Vec<u8> = (0..(w * h) as usize * n)
        .map(|i| ((i as u32 * 31 + v * 97) % 251) as u8)
        .collect();
    stream_obj(
        &format!(
            "/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace {cs} \
             /BitsPerComponent 8 {extra}"
        ),
        &data,
    )
}

// ---- coordinates and text ------------------------------------------

#[test]
fn text_under_nested_transforms() {
    let o = RedactOptions::new();
    accepted(
        "nested cm",
        |v| {
            let c = format!(
                "q 2 0 0 2 0 0 cm q 0.5 0 0 0.5 0 0 cm BT /F1 10 Tf 70 100 Td ({}) Tj ET Q Q {KEPT}",
                secret(v)
            );
            page(&c, "", "", vec![])
        },
        &o,
    );
    accepted(
        "form /Matrix",
        |v| {
            let form = stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Matrix [1 0 0 1 50 0] \
                 /Resources << /Font << /F1 20 0 R >> >>",
                format!("BT /F1 10 Tf 20 100 Td ({}) Tj ET", secret(v)).as_bytes(),
            );
            page(
                &format!("/Fm Do {KEPT}"),
                "/XObject << /Fm 40 0 R >>",
                "",
                vec![(40, form)],
            )
        },
        &o,
    );
    refused(
        "strongly anisotropic text",
        |v| {
            page(
                &format!(
                    "BT /F1 10 Tf 0.1 0 0 10 80 100 Tm ({}) Tj ET {KEPT}",
                    secret(v)
                ),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
}

#[test]
fn removed_text_positions_and_advances() {
    let o = RedactOptions::new();
    accepted(
        "TJ kerns between removed glyphs",
        |v| {
            // The kept prefix ends at 40; the removed glyphs start at 65.
            let k = v * 13;
            page(
                &format!(
                    "BT /F1 10 Tf 10 100 Td [(Name:) -2500 (S) -{k} (E) -{} (C) {k} (R) -{} (ET)] TJ \
                     1 0 0 1 150 100 Tm (kept) Tj ET {KEPT}",
                    k * 2,
                    k * 3
                ),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
    accepted(
        "per-glyph Tm",
        |v| {
            let glyphs: String = secret(v)
                .chars()
                .enumerate()
                .map(|(i, c)| format!("1 0 0 1 {} 100 Tm ({c}) Tj ", 70.0 + 5.3 * i as f64))
                .collect();
            page(
                &format!("BT /F1 10 Tf {glyphs}1 0 0 1 150 100 Tm (kept) Tj ET {KEPT}"),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
    accepted(
        "per-word Td with leading",
        |v| {
            page(
                &format!(
                    "BT /F1 10 Tf 12 TL 10 100 Td (Name:) Tj {} 0 Td ({}) Tj T* ({}) ' \
                     ET {KEPT}",
                    60 + v,
                    secret(v),
                    secret(v)
                ),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
}

#[test]
fn replacement_text_and_operators_in_text() {
    let o = RedactOptions::new();
    for key in ["ActualText", "Alt", "E"] {
        accepted(
            key,
            |v| {
                page(
                    &format!(
                        "/Span << /{key} ({}) /MCID 0 >> BDC BT /F1 10 Tf 80 100 Td (X) Tj ET EMC {KEPT}",
                        secret(v)
                    ),
                    "",
                    "",
                    vec![],
                )
            },
            &o,
        );
    }
    refused(
        "a form drawn inside a text object among removed text",
        |v| {
            let form = stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b"");
            page(
                &format!(
                    "BT /F1 10 Tf 80 100 Td ({}) Tj /Fm Do 1 0 0 1 150 100 Tm (k) Tj ET {KEPT}",
                    secret(v)
                ),
                "/XObject << /Fm 40 0 R >>",
                "",
                vec![(40, form)],
            )
        },
        &o,
    );
}

#[test]
fn type3_and_non_embedded_fonts() {
    let o = RedactOptions::new();
    accepted(
        "Type 3 glyph shown only in the rect",
        |v| {
            let font = format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /A 51 0 R /B 52 0 R >> \
                 /Encoding << /Type /Encoding /Differences [65 /A /B] >> /FirstChar 65 \
                 /LastChar 66 /Widths [1000 {}] /Resources << >> >>",
                500 + v
            );
            page(
                "BT /T3 10 Tf 10 20 Td (A) Tj 80 80 Td (B) Tj ET",
                "/Font << /F1 20 0 R /T3 50 0 R >>",
                "",
                vec![
                    (50, font.into_bytes()),
                    (51, stream_obj("", b"1000 0 d0 0 0 500 500 re f")),
                    (
                        52,
                        stream_obj("", format!("1000 0 d0 {v} 0 600 700 re f").as_bytes()),
                    ),
                ],
            )
        },
        &o,
    );
    refused(
        "non-embedded font",
        |v| {
            page(
                &format!("BT /H 10 Tf 80 100 Td ({}) Tj ET {KEPT}", secret(v)),
                "/Font << /F1 20 0 R /H 60 0 R >>",
                "",
                vec![(
                    60,
                    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
                )],
            )
        },
        &o,
    );
}

// ---- drawing and images --------------------------------------------

#[test]
fn hidden_content_in_the_rect() {
    let o = RedactOptions::new();
    accepted(
        "clipped away",
        |v| {
            page(
                &format!(
                    "q 0 0 10 10 re W n BT /F1 10 Tf 80 100 Td ({}) Tj ET Q {KEPT}",
                    secret(v)
                ),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
    accepted(
        "optional content off",
        |v| {
            page(
                &format!(
                    "/OC /L BDC BT /F1 10 Tf 80 100 Td ({}) Tj ET EMC {KEPT}",
                    secret(v)
                ),
                "/Properties << /L 70 0 R >>",
                "",
                vec![(70, b"<< /Type /OCG /Name (L) >>".to_vec())],
            )
        },
        &o,
    );
}

#[test]
fn image_samples_in_the_rect() {
    let o = RedactOptions::new();
    for (cs, n, extra, objs) in [
        ("/DeviceRGB", 3, "/Interpolate true", vec![]),
        ("/DeviceGray", 1, "", vec![]),
        ("/DeviceCMYK", 4, "", vec![]),
        (
            "[/ICCBased 81 0 R]",
            3,
            "",
            vec![(81, stream_obj("/N 3 /Alternate /DeviceRGB", b"p"))],
        ),
        (
            "[/Indexed /DeviceRGB 255 82 0 R]",
            1,
            "/Decode [0 255]",
            vec![(82, stream_obj("", &[9u8; 768]))],
        ),
    ] {
        accepted(
            &format!("image {cs} {extra}"),
            |v| {
                let mut x = objs.clone();
                // Samples only inside the rect differ: a 10×10 image over
                // 0…200, the rect over samples 3…6 (whole) in each axis.
                let mut img = image(10, 10, cs, n, 1, extra);
                if v == 2 {
                    let data: Vec<u8> = (0..100 * n)
                        .map(|i| {
                            let (x, y) = ((i / n) % 10, 9 - (i / n) / 10);
                            if (3..7).contains(&x) && (3..7).contains(&y) {
                                ((i * 7) % 250) as u8
                            } else {
                                ((i as u32 * 31 + 97) % 251) as u8
                            }
                        })
                        .collect();
                    img = stream_obj(
                        &format!(
                            "/Type /XObject /Subtype /Image /Width 10 /Height 10 /ColorSpace {cs} \
                             /BitsPerComponent 8 {extra}"
                        ),
                        &data,
                    );
                }
                x.push((80, img));
                page(
                    &format!("q 200 0 0 200 0 0 cm /Im Do Q {KEPT}"),
                    "/XObject << /Im 80 0 R >>",
                    "",
                    x,
                )
            },
            &o,
        );
    }
    refused(
        "soft-masked image",
        |v| {
            let mask = image(10, 10, "/DeviceGray", 1, v, "");
            page(
                &format!("q 200 0 0 200 0 0 cm /Im Do Q {KEPT}"),
                "/XObject << /Im 80 0 R >>",
                "",
                vec![
                    (80, image(10, 10, "/DeviceRGB", 3, v, "/SMask 81 0 R")),
                    (81, mask),
                ],
            )
        },
        &o,
    );
    accepted(
        "image in a form",
        |v| {
            let form = stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /XObject << /Im 80 0 R >> >>",
                b"q 80 0 0 80 60 60 cm /Im Do Q",
            );
            page(
                &format!("/Fm Do {KEPT}"),
                "/XObject << /Fm 40 0 R >>",
                "",
                vec![(40, form), (80, image(4, 4, "/DeviceRGB", 3, v, ""))],
            )
        },
        &o,
    );
}

#[test]
fn strokes_shadings_and_patterns_in_the_rect() {
    let o = RedactOptions::new();
    for (gs, w) in [("/SA1 gs", "1"), ("", "0")] {
        accepted(
            &format!("stroke {gs} {w} w"),
            |v| {
                page(
                    &format!(
                        "{gs} {w} w 70 {} m 120 {} l S 10 10 m 30 30 l S {KEPT}",
                        70 + v,
                        100 + v
                    ),
                    "/ExtGState << /SA1 << /SA true >> >>",
                    "",
                    vec![],
                )
            },
            &o,
        );
    }
    accepted(
        "shading inside the rect",
        |v| {
            let sh = format!(
                "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 0] \
                 /Function << /FunctionType 2 /Domain [0 1] /C0 [0.{v} 0 0] /C1 [1 1 1] /N 1 >> >>"
            );
            page(
                &format!("q 70 70 20 20 re W n /S sh Q {KEPT}"),
                "/Shading << /S 90 0 R >>",
                "",
                vec![(90, sh.into_bytes())],
            )
        },
        &o,
    );
    accepted(
        "tiling pattern fill inside the rect",
        |v| {
            let cell = stream_obj(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 4 4] /XStep 4 /YStep 4 /Resources << >>",
                format!("0.{v} 0 0 rg 0 0 2 2 re f").as_bytes(),
            );
            page(
                &format!("/Pattern cs /P scn 70 70 20 20 re f 0 g {KEPT}"),
                "/Pattern << /P 91 0 R >>",
                "",
                vec![(91, cell)],
            )
        },
        &o,
    );
}

#[test]
fn annotation_appearances_in_the_rect() {
    let o = RedactOptions::new().remove_annotations(true);
    accepted(
        "markup annotation with its appearance",
        |v| {
            let ap = stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Resources << /Font << /F1 20 0 R >> >>",
                format!("BT /F1 8 Tf 2 2 Td ({}) Tj ET", secret(v)).as_bytes(),
            );
            page(
                KEPT,
                "",
                "/Annots [95 0 R]",
                vec![
                    (
                        95,
                        format!(
                            "<< /Type /Annot /Subtype /Square /Rect [80 80 100 100] /Contents ({}) \
                             /AP << /N 96 0 R >> >>",
                            secret(v)
                        )
                        .into_bytes(),
                    ),
                    (96, ap),
                ],
            )
        },
        &o,
    );
}

// ---- literal strings -----------------------------------------------

/// A raw CR in a shown string is a line feed to some readers and a
/// CR to others: a glyph after it lands in different places, inside
/// the rect for one reader, outside for another. Refused, whatever the
/// glyph holds.
#[test]
fn raw_line_ends_in_shown_strings() {
    let o = RedactOptions::new();
    refused(
        "raw CR before a glyph",
        |v| {
            // CR advances 10 pt, LF 60 pt at 20 pt: the box after them
            // lands at x 30 or x 80.
            let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                        /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /cr 51 0 R /lf 52 0 R /S 53 0 R >> \
                        /Encoding << /Type /Encoding /Differences [10 /lf 13 /cr 83 /S] >> \
                        /FirstChar 10 /LastChar 83 /Widths [3000 0 0 500 "
                .to_string()
                + &"0 ".repeat(69)
                + "1000] /Resources << >> >>";
            page(
                &format!("BT /T 20 Tf 20 90 Td (\r\rS) Tj ET {KEPT}"),
                "/Font << /F1 20 0 R /T 50 0 R >>",
                "",
                vec![
                    (50, font.into_bytes()),
                    (51, stream_obj("", b"500 0 d0")),
                    (52, stream_obj("", b"3000 0 d0")),
                    (
                        53,
                        stream_obj(
                            "",
                            format!("1000 0 d0 0 0 {} 1000 re f", 900 + v).as_bytes(),
                        ),
                    ),
                ],
            )
        },
        &o,
    );
    // Kept codes 0x0D and 0x0A, escaped or in hex, next to removed text.
    accepted(
        "kept CR and LF codes",
        |v| {
            page(
                &format!(
                    "BT /F1 10 Tf 80 100 Td ({}) Tj ET BT /F1 10 Tf 10 40 Td (A\\r\\nB\\015) Tj \
                     <0d0a43> Tj ET {KEPT}",
                    secret(v)
                ),
                "",
                "",
                vec![],
            )
        },
        &o,
    );
}

// ---- the structure tree ----------------------------------------------

/// A tagged page: `content` with `/StructParents 0`, the tree's
/// elements from object 31 on (root 30), `/ParentTree` `pt`, and
/// `extra` objects (object 41, when given, is the page's annotation).
fn tagged_page(
    content: &str,
    elements: &[String],
    pt: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let annots = if extra.iter().any(|(n, _)| *n == 41) {
        "/Annots [41 0 R]"
    } else {
        ""
    };
    let kids: Vec<String> = (0..elements.len())
        .map(|i| format!("{} 0 R", 31 + i))
        .collect();
    let mut objs = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 30 0 R \
              /MarkInfo << /Marked true >> >>"
                .to_vec(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                 /Font << /F1 20 0 R >> >> /Contents 4 0 R /StructParents 0 {annots} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            30,
            format!(
                "<< /Type /StructTreeRoot /K [{}] /ParentTree {pt} >>",
                kids.join(" ")
            )
            .into_bytes(),
        ),
    ];
    for (i, e) in elements.iter().enumerate() {
        objs.push((31 + i as u32, e.clone().into_bytes()));
    }
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

/// The text of an element whose content the rect touches — `/Alt`,
/// `/ActualText`, `/E`, `/T`, direct or indirect, on the element or an
/// ancestor — leaves nothing behind; a removed link's `/OBJR` neither.
#[test]
fn structure_tree_text_of_touched_elements() {
    let o = RedactOptions::new();
    let content = |v: u32| {
        format!(
            "/P <</MCID 0>> BDC BT /F1 10 Tf 80 100 Td ({}) Tj ET EMC \
             /P <</MCID 1>> BDC {KEPT} EMC",
            secret(v)
        )
    };
    for key in ["Alt", "ActualText", "E", "T"] {
        accepted(
            key,
            |v| {
                tagged_page(
                    &content(v),
                    &[
                        format!(
                            "<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /{key} ({}) >>",
                            secret(v)
                        ),
                        "<< /S /P /P 30 0 R /Pg 3 0 R /K 1 /Alt (KEPTALT) >>".into(),
                    ],
                    "<< /Nums [0 [31 0 R 32 0 R]] >>",
                    vec![],
                )
            },
            &o,
        );
    }
    accepted(
        "indirect text on an ancestor",
        |v| {
            tagged_page(
                &content(v),
                &[
                    "<< /S /Sect /P 30 0 R /K [33 0 R] /Alt 40 0 R >>".into(),
                    "<< /S /P /P 30 0 R /Pg 3 0 R /K 1 >>".into(),
                    "<< /S /Figure /P 31 0 R /Pg 3 0 R /K 0 >>".into(),
                ],
                "<< /Nums [0 [33 0 R 32 0 R]] >>",
                vec![(40, format!("({})", secret(v)).into_bytes())],
            )
        },
        &o,
    );
    accepted(
        "a tagged link",
        |v| {
            tagged_page(
                &content(v),
                &[
                    format!(
                        "<< /S /Link /P 30 0 R /Pg 3 0 R /Alt ({}) \
                         /K [0 << /Type /OBJR /Obj 41 0 R >>] >>",
                        secret(v)
                    ),
                    "<< /S /P /P 30 0 R /Pg 3 0 R /K 1 >>".into(),
                ],
                "<< /Nums [0 [31 0 R 32 0 R] 1 31 0 R] >>",
                vec![(
                    41,
                    format!(
                        "<< /Type /Annot /Subtype /Link /Rect [80 95 120 110] /StructParent 1 \
                         /A << /S /URI /URI (https://example.com/{}) >> >>",
                        secret(v)
                    )
                    .into_bytes(),
                )],
            )
        },
        &o,
    );
}
