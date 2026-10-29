# Plan — Render 90 % on the compatibility corpus

Status after the recovery / knockout / symbol-fallback phase (the run
recorded in `compat-corpus.md` § *Results after §7.3.10 null
references, recovery rules, knockout groups and the symbol fallback*).
Per-file details live in the gitignored runner reports and in
`scripts/diag-render-blockers.py`'s output; this file carries only
category counts and the generalised plan.

| metric (expect: pass, 427 files) | 0.2.0 | now | 90 % line | still needed |
|---|---:|---:|---:|---:|
| strict render | 368 (86.2 %) | **378 (88.5 %)** | 385 | **+7** |
| of which approximate (reported substitution, colour or image approximation) | 65 | 69 | — | — |
| recovered render (recovery mode only, never counted as strict) | 8 | 18 | — | — |
| text, strict | 384 | 390 | — | — |
| full PASS | 318 | 325 | — | — |

"Strict" counts a file only when it parsed strictly and rendered every
page with constructs the specification allows; a warning the standard
itself resolves (§7.3.10 null, an open block closed at the end of a
stream, an unrecognised rendering intent) does not disqualify it, a
recovery rule does. Nothing in this phase widened a tolerance to move
the number: every acceptance is recorded as a `Warning`, counted apart
from a clean pass, and listed in `security.md` § Three outcomes.

## What this phase did

| item | result on `expect: pass` | class |
|---|---:|---|
| references to undefined objects are null (§7.3.10), recorded once | +5 strict | spec-required |
| open `q` / `BT` / `BMC` / `BX` blocks closed at the end of a stream, recorded | +2 strict | exact |
| unrecognised rendering intent → RelativeColorimetric (§8.6.5.8), recorded | +1 strict | spec fallback |
| knockout transparency groups (§11.4.8) | +1 strict (the second candidate file is blocked by nesting depth) | exact |
| Symbol / ZapfDingbats drawn with the bundled symbol fallback face | +1 strict, approximate | reported approximation |
| indirect image `/DecodeParms`, `/TR /Identity`, ASCII85 one-character final group | fixes on the way to the above | exact |
| recovery rules: page-box defaults, stray `Q`, unknown operator, operator inside a path object, invalid annotation entries / states / appearances without `/BBox`, undefined `/Properties`, missing font resource → Helvetica, malformed `/Differences` element, image `/SMask` that is not a stream | +10 recovered | recovery only, each recorded |

## The remaining 51 strict blockers, by category

| category | files | strict possible? | recovery | note |
|---|---:|---|---|---|
| damaged file structure (no `startxref`, bad offsets, broken syntax) | 5 | no | xref rebuild; then the content decides | 2 of them then hit an ambiguous operand count or a form without `/BBox` |
| page without `/MediaBox` | 4 | no — Table 30 gives no default | **yes** (US Letter) | counted as recovered |
| operator inside a path object | 3 | no — §8.2 Figure 9 | **yes** | counted as recovered |
| encryption | 3 | feature work | no | v0.3.0 (RC4 / AES, empty user password) |
| non-embedded CID font with `/Ordering (Identity)` | 3 | no — glyph indices of an unknown font | only with a complete `/ToUnicode`, as an approximation | 2 of the 3 carry one |
| `/AP /N` with several states and no `/AS` | 2 | no — no basis to choose | no | — |
| image larger than `max_image_dimension` | 2 | yes, with DCT scaled decode under the same limit | n/a | 2 d |
| invalid page tree (`/Type /Template` kid, cycle) | 2 | no | no | — |
| stray `Q`, unknown operator, non-dictionary `/Annots` entry, appearance state that is not a stream, undefined `/Properties`, malformed `/Differences`, missing font resource, image `/SMask` that is not a stream, degenerate page boxes | 9 | no | **yes** | counted as recovered |
| shading function with fewer outputs than the colour space has components | 1 | no | possible (replicate), approximate | 1 d |
| form nesting deeper than `max_xobject_depth` (32) | 1 | limits policy | n/a | the knockout file; not raised for a number |
| JBIG2 on a content stream; JBIG2 page size ≠ `/Width`×`/Height`; corrupt Flate; undecodable DCT; inline image without `EI`; pattern cell cycle; `/BrotliDecode`; non-Latin field value with a single-byte field font; symbolic non-embedded TrueType with an unknown name | 9 | no | no | each needs either a feature (Brotli, Unicode field font) or is unrecoverable |

Recovery mode now renders 18 of these 51; 9 more remain for a
hypothetical "trust the codestream" class of rules that were
deliberately not added (they change the drawing, not only the
structure).

## Shortest path to 385 from here

| # | item | effort | expected strict gain | what it needs |
|---|---|---:|---:|---|
| 1 | ~~DCT scaled decode~~ — **done**: oversized JPEGs drawn small decode with a reduced inverse DCT, oversized plain soft masks are sampled at their grid | — | +2 (realised) | `RenderedPage::image_approximations()` |
| 2 | ~~Identity-ordering CID fonts via `/ToUnicode`~~ — **dropped after inspection**: of the three files, one has no `/ToUnicode` and uses the real font's glyph indices, one maps its glyphs to unrelated Latin-1 code points, one uses codes that merely coincide with Unicode; none can be drawn soundly | — | 0 | — |
| 3 | encryption: RC4 and AES-128/256 with the empty user password; owner-password-only files refused | 5–8 d | +3 | RustCrypto `aes`, `sha2`, `md-5` (pure Rust, WASM-clean); `Error::Encrypted` stays for anything else |
| 4 | ~~transfer functions~~ — **no corpus need**: the one file's `/TR` is `/Identity` (handled); it is blocked by form nesting of 33–48 levels against `max_xobject_depth` 32, which stays (a caller may raise it through `Limits`) | — | 0 | — |
| 5 | shading function output replication (1 → n), reported as an approximation | 1 d | +1 (approximate) | `color_approximations` entry |

Items 1, 2 and 4 are resolved (+2 realised, the rest unsound or
limit-bound). From 378, the realistic remaining short path is
encryption (+3) and shading output replication (+1, approximate),
reaching **382 of 427 (89.5 %)**; the last three files to 385 need
either the corpus to grow or a feature exercised once each (Brotli, a
Unicode field font, a decision on the nesting limit). Originally: items 1, 2 and 5 are additive under the 0.2
contract, item 3 adds a document-options builder (additive) and turns
some `Error::Encrypted` refusals into successes. The remaining 42
blockers after that are files the standard does not define a result
for, or features (Brotli, a Unicode field font) that the corpus
exercises once each; past 385 the corpus itself has to grow.

## Recommended order

1. **0.2.1** — this phase (release the recovery rules, knockout groups,
   the symbol fallback, the runner's three render outcomes).
2. **0.2.2** — DCT scaled decode (item 1), transfer functions (item 4).
3. **0.2.3** — Identity-CID via `/ToUnicode` (item 2), shading output
   replication (item 5).
4. **0.3.0** — encryption (item 3), the CJK asset crate for
   GB1 / CNS1 / Korea1 (no gain on this corpus; needed for Chinese and
   Korean documents in general), and corpus expansion with
   licence-cleared real-world documents, which will reset the target.

## Compatibility of the published API

Everything in this phase and in the shortest path is additive under the
0.2 contract: new `Warning` and `RecoveryRule` variants (both
`#[non_exhaustive]`), new `Limits` fields through `with_*`, new
`SubstKind` variants, new accessors (`Document::recovery_mode`). No
signature or default changed; `Document::from_bytes_recovering` applies
its rules to conforming input identically to a strict parse. Encryption
will add a password option on a document-options builder.
