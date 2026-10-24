//! Incremental save (ISO 32000-1 §7.5.6): the original bytes are kept
//! verbatim and a new revision is appended — on classic-xref and
//! xref-stream files, repeatedly, for annotation add/remove/modify,
//! form-field values and `/Info`; and the signature policy that gates
//! it (§12.8).

mod common;

use common::{build_pdf, fixture, stream_obj};
use kasane_pdf::{render_page, signatures, Dict, Document, Error, Object, RenderOptions};

fn base_pdf() -> Vec<u8> {
    build_pdf(&[
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_vec(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R \
              /Annots [5 0 R 7 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"0 0 1 rg 0 0 200 20 re f")),
        (
            5,
            b"<< /Type /Annot /Subtype /Square /Rect [20 100 80 160] /C [1 0 0] /Contents (one) >>"
                .to_vec(),
        ),
        (6, b"<< /Fields [7 0 R] /DA (/Helv 0 Tf 0 g) >>".to_vec()),
        (
            7,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (old) /Rect [20 20 180 50] \
              /DA (/Helv 0 Tf 0 g) /MK << /BC [0] >> >>"
                .to_vec(),
        ),
    ])
}

fn px(r: &kasane_pdf::RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

fn ink(r: &kasane_pdf::RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = r.pixmap().pixel(x, y).unwrap();
            (p.red() as u32 + p.green() as u32 + p.blue() as u32) < 600
        })
        .count()
}

fn count(
    r: &kasane_pdf::RenderedPage,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    f: fn((u8, u8, u8)) -> bool,
) -> usize {
    (x0..x1)
        .flat_map(|x| (y0..y1).map(move |y| (x, y)))
        .filter(|&(x, y)| f(px(r, x, y)))
        .count()
}
fn green(r: &kasane_pdf::RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    count(r, x0, y0, x1, y1, |p| p.1 > 150 && p.0 < 120 && p.2 < 120)
}
fn redish(r: &kasane_pdf::RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    count(r, x0, y0, x1, y1, |p| p.0 > 150 && p.1 < 120 && p.2 < 120)
}
fn blueish(r: &kasane_pdf::RenderedPage, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
    count(r, x0, y0, x1, y1, |p| p.2 > 150 && p.0 < 120 && p.1 < 120)
}

fn render(doc: &Document) -> kasane_pdf::RenderedPage {
    render_page(doc, &doc.pages()[0], &RenderOptions::default()).unwrap()
}

fn annots(doc: &Document) -> Vec<Object> {
    let p = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let a = p
        .as_dict()
        .unwrap()
        .get(b"Annots".as_slice())
        .cloned()
        .unwrap();
    doc.deref(&a).unwrap().as_array().unwrap().to_vec()
}

fn qpdf_ok(bytes: &[u8]) -> Option<bool> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("kasane-inc-{}-{n}.pdf", std::process::id()));
    std::fs::write(&tmp, bytes).ok()?;
    let out = std::process::Command::new("qpdf")
        .args(["--check", tmp.to_str()?])
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!(
            "qpdf --check failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let keep = std::env::temp_dir().join(format!("kasane-inc-fail-{n}.pdf"));
        let _ = std::fs::copy(&tmp, &keep);
    }
    let _ = std::fs::remove_file(&tmp);
    Some(out.status.success())
}

#[test]
fn appends_a_revision_and_keeps_the_original_bytes() {
    let orig = base_pdf();
    let doc = Document::from_bytes(&orig).unwrap();
    let mut e = doc.edit().unwrap();
    e.add_square(
        0,
        kasane_pdf::Rect {
            llx: 100.0,
            lly: 100.0,
            urx: 160.0,
            ury: 160.0,
        },
        [0.0, 1.0, 0.0],
        2.0,
    )
    .unwrap();
    let out = e.save_incremental().unwrap();
    assert!(out.starts_with(&orig), "original bytes are kept verbatim");
    assert!(out.len() > orig.len());
    let tail = &out[orig.len()..];
    assert!(
        tail.windows(5).any(|w| w == b"/Prev"),
        "trailer chains to the old xref"
    );
    assert!(tail.windows(4).any(|w| w == b"xref"));
    let doc2 = Document::from_bytes(&out).unwrap();
    assert_eq!(doc2.page_count(), 1);
    assert_eq!(annots(&doc2).len(), 3, "two originals plus the new square");
    // The original objects are still read from their original offsets.
    let a0 = doc2.deref(&annots(&doc2)[0]).unwrap();
    assert_eq!(
        a0.as_dict().unwrap().get(b"Contents".as_slice()),
        Some(&Object::String(b"one".to_vec()))
    );
    let r = render(&doc2);
    assert!(green(&r, 100, 38, 160, 44) > 20, "new square stroke");
    assert!(redish(&r, 18, 40, 24, 100) > 20, "old square still there");
    if let Some(ok) = qpdf_ok(&out) {
        assert!(ok, "qpdf --check accepts the update");
    }
}

#[test]
fn remove_modify_and_field_value() {
    let orig = base_pdf();
    let doc = Document::from_bytes(&orig).unwrap();
    let mut e = doc.edit().unwrap();
    let mut d = Dict::new();
    d.insert(
        b"C".to_vec(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(1),
        ]),
    );
    d.insert(b"Contents".to_vec(), Object::String(b"changed".to_vec()));
    e.set_annotation_entries(0, 0, d).unwrap();
    e.set_field_value("name", Object::String(b"NEW VALUE".to_vec()))
        .unwrap();
    let out = e.save_incremental().unwrap();
    assert!(out.starts_with(&orig));
    let doc2 = Document::from_bytes(&out).unwrap();
    let a0 = doc2.deref(&annots(&doc2)[0]).unwrap();
    assert_eq!(
        a0.as_dict().unwrap().get(b"Contents".as_slice()),
        Some(&Object::String(b"changed".to_vec()))
    );
    let r = render(&doc2);
    assert!(
        blueish(&r, 18, 40, 24, 100) > 20,
        "square now blue (no /AP: synthesised)"
    );
    assert_eq!(redish(&r, 18, 40, 24, 100), 0);
    assert!(ink(&r, 22, 152, 178, 178) > 30, "field value drawn");
    let af = doc2
        .deref(doc2.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    assert_eq!(
        af.as_dict().unwrap().get(b"NeedAppearances".as_slice()),
        Some(&Object::Bool(true))
    );
    // A second revision on top of the first: remove the square.
    let mut e2 = doc2.edit().unwrap();
    e2.remove_annotation(0, 0).unwrap();
    let out2 = e2.save_incremental().unwrap();
    assert!(
        out2.starts_with(&out),
        "the first update is kept verbatim too"
    );
    let doc3 = Document::from_bytes(&out2).unwrap();
    assert_eq!(annots(&doc3).len(), 1);
    let r = render(&doc3);
    assert_eq!(blueish(&r, 18, 40, 24, 100), 0, "square gone");
    assert!(
        ink(&r, 22, 152, 178, 178) > 30,
        "field value from revision 1 survives"
    );
    if let Some(ok) = qpdf_ok(&out2) {
        assert!(ok);
    }
    // Out-of-range index and unknown field are errors, nothing written.
    let mut e3 = doc3.edit().unwrap();
    assert!(e3.remove_annotation(0, 5).is_err());
    assert!(e3.set_field_value("nope", Object::Null).is_err());
}

#[test]
fn info_update_and_xref_stream_input() {
    let orig = base_pdf();
    let doc = Document::from_bytes(&orig).unwrap();
    let mut e = doc.edit().unwrap();
    let mut d = Dict::new();
    d.insert(b"Title".to_vec(), Object::String(b"Updated".to_vec()));
    e.set_info(d).unwrap();
    let out = e.save_incremental().unwrap();
    let doc2 = Document::from_bytes(&out).unwrap();
    let info = doc2
        .deref(doc2.trailer().get(b"Info".as_slice()).unwrap())
        .unwrap();
    assert_eq!(
        info.as_dict().unwrap().get(b"Title".as_slice()),
        Some(&Object::String(b"Updated".to_vec()))
    );
    // A file whose newest section is an xref stream gets an xref
    // stream back (§7.5.8.4), never a classic table.
    let orig = std::fs::read(fixture("xref_stream.pdf")).unwrap();
    let doc = Document::from_bytes(&orig).unwrap();
    let mut e = doc.edit().unwrap();
    e.add_square(
        0,
        kasane_pdf::Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 50.0,
            ury: 50.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let out = e.save_incremental().unwrap();
    assert!(out.starts_with(&orig));
    let tail = &out[orig.len()..];
    assert!(
        tail.windows(11).any(|w| w == b"/Type /XRef"),
        "xref stream appended"
    );
    assert!(
        !tail.windows(8).any(|w| w == b"trailer\n"),
        "no classic table"
    );
    let doc2 = Document::from_bytes(&out).unwrap();
    assert_eq!(doc2.page_count(), doc.page_count());
    assert_eq!(annots(&doc2).len(), 1);
    if let Some(ok) = qpdf_ok(&out) {
        assert!(ok);
    }
}

/// A PDF carrying a signature whose /ByteRange is patched to cover the
/// whole file except the /Contents hex string, with `/DocMDP /P p`
/// when `p` is given.
fn signed_pdf(p: Option<i64>) -> Vec<u8> {
    let reference = match p {
        Some(p) => {
            format!("/Reference [<< /TransformMethod /DocMDP /TransformParams << /P {p} >> >>]")
        }
        None => String::new(),
    };
    let perms = if p.is_some() {
        "/Perms << /DocMDP 8 0 R >>"
    } else {
        ""
    };
    let sig = format!(
        "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
         /ByteRange [0000000000 0000000000 0000000000 0000000000] /Contents <{}> {reference} >>",
        "00".repeat(32)
    );
    let mut pdf = build_pdf(&[
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R {perms} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << >> /Contents 4 0 R \
              /Annots [7 0 R 9 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"")),
        (6, b"<< /Fields [7 0 R 9 0 R] >>".to_vec()),
        (
            7,
            b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (sig1) /V 8 0 R /Rect [0 0 0 0] >>"
                .to_vec(),
        ),
        (8, sig.into_bytes()),
        (
            9,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (x) /Rect [20 20 180 50] >>"
                .to_vec(),
        ),
    ]);
    // Patch /ByteRange: [0 a b c] with a = offset of '<', b = after '>'.
    let a = pdf.windows(11).position(|w| w == b"/Contents <").unwrap() + 10;
    let b = a + 2 + 64;
    let c = pdf.len() - b;
    let range = format!("[{:010} {:010} {:010} {:010}]", 0, a, b, c);
    let pos = pdf.windows(11).position(|w| w == b"/ByteRange ").unwrap() + 11;
    pdf[pos..pos + range.len()].copy_from_slice(range.as_bytes());
    pdf
}

#[test]
fn signature_policy_gates_incremental_saves() {
    // Detection.
    let doc = Document::from_bytes(&signed_pdf(Some(2))).unwrap();
    let sigs = signatures(&doc).unwrap();
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].field, "sig1");
    assert!(sigs[0].range_intact, "{:?}", sigs[0]);
    assert_eq!(sigs[0].docmdp_p, Some(2));
    assert!(sigs[0].is_certification());
    // A full rewrite is always refused.
    assert!(matches!(
        kasane_pdf::rewrite(&doc),
        Err(Error::SignaturePolicy { .. })
    ));
    // /P 2: form filling may be appended; annotations and metadata may not.
    let mut e = doc.edit().unwrap();
    e.set_field_value("name", Object::String(b"filled".to_vec()))
        .unwrap();
    let out = e.save_incremental().unwrap();
    assert!(out.starts_with(doc_bytes(&signed_pdf(Some(2)))));
    let doc2 = Document::from_bytes(&out).unwrap();
    assert!(
        signatures(&doc2).unwrap()[0].range_intact,
        "signed bytes still intact"
    );
    let mut e = doc.edit().unwrap();
    e.add_square(
        0,
        kasane_pdf::Rect {
            llx: 10.0,
            lly: 60.0,
            urx: 50.0,
            ury: 90.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    assert!(matches!(
        e.save_incremental(),
        Err(Error::SignaturePolicy { .. })
    ));
    let mut e = doc.edit().unwrap();
    e.set_info(Dict::new()).unwrap();
    assert!(matches!(
        e.save_incremental(),
        Err(Error::SignaturePolicy { .. })
    ));
    // /P 1: nothing.
    let doc = Document::from_bytes(&signed_pdf(Some(1))).unwrap();
    let mut e = doc.edit().unwrap();
    e.set_field_value("name", Object::String(b"filled".to_vec()))
        .unwrap();
    assert!(matches!(
        e.save_incremental(),
        Err(Error::SignaturePolicy { .. })
    ));
    // /P 3 and an approval signature: annotations too, metadata not.
    for p in [Some(3), None] {
        let doc = Document::from_bytes(&signed_pdf(p)).unwrap();
        let mut e = doc.edit().unwrap();
        e.add_square(
            0,
            kasane_pdf::Rect {
                llx: 10.0,
                lly: 60.0,
                urx: 50.0,
                ury: 90.0,
            },
            [1.0, 0.0, 0.0],
            1.0,
        )
        .unwrap();
        assert!(e.save_incremental().is_ok(), "{p:?}");
        let mut e = doc.edit().unwrap();
        e.set_info(Dict::new()).unwrap();
        assert!(
            matches!(e.save_incremental(), Err(Error::SignaturePolicy { .. })),
            "{p:?}"
        );
    }
    // A /ByteRange that does not cover the file intact: refused for
    // everything (the claim cannot be reasoned about).
    let mut broken = signed_pdf(None);
    let pos = broken
        .windows(11)
        .position(|w| w == b"/ByteRange ")
        .unwrap()
        + 11;
    broken[pos..pos + 12].copy_from_slice(b"[0000000000 ");
    broken[pos + 12..pos + 22].copy_from_slice(b"0000000005");
    let doc = Document::from_bytes(&broken).unwrap();
    assert!(!signatures(&doc).unwrap()[0].range_intact);
    let mut e = doc.edit().unwrap();
    e.set_field_value("name", Object::String(b"x".to_vec()))
        .unwrap();
    assert!(matches!(
        e.save_incremental(),
        Err(Error::SignaturePolicy { .. })
    ));
    // Unsigned: everything may be appended.
    let doc = Document::from_bytes(&base_pdf()).unwrap();
    assert!(signatures(&doc).unwrap().is_empty());
}

fn doc_bytes(v: &[u8]) -> &[u8] {
    v
}
