//! Q1 (v0.11): the codes WinAnsiEncoding leaves unused draw the
//! bullet. ISO 32000-1 Annex D.2, Table D.2 note 3 (PDF Reference 1.7
//! Appendix D.1, note 3): "In WinAnsiEncoding, all unused codes greater
//! than 40 map to the bullet character. However, only code 225 shall be
//! specifically assigned to the bullet character" (octal: codes above
//! 0x20; 225 is 0x95). Those codes are 0x7F, 0x81, 0x8D, 0x8F, 0x90 and
//! 0x9D. Poppler and MuPDF draw a bullet there; kasane drew nothing.
//! The control codes (0x20 and below), `/Differences`, and the other
//! named encodings are unchanged.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, render_page, Document, OpenOptions, RedactOptions, RenderOptions,
    ResolveKind, TextOptions,
};

const UNUSED: [u8; 6] = [0x7F, 0x81, 0x8D, 0x8F, 0x90, 0x9D];
const SANS: &[u8] = include_bytes!("../fonts/liberation/LiberationSans-Regular.ttf");
/// A CFF subset of A–f only: no `bullet` (tests/fixtures/README.md).
const CFF: &[u8] = include_bytes!("fixtures/type1_cff.bin");

/// A one-page PDF showing the bytes `show` (and then "A") with font
/// object 5; `objects` supplies 5 and anything it references (from 6).
fn pdf(show: &[u8], objects: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut content = b"BT /F 24 Tf 10 12 Td <".to_vec();
    for b in show.iter().chain(b"A") {
        content.extend(format!("{b:02X}").bytes());
    }
    content.extend(b"> Tj ET");
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 40] /Contents 4 0 R \
              /Resources << /Font << /F 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", &content)),
    ];
    objs.extend(objects);
    build_pdf(&objs)
}

/// Non-embedded Helvetica with `encoding` (a PDF object, e.g.
/// `/WinAnsiEncoding`), or none.
fn helvetica(encoding: Option<&str>) -> Vec<(u32, Vec<u8>)> {
    let enc = encoding
        .map(|e| format!(" /Encoding {e}"))
        .unwrap_or_default();
    vec![(
        5,
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica{enc} >>").into_bytes(),
    )]
}

/// Liberation Sans embedded as a non-symbolic TrueType font with
/// WinAnsiEncoding; every code 600 wide.
fn embedded_sans() -> Vec<(u32, Vec<u8>)> {
    embedded_sans_with(32, "")
}

/// The same with `/Flags flags` and `extra` in the font dictionary.
fn embedded_sans_with(flags: u32, extra: &str) -> Vec<(u32, Vec<u8>)> {
    let widths = vec!["600"; 224].join(" ");
    vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans \
                 /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths [{widths}] \
                 /FontDescriptor 6 0 R {extra} >>"
            )
            .into_bytes(),
        ),
        (
            6,
            format!(
                "<< /Type /FontDescriptor /FontName /LiberationSans /Flags {flags} \
                 /FontBBox [-203 -303 1050 910] /ItalicAngle 0 /Ascent 905 /Descent -212 \
                 /CapHeight 729 /StemV 80 /FontFile2 7 0 R >>"
            )
            .into_bytes(),
        ),
        (7, stream_obj(&format!("/Length1 {}", SANS.len()), SANS)),
    ]
}

fn pixels(pdf: &[u8], recovering: bool) -> Vec<u8> {
    let d = Document::open(pdf, &OpenOptions::new().with_recovery(recovering)).unwrap();
    render_page(&d, &d.pages()[0], &RenderOptions::new().with_dpi(144.0))
        .unwrap()
        .pixels()
        .to_vec()
}

fn text(pdf: &[u8]) -> String {
    text_in(pdf, false)
}

fn text_in(pdf: &[u8], recovering: bool) -> String {
    chars(pdf, recovering)
        .into_iter()
        .map(|(u, _)| u.unwrap_or_else(|| "\u{FFFD}".into()))
        .collect()
}

fn chars(pdf: &[u8], recovering: bool) -> Vec<(Option<String>, ResolveKind)> {
    let d = Document::open(pdf, &OpenOptions::new().with_recovery(recovering)).unwrap();
    extract_text(&d, &d.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .into_iter()
        .map(|c| (c.unicode, c.resolved_via))
        .collect()
}

fn ink(px: &[u8]) -> usize {
    px.chunks(4).filter(|p| p[..3] != [255, 255, 255]).count()
}

/// Each unused code draws exactly what 0x95 (the bullet) draws, at the
/// bullet's width (the "A" after it lands in the same place), in strict
/// and recovery mode, for a substituted and an embedded font.
#[test]
fn unused_codes_draw_the_bullet() {
    for (what, font) in [
        ("Helvetica", helvetica(Some("/WinAnsiEncoding"))),
        ("embedded TrueType", embedded_sans()),
    ] {
        for recovering in [false, true] {
            let bullet = pixels(&pdf(&[0x95], font.clone()), recovering);
            let a_only = pixels(&pdf(&[], font.clone()), recovering);
            assert!(
                ink(&bullet) > ink(&a_only),
                "{what}: the bullet itself draws"
            );
            for code in UNUSED {
                let got = pixels(&pdf(&[code], font.clone()), recovering);
                assert!(
                    got == bullet,
                    "{what}, code {code:#04X}, recovery {recovering}: not the bullet"
                );
            }
        }
    }
}

/// The text of an unused code is the bullet, U+2022, through the
/// encoding's glyph name — in strict and recovery mode.
#[test]
fn unused_codes_read_as_the_bullet() {
    for font in [helvetica(Some("/WinAnsiEncoding")), embedded_sans()] {
        for recovering in [false, true] {
            for code in UNUSED {
                let got = chars(&pdf(&[code], font.clone()), recovering);
                assert_eq!(
                    got[0],
                    (Some("\u{2022}".to_string()), ResolveKind::EncodingFallback),
                    "code {code:#04X}, recovery {recovering}"
                );
                assert_eq!(
                    text_in(&pdf(&[code], font.clone()), recovering),
                    "\u{2022}A"
                );
            }
        }
    }
}

/// The control codes (0x20 and below are not "greater than 40") stay
/// unused: nothing is drawn for them, as before.
#[test]
fn control_codes_draw_nothing() {
    let font = helvetica(Some("/WinAnsiEncoding"));
    let a_only = pixels(&pdf(&[], font.clone()), false);
    for code in [0x01, 0x1F] {
        let got = pixels(&pdf(&[code], font.clone()), false);
        assert_eq!(ink(&got), ink(&a_only), "code {code:#04X}");
    }
}

/// A name `/Differences` gives an unused code wins over the bullet.
#[test]
fn differences_name_an_unused_code() {
    let named = helvetica(Some(
        "<< /BaseEncoding /WinAnsiEncoding /Differences [129 /A] >>",
    ));
    let a = pixels(&pdf(b"A", helvetica(Some("/WinAnsiEncoding"))), false);
    assert!(pixels(&pdf(&[0x81], named.clone()), false) == a);
    assert_eq!(text(&pdf(&[0x81], named)), "AA");
    // The other unused codes of the same base still draw the bullet.
    let named = helvetica(Some(
        "<< /BaseEncoding /WinAnsiEncoding /Differences [129 /A] >>",
    ));
    let bullet = pixels(&pdf(&[0x95], named.clone()), false);
    assert!(pixels(&pdf(&[0x8D], named), false) == bullet);
}

/// The note is WinAnsiEncoding's: StandardEncoding (named, or the
/// font's own) and MacRomanEncoding leave their undefined codes
/// undefined.
#[test]
fn other_encodings_are_unchanged() {
    for (enc, code) in [
        (Some("/StandardEncoding"), 0x81u8),
        (None, 0x81),
        (Some("/StandardEncoding"), 0x7F),
    ] {
        let font = helvetica(enc);
        let a_only = pixels(&pdf(&[], font.clone()), false);
        let got = pixels(&pdf(&[code], font), false);
        assert_eq!(ink(&got), ink(&a_only), "{enc:?} {code:#04X}");
    }
    // MacRomanEncoding defines 0x81 (Aring): drawn as before, not as a bullet.
    let mac = helvetica(Some("/MacRomanEncoding"));
    let bullet = pixels(&pdf(&[0x95], helvetica(Some("/WinAnsiEncoding"))), false);
    assert!(pixels(&pdf(&[0x81], mac.clone()), false) != bullet);
    assert_eq!(text(&pdf(&[0x81], mac)), "\u{C5}A");
}

/// A `/ToUnicode` entry still wins over the encoding's bullet.
#[test]
fn to_unicode_wins() {
    let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CMapName /T def 1 begincodespacerange <00> <FF> endcodespacerange \
        1 beginbfchar <81> <0058> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end";
    let mut font = embedded_sans_with(32, "/ToUnicode 8 0 R");
    font.push((8, stream_obj("", cmap)));
    let got = chars(&pdf(&[0x81], font), false);
    assert_eq!(got[0], (Some("X".to_string()), ResolveKind::ToUnicode));
}

/// A symbolic TrueType font with an explicit WinAnsiEncoding takes the
/// names first (Table 114): the unused codes are its bullet as well.
#[test]
fn a_symbolic_truetype_font_names_the_bullet_too() {
    let font = embedded_sans_with(4, "");
    let bullet = pixels(&pdf(&[0x95], font.clone()), false);
    assert!(ink(&bullet) > ink(&pixels(&pdf(&[], font.clone()), false)));
    for code in UNUSED {
        assert!(
            pixels(&pdf(&[code], font.clone()), false) == bullet,
            "code {code:#04X}"
        );
    }
}

/// A Type 3 font on a WinAnsiEncoding base draws its `/bullet`
/// procedure for the unused codes.
#[test]
fn a_type3_font_draws_its_bullet_procedure() {
    let mut widths = vec!["1000"; 256];
    widths[0x41] = "1000";
    let widths = widths.join(" ");
    let font = vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] /FirstChar 0 /LastChar 255 /Widths [{widths}] \
                 /Encoding << /BaseEncoding /WinAnsiEncoding >> /CharProcs << /A 8 0 R /bullet 9 0 R >> \
                 /Resources << >> >>"
            )
            .into_bytes(),
        ),
        (8, stream_obj("", b"1000 0 0 0 1000 1000 d1 0 0 400 1000 re f")),
        (9, stream_obj("", b"1000 0 0 0 1000 1000 d1 300 300 400 400 re f")),
    ];
    let bullet = pixels(&pdf(&[0x95], font.clone()), false);
    assert!(ink(&bullet) > ink(&pixels(&pdf(&[], font.clone()), false)));
    for code in UNUSED {
        assert!(
            pixels(&pdf(&[code], font.clone()), false) == bullet,
            "code {code:#04X}"
        );
    }
}

/// An embedded program without a `bullet` glyph (a CFF subset of A–f)
/// draws `.notdef` for the unused codes, as before — nothing — while the
/// text follows the encoding, as for any name a subset lacks.
#[test]
fn a_program_without_a_bullet_draws_nothing_for_them() {
    let widths = vec!["600"; 224].join(" ");
    let font = vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /PAXEKO+KasaneSansPro-Bold \
                 /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths [{widths}] \
                 /FontDescriptor 6 0 R >>"
            )
            .into_bytes(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /PAXEKO+KasaneSansPro-Bold /Flags 32 \
              /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile3 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj("/Subtype /Type1C", CFF)),
    ];
    let space = pixels(&pdf(b" ", font.clone()), false);
    for code in UNUSED {
        assert!(
            pixels(&pdf(&[code], font.clone()), false) == space,
            "code {code:#04X}"
        );
        assert_eq!(
            text(&pdf(&[code], font.clone())),
            "\u{2022}A",
            "code {code:#04X}"
        );
    }
}

/// 0xA0 and 0xAD are WinAnsiEncoding's space and hyphen, not unused.
#[test]
fn nbsp_and_hyphen_are_not_bullets() {
    let font = helvetica(Some("/WinAnsiEncoding"));
    let draw = |b: &[u8]| pixels(&pdf(b, font.clone()), false);
    assert!(draw(&[0xA0]) == draw(b" "));
    assert!(draw(&[0xAD]) == draw(b"-"));
    assert!(draw(&[0xAD]) != draw(&[0x95]));
}

/// Redaction removes an unused code drawn as a bullet like any glyph:
/// a rectangle over the bullet takes it, the text after it stays, and
/// the output reads without it.
#[test]
fn a_bullet_from_an_unused_code_is_redacted() {
    let doc = Document::from_bytes(&pdf(&[0x81], embedded_sans())).unwrap();
    // 24 pt at (10, 12): the bullet's 600-unit cell is x 10..24.4.
    let r = redact(
        &doc,
        &[(0, vec![(9.0, 8.0, 23.0, 36.0)])],
        &RedactOptions::new(),
    )
    .unwrap();
    assert_eq!(r.codes_removed, 1);
    assert_eq!(text(&r.output), "A");
}
