//! `UsageRightsPolicy::Remove` (v0.5.0 C2, D1): only `/Perms /UR` and
//! `/UR3` usage-rights signatures are removed, `/Perms` is dropped when
//! empty, `/SigFlags` bit 1 is cleared and every other bit kept; every
//! other signature, a field lock, `/DSS`, `/Legal`, sharing and anything
//! undeterminable refuses; the report says what was removed; the
//! default is `rewrite` byte for byte and error for error.

mod common;

use common::build_pdf;
use kasane_pdf::{
    rewrite, rewrite_with, Document, Error, ExecutableContentPolicy as P, Limits, Object,
    RewriteOptions, SignatureRule, SubFilterKind, UsageRight, UsageRightsKey, UsageRightsOutcome,
    UsageRightsPolicy as U,
};

const UR3: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
    /ByteRange [0 10 20 30] /Contents <0123456789abcdef> /M (D:20200101000000Z) \
    /Reference [<< /Type /SigRef /TransformMethod /UR3 /TransformParams << /Type \
    /TransformParams /V /2.2 /Document [/FullSave] /Form [/FillIn /Import] /Annots [/Create] >> \
    >>] >>";

fn o(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// A one-page document: catalog entries `cat`, page entries `page`, and
/// further objects.
fn pdf(cat: &str, page: &str, objs: &[(u32, &str)]) -> Vec<u8> {
    let mut all = vec![
        (1, o(&format!("<< /Type /Catalog /Pages 2 0 R {cat} >>"))),
        (2, o("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            o(&format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page} >>"
            )),
        ),
    ];
    all.extend(objs.iter().map(|(n, b)| (*n, o(b))));
    build_pdf(&all)
}

/// The corpus shape: `/Perms` with only an indirect `/UR3`, an
/// `/AcroForm` with `/SigFlags` `flags`, one text field.
fn ur3_form(flags: i64) -> Vec<u8> {
    pdf(
        &format!("/Perms << /UR3 10 0 R >> /AcroForm << /Fields [20 0 R] /SigFlags {flags} >>"),
        "/Annots [20 0 R]",
        &[
            (10, UR3),
            (
                20,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [0 0 5 5] /P 3 0 R >>",
            ),
        ],
    )
}

fn remove() -> RewriteOptions {
    RewriteOptions::new().with_usage_rights(U::Remove)
}

fn run(b: &[u8], opts: &RewriteOptions) -> Result<kasane_pdf::RewriteOutput, Error> {
    rewrite_with(&Document::from_bytes(b).unwrap(), opts)
}

fn rule(b: &[u8], opts: &RewriteOptions) -> SignatureRule {
    match run(b, opts) {
        Err(Error::SignaturePolicy { rule, .. }) => rule,
        Err(e) => panic!("expected a signature refusal, got {e:?}"),
        Ok(_) => panic!("expected a signature refusal, got a rewrite"),
    }
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

/// The output's catalog, read back.
fn out_catalog(bytes: &[u8]) -> (Document, kasane_pdf::Dict) {
    let d = Document::from_bytes(bytes).unwrap();
    let c = d.catalog().clone();
    (d, c)
}

fn sig_flags_of(bytes: &[u8]) -> Option<i64> {
    let (d, c) = out_catalog(bytes);
    let af = d.deref(c.get(b"AcroForm".as_slice())?).unwrap();
    let f = af.as_dict()?.get(b"SigFlags".as_slice())?.clone();
    d.deref(&f).unwrap().as_i64()
}

#[test]
fn removes_the_corpus_shape_and_reports_it() {
    let b = ur3_form(2);
    let out = run(&b, &remove()).unwrap();
    let r = out.report();
    assert!(r.signatures_removed());
    assert_eq!(r.usage_rights_policy(), U::Remove);
    let UsageRightsOutcome::Removed(rm) = r.usage_rights() else {
        panic!("{:?}", r.usage_rights())
    };
    assert_eq!(rm.entries.len(), 1);
    let e = &rm.entries[0];
    assert_eq!(e.key, UsageRightsKey::Ur3);
    assert_eq!(
        e.input_object.map(|r| (r.number, r.generation)),
        Some((10, 0))
    );
    assert_eq!(e.sub_filter, Some(SubFilterKind::AdbePkcs7Detached));
    assert_eq!(
        e.rights,
        vec![UsageRight::Document, UsageRight::Annots, UsageRight::Form]
    );
    assert!(e.byte_range_present);
    assert!(rm.perms_removed);
    assert_eq!(
        rm.sig_flags.map(|s| (s.before, s.after)),
        Some((2, 2)),
        "bit 1 not set: unchanged, bit 2 kept"
    );
    assert_eq!(rm.input_revisions, 1);
    let c = rm.output_check;
    assert_eq!((c.pages, c.objects_removed), (1, 1));
    assert_eq!(c.objects, c.reachable_objects);
    assert!(c.values_examined > 0);
    // The output: no /Perms, no signature bytes, the field kept.
    let bytes = out.bytes();
    assert!(!contains(bytes, b"/ByteRange") && !contains(bytes, b"/Perms"));
    assert!(!contains(bytes, b"0123456789") && !contains(bytes, b"UR3"));
    assert!(contains(bytes, &hex("name")));
    let (_, cat) = out_catalog(bytes);
    assert_eq!(cat.len(), 3, "/Type /Pages /AcroForm");
    assert_eq!(sig_flags_of(bytes), Some(2));
    // The Display line says so; the error under the default is rewrite's.
    assert!(r
        .to_string()
        .ends_with("; 1 usage-rights signature(s) removed"));
}

#[test]
fn sig_flags_bit_1_is_cleared_and_every_other_bit_kept() {
    for (flags, after) in [(1, 0), (3, 2), (7, 6), (-1, -2), (2, 2), (0, 0)] {
        let out = run(&ur3_form(flags), &remove()).unwrap();
        let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
            panic!()
        };
        assert_eq!(
            rm.sig_flags.map(|c| (c.before, c.after)),
            Some((flags, after)),
            "{flags}"
        );
        assert_eq!(sig_flags_of(out.bytes()), Some(after), "{flags}");
    }
    // Indirect: the integer object, and an indirect /AcroForm.
    for (cat, objs) in [
        (
            "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags 30 0 R >>",
            vec![(10, UR3), (30, "3")],
        ),
        (
            "/Perms << /UR3 10 0 R >> /AcroForm 30 0 R",
            vec![(10, UR3), (30, "<< /Fields [] /SigFlags 3 >>")],
        ),
    ] {
        let out = run(&pdf(cat, "", &objs), &remove()).unwrap();
        assert_eq!(sig_flags_of(out.bytes()), Some(2), "{cat}");
    }
    // No /SigFlags: none reported, none written.
    let b = pdf(
        "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] >>",
        "",
        &[(10, UR3)],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert_eq!(rm.sig_flags, None);
    assert_eq!(sig_flags_of(out.bytes()), None);
}

#[test]
fn perms_shapes() {
    // /UR and /UR3 together; direct dictionaries; /Perms indirect.
    let ur = UR3.replace("/TransformMethod /UR3", "/TransformMethod /UR");
    let b = pdf(
        "/Perms 30 0 R",
        "",
        &[(30, &format!("<< /UR {ur} /UR3 {UR3} >>"))],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    let keys: Vec<_> = rm.entries.iter().map(|e| (e.key, e.input_object)).collect();
    assert_eq!(
        keys,
        vec![(UsageRightsKey::Ur, None), (UsageRightsKey::Ur3, None)]
    );
    assert!(rm.perms_removed);
    assert_eq!(rm.output_check.objects_removed, 1, "the /Perms object");
    assert!(!contains(out.bytes(), b"/Perms"));
    // Another /Perms entry stays (and /Perms with it).
    let b = pdf("/Perms << /UR3 10 0 R /X 5 >>", "", &[(10, UR3)]);
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert!(!rm.perms_removed);
    let (d, cat) = out_catalog(out.bytes());
    let p = d.deref(cat.get(b"Perms".as_slice()).unwrap()).unwrap();
    let p = p.as_dict().unwrap();
    assert_eq!(
        (p.len(), p.get(b"X".as_slice())),
        (1, Some(&Object::Integer(5)))
    );
    // ... in an indirect /Perms, edited in place.
    let b = pdf(
        "/Perms 30 0 R",
        "",
        &[(10, UR3), (30, "<< /UR3 10 0 R /X 5 >>")],
    );
    let out = run(&b, &remove()).unwrap();
    assert!(contains(out.bytes(), b"/X 5") && !contains(out.bytes(), b"/UR3"));
    // What only the signature holds goes with it; nothing else does.
    let held = UR3.replace(
        "/TransformParams << /Type /TransformParams /V /2.2 /Document [/FullSave] /Form [/FillIn \
         /Import] /Annots [/Create] >>",
        "/TransformParams 11 0 R",
    );
    let b = pdf(
        "/Perms << /UR3 10 0 R >> /Keep 12 0 R",
        "",
        &[
            (10, &held),
            (11, "<< /Form [/FillIn] >>"),
            (12, "<< /K 1 >>"),
        ],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert_eq!(rm.output_check.objects_removed, 2);
    assert_eq!(rm.entries[0].rights, vec![UsageRight::Form]);
    assert!(contains(out.bytes(), b"/K 1"));
}

/// The corpus shape as Acrobat writes it: the signature reference's
/// `/Data` points at the catalog, `/Perms` is an object, the
/// `/TransformParams` held only by the signature. The catalog stays (it
/// is reached from the trailer); what only the signature holds goes.
#[test]
fn a_reference_data_entry_pointing_at_the_catalog() {
    let ur3 = UR3
        .replace("/Type /SigRef", "/Type /SigRef /Data 1 0 R")
        .replace(
            "/TransformParams << /Type /TransformParams /V /2.2 /Document [/FullSave] /Form \
             [/FillIn /Import] /Annots [/Create] >>",
            "/TransformParams 11 0 R",
        );
    assert!(ur3.contains("/Data 1 0 R") && ur3.contains("11 0 R"));
    let b = pdf(
        "/Perms 30 0 R /AcroForm << /Fields [20 0 R] /SigFlags 2 >>",
        "/Annots [20 0 R]",
        &[
            (10, &ur3),
            (11, "<< /Type /TransformParams /V /2.2 /Form [/FillIn] >>"),
            (
                20,
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [0 0 5 5] /P 3 0 R >>",
            ),
            (30, "<< /UR3 10 0 R >>"),
        ],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert_eq!(
        rm.output_check.objects_removed, 3,
        "/UR3, its /TransformParams, /Perms"
    );
    assert_eq!(rm.output_check.pages, 1);
    assert!(!contains(out.bytes(), b"/ByteRange") && !contains(out.bytes(), b"/Perms"));
    // A held object shared with another structure stays (it is not a
    // signature: the marker walk refuses any other).
    let b = pdf(
        "/Perms << /UR3 10 0 R >> /Keep 11 0 R",
        "",
        &[
            (10, &ur3),
            (11, "<< /Type /TransformParams /V /2.2 /Form [/FillIn] >>"),
        ],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert_eq!(rm.output_check.objects_removed, 1, "only the signature");
    let (d, cat) = out_catalog(out.bytes());
    let keep = d.deref(cat.get(b"Keep".as_slice()).unwrap()).unwrap();
    assert!(keep.as_dict().unwrap().contains_key(b"Form".as_slice()));
}

/// A stream's indirect `/Length` is not written (the rewrite writes
/// the length directly); the check expects exactly that.
#[test]
fn an_indirect_stream_length() {
    let b = pdf(
        "/Perms << /UR3 10 0 R >>",
        "/Contents 13 0 R",
        &[
            (10, UR3),
            (13, "<< /Length 14 0 R >>\nstream\nq Q\nendstream"),
            (14, "3"),
        ],
    );
    let out = run(&b, &remove()).unwrap();
    assert!(out.report().signatures_removed());
}

/// Review 2: documents without any signature are `NonePresent` and the
/// plain rewrite — a dangling or null `/Perms` (P2-1), a graph larger
/// than the audit's own element limit (P2-2), an `/AcroForm` read
/// through a chain when nothing is edited (P3-1).
#[test]
fn review_2_no_false_refusals() {
    let cases = [
        pdf("/Perms 99 0 R", "", &[]),
        pdf("/Perms 5 0 R", "", &[(5, "null")]),
        pdf(
            "/AcroForm 5 0 R",
            "",
            &[(5, "30 0 R"), (30, "<< /Fields [] /SigFlags 2 >>")],
        ),
    ];
    for b in &cases {
        let d = Document::from_bytes(b).unwrap();
        let out = rewrite_with(&d, &remove()).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            out.report().usage_rights(),
            &UsageRightsOutcome::NonePresent
        );
        assert_eq!(out.bytes(), rewrite(&d).unwrap());
    }
    // More values than max_audit_elements, within the walk budget.
    let b = pdf(&format!("/X [{}]", "[] ".repeat(3_000)), "", &[]);
    let small = remove().with_limits(Limits::default().with_max_audit_elements(1_000));
    let out = run(&b, &small).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(
        out.report().usage_rights(),
        &UsageRightsOutcome::NonePresent
    );
    // With a usage-rights signature too, and /SigFlags behind a chain
    // that need not change.
    let b = pdf(
        &format!(
            "/Perms << /UR3 10 0 R >> /AcroForm 5 0 R /X [{}]",
            "[] ".repeat(3_000)
        ),
        "",
        &[
            (10, UR3),
            (5, "30 0 R"),
            (30, "<< /Fields [] /SigFlags 2 >>"),
        ],
    );
    assert!(run(&b, &small).unwrap().report().signatures_removed());
    // ... but one that must change still refuses.
    let b = pdf(
        "/Perms << /UR3 10 0 R >> /AcroForm 5 0 R",
        "",
        &[
            (10, UR3),
            (5, "30 0 R"),
            (30, "<< /Fields [] /SigFlags 3 >>"),
        ],
    );
    assert_eq!(rule(&b, &remove()), SignatureRule::UsageRightsNotRemovable);
}

/// An incremental update after the signature: the input's revisions are
/// counted, the output has one.
#[test]
fn input_revisions_are_reported() {
    let mut b = ur3_form(2);
    let prev = {
        let s = String::from_utf8_lossy(&b);
        let i = s.rfind("startxref\n").unwrap() + 10;
        s[i..]
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let off = b.len();
    b.extend_from_slice(b"20 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (filled) /Rect [0 0 5 5] /P 3 0 R >>\nendobj\n");
    let x = b.len();
    b.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n20 1\n{off:010} 00000 n \ntrailer\n<< /Size 21 \
             /Root 1 0 R /Prev {prev} >>\nstartxref\n{x}\n%%EOF\n"
        )
        .as_bytes(),
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert_eq!(rm.input_revisions, 2);
    assert!(contains(out.bytes(), &hex("filled")));
}

#[test]
fn the_default_is_rewrite_and_never_removes() {
    for b in [ur3_form(3), pdf("", "", &[])] {
        let d = Document::from_bytes(&b).unwrap();
        let plain = rewrite(&d);
        let with = rewrite_with(&d, &RewriteOptions::default());
        match (plain, with) {
            (Ok(a), Ok(w)) => {
                assert_eq!(a, w.bytes());
                assert_eq!(w.report().usage_rights(), &UsageRightsOutcome::NotRequested);
                assert!(!w.report().signatures_removed());
            }
            (Err(a), Err(w)) => {
                assert_eq!(format!("{a:?}"), format!("{w:?}"));
                assert!(matches!(
                    w,
                    Error::SignaturePolicy {
                        rule: SignatureRule::RewriteOfSignedDocument,
                        ..
                    }
                ));
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(RewriteOptions::default().usage_rights, U::Refuse);
}

/// Remove on a document without any signature: nothing removed, the
/// bytes are `rewrite`'s.
#[test]
fn none_present_is_the_plain_rewrite() {
    let b = pdf(
        "/AcroForm << /Fields [10 0 R] /SigFlags 2 >>",
        "",
        &[(10, "<< /FT /Tx /T (a) >>")],
    );
    let d = Document::from_bytes(&b).unwrap();
    let out = rewrite_with(&d, &remove()).unwrap();
    assert_eq!(
        out.report().usage_rights(),
        &UsageRightsOutcome::NonePresent
    );
    assert!(!out.report().signatures_removed());
    assert_eq!(out.bytes(), rewrite(&d).unwrap());
    // An unsigned signature field with a seed value is carried.
    let b = pdf(
        "/AcroForm << /Fields [10 0 R] /SigFlags 0 >>",
        "",
        &[(
            10,
            "<< /FT /Sig /T (s) /SV << /Type /SV /Filter /Adobe.PPKLite >> >>",
        )],
    );
    let out = run(&b, &remove()).unwrap();
    assert_eq!(
        out.report().usage_rights(),
        &UsageRightsOutcome::NonePresent
    );
}

#[test]
fn every_other_signature_refuses() {
    use SignatureRule::{
        OtherSignaturePresent as Other, SecurityStore, UsageRightsNotRemovable as NotRemovable,
    };
    let docmdp = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> \
                  /Reference [<< /TransformMethod /DocMDP /TransformParams << /P 2 >> >>] >>";
    let approval = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>";
    let cases: Vec<(&str, Vec<u8>, SignatureRule)> = vec![
        (
            "DocMDP next to UR3",
            pdf("/Perms << /UR3 10 0 R /DocMDP 11 0 R >>", "", &[(10, UR3), (11, docmdp)]),
            Other,
        ),
        (
            "DocMDP alone",
            pdf("/Perms << /DocMDP 11 0 R >>", "", &[(11, docmdp)]),
            Other,
        ),
        (
            "an approval signature",
            pdf(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [20 0 R] /SigFlags 3 >>",
                "/Annots [20 0 R]",
                &[(10, UR3), (20, "<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /V 21 0 R /Rect [0 0 1 1] >>"), (21, approval)],
            ),
            Other,
        ),
        (
            "a signature on a widget outside /Fields",
            pdf(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] >>",
                "/Annots [20 0 R]",
                &[(10, UR3), (20, "<< /Type /Annot /Subtype /Widget /FT /Sig /V 21 0 R /Rect [0 0 1 1] >>"), (21, approval)],
            ),
            Other,
        ),
        (
            "a document timestamp",
            pdf(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [20 0 R] >>",
                "",
                &[(10, UR3), (20, "<< /FT /Sig /T (t) /V << /Type /DocTimeStamp /SubFilter /ETSI.RFC3161 >> >>")],
            ),
            Other,
        ),
        (
            "a signature without /FT /Sig",
            pdf("/Perms << /UR3 10 0 R >> /X << /Y << /Contents <00> /Filter /F >> >>", "", &[(10, UR3)]),
            Other,
        ),
        (
            "/DSS",
            pdf("/Perms << /UR3 10 0 R >> /DSS << /Certs [] >>", "", &[(10, UR3)]),
            SecurityStore,
        ),
        (
            "/Legal",
            pdf("/Perms << /UR3 10 0 R >> /Legal << /JavaScriptActions 1 >>", "", &[(10, UR3)]),
            SecurityStore,
        ),
        (
            "a shared signature dictionary",
            pdf(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [20 0 R] >>",
                "",
                &[(10, UR3), (20, "<< /FT /Tx /T (x) /V 10 0 R >>")],
            ),
            NotRemovable,
        ),
        (
            "a field /Lock",
            pdf(
                "/Perms << /UR3 10 0 R >> /AcroForm << /Fields [20 0 R] >>",
                "",
                &[(10, UR3), (20, "<< /FT /Sig /T (x) /Lock << /Action /All >> >>")],
            ),
            Other,
        ),
        (
            "an empty /Reference",
            pdf("/Perms << /UR3 10 0 R >>", "", &[(10, "<< /Type /Sig /Filter /F /Contents <00> /Reference [] >>")]),
            NotRemovable,
        ),
        (
            "no /TransformMethod",
            pdf("/Perms << /UR3 10 0 R >>", "", &[(10, &UR3.replace("/TransformMethod /UR3", ""))]),
            NotRemovable,
        ),
        (
            "an unreadable object",
            pdf("/Perms << /UR3 10 0 R >> /X 11 0 R", "", &[(10, UR3), (11, "<< /Unterminated")]),
            NotRemovable,
        ),
    ];
    for (what, b, want) in &cases {
        assert_eq!(rule(b, &remove()), *want, "{what}");
        // Under every executable-content policy.
        for p in [P::Carry, P::RefuseScriptsAndExternal] {
            assert_eq!(
                rule(b, &remove().with_executable_content(p)),
                *want,
                "{what} {p:?}"
            );
        }
    }
    // XFA XML-DSig: refused by the rewrite's own check.
    let xfa = "<xdp><ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"/></xdp>";
    let b = build_pdf(&[
        (1, o("<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /XFA 11 0 R >> >>")),
        (2, o("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (3, o("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>")),
        (10, o(UR3)),
        (11, common::stream_obj("", xfa.as_bytes())),
    ]);
    assert!(matches!(
        run(&b, &remove()),
        Err(Error::SignaturePolicy { .. })
    ));
    // The walk budget.
    let tiny = remove().with_limits(Limits::default().with_max_output_objects(1));
    let many = pdf(
        &format!("/Perms << /UR3 10 0 R >> /X [{}]", "1 ".repeat(100)),
        "",
        &[(10, UR3)],
    );
    assert_eq!(rule(&many, &tiny), NotRemovable);
}

/// The executable-content policy applies under Remove as without it.
#[test]
fn policy_combinations() {
    let script = "/AA << /WC << /S /JavaScript /JS (app.alert(1)) >> >>";
    let b = pdf(
        &format!("/Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags 3 >> {script}"),
        "",
        &[(10, UR3)],
    );
    // Remove (Profile): the catalog /AA refuses, as by `rewrite`.
    assert!(matches!(run(&b, &remove()), Err(Error::Unsupported { .. })));
    // Carry + Remove: removed, the script carried and reported.
    let out = run(&b, &remove().with_executable_content(P::Carry)).unwrap();
    assert!(out.report().signatures_removed());
    assert_eq!(out.report().executable_summary().script, 1);
    assert!(contains(out.bytes(), &hex("app.alert(1)")) && !contains(out.bytes(), b"/ByteRange"));
    // RefuseScriptsAndExternal + Remove: a page script refuses although
    // the removal is possible.
    let b = pdf(
        "/Perms << /UR3 10 0 R >>",
        "/AA << /O << /S /JavaScript /JS (x) >> >>",
        &[(10, UR3)],
    );
    let strict = remove().with_executable_content(P::RefuseScriptsAndExternal);
    assert!(run(&b, &strict).is_err());
    assert!(run(&b, &remove()).unwrap().report().signatures_removed());
    // Carry alone still refuses the usage-rights signature.
    assert!(matches!(
        run(&b, &RewriteOptions::new().with_executable_content(P::Carry)),
        Err(Error::SignaturePolicy {
            rule: SignatureRule::RewriteOfSignedDocument,
            ..
        })
    ));
}

/// A file with an xref stream: plain objects `plain` and, in object
/// stream 40, the compressed objects `packed`.
fn objstm_pdf(plain: &[(u32, &str)], packed: &[(u32, &str)]) -> Vec<u8> {
    let (mut head, mut body) = (String::new(), String::new());
    for (n, s) in packed {
        head.push_str(&format!("{n} {} ", body.len()));
        body.push_str(s);
        body.push(' ');
    }
    let os = format!("{head}{body}");
    let mut all: Vec<(u32, String)> = plain.iter().map(|(n, s)| (*n, s.to_string())).collect();
    all.push((
        40,
        format!(
            "<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{os}\nendstream",
            packed.len(),
            head.len(),
            os.len()
        ),
    ));
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
    for (n, s) in &all {
        rows.push((*n, 1, b.len() as u64, 0));
        b.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
    }
    for (i, (n, _)) in packed.iter().enumerate() {
        rows.push((*n, 2, 40, i as u64));
    }
    let x = b.len() as u64;
    rows.push((41, 1, x, 0));
    rows.sort();
    let (mut idx, mut data) = (String::new(), Vec::new());
    for (n, t, f2, f3) in &rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(*t);
        data.extend_from_slice(&(*f2 as u32).to_be_bytes());
        data.extend_from_slice(&(*f3 as u16).to_be_bytes());
    }
    b.extend_from_slice(
        format!(
            "41 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 42 /Root 1 0 R /Length {} \
             >>\nstream\n",
            data.len()
        )
        .as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

const TREE: [(u32, &str); 2] = [
    (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
    (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
];

/// Targets, `/Perms` and `/AcroForm` inside an object stream.
#[test]
fn compressed_objects() {
    let mut plain = vec![(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Perms 30 0 R /AcroForm 31 0 R >>",
    )];
    plain.extend(TREE);
    let b = objstm_pdf(
        &plain,
        &[
            (10, UR3),
            (30, "<< /UR3 10 0 R >>"),
            (31, "<< /Fields [] /SigFlags 3 >>"),
        ],
    );
    let out = run(&b, &remove()).unwrap();
    let UsageRightsOutcome::Removed(rm) = out.report().usage_rights() else {
        panic!()
    };
    assert!(rm.perms_removed);
    assert_eq!(rm.output_check.objects_removed, 2, "/UR3 and /Perms");
    assert_eq!(sig_flags_of(out.bytes()), Some(2));
    assert!(!contains(out.bytes(), b"/ByteRange"));
}

/// Object-stream members that are references (aliases) are written
/// link by link; the output check reaches every link (fuzz
/// `signature_markers`: the audit walk resolved a written chain in one
/// step and called its links unreachable).
#[test]
fn compressed_alias_chains_are_carried_and_checked() {
    for cat in [
        "<< /Type /Catalog /Pages 2 0 R /C 103 0 R >>",
        "<< /Type /Catalog /Pages 2 0 R /C 103 0 R /Perms << /UR3 10 0 R >> >>",
    ] {
        let mut plain = vec![(1, cat)];
        plain.extend(TREE);
        let b = objstm_pdf(
            &plain,
            &[
                (10, UR3),
                (100, "<< /K 1 >>"),
                (103, "104 0 R"),
                (104, "100 0 R"),
            ],
        );
        let out = run(&b, &remove()).unwrap();
        assert_eq!(out.report().signatures_removed(), cat.contains("/Perms"));
        let d = Document::from_bytes(out.bytes()).unwrap();
        let k = d.deref(d.catalog().get(b"C".as_slice()).unwrap()).unwrap();
        assert_eq!(d.deref(&k).unwrap().as_dict().unwrap().len(), 1);
    }
}

/// Nothing is left on the document: the warnings are the plain
/// rewrite's (the assessment and the output check record none).
#[test]
fn no_side_effects_on_the_document() {
    let b = pdf("/X 99 0 R", "", &[]);
    let (a, c) = (
        Document::from_bytes(&b).unwrap(),
        Document::from_bytes(&b).unwrap(),
    );
    let out = rewrite_with(&a, &remove()).unwrap();
    assert_eq!(
        out.report().usage_rights(),
        &UsageRightsOutcome::NonePresent
    );
    rewrite(&c).unwrap();
    assert_eq!(a.warnings(), c.warnings());
    assert!(!a.warnings().is_empty());
}
