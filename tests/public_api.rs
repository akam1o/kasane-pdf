//! The public surface of the crate as a user sees it: every item the
//! README and the feature docs rely on is exercised through the public
//! paths only, so a rename or a signature change is caught here before
//! it reaches a release. Also the warning channel
//! (`Document::take_warnings`) and the accepted-with-warnings inputs it
//! exists for.

mod common;

use common::{build_pdf, fixture, stream_obj};
use kasane_pdf::{
    extract_text, pixmap_to_png, render_page, rewrite, rewrite_with_limits, signatures, Dict,
    Document, Error, Limits, Object, Rect, RenderOptions, Warning,
};

fn hello() -> Vec<u8> {
    std::fs::read(fixture("hello.pdf")).unwrap()
}

#[test]
fn load_inspect_render_extract_rewrite_through_public_paths() {
    let data = hello();
    let doc = Document::from_bytes(&data).unwrap();
    let doc2 = Document::from_bytes_with_limits(&data, Limits::default()).unwrap();
    assert_eq!(doc.page_count(), doc2.page_count());
    assert!(doc.recovery().is_none(), "strictly parsed");

    let page = &doc.pages()[0];
    assert_eq!(page.index(), 0);
    let mb: Rect = page.media_box();
    assert!(mb.urx > mb.llx && mb.ury > mb.lly);
    let _rot: i64 = page.rotate();
    let pref = page.object_ref().expect("page is an indirect object");
    let pdict = doc.resolve(pref).unwrap();
    assert!(pdict.as_dict().is_some());
    let _catalog: &Dict = doc.catalog();
    let _trailer: &Dict = doc.trailer();
    let _ops = doc.page_operations(page).unwrap();
    let _ver = doc.catalog_version().unwrap();

    let opts = RenderOptions {
        dpi: 36.0,
        ..RenderOptions::default()
    };
    let r = render_page(&doc, page, &opts).unwrap();
    assert_eq!((r.width, r.height), (r.pixmap.width(), r.pixmap.height()));
    assert!(r.font_substitutions.is_empty() || !r.font_substitutions[0].substitute.is_empty());
    let png = pixmap_to_png(&r.pixmap).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    let t = extract_text(&doc, page, &Limits::default()).unwrap();
    assert!(!t.chars.is_empty());
    let c = &t.chars[0];
    let _ = (
        c.code,
        c.origin,
        c.advance,
        c.font_size,
        c.unicode.as_deref(),
    );

    let out = rewrite(&doc).unwrap();
    let out2 = rewrite_with_limits(&doc, &Limits::default()).unwrap();
    assert_eq!(out, out2);
    assert_eq!(Document::from_bytes(&out).unwrap().page_count(), 1);

    assert!(signatures(&doc).unwrap().is_empty());
    assert!(
        doc.take_warnings().is_empty(),
        "a conforming file records no warning"
    );
    assert_eq!(doc.warnings_dropped(), 0);
}

#[test]
fn errors_are_classified_by_kind() {
    let e = Document::from_bytes(b"not a pdf at all").unwrap_err();
    assert!(matches!(e, Error::NotPdf));
    assert_eq!(e.kind(), "not a PDF");
    let e = Document::from_bytes(&std::fs::read(fixture("encrypted.pdf")).unwrap()).unwrap_err();
    assert!(matches!(e, Error::Encrypted));
    let e = Document::from_bytes(&std::fs::read(fixture("truncated.pdf")).unwrap()).unwrap_err();
    assert!(matches!(
        e,
        Error::Parse(_) | Error::Structure(_) | Error::UnexpectedEof { .. }
    ));
    // A limit is its own kind, distinguishable from malformed input.
    let tiny = Limits {
        max_input_bytes: 10,
        ..Limits::default()
    };
    let e = Document::from_bytes_with_limits(&hello(), tiny).unwrap_err();
    assert!(matches!(e, Error::LimitExceeded { .. }));
    assert_eq!(e.kind(), "limit exceeded");
    // Error is a std error, Send + Sync, with a Display.
    fn assert_err<E: std::error::Error + Send + Sync + 'static>() {}
    assert_err::<Error>();
    assert!(!e.to_string().is_empty());
}

#[test]
fn recovery_mode_is_explicit_and_reported() {
    // Junk after the header makes every xref offset stale.
    let good = hello();
    let mut data = b"%PDF-1.4\n%junk junk junk junk junk\n".to_vec();
    data.extend_from_slice(&good[9..]);
    assert!(Document::from_bytes(&data).is_err(), "strict refuses");
    let doc = Document::from_bytes_recovering(&data).unwrap();
    let report = doc.recovery().expect("rebuilt");
    assert!(!report.strict_error.is_empty());
    assert!(report.objects_found > 0);
    assert_eq!(doc.page_count(), 1);
    // A clean file through the recovering entry point is still a
    // strict parse: no report.
    let doc = Document::from_bytes_recovering(&hello()).unwrap();
    assert!(doc.recovery().is_none());
}

#[test]
fn edit_session_rewrite_and_incremental() {
    let data = hello();
    let doc = Document::from_bytes(&data).unwrap();
    let mut e = doc.edit().unwrap();
    let mut info = Dict::new();
    info.insert(b"Title".to_vec(), Object::String(b"t".to_vec()));
    e.set_info(info).unwrap();
    e.add_square(
        0,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 50.0,
            ury: 50.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let inc = e.save_incremental().unwrap();
    assert!(inc.starts_with(&data), "original bytes kept verbatim");
    let mut e = doc.edit_with_limits(Limits::default()).unwrap();
    e.add_square(
        0,
        Rect {
            llx: 10.0,
            lly: 10.0,
            urx: 50.0,
            ury: 50.0,
        },
        [1.0, 0.0, 0.0],
        1.0,
    )
    .unwrap();
    let full = e.rewrite().unwrap();
    assert!(!full.starts_with(&data), "a rewrite is a fresh file");
    for bytes in [&inc, &full] {
        let d = Document::from_bytes(bytes).unwrap();
        assert_eq!(d.page_count(), 1);
    }
}

/// zlib data that ends after a sync flush (`00 00 FF FF`) with no final
/// block and no Adler-32: what some producers write.
fn sync_flushed_zlib(data: &[u8]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut c = Compress::new(Compression::default(), true);
    let mut out = Vec::with_capacity(data.len() + 64);
    c.compress_vec(data, &mut out, FlushCompress::Sync).unwrap();
    assert!(out.ends_with(&[0, 0, 0xFF, 0xFF]), "{out:02x?}");
    out
}

fn page_with_contents(contents: Vec<u8>, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    page_with_resources(contents, "<< >>", extra)
}

fn page_with_resources(contents: Vec<u8>, resources: &str, extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources {resources} /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, contents),
    ];
    objs.extend(extra.iter().cloned());
    build_pdf(&objs)
}

#[test]
fn unterminated_flate_stream_is_accepted_with_a_warning() {
    let content = b"0 0 1 rg 10 10 80 80 re f";
    let z = sync_flushed_zlib(content);
    let pdf = page_with_contents(stream_obj("/Filter /FlateDecode", &z), &[]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let p = r.pixmap.pixel(50, 50).unwrap();
    assert_eq!(
        (p.red(), p.green(), p.blue()),
        (0, 0, 255),
        "content was decoded in full"
    );
    let w = doc.take_warnings();
    assert_eq!(
        w,
        vec![Warning::UnterminatedFlateStream {
            decoded_bytes: content.len()
        }]
    );
    assert!(w[0].to_string().contains("sync flush"));
    assert!(doc.take_warnings().is_empty(), "take drains");
    // The same data cut inside a block is still refused, not accepted.
    let mut cut = z.clone();
    cut.truncate(cut.len() - 6);
    let pdf = page_with_contents(stream_obj("/Filter /FlateDecode", &cut), &[]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap_err();
    assert!(
        matches!(&e, Error::Parse(m) if m.contains("incomplete deflate stream")),
        "{e:?}"
    );
    assert!(doc.take_warnings().is_empty());
}

#[test]
fn malformed_tounicode_entries_are_skipped_with_a_warning() {
    // Three bfchar entries: one valid, one whose destination is not
    // hex (skipped), one with white space inside the hex string
    // (allowed by §7.3.4.3, not a warning).
    let cmap = b"/CIDInit /ProcSet findresource begin begincmap\n\
        1 begincodespacerange <00> <FF> endcodespacerange\n\
        3 beginbfchar <41> <0041> <42> <00ZZ> <43> <00 43> endbfchar\n\
        endcmap";
    let ttf = std::fs::read(fixture("KasaneTest.ttf")).unwrap();
    let pdf = page_with_resources(
        stream_obj("", b"BT /F1 20 Tf 10 50 Td (ABC) Tj ET"),
        "<< /Font << /F1 5 0 R >> >>",
        &[
            (
                5,
                b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular \
                   /Encoding /WinAnsiEncoding /FirstChar 65 /LastChar 67 /Widths [600 600 600] \
                   /FontDescriptor 6 0 R /ToUnicode 8 0 R >>"
                    .to_vec(),
            ),
            (
                6,
                b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular /Flags 32 \
                   /FontBBox [0 -200 600 800] /ItalicAngle 0 /Ascent 800 /Descent -200 \
                   /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                    .to_vec(),
            ),
            (7, stream_obj(&format!("/Length1 {}", ttf.len()), &ttf)),
            (8, stream_obj("", cmap)),
        ],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = extract_text(&doc, &doc.pages()[0], &Limits::default()).unwrap();
    let uni: Vec<Option<&str>> = t.chars.iter().map(|c| c.unicode.as_deref()).collect();
    assert_eq!(
        uni,
        vec![Some("A"), None, Some("C")],
        "B stays unmapped, never guessed"
    );
    let w = doc.take_warnings();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        matches!(&w[0], Warning::MalformedToUnicodeEntry { detail } if detail.contains("42")),
        "{w:?}"
    );
}

#[test]
fn warning_list_is_bounded() {
    // 1 100 unterminated content streams in one page: the list stops at
    // MAX_WARNINGS and the overflow is counted.
    let z = sync_flushed_zlib(b"0 0 m 1 1 l S");
    let n = kasane_pdf::warning::MAX_WARNINGS + 76;
    let mut objs = vec![
        (1u32, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
    ];
    let refs: Vec<String> = (0..n).map(|i| format!("{} 0 R", 10 + i)).collect();
    objs.push((
        3,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents [{}] >>",
            refs.join(" ")
        )
        .into_bytes(),
    ));
    for i in 0..n {
        objs.push((10 + i as u32, stream_obj("/Filter /FlateDecode", &z)));
    }
    let pdf = build_pdf(&objs);
    let doc = Document::from_bytes(&pdf).unwrap();
    render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    assert_eq!(doc.warnings().len(), kasane_pdf::warning::MAX_WARNINGS);
    assert_eq!(doc.warnings_dropped(), 76);
}
