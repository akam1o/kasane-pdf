//! `UsageRightsPolicy::Remove` (D1) against a ground truth: documents
//! built from the input with an optional usage-rights signature (direct
//! or indirect, valid or not, shared or not, holding indirect objects)
//! and signature-like decoys of every marker (`/ByteRange`; `/Type /Sig`
//! or `/DocTimeStamp`, directly or through a reference; `/Contents` with
//! `/Filter` or `/SubFilter`; `/Cert` with `/Contents`; `/Reference` with
//! `/TransformMethod`, alone, in an array, nested, through a reference;
//! a stream) or near misses that are not markers, placed in the catalog,
//! a page, an annotation, a field `/V`, a widget outside `/Fields`, only
//! behind `/P` or `/Parent`, in `/Info`, below a seed value, behind a
//! reference chain or an object-stream alias, at the end of a deep
//! chain, or nowhere (unreachable). Optionally field locks (on a field,
//! on a widget outside `/Fields`, on a seed value itself), `/DSS`,
//! `/Legal`, an XFA XML signature, a page script, `/RichMedia`,
//! `/P`/`/Parent` cycles, a large dictionary, objects packed in an object
//! stream, tight limits.
//!
//! Oracles (the generator's knowledge, and a walker of the target's own
//! over the output — never the library's):
//! - the default is `rewrite`, bytes or error;
//! - Remove refuses (a signature refusal) whenever a reachable decoy, a
//!   lock, a store, an XFA signature, a bad or shared target, `/DocMDP`
//!   is present; and, with nothing to refuse and no script or
//!   multimedia, succeeds;
//! - a success: no signature-like dictionary reachable in the output, no
//!   `/ByteRange` anywhere in its bytes, no `/Perms /UR`, `/UR3`;
//!   `/SigFlags` is the input's with bit 1 cleared, every other bit kept;
//!   the report says removed exactly when a target was present;
//! - `RefuseScriptsAndExternal` + Remove never writes a page script;
//! - reports and errors hold no signature value or certificate.
//!
//! With bit 7 of the first byte one byte is corrupted: then only no
//! panic / hang / runaway allocation.
#![no_main]

use kasane_pdf::{
    rewrite_with, rewrite_with_limits, Document, Error, ExecutableContentPolicy as P, Limits,
    Object, OpenOptions, RewriteOptions, UsageRightsOutcome, UsageRightsPolicy as U,
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

/// Decoys: (dictionary body, signature-like).
const DECOYS: [(&str, bool); 17] = [
    ("/ByteRange [0 1 2 3]", true),
    ("/Type /Sig", true),
    ("/Type 950 0 R", true),
    ("/Type /DocTimeStamp /Contents (SECRET-ts)", true),
    ("/Contents (SECRET-c) /Filter /Adobe.PPKLite", true),
    ("/Contents (SECRET-c) /SubFilter /Vendor.x", true),
    ("/Cert (SECRET-cert) /Contents (SECRET-c)", true),
    ("/Reference [<< /TransformMethod /DocMDP >>]", true),
    ("/Reference << /TransformMethod /FieldMDP >>", true),
    ("/Reference [[<< /TransformMethod /UR3 >>]]", true),
    ("/Reference 951 0 R", true),
    ("/Contents (note)", false),
    ("/Cert (SECRET-cert)", false),
    ("/Reference [<< /Data 1 >>]", false),
    ("/Type /Annot", false),
    ("/Filter /FlateDecode", false),
    ("/Lockx 1 /SVx 2", false),
];

const UR3: &str = "/Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
    /ByteRange [0 1 2 3] /Contents (SECRET-sig) /Cert (SECRET-cert) /Reference [<< /Type /SigRef \
    /TransformMethod /UR3 /TransformParams TP >>]";
/// Target variants: (substitutions, a usage-rights signature).
const BAD_TARGETS: [(&str, &str); 5] = [
    (" /Reference [", " /Ref ["),
    ("/TransformMethod /UR3", "/TransformMethod /DocMDP"),
    ("/TransformMethod /UR3", ""),
    ("/Type /Sig", "/Type /DocTimeStamp"),
    ("/Filter /Adobe.PPKLite", ""),
];

struct Doc {
    objs: Vec<(u32, String)>,
    cat: String,
    page: String,
    annots: Vec<String>,
    fields: Vec<String>,
    acroform: String,
    info: String,
    must_refuse: bool,
    target: bool,
    script: bool,
    multimedia: bool,
    sig_flags: Option<i64>,
}

impl Doc {
    fn obj(&mut self, n: u32, body: impl Into<String>) {
        self.objs.push((n, body.into()));
    }
}

/// Signature-like, by the target's own reading (direct names, one hop).
fn marked(doc: &Document, d: &kasane_pdf::Dict) -> bool {
    let has = |k: &[u8]| d.contains_key(k);
    if has(b"ByteRange")
        || (has(b"Contents") && (has(b"Filter") || has(b"SubFilter")))
        || (has(b"Cert") && has(b"Contents"))
    {
        return true;
    }
    if let Some(t) = d.get(b"Type".as_slice()) {
        if let Ok(Object::Name(n)) = doc.deref(t) {
            if n == b"Sig" || n == b"DocTimeStamp" {
                return true;
            }
        }
    }
    fn tm(doc: &Document, o: &Object, depth: u32) -> bool {
        if depth > 8 {
            return true;
        }
        match doc.deref(o) {
            Ok(Object::Array(a)) => a.iter().any(|x| tm(doc, x, depth + 1)),
            Ok(Object::Dict(e)) => e.contains_key(b"TransformMethod".as_slice()),
            _ => false,
        }
    }
    d.get(b"Reference".as_slice()).is_some_and(|r| tm(doc, r, 0))
}

/// Whether a signature-like dictionary is reachable in `doc` from the
/// trailer; `None` when the walk did not finish.
fn residual(doc: &Document) -> Option<bool> {
    let mut seen = HashSet::new();
    let mut stack: Vec<Object> = doc.trailer().values().cloned().collect();
    let mut nodes = 0;
    while let Some(o) = stack.pop() {
        nodes += 1;
        if nodes > 500_000 {
            return None;
        }
        let o = match o {
            Object::Ref(r) => {
                if !seen.insert(r) {
                    continue;
                }
                doc.resolve(r).ok()?
            }
            o => o,
        };
        match o {
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                if marked(doc, &d) {
                    return Some(true);
                }
                stack.extend(d.into_values());
            }
            Object::Array(a) => stack.extend(a),
            _ => {}
        }
    }
    Some(false)
}

fn sig_flags(doc: &Document) -> Option<i64> {
    let af = doc.deref(doc.catalog().get(b"AcroForm".as_slice())?).ok()?;
    let f = af.as_dict()?.get(b"SigFlags".as_slice())?.clone();
    doc.deref(&f).ok()?.as_i64()
}

fn build(inp: &mut Input, d: &mut Doc) {
    let f = [inp.byte(), inp.byte(), inp.byte()];
    d.obj(950, "/Sig");
    d.obj(951, "[<< /TransformMethod /UR >>]");
    // The target.
    let tmode = inp.byte() % 8;
    let tp = if f[0] & 1 != 0 {
        d.obj(960, "<< /Type /TransformParams /V /2.2 /Form [/FillIn] >>");
        "960 0 R"
    } else {
        "<< /Type /TransformParams /V /2.2 /Form [/FillIn] /Document [/FullSave] >>"
    };
    let mut body = UR3.replace("TP", tp);
    if tmode == 6 {
        let (a, b) = BAD_TARGETS[inp.byte() as usize % BAD_TARGETS.len()];
        body = body.replacen(a, b, 1);
        d.must_refuse = true;
    }
    let mut perms = Vec::new();
    if tmode >= 1 {
        d.target = true;
        if tmode % 2 == 1 {
            d.obj(10, format!("<< {body} >>"));
            perms.push("/UR3 10 0 R".to_string());
        } else {
            perms.push(format!("/UR3 << {body} >>"));
        }
        if tmode == 5 && f[0] & 2 != 0 && tmode % 2 == 1 {
            // Shared with a field value.
            d.fields.push("<< /FT /Tx /T (s) /V 10 0 R >>".into());
            d.must_refuse = true;
        }
        if tmode == 7 && tp.ends_with('R') {
            // What it holds is shared.
            d.cat.push_str(" /Held 960 0 R");
            d.must_refuse = true;
        }
    }
    if f[0] & 4 != 0 {
        perms.push("/Other 1".into());
    }
    if f[0] & 8 != 0 {
        perms.push("/DocMDP << /Type /Sig /Filter /F /Contents (SECRET-m) >>".into());
        d.must_refuse = true;
    }
    if !perms.is_empty() {
        let p = format!("<< {} >>", perms.join(" "));
        if f[0] & 0x10 != 0 {
            d.obj(20, p);
            d.cat.push_str(" /Perms 20 0 R");
        } else {
            d.cat.push_str(&format!(" /Perms {p}"));
        }
    }
    // /SigFlags.
    let sf = match inp.byte() % 10 {
        0 => None,
        8 => Some(-1),
        9 => Some(i64::MAX),
        k => Some(k as i64 - 1),
    };
    d.sig_flags = sf;
    if let Some(v) = sf {
        if f[0] & 0x20 != 0 {
            d.obj(30, format!("{v}"));
            d.acroform.push_str(" /SigFlags 30 0 R");
        } else {
            d.acroform.push_str(&format!(" /SigFlags {v}"));
        }
    }
    // Decoys.
    let n = inp.byte() % 4;
    for k in 0..n as u32 {
        let (decoy, marks) = DECOYS[inp.byte() as usize % DECOYS.len()];
        let num = 100 + k * 10;
        let indirect = inp.byte() & 1 == 0;
        let val = if indirect {
            d.obj(num, format!("<< {decoy} >>"));
            format!("{num} 0 R")
        } else {
            format!("<< {decoy} >>")
        };
        let pos = inp.byte() % 13;
        let mut reachable = true;
        match pos {
            0 => d.cat.push_str(&format!(" /D{k} {val}")),
            1 => d.page.push_str(&format!(" /D{k} {val}")),
            2 => d.annots.push(format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /X {val} >>")),
            3 => d.fields.push(format!("<< /FT /Tx /T (f{k}) /V {val} >>")),
            4 => {
                // A /Sig widget with a dictionary /V is signed, marker or
                // not (the rewrite's own refusal).
                d.annots.push(format!("<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /V {val} >>"));
                d.must_refuse = true;
            }
            5 => {
                // Only through /P: a page outside the tree.
                d.obj(num + 1, format!("<< /Type /Page /X {val} /Annots [{} 0 R] >>", num + 2));
                d.obj(num + 2, format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /P {} 0 R >>", num + 1));
                d.annots.push(format!("{} 0 R", num + 2));
            }
            6 => {
                // Only through /Parent, with a /Parent cycle.
                d.obj(num + 1, format!("<< /Kids [{} 0 R] /V {val} /Parent {} 0 R >>", num + 2, num + 2));
                d.obj(num + 2, format!("<< /T (k{k}) /Parent {} 0 R >>", num + 1));
                d.fields.push(format!("{} 0 R", num + 2));
            }
            7 => d.info.push_str(&format!(" /D{k} {val}")),
            8 => d.fields.push(format!("<< /FT /Sig /T (sv{k}) /SV << /Child {val} >> >>")),
            9 => {
                // A chain: num+3 → num+4 → value (packed: an alias).
                d.obj(num + 3, format!("{} 0 R", num + 4));
                d.obj(num + 4, val.clone());
                d.cat.push_str(&format!(" /C{k} {} 0 R", num + 3));
            }
            10 => {
                // A deep chain of dictionaries.
                let depth = 1 + inp.byte() as u32 % 60;
                for i in 0..depth {
                    let next = if i + 1 == depth { val.clone() } else { format!("{} 0 R", 2000 + k * 100 + i + 1) };
                    d.obj(2000 + k * 100 + i, format!("<< /N {next} >>"));
                }
                d.cat.push_str(&format!(" /Deep{k} {} 0 R", 2000 + k * 100));
            }
            11 => {
                // A stream whose dictionary is the decoy.
                d.obj(num + 5, format!("<< {decoy} /Length 3 >>\nstream\nabc\nendstream"));
                d.page.push_str(&format!(" /S{k} {} 0 R", num + 5));
            }
            _ => reachable = false,
        }
        if marks && reachable {
            d.must_refuse = true;
        }
    }
    // Locks, stores, XFA.
    match f[1] % 8 {
        1 => {
            d.fields.push("<< /FT /Sig /T (l) /Lock << /Action /All >> >>".into());
            d.must_refuse = true;
        }
        2 => {
            d.annots.push("<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 1 1] /Lock << /Action /All >> >>".into());
            d.must_refuse = true;
        }
        // A lock on a seed value itself: carried.
        3 => d.fields.push("<< /FT /Sig /T (v) /SV << /Type /SV /Lock << /Action /All >> >> >>".into()),
        4 => {
            d.cat.push_str(" /DSS << /Certs [] >>");
            d.must_refuse = true;
        }
        5 => {
            d.cat.push_str(" /Legal << /JavaScriptActions 0 >>");
            d.must_refuse = true;
        }
        6 => {
            let x = "<xdp><ds:Signature xmlns:ds=\"http://www.w3.org/2000/09/xmldsig#\"/></xdp>";
            d.obj(70, format!("<< /Length {} >>\nstream\n{x}\nendstream", x.len()));
            d.acroform.push_str(" /XFA 70 0 R");
            d.must_refuse = true;
        }
        _ => {}
    }
    if f[1] & 0x10 != 0 {
        d.page.push_str(" /AA << /O << /S /JavaScript /JS (SECRET-js) >> >>");
        d.script = true;
    }
    if f[1] & 0x20 != 0 {
        d.cat.push_str(" /RichMedia << >>");
        d.multimedia = true;
    }
    if f[1] & 0x40 != 0 {
        // /P to its own page; a page with a large dictionary.
        d.annots.push("<< /Type /Annot /Subtype /Text /Rect [0 0 1 1] /P 3 0 R >>".into());
        let wide: String = (0..(inp.byte() as u32 * 40)).map(|i| format!(" /K{i} {i}")).collect();
        d.page.push_str(&wide);
    }
}

fn write(d: &Doc, packed: bool) -> Vec<u8> {
    let mut all: Vec<(u32, String)> = vec![
        (
            1,
            format!(
                "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [{}] {} >> {} >>",
                d.fields.join(" "),
                d.acroform,
                d.cat
            ),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Annots [{}] {} >>",
                d.annots.join(" "),
                d.page
            ),
        ),
        (4, format!("<< /Producer (p) {} >>", d.info)),
    ];
    all.extend(d.objs.iter().cloned());
    all.sort_by_key(|o| o.0);
    all.dedup_by_key(|o| o.0);
    // Packed: every non-stream object above 4 goes into object stream 9000.
    let (plain, inside): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|(n, b)| !packed || *n <= 4 || b.contains("stream"));
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
    for (n, s) in &plain {
        rows.push((*n, 1, b.len() as u64, 0));
        b.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
    }
    if packed && !inside.is_empty() {
        let (mut head, mut body) = (String::new(), String::new());
        for (n, s) in &inside {
            head.push_str(&format!("{n} {} ", body.len()));
            body.push_str(s);
            body.push(' ');
        }
        let os = format!("{head}{body}");
        rows.push((9000, 1, b.len() as u64, 0));
        b.extend_from_slice(
            format!(
                "9000 0 obj\n<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{os}\nendstream\nendobj\n",
                inside.len(),
                head.len(),
                os.len()
            )
            .as_bytes(),
        );
        for (i, (n, _)) in inside.iter().enumerate() {
            rows.push((*n, 2, 9000, i as u64));
        }
    }
    let x = b.len() as u64;
    rows.push((9001, 1, x, 0));
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
            "9001 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 9002 /Root 1 0 R /Info 4 0 R /Length {} >>\nstream\n",
            data.len()
        )
        .as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let flags = inp.byte();
    let mut d = Doc {
        objs: Vec::new(),
        cat: String::new(),
        page: String::new(),
        annots: Vec::new(),
        fields: Vec::new(),
        acroform: String::new(),
        info: String::new(),
        must_refuse: false,
        target: false,
        script: false,
        multimedia: false,
        sig_flags: None,
    };
    build(&mut inp, &mut d);
    let mut b = write(&d, flags & 1 != 0);
    let corrupt = flags & 0x80 != 0;
    if corrupt {
        let pos = ((inp.byte() as usize) << 8 | inp.byte() as usize) % b.len();
        b[pos] = inp.byte();
    }
    let tight = flags & 2 != 0;
    let lim = if tight {
        Limits::default().with_max_output_objects(1 + inp.byte() as usize)
    } else {
        limits()
    };
    let Ok(doc) = Document::open(&b, &OpenOptions::new().with_limits(limits())) else {
        assert!(corrupt, "the generated file must open");
        return;
    };
    // The default is `rewrite`.
    let old = rewrite_with_limits(&doc, &lim);
    let new = rewrite_with(&doc, &RewriteOptions::new().with_limits(lim.clone()));
    match (&old, &new) {
        (Ok(a), Ok(o)) => assert_eq!(a.as_slice(), o.bytes(), "default bytes"),
        (Err(a), Err(e)) => assert_eq!(a.to_string(), e.to_string(), "default error"),
        _ => panic!("default differs from rewrite"),
    }
    for p in [P::Profile, P::Carry, P::RefuseScriptsAndExternal] {
        let opts = RewriteOptions::new()
            .with_executable_content(p)
            .with_usage_rights(U::Remove)
            .with_limits(lim.clone());
        let r = rewrite_with(&doc, &opts);
        if let Err(e) = &r {
            let s = format!("{e} {e:?}");
            assert!(!s.contains("SECRET"), "{p:?}: content in the error");
        }
        if corrupt {
            if let Ok(o) = r {
                let _ = Document::from_bytes(o.bytes());
            }
            continue;
        }
        match r {
            Ok(out) => {
                assert!(!d.must_refuse, "{p:?}: a document that must be refused was written");
                assert!(!(d.script && p == P::RefuseScriptsAndExternal), "strict wrote a script");
                assert!(!d.multimedia, "/RichMedia was written");
                let report = format!("{:?} {}", out.report(), out.report());
                assert!(!report.contains("SECRET"), "{p:?}: content in the report");
                let bytes = out.bytes();
                assert!(!contains(bytes, b"/ByteRange"), "{p:?}: /ByteRange in the output");
                let o = Document::from_bytes(bytes).expect("the output reparses");
                assert_eq!(residual(&o), Some(false), "{p:?}: a signature-like residue");
                if let Some(pr) = o.catalog().get(b"Perms".as_slice()) {
                    let pr = o.deref(pr).unwrap();
                    let pr = pr.as_dict().unwrap();
                    assert!(!pr.contains_key(b"UR3".as_slice()) && !pr.contains_key(b"UR".as_slice()));
                }
                let want = d.sig_flags.map(|v| if d.target { v & !1 } else { v });
                assert_eq!(sig_flags(&o), want, "{p:?}: /SigFlags");
                match out.report().usage_rights() {
                    UsageRightsOutcome::Removed(rm) => {
                        assert!(d.target);
                        assert_eq!(rm.entries.len(), 1);
                    }
                    UsageRightsOutcome::NonePresent => assert!(!d.target),
                    other => panic!("{other:?}"),
                }
            }
            Err(e) => {
                if d.must_refuse {
                    let sig = matches!(e, Error::SignaturePolicy { .. })
                        || e.refusal_reason() == Some(kasane_pdf::RefusalReason::Signature);
                    assert!(sig || tight, "{p:?}: refused for another reason: {e:?}");
                } else if !tight && !d.multimedia && !(d.script && p == P::RefuseScriptsAndExternal) {
                    panic!("{p:?}: refused a removable document: {e:?}");
                }
            }
        }
    }
});
