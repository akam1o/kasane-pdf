//! Content-stream parsing integration tests (milestone M2):
//! typed operations, the affine `Matrix` type, resource lookup,
//! structural balance, and the M2 resource limits.

use kasane_pdf::{Document, Error, Limits, Matrix, Object, Operation, TJItem};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Build a minimal 1-page PDF whose `/Contents` is `stream` (raw bytes).
fn pdf_with_stream(stream: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes(),
                stream.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

/// Build a 1-page PDF with two `/Contents` streams.
fn pdf_with_two_streams(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [4 0 R 5 0 R] >>"
                .to_vec(),
        ),
        (
            4,
            [
                format!("<< /Length {} >>\nstream\n", a.len()).into_bytes(),
                a.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
        (
            5,
            [
                format!("<< /Length {} >>\nstream\n", b.len()).into_bytes(),
                b.to_vec(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

fn ops_of(stream: &[u8]) -> Result<Vec<Operation>, Error> {
    let pdf = pdf_with_stream(stream);
    let doc = Document::from_bytes(&pdf)?;
    doc.page_operations(&doc.pages()[0])
}

// -------------------------------------------------------------------
// Basic operator parsing
// -------------------------------------------------------------------

#[test]
fn hello_text_ops_parse() {
    let doc = Document::from_bytes(&fixture("hello.pdf")).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::BeginText,
            Operation::SetTextFont {
                font: b"F1".to_vec(),
                size: 24.0
            },
            Operation::MoveTextPosition(72.0, 720.0),
            Operation::ShowText(b"Hello, PDF!".to_vec()),
            Operation::EndText,
        ]
    );
}

#[test]
fn operand_order_is_first_pushed_first() {
    // `10 20 m` must produce MoveTo(10, 20), not MoveTo(20, 10).
    let ops = ops_of(b"10 20 m 30 40 l").unwrap();
    assert_eq!(
        ops,
        vec![Operation::MoveTo(10.0, 20.0), Operation::LineTo(30.0, 40.0)]
    );
}

#[test]
fn graphics_state_and_path_ops() {
    let ops = ops_of(b"q 2 w 1 J 0 j 10 M [3 2] 0 d /Perceptual ri 1 i /GS1 gs 1 0 0 1 5 6 cm Q")
        .unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::SaveState,
            Operation::SetLineWidth(2.0),
            Operation::SetLineCap(1),
            Operation::SetLineJoin(0),
            Operation::SetMiterLimit(10.0),
            Operation::SetDash {
                dashes: vec![3.0, 2.0],
                phase: 0.0
            },
            Operation::SetRenderingIntent(b"Perceptual".to_vec()),
            Operation::SetFlatness(1.0),
            Operation::SetExtGState(b"GS1".to_vec()),
            Operation::ConcatMatrix(Matrix::new(1.0, 0.0, 0.0, 1.0, 5.0, 6.0)),
            Operation::RestoreState,
        ]
    );
}

#[test]
fn painting_and_clipping_ops() {
    for (tok, want) in [
        ("S", Operation::Stroke),
        ("s", Operation::CloseAndStroke),
        ("f", Operation::Fill),
        ("F", Operation::Fill),
        ("f*", Operation::FillEvenOdd),
        ("B", Operation::FillAndStroke),
        ("B*", Operation::FillAndStrokeEvenOdd),
        ("b", Operation::CloseFillAndStroke),
        ("b*", Operation::CloseFillAndStrokeEvenOdd),
        ("n", Operation::EndPath),
        ("W", Operation::Clip),
        ("W*", Operation::ClipEvenOdd),
    ] {
        let ops = ops_of(tok.as_bytes()).unwrap();
        assert_eq!(ops, vec![want], "operator {tok}");
    }
}

#[test]
fn colour_ops() {
    let ops = ops_of(b"0.5 G 0.2 g 0.1 0.2 0.3 RG 0.4 0.5 0.6 rg 0 1 0 0 K 1 0 0 0 k /CS1 CS /CS2 cs 0.5 SC 0.1 0.2 sc 0.5 /P1 SCN /P2 scn").unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::SetGrayStroke(0.5),
            Operation::SetGrayFill(0.2),
            Operation::SetRgbStroke(0.1, 0.2, 0.3),
            Operation::SetRgbFill(0.4, 0.5, 0.6),
            Operation::SetCmykStroke(0.0, 1.0, 0.0, 0.0),
            Operation::SetCmykFill(1.0, 0.0, 0.0, 0.0),
            Operation::SetColorSpaceStroke(b"CS1".to_vec()),
            Operation::SetColorSpaceFill(b"CS2".to_vec()),
            Operation::SetColorStroke(vec![Object::Real(0.5)]),
            Operation::SetColorFill(vec![Object::Real(0.1), Object::Real(0.2)]),
            Operation::SetColorStroke(vec![Object::Real(0.5), Object::Name(b"P1".to_vec()),]),
            Operation::SetColorFill(vec![Object::Name(b"P2".to_vec())]),
        ]
    );
}

#[test]
fn text_state_and_positioning_ops() {
    let ops = ops_of(
        b"BT 0.5 Tc 1.5 Tw 110 Tz 14 TL /F1 12 Tf 2 Tr 3 Ts 10 20 Td 1 2 TD 1 0 0 1 30 40 Tm T* ET",
    )
    .unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::BeginText,
            Operation::SetCharSpacing(0.5),
            Operation::SetWordSpacing(1.5),
            Operation::SetHorizontalScaling(110.0),
            Operation::SetTextLeading(14.0),
            Operation::SetTextFont {
                font: b"F1".to_vec(),
                size: 12.0
            },
            Operation::SetTextRenderingMode(2),
            Operation::SetTextRise(3.0),
            Operation::MoveTextPosition(10.0, 20.0),
            Operation::MoveTextPositionAndSetLeading(1.0, 2.0),
            Operation::SetTextMatrix(Matrix::new(1.0, 0.0, 0.0, 1.0, 30.0, 40.0)),
            Operation::NextLine,
            Operation::EndText,
        ]
    );
}

#[test]
fn text_showing_ops() {
    let ops = ops_of(b"BT (abc) Tj [(a) -250 (b) 30] TJ (c) ' 0.5 0.25 (d) \" ET").unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::BeginText,
            Operation::ShowText(b"abc".to_vec()),
            Operation::ShowTextAdjusted(vec![
                TJItem::Text(b"a".to_vec()),
                TJItem::Kern(-250.0),
                TJItem::Text(b"b".to_vec()),
                TJItem::Kern(30.0),
            ]),
            Operation::NextLineShowText(b"c".to_vec()),
            Operation::SetSpacingNextLineShowText {
                word_spacing: 0.5,
                char_spacing: 0.25,
                text: b"d".to_vec()
            },
            Operation::EndText,
        ]
    );
}

#[test]
fn external_and_marked_content_ops() {
    let ops = ops_of(b"/Im1 Do /Sh1 sh /Tag MP /Tag << /K 1 >> DP /Tag BMC /Tag2 /P1 BDC EMC EMC")
        .unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::Do(b"Im1".to_vec()),
            Operation::Shading(b"Sh1".to_vec()),
            Operation::MarkedContentPoint(b"Tag".to_vec()),
            Operation::DesignatedMarkedContentPoint {
                tag: b"Tag".to_vec(),
                properties: Object::Dict(
                    [(b"K".to_vec(), Object::Integer(1))].into_iter().collect()
                ),
            },
            Operation::BeginMarkedContent(b"Tag".to_vec()),
            Operation::BeginMarkedContentWithProps {
                tag: b"Tag2".to_vec(),
                properties: Object::Name(b"P1".to_vec()),
            },
            Operation::EndMarkedContent,
            Operation::EndMarkedContent,
        ]
    );
}

#[test]
fn fixture_content_ops_parses_fully() {
    let doc = Document::from_bytes(&fixture("content_ops.pdf")).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    assert_eq!(ops.len(), 68);
    // Spot-check a few.
    assert_eq!(ops[0], Operation::SaveState);
    assert_eq!(
        ops[9],
        Operation::ConcatMatrix(Matrix::new(1.0, 0.0, 0.0, 1.0, 50.0, 60.0))
    );
    assert_eq!(ops[56], Operation::Do(b"Im1".to_vec()));
    assert_eq!(
        ops[65],
        Operation::Unknown {
            operator: b"Bar".to_vec(),
            operands: vec![
                Object::Name(b"Foo".to_vec()),
                Object::Integer(1),
                Object::Integer(2)
            ]
        }
    );
}

// -------------------------------------------------------------------
// Lexical details
// -------------------------------------------------------------------

#[test]
fn comments_and_whitespace_are_skipped() {
    let ops = ops_of(b"% leading comment\n10 %mid\n20 m\t30 40 l%trailing").unwrap();
    assert_eq!(
        ops,
        vec![Operation::MoveTo(10.0, 20.0), Operation::LineTo(30.0, 40.0)]
    );
}

#[test]
fn string_escapes_and_binary_bytes_survive() {
    // \n -> LF, \( -> (, octal \101 -> 'A'; byte 0xFE is kept raw.
    let ops = ops_of(b"BT (a\\n\\(b\\101\\376) Tj <feff0041> Tj ET").unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::BeginText,
            Operation::ShowText(b"a\n(bA\xfe".to_vec()),
            Operation::ShowText(b"\xfe\xff\x00\x41".to_vec()),
            Operation::EndText,
        ]
    );
}

#[test]
fn indirect_reference_is_not_an_operand() {
    // `10 0 R` in a content stream: R is an unknown operator, and the
    // numbers are operands — the operator must not consume them.
    let err = ops_of(b"10 0 R").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("unknown operator")),
        "got: {err:?}"
    );
}

#[test]
fn real_and_integer_numbers_both_accepted() {
    let ops = ops_of(b"1.5 2 m").unwrap();
    assert_eq!(ops, vec![Operation::MoveTo(1.5, 2.0)]);
}

// -------------------------------------------------------------------
// Concatenated /Contents arrays
// -------------------------------------------------------------------

#[test]
fn split_contents_share_operand_stack() {
    // Operands in stream 1 feed the operator in stream 2 — as if the
    // streams were concatenated (§7.8.1).
    let pdf = pdf_with_two_streams(b"10 20", b"m");
    let doc = Document::from_bytes(&pdf).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    assert_eq!(ops, vec![Operation::MoveTo(10.0, 20.0)]);
}

/// §7.8.2: the parts of a `/Contents` array divide one stream only at
/// token boundaries, so a property-list dictionary may continue in the
/// next part (Acrobat Distiller splits IRS forms inside
/// `/Artifact <</MCID 59 >>BDC`).
#[test]
fn dictionary_split_across_two_streams() {
    let pdf = pdf_with_two_streams(b"/Artifact <</MCID", b"59 >>BDC EMC");
    let doc = Document::from_bytes(&pdf).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert!(matches!(
        ops[0],
        Operation::BeginMarkedContentWithProps { .. }
    ));
    // A token itself never spans the join: `1` + `0` are two numbers.
    let pdf = pdf_with_two_streams(b"1", b"0 20 m");
    let doc = Document::from_bytes(&pdf).unwrap();
    assert!(doc.page_operations(&doc.pages()[0]).is_err());
}

#[test]
fn split_contents_share_q_depth() {
    let pdf = pdf_with_two_streams(b"q 1 0 0 1 0 0 cm", b"Q");
    let doc = Document::from_bytes(&pdf).unwrap();
    let ops = doc.page_operations(&doc.pages()[0]).unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::SaveState,
            Operation::ConcatMatrix(Matrix::IDENTITY),
            Operation::RestoreState
        ]
    );
}

// -------------------------------------------------------------------
// Matrix
// -------------------------------------------------------------------

#[test]
fn matrix_identity_and_transform() {
    let m = Matrix::IDENTITY;
    assert!(m.is_finite());
    assert_eq!(m.transform(3.0, 4.0), (3.0, 4.0));
}

#[test]
fn matrix_transform_matches_pdf_formula() {
    // x' = a·x + c·y + e ; y' = b·x + d·y + f
    let m = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
    assert_eq!(m.transform(1.0, 1.0), (12.0, 23.0));
}

#[test]
fn matrix_concat_is_cm_order_and_noncommutative() {
    // PDF cm concatenates to the left: CTM' = cm × CTM.
    // Equivalent to `existing.concat(cm_matrix)`.
    let t = Matrix::translate(10.0, 0.0);
    let s = Matrix::scale(2.0, 2.0);
    // CTM after `cm = translate then scale` = S × T (scale applied to
    // the translated coords).
    let ctm = t.concat(s);
    let (x, y) = ctm.transform(1.0, 0.0);
    assert_eq!((x, y), (22.0, 0.0)); // (1+10)*2
                                     // Reversed order differs: T × S gives (2+10)=12.
    let ctm2 = s.concat(t);
    let (x2, y2) = ctm2.transform(1.0, 0.0);
    assert_eq!((x2, y2), (12.0, 0.0));
}

#[test]
fn matrix_non_finite_detected() {
    let bad = Matrix::new(f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0);
    assert!(!bad.is_finite());
    let inf = Matrix::new(1.0, 0.0, 0.0, f64::INFINITY, 0.0, 0.0);
    assert!(!inf.is_finite());
}

// -------------------------------------------------------------------
// Structural balance
// -------------------------------------------------------------------

#[test]
fn unbalanced_q_is_structure_error() {
    let err = ops_of(b"q 1 0 0 1 0 0 cm").unwrap_err();
    assert!(
        matches!(err, Error::Structure(ref m) if m.contains("unmatched 'q'")),
        "got: {err:?}"
    );
}

#[test]
fn q_underflow_is_structure_error() {
    let err = ops_of(b"Q").unwrap_err();
    assert!(
        matches!(err, Error::Structure(ref m) if m.contains("'Q' without matching")),
        "got: {err:?}"
    );
}

#[test]
fn unclosed_text_block_is_structure_error() {
    let err = ops_of(b"BT /F1 12 Tf").unwrap_err();
    assert!(
        matches!(err, Error::Structure(ref m) if m.contains("BT/ET")),
        "got: {err:?}"
    );
}

#[test]
fn et_without_bt_is_structure_error() {
    let err = ops_of(b"ET").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

#[test]
fn nested_bt_is_structure_error() {
    let err = ops_of(b"BT BT ET ET").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

#[test]
fn unbalanced_marked_content_is_structure_error() {
    let err = ops_of(b"/Tag BMC").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
    let err = ops_of(b"EMC").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

#[test]
fn unclosed_compat_section_is_structure_error() {
    let err = ops_of(b"BX").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
    let err = ops_of(b"EX").unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

// -------------------------------------------------------------------
// Operand count / type errors
// -------------------------------------------------------------------

#[test]
fn wrong_operand_count_is_parse_error() {
    let err = ops_of(b"10 m").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("expects 2")),
        "got: {err:?}"
    );
    let err = ops_of(b"1 2 3 4 5 cm").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("expects 6")),
        "got: {err:?}"
    );
}

#[test]
fn wrong_operand_type_is_parse_error() {
    // `Tf` wants name + number; two numbers must fail.
    let err = ops_of(b"BT 12 12 Tf ET").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "got: {err:?}");
    // `Tj` wants a string; a name must fail.
    let err = ops_of(b"BT /F1 Tj ET").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "got: {err:?}");
}

#[test]
fn leftover_operands_at_eof_is_parse_error() {
    let err = ops_of(b"10 20").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("un-consumed operand")),
        "got: {err:?}"
    );
}

#[test]
fn tj_rejects_non_string_non_number_elements() {
    let err = ops_of(b"BT [(a) /name] TJ ET").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("TJ")),
        "got: {err:?}"
    );
}

#[test]
fn scn_accepts_more_than_4_components() {
    // DeviceN colour spaces can exceed 4 components; the parser keeps
    // the operands and a later stage validates arity against the space.
    let ops = ops_of(b"1 2 3 4 5 6 SCN").unwrap();
    assert_eq!(
        ops,
        vec![Operation::SetColorStroke(vec![
            Object::Integer(1),
            Object::Integer(2),
            Object::Integer(3),
            Object::Integer(4),
            Object::Integer(5),
            Object::Integer(6),
        ])]
    );
}

#[test]
fn sc_rejects_non_numeric_operands() {
    // `SC` (not `SCN`) may not take a trailing pattern name.
    let err = ops_of(b"0.5 /P1 SC").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "got: {err:?}");
    let err = ops_of(b"(abc) SC").unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "got: {err:?}");
}

#[test]
fn unknown_operator_is_parse_error() {
    let err = ops_of(b"10 20 XX").unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("unknown operator")),
        "got: {err:?}"
    );
}

#[test]
fn unknown_operator_inside_bx_is_recorded() {
    let ops = ops_of(b"BX 1 2 FooBar EX").unwrap();
    assert_eq!(
        ops,
        vec![
            Operation::BeginCompatibility,
            Operation::Unknown {
                operator: b"FooBar".to_vec(),
                operands: vec![Object::Integer(1), Object::Integer(2)]
            },
            Operation::EndCompatibility,
        ]
    );
}

#[test]
fn stray_inline_image_operators_are_malformed() {
    // `ID`/`EI` outside `BI … EI`, or a `BI` whose data never ends.
    for kw in [b"ID".as_slice(), b"EI".as_slice()] {
        let err = ops_of(kw).unwrap_err();
        assert!(
            matches!(err, Error::Parse(_)),
            "{:?}: got {err:?}",
            String::from_utf8_lossy(kw)
        );
    }
    let err = ops_of(b"BI /W 1 /H 1 /BPC 8 /CS /G ID \x00").unwrap_err();
    assert!(
        matches!(err, Error::Parse(_) | Error::UnexpectedEof { .. }),
        "got: {err:?}"
    );
}

#[test]
fn inline_image_is_delimited_by_its_computed_length() {
    // 2×1 gray, 8 bpc → exactly 2 data bytes; the bytes happen to
    // spell "EI" — the length rule, not the first `EI`, decides.
    let ops = ops_of(b"BI /W 2 /H 1 /BPC 8 /CS /G ID EI EI (x) Tj").unwrap();
    assert_eq!(ops.len(), 2);
    match &ops[0] {
        Operation::InlineImage { dict, data } => {
            assert_eq!(data, b"EI");
            assert_eq!(dict.get(b"W".as_slice()), Some(&Object::Integer(2)));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&ops[1], Operation::ShowText(_)));
}

#[test]
fn inline_image_flate_data_containing_ei_is_not_cut_short() {
    // Compressed bytes that contain a delimited " EI " must not end
    // the image: only a complete zlib stream before an EI counts.
    let raw = b" EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI EI".repeat(4);
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
    std::io::Write::write_all(&mut enc, &raw).unwrap();
    let enc = enc.finish().unwrap();
    assert!(
        enc.windows(4).any(|w| w == b" EI "),
        "fixture must contain a delimited EI"
    );
    let mut stream = b"BI /W 80 /H 1 /BPC 8 /CS /G /F /Fl ID ".to_vec();
    stream.extend_from_slice(&enc);
    stream.extend_from_slice(b" EI 1 0 0 RG");
    let ops = ops_of(&stream).unwrap();
    assert_eq!(ops.len(), 2, "{ops:?}");
    match &ops[0] {
        Operation::InlineImage { data, .. } => assert_eq!(data, &enc),
        other => panic!("{other:?}"),
    }
}

// -------------------------------------------------------------------
// Resources
// -------------------------------------------------------------------

#[test]
fn resource_lookup_resolves_named_entries() {
    let doc = Document::from_bytes(&fixture("content_ops.pdf")).unwrap();
    let page = &doc.pages()[0];
    let font = doc.page_resource(page, "Font", b"F1").unwrap().unwrap();
    let dict = font.as_dict().unwrap();
    assert_eq!(
        dict.get(b"BaseFont".as_slice()).unwrap().as_name(),
        Some(b"Helvetica".as_slice())
    );
    let xo = doc.page_resource(page, "XObject", b"Im1").unwrap().unwrap();
    assert!(xo.as_stream().is_some());
    let gs = doc
        .page_resource(page, "ExtGState", b"GS1")
        .unwrap()
        .unwrap();
    assert_eq!(
        gs.as_dict()
            .unwrap()
            .get(b"CA".as_slice())
            .unwrap()
            .as_number(),
        Some(0.5)
    );
    let cs = doc
        .page_resource(page, "ColorSpace", b"CS1")
        .unwrap()
        .unwrap();
    assert_eq!(cs.as_name(), Some(b"DeviceRGB".as_slice()));
    let sh = doc.page_resource(page, "Shading", b"Sh1").unwrap().unwrap();
    assert_eq!(
        sh.as_dict()
            .unwrap()
            .get(b"ShadingType".as_slice())
            .unwrap()
            .as_i64(),
        Some(2)
    );
    // Missing name and missing category.
    assert!(doc.page_resource(page, "Font", b"Nope").unwrap().is_none());
    assert!(doc.page_resource(page, "Pattern", b"P").unwrap().is_none());
}

#[test]
fn resource_lookup_inherits_from_ancestors() {
    // pages.pdf: page 3 has no own /Resources; the root Pages node
    // doesn't set one either. hello.pdf: page has direct resources.
    let doc = Document::from_bytes(&fixture("hello.pdf")).unwrap();
    let font = doc
        .page_resource(&doc.pages()[0], "Font", b"F1")
        .unwrap()
        .unwrap();
    assert!(font.as_dict().is_some());
    // contents_none.pdf: no Resources at all → None, not an error.
    let doc = Document::from_bytes(&fixture("contents_none.pdf")).unwrap();
    assert!(doc
        .page_resource(&doc.pages()[0], "Font", b"F1")
        .unwrap()
        .is_none());
}

#[test]
fn resource_lookup_bad_category_type_errors() {
    // /Resources /Font is a number instead of a dict.
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let bodies = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font 5 >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, b"<< /Length 0 >>\nstream\n\nendstream".to_vec()),
    ];
    for (n, b) in &bodies {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(b);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 5\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for (_n, off) in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(b"trailer\n<< /Size 5 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    let doc = Document::from_bytes(&out).unwrap();
    let err = doc
        .page_resource(&doc.pages()[0], "Font", b"F1")
        .unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
}

// -------------------------------------------------------------------
// Limits
// -------------------------------------------------------------------

#[test]
fn operations_limit_is_enforced() {
    let limits = Limits {
        max_operations: 3,
        ..Default::default()
    };
    let pdf = pdf_with_stream(b"q 1 w 2 w 3 w");
    let doc = Document::from_bytes_with_limits(&pdf, limits).unwrap();
    let err = doc.page_operations(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_operations"),
        "got: {err:?}"
    );
}

#[test]
fn operations_limit_boundary() {
    // Exactly max_operations ops succeed.
    let limits = Limits {
        max_operations: 3,
        ..Default::default()
    };
    let pdf = pdf_with_stream(b"1 w 2 w 3 w");
    let doc = Document::from_bytes_with_limits(&pdf, limits).unwrap();
    assert_eq!(doc.page_operations(&doc.pages()[0]).unwrap().len(), 3);
}

#[test]
fn operand_stack_limit_is_enforced() {
    let limits = Limits {
        max_operand_stack: 4,
        ..Default::default()
    };
    // 5 operands are pushed before the operator runs.
    let pdf = pdf_with_stream(b"1 2 3 4 5 m");
    let doc = Document::from_bytes_with_limits(&pdf, limits).unwrap();
    let err = doc.page_operations(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_operand_stack"),
        "got: {err:?}"
    );
}

#[test]
fn gsave_depth_limit_is_enforced() {
    let limits = Limits {
        max_gsave_depth: 2,
        ..Default::default()
    };
    let pdf = pdf_with_stream(b"q q q Q Q Q");
    let doc = Document::from_bytes_with_limits(&pdf, limits).unwrap();
    let err = doc.page_operations(&doc.pages()[0]).unwrap_err();
    assert!(
        matches!(err, Error::LimitExceeded { limit, .. } if limit == "max_gsave_depth"),
        "got: {err:?}"
    );
    // Exactly 2 deep is fine.
    let pdf = pdf_with_stream(b"q q Q Q");
    let doc = Document::from_bytes_with_limits(
        &pdf,
        Limits {
            max_gsave_depth: 2,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(doc.page_operations(&doc.pages()[0]).unwrap().len(), 4);
}

#[test]
fn nesting_depth_limit_in_content_array() {
    // [[[1]]] TJ with a depth limit of 2 → LimitExceeded.
    let limits = Limits {
        max_nesting_depth: 2,
        ..Default::default()
    };
    let pdf = pdf_with_stream(b"BT [[[(a)]]] TJ ET");
    let doc = Document::from_bytes_with_limits(&pdf, limits).unwrap();
    let err = doc.page_operations(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "got: {err:?}");
}

// -------------------------------------------------------------------
// No partial results
// -------------------------------------------------------------------

#[test]
fn error_discards_all_operations() {
    // The good ops before the error must not leak out as a partial
    // result: page_operations returns Err, not a short Vec.
    let result = ops_of(b"q 1 w 2 w XX 3 w Q");
    assert!(result.is_err());
}

// -------------------------------------------------------------------
// Regression: M0–M1.1 still work
// -------------------------------------------------------------------

#[test]
fn existing_pages_still_load() {
    // A document with unbalanced content still loads and reports pages;
    // the error only surfaces when operations are requested.
    let doc = Document::from_bytes(&fixture("contents_array.pdf")).unwrap();
    assert_eq!(doc.page_count(), 1);
    let err = doc.page_operations(&doc.pages()[0]).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "got: {err:?}");
    // And the raw contents API is unaffected.
    let contents = doc.page_contents(&doc.pages()[0]).unwrap();
    assert_eq!(contents.len(), 2);
}
