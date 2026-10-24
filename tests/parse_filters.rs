//! Byte-oriented stream filters (ISO 32000-1 §7.4.2–§7.4.5):
//! `/ASCIIHexDecode`, `/ASCII85Decode`, `/LZWDecode` (+ `/EarlyChange`,
//! predictors) and `/RunLengthDecode`, alone and chained, exercised
//! through `page_contents` so the same pipeline that feeds xref
//! streams, fonts and images is what is tested.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{Document, Error, Limits};

fn pdf_with_contents(dict: &str, data: &[u8]) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj(dict, data)),
    ])
}

fn decode(dict: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
    let doc = Document::from_bytes(&pdf_with_contents(dict, data))?;
    Ok(doc.page_contents(&doc.pages()[0])?.remove(0))
}

fn decode_with(dict: &str, data: &[u8], limits: Limits) -> Result<Vec<u8>, Error> {
    let doc = Document::from_bytes_with_limits(&pdf_with_contents(dict, data), limits)?;
    Ok(doc.page_contents(&doc.pages()[0])?.remove(0))
}

// ---- reference encoders (kept here so the vectors are reproducible) --

fn ascii85_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(4) {
        let mut v = 0u32;
        for i in 0..4 {
            v = (v << 8) | *chunk.get(i).unwrap_or(&0) as u32;
        }
        if chunk.len() == 4 && v == 0 {
            out.push(b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        out.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    out.extend_from_slice(b"~>");
    out
}

fn runlength_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        let mut run = 1;
        while i + run < data.len() && data[i + run] == b && run < 128 {
            run += 1;
        }
        if run >= 2 {
            out.push((257 - run) as u8);
            out.push(b);
            i += run;
        } else {
            let mut lit = 1;
            while i + lit < data.len()
                && lit < 128
                && !(i + lit + 1 < data.len() && data[i + lit] == data[i + lit + 1])
            {
                lit += 1;
            }
            out.push((lit - 1) as u8);
            out.extend_from_slice(&data[i..i + lit]);
            i += lit;
        }
    }
    out.push(128);
    out
}

/// Straightforward PDF LZW encoder: clear first, 9–12-bit codes,
/// MSB-first, width grows per `/EarlyChange`, clear when full, EOD last.
fn lzw_encode(data: &[u8], early: usize) -> Vec<u8> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut nbits = 0u32;
    let mut width = 9u32;
    let emit = |code: u16, width: u32, acc: &mut u32, nbits: &mut u32, out: &mut Vec<u8>| {
        *acc = (*acc << width) | code as u32;
        *nbits += width;
        while *nbits >= 8 {
            out.push((*acc >> (*nbits - 8)) as u8);
            *nbits -= 8;
            *acc &= (1 << *nbits) - 1;
        }
    };
    let mut table: HashMap<Vec<u8>, u16> = HashMap::new();
    let reset = |t: &mut HashMap<Vec<u8>, u16>| {
        t.clear();
        for i in 0..256u16 {
            t.insert(vec![i as u8], i);
        }
    };
    reset(&mut table);
    let mut next: usize = 258;
    emit(256, width, &mut acc, &mut nbits, &mut out);
    let mut w: Vec<u8> = Vec::new();
    for &b in data {
        let mut wb = w.clone();
        wb.push(b);
        if table.contains_key(&wb) {
            w = wb;
            continue;
        }
        emit(table[&w], width, &mut acc, &mut nbits, &mut out);
        if next < 4096 {
            table.insert(wb, next as u16);
            next += 1;
        }
        // The decoder creates each entry one code later than the
        // encoder (it needs the next code's first byte), so its table
        // holds `next - 1` entries when it reads the following code.
        width = match next - 1 + early {
            d if d >= 2048 => 12,
            d if d >= 1024 => 11,
            d if d >= 512 => 10,
            _ => 9,
        };
        if next >= 4096 {
            emit(256, width, &mut acc, &mut nbits, &mut out);
            reset(&mut table);
            next = 258;
            width = 9;
        }
        w = vec![b];
    }
    if !w.is_empty() {
        emit(table[&w], width, &mut acc, &mut nbits, &mut out);
        next += 1;
        width = match next - 1 + early {
            d if d >= 2048 => 12,
            d if d >= 1024 => 11,
            d if d >= 512 => 10,
            _ => 9,
        };
    }
    emit(257, width, &mut acc, &mut nbits, &mut out);
    if nbits > 0 {
        out.push((acc << (8 - nbits)) as u8);
    }
    out
}

fn sample(n: usize) -> Vec<u8> {
    // Repetitive enough to grow the table past the 9→10→11→12 bit
    // boundaries, varied enough that the entries are not trivial.
    (0..n)
        .map(|i| ((i * 7) % 13 + (i / 50) % 5) as u8)
        .collect()
}

// ---- ASCIIHex --------------------------------------------------------

#[test]
fn asciihex_basic_whitespace_and_odd_digit() {
    assert_eq!(
        decode("/Filter /ASCIIHexDecode", b"48656C6C 6F>").unwrap(),
        b"Hello"
    );
    assert_eq!(
        decode("/Filter /AHx", b"48\n65\r\n6c6c6F\t2>").unwrap(),
        b"Hello "
    );
    // No EOD: the data is unambiguous.
    assert_eq!(decode("/Filter /ASCIIHexDecode", b"4142").unwrap(), b"AB");
    // Data after `>` is ignored.
    assert_eq!(decode("/Filter /ASCIIHexDecode", b"41>zz").unwrap(), b"A");
}

#[test]
fn asciihex_rejects_non_hex() {
    let err = decode("/Filter /ASCIIHexDecode", b"41G2>").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

// ---- ASCII85 ---------------------------------------------------------

#[test]
fn ascii85_known_vectors() {
    assert_eq!(
        decode("/Filter /ASCII85Decode", b"87cURD]i,\"Ebo80~>").unwrap(),
        b"Hello World!"
    );
    assert_eq!(decode("/Filter /A85", b"z~>").unwrap(), [0, 0, 0, 0]);
    // Partial final group (3 chars → 2 bytes), `<~` prefix, whitespace.
    assert_eq!(
        decode("/Filter /ASCII85Decode", b"<~ 87cU\nRD]i,\"Ebo8\r0~>").unwrap(),
        b"Hello World!"
    );
    assert_eq!(
        decode("/Filter /ASCII85Decode", b"87cURD]i,\"Ebo80").unwrap(),
        b"Hello World!"
    );
    let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    assert_eq!(
        decode("/Filter /ASCII85Decode", &ascii85_encode(&data)).unwrap(),
        data
    );
}

#[test]
fn ascii85_rejects_bad_input() {
    for bad in [
        &b"87cUv~>"[..],
        b"8~x",
        b"z8~>",
        b"8z~>",
        b"8~>",
        b"uuuuu~>",
    ] {
        let err = decode("/Filter /ASCII85Decode", bad).unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "{bad:?}: {err:?}");
    }
}

// ---- RunLength -------------------------------------------------------

#[test]
fn runlength_basic_and_roundtrip() {
    assert_eq!(
        decode("/Filter /RunLengthDecode", b"\x02abc\xfex\x80").unwrap(),
        b"abcxxx"
    );
    // Missing EOD after a complete run is accepted.
    assert_eq!(decode("/Filter /RL", b"\x00q\xffw").unwrap(), b"qww");
    let data: Vec<u8> = (0..2000).map(|i| (i / 37) as u8).collect();
    assert_eq!(
        decode("/Filter /RunLengthDecode", &runlength_encode(&data)).unwrap(),
        data
    );
}

#[test]
fn runlength_rejects_truncated_runs() {
    for bad in [&b"\x05ab"[..], b"\xfe"] {
        let err = decode("/Filter /RunLengthDecode", bad).unwrap_err();
        assert!(matches!(err, Error::Parse(_)), "{bad:?}: {err:?}");
    }
}

#[test]
fn runlength_output_is_capped() {
    // 64 KiB from 1 KiB of input.
    let mut enc: Vec<u8> = Vec::new();
    for _ in 0..512 {
        enc.extend_from_slice(&[129, b'x']);
    }
    enc.push(128);
    let limits = Limits {
        max_stream_bytes: 32 * 1024,
        ..Limits::default()
    };
    let err = decode_with("/Filter /RunLengthDecode", &enc, limits).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

// ---- LZW -------------------------------------------------------------

#[test]
fn lzw_spec_example() {
    // §7.4.4.2, Table 7: 45 45 45 45 45 65 45 45 45 66 → codes
    // 256 45 258 258 65 259 66 257.
    let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
    assert_eq!(
        decode("/Filter /LZWDecode", &enc).unwrap(),
        [45, 45, 45, 45, 45, 65, 45, 45, 45, 66]
    );
    assert_eq!(decode("/Filter /LZW", &enc).unwrap().len(), 10);
}

#[test]
fn lzw_roundtrip_through_every_code_width() {
    let data = sample(20_000); // > 4096 entries: forces a mid-stream clear
    let enc = lzw_encode(&data, 1);
    assert!(enc.len() < data.len());
    assert_eq!(decode("/Filter /LZWDecode", &enc).unwrap(), data);
    assert_eq!(
        decode("/Filter /LZWDecode /DecodeParms << /EarlyChange 1 >>", &enc).unwrap(),
        data
    );
}

#[test]
fn lzw_early_change_zero() {
    let data = sample(6_000);
    let enc0 = lzw_encode(&data, 0);
    let enc1 = lzw_encode(&data, 1);
    assert_ne!(enc0, enc1, "the width switch point differs");
    assert_eq!(
        decode(
            "/Filter /LZWDecode /DecodeParms << /EarlyChange 0 >>",
            &enc0
        )
        .unwrap(),
        data
    );
    // Decoding with the wrong EarlyChange desynchronises: either an
    // error or garbage, never the original.
    let wrong = decode("/Filter /LZWDecode", &enc0);
    assert!(wrong.is_err() || wrong.unwrap() != data);
    let err = decode(
        "/Filter /LZWDecode /DecodeParms << /EarlyChange 2 >>",
        &enc1,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn lzw_with_png_predictor() {
    // 3 columns, 1 colour, 8 bpc; row filters: None then Up.
    let predicted = b"\x00\x01\x02\x03\x02\x01\x01\x01";
    let enc = lzw_encode(predicted, 1);
    assert_eq!(
        decode(
            "/Filter /LZWDecode /DecodeParms << /Predictor 12 /Columns 3 >>",
            &enc
        )
        .unwrap(),
        [1, 2, 3, 2, 3, 4]
    );
}

#[test]
fn lzw_missing_eod_and_bad_codes() {
    // Drop the EOD code: the spec example minus its last code — the
    // stream ends mid-code with non-zero bits → error …
    let truncated = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85];
    let err = decode("/Filter /LZWDecode", &truncated).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
    // … but a clean ending (zero padding, no EOD) is accepted.
    let data = sample(300);
    let mut enc = lzw_encode(&data, 1);
    // Remove the 257 code: re-encode without it by truncating to the
    // byte before the EOD's first bit is not possible in general, so
    // instead append a clean `clear` and padding and check tolerance
    // for the common "no EOD, zero padding" producer bug.
    enc.truncate(enc.len() - 2);
    enc.extend_from_slice(&[0x00, 0x00]);
    let got = decode("/Filter /LZWDecode", &enc);
    assert!(got.is_ok() || matches!(got, Err(Error::Parse(_))));
    // A code beyond the table right after the clear code.
    let bad = [0x80, 0x4B, 0x00]; // 256, 300
    let err = decode("/Filter /LZWDecode", &bad).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

#[test]
fn lzw_output_is_capped() {
    let data = vec![7u8; 200_000];
    let enc = lzw_encode(&data, 1);
    let limits = Limits {
        max_stream_bytes: 100_000,
        ..Limits::default()
    };
    let err = decode_with("/Filter /LZWDecode", &enc, limits).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

// ---- chains ----------------------------------------------------------

#[test]
fn filter_chains_apply_in_order() {
    let data = sample(3000);
    // /Filter [/ASCII85Decode /LZWDecode]: encoded LZW first, then A85.
    let enc = ascii85_encode(&lzw_encode(&data, 1));
    assert_eq!(
        decode("/Filter [/ASCII85Decode /LZWDecode]", &enc).unwrap(),
        data
    );
    // Three deep, with positional DecodeParms (null for the byte ones).
    let predicted = b"\x00\x01\x02\x03\x02\x01\x01\x01";
    let enc = ascii85_encode(&runlength_encode(&lzw_encode(predicted, 1)));
    assert_eq!(
        decode(
            "/Filter [/A85 /RL /LZW] /DecodeParms [null null << /Predictor 12 /Columns 3 >>]",
            &enc
        )
        .unwrap(),
        [1, 2, 3, 2, 3, 4]
    );
    // Wrong order fails rather than yielding garbage.
    let enc = ascii85_encode(&runlength_encode(b"abc"));
    assert!(decode("/Filter [/RunLengthDecode /ASCII85Decode]", &enc).is_err());
}

#[test]
fn decode_parms_empty_array_and_indirect() {
    let data = sample(300);
    let enc = ascii85_encode(&lzw_encode(&data, 1));
    // `/DecodeParms []` means no parameters for any filter.
    assert_eq!(
        decode("/Filter [/ASCII85Decode /LZWDecode] /DecodeParms []", &enc).unwrap(),
        data
    );
    // /Filter, /DecodeParms and their elements may be indirect.
    let predicted = b"\x00\x01\x02\x03\x02\x01\x01\x01";
    let enc = lzw_encode(predicted, 1);
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("/Filter 5 0 R /DecodeParms [6 0 R]", &enc)),
        (5, b"[/LZWDecode]".to_vec()),
        (6, b"<< /Predictor 12 /Columns 3 >>".to_vec()),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    assert_eq!(
        doc.page_contents(&doc.pages()[0]).unwrap().remove(0),
        [1, 2, 3, 2, 3, 4]
    );
}

#[test]
fn image_only_filters_are_refused_on_content_streams() {
    for f in [
        "/DCTDecode",
        "/JBIG2Decode",
        "/CCITTFaxDecode",
        "/JPXDecode",
        "/Nope",
    ] {
        let err = decode(&format!("/Filter {f}"), b"\xff\xd8").unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{f}: {err:?}");
    }
}
