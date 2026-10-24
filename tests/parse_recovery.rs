//! Cross-reference recovery (`Document::from_bytes_recovering`):
//! strict parsing stays strict; recovery rebuilds the table from the
//! `n g obj` definitions in the file, reports what it did, and never
//! turns a hostile file into a hang or a mis-wired object graph.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};

/// A well-formed one-page file whose content fills the page black.
fn good_pdf() -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("", b"0 g 0 0 100 100 re f")),
    ])
}

fn renders_black(doc: &Document) -> bool {
    let r = render_page(doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let p = r.pixmap().pixel(50, 50).unwrap();
    (p.red(), p.green(), p.blue()) == (0, 0, 0)
}

fn break_startxref(pdf: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(pdf).to_string();
    let i = s.rfind("startxref\n").unwrap() + 10;
    let j = s[i..].find('\n').unwrap() + i;
    format!("{}{}{}", &s[..i], "999999", &s[j..]).into_bytes()
}

#[test]
fn strict_parse_is_unchanged_and_reports_no_recovery() {
    let doc = Document::from_bytes(&good_pdf()).unwrap();
    assert!(doc.recovery().is_none());
    let doc = Document::from_bytes_recovering(&good_pdf()).unwrap();
    assert!(
        doc.recovery().is_none(),
        "a clean file is never 'recovered'"
    );
}

#[test]
fn missing_endobj_is_refused_strictly_and_recovered() {
    // §7.3.10: every indirect object ends with `endobj`. A file whose
    // definitions lack it is malformed — the strict parser says so —
    // yet each object is delimited by its own syntax, so recovery
    // reads it. The keyword is blanked in place so the xref offsets
    // stay valid and the missing keyword is the only fault.
    let mut stripped = good_pdf();
    let mut i = 0;
    let mut blanked = 0;
    while let Some(k) = stripped[i..].windows(6).position(|w| w == b"endobj") {
        stripped[i + k..i + k + 6].copy_from_slice(b"      ");
        i += k + 6;
        blanked += 1;
    }
    assert!(blanked >= 4);
    let err = Document::from_bytes(&stripped).unwrap_err();
    assert!(
        matches!(&err, Error::Parse(m) if m.contains("endobj")),
        "{err:?}"
    );
    let doc = Document::from_bytes_recovering(&stripped).unwrap();
    assert!(renders_black(&doc));
}

#[test]
fn broken_startxref_is_refused_strictly_and_recovered() {
    let pdf = break_startxref(&good_pdf());
    let err = Document::from_bytes(&pdf).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
    let doc = Document::from_bytes_recovering(&pdf).unwrap();
    let rep = doc.recovery().expect("recovered");
    assert_eq!(rep.objects_found, 4);
    assert_eq!(rep.trailer_source, "trailer dictionary");
    assert!(rep.strict_error.contains("xref"), "{}", rep.strict_error);
    assert!(renders_black(&doc));
}

#[test]
fn missing_xref_and_trailer_falls_back_to_catalog_scan() {
    let pdf = good_pdf();
    let s = String::from_utf8_lossy(&pdf).to_string();
    let cut = s.find("xref\n").unwrap();
    let truncated = s.as_bytes()[..cut].to_vec();
    assert!(matches!(
        Document::from_bytes(&truncated),
        Err(Error::Parse(_))
    ));
    let doc = Document::from_bytes_recovering(&truncated).unwrap();
    let rep = doc.recovery().unwrap();
    assert_eq!(rep.trailer_source, "/Type /Catalog scan");
    assert!(
        rep.warnings.iter().any(|w| w.contains("Catalog")),
        "{:?}",
        rep.warnings
    );
    assert!(renders_black(&doc));
}

#[test]
fn wrong_offsets_are_recovered_from_the_definitions() {
    // Insert junk after the header so every xref offset is stale.
    let pdf = good_pdf();
    let mut shifted = b"%PDF-1.5\n%junk junk junk junk junk\n".to_vec();
    shifted.extend_from_slice(&pdf[9..]);
    let err = Document::from_bytes(&shifted).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
    let doc = Document::from_bytes_recovering(&shifted).unwrap();
    assert!(doc.recovery().is_some());
    assert!(renders_black(&doc));
}

#[test]
fn later_definition_wins_like_an_incremental_update() {
    // Append a second definition of the content stream (white fill)
    // without updating the xref: strict reads the old one, recovery
    // the newest, and says that it replaced an object.
    let mut pdf = break_startxref(&good_pdf());
    pdf.extend_from_slice(b"4 0 obj\n");
    pdf.extend_from_slice(&stream_obj("", b"1 g 0 0 100 100 re f"));
    pdf.extend_from_slice(b"\nendobj\n");
    let doc = Document::from_bytes_recovering(&pdf).unwrap();
    let rep = doc.recovery().unwrap();
    assert!(
        rep.warnings
            .iter()
            .any(|w| w.contains("defined more than once")),
        "{:?}",
        rep.warnings
    );
    assert!(!renders_black(&doc));
}

#[test]
fn object_stream_members_fill_gaps_only() {
    // Catalog and pages live in an object stream; the page and its
    // content are direct. No xref at all.
    let inner = b"<< /Type /Catalog /Pages 2 0 R >> << /Type /Pages /Kids [3 0 R] /Count 1 >>";
    let header = b"1 0 2 34 ";
    let mut body = header.to_vec();
    body.extend_from_slice(inner);
    let objstm = stream_obj(
        &format!("/Type /ObjStm /N 2 /First {}", header.len()),
        &body,
    );
    let mut pdf = b"%PDF-1.5\n".to_vec();
    for (n, b) in [
        (5u32, objstm),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("", b"0 g 0 0 100 100 re f")),
    ] {
        pdf.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        pdf.extend_from_slice(&b);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    assert!(Document::from_bytes(&pdf).is_err());
    let doc = Document::from_bytes_recovering(&pdf).unwrap();
    let rep = doc.recovery().unwrap();
    assert_eq!(rep.objects_found, 3);
    assert_eq!(rep.objects_from_streams, 2);
    assert_eq!(rep.trailer_source, "/Type /Catalog scan");
    assert!(renders_black(&doc));
}

#[test]
fn recovery_does_not_apply_to_encryption_or_non_pdf() {
    let mut pdf = break_startxref(&good_pdf());
    let s = String::from_utf8_lossy(&pdf).to_string();
    let s = s.replace(
        "/Root 1 0 R",
        "/Root 1 0 R /Encrypt << /Filter /Standard >>",
    );
    pdf = s.into_bytes();
    assert!(matches!(
        Document::from_bytes_recovering(&pdf),
        Err(Error::Encrypted)
    ));
    assert!(matches!(
        Document::from_bytes_recovering(b"not a pdf at all, 1 0 obj << >> endobj"),
        Err(Error::NotPdf)
    ));
}

#[test]
fn recovery_keeps_cycle_detection_and_limits() {
    // Pages node that is its own kid.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [2 0 R] /Count 1 >>".to_vec()),
    ]);
    let pdf = break_startxref(&pdf);
    let err = Document::from_bytes_recovering(&pdf).unwrap_err();
    assert!(
        matches!(err, Error::CycleDetected { .. } | Error::Structure(_)),
        "{err:?}"
    );
    // Object-count bound.
    let pdf = break_startxref(&good_pdf());
    let err = Document::from_bytes_recovering_with_limits(
        &pdf,
        Limits::default().with_max_recovery_objects(2),
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
}

#[test]
fn garbage_with_no_objects_is_a_parse_error() {
    let err = Document::from_bytes_recovering(b"%PDF-1.4\nnothing here obj obj 1 obj").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "{err:?}");
}

#[test]
fn glued_tokens_are_not_mistaken_for_definitions() {
    // "x12 0 obj" and "12 0obj" are not object headers; only the real
    // one counts, so the file still recovers to a single catalog.
    let mut pdf = break_startxref(&good_pdf());
    pdf.extend_from_slice(b"%x12 0 obj << /Type /Catalog >>\n%12 0obj << >>\n");
    let doc = Document::from_bytes_recovering(&pdf).unwrap();
    assert_eq!(doc.recovery().unwrap().objects_found, 4);
}
