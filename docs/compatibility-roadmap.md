# Compatibility roadmap — v0.11.0 and after

Proposed from the measurements in
[`compatibility-report.md`](compatibility-report.md) (main `e7d52aa`).
The version assignments are proposals; each one is decided when its
phase starts. Counts are files of the 511-file corpus (`expect: pass`
unless stated). Every count is an **upper bound**, and each file is
assigned to exactly one candidate (§ 2), so the counts of different
candidates add up without double counting.

## 1. What the measurement changes

The past feature table (JBIG2 45, annotation appearances 42,
CIDFontType0 34, …) does not describe the current engine. Every
feature on it is implemented, and none causes a strict failure that
the standard lets a reader fix (report § 7). Three facts set the
priorities instead:

1. **Strict mode has almost no headroom on this corpus.** 43 strict
   blockers remain:
   - 38 are invalid input;
   - 4 need the exact non-embedded font;
   - 1 needs a Unicode field font.

   The most that strict-mode work can add is **5 files**, and all 5
   need a design decision (a font source, a bundled font). The
   compatibility rate (89.9 %) no longer measures progress well.
2. **The other measurable gaps are text, recovery mode and the
   measurement itself.**
   - Text: 37 extractions below F1 0.9. 15 are annotation text the API
     already produces, and 11 have unresolved codes.
   - Recovery mode: 25 of the 43 blockers are not drawn.
   - Measurement: 56 of the 68 low-SSIM files are reference
     disagreement that a single 96 dpi witness cannot separate from a
     real regression.
3. **The corpus under-samples the areas most likely to hide real
   gaps**: 9 Japanese, 13 scanned and 16 Office `expect: pass` files.

The recommended order is therefore:
1. make the measurement able to see regressions and new gaps;
2. take the cheap, well-bounded text and recovery work;
3. grow the corpus where it is thin;
4. take the design decisions (fonts) and the large quality items to
   v0.12 and later, informed by what the larger corpus shows.

## 2. Improvement estimates without double counting

Each strict blocker and each low-quality file is assigned to one
candidate: for a recovery failure, by its first recovery error; for a
low-SSIM file, by its attribution class; for a text file, by the first
matching group in report § 8. Columns:
- (1) files using the feature or construct;
- (2) files where it is the current cause;
- (3) likely to pass with this change alone;
- (4) needing another change as well;
- (5) no improvement expected.

| id | candidate | metric it moves | (1) | (2) | (3) | (4) | (5) | uncertainty |
|---|---|---|---:|---:|---:|---:|---:|---|
| T1 | annotation text in the CLI and the runner's headline text score | text F1 ≥ 0.9 | 167 with `/Annots` | 15 | 15 | 0 | 0 | low: measured with the API's `with_annotations(true)` (F1 ≥ 0.979 on all 15) |
| T2 | unresolved character codes (investigation, then fixes) | text F1 | 23 with unresolved codes on page 1 | 11 (F1 < 0.9) | unknown, expected ≤ 5 | — | likely ≥ 6 (Identity CID without ToUnicode or program mapping is undefined by §9.10.2) | high: the per-font cause is undetermined |
| R1 | recovery rule: operator with too few operands | recovery render | 3 | 3 | ≤ 3 | unknown (recovery stops at the first error; a later one may follow) | 0 | medium |
| R2 | recovery rule: form XObject without `/BBox` | recovery render | 1 | 1 | ≤ 1 | as R1 | 0 | medium |
| R3 | recovery rules: function input count, shading output count | recovery render | 2 | 2 | ≤ 2 | as R1 | 0 | medium |
| R4 | recovery rule: JBIG2 / JPEG codestream size ≠ image dictionary | recovery render (approximate) | 2 | 2 | ≤ 2 | as R1 | 0 | medium |
| R5 | decimated decode of oversized non-DCT images | recovery render; default-limit renders | 1 | 1 | ≤ 1 | — | 0 | low (raising the limit already draws it) |
| R6 | recovery parse: xref and page-tree rebuild | recovery parse | 5 | 5 | unknown | unknown | unknown | high: not investigated |
| A1 | `/AP /N` without `/AS`: a recovery policy | recovery render | 2 | 2 | ≤ 2 | 0 | 0 | low; the choice is a decision |
| F1 | Unicode field font for appearance synthesis | **strict** render | 1 (+ FreeText synthesis) | 1 | ≤ 1 | 0 | 0 | medium: the script the field needs decides the font |
| F2 | opt-in font source (caller-supplied font programs) for non-embedded Identity CID and symbolic TrueType | **strict** render when supplied | 4 | 4 | ≤ 4, only with the exact font | 0 | 0 | the corpus cannot test it without the fonts |
| Q1 | JPX / CCITT decode fidelity | SSIM (kasane class) | 5 (JPX 2, CCITT 3) | 3 | ≤ 3 | 1 (a JPX page also substitutes a font) | — | medium: may be in the decoder crates |
| Q2 | substitute-font metrics and shape | SSIM (kasane class) | 69 substituting | 3 | ≤ 3 | — | — | medium |
| Q3 | shading tone difference | SSIM (kasane class) | 27 with axial / radial | 1 | ≤ 1 | — | — | high: undetermined |
| Q4 | opt-in Poppler-compatible stroke adjustment | SSIM against Poppler only | ~25 hairline-form files (backlog) | 0 (spec-conforming today) | 0 by the spec; moves reference-class SSIM | — | — | it changes agreement with one witness, not correctness |
| M1 | second witness and resolution re-check in the runner | measurement | 68 below 0.95 | — | reclassifies 56 as reference, 3 page-box, 2 annotation-layer | — | — | low: measured in this analysis |
| M2 | corpus growth (Japanese, scanned, Office, forms) | finds gaps | — | — | — | — | — | by design: it measures what is not yet known |

Totals across candidates (no file in two rows):

| metric | today | all of T1–R5, A1, F1, F2, Q1–Q3 (upper bound) |
|---|---:|---:|
| strict render (`expect: pass`) | 382 | ≤ 387 (+1 F1, +4 F2 only with the exact fonts) |
| recovery render of the 43 blockers | 18 | ≤ 29 (R1 3 + R2 1 + R3 2 + R4 2 + R5 1 + A1 2), plus up to 5 from R6 |
| text F1 ≥ 0.9 (strict, 394) | 357 | ≤ 377 (T1 15, T2 ≤ 5) |
| kasane-class low SSIM | 7 (+2 annotation layer) | ≥ 0 (Q1 3, Q2 3, Q3 1) |

**Combinations asked for** (file counts, each once):

| combination | files whose outcome depends on it | why |
|---|---:|---|
| CIDFontType0 alone | 0 render; ≤ 3 text | 40 use it, 0 kasane-class SSIM; the 1 blocker is a non-embedded Identity CID font (F2); 3 files with mostly unresolved text use it (T2) |
| JBIG2 alone | 0 | 45 use it, all strict renders pass, low SSIM all reference class; the 2 JBIG2 blockers are invalid input (R4 covers 1 in recovery) |
| annotation appearances alone | 2 recovery (A1) + 1 strict (F1) + 15 text (T1) | appearance drawing itself has no kasane-class gap; the 2 annotation-layer files agree with MuPDF |
| CIDFontType0 + ActualText | 0 | no file has both as a cause |
| JBIG2 + CCITT | 1 (Q1, CCITT) | no file needs both |

## 3. Cost (code-based)

Efforts are person-days for one developer who knows the codebase,
including tests and documentation. They are based on the modules named.

| id | modules and APIs | change | effort | dependencies | memory / speed | security | maintenance |
|---|---|---|---:|---|---|---|---|
| R0 | `tests/recovery_rules.rs`, `tests/redact_model.rs` / `fuzz` `redact_model`, `src/redact_diag.rs` (`uncertain_state`) | redaction × recovery harness: every `RecoveryRule` opened with `from_bytes_recovering` and passed to preflight / remove / `verify-redact`; assert refusal or a sound result | 1.5 | — | — | **prerequisite** for every R-item: `redact.rs:1041`, `redact_diag.rs:3379`, `remove_region.rs`, `remove_text.rs:371`, `redact_type3.rs:313`, `redact_usage.rs:975` parse with `doc.recovery_mode()`, and no redaction test opens a recovered document today | small |
| T1 | `src/main.rs` (`run_text`), `TextOptions::with_annotations` (`src/text_extract.rs:143`), `compat/runner/src/main.rs` (`compare_text_with_annots`), `scripts/ci-regression.py` | `kasane-pdf text --annotations`; the runner reports both scores and gates on neither for a phase | 1 | — | none | extraction only; `redact` unaffected. Keep the API default unchanged (decision § 8) | small |
| T2 | `src/font.rs` (`unicode` at 1180 and 2586: ToUnicode → collection → program), `src/text_extract.rs` (`ResolveKind`), `src/cjk.rs` (Adobe-Japan1-UCS2) | investigation tool: per font, the reason a code is unresolved (no ToUnicode and Identity, collection and program disagree, non-AGL names); then the fixes it justifies | 2 + 2–4 | possibly further UCS2 tables for GB1 / CNS1 / Korea1 (Adobe, BSD-3) | small | text that redaction searches changes: a newly resolved code may make a search hit where none was. That is correct behaviour, but `verify-redact` residual checks must be re-baselined deliberately | medium |
| R1 | `src/content.rs` (`expect_operands` ~1298), `src/warning.rs` (`RecoveryRule`) | in recovery mode only, keep the operator as `Operation::Unknown` with its operands and a new `RecoveryRule`; the renderer ignores it; the redaction walker's `uncertain_state` taint refuses the page | 1.5 | R0 | none | must stay *visible* to redaction (taint), never dropped silently | small |
| R2 | `src/render.rs:3224`, `src/redact_diag.rs:3245` | recovery mode: a form without `/BBox` draws unclipped (MuPDF's behaviour); redaction treats its extent as unknown (refuse) | 1 | R0 | none | the redaction geometry must not invent a box | small |
| R3 | `src/function.rs:356,370`, `src/shading.rs:664,895` | recovery mode: use the first input; pad or truncate shading outputs, recorded | 1.5 | R0 | none | shadings already block region redaction (`blockers: shading`) | small |
| R4 | `src/bilevel.rs:171`, `src/render.rs:5382`, `src/remove_image.rs` | recovery mode: trust the codestream's size, mark approximate | 2 | R0 | the decoded size is bounded by the existing limits | **image redaction must keep refusing** a size mismatch (it rewrites samples by the dictionary's geometry; a mismatch would misplace the redacted area) | small |
| R5 | `src/render.rs` (`image_dims` ~4512, the DCT scale search ~4476), `src/filter.rs` | stream-decode a raw / Flate / RunLength image row by row, keeping every k-th sample, when its stored size exceeds the limits but its drawn size does not | 3–4 | — | peak memory falls for huge images; one extra decode path | limits are a DoS control: the decimated path must be bounded by the same decoded-bytes budget; fuzz target needed | medium |
| R6 | `src/xref.rs` (rebuild), `src/page.rs` (page tree) | investigation first: what the 5 files need | 1–2 + unknown | — | — | recovered structure feeds redaction (R0 covers it) | — |
| A1 | `src/annot_appearance.rs` (state selection), `src/render.rs:3866` | recovery mode: the appearance state chosen by a stated rule (decision § 8), recorded | 0.5–1 | R0 | none | annotation redaction refuses non-link annotations already | small |
| F1 | `src/acroform.rs:219` (field font), `src/fontsubst.rs`, `fonts/` | a bundled Unicode field font (scripts to be chosen) for synthesised appearances | 2–4 | font asset (SIL OFL 1.1, e.g. Noto Sans); crate size limit 10 MB, today ≈ 4.3 MB fonts + 5.8 MB `assets` (the CJK asset is feature-gated) | +1–3 MB for each script group | redaction of forms already refuses synthesis cases it cannot verify; keep the font out of `redact` | medium |
| F2 | `RenderOptions` (new font provider), `src/font.rs` (non-embedded paths ~105, 2049), `src/cjk.rs` | caller-supplied font bytes by PostScript name; never an implicit OS lookup | 5–8 | — | per-render font parse (cached) | **`redact` must keep refusing non-embedded fonts** (readers choose differently; `limitations.md`); the provider is render / text only | medium–high (API surface) |
| Q1 | `src/jpx.rs` (`hayro-jpeg2000` wrapper), `src/bilevel.rs` (`hayro-ccitt`) | find whether the difference is colour (JPX `/ColorSpace` vs codestream), `/Decode`, or the decoder; fix here or upstream | 1 + 1–2 | upstream `hayro` releases if the defect is there | none | none for redaction (images are not decoded for removal, except JPEG) | small |
| Q2 | `src/fontsubst.rs` (672 lines) | horizontal scaling of substitutes to `/Widths` where it is missing; weight / italic selection | 2–3 | — | none | redaction refuses non-embedded fonts; no effect | small |
| Q3 | `src/shading.rs` | investigation of the tone difference (sampling, function precision, colour conversion) | 1 + unknown | — | — | — | — |
| Q4 | `src/render.rs` stroke path | `RenderOptions::stroke_adjust_always` (default off, spec behaviour unchanged) | 1–2 | — | none | none | small |
| M1 | `compat/runner/src/main.rs`, `scripts/compat-attribute.py`, `scripts/ci-regression.py`, nightly workflow | per page below a threshold: MuPDF render and the 300 dpi re-check; the class in the report; regressions keyed on kasane-class pages | 2–3 | MuPDF already installed in the renderer jobs | nightly +2–3 min | none; `workflow guard` must accept the change | small |
| M2 | `compat/manifest.yaml`, the corpus store, `docs/compat-corpus.md` | +40–60 files (Japanese, scanned, Office, forms) with licence and `expect` evidence; a baseline update reviewed by hand | 3–5 | licence review per source | nightly time grows linearly | none; the corpus stays private (AGENTS rules) | ongoing |

### External libraries

| area | today | alternatives | verdict |
|---|---|---|---|
| JBIG2 | `hayro-jbig2` 0.3.1 (Apache-2.0 OR MIT, pure Rust) | jbig2dec (AGPL-3.0: incompatible with Apache-2.0 distribution); PDFium via FFI (BSD-3, C++; FFI and build cost, no WASM) | keep. 0 failures, 0 kasane-class low pages |
| JPEG 2000 | `hayro-jpeg2000` 0.4.1 (pure Rust) | OpenJPEG via `openjpeg-sys` / `jpeg2k` (BSD-2, C; a long CVE history, FFI, no WASM) | keep; Q1 decides whether a defect is upstream, and it is fixed there (same maintainer as JBIG2 and CCITT) |
| CCITT | `hayro-ccitt` 0.4.0 | libtiff (C) | keep |
| CFF / CFF CID / Type 1 | in-house (`cff_matrix.rs`, `type1.rs`) on `ttf-parser` 0.25.1 (RUSTSEC-2026-0192 unmaintained notice, kept by `decisions/ttf-parser.md`) | `skrifa` / `read-fonts` (Google fontations, MIT OR Apache-2.0, maintained, pure Rust) | no change for compatibility (0 failures). For maintenance, evaluate `skrifa` when glyph rasterisation is reworked (v0.13+), not before |
| glyph rasterisation | `tiny-skia` 0.12 (BSD-3) | `skrifa` outlines + an exact-area rasteriser (`zeno`, or in-house) | v0.13+ research item; the coverage difference is one cause of the reference classes |
| Unicode field font | none (Latin only) | Noto Sans families (SIL OFL 1.1), Liberation (already bundled; Latin / Greek / Cyrillic only) | F1: pick by the scripts needed; a feature flag if large |

## 4. Priorities

Each criterion is rated ++, +, 0 or −:
- **corpus**: the files it moves (§ 2);
- **quality**: its contribution to render or text fidelity;
- **enabling**: how far other work depends on it;
- **general**: how common the construct is in general PDFs;
- **JP**: its contribution to Japanese PDFs;
- **safety**: its security weight. A prerequisite is "++"; a risk to
  manage is "−".
- **maint.**: its long-term maintenance cost.

| rank | id | feature | files (upper bound) | effort (d) | dependencies | corpus | quality | enabling | general | JP | safety | maint. | reason |
|---:|---|---|---:|---:|---|---|---|---|---|---|---|---|---|
| 1 | M1 | second witness + resolution re-check | reclassifies 56 | 2–3 | — | 0 | + | ++ | + | + | 0 | + | without it a regression in 56 files is invisible and every later quality item is unmeasurable |
| 2 | R0 | redaction × recovery harness | 0 | 1.5 | — | 0 | 0 | ++ | 0 | 0 | ++ | + | gates every recovery rule; closes an untested path today |
| 3 | T1 | annotation text | 15 text | 1 | — | ++ | + | 0 | + | 0 | 0 | + | the largest single text gain, already implemented in the API |
| 4 | R1 | operand-count recovery | ≤ 3 recovery | 1.5 | R0 | + | 0 | 0 | + | 0 | − | + | most common recovery failure; MuPDF draws these files |
| 5 | M2 | corpus growth | — | 3–5 | M1 | ? | + | ++ | ++ | ++ | 0 | 0 | strict mode is saturated; only new files can show the next gaps, above all Japanese and scanned |
| 6 | R2 | form without `/BBox` | ≤ 1 recovery | 1 | R0 | + | 0 | 0 | + | 0 | − | + | small, bounded |
| 7 | R3 | function / shading counts | ≤ 2 recovery | 1.5 | R0 | + | 0 | 0 | 0 | 0 | 0 | + | small, bounded |
| 8 | R4 | codestream size mismatch | ≤ 2 recovery | 2 | R0 | + | 0 | 0 | + | 0 | − | + | scanned / producer bugs; must not reach image redaction |
| 9 | T2 | unresolved-code investigation | ≤ 11 text (expected ≤ 5) | 2 (+2–4) | — | + | + | + | + | ++ | 0 | 0 | CID fonts without ToUnicode are the Japanese text risk |
| 10 | Q1 | JPX / CCITT fidelity | ≤ 3 SSIM | 2–3 | M1 | + | + | 0 | + | 0 | 0 | 0 | the only image-decoder gaps left; scans |
| 11 | Q2 | substitute-font metrics | ≤ 3 SSIM | 2–3 | M1 | + | + | 0 | ++ | 0 | 0 | 0 | non-embedded fonts are common in Office output |
| 12 | A1 | `/AS` policy | ≤ 2 recovery | 0.5–1 | R0, decision | + | 0 | 0 | 0 | 0 | 0 | + | cheap, but invalid input with no basis to choose |
| 13 | R5 | decimated non-DCT decode | ≤ 1 | 3–4 | — | + | 0 | 0 | 0 | 0 | − | − | rare; a caller can raise the limit today |
| 14 | F1 | Unicode field font | ≤ 1 strict | 2–4 | decision | + | + | 0 | + | + | 0 | − | the only strict gain without an external font; asset size |
| 15 | F2 | opt-in font source | ≤ 4 strict (with fonts) | 5–8 | decision | + | + | + | + | ++ | − | − | needs API design; untestable on the corpus without the fonts |
| 16 | Q4 | opt-in stroke adjustment | 0 by spec | 1–2 | M1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | agreement with one witness, not correctness |
| 17 | Q3 | shading tone | ≤ 1 | 1 + ? | M1 | 0 | + | 0 | 0 | 0 | 0 | 0 | undetermined |
| 18 | R6 | xref / page-tree rebuild | ≤ 5 (unknown) | 1–2 + ? | R0 | ? | 0 | 0 | + | 0 | − | − | investigation first |

## 5. Plans

| | Plan A — quick wins | **Plan B — balanced (recommended)** | Plan C — maximum compatibility |
|---|---|---|---|
| items | R0, T1, R1, R2, R3, A1 | M1, R0, T1, R1, R2, R3, R4, T2 (investigation), Q1, M2 (first tranche) | Plan B + T2 fixes, Q2, A1, R5, R6, F1, F2, Q3, Q4, exact-coverage glyph rasterisation |
| effort | ≈ 7 d | ≈ 18–20 d | ≈ 50–55 d, plus open-ended research (R6, rasterisation) |
| expected gain (upper bound) | text +15 (F1 ≥ 0.9: 357 → 372); recovery render +8 of 43 (18 → 26) | text +15 (+≤ 5 after T2 fixes in v0.12); recovery +8 (18 → 26); kasane-class SSIM −3 at most (Q1); 56 low-SSIM files reclassified; the corpus +40–60 files in the thin categories | strict +≤ 5 (with fonts for 4); recovery +≤ 16 (18 → ≤ 34); text +≤ 20; kasane-class SSIM to ≈ 0 |
| main risk | gains are invisible in the compatibility rate (recovery mode and text only); quality regressions stay hidden in the reference classes | M2 adds review work and may raise the failure count (by design: new gaps); Q1 may sit upstream | API surface (F2), asset size (F1), parser work with unknown yield (R6), a long research item (rasterisation); safety work grows with every recovery rule |
| uncertainty | low | medium (Q1 location, M2 sourcing) | high (R6, T2, F2 untestable without fonts, rasterisation) |

**Recommendation: Plan B.** Plan A spends a week on gains that the
headline rate cannot show, and it leaves the measurement unable to
see regressions in 56 files. Plan C buys at most 5 strict files with
a new API and a font asset, plus parser work of unknown yield, before
the corpus can show whether those are the gaps that matter. Plan B
does the following:
- makes the measurement trustworthy first (M1), so that every later
  change shows up as kasane-class movement, not noise;
- closes the untested redaction × recovery path before adding any
  recovery rule (R0);
- takes every bounded, cheap gain: text (T1) and four recovery rules;
- grows the corpus where the next real gaps are likely: Japanese,
  scanned and Office files (M2);
- answers the two open questions (T2, Q1) with data.

The design decisions (fonts, policies) then go to v0.12 with
evidence.

## 6. Version plan

### v0.11.0 (≈ 18–20 d, Plan B)

Order matters: M1 and R0 come first; R1–R4 depend on R0; M2 depends
on M1.

| task | purpose | modules | change | tests | done when | risk | prerequisite |
|---|---|---|---|---|---|---|---|
| **M1-a** | second witness in the runner | `compat/runner/src/main.rs` | for every page below 0.95: MuPDF render, kasane–MuPDF and Poppler–MuPDF `dpx`, the attribution class; JSON fields `render_attribution`, `render_dpx_*` | runner unit tests on synthetic pages (agreeing, kasane-only, reference-only) | the nightly report carries the class for every page below 0.95; this analysis's counts reproduced (56 / 7 / 3 / 2) | runtime; MuPDF version drift → record the version in the report | — |
| **M1-b** | resolution re-check | runner, `scripts/compat-attribute.py` (logic shared or ported) | 300 dpi re-render of those pages; class `rasterisation` when the difference falls below 0.5 % | as above | report counts match § 6.2 of the report | runtime | M1-a |
| **M1-c** | regressions keyed on kasane-class pages | `scripts/ci-regression.py`, baseline schema | a page that moves *into* the kasane class is a regression; reference-class moves are report-only | `scripts/test_ci_scripts.py` cases | nightly fails on a synthetic kasane-class regression, not on a reference-class change | baseline schema change → migration note | M1-a |
| **R0** | redaction × recovery harness | `tests/recovery_rules.rs`, `tests/redact_model.rs`, `fuzz` `redact_model` seeds | for each `RecoveryRule` (10 today): a synthetic file opened with `from_bytes_recovering`, then preflight, region / text / image removal and `verify-redact`; expected: refusal, or a result `verify-redact` accepts | the harness itself; a fuzz seed per rule | every rule has a redaction test; the matrix is documented in `redaction-safety-model.md` | finds an existing gap → fix it first (security process, `security/`) | — |
| **T1** | annotation text | `src/main.rs`, runner, `scripts/ci-regression.py` | `kasane-pdf text --annotations`; the runner keeps the default F1 and reports `with_annots` beside it | CLI test on the FreeText probe; runner report test | the probe's annotation word is extracted with the flag; the default output is unchanged | none | — |
| **R1** | operand-count recovery | `src/content.rs`, `src/warning.rs` | recovery mode: an operator with the wrong operand count becomes `Operation::Unknown` plus `RecoveryRule::IgnoredWrongOperandCount`; strict mode unchanged | `tests/recovery_rules.rs` (each operator family); R0 matrix row (redaction refuses through `uncertain_state`); a `render_content` fuzz seed | the `operand-count` probe renders in recovery mode and is refused by strict mode and by redaction; the corpus recovery count moves only on R1's files | over-acceptance: limit it to operand-count mismatches, never type errors | R0 |
| **R2** | form without `/BBox` | `src/render.rs`, `src/redact_diag.rs` | recovery mode: draw unclipped, with a warning; redaction: extent unknown, so refuse | recovery test; R0 row; the `form-no-bbox` probe | probe renders in recovery mode; redaction refuses it | none | R0 |
| **R3** | function / shading counts | `src/function.rs`, `src/shading.rs` | recovery mode: first input; outputs padded with 0 or truncated, with a warning | unit tests in `function.rs` / `shading.rs`; R0 row | synthetic cases render in recovery mode | a wrong colour shown as right → recorded as approximate | R0 |
| **R4** | codestream size mismatch | `src/bilevel.rs`, `src/render.rs`, `src/remove_image.rs` | recovery mode render: decode at the codestream size, scale to the image space, approximate; `remove_image` keeps refusing | JBIG2 and JPEG synthetic mismatches; `redact_images.rs` asserts the refusal | render recovers, image redaction refuses, `verify-redact` unchanged | memory: the codestream size goes through the same limits | R0 |
| **T2-a** | unresolved-code investigation | `src/font.rs`, a script under `scripts/` | per font: the reason a code is unresolved (counts only) | — | a categorised count for the 23 files in the report; a decision on T2-b | none | — |
| **Q1** | JPX / CCITT fidelity | `src/jpx.rs`, `src/bilevel.rs` | find the cause on synthetic equivalents; fix here or report upstream | synthetic JPX / CCITT fixtures against Poppler and MuPDF | the 3 files leave the kasane class, or the cause is documented as upstream | the fix depends on an upstream release | M1-a |
| **M2-a** | corpus growth, first tranche | `compat/manifest.yaml`, store, `docs/compat-corpus.md` | +40–60 files: Japanese (CID, vertical writing, ToUnicode-less), scanned (JBIG2, CCITT, JPX, DCT), Office, forms; licence and `expect` evidence each | — | baseline refreshed by hand with a reviewed diff; the report's § 3 recomputed with the new denominators shown separately | licensing; private store only (AGENTS rules) | M1 |

v0.11.0 completion criteria:
- the nightly report carries the attribution classes;
- R0 has a row for every recovery rule;
- R1–R4 are live, each with strict-mode behaviour unchanged (the
  corpus strict counts identical);
- T1 is shipped;
- T2-a and Q1 are resolved or documented;
- the corpus has grown;
- every security check passes unchanged:
  - `verify-redact` (all profiles);
  - advisory regression and the 14 GHSA reproductions on old versions;
  - the non-interference model;
  - fuzz smoke;
  - Fast / Full Gate;
  - `workflow guard`;
  - Linux renderer comparison;
  - corpus baseline (no new kasane-class regression);
  - release verification.

### v0.12.0 (≈ 15–20 d; proposed)

T2-b (the unresolved-code fixes T2-a justifies, 2–4 d), Q2
substitute-font metrics (2–3 d), A1 `/AS` policy (0.5–1 d, after the
decision), F1 Unicode field font (2–4 d, after the decision on scripts
and size), R5 decimated decode (3–4 d, with a fuzz target), R6
investigation (1–2 d), Q4 opt-in stroke adjustment (1–2 d, if M1 shows
it is worth reporting), M2 second tranche. Each R-item adds its R0 row
before it lands.

### v0.13.0 and later (design changes)

| item | why later |
|---|---|
| F2 opt-in font source (5–8 d) | new public API; the corpus cannot test it without the fonts; redaction must stay refusing |
| exact-coverage glyph rasterisation (10+ d; `skrifa` evaluation) | a renderer-wide change; one cause of the reference classes; needs M1 to measure |
| R6 fixes | depend on the R6 investigation |
| ICC with Lab PCS, `/Matte`, `/SMaskInData`, shading `/ExtGState`, Type 3 in clip modes (`limitations.md`) | 0 corpus files need them today (`features-v0.11.0`: Lab 0, `/Matte` 0, `/SMaskInData` 0); revisit when M2 finds users |
| GB1 / CNS1 / Korea1 fallback fonts | 0 corpus files need them (backlog); size (companion crate) |
| XFA | out of scope; the static AcroForm appearances are drawn |

## 7. Security and redaction impact

Every engine change below parses or draws content. Each must hold the
v0.10.0 guarantees:
- `verify-redact`;
- advisory regression and the 14 GHSA reproductions on old versions;
- the non-interference model;
- fuzz;
- Fast / Full Gate;
- `workflow guard`;
- Linux renderer comparison;
- corpus baseline;
- release verification.

**Rules for the work:**
1. **Recovery is shared.** The redaction paths parse content with
   `doc.recovery_mode()`, so every recovery rule changes what
   redaction sees in a recovered document. Therefore R0 comes first,
   and each R-item adds its R0 row. A recovery rule may make a page
   *drawable*. It must never make an unverifiable page *redactable*:
   - dropped or repaired constructs stay visible to the walker's
     `uncertain_state` taint (`Operation::Unknown`, not omission);
   - geometry the file does not state (R2) is unknown, not invented;
   - an image whose sizes disagree (R4) is never rewritten.
2. **Strict mode does not change** in any v0.11 item. The strict
   corpus counts must stay identical, which is a release check.
3. **Text changes are redaction-visible.** T2 can resolve codes that
   redaction search did not match before. That is correct, but the
   redaction corpus outcomes and `verify-redact` residual checks are
   re-run and any change reviewed. T1 changes the CLI only; the API
   default stays as it is.
4. **Font sources (F1, F2) are render / text only.** `redact` keeps
   refusing non-embedded fonts; a caller-supplied font is never used to
   decide what a redaction removed.
5. **Limits are DoS controls.** R4 and R5 decode within the existing
   `Limits`, and the decoded-bytes budget, with a fuzz target or seed
   each.
6. **No check is weakened.** No allow-list, threshold or baseline is
   relaxed to make an item pass; baselines change only by a reviewed,
   hand-applied update.

| item | redaction paths touched | new tests | risk if wrong |
|---|---|---|---|
| R1 | content parsing in every redaction walker | R0 row, recovery test, fuzz seed | a dropped operator hiding text from the walker → mitigated by keeping it `Unknown` (taint) |
| R2 | `redact_diag.rs` form extent | R0 row | an invented clip → refused instead |
| R3 | shading / function evaluation (render only; redaction blocks shadings) | R0 row | none for redaction |
| R4 | `remove_image.rs` | `redact_images.rs` refusal | misplaced removal → refused instead |
| R5 | none (render) | fuzz target | memory exhaustion → limits |
| T1 | none (CLI) | CLI test | none |
| T2 | text search in preflight / remove-text | redaction corpus re-run, `verify-redact` | new search hits (intended), reviewed |
| F1 / F2 | none (render / text); `redact` refuses | tests that redaction still refuses | redaction trusting a substitute → forbidden by rule 4 |
| M1 / M2 | none (tooling, corpus) | `test_ci_scripts.py`, `check-workflows.py` | a workflow change rejected by `workflow guard` → fixed in the PR |

## 8. Decisions needed before the implementation phase

1. **Plan B for v0.11.0**, including corpus growth (M2) whose
   sourcing needs licence review.
2. **Text default.** Keep `TextOptions` without annotation text by
   default and add a CLI flag (recommended), or include annotation text
   by default as Poppler and MuPDF do (a behaviour change for API
   users).
3. **`/AS` missing (A1)**: in recovery mode, draw nothing, the first
   state, or the `/Off`-other state. This is invalid input with no
   basis in the standard; the recommendation is to draw nothing and
   record it.
4. **Unicode field font (F1)**: which scripts, bundled or
   feature-gated, and the size budget against the crates.io limit.
5. **Font source (F2)**: whether a caller-supplied font API is wanted
   at all. It is the only path to the 4 font-blocked files.
6. **Stroke adjustment (Q4)**: whether an opt-in Poppler-compatible
   mode is worth shipping (it improves agreement with one witness, not
   correctness).
