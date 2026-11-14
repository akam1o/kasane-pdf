//! One regression test per published security advisory, on minimal
//! synthetic inputs (no real document's content), each with variants of
//! the construction it was about. Each asserts the patched behaviour:
//! the content is removed, or the input is refused.
//!
//! With `KASANE_ADVISORY_DUMP=<dir>` the tests also write every input
//! and its manifest line (`manifest-*.tsv`: file, advisory, CLI arguments,
//! expectation) that `scripts/check-advisory-regressions.py` runs
//! against any `kasane-pdf` CLI — affected versions show the leak, the
//! patched ones remove or refuse (checked with qpdf and pdftotext, which
//! share no code with kasane). Index: `docs/security/advisory-regressions.md`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    diagnose_redaction, extract_text, redact, render_page, Document, Limits, RedactOptions,
    RenderOptions, TextOptions,
};

const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);

/// Write `pdf` for the CLI check when asked: `expect` is `safe-text:T`
/// (refused, or no `T` extracted), `safe-bytes:T` (refused, or no `T` in
/// the fully decoded file), `refused`, or `fast` (renders, or refuses,
/// within the script's time bound).
fn dump(name: &str, ghsa: &str, args: &str, expect: &str, pdf: &[u8]) {
    let Ok(dir) = std::env::var("KASANE_ADVISORY_DUMP") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/{name}.pdf"), pdf).unwrap();
    // One manifest file per input: the tests run concurrently.
    std::fs::write(
        format!("{dir}/manifest-{name}.tsv"),
        format!("{name}.pdf\t{ghsa}\t{args}\t{expect}\n"),
    )
    .unwrap();
}

const REDACT_ARGS: &str = "redact --page 1 --rect 60 60 140 140";

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

/// One page: `content`, resources `/Font << /F1 20 0 R >> res`, `cat`
/// in the catalog, `page_extra` in the page, `extra` objects.
fn page(
    content: &[u8],
    res: &str,
    cat: &str,
    page_extra: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                 /Resources << /Font << /F1 20 0 R >> {res} >> /Contents 4 0 R {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

const KEPT: &str = "BT /F1 10 Tf 10 20 Td (PUBLICWORD) Tj ET";

fn run(pdf: &[u8], opts: &RedactOptions) -> Result<kasane_pdf::Redaction, kasane_pdf::Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(&doc, &[(0, vec![RECT])], opts)
}

fn text(pdf: &[u8]) -> String {
    let doc = Document::from_bytes(pdf).unwrap();
    doc.pages()
        .iter()
        .map(|p| {
            extract_text(&doc, p, &TextOptions::default())
                .unwrap()
                .chars
                .iter()
                .filter_map(|c| c.unicode.clone())
                .collect::<String>()
        })
        .collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Removed (no `marker` extracted, the public text kept) or refused.
fn removed_or_refused(name: &str, pdf: &[u8], marker: &str, opts: &RedactOptions) {
    if let Ok(r) = run(pdf, opts) {
        let t = text(&r.output);
        assert!(!t.contains(marker), "{name}: {marker} still extracted");
        assert!(t.contains("PUBLICWORD"), "{name}: the public text was lost");
    }
}

// ---- GHSA-2337-4h5g-jqwx (≤ 0.5.1): nested transforms -----------------

/// Content under nested `cm` and a form's `/Matrix` is located where it
/// is drawn: the diagnosis finds it in the rect, `redact` removes it.
#[test]
fn ghsa_2337_nested_transforms() {
    let form = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [0.5 0 0 0.5 10 10] \
         /Resources << /Font << /F1 20 0 R >> >>",
        b"BT /F1 20 Tf 0 0 Td (FORMSECRET) Tj ET",
    );
    for (name, content) in [
        // A rotation then a translation: composed in the wrong order,
        // the text lands at (170, 90), outside the rect.
        ("cm", format!("q 0 1 -1 0 150 50 cm q 1 0 0 1 20 40 cm BT /F1 8 Tf 0 0 Td (NESTSECRET) Tj ET Q Q {KEPT}")),
        ("form", format!("q 1 0 0 1 55 50 cm /Fm Do Q {KEPT}")),
    ] {
        let pdf = page(content.as_bytes(), "/XObject << /Fm 30 0 R >>", "", "", vec![(30, form.clone())]);
        let marker = if name == "cm" { "NESTSECRET" } else { "FORMSECRET" };
        let doc = Document::from_bytes(&pdf).unwrap();
        let d = diagnose_redaction(&doc, 0, &[RECT], &Limits::default()).unwrap();
        assert!(!d.targets.is_empty(), "{name}: the diagnosis finds nothing in the rect");
        let r = run(&pdf, &RedactOptions::new()).unwrap();
        assert!(r.codes_removed >= 8, "{name}: {} codes removed", r.codes_removed);
        assert!(!text(&r.output).contains(marker), "{name}");
        // Before 0.6 there is no `redact`: the editor the advisory names.
        dump(
            &format!("2337-{name}"),
            "GHSA-2337-4h5g-jqwx",
            &format!("{REDACT_ARGS}|experimental-remove-text 1 --rect 60 60 140 140"),
            &format!("safe-text:{marker}"),
            &pdf,
        );
    }
}

// ---- GHSA-fm2j-3w72-p7x3 (< 0.5.1): dashed strokes --------------------

/// A long back-and-forth dashed stroke with round caps renders (or is
/// refused) quickly.
#[test]
fn ghsa_fm2j_dashed_strokes_are_bounded() {
    // A zigzag over the page's height: every segment crosses every row.
    let mut c = String::from("100 w 1 J [1 1] 0 d 0 0 m");
    for i in 0..1200 {
        c.push_str(&format!(
            " {} {} l",
            i % 600,
            if i % 2 == 0 { 600 } else { 0 }
        ));
    }
    c.push_str(" S");
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 600] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("", c.as_bytes())),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    let t = std::time::Instant::now();
    let _ = render_page(&doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0));
    assert!(t.elapsed().as_secs() < 30, "{:?}", t.elapsed());
    dump(
        "fm2j-dash",
        "GHSA-fm2j-3w72-p7x3",
        "render 1 --dpi 72",
        "fast",
        &pdf,
    );
}

// ---- GHSA-x455-j82p-6jmv (< 0.6.1): clips hide ink, not data ----------

#[test]
fn ghsa_x455_clipped_content_is_removed() {
    let im = stream_obj(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[11, 22, 33, 44],
    );
    for (name, content) in [
        (
            "text",
            "q 0 0 10 10 re W n BT /F1 12 Tf 70 100 Td (CLIPSECRET) Tj ET Q",
        ),
        (
            "text-invisible-clip",
            "q 150 150 10 10 re W n BT /F1 12 Tf 3 Tr 70 100 Td (CLIPSECRET) Tj ET Q",
        ),
        (
            "image",
            "q 0 0 10 10 re W n q 40 0 0 40 80 80 cm /Im Do Q Q",
        ),
    ] {
        let pdf = page(
            format!("{content} {KEPT}").as_bytes(),
            "/XObject << /Im 30 0 R >>",
            "",
            "",
            vec![(30, im.clone())],
        );
        if name == "image" {
            if let Ok(r) = run(&pdf, &RedactOptions::new()) {
                assert!(
                    !contains(&r.output, &[11, 22, 33, 44]),
                    "{name}: samples kept"
                );
            }
            dump(
                "x455-image",
                "GHSA-x455-j82p-6jmv",
                REDACT_ARGS,
                "safe-bytes:hex:0b16212c",
                &pdf,
            );
        } else {
            removed_or_refused(name, &pdf, "CLIPSECRET", &RedactOptions::new());
            dump(
                &format!("x455-{name}"),
                "GHSA-x455-j82p-6jmv",
                REDACT_ARGS,
                "safe-text:CLIPSECRET",
                &pdf,
            );
        }
    }
}

// ---- GHSA-frr2-h56x-mr4h (< 0.6.1): strongly anisotropic text ---------

#[test]
fn ghsa_frr2_anisotropic_text_is_refused() {
    for (name, content) in [
        ("tm", "BT /F1 1 Tf 0.1 0 0 10 70 100 Tm (ANISOSECRET) Tj ET"),
        (
            "tm-cancelled-by-cm",
            "q 10 0 0 0.1 0 0 cm BT /F1 1 Tf 1 0 0 1000 7 1000 Tm (ANISOSECRET) Tj ET Q",
        ),
    ] {
        let pdf = page(format!("{content} {KEPT}").as_bytes(), "", "", "", vec![]);
        assert!(
            run(&pdf, &RedactOptions::new()).is_err(),
            "{name}: accepted"
        );
        dump(
            &format!("frr2-{name}"),
            "GHSA-frr2-h56x-mr4h",
            REDACT_ARGS,
            "refused",
            &pdf,
        );
    }
}

// ---- GHSA-9mw8-m2cr-vxqm (< 0.6.2): removed glyphs' positions ---------

/// Two inputs differing only in where removed glyphs were placed (`Td`,
/// `TJ` kerning, one show per glyph) redact to the same bytes.
#[test]
fn ghsa_9mw8_removed_positions_do_not_remain() {
    let variant = |v: i32| {
        page(
            format!(
                "BT /F1 10 Tf 70 100 Td [(S) {} (E) {} (C)] TJ ET \
                 BT /F1 10 Tf 70 120 Td (A) Tj {} 0 Td (B) Tj ET {KEPT}",
                -100 * v,
                -37 * v,
                3 * v
            )
            .as_bytes(),
            "",
            "",
            "",
            vec![],
        )
    };
    let (a, b) = (variant(1), variant(2));
    let (x, y) = (
        run(&a, &RedactOptions::new()).unwrap(),
        run(&b, &RedactOptions::new()).unwrap(),
    );
    assert!(x.output == y.output, "positions of removed glyphs remain");
}

// ---- GHSA-f228-qvw7-4x37 (< 0.7.0): unescaped carriage returns --------

#[test]
fn ghsa_f228_raw_carriage_returns_are_refused() {
    for (name, content) in [
        ("shown", &b"BT /F1 10 Tf 30 100 Td (AB\rCRSECRET) Tj ET"[..]),
        ("crlf", b"BT /F1 10 Tf 30 100 Td (AB\r\nCRSECRET) Tj ET"),
    ] {
        let mut c = content.to_vec();
        c.extend_from_slice(format!(" {KEPT}").as_bytes());
        let pdf = page(&c, "", "", "", vec![]);
        assert!(
            run(&pdf, &RedactOptions::new()).is_err(),
            "{name}: accepted"
        );
        dump(
            &format!("f228-{name}"),
            "GHSA-f228-qvw7-4x37",
            REDACT_ARGS,
            "refused",
            &pdf,
        );
    }
}

// ---- Type 0 fonts with an embedded CMap (GHSA-xpph, GHSA-3f9j) ---------

/// A Type 0 LiberationSans page: CMap body `mappings` (codespace
/// `codespace`), content showing `<30534543524554>` (a padding code and
/// SECRET) at x 20, y 100, and `KEPT`-like public text in a simple font.
fn cid_page(mappings: &str, codespace: &str, ncs: u32) -> Vec<u8> {
    let ff = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
         /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def \
         /CMapName /T def /CMapType 1 def {ncs} begincodespacerange {codespace} endcodespacerange \
         1 begincidrange <41> <5A> 36 endcidrange {mappings} \
         endcmap CMapName currentdict /CMap defineresource pop end end"
    );
    let tou = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /U def \
               1 begincodespacerange <00> <FF> endcodespacerange \
               1 beginbfrange <41> <5A> <0041> endbfrange \
               endcmap CMapName currentdict /CMap defineresource pop end end";
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
              /Resources << /Font << /F 5 0 R /F1 20 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (
            4,
            stream_obj(
                "",
                format!("BT /F 10 Tf 20 100 Td <30534543524554> Tj ET {KEPT}").as_bytes(),
            ),
        ),
        (
            5,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding 9 0 R \
              /DescendantFonts [8 0 R] /ToUnicode 10 0 R >>"
                .to_vec(),
        ),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
              /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
              /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>"
                .to_vec(),
        ),
        (7, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
        (
            8,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 667 /W [3 [250] 4 [5000]] /CIDToGIDMap /Identity /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
        (9, stream_obj("/Type /CMap /CMapName /T", cmap.as_bytes())),
        (10, stream_obj("", tou.as_bytes())),
    ];
    objs.extend(font_objs());
    build_pdf(&objs)
}

/// The rect where readers draw the six marker glyphs after a 50 pt
/// padding advance (CID 4).
const CID_RECT: &str = "redact --page 1 --rect 70 95 140 112";

#[test]
fn ghsa_xpph_literal_string_cmap_codes_are_refused() {
    let pdf = cid_page("1 begincidchar (0) 4 endcidchar", "<00> <FF>", 1);
    let doc = Document::from_bytes(&pdf).unwrap();
    assert!(redact(
        &doc,
        &[(0, vec![(70.0, 95.0, 140.0, 112.0)])],
        &RedactOptions::new()
    )
    .is_err());
    dump(
        "xpph-literal",
        "GHSA-xpph-r5fc-83rj",
        CID_RECT,
        "refused",
        &pdf,
    );
}

/// Definition order (a `cidchar` then an overlapping `cidrange`: the
/// later wins), a value defined for another length, codes passed through
/// an Identity base, bf mappings: removed where readers draw them, or
/// refused.
#[test]
fn ghsa_3f9j_cmap_readings() {
    let r = (70.0, 95.0, 140.0, 112.0);
    let order = cid_page(
        "1 begincidchar <30> 3 endcidchar 1 begincidrange <30> <30> 4 endcidrange",
        "<00> <FF>",
        1,
    );
    let doc = Document::from_bytes(&order).unwrap();
    let out = redact(&doc, &[(0, vec![r])], &RedactOptions::new()).unwrap();
    assert_eq!(out.codes_removed, 6);
    assert!(!text(&out.output).contains("SECRET"));
    dump(
        "3f9j-order",
        "GHSA-3f9j-35f2-6659",
        CID_RECT,
        "safe-text:SECRET",
        &order,
    );
    // A reader draws the six glyphs right after a narrow padding glyph
    // (x 22–68) where earlier versions placed them after the wide one: a
    // rect there kept them. Refused now, whichever rect.
    let reader_rect = (22.0, 95.0, 68.0, 112.0);
    for (name, pdf) in [
        (
            "length",
            cid_page(
                "1 begincidchar <0030> 4 endcidchar",
                "<00> <7F> <8000> <FFFF>",
                2,
            ),
        ),
        (
            "bfchar",
            cid_page(
                "1 beginbfchar <30> <0041> endbfchar 1 begincidchar <30> 4 endcidchar",
                "<00> <FF>",
                1,
            ),
        ),
        (
            "after-endcmap",
            cid_page("endcmap 1 begincidchar <30> 4 endcidchar", "<00> <FF>", 1),
        ),
    ] {
        let doc = Document::from_bytes(&pdf).unwrap();
        for rect in [r, reader_rect] {
            assert!(
                redact(&doc, &[(0, vec![rect])], &RedactOptions::new()).is_err(),
                "{name}: accepted"
            );
        }
        dump(
            &format!("3f9j-{name}"),
            "GHSA-3f9j-35f2-6659",
            "redact --page 1 --rect 22 95 68 112",
            "safe-text:SECRET",
            &pdf,
        );
    }
}

// ---- GHSA-vph3-9p4m-58fh (< 0.7.0): input object streams ---------------

/// A removed link stored in an input object stream that something else
/// refers to: the stream is not carried with it.
#[test]
fn ghsa_vph3_object_streams_are_not_carried() {
    let members = [
        (40u32, "<< /Type /Annot /Subtype /Link /Rect [70 90 130 110] /A << /S /URI /URI (https://vph3.example/SECRETLINK) >> >>"),
        (41, "<< /Note (other) >>"),
    ];
    let (mut head, mut body) = (String::new(), String::new());
    for (n, o) in members {
        head.push_str(&format!("{n} {} ", body.len()));
        body.push_str(o);
        body.push(' ');
    }
    let data = format!("{head}{body}");
    let objstm = stream_obj(
        &format!("/Type /ObjStm /N 2 /First {}", head.len()),
        data.as_bytes(),
    );
    // Classic objects plus the object stream, then a cross-reference
    // stream listing the members.
    let mut plain = vec![
        (1u32, b"<< /Type /Catalog /Pages 2 0 R /XKeep 41 0 R /XStm 50 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 20 0 R >> >> \
              /Contents 4 0 R /Annots [40 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", KEPT.as_bytes())),
        (50, objstm),
    ];
    plain.extend(font_objs());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut rows: std::collections::BTreeMap<u32, (u8, u32, u16)> = Default::default();
    for (n, o) in &plain {
        rows.insert(*n, (1, out.len() as u32, 0));
        out.extend(format!("{n} 0 obj\n").as_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    rows.insert(40, (2, 50, 0));
    rows.insert(41, (2, 50, 1));
    let x = out.len();
    rows.insert(60, (1, x as u32, 0));
    let mut table = Vec::new();
    for n in 0..61u32 {
        let (t, f2, f3) = rows.get(&n).copied().unwrap_or((0, 0, 0xffff));
        table.push(t);
        table.extend(f2.to_be_bytes());
        table.extend(f3.to_be_bytes());
    }
    out.extend(b"60 0 obj\n");
    out.extend(stream_obj(
        "/Type /XRef /Size 61 /W [1 4 2] /Root 1 0 R",
        &table,
    ));
    out.extend(format!("\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    if let Ok(r) = run(&out, &RedactOptions::new()) {
        assert!(
            !contains(&r.output, b"SECRETLINK"),
            "the removed link is still in the output"
        );
        assert!(!contains(&r.output, b"/ObjStm"));
    }
    dump(
        "vph3-objstm",
        "GHSA-vph3-9p4m-58fh",
        REDACT_ARGS,
        "safe-bytes:SECRETLINK",
        &out,
    );
}

// ---- GHSA-cjhc-76x8-crrq (< 0.8.0): removed data kept by reference -----

/// What the redaction replaced or removed, kept through a reference from
/// a place that draws nothing: refused (or gone).
#[test]
fn ghsa_cjhc_removed_data_is_not_kept_by_reference() {
    let secret_content = format!("BT /F1 12 Tf 70 100 Td (ORIGSECRET) Tj ET {KEPT}");
    let ap = stream_obj(
        "/Type /XObject /Subtype /Form /BBox [0 0 40 10] /Resources << /Font << /F1 20 0 R >> >>",
        b"BT /F1 8 Tf 0 0 Td (APSECRET) Tj ET",
    );
    let cases: Vec<(&str, Vec<u8>, &str, bool)> = vec![
        ("contents-catalog", page(secret_content.as_bytes(), "", "/XPriv 4 0 R", "", vec![]), "ORIGSECRET", false),
        ("contents-pieceinfo", page(secret_content.as_bytes(), "", "/PieceInfo << /App << /Private 4 0 R >> >>", "", vec![]), "ORIGSECRET", false),
        (
            "contents-attachment",
            page(
                secret_content.as_bytes(),
                "",
                "/Names << /EmbeddedFiles << /Names [(x) << /Type /Filespec /F (x) /EF << /F 4 0 R >> >>] >> >>",
                "",
                vec![],
            ),
            "ORIGSECRET",
            false,
        ),
        (
            "link-appearance",
            page(
                KEPT.as_bytes(),
                "",
                "/XPriv 31 0 R",
                "/Annots [30 0 R]",
                vec![
                    (30, b"<< /Type /Annot /Subtype /Link /Rect [70 90 110 100] /AP << /N 31 0 R >> >>".to_vec()),
                    (31, ap.clone()),
                ],
            ),
            "APSECRET",
            false,
        ),
        (
            "stamp-appearance",
            page(
                KEPT.as_bytes(),
                "",
                "/XPriv 31 0 R",
                "/Annots [30 0 R]",
                vec![
                    (30, b"<< /Type /Annot /Subtype /Stamp /Rect [70 90 110 100] /AP << /N 31 0 R >> >>".to_vec()),
                    (31, ap),
                ],
            ),
            "APSECRET",
            true,
        ),
    ];
    for (name, pdf, marker, annotations) in cases {
        let opts = RedactOptions::new().remove_annotations(annotations);
        if let Ok(r) = run(&pdf, &opts) {
            assert!(
                !contains(&r.output, marker.as_bytes()),
                "{name}: {marker} kept"
            );
        }
        let args = if annotations {
            format!("{REDACT_ARGS} --remove-annotations")
        } else {
            REDACT_ARGS.to_string()
        };
        dump(
            &format!("cjhc-{name}"),
            "GHSA-cjhc-76x8-crrq",
            &args,
            &format!("safe-bytes:{marker}"),
            &pdf,
        );
    }
}

/// Whether any object of `pdf` holds `needle` — in a string (exactly as
/// stored, hexadecimal ones included) or a stream's bytes.
fn holds(pdf: &[u8], needle: &[u8]) -> bool {
    fn walk(o: &kasane_pdf::Object, needle: &[u8]) -> bool {
        use kasane_pdf::Object as O;
        match o {
            O::String(b) => contains(b, needle),
            O::Array(a) => a.iter().any(|x| walk(x, needle)),
            O::Dict(d) => d.values().any(|x| walk(x, needle)),
            O::Stream { dict, data } => {
                contains(data, needle) || dict.values().any(|x| walk(x, needle))
            }
            _ => false,
        }
    }
    let doc = Document::from_bytes(pdf).unwrap();
    let size = doc
        .trailer()
        .get(b"Size".as_slice())
        .and_then(|s| s.as_i64())
        .unwrap_or(0);
    (1..size as u32).any(|n| {
        doc.resolve(kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        })
        .is_ok_and(|o| walk(&o, needle))
    })
}

// ---- GHSA-2r8v-x8mp-wp79 (0.6.0 – 0.8.0): colour data of removed appearances

/// The colour data a removed link's appearance painted with (an
/// `/Indexed` table, a tint or transfer function) does not stay through
/// a reference from something that does not draw it.
#[test]
fn ghsa_2r8v_colour_data_of_removed_appearances() {
    const PAL: &[u8] = b"\x12\x34\x56\xab\xcd\xef";
    let marker = "123456abcdef";
    let link = |ap_body: &[u8], ap_res: &str| {
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [70 90 110 100] /AP << /N 31 0 R >> \
                  /A << /S /URI /URI (https://example.com/) >> >>"
                    .to_vec(),
            ),
            (
                31,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 40 10] /Resources << {ap_res} >>"
                    ),
                    ap_body,
                ),
            ),
            (40, stream_obj("", PAL)),
            (
                41,
                stream_obj(
                    "/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2] /BitsPerSample 8",
                    PAL,
                ),
            ),
            // The palette as a string object rather than a stream.
            (42, b"<123456abcdef>".to_vec()),
            // An ICC profile's bytes (not a valid profile: readers fall
            // back to the alternate) carrying the colour data.
            (43, stream_obj("/N 3 /Alternate /DeviceRGB", PAL)),
        ]
    };
    let indexed = "/ColorSpace << /C [/Indexed /DeviceRGB 1 40 0 R] >>";
    let cases = [
        (
            "palette-catalog",
            "/XPriv 40 0 R",
            link(b"/C cs 1 sc 0 0 10 10 re f", indexed),
        ),
        (
            "palette-pieceinfo",
            "/PieceInfo << /App << /Private 40 0 R >> >>",
            link(b"/C cs 1 sc 0 0 10 10 re f", indexed),
        ),
        (
            "tint-catalog",
            "/XPriv 41 0 R",
            link(
                b"/S cs 1 scn 0 0 10 10 re f",
                "/ColorSpace << /S [/Separation /Ink /DeviceRGB 41 0 R] >>",
            ),
        ),
        (
            "transfer-catalog",
            "/XPriv 41 0 R",
            link(
                b"/G gs 0 0 10 10 re f",
                "/ExtGState << /G << /TR 41 0 R >> >>",
            ),
        ),
        (
            "tint-pieceinfo",
            "/PieceInfo << /App << /Private 41 0 R >> >>",
            link(
                b"/S cs 1 scn 0 0 10 10 re f",
                "/ColorSpace << /S [/Separation /Ink /DeviceRGB 41 0 R] >>",
            ),
        ),
        (
            "devicen-catalog",
            "/XPriv 41 0 R",
            link(
                b"/D cs 1 scn 0 0 10 10 re f",
                "/ColorSpace << /D [/DeviceN [/Ink] /DeviceRGB 41 0 R] >>",
            ),
        ),
        (
            "palette-string-catalog",
            "/XPriv 42 0 R",
            link(
                b"/C cs 1 sc 0 0 10 10 re f",
                "/ColorSpace << /C [/Indexed /DeviceRGB 1 42 0 R] >>",
            ),
        ),
        (
            "default-rgb-catalog",
            "/XPriv 43 0 R",
            link(
                b"1 0 0 rg 0 0 10 10 re f",
                "/ColorSpace << /DefaultRGB [/ICCBased 43 0 R] >>",
            ),
        ),
    ];
    for (name, cat, extra) in cases {
        let pdf = page(KEPT.as_bytes(), "", cat, "/Annots [30 0 R]", extra);
        if let Ok(r) = run(&pdf, &RedactOptions::new()) {
            assert!(
                !holds(&r.output, PAL) && !contains(&r.output, b"123456abcdef"),
                "{name}: the colour data stayed"
            );
        }
        dump(
            &format!("2r8v-{name}"),
            "GHSA-2r8v-x8mp-wp79",
            REDACT_ARGS,
            &format!("safe-bytes:hex:{marker}"),
            &pdf,
        );
    }
}

// ---- GHSA-92wc-597g-rqx9 (0.7.0 – 0.8.0): removed annotations kept with remove_javascript

/// With `remove_javascript`, a removed annotation that has a script and
/// that something else refers to is not written out.
#[test]
fn ghsa_92wc_removed_annotations_with_scripts() {
    let rect = "/Rect [70 90 130 110]";
    let js = "/A << /S /JavaScript /JS (app.alert\\(1\\)) >>";
    let markup = format!(
        "<< /Type /Annot /Subtype /FreeText {rect} /DA (/Helv 9 Tf 0 g) /Contents (CONTENTSSECRET) \
         /T (AUTHORSECRET) /Subj (SUBJSECRET) /NM (NMSECRET) /RC (<p>RCSECRET</p>) {js} >>"
    );
    let link = format!(
        "<< /Type /Annot /Subtype /Link {rect} /A << /S /URI /URI (https://secret.example/LINKSECRET) \
         /Next << /S /JavaScript /JS (x) >> >> >>"
    );
    let hide = b"<< /Type /Annot /Subtype /Link /Rect [150 150 190 190] \
                 /A << /S /Hide /T 30 0 R >> >>"
        .to_vec();
    // (name, annotation, catalog entries, extra objects, marker, remove_annotations)
    type Case<'a> = (&'a str, String, &'a str, Vec<(u32, Vec<u8>)>, &'a str, bool);
    let cases: Vec<Case> = vec![
        (
            "markup-catalog",
            markup.clone(),
            "/XPriv 30 0 R",
            vec![],
            "CONTENTSSECRET",
            true,
        ),
        (
            "markup-pieceinfo",
            markup.clone(),
            "/PieceInfo << /App << /Private 30 0 R >> >>",
            vec![],
            "CONTENTSSECRET",
            true,
        ),
        (
            "markup-hide",
            markup,
            "",
            vec![(31, hide)],
            "CONTENTSSECRET",
            true,
        ),
        (
            "link-catalog",
            link,
            "/XPriv 30 0 R",
            vec![],
            "LINKSECRET",
            false,
        ),
    ];
    // The other text fields of the same annotation, each its own case.
    let mut cases = cases;
    for field in ["AUTHORSECRET", "SUBJSECRET", "NMSECRET", "RCSECRET"] {
        let base = cases[0].clone();
        cases.push((
            match field {
                "AUTHORSECRET" => "markup-t-catalog",
                "SUBJSECRET" => "markup-subj-catalog",
                "NMSECRET" => "markup-nm-catalog",
                _ => "markup-rc-catalog",
            },
            base.1,
            base.2,
            base.3,
            field,
            base.5,
        ));
    }
    for (name, annot, cat, extra, marker, annotations) in cases {
        let annots = if extra.is_empty() {
            "/Annots [30 0 R]"
        } else {
            "/Annots [30 0 R 31 0 R]"
        };
        let mut objs = vec![(30, annot.into_bytes())];
        objs.extend(extra);
        let pdf = page(KEPT.as_bytes(), "", cat, annots, objs);
        let opts = RedactOptions::new()
            .remove_javascript(true)
            .remove_annotations(annotations);
        if let Ok(r) = run(&pdf, &opts) {
            assert!(
                !contains(&r.output, marker.as_bytes()),
                "{name}: {marker} kept"
            );
        }
        let args = format!(
            "{REDACT_ARGS} --remove-javascript{}",
            if annotations {
                " --remove-annotations"
            } else {
                ""
            }
        );
        dump(
            &format!("92wc-{name}"),
            "GHSA-92wc-597g-rqx9",
            &args,
            &format!("safe-bytes:{marker}"),
            &pdf,
        );
    }
}

// ---- GHSA-6pgh-gg28-m4w4 (0.6.0 – 0.8.0): removed links' actions kept by other references

/// A removed link's separate action object does not stay through an
/// entry that does not act on it; one a kept link uses does.
#[test]
fn ghsa_6pgh_removed_link_actions() {
    let actions = [
        (
            "uri",
            "<< /S /URI /URI (https://secret.example/ACTSECRET) >>",
        ),
        ("launch", "<< /S /Launch /F (ACTSECRET.exe) >>"),
        ("goto", "<< /S /GoTo /D (ACTSECRET) >>"),
        ("javascript", "<< /S /JavaScript /JS (ACTSECRET) >>"),
        ("named", "<< /S /Named /N /NextPage /XNote (ACTSECRET) >>"),
    ];
    for (a, action) in actions {
        for (via, entry) in [("A", "/A 40 0 R"), ("AA", "/AA << /E 40 0 R >>")] {
            for (w, cat, extra) in [
                ("catalog", "/XPriv 40 0 R", vec![]),
                ("pieceinfo", "/PieceInfo << /App << /Private 40 0 R >> >>", vec![]),
                (
                    "next",
                    "/XPriv 41 0 R",
                    vec![(41, b"<< /S /Named /N /NextPage /XNext 40 0 R >>".to_vec())],
                ),
            ] {
                let mut objs = vec![
                    (
                        30,
                        format!("<< /Type /Annot /Subtype /Link /Rect [70 90 130 110] {entry} >>")
                            .into_bytes(),
                    ),
                    (40, action.as_bytes().to_vec()),
                ];
                objs.extend(extra);
                let pdf = page(KEPT.as_bytes(), "", cat, "/Annots [30 0 R]", objs);
                let name = format!("{a}-{via}-{w}");
                if let Ok(r) = run(&pdf, &RedactOptions::new()) {
                    assert!(
                        !contains(&r.output, b"ACTSECRET"),
                        "{name}: the action stayed"
                    );
                }
                dump(
                    &format!("6pgh-{name}"),
                    "GHSA-6pgh-gg28-m4w4",
                    REDACT_ARGS,
                    "safe-bytes:ACTSECRET",
                    &pdf,
                );
            }
        }
    }
    // An action a kept link also uses is that link's: kept.
    let pdf = page(
        KEPT.as_bytes(),
        "",
        "",
        "/Annots [30 0 R 31 0 R]",
        vec![
            (
                30,
                b"<< /Type /Annot /Subtype /Link /Rect [70 90 130 110] /A 40 0 R >>".to_vec(),
            ),
            (
                31,
                b"<< /Type /Annot /Subtype /Link /Rect [150 150 190 190] /A 40 0 R >>".to_vec(),
            ),
            (
                40,
                b"<< /S /URI /URI (https://example.com/shared) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf, &RedactOptions::new()).unwrap();
    assert!(holds(&r.output, b"example.com/shared"));
    dump(
        "6pgh-shared-kept",
        "GHSA-6pgh-gg28-m4w4",
        REDACT_ARGS,
        "kept-bytes:example.com/shared",
        &pdf,
    );
}

// ---- GHSA-fqgr-r67f-vx25 (0.6.0 – 0.8.0): glyphs viewers resolve or place differently

/// A synthetic eexec Type 1 program: `.notdef` (empty), `underscore`
/// (a bar below the baseline) and `W` (a box), with StandardEncoding as
/// its own encoding.
fn type1_program() -> Vec<u8> {
    let w: Option<&[u8]> = None;
    fn num(v: i32, out: &mut Vec<u8>) {
        match v {
            -107..=107 => out.push((v + 139) as u8),
            108..=1131 => out.extend([(((v - 108) >> 8) + 247) as u8, ((v - 108) & 255) as u8]),
            -1131..=-108 => out.extend([(((-v - 108) >> 8) + 251) as u8, ((-v - 108) & 255) as u8]),
            _ => {
                out.push(255);
                out.extend(v.to_be_bytes());
            }
        }
    }
    fn encrypt(data: &[u8], mut r: u16) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in [0u8; 4].iter().chain(data) {
            let c = b ^ (r >> 8) as u8;
            r = (c as u16)
                .wrapping_add(r)
                .wrapping_mul(52845)
                .wrapping_add(22719);
            out.push(c);
        }
        out
    }
    // hsbw, then a closed box (x, y, w, h) or none, then endchar.
    let glyph = |wx: i32, b: Option<(i32, i32, i32, i32)>| {
        let mut c = Vec::new();
        num(0, &mut c);
        num(wx, &mut c);
        c.push(13);
        if let Some((x, y, w, h)) = b {
            for (v, op) in [
                (vec![x, y], 21),
                (vec![w, 0], 5),
                (vec![0, h], 5),
                (vec![-w, 0], 5),
            ] {
                for n in v {
                    num(n, &mut c);
                }
                c.push(op);
            }
            c.push(9);
        }
        c.push(14);
        c
    };
    let mut private = b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n\
        /ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/lenIV 4 def\n\
        /MinFeature{16 16} def\n/password 5839 def\n/Subrs 0 array\nND\n2 index /CharStrings 3 dict dup begin\n"
        .to_vec();
    for (name, c) in [
        (".notdef", glyph(500, None)),
        ("underscore", glyph(600, Some((0, -200, 600, 100)))),
        (
            "W",
            w.map_or_else(|| glyph(1000, Some((0, 0, 1000, 700))), <[u8]>::to_vec),
        ),
    ] {
        let e = encrypt(&c, 4330);
        private.extend(format!("/{name} {} RD ", e.len()).into_bytes());
        private.extend(e);
        private.extend(b" ND\n");
    }
    private.extend(
        b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\nmark currentfile closefile\n",
    );
    let mut p =
        b"%!FontType1-1.0: KTest 001\n12 dict begin\n/FontName /KTest def\n/FontType 1 def\n\
        /FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/FontBBox {0 -300 1000 1000} readonly def\n\
        /PaintType 0 def\n/Encoding StandardEncoding def\ncurrentdict end\ncurrentfile eexec\n"
            .to_vec();
    p.extend(encrypt(&private, 55665));
    for _ in 0..8 {
        p.extend([b'0'; 64]);
        p.push(b'\n');
    }
    p.extend(b"cleartomark\n");
    p
}

/// A synthetic bare CFF program (`/FontFile3 /Type1C`): `.notdef`
/// (empty), `underscore` (a bar below the baseline) and `W` (a 1000 ×
/// 700 box), its own encoding giving them codes 0x5F and 0x57.
fn cff_program() -> Vec<u8> {
    fn num(v: i32) -> Vec<u8> {
        match v {
            -107..=107 => vec![(v + 139) as u8],
            108..=1131 => vec![(((v - 108) >> 8) + 247) as u8, ((v - 108) & 255) as u8],
            -1131..=-108 => vec![(((-v - 108) >> 8) + 251) as u8, ((-v - 108) & 255) as u8],
            _ => [vec![255u8], v.to_be_bytes().to_vec()].concat(),
        }
    }
    fn index(items: &[Vec<u8>]) -> Vec<u8> {
        if items.is_empty() {
            return vec![0, 0];
        }
        let mut offsets = vec![1u16];
        for i in items {
            offsets.push(offsets.last().unwrap() + i.len() as u16);
        }
        let mut out = (items.len() as u16).to_be_bytes().to_vec();
        out.push(2);
        for o in offsets {
            out.extend(o.to_be_bytes());
        }
        for i in items {
            out.extend(i);
        }
        out
    }
    let glyph = |b: Option<(i32, i32, i32, i32)>| {
        let mut c = Vec::new();
        if let Some((x, y, w, h)) = b {
            for (v, op) in [
                (vec![x, y], 21u8),
                (vec![w, 0], 5),
                (vec![0, h], 5),
                (vec![-w, 0], 5),
                (vec![0, -h], 5),
            ] {
                for n in v {
                    c.extend(num(n));
                }
                c.push(op);
            }
        }
        c.push(14);
        c
    };
    let glyphs = vec![
        glyph(None),
        glyph(Some((0, -400, 600, 100))),
        glyph(Some((0, 0, 1000, 700))),
    ];
    let dn = |n: i32| [vec![0x1d], n.to_be_bytes().to_vec()].concat();
    let top = |charset: i32, encoding: i32, chars: i32, private: i32| {
        [
            dn(charset),
            vec![0x0f],
            dn(encoding),
            vec![0x10],
            dn(chars),
            vec![0x11],
            dn(0),
            dn(private),
            vec![0x12],
        ]
        .concat()
    };
    let header = vec![1u8, 0, 4, 4];
    let (name, strings, gsubrs) = (index(&[b"KTest".to_vec()]), index(&[]), index(&[]));
    let start = (header.len()
        + name.len()
        + index(&[top(0, 0, 0, 0)]).len()
        + strings.len()
        + gsubrs.len()) as i32;
    // Charset format 0: SIDs 64 (underscore), 56 (W); encoding format 0.
    let charset = vec![0u8, 0, 64, 0, 56];
    let encoding = vec![0u8, 2, 0x5f, 0x57];
    let chars = index(&glyphs);
    let (c, e) = (charset.len() as i32, encoding.len() as i32);
    let topdict = index(&[top(
        start,
        start + c,
        start + c + e,
        start + c + e + chars.len() as i32,
    )]);
    [
        header, name, topdict, strings, gsubrs, charset, encoding, chars,
    ]
    .concat()
}

/// A code whose glyph viewers resolve or place differently from kasane
/// goes when any of the places they draw it meets the rect: a Type 1 or
/// CFF code resolving to `.notdef` (MuPDF falls back to the program's
/// own encoding), a TrueType glyph whose bearing disagrees with its
/// outline (pdf.js draws it in place, MuPDF and Poppler shifted). Its
/// `/ToUnicode` text marks whether the code stayed.
#[test]
fn ghsa_fqgr_glyphs_viewers_draw_elsewhere() {
    let marker = "IADVSECRET";
    let tounicode = stream_obj(
        "",
        format!(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /X def \
             1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <5F> <{}> endbfchar \
             endcmap CMapName currentdict /CMap defineresource pop end end",
            marker
                .bytes()
                .map(|c| format!("00{c:02X}"))
                .collect::<String>()
        )
        .as_bytes(),
    );
    let mut ttf = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap();
    let underscore_gid;
    {
        // `_`'s bearing one em past its `xMin`.
        let (gid, x_min) = {
            let face = ttf_parser::Face::parse(&ttf, 0).unwrap();
            let g = face.glyph_index('_').unwrap();
            (g.0 as usize, face.glyph_bounding_box(g).unwrap().x_min)
        };
        let n = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
        let hmtx = (0..n)
            .map(|i| 12 + 16 * i)
            .find(|&r| &ttf[r..r + 4] == b"hmtx")
            .map(|r| u32::from_be_bytes(ttf[r + 8..r + 12].try_into().unwrap()) as usize)
            .unwrap();
        let at = hmtx + 4 * gid + 2;
        ttf[at..at + 2].copy_from_slice(&(x_min + 2048).to_be_bytes());
        underscore_gid = gid;
    }
    let widths = vec!["1000"; 256].join(" ");
    let simple = |subtype: &str, encoding: &str, file: &str, program: Vec<u8>, extra: &str| {
        vec![
            (
                50,
                format!(
                    "<< /Type /Font /Subtype /{subtype} /BaseFont /KTest /FirstChar 0 /LastChar 255 \
                     /Widths [{widths}] {encoding} /ToUnicode 53 0 R /FontDescriptor 51 0 R >>"
                )
                .into_bytes(),
            ),
            (
                51,
                format!(
                    "<< /Type /FontDescriptor /FontName /KTest /Flags 32 /FontBBox [-200 -800 2000 1000] \
                     /ItalicAngle 0 /Ascent 900 /Descent -800 /CapHeight 700 /StemV 80 /{file} 52 0 R >>"
                )
                .into_bytes(),
            ),
            (52, stream_obj(extra, &program)),
            (53, tounicode.clone()),
        ]
    };
    let cases = [
        (
            "type1-absent-name",
            simple(
                "Type1",
                "/Encoding << /Differences [95 /zz95zz] >>",
                "FontFile",
                type1_program(),
                "",
            ),
            (60.0, 67.0, 96.0, 75.0),
        ),
        (
            "cff-notdef",
            simple(
                "Type1",
                "/Encoding << /Differences [95 /.notdef] >>",
                "FontFile3",
                cff_program(),
                "/Subtype /Type1C",
            ),
            (60.0, 55.0, 96.0, 65.0),
        ),
        (
            "truetype-bearing",
            simple(
                "TrueType",
                "/Encoding /WinAnsiEncoding",
                "FontFile2",
                ttf.clone(),
                &format!("/Length1 {}", ttf.len()),
            ),
            (60.0, 60.0, 90.0, 79.0),
        ),
    ];
    // The same TrueType program as a CIDFontType2 descendant of a Type 0
    // font (Identity-H, CID = GID): two-byte codes, the glyph's bearing
    // disagreeing with its outline as above.
    let code = format!("{underscore_gid:04X}");
    let cid_tounicode = stream_obj(
        "",
        format!(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /X def \
             1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <{code}> <{}> endbfchar \
             endcmap CMapName currentdict /CMap defineresource pop end end",
            marker
                .bytes()
                .map(|c| format!("00{c:02X}"))
                .collect::<String>()
        )
        .as_bytes(),
    );
    let cid = vec![
        (
            50,
            b"<< /Type /Font /Subtype /Type0 /BaseFont /KTest /Encoding /Identity-H \
              /DescendantFonts [54 0 R] /ToUnicode 53 0 R >>"
                .to_vec(),
        ),
        (
            54,
            b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /KTest \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /FontDescriptor 51 0 R /DW 1000 /CIDToGIDMap /Identity >>"
                .to_vec(),
        ),
        (
            51,
            b"<< /Type /FontDescriptor /FontName /KTest /Flags 32 /FontBBox [-200 -800 2000 1000] \
              /ItalicAngle 0 /Ascent 900 /Descent -800 /CapHeight 700 /StemV 80 /FontFile2 52 0 R >>"
                .to_vec(),
        ),
        (52, stream_obj(&format!("/Length1 {}", ttf.len()), &ttf)),
        (53, cid_tounicode),
    ];
    let cases: Vec<(&str, Vec<(u32, Vec<u8>)>, (f64, f64, f64, f64), String)> = cases
        .into_iter()
        .map(|(n, o, r)| (n, o, r, "5F".to_string()))
        .chain([("cidfonttype2-bearing", cid, (60.0, 60.0, 90.0, 79.0), code.clone())])
        .collect();
    for (name, objs, rect, code) in cases {
        let content = format!("BT /F2 60 Tf 60 80 Td <{code}> Tj ET {KEPT}");
        let mut all = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                  /Resources << /Font << /F1 20 0 R /F2 50 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
            ),
            (4, stream_obj("", content.as_bytes())),
        ];
        all.extend(font_objs());
        all.extend(objs);
        let pdf = build_pdf(&all);
        let doc = Document::from_bytes(&pdf).unwrap();
        // The Type 1, CFF and simple TrueType cases are redacted (0.10.0);
        // the composite one may also be refused — never kept.
        match redact(&doc, &[(0, vec![rect])], &RedactOptions::new()) {
            Ok(r) => {
                let t = text(&r.output);
                assert!(
                    !t.contains(marker),
                    "{name}: the code a viewer draws inside the rect stayed"
                );
                assert!(t.contains("PUBLICWORD"), "{name}: the public text was lost");
            }
            Err(e) => assert!(name.starts_with("cid"), "{name}: {e}"),
        }
        dump(
            &format!("fqgr-{name}"),
            "GHSA-fqgr-r67f-vx25",
            &format!(
                "redact --page 1 --rect {} {} {} {}",
                rect.0, rect.1, rect.2, rect.3
            ),
            &format!("safe-text:{marker}"),
            &pdf,
        );
    }
}
