//! M7 — Type 0 / CIDFontType2 / Identity-H: rendering and extraction.
//!
//! Covers: dictionary validation, 2-byte code reading (odd tails
//! refused, codes never joined across TJ items), CIDToGIDMap
//! (Identity/stream/absent, CID≠GID), DW/W (both forms, defaults),
//! 2-byte ToUnicode (incl. surrogates, multi-scalar, unresolved),
//! Tw non-application on 2-byte codes, budgets, and coordinate
//! semantics shared with M6.1.

use kasane_pdf::{extract_text, render_page, Document, Error, Limits, RenderOptions, ResolveKind};

fn flate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut e = ZlibEncoder::new(Vec::new(), Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

const TTF: &str = "tests/fixtures/KasaneCIDTest.ttf";

/// Build a Type0/CIDFontType2 PDF.
/// - `stream` — page content stream.
/// - `cid_map` — `None` = /Identity, `Some(bytes)` = stream, `"omit"` sentinel = entry absent.
/// - `dw` — `/DW` value (default 1000 when `None`).
/// - `w` — `/W` array body text, e.g. `65 [500] 66 67 800` or empty for absent.
/// - `tounicode` — optional CMap text (auto-2-byte codespace implied).
/// - `extra` — extra CIDFont dict entries appended (e.g. "/WMode 1").
#[allow(clippy::too_many_arguments)]
fn pdf_type0(
    stream: &[u8],
    encoding: &str,
    subtype: &str,
    cid_map: Option<Option<Vec<u8>>>,
    dw: Option<i64>,
    w: Option<&str>,
    tounicode: Option<&[u8]>,
    extra_cid: &str,
) -> Vec<u8> {
    let ttf = std::fs::read(TTF).unwrap();
    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    // Type0 parent.
    let mut t0 = b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneCIDTest-Regular \
        /Encoding /"
        .to_vec();
    t0.extend_from_slice(encoding.as_bytes());
    t0.extend_from_slice(b" /DescendantFonts [6 0 R]");
    let tu_idx = bodies.len() + 6; // will be obj 9 if present
    let has_tu = tounicode.is_some();
    if has_tu {
        t0.extend_from_slice(b" /ToUnicode 9 0 R");
    }
    t0.extend_from_slice(b" >>");
    bodies.push((5, t0));
    // CIDFontType2 descendant.
    let mut cid = b"<< /Type /Font /Subtype /".to_vec();
    cid.extend_from_slice(subtype.as_bytes());
    cid.extend_from_slice(
        b" /BaseFont /KasaneCIDTest-Regular /CIDSystemInfo \
          << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R"
            .as_slice(),
    );
    match cid_map {
        Some(None) => cid.extend_from_slice(b" /CIDToGIDMap /Identity".as_slice()),
        Some(Some(_)) => cid.extend_from_slice(b" /CIDToGIDMap 8 0 R".as_slice()),
        None => {} // absent
    }
    if let Some(d) = dw {
        cid.extend_from_slice(format!(" /DW {d}").as_bytes());
    }
    if let Some(w) = w {
        cid.extend_from_slice(format!(" /W [{w}]").as_bytes());
    }
    if !extra_cid.is_empty() {
        cid.extend_from_slice(b" ");
        cid.extend_from_slice(extra_cid.as_bytes());
    }
    cid.extend_from_slice(b" >>");
    bodies.push((6, cid));
    // FontDescriptor + FontFile2.
    bodies.push((
        7,
        b"<< /Type /FontDescriptor /FontName /KasaneCIDTest-Regular \
           /Flags 4 /FontBBox [0 -200 600 800] /ItalicAngle 0 \
           /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 \
           /FontFile2 8 0 R >>"
            .to_vec(),
    ));
    bodies.push((
        8,
        [
            format!(
                "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
                flate(&ttf).len(),
                ttf.len()
            )
            .into_bytes(),
            flate(&ttf),
            b"\nendstream".to_vec(),
        ]
        .concat(),
    ));
    // Optional ToUnicode + optional CIDToGIDMap stream — objects 9+
    // are renumbered below; keep track.
    let mut next = 9u32;
    let tu_num;
    if let Some(tu) = tounicode {
        tu_num = next;
        bodies.push((
            tu_num,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                    flate(tu).len()
                )
                .into_bytes(),
                flate(tu),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ));
        next += 1;
    }
    let map_num;
    if let Some(Some(map)) = &cid_map {
        map_num = next;
        bodies.push((
            map_num,
            [
                format!("<< /Length {} >>\nstream\n", map.len()).into_bytes(),
                map.clone(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ));
        // Fix the CIDFont's reference to the map stream.
        let cid_fixed = {
            let c = bodies.iter().find(|(n, _)| *n == 6).unwrap().1.clone();
            let s = String::from_utf8_lossy(&c).to_string();
            let fixed = s.replace("/CIDToGIDMap 8 0 R", &format!("/CIDToGIDMap {map_num} 0 R"));
            fixed.into_bytes()
        };
        bodies.retain(|(n, _)| *n != 6);
        bodies.push((6, cid_fixed));
        let _ = next;
    }
    let _ = tu_idx;
    bodies.sort_by_key(|(n, _)| *n);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = bodies.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..size {
        if let Some((_, off)) = offs.iter().find(|(on, _)| *on == n) {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            out.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn extract_bytes(bytes: &[u8]) -> Result<kasane_pdf::ExtractedText, Error> {
    let doc = Document::from_bytes(bytes)?;
    extract_text(&doc, &doc.pages()[0], &Limits::default())
}

fn render_bytes(bytes: &[u8]) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(bytes)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

/// Two-byte BE codes as a hex *string* body — `<`/`>` are added by
/// the caller. `be(&[0x0041,0x0042])` → `b"0041 0042"`-style hex
/// digits (space separated, legal inside `<…>`).
fn be(codes: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in codes.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(format!("{c:04X}").as_bytes());
    }
    out
}

// --------------------------------------------------------------------
// Dictionary / encoding validation
// --------------------------------------------------------------------

#[test]
fn basic_type0_renders_and_extracts() {
    let s = be(&[0x0041, 0x0042]);
    let mut st = b"BT /F1 24 Tf 20 60 Td <".to_vec();
    st.extend_from_slice(&s);
    st.extend_from_slice(b"> Tj ET");
    let bytes = pdf_type0(
        &st,
        "Identity-H",
        "CIDFontType2",
        None,
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars.len(), 2);
    assert_eq!(t.chars[0].code, 0x0041);
    assert_eq!(t.chars[1].code, 0x0042);
    // No ToUnicode → unresolved (never guess Identity=Unicode).
    assert_eq!(t.chars[0].resolved_via, ResolveKind::Unresolved);
}

#[test]
fn identity_v_refused() {
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-V",
        "CIDFontType2",
        None,
        None,
        None,
        None,
        "",
    );
    let err = extract_bytes(&bytes).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn non_identity_encoding_refused() {
    for enc in ["UniJIS-UCS2-H", "90ms-RKSJ-H"] {
        let bytes = pdf_type0(
            b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
            enc,
            "CIDFontType2",
            None,
            None,
            None,
            None,
            "",
        );
        let err = extract_bytes(&bytes).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{enc}: {err:?}");
    }
}

#[test]
fn cidfont_type0_refused() {
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType0",
        None,
        None,
        None,
        None,
        "",
    );
    let err = extract_bytes(&bytes).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn odd_length_string_refused() {
    // A 1-byte trailing byte can't form a complete 2-byte code.
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041 42> Tj ET",
        "Identity-H",
        "CIDFontType2",
        None,
        None,
        None,
        None,
        "",
    );
    let err = extract_bytes(&bytes).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn tj_items_do_not_join_codes() {
    // TJ ["\x00" 0 "AB"] — the split must not glue the tail of one
    // string to the head of the next.
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td [<00> 0 <41>] TJ ET",
        "Identity-H",
        "CIDFontType2",
        None,
        None,
        None,
        None,
        "",
    );
    let err = extract_bytes(&bytes).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

// --------------------------------------------------------------------
// CIDToGIDMap
// --------------------------------------------------------------------

#[test]
fn identity_map_draws_glyph_at_cid() {
    // Identity: CID 1 → GID 1 (box), CID 2 → GID 2 (tri).
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0001 0002> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None), // explicit /Identity
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars.len(), 2);
    assert!(t.chars[0].bounds.is_some());
    assert!(t.chars[1].bounds.is_some());
    // Different glyphs → different ink.
    assert_ne!(t.chars[0].bounds, t.chars[1].bounds);
}

#[test]
fn stream_map_remaps_cid() {
    // CIDToGIDMap stream: CID 0x41 → gid 3 (bar), CID 0x42 → gid 1 (box).
    let mut map = vec![0u8; 0x43 * 2];
    map[0x41 * 2 + 1] = 3;
    map[0x42 * 2 + 1] = 1;
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(Some(map)),
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    let b = t.chars[0].bounds.unwrap();
    // Bar glyph is narrower than the box — left bar is ~200 units vs
    // the box's 400. Its ink bbox width at 24pt ≈ 200/1000*24 = 4.8.
    let ink_w = b.2 - b.0;
    assert!(ink_w < 8.0, "bar glyph should be narrow, got {ink_w}");
}

#[test]
fn absent_map_is_identity() {
    // No /CIDToGIDMap entry at all → default Identity.
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0003> Tj ET",
        "Identity-H",
        "CIDFontType2",
        None, // absent
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // CID 3 → gid 3 = bar → narrow ink.
    let b = t.chars[0].bounds.unwrap();
    assert!(b.2 - b.0 < 8.0);
}

#[test]
fn map_entry_beyond_glyphs_is_notdef() {
    // Map CID 0x41 → gid 99 — font has 6 glyphs → .notdef.
    let mut map = vec![0u8; 0x42 * 2];
    map[0x41 * 2 + 1] = 99;
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(Some(map)),
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // .notdef has an empty outline in our font → no bounds.
    assert_eq!(t.chars[0].bounds, None);
}

#[test]
fn short_map_missing_entries_are_notdef() {
    // Map covers CIDs 0..0x20 only; code 0x41 is beyond → .notdef.
    let mut map = vec![0u8; 0x21 * 2];
    map[0x10 * 2 + 1] = 1; // CID 0x10 → gid 1
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0010 0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(Some(map)),
        None,
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert!(t.chars[0].bounds.is_some()); // in map → box
    assert_eq!(t.chars[1].bounds, None); // beyond map → .notdef
}

// --------------------------------------------------------------------
// DW / W
// --------------------------------------------------------------------

#[test]
fn dw_default_1000() {
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001 0002> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None, // absent → 1000
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // Advance = 1000/1000 × 10 = 10pt.
    assert!((t.chars[0].advance.0 - 10.0).abs() < 0.01);
}

#[test]
fn dw_explicit_overrides_default() {
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001 0002> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(500), // /DW 500
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert!((t.chars[0].advance.0 - 5.0).abs() < 0.01);
}

#[test]
fn w_array_form_per_cid() {
    // /W [ 1 [600] 2 [700] ] — CID 1 → 600, CID 2 → 700.
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001 0002> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        Some("1 [600] 2 [700]"),
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert!((t.chars[0].advance.0 - 6.0).abs() < 0.01);
    assert!((t.chars[1].advance.0 - 7.0).abs() < 0.01);
}

#[test]
fn w_range_form() {
    // /W [ 1 3 800 ] — CIDs 1..3 all 800.
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001 0003> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        Some("1 3 800"),
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert!((t.chars[0].advance.0 - 8.0).abs() < 0.01);
    assert!((t.chars[1].advance.0 - 8.0).abs() < 0.01);
}

#[test]
fn w_unlisted_cid_uses_dw() {
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0005 0001> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(400),
        Some("1 [999]"),
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // CID 5 not in W → DW 400 → 4pt.
    assert!((t.chars[0].advance.0 - 4.0).abs() < 0.01);
    assert!((t.chars[1].advance.0 - 9.99).abs() < 0.01);
}

#[test]
fn pdf_width_not_font_hmtx() {
    // The font's hmtx advance is 600 for all glyphs; /W 1 [123] must
    // produce a 1.23pt advance at 10pt — not the 6pt hmtx would give.
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        Some("1 [123]"),
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert!((t.chars[0].advance.0 - 1.23).abs() < 0.01);
}

// --------------------------------------------------------------------
// Spacing — Tw on 2-byte codes
// --------------------------------------------------------------------

#[test]
fn tw_not_applied_to_2byte_space() {
    // 0x0020 contains byte 0x20 but is a 2-byte code — Tw must NOT
    // apply (§9.3.3).
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 5 Tw 20 60 Td <0020> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        Some("32 [600]"),
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // Advance = 600/1000×10 = 6pt, not 6+5.
    assert!((t.chars[0].advance.0 - 6.0).abs() < 0.01);
}

// --------------------------------------------------------------------
// ToUnicode (2-byte)
// --------------------------------------------------------------------

#[test]
fn tounicode_2byte_bfchar() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0041> <3042>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("あ"));
    assert_eq!(t.chars[0].resolved_via, ResolveKind::ToUnicode);
}

#[test]
fn tounicode_2byte_bfrange() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfrange
        <0041> <0043> [<3042><3044><3046>]
        endbfrange
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041 0043> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("あ"));
    assert_eq!(t.chars[1].unicode.as_deref(), Some("う"));
}

#[test]
fn tounicode_surrogate_pair_2byte() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0041> <D840DC3E>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("\u{2003E}"));
}

#[test]
fn tounicode_multi_scalar_2byte() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0041> <00660066>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("ff"));
}

#[test]
fn tounicode_missing_entry_unresolved() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0041> <0041>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0041 0042> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[1].unicode, None);
    assert_eq!(t.chars[1].resolved_via, ResolveKind::Unresolved);
}

#[test]
fn glyph_and_unicode_are_independent() {
    // ToUnicode maps CID 1 → 'A' but the glyph drawn is the box
    // (gid 1). Rendering and extraction report different things
    // — by design; they must not conflate.
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0001> <0041>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td <0001> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    // Glyph is the box — ink bbox is box-shaped, not A-shaped.
    let b = t.chars[0].bounds.unwrap();
    assert!(b.2 - b.0 > 5.0, "box glyph is wide");
}

// --------------------------------------------------------------------
// Position / spacing / form / limits — shared with M5/M6
// --------------------------------------------------------------------

#[test]
fn tj_adjustment_moves_next_glyph() {
    // <0041> 100 <0042> — TJ number shifts next glyph's origin.
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td [<0041> 100 <0042>] TJ ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // 'B' origin shifted left by 100/1000×10 = 1pt.
    assert!((t.chars[1].origin.0 - (20.0 + 10.0 - 1.0)).abs() < 0.01);
}

#[test]
fn tc_tw_tz_ts_apply() {
    // Tz 50% halves the advance; Tc 2 adds spacing; Ts moves baseline.
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 50 Tz 2 Tc 5 Ts 20 60 Td <0001 0002> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    // tx = (1000/1000 × 10 + 2) × 0.5 = 6pt.
    assert!((t.chars[0].advance.0 - 6.0).abs() < 0.01);
    assert!((t.chars[0].origin.1 - 65.0).abs() < 0.01); // Ts 5
}

#[test]
fn quote_and_double_quote_ops() {
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 14 TL 20 80 Td <0001> Tj <0002> ' ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars.len(), 2);
    // Second line at 80-14=66.
    assert!((t.chars[1].origin.1 - 66.0).abs() < 0.01);
}

#[test]
fn form_with_cid_text_extracts() {
    let form = b"BT /F2 20 Tf 0 0 Td <0001> Tj ET";
    let form_obj: Vec<u8> = [
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] \
             /Resources << /Font << /F2 9 0 R >> >> /Matrix [1 0 0 1 50 20] \
             /Length {} >>\nstream\n",
            form.len()
        )
        .into_bytes(),
        form.to_vec(),
        b"\nendstream".to_vec(),
    ]
    .concat();
    // Build page with XObject pointing at form + font renumber.
    let ttf = std::fs::read(TTF).unwrap();
    let mut bodies: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
               /Resources << /XObject << /Fm 8 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, b"<< /Length 7 >>\nstream\n/Fm Do\nendstream".to_vec()),
        (8, form_obj),
        // Type0 parent / CIDFont / FontDescriptor / FontFile2 for F2.
        (
            9,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneCIDTest-Regular \
               /Encoding /Identity-H /DescendantFonts [10 0 R] >>"
                .to_vec(),
        ),
        (
            10,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KasaneCIDTest-Regular \
               /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
               /FontDescriptor 11 0 R /CIDToGIDMap /Identity /DW 1000 >>"
                .to_vec(),
        ),
        (
            11,
            b"<< /Type /FontDescriptor /FontName /KasaneCIDTest-Regular \
               /Flags 4 /FontBBox [0 -200 600 800] /ItalicAngle 0 \
               /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 \
               /FontFile2 12 0 R >>"
                .to_vec(),
        ),
        (
            12,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
                    flate(&ttf).len(),
                    ttf.len()
                )
                .into_bytes(),
                flate(&ttf),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    bodies.sort_by_key(|(n, _)| *n);
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offs = Vec::new();
    for (n, b) in &bodies {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 13\n0000000000 65535 f \n");
    for n in 1..13 {
        if let Some((_, off)) = offs.iter().find(|(on, _)| *on == n) {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            // Free entry for absent object numbers — the xref count
            // must cover every slot in the declared range.
            out.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    out.extend_from_slice(b"trailer\n<< /Size 13 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    let t = extract_bytes(&out).unwrap();
    assert_eq!(t.chars.len(), 1);
    assert_eq!(t.chars[0].origin, (50.0, 20.0));
}

#[test]
fn unicode_budget_2byte() {
    let cmap = b"begincmap
        1 begincodespacerange
        <0000> <FFFF>
        endcodespacerange
        1 beginbfchar
        <0041> <0066>
        endbfchar
        endcmap";
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0041 0041> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        Some(cmap),
        "",
    );
    let doc = Document::from_bytes(&bytes).unwrap();
    let ok = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(ok.chars.len(), 2);
    let err = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_extracted_unicode_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }));
}

#[test]
fn max_text_codes_counts_2byte_codes() {
    // 3 2-byte codes = 3 text codes (not 6 bytes).
    let bytes = pdf_type0(
        b"BT /F1 10 Tf 20 60 Td <0001 0002 0003> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        None,
        "",
    );
    let doc = Document::from_bytes(&bytes).unwrap();
    let err = extract_text(
        &doc,
        &doc.pages()[0],
        &Limits {
            max_text_codes: 2,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }));
}

// --------------------------------------------------------------------
// Rendering unchanged / regression
// --------------------------------------------------------------------

#[test]
fn type0_renders_distinct_glyphs() {
    // CID 1=box vs CID 3=bar — different ink footprint.
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 40 Td <0001 0003> Tj ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        None,
        None,
        "",
    );
    let page = render_bytes(&bytes).unwrap();
    let data = page.pixmap.data();
    assert!(data.chunks_exact(4).any(|p| p[3] > 0));
}

#[test]
fn rotated_cid_text() {
    let bytes = pdf_type0(
        b"q 0 1 -1 0 50 20 cm BT /F1 20 Tf <0001> Tj ET Q",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        Some(1000),
        None,
        None,
        "",
    );
    let t = extract_bytes(&bytes).unwrap();
    assert_eq!(t.chars[0].origin, (50.0, 20.0));
    assert!((t.chars[0].advance.1 - 20.0).abs() < 0.01); // advance points +y
}

#[test]
fn actual_text_refused_in_composite_text() {
    // M6.1's ActualText detection is a shared marked-content hook —
    // it must fire inside composite-font text too.
    let bytes = pdf_type0(
        b"BT /F1 24 Tf 20 60 Td /Span << /ActualText (x) >> BDC <0001> Tj EMC ET",
        "Identity-H",
        "CIDFontType2",
        Some(None),
        None,
        None,
        None,
        "",
    );
    let err = extract_bytes(&bytes).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

#[test]
fn m5_simple_font_regression() {
    // Existing M5/M6 fixtures still work — no composite regression.
    let pdf = std::fs::read("tests/fixtures/text_basic.pdf").unwrap();
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    assert_eq!(t.chars.len(), 3);
    assert_eq!(t.chars[0].unicode.as_deref(), Some("A"));
    assert_eq!(t.chars[0].resolved_via, ResolveKind::EncodingFallback);
}
