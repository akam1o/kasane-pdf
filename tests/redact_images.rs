//! `redact` on images stored at 1, 2 or 4 bits per component, through
//! filter chains, and in CCITT or JBIG2 (0.9). The samples inside the
//! rects become 0 and the image is written back raw at its own depth;
//! the samples outside are kept bit for bit. Two images differing only
//! inside the rect redact to the same bytes. A JBIG2 image with shared
//! `/JBIG2Globals` is refused.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{redact, Document, Object, RedactOptions};

/// The page draws the image over x 40..120, y 90..100 (half outside
/// the rect 60..140 on the left).
const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);
const DRAW: &str = "q 80 0 0 10 40 90 cm /Im Do Q";

fn page(image: Vec<u8>) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", DRAW.as_bytes())),
        (5, image),
    ])
}

fn run(pdf: &[u8]) -> Result<kasane_pdf::Redaction, kasane_pdf::Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(&doc, &[(0, vec![RECT])], &RedactOptions::new())
}

/// The output's image (the page names one, the edited copy): its
/// dictionary and stored samples.
fn out_image(out: &[u8]) -> (kasane_pdf::Dict, Vec<u8>) {
    let doc = Document::from_bytes(out).unwrap();
    let res = doc.pages()[0].resources().cloned().unwrap();
    let res = doc.deref(&res).unwrap();
    let xo = doc
        .deref(res.as_dict().unwrap().get(b"XObject".as_slice()).unwrap())
        .unwrap();
    let first = xo.as_dict().unwrap().values().next().unwrap().clone();
    match doc.deref(&first).unwrap() {
        Object::Stream { dict, data } => (dict, data),
        _ => panic!("not a stream"),
    }
}

/// A `w × h` image of `ncomps` components at `bpc` bits whose samples
/// are `sample(x, y, c)` (masked to `bpc` bits), packed MSB first.
fn packed(w: u32, h: u32, ncomps: u32, bpc: u32, sample: impl Fn(u32, u32, u32) -> u32) -> Vec<u8> {
    let row = (w * ncomps * bpc).div_ceil(8) as usize;
    let mut d = vec![0u8; row * h as usize];
    for y in 0..h {
        for x in 0..w {
            for c in 0..ncomps {
                let v = sample(x, y, c) & ((1 << bpc) - 1);
                let bit = ((x * ncomps + c) * bpc) as usize;
                let i = y as usize * row + bit / 8;
                d[i] |= (v << (8 - bpc as usize - bit % 8)) as u8;
            }
        }
    }
    d
}

fn sample_at(data: &[u8], w: u32, ncomps: u32, bpc: u32, x: u32, y: u32, c: u32) -> u32 {
    let row = (w * ncomps * bpc).div_ceil(8) as usize;
    let bit = ((x * ncomps + c) * bpc) as usize;
    let b = data[y as usize * row + bit / 8] as u32;
    (b >> (8 - bpc as usize - bit % 8)) & ((1 << bpc) - 1)
}

/// Low-bit images: the columns inside the rect (x ≥ 60 ↔ column ≥ 2 of
/// 8, each 10 pt) become 0, the others keep every bit; the output keeps the depth
/// and drops the filter; two inputs differing only inside redact alike.
#[test]
fn low_bit_images_are_edited_at_their_depth() {
    for (bpc, cs, n) in [
        (1, "/DeviceGray", 1),
        (2, "/DeviceGray", 1),
        (4, "/DeviceRGB", 3),
    ] {
        let make = |v: u32| {
            let data = packed(8, 2, n, bpc, |x, y, c| {
                if x >= 3 {
                    x * 7 + y * 3 + c + v * 5
                } else {
                    x * 3 + y + c + 1
                }
            });
            page(stream_obj(
                &format!(
                    "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace {cs} \
                     /BitsPerComponent {bpc}"
                ),
                &data,
            ))
        };
        let a = run(&make(1)).unwrap_or_else(|e| panic!("{bpc} bits: {e}"));
        let b = run(&make(2)).unwrap();
        assert!(
            a.output == b.output,
            "{bpc} bits: samples inside reached the output"
        );
        let (dict, data) = out_image(&a.output);
        assert_eq!(
            dict.get(b"BitsPerComponent".as_slice())
                .and_then(|o| o.as_i64()),
            Some(bpc as i64)
        );
        assert!(!dict.contains_key(b"Filter".as_slice()));
        for y in 0..2 {
            for x in 0..8 {
                for c in 0..n {
                    let got = sample_at(&data, 8, n, bpc, x, y, c);
                    if x >= 2 {
                        assert_eq!(got, 0, "{bpc} bits: ({x},{y},{c}) inside not zeroed");
                    } else {
                        let want = (x * 3 + y + c + 1) & ((1 << bpc) - 1);
                        assert_eq!(got, want, "{bpc} bits: ({x},{y},{c}) outside changed");
                    }
                }
            }
        }
    }
}

/// A CCITT Group 4 image (8 × 2) and an embedded JBIG2 image without
/// globals: decoded, edited, written raw at 1 bit.
#[test]
fn ccitt_and_jbig2_images() {
    const G4: &[u8] = &[0x36, 0xF0, 0x01, 0x00, 0x10];
    let ccitt = stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
         /BitsPerComponent 1 /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 8 /Rows 2 >>",
        G4,
    );
    let r = run(&page(ccitt)).unwrap();
    let (dict, data) = out_image(&r.output);
    assert!(!dict.contains_key(b"Filter".as_slice()));
    assert_eq!(data.len(), 2);
    for y in 0..2 {
        for x in 2..8 {
            assert_eq!(sample_at(&data, 8, 1, 1, x, y, 0), 0);
        }
    }
    // The same image as an embedded JBIG2 stream (page information and
    // an immediate MMR generic region): columns 0 and 1 (outside) keep
    // their white samples, the rest become 0; with a file header it is
    // refused (readers disagree on it).
    let seg = |num: u32, kind: u8, data: &[u8]| {
        let mut s = num.to_be_bytes().to_vec();
        s.extend_from_slice(&[kind, 0, 1]);
        s.extend_from_slice(&(data.len() as u32).to_be_bytes());
        s.extend_from_slice(data);
        s
    };
    let mut info = Vec::new();
    for v in [8u32, 2, 0, 0] {
        info.extend_from_slice(&v.to_be_bytes());
    }
    info.extend_from_slice(&[0, 0, 0]);
    let mut region = Vec::new();
    for v in [8u32, 2, 0, 0] {
        region.extend_from_slice(&v.to_be_bytes());
    }
    region.extend_from_slice(&[0, 1]);
    region.extend_from_slice(G4);
    let mut jb = seg(0, 48, &info);
    jb.extend_from_slice(&seg(1, 38, &region));
    let jbig2 = |data: &[u8]| {
        stream_obj(
            "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
             /BitsPerComponent 1 /Filter /JBIG2Decode",
            data,
        )
    };
    let r = run(&page(jbig2(&jb))).unwrap();
    let (dict, data) = out_image(&r.output);
    assert!(!dict.contains_key(b"Filter".as_slice()));
    for y in 0..2 {
        for x in 0..8 {
            let want = u32::from(x < 2);
            assert_eq!(sample_at(&data, 8, 1, 1, x, y, 0), want, "JBIG2 ({x},{y})");
        }
    }
    let mut with_header = b"\x97JB2\r\n\x1a\n\x01\x00\x00\x00\x01".to_vec();
    with_header.extend_from_slice(&jb);
    assert!(run(&page(jbig2(&with_header))).is_err());
    // JBIG2 with shared globals: refused (the symbol dictionary other
    // images use stays as it is).
    let jb = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", DRAW.as_bytes())),
        (
            5,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
                 /BitsPerComponent 1 /Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 6 0 R >>",
                b"\x00",
            ),
        ),
        (6, stream_obj("", b"\x00")),
    ]);
    let e = run(&jb).unwrap_err().to_string();
    assert!(e.contains("JBIG2Decode"), "{e}");
}

fn hex(b: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = b
        .iter()
        .flat_map(|x| format!("{x:02x}").into_bytes())
        .collect();
    s.push(b'>');
    s
}

fn deflate(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

/// Filter chains: ASCIIHex over Flate, ASCII85 alone (8-bit), ASCIIHex
/// over a JPEG.
#[test]
fn filter_chains() {
    let samples: Vec<u8> = (0..16).map(|i| 10 + i as u8).collect();
    let flate = stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
         /BitsPerComponent 8 /Filter [/ASCIIHexDecode /FlateDecode]",
        &hex(&deflate(&samples)),
    );
    let r = run(&page(flate)).unwrap();
    let (dict, data) = out_image(&r.output);
    assert!(!dict.contains_key(b"Filter".as_slice()));
    assert_eq!(&data[..2], &samples[..2], "outside kept");
    assert!(data[2..8].iter().all(|b| *b == 0), "inside zeroed");
    // ASCII85 of the raw samples.
    let a85 = {
        let mut out = Vec::new();
        for chunk in samples.chunks(4) {
            let v = u32::from_be_bytes(chunk.try_into().unwrap());
            let mut d = [0u8; 5];
            let mut x = v;
            for i in (0..5).rev() {
                d[i] = b'!' + (x % 85) as u8;
                x /= 85;
            }
            out.extend_from_slice(&d);
        }
        out.extend_from_slice(b"~>");
        out
    };
    let r = run(&page(stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 2 /ColorSpace /DeviceGray \
         /BitsPerComponent 8 /Filter /ASCII85Decode",
        &a85,
    )))
    .unwrap();
    let (_, data) = out_image(&r.output);
    assert_eq!(&data[..2], &samples[..2]);
    // ASCIIHex over an 8 × 8 grey JPEG, drawn half inside.
    let jpeg: Vec<u8> = include_str!("fixtures/jpeg_g8.hex")
        .trim()
        .as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    let r = run(&page(stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
         /BitsPerComponent 8 /Filter [/ASCIIHexDecode /DCTDecode]",
        &hex(&jpeg),
    )))
    .unwrap();
    assert!(r.pixels_changed > 0);
    let (dict, data) = out_image(&r.output);
    assert!(!dict.contains_key(b"Filter".as_slice()));
    for y in 0..8 {
        assert!(
            data[y * 8 + 2..y * 8 + 8].iter().all(|b| *b == 0),
            "row {y}"
        );
    }
}
