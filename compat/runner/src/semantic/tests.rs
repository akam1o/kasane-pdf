//! Synthetic documents for the semantic snapshot: one rich document in
//! several variants — renumbered, stored differently, or with one
//! meaning deliberately changed — compared against the base.

use super::*;

#[derive(Clone, Copy, PartialEq)]
enum Mut {
    None,
    OutlineTitleGone,
    OutlineTarget,
    TitleUtf16,
    DestsInNameTree,
    LabelChanged,
    LabelsSplitTree,
    McidSwapped,
    EmbeddedContent,
    EmbeddedDetached,
    EmbeddedCompressed,
    OcgOff,
    FieldValueGone,
}

/// Serialise numbered bodies; object `root` is the catalog.
fn build(objs: &[(u32, String)], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objs {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body.as_bytes());
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = objs.iter().map(|o| o.0).max().unwrap() + 1;
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> String {
    // Data kept ASCII-safe by the callers (zlib output is hex-encoded
    // into an /ASCIIHexDecode-free form below only where needed).
    format!(
        "<< {dict} /Length {} >>\nstream\n{}\nendstream",
        data.len(),
        String::from_utf8_lossy(data)
    )
}

fn rich(off: u32, m: Mut) -> Vec<u8> {
    let r = |n: u32| format!("{} 0 R", n + off);
    let names = match m {
        Mut::DestsInNameTree => format!("/Names << /Dests {} /EmbeddedFiles {} >>", r(21), r(50)),
        Mut::EmbeddedDetached => String::new(),
        _ => format!("/Names << /EmbeddedFiles {} >> /Dests {}", r(50), r(20)),
    };
    let names = if m == Mut::EmbeddedDetached {
        format!("/Dests {}", r(20))
    } else {
        names
    };
    let mut o: Vec<(u32, String)> = vec![
        (
            1,
            format!(
                "<< /Type /Catalog /Pages {} /Outlines {} {names} /PageLabels {} \
                 /StructTreeRoot {} /MarkInfo << /Marked true >> /OCProperties {} /AcroForm {} >>",
                r(2),
                r(10),
                r(30),
                r(40),
                r(60),
                r(70)
            ),
        ),
        (
            2,
            format!("<< /Type /Pages /Kids [{} {}] /Count 2 >>", r(3), r(4)),
        ),
        (
            3,
            format!(
                "<< /Type /Page /Parent {} /MediaBox [0 0 200 200] /Contents {} \
                 /Resources << /Properties << /MC0 {} >> >> /StructParents 0 /Annots [{}] >>",
                r(2),
                r(5),
                r(61),
                r(71)
            ),
        ),
        (
            4,
            format!(
                "<< /Type /Page /Parent {} /MediaBox [0 0 200 200] /Contents {} >>",
                r(2),
                r(6)
            ),
        ),
        (
            5,
            stream(
                "",
                b"/P <</MCID 0>> BDC EMC /P <</MCID 1>> BDC EMC /OC /MC0 BDC EMC",
            ),
        ),
        (6, stream("", b"")),
        (
            10,
            format!(
                "<< /Type /Outlines /First {} /Last {} /Count 2 >>",
                r(11),
                r(12)
            ),
        ),
    ];
    let title = match m {
        Mut::OutlineTitleGone => String::new(),
        // "Intro" in UTF-16BE with a BOM: the same text.
        Mut::TitleUtf16 => "/Title <FEFF0049006E00740072006F>".into(),
        _ => "/Title (Intro)".into(),
    };
    let page = if m == Mut::OutlineTarget { r(4) } else { r(3) };
    o.push((
        11,
        format!(
            "<< {title} /Parent {} /Next {} /Dest [{page} /XYZ 0 200 0] >>",
            r(10),
            r(12)
        ),
    ));
    o.push((
        12,
        format!(
            "<< /Title (Named) /Parent {} /Prev {} /Dest (chap) >>",
            r(10),
            r(11)
        ),
    ));
    o.push((20, format!("<< /chap [{} /Fit] >>", r(4))));
    o.push((21, format!("<< /Names [(chap) [{} /Fit]] >>", r(4))));
    let st = if m == Mut::LabelChanged { 5 } else { 1 };
    if m == Mut::LabelsSplitTree {
        o.push((30, format!("<< /Kids [{} {}] >>", r(31), r(32))));
        o.push((31, "<< /Limits [0 0] /Nums [0 << /S /r >>] >>".into()));
        o.push((
            32,
            format!("<< /Limits [1 1] /Nums [1 << /S /D /P (A-) /St {st} >>] >>"),
        ));
    } else {
        o.push((
            30,
            format!("<< /Nums [0 << /S /r >> 1 << /S /D /P (A-) /St {st} >>] >>"),
        ));
    }
    o.push((
        40,
        format!(
            "<< /Type /StructTreeRoot /K [{} {}] /ParentTree {} /RoleMap << /Heading /H1 >> >>",
            r(41),
            r(43),
            r(42)
        ),
    ));
    o.push((
        41,
        format!(
            "<< /Type /StructElem /S /Heading /P {} /Pg {} /K [0 << /Type /OBJR /Obj {} >>] >>",
            r(40),
            r(3),
            r(71)
        ),
    ));
    o.push((
        43,
        format!(
            "<< /Type /StructElem /S /P /P {} /Pg {} /K 1 >>",
            r(40),
            r(3)
        ),
    ));
    let (a, b) = if m == Mut::McidSwapped {
        (43, 41)
    } else {
        (41, 43)
    };
    o.push((
        42,
        format!("<< /Nums [0 [{} {}] 1 {}] >>", r(a), r(b), r(41)),
    ));
    o.push((50, format!("<< /Names [(a.txt) {}] >>", r(51))));
    o.push((
        51,
        format!(
            "<< /Type /Filespec /F (a.txt) /UF (a.txt) /EF << /F {} >> >>",
            r(52)
        ),
    ));
    let ef = match m {
        Mut::EmbeddedContent => stream(
            "/Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 5 >>",
            b"hellp",
        ),
        Mut::EmbeddedCompressed => {
            // The same five bytes, deflated (zlib stream, stored block).
            let z = miniz_oxide::deflate::compress_to_vec_zlib(b"hello", 0);
            let mut s = format!(
                "<< /Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 5 >> /Filter /FlateDecode /Length {} >>\nstream\n",
                z.len()
            )
            .into_bytes();
            s.extend_from_slice(&z);
            s.extend_from_slice(b"\nendstream");
            // Bodies are strings; zlib level 0 output is the header,
            // a stored block with the raw bytes and the checksum — keep
            // it byte-exact through latin-1.
            s.iter().map(|&b| b as char).collect()
        }
        _ => stream(
            "/Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size 5 >>",
            b"hello",
        ),
    };
    o.push((52, ef));
    let d = if m == Mut::OcgOff {
        format!("<< /OFF [{}] /Order [{}] >>", r(61), r(61))
    } else {
        format!("<< /ON [{}] /Order [{}] >>", r(61), r(61))
    };
    o.push((60, format!("<< /OCGs [{}] /D {d} >>", r(61))));
    o.push((61, "<< /Type /OCG /Name (Layer) >>".into()));
    o.push((
        70,
        format!("<< /Fields [{}] /DA (/Helv 0 Tf 0 g) >>", r(71)),
    ));
    let v = if m == Mut::FieldValueGone {
        ""
    } else {
        "/V (Alice)"
    };
    o.push((
        71,
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) {v} /Rect [10 10 100 30] /P {} /StructParent 1 >>",
            r(3)
        ),
    ));
    let o: Vec<(u32, String)> = o.into_iter().map(|(n, b)| (n + off, b)).collect();
    // Latin-1 bodies (the compressed stream) must reach the file as bytes.
    let mut out = build(&o, 1 + off);
    if m == Mut::EmbeddedCompressed {
        out = latin1_fix(&o, 1 + off);
    }
    out
}

/// `build`, writing each body's chars as single bytes.
fn latin1_fix(objs: &[(u32, String)], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objs {
        offs.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend(body.chars().map(|c| c as u32 as u8));
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = objs.iter().map(|o| o.0).max().unwrap() + 1;
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
    );
    out
}

fn doc(b: &[u8]) -> Document {
    Document::from_bytes(b).unwrap()
}

fn class_of(r: &SemanticRoundTrip, cat: &str) -> SemClass {
    r.checks.iter().find(|c| c.category == cat).unwrap().class
}

fn only_fail(m: Mut, cat: &str) {
    let r = compare(&doc(&rich(0, Mut::None)), &doc(&rich(0, m)));
    for c in &r.checks {
        let want = if c.category == cat {
            SemClass::Fail
        } else {
            SemClass::Pass
        };
        assert_eq!(
            c.class, want,
            "{cat}: {} was {:?} ({:?})",
            c.category, c.class, c.notes
        );
    }
    assert_eq!(r.class, SemClass::Fail);
}

#[test]
fn base_document_covers_every_category() {
    let s = snapshot(&doc(&rich(0, Mut::None)));
    for (c, sec) in CATEGORIES.iter().zip(&s) {
        assert!(
            matches!(sec, Section::Data(v) if !v.is_empty()),
            "{c}: {sec:?}"
        );
    }
    // Stable: the same document gives the same snapshot.
    assert_eq!(s, snapshot(&doc(&rich(0, Mut::None))));
}

#[test]
fn renumbering_and_storage_form_are_not_differences() {
    let base = doc(&rich(0, Mut::None));
    for (m, what) in [
        (Mut::None, "renumbered by 100"),
        (Mut::DestsInNameTree, "named destinations in a name tree"),
        (
            Mut::LabelsSplitTree,
            "page labels in a two-level number tree",
        ),
        (Mut::TitleUtf16, "a title as UTF-16"),
        (Mut::EmbeddedCompressed, "the embedded file deflated"),
    ] {
        let r = compare(&base, &doc(&rich(100, m)));
        assert_eq!(r.class, SemClass::Pass, "{what}: {:?}", r.checks);
    }
}

#[test]
fn deliberate_losses_are_detected() {
    only_fail(Mut::OutlineTitleGone, "outlines");
    only_fail(Mut::OutlineTarget, "outlines");
    only_fail(Mut::LabelChanged, "page_labels");
    only_fail(Mut::McidSwapped, "struct_tree");
    only_fail(Mut::EmbeddedContent, "embedded_files");
    only_fail(Mut::EmbeddedDetached, "embedded_files");
    only_fail(Mut::OcgOff, "optional_content");
    only_fail(Mut::FieldValueGone, "acroform");
}

#[test]
fn failure_notes_name_fields_not_values() {
    let r = compare(
        &doc(&rich(0, Mut::None)),
        &doc(&rich(0, Mut::FieldValueGone)),
    );
    let notes = r
        .checks
        .iter()
        .flat_map(|c| c.notes.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(notes.contains("differing: v"), "{notes}");
    assert!(!notes.contains("Alice"), "{notes}");
}

#[test]
fn absent_features_are_not_applicable() {
    let plain = build(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
            ),
        ],
        1,
    );
    let r = compare(&doc(&plain), &doc(&plain));
    assert!(
        r.checks.iter().all(|c| c.class == SemClass::NotApplicable),
        "{:?}",
        r.checks
    );
    assert_eq!(r.class, SemClass::NotApplicable);
    // A rewrite that drops every structure is a failure of each one.
    let r = compare(&doc(&rich(0, Mut::None)), &doc(&plain));
    assert!(
        r.checks.iter().all(|c| c.class == SemClass::Fail),
        "{:?}",
        r.checks
    );
}

#[test]
fn unsupported_structures_are_not_success() {
    // An embedded file under a filter the snapshot does not decode, and
    // an XFA form.
    let src = String::from_utf8_lossy(&rich(0, Mut::None)).into_owned();
    let hex = src
        .replace(
            "/Params << /Size 5 >> /Length 5 >>\nstream\nhello",
            "/Params << /Size 5 >> /Filter /ASCIIHexDecode /Length 5 >>\nstream\nhello",
        )
        .replace("/DA (/Helv 0 Tf 0 g)", "/DA (/Helv 0 Tf 0 g) /XFA [] ");
    // Offsets moved: reparse in recovery mode to read the edited file.
    let d = Document::from_bytes_recovering(hex.as_bytes()).unwrap();
    let r = compare(&d, &d);
    assert_eq!(class_of(&r, "embedded_files"), SemClass::Unsupported);
    assert_eq!(class_of(&r, "acroform"), SemClass::Unsupported);
    assert_eq!(class_of(&r, "outlines"), SemClass::Pass);
    assert_eq!(r.class, SemClass::Unsupported);
}

#[test]
fn cycles_end_safely_as_errors() {
    // Outline item whose /Next is itself; a struct element that is its
    // own kid; a name tree whose kid is the root.
    let cyc = build(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /StructTreeRoot 40 0 R \
                 /Names << /EmbeddedFiles 50 0 R >> >>"
                    .into(),
            ),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".into(),
            ),
            (10, "<< /First 11 0 R >>".into()),
            (11, "<< /Title (loop) /Next 11 0 R >>".into()),
            (40, "<< /Type /StructTreeRoot /K 41 0 R >>".into()),
            (41, "<< /S /P /K 41 0 R >>".into()),
            (50, "<< /Kids [50 0 R] >>".into()),
        ],
        1,
    );
    let d = doc(&cyc);
    let r = compare(&d, &d);
    for c in ["outlines", "struct_tree", "embedded_files"] {
        assert_eq!(class_of(&r, c), SemClass::Error, "{c}: {:?}", r.checks);
    }
    assert_eq!(r.class, SemClass::Error);
}

#[test]
fn kasane_rewrite_keeps_the_meaning() {
    // The rewriter refuses /OCProperties: compare without it.
    let src = rich(0, Mut::None);
    let s = String::from_utf8_lossy(&src).replace(" /OCProperties 60 0 R", "");
    let d = Document::from_bytes_recovering(s.as_bytes()).unwrap();
    let out = kasane_pdf::rewrite(&d).unwrap();
    let r = compare(&d, &Document::from_bytes(&out).unwrap());
    assert_eq!(r.class, SemClass::Pass, "{:?}", r.checks);
}

#[test]
fn text_strings_normalise() {
    assert_eq!(
        text(b"Intro"),
        text(&[0xFE, 0xFF, 0, b'I', 0, b'n', 0, b't', 0, b'r', 0, b'o'])
    );
    // PDFDocEncoding 0x93 is U+FB01; NFC keeps the ligature (no NFKC).
    assert_eq!(text(&[0x93]), "\u{FB01}");
    // Decomposed é (UTF-16) and PDFDocEncoding é compare equal (NFC).
    assert_eq!(text(&[0xFE, 0xFF, 0, 0x65, 0x03, 0x01]), text(&[0xE9]));
    assert_eq!(text(&[0xEF, 0xBB, 0xBF, b'a', b'\t']), "a\\t");
    assert_eq!(letters(28), "bb");
    assert_eq!(roman(14), "xiv");
}
