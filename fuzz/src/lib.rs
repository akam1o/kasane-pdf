//! Helpers shared by the fuzz targets: split a classic-xref PDF into
//! its objects and reassemble it with a correct table, and exercise an
//! opened document within tight limits.

use kasane_pdf::{extract_text, render_page, Document, Limits, RenderOptions, TextOptions};

/// The encrypted fixtures (classic cross-reference tables) and the user
/// password each needs (`None`: the empty user password opens it).
pub const TEMPLATES: &[(&[u8], Option<&str>)] = &[
    (include_bytes!("../../tests/fixtures/encrypted/enc_r2_rc4_40.pdf"), None),
    (include_bytes!("../../tests/fixtures/encrypted/enc_r3_rc4_128.pdf"), Some("usr")),
    (include_bytes!("../../tests/fixtures/encrypted/enc_r4_aes128.pdf"), Some("usr")),
    (include_bytes!("../../tests/fixtures/encrypted/enc_r5_aes256.pdf"), None),
    (include_bytes!("../../tests/fixtures/encrypted/enc_r6_aes256.pdf"), Some("usr")),
    (include_bytes!("../../tests/fixtures/encrypted/enc_r6_aes256_empty.pdf"), None),
];

/// `(number, body)` of every `N 0 obj … endobj`, and the trailer
/// dictionary text (without `trailer`).
pub fn split(pdf: &[u8]) -> (Vec<(u32, Vec<u8>)>, Vec<u8>) {
    let mut objs = Vec::new();
    let mut i = 0;
    while let Some(p) = find(&pdf[i..], b" 0 obj\n") {
        let at = i + p;
        let mut s = at;
        while s > 0 && pdf[s - 1].is_ascii_digit() {
            s -= 1;
        }
        let num: u32 = std::str::from_utf8(&pdf[s..at]).unwrap().parse().unwrap();
        let body_start = at + 7;
        let end = body_start + find(&pdf[body_start..], b"\nendobj").unwrap();
        objs.push((num, pdf[body_start..end].to_vec()));
        i = end;
    }
    let t = find(pdf, b"trailer ").unwrap() + 8;
    let e = t + find(&pdf[t..], b"\nstartxref").unwrap();
    (objs, pdf[t..e].to_vec())
}

pub fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// A PDF with these objects, a correct classic table and `trailer`.
pub fn build(objs: &[(u32, Vec<u8>)], trailer: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let size = objs.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let mut offs = vec![None; size as usize];
    for (n, body) in objs {
        offs[*n as usize] = Some(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for o in &offs[1..] {
        match o {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(b"trailer ");
    out.extend_from_slice(trailer);
    out.extend_from_slice(format!("\nstartxref\n{x}\n%%EOF\n").as_bytes());
    out
}

/// Small limits so a fuzz iteration stays fast and bounded.
pub fn limits() -> Limits {
    Limits::default()
        .with_max_operations(20_000)
        .with_max_stream_bytes(1 << 20)
}

/// Read everything a caller would: content, text, a tiny render.
pub fn exercise(doc: &Document) {
    for page in doc.pages().iter().take(2) {
        let _ = doc.page_operations(page);
        let _ = extract_text(doc, page, &TextOptions::default());
        let _ = render_page(doc, page, &RenderOptions::new().with_dpi(9.0).with_limits(limits()));
    }
    let _ = doc.take_warnings();
}
