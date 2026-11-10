//! Undefined codes in a Type 0 font's embedded CMap (§9.7.6.3): notdef
//! mappings are honoured by rendering and extraction alike; `redact`
//! refuses showing a code mapped by no `cidchar` / `cidrange`, since
//! Poppler, MuPDF and pdf.js each draw it differently (see
//! `docs/features/redaction.md`), and redacts fonts that merely declare
//! such mappings.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, render_page, Document, Error, RedactOptions, RenderOptions, TextOptions,
};

fn liberation() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap()
}

/// Codespace `<00> <7F>` and `<8140> <9FFC>`; `<41>` → CID 36 ('A'),
/// then `mappings`. CIDs 0, 36, 37, 38 advance 3000, 1000, 2000, 1500.
fn page(mappings: &str, show: &str) -> Vec<u8> {
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def \
         /CMapName /Test def /CMapType 1 def \
         2 begincodespacerange <00> <7F> <8140> <9FFC> endcodespacerange \
         1 begincidchar <41> 36 endcidchar {mappings} \
         endcmap CMapName currentdict /CMap defineresource pop end end"
    );
    let font = liberation();
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                format!("BT /F 10 Tf 10 100 Td <{show}> Tj ET").as_bytes(),
            ),
        ),
        (
            5,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding 9 0 R \
              /DescendantFonts [8 0 R] >>"
                .to_vec(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj(&format!("/Length1 {}", font.len()), &font)),
        (
            8,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 500 /W [0 [3000] 36 [1000 2000 1500]] /CIDToGIDMap /Identity \
              /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
        (
            9,
            stream_obj("/Type /CMap /CMapName /Test", cmap.as_bytes()),
        ),
    ])
}

const NOTDEF: &str = "1 beginnotdefchar <42> 37 endnotdefchar \
                      1 beginnotdefrange <50> <5F> 38 endnotdefrange";

/// `(code, origin x)` per shown code.
fn chars(pdf: &[u8]) -> Vec<(u32, f64)> {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .map(|c| (c.code & 0xFF_FFFF, (c.origin.0 * 1000.0).round() / 1000.0))
        .collect()
}

fn pixels(pdf: &[u8]) -> Vec<u8> {
    let doc = Document::from_bytes(pdf).unwrap();
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
        .unwrap()
        .pixels()
        .to_vec()
}

#[test]
fn notdef_mappings_give_their_cid_to_extraction_and_rendering() {
    let pdf = page(NOTDEF, "4142505F4341");
    // A 10 pt; <42> CID 37, 20 pt; <50> and <5F> both CID 38 (a notdef
    // range does not increment), 15 pt each; <43> unmapped, CID 0, 30 pt.
    assert_eq!(
        chars(&pdf),
        [
            (0x41, 10.0),
            (0x42, 20.0),
            (0x50, 40.0),
            (0x5F, 55.0),
            (0x43, 70.0),
            (0x41, 100.0)
        ]
    );
    // Drawn exactly as with the same CIDs mapped by `cidchar`.
    let defined = page(
        "4 begincidchar <42> 37 <50> 38 <5F> 38 <43> 0 endcidchar",
        "4142505F4341",
    );
    assert!(pixels(&pdf) == pixels(&defined));
    assert!(pixels(&pdf) != pixels(&page("", "4142505F4341")));
    // A defined mapping beats a notdef one; a notdef range applies to
    // codes of its own length only.
    let pdf = page(
        "1 beginnotdefrange <40> <4F> 38 endnotdefrange \
         1 beginnotdefrange <8144> <9FFC> 37 endnotdefrange",
        "4142815041",
    );
    assert_eq!(
        chars(&pdf),
        [(0x41, 10.0), (0x42, 20.0), (0x8150, 35.0), (0x41, 55.0)]
    );
}

#[test]
fn redaction_refuses_codes_readers_map_differently() {
    // Refused wherever the code is: a rect away from all text.
    let rect = (250.0, 10.0, 260.0, 20.0);
    for (show, why) in [
        // Notdef mapping: CID 37 here, CID 0 / nothing / CID 0x42
        // in Poppler / MuPDF / pdf.js.
        ("414241", "mapped only by a notdef mapping"),
        ("41415241", "mapped only by a notdef mapping"),
        // Unmapped: CID 0 here and in Poppler, nothing in MuPDF, CID
        // 0x43 in pdf.js.
        ("414341", "mapped to no CID"),
        // In no codespace range: one byte here and in pdf.js, two in
        // Poppler (`<8120>` prefix-matches `<8140> <9FFC>`).
        ("41812041", "in no codespace range"),
        ("41A041", "in no codespace range"),
    ] {
        let doc = Document::from_bytes(&page(NOTDEF, show)).unwrap();
        match redact(&doc, &[(0, vec![rect])], &RedactOptions::new()) {
            Err(Error::Unsupported { feature, detail }) => {
                assert!(feature.contains("CMap does not map"), "{feature}");
                assert!(detail.contains(why), "{show}: {detail}");
            }
            r => panic!("{show}: {:?}", r.map(|r| r.codes_removed)),
        }
    }
}

#[test]
fn redaction_keeps_fonts_declaring_notdef_that_show_mapped_codes() {
    let pdf = page(NOTDEF, "414141");
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![(18.0, 95.0, 25.0, 110.0)])],
        &RedactOptions::new(),
    )
    .unwrap();
    assert_eq!(r.codes_removed, 1);
    assert_eq!(chars(&r.output), [(0x41, 10.0), (0x41, 30.0)]);
}

#[test]
fn notdef_does_not_lift_the_literal_code_refusal() {
    let pdf = page("1 beginnotdefchar (B) 37 endnotdefchar", "414141");
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = redact(
        &doc,
        &[(0, vec![(18.0, 95.0, 25.0, 110.0)])],
        &RedactOptions::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("codes written as strings"), "{e}");
}

/// The later of a `cidchar` and a `cidrange` defining the same code
/// wins, as Poppler, MuPDF and pdf.js read it: glyphs are placed with
/// the later CID's width, and redaction follows. (Independent review,
/// P0: the `cidchar` used to win whatever the order.)
#[test]
fn the_latest_definition_wins_across_kinds() {
    let pdf = page("1 begincidrange <41> <41> 37 endcidrange", "4141");
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    // CID 37 advances 2000: the second glyph starts 20 pt after the first.
    let x: Vec<f64> = t.chars.iter().map(|c| c.origin.0).collect();
    assert!((x[1] - x[0] - 20.0).abs() < 1e-6, "{x:?}");
    // Readers draw the second glyph at x 30–50: that is what the rect
    // removes, and nothing of it is left there.
    let r = redact(
        &doc,
        &[(0, vec![(29.0, 95.0, 50.0, 115.0)])],
        &RedactOptions::new(),
    )
    .unwrap();
    assert_eq!(r.codes_removed, 1);
    let out = Document::from_bytes(&r.output).unwrap();
    let t = extract_text(&out, &out.pages()[0], &TextOptions::default()).unwrap();
    let x: Vec<f64> = t.chars.iter().map(|c| c.origin.0).collect();
    assert_eq!(x.len(), 1, "{x:?}");
    assert!((x[0] - 10.0).abs() < 1e-6, "{x:?}");
}

/// The same value defined for another code length: pdf.js and MuPDF
/// read mappings by value, Poppler by bytes — refused in redaction.
#[test]
fn a_value_defined_for_another_length_is_refused() {
    let pdf = page(
        "1 begincidchar <8142> 38 endcidchar 1 begincidchar <0041> 37 endcidchar",
        "41",
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = redact(
        &doc,
        &[(0, vec![(5.0, 95.0, 50.0, 115.0)])],
        &RedactOptions::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("another code length"), "{e}");
}

/// A code outside every codespace range is invalid whatever a mapping
/// says of it: readers split and skip it differently. (Confirmation
/// review, P0.)
#[test]
fn a_mapping_does_not_make_an_invalid_code_valid() {
    let pdf = page("1 begincidchar <A0> 37 endcidchar", "41A041");
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = redact(
        &doc,
        &[(0, vec![(5.0, 95.0, 50.0, 115.0)])],
        &RedactOptions::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("no codespace range"), "{e}");
}

/// CMap constructions readers read differently refuse the font in
/// redaction: bf mappings in an encoding CMap, definitions after
/// `endcmap`, codes an embedded CMap passes through an Identity base (`usecmap` Identity) rather than listing them, a `/WMode` other
/// than 0 or 1. (Release review.)
#[test]
fn reader_dependent_cmap_constructions_are_refused() {
    for m in [
        "1 beginbfchar <41> <0042> endbfchar",
        "endcmap 1 begincidchar <42> 37 endcidchar",
        "/WMode 2 def",
    ] {
        let pdf = page(m, "41");
        let doc = Document::from_bytes(&pdf).unwrap();
        let e = redact(
            &doc,
            &[(0, vec![(5.0, 95.0, 50.0, 115.0)])],
            &RedactOptions::new(),
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("readers read differently"),
            "{m}: {e}"
        );
    }
}

/// A CMap adding mappings to an Identity base: a code it does not list
/// is passed through (MuPDF, pdf.js) or given CID 0 (Poppler) —
/// refused; listed codes redact. (Release review.)
#[test]
fn an_identity_base_with_mappings_of_its_own() {
    use common::{build_pdf, stream_obj};
    let make = |show: &str| {
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
            /Identity-H usecmap 1 begincidchar <0041> 36 endcidchar \
            endcmap CMapName currentdict /CMap defineresource pop end end";
        let font = liberation();
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
                  /Resources << /Font << /F 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (
                4,
                stream_obj(
                    "",
                    format!("BT /F 10 Tf 10 100 Td <{show}> Tj ET").as_bytes(),
                ),
            ),
            (
                5,
                b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding 9 0 R \
                  /DescendantFonts [8 0 R] >>"
                    .to_vec(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
                  /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
                  /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj(&format!("/Length1 {}", font.len()), &font)),
            (
                8,
                b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
                  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                  /DW 500 /CIDToGIDMap /Identity /FontDescriptor 6 0 R >>"
                    .to_vec(),
            ),
            (9, stream_obj("/Type /CMap /CMapName /Test", cmap)),
        ])
    };
    let run = |show: &str| {
        let doc = Document::from_bytes(&make(show)).unwrap();
        redact(
            &doc,
            &[(0, vec![(5.0, 95.0, 50.0, 115.0)])],
            &RedactOptions::new(),
        )
    };
    run("00410041").unwrap();
    let e = run("00360041").unwrap_err();
    assert!(e.to_string().contains("Identity base"), "{e}");
}

/// An embedded CMap that is only an Identity base (`/Identity-H usecmap`
/// and nothing else): Poppler reads one-byte codes with CID 0, MuPDF
/// and pdf.js Identity — refused. (Release review.)
#[test]
fn an_embedded_cmap_that_is_only_an_identity_base() {
    use common::{build_pdf, stream_obj};
    for body in [&b""[..], b"0 begincidchar endcidchar"] {
        let mut cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
            /Identity-H usecmap "
            .to_vec();
        cmap.extend_from_slice(body);
        cmap.extend_from_slice(b" endcmap CMapName currentdict /CMap defineresource pop end end");
        let font = liberation();
        let pdf = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
                  /Resources << /Font << /F 5 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"BT /F 10 Tf 10 100 Td <00410042> Tj ET")),
            (
                5,
                b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding 9 0 R \
                  /DescendantFonts [8 0 R] >>"
                    .to_vec(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
                  /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
                  /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj(&format!("/Length1 {}", font.len()), &font)),
            (
                8,
                b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
                  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                  /DW 500 /CIDToGIDMap /Identity /FontDescriptor 6 0 R >>"
                    .to_vec(),
            ),
            (9, stream_obj("/Type /CMap /CMapName /Test", &cmap)),
        ]);
        let doc = Document::from_bytes(&pdf).unwrap();
        let e = redact(
            &doc,
            &[(0, vec![(5.0, 95.0, 50.0, 115.0)])],
            &RedactOptions::new(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("Identity base"), "{e}");
    }
}
