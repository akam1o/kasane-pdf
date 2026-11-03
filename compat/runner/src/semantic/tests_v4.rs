//! V4: strict action comparison (outlines and the document-level
//! categories), the report-only `multimedia` and `signatures`
//! categories, declared transformations (T1, T2) and the action
//! inventory. Each category: equal, renumbered, recompressed and keys
//! reordered pass; a change, a removal, an addition, a reorder and a
//! sharing change fail; cycles, undefined and unreadable references,
//! limits and unsupported filters are told apart.

use super::pdf20::{compare_report, snapshot_report, REPORT_ONLY};
use super::tests_pdf20::{d, doc, flate, pdf, st, Body};
use super::transform::{classify as tclassify, structure_allows, Declaration, VersionReason};
use super::*;

/// `b` with the first `from` replaced by `to`, the cross-reference
/// offsets after it (and `startxref`) shifted by the length change.
/// Byte-level: stream data is left untouched.
fn patch(b: &[u8], from: &str, to: &str) -> Vec<u8> {
    let (from, to) = (from.as_bytes(), to.as_bytes());
    let at = b
        .windows(from.len())
        .position(|w| w == from)
        .expect("patch target");
    let delta = to.len() as i64 - from.len() as i64;
    let mut out = [&b[..at], to, &b[at + from.len()..]].concat();
    let x = out.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 1;
    let sx = out.windows(10).rposition(|w| w == b"startxref\n").unwrap();
    let mut table = String::from_utf8(out[x..sx].to_vec()).unwrap();
    table = table
        .split_inclusive('\n')
        .map(|l| match l.strip_suffix(" n \n") {
            Some(e) if e.len() == 16 => {
                let o: i64 = e[..10].parse().unwrap();
                let o = if o > at as i64 { o + delta } else { o };
                format!("{o:010}{}", &l[10..])
            }
            _ => l.to_string(),
        })
        .collect();
    let tail = format!("startxref\n{x}\n%%EOF\n");
    out.truncate(x);
    out.extend_from_slice(table.as_bytes());
    out.extend_from_slice(tail.as_bytes());
    out
}

fn cat_of(r: &SemanticRoundTrip, cat: &str) -> SemCheck {
    r.checks.iter().find(|c| c.category == cat).unwrap().clone()
}

fn report_of(a: &[u8], b: &[u8], cat: &str) -> SemCheck {
    compare_report(&doc(a), a, &doc(b), b)
        .into_iter()
        .find(|c| c.category == cat)
        .unwrap()
}

fn section(b: &[u8], cat: &str) -> Section {
    let i = REPORT_ONLY.iter().position(|c| *c == cat).unwrap();
    snapshot_report(&doc(b), b).swap_remove(i)
}

fn lines(b: &[u8], cat: &str) -> Vec<String> {
    match section(b, cat) {
        Section::Data(v) => v,
        other => panic!("{cat}: {other:?}"),
    }
}

// ---------------------------------------------------------------- actions

/// An outline whose first item's action is a JavaScript chain `steps`
/// long (`/Next`), the script of step `changed` altered when given, the
/// second item sharing the first item's action or holding a copy.
/// `js_stream`: 0 every script a string, 1 the first a Flate stream, 2
/// the first an uncompressed stream.
fn outline_doc(steps: u32, changed: Option<u32>, shared: bool, js_stream: u8, off: u32) -> Vec<u8> {
    let mut objs: Vec<(u32, Body)> = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>")),
        (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
        (
            10,
            d("<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 2 >>"),
        ),
        (
            11,
            d("<< /Title (a) /Parent 10 0 R /Next 12 0 R /A 100 0 R >>"),
        ),
    ];
    let second = if shared { "100 0 R" } else { "300 0 R" };
    objs.push((
        12,
        d(&format!(
            "<< /Title (b) /Parent 10 0 R /Prev 11 0 R /A {second} >>"
        )),
    ));
    for (base, copy) in [(100u32, false), (300, true)] {
        for i in 0..steps {
            let n = base + i;
            let script = if Some(i) == changed && !copy {
                format!("app.alert({i}+1)")
            } else {
                format!("app.alert({i})")
            };
            let next = if i + 1 < steps {
                format!("/Next {} 0 R", n + 1)
            } else {
                String::new()
            };
            if js_stream > 0 && i == 0 {
                objs.push((
                    n,
                    d(&format!("<< /S /JavaScript /JS {} 0 R {next} >>", n + 1000)),
                ));
                let body = if js_stream == 1 {
                    flate("", script.as_bytes())
                } else {
                    st("", script.as_bytes())
                };
                objs.push((n + 1000, body));
            } else {
                objs.push((n, d(&format!("<< /S /JavaScript /JS ({script}) {next} >>"))));
            }
        }
    }
    pdf("1.7", &objs, off)
}

#[test]
fn outline_actions_are_compared_strictly() {
    let base = outline_doc(12, None, true, 1, 0);
    let same = |a: &[u8], b: &[u8]| cat_of(&compare(&doc(a), &doc(b)), "outlines").class;
    assert_eq!(same(&base, &base), SemClass::Pass);
    assert_eq!(
        same(&base, &outline_doc(12, None, true, 1, 500)),
        SemClass::Pass,
        "renumbered"
    );
    // The stream script stored uncompressed: the same script.
    assert_eq!(
        same(&base, &outline_doc(12, None, true, 2, 0)),
        SemClass::Pass,
        "recompressed"
    );
    // A change at step 10 of 12: past the old depth-8 cut.
    assert_eq!(
        same(&base, &outline_doc(12, Some(10), true, 1, 0)),
        SemClass::Fail
    );
    // Shared action vs two identical copies: a sharing change.
    assert_eq!(
        same(&base, &outline_doc(12, None, false, 1, 0)),
        SemClass::Fail
    );
    // A chain past the strict limit is an error, not a cut.
    let deep = outline_doc(MAX_ACTION_DEPTH as u32 + 5, None, true, 0, 0);
    assert_eq!(same(&deep, &deep), SemClass::Error);
}

/// A page whose `/AA` /O action is `act` and whose link's `/A` is
/// `link`, plus `objs`.
fn page_aa_link(act: &str, link: &str, objs: Vec<(u32, Body)>) -> Vec<u8> {
    let mut all: Vec<(u32, Body)> = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
        (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (3, d(&format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /AA << /O {act} >> /Annots [4 0 R] >>"))),
        (4, d(&format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A {link} >>"))),
    ];
    all.extend(objs);
    pdf("1.7", &all, 0)
}

fn page_aa(act: &str, objs: Vec<(u32, Body)>) -> Vec<u8> {
    page_aa_link(act, "20 0 R", objs)
}

#[test]
fn document_actions_compare_sharing_order_and_kinds() {
    let base = || {
        page_aa(
            "20 0 R",
            vec![
                (
                    20,
                    d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
                ),
                (21, d("<< /S /JavaScript /JS 23 0 R >>")),
                (22, d("<< /S /URI /URI (https://example.invalid/) >>")),
                (23, flate("", b"app.alert(1)")),
                (30, d("<< /Type /DPart >>")),
            ],
        )
    };
    let a = base();
    let cls = |b: &[u8]| cat_of(&compare(&doc(&a), &doc(b)), "doc_actions").class;
    assert_eq!(cls(&a), SemClass::Pass);
    // /Next reordered, an element removed, the kind changed.
    for (what, b) in [
        ("reordered", patch(&a, "[21 0 R 22 0 R]", "[22 0 R 21 0 R]")),
        (
            "removed",
            patch(&a, "/Next [21 0 R 22 0 R]", "/Next [21 0 R]          "),
        ),
        ("kind", patch(&a, "/S /URI /URI", "/S /Launch /F ")),
    ] {
        assert_eq!(cls(&b), SemClass::Fail, "{what}");
    }
    // The page /AA and the link share one action; a copy for the link is
    // a sharing change.
    let unshared = page_aa_link(
        "20 0 R",
        "40 0 R",
        vec![
            (
                20,
                d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
            ),
            (21, d("<< /S /JavaScript /JS 23 0 R >>")),
            (22, d("<< /S /URI /URI (https://example.invalid/) >>")),
            (23, flate("", b"app.alert(1)")),
            (30, d("<< /Type /DPart >>")),
            (
                40,
                d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
            ),
        ],
    );
    assert_eq!(cls(&unshared), SemClass::Fail, "sharing");
    // Compression of the script stream is not a difference.
    let plain = page_aa(
        "20 0 R",
        vec![
            (
                20,
                d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
            ),
            (21, d("<< /S /JavaScript /JS 23 0 R >>")),
            (22, d("<< /S /URI /URI (https://example.invalid/) >>")),
            (23, st("", b"app.alert(1)")),
            (30, d("<< /Type /DPart >>")),
        ],
    );
    assert_eq!(cls(&plain), SemClass::Pass, "recompressed");
    // A /Next cycle ends; undefined and unreadable targets differ.
    let cyc = page_aa(
        "20 0 R",
        vec![
            (20, d("<< /S /GoTo /D [3 0 R /Fit] /Next 21 0 R >>")),
            (21, d("<< /S /GoTo /D [3 0 R /Fit] /Next 20 0 R >>")),
        ],
    );
    assert_eq!(
        cat_of(&compare(&doc(&cyc), &doc(&cyc)), "doc_actions").class,
        SemClass::Pass
    );
    let undefined = page_aa(
        "20 0 R",
        vec![(20, d("<< /S /GoTo /D [3 0 R /Fit] /Next 99 0 R >>"))],
    );
    let unreadable = page_aa(
        "20 0 R",
        vec![
            (20, d("<< /S /GoTo /D [3 0 R /Fit] /Next 99 0 R >>")),
            (99, Body::Broken),
        ],
    );
    assert_eq!(
        cat_of(&compare(&doc(&undefined), &doc(&unreadable)), "doc_actions").class,
        SemClass::Fail
    );
}

// ---------------------------------------------------------------- multimedia

fn mm_doc(asset: &[u8], activation: &str, shared: bool, compressed: bool, off: u32) -> Vec<u8> {
    let content = if shared { "50 0 R" } else { "51 0 R" };
    let asset_body = if compressed {
        flate("/Type /EmbeddedFile", asset)
    } else {
        st("/Type /EmbeddedFile", asset)
    };
    let objs: Vec<(u32, Body)> = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
        (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R 5 0 R 6 0 R] >>")),
        (4, d(&format!("<< /Type /Annot /Subtype /RichMedia /Rect [0 0 1 1] /RichMediaContent 50 0 R /RichMediaSettings << /Activation << /Condition /{activation} >> >> >>"))),
        (5, d(&format!("<< /Type /Annot /Subtype /RichMedia /Rect [0 0 1 1] /RichMediaContent {content} >>"))),
        (6, d("<< /Type /Annot /Subtype /Screen /Rect [0 0 1 1] /A << /S /Rendition /OP 0 /R 60 0 R >> >>")),
        (50, d("<< /Type /RichMediaContent /Assets << /Names [(clip) 52 0 R] >> >>")),
        (51, d("<< /Type /RichMediaContent /Assets << /Names [(clip) 52 0 R] >> >>")),
        (52, d("<< /Type /Filespec /F (clip) /EF << /F 53 0 R >> >>")),
        (53, asset_body),
        (60, d("<< /S /MR /C << /S /MCD /D << /Type /Filespec /F (m) >> >> >>")),
    ];
    pdf("1.7", &objs, off)
}

#[test]
fn multimedia_detects_content_settings_sharing_and_presence() {
    let base = mm_doc(b"MEDIA", "XA", true, true, 0);
    let cls = |b: &[u8]| report_of(&base, b, "multimedia").class;
    assert_eq!(cls(&base), SemClass::Pass);
    assert_eq!(
        cls(&mm_doc(b"MEDIA", "XA", true, true, 300)),
        SemClass::Pass,
        "renumbered"
    );
    assert_eq!(
        cls(&mm_doc(b"MEDIA", "XA", true, false, 0)),
        SemClass::Pass,
        "recompressed"
    );
    assert_eq!(
        cls(&mm_doc(b"MEDIb", "XA", true, true, 0)),
        SemClass::Fail,
        "asset content"
    );
    assert_eq!(
        cls(&mm_doc(b"MEDIA", "PO", true, true, 0)),
        SemClass::Fail,
        "activation setting"
    );
    assert_eq!(
        cls(&mm_doc(b"MEDIA", "XA", false, true, 0)),
        SemClass::Fail,
        "sharing"
    );
    let gone = patch(&base, "[4 0 R 5 0 R 6 0 R]", "[4 0 R 6 0 R]");
    assert_eq!(cls(&gone), SemClass::Fail, "removed");
    let l = lines(&base, "multimedia");
    assert_eq!(
        l.iter().filter(|l| l.starts_with("annot\t")).count(),
        3,
        "{l:#?}"
    );
    assert!(
        l.iter().any(|l| l.starts_with("action\towner=annot0.2/A")),
        "{l:#?}"
    );
    // An unsupported filter on an asset: not success.
    let odd = patch(
        &mm_doc(b"MEDIA", "XA", true, false, 0),
        "/Type /EmbeddedFile",
        "/Type /EmbeddedFile /Filter /LZWDecode",
    );
    assert!(matches!(
        section(&odd, "multimedia"),
        Section::Unsupported(_)
    ));
    // Absent everywhere: not applicable.
    let none = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
        ],
        0,
    );
    assert_eq!(
        report_of(&none, &none, "multimedia").class,
        SemClass::NotApplicable
    );
}

// ---------------------------------------------------------------- signatures

#[derive(Clone, Copy, PartialEq)]
enum S {
    Base,
    Removed,     // T2 output: no /Perms, SigFlags 2
    TpChanged,   // TransformParams changed
    ContentsLen, // /Contents length changed
    DssGone,
    ExtraSig,  // a second signature-like dict
    SharedUr3, // the /UR3 dict also a field's /V
    SvChanged,
    VendorSig, // /Contents + unlisted /SubFilter
}

fn sig_doc(v: S, off: u32) -> Vec<u8> {
    let perms = match v {
        S::Removed => "",
        _ => "/Perms << /UR3 20 0 R >>",
    };
    let sigflags = if matches!(v, S::Removed) { 2 } else { 3 };
    let dss = if v == S::DssGone {
        ""
    } else {
        "/DSS << /Certs [40 0 R] /VRI << >> >>"
    };
    let field_v = if v == S::SharedUr3 { "/V 20 0 R" } else { "" };
    let sv = if v == S::SvChanged {
        "/SV << /Ff 2 >>"
    } else {
        "/SV << /Ff 1 >>"
    };
    let annots = match v {
        S::ExtraSig => "/Annots [7 0 R 8 0 R]",
        S::VendorSig => "/Annots [7 0 R 9 0 R]",
        _ => "/Annots [7 0 R]",
    };
    let tp = if v == S::TpChanged {
        "/Form [/Add]"
    } else {
        "/Form [/FillIn]"
    };
    let contents = if v == S::ContentsLen {
        "<0000>"
    } else {
        "<00000000>"
    };
    let objs: Vec<(u32, Body)> = vec![
        (1, d(&format!("<< /Type /Catalog /Pages 2 0 R {perms} /AcroForm << /Fields [7 0 R] /SigFlags {sigflags} >> {dss} /Legal << /JavaScriptActions 0 >> >>"))),
        (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (3, d(&format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] {annots} >>"))),
        (7, d(&format!("<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 1 1] /Lock << /Action /All >> {sv} {field_v} >>"))),
        (8, d("<< /Type /DocTimeStamp /Filter /Adobe.PPKLite /SubFilter /ETSI.RFC3161 /ByteRange [0 1 2 3] /Contents <00> >>")),
        (9, d("<< /SubFilter /VendorCorp.SignV2 /Contents <00> >>")),
        (20, d(&format!("<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents {contents} /Reference [<< /Type /SigRef /TransformMethod /UR3 /TransformParams << /Type /TransformParams {tp} /V /2.2 >> >>] /M (D:2026) >>"))),
        (40, flate("", b"CERT")),
    ];
    pdf("1.7", &objs, off)
}

#[test]
fn signatures_record_structure_without_values() {
    let b = sig_doc(S::Base, 0);
    let l = lines(&b, "signatures");
    for want in [
        "perms\tat=",
        "perms_entry\tkey=/UR3\ttarget=/Root/Perms/UR3",
        "sigflags\tvalue=3",
        "dss\t",
        "legal\t",
    ] {
        assert!(l.iter().any(|x| x.starts_with(want)), "{want}: {l:#?}");
    }
    let sig = l
        .iter()
        .find(|x| x.starts_with("sig\tat=/Root/Perms/UR3\t"))
        .expect("the UR3 dict");
    assert!(
        sig.contains("\trefs=1\t") && sig.contains("contents_bytes=4") && sig.contains("UR3:"),
        "{sig}"
    );
    assert!(
        l.iter().any(|x| x.starts_with("field\tpath=0\tft=Sig")
            && x.contains("lock=")
            && x.contains("sv=")),
        "{l:#?}"
    );
    // No value: neither the /Contents bytes nor the signing time.
    assert!(
        l.iter()
            .all(|x| !x.contains("00000000") && !x.contains("D:2026")),
        "{l:#?}"
    );
}

#[test]
fn signatures_detect_changes_and_allow_storage_differences() {
    let b = sig_doc(S::Base, 0);
    let cls = |x: &[u8]| report_of(&b, x, "signatures").class;
    assert_eq!(cls(&b), SemClass::Pass);
    assert_eq!(cls(&sig_doc(S::Base, 400)), SemClass::Pass, "renumbered");
    for (what, v) in [
        ("transform params", S::TpChanged),
        ("contents length", S::ContentsLen),
        ("dss removed", S::DssGone),
        ("timestamp added", S::ExtraSig),
        ("vendor signature added", S::VendorSig),
        ("sharing", S::SharedUr3),
        ("seed value", S::SvChanged),
        ("usage rights removed (undeclared)", S::Removed),
    ] {
        assert_eq!(cls(&sig_doc(v, 0)), SemClass::Fail, "{what}");
    }
    // The shared /UR3 counts two references.
    let l = lines(&sig_doc(S::SharedUr3, 0), "signatures");
    assert!(
        l.iter()
            .any(|x| x.starts_with("sig\tat=") && x.contains("\trefs=2\t")),
        "{l:#?}"
    );
    // An unreadable object in the walk is recorded, not dropped.
    let broken = pdf(
        "1.7",
        &[
            (
                1,
                d("<< /Type /Catalog /Pages 2 0 R /X 99 0 R /Perms << /UR3 20 0 R >> >>"),
            ),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (20, d("<< /Type /Sig /ByteRange [0 1 2 3] >>")),
            (99, Body::Broken),
        ],
        0,
    );
    assert!(lines(&broken, "signatures")
        .iter()
        .any(|x| x == "walk\tunresolved=1"));
}

// ---------------------------------------------------------------- transformations

fn sec(lines: &[&str]) -> Section {
    if lines.is_empty() {
        Section::Absent
    } else {
        let mut v: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        v.sort();
        Section::Data(v)
    }
}

fn check(category: &str, a: &Section, b: &Section, decls: &[Declaration]) -> SemCheck {
    let mut c = super::pdf20::compare_report_section(category, a, b);
    tclassify(&mut c, a, b, decls);
    c
}

fn t1(from: (u32, u32), to: (u32, u32)) -> Declaration {
    Declaration::VersionRaised {
        from,
        to,
        reason: VersionReason::OptionalContent,
    }
}

#[test]
fn t1_version_raised() {
    let a = sec(&[
        "effective\tversion=1.4",
        "~header\tdeclared=1.4",
        "~catalog\tdeclared=absent",
    ]);
    let b = sec(&[
        "effective\tversion=1.5",
        "~header\tdeclared=1.5",
        "~catalog\tdeclared=absent",
    ]);
    let b16 = sec(&[
        "effective\tversion=1.6",
        "~header\tdeclared=1.6",
        "~catalog\tdeclared=absent",
    ]);
    // 1. the transformation
    let c = check("version", &a, &b, &[t1((1, 4), (1, 5))]);
    assert_eq!(
        (c.class, c.rules.clone()),
        (SemClass::Transformed, vec!["T1 version raised".to_string()])
    );
    // 2. undeclared
    assert_eq!(check("version", &a, &b, &[]).class, SemClass::Fail);
    // 3. declared, nothing happened
    assert_eq!(
        check("version", &a, &a, &[t1((1, 4), (1, 5))]).class,
        SemClass::Fail
    );
    // 4. a change the rule does not cover (raised to 1.6)
    assert_eq!(
        check("version", &a, &b16, &[t1((1, 4), (1, 5))]).class,
        SemClass::Fail
    );
    // 5. near miss: the raise plus one unrelated line
    let near = sec(&["effective\tversion=1.5", "extra\tx=1"]);
    assert_eq!(
        check("version", &a, &near, &[t1((1, 4), (1, 5))]).class,
        SemClass::Fail
    );
    // 6. two unrelated lines more
    let more = sec(&["effective\tversion=1.5", "extra\tx=1", "extra\tx=2"]);
    assert_eq!(
        check("version", &a, &more, &[t1((1, 4), (1, 5))]).class,
        SemClass::Fail
    );
    // 7. invalid declarations: the wrong input version, a target other
    // than 1.5, a "raise" from 1.5
    for decl in [t1((1, 3), (1, 5)), t1((1, 4), (1, 6)), t1((1, 5), (1, 5))] {
        let c = check("version", &a, &b, &[decl]);
        assert_eq!(c.class, SemClass::Fail);
        assert!(
            c.notes.iter().any(|n| n.starts_with("invalid declaration")),
            "{:?}",
            c.notes
        );
    }
    // 8. a section that could not be taken stays what it is
    assert_eq!(
        check(
            "version",
            &Section::Error("x".into()),
            &b,
            &[t1((1, 4), (1, 5))]
        )
        .class,
        SemClass::Error
    );
    // A rule does not touch other categories.
    let pm = sec(&["page\tp=0"]);
    assert_eq!(
        check("page_metadata", &pm, &pm, &[t1((1, 4), (1, 5))]).class,
        SemClass::Pass
    );
}

#[test]
fn t1_on_a_real_rewrite() {
    // Header 1.4, /OCProperties: the rewriter writes 1.5; the runner
    // derives the declaration from the input and the version category is
    // `transformed`; every other category passes.
    let b = pdf(
        "1.4",
        &[
            (
                1,
                d("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R] /D << >> >> >>"),
            ),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
            (5, d("<< /Type /OCG /Name (l) >>")),
        ],
        0,
    );
    let src = doc(&b);
    let out = kasane_pdf::rewrite(&src).unwrap();
    assert!(out.starts_with(b"%PDF-1.5"));
    let decls = transform::derive_default(&src, &b);
    assert_eq!(decls, vec![t1((1, 4), (1, 5))]);
    let r = compare_report_declared(&src, &b, &doc(&out), &out, &decls);
    let v = r.iter().find(|c| c.category == "version").unwrap();
    assert_eq!(v.class, SemClass::Transformed, "{:?}", v.notes);
    let plain = compare_report_declared(&src, &b, &doc(&out), &out, &[]);
    assert_eq!(
        plain
            .iter()
            .find(|c| c.category == "version")
            .unwrap()
            .class,
        SemClass::Fail
    );
    // A 1.5 document with optional content needs no declaration.
    let b15 = String::from_utf8_lossy(&b)
        .replacen("%PDF-1.4", "%PDF-1.5", 1)
        .into_bytes();
    assert!(transform::derive_default(&doc(&b15), &b15).is_empty());
}

fn t2(keys: &[&str], perms_removed: bool, sig_flags: Option<(i64, i64)>) -> Declaration {
    Declaration::UsageRightsRemoved {
        keys: keys.iter().map(|k| k.to_string()).collect(),
        perms_removed,
        sig_flags,
    }
}

#[test]
fn t2_usage_rights_removed() {
    // The original and a hand-written D1 output (no /Perms, /SigFlags 3 → 2).
    let orig_bytes = |extra: &str| {
        pdf(
            "1.7",
            &[
                (1, d(&format!("<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 20 0 R {extra} >> /AcroForm << /Fields [] /SigFlags 3 >> >>"))),
                (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
                (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
                (20, d("<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> /Reference [<< /TransformMethod /UR3 /TransformParams << /Form [/FillIn] >> >>] >>")),
            ],
            0,
        )
    };
    let out_bytes = |perms: &str, sf: i64| {
        pdf(
            "1.7",
            &[
                (1, d(&format!("<< /Type /Catalog /Pages 2 0 R {perms} /AcroForm << /Fields [] /SigFlags {sf} >> >>"))),
                (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
                (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
            ],
            0,
        )
    };
    let orig = orig_bytes("");
    let out = out_bytes("", 2);
    let decl = t2(&["UR3"], true, Some((3, 2)));
    let sig = |a: &[u8], b: &[u8], decls: &[Declaration]| {
        let (sa, sb) = (section(a, "signatures"), section(b, "signatures"));
        check("signatures", &sa, &sb, decls)
    };
    let form = |a: &[u8], b: &[u8], decls: &[Declaration]| {
        let (da, db) = (doc(a), doc(b));
        let r = compare_declared(&da, &db, decls);
        cat_of(&r, "acroform")
    };
    // 1. the transformation: signatures and acroform transformed
    let c = sig(&orig, &out, std::slice::from_ref(&decl));
    assert_eq!(c.class, SemClass::Transformed, "{:?}", c.notes);
    assert_eq!(
        form(&orig, &out, std::slice::from_ref(&decl)).class,
        SemClass::Transformed
    );
    // 2. undeclared
    assert_eq!(sig(&orig, &out, &[]).class, SemClass::Fail);
    assert_eq!(form(&orig, &out, &[]).class, SemClass::Fail);
    // 3. declared, nothing happened
    assert_eq!(
        sig(&orig, &orig, std::slice::from_ref(&decl)).class,
        SemClass::Fail
    );
    // 4. bit 2 dropped too (SigFlags 3 → 0): out of the rule
    assert_eq!(
        sig(&orig, &out_bytes("", 0), std::slice::from_ref(&decl)).class,
        SemClass::Fail
    );
    assert_eq!(
        form(&orig, &out_bytes("", 0), std::slice::from_ref(&decl)).class,
        SemClass::Fail
    );
    // 5. near miss: the removal plus one unrelated signature-like dict left
    let near = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] /SigFlags 2 >> /X 30 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
            (30, d("<< /ByteRange [0 1 2 3] >>")),
        ],
        0,
    );
    assert_eq!(
        sig(&orig, &near, std::slice::from_ref(&decl)).class,
        SemClass::Fail
    );
    // 6. two more: an extra dict and a /DSS
    let more = patch(&near, "/X 30 0 R", "/X 30 0 R /DSS << >>");
    assert_eq!(
        sig(&orig, &more, std::slice::from_ref(&decl)).class,
        SemClass::Fail
    );
    // /Perms kept with another key: declared as kept
    let keep_orig = orig_bytes("/X 1");
    let keep_out = out_bytes("/Perms << /X 1 >>", 2);
    assert_eq!(
        sig(&keep_orig, &keep_out, &[t2(&["UR3"], false, Some((3, 2)))]).class,
        SemClass::Transformed
    );
    assert_eq!(
        sig(&keep_orig, &keep_out, &[t2(&["UR3"], true, Some((3, 2)))]).class,
        SemClass::Fail,
        "perms not empty"
    );
    // 7. invalid declarations
    for bad in [
        t2(&[], true, Some((3, 2))),
        t2(&["DocMDP"], true, Some((3, 2))),
        t2(&["UR"], true, Some((3, 2))),
        t2(&["UR3"], true, Some((2, 2))),
        t2(&["UR3"], true, Some((3, 0))),
        t2(&["UR3"], true, Some((7, 6))),
    ] {
        let c = sig(&orig, &out, std::slice::from_ref(&bad));
        assert_eq!(c.class, SemClass::Fail, "{bad:?}");
        assert!(
            c.notes.iter().any(|n| n.starts_with("invalid declaration")),
            "{bad:?}: {:?}",
            c.notes
        );
    }
    // A shared /UR3 cannot be declared removed.
    let shared = patch(&orig, "/Fields []", "/Fields [] /Y 20 0 R");
    let c = sig(&shared, &out, std::slice::from_ref(&decl));
    assert!(
        c.notes
            .iter()
            .any(|n| n.contains("reachable from elsewhere")),
        "{:?}",
        c.notes
    );
    // 8. a section that could not be taken stays what it is
    assert_eq!(
        check(
            "signatures",
            &Section::Error("limit".into()),
            &sec(&[]),
            std::slice::from_ref(&decl)
        )
        .class,
        SemClass::Error
    );
    // The graph comparison's allowance.
    let notes = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(structure_allows(
        &notes(&[
            "/Root/Perms: dropped by the rewrite",
            "/Root/AcroForm/SigFlags: 3 → 2"
        ]),
        std::slice::from_ref(&decl)
    ));
    assert!(
        !structure_allows(
            &notes(&["/Root/Perms: dropped by the rewrite"]),
            std::slice::from_ref(&decl)
        ),
        "declared SigFlags change missing"
    );
    assert!(
        !structure_allows(
            &notes(&[
                "/Root/Perms: dropped",
                "/Root/AcroForm/SigFlags: x",
                "/Root/Names: dropped"
            ]),
            std::slice::from_ref(&decl)
        ),
        "near miss"
    );
    assert!(structure_allows(&[], &[]));
}

// ---------------------------------------------------------------- inventory

/// (class, kind, owner, position, shared_with, script_bytes)
type Row<'a> = (
    ActionClass,
    &'a str,
    &'a str,
    u32,
    Option<usize>,
    Option<u64>,
);

#[test]
fn the_inventory_lists_actions_by_class_owner_and_chain() {
    let b = page_aa(
        "20 0 R",
        vec![
            (
                20,
                d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
            ),
            (21, d("<< /S /JavaScript /JS 23 0 R >>")),
            (22, d("<< /S /URI /URI (https://example.invalid/) >>")),
            (23, flate("", b"app.alert(12345)")),
            (30, d("<< /Type /DPart >>")),
        ],
    );
    let inv = inventory(&doc(&b)).unwrap();
    let got: Vec<Row> = inv
        .iter()
        .map(|i| {
            (
                i.class,
                i.kind.as_str(),
                i.owner.as_str(),
                i.position,
                i.shared_with,
                i.script_bytes,
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                ActionClass::Navigation,
                "GoToDp",
                "page0/AA/O",
                0,
                None,
                None
            ),
            (
                ActionClass::Script,
                "JavaScript",
                "page0/AA/O",
                1,
                None,
                Some(16)
            ),
            (ActionClass::External, "URI", "page0/AA/O", 2, None, None),
            (
                ActionClass::Navigation,
                "GoToDp",
                "annot0.0/A",
                0,
                Some(0),
                None
            ),
        ]
    );
    // Renumbered: the same inventory.
    let renumbered = {
        let mut objs: Vec<(u32, Body)> = vec![
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /AA << /O 20 0 R >> /Annots [4 0 R] >>")),
            (4, d("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A 20 0 R >>")),
        ];
        objs.extend(vec![
            (
                20,
                d("<< /S /GoToDp /DP 30 0 R /D [3 0 R /Fit] /Next [21 0 R 22 0 R] >>"),
            ),
            (21, d("<< /S /JavaScript /JS 23 0 R >>")),
            (22, d("<< /S /URI /URI (https://example.invalid/) >>")),
            (23, st("", b"app.alert(12345)")),
            (30, d("<< /Type /DPart >>")),
        ]);
        pdf("1.7", &objs, 77)
    };
    assert_eq!(inventory(&doc(&renumbered)).unwrap(), inv);
    // A destination /OpenAction is not an action; an action one is, with
    // its /Next chain.
    let open = |oa: &str| {
        pdf(
            "1.7",
            &[
                (
                    1,
                    d(&format!(
                        "<< /Type /Catalog /Pages 2 0 R /OpenAction {oa} >>"
                    )),
                ),
                (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
                (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>")),
            ],
            0,
        )
    };
    assert!(inventory(&doc(&open("[3 0 R /Fit]"))).unwrap().is_empty());
    let inv = inventory(&doc(&open(
        "<< /S /Named /N /NextPage /Next << /S /JavaScript /JS (x) >> >>",
    )))
    .unwrap();
    let got: Vec<(&str, u32, Option<u64>)> = inv
        .iter()
        .map(|i| (i.kind.as_str(), i.position, i.script_bytes))
        .collect();
    assert_eq!(got, vec![("Named", 0, None), ("JavaScript", 1, Some(1))]);
    assert!(inv.iter().all(|i| i.owner == "catalog/OpenAction"));
}

// ---------------------------------------------------------------- limits

#[test]
fn limits_are_errors() {
    // Node budget: an annotation with 250 000 references.
    let refs = "5 0 R ".repeat(250_000);
    let big = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (
                3,
                d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R] >>"),
            ),
            (
                4,
                d(&format!(
                    "<< /Type /Annot /Subtype /Screen /Rect [0 0 1 1] /X [{refs}] >>"
                )),
            ),
            (5, d("<< /K 1 >>")),
        ],
        0,
    );
    assert!(
        matches!(section(&big, "multimedia"), Section::Error(_)),
        "node budget"
    );
    // Value depth.
    let nested = format!("{}1{}", "[".repeat(40), "]".repeat(40));
    let deep = pdf(
        "1.7",
        &[
            (
                1,
                d(&format!(
                    "<< /Type /Catalog /Pages 2 0 R /Legal << /X {nested} >> >>"
                )),
            ),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
        ],
        0,
    );
    assert!(
        matches!(section(&deep, "signatures"), Section::Error(_)),
        "value depth"
    );
    // Decoded size: an embedded asset inflating past the per-stream limit.
    let zeros = vec![0u8; MAX_DECODED_BYTES + 1];
    let huge = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (
                3,
                d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R] >>"),
            ),
            (
                4,
                d("<< /Type /Annot /Subtype /Sound /Rect [0 0 1 1] /Sound 5 0 R >>"),
            ),
            (5, flate("/Type /Sound /R 8000", &zeros)),
        ],
        0,
    );
    assert!(
        matches!(section(&huge, "multimedia"), Section::Error(_)),
        "decoded size"
    );
}

#[test]
fn each_signature_marker_alone_is_seen() {
    let with = |x: &str| {
        pdf(
            "1.7",
            &[
                (1, d("<< /Type /Catalog /Pages 2 0 R /X 30 0 R >>")),
                (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
                (30, d(x)),
            ],
            0,
        )
    };
    let none = with("<< /Contents <00> >>");
    assert_eq!(
        report_of(&none, &none, "signatures").class,
        SemClass::NotApplicable
    );
    for marker in [
        "<< /ByteRange [0 1 2 3] >>",
        "<< /Type /Sig >>",
        "<< /Type /DocTimeStamp >>",
        "<< /Contents <00> /Filter /Adobe.PPKLite >>",
        "<< /Contents <00> /SubFilter /x.y >>",
        "<< /Reference [<< /TransformMethod /DocMDP >>] >>",
        "<< /Cert <00> /Contents <00> >>",
    ] {
        let b = with(marker);
        let l = lines(&b, "signatures");
        assert!(
            l.iter().any(|x| x.starts_with("sig\tat=/Root/X\t")),
            "{marker}: {l:#?}"
        );
        // Added to a document without it: a difference.
        assert_eq!(
            report_of(&none, &b, "signatures").class,
            SemClass::Fail,
            "{marker}"
        );
    }
}

#[test]
fn an_outline_action_nested_past_the_limit_is_an_error() {
    let nested = format!("{}1{}", "[".repeat(40), "]".repeat(40));
    let b = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (
                10,
                d("<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>"),
            ),
            (
                11,
                d(&format!(
                    "<< /Title (a) /Parent 10 0 R /A << /S /Named /N /X /Y {nested} >> >>"
                )),
            ),
        ],
        0,
    );
    assert_eq!(
        cat_of(&compare(&doc(&b), &doc(&b)), "outlines").class,
        SemClass::Error
    );
}

#[test]
fn unreadable_actions_are_listed_not_dropped() {
    let b =
        |a: &str, extra: Vec<(u32, Body)>| {
            let mut objs =
                vec![
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [4 0 R 6 0 R] >>")),
            (4, d(&format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A {a} >>"))),
        ];
            objs.extend(extra);
            pdf("1.7", &objs, 0)
        };
    // Undefined (null): nothing. Unreadable: an Unknown item, and the
    // unreadable annotation 6 a multimedia line.
    let undefined = b("5 0 R", vec![]);
    assert!(inventory(&doc(&undefined)).unwrap().is_empty());
    let unreadable = b("5 0 R", vec![(5, Body::Broken), (6, Body::Broken)]);
    let inv = inventory(&doc(&unreadable)).unwrap();
    assert_eq!(inv.len(), 1);
    assert_eq!(
        (inv[0].class, inv[0].kind.as_str()),
        (ActionClass::Unknown, "#unresolved")
    );
    let malformed = b("42", vec![]);
    assert_eq!(inventory(&doc(&malformed)).unwrap()[0].kind, "#malformed");
    let l = lines(&unreadable, "multimedia");
    assert!(
        l.contains(&"annot\tat=0.1\tunresolved".to_string()),
        "{l:#?}"
    );
    assert!(
        l.contains(&"action\towner=annot0.0/A\tposition=0\tunresolved".to_string()),
        "{l:#?}"
    );
    assert_eq!(
        report_of(&undefined, &unreadable, "multimedia").class,
        SemClass::Fail
    );
}

#[test]
fn hostile_graphs_stay_bounded() {
    // A 60 000-object chain ending in a signature-like dictionary: the
    // walk keeps one node per value (no path string per object) and
    // still names the signature by its path.
    let n = 60_000u32;
    let mut objs: Vec<(u32, Body)> = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R /X 10 0 R >>")),
        (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
    ];
    for i in 0..n {
        objs.push((10 + i, d(&format!("<< /N {} 0 R >>", 11 + i))));
    }
    objs.push((10 + n, d("<< /ByteRange [0 1 2 3] >>")));
    let b = pdf("1.7", &objs, 0);
    let t = std::time::Instant::now();
    let l = lines(&b, "signatures");
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    let sig = l
        .iter()
        .find(|x| x.starts_with("sig\tat=/Root/X/N/N"))
        .expect("the signature");
    assert!(sig.contains("\trefs=1\t"));
    // A /Next array past the node budget: an error, not a partial list.
    let refs = "5 0 R ".repeat(MAX_NODES + 1);
    let wide = pdf(
        "1.7",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /OpenAction 4 0 R >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (
                4,
                d(&format!("<< /S /Named /N /NextPage /Next [{refs}] >>")),
            ),
            (5, d("<< /S /Named /N /PrevPage >>")),
        ],
        0,
    );
    assert!(inventory(&doc(&wide)).is_err());
    assert!(matches!(section(&wide, "multimedia"), Section::Error(_)));
}
