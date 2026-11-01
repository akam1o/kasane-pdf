//! Cross-reference update chains built from the input: 1–6 sections,
//! each a classic table, an xref stream or a hybrid section (table +
//! `/XRefStm`), defining, freeing, compressing or relisting objects
//! 4–11 at generations 0–2. A model of the precedence rule (per number,
//! the newest section that lists it decides; in a hybrid section the
//! table before its stream) predicts every object; the document must
//! agree exactly. With bit 7 of the first byte, one byte of the file is
//! overwritten first: then only no panic / hang / runaway allocation.
#![no_main]

use kasane_pdf::{Document, IndirectRef, Object, OpenOptions};
use kasane_pdf_fuzz::{exercise, limits};
use libfuzzer_sys::fuzz_target;

#[derive(Clone, Debug, PartialEq)]
enum Ent {
    /// In use at `gen`, body `(marker)`.
    Used(u16, String),
    Free(u16),
    /// In the section's object stream, body `(marker)`.
    Packed(String),
}

struct Input<'a>(std::slice::Iter<'a, u8>);
impl Input<'_> {
    fn byte(&mut self) -> u8 {
        self.0.next().copied().unwrap_or(0)
    }
}

fn obj(b: &mut Vec<u8>, n: u32, g: u16, body: &str) -> u64 {
    let at = b.len() as u64;
    b.extend_from_slice(format!("{n} {g} obj\n{body}\nendobj\n").as_bytes());
    at
}

/// `(number, type, f2, f3)` rows as an uncompressed xref stream.
fn xstream(b: &mut Vec<u8>, num: u32, rows: &mut Vec<(u32, u8, u64, u64)>, extra: &str) -> u64 {
    let at = b.len() as u64;
    rows.push((num, 1, at, 0));
    rows.sort();
    let mut index = String::new();
    let mut data = Vec::new();
    for (n, t, f2, f3) in rows.iter() {
        index.push_str(&format!("{n} 1 "));
        data.push(*t);
        data.extend_from_slice(&(*f2 as u32).to_be_bytes());
        data.extend_from_slice(&(*f3 as u16).to_be_bytes());
    }
    b.extend_from_slice(
        format!(
            "{num} 0 obj\n<< /Type /XRef /W [1 4 2] /Index [{index}] {extra} /Length {} >>\nstream\n",
            data.len()
        )
        .as_bytes(),
    );
    b.extend_from_slice(&data);
    b.extend_from_slice(b"\nendstream\nendobj\n");
    at
}

fn classic(b: &mut Vec<u8>, rows: &mut Vec<(u32, u16, u8, u64)>, trailer: &str) -> u64 {
    let at = b.len() as u64;
    rows.sort();
    b.extend_from_slice(b"xref\n");
    for (n, g, k, off) in rows.iter() {
        b.extend_from_slice(format!("{n} 1\n{off:010} {g:05} {} \n", *k as char).as_bytes());
    }
    b.extend_from_slice(format!("trailer\n{trailer}\n").as_bytes());
    at
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let flags = inp.byte();
    let mut b = b"%PDF-1.5\n".to_vec();
    let base = [
        obj(&mut b, 1, 0, "<< /Type /Catalog /Pages 2 0 R >>"),
        obj(&mut b, 2, 0, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        obj(
            &mut b,
            3,
            0,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>",
        ),
    ];
    // Sections oldest first; each: (table entries, stream entries).
    let mut model: Vec<(Vec<(u32, Ent)>, Vec<(u32, Ent)>)> = Vec::new();
    let mut prev: Option<u64> = None;
    let sections = 1 + inp.byte() % 6;
    for s in 0..sections as u32 {
        let kind = inp.byte() % 3; // 0 classic, 1 stream, 2 hybrid
        let (xnum, osnum) = (20 + 2 * s, 21 + 2 * s);
        let mut table: Vec<(u32, Ent)> = Vec::new();
        let mut stream: Vec<(u32, Ent)> = Vec::new();
        let mut packed: Vec<(u32, String)> = Vec::new();
        let mut offs: Vec<(u32, u64)> = Vec::new();
        for n in 4..12u32 {
            let op = inp.byte();
            let g = (op >> 4) as u16 % 3;
            let ent = match op % 6 {
                0 | 1 => Ent::Used(g, format!("m{s}x{n}x{g}")),
                2 => Ent::Free(g),
                3 if kind != 0 => Ent::Packed(format!("c{s}x{n}")),
                _ => continue,
            };
            if let Ent::Used(g, m) = &ent {
                offs.push((n, obj(&mut b, n, *g, &format!("({m})"))));
            }
            if let Ent::Packed(m) = &ent {
                packed.push((n, m.clone()));
            }
            // Hybrid: the table or the stream, or both (the table first).
            let place = if kind == 2 {
                (op >> 6) % 3
            } else {
                kind.min(1)
            };
            if kind == 0 || (kind == 2 && place != 1 && !matches!(ent, Ent::Packed(_))) {
                table.push((n, ent.clone()));
            }
            if kind == 1
                || (kind == 2 && (place == 1 || matches!(ent, Ent::Packed(_)) || place == 2))
            {
                stream.push((n, ent));
            }
        }
        let os_at = if packed.is_empty() {
            None
        } else {
            let mut head = String::new();
            let mut body = String::new();
            for (n, m) in &packed {
                head.push_str(&format!("{n} {} ", body.len()));
                body.push_str(&format!("({m}) "));
            }
            let st = format!("{head}{body}");
            Some(obj(
                &mut b,
                osnum,
                0,
                &format!(
                    "<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{st}\nendstream",
                    packed.len(),
                    head.len(),
                    st.len()
                ),
            ))
        };
        let off_of = |n: u32| offs.iter().find(|o| o.0 == n).map(|o| o.1).unwrap_or(0);
        let pidx = |n: u32| packed.iter().position(|p| p.0 == n).unwrap_or(0) as u64;
        let srow = |n: u32, e: &Ent| match e {
            Ent::Used(g, _) => (n, 1u8, off_of(n), *g as u64),
            Ent::Free(g) => (n, 0, 0, *g as u64),
            Ent::Packed(_) => (n, 2, osnum as u64, pidx(n)),
        };
        let trow = |n: u32, e: &Ent| match e {
            Ent::Used(g, _) => (n, *g, b'n', off_of(n)),
            Ent::Free(g) => (n, *g, b'f', 0),
            Ent::Packed(_) => unreachable!(),
        };
        let prev_s = prev.map(|p| format!("/Prev {p}")).unwrap_or_default();
        let mut trows: Vec<(u32, u16, u8, u64)> = table.iter().map(|(n, e)| trow(*n, e)).collect();
        let mut srows: Vec<(u32, u8, u64, u64)> = stream.iter().map(|(n, e)| srow(*n, e)).collect();
        if let Some(at) = os_at {
            srows.push((osnum, 1, at, 0));
        }
        if s == 0 {
            trows.push((0, 65535, b'f', 0));
            for (i, at) in base.iter().enumerate() {
                trows.push((i as u32 + 1, 0, b'n', *at));
            }
        }
        let x = match kind {
            0 => classic(
                &mut b,
                &mut trows,
                &format!("<< /Size 40 /Root 1 0 R {prev_s} >>"),
            ),
            1 => {
                srows.extend(trows.iter().map(|&(n, g, k, off)| match k {
                    b'n' => (n, 1, off, g as u64),
                    _ => (n, 0, 0, g as u64),
                }));
                xstream(
                    &mut b,
                    xnum,
                    &mut srows,
                    &format!("/Size 40 /Root 1 0 R {prev_s}"),
                )
            }
            _ => {
                let xs = xstream(&mut b, xnum, &mut srows, "/Size 40");
                classic(
                    &mut b,
                    &mut trows,
                    &format!("<< /Size 40 /Root 1 0 R {prev_s} /XRefStm {xs} >>"),
                )
            }
        };
        b.extend_from_slice(format!("startxref\n{x}\n%%EOF\n").as_bytes());
        prev = Some(x);
        model.push((table, stream));
    }
    let corrupt = flags & 0x80 != 0;
    if corrupt && !b.is_empty() {
        let pos = (inp.byte() as usize) << 8 | inp.byte() as usize;
        let at = pos % b.len();
        b[at] = inp.byte();
    }
    for recovery in [false, true] {
        let o = OpenOptions::new()
            .with_limits(limits())
            .with_recovery(recovery);
        let Ok(doc) = Document::open(&b, &o) else {
            assert!(corrupt || recovery, "the generated file must open strictly");
            continue;
        };
        exercise(&doc);
        if corrupt || doc.recovery_mode() {
            continue;
        }
        for n in 4..12u32 {
            let decided = model.iter().rev().find_map(|(t, s)| {
                t.iter()
                    .chain(s.iter())
                    .find(|(m, _)| *m == n)
                    .map(|(_, e)| e.clone())
            });
            for g in 0..3u16 {
                let got = doc
                    .resolve(IndirectRef {
                        number: n,
                        generation: g,
                    })
                    .unwrap();
                let want = match &decided {
                    Some(Ent::Used(dg, m)) if *dg == g => Object::String(m.clone().into_bytes()),
                    Some(Ent::Packed(m)) if g == 0 => Object::String(m.clone().into_bytes()),
                    _ => Object::Null,
                };
                assert_eq!(got, want, "{n} {g} R decided by {decided:?}");
            }
        }
    }
});
