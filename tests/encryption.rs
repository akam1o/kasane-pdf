//! The Standard security handler (§7.6): every revision 2–6 against
//! the same plaintext document (`tests/fixtures/encrypted/base.pdf`,
//! encrypted by qpdf — see `tests/fixtures/README.md`), passwords,
//! crypt filters, refusals, and the interaction with recovery mode and
//! the writers.

mod common;

use common::fixture;
use kasane_pdf::{
    extract_text, render_page, Authentication, CryptMethod, Document, Error, Limits, Object,
    OpenOptions, RenderOptions, TextOptions,
};

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture(&format!("encrypted/{name}"))).unwrap()
}

/// What a correctly decrypted document must reproduce exactly: the
/// rendered pixels, the extracted character codes, the /Info title.
fn fingerprint(doc: &Document) -> (Vec<u8>, Vec<u32>, Option<Vec<u8>>) {
    let page = &doc.pages()[0];
    let pixels = render_page(doc, page, &RenderOptions::default())
        .unwrap()
        .pixels()
        .to_vec();
    let codes = extract_text(doc, page, &TextOptions::default())
        .unwrap()
        .chars
        .iter()
        .map(|c| c.code)
        .collect();
    let title = doc
        .trailer()
        .get(b"Info".as_slice())
        .map(|i| doc.deref(i).unwrap())
        .and_then(|i| match i.as_dict()?.get(b"Title".as_slice())? {
            Object::String(s) => Some(s.clone()),
            _ => None,
        });
    (pixels, codes, title)
}

const EMPTY_USER: &[&str] = &[
    "enc_r2_rc4_40.pdf",
    "enc_r4_aes128_objstm.pdf",
    "enc_r4_rc4_objstm.pdf",
    "enc_r5_aes256.pdf",
    "enc_r6_aes256_empty.pdf",
];
const WITH_USER: &[&str] = &[
    "enc_r3_rc4_128.pdf",
    "enc_r4_aes128.pdf",
    "enc_r6_aes256.pdf",
];

#[test]
fn every_revision_decrypts_to_the_plaintext_document() {
    let plain = fingerprint(&Document::from_bytes(&bytes("base.pdf")).unwrap());
    assert_eq!(plain.2.as_deref(), Some(&b"Secret title"[..]));
    for name in EMPTY_USER {
        let doc = Document::from_bytes(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(fingerprint(&doc), plain, "{name}");
        let info = doc.encryption().expect("encrypted");
        assert!(info.empty_user_password, "{name}");
        assert_eq!(info.authenticated_as, Authentication::User);
    }
    for name in WITH_USER {
        let doc = Document::open(&bytes(name), &OpenOptions::new().with_password("usr"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(fingerprint(&doc), plain, "{name}");
        assert!(!doc.encryption().unwrap().empty_user_password);
    }
    assert!(Document::from_bytes(&bytes("base.pdf"))
        .unwrap()
        .encryption()
        .is_none());
}

#[test]
fn owner_password_opens_and_is_reported() {
    for name in WITH_USER.iter().chain(EMPTY_USER) {
        let doc = Document::open(&bytes(name), &OpenOptions::new().with_password("own"))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let info = doc.encryption().unwrap();
        // Tried as the user password first; it is not, so the owner
        // password authenticates.
        assert_eq!(info.authenticated_as, Authentication::Owner, "{name}");
    }
}

#[test]
fn wrong_or_missing_password_returns_no_document() {
    for name in WITH_USER {
        let data = bytes(name);
        assert!(
            matches!(Document::from_bytes(&data), Err(Error::Encrypted)),
            "{name}"
        );
        let e = Document::open(&data, &OpenOptions::new().with_password("hunter2")).unwrap_err();
        assert!(matches!(e, Error::Encrypted), "{name}: {e:?}");
        // The password never appears in the error.
        assert!(!format!("{e} {e:?}").contains("hunter2"));
        // Recovery mode does not bypass authentication.
        assert!(matches!(
            Document::from_bytes_recovering(&data),
            Err(Error::Encrypted)
        ));
        let rec = OpenOptions::new().with_recovery(true).with_password("usr");
        assert!(Document::open(&data, &rec).is_ok(), "{name}");
    }
}

#[test]
fn options_never_show_the_password() {
    let o = OpenOptions::new().with_password("hunter2");
    assert!(!format!("{o:?}").contains("hunter2"));
    let doc = Document::open(&bytes("enc_r6_aes256.pdf"), &o.with_password("usr"));
    assert!(!format!("{doc:?}").contains("usr\""));
}

#[test]
fn methods_and_permissions_are_reported_not_enforced() {
    let cases = [
        ("enc_r2_rc4_40.pdf", 2, 40, CryptMethod::Rc4),
        ("enc_r4_aes128_objstm.pdf", 4, 128, CryptMethod::Aes128),
        ("enc_r4_rc4_objstm.pdf", 4, 128, CryptMethod::Rc4),
        ("enc_r5_aes256.pdf", 5, 256, CryptMethod::Aes256),
        ("enc_r6_aes256_empty.pdf", 6, 256, CryptMethod::Aes256),
    ];
    for (name, r, bits, m) in cases {
        let info = Document::from_bytes(&bytes(name))
            .unwrap()
            .encryption()
            .unwrap();
        assert_eq!(
            (info.revision, info.key_bits, info.stream_method),
            (r, bits, m),
            "{name}"
        );
        assert_eq!(info.string_method, m, "{name}");
    }
    // qpdf's defaults allow everything (/P -4 or -1028 style values).
    let info = Document::from_bytes(&bytes("enc_r6_aes256_empty.pdf"))
        .unwrap()
        .encryption()
        .unwrap();
    assert!(info.permission(3), "print allowed");
}

#[test]
fn metadata_stream_follows_encrypt_metadata() {
    let xmp = |doc: &Document| {
        let m = doc
            .deref(doc.catalog().get(b"Metadata".as_slice()).unwrap())
            .unwrap();
        let (dict, raw) = m.as_stream().unwrap();
        let data = if dict.contains_key(b"Filter".as_slice()) {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(raw), &mut out).unwrap();
            out
        } else {
            raw.to_vec()
        };
        String::from_utf8_lossy(&data).into_owned()
    };
    // /EncryptMetadata false: the XMP stream is stored in the clear and
    // read as is; the content stream is still decrypted.
    let doc = Document::from_bytes(&bytes("enc_r4_aes128_objstm.pdf")).unwrap();
    assert!(!doc.encryption().unwrap().encrypt_metadata);
    assert!(xmp(&doc).contains("plain metadata"));
    let c = doc.page_contents(&doc.pages()[0]).unwrap();
    assert!(String::from_utf8_lossy(&c[0]).contains("Hello encrypted"));
    // Default: metadata encrypted like everything else, and decrypted.
    let doc = Document::open(
        &bytes("enc_r4_aes128.pdf"),
        &OpenOptions::new().with_password("usr"),
    )
    .unwrap();
    assert!(doc.encryption().unwrap().encrypt_metadata);
    assert!(xmp(&doc).contains("plain metadata"));
}

#[test]
fn writers_refuse_encrypted_documents() {
    let doc = Document::from_bytes(&bytes("enc_r6_aes256_empty.pdf")).unwrap();
    for r in [
        kasane_pdf::rewrite(&doc).map(|_| ()),
        doc.edit().and_then(|e| e.save_incremental()).map(|_| ()),
    ] {
        assert!(
            matches!(&r, Err(Error::Unsupported { feature, .. }) if feature.contains("encrypted")),
            "{r:?}"
        );
    }
}

#[test]
fn limits_apply_to_the_plaintext() {
    // The page has 12 content operations; a bound of 5 refuses the
    // decrypted stream exactly as it refuses the unencrypted one.
    let lim = Limits::default().with_max_operations(5);
    for name in ["base.pdf", "enc_r6_aes256_empty.pdf", "enc_r2_rc4_40.pdf"] {
        let doc =
            Document::open(&bytes(name), &OpenOptions::new().with_limits(lim.clone())).unwrap();
        let e = doc.page_operations(&doc.pages()[0]).unwrap_err();
        assert!(matches!(e, Error::LimitExceeded { .. }), "{name}: {e:?}");
    }
}

/// Replace the first occurrence of `from` with `to` (same length).
fn patch(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let i = data
        .windows(from.len())
        .position(|w| w == from)
        .expect("pattern present");
    let mut out = data.to_vec();
    out[i..i + to.len()].copy_from_slice(to);
    out
}

#[test]
fn malformed_or_unsupported_encrypt_dictionaries_are_typed() {
    let data = bytes("enc_r6_aes256_empty.pdf");
    // Unknown security handler.
    let e = Document::from_bytes(&patch(&data, b"/Filter /Standard", b"/Filter /Customxy"))
        .unwrap_err();
    assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    // Unknown revision.
    let e = Document::from_bytes(&patch(&data, b"/R 6", b"/R 9")).unwrap_err();
    assert!(matches!(e, Error::Unsupported { .. }), "{e:?}");
    // /P altered: /Perms no longer matches (R 6 integrity check).
    let p = data.windows(3).position(|w| w == b"/P ").expect("/P");
    let mut altered = data.clone();
    // Flip the last digit of /P's value.
    let end = p
        + 3
        + altered[p + 3..]
            .iter()
            .position(|c| !(c.is_ascii_digit() || *c == b'-'))
            .unwrap();
    altered[end - 1] = if altered[end - 1] == b'4' { b'8' } else { b'4' };
    let e = Document::from_bytes(&altered).unwrap_err();
    assert!(matches!(e, Error::Structure(_) | Error::Encrypted), "{e:?}");
}
