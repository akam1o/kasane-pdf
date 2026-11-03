//! `rewrite_with` under every executable-content policy, on action graphs
//! built from the input: owners (OpenAction, catalog / page / annotation
//! / field `/AA`, annotation and outline `/A`, `/Names /JavaScript`),
//! action objects of every class, an unknown `/S`, a missing `/S` and
//! malformed values, `/Next` single or array with sharing, cycles and
//! long chains, scripts as strings, Flate streams or undecodable streams,
//! XFA packets with `script` elements, and optionally a usage-rights
//! signature, a signed field, `/RichMedia`, an outline cycle.
//!
//! Oracles, independent of the classifier:
//! - the default is `rewrite`: the same bytes or the same error;
//! - a strict (`RefuseScriptsAndExternal`) success has no dictionary
//!   with a script or external `/S` or a `/JS` entry, nor an XFA
//!   `script` element, reachable *anywhere* in the output graph — no
//!   route around the classification;
//! - a `Carry` success keeps the catalog graph identical;
//! - signatures and `/RichMedia` are refused under every policy;
//! - the report never holds script text; its output objects resolve.
//!
//! With bit 7 of the first byte one byte is corrupted: then only no
//! panic / hang / runaway allocation.
#![no_main]

use kasane_pdf::{
    rewrite, rewrite_with, Document, ExecutableContentPolicy as P, Object, OpenOptions,
    RefusalReason, RewriteOptions,
};
use kasane_pdf_fuzz::limits;
use libfuzzer_sys::fuzz_target;
use std::collections::HashSet;

struct Input<'a>(std::slice::Iter<'a, u8>);
impl Input<'_> {
    fn byte(&mut self) -> u8 {
        self.0.next().copied().unwrap_or(0)
    }
}

/// Action numbers 500.. ; generic objects 10.. never point at them.
const ACT: u32 = 500;
const KINDS: [&str; 12] = [
    "/S /JavaScript /JS (SECRET-js)",
    "/S /JavaScript /JS 900 0 R",
    "/S /JavaScript /JS 901 0 R",
    "/S /URI /URI (SECRET-uri)",
    "/S /Launch /F (SECRET.exe)",
    "/S /SubmitForm /F (u)",
    "/S /GoTo /D [3 0 R /Fit]",
    "/S /Named /N /NextPage",
    "/S /Hide /T (a)",
    "/S /VendorMagic",
    "/Foo 1",
    "/S /Rendition /OP 0",
];
const BAD: &[&[u8]] = &[b"JavaScript", b"URI", b"Launch", b"SubmitForm", b"ImportData", b"GoToR", b"GoToE"];

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn stored_zlib(d: &[u8]) -> Vec<u8> {
    let len = d.len() as u16;
    let mut v = vec![0x78, 0x01, 0x01];
    v.extend_from_slice(&len.to_le_bytes());
    v.extend_from_slice(&(!len).to_le_bytes());
    v.extend_from_slice(d);
    v.extend_from_slice(&adler32(d).to_be_bytes());
    v
}

fn stream(filter: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {filter} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// Whether a dictionary with a script or external `/S` or a `/JS` entry,
/// or an XFA `script` element, is reachable from the output's catalog —
/// in any position, owner or not. (An unknown action out of an owner
/// position is not recognisable as an action; in owner positions the
/// classification refuses it, tested elsewhere.)
fn forbidden_reachable(doc: &Document) -> bool {
    let mut seen = HashSet::new();
    let mut stack: Vec<Object> = vec![Object::Dict(doc.catalog().clone())];
    let mut nodes = 0;
    while let Some(o) = stack.pop() {
        nodes += 1;
        if nodes > 200_000 {
            return false;
        }
        let o = match o {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                match doc.resolve(r) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            }
            o => o,
        };
        match o {
            Object::Dict(d) => {
                if let Some(s) = d.get(b"S".as_slice()).and_then(|s| s.as_name()) {
                    if BAD.contains(&s) {
                        return true;
                    }
                }
                if d.contains_key(b"JS".as_slice()) {
                    return true;
                }
                stack.extend(d.into_values());
            }
            Object::Array(a) => stack.extend(a),
            Object::Stream { dict, data } => {
                if data.windows(7).any(|w| w == b"<script" || w == b":script") {
                    return true;
                }
                stack.extend(dict.into_values());
            }
            _ => {}
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let flags = inp.byte();
    let flags2 = inp.byte();
    let n = 1 + (inp.byte() % 24) as u32;
    let aref = |inp: &mut Input| -> String {
        let k = inp.byte() as u32;
        match k % 16 {
            0 => "999 0 R".into(),       // undefined
            1 => "42".into(),            // malformed
            _ => format!("{} 0 R", ACT + k % n),
        }
    };
    let mut objs: Vec<(u32, Vec<u8>)> = Vec::new();
    // Actions, each with an optional /Next (single or array).
    for i in 0..n {
        let kind = KINDS[inp.byte() as usize % KINDS.len()];
        let next = match inp.byte() % 5 {
            0 | 1 => String::new(),
            2 => format!("/Next {}", aref(&mut inp)),
            3 => format!("/Next [{} {}]", aref(&mut inp), aref(&mut inp)),
            // A chain step to the next action: long chains.
            _ => format!("/Next {} 0 R", ACT + (i + 1) % n),
        };
        objs.push((ACT + i, format!("<< {kind} {next} >>").into_bytes()));
    }
    objs.push((900, stream("/Filter /FlateDecode", &stored_zlib(b"SECRET-stream"))));
    objs.push((901, stream("/Filter /FlateDecode", b"SECRET not zlib")));
    // Owners.
    let mut cat = String::new();
    let mut page = String::new();
    let oa = inp.byte();
    if oa % 4 == 1 {
        cat.push_str(&format!(" /OpenAction {}", aref(&mut inp)));
    } else if oa % 4 == 2 {
        cat.push_str(" /OpenAction [3 0 R /Fit]");
    } else if oa % 4 == 3 {
        cat.push_str(&format!(" /OpenAction [{} {}]", aref(&mut inp), aref(&mut inp)));
    }
    if flags & 1 != 0 {
        cat.push_str(&format!(" /AA << /WC {} /Zz {} >>", aref(&mut inp), aref(&mut inp)));
    }
    if flags & 2 != 0 {
        page.push_str(&format!(" /AA << /O {} >>", aref(&mut inp)));
    }
    let annots = inp.byte() % 3;
    let mut list = Vec::new();
    for k in 0..annots {
        let num = 30 + k as u32;
        let sub = ["Link", "Screen", "Widget"][inp.byte() as usize % 3];
        objs.push((
            num,
            format!(
                "<< /Type /Annot /Subtype /{sub} /Rect [0 0 1 1] /A {} /AA << /U {} >> >>",
                aref(&mut inp),
                aref(&mut inp)
            )
            .into_bytes(),
        ));
        list.push(format!("{num} 0 R"));
    }
    if !list.is_empty() {
        page.push_str(&format!(" /Annots [{}]", list.join(" ")));
    }
    if flags & 4 != 0 {
        objs.push((40, format!("<< /T (SECRET-field) /AA << /K {} >> /Kids [41 0 R] >>", aref(&mut inp)).into_bytes()));
        objs.push((41, format!("<< /Parent 40 0 R /AA << /F {} >> >>", aref(&mut inp)).into_bytes()));
    }
    let mut acroform = if flags & 4 != 0 { "/Fields [40 0 R]".to_string() } else { "/Fields []".to_string() };
    if flags & 8 != 0 {
        let cyc = if flags2 & 1 != 0 { "/Next 51 0 R" } else { "" };
        objs.push((50, b"<< /First 51 0 R >>".to_vec()));
        objs.push((51, format!("<< /Title (SECRET-title) /A {} {cyc} >>", aref(&mut inp)).into_bytes()));
        cat.push_str(" /Outlines 50 0 R");
    }
    if flags & 0x10 != 0 {
        cat.push_str(&format!(" /Names << /JavaScript << /Names [(SECRET-key) {}] >> >>", aref(&mut inp)));
    }
    let xfa = flags & 0x20 != 0;
    if xfa {
        let p = [&b"<template><script>SECRET()</script></template>"[..], b"<a/>", b"<x:script/>", b"<scr"][inp.byte() as usize % 4];
        let q = [&b"ipt>x</script>"[..], b"<b/>"][inp.byte() as usize % 2];
        objs.push((60, stream("", p)));
        objs.push((61, stream("", q)));
        acroform.push_str(" /XFA [(template) 60 0 R (datasets) 61 0 R]");
    }
    let ur3 = flags & 0x40 != 0;
    if ur3 {
        cat.push_str(" /Perms << /UR3 << /Type /Sig >> >>");
    }
    let signed = flags2 & 2 != 0;
    if signed {
        objs.push((70, b"<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /V << /ByteRange [0 1 2 3] >> >>".to_vec()));
        page.push_str(" /Foo [70 0 R]");
        acroform = acroform.replacen("/Fields [", "/Fields [70 0 R ", 1);
    }
    let richmedia = flags2 & 4 != 0;
    if richmedia {
        cat.push_str(" /RichMedia << >>");
    }
    let mut all: Vec<(u32, Vec<u8>)> = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << {acroform} >> {cat} >>").into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] {page} >>").into_bytes()),
    ];
    all.extend(objs);
    all.sort_by_key(|o| o.0);
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows = vec![(0u32, 65535u16, b'f', 0usize)];
    for (num, body) in &all {
        rows.push((*num, 0, b'n', b.len()));
        b.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(b"\nendobj\n");
    }
    let x = b.len();
    b.extend_from_slice(b"xref\n");
    for (num, g, k, off) in &rows {
        b.extend_from_slice(format!("{num} 1\n{off:010} {g:05} {} \n", *k as char).as_bytes());
    }
    b.extend_from_slice(format!("trailer\n<< /Size 1000 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
    let corrupt = flags & 0x80 != 0;
    if corrupt {
        let pos = ((inp.byte() as usize) << 8 | inp.byte() as usize) % b.len();
        b[pos] = inp.byte();
    }
    let Ok(doc) = Document::open(&b, &OpenOptions::new().with_limits(limits())) else {
        assert!(corrupt, "the generated file must open");
        return;
    };
    let opts = |p: P| RewriteOptions::new().with_executable_content(p).with_limits(limits());
    // The default is `rewrite`.
    let old = kasane_pdf::rewrite_with_limits(&doc, &limits());
    let new = rewrite_with(&doc, &opts(P::Profile));
    match (&old, &new) {
        (Ok(a), Ok(o)) => assert_eq!(a.as_slice(), o.bytes(), "default bytes"),
        (Err(a), Err(e)) => assert_eq!(a.to_string(), e.to_string(), "default error"),
        _ => panic!("default differs from rewrite: {:?} vs {:?}", old.as_ref().err(), new.as_ref().err()),
    }
    let _ = rewrite(&doc);
    for p in [P::Profile, P::RefuseScriptsAndExternal, P::Carry] {
        let r = rewrite_with(&doc, &opts(p));
        if corrupt {
            if let Ok(o) = r {
                let _ = Document::from_bytes(o.bytes());
            }
            continue;
        }
        match r {
            Ok(out) => {
                assert!(!ur3 && !signed && !richmedia, "{p:?}: a refused document was written");
                let report = format!("{:?}", out.report());
                assert!(!report.contains("SECRET"), "{p:?}: content in the report");
                let d2 = Document::from_bytes(out.bytes()).expect("the output reparses");
                for it in out.report().executable_content() {
                    if let Some(o) = it.output_object {
                        assert!(d2.resolve(o).is_ok(), "{p:?}: output object {o:?}");
                    }
                }
                if p == P::RefuseScriptsAndExternal {
                    assert!(!forbidden_reachable(&d2), "strict policy bypassed");
                    assert!(out.report().unclassified().is_empty());
                }
                if p == P::Carry {
                    assert!(out.report().unclassified().is_empty());
                    assert_eq!(doc.catalog().keys().collect::<Vec<_>>(), d2.catalog().keys().collect::<Vec<_>>());
                }
            }
            Err(e) => {
                let why = e.refusal_reason();
                if ur3 || signed {
                    assert_eq!(why, Some(RefusalReason::Signature), "{p:?}: {e}");
                } else if richmedia {
                    assert_eq!(why, Some(RefusalReason::Multimedia), "{p:?}: {e}");
                }
                assert!(!e.to_string().contains("SECRET"), "{p:?}: content in the error");
            }
        }
    }
});
