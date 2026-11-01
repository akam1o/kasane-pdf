//! Synthetic documents for the document-level categories (XFA, actions,
//! JavaScript, metadata, viewer preferences): one base document carrying
//! all five, and variants that store the same meaning differently (must
//! pass) or change, drop, rename or reorder exactly one thing (must fail
//! in exactly that category).

use super::doclevel::{ascii85, asciihex};
use super::*;

const DOC_LEVEL: [&str; 5] = [
    "xfa",
    "doc_actions",
    "javascript",
    "metadata",
    "viewer_prefs",
];

enum Body {
    Dict(String),
    Stream(String, Vec<u8>),
}

fn d(s: &str) -> Body {
    Body::Dict(s.into())
}

fn st(dict: &str, data: &[u8]) -> Body {
    Body::Stream(dict.into(), data.to_vec())
}

fn flate(dict: &str, data: &[u8]) -> Body {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    Body::Stream(format!("{dict} /Filter /FlateDecode"), z)
}

/// `n 0 R` → `n+off 0 R` (dictionary text only; stream data untouched).
fn shift_refs(s: &str, off: u32) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let starts = b[i].is_ascii_digit()
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.'));
        if starts {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if s[i..].starts_with(" 0 R")
                && !s[i + 4..].starts_with(|c: char| c.is_ascii_alphanumeric())
            {
                let n: u32 = s[st..i].parse().unwrap();
                out.push_str(&format!("{} 0 R", n + off));
                i += 4;
            } else {
                out.push_str(&s[st..i]);
            }
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// Serialise; object 1 is the catalog; `off` renumbers every object.
fn pdf(objs: &[(u32, Body)], trailer: &str, off: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objs {
        let n = n + off;
        offs.push((n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        match body {
            Body::Dict(s) => out.extend_from_slice(shift_refs(s, off).as_bytes()),
            Body::Stream(dict, data) => {
                out.extend_from_slice(
                    format!(
                        "<< {} /Length {} >>\nstream\n",
                        shift_refs(dict, off),
                        data.len()
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(data);
                out.extend_from_slice(b"\nendstream");
            }
        }
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = offs.iter().map(|o| o.0).max().unwrap() + 1;
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offs.iter().find(|o| o.0 == n) {
            Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root {} 0 R {} >>\nstartxref\n{x}\n%%EOF\n",
            1 + off,
            shift_refs(trailer, off)
        )
        .as_bytes(),
    );
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum V {
    None,
    KeysReordered,
    // xfa
    XfaCompressed,
    XfaPacketChanged,
    XfaPacketGone,
    XfaPacketRenamed,
    XfaPacketsSwapped,
    XfaDuplicate,
    XfaAsStream,
    XfaGone,
    NeedsRenderingChanged,
    // doc_actions
    OpenActionTarget,
    OpenActionNextGone,
    NextBranchesSwapped,
    CatalogScriptChanged,
    CatalogScriptCompressed,
    ScriptAsStream,
    PageAaGone,
    LinkTargetChanged,
    FieldAaChanged,
    // javascript
    JsTreeSplit,
    JsScriptChanged,
    JsEntryGone,
    JsNameChanged,
    JsNextChanged,
    // metadata
    InfoUtf16,
    InfoTitleChanged,
    InfoGone,
    XmpCompressed,
    XmpChanged,
    XmpGone,
    XmpEmpty,
    // viewer_prefs
    PrefsIndirect,
    PrefChanged,
    PrefExtensionGone,
    PageModeChanged,
}

fn base(v: V) -> Vec<u8> {
    base_off(v, 0)
}

fn base_off(v: V, off: u32) -> Vec<u8> {
    use V::*;
    let prefs = match v {
        PrefsIndirect => "60 0 R".to_string(),
        PrefChanged => "<< /HideToolbar false /Direction /L2R /XVendorExt (keep) >>".into(),
        PrefExtensionGone => "<< /HideToolbar true /Direction /L2R >>".into(),
        _ => "<< /HideToolbar true /Direction /L2R /XVendorExt (keep) >>".into(),
    };
    let mode = if v == PageModeChanged {
        "/UseThumbs"
    } else {
        "/UseOutlines"
    };
    let nr = if v == NeedsRenderingChanged {
        "true"
    } else {
        "false"
    };
    let meta = if v == XmpGone { "" } else { "/Metadata 40 0 R" };
    let catalog = if v == KeysReordered {
        format!(
            "<< /PageLayout /TwoColumnLeft {meta} /ViewerPreferences {prefs} /PageMode {mode} \
             /Names << /JavaScript 30 0 R >> /AA << /WC 22 0 R >> /OpenAction 20 0 R \
             /NeedsRendering {nr} /AcroForm 10 0 R /Pages 2 0 R /Type /Catalog >>"
        )
    } else {
        format!(
            "<< /Type /Catalog /Pages 2 0 R /AcroForm 10 0 R /NeedsRendering {nr} \
             /OpenAction 20 0 R /AA << /WC 22 0 R >> /Names << /JavaScript 30 0 R >> \
             {meta} /ViewerPreferences {prefs} /PageMode {mode} /PageLayout /TwoColumnLeft >>"
        )
    };
    let page_aa = if v == PageAaGone {
        ""
    } else {
        "/AA << /O 23 0 R >>"
    };
    let mut o: Vec<(u32, Body)> = vec![
        (1, Body::Dict(catalog)),
        (2, d("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")),
        (
            3,
            Body::Dict(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page_aa} /Annots [50 0 R] >>"
            )),
        ),
        (
            4,
            d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
        ),
    ];
    // XFA: preamble, template, datasets, postamble.
    let mut packets: Vec<(&str, &[u8])> = vec![
        ("preamble", b"<xdp:xdp>"),
        ("template", b"<template><field name=\"f\"/></template>"),
        ("datasets", b"<xfa:datasets><f>1</f></xfa:datasets>"),
        ("postamble", b"</xdp:xdp>"),
    ];
    match v {
        XfaPacketChanged => packets[2].1 = b"<xfa:datasets><f>2</f></xfa:datasets>",
        XfaPacketGone => {
            packets.remove(2);
        }
        XfaPacketRenamed => packets[2].0 = "data",
        XfaPacketsSwapped => packets.swap(1, 2),
        XfaDuplicate => packets.insert(3, ("datasets", b"<xfa:datasets><f>1</f></xfa:datasets>")),
        _ => {}
    }
    let xfa = match v {
        XfaGone => String::new(),
        XfaAsStream => "/XFA 11 0 R".into(),
        _ => format!(
            "/XFA [{}]",
            (0..packets.len())
                .map(|i| format!("({}) {} 0 R", packets[i].0, 11 + i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    o.push((
        10,
        Body::Dict(format!("<< /Fields [51 0 R] /DA (/Helv 0 Tf 0 g) {xfa} >>")),
    ));
    if v == XfaAsStream {
        let all: Vec<u8> = packets.iter().flat_map(|p| p.1.to_vec()).collect();
        o.push((11, st("", &all)));
    } else {
        for (i, (_, data)) in packets.iter().enumerate() {
            let body = if v == XfaCompressed {
                flate("", data)
            } else {
                st("", data)
            };
            o.push((11 + i as u32, body));
        }
    }
    // Actions.
    let oa_target = if v == OpenActionTarget {
        "[3 0 R /Fit]"
    } else {
        "[4 0 R /Fit]"
    };
    let oa_next = match v {
        OpenActionNextGone => "",
        NextBranchesSwapped => "/Next [26 0 R 21 0 R]",
        _ => "/Next [21 0 R 26 0 R]",
    };
    o.push((
        20,
        Body::Dict(format!("<< /S /GoTo /D {oa_target} {oa_next} >>")),
    ));
    if v == ScriptAsStream {
        // /JS as a text stream with the same text.
        o.push((21, d("<< /S /JavaScript /JS 27 0 R >>")));
        o.push((27, st("", b"app.alert(1)")));
    } else {
        o.push((21, d("<< /S /JavaScript /JS (app.alert\\(1\\)) >>")));
    }
    o.push((
        22,
        if v == KeysReordered {
            d("<< /JS 24 0 R /S /JavaScript >>")
        } else {
            d("<< /S /JavaScript /JS 24 0 R >>")
        },
    ));
    o.push((23, d("<< /S /URI /URI (https://example.org/) >>")));
    let print: &[u8] = if v == CatalogScriptChanged {
        b"this.print(false);"
    } else {
        b"this.print();"
    };
    o.push((
        24,
        if v == CatalogScriptCompressed {
            flate("", print)
        } else {
            st("", print)
        },
    ));
    let fk = if v == FieldAaChanged {
        "AFNumber_Keystroke(3)"
    } else {
        "AFNumber_Keystroke(2)"
    };
    o.push((
        25,
        Body::Dict(format!(
            "<< /S /JavaScript /JS ({}) >>",
            fk.replace('(', "\\(").replace(')', "\\)")
        )),
    ));
    o.push((26, d("<< /S /Named /N /NextPage >>")));
    // JavaScript name tree.
    let b2 = if v == JsNextChanged {
        "b3\\(\\)"
    } else {
        "b2\\(\\)"
    };
    let a_name = if v == JsNameChanged { "a1" } else { "a" };
    let a_js = if v == JsScriptChanged {
        "function a\\(\\){return 1}"
    } else {
        "function a\\(\\){}"
    };
    match v {
        JsTreeSplit => {
            o.push((30, d("<< /Kids [34 0 R 35 0 R] >>")));
            o.push((34, d("<< /Limits [(a) (a)] /Names [(a) 31 0 R] >>")));
            o.push((35, d("<< /Limits [(b) (b)] /Names [(b) 32 0 R] >>")));
        }
        JsEntryGone => o.push((30, d("<< /Names [(a) 31 0 R] >>"))),
        _ => o.push((
            30,
            Body::Dict(format!("<< /Names [({a_name}) 31 0 R (b) 32 0 R] >>")),
        )),
    }
    o.push((31, Body::Dict(format!("<< /S /JavaScript /JS ({a_js}) >>"))));
    o.push((
        32,
        d("<< /S /JavaScript /JS (function b\\(\\){}) /Next 33 0 R >>"),
    ));
    o.push((33, Body::Dict(format!("<< /S /JavaScript /JS ({b2}) >>"))));
    // Metadata.
    let xmp: &[u8] = match v {
        XmpChanged => b"<x:xmpmeta><dc:title>Other</dc:title></x:xmpmeta>",
        XmpEmpty => b"",
        _ => b"<x:xmpmeta><dc:title>Report</dc:title></x:xmpmeta>",
    };
    o.push((
        40,
        if v == XmpCompressed {
            flate("/Type /Metadata /Subtype /XML", xmp)
        } else {
            st("/Type /Metadata /Subtype /XML", xmp)
        },
    ));
    let title = match v {
        // "Report" in UTF-16BE with a BOM: the same text.
        InfoUtf16 => "<FEFF005200650070006F00720074>",
        InfoTitleChanged => "(Draft)",
        _ => "(Report)",
    };
    o.push((
        41,
        Body::Dict(format!(
            "<< /Title {title} /Author (A. Person) /CreationDate (D:20260101) >>"
        )),
    ));
    // A link on page 1 and a field with keystroke actions.
    let link = if v == LinkTargetChanged {
        "[3 0 R /Fit]"
    } else {
        "[4 0 R /XYZ 0 10 0]"
    };
    o.push((
        50,
        Body::Dict(format!(
            "<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] /A << /S /GoTo /D {link} >> >>"
        )),
    ));
    o.push((
        51,
        d("<< /T (f) /FT /Tx /Kids [52 0 R] /AA << /K 25 0 R >> >>"),
    ));
    o.push((
        52,
        d("<< /Parent 51 0 R /Subtype /Widget /Rect [0 0 5 5] >>"),
    ));
    o.push((
        60,
        d("<< /HideToolbar true /Direction /L2R /XVendorExt (keep) >>"),
    ));
    let trailer = if v == InfoGone { "" } else { "/Info 41 0 R" };
    pdf(&o, trailer, off)
}

fn doc(b: &[u8]) -> Document {
    Document::from_bytes(b).unwrap()
}

fn class_of(r: &SemanticRoundTrip, cat: &str) -> SemClass {
    r.checks.iter().find(|c| c.category == cat).unwrap().class
}

fn section(d: &Document, cat: &str) -> Section {
    let i = CATEGORIES.iter().position(|c| *c == cat).unwrap();
    snapshot(d).swap_remove(i)
}

fn lines(d: &Document, cat: &str) -> Vec<String> {
    match section(d, cat) {
        Section::Data(v) => v,
        other => panic!("{cat}: {other:?}"),
    }
}

/// `v` against the base: `cat` (if any) fails, every other category
/// keeps its class against the base compared with itself.
fn expect(v: V, cat: Option<&str>) {
    let b = doc(&base(V::None));
    let same = compare(&b, &b);
    let r = compare(&b, &doc(&base(v)));
    for (c, s) in r.checks.iter().zip(&same.checks) {
        let want = if Some(c.category.as_str()) == cat {
            SemClass::Fail
        } else {
            s.class
        };
        assert_eq!(
            c.class, want,
            "{v:?}: {} was {:?} ({:?})",
            c.category, c.class, c.notes
        );
    }
    let want = if cat.is_some() {
        SemClass::Fail
    } else {
        SemClass::Pass
    };
    assert_eq!(r.class, want, "{v:?}");
}

#[test]
fn base_document_covers_the_document_level_categories() {
    let b = doc(&base(V::None));
    for c in DOC_LEVEL {
        assert!(!lines(&b, c).is_empty(), "{c}");
    }
    let r = compare(&b, &b);
    for c in DOC_LEVEL.iter().chain(&["acroform"]) {
        assert_eq!(class_of(&r, c), SemClass::Pass, "{c}: {:?}", r.checks);
    }
    assert_eq!(r.class, SemClass::Pass);
    assert_eq!(snapshot(&b), snapshot(&doc(&base(V::None))));
}

#[test]
fn storage_differences_pass() {
    expect(V::None, None);
    for v in [
        V::KeysReordered,
        V::XfaCompressed,
        V::CatalogScriptCompressed,
        V::ScriptAsStream,
        V::JsTreeSplit,
        V::InfoUtf16,
        V::XmpCompressed,
        V::PrefsIndirect,
    ] {
        expect(v, None);
    }
    // Every object renumbered.
    let r = compare(&doc(&base(V::None)), &doc(&base_off(V::None, 100)));
    assert_eq!(r.class, SemClass::Pass, "{:?}", r.checks);
}

#[test]
fn xfa_changes_fail() {
    for v in [
        V::XfaPacketChanged,
        V::XfaPacketGone,
        V::XfaPacketRenamed,
        V::XfaPacketsSwapped,
        V::XfaDuplicate,
        V::XfaAsStream,
        V::XfaGone,
        V::NeedsRenderingChanged,
    ] {
        expect(v, Some("xfa"));
    }
    // A duplicate packet is kept as such: equal to itself, and losing
    // either copy is a difference.
    let dup = doc(&base(V::XfaDuplicate));
    assert_eq!(class_of(&compare(&dup, &dup), "xfa"), SemClass::Pass);
    let l = lines(&dup, "xfa");
    assert_eq!(
        l.iter().filter(|x| x.contains("name=datasets")).count(),
        2,
        "{l:?}"
    );
}

#[test]
fn xfa_fail_is_not_hidden_by_an_acroform_pass() {
    let r = compare(&doc(&base(V::None)), &doc(&base(V::XfaPacketChanged)));
    assert_eq!(class_of(&r, "acroform"), SemClass::Pass);
    assert_eq!(class_of(&r, "xfa"), SemClass::Fail);
    assert_eq!(r.class, SemClass::Fail);
}

#[test]
fn action_changes_fail() {
    for v in [
        V::OpenActionTarget,
        V::OpenActionNextGone,
        V::NextBranchesSwapped,
        V::CatalogScriptChanged,
        V::PageAaGone,
        V::LinkTargetChanged,
        V::FieldAaChanged,
    ] {
        expect(v, Some("doc_actions"));
    }
}

#[test]
fn javascript_changes_fail() {
    for v in [
        V::JsScriptChanged,
        V::JsEntryGone,
        V::JsNameChanged,
        V::JsNextChanged,
    ] {
        expect(v, Some("javascript"));
    }
}

#[test]
fn metadata_changes_fail() {
    for v in [
        V::InfoTitleChanged,
        V::InfoGone,
        V::XmpChanged,
        V::XmpGone,
        V::XmpEmpty,
    ] {
        expect(v, Some("metadata"));
    }
    let l = lines(&doc(&base(V::XmpEmpty)), "metadata");
    assert!(l.iter().any(|x| x.starts_with("xmp\tstate=empty")), "{l:?}");
}

#[test]
fn viewer_preference_changes_fail() {
    for v in [V::PrefChanged, V::PrefExtensionGone, V::PageModeChanged] {
        expect(v, Some("viewer_prefs"));
    }
}

#[test]
fn notes_carry_no_content() {
    for v in [
        V::InfoTitleChanged,
        V::XmpChanged,
        V::JsScriptChanged,
        V::XfaPacketChanged,
        V::FieldAaChanged,
    ] {
        let r = compare(&doc(&base(V::None)), &doc(&base(v)));
        let notes = r
            .checks
            .iter()
            .flat_map(|c| c.notes.clone())
            .collect::<Vec<_>>()
            .join(" ");
        for secret in [
            "Report",
            "Draft",
            "Other",
            "datasets>",
            "function",
            "AFNumber",
            "alert",
        ] {
            assert!(!notes.contains(secret), "{v:?}: {notes}");
        }
        // No digest either: notes name fields, never values.
        let l = lines(
            &doc(&base(v)),
            DOC_LEVEL
                .iter()
                .find(|c| {
                    r.checks
                        .iter()
                        .any(|x| x.category == **c && x.class == SemClass::Fail)
                })
                .unwrap(),
        );
        for line in l {
            for field in line
                .split('\t')
                .filter_map(|f| f.split_once('='))
                .map(|(_, v)| v)
            {
                if field.len() >= 16 {
                    assert!(!notes.contains(field), "{v:?}: {notes}");
                }
            }
        }
    }
}

fn plain(catalog_extra: &str, objs: Vec<(u32, Body)>, trailer: &str) -> Document {
    let mut o = vec![
        (
            1,
            Body::Dict(format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>")),
        ),
        (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
        (
            3,
            d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
        ),
    ];
    o.extend(objs);
    doc(&pdf(&o, trailer, 0))
}

#[test]
fn absent_structures_are_not_applicable() {
    let p = plain("", vec![], "");
    let r = compare(&p, &p);
    for c in DOC_LEVEL {
        assert_eq!(class_of(&r, c), SemClass::NotApplicable, "{c}");
    }
    // Dropping all of them is a failure of each.
    let r = compare(&doc(&base(V::None)), &p);
    for c in DOC_LEVEL {
        assert_eq!(class_of(&r, c), SemClass::Fail, "{c}");
    }
    // An AcroForm without XFA is not an XFA form.
    let f = plain("/AcroForm << /Fields [] >>", vec![], "");
    assert_eq!(class_of(&compare(&f, &f), "xfa"), SemClass::NotApplicable);
}

#[test]
fn unsupported_encodings_are_not_success() {
    let hex = |cat: &str, objs: Vec<(u32, Body)>, trailer: &str| {
        let p = plain(cat, objs, trailer);
        compare(&p, &p)
    };
    let r = hex(
        "/AcroForm << /Fields [] /XFA [(template) 5 0 R] >>",
        vec![(5, st("/Filter /RunLengthDecode", b"\x02<t>\x80"))],
        "",
    );
    assert_eq!(class_of(&r, "xfa"), SemClass::Unsupported);
    assert_eq!(class_of(&r, "acroform"), SemClass::Pass);
    assert_eq!(r.class, SemClass::Unsupported);
    let r = hex(
        "/Metadata 5 0 R",
        vec![(5, st("/Type /Metadata /Filter /LZWDecode", b"x"))],
        "",
    );
    assert_eq!(class_of(&r, "metadata"), SemClass::Unsupported);
    let r = hex(
        "/Names << /JavaScript 5 0 R >>",
        vec![
            (5, d("<< /Names [(a) 6 0 R] >>")),
            (6, d("<< /S /JavaScript /JS 7 0 R >>")),
            (
                7,
                flate("/DecodeParms << /Predictor 12 /Columns 4 >>", b"abcd"),
            ),
        ],
        "",
    );
    assert_eq!(class_of(&r, "javascript"), SemClass::Unsupported);
}

/// Catalog entries, objects, trailer entries, category, expected class.
type Case = (
    &'static str,
    Vec<(u32, Body)>,
    &'static str,
    &'static str,
    SemClass,
);

#[test]
fn ascii_filters_and_chains_decode() {
    let xmp = b"<x:xmpmeta>Report</x:xmpmeta>";
    let meta = |b: Body| plain("/Metadata 5 0 R", vec![(5, b)], "");
    let plain_xmp = meta(st("/Type /Metadata", xmp));
    let hexed: String = xmp.iter().map(|b| format!("{b:02X}")).collect();
    let z = miniz_oxide::deflate::compress_to_vec_zlib(xmp, 6);
    let zhex: String = z.iter().map(|b| format!("{b:02x} ")).collect();
    for b in [
        st(
            "/Type /Metadata /Filter /AHx",
            format!("{hexed}>").as_bytes(),
        ),
        st(
            "/Type /Metadata /Filter [/ASCIIHexDecode /FlateDecode]",
            format!("{zhex}>").as_bytes(),
        ),
        // "Report" in ASCII85, abbreviated filter name, with white space.
        st("/Type /Metadata /Filter /A85", &a85(xmp)),
    ] {
        assert_eq!(
            class_of(&compare(&plain_xmp, &meta(b)), "metadata"),
            SemClass::Pass
        );
    }
    assert_eq!(ascii85(b"9jqo^~>").unwrap(), b"Man ");
    assert_eq!(ascii85(b"z9jqo^~>").unwrap(), b"\0\0\0\0Man ");
    assert_eq!(ascii85(b"9jqo~>").unwrap(), b"Man");
    assert!(ascii85(b"9~>").is_none() && ascii85(b"s8W-\"~>").is_none() && ascii85(b"{").is_none());
    assert_eq!(asciihex(b"41 41 4>").unwrap(), b"AA@");
    assert!(asciihex(b"4g").is_none());
}

/// ASCII85 encoding for the tests (no `z` shortcut), with a line break.
fn a85(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in b.chunks(4) {
        let mut g = [0u8; 4];
        g[..c.len()].copy_from_slice(c);
        let mut v = u32::from_be_bytes(g) as u64;
        let mut d = [0u8; 5];
        for x in d.iter_mut().rev() {
            *x = (v % 85) as u8 + b'!';
            v /= 85;
        }
        out.extend_from_slice(&d[..c.len() + 1]);
        out.push(b'\n');
    }
    out.extend_from_slice(b"~>");
    out
}

#[test]
fn malformed_structures_do_not_panic() {
    let cases: Vec<Case> = vec![
        (
            "/AcroForm << /XFA [(template)] >>",
            vec![],
            "",
            "xfa",
            SemClass::Error,
        ),
        (
            "/AcroForm << /XFA [5 6 0 R] >>",
            vec![(6, st("", b"x"))],
            "",
            "xfa",
            SemClass::Error,
        ),
        (
            "/AcroForm << /XFA [(t) 5 0 R] >>",
            vec![(5, d("<< >>"))],
            "",
            "xfa",
            SemClass::Error,
        ),
        (
            "/AcroForm << /XFA 42 >>",
            vec![],
            "",
            "xfa",
            SemClass::Error,
        ),
        ("/OpenAction 42", vec![], "", "doc_actions", SemClass::Pass),
        ("/AA 42", vec![], "", "doc_actions", SemClass::Pass),
        (
            "/Names << /JavaScript 5 0 R >>",
            vec![(5, d("<< /Kids [42] >>"))],
            "",
            "javascript",
            SemClass::Error,
        ),
        (
            "",
            vec![(5, d("[1 2]"))],
            "/Info 5 0 R",
            "metadata",
            SemClass::Pass,
        ),
        (
            "/Metadata 5 0 R",
            vec![(5, d("<< /Type /Metadata >>"))],
            "",
            "metadata",
            SemClass::Pass,
        ),
        (
            "/ViewerPreferences [1 2]",
            vec![],
            "",
            "viewer_prefs",
            SemClass::Pass,
        ),
    ];
    for (cat_extra, objs, trailer, cat, want) in cases {
        let p = plain(cat_extra, objs, trailer);
        let r = compare(&p, &p);
        assert_eq!(class_of(&r, cat), want, "{cat_extra}: {:?}", r.checks);
    }
    // Corrupt FlateDecode data: compared by its raw bytes.
    let bad = |data: &[u8]| {
        plain(
            "/Metadata 5 0 R",
            vec![(5, st("/Type /Metadata /Filter /FlateDecode", data))],
            "",
        )
    };
    let (a, b) = (bad(b"\x78\x9c not deflate"), bad(b"\x78\x9c other junk"));
    assert_eq!(class_of(&compare(&a, &a), "metadata"), SemClass::Pass);
    assert_eq!(class_of(&compare(&a, &b), "metadata"), SemClass::Fail);
    assert!(lines(&a, "metadata")[0].contains("state=undecodable"));
}

#[test]
fn action_cycles_end() {
    // A /Next chain that returns to its start, and one that branches back.
    let p = plain(
        "/OpenAction 5 0 R",
        vec![
            (5, d("<< /S /JavaScript /JS (a) /Next 6 0 R >>")),
            (6, d("<< /S /JavaScript /JS (b) /Next [5 0 R 6 0 R] >>")),
        ],
        "",
    );
    let l = lines(&p, "doc_actions");
    assert!(
        l[0].contains("<cycle@0>") && l[0].contains("<cycle@1>"),
        "{l:?}"
    );
    assert_eq!(class_of(&compare(&p, &p), "doc_actions"), SemClass::Pass);
    // A cycle in the JavaScript name tree and in the field tree.
    let t = plain(
        "/Names << /JavaScript 5 0 R >>",
        vec![(5, d("<< /Kids [5 0 R] >>"))],
        "",
    );
    assert_eq!(class_of(&compare(&t, &t), "javascript"), SemClass::Error);
    let f = plain(
        "/AcroForm << /Fields [5 0 R] >>",
        vec![
            (5, d("<< /T (a) /Kids [6 0 R] >>")),
            (6, d("<< /T (b) /Kids [5 0 R] >>")),
        ],
        "",
    );
    assert_eq!(class_of(&compare(&f, &f), "doc_actions"), SemClass::Error);
}

#[test]
fn limits_are_errors_not_cuts() {
    // An action chain deeper than the limit.
    let n = MAX_ACTION_DEPTH as u32 + 4;
    let objs: Vec<(u32, Body)> = (0..n)
        .map(|i| {
            let next = if i + 1 < n {
                format!("/Next {} 0 R", 10 + i + 1)
            } else {
                String::new()
            };
            (
                10 + i,
                Body::Dict(format!("<< /S /Named /N /NextPage {next} >>")),
            )
        })
        .collect();
    let p = plain("/OpenAction 10 0 R", objs, "");
    assert_eq!(class_of(&compare(&p, &p), "doc_actions"), SemClass::Error);
    // A value nested deeper than the value-depth limit.
    let deep = format!(
        "{}1{}",
        "[".repeat(MAX_VALUE_DEPTH + 2),
        "]".repeat(MAX_VALUE_DEPTH + 2)
    );
    let p = plain(&format!("/ViewerPreferences << /X {deep} >>"), vec![], "");
    assert_eq!(class_of(&compare(&p, &p), "viewer_prefs"), SemClass::Error);
    // A line longer than the limit.
    let long = "a".repeat(MAX_LINE_BYTES + 10);
    let p = plain(
        &format!("/OpenAction << /S /URI /URI ({long}) >>"),
        vec![],
        "",
    );
    assert_eq!(class_of(&compare(&p, &p), "doc_actions"), SemClass::Error);
    // A decoded packet larger than the per-stream limit (a small zlib
    // stream of zeros).
    let bomb = vec![0u8; MAX_DECODED_BYTES + 1];
    let p = plain(
        "/AcroForm << /XFA [(template) 5 0 R] >>",
        vec![(5, flate("", &bomb))],
        "",
    );
    assert_eq!(class_of(&compare(&p, &p), "xfa"), SemClass::Error);
}

#[test]
fn undefined_and_unreadable_references() {
    // §7.3.10: a reference to an undefined object is null — the same as
    // writing null; losing the body a reference pointed at is a
    // difference.
    let with = |next: &str, objs: Vec<(u32, Body)>| {
        let mut o = vec![(
            5,
            Body::Dict(format!("<< /S /JavaScript /JS (a) /Next {next} >>")),
        )];
        o.extend(objs);
        plain("/OpenAction 5 0 R", o, "")
    };
    let kept = with("6 0 R", vec![(6, d("<< /S /JavaScript /JS (b) >>"))]);
    let lost = with("6 0 R", vec![]);
    let null = with("null", vec![]);
    assert_eq!(
        class_of(&compare(&kept, &lost), "doc_actions"),
        SemClass::Fail
    );
    assert_eq!(
        class_of(&compare(&lost, &null), "doc_actions"),
        SemClass::Pass
    );
    // An object the engine cannot read is not null.
    let mut bytes = pdf(
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /OpenAction 5 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (
                3,
                d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
            ),
            (5, d("<< /S /JavaScript /JS (a) /Next 6 0 R >>")),
            (6, d("<< /S /JavaScript /JS (b) >>")),
        ],
        "",
        0,
    );
    let at = bytes.windows(7).position(|w| w == b"6 0 obj").unwrap();
    bytes[at + 8] = b'}';
    let broken = Document::from_bytes(&bytes).unwrap();
    let l = lines(&broken, "doc_actions");
    assert!(l[0].contains("#unresolved"), "{l:?}");
    assert_eq!(
        class_of(&compare(&broken, &null), "doc_actions"),
        SemClass::Fail
    );
}

#[test]
fn kasane_rewrite_keeps_the_document_level_meaning() {
    // The rewriter refuses /XFA, catalog /AA and /Names /JavaScript:
    // compare without them.
    let s = String::from_utf8_lossy(&base(V::None))
        .replace(" /AA << /WC 22 0 R >>", "")
        .replace(" /Names << /JavaScript 30 0 R >>", "")
        .replace(
            " /XFA [(preamble) 11 0 R (template) 12 0 R (datasets) 13 0 R (postamble) 14 0 R]",
            "",
        );
    let d0 = Document::from_bytes_recovering(s.as_bytes()).unwrap();
    let out = kasane_pdf::rewrite(&d0).unwrap();
    let r = compare(&d0, &Document::from_bytes(&out).unwrap());
    assert_eq!(r.class, SemClass::Pass, "{:?}", r.checks);
    for c in ["doc_actions", "metadata", "viewer_prefs", "xfa"] {
        assert_eq!(class_of(&r, c), SemClass::Pass, "{c}");
    }
}

#[test]
fn structure_destinations_name_the_element() {
    // A GoTo with a structure destination (/SD, PDF 2.0) into a tree
    // deeper than the value-depth limit: the element is named by its
    // path, not expanded; pointing at another element is a difference.
    let n = MAX_VALUE_DEPTH as u32 + 6;
    let with = |target: u32| {
        let mut objs: Vec<(u32, Body)> = (0..n)
            .map(|i| {
                let kid = if i + 1 < n {
                    format!("/K [{} 0 R]", 20 + i + 1)
                } else {
                    "/K 0 /Pg 3 0 R".into()
                };
                (
                    20 + i,
                    Body::Dict(format!("<< /Type /StructElem /S /Sect {kid} >>")),
                )
            })
            .collect();
        objs.push((10, d("<< /Type /StructTreeRoot /K 20 0 R >>")));
        objs.push((
            11,
            Body::Dict(format!(
                "<< /S /GoTo /D [3 0 R /Fit] /SD [{target} 0 R /Fit] >>"
            )),
        ));
        plain("/StructTreeRoot 10 0 R /OpenAction 11 0 R", objs, "")
    };
    let a = with(20 + n - 1);
    let r = compare(&a, &a);
    assert_eq!(
        class_of(&r, "doc_actions"),
        SemClass::Pass,
        "{:?}",
        r.checks
    );
    assert!(
        lines(&a, "doc_actions")[0].contains("/SD=[elem#"),
        "{:?}",
        lines(&a, "doc_actions")
    );
    assert_eq!(
        class_of(&compare(&a, &with(20 + n - 2)), "doc_actions"),
        SemClass::Fail
    );
    // The structure tree category itself is unchanged by this.
    assert_eq!(class_of(&r, "struct_tree"), SemClass::Pass);
}
