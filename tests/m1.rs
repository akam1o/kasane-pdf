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
fn hybrid_stream_overrides_classic_entry() {
    // §7.5.8.4 / qpdf implementation notes: within the same update
    // section the /XRefStm stream replaces entries from the classic
    // table. The fixture marks object 3 in-use in the table but free in
    // the stream, so resolution must fail.
    let err = Document::from_bytes(&fixture("hybrid.pdf")).unwrap_err();
    assert!(
        matches!(err, Error::MissingReference(_)),
        "expected MissingReference, got: {err:?}"
    );
}

#[test]
fn hybrid_stream_supplies_missing_entry() {
    // The classic table covers only objects 0-3; the /XRefStm stream
    // additionally marks object 5 in-use. The stream's view wins.
    let doc = Document::from_bytes(&fixture("hybrid_gap.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.pages()[0].media_box().urx, 400.0);
}

#[test]
fn hybrid_stream_supplies_compressed_entry() {
    // The classic table covers objects 0-4 and marks 5 free; the stream
    // marks 5 compressed inside object stream 4. Resolution must go
    // through the ObjStm.
    let doc = Document::from_bytes(&fixture("hybrid_compressed.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
    assert_eq!(doc.pages()[0].media_box().urx, 400.0);
    assert_eq!(
        doc.pages()[0].object_ref(),
        Some(IndirectRef {
            number: 5,
            generation: 0
        })
    );
}

#[test]
fn prev_newer_section_wins() {
    let doc = Document::from_bytes(&fixture("prev_chain.pdf")).unwrap();
    assert_eq!(doc.pages()[0].media_box().urx, 999.0);
}

#[test]
fn xref_stream_bad_entry_type_errors() {
    let err = Document::from_bytes(&fixture("xref_stream_bad_type.pdf")).unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("entry type")),
        "got: {err:?}"
    );
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

/// Boundary: decoded size exactly at the limit succeeds; one byte over
/// fails.
#[test]
fn decoded_stream_limit_boundary() {
    // Flate-decoded to exactly 10 bytes.
    let decoded = b"0123456789";
    let comp = zlib_compress(decoded);
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
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                    comp.len()
                )
                .into_bytes(),
                comp.clone(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        if *n == 4 {
            data.extend_from_slice(b"\nendstream\nendobj\n");
        } else {
            data.extend_from_slice(b"\nendobj\n");
        }
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 5\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());

    // Exactly at the limit: OK.
    let limits = Limits {
        max_stream_bytes: 10,
        ..Default::default()
    };
    let doc = Document::from_bytes_with_limits(&data, limits).unwrap();
    assert_eq!(doc.page_contents(&doc.pages()[0]).unwrap()[0], decoded);

    // One byte less: fail.
    let limits = Limits {
        max_stream_bytes: 9,
        ..Default::default()
    };
    let doc = Document::from_bytes_with_limits(&data, limits).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "got: {err:?}");
}

/// Cumulative limit: two small streams that each fit under
/// max_stream_bytes together exceed max_total_decoded_bytes.
#[test]
fn cumulative_decoded_limit() {
    let decoded_a = b"AAAA"; // 4 bytes
    let decoded_b = b"BBBB"; // 4 bytes
    let comp_a = zlib_compress(decoded_a);
    let comp_b = zlib_compress(decoded_b);
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>"
                .to_vec(),
        ),
        (
            4,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                    comp_a.len()
                )
                .into_bytes(),
                comp_a.clone(),
            ]
            .concat(),
        ),
        (
            5,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                    comp_b.len()
                )
                .into_bytes(),
                comp_b.clone(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        if *n >= 4 {
            data.extend_from_slice(b"\nendstream\nendobj\n");
        } else {
            data.extend_from_slice(b"\nendobj\n");
        }
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 6\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());

    // Each stream is 4 bytes; total = 8.  A cumulative limit of 6 lets
    // the first pass and the second fail.
    let limits = Limits {
        max_stream_bytes: 100,
        max_total_decoded_bytes: 6,
        ..Default::default()
    };
    let doc = Document::from_bytes_with_limits(&data, limits).unwrap();
    let err = doc.page_contents(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit.contains("total")),
        "got: {err:?}"
    );
}

/// max_objstm_entries: /N above the limit is rejected.
#[test]
fn objstm_entries_limit() {
    // Craft an ObjStm with /N = 2; limit is 1.
    let page_dict = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] >>";
    let header = b"5 0 6 20"; // objnum 5 at 0, objnum 6 at offset 20
    let mut body = page_dict.to_vec();
    body.extend_from_slice(b" ");
    body.extend_from_slice(page_dict); // dummy second obj
    let first = header.len() + 1;
    let mut data = header.to_vec();
    data.extend_from_slice(b" ");
    data.extend_from_slice(&body);
    let comp = zlib_compress(&data);

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets = Vec::new();
    let objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [5 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_vec(),
        ),
        (
            4,
            [
                format!(
                    "<< /Type /ObjStm /N 2 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
                    first,
                    comp.len()
                )
                .into_bytes(),
                comp.clone(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &objs {
        offsets.push((*n, pdf.len()));
        pdf.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        pdf.extend_from_slice(b);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    // xref stream covering objects 0..5
    let xref_obj_pos = pdf.len();
    let w = [1u8, 4, 2];
    let mut xdata = Vec::new();
    let entries: [(u32, u8, u64, u64); 6] = [
        (0, 0, 0, 65535),
        (1, 1, offsets[0].1 as u64, 0),
        (2, 1, offsets[1].1 as u64, 0),
        (3, 0, 0, 0),
        (4, 1, offsets[2].1 as u64, 0),
        (5, 2, 4, 0), // compressed in ObjStm 4, index 0
    ];
    for (_num, t, f2, f3) in &entries {
        xdata.push(*t);
        // f2/f3 encode only the low W[i] bytes (big-endian).
        let f2b = f2.to_be_bytes();
        let f3b = f3.to_be_bytes();
        xdata.extend_from_slice(&f2b[8 - w[1] as usize..]);
        xdata.extend_from_slice(&f3b[8 - w[2] as usize..]);
    }
    pdf.extend_from_slice(b"6 0 obj\n");
    pdf.extend_from_slice(
        format!(
            "<< /Type /XRef /Length {} /W [1 4 2] /Size 6 /Root 1 0 R >>\nstream\n",
            xdata.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&xdata);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    pdf.extend_from_slice(format!("startxref\n{xref_obj_pos}\n%%EOF\n").as_bytes());

    let limits = Limits {
        max_objstm_entries: 1,
        ..Default::default()
    };
    let err = Document::from_bytes_with_limits(&pdf, limits).unwrap_err();
    assert!(
        matches!(err, Error::Structure(ref m) if m.contains("/N")),
        "got: {err:?}"
    );
}

/// max_reference_depth: exhaust the budget with a reference chain.
#[test]
fn reference_depth_limit() {
    let limits = Limits {
        max_reference_depth: 3,
        ..Default::default()
    };
    // Chain 4->5->6->7, but budget is 3.
    let mut data = Vec::new();
    data.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>".to_vec(),
        ),
        (4, b"5 0 R".to_vec()),
        (5, b"6 0 R".to_vec()),
        (6, b"7 0 R".to_vec()),
        (7, b"8 0 R".to_vec()), // never reached
    ];
    for (n, b) in &bodies {
        offsets.push((*n, data.len()));
        data.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        data.extend_from_slice(b);
        data.extend_from_slice(b"\nendobj\n");
    }
    let xref = data.len();
    data.extend_from_slice(b"xref\n0 8\n");
    data.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        data.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    data.extend_from_slice(b"trailer\n<< /Size 8 /Root 1 0 R >>\n");
    data.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());

    let doc = Document::from_bytes_with_limits(&data, limits).unwrap();
    let err = doc
        .resolve(IndirectRef {
            number: 4,
            generation: 0,
        })
        .unwrap_err();
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
