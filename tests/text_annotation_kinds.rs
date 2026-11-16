//! T1 (v0.11): what each kind of annotation contributes as text
//! (`AnnotationText::shown_text`, `AnnotationText::note`,
//! `AnnotationText::recovered`; `docs/annotation-text.md`).
//!
//! * `shown_text` — the characters its normal appearance draws on the
//!   page: what a viewer shows and what Poppler and MuPDF extract.
//! * `note` — the comment a markup annotation carries in `/Contents`
//!   and shows in its pop-up, not on the page. `/Contents` of a
//!   free-text annotation is its drawn text (part of `shown_text`), of a
//!   widget or a link an alternate description, of other annotations
//!   an alternate description: none of these is a note.
//! * Never text: `/T` (an author, or a field's name), `/Subj`, `/NM`,
//!   `/TU`, a field's `/DV`, `/RC` (the rich-text form of `/Contents`).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, AnnotationText, Document, Error, ExtractedText, Limits, OpenOptions, TextOptions,
};

/// The identifiers and metadata every annotation below carries; none
/// may ever come out as text.
const META: &str =
    "/T (AUTHORX) /Subj (SUBJX) /NM (NMX) /TU (TOOLTIPX) /RC (<body><p>RICHX</p></body>)";
const META_WORDS: [&str; 5] = ["AUTHORX", "SUBJX", "NMX", "TOOLTIPX", "RICHX"];

/// A page showing "Body" with `/Annots [annots]`; `objects` from 10.
fn pdf(annots: &str, objects: Vec<(u32, Vec<u8>)>, catalog_extra: &str) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
                 /Resources << /Font << /F 5 0 R >> >> /Annots [{annots}] >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"BT /F 12 Tf 10 280 Td (Body) Tj ET")),
        (
            5,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ),
    ];
    objs.extend(objects);
    build_pdf(&objs)
}

/// An appearance stream drawing `text` with Helvetica in a 100 × 20 box.
fn ap(text: &str) -> Vec<u8> {
    ap_content(&format!("BT /H 10 Tf 2 5 Td ({text}) Tj ET"))
}

fn ap_content(content: &str) -> Vec<u8> {
    stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /H 5 0 R >> >>",
        content.as_bytes(),
    )
}

/// One annotation (object 10) with its appearance in object 11 when
/// `appearance` is given.
fn one(annot: &str, appearance: Option<&str>) -> Vec<u8> {
    let ap_entry = if appearance.is_some() {
        "/AP << /N 11 0 R >>"
    } else {
        ""
    };
    let mut objs = vec![(
        10,
        format!("<< /Type /Annot /Rect [10 100 110 120] {annot} {ap_entry} >>").into_bytes(),
    )];
    if let Some(a) = appearance {
        objs.push((11, ap(a)));
    }
    let catalog = if annot.contains("/Widget") {
        "/AcroForm << /Fields [10 0 R] /DA (/Helv 10 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >>"
    } else {
        ""
    };
    pdf("10 0 R", objs, catalog)
}

fn extract(pdf: &[u8]) -> ExtractedText {
    try_extract(pdf, false).unwrap()
}

fn try_extract(pdf: &[u8], recovering: bool) -> Result<ExtractedText, Error> {
    let doc = Document::open(pdf, &OpenOptions::new().with_recovery(recovering))?;
    extract_text(
        &doc,
        &doc.pages()[0],
        &TextOptions::new().with_annotations(true),
    )
}

fn only(t: &ExtractedText) -> &AnnotationText {
    assert_eq!(t.annotations.len(), 1, "{:?}", t.annotations);
    &t.annotations[0]
}

/// (shown_text, note) of the page's single annotation, after checking
/// that no identifier or metadata came out as text.
fn kinds(pdf: &[u8]) -> (Option<String>, Option<String>) {
    let t = extract(pdf);
    let a = only(&t);
    for w in META_WORDS {
        for s in [a.shown_text(), a.note(), a.text()].into_iter().flatten() {
            assert!(!s.contains(w), "{w} came out as text: {s:?}");
        }
    }
    assert!(!a.recovered, "a conforming annotation is not recovered");
    (a.shown_text(), a.note())
}

fn s(x: &str) -> Option<String> {
    Some(x.to_string())
}

#[test]
fn markup_annotations_show_their_appearance_and_carry_their_contents_as_a_note() {
    // A note (sticky note): its icon draws no text; /Contents is the note.
    assert_eq!(
        kinds(&one(
            &format!("/Subtype /Text /Contents (NOTEX) {META}"),
            None
        )),
        (None, s("NOTEX"))
    );
    // With an appearance that draws text, both, apart.
    assert_eq!(
        kinds(&one(
            &format!("/Subtype /Text /Contents (NOTEX) {META}"),
            Some("ICONX")
        )),
        (s("ICONX"), s("NOTEX"))
    );
    for sub in [
        "Stamp",
        "Highlight",
        "Underline",
        "Squiggly",
        "StrikeOut",
        "Square",
        "Circle",
        "Line",
        "Polygon",
        "PolyLine",
        "Ink",
        "Caret",
        "FileAttachment",
    ] {
        let extra = match sub {
            "Highlight" | "Underline" | "Squiggly" | "StrikeOut" => {
                "/QuadPoints [10 120 110 120 10 100 110 100]"
            }
            "Line" => "/L [10 100 110 120]",
            "Polygon" | "PolyLine" => "/Vertices [10 100 110 120 60 110]",
            "Ink" => "/InkList [[10 100 110 120]]",
            _ => "",
        };
        assert_eq!(
            kinds(&one(
                &format!("/Subtype /{sub} {extra} /Contents (NOTEX) {META}"),
                Some("DRAWNX")
            )),
            (s("DRAWNX"), s("NOTEX")),
            "{sub}"
        );
    }
    // An empty or blank /Contents is no note.
    assert_eq!(
        kinds(&one("/Subtype /Text /Contents ( )", None)),
        (None, None)
    );
}

#[test]
fn free_text_contents_is_its_drawn_text_not_a_note() {
    // Drawn from its appearance: what it shows, even when /Contents
    // says otherwise (Poppler and MuPDF extract the appearance).
    let p = one(
        &format!("/Subtype /FreeText /DA (/Helv 10 Tf 0 g) /Contents (CONTENTSX) {META}"),
        Some("DRAWNX"),
    );
    assert_eq!(kinds(&p), (s("DRAWNX"), None));
    // Equal: once, not twice.
    assert_eq!(
        kinds(&one(
            "/Subtype /FreeText /DA (/Helv 10 Tf 0 g) /Contents (SAMEX)",
            Some("SAMEX")
        )),
        (s("SAMEX"), None)
    );
    // Without an appearance: the one drawn from /Contents and /DA.
    let (shown, note) = kinds(&one(
        &format!("/Subtype /FreeText /DA (/Helv 10 Tf 0 g) /Contents (CONTENTSX) {META}"),
        None,
    ));
    assert_eq!(shown.as_deref().map(|t| t.replace(' ', "")), s("CONTENTSX"));
    assert_eq!(note, None);
}

#[test]
fn links_widgets_and_other_annotations_have_no_note() {
    // A link's /Contents is an alternate description (§12.5.2).
    assert_eq!(
        kinds(&one(
            &format!("/Subtype /Link /Contents (ALTX) {META} /A << /S /URI /URI (http://example.invalid/) >>"),
            None
        )),
        (None, None)
    );
    for sub in [
        "Screen",
        "PrinterMark",
        "Watermark",
        "Movie",
        "NotARealSubtype",
    ] {
        assert_eq!(
            kinds(&one(
                &format!("/Subtype /{sub} /Contents (ALTX) {META}"),
                Some("DRAWNX")
            )),
            (s("DRAWNX"), None),
            "{sub}"
        );
    }
}

#[test]
fn widgets_show_their_appearance_and_report_value_and_default_apart() {
    let tx = "/Subtype /Widget /FT /Tx /T (FIELDNAMEX) /TU (TOOLTIPX) /V (VALUEX) /DV (DEFAULTX) \
              /DA (/Helv 10 Tf 0 g) /Contents (WALTX)";
    // Appearance and value agree.
    let t = extract(&one(tx, Some("VALUEX")));
    let a = only(&t);
    assert_eq!((a.shown_text(), a.note()), (s("VALUEX"), None));
    let f = a.field.as_ref().unwrap();
    assert_eq!(
        (f.name.as_str(), f.value.clone(), f.default_value.clone()),
        (
            "FIELDNAMEX",
            vec!["VALUEX".to_string()],
            vec!["DEFAULTX".to_string()]
        )
    );
    // A stale appearance: shown is what is drawn; the value stays in
    // `field.value` (and `text()`, unchanged, still prefers it).
    let t = extract(&one(tx, Some("STALEX")));
    let a = only(&t);
    assert_eq!((a.shown_text(), a.note()), (s("STALEX"), None));
    assert_eq!(a.field.as_ref().unwrap().value, vec!["VALUEX".to_string()]);
    assert_eq!(a.text(), s("VALUEX"));
    // No appearance: the synthesised one shows the value.
    let t = extract(&one(tx, None));
    assert_eq!(
        only(&t).shown_text().map(|t| t.replace(' ', "")),
        s("VALUEX")
    );
    // The default value is never shown when there is no value.
    let t = extract(&one(
        "/Subtype /Widget /FT /Tx /T (F) /DV (DEFAULTX) /DA (/Helv 10 Tf 0 g)",
        None,
    ));
    let a = only(&t);
    assert_eq!((a.shown_text(), a.note()), (None, None));
}

#[test]
fn a_popup_is_reported_by_its_parent_only() {
    let p = pdf(
        "10 0 R 12 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /Text /Rect [10 100 30 120] /Contents (PARENTX) /Popup 12 0 R >>".to_vec()),
            (12, b"<< /Type /Annot /Subtype /Popup /Rect [30 50 200 100] /Parent 10 0 R /Open true >>".to_vec()),
        ],
        "",
    );
    let t = extract(&p);
    let a = only(&t);
    assert_eq!((a.subtype.as_str(), a.note()), ("Text", s("PARENTX")));
}

#[test]
fn hidden_annotations_show_nothing_and_carry_no_note() {
    // Hidden (bit 2), NoView (bit 6): nothing. NoPrint (bit 3 unset)
    // is shown on screen and extracted.
    for (flags, shown) in [
        ("/F 2", false),
        ("/F 32", false),
        ("/F 0", true),
        ("/F 4", true),
    ] {
        let t = extract(&one(
            &format!("/Subtype /Stamp {flags} /Contents (NOTEX)"),
            Some("DRAWNX"),
        ));
        let a = only(&t);
        assert_eq!(a.hidden, !shown, "{flags}");
        assert_eq!(
            (a.shown_text(), a.note()),
            if shown {
                (s("DRAWNX"), s("NOTEX"))
            } else {
                (None, None)
            },
            "{flags}"
        );
    }
    // Optional content OFF.
    let p = pdf(
        "10 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /Stamp /Rect [10 100 110 120] /Contents (NOTEX) /OC 12 0 R /AP << /N 11 0 R >> >>".to_vec()),
            (11, ap("DRAWNX")),
            (12, b"<< /Type /OCG /Name (Layer) >>".to_vec()),
        ],
        "/OCProperties << /OCGs [12 0 R] /D << /OFF [12 0 R] >> >>",
    );
    let t = extract(&p);
    let a = only(&t);
    assert!(a.hidden);
    assert_eq!((a.shown_text(), a.note()), (None, None));
}

#[test]
fn equal_strings_of_different_annotations_are_each_reported() {
    let p = pdf(
        "10 0 R 12 0 R 10 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /FreeText /Rect [10 100 110 120] /DA (/Helv 10 Tf 0 g) /AP << /N 11 0 R >> >>".to_vec()),
            (11, ap("SAMEX")),
            (12, b"<< /Type /Annot /Subtype /FreeText /Rect [10 150 110 170] /DA (/Helv 10 Tf 0 g) /AP << /N 13 0 R >> >>".to_vec()),
            (13, ap("SAMEX")),
        ],
        "",
    );
    let t = extract(&p);
    // Two annotations with the same words: both. The same annotation
    // listed twice: once.
    let shown: Vec<_> = t
        .annotations
        .iter()
        .map(|a| (a.index, a.shown_text()))
        .collect();
    assert_eq!(shown, vec![(0, s("SAMEX")), (1, s("SAMEX"))]);
}

/// A Type 0 font (Identity-H) whose ToUnicode maps 1-4 to 日, 本, 中, 한.
fn cjk_objects() -> Vec<(u32, Vec<u8>)> {
    let font = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
                /CMapName /U def 1 begincodespacerange <0000> <FFFF> endcodespacerange \
                4 beginbfchar <0001> <65E5> <0002> <672C> <0003> <4E2D> <0004> <D55C> endbfchar \
                endcmap CMapName currentdict /CMap defineresource pop end end";
    vec![
        (
            20,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding /Identity-H \
              /DescendantFonts [21 0 R] /ToUnicode 24 0 R >>"
                .to_vec(),
        ),
        (
            21,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 1000 /CIDToGIDMap /Identity /FontDescriptor 22 0 R >>"
                .to_vec(),
        ),
        (
            22,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 23 0 R >>"
                .to_vec(),
        ),
        (23, stream_obj(&format!("/Length1 {}", font.len()), &font)),
        (24, stream_obj("", cmap.as_bytes())),
    ]
}

#[test]
fn unicode_japanese_and_cjk() {
    // A note in UTF-16BE (日本語のメモ), one in UTF-8 with its BOM
    // (中文 한국어), one in PDFDocEncoding (Café): decoded, not
    // normalised.
    let utf16: String = "日本語のメモ"
        .encode_utf16()
        .flat_map(|u| u.to_be_bytes())
        .map(|b| format!("{b:02X}"))
        .collect();
    let utf8: String = "中文 한국어 e\u{301}"
        .bytes()
        .map(|b| format!("{b:02X}"))
        .collect();
    let p = pdf(
        "10 0 R 11 0 R 12 0 R 13 0 R",
        [
            (10, format!("<< /Type /Annot /Subtype /Text /Rect [10 100 30 120] /Contents <FEFF{utf16}> >>")),
            (11, format!("<< /Type /Annot /Subtype /Text /Rect [40 100 60 120] /Contents <EFBBBF{utf8}> >>")),
            (12, "<< /Type /Annot /Subtype /Text /Rect [70 100 90 120] /Contents (Caf\\351) >>".to_string()),
            (
                13,
                "<< /Type /Annot /Subtype /Stamp /Rect [10 150 110 170] /AP << /N 14 0 R >> >>".to_string(),
            ),
        ]
        .into_iter()
        .map(|(n, d)| (n, d.into_bytes()))
        .chain([(
            14,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /C 20 0 R >> >>",
                b"BT /C 10 Tf 2 5 Td <0001000200030004> Tj ET",
            ),
        )])
        .chain(cjk_objects())
        .collect(),
        "",
    );
    let t = extract(&p);
    let notes: Vec<_> = t.annotations.iter().map(|a| a.note()).collect();
    assert_eq!(
        notes,
        vec![
            s("日本語のメモ"),
            s("中文 한국어 e\u{301}"),
            s("Café"),
            None
        ]
    );
    assert_eq!(t.annotations[3].shown_text(), s("日本中한"));
}

#[test]
fn invalid_string_encodings_are_replaced_not_refused() {
    // An odd UTF-16 tail, an unpaired surrogate, invalid UTF-8 after
    // its BOM, undefined PDFDocEncoding codes.
    for (raw, want) in [
        ("<FEFF0041D800>", "A\u{FFFD}"),
        ("<FEFF004100>", "A"),
        ("<EFBBBF41FF42>", "A\u{FFFD}B"),
        ("<41AD7F42>", "A\u{FFFD}\u{FFFD}B"),
    ] {
        let t = extract(&one(&format!("/Subtype /Text /Contents {raw}"), None));
        assert_eq!(only(&t).note().as_deref(), Some(want), "{raw}");
    }
}

#[test]
fn a_broken_appearance_fails_strictly_and_is_skipped_and_marked_in_recovery() {
    // The appearance state is not a stream.
    let p = pdf(
        "10 0 R 12 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /Stamp /Rect [10 100 110 120] /Contents (NOTEX) /AP << /N 11 0 R >> >>".to_vec()),
            (11, b"<< /NotAStream true >>".to_vec()),
            (12, b"<< /Type /Annot /Subtype /Stamp /Rect [10 150 110 170] /Contents (OKX) /AP << /N 13 0 R >> >>".to_vec()),
            (13, ap("DRAWNX")),
        ],
        "",
    );
    assert!(
        try_extract(&p, false).is_err(),
        "strict mode refuses the page"
    );
    let t = try_extract(&p, true).unwrap();
    let (a, b) = (&t.annotations[0], &t.annotations[1]);
    assert!(a.recovered && !b.recovered);
    assert_eq!((a.shown_text(), a.note()), (None, s("NOTEX")));
    assert_eq!((b.shown_text(), b.note()), (s("DRAWNX"), s("OKX")));
    // An appearance whose content needs a recovery rule (an operator
    // with the wrong operand count, R1): drawn in part, and marked.
    let p = pdf(
        "10 0 R",
        vec![
            (10, b"<< /Type /Annot /Subtype /Stamp /Rect [10 100 110 120] /Contents (NOTEX) /AP << /N 11 0 R >> >>".to_vec()),
            (11, ap_content("0 0 10 re f BT /H 10 Tf 2 5 Td (PARTX) Tj ET")),
        ],
        "",
    );
    assert!(try_extract(&p, false).is_err());
    let t = try_extract(&p, true).unwrap();
    let a = only(&t);
    assert!(a.recovered);
    assert_eq!(a.shown_text(), s("PARTX"));
    // A conforming document opened in recovery mode: nothing recovered.
    let t = try_extract(
        &one("/Subtype /Stamp /Contents (NOTEX)", Some("DRAWNX")),
        true,
    )
    .unwrap();
    assert!(!only(&t).recovered);
}

/// The recovery flag counts every rule applied, also those whose
/// warnings the full warning list could not keep: here a font whose
/// /Widths names 1,030 undefined objects (a warning each) before an
/// invalid /Differences entry is skipped (a recovery rule), loaded
/// twice (the second time from the font cache). (Review.)
#[test]
fn a_recovery_beyond_the_warning_list_still_marks_the_annotation() {
    let widths: String = (0..1030).map(|i| format!("{} 0 R ", 5000 + i)).collect();
    let font = format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 0 /LastChar 1029 \
         /Widths [{widths}] /Encoding << /Differences [65 /A true] >> >>"
    );
    let appearance = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /W 20 0 R >> >>",
        b"BT /W 10 Tf 2 5 Td (A) Tj ET",
    );
    let p = pdf(
        "10 0 R 12 0 R",
        vec![
            (
                10,
                b"<< /Type /Annot /Subtype /Stamp /Rect [10 100 110 120] /AP << /N 11 0 R >> >>"
                    .to_vec(),
            ),
            (11, appearance.clone()),
            (
                12,
                b"<< /Type /Annot /Subtype /Stamp /Rect [10 150 110 170] /AP << /N 13 0 R >> >>"
                    .to_vec(),
            ),
            (13, appearance),
            (20, font.into_bytes()),
        ],
        "",
    );
    let doc = Document::open(&p, &OpenOptions::new().with_recovery(true)).unwrap();
    let t = extract_text(
        &doc,
        &doc.pages()[0],
        &TextOptions::new().with_annotations(true),
    )
    .unwrap();
    assert!(doc.warnings_dropped() > 0, "the warning list overflowed");
    let rec: Vec<bool> = t.annotations.iter().map(|a| a.recovered).collect();
    assert_eq!(rec, vec![true, true]);
}

#[test]
fn several_pages_keep_their_own_annotations() {
    let objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Annots [10 0 R] >>".to_vec(),
        ),
        (
            6,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Annots [12 0 R] >>".to_vec(),
        ),
        (
            10,
            b"<< /Type /Annot /Subtype /Text /Rect [10 100 30 120] /Contents (ONEX) >>".to_vec(),
        ),
        (
            12,
            b"<< /Type /Annot /Subtype /Text /Rect [10 100 30 120] /Contents (TWOX) >>".to_vec(),
        ),
    ];
    let p = build_pdf(&objs);
    let doc = Document::from_bytes(&p).unwrap();
    let notes: Vec<_> = doc
        .pages()
        .iter()
        .map(|pg| {
            extract_text(&doc, pg, &TextOptions::new().with_annotations(true))
                .unwrap()
                .annotations[0]
                .note()
        })
        .collect();
    assert_eq!(notes, vec![s("ONEX"), s("TWOX")]);
}

#[test]
fn cycles_depth_and_the_text_budget_are_errors() {
    // A /Parent cycle in a widget's field chain.
    let p = pdf(
        "10 0 R",
        vec![
            (
                10,
                b"<< /Type /Annot /Subtype /Widget /Rect [10 100 110 120] /Parent 12 0 R >>"
                    .to_vec(),
            ),
            (12, b"<< /T (a) /Parent 13 0 R >>".to_vec()),
            (13, b"<< /T (b) /Parent 12 0 R >>".to_vec()),
        ],
        "",
    );
    // Refused, not followed forever: as a cycle, or by the depth bound
    // of the inherited-attribute walk that meets it first.
    let e = try_extract(&p, false);
    assert!(
        matches!(
            e,
            Err(Error::CycleDetected { .. })
                | Err(Error::LimitExceeded {
                    limit: "form field /Parent depth",
                    ..
                })
        ),
        "{e:?}"
    );
    // A /Parent chain deeper than the bound.
    let mut objs = vec![(
        10,
        b"<< /Type /Annot /Subtype /Widget /Rect [10 100 110 120] /Parent 100 0 R >>".to_vec(),
    )];
    for i in 0..40u32 {
        objs.push((
            100 + i,
            format!("<< /T (p{i}) /Parent {} 0 R >>", 101 + i).into_bytes(),
        ));
    }
    objs.push((140, b"<< /T (root) >>".to_vec()));
    assert!(matches!(
        try_extract(&pdf("10 0 R", objs, ""), false),
        Err(Error::LimitExceeded { .. })
    ));
    // A note beyond `max_extracted_unicode_bytes`.
    let big = "N".repeat(5000);
    let p = one(&format!("/Subtype /Text /Contents ({big})"), None);
    let doc = Document::from_bytes(&p).unwrap();
    let mut limits = Limits::default();
    limits.max_extracted_unicode_bytes = 1000;
    let e = extract_text(
        &doc,
        &doc.pages()[0],
        &TextOptions::new()
            .with_limits(limits)
            .with_annotations(true),
    );
    assert!(
        matches!(
            e,
            Err(Error::LimitExceeded {
                limit: "max_extracted_unicode_bytes",
                ..
            })
        ),
        "{e:?}"
    );
}

#[test]
fn without_the_option_nothing_changes() {
    let p = one(
        &format!("/Subtype /FreeText /DA (/Helv 10 Tf 0 g) /Contents (CONTENTSX) {META}"),
        Some("DRAWNX"),
    );
    let doc = Document::from_bytes(&p).unwrap();
    let off = extract_text(&doc, &doc.pages()[0], &TextOptions::new()).unwrap();
    let on = extract_text(
        &doc,
        &doc.pages()[0],
        &TextOptions::new().with_annotations(true),
    )
    .unwrap();
    assert!(off.annotations.is_empty());
    let text = |t: &ExtractedText| -> String {
        t.chars.iter().filter_map(|c| c.unicode.clone()).collect()
    };
    assert_eq!(text(&off), "Body");
    assert_eq!(text(&on), "Body");
    assert_eq!(format!("{:?}", off.chars), format!("{:?}", on.chars));
}
