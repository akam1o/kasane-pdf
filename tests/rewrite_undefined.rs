//! `rewrite` and references to undefined objects (ISO 32000-1 §7.3.10):
//! a reference with no in-use or compressed cross-reference entry for
//! its number and generation is a reference to the null object — the
//! rewrite keeps it, pointing at an indirect `null`, and records
//! `Warning::UndefinedReference`. A defined object that cannot be read
//! is an error, never null.

use kasane_pdf::{audit_residuals, rewrite, Document, Error, IndirectRef, Limits, Object, Warning};

/// Objects (number, body) plus extra cross-reference rows
/// `(number, generation, 'n' | 'f', offset or None = point into garbage)`.
fn pdf(
    catalog_extra: &str,
    objs: &[(u32, &str)],
    extra_rows: &[(u32, u16, u8, Option<u64>)],
) -> Vec<u8> {
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u16, u8, u64)> = Vec::new();
    let mut all: Vec<(u32, String)> = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
        ),
    ];
    all.extend(objs.iter().map(|(n, s)| (*n, s.to_string())));
    for (n, body) in &all {
        rows.push((*n, 0, b'n', b.len() as u64));
        b.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let garbage = b.len() as u64;
    b.extend_from_slice(b"%% not an object %%\n");
    for &(n, g, k, off) in extra_rows {
        rows.push((n, g, k, off.unwrap_or(garbage)));
    }
    rows.push((0, 65535, b'f', 0));
    rows.sort();
    let x = b.len();
    b.extend_from_slice(b"xref\n");
    for (n, g, k, off) in &rows {
        b.extend_from_slice(format!("{n} 1\n{off:010} {g:05} {} \n", *k as char).as_bytes());
    }
    let size = rows.iter().map(|r| r.0).max().unwrap() + 1;
    b.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
    );
    b
}

fn r(number: u32, generation: u16) -> IndirectRef {
    IndirectRef { number, generation }
}

/// The catalog entry `key` of `doc`, resolved one level.
fn entry(doc: &Document, key: &str) -> Option<Object> {
    doc.catalog().get(key.as_bytes()).cloned()
}

/// Rewrite `doc`, reparse strictly, and check that every reachable
/// reference of the output is defined.
fn rewritten(doc: &Document) -> Document {
    let out = rewrite(doc).unwrap();
    let d2 = Document::from_bytes(&out).unwrap();
    let audit = audit_residuals(&d2, &Limits::default()).unwrap();
    assert_eq!(
        audit.reachability.missing_references, 0,
        "dangling reference in the output"
    );
    assert!(d2
        .take_warnings()
        .iter()
        .all(|w| !matches!(w, Warning::UndefinedReference { .. })));
    d2
}

fn undefined_warnings(doc: &Document) -> Vec<u32> {
    let mut v: Vec<u32> = doc
        .take_warnings()
        .into_iter()
        .filter_map(|w| match w {
            Warning::UndefinedReference { reference } => Some(reference.number),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

#[test]
fn an_undefined_number_becomes_an_indirect_null() {
    let doc =
        Document::from_bytes(&pdf("/Extra 99 0 R /Keep 4 0 R", &[(4, "(kept)")], &[])).unwrap();
    let d2 = rewritten(&doc);
    assert_eq!(undefined_warnings(&doc), vec![99]);
    // The key and the reference stay; its target is null.
    let Some(Object::Ref(x)) = entry(&d2, "Extra") else {
        panic!("/Extra is not a reference")
    };
    assert_eq!(d2.resolve(x).unwrap(), Object::Null);
    let Some(Object::Ref(k)) = entry(&d2, "Keep") else {
        panic!()
    };
    assert_eq!(d2.resolve(k).unwrap(), Object::String(b"kept".to_vec()));
}

#[test]
fn another_generation_and_a_free_entry_are_undefined() {
    // 4 0 is defined; 4 1 R is another object (undefined). 7 is free.
    let doc = Document::from_bytes(&pdf(
        "/Gen 4 1 R /Free 7 0 R /Keep 4 0 R",
        &[(4, "(four)")],
        &[(7, 1, b'f', Some(0))],
    ))
    .unwrap();
    let d2 = rewritten(&doc);
    for key in ["Gen", "Free"] {
        let Some(Object::Ref(x)) = entry(&d2, key) else {
            panic!("{key}")
        };
        assert_eq!(d2.resolve(x).unwrap(), Object::Null, "{key}");
    }
    let Some(Object::Ref(k)) = entry(&d2, "Keep") else {
        panic!()
    };
    assert_eq!(d2.resolve(k).unwrap(), Object::String(b"four".to_vec()));
    assert_eq!(undefined_warnings(&doc), vec![4, 7]);
}

#[test]
fn positions_in_arrays_and_dictionaries_are_kept() {
    let doc = Document::from_bytes(&pdf(
        "/Arr [1 99 0 R 3] /Dict << /A 98 0 R /B 2 >> /Explicit null",
        &[],
        &[],
    ))
    .unwrap();
    let d2 = rewritten(&doc);
    let Some(Object::Array(a)) = entry(&d2, "Arr") else {
        panic!()
    };
    assert_eq!(a.len(), 3);
    assert_eq!(a[0], Object::Integer(1));
    assert_eq!(d2.deref(&a[1]).unwrap(), Object::Null);
    assert_eq!(a[2], Object::Integer(3));
    let Some(Object::Dict(d)) = entry(&d2, "Dict") else {
        panic!()
    };
    assert_eq!(
        d2.deref(d.get(b"A".as_slice()).unwrap()).unwrap(),
        Object::Null
    );
    assert_eq!(d.get(b"B".as_slice()), Some(&Object::Integer(2)));
    // A direct null is an absent entry (§7.3.7) before and after.
    assert_eq!(entry(&doc, "Explicit"), None);
    assert_eq!(entry(&d2, "Explicit"), None);
}

#[test]
fn a_chain_to_an_undefined_object_ends_in_null() {
    // 5 0 obj is the reference 6 0 R; 6 is undefined.
    let doc = Document::from_bytes(&pdf("/Chain 5 0 R", &[(5, "6 0 R")], &[])).unwrap();
    let d2 = rewritten(&doc);
    let Some(Object::Ref(x)) = entry(&d2, "Chain") else {
        panic!()
    };
    assert_eq!(d2.resolve(x).unwrap(), Object::Null);
}

#[test]
fn a_shared_undefined_target_is_one_null_object_and_several_are_several() {
    let doc = Document::from_bytes(&pdf("/A 99 0 R /B [99 0 R] /C 98 0 R", &[], &[])).unwrap();
    let d2 = rewritten(&doc);
    let Some(Object::Ref(a)) = entry(&d2, "A") else {
        panic!()
    };
    let Some(Object::Array(b)) = entry(&d2, "B") else {
        panic!()
    };
    let Some(Object::Ref(c)) = entry(&d2, "C") else {
        panic!()
    };
    assert_eq!(b[0], Object::Ref(a), "shared target renumbered once");
    assert_ne!(a, c);
    assert_eq!(undefined_warnings(&doc), vec![98, 99]);
}

#[test]
fn defined_but_unreadable_objects_are_never_null() {
    // A reference cycle between two objects that are only references.
    let cyc = Document::from_bytes(&pdf("/Cyc 5 0 R", &[(5, "6 0 R"), (6, "5 0 R")], &[])).unwrap();
    assert!(
        matches!(rewrite(&cyc), Err(Error::CycleDetected { .. })),
        "{:?}",
        rewrite(&cyc)
    );
    // An entry whose offset points at bytes that are not the object.
    let broken = Document::from_bytes(&pdf("/Broken 8 0 R", &[], &[(8, 0, b'n', None)])).unwrap();
    assert!(
        matches!(
            rewrite(&broken),
            Err(Error::Parse(_)) | Err(Error::UnexpectedEof { .. })
        ),
        "{:?}",
        rewrite(&broken)
    );
}

#[test]
fn a_missing_object_stream_member_is_an_error() {
    // Object 13 is declared compressed in object stream 11 at index 1,
    // which holds one member only.
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    let mut obj = |b: &mut Vec<u8>, n: u32, body: &str| {
        offs.push((n, b.len() as u64));
        b.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    obj(
        &mut b,
        1,
        "<< /Type /Catalog /Pages 2 0 R /Packed 13 0 R >>",
    );
    obj(&mut b, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    obj(
        &mut b,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
    );
    let data = "12 0 (twelve) ";
    obj(
        &mut b,
        11,
        &format!(
            "<< /Type /ObjStm /N 1 /First 5 /Length {} >>\nstream\n{data}\nendstream",
            data.len()
        ),
    );
    let x = b.len() as u64;
    let mut rows: Vec<(u32, u8, u64, u64)> = offs.iter().map(|&(n, o)| (n, 1, o, 0)).collect();
    rows.push((12, 2, 11, 0));
    rows.push((13, 2, 11, 1));
    rows.push((14, 1, x, 0));
    rows.sort();
    let mut idx = String::new();
    let mut data = Vec::new();
    for (n, t, f2, f3) in &rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(*t);
        data.extend_from_slice(&(*f2 as u32).to_be_bytes());
        data.extend_from_slice(&(*f3 as u16).to_be_bytes());
    }
    b.extend_from_slice(
        format!("14 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 15 /Root 1 0 R /Length {} >>\nstream\n", data.len()).as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    let doc = Document::from_bytes(&b).unwrap();
    assert!(doc.resolve(r(13, 0)).is_err());
    assert!(rewrite(&doc).is_err());
}

#[test]
fn the_refusal_checks_read_undefined_as_absent() {
    // /Perms, /AcroForm /Fields and a signature field's /V that point
    // at undefined objects are absent — not a signature, not an error.
    let doc = Document::from_bytes(&pdf(
        "/Perms 97 0 R /AcroForm << /Fields [5 0 R 96 0 R] >>",
        &[(5, "<< /FT /Sig /T (sig) /V 95 0 R >>")],
        &[],
    ))
    .unwrap();
    let d2 = rewritten(&doc);
    assert_eq!(undefined_warnings(&doc), vec![95, 96, 97]);
    assert!(d2.catalog().contains_key(b"Perms".as_slice()));
    // A real usage-rights signature is still refused.
    let signed = Document::from_bytes(&pdf(
        "/Perms << /UR3 5 0 R >>",
        &[(5, "<< /Type /Sig >>")],
        &[],
    ))
    .unwrap();
    assert!(matches!(
        rewrite(&signed),
        Err(Error::SignaturePolicy { .. })
    ));
    // A signature field whose /V is defined is still refused.
    let field = Document::from_bytes(&pdf(
        "/AcroForm << /Fields [5 0 R] >>",
        &[
            (5, "<< /FT /Sig /T (sig) /V 6 0 R >>"),
            (6, "<< /Type /Sig /ByteRange [0 1 2 3] >>"),
        ],
        &[],
    ))
    .unwrap();
    assert!(rewrite(&field).is_err());
}

#[test]
fn a_recovered_document_rewrites_its_undefined_references() {
    let mut b = pdf("/Extra 99 0 R", &[], &[]);
    let at = b.windows(9).rposition(|w| w == b"startxref").unwrap() + 10;
    let end = at + b[at..].iter().position(|c| *c == b'\n').unwrap();
    b.splice(at..end, *b"5");
    let doc = Document::from_bytes_recovering(&b).unwrap();
    assert!(doc.recovery_mode());
    let d2 = rewritten(&doc);
    let Some(Object::Ref(x)) = entry(&d2, "Extra") else {
        panic!()
    };
    assert_eq!(d2.resolve(x).unwrap(), Object::Null);
}
