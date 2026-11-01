//! Text for codes without a `/ToUnicode` entry (§9.10.2): glyph names
//! through the full Adobe Glyph List (simple fonts, Type 3 included),
//! the CFF built-in encoding, the embedded program's Unicode `cmap`
//! read backwards (unambiguous glyphs only) and, with `cjk-fallback`,
//! the Adobe-Japan1 collection. Nothing is guessed: names without
//! meaning, symbol-only cmaps, ambiguous or private-use glyphs and
//! oversized cmaps stay unresolved; `/ToUnicode` always wins.
//!
//! Fonts: `fixtures/unicode_fallback/make_fonts.py` (synthetic boxes),
//! `fixtures/type1_cff.bin` (see NOTICE).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{extract_text, Document, ResolveKind, TextOptions};

const UNI: &[u8] = include_bytes!("fixtures/unicode_fallback/uni.ttf");
const SYMBOL: &[u8] = include_bytes!("fixtures/unicode_fallback/symbol.ttf");
const BIG: &[u8] = include_bytes!("fixtures/unicode_fallback/big.ttf");
const BROKEN: &[u8] = include_bytes!("fixtures/unicode_fallback/broken.ttf");
const CFF: &[u8] = include_bytes!("fixtures/type1_cff.bin");

/// A one-page PDF showing `show` (a PDF string token, e.g. `(abc)` or
/// `<0001>`) with font object 5; `objects` supplies 5 and anything it
/// references (numbers from 6).
fn pdf(show: &str, objects: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let content = format!("BT /F 12 Tf 10 10 Td {show} Tj ET");
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 50] /Contents 4 0 R \
              /Resources << /Font << /F 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(objects);
    build_pdf(&objs)
}

type Got = Vec<(Option<String>, ResolveKind)>;

fn text(pdf: &[u8]) -> Got {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .into_iter()
        .map(|c| (c.unicode, c.resolved_via))
        .collect()
}

fn s(t: &str, k: ResolveKind) -> (Option<String>, ResolveKind) {
    (Some(t.to_string()), k)
}

const UNRESOLVED: (Option<String>, ResolveKind) = (None, ResolveKind::Unresolved);

fn tounicode(map: &str) -> Vec<u8> {
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
         1 begincodespacerange <00> <FF> endcodespacerange {map} endcmap end end"
    );
    stream_obj("", cmap.as_bytes())
}

// ---- simple fonts: names ------------------------------------------

#[test]
fn names_outside_the_old_table_map_through_the_glyph_list() {
    // Non-embedded Helvetica with names the WinAnsi-range table never
    // knew; meaningless names stay unresolved.
    let got = text(&pdf(
        "<616263646566>",
        vec![(
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
              /Encoding << /Differences [97 /fi /f_f_i /a.sc /minus /g12 /rect] >> >>"
                .to_vec(),
        )],
    ));
    let e = ResolveKind::EncodingFallback;
    assert_eq!(
        got,
        vec![
            s("\u{FB01}", e),
            s("ffi", e),
            s("a", e),
            s("\u{2212}", e),
            UNRESOLVED,
            UNRESOLVED
        ]
    );
}

#[test]
fn mac_roman_and_win_ansi_names() {
    // MacRoman 0xDE is /fi (outside the old table); WinAnsi 0x96 is
    // /endash and 0x80 /Euro (inside it, unchanged).
    let mac = text(&pdf(
        "<DE41>",
        vec![(
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /MacRomanEncoding >>"
                .to_vec(),
        )],
    ));
    let e = ResolveKind::EncodingFallback;
    assert_eq!(mac, vec![s("\u{FB01}", e), s("A", e)]);
    let win = text(&pdf(
        "<9680>",
        vec![(
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        )],
    ));
    assert_eq!(win, vec![s("\u{2013}", e), s("\u{20AC}", e)]);
}

#[test]
fn tounicode_wins_and_has_no_fallback() {
    // Code 97 is /fi by name but `X` by /ToUnicode; code 98 is /b by
    // name but absent from the ToUnicode map: no text (§9.10.2).
    let got = text(&pdf(
        "(ab)",
        vec![
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                  /Encoding << /Differences [97 /fi /b] >> /ToUnicode 6 0 R >>"
                    .to_vec(),
            ),
            (6, tounicode("1 beginbfchar <61> <0058> endbfchar")),
        ],
    ));
    assert_eq!(got, vec![s("X", ResolveKind::ToUnicode), UNRESOLVED]);
}

#[test]
fn type3_character_names() {
    let font = |extra: &str| {
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [0.001 0 0 0.001 0 0] \
             /CharProcs << /a 7 0 R /zero 7 0 R /g12 7 0 R /space 7 0 R /a12 7 0 R >> \
             /Encoding << /Differences [97 /a /zero /g12 /space /a12] >> \
             /FirstChar 97 /LastChar 101 /Widths [500 500 500 500 500] {extra} >>"
        )
        .into_bytes()
    };
    let cp = (7, stream_obj("", b"500 0 d0"));
    let e = ResolveKind::EncodingFallback;
    // `a12` is a ZapfDingbats name, but a Type 3 character name is
    // arbitrary: no dingbat.
    let got = text(&pdf("(abcde)", vec![(5, font("")), cp.clone()]));
    assert_eq!(
        got,
        vec![s("a", e), s("0", e), UNRESOLVED, s(" ", e), UNRESOLVED]
    );
    // With a /ToUnicode, it alone decides.
    let got = text(&pdf(
        "(ab)",
        vec![
            (5, font("/ToUnicode 6 0 R")),
            (6, tounicode("1 beginbfchar <61> <0051> endbfchar")),
            cp,
        ],
    ));
    assert_eq!(got, vec![s("Q", ResolveKind::ToUnicode), UNRESOLVED]);
}

#[test]
fn the_cff_built_in_encoding_names_the_glyph() {
    // No /Encoding: the program's StandardEncoding and charset name the
    // glyphs (A, b); a code without a glyph has no text.
    let got = text(&pdf(
        "(AbZ)",
        vec![
            (
                5,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /PAXEKO+KasaneSansPro-Bold \
                  /FirstChar 65 /LastChar 98 /Widths [600 600 600 600 600 600 600 600 600 600 \
                  600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 \
                  600 600 600 600] /FontDescriptor 6 0 R >>"
                    .to_vec(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /PAXEKO+KasaneSansPro-Bold /Flags 4 \
                  /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                  /CapHeight 700 /StemV 80 /FontFile3 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj("/Subtype /Type1C", CFF)),
        ],
    ));
    let e = ResolveKind::EncodingFallback;
    assert_eq!(got, vec![s("A", e), s("b", e), UNRESOLVED]);
}

// ---- simple TrueType: the embedded cmap ---------------------------

/// A simple TrueType font over `program`, codes 1..=7 named /g1../g7
/// (glyph indices), or with no /Encoding at all when `names` is false.
fn truetype(program: &[u8], names: bool, flags: u32) -> Vec<(u32, Vec<u8>)> {
    let enc = if names {
        "/Encoding << /Differences [1 /g1 /g2 /g3 /g4 /g5 /g6 /g7] >>"
    } else {
        ""
    };
    vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /KasaneUni {enc} \
                 /FirstChar 1 /LastChar 255 /Widths [{}] /FontDescriptor 6 0 R >>",
                vec!["600"; 255].join(" ")
            )
            .into_bytes(),
        ),
        (
            6,
            format!(
                "<< /Type /FontDescriptor /FontName /KasaneUni /Flags {flags} \
                 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
            )
            .into_bytes(),
        ),
        (7, stream_obj("", program)),
    ]
}

#[test]
fn a_glyph_index_name_reads_the_font_cmap_backwards() {
    // g1 → U+0061; g2 ← A and Alpha: ambiguous; g3 ← space and
    // no-break space: equivalent; g4 ← private use only; g5 has no cmap
    // entry but the post name "Omega"; g6 nothing; g7 → é.
    let got = text(&pdf("<01020304050607>", truetype(UNI, true, 32)));
    let p = ResolveKind::FontProgram;
    assert_eq!(
        got,
        vec![
            s("a", p),
            UNRESOLVED,
            s(" ", p),
            UNRESOLVED,
            s("\u{2126}", p),
            UNRESOLVED,
            s("\u{E9}", p),
        ]
    );
}

#[test]
fn a_symbol_cmap_carries_no_text() {
    // Symbolic, (3,0) only: code 0x41 draws glyph 1 through U+F041, but
    // nothing says what it means.
    let got = text(&pdf("(A)", truetype(SYMBOL, false, 4)));
    assert_eq!(got, vec![UNRESOLVED]);
}

#[test]
fn an_oversized_cmap_is_not_read_backwards() {
    // 196 608 code points to one glyph exceed the reverse-table bound:
    // even g2, whose (3,1) entry is U+0061, stays unresolved.
    let got = text(&pdf("<0102>", truetype(BIG, true, 32)));
    assert_eq!(got, vec![UNRESOLVED, UNRESOLVED]);
}

#[test]
fn a_broken_cmap_never_panics() {
    // Every cmap subtable offset points past the table: no glyph is
    // reachable through it and no text comes out of it.
    let doc = Document::from_bytes(&pdf("<0107>", truetype(BROKEN, true, 32))).unwrap();
    match extract_text(&doc, &doc.pages()[0], &TextOptions::default()) {
        Ok(t) => assert!(
            t.chars
                .iter()
                .all(|c| c.resolved_via != ResolveKind::FontProgram),
            "{:?}",
            t.chars
        ),
        Err(e) => panic!("extraction refused: {e}"),
    }
}

#[test]
fn a_garbage_cff_never_panics() {
    let mut objs = vec![
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /X /FirstChar 65 /LastChar 65 \
              /Widths [600] /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /X /Flags 4 /FontBBox [0 0 1 1] /ItalicAngle 0 \
              /Ascent 1 /Descent 0 /CapHeight 1 /StemV 1 /FontFile3 7 0 R >>"
                .to_vec(),
        ),
    ];
    objs.push((
        7,
        stream_obj("/Subtype /Type1C", &[1, 0, 4, 1, 0xFF, 0xFF, 0, 0, 0, 0]),
    ));
    let doc = Document::from_bytes(&pdf("(A)", objs)).unwrap();
    if let Ok(t) = extract_text(&doc, &doc.pages()[0], &TextOptions::default()) {
        assert!(t.chars.iter().all(|c| c.unicode.is_none()));
    }
}

// ---- composite fonts ------------------------------------------------

/// A Type 0 font over `program` with `encoding` (Identity-H / -V),
/// `ordering` of the Adobe registry and an optional /CIDToGIDMap object.
fn type0(
    program: &[u8],
    encoding: &str,
    ordering: &str,
    c2g: Option<Vec<u8>>,
) -> Vec<(u32, Vec<u8>)> {
    let map = if c2g.is_some() {
        "/CIDToGIDMap 9 0 R"
    } else {
        ""
    };
    let mut v = vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /KasaneUni /Encoding /{encoding} \
                 /DescendantFonts [6 0 R] >>"
            )
            .into_bytes(),
        ),
        (
            6,
            format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KasaneUni \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering ({ordering}) /Supplement 0 >> \
                 /FontDescriptor 7 0 R /DW 1000 {map} >>"
            )
            .into_bytes(),
        ),
        (
            7,
            b"<< /Type /FontDescriptor /FontName /KasaneUni /Flags 4 /FontBBox [0 0 1000 1000] \
              /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 8 0 R >>"
                .to_vec(),
        ),
        (8, stream_obj("", program)),
    ];
    if let Some(m) = c2g {
        v.push((9, stream_obj("", &m)));
    }
    v
}

#[test]
fn identity_h_and_v_read_the_program_cmap() {
    let p = ResolveKind::FontProgram;
    let want = vec![
        s("a", p),
        UNRESOLVED,
        s(" ", p),
        UNRESOLVED,
        s("\u{2126}", p),
        UNRESOLVED,
        s("\u{E9}", p),
    ];
    for enc in ["Identity-H", "Identity-V"] {
        let got = text(&pdf(
            "<0001000200030004000500060007>",
            type0(UNI, enc, "Identity", None),
        ));
        assert_eq!(got, want, "{enc}");
    }
}

#[test]
fn the_glyph_not_the_cid_is_looked_up() {
    // /CIDToGIDMap: CID 1 → GID 7 (é), CID 7 → GID 1 (a). Codes are CIDs
    // under Identity-H; the text follows the glyph drawn.
    let mut map = vec![0u8; 16];
    map[2..4].copy_from_slice(&7u16.to_be_bytes());
    map[14..16].copy_from_slice(&1u16.to_be_bytes());
    let got = text(&pdf(
        "<00010007>",
        type0(UNI, "Identity-H", "Identity", Some(map)),
    ));
    let p = ResolveKind::FontProgram;
    assert_eq!(got, vec![s("\u{E9}", p), s("a", p)]);
}

#[test]
fn a_tounicode_on_a_composite_font_wins() {
    let mut objs = type0(UNI, "Identity-H", "Identity", None);
    objs[0].1 = b"<< /Type /Font /Subtype /Type0 /BaseFont /KasaneUni /Encoding /Identity-H \
                   /DescendantFonts [6 0 R] /ToUnicode 10 0 R >>"
        .to_vec();
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                1 begincodespacerange <0000> <FFFF> endcodespacerange \
                1 beginbfchar <0001> <005A> endbfchar endcmap end end";
    objs.push((10, stream_obj("", cmap.as_bytes())));
    let got = text(&pdf("<00010007>", objs));
    assert_eq!(got, vec![s("Z", ResolveKind::ToUnicode), UNRESOLVED]);
}

/// §9.10.2 method 3: an Adobe-Japan1 font without /ToUnicode maps its
/// CIDs through Adobe-Japan1-UCS2. Where the embedded program's cmap
/// disagrees about a glyph, the code is left unresolved.
#[cfg(feature = "cjk-fallback")]
#[test]
fn the_japan1_collection_maps_cids_and_conflicts_stay_unresolved() {
    // uni.ttf under Japan1: CID 34 is "A" in the collection and GID 34
    // does not exist (program silent) → "A"; CID 1 is " " in the
    // collection but GID 1 is "a" in the program → conflict.
    let got = text(&pdf("<00220001>", type0(UNI, "Identity-H", "Japan1", None)));
    assert_eq!(
        got,
        vec![s("A", ResolveKind::CharacterCollection), UNRESOLVED]
    );
    // CID 0 is .notdef: no text (the UCS2 resource maps it to U+FFFD).
    let got = text(&pdf("<0000>", type0(UNI, "Identity-H", "Japan1", None)));
    assert_eq!(got, vec![UNRESOLVED]);
}

/// `UNI` with its `cmap` replaced by one (3,10) format 12 subtable of
/// `groups` groups, each mapping all of U+0000..U+10FFFF to glyph 1.
fn hostile_cmap(groups: u32) -> Vec<u8> {
    let mut cmap = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12, 0, 12, 0, 0];
    cmap.extend_from_slice(&(16 + 12 * groups).to_be_bytes());
    cmap.extend_from_slice(&[0, 0, 0, 0]);
    cmap.extend_from_slice(&groups.to_be_bytes());
    for _ in 0..groups {
        cmap.extend_from_slice(&0u32.to_be_bytes());
        cmap.extend_from_slice(&0x10_FFFFu32.to_be_bytes());
        cmap.extend_from_slice(&1u32.to_be_bytes());
    }
    let mut font = UNI.to_vec();
    let n = u16::from_be_bytes([font[4], font[5]]) as usize;
    let rec = (0..n)
        .map(|i| 12 + 16 * i)
        .find(|&r| &font[r..r + 4] == b"cmap")
        .unwrap();
    let off = font.len() as u32;
    font[rec + 8..rec + 12].copy_from_slice(&off.to_be_bytes());
    font[rec + 12..rec + 16].copy_from_slice(&(cmap.len() as u32).to_be_bytes());
    font.extend_from_slice(&cmap);
    font
}

#[test]
fn a_hostile_cmap_is_not_enumerated() {
    // 20 000 groups over all of Unicode: enumerating it would take
    // 2 · 10^10 callbacks; its coverage is counted from the headers
    // and the cmap is not read backwards.
    let t = std::time::Instant::now();
    let got = text(&pdf("<0105>", truetype(&hostile_cmap(20_000), true, 32)));
    assert!(t.elapsed().as_secs() < 5, "took {:?}", t.elapsed());
    // g1 is meaningless; g5 still has its post name "Omega".
    assert_eq!(
        got,
        vec![UNRESOLVED, s("\u{2126}", ResolveKind::FontProgram)]
    );
}

/// `UNI` with its cmap replaced by one (3,10) format 12 subtable of
/// sequential groups `(start, end, first glyph)`.
fn with_cmap12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut cmap = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12, 0, 12, 0, 0];
    cmap.extend_from_slice(&(16 + 12 * groups.len() as u32).to_be_bytes());
    cmap.extend_from_slice(&[0, 0, 0, 0]);
    cmap.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (s, e, g) in groups {
        cmap.extend_from_slice(&s.to_be_bytes());
        cmap.extend_from_slice(&e.to_be_bytes());
        cmap.extend_from_slice(&g.to_be_bytes());
    }
    let mut font = UNI.to_vec();
    let n = u16::from_be_bytes([font[4], font[5]]) as usize;
    let rec = (0..n)
        .map(|i| 12 + 16 * i)
        .find(|&r| &font[r..r + 4] == b"cmap")
        .unwrap();
    let off = font.len() as u32;
    font[rec + 8..rec + 12].copy_from_slice(&off.to_be_bytes());
    font[rec + 12..rec + 16].copy_from_slice(&(cmap.len() as u32).to_be_bytes());
    font.extend_from_slice(&cmap);
    font
}

#[test]
fn the_reverse_cmap_bound_is_exact() {
    // One sequential group from U+0041: glyph 1 ← "A" only (later code
    // points map to glyph ids beyond 65 535, which no font has).
    let max = 131_072u32;
    let p = ResolveKind::FontProgram;
    let at_bound = text(&pdf(
        "<01>",
        truetype(&with_cmap12(&[(0x41, 0x41 + max - 1, 1)]), true, 32),
    ));
    assert_eq!(at_bound, vec![s("A", p)]);
    let over = text(&pdf(
        "<01>",
        truetype(&with_cmap12(&[(0x41, 0x41 + max, 1)]), true, 32),
    ));
    assert_eq!(over, vec![UNRESOLVED]);
}

#[test]
fn cmap_entries_for_missing_glyphs_and_odd_ranges_are_harmless() {
    // Glyph ids the font does not have, a reversed group, a group at the
    // top of Unicode and one beyond it; glyph 7 keeps a valid entry.
    // Groups in the ascending order the format requires (lookups are
    // a binary search; an unsorted table is simply not found into).
    let font = with_cmap12(&[
        (0x50, 0x60, 60_000),
        (0x70, 0x60, 1),
        (0xE9, 0xE9, 7),
        (0x10_FFFF, 0x10_FFFF, 2),
        (0x11_0000, 0x11_0005, 3),
    ]);
    let got = text(&pdf("<010203070506>", truetype(&font, true, 32)));
    let p = ResolveKind::FontProgram;
    // U+10FFFF is a noncharacter and beyond-Unicode values are never
    // text; glyph 5 still has its post name.
    assert_eq!(
        got,
        vec![
            UNRESOLVED,
            UNRESOLVED,
            UNRESOLVED,
            s("\u{E9}", p),
            s("\u{2126}", p),
            UNRESOLVED
        ]
    );
}
