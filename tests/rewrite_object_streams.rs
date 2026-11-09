//! Input object streams and cross-reference streams in a rewrite that
//! edits or removes something. Such a stream holds the input's objects
//! (or trailer) as they were stored: written out, it would keep what an
//! edit removed. The rewrite writes objects afresh and drops the input's
//! containers; one that an object refers to is refused by an edited
//! rewrite (a plain rewrite copies it, removing nothing). Malformed
//! containers are typed errors.

use kasane_pdf::{
    redact, remove_document_metadata, rewrite, Document, Error, Limits, RedactOptions,
};

/// An object stream: its number, members, extra dictionary entries and
/// whether it is compressed.
type Container<'a> = (u32, Vec<(u32, &'a str)>, &'a str, bool);

fn stream(d: &str, b: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {d} /Length {} >>\nstream\n", b.len()).into_bytes();
    o.extend(b);
    o.extend(b"\nendstream");
    o
}

fn deflate(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

/// An object stream body for `objs` and its `/N`, `/First`.
fn objstm(objs: &[(u32, &str)]) -> (Vec<u8>, usize, usize) {
    let (mut h, mut s) = (String::new(), String::new());
    for (n, o) in objs {
        h.push_str(&format!("{n} {} ", s.len()));
        s.push_str(o);
        s.push(' ');
    }
    (format!("{h}{s}").into_bytes(), objs.len(), h.len())
}

/// A file with `plain` objects, object streams `(number, members,
/// extra dictionary entries, compressed)`, and a cross-reference stream
/// (object `xref`) whose trailer adds `trailer_extra`.
fn file(
    plain: &[(u32, Vec<u8>)],
    streams: &[Container],
    xref: u32,
    trailer_extra: &str,
) -> Vec<u8> {
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u8, u32, u16)> = Vec::new();
    let mut objs: Vec<(u32, Vec<u8>)> = plain.to_vec();
    for (n, members, extra, flate) in streams {
        let (body, count, first) = objstm(members);
        let (body, filter) = if *flate {
            (deflate(&body), " /Filter /FlateDecode")
        } else {
            (body, "")
        };
        objs.push((
            *n,
            stream(
                &format!("/Type /ObjStm /N {count} /First {first}{filter} {extra}"),
                &body,
            ),
        ));
        for (i, (m, _)) in members.iter().enumerate() {
            rows.push((*m, 2, *n, i as u16));
        }
    }
    for (n, o) in objs {
        rows.push((n, 1, b.len() as u32, 0));
        b.extend(format!("{n} 0 obj\n").as_bytes());
        b.extend(o);
        b.extend(b"\nendobj\n");
    }
    let x = b.len();
    rows.push((xref, 1, x as u32, 0));
    rows.sort();
    let size = rows.iter().map(|r| r.0).max().unwrap() + 1;
    let mut idx = String::new();
    let mut data = Vec::new();
    for (n, t, f, g) in rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(t);
        data.extend(f.to_be_bytes());
        data.extend(g.to_be_bytes());
    }
    b.extend(format!("{xref} 0 obj\n").as_bytes());
    b.extend(stream(
        &format!("/Type /XRef /W [1 4 2] /Index [{idx}] /Size {size} /Root 1 0 R {trailer_extra}"),
        &data,
    ));
    b.extend(format!("\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

const PAGE: &str = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
    /Annots [<< /Type /Annot /Subtype /Square /Rect [1 1 9 9] /Contents (ANNOTSECRET) \
    /RC [(nested) << /Deep [(ARRAYSECRET) << /K (DICTSECRET) >>] >>] >> 6 0 R] >>";
const KEPT_ANNOT: &str =
    "<< /Type /Annot /Subtype /Square /Rect [150 150 160 160] /Contents (KEPTNOTE) >>";

fn plain(cat: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (4, stream("", b"0 g 50 50 10 10 re f")),
    ]
}

fn members() -> Vec<(u32, &'static str)> {
    vec![
        (3, PAGE),
        (5, "<< /Title (INFOSECRET) /Producer (p) >>"),
        (6, KEPT_ANNOT),
    ]
}

fn redacted(pdf: &[u8]) -> Result<kasane_pdf::Redaction, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    let o = RedactOptions::new().remove_annotations(true);
    redact(&doc, &[(0, vec![(0.0, 0.0, 10.0, 10.0)])], &o)
}

/// `s` in `out` as written raw or as a hex string (either case).
fn has(out: &[u8], s: &[u8]) -> bool {
    let find = |n: &[u8]| out.windows(n.len()).any(|w| w == n);
    let lower: String = s.iter().map(|b| format!("{b:02x}")).collect();
    find(s) || find(lower.as_bytes()) || find(lower.to_uppercase().as_bytes())
}

/// Not referred to: the containers are dropped, the edited page and the
/// kept annotation written afresh, nothing removed left — compressed or
/// not, with nested dictionaries and arrays and several members.
#[test]
fn unreferenced_containers_are_dropped() {
    for flate in [false, true] {
        let pdf = file(&plain(""), &[(20, members(), "", flate)], 21, "/Info 5 0 R");
        let r = redacted(&pdf).unwrap();
        assert_eq!(r.annotations_removed, 1);
        for s in [
            &b"ANNOTSECRET"[..],
            b"ARRAYSECRET",
            b"DICTSECRET",
            b"INFOSECRET",
            b"/ObjStm",
            b"/XRef",
        ] {
            assert!(!has(&r.output, s), "{}", String::from_utf8_lossy(s));
        }
        assert!(has(&r.output, b"KEPTNOTE"), "the other annotation stays");
    }
}

/// Referred to by an object — directly, through /Extends, in a cycle of
/// /Extends, or the cross-reference stream itself — an edited rewrite
/// refuses: redaction and metadata removal alike, compressed or not.
#[test]
fn referenced_containers_are_refused() {
    let cases: Vec<(&str, Vec<Container>)> = vec![
        ("/Cache 20 0 R", vec![(20, members(), "", false)]),
        ("/Cache 20 0 R", vec![(20, members(), "", true)]),
        (
            "/Cache 22 0 R",
            vec![
                (20, members(), "", false),
                (22, vec![], "/Extends 20 0 R", false),
            ],
        ),
        (
            "/Cache 22 0 R",
            vec![
                (20, members(), "/Extends 22 0 R", false),
                (22, vec![], "/Extends 20 0 R", false),
            ],
        ),
        ("/Cache 21 0 R", vec![(20, members(), "", false)]),
    ];
    for (cat, streams) in cases {
        let pdf = file(&plain(cat), &streams, 21, "/Info 5 0 R");
        let e = redacted(&pdf).unwrap_err();
        assert!(matches!(e, Error::Unsupported { .. }), "{cat}: {e}");
        let doc = Document::from_bytes(&pdf).unwrap();
        let e = remove_document_metadata(&doc, &Limits::default()).unwrap_err();
        assert!(e.to_string().contains("object stream"), "{cat}: {e}");
        // A plain rewrite removes nothing and copies it.
        assert!(rewrite(&doc).is_ok(), "{cat}");
    }
}

/// An edit through `Edit` refuses the same way.
#[test]
fn edits_refuse_referenced_containers() {
    let pdf = file(
        &plain("/Cache 20 0 R"),
        &[(20, members(), "", false)],
        21,
        "",
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let mut e = doc.edit().unwrap();
    e.remove_annotation(0, 0).unwrap();
    assert!(e
        .rewrite()
        .unwrap_err()
        .to_string()
        .contains("object stream"));
}

/// An incremental update replacing the page: the old revision's object
/// stream is unreachable and not written; the old page's text is gone.
#[test]
fn superseded_revisions_are_not_carried() {
    let first = file(&plain(""), &[(20, members(), "", true)], 21, "");
    let mut b = first.clone();
    let prev = {
        let s = String::from_utf8_lossy(&first);
        let i = s.rfind("startxref\n").unwrap() + 10;
        s[i..]
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let at = b.len();
    b.extend(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n",
    );
    let x = b.len();
    b.extend(
        format!(
            "xref\n3 1\n{at:010} 00000 n \ntrailer\n<< /Size 22 /Root 1 0 R /Prev {prev} >>\nstartxref\n{x}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let doc = Document::from_bytes(&b).unwrap();
    let out = rewrite(&doc).unwrap();
    assert!(!has(&out, b"ANNOTSECRET") && !has(&out, b"/ObjStm"));
    let r = redacted(&b).unwrap();
    assert!(!has(&r.output, b"ANNOTSECRET"));
}

/// A container written as an ordinary object of generation 1 and
/// referred to: refused like any other.
#[test]
fn a_container_of_another_generation_is_refused() {
    let pdf = file(
        &plain("/Cache 20 0 R"),
        &[(20, members(), "", false)],
        21,
        "",
    );
    // Refer to object 30 generation 1 instead (same length: the offsets
    // stay right), and append it as a copy of the container.
    let pos = pdf.windows(13).position(|w| w == b"/Cache 20 0 R").unwrap();
    let mut b = pdf.clone();
    b[pos..pos + 13].copy_from_slice(b"/Cache 30 1 R");
    let i = pdf.windows(9).position(|w| w == b"20 0 obj\n").unwrap() + 9;
    let j = pdf[i..].windows(7).position(|w| w == b"\nendobj").unwrap() + i;
    let body = pdf[i..j].to_vec();
    let prev = {
        let t = String::from_utf8_lossy(&pdf);
        let k = t.rfind("startxref\n").unwrap() + 10;
        t[k..]
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let at = b.len();
    b.extend(b"30 1 obj\n");
    b.extend(&body);
    b.extend(b"\nendobj\n");
    let x = b.len();
    b.extend(
        format!(
            "xref\n30 1\n{at:010} 00001 n \ntrailer\n<< /Size 31 /Root 1 0 R /Prev {prev} >>\nstartxref\n{x}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let doc = Document::from_bytes(&b).unwrap();
    let e = remove_document_metadata(&doc, &Limits::default()).unwrap_err();
    assert!(e.to_string().contains("object stream"), "{e}");
}

/// Malformed containers are typed errors, and a container over the limit
/// a limit — never a panic.
#[test]
fn malformed_containers_are_typed_errors() {
    // Wrong /N and /First: whatever the reader makes of the file, a
    // typed result, never a panic.
    for header in ["/N 3 /First 4", "/N 99 /First 9", "/N 1 /First 999999"] {
        let pdf = file(&plain(""), &[(20, members(), "", false)], 21, "");
        let s = String::from_utf8_lossy(&pdf).into_owned();
        let i = s.find("/Type /ObjStm /N 3 /First").unwrap();
        let j = s[i..].find(" /Length").unwrap() + i;
        // Same length, so the offsets stay right.
        let mut fixed = format!("/Type /ObjStm {header}");
        while fixed.len() < j - i {
            fixed.push(' ');
        }
        if fixed.len() != j - i {
            continue;
        }
        let pdf = format!("{}{}{}", &s[..i], fixed, &s[j..]).into_bytes();
        let _ = Document::from_bytes(&pdf).and_then(|d| {
            redact(
                &d,
                &[(0, vec![(0.0, 0.0, 10.0, 10.0)])],
                &RedactOptions::new(),
            )
            .map(|_| ())
        });
    }
    let pdf = file(&plain(""), &[(20, members(), "", false)], 21, "");
    let doc = Document::from_bytes_with_limits(&pdf, Limits::default().with_max_objstm_bytes(16));
    let r = doc.and_then(|d| rewrite(&d).map(|_| ()));
    assert!(
        matches!(
            r,
            Err(Error::LimitExceeded { .. }) | Err(Error::Structure(_)) | Err(Error::Parse(_))
        ),
        "{r:?}"
    );
}
