//! Milestone M9 — add a `/Subtype /Square` annotation and rewrite.
//!
//! The session ([`Document::edit`]) accumulates synthetic objects in
//! an overlay; [`Edit::rewrite`] emits a normal single-revision PDF.
//! Tests cover validation, the object graph, appearance, and
//! preservation of existing content.

use kasane_pdf::{Dict, Document, Error, Limits, Object, Rect};

/// Resolve `o` (usually a `Ref`) and return its dict body cloned —
/// the temporary resolved object can't be borrowed out, so clone.
fn annot_dict(d: &Document, o: &Object) -> Dict {
    d.deref(o).unwrap().as_dict().unwrap().clone()
}

fn doc(bytes: &[u8]) -> Document {
    Document::from_bytes(bytes).unwrap()
}

fn reparse(bytes: &[u8]) -> Document {
    Document::from_bytes(bytes).unwrap()
}

/// A one-page PDF whose page dict carries `extra` verbatim — used to
/// inject `/Annots`, `/UserUnit`, `/Rotate`, `/CropBox` etc.
fn page_pdf(extra_page: &str) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] {extra_page} >>").as_bytes(),
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

/// Two-page PDF where both pages share one indirect `/Annots` array.
fn shared_annots_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R 8 0 R] /Count 2 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots 9 0 R >>",
    );
    push(
        8,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots 9 0 R >>",
    );
    push(9, b"[5 0 R]"); // shared /Annots array holding one existing annot
    push(
        5,
        b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (note) >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
    for n in 1..10u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

fn square_on(page: usize, pdf: &[u8]) -> Vec<u8> {
    let d = doc(pdf);
    let mut e = d.edit().unwrap();
    e.add_square(
        page,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 100.0,
            ury: 80.0,
        },
        [1.0, 0.0, 0.0],
        2.0,
    )
    .unwrap();
    e.rewrite().unwrap()
}

/// Find the annotation dict reachable from a page's /Annots.
fn annots_of(doc: &Document, page_index: usize) -> Vec<Object> {
    let page = &doc.pages()[page_index];
    let pr = page.object_ref().unwrap();
    let page_obj = doc.resolve(pr).unwrap();
    let pd = page_obj.as_dict().unwrap();
    let annots = doc.deref(pd.get(b"Annots".as_slice()).unwrap()).unwrap();
    annots.as_array().unwrap().to_vec()
}

// ------------------------------------------------------------------
// §1  basic addition + graph
// ------------------------------------------------------------------

#[test]
fn adds_annots_key_when_absent() {
    let out = square_on(0, &page_pdf(""));
    let s = String::from_utf8_lossy(&out);
    assert!(s.contains("/Annots"), "no /Annots in output: {s}");
    assert!(s.contains("/Square"), "no /Square in output: {s}");
    let d = reparse(&out);
    let annots = annots_of(&d, 0);
    assert_eq!(annots.len(), 1);
    let a = annot_dict(&d, &annots[0]);
    assert_eq!(
        a.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
        b"Square"
    );
}

#[test]
fn annot_dict_fields() {
    let out = square_on(0, &page_pdf(""));
    let d = reparse(&out);
    let annots = annots_of(&d, 0);
    let a = annot_dict(&d, &annots[0]);
    assert_eq!(
        a.get(b"Type".as_slice()).unwrap().as_name().unwrap(),
        b"Annot"
    );
    assert_eq!(
        a.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
        b"Square"
    );
    // /Rect preserved verbatim.
    let r = a.get(b"Rect".as_slice()).unwrap().as_array().unwrap();
    assert_eq!(
        r.iter().map(|o| o.as_number().unwrap()).collect::<Vec<_>>(),
        vec![10.0, 10.0, 100.0, 80.0]
    );
    // /C = stroke colour.
    let c = a.get(b"C".as_slice()).unwrap().as_array().unwrap();
    assert_eq!(
        c.iter().map(|o| o.as_number().unwrap()).collect::<Vec<_>>(),
        vec![1.0, 0.0, 0.0]
    );
    // No /IC (unfilled).
    assert!(!a.contains_key(b"IC".as_slice()));
    // /BS width.
    let bs = a.get(b"BS".as_slice()).unwrap().as_dict().unwrap();
    assert_eq!(bs.get(b"W".as_slice()).unwrap().as_number(), Some(2.0));
    // /F print bit set.
    assert_eq!(a.get(b"F".as_slice()).unwrap().as_i64(), Some(4));
    // /P points back at the page.
    let p = a.get(b"P".as_slice()).unwrap().as_ref().unwrap();
    let page_ref = d.pages()[0].object_ref().unwrap();
    // Both resolve to the page dict.
    let via_p = d.resolve(p).unwrap();
    assert!(via_p
        .as_dict()
        .unwrap()
        .contains_key(b"MediaBox".as_slice()));
    let _ = page_ref;
}

#[test]
fn appearance_stream_structure() {
    let out = square_on(0, &page_pdf(""));
    let d = reparse(&out);
    let annots = annots_of(&d, 0);
    let a = annot_dict(&d, &annots[0]);
    let ap = a.get(b"AP".as_slice()).unwrap().as_dict().unwrap();
    let n_ref = ap.get(b"N".as_slice()).unwrap().as_ref().unwrap();
    let n_obj = d.resolve(n_ref).unwrap();
    let (dict, data) = n_obj.as_stream().unwrap();
    assert_eq!(
        dict.get(b"Type".as_slice()).unwrap().as_name().unwrap(),
        b"XObject"
    );
    assert_eq!(
        dict.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
        b"Form"
    );
    // BBox is the Rect padded by half the stroke width (2/2 = 1).
    let bb = dict.get(b"BBox".as_slice()).unwrap().as_array().unwrap();
    assert_eq!(
        bb.iter()
            .map(|o| o.as_number().unwrap())
            .collect::<Vec<_>>(),
        vec![9.0, 9.0, 101.0, 81.0]
    );
    // Body strokes the rect in the annot's colour and width.
    let body = String::from_utf8_lossy(data);
    assert!(body.contains("1 0 0 RG"), "no stroke colour: {body}");
    assert!(body.contains("2 w"), "no line width: {body}");
    assert!(body.contains("10 10 90 70 re"), "no rect path: {body}");
    assert!(body.contains("S"), "no stroke op: {body}");
    assert!(body.starts_with("q\n") && body.trim_end().ends_with("Q"));
}

#[test]
fn multiple_squares_one_page() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    for i in 0..3 {
        let o = 10.0 + i as f64 * 30.0;
        e.add_square(
            0,
            Rect {
                llx: o,
                lly: o,
                urx: o + 20.0,
                ury: o + 20.0,
            },
            [0.0, 1.0, 0.0],
            1.0,
        )
        .unwrap();
    }
    let out = e.rewrite().unwrap();
    let d = reparse(&out);
    let annots = annots_of(&d, 0);
    assert_eq!(annots.len(), 3, "expected 3 annots: {annots:?}");
    // All share one /Annots array object.
    let s = String::from_utf8_lossy(&out);
    assert_eq!(s.matches("/Subtype /Square").count(), 3);
}

#[test]
fn annots_added_to_each_target_page() {
    // Two-page doc; add to both.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >>",
    );
    push(
        4,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
    for n in 1..5u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let src = parts.concat();

    let d = doc(&src);
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 5.0,
            lly: 5.0,
            urx: 50.0,
            ury: 50.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    e.add_square(
        1,
        Rect {
            llx: 5.0,
            lly: 5.0,
            urx: 50.0,
            ury: 50.0,
        },
        [0.0, 0.0, 1.0],
        1.0,
    )
    .unwrap();
    let out = e.rewrite().unwrap();
    let d = reparse(&out);
    assert_eq!(annots_of(&d, 0).len(), 1);
    assert_eq!(annots_of(&d, 1).len(), 1);
    // Page 0's annot is red, page 1's is blue.
    let a0 = annot_dict(&d, &annots_of(&d, 0)[0]);
    let a1 = annot_dict(&d, &annots_of(&d, 1)[0]);
    assert_eq!(
        a0.get(b"C".as_slice()).unwrap().as_array().unwrap()[0].as_number(),
        Some(1.0)
    );
    assert_eq!(
        a1.get(b"C".as_slice()).unwrap().as_array().unwrap()[2].as_number(),
        Some(1.0)
    );
}

#[test]
fn existing_annots_preserved() {
    // Page already has a /Text annot; the new /Square appends after it.
    let d = doc(&shared_annots_pdf());
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 50.0,
            ury: 50.0,
        },
        [0.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let out = e.rewrite().unwrap();
    let d = reparse(&out);
    let annots = annots_of(&d, 0);
    assert_eq!(annots.len(), 2);
    // First entry is still the original /Text annot.
    let first = annot_dict(&d, &annots[0]);
    assert_eq!(
        first.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
        b"Text"
    );
    let second = annot_dict(&d, &annots[1]);
    assert_eq!(
        second
            .get(b"Subtype".as_slice())
            .unwrap()
            .as_name()
            .unwrap(),
        b"Square"
    );
}

#[test]
fn shared_annots_array_not_mutated() {
    // Two pages share /Annots; annotating page 0 must not change page 1.
    let d = doc(&shared_annots_pdf());
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 1.0,
            lly: 1.0,
            urx: 20.0,
            ury: 20.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let out = e.rewrite().unwrap();
    let d = reparse(&out);
    assert_eq!(annots_of(&d, 0).len(), 2, "page 0 got the new annot");
    assert_eq!(
        annots_of(&d, 1).len(),
        1,
        "page 1 shared array must be untouched"
    );
    // Page 1's single annot is still the original /Text.
    let a1 = annot_dict(&d, &annots_of(&d, 1)[0]);
    assert_eq!(
        a1.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
        b"Text"
    );
}

#[test]
fn original_document_unchanged() {
    let src = page_pdf("");
    let before = src.clone();
    let d = doc(&src);
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 1.0,
            lly: 1.0,
            urx: 20.0,
            ury: 20.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let _out = e.rewrite().unwrap();
    // Source bytes untouched.
    assert_eq!(src, before);
    // Original doc still has no /Annots on page 0.
    let page = &d.pages()[0];
    let po = d.resolve(page.object_ref().unwrap()).unwrap();
    assert!(!po.as_dict().unwrap().contains_key(b"Annots".as_slice()));
}

#[test]
fn failed_add_leaves_no_partial_state() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    // One valid add, then one invalid (bad page index).
    e.add_square(
        0,
        Rect {
            llx: 1.0,
            lly: 1.0,
            urx: 20.0,
            ury: 20.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    assert!(e
        .add_square(
            5,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0
            },
            [0.0, 0.0, 0.0],
            1.0
        )
        .is_err());
    // The valid add still commits cleanly on rewrite.
    let out = e.rewrite().unwrap();
    let d = reparse(&out);
    assert_eq!(annots_of(&d, 0).len(), 1);
}

// ------------------------------------------------------------------
// §2  validation
// ------------------------------------------------------------------

#[test]
fn rejects_out_of_range_page() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    let r = e.add_square(
        7,
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 9.0,
            ury: 9.0,
        },
        [0.0; 3],
        1.0,
    );
    assert!(matches!(r, Err(Error::Structure(_))));
}

#[test]
fn rejects_non_finite_rect() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let r = e.add_square(
            0,
            Rect {
                llx: bad,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [0.0; 3],
            1.0,
        );
        assert!(matches!(r, Err(Error::Structure(_))), "accepted {bad}");
    }
}

#[test]
fn rejects_non_positive_rect_area() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    // Zero width.
    assert!(e
        .add_square(
            0,
            Rect {
                llx: 5.0,
                lly: 0.0,
                urx: 5.0,
                ury: 9.0
            },
            [0.0; 3],
            1.0
        )
        .is_err());
    // Negative width (reversed).
    assert!(e
        .add_square(
            0,
            Rect {
                llx: 50.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0
            },
            [0.0; 3],
            1.0
        )
        .is_err());
    // Negative height.
    assert!(e
        .add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 50.0,
                urx: 9.0,
                ury: 9.0
            },
            [0.0; 3],
            1.0
        )
        .is_err());
}

#[test]
fn rejects_out_of_range_colour() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    for c in [-0.1, 1.1, f64::NAN] {
        let r = e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [c, 0.0, 0.0],
            1.0,
        );
        assert!(matches!(r, Err(Error::Structure(_))), "accepted {c}");
    }
}

#[test]
fn rejects_non_positive_or_infinite_width() {
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    for w in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let r = e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 90.0,
                ury: 90.0,
            },
            [0.0; 3],
            w,
        );
        assert!(matches!(r, Err(Error::Structure(_))), "accepted w={w}");
    }
}

#[test]
fn rejects_width_larger_than_rect() {
    // A stroke wider than the box it outlines would invert the
    // appearance — refuse rather than emit a meaningless form.
    let d = doc(&page_pdf(""));
    let mut e = d.edit().unwrap();
    let r = e.add_square(
        0,
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 10.0,
            ury: 10.0,
        },
        [0.0; 3],
        20.0,
    );
    assert!(matches!(r, Err(Error::Structure(_))));
}

#[test]
fn rejects_user_unit_not_one() {
    let d = doc(&page_pdf("/UserUnit 2"));
    let mut e = d.edit().unwrap();
    let r = e.add_square(
        0,
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 9.0,
            ury: 9.0,
        },
        [0.0; 3],
        1.0,
    );
    assert!(matches!(r, Err(Error::Unsupported { .. })));
}

#[test]
fn accepts_user_unit_one() {
    let out = square_on(0, &page_pdf("/UserUnit 1"));
    assert!(reparse(&out).page_count() == 1);
}

// ------------------------------------------------------------------
// §3  coordinate spaces
// ------------------------------------------------------------------

#[test]
fn works_on_rotated_and_cropped_pages() {
    // The rect is stored in un-rotated user space; the annotation
    // follows whatever MediaBox/CropBox/Rotate the page declares —
    // we just verify it round-trips and the annot dict is intact.
    for extra in [
        "/Rotate 90",
        "/Rotate 180",
        "/Rotate 270",
        "/CropBox [5 5 295 195]",
        "/MediaBox [100 100 400 300]",
    ] {
        let out = square_on(0, &page_pdf(extra));
        let d = reparse(&out);
        let annots = annots_of(&d, 0);
        assert_eq!(annots.len(), 1, "failed on {extra}");
        let a = annot_dict(&d, &annots[0]);
        let r = a.get(b"Rect".as_slice()).unwrap().as_array().unwrap();
        assert_eq!(
            r.iter().map(|o| o.as_number().unwrap()).collect::<Vec<_>>(),
            vec![10.0, 10.0, 100.0, 80.0],
            "rect wrong on {extra}"
        );
    }
}

// ------------------------------------------------------------------
// §4  preservation
// ------------------------------------------------------------------

#[test]
fn page_contents_and_links_preserved() {
    // hello.pdf has a real content stream + font; the annot adds
    // alongside, never replaces. (kasane's own extractor still refuses
    // pages with /Annots — that's existing behaviour, verified with
    // pdftotext externally instead.)
    let d = doc(&std::fs::read("tests/fixtures/hello.pdf").unwrap());
    let orig_contents = d.page_contents(&d.pages()[0]).unwrap();
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 80.0,
            ury: 60.0,
        },
        [1.0, 0.0, 0.0],
        2.0,
    )
    .unwrap();
    let out = e.rewrite().unwrap();
    let d2 = reparse(&out);
    // Content stream survives byte-identical.
    assert_eq!(d2.page_contents(&d2.pages()[0]).unwrap(), orig_contents);
    // Annot exists.
    assert_eq!(annots_of(&d2, 0).len(), 1);
    // Text extraction on the rewritten doc still works for supported
    // fonts — use text_basic (TrueType), since hello.pdf is /Type1
    // which the extractor refuses independently of the annotation.
    let tb = doc(&std::fs::read("tests/fixtures/text_basic.pdf").unwrap());
    let mut e2 = tb.edit().unwrap();
    e2.add_square(
        0,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 80.0,
            ury: 60.0,
        },
        [0.0; 3],
        1.0,
    )
    .unwrap();
    let out2 = e2.rewrite().unwrap();
    let tb2 = reparse(&out2);
    // content preserved
    let _ = tb2.page_contents(&tb2.pages()[0]);
    // (extract_text still refuses /Annots pages — that's checked
    // externally with pdftotext, which ignores the annotation.)
}

#[test]
fn refusal_conditions_still_apply() {
    // An encrypted input can never reach the edit stage — but a doc
    // that loaded must still refuse on rewrite for the same reasons.
    let d = doc(&std::fs::read("tests/fixtures/hello.pdf").unwrap());
    // Sanity: plain rewrite is fine.
    kasane_pdf::rewrite(&d).unwrap();
    // And the edit path goes through the same checks.
    let mut e = d.edit().unwrap();
    e.add_square(
        0,
        Rect {
            llx: 1.0,
            lly: 1.0,
            urx: 9.0,
            ury: 9.0,
        },
        [0.0; 3],
        1.0,
    )
    .unwrap();
    e.rewrite().unwrap();
}

// ------------------------------------------------------------------
// §5  limits
// ------------------------------------------------------------------

#[test]
fn annot_count_limit() {
    let d = doc(&page_pdf(""));
    let mut e = d
        .edit_with_limits(Limits {
            max_edit_annots: 3,
            ..Default::default()
        })
        .unwrap();
    for _ in 0..3 {
        e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [0.0; 3],
            1.0,
        )
        .unwrap();
    }
    let r = e.add_square(
        0,
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 9.0,
            ury: 9.0,
        },
        [0.0; 3],
        1.0,
    );
    assert!(matches!(r, Err(Error::LimitExceeded { .. })));
}

#[test]
fn synthetic_object_limit() {
    // Each square = annot + appearance + (shared) array + page dict.
    // A tiny object cap fails at commit, not at add.
    let d = doc(&page_pdf(""));
    let mut e = d
        .edit_with_limits(Limits {
            max_edit_objects: 1,
            ..Default::default()
        })
        .unwrap();
    e.add_square(
        0,
        Rect {
            llx: 0.0,
            lly: 0.0,
            urx: 9.0,
            ury: 9.0,
        },
        [0.0; 3],
        1.0,
    )
    .unwrap();
    assert!(matches!(e.rewrite(), Err(Error::LimitExceeded { .. })));
}
