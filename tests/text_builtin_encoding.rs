//! Text extraction through the built-in encoding of a substituted
//! (non-embedded) simple font.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, Document, ResolveKind, TextOptions};

/// A non-embedded Standard 14 font without `/Encoding` has a built-in
/// encoding (Table 114): StandardEncoding for Helvetica, its own for
/// Symbol. Extraction resolves codes through it, as rendering draws
/// them — not as unresolved.
#[test]
fn builtin_encoding_of_a_substituted_standard_font_resolves_text() {
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /H 5 0 R /S 6 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                b"BT /H 12 Tf 10 40 Td (Hi\\047) Tj /S 12 Tf 40 0 Td (ab) Tj ET",
            ),
        ),
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
        (
            6,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>".to_vec(),
        ),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &TextOptions::default()).unwrap();
    let got: Vec<(Option<String>, ResolveKind)> = t
        .chars
        .iter()
        .map(|c| (c.unicode.clone(), c.resolved_via))
        .collect();
    let want = |s: &str| (Some(s.to_string()), ResolveKind::EncodingFallback);
    // StandardEncoding 0x27 is quoteright (U+2019), not the ASCII quote.
    assert_eq!(
        got,
        vec![
            want("H"),
            want("i"),
            want("\u{2019}"),
            want("\u{3b1}"),
            want("\u{3b2}")
        ]
    );
}
