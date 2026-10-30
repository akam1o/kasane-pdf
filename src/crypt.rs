//! The Standard security handler (ISO 32000-2 §7.6): authentication
//! against `/O` `/U` (`/OE` `/UE` `/Perms`) and decryption of strings
//! and streams, revisions 2–6, RC4 / AES-128 / AES-256, crypt filters.
//!
//! The ciphers and hashes come from RustCrypto (`aes`, `cbc`, `rc4`,
//! `md-5`, `sha2`); what is implemented here is the PDF protocol on top
//! of them — the padding string, Algorithms 2, 2.A, 2.B, 4–7, object
//! keys and the crypt-filter selection. Only decryption is implemented;
//! the writers refuse encrypted documents.
//!
//! Secrets (the password, the file key, object keys) are held in
//! `Zeroizing` buffers and never formatted: `Debug` on [`Crypt`] and on
//! `OpenOptions` redacts them, and no error message contains them.

use std::collections::HashMap;

use aes::cipher::block_padding::{NoPadding, Pkcs7};
use aes::cipher::{Array, StreamCipher};
use aes::cipher::{BlockCipherDecrypt, BlockModeDecrypt, BlockModeEncrypt, KeyInit, KeyIvInit};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};
use zeroize::Zeroizing;

use crate::error::Error;
use crate::object::{Dict, IndirectRef, Object};
use crate::warning::{RecoveryRule, Warning};

/// The 32-byte padding string of Algorithm 2 step (a).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// How a class of strings or streams is encrypted.
///
/// New methods may be added in minor releases; match with a wildcard arm.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptMethod {
    /// Not encrypted (the `/Identity` crypt filter).
    Identity,
    /// RC4 (`/V2`, or encryption versions 1 and 2).
    Rc4,
    /// AES-128 in CBC mode (`/AESV2`).
    Aes128,
    /// AES-256 in CBC mode (`/AESV3`).
    Aes256,
}

/// Which password opened the document.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authentication {
    /// The user password (possibly the empty one).
    User,
    /// The owner password.
    Owner,
}

/// What a document's `/Encrypt` dictionary declares, and how it was
/// opened. Read from the file; nothing here is enforced.
///
/// **Permissions are not security.** `/P` lists what the author asks a
/// conforming reader to allow; anyone who can open the document can
/// read all of its content. kasane-pdf reports the bits and does not
/// use them to grant or withhold anything (its writers refuse every
/// encrypted document).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionInfo {
    /// `/V` — the algorithm version (1, 2, 4 or 5).
    pub version: u8,
    /// `/R` — the Standard security handler revision (2–6).
    pub revision: u8,
    /// Length of the file encryption key in bits.
    pub key_bits: u32,
    /// Method for strings (`/StrF`).
    pub string_method: CryptMethod,
    /// Method for streams (`/StmF`).
    pub stream_method: CryptMethod,
    /// `/P`, the user access permissions, as the signed 32-bit value
    /// the file stores (bit 1 is the least significant).
    pub permissions: i32,
    /// `/EncryptMetadata` (true when absent).
    pub encrypt_metadata: bool,
    /// Which password authenticated.
    pub authenticated_as: Authentication,
    /// The document opened with the empty user password — it is
    /// readable by anyone; the encryption provides no confidentiality.
    pub empty_user_password: bool,
}

impl EncryptionInfo {
    /// Whether permission bit `bit` (1-based, Table 22 — e.g. 3 print,
    /// 4 modify, 5 copy/extract, 6 annotate) is set in `/P`. Advisory.
    pub fn permission(&self, bit: u8) -> bool {
        (1..=32).contains(&bit) && (self.permissions as u32 >> (bit - 1)) & 1 == 1
    }
}

/// An authenticated security handler: the file key and the rules for
/// which object is decrypted how.
pub(crate) struct Crypt {
    key: Zeroizing<Vec<u8>>,
    version: u8,
    strf: CryptMethod,
    stmf: CryptMethod,
    eff: CryptMethod,
    filters: HashMap<Vec<u8>, CryptMethod>,
    encrypt_metadata: bool,
    /// The `/Encrypt` dictionary's own object (never decrypted).
    encrypt_ref: Option<IndirectRef>,
    info: EncryptionInfo,
}

impl std::fmt::Debug for Crypt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crypt")
            .field("key", &"<redacted>")
            .field("info", &self.info)
            .finish()
    }
}

fn structure(msg: &str) -> Error {
    Error::Structure(format!("/Encrypt: {msg}"))
}

fn unsupported(detail: String) -> Error {
    Error::Unsupported {
        feature: "encryption",
        detail,
    }
}

fn int(d: &Dict, k: &[u8]) -> Option<i64> {
    d.get(k).and_then(|o| o.as_i64())
}

fn bytes<'a>(d: &'a Dict, k: &[u8]) -> Option<&'a [u8]> {
    match d.get(k) {
        Some(Object::String(s)) => Some(s),
        _ => None,
    }
}

/// `/key` if it is a string of at least `specified` bytes; a longer
/// one is recorded (only the leading `specified` bytes are ever used).
fn sized<'a>(
    d: &'a Dict,
    key: &'static str,
    specified: usize,
    warnings: &mut Vec<Warning>,
) -> Option<&'a [u8]> {
    let s = bytes(d, key.as_bytes()).filter(|s| s.len() >= specified)?;
    if s.len() > specified {
        warnings.push(Warning::OverlongEncryptionEntry {
            key,
            length: s.len(),
            specified,
        });
    }
    Some(s)
}

/// The characters PDFDocEncoding (Annex D, Table D.2) places outside
/// the Latin-1 positions.
const PDFDOC_SPECIALS: [(char, u8); 40] = [
    ('\u{02D8}', 0x18),
    ('\u{02C7}', 0x19),
    ('\u{02C6}', 0x1A),
    ('\u{02D9}', 0x1B),
    ('\u{02DD}', 0x1C),
    ('\u{02DB}', 0x1D),
    ('\u{02DA}', 0x1E),
    ('\u{02DC}', 0x1F),
    ('\u{2022}', 0x80),
    ('\u{2020}', 0x81),
    ('\u{2021}', 0x82),
    ('\u{2026}', 0x83),
    ('\u{2014}', 0x84),
    ('\u{2013}', 0x85),
    ('\u{0192}', 0x86),
    ('\u{2044}', 0x87),
    ('\u{2039}', 0x88),
    ('\u{203A}', 0x89),
    ('\u{2212}', 0x8A),
    ('\u{2030}', 0x8B),
    ('\u{201E}', 0x8C),
    ('\u{201C}', 0x8D),
    ('\u{201D}', 0x8E),
    ('\u{2018}', 0x8F),
    ('\u{2019}', 0x90),
    ('\u{201A}', 0x91),
    ('\u{2122}', 0x92),
    ('\u{FB01}', 0x93),
    ('\u{FB02}', 0x94),
    ('\u{0141}', 0x95),
    ('\u{0152}', 0x96),
    ('\u{0160}', 0x97),
    ('\u{0178}', 0x98),
    ('\u{017D}', 0x99),
    ('\u{0131}', 0x9A),
    ('\u{0142}', 0x9B),
    ('\u{0153}', 0x9C),
    ('\u{0161}', 0x9D),
    ('\u{017E}', 0x9E),
    ('\u{20AC}', 0xA0),
];

/// `s` in PDFDocEncoding, or `None` if a character has no code there.
fn to_pdfdoc(s: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut out = Zeroizing::new(Vec::with_capacity(s.len()));
    for c in s.chars() {
        let b = match c {
            '\u{00A0}' | '\u{00AD}' | '\u{007F}' | '\u{0080}'..='\u{009F}' => return None,
            '\t' | '\n' | '\r' | ' '..='\u{00FF}' => c as u8,
            _ => PDFDOC_SPECIALS
                .iter()
                .find(|(u, _)| *u == c)
                .map(|(_, b)| *b)?,
        };
        out.push(b);
    }
    Some(out)
}

/// The byte strings a supplied password is tried as, in order. The
/// caller passes bytes; the specification defines the password as
/// PDFDocEncoding text for revisions 2–4 and as UTF-8 after SASLprep
/// (RFC 4013) for revisions 5–6 (§7.6.4.3.3 Algorithm 2.A). So:
/// R5/R6 — the SASLprep form of a UTF-8 password first (when it
/// differs), then the bytes as given; R2–R4 — the bytes as given, then
/// a non-ASCII UTF-8 password converted to PDFDocEncoding. Every
/// candidate comes from the caller's own password; none weakens the
/// check. A password SASLprep refuses (prohibited or unassigned code
/// points) is tried as given only.
fn candidates(revision: u8, pw: &[u8]) -> Vec<Zeroizing<Vec<u8>>> {
    let raw = Zeroizing::new(pw.to_vec());
    let text = std::str::from_utf8(pw).ok();
    let mut out = Vec::with_capacity(2);
    if revision >= 5 {
        // The error value names the offending character: discarded.
        if let Some(prepped) = text.and_then(|t| stringprep::saslprep(t).ok()) {
            if prepped.as_bytes() != pw {
                out.push(Zeroizing::new(prepped.as_bytes().to_vec()));
            }
        }
        out.push(raw);
    } else {
        out.push(raw);
        if let Some(t) = text.filter(|t| !t.is_ascii()) {
            if let Some(doc) = to_pdfdoc(t) {
                out.push(doc);
            }
        }
    }
    out
}

/// Constant-time equality (no early exit on the first mismatch).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn rc4(key: &[u8], data: &mut [u8]) {
    // Key lengths here are 5..=16 bytes, within RC4's range.
    let mut c = rc4::Rc4::new_from_slice(key).expect("RC4 key length 1..=256");
    c.apply_keystream(data);
}

fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut h = Md5::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// The password padded or truncated to 32 bytes (Algorithm 2 step a).
fn padded(pw: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    let n = pw.len().min(32);
    out[..n].copy_from_slice(&pw[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

/// What Algorithms 2–7 need from the dictionary (revisions 2–4).
struct Legacy<'a> {
    r: u8,
    n: usize,
    o: &'a [u8],
    u: &'a [u8],
    p: i32,
    id0: &'a [u8],
    encrypt_metadata: bool,
}

impl Legacy<'_> {
    /// Algorithm 2: the file key from a (user) password.
    fn file_key(&self, pw: &[u8]) -> Zeroizing<Vec<u8>> {
        let pw = padded(pw);
        let mut h = Md5::new();
        h.update(&pw[..]);
        h.update(self.o);
        h.update(self.p.to_le_bytes());
        h.update(self.id0);
        if self.r >= 4 && !self.encrypt_metadata {
            h.update([0xFF; 4]);
        }
        let mut d: [u8; 16] = h.finalize().into();
        if self.r >= 3 {
            for _ in 0..50 {
                d = md5(&[&d[..self.n]]);
            }
        }
        Zeroizing::new(d[..self.n].to_vec())
    }

    /// Algorithms 4 / 5: does `key` reproduce `/U`?
    fn check_user(&self, key: &[u8]) -> bool {
        if self.r == 2 {
            let mut u = PAD;
            rc4(key, &mut u);
            ct_eq(&u, &self.u[..32])
        } else {
            let mut u = md5(&[&PAD, self.id0]);
            rc4(key, &mut u);
            for i in 1..=19u8 {
                let k: Zeroizing<Vec<u8>> = Zeroizing::new(key.iter().map(|b| b ^ i).collect());
                rc4(&k, &mut u);
            }
            ct_eq(&u, &self.u[..16])
        }
    }

    /// Algorithm 7: the padded user password an owner password
    /// unlocks from `/O`.
    fn user_from_owner(&self, owner: &[u8]) -> Zeroizing<Vec<u8>> {
        let pw = padded(owner);
        let mut d = md5(&[&pw[..]]);
        if self.r >= 3 {
            for _ in 0..50 {
                d = md5(&[&d[..]]);
            }
        }
        let key = Zeroizing::new(d[..self.n].to_vec());
        let mut out = Zeroizing::new(self.o[..32].to_vec());
        if self.r == 2 {
            rc4(&key, &mut out);
        } else {
            for i in (0..=19u8).rev() {
                let k: Zeroizing<Vec<u8>> = Zeroizing::new(key.iter().map(|b| b ^ i).collect());
                rc4(&k, &mut out);
            }
        }
        out
    }
}

/// Algorithm 2.B (revision 6) — or a single SHA-256 for revision 5.
fn hash_v5(r: u8, pw: &[u8], salt: &[u8], udata: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut k: Zeroizing<Vec<u8>> = Zeroizing::new({
        let mut h = Sha256::new();
        h.update(pw);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    });
    if r == 6 {
        let mut round: u32 = 0;
        loop {
            let unit = pw.len() + k.len() + udata.len();
            let mut k1: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(unit * 64));
            for _ in 0..64 {
                k1.extend_from_slice(pw);
                k1.extend_from_slice(&k);
                k1.extend_from_slice(udata);
            }
            // `unit * 64` is a multiple of 16, so no padding is needed.
            let len = k1.len();
            let enc = cbc::Encryptor::<aes::Aes128>::new_from_slices(&k[..16], &k[16..32])
                .expect("AES-128 key and IV are 16 bytes");
            enc.encrypt_padded::<NoPadding>(&mut k1, len)
                .expect("length is a multiple of the block size");
            let e = &k1[..];
            let m = e[..16].iter().map(|b| *b as u32).sum::<u32>() % 3;
            k = Zeroizing::new(match m {
                0 => Sha256::digest(e).to_vec(),
                1 => Sha384::digest(e).to_vec(),
                _ => Sha512::digest(e).to_vec(),
            });
            round += 1;
            if round >= 64 && (*e.last().expect("non-empty") as u32) <= round - 32 {
                break;
            }
        }
    }
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&k[..32]);
    out
}

/// AES-256-CBC with a zero IV and no padding — `/UE` and `/OE`.
fn unwrap_key(kek: &[u8], wrapped: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut buf = Zeroizing::new(wrapped[..32].to_vec());
    let dec = cbc::Decryptor::<aes::Aes256>::new_from_slices(kek, &[0u8; 16])
        .expect("AES-256 key is 32 bytes");
    dec.decrypt_padded::<NoPadding>(&mut buf)
        .expect("32 bytes is a multiple of the block size");
    buf
}

/// Algorithm 1: the key for objects of `r` under `method`; the file key
/// itself for AES-256 (version 5).
fn object_key_from(
    key: &[u8],
    version: u8,
    r: IndirectRef,
    method: CryptMethod,
) -> Zeroizing<Vec<u8>> {
    if version == 5 {
        return Zeroizing::new(key.to_vec());
    }
    let n = r.number.to_le_bytes();
    let g = r.generation.to_le_bytes();
    let salt: &[u8] = if method == CryptMethod::Aes128 {
        b"sAlT"
    } else {
        b""
    };
    let d = Zeroizing::new(md5(&[key, &n[..3], &g[..2], salt]));
    Zeroizing::new(d[..(key.len() + 5).min(16)].to_vec())
}

/// The first bytes (at most about 1 KiB) of `data` decrypted, without
/// padding checks; `None` if the key does not fit the cipher or there
/// is no whole AES block.
fn decrypt_prefix(method: CryptMethod, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    const PREFIX: usize = 1024;
    match method {
        CryptMethod::Identity => Some(data[..data.len().min(PREFIX)].to_vec()),
        CryptMethod::Rc4 => {
            let mut out = data[..data.len().min(PREFIX)].to_vec();
            rc4(key, &mut out);
            Some(out)
        }
        CryptMethod::Aes128 | CryptMethod::Aes256 => {
            let blocks = (data.len().min(PREFIX + 16) / 16).checked_sub(1)?;
            if blocks == 0 {
                return None;
            }
            let (iv, body) = data.split_at(16);
            let mut buf = body[..blocks * 16].to_vec();
            let ok = if method == CryptMethod::Aes128 {
                cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
                    .ok()?
                    .decrypt_padded::<NoPadding>(&mut buf)
                    .is_ok()
            } else {
                cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
                    .ok()?
                    .decrypt_padded::<NoPadding>(&mut buf)
                    .is_ok()
            };
            ok.then_some(buf)
        }
    }
}

/// Whether `prefix` begins a zlib stream that inflates without error
/// as far as it goes (a random prefix fails the header check or the
/// first block almost always).
fn inflate_starts(prefix: &[u8]) -> bool {
    let mut z = flate2::Decompress::new(true);
    let mut out = vec![0u8; 4096];
    z.decompress(prefix, &mut out, flate2::FlushDecompress::None)
        .is_ok()
        && z.total_out() > 0
}

/// Decrypt `data` with `method` under `key` (the object key, or the
/// file key for AES-256). Output is never longer than the input.
fn decrypt_bytes(method: CryptMethod, key: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    match method {
        CryptMethod::Identity => Ok(data.to_vec()),
        CryptMethod::Rc4 => {
            let mut out = data.to_vec();
            rc4(key, &mut out);
            Ok(out)
        }
        CryptMethod::Aes128 | CryptMethod::Aes256 => {
            if data.is_empty() {
                return Ok(Vec::new());
            }
            if data.len() < 16 || !data.len().is_multiple_of(16) {
                return Err(Error::Parse(format!(
                    "AES-encrypted data is {} bytes (must be a positive multiple of 16 including the IV)",
                    data.len()
                )));
            }
            let (iv, body) = data.split_at(16);
            let mut buf = body.to_vec();
            if buf.is_empty() {
                return Ok(buf);
            }
            let n = if method == CryptMethod::Aes128 {
                cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
                    .map_err(|_| Error::Parse("AES-128 key length".into()))?
                    .decrypt_padded::<Pkcs7>(&mut buf)
                    .map(|s| s.len())
            } else {
                cbc::Decryptor::<aes::Aes256>::new_from_slices(key, iv)
                    .map_err(|_| Error::Parse("AES-256 key length".into()))?
                    .decrypt_padded::<Pkcs7>(&mut buf)
                    .map(|s| s.len())
            }
            .map_err(|_| Error::Parse("AES-decrypted data has invalid padding".into()))?;
            buf.truncate(n);
            Ok(buf)
        }
    }
}

impl Crypt {
    /// Authenticate `password` (user, then owner; `None` = the empty
    /// user password) against the `/Encrypt` dictionary `enc`.
    /// `Error::Encrypted` when it does not authenticate.
    /// The returned warnings belong to the document.
    pub(crate) fn open(
        enc: &Dict,
        encrypt_ref: Option<IndirectRef>,
        trailer: &Dict,
        password: Option<&[u8]>,
    ) -> Result<(Crypt, Vec<Warning>), Error> {
        let mut warnings = Vec::new();
        match enc.get(b"Filter".as_slice()).and_then(|o| o.as_name()) {
            Some(b"Standard") => {}
            Some(other) => {
                return Err(unsupported(format!(
                    "security handler /{} (only /Standard is implemented)",
                    String::from_utf8_lossy(other)
                )))
            }
            None => return Err(structure("no /Filter")),
        }
        let v = int(enc, b"V").unwrap_or(0);
        let r = int(enc, b"R").ok_or_else(|| structure("no /R"))?;
        let version: u8 = match v {
            1 | 2 | 4 | 5 => v as u8,
            // /V 0 (also the default when absent) is "an algorithm that
            // is undocumented and no longer supported" (Table 20).
            0 => {
                return Err(unsupported(
                    "/V 0 or absent (undocumented algorithm)".into(),
                ))
            }
            _ => return Err(unsupported(format!("/V {v}"))),
        };
        let revision: u8 = match r {
            2..=6 => r as u8,
            _ => return Err(unsupported(format!("/R {r}"))),
        };
        let consistent = matches!(
            (version, revision),
            (1, 2) | (1, 3) | (2, 2) | (2, 3) | (4, 4) | (5, 5) | (5, 6)
        );
        if !consistent {
            return Err(structure(&format!("/V {version} with /R {revision}")));
        }
        let p = int(enc, b"P").ok_or_else(|| structure("no /P"))?;
        let p: i32 = if (i32::MIN as i64..=i32::MAX as i64).contains(&p) {
            p as i32
        } else if (0..=u32::MAX as i64).contains(&p) {
            p as u32 as i32
        } else {
            return Err(structure("/P is not a 32-bit value"));
        };
        let encrypt_metadata = match enc.get(b"EncryptMetadata".as_slice()) {
            None => true,
            Some(o) => o
                .as_bool()
                .ok_or_else(|| structure("/EncryptMetadata is not a boolean"))?,
        };

        // Crypt filters (V 4 and 5).
        let mut filters: HashMap<Vec<u8>, CryptMethod> = HashMap::new();
        let mut key_bits: u32 = match version {
            1 => 40,
            2 => {
                let l = int(enc, b"Length").unwrap_or(40);
                if !(40..=128).contains(&l) || l % 8 != 0 {
                    return Err(structure(&format!("/Length {l}")));
                }
                l as u32
            }
            4 => 128,
            _ => 256,
        };
        let (mut strf, mut stmf, mut eff) = (CryptMethod::Rc4, CryptMethod::Rc4, CryptMethod::Rc4);
        if version >= 4 {
            if let Some(cf) = enc.get(b"CF".as_slice()) {
                let cf = cf
                    .as_dict()
                    .ok_or_else(|| structure("/CF is not a dictionary"))?;
                for (name, f) in cf {
                    let f = f
                        .as_dict()
                        .ok_or_else(|| structure("crypt filter is not a dictionary"))?;
                    let m = match f.get(b"CFM".as_slice()).and_then(|o| o.as_name()) {
                        None | Some(b"None") => {
                            return Err(unsupported(
                                "crypt filter /CFM /None (application-defined decryption)".into(),
                            ))
                        }
                        Some(b"V2") if version == 4 => CryptMethod::Rc4,
                        Some(b"AESV2") if version == 4 => CryptMethod::Aes128,
                        Some(b"AESV3") if version == 5 => CryptMethod::Aes256,
                        Some(other) => {
                            return Err(unsupported(format!(
                                "crypt filter method /{} with /V {version}",
                                String::from_utf8_lossy(other)
                            )))
                        }
                    };
                    if m == CryptMethod::Rc4 {
                        // /Length in bytes or bits, as writers differ.
                        if let Some(l) = int(f, b"Length") {
                            let bits = if l <= 16 { l * 8 } else { l };
                            if !(40..=128).contains(&bits) || bits % 8 != 0 {
                                return Err(structure(&format!("crypt filter /Length {l}")));
                            }
                            key_bits = bits as u32;
                        }
                    }
                    filters.insert(name.clone(), m);
                }
            }
            let pick = |k: &[u8], default: CryptMethod| -> Result<CryptMethod, Error> {
                match enc.get(k).and_then(|o| o.as_name()) {
                    None | Some(b"Identity") => Ok(default),
                    Some(n) => filters.get(n).copied().ok_or_else(|| {
                        structure(&format!(
                            "/{} names an undefined crypt filter /{}",
                            String::from_utf8_lossy(k),
                            String::from_utf8_lossy(n)
                        ))
                    }),
                }
            };
            strf = pick(b"StrF", CryptMethod::Identity)?;
            stmf = pick(b"StmF", CryptMethod::Identity)?;
            eff = match enc.get(b"EFF".as_slice()) {
                None => stmf,
                Some(_) => pick(b"EFF", CryptMethod::Identity)?,
            };
            if version == 5 && [strf, stmf, eff].contains(&CryptMethod::Rc4) {
                return Err(structure("RC4 crypt filter with /V 5"));
            }
        }
        let n = (key_bits / 8) as usize;

        let pws = candidates(revision, password.unwrap_or(b""));
        let (key, auth, empty_user) = if revision <= 4 {
            let o = sized(enc, "O", 32, &mut warnings);
            let u = sized(enc, "U", 32, &mut warnings);
            let (Some(o), Some(u)) = (o, u) else {
                return Err(structure("/O and /U must be strings of at least 32 bytes"));
            };
            let id0 = match trailer.get(b"ID".as_slice()) {
                Some(Object::Array(a)) => match a.first() {
                    Some(Object::String(s)) => s.as_slice(),
                    _ => return Err(structure("trailer /ID[0] is not a string")),
                },
                _ => return Err(structure("trailer has no /ID (required with /R 2–4)")),
            };
            let l = Legacy {
                r: revision,
                n,
                o,
                u,
                p,
                id0,
                encrypt_metadata,
            };
            let empty = {
                let k = l.file_key(b"");
                l.check_user(&k)
            };
            let as_user = pws.iter().find_map(|pw| {
                let k = l.file_key(pw);
                l.check_user(&k).then_some(k)
            });
            let (k, auth) = match as_user {
                Some(k) => (k, Authentication::User),
                None => {
                    let as_owner = pws.iter().find_map(|pw| {
                        let k = l.file_key(&l.user_from_owner(pw));
                        l.check_user(&k).then_some(k)
                    });
                    (as_owner.ok_or(Error::Encrypted)?, Authentication::Owner)
                }
            };
            (k, auth, empty)
        } else {
            let o = sized(enc, "O", 48, &mut warnings);
            let u = sized(enc, "U", 48, &mut warnings);
            let oe = sized(enc, "OE", 32, &mut warnings);
            let ue = sized(enc, "UE", 32, &mut warnings);
            let (Some(o), Some(u), Some(oe), Some(ue)) = (o, u, oe, ue) else {
                return Err(structure(
                    "/O and /U must be at least 48 bytes, /OE and /UE at least 32",
                ));
            };
            // Algorithm 2.A step (a): at most 127 bytes are used.
            let trunc = |pw: &[u8]| pw[..pw.len().min(127)].to_vec();
            let user_ok = |pw: &[u8]| ct_eq(&hash_v5(revision, pw, &u[32..40], b"")[..], &u[..32]);
            let owner_ok =
                |pw: &[u8]| ct_eq(&hash_v5(revision, pw, &o[32..40], &u[..48])[..], &o[..32]);
            let empty = user_ok(b"");
            let pws: Vec<Zeroizing<Vec<u8>>> =
                pws.iter().map(|p| Zeroizing::new(trunc(p))).collect();
            let (key, auth) = if let Some(pw) = pws.iter().find(|p| user_ok(p)) {
                let kek = hash_v5(revision, pw, &u[40..48], b"");
                (unwrap_key(&kek[..], ue), Authentication::User)
            } else if let Some(pw) = pws.iter().find(|p| owner_ok(p)) {
                let kek = hash_v5(revision, pw, &o[40..48], &u[..48]);
                (unwrap_key(&kek[..], oe), Authentication::Owner)
            } else {
                return Err(Error::Encrypted);
            };
            // Algorithm 13: /Perms, encrypted with the file key, must
            // repeat /P and /EncryptMetadata (required for R 6).
            match bytes(enc, b"Perms") {
                Some(perms) if perms.len() >= 16 => {
                    if perms.len() > 16 {
                        warnings.push(Warning::OverlongEncryptionEntry {
                            key: "Perms",
                            length: perms.len(),
                            specified: 16,
                        });
                    }
                    let mut block = Array::try_from(&perms[..16]).expect("16 bytes");
                    aes::Aes256::new_from_slice(&key)
                        .expect("AES-256 key is 32 bytes")
                        .decrypt_block(&mut block);
                    let ok = &block[9..12] == b"adb"
                        && block[..4] == p.to_le_bytes()
                        && block[8] == if encrypt_metadata { b'T' } else { b'F' };
                    if !ok {
                        return Err(structure(
                            "/Perms does not match /P and /EncryptMetadata (altered dictionary)",
                        ));
                    }
                }
                Some(_) => return Err(structure("/Perms is shorter than 16 bytes")),
                None if revision == 6 => return Err(structure("no /Perms (required with /R 6)")),
                None => {}
            }
            (key, auth, empty)
        };
        if version < 4 {
            strf = CryptMethod::Rc4;
            stmf = CryptMethod::Rc4;
            eff = CryptMethod::Rc4;
        }
        Ok((
            Crypt {
                key,
                version,
                strf,
                stmf,
                eff,
                filters,
                encrypt_metadata,
                encrypt_ref,
                info: EncryptionInfo {
                    version,
                    revision,
                    key_bits,
                    string_method: strf,
                    stream_method: stmf,
                    permissions: p,
                    encrypt_metadata,
                    authenticated_as: auth,
                    empty_user_password: empty_user,
                },
            },
            warnings,
        ))
    }

    pub(crate) fn info(&self) -> &EncryptionInfo {
        &self.info
    }

    #[cfg(test)]
    pub(crate) fn key_for_test(&self) -> Vec<u8> {
        self.key.to_vec()
    }

    /// Revision 4 with a file key shorter than 16 bytes: Algorithm 1
    /// derives object keys from the key as it is, Acrobat from the key
    /// zero-padded to 16 bytes (see
    /// [`RecoveryRule::PaddedShortEncryptionKey`]); the two readings
    /// decrypt the same file differently.
    pub(crate) fn has_short_key(&self) -> bool {
        self.info.revision == 4 && self.key.len() < 16
    }

    /// Switch to the zero-padded key (recovery mode only, after
    /// [`Crypt::probe_short_key`] showed that only it decrypts).
    pub(crate) fn pad_short_key(&mut self) -> Warning {
        let bits = self.key.len() * 8;
        self.key.resize(16, 0);
        Warning::RecoveryApplied {
            rule: RecoveryRule::PaddedShortEncryptionKey,
            detail: format!("{bits}-bit key; only the padded key decrypts the data"),
        }
    }

    /// Evidence for the key reading: whether the start of the raw
    /// stream `(r, dict, data)`, decrypted under the key as specified
    /// and under the zero-padded key, begins a valid zlib stream.
    /// `None` when the stream cannot serve as evidence (not
    /// `/FlateDecode` first, exempt from encryption, `/Identity`).
    pub(crate) fn probe_short_key(
        &self,
        r: IndirectRef,
        dict: &Dict,
        data: &[u8],
    ) -> Option<(bool, bool)> {
        if self.encrypt_ref == Some(r) {
            return None;
        }
        let first = match dict.get(b"Filter".as_slice()) {
            Some(Object::Name(n)) => n.as_slice(),
            Some(Object::Array(a)) => a.first()?.as_name()?,
            _ => return None,
        };
        let ty = dict.get(b"Type".as_slice()).and_then(|o| o.as_name());
        if first != b"FlateDecode" || ty == Some(b"XRef") {
            return None;
        }
        let method = match ty {
            Some(b"Metadata") if !self.encrypt_metadata => return None,
            Some(b"EmbeddedFile") => self.eff,
            _ => self.stmf,
        };
        if method == CryptMethod::Identity {
            return None;
        }
        let mut padded = Zeroizing::new(self.key.to_vec());
        padded.resize(16, 0);
        let inflates = |key: &[u8]| {
            let k = object_key_from(key, self.version, r, method);
            decrypt_prefix(method, &k, data).is_some_and(|p| inflate_starts(&p))
        };
        Some((inflates(&self.key), inflates(&padded)))
    }

    /// The key for objects of `r` under `method` (Algorithm 1); the file
    /// key itself for AES-256.
    fn object_key(&self, r: IndirectRef, method: CryptMethod) -> Zeroizing<Vec<u8>> {
        object_key_from(&self.key, self.version, r, method)
    }

    /// Decrypt the strings and stream data of the indirect object `r`
    /// as read from the file (§7.6.2). Exceptions: the `/Encrypt`
    /// dictionary, cross-reference streams, signature `/Contents`, the
    /// metadata stream under `/EncryptMetadata false`.
    pub(crate) fn decrypt_object(&self, r: IndirectRef, obj: Object) -> Result<Object, Error> {
        if self.encrypt_ref == Some(r) {
            return Ok(obj);
        }
        match obj {
            Object::Stream { mut dict, data } => {
                let ty = dict.get(b"Type".as_slice()).and_then(|o| o.as_name());
                if ty == Some(b"XRef") {
                    return Ok(Object::Stream { dict, data });
                }
                let is_metadata = ty == Some(b"Metadata");
                let is_embedded = ty == Some(b"EmbeddedFile");
                let method = match self.take_crypt_filter(&mut dict)? {
                    Some(m) => m,
                    None if is_metadata && !self.encrypt_metadata => CryptMethod::Identity,
                    None if is_embedded => self.eff,
                    None => self.stmf,
                };
                let dict = self.decrypt_dict(r, dict)?;
                let data = decrypt_bytes(method, &self.object_key(r, method), &data)?;
                Ok(Object::Stream { dict, data })
            }
            other => self.decrypt_value(r, other),
        }
    }

    fn decrypt_value(&self, r: IndirectRef, obj: Object) -> Result<Object, Error> {
        Ok(match obj {
            Object::String(s) => Object::String(decrypt_bytes(
                self.strf,
                &self.object_key(r, self.strf),
                &s,
            )?),
            Object::Array(a) => Object::Array(
                a.into_iter()
                    .map(|o| self.decrypt_value(r, o))
                    .collect::<Result<_, _>>()?,
            ),
            Object::Dict(d) => Object::Dict(self.decrypt_dict(r, d)?),
            other => other,
        })
    }

    fn decrypt_dict(&self, r: IndirectRef, d: Dict) -> Result<Dict, Error> {
        // A signature dictionary's /Contents is not encrypted (§7.6.2):
        // the signature covers the file bytes.
        let is_sig = matches!(
            d.get(b"Type".as_slice()).and_then(|o| o.as_name()),
            Some(b"Sig") | Some(b"DocTimeStamp")
        ) || (d.contains_key(b"ByteRange".as_slice())
            && d.contains_key(b"Contents".as_slice()));
        let mut out = Dict::new();
        for (k, v) in d {
            let v = if is_sig && k == b"Contents" {
                v
            } else {
                self.decrypt_value(r, v)?
            };
            out.insert(k, v);
        }
        Ok(out)
    }

    /// A stream whose `/Filter` begins with `/Crypt` names its own crypt
    /// filter (§7.4.10, default `/Identity`). The entry is removed from
    /// `/Filter` and `/DecodeParms` so the filter chain never sees it.
    fn take_crypt_filter(&self, dict: &mut Dict) -> Result<Option<CryptMethod>, Error> {
        let (first_is_crypt, single) = match dict.get(b"Filter".as_slice()) {
            Some(Object::Name(n)) => (n == b"Crypt", true),
            Some(Object::Array(a)) => (
                matches!(a.first(), Some(Object::Name(n)) if n == b"Crypt"),
                false,
            ),
            _ => (false, true),
        };
        if !first_is_crypt {
            return Ok(None);
        }
        let parms = match dict.get(b"DecodeParms".as_slice()) {
            Some(Object::Dict(p)) if single => Some(p.clone()),
            Some(Object::Array(a)) => a.first().and_then(|o| o.as_dict()).cloned(),
            _ => None,
        };
        let name = parms
            .as_ref()
            .and_then(|p| p.get(b"Name".as_slice()))
            .and_then(|o| o.as_name())
            .map(|n| n.to_vec())
            .unwrap_or_else(|| b"Identity".to_vec());
        let method = if name == b"Identity" {
            CryptMethod::Identity
        } else {
            *self.filters.get(&name).ok_or_else(|| {
                structure(&format!(
                    "/Crypt filter names an undefined crypt filter /{}",
                    String::from_utf8_lossy(&name)
                ))
            })?
        };
        if single {
            dict.remove(b"Filter".as_slice());
            dict.remove(b"DecodeParms".as_slice());
        } else {
            if let Some(Object::Array(a)) = dict.get_mut(b"Filter".as_slice()) {
                a.remove(0);
            }
            if let Some(Object::Array(a)) = dict.get_mut(b"DecodeParms".as_slice()) {
                if !a.is_empty() {
                    a.remove(0);
                }
            }
        }
        Ok(Some(method))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_string_and_ct_eq() {
        assert_eq!(&padded(b"")[..], &PAD[..]);
        assert_eq!(&padded(b"ab")[..2], b"ab");
        assert_eq!(&padded(b"ab")[2..], &PAD[..30]);
        assert!(ct_eq(b"abc", b"abc") && !ct_eq(b"abc", b"abd") && !ct_eq(b"a", b"ab"));
    }

    #[test]
    fn aes_length_and_padding_are_checked() {
        let key = [7u8; 16];
        for bad in [&[0u8; 15][..], &[0u8; 17][..]] {
            assert!(matches!(
                decrypt_bytes(CryptMethod::Aes128, &key, bad),
                Err(Error::Parse(_))
            ));
        }
        // IV only: empty plaintext.
        assert!(decrypt_bytes(CryptMethod::Aes128, &key, &[0u8; 16])
            .unwrap()
            .is_empty());
        // Random block: padding almost surely invalid.
        let r = decrypt_bytes(CryptMethod::Aes128, &key, &[0x5Au8; 32]);
        assert!(r.is_err() || r.unwrap().len() < 16);
    }

    /// Known answers: the file key qpdf 12.4.2 reports
    /// (`--show-encryption-key`) for each fixture, under the password
    /// given — an independent implementation of Algorithms 2, 2.A, 2.B.
    #[test]
    fn file_keys_match_qpdf() {
        let cases: &[(&str, &str, &str)] = &[
            ("enc_r2_rc4_40.pdf", "", "91a6add96f"),
            (
                "enc_r3_rc4_128.pdf",
                "usr",
                "e35963280366c0544f05a2d6628f7188",
            ),
            (
                "enc_r4_aes128.pdf",
                "usr",
                "e35963280366c0544f05a2d6628f7188",
            ),
            (
                "enc_r4_aes128_objstm.pdf",
                "",
                "6b4d22e555248b75ff6669b5c0d25f17",
            ),
            (
                "enc_r4_rc4_objstm.pdf",
                "",
                "6b4d22e555248b75ff6669b5c0d25f17",
            ),
            (
                "enc_r5_aes256.pdf",
                "",
                "f39b2f36abefd52d86bef16c3517aac9e4e00efc0755c396d18381e4444e555d",
            ),
            (
                "enc_r6_aes256.pdf",
                "usr",
                "20950be7a4a0865e3ee6409cdf36f58f15bc20b3a045ac518b2011c798a40ad6",
            ),
            (
                "enc_r6_aes256_empty.pdf",
                "",
                "92ea67e7933c8308d39f527307b50cd05276e1404aec7fd56b47c3860a889c9c",
            ),
            (
                "enc_r6_unicode.pdf",
                "own",
                "0dca8259c1640c7ce365f93cd2fc263b86c3ba74fe7aa81aaa7b44bba6e20523",
            ),
            (
                "enc_r6_emoji.pdf",
                "own",
                "2b7776adcd9ec3acbb9d37a5e32531a862d7f0900fdef217f21546816d6b22cd",
            ),
            (
                "enc_r4_pdfdoc.pdf",
                "own",
                "827531c8651c6c28d85d4a1d395089f6",
            ),
            ("enc_r4_euro.pdf", "own", "56d1eefb6ae7b424989dc2a34dc02294"),
        ];
        for (name, pw, key) in cases {
            let path = format!(
                "{}/tests/fixtures/encrypted/{name}",
                env!("CARGO_MANIFEST_DIR")
            );
            let data = std::fs::read(path).unwrap();
            let doc = crate::Document::open(&data, &crate::OpenOptions::new().with_password(pw))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let hex: String = doc
                .file_key_for_test()
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(&hex, key, "{name}");
        }
    }

    #[test]
    fn pdfdoc_conversion_follows_annex_d() {
        assert_eq!(&to_pdfdoc("caf\u{e9}").unwrap()[..], b"caf\xE9");
        assert_eq!(
            &to_pdfdoc("\u{20AC}\u{2022}\u{FB01}\u{02D8}").unwrap()[..],
            b"\xA0\x80\x93\x18"
        );
        // No code in PDFDocEncoding: U+00A0, soft hyphen, C1 controls, CJK.
        for s in ["a\u{00A0}", "a\u{00AD}", "\u{0085}", "\u{65E5}"] {
            assert!(to_pdfdoc(s).is_none(), "{s:?}");
        }
    }

    #[test]
    fn password_candidates_per_revision() {
        let v = |r, p: &str| -> Vec<Vec<u8>> {
            candidates(r, p.as_bytes())
                .iter()
                .map(|c| c.to_vec())
                .collect()
        };
        // ASCII: one candidate everywhere.
        assert_eq!(v(6, "usr"), vec![b"usr".to_vec()]);
        assert_eq!(v(4, "usr"), vec![b"usr".to_vec()]);
        // R6: SASLprep (NFKC, non-ASCII space -> space) first, then raw.
        assert_eq!(
            v(6, "cafe\u{301}"),
            vec![
                "caf\u{e9}".as_bytes().to_vec(),
                "cafe\u{301}".as_bytes().to_vec()
            ]
        );
        assert_eq!(v(6, "a\u{3000}b")[0], b"a b".to_vec());
        // Already normalised: SASLprep is a no-op, one candidate.
        assert_eq!(v(6, "caf\u{e9}"), vec!["caf\u{e9}".as_bytes().to_vec()]);
        // Refused by SASLprep (unassigned in Unicode 3.2): raw only.
        assert_eq!(v(6, "\u{1F600}"), vec!["\u{1F600}".as_bytes().to_vec()]);
        // R2-R4: raw first, then PDFDocEncoding.
        assert_eq!(
            v(4, "caf\u{e9}"),
            vec!["caf\u{e9}".as_bytes().to_vec(), b"caf\xE9".to_vec()]
        );
        // Not UTF-8: raw only.
        assert_eq!(candidates(6, b"\xff\xfe").len(), 1);
    }

    #[test]
    fn permission_bits() {
        let info = EncryptionInfo {
            version: 2,
            revision: 3,
            key_bits: 128,
            string_method: CryptMethod::Rc4,
            stream_method: CryptMethod::Rc4,
            permissions: -3904,
            encrypt_metadata: true,
            authenticated_as: Authentication::User,
            empty_user_password: true,
        };
        // -3904 = 0xFFFFF0C0: bits 7, 8 set; 3–6 clear.
        assert!(!info.permission(3) && !info.permission(5));
        assert!(info.permission(7) && info.permission(8));
        assert!(!info.permission(0) && !info.permission(33));
    }
}
