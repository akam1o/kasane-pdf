//! `rewrite` of PDF 2.0 documents (ISO 32000-2): the output declares
//! 2.0 (header; a catalog `/Version` is carried), has the trailer `/ID`
//! 2.0 requires (Table 15; the permanent first element carried, the
//! second derived from the written body, §14.4), and carries what 2.0
//! adds — page-level `/Metadata`, associated files (`/AF`, §14.13) with
//! their embedded files, document parts (`/DPartRoot`, §14.12) — as
//! ordinary objects with renumbered references and sharing kept. A
//! version above 2.0 is refused; 1.x output is unchanged (no `/ID`).

use kasane_pdf::{rewrite, Dict, Document, Error, Object};

/// Header `%PDF-{header}`; object 1 is the catalog (with `catalog_extra`),
/// 2 the page tree, 3 and 4 two pages (with `page_extra`), then `objs`.
fn pdf(
    header: &str,
    catalog_extra: &str,
    page_extra: [&str; 2],
    objs: &[(u32, Vec<u8>)],
    trailer_extra: &str,
) -> Vec<u8> {
    let mut all: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {} >>",
                page_extra[0]
            )
            .into_bytes(),
        ),
        (
            4,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {} >>",
                page_extra[1]
            )
            .into_bytes(),
        ),
    ];
    all.extend(objs.iter().cloned());
    let mut b = format!("%PDF-{header}\n").into_bytes();
    let mut offs = Vec::new();
    for (n, body) in &all {
        offs.push((*n, b.len()));
        b.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(b"\nendobj\n");
    }
    let garbage = b.len();
    b.extend_from_slice(b"% not an object\n");
    let size = all.iter().map(|o| o.0).max().unwrap().max(99) + 1;
    let x = b.len();
    b.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, o)) => b.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            // 99: in use, but its offset is not an object (unreadable).
            None if n == 99 => b.extend_from_slice(format!("{garbage:010} 00000 n \n").as_bytes()),
            None => b.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    b.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{x}\n%%EOF\n")
            .as_bytes(),
    );
    b
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

fn flate(dict: &str, data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    stream(
        &format!("{dict} /Filter /FlateDecode"),
        &e.finish().unwrap(),
    )
}

fn rw(bytes: &[u8]) -> (Vec<u8>, Document) {
    let doc = Document::from_bytes(bytes).unwrap();
    let out = rewrite(&doc).unwrap();
    let d2 = Document::from_bytes(&out).unwrap();
    (out, d2)
}

fn header(out: &[u8]) -> &[u8] {
    &out[..8]
}

/// The trailer `/ID` of a written file, read from its bytes.
fn trailer_id(out: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let t = &out[out.windows(7).rposition(|w| w == b"trailer")?..];
    let t = std::str::from_utf8(t).ok()?;
    let i = t.find("/ID [<")? + 6;
    let rest = &t[i..];
    let a_end = rest.find('>')?;
    let b_start = rest.find("<")? + 1;
    let b_end = b_start + rest[b_start..].find('>')?;
    let hex = |s: &str| {
        (0..s.len())
            .step_by(2)
            .map(|k| u8::from_str_radix(&s[k..k + 2], 16).unwrap())
            .collect()
    };
    Some((hex(&rest[..a_end]), hex(&rest[b_start..b_end])))
}

fn entry(doc: &Document, d: &Dict, key: &str) -> Object {
    doc.deref(d.get(key.as_bytes()).unwrap()).unwrap()
}

fn page(doc: &Document, i: usize) -> Dict {
    doc.resolve(doc.pages()[i].object_ref().unwrap())
        .unwrap()
        .as_dict()
        .unwrap()
        .clone()
}

fn decoded(doc: &Document, o: &Object) -> Vec<u8> {
    let Object::Stream { dict, data } = doc.deref(o).unwrap() else {
        panic!("not a stream: {o:?}")
    };
    if dict.contains_key(b"Filter".as_slice()) {
        use std::io::Read;
        let mut v = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .read_to_end(&mut v)
            .unwrap();
        v
    } else {
        data
    }
}

#[test]
fn versions_declared_2_0_are_written_as_2_0() {
    // Header 2.0; catalog 2.0 under a 1.7 header; header 2.0 with a
    // lower catalog entry (the later one applies, §7.2.2).
    for (h, cat) in [
        ("2.0", ""),
        ("1.7", "/Version /2.0"),
        ("2.0", "/Version /1.7"),
    ] {
        let (out, d2) = rw(&pdf(h, cat, ["", ""], &[], ""));
        assert_eq!(header(&out), b"%PDF-2.0", "{h} {cat}");
        assert_eq!(d2.header_version(), Some((2, 0)));
        // The catalog entry is carried as it was.
        let want = (!cat.is_empty()).then(|| Object::Name(cat.as_bytes()[10..].to_vec()));
        assert_eq!(
            d2.catalog().get(b"Version".as_slice()).cloned(),
            want,
            "{h} {cat}"
        );
        let (first, second) = trailer_id(&out).expect("PDF 2.0 requires a trailer /ID");
        assert_eq!((first.len(), second.len()), (16, 16));
    }
}

#[test]
fn versions_above_2_0_are_refused() {
    for (h, cat) in [("2.1", ""), ("1.7", "/Version /2.1"), ("3.0", "")] {
        let doc = Document::from_bytes(&pdf(h, cat, ["", ""], &[], "")).unwrap();
        match rewrite(&doc) {
            Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "PDF version"),
            other => panic!("{h} {cat}: {:?}", other.map(|_| "rewritten")),
        }
    }
}

#[test]
fn the_trailer_id() {
    // The permanent identifier is carried, the changing one is the
    // written body's digest: the same for the same input.
    let a = [0xA1u8; 16];
    let b = [0xB2u8; 16];
    let hex = |x: &[u8]| x.iter().map(|c| format!("{c:02X}")).collect::<String>();
    let src = pdf(
        "2.0",
        "",
        ["", ""],
        &[],
        &format!("/ID [<{}> <{}>]", hex(&a), hex(&b)),
    );
    let (out, _) = rw(&src);
    let (first, second) = trailer_id(&out).unwrap();
    assert_eq!(first, a);
    assert_ne!(second, b);
    let (again, _) = rw(&src);
    assert_eq!(out, again, "deterministic");
    // Without an input /ID both are the body digest.
    let (out, _) = rw(&pdf("2.0", "", ["", ""], &[], ""));
    let (first, second) = trailer_id(&out).unwrap();
    assert_eq!(first, second);
    // A malformed input /ID is not carried.
    let (out, _) = rw(&pdf("2.0", "", ["", ""], &[], "/ID 7"));
    let (first, second) = trailer_id(&out).unwrap();
    assert_eq!(first, second);
    // 1.x output keeps writing no /ID.
    let (out, _) = rw(&pdf(
        "1.7",
        "",
        ["", ""],
        &[],
        &format!("/ID [<{}> <{}>]", hex(&a), hex(&b)),
    ));
    assert_eq!(header(&out), b"%PDF-1.7");
    assert!(trailer_id(&out).is_none());
}

#[test]
fn page_metadata_is_carried() {
    let x0 = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">zero</x:xmpmeta>";
    let x1 = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">one</x:xmpmeta>";
    let src = pdf(
        "2.0",
        "",
        ["/Metadata 50 0 R", "/Metadata 51 0 R"],
        &[
            (50, flate("/Type /Metadata /Subtype /XML", x0)),
            (51, stream("/Type /Metadata /Subtype /XML", x1)),
        ],
        "",
    );
    let (_, d2) = rw(&src);
    for (i, want) in [(0, &x0[..]), (1, &x1[..])] {
        let p = page(&d2, i);
        assert_eq!(
            decoded(&d2, p.get(b"Metadata".as_slice()).unwrap()),
            want,
            "page {i}"
        );
    }
}

#[test]
fn associated_files_and_embedded_files_are_carried_with_sharing() {
    let csv = b"a,b\n1,2\n";
    let src = pdf(
        "2.0",
        "/AF [10 0 R 11 0 R]",
        ["/AF [11 0 R]", "/AF [12 0 R]"],
        &[
            (
                10,
                b"<< /Type /Filespec /F (data.csv) /UF (data.csv) /Desc (numbers) /AFRelationship /Data /EF << /F 13 0 R /UF 13 0 R >> >>".to_vec(),
            ),
            (11, b"<< /Type /Filespec /F (src.xml) /UF (src.xml) /AFRelationship /Source /EF << /F 14 0 R >> >>".to_vec()),
            (12, b"<< /Type /Filespec /F (ext.pdf) /AFRelationship /Unspecified >>".to_vec()),
            (
                13,
                flate("/Type /EmbeddedFile /Subtype /text#2Fcsv /Params << /Size 8 /ModDate (D:20260101000000Z) >>", csv),
            ),
            (14, stream("/Type /EmbeddedFile", b"<x/>")),
        ],
        "",
    );
    let (_, d2) = rw(&src);
    let cat = d2.catalog().clone();
    let af = entry(&d2, &cat, "AF");
    let af = af.as_array().unwrap();
    assert_eq!(af.len(), 2);
    let fs0 = d2.deref(&af[0]).unwrap().as_dict().unwrap().clone();
    assert_eq!(
        fs0.get(b"AFRelationship".as_slice()),
        Some(&Object::Name(b"Data".to_vec()))
    );
    assert_eq!(
        fs0.get(b"Desc".as_slice()),
        Some(&Object::String(b"numbers".to_vec()))
    );
    let ef = entry(&d2, &fs0, "EF");
    let ef = ef.as_dict().unwrap();
    // /F and /UF still share one embedded-file stream.
    assert_eq!(ef.get(b"F".as_slice()), ef.get(b"UF".as_slice()));
    assert_eq!(decoded(&d2, ef.get(b"F".as_slice()).unwrap()), csv);
    let Object::Stream { dict, .. } = d2.deref(ef.get(b"F".as_slice()).unwrap()).unwrap() else {
        panic!()
    };
    let params = d2.deref(dict.get(b"Params".as_slice()).unwrap()).unwrap();
    assert_eq!(
        params.as_dict().unwrap().get(b"Size".as_slice()),
        Some(&Object::Integer(8))
    );
    // The catalog's second entry and the first page's entry are one
    // file specification.
    let p0 = page(&d2, 0);
    let p0af = entry(&d2, &p0, "AF");
    assert_eq!(p0af.as_array().unwrap()[0], af[1]);
    let p1 = page(&d2, 1);
    let p1af = entry(&d2, &p1, "AF");
    let fs2 = d2.deref(&p1af.as_array().unwrap()[0]).unwrap();
    assert_eq!(
        fs2.as_dict().unwrap().get(b"F".as_slice()),
        Some(&Object::String(b"ext.pdf".to_vec()))
    );
}

/// A DPart tree: root node 21 with two leaves, 22 (page 0) and 23
/// (page 1); each page points back at its leaf.
fn dparts(kids: &str, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (20, b"<< /Type /DPartRoot /DPartRootNode 21 0 R /RecordLevel 1 /NodeNameList [/Doc /Rec] >>".to_vec()),
        (21, format!("<< /Type /DPart /Parent 20 0 R /DParts {kids} /DPM << /Doc << /Title (t) >> >> >>").into_bytes()),
        (22, b"<< /Type /DPart /Parent 21 0 R /Start 3 0 R /End 3 0 R /DPM << /Rec << /Id 1 >> >> >>".to_vec()),
        (23, b"<< /Type /DPart /Parent 21 0 R /Start 4 0 R /DPM << /Rec << /Id 2 >> >> >>".to_vec()),
    ];
    objs.extend(extra.iter().cloned());
    pdf(
        "2.0",
        "/DPartRoot 20 0 R",
        ["/DPart 22 0 R", "/DPart 23 0 R"],
        &objs,
        "",
    )
}

#[test]
fn document_parts_are_carried() {
    let (_, d2) = rw(&dparts("[[22 0 R 23 0 R]]", &[]));
    let cat = d2.catalog().clone();
    let root_ref = cat.get(b"DPartRoot".as_slice()).unwrap().clone();
    let root = d2.deref(&root_ref).unwrap().as_dict().unwrap().clone();
    assert_eq!(
        root.get(b"RecordLevel".as_slice()),
        Some(&Object::Integer(1))
    );
    let node_ref = root.get(b"DPartRootNode".as_slice()).unwrap().clone();
    let node = d2.deref(&node_ref).unwrap().as_dict().unwrap().clone();
    assert_eq!(node.get(b"Parent".as_slice()), Some(&root_ref));
    let groups = entry(&d2, &node, "DParts");
    let leaves = groups.as_array().unwrap()[0].as_array().unwrap().to_vec();
    assert_eq!(leaves.len(), 2);
    for (i, leaf_ref) in leaves.iter().enumerate() {
        let leaf = d2.deref(leaf_ref).unwrap().as_dict().unwrap().clone();
        assert_eq!(leaf.get(b"Parent".as_slice()), Some(&node_ref));
        // /Start names page i, and page i points back at this leaf.
        let page_ref = d2.pages()[i].object_ref().unwrap();
        assert_eq!(leaf.get(b"Start".as_slice()), Some(&Object::Ref(page_ref)));
        assert_eq!(page(&d2, i).get(b"DPart".as_slice()), Some(leaf_ref));
        let dpm = entry(&d2, &leaf, "DPM");
        let rec = dpm
            .as_dict()
            .unwrap()
            .get(b"Rec".as_slice())
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        assert_eq!(
            rec.get(b"Id".as_slice()),
            Some(&Object::Integer(i as i64 + 1))
        );
    }
}

#[test]
fn document_part_references_follow_the_reference_rules() {
    // An undefined node is a reference to null (§7.3.10, W1): kept.
    let (_, d2) = rw(&dparts("[[22 0 R 23 0 R 77 0 R]]", &[]));
    let cat = d2.catalog().clone();
    let root = entry(&d2, &cat, "DPartRoot");
    let node = entry(&d2, root.as_dict().unwrap(), "DPartRootNode");
    let groups = entry(&d2, node.as_dict().unwrap(), "DParts");
    let leaves = groups.as_array().unwrap()[0].as_array().unwrap().to_vec();
    assert_eq!(leaves.len(), 3);
    assert_eq!(d2.deref(&leaves[2]).unwrap(), Object::Null);
    // A node whose object cannot be read is an error, never null.
    let doc = Document::from_bytes(&dparts("[[22 0 R 23 0 R 99 0 R]]", &[])).unwrap();
    assert!(rewrite(&doc).is_err());
    // A cyclic tree (a node its own descendant) is carried as it is.
    let src = dparts(
        "[[22 0 R 23 0 R 24 0 R]]",
        &[(
            24,
            b"<< /Type /DPart /Parent 21 0 R /DParts [[21 0 R]] >>".to_vec(),
        )],
    );
    let (_, d2) = rw(&src);
    assert!(d2.catalog().contains_key(b"DPartRoot".as_slice()));
}

#[test]
fn mixed_1_x_and_2_0_structures() {
    // A 2.0 document with 1.x structures (optional content, an XFA
    // form) and a 1.7 document with an /AF (PDF/A-3 uses /AF in 1.7
    // files): each keeps its own version; nothing is upgraded.
    let ocp = "/OCProperties << /OCGs [30 0 R] /D << /Order [30 0 R] >> >>";
    let src = pdf(
        "2.0",
        &format!("{ocp} /AcroForm << /Fields [] /XFA [(template) 31 0 R] >> /AF [12 0 R]"),
        ["", ""],
        &[
            (30, b"<< /Type /OCG /Name (layer) >>".to_vec()),
            (31, stream("", b"<template/>")),
            (
                12,
                b"<< /Type /Filespec /F (x) /AFRelationship /Data >>".to_vec(),
            ),
        ],
        "",
    );
    let (out, d2) = rw(&src);
    assert_eq!(header(&out), b"%PDF-2.0");
    for k in ["OCProperties", "AcroForm", "AF"] {
        assert!(d2.catalog().contains_key(k.as_bytes()), "{k}");
    }
    let (out, _) = rw(&pdf(
        "1.7",
        "/AF [12 0 R]",
        ["", ""],
        &[(
            12,
            b"<< /Type /Filespec /F (x) /AFRelationship /Data >>".to_vec(),
        )],
        "",
    ));
    assert_eq!(header(&out), b"%PDF-1.7");
    assert!(trailer_id(&out).is_none());
}

#[test]
fn refusal_policies_apply_to_2_0() {
    let doc = Document::from_bytes(&pdf(
        "2.0",
        "/Perms << /UR3 30 0 R >>",
        ["", ""],
        &[(30, b"<< /Type /Sig /Filter /Adobe.PPKLite >>".to_vec())],
        "",
    ))
    .unwrap();
    assert!(matches!(rewrite(&doc), Err(Error::SignaturePolicy { .. })));
    let doc = Document::from_bytes(&pdf(
        "2.0",
        "/OpenAction << /S /JavaScript /JS (app.alert(1)) >>",
        ["", ""],
        &[],
        "",
    ))
    .unwrap();
    assert!(matches!(rewrite(&doc), Err(Error::Unsupported { .. })));
    let doc = Document::from_bytes(&pdf(
        "2.0",
        "/AcroForm << /Fields [] /XFA [(datasets) 31 0 R] >>",
        ["", ""],
        &[(
            31,
            stream(
                "",
                b"<ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"/>",
            ),
        )],
        "",
    ))
    .unwrap();
    assert!(matches!(rewrite(&doc), Err(Error::SignaturePolicy { .. })));
}

#[test]
fn metadata_removal_does_not_carry_the_permanent_identifier() {
    // `remove_document_metadata` drops /Info and XMP; the input's
    // permanent /ID identifies the document too, so a 2.0 output gets
    // a fresh one (both elements the body digest).
    let src = pdf(
        "2.0",
        "",
        ["", ""],
        &[],
        &format!("/ID [<{}> <{}>]", "A1".repeat(16), "B2".repeat(16)),
    );
    let doc = Document::from_bytes(&src).unwrap();
    let r = kasane_pdf::remove_document_metadata(&doc, &kasane_pdf::Limits::default()).unwrap();
    let (first, second) = trailer_id(&r.output).unwrap();
    assert_eq!(first, second);
    assert_ne!(first, vec![0xA1; 16]);
}
