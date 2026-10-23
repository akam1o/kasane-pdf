//! Type 1 (and MMType1) font support.
//!
//! Covers the implementable subset:
//!   * `/FontFile3` (raw CFF) — outlines via `ttf_parser::cff`, glyph
//!     names resolved through the CFF charset + `/Encoding`.
//!   * non-embedded Type1 — no program, so rendering refuses but text
//!     extraction still gets `/Widths` + encoding → Unicode.
//!
//! And the refused subset:
//!   * `/FontFile` (Adobe Type 1 / eexec) — a Type 1 charstring VM we
//!     don't implement; must be `Unsupported`, never silently wrong.
//!
//! The CFF program is a real `/FontFile3` extracted from a corpus PDF
//! (a SourceSansPro-Bold subset, charset `A`–`f`). Stored as a binary
//! fixture and embedded with `include_bytes!`.

use kasane_pdf::{extract_text, render_page};
use kasane_pdf::{Document, Error, Limits, RenderOptions};

/// The raw CFF program (a `/FontFile3` `/Type1C` subset).
const CFF: &[u8] = include_bytes!("fixtures/type1_cff.bin");

/// Assemble a 1-page PDF around a font dict body + optional extra
/// objects (descriptor, font-file stream, ToUnicode). `font_extra`
/// is appended to the `/Font` dict; `objs` is `(num, body)` pairs
/// beyond the fixed catalog/pages/page/contents (1–4).
fn pdf_with_font(stream: &[u8], font_extra: &str, objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    pdf_with_font_typed(stream, "/Type1", font_extra, objs)
}

/// As `pdf_with_font` but with a caller-chosen font `/Subtype`.
fn pdf_with_font_typed(
    stream: &[u8],
    subtype: &str,
    font_extra: &str,
    objs: &[(u32, Vec<u8>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
        /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>";
    // WinAnsi by default; a `font_extra` that names its own /Encoding
    // (or `/Encoding null` for "none") replaces it.
    let enc = if font_extra.contains("/Encoding") {
        ""
    } else {
        "/Encoding /WinAnsiEncoding"
    };
    let font = format!(
        "<< /Type /Font /Subtype {subtype} /BaseFont /Test \
           /FirstChar 65 /LastChar 102 \
           /Widths [573 0 582 0 548 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 573 0 573 0 341] \
           {enc} {font_extra} >>"
    );
    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, page.bytes().collect()),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (5, font.into_bytes()),
    ];
    bodies.extend(objs.iter().map(|(n, b)| (*n, b.clone())));
    let mut offsets = Vec::new();
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = bodies.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    let mut lines = vec![b"0000000000 65535 f \n".to_vec()];
    let mut by_n: std::collections::BTreeMap<u32, usize> = offsets.iter().cloned().collect();
    for n in 1..size {
        let off = by_n.remove(&n).unwrap_or(0);
        lines.push(format!("{off:010} 00000 n \n").into_bytes());
    }
    for l in lines {
        out.extend_from_slice(&l);
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

/// Standard objects for an embedded-CFF Type1 font: descriptor (6) +
/// `/FontFile3` stream (7).
fn cff_font_objs() -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            6,
            b"<< /Type /FontDescriptor /FontName /Test /Flags 32 \
              /FontBBox [-6 -12 579 713] /Ascent 751 /Descent -173 \
              /CapHeight 660 /StemV 80 /FontFile3 7 0 R >>"
                .to_vec(),
        ),
        (
            7,
            [
                format!("<< /Length {} /Subtype /Type1C >>\nstream\n", CFF.len()).into_bytes(),
                CFF.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ]
}

fn render(pdf: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn text(pdf: &[u8]) -> Result<String, Error> {
    let doc = Document::from_bytes(pdf)?;
    let mut s = String::new();
    for c in extract_text(&doc, &doc.pages()[0], &Limits::default())?.chars {
        if let Some(u) = &c.unicode {
            s.push_str(u);
        }
    }
    Ok(s)
}

fn inked(page: &kasane_pdf::RenderedPage) -> usize {
    let mut n = 0;
    for y in 0..page.pixmap.height() {
        for x in 0..page.pixmap.width() {
            let p = page.pixmap.pixel(x, y).unwrap();
            if (p.red(), p.green(), p.blue()) != (255, 255, 255) {
                n += 1;
            }
        }
    }
    n
}

// --------------------------------------------------------------------
// Embedded CFF (/FontFile3) — the implementable render path.
// --------------------------------------------------------------------

#[test]
fn type1_cff_renders_ink() {
    // Draw "ABC" — WinAnsi codes 65,66,67 → the CFF subset's A,B,C.
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (ABC) Tj ET",
        "/FontDescriptor 6 0 R",
        &cff_font_objs(),
    );
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 50, "CFF Type1 drew no ink");
}

#[test]
fn type1_cff_extracts_text() {
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (AB) Tj ET",
        "/FontDescriptor 6 0 R",
        &cff_font_objs(),
    );
    let s = text(&pdf).unwrap();
    assert_eq!(s.trim_end_matches('\n'), "AB", "got {s:?}");
}

#[test]
fn type1_cff_corrupt_program_is_structure_error() {
    // Truncate the CFF → parse must fail as Structure, not panic.
    let mut objs = cff_font_objs();
    objs[1].1 = [
        b"<< /Length 12 /Subtype /Type1C >>\nstream\n".to_vec(),
        b"\x01\x00\x04\x04garbage!!".to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat();
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/FontDescriptor 6 0 R",
        &objs,
    );
    match render(&pdf) {
        Err(Error::Structure(_)) | Err(Error::Unsupported { .. }) => {}
        other => panic!("corrupt CFF should error, got {other:?}"),
    }
}

// --------------------------------------------------------------------
// /FontFile (Adobe Type 1 / eexec) — parsed by the Type-1 charstring
// interpreter (src/type1.rs).
// --------------------------------------------------------------------

/// The real CMBSY10 eexec program from the corpus (subset: `.notdef`
/// + `periodcentered`) — see tests/fixtures/README.md.
const EEXEC: &[u8] = include_bytes!("fixtures/eexec_cmsy10.bin");

fn eexec_font_objs(program: &[u8]) -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            6,
            b"<< /Type /FontDescriptor /FontName /Test /Flags 32 \
              /FontBBox [0 0 500 700] /Ascent 700 /Descent -200 \
              /CapHeight 660 /StemV 80 /FontFile 7 0 R >>"
                .to_vec(),
        ),
        (
            7,
            [
                format!("<< /Length {} >>\nstream\n", program.len()).into_bytes(),
                program.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ]
}

#[test]
fn type1_eexec_fontfile_renders_charstring() {
    // WinAnsi 0xB7 = /periodcentered — the one real glyph in the
    // subset. It must paint ink; a glyph the subset lacks (`A`)
    // resolves to .notdef and paints nothing but still renders.
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\267) Tj ET",
        "/FontDescriptor 6 0 R",
        &eexec_font_objs(EEXEC),
    );
    let r = render(&pdf).expect("eexec /FontFile renders");
    assert!(inked(&r) > 0, "periodcentered painted no ink");

    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/FontDescriptor 6 0 R",
        &eexec_font_objs(EEXEC),
    );
    let r = render(&pdf).expect("missing glyph → .notdef, not an error");
    assert_eq!(inked(&r), 0, ".notdef must not paint");
}

#[test]
fn type1_eexec_garbage_fontfile_is_structure_error() {
    // A /FontFile that is not a Type 1 program (no CharStrings at all)
    // is a broken font: refuse with a Structure error rather than
    // silently rendering every glyph blank.
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/FontDescriptor 6 0 R",
        &eexec_font_objs(b"\x80\x01\x00\x00eexec!"),
    );
    match render(&pdf) {
        Err(Error::Structure(msg)) => {
            assert!(msg.contains("Type 1") || msg.contains("eexec"), "got {msg}");
        }
        other => panic!("garbage /FontFile must be a Structure error, got {other:?}"),
    }
}

// --------------------------------------------------------------------
// Non-embedded Type1: text extraction works, render refuses.
// --------------------------------------------------------------------

#[test]
fn type1_nonembedded_unknown_name_uses_generic_substitute() {
    // No /FontDescriptor, /BaseFont /Test (unknown): extraction uses
    // the PDF widths; render draws with a *generic* bundled face and
    // reports exactly that.
    let pdf = pdf_with_font(b"BT /F0 40 Tf 20 100 Td (AB) Tj ET", "", &[]);
    let s = text(&pdf).unwrap();
    assert_eq!(s.trim_end_matches('\n'), "AB", "got {s:?}");
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 20, "substitute drew no ink");
    assert_eq!(r.font_substitutions.len(), 1);
    let sub = &r.font_substitutions[0];
    assert_eq!(sub.base_font, "Test");
    assert_eq!(sub.kind, kasane_pdf::SubstKind::Generic);
    assert!(
        sub.substitute.starts_with("LiberationSans"),
        "{}",
        sub.substitute
    );
}

/// Standard 14 without `/Widths` or `/FontDescriptor` — the common
/// `<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>` shape.
fn pdf_std14(stream: &[u8], base: &str) -> Vec<u8> {
    let font =
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding /WinAnsiEncoding >>");
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
        /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>";
    let bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, page.bytes().collect()),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (5, font.into_bytes()),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (n, b) in &bodies {
        offsets.push(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

#[test]
fn type1_standard14_substitute_metrics_and_report() {
    for (base, expect_face, expect_adv_a) in [
        ("Helvetica", "LiberationSans-Regular", 667.0),
        ("Helvetica-BoldOblique", "LiberationSans-BoldItalic", 722.0),
        ("Times-Roman", "LiberationSerif-Regular", 722.0),
        ("Courier-Bold", "LiberationMono-Bold", 600.0),
    ] {
        let pdf = pdf_std14(b"BT /F0 20 Tf 10 100 Td (A) Tj ET", base);
        let r = render(&pdf).unwrap_or_else(|e| panic!("{base}: {e:?}"));
        assert!(inked(&r) > 10, "{base}: no ink");
        assert_eq!(r.font_substitutions.len(), 1, "{base}");
        assert_eq!(r.font_substitutions[0].substitute, expect_face, "{base}");
        assert_eq!(
            r.font_substitutions[0].kind,
            kasane_pdf::SubstKind::Standard14
        );
        // No /Widths → advance comes from the metric-compatible face.
        let doc = Document::from_bytes(&pdf).unwrap();
        let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
        let adv = t.chars[0].advance.0;
        assert!(
            (adv - expect_adv_a * 20.0 / 1000.0).abs() < 0.05,
            "{base}: 'A' advance {adv}"
        );
    }
}

#[test]
fn type1_symbol_fonts_are_refused_not_approximated() {
    for base in ["Symbol", "ZapfDingbats"] {
        let pdf = pdf_std14(b"BT /F0 20 Tf 10 100 Td (a) Tj ET", base);
        match render(&pdf) {
            Err(Error::Unsupported { feature, .. }) => {
                assert!(feature.contains("non-embedded"), "{base}: {feature}")
            }
            other => panic!("{base}: expected Unsupported, got {other:?}"),
        }
    }
}

#[test]
fn type1_substitute_honours_pdf_widths_and_tounicode() {
    // /Widths present → PDF advances win over the substitute's hmtx;
    // /ToUnicode still drives extraction.
    let objs = vec![(
        9,
        b"<< /Length 118 >>\nstream\n/CIDInit /ProcSet findresource begin begincmap \
          1 begincodespacerange <00> <FF> endcodespacerange \
          1 beginbfchar <41> <0042> endbfchar endcmap\nendstream"
            .to_vec(),
    )];
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/ToUnicode 9 0 R",
        &objs,
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("B"));
    // pdf_with_font declares /Widths[0] = 573 for 'A' → 573/1000 × 40.
    assert!(
        (t.chars[0].advance.0 - 22.92).abs() < 0.01,
        "{}",
        t.chars[0].advance.0
    );
    assert!(render(&pdf).is_ok());
}

#[test]
fn type1_mmtype1_subtype_accepted() {
    // /MMType1 routes through the same loader as /Type1.
    let pdf = pdf_with_font_typed(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/MMType1",
        "/FontDescriptor 6 0 R",
        &cff_font_objs(),
    );
    let r = render(&pdf).unwrap();
    assert!(inked(&r) > 20, "MMType1 CFF drew no ink");
}

// --------------------------------------------------------------------
// Encoding: the program's built-in encoding, MacRoman, Differences
// --------------------------------------------------------------------

#[test]
fn type1_without_encoding_uses_the_programs_builtin_table() {
    // The cmsy10 subset's cleartext /Encoding maps code 1 →
    // /periodcentered (Table 114: no /Encoding → built-in). Code 0xB7
    // is unassigned in that table → .notdef, no ink.
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\001) Tj ET",
        "/FontDescriptor 6 0 R /Encoding null",
        &eexec_font_objs(EEXEC),
    );
    let r = render(&pdf).expect("built-in encoding renders");
    assert!(inked(&r) > 0, "code 1 must paint periodcentered");
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\267) Tj ET",
        "/FontDescriptor 6 0 R /Encoding null",
        &eexec_font_objs(EEXEC),
    );
    let r = render(&pdf).unwrap();
    assert_eq!(inked(&r), 0, "0xB7 is not in the built-in table");
}

#[test]
fn type1_differences_without_base_overlay_the_builtin_table() {
    // /Differences names code 65 → periodcentered; code 1 still comes
    // from the built-in table.
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (A) Tj ET",
        "/FontDescriptor 6 0 R /Encoding << /Differences [65 /periodcentered] >>",
        &eexec_font_objs(EEXEC),
    );
    assert!(inked(&render(&pdf).unwrap()) > 0);
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\001) Tj ET",
        "/FontDescriptor 6 0 R /Encoding << /Differences [65 /periodcentered] >>",
        &eexec_font_objs(EEXEC),
    );
    assert!(inked(&render(&pdf).unwrap()) > 0);
}

#[test]
fn type1_mac_roman_encoding_maps_its_own_codes() {
    // MacRoman 0xE1 = periodcentered (WinAnsi puts it at 0xB7).
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\341) Tj ET",
        "/FontDescriptor 6 0 R /Encoding /MacRomanEncoding",
        &eexec_font_objs(EEXEC),
    );
    assert!(
        inked(&render(&pdf).unwrap()) > 0,
        "MacRoman 0xE1 → periodcentered"
    );
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\267) Tj ET",
        "/FontDescriptor 6 0 R /Encoding /MacRomanEncoding",
        &eexec_font_objs(EEXEC),
    );
    assert_eq!(
        inked(&render(&pdf).unwrap()),
        0,
        "0xB7 is /paragraph in MacRoman"
    );
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\341) Tj ET",
        "/FontDescriptor 6 0 R /Encoding /MacExpertEncoding",
        &eexec_font_objs(EEXEC),
    );
    assert!(matches!(render(&pdf), Err(Error::Unsupported { .. })));
}

#[test]
fn type1_text_extraction_follows_the_builtin_encoding() {
    let pdf = pdf_with_font(
        b"BT /F0 40 Tf 20 100 Td (\\001) Tj ET",
        "/FontDescriptor 6 0 R /Encoding null",
        &eexec_font_objs(EEXEC),
    );
    assert_eq!(text(&pdf).unwrap(), "\u{00B7}");
}
