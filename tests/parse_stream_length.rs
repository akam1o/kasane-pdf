//! Stream capture with an *indirect* `/Length` (§7.3.8.2): the length
//! object is resolved and trusted when `endstream` follows, so data
//! whose last byte is CR or LF is not clipped by the `endstream` scan;
//! an unresolvable or wrong indirect length falls back to the scan,
//! and a self-referential length cannot recurse.

mod common;

use common::build_pdf;
use kasane_pdf::{Document, IndirectRef};

fn data_of(pdf: &[u8], num: u32) -> Vec<u8> {
    let doc = Document::from_bytes(pdf).unwrap();
    let o = doc
        .resolve(IndirectRef {
            number: num,
            generation: 0,
        })
        .unwrap();
    o.as_stream().unwrap().1.to_vec()
}

fn page(stream_obj: &[u8], extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj.to_vec()),
    ];
    objs.extend(extra.iter().cloned());
    build_pdf(&objs)
}

#[test]
fn indirect_length_keeps_a_trailing_cr_in_the_data() {
    // Data is "AB\r" (3 bytes) followed by the "\n" EOL and endstream.
    let pdf = page(
        b"<< /Length 6 0 R >>\nstream\nAB\r\nendstream",
        &[(6, b"3".to_vec())],
    );
    assert_eq!(data_of(&pdf, 4), b"AB\r");
    // The same bytes with a direct /Length behave identically.
    let pdf = page(b"<< /Length 3 >>\nstream\nAB\r\nendstream", &[]);
    assert_eq!(data_of(&pdf, 4), b"AB\r");
}

#[test]
fn wrong_or_unresolvable_indirect_length_falls_back_to_the_scan() {
    // Length says 99: endstream does not follow → scan, EOL stripped.
    let pdf = page(
        b"<< /Length 6 0 R >>\nstream\nAB\r\nendstream",
        &[(6, b"99".to_vec())],
    );
    assert_eq!(data_of(&pdf, 4), b"AB");
    // Length object missing → scan.
    let pdf = page(b"<< /Length 6 0 R >>\nstream\nABCD\nendstream", &[]);
    assert_eq!(data_of(&pdf, 4), b"ABCD");
    // Length is not an integer → scan.
    let pdf = page(
        b"<< /Length 6 0 R >>\nstream\nABCD\nendstream",
        &[(6, b"(x)".to_vec())],
    );
    assert_eq!(data_of(&pdf, 4), b"ABCD");
}

#[test]
fn self_referential_length_does_not_recurse() {
    // /Length points at the stream itself, and at another stream whose
    // /Length points back: both resolve to nothing and the scan wins.
    let pdf = page(b"<< /Length 4 0 R >>\nstream\nABCD\nendstream", &[]);
    assert_eq!(data_of(&pdf, 4), b"ABCD");
    let pdf = page(
        b"<< /Length 6 0 R >>\nstream\nABCD\nendstream",
        &[(6, b"<< /Length 4 0 R >>\nstream\nWXYZ\nendstream".to_vec())],
    );
    assert_eq!(data_of(&pdf, 4), b"ABCD");
    assert_eq!(data_of(&pdf, 6), b"WXYZ");
}

#[test]
fn empty_stream_without_length_is_empty_not_a_panic() {
    // Found by fuzzing: with no usable /Length the scan finds
    // `endstream` right at the start of the data, and backing off "the
    // EOL before endstream" stepped onto the EOL of the `stream` line —
    // an end before the start, which panicked (0.1.0 – 0.2.1).
    for (body, want) in [
        (&b"<< >>\nstream\nendstream"[..], &b""[..]),
        (b"<< >>\nstream\r\nendstream", b""),
        (b"<< /Length 99 >>\nstream\nendstream", b""),
        (b"<< >>\nstream\n\nendstream", b""),
        (b"<< >>\nstream\r\n\r\nendstream", b""),
        (b"<< >>\nstream\nq Q\nendstream", b"q Q"),
    ] {
        let pdf = page(body, &[]);
        assert_eq!(
            data_of(&pdf, 4),
            want,
            "{:?}",
            String::from_utf8_lossy(body)
        );
        let doc = Document::from_bytes(&pdf).unwrap();
        doc.page_operations(&doc.pages()[0]).unwrap();
    }
}
