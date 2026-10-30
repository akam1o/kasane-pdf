# Encrypted documents

Since 0.3.0 kasane-pdf **reads** documents encrypted with the Standard
security handler (ISO 32000-2 §7.6). It does **not write** encrypted
documents. This page is the user guide; the design, the review and the
fuzzing record are in [`plans/encryption.md`](plans/encryption.md).

## What is supported

| | supported |
|---|---|
| security handler | `/Standard` only |
| revisions | 2, 3, 4, 5 (Adobe extension level 3), 6 |
| ciphers | RC4 40–128-bit, AES-128-CBC (`/AESV2`), AES-256-CBC (`/AESV3`) |
| crypt filters | `/CF` with `/StmF`, `/StrF`, `/EFF`; `/Identity`; the `/Crypt` stream filter with `/DecodeParms /Name` |
| also | `/EncryptMetadata false`; unencrypted cross-reference streams and signature `/Contents`; object streams (decrypted once, as a whole); incremental updates |
| passwords | empty, user, owner; Unicode text (SASLprep for R5/R6, PDFDocEncoding for R2–R4); R5/R6 truncated to 127 bytes |

## Opening a document

```rust
use kasane_pdf::{Document, Error, OpenOptions};

# fn main() -> Result<(), Error> {
# let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/encrypted/enc_r6_aes256.pdf"))?;
// Without a password: the empty user password is tried, as every
// reader does. Most "protected" files (permissions only) open here.
match Document::from_bytes(&data) {
    Ok(_doc) => {}
    Err(Error::Encrypted) => {
        // A password is needed. It is tried as user, then as owner
        // password, and never appears in errors, warnings or `Debug`.
        let doc = Document::open(&data, &OpenOptions::new().with_password("usr"))?;
        let info = doc.encryption().expect("encrypted");
        println!("R{} opened as {:?}", info.revision, info.authenticated_as);
    }
    Err(e) => return Err(e),
}
# Ok(())
# }
```

`OpenOptions` also carries `with_limits` and `with_recovery`, so
`Document::open` replaces the four `from_bytes*` constructors when a
password is involved. `doc.encryption()` is `None` for an unencrypted
document. The CLI takes `--password-file <path>` (`-` reads standard
input; the first line is the password). There is no `--password`
option: command-line arguments are visible to other local users.

## Errors

| error | meaning | what helps |
|---|---|---|
| `Error::Encrypted` | the password (none: the empty one) opens it neither as user nor as owner | another password |
| `Error::Structure` | the `/Encrypt` dictionary is malformed: a required entry missing, a wrong type, inconsistent values (`/V` vs `/R`, crypt-filter key lengths), an altered `/Perms` | nothing — no password can open it |
| `Error::Unsupported { feature: "encryption", .. }` | a handler, revision or crypt-filter method that is not implemented (public-key, `/CFM /None`, `/V 0`, …); a damaged encrypted file whose cross-reference table would have to be rebuilt; a revision-4 short key that only Acrobat's undocumented reading decrypts (strict mode) | recovery mode for the last case; nothing for the others |
| `Error::Unsupported { feature: "writing an encrypted document", .. }` | `rewrite`, `Edit::rewrite` or `Edit::save_incremental` on an encrypted document | — (writing is not implemented) |
| `Error::Parse` | ciphertext that cannot be decrypted (AES data not a whole number of blocks, invalid padding) | — |

On any error no document is returned: there is no partially opened
state, and recovery mode authenticates exactly like strict mode.

## What encryption does and does not mean

- **Permissions are not access control.** `/P` lists what the author
  asks a conforming reader to allow (print, copy, modify, …).
  `EncryptionInfo::permission(bit)` reports the bits; kasane-pdf does
  not enforce them, and anyone who can open a document can read all of
  it.
- **An empty user password gives no confidentiality.**
  `EncryptionInfo::empty_user_password` tells you so: the file opens
  for everyone, whatever the cipher.
- **Opening a file says nothing about how well it was protected.**
  Revisions 2–4 use RC4 and MD5, which are broken; R5's key check is a
  single SHA-256. Only R6 with a strong password is meaningful
  protection, and only as strong as that password. Rust's memory safety
  does not change the strength of the algorithms.
- Decrypted content is plaintext in memory for as long as the
  `Document` lives (as for any reader). Passwords and keys are held in
  buffers that are zeroed when dropped; this is best effort (copies made
  by the optimiser or by dependencies are not covered).

## Not supported

- **Writing** encrypted documents, including an incremental save that
  keeps the encryption: the writers refuse.
- Public-key (`/Adobe.PubSec`) and third-party security handlers.
- Rebuilding the cross-reference table of a damaged encrypted file
  (the scan would read ciphertext before a key exists).
- Strings of a *direct* `/Info` dictionary in the trailer (ISO 32000
  requires an indirect one; a direct one has no object number to derive
  its key from) are returned as stored.
