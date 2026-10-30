# Design — encrypted documents (0.3.0)

Status: released in 0.3.0 for reading (milestones 1–3 below), reviewed
independently (§ Review), audited again for the release (§ Final
audit), fuzzed (§ Fuzzing). Writing encrypted output is not
implemented. User guide: [`../encryption.md`](../encryption.md).
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
| SASLprep of R5/R6 passwords; PDFDocEncoding of R2–R4 passwords | §7.6.4.3.3 Alg. 2.A (RFC 4013); §7.6.4.3.2 | medium (Unicode tables — `stringprep` crate) | wrong normalisation = authentication failure, not a leak | done (§ Passwords) |
| R4 RC4/AES keys shorter than 128 bits | §7.6.3.2 Alg. 1 (key used as is) vs Acrobat (zero-padded) | low | an undocumented reading must not change conforming files | done: evidence-based, recovery only (§ Short keys) |
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
  exists and would treat ciphertext as plaintext. "Encrypted" is
  decided from everything the scan finds, not only a surviving trailer:
  an `/Encrypt` entry in any dictionary, or an encryption dictionary
  itself (a non-stream dictionary with a `/Filter` name and `/O` + `/U`,
  `/Recipients` or `/CF`), directly in the file or inside an object
  stream.
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

## Passwords

`OpenOptions::with_password` takes bytes, normally UTF-8 text. The
specification defines the password differently per revision, so the
supplied password is tried as a short list of byte strings, every one
derived from it (none weakens the check), user before owner:

| revision | specified encoding | tried, in order |
|---|---|---|
| 2–4 | PDFDocEncoding (§7.6.4.3.2) | the bytes as given; a non-ASCII UTF-8 password converted to PDFDocEncoding (Annex D; skipped if a character has no code there) |
| 5–6 | UTF-8 after SASLprep (RFC 4013, Algorithm 2.A step a), at most 127 bytes | the SASLprep form when it differs; the bytes as given |

SASLprep comes from the `stringprep` crate (stored-string profile: a
password with a code point unassigned in Unicode 3.2, such as an emoji,
or with prohibited characters is refused by SASLprep and then tried as
given — the form writers that skip SASLprep produce). The crate's error
value names the offending character; it is discarded unread. Both
paths truncate to 127 bytes before hashing, as Algorithm 2.A requires.
(qpdf 12.4.2 truncates when checking but not when writing, so it cannot
open its own files with longer passwords; the fixture uses 127 bytes.)

## Short keys (revision 4, key under 128 bits)

Algorithm 1 derives each object key from the file key as it is: with a
40-bit key, `MD5(key ‖ obj ‖ gen)[..10]`. Acrobat, and pdf.js since
2025 (issue 19484, PR 19555: "undocumented"), first zero-pad a
revision-4 key to 16 bytes. qpdf forces 128 bits for `/V 4` (such files
fail authentication there); Poppler uses the key as is. The readings
are mutually exclusive, and RC4 has no integrity check, so choosing
blindly would decrypt one kind of file to garbage.

kasane-pdf decides from evidence, only when the configuration is
ambiguous (revision 4, key < 16 bytes): the first uncompressed
`/FlateDecode` stream is decrypted under both readings and must begin a
valid zlib stream.

| evidence | strict | recovery |
|---|---|---|
| the specified key inflates (or no stream gives evidence) | specified key | specified key |
| only the zero-padded key inflates | `Error::Unsupported` — the data does not decrypt as ISO 32000 specifies; refusing is better than returning garbage | padded key, `Warning::RecoveryApplied { rule: RecoveryRule::PaddedShortEncryptionKey }` |

A conforming file therefore reads identically in both modes.

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

## Review (independent)

The implementation was reviewed by a second model (a different vendor's
code model), read-only, against §7.6, with reproductions. Findings and
outcome:

| id | finding | severity | outcome |
|---|---|---|---|
| C1/C2 | a damaged encrypted file whose trailer was lost (or no longer names `/Encrypt`) was rebuilt as an *unencrypted* document: ciphertext object streams expanded as plaintext, and the writers could emit the result without `/Encrypt`. No key was ever derived, so no plaintext leaked; but the refusal promised above did not hold | high | fixed — encryption is detected from every scanned object (§ Model); regression tests for a cut trailer, a renamed `/Encrypt`, an encryption dictionary inside an object stream |
| M2 | `/Encrypt` reached through a reference chain exempted only the first object of the chain | low | fixed — the object the dictionary is read from is exempt; regression test |
| M7 | `/O` `/U` `/OE` `/UE` `/Perms` longer than specified accepted silently | info | now recorded as `Warning::OverlongEncryptionEntry` (writers do pad them; every reader uses the leading bytes) |
| M8 | missing `/V` reported as "/V 0" | trivia | message clarified (`/V` 0 is the default and is the undocumented algorithm, so `Unsupported` is right) |
| M1 | strings of a *direct* `/Info` (or `/Root`) dictionary in the trailer are not decrypted | low | kept: Table 15 requires both to be indirect references, and Algorithm 1 needs the containing object's number — a direct one has none. Such a file is non-conforming; its strings read as stored |
| M3 | `/V 4` ignores the dictionary-level `/Length` | low | kept: Table 20 limits `/Length` to `/V` 2 and 3; `/V 4` key length comes from the crypt filters: 128 bits for `/AESV2`, the RC4 filters' `/Length` otherwise (all of them must agree, and a short one beside `/AESV2` is `Error::Structure` — § Final audit) |
| M4 | R5 `/Perms` checked whenever present | low | kept: stricter, and a mismatch means the dictionary was altered |
| M5 | an indirect `/Filter` naming `/Crypt` is not recognised before decryption | low | kept, fails closed: the filter chain then refuses `/Crypt` with `Error::Unsupported`; no plaintext is invented |

Everything else the review checked matched the specification and qpdf:
Algorithms 2, 3–7, 2.A, 2.B (re-implemented by the reviewer), 1, 13,
the crypt-filter defaults, the exemptions, the user-before-owner order,
secret handling and the Algorithm 2.B bounds.

## Final audit (0.3.0)

A second pass over the implementation before the release, against the
same sections, looking for paths where a malformed dictionary is given
a default instead of an error.

| priority | finding | outcome |
|---|---|---|
| P1 | `/StmF`, `/StrF`, `/EFF` and a `/Crypt` filter's `/DecodeParms /Name` that were present but not names were treated as absent — `/Identity` — so those streams or strings were read without decryption | fixed: `Error::Structure`; tests (one malformed dictionary opened on the previous code) |
| P1 | the file key length came from whichever RC4 crypt filter was iterated last | fixed: RC4 filters must agree; a short one beside `/AESV2` is `Error::Structure` |
| P1 | no CLI way to give a password | fixed: `--password-file` (file or standard input; never an argument) |
| P2 | a `/Length` present but not an integer fell back to 40 bits or was ignored | fixed: `Error::Structure` |
| P2 | an R6 dictionary without `/Perms` reported `Encrypted` with a wrong password | fixed: shape checked before authentication |
| P2 | the short-key probe could parse one large stream 256 times through duplicate table entries | fixed: distinct offsets only |
| P2 | `stringprep` returns a plain `String` (the normalised password), dropped without zeroing; some hash intermediates likewise | kept, documented: zeroing is best effort |
| P2 | a stream marked `/Type /XRef` (or `/Type /Metadata` under `/EncryptMetadata false`) is not decrypted whatever it is used for | kept: that is §7.6.2's rule; it only lets a file's author store data in the clear, never exposes another file's plaintext |
| P2 | a generation number that differs between the cross-reference entry and the object header is tolerated; the key uses the header's (Algorithm 1: the object's own numbers) | kept |

Checked and found correct, without change: Algorithms 1, 2, 2.A, 2.B,
3–7, 13 (known answers against qpdf for all twelve file keys, and
Poppler's text on every fixture); AES IV and PKCS#5 checks; object
streams decrypted once; the exemptions; user before owner; recovery
refusing every encrypted rebuild; Limits applied to the plaintext;
Algorithm 2.B bounded (at most 287 rounds, < 15 KB per round).

## Fuzzing

`fuzz/` holds three cargo-fuzz (libFuzzer, AddressSanitizer) targets;
see `fuzz/README.md`. Each iteration opens strict and in recovery mode,
reads the content, extracts text and renders at 9 dpi under tight
limits; the encrypted templates are the qpdf fixtures.

| campaign | code | open_any | encrypt_dict | ciphertext | result |
|---|---|---:|---:|---:|---|
| 1 — 20 min per target | before the review fixes | 1.17 M | 3.05 M | 1.44 M | no crash, hang or OOM; but the oracle did not yet cover the C1 shape (a rebuilt document reporting no encryption) |
| 2 — 40 min per target | after the fixes, stronger oracle | ≈ 0.55 M, then a crash | stopped at once | 1.99 M | **panic** in the stream parser: an empty stream without a usable `/Length` (`stream⏎endstream`) — present since 0.1.0, also in the published 0.2.1; fixed, regression test. The `encrypt_dict` stop was the oracle's fault: all fixtures share one `/ID` and `/P`, so another fixture's empty-password dictionary legitimately authenticated (its key decrypts this file to garbage) |
| 3 — 40 min per target | parser fixed, confidentiality oracle | 0.52 M | 0.95 M | 0.72 M | no crash, hang or OOM; peak RSS 1.2 GB (libFuzzer + ASan, corpus in memory; `-rss_limit_mb=2048`) |
| 4 — 40 min wall per target (0.3.0 release candidate) | final-audit fixes; eleven templates (adds Unicode / PDFDocEncoding passwords, the encrypted incremental update, the spec and Acrobat short-key pair) | 0.65 M | 1.18 M | 0.70 M | no crash, timeout or OOM; coverage 8717 / 5210 / 5819 (campaign 3: 8556 / 4762 / 3100); peak RSS 1.0 GB. libFuzzer flagged one "slow unit" per target at 901 s — the machine was asleep (system log); the three inputs replay in 125 ms, 1 ms and 50 ms |

The confidentiality oracle: a fixture whose user password is not empty
never reveals its plaintext (the content text, the `/Info` title)
without the password — whatever replaces its `/Encrypt` dictionary,
strict or recovery, with or without its trailer. About 12.5 million
executions in total over the four campaigns.

Not reached by a dedicated target: an object stream whose *contents*
are fuzzed under encryption (a target would have to re-encrypt the
mutated stream and rebuild a cross-reference stream). The encrypted
object-stream fixtures are in `open_any`'s seed corpus, so their bytes
are mutated there, and `tests/encryption.rs` covers a damaged one.

## Milestones

1. **Done in this phase:** R2–R6 read with the empty user password and
   with a supplied user or owner password; crypt filters incl.
   `/Identity` and `/Crypt`; `EncryptMetadata`; signature `/Contents`;
   writers refuse encrypted documents; tests on synthetic files
   produced by qpdf (redistributable — derived from our own fixtures).
2. **Done:** SASLprep and PDFDocEncoding of passwords; the short-key
   decision (evidence-based, recovery only); the review fixes; known-
   answer tests of every file key against qpdf; fuzzing.
3. **Done (0.3.0):** the final audit, `--password-file`, the user
   guide and migration notes, the fourth fuzz campaign.
4. Not scheduled: writing encrypted output (incremental save that keeps
   the encryption; rewrite with re-encryption) — needs an RNG for IVs,
   so a `getrandom` dependency and a WASM story; public-key and
   third-party handlers.
