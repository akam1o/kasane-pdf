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
use kasane_pdf::{extract_text, render_page, Document, OpenOptions, RenderOptions, TextOptions};

const UNUSED: [u8; 6] = [0x7F, 0x81, 0x8D, 0x8F, 0x90, 0x9D];
const SANS: &[u8] = include_bytes!("../fonts/liberation/LiberationSans-Regular.ttf");

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
    let widths = vec!["600"; 224].join(" ");
    vec![
        (
            5,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans \
                 /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths [{widths}] \
                 /FontDescriptor 6 0 R >>"
            )
            .into_bytes(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-203 -303 1050 910] /ItalicAngle 0 /Ascent 905 /Descent -212 \
              /CapHeight 729 /StemV 80 /FontFile2 7 0 R >>"
                .to_vec(),
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
    let d = Document::from_bytes(pdf).unwrap();
    extract_text(&d, &d.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
        .into_iter()
        .map(|c| c.unicode.unwrap_or_else(|| "\u{FFFD}".into()))
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

/// The text of an unused code is the bullet, U+2022.
#[test]
fn unused_codes_read_as_the_bullet() {
    for font in [helvetica(Some("/WinAnsiEncoding")), embedded_sans()] {
        for code in UNUSED {
            assert_eq!(
                text(&pdf(&[code], font.clone())),
                "\u{2022}A",
                "code {code:#04X}"
            );
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
