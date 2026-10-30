//! The fuzz input becomes the `/Encrypt` dictionary of a well-formed
//! encrypted file — or, with bit 7 of the first byte set, an extra
//! object of that file with its trailer and table cut off (recovery
//! has to rebuild the table). Low bits: which template.
//! Oracle (confidentiality): a template whose user password is not
//! empty never reveals its plaintext — the content text or the /Info
//! title — without that password, strict or recovery, with or without
//! its trailer. (A replaced dictionary may legitimately authenticate
//! with the empty password, e.g. another fixture's, since all share
//! one /ID; its key then decrypts this file's objects to garbage.)
#![no_main]

use kasane_pdf::{Document, Object, OpenOptions};
use kasane_pdf_fuzz::{build, exercise, find, limits, split, TEMPLATES};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, dict)) = data.split_first() else {
        return;
    };
    let (tpl, user) = TEMPLATES[(sel & 0x7f) as usize % TEMPLATES.len()];
    let (mut objs, trailer) = split(tpl);
    let cut_trailer = sel & 0x80 != 0;
    if cut_trailer {
        // The encryption dictionary stays (without it nothing marks the
        // file as encrypted); the input becomes an extra object.
        objs.push((9, dict.to_vec()));
    } else {
        for o in objs.iter_mut() {
            if o.0 == 8 {
                o.1 = dict.to_vec();
            }
        }
    }
    let mut pdf = build(&objs, &trailer);
    if cut_trailer {
        let cut = find(&pdf, b"xref\n").unwrap();
        pdf.truncate(cut);
    }
    for recovery in [false, true] {
        for pw in [None, user, Some("own")] {
            let mut o = OpenOptions::new()
                .with_limits(limits())
                .with_recovery(recovery);
            if let Some(p) = pw {
                o = o.with_password(p);
            }
            if let Ok(doc) = Document::open(&pdf, &o) {
                if pw.is_none() && user.is_some() {
                    assert!(!reveals_plaintext(&doc), "plaintext without the password");
                }
                exercise(&doc);
            }
        }
    }
});

/// Whether `doc` exposes the templates' plaintext.
fn reveals_plaintext(doc: &Document) -> bool {
    let contents = doc
        .pages()
        .first()
        .and_then(|p| doc.page_contents(p).ok())
        .unwrap_or_default();
    let in_content = contents
        .iter()
        .any(|c| c.windows(15).any(|w| w == b"Hello encrypted"));
    let title = doc
        .trailer()
        .get(b"Info".as_slice())
        .and_then(|i| doc.deref(i).ok())
        .and_then(|i| match i.as_dict()?.get(b"Title".as_slice())? {
            Object::String(s) => Some(s.clone()),
            _ => None,
        });
    in_content || title.as_deref() == Some(b"Secret title".as_slice())
}
