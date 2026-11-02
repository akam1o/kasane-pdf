//! `rewrite` of XFA forms (ISO 32000-1 §12.7.8): the `/XFA` stream or
//! packet array is carried verbatim — names, order, duplicates, every
//! packet's stream bytes — with renumbered references; the catalog's
//! `/NeedsRendering` is kept. An XFA packet holding an XML signature
//! keeps the document refused, like every other signature, and so does
//! a usage-rights signature.

use kasane_pdf::{rewrite, Document, Error, Limits, Object};

/// Objects (number, body bytes); object 1 is the catalog, 2 the page
/// tree, 3 the page.
fn pdf(catalog_extra: &str, objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut all: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_vec(),
        ),
    ];
    all.extend(objs);
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (n, body) in &all {
        offs.push((*n, b.len()));
        b.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(b"\nendobj\n");
    }
    let size = all.iter().map(|o| o.0).max().unwrap() + 1;
    let x = b.len();
    b.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, o)) => b.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => b.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    b.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
    );
    b
}

fn stream(extra: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {extra} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn unzlib(data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut v = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut v)
        .unwrap();
    v
}

fn flate(extra: &str, data: &[u8]) -> Vec<u8> {
    stream(&format!("{extra} /Filter /FlateDecode"), &zlib(data))
}

/// One XFA packet: name (`None` for the single stream), `/Filter`, raw
/// stream bytes.
type Packet = (Option<Vec<u8>>, Option<Object>, Vec<u8>);

/// The packets of `doc`'s `/XFA`.
fn packets(doc: &Document) -> Vec<Packet> {
    let af = doc
        .deref(doc.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let xfa = af
        .as_dict()
        .unwrap()
        .get(b"XFA".as_slice())
        .unwrap()
        .clone();
    let raw = |o: &Object| match doc.deref(o).unwrap() {
        Object::Stream { dict, data } => (dict.get(b"Filter".as_slice()).cloned(), data),
        other => panic!("not a stream: {other:?}"),
    };
    match doc.deref(&xfa).unwrap() {
        Object::Array(a) => a
            .chunks(2)
            .map(|p| {
                let name = match doc.deref(&p[0]).unwrap() {
                    Object::String(s) => s,
                    other => panic!("name {other:?}"),
                };
                let (f, d) = raw(&p[1]);
                (Some(name), f, d)
            })
            .collect(),
        Object::Stream { .. } => {
            let (f, d) = raw(&xfa);
            vec![(None, f, d)]
        }
        other => panic!("XFA {other:?}"),
    }
}

/// Rewrite, reparse strictly, and require identical packets.
fn round_trip(bytes: &[u8]) -> Document {
    let doc = Document::from_bytes(bytes).unwrap();
    let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
    assert_eq!(packets(&doc), packets(&out), "XFA packets changed");
    out
}

fn xfa_array(names_streams: &[(&str, u32)]) -> String {
    let parts: Vec<String> = names_streams
        .iter()
        .map(|(n, o)| format!("({n}) {o} 0 R"))
        .collect();
    format!("[{}]", parts.join(" "))
}

const TEMPLATE: &[u8] = b"<template xmlns=\"http://www.xfa.org/schema/xfa-template/3.3/\"><subform name=\"f\"/></template>";
const DATASETS: &[u8] = b"<xfa:datasets xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\"><xfa:data><f>1</f></xfa:data></xfa:datasets>";

#[test]
fn a_single_stream_is_carried() {
    let xdp = b"<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\"></xdp:xdp>";
    let out = round_trip(&pdf(
        "/AcroForm << /Fields [] /XFA 10 0 R >>",
        vec![(10, stream("", xdp))],
    ));
    assert_eq!(packets(&out)[0].2, xdp);
}

#[test]
fn packets_keep_names_order_duplicates_and_bytes() {
    // preamble, template, datasets, a second datasets, postamble —
    // compressed, plain and chained filters mixed.
    let objs = vec![
        (
            10,
            stream("", b"<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\">"),
        ),
        (11, flate("", TEMPLATE)),
        (12, flate("", DATASETS)),
        (
            13,
            stream(
                "/Filter [/ASCIIHexDecode]",
                format!(
                    "{}>",
                    DATASETS
                        .iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<String>()
                )
                .as_bytes(),
            ),
        ),
        (14, stream("", b"</xdp:xdp>")),
    ];
    let xfa = xfa_array(&[
        ("preamble", 10),
        ("template", 11),
        ("datasets", 12),
        ("datasets", 13),
        ("postamble", 14),
    ]);
    let out = round_trip(&pdf(
        &format!("/AcroForm << /Fields [] /XFA {xfa} >>"),
        objs,
    ));
    let names: Vec<_> = packets(&out).into_iter().map(|p| p.0.unwrap()).collect();
    assert_eq!(
        names,
        [
            &b"preamble"[..],
            b"template",
            b"datasets",
            b"datasets",
            b"postamble"
        ]
        .map(|n| n.to_vec())
    );
}

#[test]
fn decoded_xml_is_unchanged() {
    let doc = Document::from_bytes(&pdf(
        &format!(
            "/AcroForm << /Fields [] /XFA {} >>",
            xfa_array(&[("template", 10), ("datasets", 11)])
        ),
        vec![(10, flate("", TEMPLATE)), (11, flate("", DATASETS))],
    ))
    .unwrap();
    let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
    for (o, want) in [(&out, [TEMPLATE, DATASETS]), (&doc, [TEMPLATE, DATASETS])] {
        let p = packets(o);
        for (i, w) in want.iter().enumerate() {
            let d = unzlib(&p[i].2);
            assert_eq!(&d, w);
        }
    }
}

#[test]
fn indirect_xfa_shared_objects_and_needs_rendering() {
    // /XFA is an indirect array; the template stream is also referenced
    // from a field (shared); /NeedsRendering is set.
    let doc_bytes = pdf(
        "/NeedsRendering true /AcroForm << /XFA 20 0 R /Fields [21 0 R] >>",
        vec![
            (
                20,
                xfa_array(&[("template", 10), ("datasets", 11)]).into_bytes(),
            ),
            (21, b"<< /T (f) /FT /Tx /XFASource 10 0 R >>".to_vec()),
            (10, flate("", TEMPLATE)),
            (11, flate("", DATASETS)),
        ],
    );
    let out = round_trip(&doc_bytes);
    assert_eq!(
        out.catalog().get(b"NeedsRendering".as_slice()),
        Some(&Object::Bool(true))
    );
    let af = out
        .deref(out.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let af = af.as_dict().unwrap();
    let xfa = out.deref(af.get(b"XFA".as_slice()).unwrap()).unwrap();
    let template_ref = xfa.as_array().unwrap()[1].clone();
    let fields = out.deref(af.get(b"Fields".as_slice()).unwrap()).unwrap();
    let field = out.deref(&fields.as_array().unwrap()[0]).unwrap();
    assert_eq!(
        field.as_dict().unwrap().get(b"XFASource".as_slice()),
        Some(&template_ref),
        "the shared stream is one object"
    );
}

#[test]
fn malformed_and_cyclic_xfa_is_carried_as_it_is() {
    // An odd-length array with a non-string name, and an array that
    // contains itself: copied as they are, no panic, no loop.
    for (extra, objs) in [
        (
            "/AcroForm << /Fields [] /XFA [42 10 0 R (orphan)] >>",
            vec![(10, stream("", TEMPLATE))],
        ),
        (
            "/AcroForm << /Fields [] /XFA 20 0 R >>",
            vec![
                (20, b"[(template) 10 0 R (self) 20 0 R]".to_vec()),
                (10, stream("", TEMPLATE)),
            ],
        ),
        ("/AcroForm << /Fields [] /XFA 7 >>", vec![]),
    ] {
        let doc = Document::from_bytes(&pdf(extra, objs)).unwrap();
        let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
        let af = |d: &Document| {
            let a = d
                .deref(d.catalog().get(b"AcroForm".as_slice()).unwrap())
                .unwrap();
            let x = a.as_dict().unwrap().get(b"XFA".as_slice()).unwrap().clone();
            match d.deref(&x).unwrap() {
                Object::Array(a) => a.len(),
                Object::Integer(_) => 0,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(af(&doc), af(&out), "{extra}");
    }
}

#[test]
fn large_xfa_and_many_packets() {
    // An 8 MiB template, and 2 000 small packets.
    let big: Vec<u8> = b"<template>"
        .iter()
        .copied()
        .chain(std::iter::repeat_n(b'x', 8 << 20))
        .chain(b"</template>".iter().copied())
        .collect();
    round_trip(&pdf(
        &format!(
            "/AcroForm << /Fields [] /XFA {} >>",
            xfa_array(&[("template", 10)])
        ),
        vec![(10, flate("", &big))],
    ));
    let mut objs = Vec::new();
    let mut entries = Vec::new();
    for i in 0..2000u32 {
        objs.push((10 + i, stream("", format!("<p{i}/>").as_bytes())));
        entries.push(format!("(p{i}) {} 0 R", 10 + i));
    }
    let t = std::time::Instant::now();
    round_trip(&pdf(
        &format!("/AcroForm << /Fields [] /XFA [{}] >>", entries.join(" ")),
        objs,
    ));
    assert!(t.elapsed().as_secs() < 10);
}

#[test]
fn a_packet_beyond_the_stream_limit_is_refused_not_skipped() {
    // The signature check must read the packet; if the limits forbid
    // decoding it, the rewrite is refused rather than guessing.
    let big = vec![b'x'; 4096];
    let bytes = pdf(
        &format!(
            "/AcroForm << /Fields [] /XFA {} >>",
            xfa_array(&[("template", 10)])
        ),
        vec![(10, flate("", &big))],
    );
    let doc =
        Document::from_bytes_with_limits(&bytes, Limits::default().with_max_stream_bytes(1024))
            .unwrap();
    match rewrite(&doc) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "XFA packet"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_xml_signature_in_a_packet_keeps_the_document_refused() {
    let sig = b"<xfa:datasets><Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\"><SignedInfo/></Signature></xfa:datasets>";
    for (plain, single) in [(true, false), (false, false), (false, true)] {
        let body = if plain {
            stream("", sig)
        } else {
            flate("", sig)
        };
        let extra = if single {
            "/AcroForm << /Fields [] /XFA 10 0 R >>".to_string()
        } else {
            format!(
                "/AcroForm << /Fields [] /XFA {} >>",
                xfa_array(&[("template", 11), ("datasets", 10)])
            )
        };
        let doc = Document::from_bytes(&pdf(&extra, vec![(10, body), (11, flate("", TEMPLATE))]))
            .unwrap();
        match rewrite(&doc) {
            Err(Error::SignaturePolicy { detail, .. }) => {
                assert!(detail.contains("XML signature"), "{detail}")
            }
            other => panic!("plain {plain} single {single}: {other:?}"),
        }
    }
}

#[test]
fn a_usage_rights_signature_keeps_an_xfa_form_refused() {
    let doc = Document::from_bytes(&pdf(
        &format!(
            "/Perms << /UR3 30 0 R >> /AcroForm << /Fields [] /XFA {} >>",
            xfa_array(&[("template", 10)])
        ),
        vec![
            (10, flate("", TEMPLATE)),
            (30, b"<< /Type /Sig /Filter /Adobe.PPKLite >>".to_vec()),
        ],
    ))
    .unwrap();
    assert!(matches!(rewrite(&doc), Err(Error::SignaturePolicy { .. })));
}

#[test]
fn an_undefined_packet_is_null_and_the_rest_is_kept() {
    // W1 and W3 together: a packet reference to an undefined object.
    let doc = Document::from_bytes(&pdf(
        &format!(
            "/AcroForm << /Fields [] /XFA {} >>",
            xfa_array(&[("template", 10), ("datasets", 99)])
        ),
        vec![(10, flate("", TEMPLATE))],
    ))
    .unwrap();
    let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
    let af = out
        .deref(out.catalog().get(b"AcroForm".as_slice()).unwrap())
        .unwrap();
    let xfa = out
        .deref(af.as_dict().unwrap().get(b"XFA".as_slice()).unwrap())
        .unwrap();
    let a = xfa.as_array().unwrap();
    assert_eq!(a.len(), 4);
    assert_eq!(out.deref(&a[3]).unwrap(), Object::Null);
    let Object::Stream { data, .. } = out.deref(&a[1]).unwrap() else {
        panic!()
    };
    assert_eq!(unzlib(&data), TEMPLATE);
}

/// A one-packet XFA form whose `datasets` packet holds `content`.
fn one_packet(content: &[u8]) -> Document {
    let extra = format!(
        "/AcroForm << /Fields [] /XFA {} >>",
        xfa_array(&[("template", 11), ("datasets", 10)])
    );
    Document::from_bytes(&pdf(
        &extra,
        vec![(10, flate("", content)), (11, flate("", TEMPLATE))],
    ))
    .unwrap()
}

fn utf16(s: &str, le: bool, bom: bool) -> Vec<u8> {
    let mut v = Vec::new();
    if bom {
        v.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
    }
    for u in s.encode_utf16() {
        v.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
    }
    v
}

#[test]
fn an_xml_signature_in_any_encoding_or_spelling_is_refused() {
    let sig = "<xfa:datasets><ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"/></xfa:datasets>";
    let mut cases: Vec<Vec<u8>> = Vec::new();
    for (le, bom) in [(true, true), (false, true), (true, false), (false, false)] {
        cases.push(utf16(sig, le, bom));
    }
    // The namespace spelt with a character reference.
    cases.push(
        b"<d><ds:Signature xmlns:ds=\"http&#x3A;//www.w3.org/2000/09/xmldsig#\"><ds:SignedInfo/></ds:Signature></d>"
            .to_vec(),
    );
    // The namespace bound in another packet (here: earlier in the same
    // one) than the signature element.
    cases.push(
        b"<xdp xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"><d><ds:Signature/></d></xdp>"
            .to_vec(),
    );
    for (i, c) in cases.iter().enumerate() {
        match rewrite(&one_packet(c)) {
            Err(Error::SignaturePolicy { .. }) => {}
            other => panic!("case {i}: {other:?}"),
        }
    }
    // UTF-32 cannot be checked: refused, not treated as unsigned.
    let mut u32le = vec![0xFF, 0xFE, 0, 0];
    for c in "<d/>".chars() {
        u32le.extend_from_slice(&(c as u32).to_le_bytes());
    }
    match rewrite(&one_packet(&u32le)) {
        Err(Error::Unsupported { feature, .. }) => assert_eq!(feature, "XFA packet"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn signature_fields_and_names_are_not_xml_signatures() {
    // XFA's own signature *field* UI (`<signature>`), a field named
    // Signature, and longer element names are carried.
    for c in [
        &b"<template><field name=\"Signature\"><ui><signature/></ui></field></template>"[..],
        b"<d><SignatureField>x</SignatureField><Signatures/></d>",
        b"<d>Signature: </d>",
    ] {
        let doc = one_packet(c);
        let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
        assert_eq!(packets(&doc), packets(&out));
    }
}

/// A file with an xref stream: plain objects `plain` and, in object
/// stream 40, the compressed objects `packed` (number, body).
fn pdf_objstm(plain: Vec<(u32, Vec<u8>)>, packed: &[(u32, &str)]) -> Vec<u8> {
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
    let (mut head, mut body) = (String::new(), String::new());
    for (n, s) in packed {
        head.push_str(&format!("{n} {} ", body.len()));
        body.push_str(s);
        body.push(' ');
    }
    let os = format!("{head}{body}");
    let mut all = plain;
    all.push((
        40,
        stream(
            &format!("/Type /ObjStm /N {} /First {}", packed.len(), head.len()),
            os.as_bytes(),
        ),
    ));
    for (n, s) in &all {
        rows.push((*n, 1, b.len() as u64, 0));
        b.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        b.extend_from_slice(s);
        b.extend_from_slice(b"\nendobj\n");
    }
    for (i, (n, _)) in packed.iter().enumerate() {
        rows.push((*n, 2, 40, i as u64));
    }
    let x = b.len() as u64;
    rows.push((41, 1, x, 0));
    rows.sort();
    let (mut idx, mut data) = (String::new(), Vec::new());
    for (n, t, f2, f3) in &rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(*t);
        data.extend_from_slice(&(*f2 as u32).to_be_bytes());
        data.extend_from_slice(&(*f3 as u16).to_be_bytes());
    }
    b.extend_from_slice(
        format!(
            "41 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 42 /Root 1 0 R /Length {} >>\nstream\n",
            data.len()
        )
        .as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

const PAGES: [(u32, &[u8]); 2] = [
    (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
    (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
];

/// What `rewrite` did, without the output bytes.
fn outcome(doc: &Document) -> String {
    match rewrite(doc) {
        Ok(_) => "rewritten".into(),
        Err(Error::SignaturePolicy { .. }) => "signed".into(),
        Err(Error::Unsupported { feature, .. }) => format!("unsupported {feature}"),
        Err(e) => format!("{:?}", e.kind()),
    }
}

/// Every case whose outcome is not `want`, named.
fn expect_all(want: &str, cases: Vec<(&str, Document)>) {
    let wrong: Vec<String> = cases
        .iter()
        .map(|(case, doc)| (case, outcome(doc)))
        .filter(|(_, got)| got != want)
        .map(|(case, got)| format!("{case}: {got}"))
        .collect();
    assert!(wrong.is_empty(), "expected {want}: {wrong:#?}");
}

const DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";

/// A form whose `/XFA` array lists `packets` (name, object) with the
/// given stream objects.
fn form(packets: &[(&str, u32)], objs: Vec<(u32, Vec<u8>)>) -> Document {
    let extra = format!("/AcroForm << /Fields [] /XFA {} >>", xfa_array(packets));
    Document::from_bytes(&pdf(&extra, objs)).unwrap()
}

/// `/XFA [(datasets) 20 0 R]` (or `/Perms 20 0 R`) where 20 is a
/// compressed object whose value is the reference `10 0 R`.
fn behind_compressed_ref(catalog_extra: &str, ten: Vec<u8>) -> Document {
    let mut plain: Vec<(u32, Vec<u8>)> = vec![(
        1,
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
    )];
    plain.extend(PAGES.iter().map(|(n, s)| (*n, s.to_vec())));
    plain.push((10, ten));
    Document::from_bytes(&pdf_objstm(plain, &[(20, "10 0 R")])).unwrap()
}

#[test]
fn signatures_the_lexical_check_missed_are_refused() {
    let escaped: String = DSIG.chars().map(|c| format!("&#{};", c as u32)).collect();
    let sig = format!("<Signature xmlns=\"{DSIG}\"/>");
    expect_all(
        "signed",
        vec![
            (
                "UTF-16LE, no BOM, leading white space",
                one_packet(&utf16(
                    &format!("\n  <ds:Signature xmlns:ds=\"{DSIG}\"/>"),
                    true,
                    false,
                )),
            ),
            (
                "UTF-16BE, no BOM, leading white space",
                one_packet(&utf16(
                    &format!(" <Signature xmlns=\"{DSIG}\"/>"),
                    false,
                    false,
                )),
            ),
            (
                "non-ASCII prefix, namespace behind a character reference",
                one_packet(
                    "<データ:Signature xmlns:データ=\"http&#x3A;//www.w3.org/2000/09/xmldsig#\"/>"
                        .as_bytes(),
                ),
            ),
            (
                "namespace fully escaped, other element name",
                one_packet(format!("<S xmlns=\"{escaped}\"/>").as_bytes()),
            ),
            (
                "split across two packets",
                form(
                    &[("template", 11), ("datasets", 10), ("datasets", 12)],
                    vec![
                        (
                            10,
                            flate("", b"<x:Signature xmlns:x=\"http://www.w3.org/2000/"),
                        ),
                        (12, flate("", b"09/xmldsig#\"/>")),
                        (11, flate("", TEMPLATE)),
                    ],
                ),
            ),
            (
                "packet behind a compressed reference",
                behind_compressed_ref(
                    "/AcroForm << /Fields [] /XFA [(datasets) 20 0 R] >>",
                    stream("", sig.as_bytes()),
                ),
            ),
            (
                "/Perms behind a compressed reference",
                behind_compressed_ref("/Perms 20 0 R", b"<< /UR3 << /Type /Sig >> >>".to_vec()),
            ),
        ],
    );
}

#[test]
fn packets_that_cannot_be_read_completely_are_refused() {
    let mut u32le = Vec::new();
    for c in "<d/>".chars() {
        u32le.extend_from_slice(&(c as u32).to_le_bytes());
    }
    expect_all(
        "unsupported XFA packet",
        vec![
            (
                "DTD: entities can build markup",
                one_packet(
                    b"<!DOCTYPE d [<!ENTITY s \"&#60;ds:Signature xmlns:ds='http&#58;//www.w3.org/2000/09/xml&#100;sig#'/>\">]><d>&s;</d>",
                ),
            ),
            (
                "UTF-7 declared",
                one_packet(b"<?xml version=\"1.0\" encoding=\"UTF-7\"?><d>+ADw-x/></d>"),
            ),
            (
                "EBCDIC declared",
                one_packet(b"<?xml version='1.0' encoding='IBM037'?><d/>"),
            ),
            ("EBCDIC bytes", one_packet(&[0x4C, 0x6F, 0xA7, 0x94, 0x93])),
            ("UTF-32LE without BOM", one_packet(&u32le)),
            (
                "UTF-16 declared, bytes not UTF-16",
                one_packet(b"<?xml version=\"1.0\" encoding=\"UTF-16\"?><d/>"),
            ),
            (
                "NUL bytes in no known encoding",
                one_packet(b"\x01\x00<\x00\x00<"),
            ),
            (
                "stream data in an external file",
                form(
                    &[("template", 11), ("datasets", 10)],
                    vec![(10, stream("/F (datasets.xml)", b"")), (11, flate("", TEMPLATE))],
                ),
            ),
        ],
    );
}

#[test]
fn ordinary_xfa_content_is_carried() {
    // A `Signature` element is only an XML signature in the XML-DSig
    // namespace; forms name data elements and fields Signature.
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("data elements", b"<d><Signature>J. Doe</Signature><Signature1/></d>".to_vec()),
        (
            "signature field UI and signData",
            b"<template><field name=\"Signature\"><ui><signature/></ui></field><signData operation=\"sign\"/></template>"
                .to_vec(),
        ),
        ("comment", b"<d><!-- Signature --></d>".to_vec()),
        ("UTF-16LE, no BOM, CJK", utf16("<d>\u{4E00}\u{3042}</d>", true, false)),
        (
            "UTF-16BE with BOM and declaration",
            utf16("<?xml version=\"1.0\" encoding=\"UTF-16\"?><d/>", false, true),
        ),
        (
            "Shift_JIS declared",
            b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?><d>\x93\xfa</d>".to_vec(),
        ),
        ("UTF-8 BOM", b"\xEF\xBB\xBF<d/>".to_vec()),
        ("predefined entities", b"<d a=\"&amp;&lt;&#38;\">&gt;&quot;&apos;</d>".to_vec()),
    ];
    expect_all(
        "rewritten",
        cases.iter().map(|(c, x)| (*c, one_packet(x))).collect(),
    );
    for (case, c) in &cases {
        let doc = one_packet(c);
        let out = Document::from_bytes(&rewrite(&doc).unwrap()).unwrap();
        assert_eq!(packets(&doc), packets(&out), "{case}");
    }
}

/// A form whose packets are `parts`, in order, as plain streams.
fn split_form(parts: &[&[u8]]) -> Document {
    let names: Vec<(&str, u32)> = (0..parts.len())
        .map(|i| ("datasets", 10 + i as u32))
        .collect();
    form(
        &names,
        parts
            .iter()
            .enumerate()
            .map(|(i, p)| (10 + i as u32, stream("", p)))
            .collect(),
    )
}

#[test]
fn constructs_split_across_packets_are_seen() {
    // The packets are one XML document: what straddles a boundary counts.
    expect_all(
        "signed",
        vec![
            (
                "character reference split across packets",
                split_form(&[
                    b"<xdp><ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsi&#1",
                    b"03;#\"/></xdp>",
                ]),
            ),
            (
                "an over-long character reference",
                one_packet(
                    b"<ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsi&#x000000000000000067;#\"/>",
                ),
            ),
        ],
    );
    expect_all(
        "unsupported XFA packet",
        vec![
            (
                "DTD split across packets, entities building the namespace",
                split_form(&[
                    b"<?xml version=\"1.0\"?><!DOC",
                    b"TYPE d [<!ENT",
                    b"ITY % a \"xmld\"><!ENT",
                    b"ITY % b \"sig\"><!ENT",
                    b"ITY u \"http://www.w3.org/2000/09/%a;%b#\">]><d xmlns:ds=\"&u;\"><ds:Signature/></d>",
                ]),
            ),
            (
                "XML declaration split across packets",
                split_form(&[b"<?xml version=\"1.0\" enc", b"oding=\"UTF-7\"?><d/>"]),
            ),
        ],
    );
}

#[test]
fn the_check_is_linear_in_hostile_text() {
    // 256 KiB of declarations, character references and DTD-like
    // prefixes in one packet: decided quickly in a debug build too
    // (the quadratic declaration scan needed ~10^10 steps here and did
    // not finish in 5 minutes; the linear one takes under a second, so
    // the bound leaves room for a loaded machine).
    for unit in [&b"<?xml "[..], b"&#0000000", b"<!DOCTYP", b"&#x;&#;"] {
        let content: Vec<u8> = unit.iter().copied().cycle().take(1 << 18).collect();
        let t = std::time::Instant::now();
        let _ = rewrite(&one_packet(&content));
        assert!(
            t.elapsed().as_secs() < 60,
            "{:?}",
            String::from_utf8_lossy(unit)
        );
    }
}
