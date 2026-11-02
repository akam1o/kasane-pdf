//! Synthetic documents for the report-only PDF 2.0 categories (version,
//! page metadata, associated files, document parts): one base document
//! carrying all four, and variants that store the same meaning
//! differently (must pass) or change, drop, add, reorder or re-link
//! exactly one thing (must fail in exactly that category), plus
//! malformed, cyclic and over-limit structures (no panic; `error` where
//! a limit is hit).

use super::pdf20::{compare_report, snapshot_report, REPORT_ONLY};
use super::*;

enum Body {
    Dict(String),
    Stream(String, Vec<u8>),
    /// An xref entry that points at bytes which are not this object.
    Broken,
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

/// `n 0 R` → `n+off 0 R` (dictionary text only).
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

/// Serialise with header `%PDF-{header}`; object 1 is the catalog; `off`
/// renumbers every object.
fn pdf(header: &str, objs: &[(u32, Body)], off: u32) -> Vec<u8> {
    let mut out = format!("%PDF-{header}\n").into_bytes();
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut broken = Vec::new();
    for (n, body) in objs {
        let n = n + off;
        match body {
            Body::Broken => {
                broken.push(n);
                continue;
            }
            Body::Dict(s) => {
                offs.push((n, out.len()));
                out.extend_from_slice(format!("{n} 0 obj\n{}", shift_refs(s, off)).as_bytes());
            }
            Body::Stream(dict, data) => {
                offs.push((n, out.len()));
                out.extend_from_slice(
                    format!(
                        "{n} 0 obj\n<< {} /Length {} >>\nstream\n",
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
    let garbage = out.len();
    out.extend_from_slice(b"% not an object\n");
    for n in broken {
        offs.push((n, garbage));
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
            "trailer\n<< /Size {size} /Root {} 0 R >>\nstartxref\n{x}\n%%EOF\n",
            1 + off
        )
        .as_bytes(),
    );
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum V {
    None,
    KeysReordered,
    Recompressed,
    // version
    HeaderOnly,
    CatalogOnly,
    CatalogIndirect,
    VersionLowered,
    // page metadata
    PageXmpChanged,
    PageXmpGone,
    PageXmpAdded,
    PageXmpSwapped,
    PageXmpShared,
    PageXmpDictChanged,
    // associated files
    AfContentChanged,
    AfRelationshipChanged,
    AfDescChanged,
    AfParamsChanged,
    AfEntryGone,
    AfEntryAdded,
    AfReordered,
    AfUnshared,
    AfEfUnshared,
    AfStructChanged,
    AfNestedXObjectChanged,
    AfPropsChanged,
    AfAnnotGone,
    AfDpartChanged,
    // document parts
    DpartStartChanged,
    DpartDpmChanged,
    DpartKidsSwapped,
    DpartNodeGone,
    DpartParentWrong,
    DpartPageLinkChanged,
    DpartRootEntryChanged,
    DpartShared,
    DpartNodeAdded,
    DpartUnreadable,
    PageXmpUnreadable,
}

const XMP0: &[u8] = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">page zero</x:xmpmeta>";
const XMP1: &[u8] = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">page one</x:xmpmeta>";
const CSV: &[u8] = b"a,b\n1,2\n";

fn base(v: V) -> Vec<u8> {
    base_off(v, 0)
}

fn base_off(v: V, off: u32) -> Vec<u8> {
    use V::*;
    let header = match v {
        CatalogOnly | VersionLowered => "1.7",
        _ => "2.0",
    };
    let version = match v {
        HeaderOnly | VersionLowered => "",
        CatalogIndirect => "/Version 90 0 R",
        _ => "/Version /2.0",
    };
    let cat_af = match v {
        AfEntryGone => "[10 0 R]",
        AfEntryAdded => "[10 0 R 11 0 R 18 0 R]",
        AfReordered => "[11 0 R 10 0 R]",
        _ => "[10 0 R 11 0 R]",
    };
    let catalog = if v == KeysReordered {
        format!("<< /StructTreeRoot 30 0 R /DPartRoot 20 0 R /AF {cat_af} {version} /Pages 2 0 R /Type /Catalog >>")
    } else {
        format!("<< /Type /Catalog /Pages 2 0 R {version} /AF {cat_af} /DPartRoot 20 0 R /StructTreeRoot 30 0 R >>")
    };
    let page0_af = if v == AfUnshared {
        "[71 0 R]"
    } else {
        "[11 0 R]"
    };
    let p0_xmp = match v {
        PageXmpGone => "",
        PageXmpSwapped => "/Metadata 51 0 R",
        _ => "/Metadata 50 0 R",
    };
    let p1_xmp = match v {
        PageXmpUnreadable => "/Metadata 52 0 R",
        PageXmpSwapped => "/Metadata 50 0 R",
        PageXmpShared => "/Metadata 50 0 R",
        _ => "/Metadata 51 0 R",
    };
    let p0_dpart = if v == DpartPageLinkChanged {
        "23 0 R"
    } else {
        "22 0 R"
    };
    let annots = if v == AfAnnotGone {
        "/Annots [61 0 R]"
    } else {
        "/Annots [60 0 R]"
    };
    let mut objs: Vec<(u32, Body)> = vec![
        (1, d(&catalog)),
        (
            2,
            d("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /Resources << /XObject << /Im1 40 0 R >> >> >>"),
        ),
        (
            3,
            d(&format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {p0_xmp} /AF {page0_af} /DPart {p0_dpart} {annots} >>"
            )),
        ),
        (
            4,
            d(&format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {p1_xmp} /DPart 23 0 R /Resources << /Properties << /MC0 << /AF [12 0 R] >> >> /XObject << /Fm1 41 0 R >> >> >>"
            )),
        ),
    ];
    // Associated files.
    let fs10_desc = if v == AfDescChanged {
        "(other)"
    } else {
        "(numbers)"
    };
    let fs10_rel = if v == AfRelationshipChanged {
        "/Source"
    } else {
        "/Data"
    };
    let ef10 = if v == AfEfUnshared {
        "<< /F 13 0 R /UF 72 0 R >>"
    } else {
        "<< /F 13 0 R /UF 13 0 R >>"
    };
    objs.push((
        10,
        d(&format!(
            "<< /Type /Filespec /F (data.csv) /UF (data.csv) /Desc {fs10_desc} /AFRelationship {fs10_rel} /EF {ef10} >>"
        )),
    ));
    objs.push((
        11,
        d("<< /Type /Filespec /F (src.xml) /UF (src.xml) /AFRelationship /Source /EF << /F 14 0 R >> >>"),
    ));
    let props_rel = if v == AfPropsChanged {
        "/Data"
    } else {
        "/Supplement"
    };
    objs.push((
        12,
        d(&format!(
            "<< /Type /Filespec /F (mc.txt) /AFRelationship {props_rel} /EF << /F 15 0 R >> >>"
        )),
    ));
    let csv = if v == AfContentChanged {
        &b"a,b\n1,3\n"[..]
    } else {
        CSV
    };
    let size = if v == AfParamsChanged { 10 } else { 9 };
    let ef_dict = format!(
        "/Type /EmbeddedFile /Subtype /text#2Fcsv /Params << /Size {size} /ModDate (D:20260101000000Z) >>"
    );
    objs.push((
        13,
        if v == Recompressed {
            st(&ef_dict, csv)
        } else {
            flate(&ef_dict, csv)
        },
    ));
    objs.push((14, st("/Type /EmbeddedFile", b"<x/>")));
    objs.push((15, st("/Type /EmbeddedFile", b"mc")));
    objs.push((
        16,
        d("<< /Type /Filespec /F (a.txt) /AFRelationship /Alternative /EF << /F 17 0 R >> >>"),
    ));
    objs.push((17, st("/Type /EmbeddedFile", b"annot")));
    let dpart_af_rel = if v == AfDpartChanged {
        "/Source"
    } else {
        "/Unspecified"
    };
    objs.push((
        18,
        d(&format!(
            "<< /Type /Filespec /F (ext.pdf) /AFRelationship {dpart_af_rel} >>"
        )),
    ));
    let nested_rel = if v == AfNestedXObjectChanged {
        "/Source"
    } else {
        "/Data"
    };
    objs.push((
        19,
        d(&format!(
            "<< /Type /Filespec /F (nested.txt) /AFRelationship {nested_rel} >>"
        )),
    ));
    // Document parts.
    let root_level = if v == DpartRootEntryChanged { 2 } else { 1 };
    objs.push((
        20,
        d(&format!(
            "<< /Type /DPartRoot /DPartRootNode 21 0 R /RecordLevel {root_level} /NodeNameList [/Doc /Rec] >>"
        )),
    ));
    let kids = match v {
        DpartKidsSwapped => "[[23 0 R 22 0 R]]",
        DpartNodeGone => "[[22 0 R]]",
        DpartShared => "[[22 0 R 22 0 R]]",
        DpartUnreadable => "[[22 0 R 24 0 R]]",
        DpartNodeAdded => "[[22 0 R 23 0 R 25 0 R]]",
        _ => "[[22 0 R 23 0 R]]",
    };
    objs.push((
        21,
        d(&format!(
            "<< /Type /DPart /Parent 20 0 R /DParts {kids} /DPM << /Doc << /Title (t) >> >> >>"
        )),
    ));
    let start22 = if v == DpartStartChanged {
        "4 0 R"
    } else {
        "3 0 R"
    };
    let parent22 = if v == DpartParentWrong {
        "20 0 R"
    } else {
        "21 0 R"
    };
    objs.push((
        22,
        d(&format!(
            "<< /Type /DPart /Parent {parent22} /Start {start22} /End 3 0 R /DPM << /Rec << /Id 1 >> >> /AF [18 0 R] >>"
        )),
    ));
    let dpm23 = if v == DpartDpmChanged { 3 } else { 2 };
    objs.push((
        23,
        d(&format!(
            "<< /Type /DPart /Parent 21 0 R /Start 4 0 R /DPM << /Rec << /Id {dpm23} >> >> >>"
        )),
    ));
    if v == DpartUnreadable {
        objs.push((24, Body::Broken));
    }
    if v == DpartNodeAdded {
        objs.push((25, d("<< /Type /DPart /Parent 21 0 R /Start 4 0 R >>")));
    }
    // Structure tree.
    objs.push((30, d("<< /Type /StructTreeRoot /K 31 0 R >>")));
    objs.push((
        31,
        d("<< /Type /StructElem /S /Document /P 30 0 R /K [32 0 R] >>"),
    ));
    let struct_af = if v == AfStructChanged {
        "[11 0 R]"
    } else {
        "[10 0 R]"
    };
    objs.push((
        32,
        d(&format!(
            "<< /Type /StructElem /S /Table /P 31 0 R /AF {struct_af} >>"
        )),
    ));
    // XObjects.
    objs.push((
        40,
        st(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /AF [12 0 R]",
            b"\x80",
        ),
    ));
    objs.push((
        41,
        st(
            "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Im2 42 0 R >> >>",
            b"",
        ),
    ));
    objs.push((
        42,
        st(
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /AF [19 0 R]",
            b"\x40",
        ),
    ));
    // Page metadata.
    let xmp0 = if v == PageXmpChanged {
        &b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">page 0 changed</x:xmpmeta>"[..]
    } else {
        XMP0
    };
    let xmp_dict = if v == PageXmpDictChanged {
        "/Type /Metadata /Subtype /XML /Extra 1"
    } else {
        "/Type /Metadata /Subtype /XML"
    };
    objs.push((
        50,
        if v == Recompressed {
            st(xmp_dict, xmp0)
        } else {
            flate(xmp_dict, xmp0)
        },
    ));
    objs.push((51, st("/Type /Metadata /Subtype /XML", XMP1)));
    if v == PageXmpUnreadable {
        objs.push((52, Body::Broken));
    }
    // Annotations.
    objs.push((
        60,
        d("<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 1 1] /FS 16 0 R /AF [16 0 R] /P 3 0 R >>"),
    ));
    objs.push((
        61,
        d("<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 1 1] /FS 16 0 R /P 3 0 R >>"),
    ));
    // Copies (same content, other objects): unshared variants.
    objs.push((
        71,
        d("<< /Type /Filespec /F (src.xml) /UF (src.xml) /AFRelationship /Source /EF << /F 14 0 R >> >>"),
    ));
    objs.push((72, flate(&ef_dict, csv)));
    if v == CatalogIndirect {
        objs.push((90, d("/2.0")));
    }
    if v == PageXmpAdded {
        // The first page keeps its own; the root page node is not a page,
        // so add one to the second page's sibling: a third page.
        objs[1] = (
            2,
            d("<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /Resources << /XObject << /Im1 40 0 R >> >> >>"),
        );
        objs.push((
            5,
            d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Metadata 51 0 R /Resources << >> >>"),
        ));
    }
    pdf(header, &objs, off)
}

fn doc(b: &[u8]) -> Document {
    Document::from_bytes(b).unwrap()
}

fn report(a: &[u8], b: &[u8]) -> Vec<SemCheck> {
    compare_report(&doc(a), a, &doc(b), b)
}

fn lines(b: &[u8], cat: &str) -> Vec<String> {
    let i = REPORT_ONLY.iter().position(|c| *c == cat).unwrap();
    match &snapshot_report(&doc(b), b)[i] {
        Section::Data(v) => v.clone(),
        other => panic!("{cat}: {other:?}"),
    }
}

/// `v` against the base: every report-only category passes except
/// `cat`, which fails.
fn expect(v: V, cat: Option<&str>) {
    expect_all(v, cat.as_slice());
}

/// `v` against the base: exactly the categories `cats` fail.
fn expect_all(v: V, cats: &[&str]) {
    let r = report(&base(V::None), &base(v));
    for c in &r {
        let want = if cats.contains(&c.category.as_str()) {
            SemClass::Fail
        } else {
            SemClass::Pass
        };
        assert_eq!(c.class, want, "{v:?}: {} {:?}", c.category, c.notes);
    }
}

#[test]
fn base_document_covers_the_pdf20_categories() {
    let b = base(V::None);
    let al = lines(&b, "associated_files");
    for owner in [
        "owner=catalog\t",
        "owner=page:p0\t",
        "owner=annot:0.0\t",
        "owner=struct:0.0\t",
        "owner=res:p0/xobject:Im1\t",
        "owner=res:p1/xobject:Fm1/xobject:Im2\t",
        "owner=res:p1/props:MC0\t",
        "owner=dpart:0.0.0\t",
    ] {
        assert!(al.iter().any(|l| l.contains(owner)), "{owner}: {al:#?}");
    }
    assert!(
        al.iter().any(|l| l.contains("same_as=catalog#1")),
        "{al:#?}"
    );
    assert!(
        al.iter().any(|l| l.contains("/UF same_as=catalog#0/EF/F")),
        "{al:#?}"
    );
    let dl = lines(&b, "dparts");
    assert!(
        dl.iter()
            .any(|l| l.starts_with("node\tpath=0.0.1\t") && l.contains("start=p1")),
        "{dl:#?}"
    );
    assert!(dl.iter().any(|l| l == "page\tp=0\tdpart=0.0.0"), "{dl:#?}");
    assert_eq!(lines(&b, "page_metadata").len(), 2);
    assert!(lines(&b, "version").contains(&"effective\tversion=2.0".to_string()));
    for c in report(&b, &b) {
        assert_eq!(c.class, SemClass::Pass, "{}", c.category);
    }
}

#[test]
fn storage_differences_pass() {
    let b = base(V::None);
    for (what, other) in [
        ("renumbered", base_off(V::None, 100)),
        ("keys reordered", base(V::KeysReordered)),
        ("recompressed", base(V::Recompressed)),
    ] {
        for c in report(&b, &other) {
            assert_eq!(
                c.class,
                SemClass::Pass,
                "{what}: {} {:?}",
                c.category,
                c.notes
            );
        }
    }
}

#[test]
fn version_declarations() {
    // Same effective version, declared differently: pass, with a note.
    for v in [V::HeaderOnly, V::CatalogOnly, V::CatalogIndirect] {
        let r = report(&base(V::None), &base(v));
        let c = r.iter().find(|c| c.category == "version").unwrap();
        assert_eq!(c.class, SemClass::Pass, "{v:?}");
        assert!(
            c.notes
                .iter()
                .any(|n| n.starts_with("declared form changed")),
            "{v:?}: {:?}",
            c.notes
        );
    }
    expect(V::VersionLowered, Some("version"));
    // Version rules: the catalog wins only when later than the header.
    let one = |header: &str, catalog: &str| {
        let b = pdf(
            header,
            &[
                (
                    1,
                    d(&format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>")),
                ),
                (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            ],
            0,
        );
        lines(&b, "version")
            .into_iter()
            .find(|l| l.starts_with("effective"))
            .unwrap()
    };
    assert_eq!(one("1.4", "/Version /1.7"), "effective\tversion=1.7");
    assert_eq!(one("2.0", "/Version /1.7"), "effective\tversion=2.0");
    assert_eq!(one("1.7", ""), "effective\tversion=1.7");
    assert_eq!(one("1.7", "/Version /x"), "effective\tversion=1.7");
    assert_eq!(one("x", "/Version /2.0"), "effective\tversion=2.0");
    assert_eq!(one("x", ""), "effective\tversion=unknown");
}

#[test]
fn page_metadata_changes_fail() {
    for v in [
        V::PageXmpChanged,
        V::PageXmpGone,
        V::PageXmpAdded,
        V::PageXmpSwapped,
        V::PageXmpShared,
        V::PageXmpDictChanged,
        V::PageXmpUnreadable,
    ] {
        expect(v, Some("page_metadata"));
    }
    let l = lines(&base(V::PageXmpUnreadable), "page_metadata");
    assert!(
        l.iter()
            .any(|l| l.contains("p=1\tstate=not-a-stream") && l.contains("#unresolved")),
        "{l:#?}"
    );
}

#[test]
fn associated_file_changes_fail() {
    for v in [
        V::AfContentChanged,
        V::AfRelationshipChanged,
        V::AfDescChanged,
        V::AfParamsChanged,
        V::AfEntryGone,
        V::AfEntryAdded,
        V::AfReordered,
        V::AfUnshared,
        V::AfEfUnshared,
        V::AfStructChanged,
        V::AfNestedXObjectChanged,
        V::AfPropsChanged,
        V::AfAnnotGone,
        V::AfDpartChanged,
    ] {
        expect(v, Some("associated_files"));
    }
}

#[test]
fn document_part_changes_fail() {
    for v in [
        V::DpartStartChanged,
        V::DpartDpmChanged,
        V::DpartRootEntryChanged,
        V::DpartParentWrong,
        V::DpartPageLinkChanged,
        V::DpartShared,
        V::DpartNodeAdded,
    ] {
        expect(v, Some("dparts"));
    }
    // Children swapped: the node that has an /AF moves to another path,
    // so its association moves too.
    expect_all(V::DpartKidsSwapped, &["dparts", "associated_files"]);
    // A node removed: its /AF goes too.
    let r = report(&base(V::None), &base(V::DpartNodeGone));
    let fails: Vec<&str> = r
        .iter()
        .filter(|c| c.class == SemClass::Fail)
        .map(|c| c.category.as_str())
        .collect();
    assert!(fails.contains(&"dparts"), "{fails:?}");
}

#[test]
fn unreadable_references_are_not_null() {
    // An /AF entry and a DPart that point at an object whose xref
    // offset is not that object: named `#unresolved`, so they differ
    // from both the real object and an absent one.
    let b = base(V::None);
    let r = report(&b, &base(V::DpartUnreadable));
    let c = r.iter().find(|c| c.category == "dparts").unwrap();
    assert_eq!(c.class, SemClass::Fail, "{:?}", c.notes);
    let l = lines(&base(V::DpartUnreadable), "dparts");
    assert!(
        l.iter()
            .any(|l| l.contains("path=0.0.1\tmalformed=/#unresolved")),
        "{l:#?}"
    );
    // An undefined /AF entry is null (§7.3.10), an unreadable one is not.
    let mk = |entry: &str, broken: bool| {
        let mut objs = vec![
            (
                1,
                d(&format!("<< /Type /Catalog /Pages 2 0 R /AF [{entry}] >>")),
            ),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
        ];
        if broken {
            objs.push((9, Body::Broken));
        }
        pdf("2.0", &objs, 0)
    };
    let undefined = lines(&mk("9 0 R", false), "associated_files");
    let unreadable = lines(&mk("9 0 R", true), "associated_files");
    assert!(
        undefined.iter().any(|l| l.contains("malformed=null")),
        "{undefined:#?}"
    );
    assert!(
        unreadable.iter().any(|l| l.contains("#unresolved")),
        "{unreadable:#?}"
    );
    assert_ne!(undefined, unreadable);
}

#[test]
fn cycles_and_limits() {
    // A DPart that is its own descendant: named by the path it returns
    // to, no loop.
    let cyc = pdf(
        "2.0",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /DPartRoot 20 0 R >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (20, d("<< /Type /DPartRoot /DPartRootNode 21 0 R >>")),
            (
                21,
                d("<< /Type /DPart /Parent 20 0 R /DParts [[22 0 R]] >>"),
            ),
            (
                22,
                d("<< /Type /DPart /Parent 21 0 R /DParts [[21 0 R]] >>"),
            ),
        ],
        0,
    );
    let l = lines(&cyc, "dparts");
    assert!(
        l.iter().any(|l| l == "node\tpath=0.0.0.0.0\tcycle_to=0"),
        "{l:#?}"
    );
    let _ = snapshot_report(&doc(&cyc), &cyc);
    // A DPart chain deeper than the limit: error, not a cut.
    let mut objs = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R /DPartRoot 20 0 R >>")),
        (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
        (20, d("<< /Type /DPartRoot /DPartRootNode 100 0 R >>")),
    ];
    for i in 0..(MAX_DEPTH as u32 + 5) {
        objs.push((
            100 + i,
            d(&format!(
                "<< /Type /DPart /Parent 20 0 R /DParts [[{} 0 R]] >>",
                101 + i
            )),
        ));
    }
    let deep = pdf("2.0", &objs, 0);
    let s = &snapshot_report(&doc(&deep), &deep);
    let i = REPORT_ONLY.iter().position(|c| *c == "dparts").unwrap();
    assert!(matches!(s[i], Section::Error(_)), "{:?}", s[i]);
    // A file specification that contains itself, and a value nested past
    // the depth limit: no loop; the depth limit is an error.
    let selfref = pdf(
        "2.0",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /AF [10 0 R] >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (
                10,
                d("<< /Type /Filespec /F (x) /RF << /F [1 10 0 R] >> >>"),
            ),
        ],
        0,
    );
    let l = lines(&selfref, "associated_files");
    assert!(l.iter().any(|l| l.contains("<cycle>")), "{l:#?}");
    let nested = format!("{}1{}", "[".repeat(40), "]".repeat(40));
    let deepval = pdf(
        "2.0",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /AF [10 0 R] >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (10, d(&format!("<< /Type /Filespec /F (x) /X {nested} >>"))),
        ],
        0,
    );
    let i = REPORT_ONLY
        .iter()
        .position(|c| *c == "associated_files")
        .unwrap();
    let s = &snapshot_report(&doc(&deepval), &deepval);
    assert!(matches!(s[i], Section::Error(_)), "{:?}", s[i]);
    // A line past the size limit: error, not a cut.
    let long = pdf(
        "2.0",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R /AF [10 0 R] >>")),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (
                10,
                d(&format!(
                    "<< /Type /Filespec /F (x) /Desc ({}) >>",
                    "y".repeat(MAX_LINE_BYTES + 1)
                )),
            ),
        ],
        0,
    );
    let s = &snapshot_report(&doc(&long), &long);
    assert!(matches!(s[i], Section::Error(_)), "{:?}", s[i]);
    // A structure tree cycle: error.
    let scyc = pdf(
        "2.0",
        &[
            (
                1,
                d("<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 30 0 R >>"),
            ),
            (2, d("<< /Type /Pages /Kids [] /Count 0 >>")),
            (30, d("<< /Type /StructTreeRoot /K 31 0 R >>")),
            (31, d("<< /S /Document /K [32 0 R] >>")),
            (32, d("<< /S /P /K [31 0 R] >>")),
        ],
        0,
    );
    let s = &snapshot_report(&doc(&scyc), &scyc);
    assert!(matches!(s[i], Section::Error(_)), "{:?}", s[i]);
    // A form XObject that draws itself: listed once, no loop.
    let fself = pdf(
        "2.0",
        &[
            (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (
                3,
                d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << /XObject << /F 40 0 R >> >> >>"),
            ),
            (
                40,
                st(
                    "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /AF [10 0 R] /Resources << /XObject << /F 40 0 R >> >>",
                    b"",
                ),
            ),
            (10, d("<< /Type /Filespec /F (x) >>")),
        ],
        0,
    );
    let l = lines(&fself, "associated_files");
    assert_eq!(
        l.iter().filter(|l| l.starts_with("owner")).count(),
        1,
        "{l:#?}"
    );
}

#[test]
fn unsupported_filters_are_not_success() {
    let mk = |filter: &str| {
        pdf(
            "2.0",
            &[
                (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
                (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
                (
                    3,
                    d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Metadata 50 0 R >>"),
                ),
                (50, st(&format!("/Type /Metadata {filter}"), b"xx")),
            ],
            0,
        )
    };
    let b = mk("/Filter /LZWDecode");
    let r = report(&b, &b);
    let c = r.iter().find(|c| c.category == "page_metadata").unwrap();
    assert_eq!(c.class, SemClass::Unsupported, "{:?}", c.notes);
}

#[test]
fn notes_carry_no_content() {
    for v in [
        V::PageXmpChanged,
        V::AfContentChanged,
        V::AfDescChanged,
        V::DpartDpmChanged,
    ] {
        for c in report(&base(V::None), &base(v)) {
            for n in &c.notes {
                for secret in ["page 0", "numbers", "other", "1,3", "(t)"] {
                    assert!(!n.contains(secret), "{v:?} {}: {n}", c.category);
                }
            }
        }
    }
}

#[test]
fn report_only_categories_leave_the_overall_class_alone() {
    // A difference in a report-only category does not reach `class`
    // or `checks`.
    let (a, b) = (base(V::None), base(V::AfContentChanged));
    let (da, db) = (doc(&a), doc(&b));
    let s = compare(&da, &db);
    assert!(s
        .checks
        .iter()
        .all(|c| !REPORT_ONLY.contains(&c.category.as_str())));
    assert!(s.report_only.is_empty());
    let r = compare_report(&da, &a, &db, &b);
    assert!(r.iter().any(|c| c.class == SemClass::Fail));
}

#[test]
fn kasane_rewrite_keeps_the_meaning_of_a_1_7_document() {
    // The base document declared as 1.7: every report-only category
    // passes after kasane's rewrite (renumbered, recompressed as
    // written), the version included.
    let b = base(V::VersionLowered);
    let out = kasane_pdf::rewrite(&doc(&b)).unwrap();
    for c in report(&b, &out) {
        assert_eq!(c.class, SemClass::Pass, "{} {:?}", c.category, c.notes);
    }
}

#[test]
fn shared_and_deep_form_xobjects() {
    // 40 nested forms (deeper than drawing allows, as resources may be),
    // the innermost with an /AF; the chain is used by two pages: listed
    // once under the first page, named from the second.
    let mut objs = vec![
        (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
        (2, d("<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")),
        (
            3,
            d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << /XObject << /F 100 0 R >> >> >>"),
        ),
        (
            4,
            d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Resources << /XObject << /F 100 0 R >> >> >>"),
        ),
        (10, d("<< /Type /Filespec /F (x) >>")),
    ];
    for i in 0..40u32 {
        let next = if i < 39 {
            format!("/Resources << /XObject << /F {} 0 R >> >>", 101 + i)
        } else {
            "/AF [10 0 R]".to_string()
        };
        objs.push((
            100 + i,
            st(
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] {next}"),
                b"",
            ),
        ));
    }
    let b = pdf("2.0", &objs, 0);
    let l = lines(&b, "associated_files");
    assert_eq!(
        l.iter().filter(|l| l.starts_with("owner")).count(),
        1,
        "{l:#?}"
    );
    assert!(
        l.iter()
            .any(|l| l.starts_with("owner\towner=res:p0/xobject:F/")),
        "{l:#?}"
    );
    for c in report(&b, &pdf("2.0", &objs, 50)) {
        assert!(
            matches!(c.class, SemClass::Pass | SemClass::NotApplicable),
            "{} {:?}",
            c.category,
            c.class
        );
    }
}

#[test]
fn kasane_rewrite_keeps_the_meaning_of_a_2_0_document() {
    // W2: the base document as 2.0 (header and catalog) — every
    // report-only category passes after kasane's rewrite, and so does
    // every enforced-path category.
    let b = base(V::None);
    let out = kasane_pdf::rewrite(&doc(&b)).unwrap();
    assert!(out.starts_with(b"%PDF-2.0"));
    for c in report(&b, &out) {
        assert_eq!(c.class, SemClass::Pass, "{} {:?}", c.category, c.notes);
    }
    let s = compare(&doc(&b), &doc(&out));
    for c in &s.checks {
        assert!(
            matches!(c.class, SemClass::Pass | SemClass::NotApplicable),
            "{} {:?} {:?}",
            c.category,
            c.class,
            c.notes
        );
    }
}

#[test]
fn appearance_streams_are_owners() {
    // An annotation's normal appearance (a state dictionary) holds a
    // form with an /AF, and a resource of it another: both listed; a
    // change to either fails.
    let mk = |rel: &str| {
        pdf(
            "2.0",
            &[
                (1, d("<< /Type /Catalog /Pages 2 0 R >>")),
                (2, d("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
                (3, d("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] /Annots [60 0 R] /Resources << >> >>")),
                (60, d("<< /Type /Annot /Subtype /Widget /Rect [0 0 1 1] /AS /On /AP << /N << /On 40 0 R /Off 41 0 R >> >> >>")),
                (
                    40,
                    st(
                        "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /AF [10 0 R] /Resources << /XObject << /Im 42 0 R >> >>",
                        b"",
                    ),
                ),
                (41, st("/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b"")),
                (
                    42,
                    st(
                        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /AF [11 0 R]",
                        b"\x80",
                    ),
                ),
                (10, d("<< /Type /Filespec /F (a) /AFRelationship /Data >>")),
                (11, d(&format!("<< /Type /Filespec /F (b) /AFRelationship {rel} >>"))),
            ],
            0,
        )
    };
    let b = mk("/Source");
    let l = lines(&b, "associated_files");
    assert!(
        l.iter().any(|l| l.contains("owner=annot:0.0/ap:N:On\t")),
        "{l:#?}"
    );
    assert!(
        l.iter()
            .any(|l| l.contains("owner=annot:0.0/ap:N:On/xobject:Im\t")),
        "{l:#?}"
    );
    let r = report(&b, &mk("/Data"));
    let c = r.iter().find(|c| c.category == "associated_files").unwrap();
    assert_eq!(c.class, SemClass::Fail, "{:?}", c.notes);
}
