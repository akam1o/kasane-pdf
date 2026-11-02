//! `rewrite` on object graphs built from the input: dictionaries,
//! arrays, bare references (chains, cycles), strings and streams, with
//! references to defined objects, undefined numbers, free entries and
//! other generations; optionally an XFA packet array (duplicate or
//! non-string names, undefined or non-stream packets, an XML signature,
//! an unknown filter, arbitrary bytes) and a usage-rights signature.
//! Oracle: a rewrite that succeeds reparses strictly, has no dangling
//! reference, and its catalog graph — references resolved by §7.3.10,
//! streams by raw bytes — equals the original's; a rewrite that fails
//! does so for a reason the input contains (a signature, a packet that
//! cannot be checked, a reference cycle, a limit). With bit 7 of the
//! first byte one byte is corrupted: then only no panic / hang /
//! runaway allocation.
#![no_main]

use kasane_pdf::{audit_residuals, rewrite, Document, Error, IndirectRef, Object, OpenOptions};
use kasane_pdf_fuzz::limits;
use libfuzzer_sys::fuzz_target;

struct Input<'a>(std::slice::Iter<'a, u8>);
impl Input<'_> {
    fn byte(&mut self) -> u8 {
        self.0.next().copied().unwrap_or(0)
    }
}

const XMLDSIG: &str = "http://www.w3.org/2000/09/xmldsig#";

fn adler32(d: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in d {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// A zlib stream with one stored block (valid FlateDecode data).
fn stored_zlib(d: &[u8]) -> Vec<u8> {
    let len = d.len().min(65535) as u16;
    let d = &d[..len as usize];
    let mut v = vec![0x78, 0x01, 0x01];
    v.extend_from_slice(&len.to_le_bytes());
    v.extend_from_slice(&(!len).to_le_bytes());
    v.extend_from_slice(d);
    v.extend_from_slice(&adler32(d).to_be_bytes());
    v
}

/// Canonical form of a value with references resolved (§7.3.10);
/// `path` names a reference back onto the current chain by position.
fn canon(doc: &Document, o: &Object, depth: usize, path: &mut Vec<IndirectRef>, nodes: &mut usize, out: &mut String) {
    *nodes += 1;
    if depth > 40 || *nodes > 20_000 {
        out.push('…');
        return;
    }
    match o {
        Object::Ref(r) => {
            if let Some(k) = path.iter().position(|p| p == r) {
                out.push_str(&format!("<cycle{k}>"));
                return;
            }
            match doc.resolve(*r) {
                Ok(v) => {
                    path.push(*r);
                    canon(doc, &v, depth + 1, path, nodes, out);
                    path.pop();
                }
                Err(e) => out.push_str(&format!("<err {}>", e.kind())),
            }
        }
        Object::Array(a) => {
            out.push('[');
            for x in a {
                canon(doc, x, depth + 1, path, nodes, out);
                out.push(' ');
            }
            out.push(']');
        }
        Object::Dict(d) => {
            out.push_str("<<");
            for (k, v) in d {
                out.push_str(&format!("/{} ", String::from_utf8_lossy(k)));
                canon(doc, v, depth + 1, path, nodes, out);
                out.push(' ');
            }
            out.push_str(">>");
        }
        Object::Stream { dict, data } => {
            out.push_str("stream<<");
            for (k, v) in dict {
                if k.as_slice() == b"Length" {
                    continue;
                }
                out.push_str(&format!("/{} ", String::from_utf8_lossy(k)));
                canon(doc, v, depth + 1, path, nodes, out);
                out.push(' ');
            }
            out.push_str(&format!(">>{}:{:08x}", data.len(), adler32(data)));
        }
        other => out.push_str(&format!("{other:?}")),
    }
}

fn catalog_canon(doc: &Document) -> String {
    let mut s = String::new();
    canon(doc, &Object::Dict(doc.catalog().clone()), 0, &mut Vec::new(), &mut 0, &mut s);
    s
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let flags = inp.byte();
    let n = 1 + (inp.byte() % 48) as u32;
    let first = 10u32;
    // Reference targets: defined objects, undefined numbers, free
    // entries (300+), another generation of a defined object.
    let target = |inp: &mut Input| -> String {
        let b = inp.byte();
        let k = inp.byte() as u32;
        match b % 8 {
            0 => format!("{} 0 R", 200 + k % 8),
            1 => format!("{} 0 R", 300 + k % 4),
            2 => format!("{} 1 R", first + k % n),
            _ => format!("{} 0 R", first + k % n),
        }
    };
    let mut bodies: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut has_ref_only = false;
    for i in 0..n {
        let num = first + i;
        let kind = inp.byte() % 6;
        let body: Vec<u8> = match kind {
            0 => {
                let m = inp.byte() % 4;
                let mut s = String::from("<<");
                for j in 0..m {
                    s.push_str(&format!(" /K{j} {}", target(&mut inp)));
                }
                s.push_str(" >>");
                s.into_bytes()
            }
            1 => {
                let m = inp.byte() % 5;
                let items: Vec<String> = (0..m)
                    .map(|j| if j % 2 == 0 { target(&mut inp) } else { j.to_string() })
                    .collect();
                format!("[{}]", items.join(" ")).into_bytes()
            }
            2 => {
                has_ref_only = true;
                target(&mut inp).into_bytes()
            }
            3 => format!("(s{num})").into_bytes(),
            4 => {
                let d = format!("<p{num}/>");
                format!("<< /Length {} >>\nstream\n{d}\nendstream", d.len()).into_bytes()
            }
            _ => {
                let z = stored_zlib(format!("<z{num}/>").as_bytes());
                let mut s = format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).into_bytes();
                s.extend_from_slice(&z);
                s.extend_from_slice(b"\nendstream");
                s
            }
        };
        bodies.push((num, body));
    }
    // XFA packets as separate streams 100..: names from a small set.
    let mut xfa = String::new();
    let (mut xmlsig, mut bad_filter) = (false, false);
    let raw_packet = flags & 8 != 0;
    if flags & 1 != 0 {
        let k = inp.byte() % 6;
        let mut items = Vec::new();
        for j in 0..k {
            let name = ["template", "datasets", "config", "datasets"][inp.byte() as usize % 4];
            let num = 100 + j as u32;
            let sel = inp.byte();
            let raw = raw_packet && j == 0;
            let signs = sel % 9 == 0 && !raw;
            let content: Vec<u8> = if raw {
                // Arbitrary bytes: the encoding, declaration and
                // character-reference handling of the check.
                let len = inp.byte() as usize % 64;
                (0..len).map(|_| inp.byte()).collect()
            } else if signs && sel & 0x80 != 0 {
                // UTF-16LE with a byte order mark.
                let mut v = vec![0xFF, 0xFE];
                for u in format!("<ds:Signature xmlns:ds=\"{XMLDSIG}\"/>").encode_utf16() {
                    v.extend_from_slice(&u.to_le_bytes());
                }
                v
            } else if signs {
                format!("<Signature xmlns=\"{XMLDSIG}\"/>").into_bytes()
            } else {
                format!("<{name}{j}/>").into_bytes()
            };
            let plain = |filter: &str| {
                let mut s = format!("<< {filter} /Length {} >>\nstream\n", content.len()).into_bytes();
                s.extend_from_slice(&content);
                s.extend_from_slice(b"\nendstream");
                s
            };
            let body = match sel % 7 {
                0 => plain("/Filter /NoSuchDecode"),
                1 => {
                    let z = stored_zlib(&content);
                    let mut s = format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", z.len()).into_bytes();
                    s.extend_from_slice(&z);
                    s.extend_from_slice(b"\nendstream");
                    s
                }
                _ => plain(""),
            };
            bodies.push((num, body));
            // Only a packet the array points at is checked.
            let mut checked = (false, false);
            let packet = match sel % 11 {
                3 => format!("{} 0 R", 250 + j as u32), // undefined
                4 => format!("{} 0 R", first),          // not a stream
                _ => {
                    checked = (signs, sel % 7 == 0);
                    format!("{num} 0 R")
                }
            };
            let nm = if sel % 13 == 5 { "42".to_string() } else { format!("({name})") };
            items.push((format!("{nm} {packet}"), packet, checked));
        }
        if flags & 4 != 0 && !items.is_empty() {
            // The single-stream form: only the first packet's object.
            items.truncate(1);
            xfa = format!("/AcroForm << /Fields [] /XFA {} >>", items[0].1);
        } else {
            let parts: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
            xfa = format!("/AcroForm << /Fields [] /XFA [{}] >>", parts.join(" "));
        }
        for (_, _, (signs, bad)) in &items {
            xmlsig |= signs;
            bad_filter |= bad;
        }
    }
    let ur3 = flags & 2 != 0;
    let perms = if ur3 { "/Perms << /UR3 << /Type /Sig >> >>" } else { "" };
    let catalog = format!("<< /Type /Catalog /Pages 2 0 R /Extra {} {xfa} {perms} >>", target(&mut inp));
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, catalog.into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_vec()),
    ];
    objs.extend(bodies);
    let mut b = b"%PDF-1.7\n".to_vec();
    let mut rows: Vec<(u32, u16, u8, usize)> = vec![(0, 65535, b'f', 0)];
    for (num, body) in &objs {
        rows.push((*num, 0, b'n', b.len()));
        b.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(b"\nendobj\n");
    }
    for f in 300..304u32 {
        rows.push((f, 1, b'f', 0));
    }
    rows.sort();
    let x = b.len();
    b.extend_from_slice(b"xref\n");
    for (num, g, k, off) in &rows {
        b.extend_from_slice(format!("{num} 1\n{off:010} {g:05} {} \n", *k as char).as_bytes());
    }
    b.extend_from_slice(format!("trailer\n<< /Size 400 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());

    let corrupt = flags & 0x80 != 0;
    if corrupt {
        let pos = ((inp.byte() as usize) << 8 | inp.byte() as usize) % b.len();
        b[pos] = inp.byte();
    }
    for recovery in [false, true] {
        let o = OpenOptions::new().with_limits(limits()).with_recovery(recovery);
        let Ok(doc) = Document::open(&b, &o) else {
            assert!(corrupt || recovery, "the generated file must open strictly");
            continue;
        };
        let out = rewrite(&doc);
        if corrupt || doc.recovery_mode() {
            if let Ok(out) = out {
                let _ = Document::from_bytes(&out);
            }
            continue;
        }
        match out {
            Ok(out) => {
                assert!(!ur3 && !xmlsig, "a signed document was rewritten");
                assert!(!bad_filter, "an undecodable packet was not checked");
                let d2 = Document::from_bytes(&out).expect("the rewrite reparses strictly");
                let audit = audit_residuals(&d2, &limits()).expect("audit");
                assert_eq!(audit.reachability.missing_references, 0, "dangling reference in the output");
                assert_eq!(catalog_canon(&doc), catalog_canon(&d2), "the graph changed");
            }
            Err(Error::SignaturePolicy { .. }) => assert!(ur3 || xmlsig || raw_packet),
            Err(Error::Unsupported { feature, .. }) => {
                assert!(feature == "XFA packet" && (bad_filter || raw_packet), "{feature}")
            }
            Err(Error::CycleDetected { .. }) => assert!(has_ref_only),
            Err(Error::LimitExceeded { .. }) => {}
            Err(e) => panic!("unexpected rewrite error: {e:?}"),
        }
    }
});
