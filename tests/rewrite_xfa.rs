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
