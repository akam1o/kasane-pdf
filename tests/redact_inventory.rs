//! `uninspected_data`, `Redaction::uninspected` and
//! `RedactOptions::refuse_uninspected`: what of a document `redact`
//! reads, and what it carries unread.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    diagnose_redaction, redact, uninspected_data, DataRole, DataStatus, Document, Error, Limits,
    RedactOptions,
};

const RECT: (f64, f64, f64, f64) = (45.0, 90.0, 160.0, 115.0);

fn font_objs() -> Vec<(u32, Vec<u8>)> {
    let ff = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let widths = vec!["600"; 95].join(" ");
    vec![
        (
            20,
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
                 /LastChar 126 /Widths [{widths}] /Encoding /WinAnsiEncoding \
                 /FontDescriptor 21 0 R >>"
            )
            .into_bytes(),
        ),
        (
            21,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 22 0 R >>"
                .to_vec(),
        ),
        (22, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
    ]
}

const TEXT: &str =
    "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET";

fn doc(
    content: &str,
    res: &str,
    page_extra: &str,
    catalog_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                 /Font << /F1 20 0 R >> {res} >> /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

fn run(pdf: &[u8], o: RedactOptions) -> Result<kasane_pdf::Redaction, Error> {
    let d = Document::from_bytes(pdf).unwrap();
    redact(&d, &[(0, vec![RECT])], &o)
}

fn roles(inv: &kasane_pdf::DataInventory) -> Vec<DataRole> {
    inv.uninspected_items().map(|i| i.role).collect()
}

/// Content, fonts and images: searched or interpreted, nothing listed;
/// the strict mode accepts the redaction.
#[test]
fn a_plain_document_has_nothing_uninspected() {
    let image = stream_obj(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray \
         /BitsPerComponent 8",
        &[1, 2, 3, 4],
    );
    let pdf = doc(
        &format!("{TEXT} q 20 0 0 20 150 150 cm /Im1 Do Q"),
        "/XObject << /Im1 30 0 R >>",
        "",
        "",
        vec![(30, image)],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert!(inv.complete);
    assert_eq!(inv.uninspected, 0, "{:?}", inv.items);
    assert_eq!(inv.searched, 1, "the page content");
    assert_eq!(inv.interpreted, 2, "the font program and the image");
    let r = run(&pdf, RedactOptions::new().refuse_uninspected(true)).unwrap();
    assert_eq!(r.uninspected.uninspected, 0);
}

/// An attachment, private data, XMP on a page, a thumbnail, an XFA
/// packet, rich text: each listed by its role; the strict mode refuses.
#[test]
fn uninspected_roles_are_listed_and_refused_on_request() {
    let pdf = doc(
        TEXT,
        "",
        "/Metadata 31 0 R /PieceInfo << /App << /Private 32 0 R >> >> /Annots [33 0 R]",
        "/Names << /EmbeddedFiles << /Names [(a.txt) 34 0 R] >> >> \
         /AcroForm << /Fields [] /XFA [(template) 35 0 R] >>",
        vec![
            (31, stream_obj("/Type /Metadata /Subtype /XML", b"<x/>")),
            (32, stream_obj("", b"app")),
            (
                33,
                b"<< /Type /Annot /Subtype /FreeText /Rect [150 150 160 160] /RC 36 0 R >>"
                    .to_vec(),
            ),
            (
                34,
                b"<< /Type /Filespec /F (a.txt) /EF << /F 37 0 R >> >>".to_vec(),
            ),
            (35, stream_obj("", b"<template/>")),
            (36, stream_obj("", b"<body/>")),
            (
                37,
                stream_obj("/Type /EmbeddedFile /Filter /FlateDecode", b"x"),
            ),
        ],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    let mut got = roles(&inv);
    got.sort();
    assert_eq!(
        got,
        vec![
            DataRole::EmbeddedFile,
            DataRole::Metadata,
            DataRole::Xfa,
            DataRole::RichText,
            DataRole::PrivateData,
        ]
    );
    let ef = inv
        .uninspected_items()
        .find(|i| i.role == DataRole::EmbeddedFile)
        .unwrap();
    assert_eq!(ef.filters, vec!["FlateDecode".to_string()]);
    assert_eq!(ef.stream_bytes, Some(1));
    assert!(
        ef.via.starts_with("/Root/Names/EmbeddedFiles"),
        "{}",
        ef.via
    );
    assert!(!format!("{:?}", inv).contains("SECRET"));
    // The diagnosis reports the same.
    let diag = diagnose_redaction(&d, 0, &[RECT], &Limits::default()).unwrap();
    assert_eq!(diag.data.uninspected, inv.uninspected);
    // `redact` lists them; the strict mode refuses.
    let r = run(&pdf, RedactOptions::new()).unwrap();
    assert_eq!(r.uninspected.uninspected, 5);
    let e = run(&pdf, RedactOptions::new().refuse_uninspected(true)).unwrap_err();
    assert!(e.to_string().contains("uninspected"), "{e}");
}

/// A form nothing draws is not searched: uninspected. The same form
/// drawn is searched.
#[test]
fn undrawn_forms() {
    let form = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /Resources << /Font << /F1 20 0 R >> >>",
        b"BT /F1 12 Tf 10 150 Td (COPY) Tj ET",
    );
    let unused = doc(
        TEXT,
        "/XObject << /Fm 30 0 R >>",
        "",
        "",
        vec![(30, form.clone())],
    );
    let d = Document::from_bytes(&unused).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(roles(&inv), vec![DataRole::Undrawn]);
    let drawn = doc(
        &format!("{TEXT} /Fm Do"),
        "/XObject << /Fm 30 0 R >>",
        "",
        "",
        vec![(30, form)],
    );
    let d = Document::from_bytes(&drawn).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(inv.uninspected, 0);
    assert_eq!(inv.searched, 2);
}

/// Unknown entries of the catalog and a page, direct or indirect, and a
/// stream under a key no modelled structure names.
#[test]
fn unknown_entries_and_streams() {
    let pdf = doc(
        TEXT,
        "",
        "/XYZPrivate (direct)",
        "/XYZData 30 0 R",
        vec![
            (30, b"<< /Nested 31 0 R >>".to_vec()),
            (31, stream_obj("", b"payload")),
        ],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    let items: Vec<_> = inv.uninspected_items().collect();
    assert!(items
        .iter()
        .any(|i| i.role == DataRole::UnknownEntry && i.object.is_some_and(|r| r.number == 30)));
    assert!(items
        .iter()
        .any(|i| i.role == DataRole::UnknownStream && i.object.is_some_and(|r| r.number == 31)));
    assert!(items
        .iter()
        .any(|i| i.role == DataRole::UnknownEntry && i.via.ends_with("/XYZPrivate")));
    // A font dictionary holding a stream under an unknown key: the
    // stream is unknown, the font is not listed.
    let pdf = doc(
        TEXT,
        "/Font << /F1 20 0 R /F2 40 0 R >>",
        "",
        "",
        vec![
            (
                40,
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /XYZBlob 41 0 R >>".to_vec(),
            ),
            (41, stream_obj("", b"blob")),
        ],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(roles(&inv), vec![DataRole::UnknownStream]);
}

/// A number under an unknown entry holds nothing; a transparency
/// group's ICC colour space is interpreted.
#[test]
fn scalars_and_group_colour_spaces() {
    let form = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 200 200] \
         /Group << /S /Transparency /CS [/ICCBased 31 0 R] >>",
        b"",
    );
    let icc = stream_obj("/N 1", b"icc");
    let pdf = doc(
        &format!("{TEXT} /Fm Do"),
        "/XObject << /Fm 30 0 R >>",
        "/XYZPageNum 32 0 R /XYZFlag true /ProcSet [/PDF]",
        "",
        vec![(30, form), (31, icc), (32, b"7".to_vec())],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(inv.uninspected, 0, "{:?}", inv.items);
}

/// An object nothing refers to is not carried; a known object reached
/// also through an unknown entry is not listed.
#[test]
fn unreachable_and_shared_objects() {
    let pdf = doc(
        TEXT,
        "",
        "/XYZFont 20 0 R",
        "",
        vec![(30, stream_obj("", b"orphan"))],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(inv.uninspected, 0, "{:?}", inv.items);
    assert_eq!(inv.not_carried, 1);
    let it = &inv.items[0];
    assert_eq!(
        (it.role, it.status, it.object.unwrap().number),
        (DataRole::Unreachable, DataStatus::NotCarried, 30)
    );
    let r = run(&pdf, RedactOptions::new()).unwrap();
    assert_eq!(
        r.uninspected.not_carried, 0,
        "redact writes what is reachable"
    );
}

/// The edited page's thumbnail is removed; another page's is listed.
#[test]
fn thumbnails() {
    let thumb = stream_obj(
        "/Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[9, 9, 9, 9],
    );
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec(),
        ),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
              /Font << /F1 20 0 R >> >> /Contents 4 0 R /Thumb 30 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", TEXT.as_bytes())),
        (
            5,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R \
              /Thumb 31 0 R >>"
                .to_vec(),
        ),
        (6, stream_obj("", b"")),
        (30, thumb.clone()),
        (31, thumb),
    ];
    objs.extend(font_objs());
    let pdf = build_pdf(&objs);
    let r = run(&pdf, RedactOptions::new()).unwrap();
    assert_eq!(r.thumbnails_removed, 1);
    assert_eq!(roles(&r.uninspected), vec![DataRole::Thumbnail]);
}

/// A walk stopped by a limit is incomplete (`redact`'s strict mode
/// refuses such an inventory), and calls nothing unreachable.
#[test]
fn an_incomplete_inventory_says_so() {
    let pdf = doc(TEXT, "", "", "", vec![]);
    let d = Document::from_bytes(&pdf).unwrap();
    let small = Limits::default().with_max_audit_elements(3);
    let inv = uninspected_data(&d, &small).unwrap();
    assert!(!inv.complete);
    assert_eq!(inv.not_carried, 0, "nothing is called unreachable");
}

/// What only a back edge (`/Parent`) reaches is still carried by the
/// writer: classified, not called unreachable. (Independent review, P1.)
#[test]
fn back_edges_are_followed() {
    let pdf = doc(
        TEXT,
        "",
        "/Annots [30 0 R]",
        "",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Square /Rect [150 150 160 160] /Parent 31 0 R >>"
                    .to_vec(),
            ),
            (31, stream_obj("", b"hidden")),
        ],
    );
    let r = run(&pdf, RedactOptions::new()).unwrap();
    assert_eq!(roles(&r.uninspected), vec![DataRole::UnknownStream]);
    assert!(run(&pdf, RedactOptions::new().refuse_uninspected(true)).is_err());
}

/// A long chain of objects: the paths recorded are shared, so their
/// size does not grow with the square of the chain. (Independent
/// review, P1.)
#[test]
fn long_chains_keep_paths_small() {
    let n = 8000u32;
    let mut extra = Vec::new();
    for i in 0..n {
        let next = if i + 1 < n {
            format!("/Next {} 0 R", 100 + i + 1)
        } else {
            String::new()
        };
        extra.push((100 + i, stream_obj(&next, b"")));
    }
    let pdf = doc(
        TEXT,
        "",
        "",
        "/PieceInfo << /A << /Private 100 0 R >> >>",
        extra,
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(inv.uninspected, n as u64);
    let total: usize = inv.items.iter().map(|i| i.via.len()).sum();
    assert!(total < 5_000_000, "{total} bytes of paths");
}

/// An appearance stream's own resources are resources: the image it
/// draws is interpreted. (Independent review, P2.)
#[test]
fn appearance_resources() {
    let ap = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /I 32 0 R >> >>",
        b"q 10 0 0 10 0 0 cm /I Do Q",
    );
    let img = stream_obj(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
         /BitsPerComponent 8",
        &[7],
    );
    let pdf = doc(
        TEXT,
        "",
        "/Annots [30 0 R]",
        "",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Square /Rect [150 150 160 160] /AP << /N 31 0 R >> >>"
                    .to_vec(),
            ),
            (31, ap),
            (32, img),
        ],
    );
    let r = run(&pdf, RedactOptions::new().refuse_uninspected(true)).unwrap();
    assert_eq!(r.uninspected.uninspected, 0);
}

/// An image nothing draws is not shown: uninspected. A stream that is
/// both an attachment and a page's content is handed out as it is:
/// uninspected whatever else it is. (Independent review.)
#[test]
fn undrawn_images_and_attachments_used_as_content() {
    let img = stream_obj(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
         /BitsPerComponent 8",
        b"payload-not-a-pixel",
    );
    let pdf = doc(
        TEXT,
        "/XObject << /Unused 30 0 R >>",
        "",
        "",
        vec![(30, img)],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(roles(&inv), vec![DataRole::Undrawn]);

    let pdf = doc(
        TEXT,
        "",
        "",
        "/Names << /EmbeddedFiles << /Names [(a.txt) 30 0 R] >> >>",
        vec![(
            30,
            b"<< /Type /Filespec /F (a.txt) /EF << /F 4 0 R >> >>".to_vec(),
        )],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(roles(&inv), vec![DataRole::EmbeddedFile]);
}

/// Associated files hang off any object — here a structure element —
/// and stay uninspected whatever else their stream is (a page's
/// content). (Release review.)
#[test]
fn associated_files_anywhere() {
    let pdf = doc(
        TEXT,
        "",
        "",
        "/StructTreeRoot << /K 30 0 R >>",
        vec![(
            30,
            b"<< /S /Figure /AF [<< /Type /Filespec /F (a) /EF << /F 4 0 R >> >>] >>".to_vec(),
        )],
    );
    let d = Document::from_bytes(&pdf).unwrap();
    let inv = uninspected_data(&d, &Limits::default()).unwrap();
    assert_eq!(roles(&inv), vec![DataRole::EmbeddedFile]);
}
