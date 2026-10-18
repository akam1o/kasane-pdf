//! M1 integration tests: stream decoding, xref streams, object streams,
//! page contents, free-entry invalidation, and resource limits.

use kasane_pdf::{Document, Error, IndirectRef, Limits};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

// -------------------------------------------------------------------
// Stream decoding
// -------------------------------------------------------------------

#[test]
fn flate_contents_decodes() {
    let doc = Document::from_bytes(&fixture("flate_contents.pdf")).unwrap();
    let contents = doc.page_contents(&doc.pages()[0]).unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(
        contents[0],
        b"q 1 0 0 1 0 0 cm BT /F1 12 Tf (compressed page) Tj ET Q"
    );
}

#[test]
fn contents_array_decodes_in_order() {
    let doc = Document::from_bytes(&fixture("contents_array.pdf")).unwrap();
    let contents = doc.page_contents(&doc.pages()[0]).unwrap();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0], b"q 1 0 0 1 0 0 cm");
    assert_eq!(contents[1], b"BT /F1 12 Tf (part two) Tj ET");
}

#[test]
fn contents_none_returns_empty() {
    let doc = Document::from_bytes(&fixture("contents_none.pdf")).unwrap();
    let contents = doc.page_contents(&doc.pages()[0]).unwrap();
    assert!(contents.is_empty());
}

#[test]
fn flate_truncated_data_errors() {
    let doc = Document::from_bytes(&fixture("flate_bad_data.pdf")).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::Parse(_) | Error::UnexpectedEof { .. }),
        "got: {err:?}"
    );
}

#[test]
fn flate_predictor_supported() {
    let doc = Document::from_bytes(&fixture("flate_predictor.pdf")).unwrap();
    let contents = doc.page_contents(&doc.pages()[0]).unwrap();
    assert_eq!(contents.len(), 1);
    // 3x1 RGB, filter byte 0 -> raw bytes
    assert_eq!(contents[0], b"\x11\x22\x33\x11\x22\x33\x11\x22\x33");
}

#[test]
fn flate_predictor_unsupported_is_error() {
    let doc = Document::from_bytes(&fixture("flate_predictor_bad.pdf")).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::Unsupported { feature, .. } if feature.contains("predictor")),
        "got: {err:?}"
    );
}

#[test]
fn unsupported_filter_is_error_when_contents_read() {
    let doc = Document::from_bytes(&fixture("unsupported_filter.pdf")).unwrap();
    // The document itself loads fine — the filter is only needed now.
    assert_eq!(doc.page_count(), 1);
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "got: {err:?}");
}

// -------------------------------------------------------------------
// Object streams
// -------------------------------------------------------------------

#[test]
fn objstm_resolves_compressed_page() {
    let doc = Document::from_bytes(&fixture("objstm.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
    let page = &doc.pages()[0];
    assert_eq!(page.media_box().urx, 300.0);
    assert_eq!(
        page.object_ref(),
        Some(IndirectRef {
            number: 5,
            generation: 0
        })
    );
    let contents = doc.page_contents(page).unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0], b"q 1 0 0 1 10 10 cm Q");
}

// -------------------------------------------------------------------
// xref stream and hybrid files
// -------------------------------------------------------------------

#[test]
fn hybrid_file_uses_classic_primary() {
    let doc = Document::from_bytes(&fixture("hybrid.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.pages()[0].media_box().urx, 500.0);
}

// -------------------------------------------------------------------
// Free entries / generation / /Prev hardening
// -------------------------------------------------------------------

#[test]
fn free_entry_invalidates_older_object() {
    // Object 4 is defined in the base section and freed in the update.
    // Resolving it must fail with MissingReference, not reuse the stale
    // in-use entry.
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (4, b"<< /Length 4 >>\nstream\nhi\n\nendstream".to_vec()),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref1 = data.len();
    data.extend_from_slice(b"xref\n0 5\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    // Update section frees object 4 and adds nothing else.
    data.extend_from_slice(b"4 0 obj\nendobj\n");
    let xref2 = data.len();
    data.extend_from_slice(b"xref\n4 1\n");
    data.extend_from_slice(b"0000000000 00000 f \n");
    data.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R /Prev ");
    data.extend_from_slice(xref1.to_string().as_bytes());
    data.extend_from_slice(b" >>\n");
    data.extend_from_slice(format!("startxref\n{xref2}\n%%EOF\n").as_bytes());

    let doc = Document::from_bytes(&data).unwrap();
    // Page 1 has a Contents reference to object 4, which is now free.
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::MissingReference(_)), "got: {err:?}");
}

#[test]
fn generation_mismatch_is_reported() {
    // Document expects object 4 gen 0; the xref only has gen 5.
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (4, b"<< /Length 4 >>\nstream\nhi\n\nendstream".to_vec()),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 5\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    data.extend_from_slice(format!("{:010} 00000 n \n", offsets[0].1).as_bytes());
    data.extend_from_slice(format!("{:010} 00000 n \n", offsets[1].1).as_bytes());
    data.extend_from_slice(format!("{:010} 00000 n \n", offsets[2].1).as_bytes());
    // gen 5, not gen 0
    data.extend_from_slice(format!("{:010} 00005 n \n", offsets[3].1).as_bytes());
    data.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());

    let doc = Document::from_bytes(&data).unwrap();
    let err = doc
        .resolve(IndirectRef {
            number: 4,
            generation: 0,
        })
        .unwrap_err();
    assert!(matches!(err, Error::MissingReference(_)), "got: {err:?}");
}

// -------------------------------------------------------------------
// Limits
// -------------------------------------------------------------------

#[test]
fn decoded_stream_limit_is_enforced() {
    let limits = Limits {
        max_stream_bytes: 10, // tiny: any decoded stream exceeds this
        ..Default::default()
    };
    let doc = Document::from_bytes_with_limits(&fixture("flate_contents.pdf"), limits).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "got: {err:?}");
}

#[test]
fn filter_chain_limit_is_enforced() {
    let limits = Limits {
        max_filter_chain: 1,
        ..Default::default()
    };
    // Craft a stream with two filters (both FlateDecode, but that's 2).
    let raw = zlib_compress(b"hi");
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            format!(
                "<< /Length {} /Filter [/FlateDecode /FlateDecode] >>\nstream\n",
                raw.len()
            )
            .into_bytes(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendstream\nendobj\n");
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 5\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());

    let doc = Document::from_bytes_with_limits(&data, limits).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit.contains("filter")),
        "got: {err:?}"
    );
}

fn zlib_compress(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}
