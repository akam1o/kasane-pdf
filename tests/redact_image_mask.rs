//! `redact` of image masks (`/ImageMask true`, §8.9.6.2): a stencil
//! paints the colour in force where its samples say so; inside the rects
//! its samples are set to the value that paints nothing (1 with the
//! default `/Decode [0 1]`, 0 with `[1 0]`), in a new image object, the
//! rest of the mask, the colour and the original's other uses kept. What
//! cannot be edited exactly refuses.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{redact, Document, ElsewhereLocation, Error, Limits, RedactOptions, Removed};

/// 16 × 16 samples, one bit each (2 bytes a row): a row pattern that
/// occurs nowhere else; `painted(bit)` says whether a sample paints.
fn mask_bits(paint_bit: u8) -> Vec<u8> {
    (0..16u32)
        .flat_map(|y| {
            // Every sample paints but a diagonal; the pattern is the
            // secret (its rows differ).
            let row: u16 = !(1u16 << (y % 16));
            let row = if paint_bit == 1 { row } else { !row };
            row.to_be_bytes()
        })
        .collect()
}

fn mask(extra: &str, data: &[u8]) -> Vec<u8> {
    stream_obj(
        &format!("/Type /XObject /Subtype /Image /Width 16 /Height 16 /ImageMask true {extra}"),
        data,
    )
}

/// One 200 × 200 page drawing `content` with /M (object 5) and /Fm
/// (object 6, when given).
fn page(content: &str, m: Vec<u8>, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /M 5 0 R /Fm 6 0 R >> /Pattern << /P 7 0 R >> >> \
              /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (5, m),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

fn run(pdf: &[u8], rect: (f64, f64, f64, f64)) -> Result<kasane_pdf::Redaction, Error> {
    redact(
        &Document::from_bytes(pdf).unwrap(),
        &[(0, vec![rect])],
        &RedactOptions::new(),
    )
}

/// RGB at page point (x, y) (72 dpi, 200 pt page).
fn rgb(pdf: &[u8], x: u32, y: u32) -> (u8, u8, u8) {
    let doc = Document::from_bytes(pdf).unwrap();
    let p = kasane_pdf::render_page(&doc, &doc.pages()[0], &kasane_pdf::RenderOptions::default())
        .unwrap();
    let c = p.pixmap().pixel(x, 200 - y).unwrap();
    (c.red(), c.green(), c.blue())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The image streams of the output: the data of every image (written
/// without a filter here).
fn images(pdf: &[u8]) -> Vec<Vec<u8>> {
    let doc = Document::from_bytes(pdf).unwrap();
    let mut out = Vec::new();
    for n in 1..2000u32 {
        let Ok(o) = doc.resolve(kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        }) else {
            continue;
        };
        if let Some((d, raw)) = o.as_stream() {
            if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Image") {
                assert!(!d.contains_key(b"Filter".as_slice()));
                out.push(raw.to_vec());
            }
        }
    }
    out
}

const DRAW: &str = "q 100 0 0 100 50 50 cm /M Do Q";
/// The right half of the mask: page x 100..150, the whole height.
const RIGHT: (f64, f64, f64, f64) = (100.0, 0.0, 200.0, 200.0);

fn unpainted_right_half(out: &[u8], paint_bit: u8) {
    let imgs = images(out);
    assert_eq!(imgs.len(), 1, "the original is not carried");
    let data = &imgs[0];
    for y in 0..16usize {
        let row = u16::from_be_bytes([data[2 * y], data[2 * y + 1]]);
        let orig =
            u16::from_be_bytes([mask_bits(paint_bit)[2 * y], mask_bits(paint_bit)[2 * y + 1]]);
        // Columns 8..16 (the low byte) paint nothing; 0..8 as they were.
        let right = row & 0x00FF;
        assert_eq!(right, if paint_bit == 1 { 0 } else { 0x00FF }, "row {y}");
        assert_eq!(
            row & 0xFF00,
            orig & 0xFF00,
            "row {y}: the left half is kept"
        );
    }
}

#[test]
fn default_decode_samples_inside_paint_nothing() {
    let pdf = page(&format!("0 0 0 rg {DRAW}"), mask("", &mask_bits(0)), vec![]);
    let r = run(&pdf, RIGHT).unwrap();
    assert_eq!(r.images_edited, 1);
    assert!(r.pixels_changed > 0);
    unpainted_right_half(&r.output, 0);
    assert!(!contains(&r.output, &mask_bits(0)));
    // Rendered: white inside the rect, the mask's black outside.
    assert_eq!(rgb(&r.output, 125, 100), (255, 255, 255));
    assert_eq!(rgb(&pdf, 125, 101), (0, 0, 0));
    assert_eq!(rgb(&r.output, 75, 101), rgb(&pdf, 75, 101));
}

#[test]
fn inverted_decode_and_the_fill_colour() {
    let pdf = page(
        &format!("1 0 0 rg {DRAW}"),
        mask("/Decode [1 0] /BitsPerComponent 1", &mask_bits(1)),
        vec![],
    );
    let r = run(&pdf, RIGHT).unwrap();
    unpainted_right_half(&r.output, 1);
    assert_eq!(rgb(&r.output, 125, 101), (255, 255, 255));
    assert_eq!(
        rgb(&r.output, 75, 101),
        (255, 0, 0),
        "the colour paints outside"
    );
    // /Decode is kept: it still says which value paints.
    let out = Document::from_bytes(&r.output).unwrap();
    let mut decode_kept = false;
    for n in 1..100u32 {
        if let Ok(o) = out.resolve(kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        }) {
            if let Some((d, _)) = o.as_stream() {
                decode_kept |=
                    d.contains_key(b"Decode".as_slice()) && d.contains_key(b"ImageMask".as_slice());
            }
        }
    }
    assert!(decode_kept);
}

#[test]
fn rotated_and_partially_crossed() {
    // A quarter turn: image x runs down the page.
    let pdf = page(
        "0 g q 0 -100 100 0 50 150 cm /M Do Q",
        mask("", &mask_bits(0)),
        vec![],
    );
    // The lower quarter of the page square 50..150: page y 50..75.
    let r = run(&pdf, (0.0, 0.0, 200.0, 75.0)).unwrap();
    assert_eq!(r.images_edited, 1);
    assert_eq!(rgb(&r.output, 100, 60), (255, 255, 255));
    for (x, y) in [(60, 100), (140, 140), (100, 90)] {
        assert_eq!(rgb(&r.output, x, y), rgb(&pdf, x, y), "({x}, {y}) kept");
    }
    // A rect touching only an edge pixel's border changes nothing there.
    let r = run(&pdf, (0.0, 0.0, 200.0, 50.0)).unwrap();
    assert_eq!(r.images_edited, 0);
}

#[test]
fn away_from_the_rects_the_mask_is_kept() {
    let pdf = page(&format!("0 g {DRAW}"), mask("", &mask_bits(0)), vec![]);
    let r = run(&pdf, (160.0, 160.0, 190.0, 190.0)).unwrap();
    assert_eq!(r.images_edited, 0);
    assert_eq!(images(&r.output), vec![mask_bits(0)]);
}

#[test]
fn shared_and_nested() {
    // The same mask drawn twice: one invocation meets the rect (a copy
    // is edited), the other keeps the original — reported.
    let pdf = page(
        "0 g q 100 0 0 100 50 50 cm /M Do Q q 20 0 0 20 5 5 cm /M Do Q",
        mask("", &mask_bits(0)),
        vec![],
    );
    let r = run(&pdf, RIGHT).unwrap();
    assert_eq!(r.images_edited, 1);
    assert_eq!(images(&r.output).len(), 2);
    assert!(r.elsewhere.iter().any(|e| matches!(
        (&e.removed, &e.location),
        (Removed::Image, ElsewhereLocation::Page(0))
    )));
    assert_eq!(rgb(&r.output, 10, 10), rgb(&pdf, 10, 10));
    // Through a form.
    let pdf = page(
        "0 g /Fm Do",
        mask("", &mask_bits(0)),
        vec![(
            6,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /XObject << /N 5 0 R >> >>",
                b"q 100 0 0 100 50 50 cm /N Do Q",
            ),
        )],
    );
    let r = run(&pdf, RIGHT).unwrap();
    assert_eq!((r.images_edited, r.forms_edited), (1, 1));
    unpainted_right_half(&r.output, 0);
    assert_eq!(rgb(&r.output, 125, 100), (255, 255, 255));
}

#[test]
fn a_clipped_mask_is_edited_where_it_is_drawn() {
    // A clip hides ink, not data: the samples under the rect go even
    // where the clip hides them.
    let pdf = page(
        &format!("0 g 50 50 50 100 re W n {DRAW}"),
        mask("", &mask_bits(0)),
        vec![],
    );
    let r = run(&pdf, RIGHT).unwrap();
    assert_eq!(r.images_edited, 1);
    unpainted_right_half(&r.output, 0);
}

fn refused(pdf: &[u8]) -> String {
    match run(pdf, RIGHT) {
        Err(Error::Unsupported { feature, detail }) => format!("{feature}: {detail}"),
        Err(e) => format!("{e:?}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

#[test]
fn what_is_not_edited_exactly_refuses() {
    let ok = mask_bits(0);
    for (extra, data, want) in [
        ("/Decode [0 0.5]", ok.clone(), "Decode"),
        ("/Decode 9 0 R", ok.clone(), "Decode"),
        ("/BitsPerComponent 8", ok.clone(), "BitsPerComponent"),
        ("/ColorSpace /DeviceGray", ok.clone(), "ColorSpace"),
        ("/SMask 9 0 R", ok.clone(), "SMask"),
        ("/ImageMask 10 0 R", ok.clone(), "ImageMask"),
        // An inline-image abbreviation readers may read instead.
        ("/D [1 0]", ok.clone(), "abbreviation"),
        ("/IM true", ok.clone(), "abbreviation"),
        ("", ok[..20].to_vec(), "length"),
    ] {
        let pdf = page(
            &format!("0 g {DRAW}"),
            mask(extra, &data),
            vec![(9, b"[0 1]".to_vec()), (10, b"true".to_vec())],
        );
        let e = refused(&pdf);
        assert!(e.contains(want), "{extra}: {e}");
    }
    // Painted with a pattern: the pattern inside would only be hidden.
    let pattern = stream_obj(
        "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] \
         /XStep 10 /YStep 10 /Resources << >>",
        b"0 0 5 5 re f",
    );
    let pdf = page(
        &format!("/Pattern cs /P scn {DRAW}"),
        mask("", &ok),
        vec![(7, pattern)],
    );
    let e = refused(&pdf);
    assert!(e.contains("image mask"), "{e}");
    // The image budget.
    let pdf = page(&format!("0 g {DRAW}"), mask("", &ok), vec![]);
    let e = redact(
        &Document::from_bytes(&pdf).unwrap(),
        &[(0, vec![RIGHT])],
        &RedactOptions::new().with_limits(Limits::default().with_max_image_bytes(16)),
    );
    assert!(matches!(e, Err(Error::LimitExceeded { .. })), "{e:?}");
}

/// A width that does not fill its last byte (13 samples, 2 bytes a
/// row): the padding bits are neither edited nor counted.
#[test]
fn rows_with_padding_bits() {
    let data: Vec<u8> = (0..13u8)
        .flat_map(|y| [y.wrapping_mul(37), 0b1010_1000])
        .collect();
    let m = stream_obj(
        "/Type /XObject /Subtype /Image /Width 13 /Height 13 /ImageMask true",
        &data,
    );
    let pdf = page(&format!("0 g {DRAW}"), m, vec![]);
    let r = run(&pdf, RIGHT).unwrap();
    let out = images(&r.output);
    assert_eq!(out.len(), 1);
    for y in 0..13usize {
        let (a, b) = (out[0][2 * y], out[0][2 * y + 1]);
        // Columns 0..6 (page x < 100) kept; 7..13 set to 1; padding kept.
        assert_eq!(a & 0b1111_1100, data[2 * y] & 0b1111_1100, "row {y}");
        assert_eq!(a & 0b0000_0001, 1, "row {y}");
        assert_eq!(b & 0b1111_1000, 0b1111_1000, "row {y}");
        assert_eq!(
            b & 0b0000_0111,
            data[2 * y + 1] & 0b0000_0111,
            "row {y}: padding"
        );
    }
}

/// JBIG2 (1 = black in the file, delivered as sample 0 = paints): the
/// right half of each row painted, then not.
#[test]
fn a_jbig2_mask() {
    let m = stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ImageMask true /Filter /JBIG2Decode",
        &common::jbig2_8x2(),
    );
    let pdf = page(&format!("0 g {DRAW}"), m, vec![]);
    assert_eq!(rgb(&pdf, 125, 101), (0, 0, 0));
    assert_eq!(rgb(&pdf, 75, 101), (255, 255, 255));
    let r = run(&pdf, RIGHT).unwrap();
    assert_eq!(r.images_edited, 1);
    assert_eq!(rgb(&r.output, 125, 101), (255, 255, 255));
    assert_eq!(rgb(&r.output, 75, 101), (255, 255, 255));
}
