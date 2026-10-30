# Migrating from kasane-pdf 0.2 to 0.3

0.3.0 adds reading of encrypted documents. **No public item was
removed or changed in signature**: `cargo semver-checks` against 0.2.2
reports no required update, and code that compiles against 0.2 compiles
against 0.3. The minor version moves because what callers *observe*
changes: inputs that 0.2 refused with `Error::Encrypted` now open, and
the meaning of that error narrows.

## Checklist

1. **If you used `Error::Encrypted` to mean "this file is encrypted"**,
   it now means "the password does not open it". An encrypted file
   that opens with the empty user password — the common
   permissions-only case — is now returned by `Document::from_bytes`.
   Check `doc.encryption().is_some()` to detect encryption:

   ```rust
   # use kasane_pdf::Document;
   # fn f(data: &[u8]) -> Result<(), kasane_pdf::Error> {
   let doc = Document::from_bytes(data)?;
   if let Some(info) = doc.encryption() {
       // R2–R6; info.empty_user_password says whether anyone can open it
   }
   # Ok(())
   # }
   ```

   To keep refusing every encrypted file, reject `Some(_)` there.
2. **To open a password-protected file**, use `Document::open` with
   `OpenOptions::new().with_password(..)` (plus `with_limits` and
   `with_recovery` if you used the `*_with_limits` or `*_recovering`
   constructors). See [`encryption.md`](encryption.md).
3. **Error classification of encrypted input changed** (all were
   `Error::Encrypted` in 0.2):

   | input | 0.2 | 0.3 |
   |---|---|---|
   | opens with the empty user password | `Encrypted` | **`Ok`** |
   | needs a password, none or a wrong one given | `Encrypted` | `Encrypted` |
   | malformed `/Encrypt` dictionary | `Encrypted` | `Structure` |
   | public-key / third-party handler, unknown revision or method | `Encrypted` | `Unsupported { feature: "encryption" }` |
   | damaged encrypted file (recovery mode would rebuild the table) | `Encrypted` | `Unsupported { feature: "encryption" }` |

   `ErrorKind::as_str()` strings are unchanged.
4. **Writers refuse encrypted documents.** `rewrite`, `Edit::rewrite`
   and `Edit::save_incremental` return
   `Unsupported { feature: "writing an encrypted document" }` for a
   document that `doc.encryption()` reports as encrypted (in 0.2 such a
   document could not be opened at all). A pipeline that opens and
   rewrites arbitrary input must handle this error.
5. **Match the classification enums with a wildcard arm.** New
   variants: `Warning::OverlongEncryptionEntry`,
   `RecoveryRule::PaddedShortEncryptionKey` (both enums are
   `#[non_exhaustive]`).
6. **Text extraction:** codes of a non-embedded Standard 14 font
   without `/Encoding` now resolve through the font's built-in
   encoding (`ResolveKind::EncodingFallback`) instead of
   `ResolveKind::Unresolved`.
7. **CLI:** every subcommand accepts `--password-file <path>`; the
   document summary (`kasane-pdf <file.pdf>`) prints an `encryption:`
   line for encrypted files.

## New items

`Document::open`, `Document::encryption`, `OpenOptions`
(`#[non_exhaustive]`; `new`, `with_limits`, `with_password`,
`with_recovery`; `pub limits`, `pub recovery`), `EncryptionInfo`
(`#[non_exhaustive]`; `permission(bit)`), `CryptMethod`,
`Authentication`, the two variants above.

## Dependencies

New: RustCrypto `aes`, `cbc`, `rc4`, `md-5`, `sha2`, `zeroize`, and
`stringprep` (with `unicode-normalization`, `unicode-bidi`,
`unicode-properties`, `tinyvec`). All pure Rust, MIT/Apache-2.0
(`unicode-normalization` additionally Unicode-3.0 for its data tables),
WASM-clean; see [`third-party.md`](third-party.md). MSRV unchanged
(1.92).
