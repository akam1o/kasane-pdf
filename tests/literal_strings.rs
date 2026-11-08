//! Line ends in literal strings (§7.3.4.2): an unescaped CR, LF or CRLF
//! is one line feed. A page written with raw end-of-lines must extract
//! and render exactly as the same page written with `\n` escapes, in
//! every font type; `redact` refuses the raw form (readers differ on it)
//! and redacts the escaped one, writing what it keeps so that it reads
//! back byte for byte.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, render_page, Document, Operation, RedactOptions, RenderOptions,
    TextOptions,
};

const CFF: &[u8] = include_bytes!("fixtures/type1_cff.bin");

fn liberation() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap()
}

/// Codes 10–102 at width 500, but CR (13) at 900 and LF (10) at 300 —
/// reading one for the other moves what follows.
fn widths() -> String {
    (10..=102)
        .map(|c| match c {
            10 => "300",
            13 => "900",
            _ => "500",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The font as object 5 (and what it needs), for each font type.
fn font(kind: &str) -> Vec<(u32, Vec<u8>)> {
    let w = widths();
    match kind {
        "TrueType" => vec![
            (
                5,
                format!(
                    "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 10 \
                     /LastChar 102 /Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>"
                )
                .into_bytes(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
                  /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
                  /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj(&format!("/Length1 {}", liberation().len()), &liberation())),
        ],
        "Type1" => vec![
            (
                5,
                format!(
                    "<< /Type /Font /Subtype /Type1 /BaseFont /Test /FirstChar 10 /LastChar 102 \
                     /Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor 6 0 R >>"
                )
                .into_bytes(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /Test /Flags 32 /FontBBox [-6 -12 579 713] \
                  /Ascent 751 /Descent -173 /CapHeight 660 /StemV 80 /FontFile3 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj("/Subtype /Type1C", CFF)),
        ],
        "Type0" => vec![
            (
                5,
                b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding /Identity-H \
                  /DescendantFonts [8 0 R] >>"
                    .to_vec(),
            ),
            (
                8,
                b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
                  /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                  /DW 600 /W [2570 [400] 3341 [900]] /CIDToGIDMap /Identity \
                  /FontDescriptor 6 0 R >>"
                    .to_vec(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
                  /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
                  /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj(&format!("/Length1 {}", liberation().len()), &liberation())),
        ],
        "Type3" => {
            let names: String = (10..=102).map(|c| format!("/g{c} ")).collect();
            let procs: String = [10, 13, 65, 66, 67, 68]
                .iter()
                .map(|c| format!("/g{c} {} 0 R ", 100 + c))
                .collect();
            let mut v = vec![(
                5,
                format!(
                    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                     /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs}>> \
                     /Encoding << /Type /Encoding /Differences [10 {names}] >> /FirstChar 10 \
                     /LastChar 102 /Widths [{w}] /Resources << >> >>"
                )
                .into_bytes(),
            )];
            for c in [10u32, 13, 65, 66, 67, 68] {
                let wd = match c {
                    10 => 300,
                    13 => 900,
                    _ => 500,
                };
                v.push((
                    100 + c,
                    stream_obj("", format!("{wd} 0 d0 0 0 {} 700 re f", wd - 50).as_bytes()),
                ));
            }
            v
        }
        _ => unreachable!(),
    }
}

fn page(kind: &str, content: &[u8]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(font(kind));
    build_pdf(&objs)
}

/// The string shown on each line, raw and as the spec reads it; the
/// second line goes inside the rect.
fn strings(kind: &str) -> [(Vec<u8>, Vec<u8>); 2] {
    if kind == "Type0" {
        // Read as the spec says these are whole codes (0x0A0A between
        // two letters); kept as written, the code boundaries shift.
        [
            (
                b"\x00\x24\r\n\n\x00\x25".to_vec(),
                b"\x00\x24\\n\\n\x00\x25".to_vec(),
            ),
            (
                b"\x00\x28\r\r\x00\x29".to_vec(),
                b"\x00\x28\\n\\n\x00\x29".to_vec(),
            ),
        ]
    } else {
        [
            (b"A\rB\r\nC\nD".to_vec(), b"A\\nB\\nC\\nD".to_vec()),
            (b"AB\rCD".to_vec(), b"AB\\nCD".to_vec()),
        ]
    }
}

fn content(kind: &str, raw: bool) -> Vec<u8> {
    let s = strings(kind);
    let pick = |p: &(Vec<u8>, Vec<u8>)| if raw { p.0.clone() } else { p.1.clone() };
    let mut c = b"BT /F 12 Tf 10 20 Td (".to_vec();
    c.extend_from_slice(&pick(&s[0]));
    c.extend_from_slice(b") Tj 1 0 0 1 120 100 Tm (");
    c.extend_from_slice(&pick(&s[1]));
    c.extend_from_slice(b") Tj ET");
    c
}

const RECT: (f64, f64, f64, f64) = (110.0, 90.0, 200.0, 120.0);
const KINDS: [&str; 4] = ["TrueType", "Type1", "Type0", "Type3"];

fn chars(pdf: &[u8]) -> Vec<(u32, (i64, i64))> {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .map(|c| {
            (
                c.code,
                (
                    (c.origin.0 * 1000.0).round() as i64,
                    (c.origin.1 * 1000.0).round() as i64,
                ),
            )
        })
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
fn raw_line_ends_read_as_line_feeds_in_every_font_type() {
    for kind in KINDS {
        let raw = page(kind, &content(kind, true));
        let esc = page(kind, &content(kind, false));
        let c = chars(&raw);
        assert!(c.len() >= 6, "{kind}: {c:?}");
        assert!(
            c.iter().all(|(code, _)| *code != 13 && *code != 0x0d0a),
            "{kind}: {c:?}"
        );
        assert_eq!(c, chars(&esc), "{kind}: extraction");
        assert!(pixels(&raw) == pixels(&esc), "{kind}: rendering");
    }
}

#[test]
fn redaction_refuses_raw_line_ends_and_keeps_escaped_ones_exact() {
    for kind in KINDS {
        let doc = Document::from_bytes(&page(kind, &content(kind, true))).unwrap();
        let e = redact(&doc, &[(0, vec![RECT])], &RedactOptions::new()).unwrap_err();
        assert!(e.to_string().contains("carriage return"), "{kind}: {e}");

        let esc = page(kind, &content(kind, false));
        let doc = Document::from_bytes(&esc).unwrap();
        let r = redact(&doc, &[(0, vec![RECT])], &RedactOptions::new())
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert!(r.codes_removed > 0, "{kind}");
        let out = Document::from_bytes(&r.output).unwrap();
        let raw: Vec<u8> = out.page_contents(&out.pages()[0]).unwrap().concat();
        assert!(!raw.contains(&b'\r'), "{kind}: a raw CR written");
        // The kept line reads back as it was, code for code and place
        // for place.
        let kept: Vec<_> = chars(&esc)
            .into_iter()
            .filter(|(_, o)| o.1 < 50_000)
            .collect();
        assert_eq!(chars(&r.output), kept, "{kind}");
        let shown: Vec<Vec<u8>> = out
            .page_operations(&out.pages()[0])
            .unwrap()
            .into_iter()
            .filter_map(|o| match o {
                Operation::ShowText(s) => Some(s),
                _ => None,
            })
            .collect();
        assert!(
            shown.iter().all(|s| !s.contains(&b'\r')),
            "{kind}: {shown:?}"
        );
    }
}

/// A Type 0 font with an embedded encoding CMap whose single mapping
/// for byte `code` is written as `code_src` (a hex or literal string).
fn cmap_page(code_src: &[u8]) -> Vec<u8> {
    let mut cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def \
        /CMapName /Test def /CMapType 1 def 1 begincodespacerange <00> <FF> endcodespacerange \
        2 begincidchar "
        .to_vec();
    cmap.extend_from_slice(code_src);
    cmap.extend_from_slice(
        b" 36 <53> 37 endcidchar endcmap CMapName currentdict /CMap defineresource pop end end",
    );
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /F 10 Tf 20 100 Td <4153> Tj ET")),
        (
            5,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding 9 0 R \
              /DescendantFonts [8 0 R] >>"
                .to_vec(),
        ),
        (
            8,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 600 /W [36 [4000]] /CIDToGIDMap /Identity /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
        (9, stream_obj("/Type /CMap /CMapName /Test", &cmap)),
    ];
    objs.extend(
        font("Type0")
            .into_iter()
            .filter(|(n, _)| *n == 6 || *n == 7),
    );
    build_pdf(&objs)
}

/// A CMap code written as a literal string maps like the same code in
/// hex (it decides the CID, so the width and where the next glyph
/// falls); one holding an unescaped CR is refused.
#[test]
fn cmap_codes_in_literal_strings() {
    let hex = chars(&cmap_page(b"<41>"));
    assert_eq!(hex.len(), 2);
    assert_eq!(hex[1].1 .0, 60_000, "CID 36 advances 40 pt");
    assert_eq!(chars(&cmap_page(b"(A)")), hex);
    let doc = Document::from_bytes(&cmap_page(b"(\r)")).unwrap();
    let e = redact(
        &doc,
        &[(0, vec![(55.0, 95.0, 70.0, 115.0)])],
        &RedactOptions::new(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("carriage return"), "{e}");
}

/// A literal string with an unescaped CRLF elsewhere in the document —
/// here an Indexed palette — is refused: rewritten, it would change for
/// some readers.
#[test]
fn object_strings_with_raw_line_ends_are_refused() {
    let raw = b"(\xff\r\n\x00\xff\x00)".to_vec();
    for (pal, ok) in [(raw, false), (b"<ff0d0a00ff00>".to_vec(), true)] {
        let mut img = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /BitsPerComponent 8 \
             /ColorSpace [/Indexed /DeviceRGB 1 "
            .to_vec();
        img.extend_from_slice(&pal);
        img.extend_from_slice(b"] /Length 2 >>\nstream\n\x00\x01\nendstream");
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
                  /Resources << /Font << /F 5 0 R >> /XObject << /Im 10 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (
                4,
                stream_obj("", b"q 20 0 0 10 10 10 cm /Im Do Q BT /F 12 Tf 120 100 Td (AB) Tj ET"),
            ),
            (10, img),
        ];
        objs.extend(font("TrueType"));
        let doc = Document::from_bytes(&build_pdf(&objs)).unwrap();
        let r = redact(&doc, &[(0, vec![RECT])], &RedactOptions::new());
        assert_eq!(r.is_ok(), ok, "{:?}", r.map(|r| r.codes_removed));
    }
}
