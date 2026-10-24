# Plan — Render 90 % on the compatibility corpus

Written after the 0.2.0 release (run-1792850880 / run-1792845977:
511 files, every page, default features).

| metric (expect: pass, 427 files) | now | 90 % line | needed |
|---|---:|---:|---:|
| render, strict counting | 368 (86.2 %) | 385 | **+17** |

"Strict counting" means a page counts only when the document parsed
strictly; 13 of the 59 non-passing files fail at parse. The rule for
every item below is the one 0.1.1 introduced: a deviation is accepted
only when its exact meaning is defined — by ISO 32000-1 itself or by a
recovery rule every major reader applies identically — and it is then
recorded as a `Warning`, counted apart from a clean pass. Nothing
here widens a tolerance or silences a refusal to move a number.

## The 59 blockers, re-aggregated

| cause | files | class |
|---|---:|---|
| strict parse: `/MediaBox` missing on page and ancestors | 4 | parse error |
| strict parse: encrypted (`/Encrypt`) | 3 | parse unsupported |
| strict parse: no `startxref`, bad xref offset, unbalanced token, page-tree type/cycle, fuzzed ASCII85, … | 6 | parse error (1 each) |
| reference to an undefined object (§7.3.10) | 4 | render error |
| non-embedded CIDFont with `/Ordering (Identity)` | 3 | render unsupported |
| operators inside a path object (`w`, `rg`, `cs`), unmatched `q`, stream ending inside `BT`, unknown operator, inline image without `EI` | 8 | render error |
| `/AP /N` state dictionary without `/AS` | 2 | render unsupported |
| knockout transparency group (`/K true`) | 2 | render unsupported |
| `max_image_dimension` (16 384) exceeded | 2 | render limit |
| non-embedded Symbol / ZapfDingbats | 2 | render unsupported |
| other single files: shading output count, JBIG2 on a content stream, `/BrotliDecode`, invalid `/BBox` allocation, Type 3 / pattern cycle, `/Differences` type, non-Latin field value, corrupt Flate, `/DecodeParms` type, undefined `/Properties`, JBIG2 size mismatch, `/RI` type, JPEG component mismatch, non-embedded TrueType without substitute, bad `/Annots` entry | 17 | mixed |

Chinese / Korean predefined CMaps block **0** `expect: pass` files
(the two affected files are `expect: any`).

## Candidates

| # | candidate | effort | expected new PASS | spec position | security risk | WASM | font licence | rank |
|---|---|---:|---:|---|---|---|---|---:|
| 5 | **Undefined object → `null` with `Warning::DanglingReference`** (strict and recovery) | 1–2 d | **+4** (`doc_actions`, `freetexts`, `pdfium-annotiter`, `pdfium-weblinks`) | §7.3.10 *requires* a conforming reader to treat it as null — strict mode is currently stricter than the standard | low: the object is absent either way; the warning records what was skipped; limits unchanged | yes | — | **1** |
| 6a | **Defined recovery rules for malformed content streams**, each a `Warning` variant: unmatched `q` at end of stream (implicit `Q`), stream ending inside `BT` (implicit `ET`), `w`/`rg`/`cs`/`gs` inside a path object (applied before the painting operator, as Acrobat/pdf.js/Poppler do), `/MediaBox` missing (Letter, as every viewer), `/AP /N` with one state and no `/AS` (that state) | 3–5 d | **+9 to +12** (`issue14256`, `issue14165`, `bug2065923`, `filled-background`, `issue7847_radial`, 4 × MediaBox, `issue15557`, `pdfium-bug_572871`; not `bug1157493`, `pdfium-tagged_table_bad_parent`, `bug1513120_reduced` whose meaning is ambiguous) | not conforming input; each rule has one reading shared by every major reader; refused when the reading is ambiguous | low–medium: each rule needs a fixture proving the result is identical to Poppler; the renderer must not resynchronise on arbitrary garbage | yes | — | **2** |
| 1 | **Knockout groups** (`/K true`): composite each object against the group's initial backdrop instead of the accumulated group | 3–5 d | **+2** (`issue1905`, `mlp-mixer`) | §11.4.8, exact implementation | low; bounded by the existing group buffers and `max_render_buffer_bytes` | yes | — | **3** |
| 2 | **Symbol / ZapfDingbats fallback**: Adobe Core 14 AFM metrics (redistributable under Adobe's AFM licence) + glyph shapes from an OFL face (Noto Sans Symbols / Symbols 2) through the Symbol and ZapfDingbats built-in encodings; reported as `SubstKind::Approximate` | 2–3 d + asset review | **+2** (`standard_fonts`, `checkbox-bad-appearance`) | approximation, reported on `RenderedPage::font_substitutions()` like the Liberation substitutes | low | yes (bundled bytes) | OFL 1.1 + Adobe AFM licence; URW base35 (AGPL) is *not* an option | **4** |
| 3 | **Identity-ordering non-embedded CID fonts** via `/ToUnicode` → Unicode → substitute glyph | 2–3 d | +0 to +3 (`issue11578/11740/12418_reduced`, only where a complete `/ToUnicode` exists) | CIDs are glyph indices of an unknown font: unsound without `/ToUnicode`; with it, an approximation — must be reported and refused when any code is unmapped | low | yes | reuses bundled faces | 5 |
| 7 | **Broader real-document corpus** (office exports, scanners, forms, non-Latin scripts) with licence-cleared sources | continuous | widens the denominator; expected to *lower* the ratio first | — | — | — | — | 6 (start in parallel) |
| 4 | **GB1 / CNS1 / Korea1 fallback** as a separate asset crate (`kasane-pdf-cjk-assets`; 7.2 MiB of 10 MiB crates.io budget already used) | 3–5 d + crate | **+0** on this corpus; needed for Chinese/Korean documents in general | approximation, as `cjk-fallback` | low | yes | Adobe CMaps BSD-3, Noto Sans SC/TC/KR OFL (≈ 20 MiB) | 7 |
| — | Encryption (RC4 / AES-128 / AES-256, empty user password; owner password refused) | 5–8 d | +3 (`bug900822`, `issue19484_1`, `pdfium-encrypted_hello_world_r3`) | §7.6, exact; today `Error::Encrypted` | medium: crypto code paths, new dependency (`aes`, `sha2`, `md-5` — RustCrypto, MIT/Apache, pure Rust, WASM-clean) | yes | — | v0.3.0 |
| — | `max_image_dimension`: DCT downscaled decode (1/2, 1/4, 1/8) when the target raster is smaller than the image | 2 d | +2 | exact at the output resolution; the limit stays | low | yes | — | v0.3.0 |

## Shortest path to 385

Ranks 1 + 2 + 3 + 4 give **+17 to +20** on the current corpus
(385–388 / 427) in roughly **9–15 working days**, with no change to
strict refusals that are not defined by the specification or by a
unanimous reader rule, and every acceptance visible as a warning or a
reported substitution. Rank 5 adds up to 3 more only where sound.
Items marked v0.3.0 are worth doing for users but are not the short
path.

## Recommended order

1. **0.2.1** — §7.3.10 null references (rank 1), the MediaBox default
   and the single-state `/AS` rule (both warnings). +10 expected,
   lowest risk, additive API (new `Warning` variants; the enum is
   `#[non_exhaustive]`).
2. **0.2.2** — the remaining content-stream recovery rules (rank 2),
   one fixture and one Poppler comparison per rule. +4 to +6.
3. **0.2.3** — knockout groups (rank 3). +2.
4. **0.2.4** — Symbol / ZapfDingbats (rank 4) once the asset licences
   are recorded in `docs/third-party.md` and `NOTICE`. +2.
5. **0.3.0** — encryption, DCT downscaling, Identity-CID via
   ToUnicode, the CJK asset crate, and the corpus expansion that will
   reset the target.

## Compatibility of the published API

Every item is additive under the 0.2 contract: new `Warning` and
`SubstKind` variants (both `#[non_exhaustive]`), new `Limits` fields
through `with_*`, new feature flags for assets. None changes a
signature or a default. Encryption will add a way to pass a password
(a `with_password` on a document-options builder, additive) and will
turn some `Error::Encrypted` refusals into successes — a behaviour
change on previously refused input, recorded in the changelog as the
Flate and ToUnicode changes were in 0.1.1.
