//! `/FirstChar` / `/LastChar` outside the single-byte range never index
//! out of bounds (a panic found by fuzzing `open_any`; present since
//! 0.1): widths for codes outside 0–255 are never used and are skipped;
//! the counts saturate on extreme values.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, render_page, Document, RenderOptions, TextOptions};

const TTF: &[u8] = include_bytes!("fixtures/KasaneTest.ttf");

fn pdf(font: String, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 50] /Contents 4 0 R \
              /Resources << /Font << /F 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /F 12 Tf 10 10 Td (AB) Tj ET")),
        (5, font.into_bytes()),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

#[test]
fn out_of_range_first_and_last_char_never_panic() {
    let widths = |n: usize| vec!["500"; n].join(" ");
    let ranges: [(i64, i64, usize); 6] = [
        (32, 256, 225), // one past the last code (the fuzz case)
        (0, 300, 301),
        (-5, 70, 76), // negative first code
        (250, 1000, 751),
        (i64::MIN, i64::MAX, 3), // saturating span
        (i64::MAX - 1, i64::MAX, 2),
    ];
    for (first, last, n) in ranges {
        let tt = format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest /Encoding /WinAnsiEncoding \
             /FirstChar {first} /LastChar {last} /Widths [{}] /FontDescriptor 6 0 R >>",
            widths(n)
        );
        let fd = (
            6,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 /FontBBox [0 0 1000 1000] \
              /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                .to_vec(),
        );
        let type1 = format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar {first} \
             /LastChar {last} /Widths [{}] >>",
            widths(n)
        );
        let type3 = format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /A 8 0 R >> /Encoding << /Differences [65 /A] >> \
             /FirstChar {first} /LastChar {last} /Widths [{}] >>",
            widths(n)
        );
        for (kind, doc) in [
            ("TrueType", pdf(tt, vec![fd, (7, stream_obj("", TTF))])),
            ("Type1", pdf(type1, vec![])),
            ("Type3", pdf(type3, vec![(8, stream_obj("", b"500 0 d0"))])),
        ] {
            let doc = Document::from_bytes(&doc).unwrap();
            let page = &doc.pages()[0];
            // Whatever the outcome, no panic.
            let t = extract_text(&doc, page, &TextOptions::default());
            let _ = render_page(&doc, page, &RenderOptions::new().with_dpi(9.0));
            // In-range codes keep their widths when the font loads.
            if let (Ok(t), true) = (t, (first..=last).contains(&65) && first >= 0) {
                assert!(
                    t.chars.iter().all(|c| (c.advance.0 - 6.0).abs() < 1e-6),
                    "{kind} {first}..{last}: {:?}",
                    t.chars.iter().map(|c| c.advance).collect::<Vec<_>>()
                );
            }
        }
    }
}
