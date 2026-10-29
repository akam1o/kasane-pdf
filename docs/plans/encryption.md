# Design — encrypted documents (0.3.0)

Status: design, with the first milestone implemented (see the end).
References are to ISO 32000-2:2020 §7.6 (which incorporates the
ISO 32000-1 Standard security handler and adds revision 6).

## Scope

The **Standard security handler** (`/Filter /Standard`) is the only
handler the specification defines; public-key (`/Adobe.PubSec`) and
third-party handlers need certificates or proprietary code and are
refused with `Error::Unsupported`.

| target | PDF spec | difficulty | security risk | priority |
|---|---|---|---|---|
| R2, V1 — RC4 40-bit | ISO 32000-1 §7.6.3, Alg. 2/4/7; deprecated in 32000-2 | low | weak cipher: decryption only, never written | P0 (read) |
| R3, V2 — RC4 40–128-bit | §7.6.3, Alg. 2/5/7; deprecated | low | as above | P0 (read) |
| R4, V4 — crypt filters `/V2` (RC4) and `/AESV2` (AES-128-CBC) | §7.6.4, §7.6.5; AESV2 deprecated in 32000-2 | medium (crypt-filter selection per string/stream, `/Identity`, `EncryptMetadata`) | padding/IV handling of attacker-controlled data | P0 (read) |
| R5, V5 — AES-256, SHA-256 key check (Adobe extension level 3) | not in ISO 32000; superseded by R6 | low once R6 exists | weak key derivation (single SHA-256) | P1 (read, compatibility) |
| R6, V5 — AES-256 (`/AESV3`), Algorithm 2.B hardened hash | ISO 32000-2 §7.6.4.3 | medium (Alg. 2.A/2.B, `/UE` `/OE` `/Perms`) | password-derived key only as strong as the password | P0 (read) |
| `/Crypt` stream filter, `/Identity` crypt filter, `/StmF` `/StrF` `/EFF` | §7.4.10, §7.6.5 | low | — | P0 |
| SASLprep of R6 passwords | §7.6.4.3.3 (RFC 4013) | medium (Unicode tables) | wrong normalisation = authentication failure, not a leak | P2 — ASCII / already-normalised UTF-8 passwords work today |
| public-key security handler | §7.6.6 | high (CMS, certificates, private keys) | key handling | out of scope |
| **writing** encrypted output | §7.6 | medium | producing weak or wrong ciphertext | out of scope for 0.3.0 — the writers refuse encrypted documents |

Algorithms come from maintained RustCrypto crates, all pure Rust,
`MIT OR Apache-2.0`, WASM-clean, no `unsafe` required from us:
`aes` (block cipher), `cbc` (CBC mode), `md-5`, `sha2`, `rc4`. kasane-pdf
implements only the PDF key-derivation and authentication algorithms
(which are protocol logic, not primitives) on top of them.

## Model

- **Authentication happens at load**, before the catalog is read. The
  `/Encrypt` dictionary is the only object read before a key exists;
  it is never decrypted. Failure returns `Error::Encrypted`, no
  document. There is no partially opened state.
- **Decryption happens at object resolution**: every indirect object
  read from the file (and every object-stream container) has its
  strings and stream data decrypted with the object key before the
  rest of the engine sees it, so all existing `Limits` apply to the
  plaintext exactly as to an unencrypted file. Ciphertext is never
  interpreted as plaintext; members of an object stream are not
  decrypted twice (§7.5.7).
- **Exceptions (§7.6.1/§7.6.2):** cross-reference streams; the
  `/Encrypt` dictionary; the `/Contents` string of a signature
  dictionary; the XMP metadata stream when `/EncryptMetadata false`;
  streams whose `/Filter` starts with `/Crypt` use the named crypt
  filter (default `/Identity`), and the `/Crypt` entry is removed
  before the filter chain runs; embedded-file streams use `/EFF`.
- **Bounds:** decryption output is never larger than its input; AES
  data must be a positive multiple of 16 bytes including the IV, with
  valid PKCS#5 padding, else `Error::Parse`; key lengths are checked
  (`/Length` 40–128 bits in steps of 8 for RC4, exactly 128 / 256 for
  AES); `/O` `/U` `/OE` `/UE` `/Perms` lengths are checked per revision.
- **Recovery mode** authenticates the same way. An encrypted file
  whose cross-reference table is intact opens in recovery mode with its
  password; one whose table would have to be *rebuilt* is refused with
  `Error::Unsupported` — the scan reads object streams before any key
  exists and would treat ciphertext as plaintext.
- **Permissions (`/P`) are reported, not enforced as security.** They
  are a request by the author that conforming readers honour; with the
  user password the content is decryptable regardless. kasane-pdf
  reports them on `EncryptionInfo` and does not use them to grant or
  withhold anything. Its writers refuse encrypted documents entirely
  in 0.3.0 (rewriting would silently remove the encryption).
- **Secrets:** passwords and keys live in `zeroize`d buffers, are never
  formatted (`Debug` redacts them) and never appear in errors or
  warnings. Rust's memory safety does not make the scheme stronger
  than its algorithms: R2–R4 use RC4/MD5 and an empty user password
  gives no confidentiality at all — a successful open says nothing
  about the protection the file had.

## API (additive)

```rust
let doc = Document::from_bytes(&data)?;                // empty user password, as every reader tries
let doc = Document::open(&data, &OpenOptions::new().with_password(b"secret"))?;
let info: Option<EncryptionInfo> = doc.encryption();   // revision, method, permissions, as user/owner
```

- `OpenOptions` (`#[non_exhaustive]`, builders): `with_limits`,
  `with_password` (bytes; tried as user password, then owner
  password), `with_recovery`.
- `EncryptionInfo`, `CryptMethod`, `Authentication` — read-only report.
- Errors: `Error::Encrypted` now means "encrypted, and the supplied
  password (none = empty) does not authenticate"; a malformed
  `/Encrypt` dictionary is `Error::Structure`; an unsupported handler,
  revision or crypt filter method is `Error::Unsupported`.
- Behaviour change (not an API change): encrypted files that open with
  the empty user password — the common "permissions only" case — are
  now read by `from_bytes`. Callers that relied on `Error::Encrypted`
  for *every* encrypted file should check `doc.encryption()`.

## Milestones

1. **Done in this phase:** R2–R6 read with the empty user password and
   with a supplied user or owner password; crypt filters incl.
   `/Identity` and `/Crypt`; `EncryptMetadata`; signature `/Contents`;
   writers refuse encrypted documents; tests on synthetic files
   produced by qpdf (redistributable — derived from our own fixtures).
2. SASLprep for R6 passwords; the Acrobat zero-padding of R4 RC4 keys
   shorter than 128 bits (undocumented; pdf.js implements it) — needs a
   decision on whether an undocumented reading belongs in strict mode.
3. Writing encrypted output (incremental save that keeps the
   encryption; rewrite with re-encryption) — needs an RNG for IVs, so
   a `getrandom` dependency and a WASM story.
