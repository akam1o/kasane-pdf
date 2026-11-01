//! Text that does not come from `/ToUnicode`: an arbitrary font program
//! (byte 0 picks how the PDF uses it — simple TrueType with glyph-index
//! names or with none, a CFF through its built-in encoding, a Type 0
//! TrueType, CFF or OpenType font under Identity-H / Japan1), or, with
//! byte 0 ≥ 128, an arbitrary annotation dictionary of a page with a
//! form. Every code 0x00–0xFF (or 0x0000–0x00FF) is shown.
//! Oracle: no panic, hang or runaway allocation in text extraction
//! (with annotations) and rendering.
#![no_main]

use kasane_pdf::{extract_text, render_page, Document, RenderOptions, TextOptions};
use kasane_pdf_fuzz::{build, limits};
use libfuzzer_sys::fuzz_target;

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    let simple_codes: String = (0..=255u8).map(|b| format!("{b:02X}")).collect();
    let cid_codes: String = (0..=255u16).map(|b| format!("{b:04X}")).collect();
    let widths = vec!["500"; 256].join(" ");
    let descriptor = |file: &str| {
        format!(
            "<< /Type /FontDescriptor /FontName /F /Flags 32 /FontBBox [0 0 1000 1000] \
             /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 {file} 8 0 R >>"
        )
        .into_bytes()
    };
    let names: String = (1..=255).map(|g| format!("/g{g} ")).collect();
    let (font, show, extra): (Vec<u8>, &str, Vec<(u32, Vec<u8>)>) = match mode % 8 {
        0 | 1 => {
            let enc = if mode % 8 == 0 { format!("/Encoding << /Differences [1 {names}] >>") } else { String::new() };
            (
                format!("<< /Type /Font /Subtype /TrueType /BaseFont /F {enc} /FirstChar 0 /LastChar 255 /Widths [{widths}] /FontDescriptor 7 0 R >>").into_bytes(),
                &simple_codes,
                vec![(7, descriptor("/FontFile2")), (8, stream("", rest))],
            )
        }
        2 => (
            format!("<< /Type /Font /Subtype /Type1 /BaseFont /F /FirstChar 0 /LastChar 255 /Widths [{widths}] /FontDescriptor 7 0 R >>").into_bytes(),
            &simple_codes,
            vec![(7, descriptor("/FontFile3")), (8, stream("/Subtype /Type1C", rest))],
        ),
        m => {
            let (sub, file, ff3, ordering) = match m {
                3 => ("CIDFontType2", "/FontFile2", "", "Identity"),
                4 => ("CIDFontType2", "/FontFile2", "", "Japan1"),
                5 => ("CIDFontType0", "/FontFile3", "/Subtype /CIDFontType0C", "Identity"),
                6 => ("CIDFontType0", "/FontFile3", "/Subtype /CIDFontType0C", "Japan1"),
                _ => ("CIDFontType0", "/FontFile3", "/Subtype /OpenType", "Japan1"),
            };
            (
                b"<< /Type /Font /Subtype /Type0 /BaseFont /F /Encoding /Identity-H /DescendantFonts [9 0 R] >>".to_vec(),
                &cid_codes,
                vec![
                    (9, format!("<< /Type /Font /Subtype /{sub} /BaseFont /F /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 0 >> /FontDescriptor 7 0 R >>").into_bytes()),
                    (7, descriptor(file)),
                    (8, stream(ff3, rest)),
                ],
            )
        }
    };
    let annots = mode >= 128;
    let content = if annots {
        String::from("BT /F 10 Tf 10 10 Td (x) Tj ET")
    } else {
        format!("BT /F 10 Tf 10 10 Td <{show}> Tj ET")
    };
    let mut objs = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R] /DR << /Font << /Helv 11 0 R >> >> /DA (/Helv 0 Tf 0 g) >> >>".to_vec(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F 5 0 R >> >> /Annots [10 0 R 10 0 R 12 0 R] >>".to_vec(),
        ),
        (4, stream("", content.as_bytes())),
        (11, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (12, b"<< /Type /Annot /Subtype /Widget /Parent 13 0 R /Rect [0 0 50 20] >>".to_vec()),
        (13, b"<< /FT /Tx /T (p) /V (v) /Kids [12 0 R] >>".to_vec()),
    ];
    if annots {
        objs.push((5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()));
        objs.push((10, rest.to_vec()));
    } else {
        objs.push((5, font));
        objs.extend(extra);
        objs.push((10, b"<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /Contents (c) >>".to_vec()));
    }
    objs.sort_by_key(|o| o.0);
    let pdf = build(&objs, b"<< /Size 14 /Root 1 0 R >>");
    for recovery in [false, true] {
        let o = kasane_pdf::OpenOptions::new().with_limits(limits()).with_recovery(recovery);
        let Ok(doc) = Document::open(&pdf, &o) else { continue };
        let Some(page) = doc.pages().first().cloned() else { continue };
        let opts = TextOptions::new().with_limits(limits()).with_annotations(true);
        if let Ok(t) = extract_text(&doc, &page, &opts) {
            for a in &t.annotations {
                let _ = a.text();
            }
        }
        let _ = render_page(&doc, &page, &RenderOptions::new().with_dpi(9.0).with_limits(limits()));
        let _ = doc.take_warnings();
    }
});
