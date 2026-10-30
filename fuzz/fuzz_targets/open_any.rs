//! Arbitrary bytes, opened without a password, with the user and with
//! the owner password of the fixtures (seed the corpus with them).
//! Oracle: an encrypted document that opens with no password must
//! report the empty user password — anything else is an
//! authentication bypass.
#![no_main]

use kasane_pdf::{Document, OpenOptions};
use kasane_pdf_fuzz::{exercise, limits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for pw in [None, Some("usr"), Some("own")] {
        for recovery in [false, true] {
            let mut o = OpenOptions::new().with_limits(limits()).with_recovery(recovery);
            if let Some(p) = pw {
                o = o.with_password(p);
            }
            if let Ok(doc) = Document::open(data, &o) {
                if let (None, Some(info)) = (pw, doc.encryption()) {
                    assert!(info.empty_user_password, "opened without a password");
                }
                exercise(&doc);
            }
        }
    }
});
