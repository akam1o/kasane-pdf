//! The Standard security handler (§7.6): every revision 2–6 against
//! the same plaintext document (`tests/fixtures/encrypted/base.pdf`,
//! encrypted by qpdf — see `tests/fixtures/README.md`), passwords,
//! crypt filters, refusals, and the interaction with recovery mode and
//! the writers.

mod common;

use common::fixture;
use kasane_pdf::{
    extract_text, render_page, Authentication, CryptMethod, Document, Error, Limits, Object,
    OpenOptions, RecoveryRule, RenderOptions, TextOptions, Warning,
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

/// The table of a damaged encrypted file is never rebuilt — not only
/// when a trailer still names `/Encrypt`, but also when the trailer is
/// lost or no longer names it while the encryption dictionary survives
/// (the rebuild would read ciphertext object streams as plaintext, and
/// the writers would then emit the result without its encryption).
#[test]
fn damaged_encrypted_file_is_not_rebuilt_without_its_trailer() {
    let refused = |pdf: &[u8], what: &str| {
        for r in [
            Document::from_bytes_recovering(pdf).map(|_| ()),
            Document::open(pdf, &OpenOptions::new().with_recovery(true)).map(|_| ()),
        ] {
            assert!(
                matches!(
                    &r,
                    Err(Error::Unsupported {
                        feature: "encryption",
                        ..
                    })
                ),
                "{what}: {r:?}"
            );
        }
    };
    let data = bytes("enc_r6_aes256_empty.pdf");
    let t = data
        .windows(8)
        .position(|w| w == b"trailer ")
        .expect("trailer");
    // Trailer and startxref cut off: only the objects remain.
    refused(&data[..t], "no trailer");
    // Trailer kept but `/Encrypt` renamed, startxref broken.
    let mut renamed = data.clone();
    let e = t + renamed[t..]
        .windows(8)
        .position(|w| w == b"/Encrypt")
        .unwrap();
    renamed[e + 1..e + 8].copy_from_slice(b"Xncrypt");
    let s = renamed.windows(9).rposition(|w| w == b"startxref").unwrap();
    renamed[s + 10] = b'9';
    refused(&renamed, "trailer without /Encrypt");
    // Same for an R4 file whose catalog sits in an object stream.
    let data = bytes("enc_r4_aes128_objstm.pdf");
    let s = data.windows(9).rposition(|w| w == b"startxref").unwrap();
    refused(&data[..s], "objstm file without startxref");
}

/// An encryption dictionary hidden in an unencrypted object stream of a
/// file without a trailer still marks the file as encrypted.
#[test]
fn encryption_dictionary_inside_an_object_stream_refuses_the_rebuild() {
    let members = b"<< /Type /Catalog /Pages 2 0 R >> << /Type /Pages /Kids [] /Count 0 >> \
                    << /Filter /Standard /V 5 /R 6 /O <00> /U <00> /P -4 >>";
    let first = "1 0 2 34 3 71 ".to_string();
    let mut body = first.clone().into_bytes();
    body.extend_from_slice(members);
    let mut pdf = b"%PDF-1.7\n".to_vec();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N 3 /First {} /Length {} >>\nstream\n",
            first.len(),
            body.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&body);
    pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
    let r = Document::from_bytes_recovering(&pdf).map(|_| ());
    assert!(
        matches!(
            &r,
            Err(Error::Unsupported {
                feature: "encryption",
                ..
            })
        ),
        "{r:?}"
    );
}

fn opens(name: &str, pw: impl AsRef<[u8]>) -> Result<Document, Error> {
    Document::open(&bytes(name), &OpenOptions::new().with_password(pw))
}

#[test]
fn unicode_passwords_follow_the_revision_encoding() {
    let plain = fingerprint(&Document::from_bytes(&bytes("base.pdf")).unwrap());
    // R6: UTF-8 after SASLprep. The file was written with the NFC form;
    // the decomposed form normalises to it.
    for pw in ["caf\u{e9}", "cafe\u{301}"] {
        let doc = opens("enc_r6_unicode.pdf", pw).unwrap_or_else(|e| panic!("{pw:?}: {e}"));
        assert_eq!(fingerprint(&doc), plain);
        assert_eq!(
            doc.encryption().unwrap().authenticated_as,
            Authentication::User
        );
    }
    assert!(matches!(
        opens("enc_r6_unicode.pdf", "CAF\u{c9}"),
        Err(Error::Encrypted)
    ));
    // A password SASLprep refuses (unassigned in Unicode 3.2) is used
    // as given.
    assert!(opens("enc_r6_emoji.pdf", "\u{1F600}").is_ok());
    // R2-R4: PDFDocEncoding. UTF-8 input is converted; the encoded
    // bytes themselves work too.
    for pw in ["caf\u{e9}".as_bytes(), b"caf\xe9".as_slice()] {
        assert!(opens("enc_r4_pdfdoc.pdf", pw).is_ok(), "{pw:?}");
    }
    assert!(opens("enc_r4_euro.pdf", "\u{20AC}").is_ok());
    assert!(matches!(
        opens("enc_r4_euro.pdf", "E"),
        Err(Error::Encrypted)
    ));
}

#[test]
fn r6_password_is_truncated_to_127_bytes() {
    // Algorithm 2.A step (a): only the first 127 bytes count.
    let p127 = "p".repeat(127);
    assert!(opens("enc_r6_long.pdf", &p127).is_ok());
    assert!(opens("enc_r6_long.pdf", "p".repeat(130)).is_ok());
    assert!(matches!(
        opens("enc_r6_long.pdf", "p".repeat(126)),
        Err(Error::Encrypted)
    ));
}

#[test]
fn short_rc4_key_follows_the_specification_and_pads_only_in_recovery() {
    let plain = fingerprint(&Document::from_bytes(&bytes("base.pdf")).unwrap());
    let padded = |doc: &Document| {
        doc.take_warnings().iter().any(|w| {
            matches!(
                w,
                Warning::RecoveryApplied {
                    rule: RecoveryRule::PaddedShortEncryptionKey,
                    ..
                }
            )
        })
    };
    let rec = OpenOptions::new().with_recovery(true);
    // Written as ISO 32000 specifies: read the same way in both modes.
    for opts in [OpenOptions::new(), rec.clone()] {
        let doc = Document::open(&bytes("enc_r4_rc4_40_spec.pdf"), &opts).unwrap();
        assert_eq!(doc.encryption().unwrap().key_bits, 40);
        assert_eq!(fingerprint(&doc).1, plain.1);
        assert_eq!(fingerprint(&doc).2, plain.2);
        assert!(!padded(&doc));
    }
    // Written as Acrobat does (key zero-padded): strict refuses instead
    // of returning undecryptable data; recovery reads it, recorded.
    let acrobat = bytes("enc_r4_rc4_40_acrobat.pdf");
    let e = Document::from_bytes(&acrobat).unwrap_err();
    assert!(
        matches!(
            e,
            Error::Unsupported {
                feature: "encryption",
                ..
            }
        ),
        "{e:?}"
    );
    let doc = Document::open(&acrobat, &rec).unwrap();
    assert_eq!(fingerprint(&doc).1, plain.1);
    assert_eq!(fingerprint(&doc).2, plain.2);
    assert!(padded(&doc));
}

#[test]
fn incremental_update_of_an_encrypted_file() {
    // An appended revision replaces the content stream and the /Info
    // title, both encrypted with the same key (written independently).
    let doc = Document::from_bytes(&bytes("enc_r2_incremental.pdf")).unwrap();
    let (_, codes, title) = fingerprint(&doc);
    let text: String = codes.iter().map(|c| char::from(*c as u8)).collect();
    assert_eq!(text.replace(' ', ""), "Updatedrevision");
    assert_eq!(title.as_deref(), Some(&b"Updated title"[..]));
    assert!(doc.encryption().is_some());
}

#[test]
fn overlong_encryption_strings_are_recorded() {
    // /U padded beyond its 32 bytes: the leading bytes are used, the
    // deviation is recorded.
    let data = bytes("enc_r2_rc4_40.pdf");
    let (mut objs, trailer) = split(&data);
    for o in objs.iter_mut() {
        if o.0 == 8 {
            let s = String::from_utf8_lossy(&o.1).into_owned();
            let i = s.find("/U <").unwrap() + 4;
            let j = i + s[i..].find('>').unwrap();
            o.1 = format!("{}{}{}", &s[..j], "00".repeat(95), &s[j..]).into_bytes();
        }
    }
    let doc = Document::from_bytes(&build(&objs, &trailer)).unwrap();
    let w = doc.take_warnings();
    assert!(
        w.iter().any(|w| matches!(
            w,
            Warning::OverlongEncryptionEntry {
                key: "U",
                length: 127,
                specified: 32
            }
        )),
        "{w:?}"
    );
}

/// `(number, body)` of each `N 0 obj … endobj` and the trailer text.
fn split(pdf: &[u8]) -> (Vec<(u32, Vec<u8>)>, Vec<u8>) {
    let find = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n);
    let mut objs = Vec::new();
    let mut i = 0;
    while let Some(p) = find(&pdf[i..], b" 0 obj\n") {
        let at = i + p;
        let mut s = at;
        while s > 0 && pdf[s - 1].is_ascii_digit() {
            s -= 1;
        }
        let num = std::str::from_utf8(&pdf[s..at]).unwrap().parse().unwrap();
        let start = at + 7;
        let end = start + find(&pdf[start..], b"\nendobj").unwrap();
        objs.push((num, pdf[start..end].to_vec()));
        i = end;
    }
    let t = find(pdf, b"trailer ").unwrap() + 8;
    let e = t + find(&pdf[t..], b"\nstartxref").unwrap();
    (objs, pdf[t..e].to_vec())
}

/// A file with these objects, a correct cross-reference table and
/// `trailer`.
fn build(objs: &[(u32, Vec<u8>)], trailer: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let size = objs.iter().map(|o| o.0).max().unwrap() + 1;
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
        let line = match o {
            Some(o) => format!("{o:010} 00000 n \n"),
            None => "0000000000 00000 f \n".into(),
        };
        out.extend_from_slice(line.as_bytes());
    }
    out.extend_from_slice(b"trailer ");
    out.extend_from_slice(trailer);
    out.extend_from_slice(format!("\nstartxref\n{x}\n%%EOF\n").as_bytes());
    out
}

#[test]
fn encrypt_dictionary_behind_a_reference_chain_is_never_decrypted() {
    // /Encrypt 9 0 R, and object 9 is `8 0 R`: the dictionary read is
    // object 8, which stays exempt from decryption afterwards.
    let data = bytes("enc_r2_rc4_40.pdf");
    let (mut objs, trailer) = split(&data);
    let o_before = objs.iter().find(|o| o.0 == 8).unwrap().1.clone();
    objs.push((9, b"8 0 R".to_vec()));
    let trailer = String::from_utf8(trailer)
        .unwrap()
        .replace("/Encrypt 8 0 R", "/Encrypt 9 0 R")
        .replace("/Size 9", "/Size 10");
    let doc = Document::from_bytes(&build(&objs, trailer.as_bytes())).unwrap();
    let enc = doc
        .deref(&Object::Ref(kasane_pdf::IndirectRef {
            number: 8,
            generation: 0,
        }))
        .unwrap();
    let o = match enc.as_dict().unwrap().get(b"O".as_slice()) {
        Some(Object::String(s)) => s.clone(),
        other => panic!("{other:?}"),
    };
    let hex: String = o.iter().map(|b| format!("{b:02x}")).collect();
    assert!(String::from_utf8_lossy(&o_before).contains(&hex));
}

/// `name` rebuilt with `from` replaced by `to` in object `num` (and the
/// trailer when `num` is 0); offsets stay correct.
fn edited(name: &str, num: u32, from: &str, to: &str) -> Vec<u8> {
    let (mut objs, mut trailer) = split(&bytes(name));
    // Bytes, not text: stream bodies are binary.
    let swap = |b: &mut Vec<u8>| {
        let at = b
            .windows(from.len())
            .position(|w| w == from.as_bytes())
            .unwrap_or_else(|| panic!("{name} {num}: {from:?} not found"));
        b.splice(at..at + from.len(), to.bytes());
    };
    if num == 0 {
        swap(&mut trailer);
    } else {
        swap(&mut objs.iter_mut().find(|o| o.0 == num).unwrap().1);
    }
    build(&objs, &trailer)
}

/// Replace the raw data of stream object `num` (a qpdf fixture, whose
/// data runs from `stream\n` to `endstream`) and fix its `/Length`.
fn with_stream_data(name: &str, num: u32, f: impl Fn(&[u8]) -> Vec<u8>) -> Vec<u8> {
    let (mut objs, trailer) = split(&bytes(name));
    let o = &mut objs.iter_mut().find(|o| o.0 == num).unwrap().1;
    let find = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n).unwrap();
    let s = find(o, b"stream\n") + 7;
    let head = String::from_utf8_lossy(&o[..s]).into_owned();
    let len: usize = head
        .split("/Length ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let data = f(&o[s..s + len]);
    let head = head.replacen(
        &format!("/Length {len}"),
        &format!("/Length {}", data.len()),
        1,
    );
    let mut body = head.into_bytes();
    body.extend_from_slice(&data);
    body.extend_from_slice(b"endstream");
    *o = body;
    build(&objs, &trailer)
}

fn err_of(pdf: &[u8], pw: &str) -> Error {
    match Document::open(pdf, &OpenOptions::new().with_password(pw)) {
        Ok(_) => panic!("opened"),
        Err(e) => e,
    }
}

#[test]
fn malformed_encrypt_dictionaries_are_structure_errors() {
    // Each is a required entry missing, a wrong type or an inconsistent
    // value (ISO 32000-2 Tables 20-22, 25): Error::Structure, never a
    // guessed default and never Error::Encrypted (no password could
    // open it).
    let cases: &[(&str, u32, &str, &str)] = &[
        ("enc_r4_aes128.pdf", 8, "/P -4 ", ""),
        ("enc_r4_aes128.pdf", 8, "/R 4 ", ""),
        ("enc_r4_aes128.pdf", 8, "/R 4 ", "/R 3 "),
        ("enc_r4_aes128.pdf", 8, "/StmF /StdCF", "/StmF /Nope"),
        // Not a name: must not fall back to /Identity (unencrypted).
        ("enc_r4_aes128.pdf", 8, "/StmF /StdCF", "/StmF (StdCF)"),
        ("enc_r4_aes128.pdf", 8, "/StrF /StdCF", "/StrF 1"),
        (
            "enc_r4_aes128.pdf",
            8,
            "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV2 /Length 16 >> >>",
            "/CF /StdCF",
        ),
        // Two RC4 filters with different key lengths: one file key.
        (
            "enc_r4_aes128.pdf",
            8,
            "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV2 /Length 16 >> >>",
            "/CF << /StdCF << /CFM /V2 /Length 16 >> /X << /CFM /V2 /Length 5 >> >>",
        ),
        // AES-128 needs 128 bits; a 40-bit RC4 filter beside it conflicts.
        (
            "enc_r4_aes128.pdf",
            8,
            "/CF << /StdCF << /AuthEvent /DocOpen /CFM /AESV2 /Length 16 >> >>",
            "/CF << /StdCF << /CFM /AESV2 >> /X << /CFM /V2 /Length 5 >> >>",
        ),
        ("enc_r4_aes128.pdf", 0, "/ID [", "/XD ["),
        ("enc_r3_rc4_128.pdf", 8, "/Length 128", "/Length /L"),
        ("enc_r3_rc4_128.pdf", 8, "/Length 128", "/Length 36"),
        ("enc_r6_aes256_empty.pdf", 8, "/Perms <", "/Xerms <"),
        (
            "enc_r6_aes256_empty.pdf",
            8,
            "/StmF /StdCF",
            "/StmF /StdCF /StrF /StdCF /EFF 7",
        ),
    ];
    for (name, num, from, to) in cases {
        let pdf = edited(name, *num, from, to);
        let e = err_of(&pdf, "usr");
        assert!(
            matches!(e, Error::Structure(_)),
            "{name}: {from:?} -> {to:?}: {e:?}"
        );
    }
}

#[test]
fn unimplemented_methods_are_unsupported() {
    let cases: &[(&str, &str, &str)] = &[
        ("enc_r4_aes128.pdf", "/CFM /AESV2", "/CFM /None"),
        ("enc_r4_aes128.pdf", "/CFM /AESV2", "/CFM /Foo"),
        ("enc_r4_aes128.pdf", "/CFM /AESV2", "/CFM /AESV3"),
        ("enc_r4_aes128.pdf", "/V 4 >>", "/V 6 >>"),
        ("enc_r4_aes128.pdf", "/V 4 >>", ">>"),
        (
            "enc_r4_aes128.pdf",
            "/Filter /Standard",
            "/Filter /Adobe.PubSec",
        ),
    ];
    for (name, from, to) in cases {
        let e = err_of(&edited(name, 8, from, to), "usr");
        assert!(
            matches!(e, Error::Unsupported { .. }),
            "{from:?} -> {to:?}: {e:?}"
        );
    }
}

#[test]
fn crypt_stream_filter_selects_its_crypt_filter() {
    // §7.4.10: /Crypt first in /Filter names the crypt filter in its
    // /DecodeParms (default /Identity) and is removed before decoding.
    let plain = fingerprint(&Document::from_bytes(&bytes("base.pdf")).unwrap());
    let with = |parms: &str| {
        edited(
            "enc_r6_aes256_empty.pdf",
            6,
            "/Filter /FlateDecode",
            &format!("/Filter [ /Crypt /FlateDecode ] /DecodeParms [ {parms} null ]"),
        )
    };
    let doc = Document::from_bytes(&with("<< /Name /StdCF >>")).unwrap();
    assert_eq!(fingerprint(&doc), plain);
    // /Identity (explicit or by default): the data is declared clear,
    // so the ciphertext is handed to Flate, which refuses it.
    for parms in ["<< /Name /Identity >>", "<< >>"] {
        let doc = Document::from_bytes(&with(parms)).unwrap();
        let e = doc.page_contents(&doc.pages()[0]).unwrap_err();
        assert!(matches!(e, Error::Parse(_)), "{parms}: {e:?}");
    }
    // Undefined, or not a name: refused, not taken as /Identity.
    for parms in ["<< /Name /Nope >>", "<< /Name (StdCF) >>"] {
        let doc = Document::from_bytes(&with(parms)).unwrap();
        let e = doc.page_contents(&doc.pages()[0]).unwrap_err();
        assert!(matches!(e, Error::Structure(_)), "{parms}: {e:?}");
    }
}

#[test]
fn damaged_ciphertext_is_a_parse_error() {
    for name in ["enc_r4_aes128.pdf", "enc_r6_aes256.pdf"] {
        // Truncated: no longer a whole number of AES blocks.
        let cut = with_stream_data(name, 6, |d| d[..d.len() - 1].to_vec());
        // Last block altered: PKCS#5 padding no longer valid.
        let flip = with_stream_data(name, 6, |d| {
            let mut d = d.to_vec();
            let n = d.len();
            d[n - 17] ^= 0x55;
            d
        });
        // IV only: empty plaintext, which Flate then refuses.
        let iv_only = with_stream_data(name, 6, |d| d[..16].to_vec());
        for pdf in [cut, flip, iv_only] {
            let doc = Document::open(&pdf, &OpenOptions::new().with_password("usr")).unwrap();
            let e = doc.page_contents(&doc.pages()[0]).unwrap_err();
            assert!(matches!(e, Error::Parse(_)), "{name}: {e:?}");
        }
        // A truncated encrypted string (the /Info title) likewise.
        let short = edited(name, 2, "/Title <", "/Title <00");
        let doc = Document::open(&short, &OpenOptions::new().with_password("usr")).unwrap();
        let info = doc.trailer().get(b"Info".as_slice()).unwrap().clone();
        assert!(matches!(doc.deref(&info), Err(Error::Parse(_))), "{name}");
    }
}

#[test]
fn damaged_encrypted_object_stream_is_refused() {
    // The catalog lives in an AES-encrypted object stream; altering one
    // ciphertext byte (same length, so every offset stays valid) must
    // fail the load, in strict and in recovery mode.
    let data = bytes("enc_r4_aes128_objstm.pdf");
    let at = data.windows(11).position(|w| w == b"/ObjStm /Le").unwrap();
    let s = at
        + data[at..]
            .windows(7)
            .position(|w| w == b"stream\n")
            .unwrap()
        + 7;
    for off in [20, 40] {
        let mut bad = data.clone();
        bad[s + off] ^= 0xFF;
        for rec in [false, true] {
            let r = Document::open(&bad, &OpenOptions::new().with_recovery(rec));
            assert!(r.is_err(), "offset {off}, recovery {rec}");
        }
    }
}
