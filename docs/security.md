# Security & safety posture

kasane-pdf treats every input byte as hostile. Two independent
mechanisms apply: a **`Limits` bound** on every unbounded operation,
and a **typed-refusal policy** — a construct the engine cannot process
*exactly* produces `Error::Unsupported`/`Error::Structure`, never a
partial or best-effort result presented as success.

## Resource bounds

Every unbounded operation is capped by [`Limits`](../src/limits.rs)
(see `architecture.md` § Safety / hostile input for the full table):
input size, indirect-resolution depth, page-tree depth/nodes,
collection sizes, string/nesting depth, decoded-stream bytes (single
+ cumulative), filter-chain length, object-stream size, parsed-op and
operand-stack counts, render pixel/buffer size, path-element count,
xref section/entry counts, the diagnostic `max_diag_*` budgets, and
the edit `max_edit_*` budgets.

Additionally: cycle detection on reference chains, `/Prev` chains,
and page-tree node revisits; object-stream bodies decoded once per
Document; `unsafe` is confined to four documented lifetime transmutes
in `src/font.rs` (see `architecture.md` § Safety / hostile input).

## Typed errors, not partial results

`Error::kind()` gives a stable machine-readable category, the
`#[non_exhaustive]` enum `ErrorKind` (`Parse`, `Structure`,
`Unsupported`, `Encrypted`, `LimitExceeded`, `InvalidArgument`,
`SignaturePolicy`, …; `as_str()` is the string the CLI prints). A
malformed file, a valid file using an unimplemented feature, a
resource bound, a caller's bad argument and a signature's policy are
*distinguishable* — automation should branch on `kind()`, not on
message text. `ErrorKind::is_malformed_input()` groups the
"the file is broken" kinds.

## Three outcomes: strict pass, accepted with warnings, recovery

| outcome | how it is produced | what it means | how to see it |
|---|---|---|---|
| **strict pass** | `Document::from_bytes` + an operation returns `Ok` and `take_warnings()` is empty | the file conformed to ISO 32000-1 in everything the engine touched | nothing to read |
| **accepted with warnings** | `Ok`, and `take_warnings()` is non-empty | the input deviated from the specification, but every deviation was one the engine can process *exactly* or that the standard itself resolves: a Flate stream ending after a sync flush with the checksum missing; a `/ToUnicode` entry that could not be read (its codes stay unmapped); a reference to an undefined object (null, §7.3.10); a `q`/`BT`/`BMC`/`BX` block still open at the end of a content stream (closed there); a rendering intent outside §8.6.5.8 (RelativeColorimetric); an ASCII85 final group of one character. The result is complete and correct for the bytes present; the file is not conforming | `Document::take_warnings()` / `warnings()`; the CLI prints `warning: …` on stderr; the compatibility runner counts these files separately from a clean pass |
| **recovery** | `Document::from_bytes_recovering`; `recovery_mode()` is `true`, and `recovery()` is `Some` when the cross-reference table had to be rebuilt | the document reflects the engine's *guesses*: a rebuilt cross-reference table (`RecoveryReport`), and the recovery rules — US Letter for a page without `/MediaBox`, a stray `Q` ignored, an unknown operator dropped, a state or colour operator inside a path object applied, an invalid `/Annots` entry skipped — each recorded as `Warning::RecoveryApplied { rule, .. }`. None of them has a basis in ISO 32000-1; every major reader applies the same reading; strict mode refuses them all | `Document::recovery_mode()`, `recovery()`, the `RecoveryApplied` warnings; the incremental writer refuses a document with a rebuilt table; the compatibility runner counts a recovered render apart from a strict one |
| **approximate** | `Ok`, and `RenderedPage::font_substitutions()` or `color_approximations()` is non-empty | the input was valid and processed, but a result the specification defines could not be produced exactly: a non-embedded font drawn with a bundled substitute (Standard 14 → Liberation; Symbol / ZapfDingbats → the symbol fallback face; Adobe-Japan1 CID fonts → Noto Sans JP), an ICC profile evaluated through its alternate space | the `RenderedPage` accessors; independent of recovery — a recovered render may also be approximate, and a warning alone never makes a render "recovered" |

The set of accepted-with-warnings deviations is deliberately small and
enumerated by the `Warning` variants; it is not a lenient mode and it
never grows to include a construct whose exact meaning is in doubt.
Approximations the renderer makes on *conforming* input (a substituted
font, an ICC profile evaluated through its alternate) are not
warnings: they are reported on `RenderedPage` so that a render is
never mistaken for a faithful one.

## Signed documents: structure, not validity

`signatures()` and the `Edit` signature policy read `/ByteRange`,
`/DocMDP` and `/FieldMDP` to decide which incremental edits a signer
permitted. Preserving the signed byte range is a *structural*
guarantee only — kasane-pdf never computes the digest, never checks
the certificate chain and never reports a signature as valid or
invalid. Use a signature validator for that question.

## Encrypted documents

Authentication happens before the catalog is read; a password that
opens neither as user nor as owner password returns `Error::Encrypted`
and no document. Every object is decrypted as it is read, so all
`Limits` apply to the plaintext. Passwords and keys are held in zeroed
buffers and never appear in `Debug`, errors or warnings. `/P`
permissions are reported on `EncryptionInfo` and are advisory: anyone
who can open the file can read all of it. Revisions 2–4 rely on RC4
and MD5; an open document says nothing about how well it was
protected. Recovery mode never rebuilds the table of an encrypted file
(it would read ciphertext as plaintext) — judged from every object the
scan finds, not only a surviving trailer. The one encryption recovery
rule (Acrobat's padding of short revision-4 keys) applies only when the
file's own data shows that the specified key does not decrypt it. See
[`plans/encryption.md`](plans/encryption.md).

## What the engine is **not**

- **Not a redaction tool.** The `experimental-remove-*` commands and
  `remove_*` APIs are *content editors*: they rewrite selected
  operations/pixels into a new object but do **not** prove the
  removed data is gone. The original stream may survive via sharing;
  `pixels_changed`/`codes_removed` report what was edited, not a
  removal guarantee. See [`features/editing-*.md`](features/) and
  [`redaction-design.md`](redaction-design.md).
- **Not a sanitizer.** `rewrite`/`rewrite_with_limits` carry every
  *reachable* object verbatim — including keys and payloads the
  engine does not interpret. "Preserved" ≠ "validated" ≠ "safe".
  See [`features/rewriting.md`](features/rewriting.md).
- **Not a "safe" verdict.** `diagnose_redaction` /
  `preflight-redaction` deliberately return no boolean verdict. A
  clean report means only "the walk finished within the examined
  scope" — see [`features/diagnosis.md`](features/diagnosis.md).
- **Not guaranteed-durable output.** The CLI write path is atomic
  (temp-file + `sync_data` + rename) but does not fsync the parent
  directory — power-loss durability is out of scope. See
  [`features/rewriting.md`](features/rewriting.md) § Save guarantees.
