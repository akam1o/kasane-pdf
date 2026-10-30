//! Arbitrary ciphertext under every cipher: the fuzz input replaces
//! the content stream's data (with a matching /Length) and the /Info
//! title string of an encrypted template that otherwise authenticates.
#![no_main]

use kasane_pdf::{Document, OpenOptions};
use kasane_pdf_fuzz::{build, exercise, limits, split, TEMPLATES};
use libfuzzer_sys::fuzz_target;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    let (tpl, user) = TEMPLATES[sel as usize % TEMPLATES.len()];
    let (mut objs, trailer) = split(tpl);
    let cut = rest.len() / 2;
    let (stream, title) = rest.split_at(cut);
    for o in objs.iter_mut() {
        if o.0 == 6 {
            let mut b = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            b.extend_from_slice(stream);
            b.extend_from_slice(b"\nendstream");
            o.1 = b;
        } else if o.0 == 2 {
            o.1 = format!("<< /Title <{}> /Producer ({}) >>", hex(title), "x").into_bytes();
        }
    }
    let pdf = build(&objs, &trailer);
    let mut o = OpenOptions::new().with_limits(limits());
    if let Some(p) = user {
        o = o.with_password(p);
    }
    if let Ok(doc) = Document::open(&pdf, &o) {
        if let Some(i) = doc.trailer().get(b"Info".as_slice()) {
            let _ = doc.deref(i);
        }
        exercise(&doc);
    }
});
