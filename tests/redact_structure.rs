//! `redact` and the structure tree: the elements whose content the
//! rects touch (and their ancestors) lose their text, the rest of the
//! tree is kept and stays consistent.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{redact, Dict, Document, Error, Object, RedactOptions};

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

/// A tagged one-page document: page 3 with `content` (font /F1,
/// `/StructParents 0`, `res` and `page_extra` added), root 30 with
/// `/K` `root_k`, `/ParentTree` `parent_tree`, and `extra` objects.
fn tagged(
    content: &str,
    res: &str,
    page_extra: &str,
    root_k: &str,
    parent_tree: &str,
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 30 0 R \
              /MarkInfo << /Marked true >> >>"
                .to_vec(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << \
                 /Font << /F1 20 0 R >> {res} >> /Contents 4 0 R /StructParents 0 {page_extra} >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (
            30,
            format!("<< /Type /StructTreeRoot /K {root_k} /ParentTree {parent_tree} >>")
                .into_bytes(),
        ),
    ];
    objs.extend(font_objs());
    objs.extend(extra);
    build_pdf(&objs)
}

const RECT: (f64, f64, f64, f64) = (45.0, 90.0, 160.0, 115.0);

/// MCID 0 inside the rect, MCID 1 outside.
const TWO: &str = "/P <</MCID 0>> BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC \
                   /P <</MCID 1>> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC";

fn run(pdf: &[u8]) -> Result<kasane_pdf::Redaction, Error> {
    let doc = Document::from_bytes(pdf).unwrap();
    redact(&doc, &[(0, vec![RECT])], &RedactOptions::new())
}

fn unsupported(r: Result<kasane_pdf::Redaction, Error>) -> String {
    match r {
        Err(Error::Unsupported { feature, detail }) => format!("{feature}: {detail}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The output's structure elements by `/S` (in tree order, each once).
fn elements(pdf: &[u8]) -> Vec<(String, Dict)> {
    let doc = Document::from_bytes(pdf).unwrap();
    let root = doc
        .deref(doc.catalog().get(b"StructTreeRoot".as_slice()).unwrap())
        .unwrap();
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<Object> = vec![root.as_dict().unwrap()[b"K".as_slice()].clone()];
    while let Some(o) = stack.pop() {
        if let Object::Ref(r) = &o {
            if !seen.insert(*r) {
                continue;
            }
        }
        match doc.deref(&o).unwrap() {
            Object::Array(a) => stack.extend(a.into_iter().rev()),
            Object::Dict(d) if d.contains_key(b"S".as_slice()) => {
                let s = String::from_utf8_lossy(d[b"S".as_slice()].as_name().unwrap()).into_owned();
                if let Some(k) = d.get(b"K".as_slice()) {
                    stack.push(k.clone());
                }
                out.push((s, d));
            }
            _ => {}
        }
    }
    out
}

fn elem<'a>(els: &'a [(String, Dict)], s: &str) -> &'a Dict {
    &els.iter()
        .find(|(n, _)| n == s)
        .unwrap_or_else(|| panic!("no /{s}"))
        .1
}

fn has_text(d: &Dict) -> bool {
    ["Alt", "ActualText", "E", "T"]
        .iter()
        .any(|k| d.contains_key(k.as_bytes()))
}

fn raw_contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether `needle` occurs in the output: the raw bytes, any string
/// of any object, any page's decoded content.
fn contains(pdf: &[u8], needle: &[u8]) -> bool {
    fn walk(o: &Object, needle: &[u8]) -> bool {
        match o {
            Object::String(b) => raw_contains(b, needle),
            Object::Array(a) => a.iter().any(|v| walk(v, needle)),
            Object::Dict(d) => d.values().any(|v| walk(v, needle)),
            Object::Stream { dict, data } => {
                raw_contains(data, needle) || dict.values().any(|v| walk(v, needle))
            }
            _ => false,
        }
    }
    if raw_contains(pdf, needle) {
        return true;
    }
    let doc = Document::from_bytes(pdf).unwrap();
    let size = doc.trailer()[b"Size".as_slice()].as_i64().unwrap() as u32;
    (1..size).any(|n| {
        doc.resolve(kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        })
        .is_ok_and(|o| walk(&o, needle))
    }) || doc.pages().iter().any(|p| {
        doc.page_contents(p)
            .unwrap()
            .iter()
            .any(|c| raw_contains(c, needle))
    })
}

/// Every reference the output's tree holds resolves: `/K` kids,
/// `/OBJR /Obj`, `/MCR /Stm`, `/Pg`, `/P`, the `/ParentTree` values.
fn tree_resolves(pdf: &[u8]) {
    let doc = Document::from_bytes(pdf).unwrap();
    let check = |o: &Object| {
        if let Object::Ref(r) = o {
            assert!(
                doc.resolve(*r).is_ok_and(|o| !matches!(o, Object::Null)),
                "{r} does not resolve"
            );
        }
    };
    for (_, d) in elements(pdf) {
        for k in ["Pg", "P"] {
            if let Some(o) = d.get(k.as_bytes()) {
                check(o);
            }
        }
        if let Some(k) = d.get(b"K".as_slice()) {
            let kids = match doc.deref(k).unwrap() {
                Object::Array(a) => a,
                o => vec![o],
            };
            for kid in kids {
                check(&kid);
                if let Object::Dict(kd) = doc.deref(&kid).unwrap() {
                    for k in ["Obj", "Stm", "Pg"] {
                        if let Some(o) = kd.get(k.as_bytes()) {
                            check(o);
                        }
                    }
                }
            }
        }
    }
    let root = doc
        .deref(doc.catalog().get(b"StructTreeRoot".as_slice()).unwrap())
        .unwrap();
    if let Some(pt) = root.as_dict().unwrap().get(b"ParentTree".as_slice()) {
        let mut stack = vec![pt.clone()];
        while let Some(n) = stack.pop() {
            let n = doc.deref(&n).unwrap();
            let n = n.as_dict().unwrap();
            if let Some(nums) = n.get(b"Nums".as_slice()) {
                for pair in doc.deref(nums).unwrap().as_array().unwrap().chunks(2) {
                    check(&pair[1]);
                    if let Object::Array(a) = doc.deref(&pair[1]).unwrap() {
                        a.iter().for_each(check);
                    }
                }
            }
            if let Some(k) = n.get(b"Kids".as_slice()) {
                stack.extend(doc.deref(k).unwrap().as_array().unwrap().iter().cloned());
            }
        }
    }
}

#[test]
fn a_single_mcid_element_loses_its_text_and_the_tree_stays() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "[31 0 R 32 0 R]",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                31,
                b"<< /Type /StructElem /S /P /P 30 0 R /Pg 3 0 R /K 0 \
                  /ActualText (SECRETTEXT) >>"
                    .to_vec(),
            ),
            (
                32,
                b"<< /Type /StructElem /S /H1 /P 30 0 R /Pg 3 0 R /K 1 \
                  /ActualText (PUBLICWORD) >>"
                    .to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 1);
    assert!(!r.structure_tree_removed);
    assert!(!contains(&r.output, b"SECRETTEXT"));
    let els = elements(&r.output);
    assert_eq!(els.len(), 2, "every element is kept");
    assert!(!has_text(elem(&els, "P")));
    assert!(
        elem(&els, "H1").contains_key(b"ActualText".as_slice()),
        "an element outside keeps its text"
    );
    let doc = Document::from_bytes(&r.output).unwrap();
    assert!(doc.catalog().contains_key(b"MarkInfo".as_slice()));
    tree_resolves(&r.output);
}

/// An element whose content lies partly inside the rect loses all of
/// its text: which part describes the removed content is not known.
#[test]
fn an_element_across_the_rect_loses_all_its_text() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "31 0 R",
        "<< /Nums [0 [31 0 R 31 0 R]] >>",
        vec![(
            31,
            b"<< /Type /StructElem /S /P /P 30 0 R /Pg 3 0 R /K [0 1] \
              /Alt (SECRETALT) /ActualText (SECRETTEXT PUBLICWORD) /E (SECRETEXP) \
              /T (SECRETTITLE) /Lang (en) >>"
                .to_vec(),
        )],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 4);
    let els = elements(&r.output);
    let p = elem(&els, "P");
    assert!(!has_text(p));
    assert!(p.contains_key(b"Lang".as_slice()), "other keys stay");
    for s in [
        &b"SECRETALT"[..],
        b"SECRETTEXT",
        b"SECRETEXP",
        b"SECRETTITLE",
    ] {
        assert!(!contains(&r.output, s));
    }
    assert!(contains(&r.output, b"PUBLICWORD"), "the kept text is drawn");
    tree_resolves(&r.output);
}

/// Ancestors of a touched element lose their text; a sibling and its
/// subtree keep theirs.
#[test]
fn nested_elements() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "33 0 R",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                33,
                b"<< /S /Sect /P 30 0 R /K [34 0 R 32 0 R] /Alt (SECTIONALT) >>".to_vec(),
            ),
            (
                34,
                b"<< /S /Div /P 33 0 R /K 31 0 R /ActualText (DIVTEXT) >>".to_vec(),
            ),
            (
                31,
                b"<< /S /Figure /P 34 0 R /Pg 3 0 R /K 0 /Alt (FIGUREALT) >>".to_vec(),
            ),
            (
                32,
                b"<< /S /Caption /P 33 0 R /Pg 3 0 R /K 1 /Alt (CAPTIONALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 3);
    let els = elements(&r.output);
    for s in ["Sect", "Div", "Figure"] {
        assert!(!has_text(elem(&els, s)), "/{s}");
    }
    assert!(has_text(elem(&els, "Caption")));
    tree_resolves(&r.output);
}

/// An element shared by two parents touches both.
#[test]
fn a_shared_element_touches_every_parent() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "[33 0 R 34 0 R]",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                33,
                b"<< /S /Sect /P 30 0 R /K 31 0 R /Alt (ONEALT) >>".to_vec(),
            ),
            (
                34,
                b"<< /S /Art /P 30 0 R /K [31 0 R 32 0 R] /Alt (TWOALT) >>".to_vec(),
            ),
            (31, b"<< /S /Figure /P 33 0 R /Pg 3 0 R /K 0 >>".to_vec()),
            (
                32,
                b"<< /S /Caption /P 34 0 R /Pg 3 0 R /K 1 /Alt (CAPTIONALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    let els = elements(&r.output);
    assert!(!has_text(elem(&els, "Sect")));
    assert!(!has_text(elem(&els, "Art")));
    assert!(has_text(elem(&els, "Caption")));
    tree_resolves(&r.output);
}

/// `/ParentTree` as a tree of indirect nodes and arrays; an element
/// reachable only through it (not from the root's `/K`) is edited too.
#[test]
fn parent_tree_through_indirect_nodes() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "32 0 R",
        "40 0 R",
        vec![
            (40, b"<< /Kids [41 0 R] >>".to_vec()),
            (41, b"<< /Limits [0 0] /Nums [0 42 0 R] >>".to_vec()),
            (42, b"[31 0 R 32 0 R]".to_vec()),
            (
                31,
                b"<< /S /Figure /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETALT) >>".to_vec(),
            ),
            (
                32,
                b"<< /S /Caption /P 30 0 R /Pg 3 0 R /K 1 /Alt (CAPTIONALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 1);
    assert!(!contains(&r.output, b"SECRETALT"));
    assert!(contains(&r.output, b"CAPTIONALT"));
    tree_resolves(&r.output);
}

/// Marked content named in `/Properties` (`/P0 BDC`) is matched by its
/// `MCID` there; an element whose page is not known (no `/Pg`) is
/// matched by number.
#[test]
fn named_properties_and_unknown_pages() {
    let pdf = tagged(
        "/P /MC0 BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC \
         /P <</MCID 1>> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC",
        "/Properties << /MC0 << /MCID 0 >> >>",
        "",
        "[31 0 R 32 0 R]",
        "<< /Nums [] >>",
        vec![
            (31, b"<< /S /P /P 30 0 R /K 0 /Alt (SECRETALT) >>".to_vec()),
            (32, b"<< /S /H1 /P 30 0 R /K 1 /Alt (PUBLICALT) >>".to_vec()),
        ],
    );
    let r = run(&pdf).unwrap();
    let els = elements(&r.output);
    assert!(!has_text(elem(&els, "P")));
    assert!(has_text(elem(&els, "H1")));
}

/// A tagged link removed with the rect: its `/OBJR` leaves the element,
/// whose text goes, and its `/ParentTree` entry goes.
#[test]
fn a_removed_link_leaves_the_tree() {
    let pdf = tagged(
        TWO,
        "",
        "/Annots [50 0 R 51 0 R]",
        "[31 0 R 32 0 R]",
        "<< /Nums [0 [31 0 R 33 0 R] 1 31 0 R 2 32 0 R] >>",
        vec![
            (
                50,
                b"<< /Type /Annot /Subtype /Link /Rect [50 95 120 110] /StructParent 1 \
                  /A << /S /URI /URI (https://example.com/SECRETLINK) >> >>"
                    .to_vec(),
            ),
            (
                51,
                b"<< /Type /Annot /Subtype /Link /Rect [10 10 40 20] /StructParent 2 \
                  /A << /S /URI /URI (https://example.com/public) >> >>"
                    .to_vec(),
            ),
            (
                31,
                b"<< /S /Link /P 30 0 R /Pg 3 0 R /Alt (SECRETLINKALT) \
                  /K [0 << /Type /OBJR /Obj 50 0 R >>] >>"
                    .to_vec(),
            ),
            (
                32,
                b"<< /S /Link /P 30 0 R /Pg 3 0 R /Alt (PUBLICLINKALT) \
                  /K << /Type /OBJR /Obj 51 0 R >> >>"
                    .to_vec(),
            ),
            (33, b"<< /S /Span /P 30 0 R /Pg 3 0 R /K 1 >>".to_vec()),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.links_removed, 1);
    assert_eq!(r.structure_items_removed, 1);
    assert!(!contains(&r.output, b"SECRETLINK"));
    assert!(contains(&r.output, b"PUBLICLINKALT"));
    tree_resolves(&r.output);
    let doc = Document::from_bytes(&r.output).unwrap();
    let root = doc
        .deref(doc.catalog().get(b"StructTreeRoot".as_slice()).unwrap())
        .unwrap();
    let pt = doc
        .deref(
            root.as_dict()
                .unwrap()
                .get(b"ParentTree".as_slice())
                .unwrap(),
        )
        .unwrap();
    let nums = doc
        .deref(pt.as_dict().unwrap().get(b"Nums".as_slice()).unwrap())
        .unwrap();
    let keys: Vec<i64> = nums
        .as_array()
        .unwrap()
        .chunks(2)
        .map(|p| p[0].as_i64().unwrap())
        .collect();
    assert_eq!(keys, vec![0, 2], "the removed link's entry goes");
}

/// A link reached only by `/StructParent` (no `/OBJR` for it in the
/// tree) still touches its element.
#[test]
fn a_removed_link_known_only_by_parent_tree() {
    let pdf = tagged(
        "BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET",
        "",
        "/Annots [50 0 R]",
        "31 0 R",
        "<< /Nums [1 31 0 R] >>",
        vec![
            (
                50,
                b"<< /Type /Annot /Subtype /Link /Rect [50 95 120 110] /StructParent 1 \
                  /A << /S /URI /URI (https://example.com/x) >> >>"
                    .to_vec(),
            ),
            (
                31,
                b"<< /S /Link /P 30 0 R /Alt (SECRETLINKALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert!(!contains(&r.output, b"SECRETLINKALT"));
}

/// Text in a form XObject tagged through the form's `/StructParents`
/// and an `/MCR /Stm`: the element loses its text, the reference to the
/// edited original goes, and the copy drawn instead carries no
/// `/StructParents`.
#[test]
fn form_content() {
    let pdf = tagged(
        "q /Fm1 Do Q /P <</MCID 0>> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC",
        "/XObject << /Fm1 60 0 R >>",
        "",
        "[31 0 R 32 0 R]",
        "<< /Nums [0 [32 0 R] 1 [31 0 R]] /ParentTreeNextKey 2 >>",
        vec![
            (
                60,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200] /StructParents 1 \
                     /Resources << /Font << /F1 20 0 R >> >>",
                    b"/P <</MCID 0>> BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC",
                ),
            ),
            (
                31,
                b"<< /S /P /P 30 0 R /Pg 3 0 R /Alt (SECRETALT) \
                  /K << /Type /MCR /Stm 60 0 R /MCID 0 >> >>"
                    .to_vec(),
            ),
            (
                32,
                b"<< /S /H1 /P 30 0 R /Pg 3 0 R /K 0 /Alt (PUBLICALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.forms_edited, 1);
    assert_eq!(r.structure_items_removed, 1);
    assert!(!contains(&r.output, b"SECRETTEXT") && !contains(&r.output, b"SECRETALT"));
    let els = elements(&r.output);
    assert!(!has_text(elem(&els, "P")));
    assert!(has_text(elem(&els, "H1")), "page MCID 0 is not the form's");
    tree_resolves(&r.output);
    let doc = Document::from_bytes(&r.output).unwrap();
    let size = doc.trailer()[b"Size".as_slice()].as_i64().unwrap() as u32;
    for n in 1..size {
        if let Ok(Object::Stream { dict, .. }) = doc.resolve(kasane_pdf::IndirectRef {
            number: n,
            generation: 0,
        }) {
            assert!(!dict.contains_key(b"StructParents".as_slice()));
        }
    }
}

/// A form without `/StructParents` has no MCIDs of its own: a tree
/// naming its MCID as the page's is matched.
#[test]
fn an_untagged_form_matches_page_mcids() {
    let pdf = tagged(
        "q /Fm1 Do Q /P <</MCID 1>> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC",
        "/XObject << /Fm1 60 0 R >>",
        "",
        "[31 0 R 32 0 R]",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                60,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 200 200] \
                     /Resources << /Font << /F1 20 0 R >> >>",
                    b"/P <</MCID 0>> BDC BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET EMC",
                ),
            ),
            (
                31,
                b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETALT) >>".to_vec(),
            ),
            (
                32,
                b"<< /S /H1 /P 30 0 R /Pg 3 0 R /K 1 /Alt (PUBLICALT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert!(!contains(&r.output, b"SECRETALT"));
    assert!(contains(&r.output, b"PUBLICALT"));
}

/// Edited content outside every tagged sequence relates to no element:
/// the tree keeps its text, and text equal to what was removed is
/// reported elsewhere.
#[test]
fn untagged_content_leaves_the_tree_alone() {
    let pdf = tagged(
        "BT /F1 12 Tf 50 100 Td (SECRETTEXT) Tj ET \
         /P <</MCID 0>> BDC BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET EMC",
        "",
        "",
        "31 0 R",
        "<< /Nums [0 [31 0 R]] >>",
        vec![(
            31,
            b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETTEXT) >>".to_vec(),
        )],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 0);
    assert!(r
        .elsewhere
        .iter()
        .any(|e| matches!(e.location, kasane_pdf::ElsewhereLocation::Object(_))));
    let doc = Document::from_bytes(&pdf).unwrap();
    let e = unsupported(redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().refuse_copies_elsewhere(true),
    ));
    assert!(e.contains("remains elsewhere"), "{e}");
}

/// A removed text kept by an untouched element is reported, not a
/// verification failure.
#[test]
fn the_same_text_on_an_untouched_element_is_reported() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "[31 0 R 32 0 R]",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                31,
                b"<< /S /Figure /P 30 0 R /Pg 3 0 R /K 0 /Alt (COMPANYLOGO) >>".to_vec(),
            ),
            (
                32,
                b"<< /S /Figure /P 30 0 R /Pg 3 0 R /K 1 /Alt (COMPANYLOGO) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, 1);
    assert!(r.elsewhere.iter().any(|e| e.removed
        == kasane_pdf::Removed::Text("COMPANYLOGO".into())
        && matches!(e.location, kasane_pdf::ElsewhereLocation::Object(_))));
}

#[test]
fn malformed_trees() {
    let with = |k31: &str, extra: Vec<(u32, Vec<u8>)>| {
        let mut objs = vec![(31, k31.as_bytes().to_vec())];
        objs.extend(extra);
        tagged(TWO, "", "", "31 0 R", "<< /Nums [0 [31 0 R]] >>", objs)
    };
    // A cycle among elements.
    let cyclic = with(
        "<< /S /Sect /P 30 0 R /K 32 0 R /Alt (X) >>",
        vec![(32, b"<< /S /Div /P 31 0 R /K [0 31 0 R] >>".to_vec())],
    );
    assert!(matches!(run(&cyclic), Err(Error::CycleDetected { .. })));
    // A marked-content reference without an MCID.
    let e = unsupported(run(&with(
        "<< /S /P /P 30 0 R /Pg 3 0 R /K << /Type /MCR /Pg 3 0 R >> /Alt (X) >>",
        vec![],
    )));
    assert!(e.contains("without an MCID"), "{e}");
    // An object reference whose /Obj is not a reference.
    let e = unsupported(run(&with(
        "<< /S /P /P 30 0 R /K << /Type /OBJR /Obj 5 >> >>",
        vec![],
    )));
    assert!(e.contains("/Obj"), "{e}");
    // A kid of an unknown kind.
    let e = unsupported(run(&with("<< /S /P /P 30 0 R /K (x) >>", vec![])));
    assert!(e.contains("unknown kind"), "{e}");
    // A cycle in /ParentTree.
    let pdf = tagged(
        TWO,
        "",
        "",
        "31 0 R",
        "40 0 R",
        vec![
            (40, b"<< /Kids [41 0 R] >>".to_vec()),
            (41, b"<< /Kids [40 0 R] >>".to_vec()),
            (31, b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 >>".to_vec()),
        ],
    );
    assert!(matches!(run(&pdf), Err(Error::CycleDetected { .. })));
    // A touched element written directly inside another.
    let e = unsupported(run(&with(
        "<< /S /Sect /P 30 0 R /K << /S /P /Pg 3 0 R /K 0 /Alt (SECRETALT) >> >>",
        vec![],
    )));
    assert!(e.contains("directly"), "{e}");
    // A dangling kid carries nothing: kept as it is.
    let r = run(&with(
        "<< /S /P /P 30 0 R /Pg 3 0 R /K [0 99 0 R] /Alt (SECRETALT) >>",
        vec![],
    ))
    .unwrap();
    assert!(!contains(&r.output, b"SECRETALT"));
}

/// The whole tree can still be removed on request.
#[test]
fn remove_structure_tree_still_removes_the_tree() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "31 0 R",
        "<< /Nums [0 [31 0 R]] >>",
        vec![(
            31,
            b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETALT) >>".to_vec(),
        )],
    );
    let doc = Document::from_bytes(&pdf).unwrap();
    let r = redact(
        &doc,
        &[(0, vec![RECT])],
        &RedactOptions::new().remove_structure_tree(true),
    )
    .unwrap();
    assert!(r.structure_tree_removed);
    assert_eq!(r.structure_texts_removed, 0);
    let out = Document::from_bytes(&r.output).unwrap();
    assert!(!out.catalog().contains_key(b"StructTreeRoot".as_slice()));
}

/// A deep and a wide tree: every ancestor of a touched element is
/// reached, the wide tree is read in linear time, and the walk is
/// bounded by `max_audit_elements`. (Deeper than the writer's
/// `max_rewrite_walk_depth` the output cannot be written.)
#[test]
fn large_and_deep_trees() {
    let deep = 200u32;
    let wide = 20_000u32;
    let mut objs = Vec::new();
    // A chain: element i's kids are element i + 1 and, on the last,
    // MCID 0.
    for i in 0..deep {
        let k = if i + 1 < deep {
            format!("{} 0 R", 1000 + i + 1)
        } else {
            "0".into()
        };
        objs.push((
            1000 + i,
            format!("<< /S /Div /Pg 3 0 R /K {k} /Alt (A{i}) >>").into_bytes(),
        ));
    }
    // Siblings on MCID 1, outside the rect.
    let ids: Vec<String> = (0..wide).map(|i| format!("{} 0 R", 2000 + i)).collect();
    for i in 0..wide {
        objs.push((
            2000 + i,
            format!("<< /S /Span /Pg 3 0 R /K 1 /Alt (B{i}) >>").into_bytes(),
        ));
    }
    let pdf = tagged(
        TWO,
        "",
        "",
        &format!("[1000 0 R {}]", ids.join(" ")),
        "<< /Nums [] >>",
        objs,
    );
    let t = std::time::Instant::now();
    let r = run(&pdf).unwrap();
    assert_eq!(r.structure_texts_removed, deep as u64, "every ancestor");
    assert!(t.elapsed().as_secs() < 60);
    let doc = Document::from_bytes(&pdf).unwrap();
    let small = kasane_pdf::Limits::default().with_max_audit_elements(1000);
    assert!(matches!(
        redact(
            &doc,
            &[(0, vec![RECT])],
            &RedactOptions::new().with_limits(small)
        ),
        Err(Error::LimitExceeded { .. })
    ));
}

/// Attribute objects carry text too: a table's `/Summary`, direct,
/// indirect, in an array with revisions, or through `/ClassMap`. A
/// touched element's go; layout attributes and untouched elements'
/// stay. (Independent review, P0.)
#[test]
fn attribute_strings_of_touched_elements() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "[31 0 R 32 0 R 33 0 R] /ClassMap << /Cls << /O /Table /Summary (SECRETCLASS) >> >>",
        "<< /Nums [0 [31 0 R 32 0 R]] >>",
        vec![
            (
                31,
                b"<< /S /Table /P 30 0 R /Pg 3 0 R /K 0 \
                  /A << /O /Table /Summary (SECRETSUMMARY) /Width 100 >> >>"
                    .to_vec(),
            ),
            (
                32,
                b"<< /S /Table /P 30 0 R /Pg 3 0 R /K 1 \
                  /A << /O /Table /Summary (PUBLICSUMMARY) >> >>"
                    .to_vec(),
            ),
            (
                33,
                b"<< /S /Figure /P 30 0 R /Pg 3 0 R /K 0 /C /Cls /A [40 0 R 0] >>".to_vec(),
            ),
            (
                40,
                b"<< /O /Layout /Placement /Block /Summary (SECRETINDIRECT) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    for s in [&b"SECRETSUMMARY"[..], b"SECRETINDIRECT", b"SECRETCLASS"] {
        assert!(!contains(&r.output, s), "{}", String::from_utf8_lossy(s));
    }
    assert!(contains(&r.output, b"PUBLICSUMMARY"));
    let els = elements(&r.output);
    let doc = Document::from_bytes(&r.output).unwrap();
    let a = doc.deref(&elem(&els, "Table")[b"A".as_slice()]).unwrap();
    assert!(
        a.as_dict().unwrap().contains_key(b"Width".as_slice()),
        "layout attributes stay"
    );
    tree_resolves(&r.output);
}

/// Elements reached only through `/IDTree`, or through a `/ParentTree`
/// entry no page names, are matched by their own `/K` too.
/// (Independent review, P0.)
#[test]
fn elements_outside_the_root_kids() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "31 0 R /IDTree << /Names [(alias) 32 0 R] >>",
        "<< /Nums [0 [31 0 R] 7 [33 0 R]] >>",
        vec![
            (
                31,
                b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETALT) >>".to_vec(),
            ),
            (
                32,
                b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /ID (alias) /Alt (SECRETALIAS) >>".to_vec(),
            ),
            (
                33,
                b"<< /S /P /P 30 0 R /Pg 3 0 R /K 0 /Alt (SECRETOTHER) >>".to_vec(),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    for s in [&b"SECRETALT"[..], b"SECRETALIAS", b"SECRETOTHER"] {
        assert!(!contains(&r.output, s), "{}", String::from_utf8_lossy(s));
    }
}

/// PDF 2.0 element text and files: `/Phoneme` and associated files
/// (`/AF`) of a touched element go.
#[test]
fn phonemes_and_associated_files() {
    let pdf = tagged(
        TWO,
        "",
        "",
        "31 0 R",
        "<< /Nums [0 [31 0 R]] >>",
        vec![
            (
                31,
                b"<< /S /Formula /P 30 0 R /Pg 3 0 R /K 0 /Phoneme (SECRETSOUND) \
                  /PhoneticAlphabet /ipa /AF [40 0 R] >>"
                    .to_vec(),
            ),
            (
                40,
                b"<< /Type /Filespec /F (f.mml) /AFRelationship /Source /EF << /F 41 0 R >> >>"
                    .to_vec(),
            ),
            (
                41,
                stream_obj("/Type /EmbeddedFile", b"<math>SECRETSOURCE</math>"),
            ),
        ],
    );
    let r = run(&pdf).unwrap();
    assert!(!contains(&r.output, b"SECRETSOUND"));
    assert!(!contains(&r.output, b"SECRETSOURCE"));
    assert_eq!(r.structure_items_removed, 1);
}
