//! `redact` with `UsageRightsPolicy::Remove`: the usage-rights
//! signatures (`/Perms /UR`, `/UR3`) are removed as `rewrite_with` removes
//! them, then the document is redacted; every other signature refuses as
//! it does there; nothing changes without the option; a redaction that
//! refuses after the removal returns nothing.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{
    extract_text, redact, rewrite_with, Document, Error, Limits, RedactOptions, RewriteOptions,
    SignatureRule, TextOptions, UsageRight, UsageRightsKey, UsageRightsPolicy as U,
};

const UR3: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
    /ByteRange [0 10 20 30] /Contents <0123456789abcdef> /M (D:20200101000000Z) \
    /Reference [<< /Type /SigRef /TransformMethod /UR3 /TransformParams << /Type \
    /TransformParams /V /2.2 /Document [/FullSave] /Form [/FillIn /Import] /Annots [/Create] >> \
    >>] >>";

const RECT: (f64, f64, f64, f64) = (45.0, 90.0, 160.0, 115.0);

fn o(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

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
            o(&format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
                 /LastChar 126 /Widths [{widths}] /Encoding /WinAnsiEncoding \
                 /FontDescriptor 21 0 R >>"
            )),
        ),
        (
            21,
            o(
                "<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
               /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
               /CapHeight 700 /StemV 80 /FontFile2 22 0 R >>",
            ),
        ),
        (22, stream_obj(&format!("/Length1 {}", ff.len()), &ff)),
    ]
}

/// Two pages sharing the font: page 1 `content` (and `page1` entries),
/// page 2 a kept word and a kept text annotation; catalog entries `cat`.
fn doc(cat: &str, page1: &str, content: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, o(&format!("<< /Type /Catalog /Pages 2 0 R {cat} >>"))),
        (2, o("<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources << /Font << /F1 20 0 R >> >> >>")),
        (
            3,
            o(&format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R {page1} >>"
            )),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            5,
            o("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R /Annots [7 0 R] >>"),
        ),
        (
            6,
            stream_obj("", b"BT /F1 12 Tf 20 50 Td (PAGETWOWORD) Tj ET"),
        ),
        (
            7,
            o("<< /Type /Annot /Subtype /Text /Rect [10 150 30 170] /Contents (NOTEKEPT) /P 5 0 R >>"),
        ),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

const TEXT: &str =
    "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET BT /F1 12 Tf 20 20 Td (PUBLICWORD) Tj ET";

/// The corpus shape: `/Perms` with only an indirect `/UR3`, a form with
/// `/SigFlags` `flags` and one text field on page 1.
fn ur3_doc(flags: i64) -> Vec<u8> {
    doc(
        &format!("/Perms << /UR3 10 0 R >> /AcroForm << /Fields [11 0 R] /SigFlags {flags} >>"),
        "/Annots [11 0 R]",
        TEXT,
        vec![
            (10, o(UR3)),
            (
                11,
                o("<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (FIELDVALUE) /Rect [150 10 190 30] /P 3 0 R >>"),
            ),
        ],
    )
}

fn remove() -> RedactOptions {
    RedactOptions::new().with_usage_rights(U::Remove)
}

fn run(b: &[u8], opts: &RedactOptions) -> Result<kasane_pdf::Redaction, Error> {
    redact(&Document::from_bytes(b).unwrap(), &[(0, vec![RECT])], opts)
}

fn text(pdf: &[u8], page: usize) -> String {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[page], &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .filter_map(|c| c.unicode.clone())
        .collect()
}

/// A string as the rewrite writes it (hex).
fn hex(s: &str) -> Vec<u8> {
    format!(
        "<{}>",
        s.bytes().map(|c| format!("{c:02X}")).collect::<String>()
    )
    .into_bytes()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn sig_rule(r: Result<kasane_pdf::Redaction, Error>) -> SignatureRule {
    match r {
        Err(Error::SignaturePolicy { rule, .. }) => rule,
        Err(e) => panic!("expected a signature refusal, got {e:?}"),
        Ok(_) => panic!("expected a signature refusal, got a redaction"),
    }
}

#[test]
fn removes_the_usage_rights_then_redacts() {
    let b = ur3_doc(3);
    // Without the option: refused as a signed document, as before.
    assert_eq!(
        sig_rule(run(&b, &RedactOptions::new())),
        SignatureRule::RewriteOfSignedDocument
    );
    let r = run(&b, &remove()).unwrap();
    let u = r.usage_rights_removed.as_ref().expect("reported");
    assert_eq!(u.entries.len(), 1);
    assert_eq!(u.entries[0].key, UsageRightsKey::Ur3);
    assert_eq!(
        u.entries[0].rights,
        [UsageRight::Document, UsageRight::Annots, UsageRight::Form]
    );
    assert!(u.perms_removed);
    let flags = u.sig_flags.as_ref().unwrap();
    assert_eq!((flags.before, flags.after), (3, 2));
    // Redacted, and the rest kept: page 1's other word, the field, page 2
    // and its annotation.
    assert_eq!(r.codes_removed, 10);
    let p1 = text(&r.output, 0);
    assert!(p1.contains("PUBLICWORD") && !p1.contains("SECRET"), "{p1}");
    assert_eq!(text(&r.output, 1), "PAGETWOWORD");
    assert!(!contains(&r.output, b"SECRETTEXT"));
    for kept in [
        hex("NOTEKEPT"),
        hex("FIELDVALUE"),
        b"/Subtype /Text".to_vec(),
    ] {
        assert!(
            contains(&r.output, &kept),
            "{}",
            String::from_utf8_lossy(&kept)
        );
    }
    // No signature left anywhere; the form says no signature exists.
    for gone in [&b"/ByteRange"[..], b"/UR3", b"/Perms", b"/TransformMethod"] {
        assert!(
            !contains(&r.output, gone),
            "{}",
            String::from_utf8_lossy(gone)
        );
    }
    let out = Document::from_bytes(&r.output).unwrap();
    let af = out.catalog().get(b"AcroForm".as_slice()).unwrap();
    let af = out.deref(af).unwrap();
    assert_eq!(
        af.as_dict()
            .unwrap()
            .get(b"SigFlags".as_slice())
            .and_then(|f| f.as_i64()),
        Some(2)
    );
    // The same removal as rewrite_with's.
    let rw = rewrite_with(
        &Document::from_bytes(&b).unwrap(),
        &RewriteOptions::new().with_usage_rights(U::Remove),
    )
    .unwrap();
    assert!(rw.report().signatures_removed());
}

#[test]
fn nothing_changes_without_a_usage_rights_signature() {
    let b = doc("", "", TEXT, vec![]);
    let plain = run(&b, &RedactOptions::new()).unwrap();
    let with = run(&b, &remove()).unwrap();
    assert_eq!(plain.output, with.output);
    assert!(with.usage_rights_removed.is_none());
    // DocMDP alone (no /UR, /UR3): the redaction's own refusal, unchanged.
    let docmdp = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> \
                  /Reference [<< /TransformMethod /DocMDP /TransformParams << /P 2 >> >>] >>";
    let b = doc(
        "/Perms << /DocMDP 10 0 R >>",
        "",
        TEXT,
        vec![(10, o(docmdp))],
    );
    let e1 = format!("{:?}", run(&b, &RedactOptions::new()).unwrap_err());
    let e2 = format!("{:?}", run(&b, &remove()).unwrap_err());
    assert_eq!(e1, e2);
}

/// Every signature `rewrite_with` refuses to remove refuses here, by the
/// same rule; so do a lock, the security stores, sharing, an
/// unconfirmed signature, an unreadable object and the budget.
#[test]
fn every_other_signature_refuses() {
    use SignatureRule::{
        OtherSignaturePresent as Other, SecurityStore, UsageRightsNotRemovable as NotRemovable,
    };
    let docmdp = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> \
                  /Reference [<< /TransformMethod /DocMDP /TransformParams << /P 2 >> >>] >>";
    let approval = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>";
    let p = |cat: &str, page1: &str, extra: Vec<(u32, &str)>| {
        doc(
            cat,
            page1,
            TEXT,
            extra.into_iter().map(|(n, s)| (n, o(s))).collect(),
        )
    };
    let cases: Vec<(&str, Vec<u8>, SignatureRule)> = vec![
        (
            "DocMDP next to UR3",
            p("/Perms << /UR3 10 0 R /DocMDP 11 0 R >>", "", vec![(10, UR3), (11, docmdp)]),
            Other,
        ),
        (
            "an approval signature",
            p(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [12 0 R] /SigFlags 3 >>",
                "/Annots [12 0 R]",
                vec![(10, UR3), (12, "<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /V 13 0 R /Rect [150 150 160 160] >>"), (13, approval)],
            ),
            Other,
        ),
        (
            "a document timestamp",
            p(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [12 0 R] >>",
                "",
                vec![(10, UR3), (12, "<< /FT /Sig /T (t) /V << /Type /DocTimeStamp /SubFilter /ETSI.RFC3161 >> >>")],
            ),
            Other,
        ),
        (
            "a signature without /FT /Sig",
            p("/Perms << /UR3 10 0 R >> /X << /Y << /Contents <00> /Filter /F >> >>", "", vec![(10, UR3)]),
            Other,
        ),
        (
            "a field /Lock",
            p(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [12 0 R] >>",
                "",
                vec![(10, UR3), (12, "<< /FT /Sig /T (x) /Lock << /Action /All >> >>")],
            ),
            Other,
        ),
        (
            "/DSS",
            p("/Perms << /UR3 10 0 R >> /DSS << /Certs [] >>", "", vec![(10, UR3)]),
            SecurityStore,
        ),
        (
            "a shared signature dictionary",
            p(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [12 0 R] >>",
                "",
                vec![(10, UR3), (12, "<< /FT /Tx /T (x) /V 10 0 R >>")],
            ),
            NotRemovable,
        ),
        (
            "/UR3 named but not a usage-rights signature",
            p("/Perms << /UR3 10 0 R >>", "", vec![(10, &UR3.replace("/TransformMethod /UR3", ""))]),
            NotRemovable,
        ),
        (
            "a chain of references to the signature",
            p("/Perms << /UR3 14 0 R >>", "", vec![(14, "10 0 R"), (10, UR3)]),
            NotRemovable,
        ),
        (
            "an unreadable object",
            p("/Perms << /UR3 10 0 R >> /X 15 0 R", "", vec![(10, UR3), (15, "<< /Unterminated")]),
            NotRemovable,
        ),
    ];
    for (what, b, want) in &cases {
        let rw = match rewrite_with(
            &Document::from_bytes(b).unwrap(),
            &RewriteOptions::new().with_usage_rights(U::Remove),
        ) {
            Err(Error::SignaturePolicy { rule, .. }) => rule,
            other => panic!("{what}: rewrite_with gave {:?}", other.map(|_| ())),
        };
        assert_eq!(rw, *want, "{what} (rewrite_with)");
        assert_eq!(sig_rule(run(b, &remove())), *want, "{what}");
    }
    // An XFA XML signature.
    let xfa = "<xdp><ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"/></xdp>";
    let b = doc(
        "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /XFA 16 0 R >>",
        "",
        TEXT,
        vec![(10, o(UR3)), (16, stream_obj("", xfa.as_bytes()))],
    );
    assert!(matches!(
        run(&b, &remove()),
        Err(Error::SignaturePolicy { .. })
    ));
    // The walk budget: refused, not cut short.
    let tiny = remove().with_limits(Limits::default().with_max_output_objects(1));
    assert_eq!(sig_rule(run(&ur3_doc(3), &tiny)), NotRemovable);
}

/// The removal succeeds but the redaction refuses: nothing is returned
/// (the unsigned intermediate document included).
#[test]
fn a_redaction_refused_after_the_removal_returns_nothing() {
    // An inline image on the page refuses the redaction.
    let b = doc(
        "/Perms << /UR3 10 0 R >>",
        "",
        &format!("{TEXT} q 10 0 0 10 5 5 cm BI /W 1 /H 1 /CS /G /BPC 8 ID \u{0} EI Q"),
        vec![(10, o(UR3))],
    );
    match run(&b, &remove()) {
        Err(Error::Unsupported { detail, .. }) => {
            assert!(detail.contains("inline image"), "{detail}")
        }
        other => panic!("{:?}", other.map(|r| r.output.len())),
    }
}

/// Scripts: the removal carries them to the redaction, which applies its
/// own policy — refused without `remove_javascript`, removed with it.
#[test]
fn scripts_follow_the_redaction_policy() {
    let b = doc(
        "/Perms << /UR3 10 0 R >> /OpenAction << /S /JavaScript /JS (app.alert(1)) >>",
        "",
        TEXT,
        vec![(10, o(UR3))],
    );
    let plain = doc(
        "/OpenAction << /S /JavaScript /JS (app.alert(1)) >>",
        "",
        TEXT,
        vec![],
    );
    // As for the same document without the signature.
    let e1 = format!("{:?}", run(&plain, &RedactOptions::new()).unwrap_err());
    let e2 = format!("{:?}", run(&b, &remove()).unwrap_err());
    assert_eq!(e1, e2);
    let r = run(&b, &remove().remove_javascript(true)).unwrap();
    assert!(r.javascript_removed > 0 && r.usage_rights_removed.is_some());
    let out = Document::from_bytes(&r.output).unwrap();
    assert!(!out.catalog().contains_key(b"OpenAction".as_slice()));
    assert!(!contains(&r.output, b"/ByteRange") && !contains(&r.output, b"/JavaScript"));
}

/// With the substitute fonts too (a third step): the same result.
#[test]
fn with_substitute_fonts() {
    let b = build_pdf(&[
        (
            1,
            o("<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 10 0 R >> >>"),
        ),
        (2, o("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            o(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
               /Resources << /Font << /F1 5 0 R >> >> >>",
            ),
        ),
        (4, stream_obj("", TEXT.as_bytes())),
        (
            5,
            o("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ),
        (10, o(UR3)),
    ]);
    assert!(run(&b, &remove()).is_err(), "a non-embedded font refuses");
    let r = run(&b, &remove().embed_substitute_fonts(true)).unwrap();
    assert_eq!(r.fonts_embedded, 1);
    assert!(r.usage_rights_removed.is_some());
    let t = text(&r.output, 0);
    assert!(t.contains("PUBLICWORD") && !t.contains("SECRET"), "{t}");
    assert!(!contains(&r.output, b"/ByteRange") && !contains(&r.output, b"/UR3"));
}

/// An encrypted document with a usage-rights signature is refused, as by
/// `rewrite_with` (`tests/fixtures/encrypted/enc_ur3_aes128.pdf`: a
/// one-page document with `/Perms /UR3`, AES-128, empty user password).
#[test]
fn encrypted_is_refused() {
    let bytes = std::fs::read(common::fixture("encrypted/enc_ur3_aes128.pdf")).unwrap();
    let doc = Document::from_bytes(&bytes).unwrap();
    let rw = rewrite_with(&doc, &RewriteOptions::new().with_usage_rights(U::Remove)).unwrap_err();
    let rd = redact(&doc, &[(0, vec![RECT])], &remove()).unwrap_err();
    assert_eq!(format!("{rw:?}"), format!("{rd:?}"));
}

/// An image both pages draw, half inside a rect on page 1: page 1 gets
/// a redacted copy, page 2 still draws the original (reported in
/// `elsewhere`), across the removal's renumbering.
#[test]
fn a_shared_image_across_the_removal() {
    let samples: Vec<u8> = (0..16 * 16 * 3).map(|i| (i * 7 % 251) as u8 + 1).collect();
    let image = stream_obj(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &samples,
    );
    let b = build_pdf(&[
        (1, o("<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 10 0 R >> >>")),
        (2, o("<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources << /XObject << /I 8 0 R >> >> >>")),
        (3, o("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>")),
        (4, stream_obj("", b"q 100 0 0 100 0 50 cm /I Do Q")),
        (5, o("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 6 0 R >>")),
        (6, stream_obj("", b"q 100 0 0 100 0 50 cm /I Do Q")),
        (8, image),
        (10, o(UR3)),
    ]);
    let doc = Document::from_bytes(&b).unwrap();
    let r = redact(&doc, &[(0, vec![(50.0, 0.0, 200.0, 200.0)])], &remove()).unwrap();
    assert_eq!(r.images_edited, 1);
    assert!(r.usage_rights_removed.is_some());
    assert!(
        r.elsewhere.iter().any(|e| matches!(
            (&e.removed, &e.location),
            (
                kasane_pdf::Removed::Image,
                kasane_pdf::ElsewhereLocation::Page(1)
            )
        )),
        "page 2 still draws the original"
    );
    assert_eq!(Document::from_bytes(&r.output).unwrap().pages().len(), 2);
}

/// The `/Perms` shapes end to end: `/UR` and `/UR3` together, direct
/// signature dictionaries, an indirect `/Perms`, another entry kept.
#[test]
fn perms_shapes_end_to_end() {
    let ur = UR3.replace("/TransformMethod /UR3", "/TransformMethod /UR");
    let both = doc(
        "/Perms 30 0 R",
        "",
        TEXT,
        vec![(30, o(&format!("<< /UR {ur} /UR3 {UR3} >>")))],
    );
    let r = run(&both, &remove()).unwrap();
    let u = r.usage_rights_removed.unwrap();
    let keys: Vec<_> = u.entries.iter().map(|e| (e.key, e.input_object)).collect();
    assert_eq!(
        keys,
        [(UsageRightsKey::Ur, None), (UsageRightsKey::Ur3, None)]
    );
    assert!(u.perms_removed && !contains(&r.output, b"/Perms"));
    assert!(!text(&r.output, 0).contains("SECRET"));
    // Another entry stays, in a direct or an indirect /Perms.
    for (cat, extra) in [
        ("/Perms << /UR3 10 0 R /X 5 >>", vec![(10, o(UR3))]),
        (
            "/Perms 30 0 R",
            vec![(10, o(UR3)), (30, o("<< /UR3 10 0 R /X 5 >>"))],
        ),
    ] {
        let r = run(&doc(cat, "", TEXT, extra), &remove()).unwrap();
        assert!(!r.usage_rights_removed.unwrap().perms_removed);
        let out = Document::from_bytes(&r.output).unwrap();
        let p = out
            .deref(out.catalog().get(b"Perms".as_slice()).unwrap())
            .unwrap();
        let p = p.as_dict().unwrap();
        assert_eq!(
            (p.len(), p.get(b"X".as_slice())),
            (1, Some(&kasane_pdf::Object::Integer(5))),
            "{cat}"
        );
    }
}

/// A literal string with an unescaped carriage return (readers disagree
/// on its bytes) refuses with the option as without it: the removal's
/// rewrite must not turn it into a plain line feed the redaction then
/// accepts.
#[test]
fn a_raw_carriage_return_still_refuses() {
    let field = |cat: &str, extra: Vec<(u32, Vec<u8>)>| {
        let mut extra = extra;
        extra.push((
            11,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (A\rB) /Rect [150 10 190 30] /P 3 0 R >>"
                .to_vec(),
        ));
        doc(
            &format!("{cat} /AcroForm << /Fields [11 0 R] >>"),
            "/Annots [11 0 R]",
            TEXT,
            extra,
        )
    };
    let plain = field("", vec![]);
    let e = format!("{:?}", run(&plain, &RedactOptions::new()).unwrap_err());
    assert!(e.contains("carriage return"), "{e}");
    let signed = field("/Perms << /UR3 10 0 R >>", vec![(10, o(UR3))]);
    let e = format!("{:?}", run(&signed, &remove()).unwrap_err());
    assert!(e.contains("carriage return"), "{e}");
}

/// Without a usage-rights signature the option reads nothing it leaves a
/// trace of: at the smallest reference budget (cumulative over the
/// document) with which an indirect `/Perms` document redacts, it
/// redacts with the option too, to the same bytes.
#[test]
fn the_gate_leaves_no_trace() {
    let b = doc("/Perms 30 0 R", "", TEXT, vec![(30, o("<< /X 5 >>"))]);
    let at = |n: u64, opts: RedactOptions| {
        let lim = Limits::default().with_max_reference_depth(n);
        let d = Document::from_bytes_with_limits(&b, lim.clone())?;
        redact(&d, &[(0, vec![RECT])], &opts.with_limits(lim)).map(|r| r.output)
    };
    let n = (1..100_000u64)
        .find(|&n| at(n, RedactOptions::new()).is_ok())
        .expect("redacts with some budget");
    assert!(at(n - 1, RedactOptions::new()).is_err());
    assert_eq!(
        at(n, remove()).unwrap(),
        at(n, RedactOptions::new()).unwrap()
    );
}

/// The removal's search budget: the rewrite fits, the search of every
/// value does not — refused, not cut short.
#[test]
fn the_search_budget_refuses() {
    let many = format!("/Perms << /UR3 10 0 R >> /X [{}]", "1 ".repeat(5000));
    let b = doc(&many, "", TEXT, vec![(10, o(UR3))]);
    let lim = Limits::default().with_max_output_objects(40);
    assert!(rewrite_with(
        &Document::from_bytes(&doc(
            &format!("/X [{}]", "1 ".repeat(5000)),
            "",
            TEXT,
            vec![]
        ))
        .unwrap(),
        &RewriteOptions::new().with_limits(lim.clone())
    )
    .is_ok());
    assert_eq!(
        sig_rule(run(&b, &remove().with_limits(lim))),
        SignatureRule::UsageRightsNotRemovable
    );
}
