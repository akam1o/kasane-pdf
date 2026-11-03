//! `rewrite_with` (v0.5.0 C1, D2): the default is `rewrite` byte for byte
//! and error for error; `Carry` carries the document-level scripts;
//! `RefuseScriptsAndExternal` refuses scripts and external actions
//! wherever they are; multimedia and signature refusals hold under every
//! policy; the report lists every carried item, content-free, with input
//! and output objects.

mod common;

use common::{build_pdf, fixture, stream_obj};
use kasane_pdf::{
    rewrite, rewrite_with, rewrite_with_limits, ActionClass, ActionKind, ActionOwner, Document,
    Error, ExecutableContentPolicy as P, Limits, Object, RefusalReason, RewriteOptions, ScriptSize,
    UnclassifiedReason,
};

/// A one-page document: catalog entries `cat`, page entries `page`, and
/// further objects from 10 on.
fn pdf(cat: &str, page: &str, objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut all = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>").into_bytes(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page} >>").into_bytes(),
        ),
    ];
    all.extend(objs.iter().cloned());
    build_pdf(&all)
}

fn o(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn flate(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn opts(p: P) -> RewriteOptions {
    RewriteOptions::new().with_executable_content(p)
}

fn run(b: &[u8], p: P) -> Result<kasane_pdf::RewriteOutput, Error> {
    rewrite_with(&Document::from_bytes(b).unwrap(), &opts(p))
}

fn reason(b: &[u8], p: P) -> Option<RefusalReason> {
    run(b, p).err().and_then(|e| e.refusal_reason())
}

/// Every action owner the classification knows, each with a script or an
/// external action, a `/Next` chain, a shared action and a stream script.
fn all_owners() -> Vec<u8> {
    pdf(
        "/OpenAction << /S /GoTo /D [3 0 R /Fit] /Next 20 0 R >> /AA << /WC 21 0 R >> \
         /AcroForm << /Fields [30 0 R] >> /Outlines 40 0 R /Names << /JavaScript 50 0 R >>",
        "/AA << /O 22 0 R >> /Annots [31 0 R 32 0 R]",
        &[
            (20, o("<< /S /JavaScript /JS (app.alert('secret-script')) >>")),
            (21, o("<< /S /JavaScript /JS 23 0 R >>")),
            (22, o("<< /S /Named /N /NextPage /Next [24 0 R 20 0 R] >>")),
            (23, stream_obj("/Filter /FlateDecode", &flate(b"this.print()"))),
            (24, o("<< /S /URI /URI (https://example.invalid/private) >>")),
            (30, o("<< /T (secret-field) /Kids [31 0 R 33 0 R] /AA << /K 25 0 R >> >>")),
            (31, o("<< /Type /Annot /Subtype /Widget /Parent 30 0 R /Rect [0 0 1 1] /AA << /U 26 0 R >> >>")),
            (32, o("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /A << /S /Launch /F (x.exe) >> >>")),
            (33, o("<< /T (k) /Parent 30 0 R /AA << /F 27 0 R >> >>")),
            (25, o("<< /S /JavaScript /JS (AFNumber_Keystroke()) >>")),
            (26, o("<< /S /SubmitForm /F (https://example.invalid/submit) >>")),
            (27, o("<< /S /ResetForm >>")),
            (40, o("<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>")),
            (41, o("<< /Title (Chapter) /Parent 40 0 R /A << /S /GoToDp /DP 42 0 R /Next << /S /GoTo /D (n) /SD [43 0 R /Fit] >> >> >>")),
            (42, o("<< /Type /DPart >>")),
            (43, o("<< /Type /StructElem /S /P >>")),
            (50, o("<< /Names [(secret-key) 51 0 R] >>")),
            (51, o("<< /S /JavaScript /JS (secret-tree-script) >>")),
        ],
    )
}

// ------------------------------------------------------------- the default

/// The documents the default is compared on: every fixture, and
/// synthetic documents for each refusal and each carried structure.
fn default_cases() -> Vec<(String, Vec<u8>)> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(fixture("")).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "pdf") {
            v.push((p.display().to_string(), std::fs::read(&p).unwrap()));
        }
    }
    for (name, b) in [
        ("all owners", all_owners()),
        (
            "catalog AA",
            pdf("/AA << /WC << /S /GoTo /D [3 0 R /Fit] >> >>", "", &[]),
        ),
        (
            "JS OpenAction",
            pdf("/OpenAction << /S /JavaScript /JS (x) >>", "", &[]),
        ),
        (
            "names JS",
            pdf(
                "/Names << /JavaScript << /Names [(a) << /S /JavaScript /JS (x) >>] >> >>",
                "",
                &[],
            ),
        ),
        ("RichMedia", pdf("/RichMedia << >>", "", &[])),
        (
            "UR3",
            pdf(
                "/Perms << /UR3 10 0 R >>",
                "",
                &[(10, o("<< /Type /Sig /ByteRange [0 1 2 3] >>"))],
            ),
        ),
        (
            "page AA",
            pdf("", "/AA << /O << /S /JavaScript /JS (x) >> >>", &[]),
        ),
        (
            "outline cycle",
            pdf(
                "/Outlines 10 0 R",
                "",
                &[
                    (10, o("<< /First 11 0 R >>")),
                    (
                        11,
                        o("<< /Title (a) /Next 11 0 R /A << /S /Named /N /NextPage >> >>"),
                    ),
                ],
            ),
        ),
        ("2.0", {
            let mut b = pdf("", "", &[]);
            b.splice(5..8, *b"2.0");
            b
        }),
    ] {
        v.push((name.to_string(), b));
    }
    v
}

#[test]
fn the_default_is_rewrite_byte_for_byte_and_error_for_error() {
    let mut compared = (0, 0);
    for (name, b) in default_cases() {
        let Ok(doc) = Document::from_bytes(&b) else {
            continue;
        };
        let old = rewrite(&doc);
        let new = rewrite_with(&doc, &RewriteOptions::default());
        match (old, new) {
            (Ok(a), Ok(out)) => {
                assert_eq!(a, out.bytes(), "{name}: bytes");
                compared.0 += 1;
            }
            (Err(a), Err(b)) => {
                assert_eq!(
                    (a.kind(), a.to_string()),
                    (b.kind(), b.to_string()),
                    "{name}: error"
                );
                compared.1 += 1;
            }
            (a, b) => panic!(
                "{name}: rewrite {:?} vs rewrite_with {:?}",
                a.err(),
                b.err()
            ),
        }
    }
    assert!(compared.0 >= 20 && compared.1 >= 5, "{compared:?}");
}

#[test]
fn limits_are_those_of_rewrite_with_limits() {
    let b = pdf("", "/AA << /O << /S /JavaScript /JS (x) >> >>", &[]);
    let doc = Document::from_bytes(&b).unwrap();
    let mut tight = Limits::default();
    tight.max_output_objects = 2;
    let a = rewrite_with_limits(&doc, &tight).unwrap_err();
    let e = rewrite_with(&doc, &RewriteOptions::new().with_limits(tight.clone())).unwrap_err();
    assert_eq!(a.to_string(), e.to_string());
    assert!(matches!(
        a,
        Error::LimitExceeded {
            limit: "max_output_objects",
            ..
        }
    ));
    // Carry, on a document the default refuses: the same limit.
    let all = Document::from_bytes(&all_owners()).unwrap();
    let e = rewrite_with(&all, &opts(P::Carry).with_limits(tight.clone())).unwrap_err();
    assert_eq!(a.to_string(), e.to_string());
    assert_eq!(
        rewrite_with_limits(&all, &tight).unwrap_err().to_string(),
        rewrite_with(&all, &RewriteOptions::new().with_limits(tight))
            .unwrap_err()
            .to_string()
    );
    let mut invalid = Limits::default();
    invalid.max_output_objects = 0;
    assert!(matches!(
        rewrite_with(&doc, &RewriteOptions::new().with_limits(invalid)),
        Err(Error::InvalidArgument { .. })
    ));
}

#[test]
fn the_output_carries_bytes_and_report() {
    let b = pdf("", "/AA << /O << /S /JavaScript /JS (x) >> >>", &[]);
    let out = run(&b, P::Profile).unwrap();
    let bytes = out.bytes().to_vec();
    assert_eq!(out.report().executable_content().len(), 1);
    assert_eq!(out.report().executable_content_policy(), P::Profile);
    let (b2, report) = out.into_parts();
    assert_eq!(bytes, b2);
    assert_eq!(report.executable_summary().script, 1);
    assert_eq!(run(&b, P::Profile).unwrap().into_bytes(), bytes);
    assert_eq!(report.version().declared, (1, 5));
    assert_eq!(report.version().reason, None);
    assert!(report
        .to_string()
        .starts_with("1 executable item(s): 1 script"));
}

// ------------------------------------------------------------- the report

#[test]
fn every_owner_is_reported_in_order_with_its_chain() {
    let b = all_owners();
    // Profile refuses this document (document-level scripts); Carry
    // carries it and reports everything.
    assert_eq!(
        reason(&b, P::Profile),
        Some(RefusalReason::DocumentLevelScript)
    );
    let out = run(&b, P::Carry).unwrap();
    let got: Vec<(String, u32, &str, ActionClass)> = out
        .report()
        .executable_content()
        .iter()
        .map(|i| {
            (
                i.owner.to_string(),
                i.chain_position,
                i.kind.as_str(),
                i.class,
            )
        })
        .collect();
    let want: Vec<(&str, u32, &str, ActionClass)> = vec![
        ("catalog/OpenAction", 0, "GoTo", ActionClass::Navigation),
        ("catalog/OpenAction", 1, "JavaScript", ActionClass::Script),
        ("catalog/AA/WC", 0, "JavaScript", ActionClass::Script),
        ("page0/AA/O", 0, "Named", ActionClass::Navigation),
        ("page0/AA/O", 1, "URI", ActionClass::External),
        ("page0/AA/O", 2, "JavaScript", ActionClass::Script), // shared with the OpenAction's
        ("annot0.0/AA/U", 0, "SubmitForm", ActionClass::External),
        ("annot0.1/A", 0, "Launch", ActionClass::External),
        ("field0/AA/K", 0, "JavaScript", ActionClass::Script),
        ("field0.1/AA/F", 0, "ResetForm", ActionClass::State),
        ("outline0/A", 0, "GoToDp", ActionClass::Navigation),
        ("outline0/A", 1, "GoTo", ActionClass::Navigation),
        ("names/JavaScript#0", 0, "JavaScript", ActionClass::Script),
    ];
    let want: Vec<(String, u32, &str, ActionClass)> = want
        .into_iter()
        .map(|(a, b, c, d)| (a.to_string(), b, c, d))
        .collect();
    assert_eq!(got, want);
    let items = out.report().executable_content();
    // Sharing: the page's third step is the OpenAction's second.
    assert_eq!(items[5].shared_with, Some(1));
    assert!(items
        .iter()
        .enumerate()
        .all(|(i, it)| i == 5 || it.shared_with.is_none()));
    // Script sizes: string and decoded stream.
    assert_eq!(items[1].script, Some(ScriptSize::Bytes(26)));
    assert_eq!(items[2].script, Some(ScriptSize::Bytes(12)));
    assert_eq!(items[4].script, None);
    // Input → output objects: the output object holds the same action.
    let out_doc = Document::from_bytes(out.bytes()).unwrap();
    for it in items {
        if let (Some(i), Some(o)) = (it.input_object, it.output_object) {
            let a = Document::from_bytes(&b).unwrap().resolve(i).unwrap();
            let b2 = out_doc.resolve(o).unwrap();
            let s = |x: &Object| x.as_dict().and_then(|d| d.get(b"S".as_slice()).cloned());
            assert_eq!(s(&a), s(&b2), "{it:?}");
        } else {
            assert!(
                it.input_object.is_none() && it.output_object.is_none(),
                "{it:?}"
            );
        }
    }
    assert!(items[2].input_object.is_some_and(|r| r.number == 21));
    // Nothing of the document's content in the report.
    let dump = format!("{:?} {}", out.report(), out.report());
    for secret in [
        "secret",
        "example.invalid",
        "AFNumber",
        "this.print",
        "x.exe",
        "Chapter",
    ] {
        assert!(!dump.contains(secret), "{secret} in the report");
    }
}

#[test]
fn profile_carries_and_reports_what_rewrite_carries() {
    // Below the document level: carried by `rewrite` and Profile alike,
    // now reported.
    let b = pdf(
        "",
        "/AA << /C << /S /JavaScript /JS (x) >> >> /Annots [10 0 R]",
        &[(
            10,
            o("<< /Type /Annot /Subtype /Screen /Rect [0 0 1 1] /A << /S /Rendition /OP 0 >> >>"),
        )],
    );
    let out = run(&b, P::Profile).unwrap();
    let kinds: Vec<(&str, String)> = out
        .report()
        .executable_content()
        .iter()
        .map(|i| (i.kind.as_str(), i.owner.to_string()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("JavaScript", "page0/AA/C".to_string()),
            ("Screen", "annot0.0".to_string()),
            ("Rendition", "annot0.0/A".to_string()),
        ]
    );
    assert_eq!(out.report().executable_summary().multimedia, 2);
    // A non-script OpenAction whose /Next is a script: carried by
    // `rewrite`, reported here.
    let b = pdf(
        "/OpenAction << /S /GoTo /D [3 0 R /Fit] /Next << /S /JavaScript /JS (x) >> >>",
        "",
        &[],
    );
    assert_eq!(
        run(&b, P::Profile)
            .unwrap()
            .report()
            .executable_summary()
            .script,
        1
    );
    assert!(rewrite(&Document::from_bytes(&b).unwrap()).is_ok());
}

#[test]
fn classification_by_effect() {
    let one = |a: &str| {
        let b = pdf("", &format!("/AA << /O {a} >>"), &[]);
        let it = run(&b, P::Profile).unwrap().report().executable_content()[0].clone();
        (it.class, it.kind)
    };
    use ActionClass as C;
    use ActionKind as K;
    assert_eq!(
        one("<< /S /Named /N /PrevPage >>"),
        (C::Navigation, K::Named)
    );
    assert_eq!(one("<< /S /Named /N /Print >>"), (C::State, K::Named));
    assert_eq!(
        one("<< /S /GoToDp /DP 3 0 R >>"),
        (C::Navigation, K::GoToDp)
    );
    assert_eq!(
        one("<< /S /GoTo /SD [3 0 R /Fit] >>"),
        (C::Navigation, K::GoTo)
    );
    assert_eq!(
        one("<< /S /SetOCGState /State [] >>"),
        (C::State, K::SetOcgState)
    );
    assert_eq!(
        one("<< /S /ImportData /F (f) >>"),
        (C::External, K::ImportData)
    );
    assert_eq!(
        one("<< /S /GoToE /T << /R /C >> >>"),
        (C::External, K::GoToE)
    );
    assert_eq!(
        one("<< /S /RichMediaExecute >>"),
        (C::Multimedia, K::RichMediaExecute)
    );
    assert_eq!(one("<< /S /VendorMagic >>"), (C::Unknown, K::Other));
    assert_eq!(
        one("<< /Next << /S /GoTo /D [3 0 R /Fit] >> >>"),
        (C::Unknown, K::Missing)
    );
    assert_eq!(one("42"), (C::Unknown, K::Malformed));
    // An indirect /S is read through its reference.
    let b = pdf(
        "",
        "/AA << /O << /S 20 0 R /JS (x) >> >>",
        &[(20, o("/JavaScript"))],
    );
    let it = run(&b, P::Profile).unwrap().report().executable_content()[0].clone();
    assert_eq!((it.class, it.kind), (C::Script, K::JavaScript));
}

#[test]
fn references_cycles_and_unreadable_structure() {
    // An undefined action reference is null: nothing to report.
    let b = pdf("", "/AA << /O 99 0 R >>", &[]);
    assert!(run(&b, P::Carry)
        .unwrap()
        .report()
        .executable_content()
        .is_empty());
    // A /Next cycle ends the chain (each action listed once).
    let b = pdf(
        "",
        "/AA << /O 10 0 R >>",
        &[
            (10, o("<< /S /GoTo /D [3 0 R /Fit] /Next 11 0 R >>")),
            (11, o("<< /S /GoTo /D [3 0 R /Fit] /Next 10 0 R >>")),
        ],
    );
    let r = run(&b, P::RefuseScriptsAndExternal).unwrap();
    assert_eq!(r.report().executable_content().len(), 2);
    assert!(r.report().unclassified().is_empty());
    // An outline cycle cannot be classified to the end: Profile carries
    // it (as `rewrite` does) and says so; Carry and the strict policy
    // refuse it.
    let b = pdf(
        "/Outlines 10 0 R",
        "",
        &[
            (10, o("<< /First 11 0 R >>")),
            (
                11,
                o("<< /Title (a) /Next 11 0 R /A << /S /Named /N /NextPage >> >>"),
            ),
        ],
    );
    let out = run(&b, P::Profile).unwrap();
    let u = out.report().unclassified();
    assert_eq!(
        (u.len(), u[0].structure, u[0].reason),
        (1, "outline tree", UnclassifiedReason::Cycle)
    );
    assert_eq!(
        out.report().executable_content().len(),
        1,
        "the item before the cycle"
    );
    assert_eq!(
        reason(&b, P::Carry),
        Some(RefusalReason::UnclassifiedStructure)
    );
    assert_eq!(
        reason(&b, P::RefuseScriptsAndExternal),
        Some(RefusalReason::UnclassifiedStructure)
    );
    // An undecodable script stream: carried byte for byte, its size
    // reported as unknown.
    let b = pdf(
        "",
        "/AA << /O << /S /JavaScript /JS 10 0 R >> >>",
        &[(10, stream_obj("/Filter /FlateDecode", b"not zlib"))],
    );
    let out = run(&b, P::Carry).unwrap();
    assert_eq!(
        out.report().executable_content()[0].script,
        Some(ScriptSize::Undecodable)
    );
    assert!(out.bytes().windows(8).any(|w| w == b"not zlib"));
}

// ------------------------------------------------------------- policies

#[test]
fn profile_refuses_exactly_what_rewrite_refuses() {
    for (b, why) in [
        (
            pdf("/AA << /WC << /S /GoTo /D [3 0 R /Fit] >> >>", "", &[]),
            RefusalReason::DocumentLevelScript,
        ),
        (
            pdf("/OpenAction << /S /JavaScript /JS (x) >>", "", &[]),
            RefusalReason::DocumentLevelScript,
        ),
        (
            pdf("/Names << /JavaScript << /Names [] >> >>", "", &[]),
            RefusalReason::DocumentLevelScript,
        ),
        (pdf("/RichMedia << >>", "", &[]), RefusalReason::Multimedia),
    ] {
        assert_eq!(reason(&b, P::Profile), Some(why));
        assert_eq!(
            rewrite(&Document::from_bytes(&b).unwrap())
                .unwrap_err()
                .refusal_reason(),
            Some(why)
        );
    }
}

#[test]
fn carry_carries_document_level_scripts_verbatim_and_nothing_else_changes() {
    for b in [
        pdf(
            "/AA << /WC << /S /JavaScript /JS (doc.close) >> >>",
            "",
            &[],
        ),
        pdf("/OpenAction << /S /JavaScript /JS (doc.close) >>", "", &[]),
        pdf(
            "/Names << /JavaScript << /Names [(k) << /S /JavaScript /JS (doc.close) >>] >> >>",
            "",
            &[],
        ),
    ] {
        let out = run(&b, P::Carry).unwrap();
        // The script string is carried (strings are written in hex).
        let hex: String = b"doc.close".iter().map(|c| format!("{c:02X}")).collect();
        assert!(
            out.bytes().windows(hex.len()).any(|w| w == hex.as_bytes()),
            "script carried"
        );
        assert_eq!(out.report().executable_summary().script, 1);
        // The output reparses with the same catalog entries.
        let (a, c) = (
            Document::from_bytes(&b).unwrap(),
            Document::from_bytes(out.bytes()).unwrap(),
        );
        let keys = |d: &Document| d.catalog().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&a), keys(&c));
    }
    // Multimedia and signatures stay refused; unknown actions too.
    assert_eq!(
        reason(&pdf("/RichMedia << >>", "", &[]), P::Carry),
        Some(RefusalReason::Multimedia)
    );
    let ur3 = pdf(
        "/Perms << /UR3 10 0 R >> /AA << /WC << /S /JavaScript /JS (x) >> >>",
        "",
        &[(10, o("<< /Type /Sig >>"))],
    );
    for p in [P::Profile, P::Carry, P::RefuseScriptsAndExternal] {
        assert_eq!(reason(&ur3, p), Some(RefusalReason::Signature), "{p:?}");
    }
    let unknown = pdf("", "/AA << /O << /S /VendorMagic >> >>", &[]);
    assert_eq!(
        reason(&unknown, P::Carry),
        Some(RefusalReason::UnknownAction)
    );
    assert!(
        run(&unknown, P::Profile).is_ok(),
        "Profile carries it, as rewrite does"
    );
}

#[test]
fn the_strict_policy_refuses_scripts_and_external_actions_anywhere() {
    let strict = |cat: &str, page: &str, objs: &[(u32, Vec<u8>)]| {
        reason(&pdf(cat, page, objs), P::RefuseScriptsAndExternal)
    };
    let js = "<< /S /JavaScript /JS (x) >>";
    let uri = "<< /S /URI /URI (u) >>";
    let s = Some(RefusalReason::Script);
    let e = Some(RefusalReason::ExternalAction);
    let annot =
        |a: &str| format!("/Annots [<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] {a} >>]");
    assert_eq!(strict("", &format!("/AA << /O {js} >>"), &[]), s, "page");
    assert_eq!(
        strict("", &annot(&format!("/A {js}")), &[]),
        s,
        "annotation /A"
    );
    assert_eq!(
        strict("", &annot(&format!("/AA << /U {uri} >>")), &[]),
        e,
        "annotation /AA"
    );
    assert_eq!(
        strict(
            "/AcroForm << /Fields [10 0 R] >>",
            "",
            &[(10, format!("<< /T (f) /AA << /K {js} >> >>").into_bytes())]
        ),
        s,
        "field"
    );
    assert_eq!(
        strict(
            "/Outlines 10 0 R",
            "",
            &[
                (10, o("<< /First 11 0 R >>")),
                (11, format!("<< /Title (t) /A {uri} >>").into_bytes())
            ]
        ),
        e,
        "outline"
    );
    // Deep in a /Next chain, behind navigation.
    let mut chain = js.to_string();
    for _ in 0..20 {
        chain =
            format!("<< /S /GoTo /D [3 0 R /Fit] /Next [<< /S /Named /N /NextPage >> {chain}] >>");
    }
    assert_eq!(
        strict("", &format!("/AA << /O {chain} >>"), &[]),
        s,
        "/Next chain"
    );
    // Under a non-script OpenAction (Profile carries it).
    assert_eq!(
        strict(
            &format!("/OpenAction << /S /GoTo /D [3 0 R /Fit] /Next {uri} >>"),
            "",
            &[]
        ),
        e
    );
    // The document-level entry points stay refused as under Profile.
    assert_eq!(
        strict(&format!("/OpenAction {js}"), "", &[]),
        Some(RefusalReason::DocumentLevelScript)
    );
    // Unknown, missing /S and malformed values.
    assert_eq!(
        strict("", "/AA << /O << /S /VendorMagic >> >>", &[]),
        Some(RefusalReason::UnknownAction)
    );
    assert_eq!(
        strict("", "/AA << /O << >> >>", &[]),
        Some(RefusalReason::UnknownAction)
    );
    assert_eq!(
        strict("", "/AA << /O 42 >>", &[]),
        Some(RefusalReason::UnknownAction)
    );
    // Navigation, state and multimedia are carried.
    let ok = pdf(
        "",
        "/AA << /O << /S /GoTo /D [3 0 R /Fit] /Next [<< /S /Hide /T (a) >> << /S /Sound /Sound 10 0 R >>] >> >>",
        &[(10, stream_obj("/R 8000", b"\0"))],
    );
    let out = run(&ok, P::RefuseScriptsAndExternal).unwrap();
    assert_eq!(out.report().executable_content().len(), 3);
}

fn xfa_doc(packets: &[&[u8]]) -> Vec<u8> {
    let mut arr = String::new();
    let mut objs = Vec::new();
    for (i, p) in packets.iter().enumerate() {
        let n = 10 + i as u32;
        arr.push_str(&format!("(p{i}) {n} 0 R "));
        objs.push((n, stream_obj("", p)));
    }
    pdf(
        &format!("/AcroForm << /Fields [] /XFA [{arr}] >>"),
        "",
        &objs,
    )
}

#[test]
fn xfa_scripts_are_detected_under_any_prefix_and_never_kept() {
    let count = |b: &[u8]| -> Vec<(String, u32)> {
        run(b, P::Carry)
            .unwrap()
            .report()
            .executable_content()
            .iter()
            .map(|i| (i.owner.to_string(), i.chain_position))
            .collect()
    };
    let b = xfa_doc(&[
        b"<template><field><event><script>secretScript()</script></event>\
          <x:script contentType='application/x-formcalc'>1</x:script><scripts/>\
          <scriptx/></field></template>",
        b"<a>&#60;script>over-report</a>",
    ]);
    assert_eq!(
        count(&b),
        vec![
            ("xfa#0".to_string(), 0),
            ("xfa#0".to_string(), 1),
            ("xfa#1".to_string(), 0)
        ]
    );
    // UTF-16 packets.
    let wide: Vec<u8> = "<script/>"
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let b = xfa_doc(&[&[&[0xFF, 0xFE][..], &wide].concat()]);
    assert_eq!(count(&b).len(), 1);
    // Split across a packet boundary.
    let b = xfa_doc(&[b"<a><scr", b"ipt>x</script></a>"]);
    assert_eq!(count(&b), vec![("xfa/across".to_string(), 0)]);
    // Refused by the strict policy; carried and reported by Profile.
    let b = xfa_doc(&[b"<script>x</script>"]);
    assert_eq!(
        reason(&b, P::RefuseScriptsAndExternal),
        Some(RefusalReason::Script)
    );
    let out = run(&b, P::Profile).unwrap();
    assert_eq!(
        out.report().executable_content()[0].kind,
        ActionKind::XfaScript
    );
    assert!(matches!(
        out.report().executable_content()[0].owner,
        ActionOwner::Xfa { packet: Some(0) }
    ));
    assert!(!format!("{:?}", out.report()).contains("secretScript"));
}

#[test]
fn every_policy_keeps_the_signature_refusals() {
    let signed_field = pdf(
        "/AcroForm << /Fields [10 0 R] /SigFlags 3 >>",
        "/Annots [10 0 R]",
        &[
            (
                10,
                o("<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /V 11 0 R >>"),
            ),
            (11, o("<< /Type /Sig /ByteRange [0 1 2 3] >>")),
        ],
    );
    let xml_sig = xfa_doc(&[b"<ds:Signature xmlns:ds='http://www.w3.org/2000/09/xmldsig#'/>"]);
    for b in [signed_field, xml_sig] {
        for p in [P::Profile, P::Carry, P::RefuseScriptsAndExternal] {
            assert_eq!(reason(&b, p), Some(RefusalReason::Signature), "{p:?}");
        }
    }
}

/// Fuzz finding: `/OpenAction [<script action> <undefined>]` was read as
/// a destination (not every member an action), so the strict policy
/// carried the script. An array whose first element is an action is a
/// list of actions; and the strict policy refuses a script or external
/// action reachable anywhere, also outside the positions it classifies.
#[test]
fn no_route_around_the_strict_policy() {
    let js = "<< /S /JavaScript /JS (x) >>";
    for cat in [
        format!("/OpenAction [{js} 999 0 R]"),
        format!("/OpenAction [{js} /Fit]"),
        // Out of any action position: a destination's parameter, an
        // unknown key, a structure the classifier does not walk.
        format!("/OpenAction [3 0 R {js}]"),
        format!("/Extra {js}"),
        "/Extra << /S /GoTo /D [3 0 R /Fit] /JS (x) >>".to_string(),
        "/Extra [<< /S /Launch /F (x) >>]".to_string(),
        // Review P2-1: an indirect /S.
        "/Extra << /S 20 0 R /URI (u) >>".to_string(),
    ] {
        let b = pdf(&cat, "", &[(20, o("/URI"))]);
        let why = reason(&b, P::RefuseScriptsAndExternal);
        assert!(
            matches!(
                why,
                Some(
                    RefusalReason::Script
                        | RefusalReason::UnclassifiedStructure
                        | RefusalReason::ExternalAction
                )
            ),
            "{cat}: {why:?}"
        );
        assert!(
            run(&b, P::Profile).is_ok(),
            "{cat}: Profile carries it, as rewrite does"
        );
    }
    // The array of actions is reported member by member.
    let b = pdf(&format!("/OpenAction [{js} 999 0 R]"), "", &[]);
    let out = run(&b, P::Carry).unwrap();
    let owners: Vec<String> = out
        .report()
        .executable_content()
        .iter()
        .map(|i| i.owner.to_string())
        .collect();
    assert_eq!(owners, vec!["catalog/OpenAction#0".to_string()]);
}

/// Corpus finding: the strict policy's search for stray actions followed
/// a stream's `/Length` to an unreadable object the rewrite never writes
/// (it writes `/Length` directly), and refused. The search follows what
/// the rewrite follows.
#[test]
fn the_strict_search_skips_a_streams_length_like_the_rewrite() {
    let b = pdf(
        "/Metadata 10 0 R",
        "",
        &[
            (
                10,
                b"<< /Type /Metadata /Length 11 0 R >>\nstream\nabc\nendstream".to_vec(),
            ),
            (11, b"(not a number".to_vec()),
        ],
    );
    let doc = Document::from_bytes(&b).unwrap();
    assert!(rewrite(&doc).is_ok(), "the rewrite does not read /Length");
    let out = run(&b, P::RefuseScriptsAndExternal);
    assert!(out.is_ok(), "{:?}", out.err());
}

/// Review P2-6/P2-7: the report leaves nothing on the document that
/// `rewrite` would not have left — no warnings, no consumed budget.
#[test]
fn the_report_leaves_no_trace_on_the_document() {
    // An undefined /Next and an undefined page /AA trigger: the report
    // resolves them (null), `rewrite` records them as it writes them.
    let b = pdf(
        "",
        "/AA << /O << /S /GoTo /D [3 0 R /Fit] /Next 99 0 R >> /C 98 0 R >>",
        &[],
    );
    let (a, c) = (
        Document::from_bytes(&b).unwrap(),
        Document::from_bytes(&b).unwrap(),
    );
    rewrite(&a).unwrap();
    for p in [P::Profile, P::Carry, P::RefuseScriptsAndExternal] {
        let _ = rewrite_with(&c, &opts(p));
        assert_eq!(a.warnings(), c.warnings(), "{p:?}");
        let _ = c.take_warnings();
        rewrite(&c).unwrap();
        let _ = c.take_warnings();
    }
    // The budget: a document opened with just enough references for
    // `rewrite` still has them after `rewrite_with`.
    for n in 1..200u64 {
        let mut l = Limits::default();
        l.max_reference_depth = n;
        let Ok(d) = Document::from_bytes_with_limits(&b, l) else {
            continue;
        };
        if rewrite(&d).is_err() {
            continue;
        }
        let d = Document::from_bytes_with_limits(&b, {
            let mut l = Limits::default();
            l.max_reference_depth = n;
            l
        })
        .unwrap();
        let first = rewrite_with(&d, &RewriteOptions::default()).map(|o| o.into_bytes());
        assert!(first.is_ok(), "budget {n}: {:?}", first.err());
        break;
    }
}
