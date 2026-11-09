//! `RedactOptions::remove_javascript`: structures around the scripts it
//! removes — object streams, an indirect `/Names`, repeated and long
//! action chains, names that only look like scripts, bounded work.

use kasane_pdf::{redact, Document, Error, Limits, Object, RedactOptions};

fn stream(d: &str, b: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {d} /Length {} >>\nstream\n", b.len()).into_bytes();
    o.extend(b);
    o.extend(b"\nendstream");
    o
}

/// `plain` objects, and `packed` ones in object stream 90000, with a
/// cross-reference stream.
fn pdf(mut plain: Vec<(u32, Vec<u8>)>, packed: &[(u32, String)]) -> Vec<u8> {
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows = Vec::new();
    if !packed.is_empty() {
        let (mut h, mut s) = (String::new(), String::new());
        for (n, o) in packed {
            h.push_str(&format!("{n} {} ", s.len()));
            s.push_str(o);
            s.push(' ');
        }
        plain.push((
            90000,
            stream(
                &format!("/Type /ObjStm /N {} /First {}", packed.len(), h.len()),
                format!("{h}{s}").as_bytes(),
            ),
        ));
    }
    for (n, o) in plain {
        rows.push((n, 1u8, b.len() as u32, 0u16));
        b.extend(format!("{n} 0 obj\n").as_bytes());
        b.extend(o);
        b.extend(b"\nendobj\n");
    }
    for (i, (n, _)) in packed.iter().enumerate() {
        rows.push((*n, 2, 90000, i as u16));
    }
    let x = b.len();
    rows.push((90001, 1, x as u32, 0));
    rows.sort();
    let mut idx = String::new();
    let mut data = Vec::new();
    for (n, t, f, g) in rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(t);
        data.extend(f.to_be_bytes());
        data.extend(g.to_be_bytes());
    }
    b.extend(b"90001 0 obj\n");
    b.extend(stream(
        &format!("/Type /XRef /W [1 4 2] /Index [{idx}] /Size 90002 /Root 1 0 R"),
        &data,
    ));
    b.extend(format!("\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

fn base(cat: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>".to_vec(),
        ),
    ]
}

fn run(b: &[u8], limits: Limits) -> Result<kasane_pdf::Redaction, Error> {
    let d = Document::from_bytes(b).unwrap();
    let o = RedactOptions::new()
        .remove_javascript(true)
        .with_limits(limits);
    redact(&d, &[(0, vec![(0.0, 0.0, 10.0, 10.0)])], &o)
}

fn has(out: &[u8], s: &[u8]) -> bool {
    out.windows(s.len()).any(|w| w == s)
}

/// An inline script removed from an object kept in an object stream that
/// something still refers to: the stream would keep it — refused.
#[test]
fn an_object_stream_still_referred_to_is_refused() {
    let mut p = base("/Cache 90000 0 R");
    p.retain(|(n, _)| *n != 3);
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                /AA << /O << /S /JavaScript /JS (INLINE_SCRIPT) >> >> >>";
    let e = run(&pdf(p, &[(3, page.into())]), Limits::default()).unwrap_err();
    assert!(e.to_string().contains("also referred to"), "{e}");
    // Without the reference the page is rewritten without the script.
    let mut p = base("");
    p.retain(|(n, _)| *n != 3);
    let r = run(&pdf(p, &[(3, page.into())]), Limits::default()).unwrap();
    assert_eq!(r.javascript_removed, 1);
    assert!(!has(&r.output, b"INLINE_SCRIPT"));
}

/// The script tree of an indirect /Names goes, whatever else is edited.
#[test]
fn an_indirect_names_dictionary_loses_its_scripts() {
    let mut p = base("/Names 5 0 R /OpenAction << /S /JavaScript /JS (OPEN_SCRIPT) >>");
    p.push((
        5,
        b"<< /JavaScript << /Names [(init) << /S /JavaScript /JS (INIT_SCRIPT) >>] >> >>".to_vec(),
    ));
    let r = run(&pdf(p, &[]), Limits::default()).unwrap();
    assert!(r.javascript_removed >= 2);
    assert!(!has(&r.output, b"INIT_SCRIPT") && !has(&r.output, b"OPEN_SCRIPT"));
}

/// An action listed twice after a removed script stays listed twice.
#[test]
fn repeated_successors_are_kept() {
    let mut p = base("/OpenAction << /S /Named /N /FirstPage /Next 5 0 R >>");
    p.push((
        5,
        b"<< /S /JavaScript /JS (SCRIPT) /Next [6 0 R 6 0 R] >>".to_vec(),
    ));
    p.push((6, b"<< /S /Named /N /NextPage >>".to_vec()));
    let r = run(&pdf(p, &[]), Limits::default()).unwrap();
    let d = Document::from_bytes(&r.output).unwrap();
    let next = d
        .catalog()
        .get(b"OpenAction".as_slice())
        .and_then(|o| d.deref(o).ok())
        .and_then(|o| o.as_dict().and_then(|a| a.get(b"Next".as_slice()).cloned()));
    match next {
        Some(Object::Array(a)) => assert_eq!(a.len(), 2),
        other => panic!("{other:?}"),
    }
}

/// A resource named /JavaScript is a name, not a script tree.
#[test]
fn a_resource_named_javascript_is_kept() {
    let mut p = base("");
    p[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
               /Resources << /ColorSpace << /JavaScript /DeviceRGB >> >> >>"
        .to_vec();
    let r = run(&pdf(p, &[]), Limits::default()).unwrap();
    assert_eq!(r.javascript_removed, 0);
}

/// Long chains and large scripts end in a typed limit.
#[test]
fn the_work_is_bounded() {
    let mut p = base("/OpenAction 10 0 R");
    for i in 10..10 + 2000u32 {
        p.push((
            i,
            format!("<< /S /JavaScript /JS (x) /Next {} 0 R >>", i + 1).into_bytes(),
        ));
    }
    p.push((2010, b"<< /S /Named /N /NextPage >>".to_vec()));
    assert!(matches!(
        run(&pdf(p, &[]), Limits::default()),
        Err(Error::LimitExceeded { .. })
    ));
    let mut p = base("/OpenAction 5 0 R");
    p.push((
        5,
        format!(
            "<< /S /JavaScript /JS (SCRIPT) /Private [{}] >>",
            vec!["null"; 100_000].join(" ")
        )
        .into_bytes(),
    ));
    let mut small = Limits::default();
    small.max_audit_elements = 1000;
    assert!(matches!(
        run(&pdf(p, &[]), small),
        Err(Error::LimitExceeded { .. })
    ));
}

/// Redaction edits objects; one kept in an object stream that something
/// still refers to would keep its input form there (here a page whose
/// removed link and its text). The output may carry no input object
/// stream: refused — with or without scripts.
#[test]
fn an_input_object_stream_is_never_carried() {
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [<< /Type /Annot \
                /Subtype /Link /Rect [1 1 9 9] /A << /S /URI /URI (https://example.com/REMOVED_LINK) >> >>] >>";
    for js in [false, true] {
        let mut p = base("/Cache 90000 0 R");
        p.retain(|(n, _)| *n != 3);
        let doc = Document::from_bytes(&pdf(p, &[(3, page.into())])).unwrap();
        let o = RedactOptions::new().remove_javascript(js);
        let e = redact(&doc, &[(0, vec![(0.0, 0.0, 10.0, 10.0)])], &o).unwrap_err();
        assert!(e.to_string().contains("object stream"), "{e}");
    }
    // Not referred to: its objects are written afresh, the link gone.
    let mut p = base("");
    p.retain(|(n, _)| *n != 3);
    let doc = Document::from_bytes(&pdf(p, &[(3, page.into())])).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
        &RedactOptions::new(),
    )
    .unwrap();
    assert_eq!(r.links_removed, 1);
    assert!(!has(&r.output, b"REMOVED_LINK"));
}

/// Inline scripts chained through referenced arrays count towards the
/// depth limit like referenced ones.
#[test]
fn inline_script_chains_are_bounded() {
    let mut p = base("/OpenAction << /S /JavaScript /JS (x) /Next 10 0 R >>");
    for i in 10..10 + 2000u32 {
        p.push((
            i,
            format!("[<< /S /JavaScript /JS (x) /Next {} 0 R >>]", i + 1).into_bytes(),
        ));
    }
    p.push((2010, b"[<< /S /Named /N /NextPage >>]".to_vec()));
    assert!(matches!(
        run(&pdf(p, &[]), Limits::default()),
        Err(Error::LimitExceeded { .. })
    ));
}
