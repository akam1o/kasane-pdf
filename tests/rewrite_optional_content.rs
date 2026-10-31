//! Rewriting documents with optional content (ISO 32000-2 §8.11).
//!
//! The rewriter carries `/OCProperties` verbatim with renumbered
//! references once its structure is well formed. Each case below is
//! checked three ways, none of them through the rewriter's own code:
//! * the rewrite reparses strictly;
//! * a renumbering-blind description of the optional-content graph —
//!   the catalog `/OCProperties`, the page `/Properties`, the form's and
//!   the annotation's `/OC`, the content stream — is identical before
//!   and after, groups named by their position in `/OCGs`;
//! * the page renders pixel-identically before and after, and (for the
//!   switching cases) the configuration does change what is shown.
//!
//! Malformed structures are refused, never written.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, rewrite, Document, Error, IndirectRef, Object, RenderOptions};
use std::collections::HashMap;

/// The page: four regions, each under a different kind of optional
/// content reference, plus a form XObject and an annotation.
///
/// * bottom-left red: `/OC /MC0` → G1 (object 10)
/// * bottom-right green: `/OC /MC1` → G2 (11)
/// * top-left black: `/OC /MD` → OCMD `/P /AllOn` [G1 G2] (30)
/// * next to it blue: `/OC /MV` → OCMD `/VE [/Not G1]` (31)
/// * the form `/Fm` (`/OC` G2) paints a grey square top-right
/// * the annotation (`/OC` G1) paints a magenta square in the corner
const CONTENT: &[u8] = b"/OC /MC0 BDC 1 0 0 rg 0 0 50 50 re f EMC \
/OC /MC1 BDC 0 1 0 rg 50 0 50 50 re f EMC \
/OC /MD BDC 0 0 0 rg 0 50 25 25 re f EMC \
/OC /MV BDC 0 0 1 rg 25 50 25 25 re f EMC \
/Fm Do";

fn pdf(ocprops: &str, extra: &[(u32, &str)]) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /OCProperties {ocprops} >>").into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /Properties << /MC0 10 0 R /MC1 11 0 R /MD 30 0 R /MV 31 0 R >> \
              /XObject << /Fm 20 0 R >> >> /Contents 4 0 R /Annots [22 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", CONTENT)),
        (10, b"<< /Type /OCG /Name (G1) >>".to_vec()),
        (11, b"<< /Type /OCG /Name (G2) >>".to_vec()),
        (12, b"<< /Type /OCG /Name (G3) /Intent [/View /Design] >>".to_vec()),
        (
            20,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /OC 11 0 R",
                b"0.5 g 50 50 25 25 re f",
            ),
        ),
        (
            22,
            b"<< /Type /Annot /Subtype /Square /Rect [75 75 100 100] /OC 10 0 R /AP << /N 23 0 R >> >>"
                .to_vec(),
        ),
        (
            23,
            stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 25 25]", b"1 0 1 rg 0 0 25 25 re f"),
        ),
        (30, b"<< /Type /OCMD /OCGs [10 0 R 11 0 R] /P /AllOn >>".to_vec()),
        (31, b"<< /Type /OCMD /OCGs [10 0 R] /VE [/Not 10 0 R] >>".to_vec()),
    ];
    for (n, body) in extra {
        if let Some(slot) = objs.iter_mut().find(|o| o.0 == *n) {
            slot.1 = body.as_bytes().to_vec();
        } else {
            objs.push((*n, body.as_bytes().to_vec()));
        }
    }
    objs.sort_by_key(|o| o.0);
    build_pdf(&objs)
}

/// A description of `o` with references followed and replaced by role:
/// optional content groups by their position in `/OCGs`, the page by
/// its index; a reference already on the path is a cycle.
fn canon(
    doc: &Document,
    o: &Object,
    groups: &HashMap<IndirectRef, usize>,
    path: &mut Vec<IndirectRef>,
) -> String {
    if let Some(r) = o.as_ref() {
        if let Some(i) = groups.get(&r) {
            // The group's own dictionary is described once, at /OCGs.
            if !path.is_empty() {
                return format!("G{i}");
            }
        }
        if doc.pages().iter().any(|p| p.object_ref() == Some(r)) {
            return "PAGE".into();
        }
        if path.contains(&r) {
            return "CYCLE".into();
        }
        path.push(r);
        let v = doc.resolve(r).unwrap();
        let s = canon(doc, &v, groups, path);
        path.pop();
        return s;
    }
    match o {
        Object::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|x| canon(doc, x, groups, path))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Object::Dict(d) => format!(
            "<<{}>>",
            d.iter()
                .filter(|(k, _)| k.as_slice() != b"Parent" && k.as_slice() != b"Length")
                .map(|(k, v)| format!(
                    "/{} {}",
                    String::from_utf8_lossy(k),
                    canon(doc, v, groups, path)
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Object::Stream { dict, data } => format!(
            "{}stream({})",
            canon(doc, &Object::Dict(dict.clone()), groups, path),
            String::from_utf8_lossy(data)
        ),
        other => format!("{other:?}"),
    }
}

/// The optional-content view of the document (see the module docs).
fn describe(doc: &Document) -> String {
    let ocp = doc
        .deref(doc.catalog().get(b"OCProperties".as_slice()).unwrap())
        .unwrap();
    let mut groups = HashMap::new();
    let ocp_d = ocp.as_dict().unwrap();
    if let Some(Object::Array(a)) = ocp_d.get(b"OCGs".as_slice()).map(|o| doc.deref(o).unwrap()) {
        for (i, g) in a.iter().enumerate() {
            if let Some(r) = g.as_ref() {
                groups.entry(r).or_insert(i);
            }
        }
    }
    let mut out = vec![format!(
        "OCProperties {}",
        canon(doc, &ocp, &groups, &mut vec![])
    )];
    let mut gs: Vec<(usize, IndirectRef)> = groups.iter().map(|(r, i)| (*i, *r)).collect();
    gs.sort();
    for (i, r) in gs {
        out.push(format!(
            "G{i} {}",
            canon(doc, &Object::Ref(r), &groups, &mut vec![])
        ));
    }
    let page = doc.resolve(doc.pages()[0].object_ref().unwrap()).unwrap();
    let pd = page.as_dict().unwrap();
    for key in [&b"Resources"[..], b"Annots", b"Contents"] {
        out.push(format!(
            "page /{} {}",
            String::from_utf8_lossy(key),
            canon(
                doc,
                pd.get(key).unwrap(),
                &groups,
                &mut vec![IndirectRef {
                    ..doc.pages()[0].object_ref().unwrap()
                }]
            )
        ));
    }
    out.join("\n")
}

fn pixels(doc: &Document) -> Vec<u8> {
    render_page(doc, &doc.pages()[0], &RenderOptions::new())
        .unwrap()
        .pixels()
        .to_vec()
}

/// Rewrite, reparse, compare; returns the rewritten document.
fn round_trip(src: &[u8], what: &str) -> Document {
    let doc = Document::from_bytes(src).unwrap();
    let out = rewrite(&doc).unwrap_or_else(|e| panic!("{what}: rewrite refused: {e}"));
    let re = Document::from_bytes(&out).unwrap_or_else(|e| panic!("{what}: reparse: {e}"));
    assert_eq!(
        describe(&doc),
        describe(&re),
        "{what}: optional content changed"
    );
    assert_eq!(pixels(&doc), pixels(&re), "{what}: rendering changed");
    re
}

/// RGB at (x, y) in a 100 × 100 render (y down).
fn rgb(doc: &Document, x: u32, y: u32) -> (u8, u8, u8) {
    let r = render_page(doc, &doc.pages()[0], &RenderOptions::new()).unwrap();
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

const WHITE: (u8, u8, u8) = (255, 255, 255);

#[test]
fn configurations_are_carried_and_keep_their_meaning() {
    let cases: [(&str, &str); 12] = [
        ("one group, ON", "<< /OCGs [10 0 R] /D << /ON [10 0 R] >> >>"),
        ("one group, OFF", "<< /OCGs [10 0 R] /D << /OFF [10 0 R] >> >>"),
        ("several groups", "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /OFF [11 0 R] >> >>"),
        (
            "several configurations",
            "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /Name (Default) /Creator (test) >> \
             /Configs [<< /Name (Print) /BaseState /OFF /ON [11 0 R] /Intent /View >> \
             << /Name (All) /BaseState /ON /ListMode /VisiblePages >>] >>",
        ),
        ("/BaseState /ON", "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /ON /OFF [11 0 R] >> >>"),
        ("/BaseState /OFF", "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /OFF /ON [11 0 R] >> >>"),
        ("/BaseState /Unchanged", "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /Unchanged >> >>"),
        ("/ON and /OFF", "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /ON [10 0 R 12 0 R] /OFF [11 0 R] >> >>"),
        (
            "nested /Order",
            "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /Order [10 0 R [(Sub) 11 0 R [12 0 R]] 40 0 R] >> >>",
        ),
        ("/RBGroups", "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /RBGroups [[10 0 R 11 0 R] [12 0 R]] >> >>"),
        ("/Locked", "<< /OCGs [10 0 R 11 0 R 12 0 R] /D << /Locked [12 0 R] >> >>"),
        (
            "/AS",
            "<< /OCGs [10 0 R 11 0 R] /D << /AS [<< /Event /View /OCGs [10 0 R] /Category [/View] >> \
             << /Event /Print /OCGs [11 0 R] /Category [/Print] >>] >> >>",
        ),
    ];
    for (what, ocp) in cases {
        let extra: &[(u32, &str)] = match what {
            // An indirect, labelled subarray of /Order.
            "nested /Order" => &[(40, "[(Indirect) 12 0 R]")],
            // /AS with usage: G1's view state OFF, G2's print state ON.
            "/AS" => &[
                (
                    10,
                    "<< /Type /OCG /Name (G1) /Usage << /View << /ViewState /OFF >> >> >>",
                ),
                (
                    11,
                    "<< /Type /OCG /Name (G2) /Usage << /Print << /PrintState /ON >> >> >>",
                ),
            ],
            _ => &[],
        };
        round_trip(&pdf(ocp, extra), what);
    }
}

#[test]
fn switching_a_group_changes_the_page_and_the_rewrite_follows() {
    // Each configuration shows a different set of regions; before and
    // after the rewrite the same.
    let on = pdf("<< /OCGs [10 0 R 11 0 R] /D << /BaseState /ON >> >>", &[]);
    let g1_off = pdf("<< /OCGs [10 0 R 11 0 R] /D << /OFF [10 0 R] >> >>", &[]);
    let g2_off = pdf(
        "<< /OCGs [10 0 R 11 0 R] /D << /BaseState /ON /OFF [11 0 R] >> >>",
        &[],
    );
    let all_off = pdf("<< /OCGs [10 0 R 11 0 R] /D << /BaseState /OFF >> >>", &[]);
    let as_off = pdf(
        "<< /OCGs [10 0 R 11 0 R] /D << /AS [<< /Event /View /OCGs [10 0 R] /Category [/View] >>] >> >>",
        &[(10, "<< /Type /OCG /Name (G1) /Usage << /View << /ViewState /OFF >> >> >>")],
    );
    // (x, y) probes: G1 red, G2 green, OCMD AllOn black, OCMD Not-G1
    // blue, form (G2) grey, annotation (G1) magenta.
    let probe = |d: &Document| {
        [(10, 90), (60, 90), (10, 40), (30, 40), (60, 40), (90, 10)].map(|(x, y)| rgb(d, x, y))
    };
    let red = (255, 0, 0);
    let green = (0, 255, 0);
    let black = (0, 0, 0);
    let blue = (0, 0, 255);
    let grey = (128, 128, 128);
    let magenta = (255, 0, 255);
    let want = [
        ("all ON", &on, [red, green, black, WHITE, grey, magenta]),
        ("G1 OFF", &g1_off, [WHITE, green, WHITE, blue, grey, WHITE]),
        (
            "G2 OFF",
            &g2_off,
            [red, WHITE, WHITE, WHITE, WHITE, magenta],
        ),
        (
            "all OFF",
            &all_off,
            [WHITE, WHITE, WHITE, blue, WHITE, WHITE],
        ),
        (
            "/AS View OFF",
            &as_off,
            [WHITE, green, WHITE, blue, grey, WHITE],
        ),
    ];
    for (what, src, expect) in want {
        let before = Document::from_bytes(src).unwrap();
        let got = probe(&before);
        for (g, e) in got.iter().zip(expect) {
            // Grey from 0.5 g may round to 127 or 128.
            assert!(
                g.0.abs_diff(e.0) <= 1 && g.1.abs_diff(e.1) <= 1 && g.2.abs_diff(e.2) <= 1,
                "{what}: {got:?}"
            );
        }
        let after = round_trip(src, what);
        assert_eq!(probe(&after), got, "{what}: after the rewrite");
    }
}

#[test]
fn shared_group_references_keep_one_target() {
    // G1 is named by /OCGs, /D /ON, /Order, /RBGroups, the page's
    // /Properties, both OCMDs and the annotation: after renumbering
    // every one of them still names the same object.
    let src = pdf(
        "<< /OCGs [10 0 R 11 0 R] /D << /ON [10 0 R] /Order [10 0 R 11 0 R] /RBGroups [[10 0 R 11 0 R]] >> >>",
        &[],
    );
    let re = round_trip(&src, "shared");
    let ocp = re
        .deref(re.catalog().get(b"OCProperties".as_slice()).unwrap())
        .unwrap();
    let d = ocp.as_dict().unwrap();
    let g1 = re
        .deref(d.get(b"OCGs".as_slice()).unwrap())
        .unwrap()
        .as_array()
        .unwrap()[0]
        .as_ref()
        .unwrap();
    let page = re.resolve(re.pages()[0].object_ref().unwrap()).unwrap();
    let res = re
        .deref(
            page.as_dict()
                .unwrap()
                .get(b"Resources".as_slice())
                .unwrap(),
        )
        .unwrap();
    let props = re
        .deref(
            res.as_dict()
                .unwrap()
                .get(b"Properties".as_slice())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        props
            .as_dict()
            .unwrap()
            .get(b"MC0".as_slice())
            .unwrap()
            .as_ref(),
        Some(g1)
    );
    let annots = re
        .deref(page.as_dict().unwrap().get(b"Annots".as_slice()).unwrap())
        .unwrap();
    let annot = re.deref(&annots.as_array().unwrap()[0]).unwrap();
    assert_eq!(
        annot
            .as_dict()
            .unwrap()
            .get(b"OC".as_slice())
            .unwrap()
            .as_ref(),
        Some(g1)
    );
    let ocmd = re
        .deref(props.as_dict().unwrap().get(b"MV".as_slice()).unwrap())
        .unwrap();
    let ve = ocmd
        .as_dict()
        .unwrap()
        .get(b"VE".as_slice())
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert_eq!(ve[1].as_ref(), Some(g1));
}

fn refused(ocp: &str, extra: &[(u32, &str)]) -> Error {
    let doc = Document::from_bytes(&pdf(ocp, extra)).unwrap();
    rewrite(&doc).expect_err("a malformed /OCProperties must be refused")
}

#[test]
fn malformed_structures_are_refused() {
    let structure = |e: Error, what: &str| {
        assert!(matches!(e, Error::Structure(_)), "{what}: {e:?}");
    };
    // References that are not groups: undefined, and the wrong type.
    structure(
        refused("<< /OCGs [10 0 R 99 0 R] /D << >> >>", &[]),
        "undefined group",
    );
    structure(
        refused("<< /OCGs [10 0 R] /D << /ON [20 0 R] >> >>", &[]),
        "a form as a group",
    );
    // Missing required entries.
    structure(refused("<< /OCGs [10 0 R] >>", &[]), "no /D");
    structure(refused("<< /D << >> >>", &[]), "no /OCGs");
    // Wrong types.
    for (what, ocp) in [
        ("/OCGs not an array", "<< /OCGs 5 /D << >> >>"),
        ("direct group", "<< /OCGs [<< /Type /OCG /Name (x) >>] /D << >> >>"),
        ("/D not a dictionary", "<< /OCGs [10 0 R] /D (x) >>"),
        ("/BaseState", "<< /OCGs [10 0 R] /D << /BaseState /Maybe >> >>"),
        ("/ListMode", "<< /OCGs [10 0 R] /D << /ListMode /Some >> >>"),
        ("/ON entry", "<< /OCGs [10 0 R] /D << /ON [1] >> >>"),
        ("/Order entry", "<< /OCGs [10 0 R] /D << /Order [10 0 R 7] >> >>"),
        ("/RBGroups", "<< /OCGs [10 0 R] /D << /RBGroups [10 0 R] >> >>"),
        ("/AS event", "<< /OCGs [10 0 R] /D << /AS [<< /Event /Hover /OCGs [10 0 R] /Category [/View] >>] >> >>"),
        ("/AS category", "<< /OCGs [10 0 R] /D << /AS [<< /Event /View /OCGs [10 0 R] >>] >> >>"),
        ("/Configs", "<< /OCGs [10 0 R] /D << >> /Configs << >> >>"),
        ("/Intent", "<< /OCGs [10 0 R] /D << /Intent 3 >> >>"),
    ] {
        structure(refused(ocp, &[]), what);
    }
    // A group without /Name.
    structure(
        refused("<< /OCGs [10 0 R] /D << >> >>", &[(10, "<< /Type /OCG >>")]),
        "no /Name",
    );
}

#[test]
fn cycles_and_limits_end_in_a_refusal() {
    // An indirect /Order array that contains itself.
    let e = refused(
        "<< /OCGs [10 0 R] /D << /Order [40 0 R] >> >>",
        &[(40, "[10 0 R 40 0 R]")],
    );
    assert!(matches!(e, Error::Structure(_)), "{e:?}");
    // An /Order nested deeper than the limit.
    let deep = format!("{}10 0 R{}", "[".repeat(40), "]".repeat(40));
    let e = refused(&format!("<< /OCGs [10 0 R] /D << /Order {deep} >> >>"), &[]);
    assert!(matches!(e, Error::LimitExceeded { .. }), "{e:?}");
}

#[test]
fn indirect_values_and_null_entries_are_accepted() {
    // §7.3.10: any value may be indirect, and a null array entry is the
    // null object — the renderer skips it, the rewriter carries it.
    for (what, ocp, extra) in [
        ("null in /OCGs and /ON", "<< /OCGs [10 0 R null 11 0 R] /D << /ON [null 10 0 R] >> >>", vec![]),
        ("indirect /Order label", "<< /OCGs [10 0 R 11 0 R] /D << /Order [[50 0 R 10 0 R] 11 0 R] >> >>", vec![(50, "(Layer set)")]),
        (
            "/AS without /OCGs (optional), unknown keys kept",
            "<< /OCGs [10 0 R] /D << /AS [<< /Event /View /Category [/Zoom] >>] /XYZ_Vendor (kept) >> /XYZ_Ext 3 >>",
            vec![],
        ),
        (
            "indirect names",
            "<< /OCGs [10 0 R] /D << /Intent [51 0 R] /AS [<< /Event 52 0 R /OCGs [10 0 R] /Category [51 0 R] >>] >> >>",
            vec![(51, "/View"), (52, "/View")],
        ),
    ] {
        round_trip(&pdf(ocp, &extra), what);
    }
}

#[test]
fn optional_content_is_declared_at_least_pdf_1_5() {
    // A 1.4 header with /OCProperties: the rewrite declares 1.5, the
    // first version that defines optional content; 1.6 stays 1.6.
    let src = pdf("<< /OCGs [10 0 R] /D << >> >>", &[]);
    for (header, want) in [
        (&b"%PDF-1.4"[..], &b"%PDF-1.5"[..]),
        (b"%PDF-1.6", b"%PDF-1.6"),
    ] {
        let mut v = src.clone();
        v[..8].copy_from_slice(header);
        let out = rewrite(&Document::from_bytes(&v).unwrap()).unwrap();
        assert_eq!(&out[..8], want, "{}", String::from_utf8_lossy(header));
    }
}

#[test]
fn the_version_floor_combines_with_the_catalog_version() {
    // Effective version = max(header, catalog /Version), then at least
    // 1.5 for optional content; 2.0 stays refused.
    let ocp = "<< /OCGs [10 0 R] /D << >> >>";
    for (cat_version, want) in [
        ("/1.4", Some(&b"%PDF-1.5"[..])),
        ("/1.6", Some(b"%PDF-1.6")),
        ("/2.0", None),
    ] {
        let catalog =
            format!("<< /Type /Catalog /Pages 2 0 R /Version {cat_version} /OCProperties {ocp} >>");
        let mut v = pdf(ocp, &[(1, &catalog)]);
        v[..8].copy_from_slice(b"%PDF-1.4");
        let r = rewrite(&Document::from_bytes(&v).unwrap());
        match want {
            Some(h) => assert_eq!(&r.unwrap()[..8], h, "catalog {cat_version}"),
            None => assert!(r.is_err(), "catalog {cat_version} must be refused"),
        }
    }
}
