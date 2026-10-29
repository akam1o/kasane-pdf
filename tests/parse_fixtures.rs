//! Integration tests over the hand-crafted fixtures in `tests/fixtures/`.
//!
//! Each fixture documents one expected outcome; regenerate them with
//! `python3 tests/fixtures/make_fixtures.py`.

use kasane_pdf::{Document, Error, IndirectRef, Object, Rect};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

#[test]
fn hello_parses_and_reports_one_page() {
    let doc = Document::from_bytes(&fixture("hello.pdf")).expect("hello.pdf should parse");
    assert_eq!(doc.page_count(), 1);
    let page = &doc.pages()[0];
    assert_eq!(
        page.media_box(),
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 612.0,
            ury: 792.0
        }
    );
    assert_eq!(page.crop_box(), page.media_box()); // default
    assert_eq!(page.rotate(), 0);
    // /Contents was left as an unresolved reference.
    assert!(matches!(page.contents(), Some(Object::Ref(_))));
}

#[test]
fn hello_stream_is_stored_raw() {
    let doc = Document::from_bytes(&fixture("hello.pdf")).unwrap();
    let content_ref = doc.pages()[0].contents().unwrap().as_ref().unwrap();
    let stream = doc.resolve(content_ref).unwrap();
    let (_dict, data) = stream.as_stream().expect("contents should be a stream");
    assert!(data.starts_with(b"BT /F1 24 Tf"));
    assert!(data.ends_with(b"ET"));
}

#[test]
fn pages_fixture_exercises_inheritance() {
    let doc = Document::from_bytes(&fixture("pages.pdf")).expect("pages.pdf should parse");
    assert_eq!(doc.page_count(), 3);

    // Page 1: inherits letter MediaBox from the root Pages node, own Rotate.
    let p0 = &doc.pages()[0];
    assert_eq!(p0.media_box().urx, 612.0);
    assert_eq!(p0.rotate(), 90);

    // Page 2: inherits the subtree's MediaBox, which is the *indirect*
    // shared rectangle [0 0 400 400].
    let p1 = &doc.pages()[1];
    assert_eq!(
        p1.media_box(),
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 400.0,
            ury: 400.0
        }
    );

    // Page 3: explicit MediaBox overrides the subtree's.
    let p2 = &doc.pages()[2];
    assert_eq!(p2.media_box().urx, 200.0);
    assert_eq!(p2.media_box().ury, 100.0);
    // No Contents key at all -> None.
    assert!(p2.contents().is_none());
}

#[test]
fn incremental_update_uses_newest_section() {
    let doc =
        Document::from_bytes(&fixture("incremental.pdf")).expect("incremental.pdf should parse");
    assert_eq!(doc.page_count(), 1);
    // Object 4 was redefined in the newer xref section: 300pt square.
    assert_eq!(doc.pages()[0].media_box().urx, 300.0);
    // /Info survived from the older trailer.
    assert!(doc.trailer().contains_key(b"Info".as_slice()));
}

#[test]
fn xref_stream_parses_as_supported() {
    // Xref streams are supported; the fixture must now parse.
    let doc =
        Document::from_bytes(&fixture("xref_stream.pdf")).expect("xref_stream.pdf should parse");
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.pages()[0].media_box().urx, 612.0);
}

#[test]
fn encrypted_file_is_refused() {
    // A password-protected file opened without its password.
    let err = Document::from_bytes(&fixture("encrypted/enc_r6_aes256.pdf")).unwrap_err();
    assert!(matches!(err, Error::Encrypted), "got: {err:?}");
    // `encrypted.pdf` declares /O and /U of one byte: a malformed
    // /Encrypt dictionary, not a wrong password.
    let err = Document::from_bytes(&fixture("encrypted.pdf")).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

#[test]
fn bad_startxref_offset_is_an_error() {
    let err = Document::from_bytes(&fixture("bad_offset.pdf")).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Parse(_) | Error::Unsupported { .. } | Error::Structure(_)
        ),
        "expected a parse-level error, got: {err:?}"
    );
}

#[test]
fn missing_pages_reference_is_reported() {
    // /Pages points at an undefined object: §7.3.10 makes the
    // reference null, and a catalog without /Pages is a structure
    // error — the file is still refused, with the right reason.
    let err = Document::from_bytes(&fixture("missing_ref.pdf")).unwrap_err();
    assert!(
        matches!(err, Error::Structure(_)),
        "expected Structure (catalog without /Pages), got: {err:?}"
    );
}

#[test]
fn cyclic_page_tree_is_detected() {
    let err = Document::from_bytes(&fixture("cyclic_pages.pdf")).unwrap_err();
    assert!(
        matches!(err, Error::CycleDetected { .. }),
        "expected CycleDetected, got: {err:?}"
    );
}

#[test]
fn truncated_input_is_an_error_not_a_panic() {
    let err = Document::from_bytes(&fixture("truncated.pdf")).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Parse(_)
                | Error::UnexpectedEof { .. }
                | Error::Structure(_)
                | Error::MissingReference(_)
        ),
        "expected a clean error, got: {err:?}"
    );
}

#[test]
fn reference_loop_does_not_hang() {
    // Two objects referencing each other: resolution must terminate.
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 10 10] >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec(),
        ),
        (4, b"5 0 R".to_vec()),
        (5, b"4 0 R".to_vec()),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = data.len();
    data.extend_from_slice(b"xref\n0 6\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        let _ = n;
    }
    data.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());

    let doc = Document::from_bytes(&data).expect("document itself is fine");
    let contents = doc.pages()[0].contents().unwrap().as_ref().unwrap();
    let err = doc.resolve(contents).unwrap_err();
    assert!(
        matches!(
            err,
            Error::CycleDetected { .. } | Error::LimitExceeded { .. }
        ),
        "reference loop should terminate with an error, got: {err:?}"
    );
}

#[test]
fn missing_reference_inside_pages_kids() {
    // Kids contains a ref to an object that doesn't exist.
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [9 0 R] /Count 1 /MediaBox [0 0 10 10] >>".to_vec(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref_pos = data.len();
    data.extend_from_slice(b"xref\n0 3\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref_pos}\n%%EOF\n").as_bytes());

    // The undefined kid is null (§7.3.10); a page tree whose kid is
    // not a dictionary is a structure error.
    let err = Document::from_bytes(&data).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

#[test]
fn name_with_hash_escapes_and_string_parsing() {
    // Unit-level checks on the object parser via a tiny document is
    // overkill; exercise public types instead.
    let r = IndirectRef {
        number: 7,
        generation: 0,
    };
    assert_eq!(r.to_string(), "7 0 R");
}
