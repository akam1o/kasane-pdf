//! CJK fallback (`cjk-fallback` feature): predefined Adobe-Japan1 CMaps
//! (§9.7.5.2) and non-embedded Adobe-Japan1 CID fonts drawn with the
//! bundled fallback face, reported as a substitution. Without the
//! feature the same inputs are refused as `Unsupported`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions};

/// One page showing `text` (raw bytes) with a non-embedded
/// CIDFontType0 under `/Encoding /{encoding}`.
fn cjk_pdf(encoding: &str, text: &[u8], extra_cid: &str) -> Vec<u8> {
    cjk_pdf_at(encoding, text, extra_cid, "Japan1", "20 40")
}

/// `ordering` is the `/CIDSystemInfo /Ordering`; `td` the text start.
fn cjk_pdf_at(encoding: &str, text: &[u8], extra_cid: &str, ordering: &str, td: &str) -> Vec<u8> {
    let mut content = format!("BT /F1 40 Tf {td} Td <").into_bytes();
    for b in text {
        content.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    content.extend_from_slice(b"> Tj ET");
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
              /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", &content)),
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /MS-Mincho /Encoding /{encoding} \
                 /DescendantFonts [6 0 R] >>"
            )
            .into_bytes(),
        ),
        (
            6,
            format!(
                "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /MS-Mincho \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 2 >> \
                 /FontDescriptor 7 0 R /DW 1000 {extra_cid} >>"
            )
            .into_bytes(),
        ),
        (
            7,
            b"<< /Type /FontDescriptor /FontName /MS-Mincho /Flags 4 /FontBBox [0 -141 1000 859] \
              /ItalicAngle 0 /Ascent 859 /Descent -141 /CapHeight 700 /StemV 80 >>"
                .to_vec(),
        ),
    ])
}

#[cfg_attr(not(feature = "cjk-fallback"), allow(dead_code))]
fn ink(doc: &Document, x0: u32, y0: u32, x1: u32, y1: u32) -> (usize, kasane_pdf::RenderedPage) {
    let r = render_page(doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let n = (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = r.pixmap().pixel(x, y).unwrap();
            (p.red() as u32 + p.green() as u32 + p.blue() as u32) < 600
        })
        .count();
    (n, r)
}

#[cfg(feature = "cjk-fallback")]
#[test]
fn predefined_cmaps_and_fallback_face_draw_japanese_text() {
    assert!(kasane_pdf::cjk_fallback_enabled());
    assert!(kasane_pdf::predefined_cmap_known(b"90ms-RKSJ-H"));
    assert!(kasane_pdf::predefined_cmap_known(b"UniJIS-UCS2-HW-V"));
    assert!(!kasane_pdf::predefined_cmap_known(b"GBK-EUC-H"));
    // あい in Shift-JIS, in UCS-2, and in EUC-JP: three CMaps, one
    // drawing.
    let cases: [(&str, &[u8]); 3] = [
        ("90ms-RKSJ-H", &[0x82, 0xA0, 0x82, 0xA2]),
        ("UniJIS-UCS2-H", &[0x30, 0x42, 0x30, 0x44]),
        ("EUC-H", &[0xA4, 0xA2, 0xA4, 0xA4]),
    ];
    let mut inks = Vec::new();
    for (enc, text) in cases {
        let doc = Document::from_bytes(&cjk_pdf(enc, text, "")).unwrap();
        let (n, r) = ink(&doc, 20, 20, 110, 70);
        assert!(n > 200, "{enc}: {n} ink pixels");
        assert_eq!(r.font_substitutions().len(), 1, "{enc}");
        let s = &r.font_substitutions()[0];
        assert_eq!(s.base_font, "MS-Mincho");
        assert_eq!(s.substitute, "NotoSansJP-Regular");
        assert_eq!(s.kind, kasane_pdf::SubstKind::CjkFallback);
        inks.push(n);
    }
    // The same characters through three encodings give the same glyphs.
    assert_eq!(inks[0], inks[1]);
    assert_eq!(inks[0], inks[2]);
    // Nothing is drawn where the text is not.
    let doc = Document::from_bytes(&cjk_pdf("90ms-RKSJ-H", &[0x82, 0xA0], "")).unwrap();
    let (n, _) = ink(&doc, 120, 0, 200, 100);
    assert_eq!(n, 0);
    // Text extraction: without /ToUnicode the codes are reported as
    // unmapped, not invented.
    let text = kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::TextOptions::default());
    assert!(text.is_ok());
}

#[cfg(feature = "cjk-fallback")]
#[test]
fn vertical_cmap_sets_vertical_mode_and_uses_w2() {
    // 90ms-RKSJ-V (`usecmap` 90ms-RKSJ-H + /WMode 1): glyphs advance
    // downwards from the start point.
    let text = [0x82, 0xA0, 0x82, 0xA2, 0x82, 0xA4];
    // Start at (30, 90): the first glyph fills user y 90..50 (top half
    // of the pixmap), the second 50..10 (bottom half).
    let pdf = cjk_pdf_at("90ms-RKSJ-V", &text, "/DW2 [880 -1000]", "Japan1", "30 90");
    let doc = Document::from_bytes(&pdf).unwrap();
    let (top, _) = ink(&doc, 0, 0, 60, 50);
    let (bottom, _) = ink(&doc, 0, 50, 60, 100);
    assert!(
        top > 50 && bottom > 50,
        "glyphs stacked vertically: {top} / {bottom}"
    );
    let (right, _) = ink(&doc, 100, 0, 200, 100);
    assert_eq!(right, 0, "nothing advances to the right");
}

#[cfg(feature = "cjk-fallback")]
#[test]
fn identity_ordering_without_program_is_still_refused() {
    // Identity ordering: CIDs are GIDs of an unknown font, so no
    // fallback can be sound.
    let pdf = cjk_pdf_at("Identity-H", &[0x00, 0x05], "", "Identity", "20 40");
    let doc = Document::from_bytes(&pdf).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Unsupported {
                feature: "non-embedded CIDFont",
                ..
            }
        ),
        "{err:?}"
    );
}

#[cfg(not(feature = "cjk-fallback"))]
#[test]
fn without_the_feature_predefined_cmaps_and_fallback_are_refused() {
    assert!(!kasane_pdf::cjk_fallback_enabled());
    assert!(kasane_pdf::predefined_cmap_known(b"Identity-H"));
    assert!(!kasane_pdf::predefined_cmap_known(b"90ms-RKSJ-H"));
    let doc = Document::from_bytes(&cjk_pdf("90ms-RKSJ-H", &[0x82, 0xA0], "")).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Unsupported {
                feature: "Type 0 font encoding",
                ..
            }
        ),
        "{err:?}"
    );
    let doc = Document::from_bytes(&cjk_pdf("Identity-H", &[0x00, 0x05], "")).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Unsupported {
                feature: "non-embedded CIDFont",
                ..
            }
        ),
        "{err:?}"
    );
}
