//! The renderer on structured object graphs (v0.5.1 R4, target A): a
//! page tree with nested `/Pages` nodes, inherited `/Resources` and
//! `/MediaBox`, a shared page and an optional `/Kids` cycle; up to 24
//! Form XObjects whose resources reference each other arbitrarily
//! (chains, cycles, sharing, a form reached through several names),
//! with or without a transparency group, a soft mask or a tiling
//! pattern around the next level; references to undefined numbers and
//! to another generation; deep direct dictionaries and arrays in the
//! resources; the forms packed in an object stream or not. Every page
//! is rendered and its text extracted, strict and in recovery mode.
//!
//! Oracles: no panic, stack overflow, timeout or runaway allocation; a
//! rendered page within `max_render_total_pixels`; deterministic
//! rendering (twice the same pixels or the same error), strict and in
//! recovery mode.
#![no_main]

use kasane_pdf::{
    extract_text, render_page, Document, Error, Limits, OpenOptions, RenderOptions, TextOptions,
};
use kasane_pdf_fuzz::limits;
use libfuzzer_sys::fuzz_target;

struct Input<'a>(std::slice::Iter<'a, u8>);
impl Input<'_> {
    fn byte(&mut self) -> u8 {
        self.0.next().copied().unwrap_or(0)
    }
}

const FORM0: u32 = 100;
/// Tiling patterns, one per form that draws through one.
const PAT0: u32 = 200;

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} /Length {} >>\nstream\n{data}\nendstream", data.len())
}

/// A reference to a form or a broken target.
fn target(inp: &mut Input, n: u32) -> String {
    let b = inp.byte();
    match b % 16 {
        0 => "999 0 R".into(), // undefined
        1 => format!("{FORM0} 1 R"), // another generation
        _ => format!("{} 0 R", FORM0 + (b as u32) % n),
    }
}

fn deep(inp: &mut Input) -> String {
    let d = (inp.byte() % 40) as usize;
    let (open, close) = if inp.byte() & 1 == 0 { ("<< /K ", " >>") } else { ("[ ", " ]") };
    format!("{}1{}", open.repeat(d), close.repeat(d))
}

/// Objects (number, body) and whether the forms go into an object stream.
fn graph(inp: &mut Input) -> (Vec<(u32, String)>, bool) {
    let flags = inp.byte();
    let n = 1 + (inp.byte() % 24) as u32;
    let mut o = Vec::new();
    for i in 0..n {
        let id = FORM0 + i;
        let k = inp.byte();
        let refs: Vec<String> = (0..(k % 3)).map(|_| target(inp, n)).collect();
        let names: String =
            refs.iter().enumerate().map(|(j, r)| format!(" /X{j} {r}")).collect();
        let draws: String = (0..refs.len()).map(|j| format!("/X{j} Do ")).collect();
        let wrap = inp.byte();
        let (res, body) = match wrap % 4 {
            // Draw the referenced forms through a soft mask.
            1 if !refs.is_empty() => (
                format!("/XObject << {names} >> /ExtGState << /M << /SMask << /S /Alpha /G {} >> >> >>", refs[0]),
                format!("/M gs 0 0 1 rg 0 0 9 9 re f {draws}"),
            ),
            // ... through a tiling pattern whose cell draws them.
            2 if !refs.is_empty() => {
                o.push((PAT0 + i, stream(&format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 9 9] /XStep 9 /YStep 9 /Resources << /XObject << {names} >> >>"), &draws)));
                (format!("/Pattern << /P {} 0 R >>", PAT0 + i), "/Pattern cs /P scn 0 0 18 18 re f".into())
            }
            _ => (format!("/XObject << {names} >>"), format!("0.{} g 0 0 9 9 re f {draws}", wrap % 10)),
        };
        let group = if k & 0x80 != 0 { "/Group << /S /Transparency /I true >>" } else { "" };
        o.push((id, stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] {group} /Resources << {res} /Deep {} >>", deep(inp)), &body)));
    }
    // Page tree: 2 = root; 5 = an intermediate node holding pages 6, 7.
    let first = target(inp, n);
    let res = format!("<< /XObject << /A {first} /B {} >> >>", target(inp, n));
    let cycle = if flags & 1 != 0 { " 5 0 R" } else { "" };
    let shared = if flags & 2 != 0 { " 6 0 R" } else { "" };
    o.push((1, "<< /Type /Catalog /Pages 2 0 R >>".into()));
    o.push((2, format!("<< /Type /Pages /Kids [5 0 R{shared}] /Count 2 /Resources {res} /MediaBox [0 0 40 40] >>")));
    o.push((5, format!("<< /Type /Pages /Parent 2 0 R /Kids [6 0 R 7 0 R{cycle}] /Count 2 >>")));
    o.push((6, "<< /Type /Page /Parent 5 0 R /Contents 8 0 R >>".into()));
    o.push((7, format!("<< /Type /Page /Parent 5 0 R /Contents 9 0 R /Resources << /XObject << /C {} >> >> >>", target(inp, n))));
    o.push((8, stream("", "/A Do /B Do")));
    o.push((9, stream("", "q 0.5 0 0 0.5 0 0 cm /C Do Q /C Do")));
    (o, flags & 4 != 0)
}

fn write(objs: &[(u32, String)], packed: bool) -> Vec<u8> {
    let mut objs = objs.to_vec();
    objs.sort_by_key(|o| o.0);
    // Packed: the forms' dictionaries cannot go into an object stream
    // (streams never do), so pack the non-stream objects instead.
    let (plain, inside): (Vec<_>, Vec<_>) =
        objs.into_iter().partition(|(n, s)| !packed || *n == 1 || s.contains("stream"));
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u8, u64, u64)> = Vec::new();
    for (n, s) in &plain {
        rows.push((*n, 1, b.len() as u64, 0));
        b.extend_from_slice(format!("{n} 0 obj\n{s}\nendobj\n").as_bytes());
    }
    if !inside.is_empty() {
        let (mut head, mut body) = (String::new(), String::new());
        for (n, s) in &inside {
            head.push_str(&format!("{n} {} ", body.len()));
            body.push_str(s);
            body.push(' ');
        }
        let os = format!("{head}{body}");
        rows.push((900, 1, b.len() as u64, 0));
        b.extend_from_slice(
            format!("900 0 obj\n<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{os}\nendstream\nendobj\n", inside.len(), head.len(), os.len()).as_bytes(),
        );
        for (i, (n, _)) in inside.iter().enumerate() {
            rows.push((*n, 2, 900, i as u64));
        }
    }
    let x = b.len() as u64;
    rows.push((901, 1, x, 0));
    rows.sort();
    let (mut idx, mut data) = (String::new(), Vec::new());
    for (n, t, f2, f3) in &rows {
        idx.push_str(&format!("{n} 1 "));
        data.push(*t);
        data.extend_from_slice(&(*f2 as u32).to_be_bytes());
        data.extend_from_slice(&(*f3 as u16).to_be_bytes());
    }
    b.extend_from_slice(
        format!("901 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{idx}] /Size 902 /Root 1 0 R /Length {} >>\nstream\n", data.len()).as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

type Rendered = Vec<Result<Vec<u8>, String>>;

fn render_all(b: &[u8], recovery: bool, l: &Limits) -> Option<Rendered> {
    let d = Document::open(b, &OpenOptions::new().with_limits(l.clone()).with_recovery(recovery)).ok()?;
    let opts = RenderOptions::new().with_dpi(18.0).with_limits(l.clone());
    let mut out = Vec::new();
    for p in d.pages().iter().take(4) {
        let r = render_page(&d, p, &opts).map(|r| {
            assert!((r.width() as u64) * (r.height() as u64) <= l.max_render_total_pixels);
            r.pixels().to_vec()
        });
        let _ = extract_text(&d, p, &TextOptions::default().with_limits(l.clone()));
        out.push(r.map_err(|e: Error| e.to_string()));
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let (objs, packed) = graph(&mut inp);
    let b = write(&objs, packed);
    let l = limits().with_max_xobject_invocations(2_000);
    for recovery in [false, true] {
        let a = render_all(&b, recovery, &l);
        let again = render_all(&b, recovery, &l);
        assert!(a == again, "rendering is not deterministic (recovery {recovery})");
    }
});
