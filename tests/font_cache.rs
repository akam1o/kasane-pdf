//! A document's fonts are loaded once per font dictionary and set of
//! limits. Nothing may be taken for the same font that is not: another
//! dictionary over the same program, other limits, or a fresh document;
//! a reused font records its warnings as a fresh load would.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, Document, ElsewhereLocation, Limits, RedactOptions, TextOptions,
};

fn liberation() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap()
}

/// Font `n` over the shared program (object 9), with `widths` and
/// `extra` (an encoding, a ToUnicode).
fn font(n: u32, width: u32, extra: &str) -> (u32, Vec<u8>) {
    let w = vec![width.to_string(); 95].join(" ");
    (
        n,
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
             /LastChar 126 /Widths [{w}] /FontDescriptor 8 0 R {extra} >>"
        )
        .into_bytes(),
    )
}

fn shared() -> Vec<(u32, Vec<u8>)> {
    vec![
        (
            8,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 9 0 R >>"
                .to_vec(),
        ),
        (
            9,
            stream_obj(&format!("/Length1 {}", liberation().len()), &liberation()),
        ),
    ]
}

/// Pages, each `(content, resources, page extra)`, over `extra` objects.
fn doc(pages: &[(&str, &str, &str)], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 100 + 2 * i))
        .collect();
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            )
            .into_bytes(),
        ),
    ];
    for (i, (c, res, pe)) in pages.iter().enumerate() {
        let n = 100 + 2 * i as u32;
        objs.push((
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {res} >> \
                 /Contents {} 0 R {pe} >>",
                n + 1
            )
            .into_bytes(),
        ));
        objs.push((n + 1, stream_obj("", c.as_bytes())));
    }
    objs.extend(shared());
    objs.extend(extra);
    build_pdf(&objs)
}

fn origins(d: &Document, page: usize, opts: &TextOptions) -> Vec<(String, i64)> {
    extract_text(d, &d.pages()[page], opts)
        .unwrap()
        .chars
        .iter()
        .map(|c| {
            (
                c.unicode.clone().unwrap_or_default(),
                (c.origin.0 * 100.0).round() as i64,
            )
        })
        .collect()
}

/// Two dictionaries over one program, with other widths and another
/// encoding, used on two pages: each page keeps its own.
#[test]
fn fonts_over_one_program_stay_apart() {
    let pdf = doc(
        &[
            (
                "BT /A 10 Tf 10 10 Td (ab) Tj ET",
                "/Font << /A 20 0 R >>",
                "",
            ),
            (
                "BT /B 10 Tf 10 10 Td (ab) Tj ET",
                "/Font << /B 21 0 R >>",
                "",
            ),
            (
                "BT /A 10 Tf 10 10 Td (ab) Tj /B 10 Tf (ab) Tj ET",
                "/Font << /A 20 0 R /B 21 0 R >>",
                "",
            ),
        ],
        vec![
            font(20, 500, "/Encoding /WinAnsiEncoding"),
            font(21, 900, "/Encoding << /Differences [97 /x /y] >>"),
        ],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let o = TextOptions::default();
    let a = origins(&d, 0, &o);
    let b = origins(&d, 1, &o);
    assert_eq!(a, vec![("a".into(), 1000), ("b".into(), 1500)]);
    assert_eq!(b, vec![("x".into(), 1000), ("y".into(), 1900)]);
    // Read again in the same document, and alternating on one page.
    assert_eq!(origins(&d, 0, &o), a);
    assert_eq!(origins(&d, 1, &o), b);
    let both = origins(&d, 2, &o);
    assert_eq!(both[..2], a[..]);
    assert_eq!(
        both[2..].iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
        ["x", "y"]
    );
}

/// A font loaded under the default limits still fails under limits it
/// does not fit, and loads again under the defaults afterwards.
#[test]
fn a_cached_font_does_not_bypass_other_limits() {
    let pdf = doc(
        &[(
            "BT /A 10 Tf 10 10 Td (ab) Tj ET",
            "/Font << /A 20 0 R >>",
            "",
        )],
        vec![font(20, 500, "/Encoding /WinAnsiEncoding")],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let page = &d.pages()[0];
    assert_eq!(
        extract_text(&d, page, &TextOptions::default())
            .unwrap()
            .chars
            .len(),
        2
    );
    let small = TextOptions::default().with_limits(Limits::default().with_max_stream_bytes(1000));
    assert!(
        extract_text(&d, page, &small).is_err(),
        "the program exceeds the limit"
    );
    assert_eq!(
        extract_text(&d, page, &TextOptions::default())
            .unwrap()
            .chars
            .len(),
        2
    );
}

/// What loading a font records (here a malformed ToUnicode entry) is
/// recorded on every use, as without the cache.
#[test]
fn a_cached_font_records_its_warnings_again() {
    let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                 1 begincodespacerange <00> <FF> endcodespacerange \
                 2 beginbfchar <61> <0041> <62> <41> endbfchar endcmap end end";
    let mut extra = vec![font(
        20,
        500,
        "/Encoding /WinAnsiEncoding /ToUnicode 30 0 R",
    )];
    extra.push((30, stream_obj("", cmap)));
    let pdf = doc(
        &[(
            "BT /A 10 Tf 10 10 Td (ab) Tj ET",
            "/Font << /A 20 0 R >>",
            "",
        )],
        extra,
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let page = &d.pages()[0];
    let mut seen = Vec::new();
    for _ in 0..3 {
        extract_text(&d, page, &TextOptions::default()).unwrap();
        seen.push(d.take_warnings());
    }
    assert!(!seen[0].is_empty(), "the CMap is malformed");
    assert_eq!(seen[1], seen[0]);
    assert_eq!(seen[2], seen[0]);
}

/// Copies of removed text are looked for in everything a page shows:
/// optional content that is off, and annotation appearances.
#[test]
fn copies_in_hidden_content_and_appearances_are_found() {
    let res = "/Font << /A 20 0 R >>";
    let secret = "BT /A 10 Tf 60 100 Td (CONFIDENTIAL) Tj ET";
    for (other, extra_page, extra) in [
        (
            "/OC /Off BDC BT /A 10 Tf 10 10 Td (CONFIDENTIAL) Tj ET EMC",
            "",
            vec![(40, b"<< /Type /OCG /Name (L) >>".to_vec())],
        ),
        (
            "BT /A 10 Tf 10 150 Td (public) Tj ET",
            "/Annots [41 0 R]",
            vec![
                (
                    41,
                    b"<< /Type /Annot /Subtype /Square /Rect [10 10 90 30] /AP << /N 42 0 R >> >>"
                        .to_vec(),
                ),
                (
                    42,
                    stream_obj(
                        "/Type /XObject /Subtype /Form /BBox [0 0 80 20] /Resources << /Font << /A 20 0 R >> >>",
                        b"BT /A 8 Tf 2 5 Td (CONFIDENTIAL) Tj ET",
                    ),
                ),
            ],
        ),
    ] {
        let mut objs = vec![font(20, 500, "/Encoding /WinAnsiEncoding")];
        objs.extend(extra.clone());
        let oc = "/Properties << /Off 40 0 R >>";
        let pdf = build_doc_with_oc(
            &[(secret, res, ""), (other, &format!("{res} {oc}"), extra_page)],
            objs,
        );
        let d = Document::from_bytes(&pdf).unwrap();
        let r = redact(&d, &[(0, vec![(50.0, 90.0, 190.0, 115.0)])], &RedactOptions::new())
            .unwrap_or_else(|e| panic!("{other}: {e}"));
        assert!(
            r.elsewhere.iter().any(|e| e.location == ElsewhereLocation::Page(1)),
            "{other}: {:?}",
            r.elsewhere
        );
    }
}

/// [`doc`] with an /OCProperties turning the group of object 40 off.
fn build_doc_with_oc(pages: &[(&str, &str, &str)], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut extra = extra;
    if !extra.iter().any(|(n, _)| *n == 40) {
        extra.push((40, b"<< /Type /OCG /Name (L) >>".to_vec()));
    }
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 100 + 2 * i))
        .collect();
    let mut objs = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [40 0 R] \
              /D << /OFF [40 0 R] >> >> >>"
                .to_vec(),
        ),
        (
            2,
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            )
            .into_bytes(),
        ),
    ];
    for (i, (c, res, pe)) in pages.iter().enumerate() {
        let n = 100 + 2 * i as u32;
        objs.push((
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << {res} >> \
                 /Contents {} 0 R {pe} >>",
                n + 1
            )
            .into_bytes(),
        ));
        objs.push((n + 1, stream_obj("", c.as_bytes())));
    }
    objs.extend(shared());
    objs.extend(extra);
    build_pdf(&objs)
}

/// A page whose annotation appearance cannot be read still has its own
/// text searched, and is reported as not wholly searched.
#[test]
fn an_unreadable_appearance_leaves_the_page_text_searched() {
    let res = "/Font << /A 20 0 R >>";
    let objs = vec![
        font(20, 500, "/Encoding /WinAnsiEncoding"),
        (
            41,
            b"<< /Type /Annot /Subtype /Square /Rect [10 10 90 30] /AP << /N 42 0 R >> >>".to_vec(),
        ),
        (
            42,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 80 20]",
                b"BT /Missing 8 Tf (x) Tj ET",
            ),
        ),
    ];
    let pdf = doc(
        &[
            ("BT /A 10 Tf 60 100 Td (CONFIDENTIAL) Tj ET", res, ""),
            (
                "BT /A 10 Tf 10 150 Td (CONFIDENTIAL) Tj ET",
                res,
                "/Annots [41 0 R]",
            ),
        ],
        objs,
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &d,
        &[(0, vec![(50.0, 90.0, 190.0, 115.0)])],
        &RedactOptions::new(),
    )
    .unwrap();
    assert!(
        r.elsewhere
            .iter()
            .any(|e| e.location == ElsewhereLocation::Page(1)),
        "{:?}",
        r.elsewhere
    );
    assert_eq!(r.elsewhere_unsearched_pages, vec![1]);
}
