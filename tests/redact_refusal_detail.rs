//! The refusals of `redact` and the region / text / image removers say
//! why, never what the page shows: two documents that differ only in
//! their text are refused with the same message (`Display` and `Debug`),
//! in strict and recovery mode, through the library and the CLI, and
//! nothing is written. Synthetic documents and dummy strings only.

use kasane_pdf::{
    redact, remove_image_region, remove_region, remove_text, Document, Error, Limits, RedactOptions,
};

/// A one-page document (300 × 200) with /F1 = `font` and `extra`
/// objects from number 6.
fn doc(font: &[u8], content: &[u8], extra: &[&[u8]]) -> Vec<u8> {
    doc_with("", "", font, content, extra)
}

/// [`doc`] with entries added to the catalog and the page.
fn doc_with(catalog: &str, page: &str, font: &[u8], content: &[u8], extra: &[&[u8]]) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![
        format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R {page} >>"
        )
        .into_bytes(),
        [
            format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(),
            content,
            b"\nendstream",
        ]
        .concat(),
        font.to_vec(),
    ];
    objs.extend(extra.iter().map(|o| o.to_vec()));
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

const HELVETICA: &[u8] =
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";
const COURIER_DIFF: &[u8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier \
    /Encoding << /Type /Encoding /Differences [65 /Q /W /E /R /T /Y] >> >>";
/// A Type 3 font whose glyphs paint a shading: not evaluated.
const TYPE3: &[u8] = b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
    /FontMatrix [0.001 0 0 0.001 0 0] /FirstChar 65 /LastChar 90 \
    /Widths [600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600 600] \
    /Encoding << /Type /Encoding /Differences [65 /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g /g] >> \
    /CharProcs << /g 6 0 R >> /Resources << /Shading << /S0 7 0 R >> >> >>";
const TYPE3_EXTRA: &[&[u8]] = &[
    b"<< /Length 22 >>\nstream\n600 0 d0 /S0 sh\nendstream",
    b"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1000 0] \
       /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>",
];

/// The dummy strings: same length, every byte different, and every
/// position different (first, middle, last).
const A: &[u8] = b"QWERTYUIOP";
const B: &[u8] = b"ZXCVBNMLKJ";

fn page(font_line: &[u8], s: &[u8]) -> Vec<u8> {
    [&b"BT /F1 14 Tf 20 100 Td ("[..], s, b") Tj ET ", font_line].concat()
}

/// Every refusal message of the editors for `pdf`, strict and
/// recovery, rects on the text and away from it.
fn messages(pdf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for recovering in [false, true] {
        let d = if recovering {
            Document::from_bytes_recovering(pdf).unwrap()
        } else {
            Document::from_bytes(pdf).unwrap()
        };
        for rect in [(10.0, 90.0, 290.0, 130.0), (200.0, 150.0, 290.0, 190.0)] {
            let l = Limits::default();
            let mut errs: Vec<Error> = Vec::new();
            for opts in [
                RedactOptions::new(),
                RedactOptions::new().remove_annotations(true),
            ] {
                match redact(&d, &[(0, vec![rect])], &opts) {
                    Ok(_) => panic!("redact accepted a page it cannot evaluate"),
                    Err(e) => errs.push(e),
                }
            }
            errs.push(refused(
                remove_region(&d, 0, &[rect], &l),
                "remove_region refused",
            ));
            errs.push(refused(
                remove_text(&d, 0, &[rect], &l),
                "remove_text refused",
            ));
            errs.push(refused(
                remove_image_region(&d, 0, &[rect], &l),
                "remove_image_region refused",
            ));
            for e in errs {
                out.push(format!("{recovering} {rect:?} {e}"));
                out.push(format!("{recovering} {rect:?} {e:?}"));
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    out.push(format!("{recovering} source {s} {s:?}"));
                    src = s.source();
                }
            }
        }
    }
    out
}

/// `expect`: what the refusals must say (so the path is the one meant).
fn refused<T>(r: Result<T, Error>, what: &str) -> Error {
    match r {
        Ok(_) => panic!("{what}: accepted a page it cannot evaluate"),
        Err(e) => e,
    }
}

fn assert_same_refusals(make: impl Fn(&[u8]) -> Vec<u8>, what: &str, expect: &[&str]) {
    let (a, b) = (messages(&make(A)), messages(&make(B)));
    for x in expect {
        assert!(
            a.iter().any(|m| m.contains(x)),
            "{what}: no refusal says {x:?}: {a:#?}"
        );
    }
    assert_eq!(a, b, "{what}: the refusal depends on the text shown");
    for m in &a {
        for s in [A, B] {
            let s = std::str::from_utf8(s).unwrap();
            assert!(!m.contains(s) && !m.contains(&s[..3]), "{what}: {m}");
        }
    }
}

#[test]
fn refusals_of_uncertain_glyphs_do_not_depend_on_the_text() {
    assert_same_refusals(
        |s| doc(HELVETICA, &page(b"", s), &[]),
        "non-embedded Standard 14",
        &["outline cannot be evaluated: unsupported PDF feature: non-embedded font"],
    );
    assert_same_refusals(
        |s| doc(COURIER_DIFF, &page(b"", s), &[]),
        "/Differences",
        &["outline cannot be evaluated"],
    );
    assert_same_refusals(
        |s| doc(TYPE3, &page(b"", s), TYPE3_EXTRA),
        "Type 3",
        &["outline cannot be evaluated: unsupported PDF feature: Type 3 glyph"],
    );
    // The text differs only after its first glyph, or only at the end.
    for prefix in [&b"SSSSSSSSS"[..], b"S"] {
        let make = |s: &[u8]| {
            let t = [prefix, &s[prefix.len()..]].concat();
            doc(HELVETICA, &page(b"", &t), &[])
        };
        assert_same_refusals(
            make,
            "after a common prefix",
            &["outline cannot be evaluated"],
        );
    }
}

#[test]
fn refusals_for_an_unknown_operator_do_not_depend_on_its_bytes() {
    // A bare word where an operator is expected: refused in strict mode
    // (parse error), unread in recovery mode (uncertain).
    assert_same_refusals(
        |s| {
            doc(
                HELVETICA,
                &[&b"0 0 10 10 re f "[..], s, b" 0 0 5 5 re f"].concat(),
                &[],
            )
        },
        "unknown operator",
        &["unknown operator at byte", "operator not interpreted"],
    );
}

/// A Type 3 font whose one glyph's procedure is `proc`.
fn type3_with(proc_: &[u8]) -> Vec<u8> {
    doc(
        TYPE3,
        &page(b"", b"ABC"),
        &[
            &[
                format!("<< /Length {} >>\nstream\n", proc_.len()).as_bytes(),
                proc_,
                b"\nendstream",
            ]
            .concat(),
            TYPE3_EXTRA[1],
        ],
    )
}

/// Helvetica with a `/ToUnicode` CMap whose body is `body`.
fn tounicode_with(body: &[u8], s: &[u8]) -> Vec<u8> {
    let cmap = [
        &b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
           1 begincodespacerange <00> <FF> endcodespacerange "[..],
        body,
        b" endcmap end end",
    ]
    .concat();
    doc(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding \
           /ToUnicode 6 0 R >>",
        &page(b"", s),
        &[&[
            format!("<< /Length {} >>\nstream\n", cmap.len()).as_bytes(),
            &cmap,
            b"\nendstream",
        ]
        .concat()],
    )
}

#[test]
fn refusals_for_damaged_operands_do_not_depend_on_their_bytes() {
    // A bare word inside an array or a dictionary operand.
    assert_same_refusals(
        |s| {
            let c = [&b"BT /F1 14 Tf 20 100 Td [(A) "[..], s, b" (B)] TJ ET"].concat();
            doc(HELVETICA, &c, &[])
        },
        "word in an array",
        &["a keyword cannot be an operand"],
    );
    assert_same_refusals(
        |s| {
            let c = [&b"/Span << /Lang "[..], s, b" >> BDC 0 0 5 5 re f EMC"].concat();
            doc(HELVETICA, &c, &[])
        },
        "word in a dictionary",
        &["a keyword cannot be an operand"],
    );
    // A damaged hexadecimal string.
    assert_same_refusals(
        |s| {
            let c = [&b"BT /F1 14 Tf 20 100 Td <41"[..], s, b"> Tj ET"].concat();
            doc(HELVETICA, &c, &[])
        },
        "hexadecimal string",
        &["hexadecimal string"],
    );
    // The same inside a Type 3 glyph's procedure.
    assert_same_refusals(
        |s| type3_with(&[&b"600 0 d0 [ "[..], s, b" ] 0 d 0 0 m 5 5 l S"].concat()),
        "Type 3 procedure",
        &["outline cannot be evaluated"],
    );
}

#[test]
fn refusals_for_a_damaged_tounicode_do_not_depend_on_its_codes() {
    // A source code wider than the codespace; an unexpected byte; a
    // bad number. The text shown is the same — only the CMap differs.
    let hex = |s: &[u8]| s.iter().map(|b| format!("{b:02X}")).collect::<String>();
    assert_same_refusals(
        |s| {
            let body = format!("1 beginbfchar <{}> <0041> endbfchar", hex(&s[..2]));
            tounicode_with(body.as_bytes(), b"AB")
        },
        "bfchar width",
        &["ToUnicode"],
    );
    assert_same_refusals(
        |s| {
            // A byte no CMap token starts with ('!' or '*' here).
            let body = [&b"1 beginbfchar <41> <0041> endbfchar "[..], &[s[0] - 0x30]].concat();
            tounicode_with(&body, b"AB")
        },
        "unexpected byte",
        &["unexpected byte in ToUnicode"],
    );
}

#[test]
fn refusals_for_a_damaged_object_do_not_depend_on_its_bytes() {
    // An indirect object read when the font loads holds a bare word.
    assert_same_refusals(
        |s| {
            doc(
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding \
                   /ToUnicode 6 0 R >>",
                &page(b"", b"AB"),
                &[s],
            )
        },
        "bare word as an object",
        &["unexpected keyword at byte"],
    );
}

/// `redact`'s refusals alone (strict and recovery, with and without
/// removing annotations), Display and Debug.
fn redact_messages(pdf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for recovering in [false, true] {
        let d = if recovering {
            Document::from_bytes_recovering(pdf).unwrap()
        } else {
            Document::from_bytes(pdf).unwrap()
        };
        for opts in [
            RedactOptions::new(),
            RedactOptions::new().remove_annotations(true),
        ] {
            let e = refused(
                redact(&d, &[(0, vec![(10.0, 10.0, 290.0, 190.0)])], &opts),
                "redact",
            );
            out.push(format!("{recovering} {e} {e:?}"));
        }
    }
    out
}

fn assert_same_redact_refusals(make: impl Fn(&[u8]) -> Vec<u8>, what: &str, expect: &str) {
    let (a, b) = (redact_messages(&make(A)), redact_messages(&make(B)));
    assert!(a.iter().any(|m| m.contains(expect)), "{what}: {a:#?}");
    assert_eq!(a, b, "{what}: the refusal depends on a document string");
}

const SHAPE: &[u8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

#[test]
fn refusals_do_not_quote_document_strings() {
    // A signature field's name.
    assert_same_redact_refusals(
        |s| {
            let field = [&b"<< /FT /Sig /T ("[..], s, b") /V 7 0 R >>"].concat();
            doc_with(
                "/AcroForm << /Fields [6 0 R] /SigFlags 3 >>",
                "",
                SHAPE,
                b"0 0 5 5 re f",
                &[
                    &field,
                    b"<< /Type /Sig /ByteRange [0 1 2 3] /Contents <00> >>",
                ],
            )
        },
        "signature field",
        "signature",
    );
    // An XFA packet's name and its encoding declaration.
    assert_same_redact_refusals(
        |s| {
            let xml = [&b"<?xml version=\"1.0\" encoding=\""[..], s, b"\"?><x/>"].concat();
            let packet = [
                format!("<< /Length {} >>\nstream\n", xml.len()).as_bytes(),
                &xml,
                b"\nendstream",
            ]
            .concat();
            let arr = [&b"<< /Fields [] /XFA [("[..], s, b") 6 0 R] >>"].concat();
            doc_with(
                &format!("/AcroForm {}", String::from_utf8_lossy(&arr)),
                "",
                SHAPE,
                b"0 0 5 5 re f",
                &[&packet],
            )
        },
        "XFA packet",
        "packet 1",
    );
    // A CIDFont's /Ordering.
    assert_same_redact_refusals(
        |s| {
            let cid = [
                &b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDSystemInfo \
                   << /Registry (Adobe) /Ordering ("[..],
                s,
                b") /Supplement 0 >> /FontDescriptor 7 0 R >>",
            ]
            .concat();
            doc(
                b"<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
                   /DescendantFonts [6 0 R] >>",
                b"BT /F1 14 Tf 20 100 Td <0001> Tj ET",
                &[
                    &cid,
                    b"<< /Type /FontDescriptor /FontName /X /Flags 4 /FontBBox [0 0 1000 1000] \
                       /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 >>",
                ],
            )
        },
        "CIDFont ordering",
        "not a registered one",
    );
}

#[test]
fn refusals_do_not_quote_a_field_value() {
    // A button field keeps one widget, which does not show the field's
    // state: refused, without naming the state.
    let make = |s: &[u8]| {
        let n = String::from_utf8_lossy(s);
        doc_with(
            "/AcroForm << /Fields [6 0 R] >>",
            "/Annots [7 0 R 8 0 R]",
            SHAPE,
            b"0 0 5 5 re f",
            &[
                format!("<< /FT /Btn /T (f) /V /{n} /Kids [7 0 R 8 0 R] >>").as_bytes(),
                format!(
                    "<< /Type /Annot /Subtype /Widget /Parent 6 0 R /Rect [20 20 40 40] /AS /{n} \
                     /AP << /N << /{n} 9 0 R /Off 9 0 R >> >> >>"
                )
                .as_bytes(),
                b"<< /Type /Annot /Subtype /Widget /Parent 6 0 R /Rect [250 170 270 190] /AS /Off \
                   /AP << /N << /Off 9 0 R >> >> >>",
                b"<< /Type /XObject /Subtype /Form /BBox [0 0 20 20] /Length 0 >>\nstream\n\nendstream",
            ],
        )
    };
    let msgs = |pdf: &[u8]| {
        let mut out = Vec::new();
        for recovering in [false, true] {
            let d = if recovering {
                Document::from_bytes_recovering(pdf).unwrap()
            } else {
                Document::from_bytes(pdf).unwrap()
            };
            let e = refused(
                redact(
                    &d,
                    &[(0, vec![(10.0, 10.0, 100.0, 100.0)])],
                    &RedactOptions::new().remove_annotations(true),
                ),
                "redact",
            );
            out.push(format!("{recovering} {e} {e:?}"));
        }
        out
    };
    let (a, b) = (msgs(&make(A)), msgs(&make(B)));
    assert!(
        a.iter().any(|m| m.contains("is a state no widget")),
        "{a:#?}"
    );
    assert_eq!(a, b);
}

/// A page whose content stream has `filter` and the encoded `data`.
fn filtered_page(filter: &str, data: &[u8]) -> Vec<u8> {
    let mut pdf = doc(HELVETICA, b"", &[]);
    // Swap in a content stream with the filter (object 4 is the empty one).
    let old = b"<< /Length 0 >>\nstream\n\nendstream".as_slice();
    let new = [
        format!("<< /Length {} /Filter /{filter} >>\nstream\n", data.len()).as_bytes(),
        data,
        b"\nendstream",
    ]
    .concat();
    let i = pdf.windows(old.len()).position(|w| w == old).unwrap();
    pdf.splice(i..i + old.len(), new);
    // Rebuild the offsets.
    let body = &pdf[..pdf.windows(4).rposition(|w| w == b"xref").unwrap()];
    let mut offs = Vec::new();
    let mut at = 0;
    while let Some(j) = body[at..].windows(7).position(|w| w == b" 0 obj\n") {
        let start = body[..at + j]
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |k| k + 1);
        offs.push(start);
        at += j + 7;
    }
    let mut out = body.to_vec();
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offs.len() + 1).as_bytes());
    for o in &offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            offs.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn refusals_for_damaged_stream_data_do_not_depend_on_its_bytes() {
    // Bytes no hex digit / no ASCII85 digit: 'Q'…'P' and '{'…
    assert_same_refusals(
        |s| filtered_page("ASCIIHexDecode", &[b"41 42 ", &s[..1], b"3>"].concat()),
        "ASCIIHex",
        &["ASCIIHexDecode: the byte at offset"],
    );
    assert_same_refusals(
        |s| {
            filtered_page(
                "ASCII85Decode",
                &[b"9jqo^", &[s[0].wrapping_add(0x2A)][..], b"~>"].concat(),
            )
        },
        "ASCII85",
        &["ASCII85Decode: the byte at offset"],
    );
    // A literal run longer than the data: its length is a stream byte.
    assert_same_refusals(
        |s| filtered_page("RunLengthDecode", &s[..1]),
        "RunLength",
        &["RunLengthDecode: the literal run at offset"],
    );
}

#[test]
fn refusals_for_a_damaged_width_array_do_not_depend_on_its_cids() {
    let ttf = include_bytes!("../fonts/liberation/LiberationSans-Regular.ttf");
    assert_same_refusals(
        |s| {
            let w = format!("/W [{} 10 [500]]", 1000 + s[0] as u32);
            let w = w.replace(" [500]]", " 500]");
            let cid = format!(
                "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /X /CIDSystemInfo \
                 << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R \
                 /CIDToGIDMap /Identity {w} >>"
            );
            doc(
                b"<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H \
                   /DescendantFonts [6 0 R] >>",
                b"BT /F1 14 Tf 20 100 Td <0024> Tj ET",
                &[
                    cid.as_bytes(),
                    b"<< /Type /FontDescriptor /FontName /X /Flags 32 /FontBBox [0 0 1000 1000] \
                       /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 \
                       /FontFile2 8 0 R >>",
                    &[
                        format!("<< /Length {} >>\nstream\n", ttf.len()).as_bytes(),
                        &ttf[..],
                        b"\nendstream",
                    ]
                    .concat(),
                ],
            )
        },
        "/W range",
        &["/W has an invalid range"],
    );
}

#[test]
fn the_cli_refuses_alike_and_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("kasane-refusal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut outputs = Vec::new();
    for (i, s) in [A, B].iter().enumerate() {
        let input = dir.join(format!("in{i}.pdf"));
        std::fs::write(&input, doc(HELVETICA, &page(b"", s), &[])).unwrap();
        let output = dir.join("out.pdf");
        let mut shown = Vec::new();
        for extra in [&[][..], &["--explain"][..]] {
            let r = std::process::Command::new(env!("CARGO_BIN_EXE_kasane-pdf"))
                .arg("redact")
                .arg(&input)
                .args(["--page", "1", "--rect", "10", "90", "290", "130"])
                .args(extra)
                .arg("--output")
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                !output.exists(),
                "{extra:?}: a refused redaction wrote an output"
            );
            let text = [r.stdout.clone(), r.stderr.clone()].concat();
            if extra.is_empty() {
                assert!(!r.status.success());
                assert!(String::from_utf8_lossy(&r.stderr).contains("uncertain content"));
            } else {
                // The explanation lists the glyphs it cannot evaluate.
                assert!(r.status.success(), "{}", String::from_utf8_lossy(&text));
                let out = String::from_utf8_lossy(&r.stdout);
                assert_eq!(
                    out.matches("outline cannot be evaluated").count(),
                    A.len(),
                    "{out}"
                );
            }
            let text = String::from_utf8_lossy(&text).into_owned();
            let input_name = input.to_string_lossy().into_owned();
            shown.push(text.replace(&input_name, "IN"));
        }
        outputs.push(shown);
    }
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(outputs[0], outputs[1]);
}
