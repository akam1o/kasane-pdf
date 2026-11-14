# Compatibility report — v0.11.0 planning (main `e7d52aa`)

The 511-file compatibility corpus, the 117-file redaction corpus and
the synthetic renderer cases, measured again on main
`e7d52aa0e028cd8dd7a3aa4a0a947d86921b1b58` (v0.10.0 plus the advisory
history work) to decide what to implement next. The plan that follows
from it is [`compatibility-roadmap.md`](compatibility-roadmap.md).

All figures are counts over the corpus; no file of the corpus is named
or quoted (per-file evidence stays in the gitignored `compat/reports/`).
**Measured** figures come from the runs below; **estimated** ones are
marked as such and carry their basis. Figures from earlier analyses are
labelled *past* and are never mixed into the current columns.

The machine-readable results are in [`../compat/analysis/`](../compat/analysis/)
(counts only):

| file | content |
|---|---|
| `compat-v0.11.0-e7d52aa.json` | stages, verdicts, SSIM and text-F1 distributions, quality-gate classes, strict blockers by root cause with their recovery outcome, attribution classes, Unicode sources, differences from the committed baseline |
| `attribution-v0.11.0-e7d52aa.json` | the low-SSIM attribution classes, their feature cross-tabulation and the resolution check medians |
| `features-v0.11.0-e7d52aa.json` | feature presence among the `expect: pass` files and its co-occurrence with blockers and low SSIM |
| `probes-v0.11.0-e7d52aa.json` | the synthetic probes (`scripts/compat-probes.py`) |

## 1. Summary

- **Strict compatibility is 382 / 425 (89.9 %)**, up 2 from the
  committed baseline (380), with 0 regressions. Robustness: 44 / 44
  `reject` files handled, 0 crash, 0 timeout, 0 unexpected acceptance.
- **Every one of the 43 remaining strict blockers is a file the
  standard does not let a strict reader draw** as it is: 39 are invalid
  input, 3 need the exact non-embedded font, and 1 needs a Unicode
  field font, which is a missing feature. Strict mode therefore has
  almost no headroom left on this corpus: at most 4 files, and only
  through an opt-in font source or a new field font. The remaining work
  on blockers is about **recovery mode**, which draws 18 of the 43
  today.
- **The features on the past table are all implemented.** JBIG2,
  CCITT, JPX, ASCIIHex / ASCII85 / LZW / RunLength, tiling patterns,
  knockout, luminosity masks, CFF CID, ActualText and Optional Content
  are no longer the cause of any strict failure, and the attribution
  puts no low-SSIM page on JBIG2, CIDFontType0 or Type 1 CFF.
- **Of the 68 rendered files below SSIM 0.95, 44 are within tolerance
  of a reference at 300 dpi** (Poppler 36, MuPDF only 8), most likely
  rasterisation differences; the check failed for 1. **23 are outside
  tolerance of both references**:
  - broad tone or colour differences: 7 (undetermined);
  - borderline Skia pages: 5;
  - non-embedded font substitution: 4;
  - annotation layer: 3 (undetermined);
  - CCITT: 2;
  - 1 JBIG2 page both references draw wrongly;
  - undetermined: 1.
- **Text is the largest measurable gap.** 37 of 394 strict extractions
  have a page-1 character F1 below 0.9:
  - 15 of them reach ≥ 0.9 once annotation text is included;
  - 11 have mostly unresolvable character codes;
  - 3 have an empty reference;
  - the remaining 8 are too small to judge (≤ 34 characters) or
    undetermined.
- The redaction corpus is unchanged (0 regressions, 0 improvements in
  all four configurations). External renderer cases: 110 passed.

## 2. Environment and procedure

| item | value |
|---|---|
| commit | `e7d52aa0e028cd8dd7a3aa4a0a947d86921b1b58` (main, 2026-11-15) |
| host | macOS 26.5.1, arm64 |
| toolchain | rustc 1.99.1 |
| references | Poppler 26.10.0 (`pdftoppm`, `pdftotext`; the compatibility reference), MuPDF 1.28.6 (`mutool`; second witness for attribution), pdf.js 6.5.438 (external renderer cases only) |
| other | qpdf 12.4.2, Pillow 13.0.0 (`.venv-corpus`) |
| runner settings | the nightly's: `--mode full`, Poppler reference, 96 dpi, 20 s per file, default features (the `cjk-fallback` runner build for the Japanese category), unchanged from the baseline |
| baseline | `compat/baselines/corpus-full.json` (2026-11-07, `fe9fff4`); not updated by this analysis |

```sh
# 1. The three corpus checks on the commit, in a clean worktree (≈ 14 min).
#    Needs the corpus (docs/compat-corpus.md) and the R2 environment.
python3 scripts/release-verify.py run --ref e7d52aa \
    --only compat_corpus --only redact_corpus --only external_renderers \
    --out /tmp/kp-work/v11-measure --keep
cp /tmp/kp-work/v11-measure/work/compat_corpus/run/latest.json compat/reports/run-v011-e7d52aa.json
K=/tmp/kp-work/v11-measure/target/release/kasane-pdf      # the binary of that commit

# 2. Strict blockers by root cause (compat/reports/diag-render-blockers-run-v011-e7d52aa.json).
python3 scripts/diag-render-blockers.py compat/reports/run-v011-e7d52aa.json

# 3. Low-SSIM attribution, with the 72/150/300 dpi resolution check (≈ 2 min).
.venv-corpus/bin/python scripts/compat-attribute.py compat/reports/run-v011-e7d52aa.json "$K" \
    --out compat/reports/attribution-v011-e7d52aa.json \
    --summary compat/analysis/attribution-v0.11.0-e7d52aa.json --resolution-check

# 4. Page-1 Unicode sources: for every strict `expect: pass` text pass,
#    `"$K" text <file> 1 --format json`, the per-character "via" values
#    counted into compat/reports/unicode-sources-v011.json
#    ([{"id", "chars", "sources": {via: count}}]).

# 5. Feature presence and the public summary.
python3 scripts/compat-featscan.py --run compat/reports/run-v011-e7d52aa.json \
    --diag compat/reports/diag-render-blockers-run-v011-e7d52aa.json \
    --attribution compat/reports/attribution-v011-e7d52aa.json \
    --out compat/reports/featscan-v011.json --summary compat/analysis/features-v0.11.0-e7d52aa.json
python3 scripts/compat-summary.py --run compat/reports/run-v011-e7d52aa.json \
    --diag compat/reports/diag-render-blockers-run-v011-e7d52aa.json \
    --attribution compat/reports/attribution-v011-e7d52aa.json \
    --unicode compat/reports/unicode-sources-v011.json \
    --baseline compat/baselines/corpus-full.json --out compat/analysis/compat-v0.11.0-e7d52aa.json

# 6. Synthetic probes (no corpus needed).
python3 scripts/compat-probes.py "$K" --json compat/analysis/probes-v0.11.0-e7d52aa.json
```

**Counting rule.** A strict stage needs a strict parse. When the strict
parse fails, the runner reads the recovered document and records that
document's render, text and rewrite in the same fields. Those outcomes
are not strict ones; they are counted as *skip*, the same rule as
`analysis::rated_class` in the runner. Two scripts counted raw
fields and are fixed in this change:
- `scripts/diag-render-blockers.py` missed the 7 recovered files and
  had no rule for a JPEG frame-size mismatch;
- the new summary applies the rule throughout.

## 3. Current scores (measured)

### 3.1 Whole corpus (511)

| verdict | files |
|---|---:|
| pass | 378 |
| error | 83 |
| unsupported | 50 |

| expect | files | meaning |
|---|---:|---|
| `pass` | 425 | well-formed; feeds the compatibility rate |
| `reject` | 44 | malformed or adversarial; must be handled without crash or timeout |
| `any` | 42 | out-of-scope features or recovery-only faults; feeds neither rate |

Strict stages over all 511 (recovered documents' downstream stages as
*skip*): parse 449 pass / 59 error / 3 unsupported; render 416 pass / 23
error / 10 unsupported / 62 skip; text 429 / 13 / 7 / 62; rewrite 407 /
4 / 38 / 62. Recovery parse (on the 62 strict parse failures): 38 pass,
19 error, 5 unsupported. Crash 0, timeout 0. Median wall time 302 ms per
file; peak RSS 1.9 GB (the largest file).

### 3.2 Compatibility (`expect: pass`, 425)

| stage | baseline 2026-11-07 | now | Δ |
|---|---:|---:|---:|
| strict parse | 408 | 408 | 0 |
| strict render | 380 (89.4 %) | **382 (89.9 %)** | +2 |
| of which approximate | 70 | 71 | +1 |
| strict text | 392 | 394 | +2 |
| rewrite (default profile) | 372 | 372 | 0 |
| verdict pass (every stage) | 345 | 347 | +2 |

The 36 rewrite refusals are profile policy, not engine failures:
signed documents 27, JavaScript name tree 4, catalog entries outside
the profile 3, writing an encrypted document 2.

**Quality gate** (on the 382 strict renders; `docs/plans/quality-gate.md`):
- ok 285;
- n/a 83: approximate renders, which the gate does not judge, or pages
  without a Poppler reference;
- report-only 7;
- exempt 6: blank Poppler pages and `/UserUnit` sizes;
- flag 1: the known, reviewed hairline-grid file.

The baseline had 283 / 82 / 8 / 6 / 1. The enforced rule fails on no
new file.

### 3.3 Robustness (`reject`, 44)

44 / 44 handled: all 44 refused strictly (41 at parse, 2 unsupported
at parse, 1 at render), 0 crash, 0 timeout, 0 unexpected acceptance.
20 of them are read by recovery mode and its render passes. That is the
best-effort reading of damaged files and does not count as acceptance.

## 4. Differences from the baseline

| class | files | which |
|---|---:|---|
| improved | 2 | one strict render `error → pass` and one `unsupported → pass` (each with its text stage); both from the engine changes since the baseline |
| regressed | 0 | — |
| unchanged | 509 | no stage class changed |
| not comparable | 0 | same 511 ids, same `expect` tags, same settings |

Environment-driven differences: the run used the same Poppler
(26.10.0) and settings as the baseline's nightly. The quality-gate
moves (ok +2, report −1, n/a +1) are engine changes, not tool
versions:
- the 2 improved files entered the gate's population, one as ok and
  one as n/a (an approximate render, which the gate does not judge);
- one unchanged file's report-only finding cleared (report → ok).

## 5. Strict blockers (`expect: pass`, 43)

Root causes from `scripts/diag-render-blockers.py`. Each file is
counted once, by its first strict error; the relations to second
causes are given after the table. "Recovery" is the outcome the runner
recorded in recovery mode: the render passes, the render fails, or
recovery was not attempted because recovery parse failed or the strict
refusal is a capability one.

| root cause | files | status | spec | recovery pass / error / not attempted |
|---|---:|---|---|---|
| damaged file structure (xref, syntax) | 11 | invalid input | §7.5 | 7 / 1 / 3 |
| page without `/MediaBox` | 4 | invalid input | Table 30 | 0 / 4 / 0 |
| operator inside a path object | 3 | invalid input | §8.2 | 3 / 0 / 0 |
| CID font without a usable program | 3 | 2 non-embedded `Identity` CIDFontType2: **spec limit** (CIDs are glyph ids of an unknown font); 1 CIDFontType0 with a Type 1 `/FontFile`, where §9.9 requires `/FontFile3`: invalid input (orderings and keys verified per font) | §9.7.4, §9.9 | 0 / 0 / 3 |
| `/AP /N` states without `/AS` | 2 | invalid input (no basis to choose) | §12.5.5 | 0 / 0 / 2 |
| invalid page tree | 2 | invalid input | §7.7.3 | 0 / 0 / 2 |
| symbolic non-embedded TrueType, unknown name | 1 | **spec limit** (glyph set unknowable) | §9.6.6.4 | 0 / 0 / 1 |
| non-Latin field value, single-byte field font | 1 | **not implemented** (Unicode field font) | §12.7.3.3 | 0 / 0 / 1 |
| JBIG2 page size ≠ `/Width` `/Height` | 1 | invalid input | §7.4.7 | 0 / 1 / 0 |
| JPEG frame size ≠ `/Width` `/Height` | 1 | invalid input | §8.9.5 | 0 / 1 / 0 |
| shading function outputs ≠ colour components | 1 | invalid input | §8.7.4.5 | 0 / 1 / 0 |
| inline image without `EI` | 1 | invalid input | §8.9.7 | 0 / 1 / 0 |
| tiling-pattern cell cycle | 1 | invalid input | §8.7.3 | 0 / 1 / 0 |
| corrupt Flate data | 1 | invalid input | §7.4.4 | 0 / 1 / 0 |
| unknown operator outside `BX`/`EX` | 1 | invalid input | §7.8.2 | 0 / 1 / 0 |
| JBIG2 filter on a non-image stream | 1 | invalid input | §7.4.7 | 0 / 0 / 1 |
| 8 single causes: stray `Q`, degenerate page box, `/AS` state not a stream, `/SMask` not a stream, malformed `/Differences`, non-dictionary `/Annots` entry, undefined `/Properties` name, null resource | 8 | invalid input | various | 8 / 0 / 0 |
| **total** | **43** | invalid 39, spec limit 3, not implemented 1 | | **18 / 12 / 13** |

**Multiple causes.** Recovery repairs the first fault and stops at the
next, so the recovery error is the second cause:
- the 4 files without `/MediaBox` each have a second fault behind it:
  operand count, form without `/BBox`, a type 2/3 function given two
  inputs, and an image larger than the default `max_image_dimension`
  (a RunLength image; valid apart from the missing box);
- one damaged-structure file and the unknown-operator file stop at an
  operand count.

**The 12 recovery failures**, each counted once by its recovery error:

| recovery error | files | fixable by a recovery rule? |
|---|---:|---|
| operator with too few operands | 3 | yes: drop the operator, with a warning (MuPDF tolerates it) |
| form XObject without `/BBox` | 1 | yes: draw unclipped or use the page box (MuPDF draws it) |
| type 2/3 function given more than one input | 1 | yes: use the first input |
| shading function outputs ≠ colour components | 1 | possible: pad or truncate the outputs |
| JBIG2 / JPEG codestream size ≠ dictionary | 2 | possible: trust the codestream (approximate) |
| image larger than `max_image_dimension` | 1 | no rule needed: a caller-raised `Limits::with_max_image_dimension` draws it today; a default draw needs decimated decoding of non-DCT images |
| inline image without `EI`, pattern cell cycle, corrupt Flate | 3 | no: the data is not recoverable, or the meaning is undefined |

**The 13 not attempted**:
- recovery parse fails on 5 (damaged structure 3, invalid page tree 2);
- 8 are capability refusals that recovery does not change:
  - CID fonts without a usable program 3;
  - non-embedded symbolic TrueType 1;
  - `/AS` missing 2;
  - Unicode field value 1;
  - JBIG2 on a content stream 1.

## 6. Rendering quality

### 6.1 SSIM against Poppler (strict renders, 96 dpi)

| level | n | median | P5 | P25 | P75 | P95 | < 0.95 | < 0.90 | < 0.80 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| file (worst page) | 365 | 0.9949 | 0.8528 | 0.9736 | 0.9995 | 1.0000 | 68 | 34 | 11 |
| page | 1010 | 0.9905 | 0.8927 | 0.9706 | 0.9973 | 1.0000 | 160 | 63 | 11 |

17 strict renders have no Poppler reference page to compare.

### 6.2 What the 68 files below 0.95 are

`scripts/compat-attribute.py` renders each file's worst page at 72 dpi,
greyscale, with the runner's page box (`-cropbox`), in four ways:
- kasane;
- Poppler;
- MuPDF;
- the same three on a copy with every `/Annots` key renamed, so that no
  renderer draws an annotation.

It compares them pairwise with two measures:
- `dpx`: the share of pixels whose grey levels differ by more than 48;
- `mad`: the mean absolute grey difference, which also catches broad
  tone differences.

Defect signals of kasane's own are tested first, so that a
disagreement between the references cannot hide them. The first
matching class wins:

| class | files | what it means |
|---|---:|---|
| blank / missing ink | 0 | kasane drew (almost) nothing, or much less ink than both references |
| extra ink | 2 | kasane drew much more ink than both references |
| annotation layer | 4 | without annotations kasane and Poppler agree; the difference is in the annotation layer, whoever is right |
| reference, kasane ≈ MuPDF | 28 | Poppler and MuPDF disagree; kasane is within half their difference of MuPDF |
| reference, three-way | 27 | Poppler and MuPDF disagree by more than kasane differs from MuPDF |
| kasane | 7 | the references agree; kasane differs from both |

A reference class says that the witnesses disagree. **It does not
clear kasane.** Clearing is the job of the **resolution check**, made
for every file: the three comparisons are repeated from 150 and 300
dpi renders, box-downsampled to the 72 dpi grid. A difference that
vanishes at a higher resolution comes from rasterisation (sampling,
anti-aliasing, stroke snapping), not from content.

The verdict per file is which reference kasane is **within tolerance**
of at 300 dpi. Within tolerance means both:
- `dpx` under 0.5 %;
- mean grey difference (`mad`) under 2 levels.

The `mad` condition keeps a broad tone difference, invisible to `dpx`,
from counting as agreement. Agreement with a witness at 300 dpi is an
observation. It is consistent with rasterisation, but it does not prove
that the 72 dpi difference had no other cause.

| class | within tolerance of Poppler | of MuPDF only | of neither | check failed |
|---|---:|---:|---:|---:|
| reference, kasane ≈ MuPDF (28) | 17 | 4 | 7 | 0 |
| reference, three-way (27) | 17 | 1 | 8 | 1 |
| kasane (7) | 2 | 1 | 4 | 0 |
| annotation layer (4) | 0 | 1 | 3 | 0 |
| extra ink (2) | 0 | 1 | 1 | 0 |
| **total (68)** | **36** | **8** | **23** | **1** |

So **44 of the 68 files are within tolerance of at least one reference
at 300 dpi**, and are most likely rasterisation differences. The check
failed for one file, whose 300 dpi kasane render exceeds the default
image limits. The **23 files outside tolerance of both references**
are what remains open:

| cause (by inspection of the measures and the file's profile) | files | status |
|---|---:|---|
| broad tone or colour difference: `dpx` ≈ 0 against a witness but a mean difference of 2–15 grey levels | 7 | undetermined; ICC, DeviceN, smooth shadings and DCT images co-occur, so colour conversion is the first suspect |
| browser-generated (Skia) pages within 0.5–0.7 % of Poppler (mean 2–3 levels) | 5 | borderline; kasane is closer to Poppler than MuPDF is |
| non-embedded font substitution (Liberation for a standard name) | 4 | kasane: metrics / shape of the substitute |
| annotation layer: kasane within 4–8 % of MuPDF while Poppler differs by 6–82 % | 3 | undetermined; which widget / appearance rendering is right is not settled |
| CCITT image | 2 | kasane or decoder: undetermined |
| JBIG2 page that both references draw wrongly (Poppler blank, MuPDF black) | 1 | reference defects; kasane draws the image |
| no font, image or colour signal (a type 1 shading co-occurs) | 1 | undetermined |

Class medians, for comparison with earlier analyses (kasane–Poppler
`dpx` at 72 / 150 / 300 dpi):
- reference, kasane ≈ MuPDF: 6.86 % / 4.39 % / 0.03 %;
- reference, three-way: 4.56 % / 1.89 % / 0.15 %;
- kasane: 1.72 % / 1.39 % / 0.73 %.

The tails are wide: the worst file of the first class keeps 32 % at
300 dpi, where kasane equals MuPDF exactly. The per-file table above is
the result; the medians are not.

Producers:
- **XFA forms from one form designer: 24 of the 55 reference-class
  files.** All carry `/XFA` and no `NeedAppearances`; kasane draws
  their AcroForm appearances as both references do.
- Browser-generated (Skia) pages: 9.

The earlier backlog (`plans/compat-backlog.md` § Fidelity) traced the
forms' hairline differences to Poppler's always-on stroke adjustment.
kasane follows §10.7.5 and adjusts only on `/SA true`. Poppler's
`-aaVector no` and `-thinlinemode shape` move the class medians by at
most 1.2 points, so the gap is not an anti-aliasing switch.

### 6.3 Failure kinds asked for

| kind | files | source |
|---|---:|---|
| near-blank render | 0 | attribution `blank` / `missing_ink` |
| missing page | 0 | every compared page rendered (pages-ok = pages-total on all 382 strict renders) |
| missing font (glyphs not drawn) | 0 | no missing-ink class; non-embedded fonts are substituted and reported (69 strict renders carry a substitution); 4 substitutions differ visibly (§ 6.2) |
| missing image | 0 | no missing-ink class; 1 image is drawn below full resolution and reported as approximate |
| colour mismatch | 0 enforced; 7 open tone / colour differences (§ 6.2) | report-only colour findings are among the gate's 7 report files; 1 render approximates an ICC profile with a Lab PCS (`/Alternate` used) |
| transparency mismatch | 0 attributable | of the 124 transparency users, 19 are below 0.95 and 9 are open; none of the 9 points to transparency (5 are the borderline Skia pages, the others tone, font or annotation cases) |
| layout shift | 0 page-box differences once the reference uses the CropBox; glyph placement matches Splash (`compat-backlog.md`) | attribution |

## 7. The past feature table, measured again

The *past* column is the earlier analysis's "related files"
(definition: files whose failure was linked to the feature at the
time). The current columns are over the 425 `expect: pass` files:
- **use**: the runner's profile or `features-v0.11.0` presence. The
  stream filters are read from the stored dictionaries, and the 5
  files qpdf cannot expand are left out;
- **blockers**: strict blockers whose root cause is the feature;
- **open**: files below 0.95 that use the feature and that still
  differ from both references at 300 dpi (§ 6.2). This is
  co-occurrence; the cause column names what the inspection found.

| feature | past | use now | blockers now (cause) | open (cause) | status |
|---|---:|---:|---:|---:|---|
| JBIG2 | 45 | 45 | 0 (2 invalid-input JBIG2 files under § 5) | 1 (both references wrong; kasane draws it) | implemented (`hayro-jbig2` 0.3) |
| annotation appearance | 42 | 167 with `/Annots` | 4 invalid (`/AS` ×2, state not a stream, non-dictionary `/Annots` entry) + 1 not implemented (Unicode field font) | 10: annotation layer 3, Skia borderline 5, font 1, tone 1 | implemented; `/AS` policy and a Unicode field font open; annotation layer undetermined |
| CIDFontType0 | 34 | 40 (38 CFF CID) | 1 (a Type 1 `/FontFile` on a CIDFontType0: invalid input) | 0 | implemented |
| ActualText | 17 | 22 with `/ActualText`; 16 yield ActualText characters on page 1 | 0 | 5 (the Skia borderline pages) | implemented (§14.9.4) |
| Optional Content | 14 | 17 | 1 invalid (undefined `/Properties` name; recovery passes) | 0 | implemented |
| tiling pattern | 9 | 17 | 1 invalid (cell cycle) | 2 (co-occur; both tone cases) | implemented |
| ASCIIHex / ASCII85 / LZW | 7 | 7 / 10 / 2 | 0 caused | 1 (co-occurs; a tone case) | implemented |
| CCITT | 4 | 3 | 0 | 2 (undetermined: kasane or decoder) | implemented (`hayro-ccitt` 0.4); quality item |
| JPX | 2 | 2 | 0 | 0 (both agree with a reference at 300 dpi) | implemented (`hayro-jpeg2000` 0.4) |
| RunLength | — | 7 | 0 caused (1 file's oversized image) | 1 (co-occurs; a borderline Skia page) | implemented |
| luminosity soft mask | — | 3 | 0 | 0 | implemented |
| knockout group | — | 3 | 0 | 0 | implemented (per element, `limitations.md`) |
| blend modes (non-Normal) | — | 19 | 0 | 0 | implemented |
| shading types 1–7 | — | 1: 3, 2–3: 27, 4–7: 1 | 1 invalid (output count) | 3 (co-occur: 1 borderline Skia page, 1 tone case, 1 undetermined) | implemented; shading is a suspect for the tone and undetermined cases |
| Type 1 CFF | — | 42 | 0 | 1 (co-occurs with substituted fonts; 25 of the 26 below 0.95 are within tolerance) | implemented |
| Type 3 | — | 37 | 0 caused (2 invalid-input files co-occur) | 5 (the Skia borderline pages) | implemented |
| XFA (static appearances) | — | 30 | 0 | 1 (a borderline Skia page) | AcroForm appearances drawn; XFA itself out of scope |

The past counts do not describe the current engine: every listed
feature is implemented, and none causes a strict failure that the
standard allows a reader to fix.

## 8. Text extraction

Strict extractions on `expect: pass`: 394 pass, 9 error, 5 unsupported
(non-embedded CID 3, non-embedded TrueType 1, image filter on a
content stream 1), 17 skip. The text errors are the render blockers'
invalid input, hit first by the text stage.

Page 1 against `pdftotext` page 1, character-multiset F1, over 394
files: median 1.0, P5 0.0, P25 1.0; below 0.9: 37, below 0.5: 33. The
baseline had 37 below 0.9 as well. The 37, each counted once, first
rule wins:

| group | files | cause | status |
|---|---:|---|---|
| annotation text | 15 | the page's text is in annotation / widget appearances; with `TextOptions::with_annotations(true)` (the runner's `text_char_f1_with_annots`) F1 ≥ 0.979 on all 15 | implemented in the API, off by default; the CLI cannot request it |
| mostly unresolved codes | 11 | more than half of page 1's characters have no Unicode (`ResolveKind::Unresolved`) | undetermined per font (below) |
| reference empty | 3 | `pdftotext` returns no text; kasane returns some | not a kasane gap |
| too small to judge, or undetermined | 8 | 5 with ≤ 10 reference characters; 2 with 16 and 34 (substituted-font pages, mapping difference undetermined); 1 ToUnicode CIDFontType2 page with ~40 % more characters than the reference (overlapping or duplicated drawing; undetermined) | undetermined |

**Unicode sources** (page 1 of the 240 files with characters):

| source | characters |
|---|---:|
| encoding fallback (§9.10.2 method 2) | 163,713 |
| ToUnicode | 119,124 |
| unresolved | 2,557 |
| ActualText | 514 |
| font program (cmap / glyph names) | 19 |

12 files have a majority of unresolved characters on page 1 and 11 have
some. The fonts in those files are mostly Type 1 without `/ToUnicode` or
`/Encoding`, and Type0 `Identity-H` or CIDFontType0 without
`/ToUnicode`. For an Identity CID font without ToUnicode, §9.10.2 gives
no method; what is left is the font program's own cmap or glyph names,
or the CID's registry and ordering. The per-font root cause is
**undetermined**: the scan is a regex over the expanded file, not the
fonts the page uses. It is a roadmap investigation, not an estimate.

**Probes** (`scripts/compat-probes.py`, synthetic, no corpus). Type 3
glyph names (`/A /B /C`, no ToUnicode) and an embedded symbolic
TrueType without encoding are extracted correctly, so they are not
gaps. A FreeText annotation's appearance text is absent from the default
extraction; Poppler and MuPDF include it. An `re` with three operands
and a form without `/BBox` are refused in strict mode; MuPDF draws
both.

## 9. Redaction corpus and external renderers

Redaction corpus, 117 files × 4 configurations (outcomes ok / refused /
unexpected):

| configuration | ok | refused | unexpected | vs baseline |
|---|---:|---:|---:|---|
| default | 65 | 50 | 2 | 0 regressions, 0 improvements |
| opt-in | 99 | 14 | 4 | 0 / 0 |
| opt-in + JavaScript | 104 | 9 | 4 | 0 / 0 |
| opt-in strict | 100 | 13 | 4 | 0 / 0 |

The default configuration's top refusals:
- region removal on a page with uncertain content: 30;
- an annotation other than a link meeting a rect: 10;
- JavaScript name tree: 3.

Each refusal is a safety rule and is not a compatibility target.

External renderer cases (`verify-renderers`): 110 cases (44 drawn, 66
refused as designed) and 132 renderer comparisons, passed.

## 10. Bottlenecks and open questions

1. **Strict compatibility is saturated on this corpus.** The gains
   left in strict mode are at most 4 files (§5), so the corpus no
   longer separates good work from bad on that measure. Its
   composition has gaps: 9 Japanese, 13 scanned and 16 Office
   `expect: pass` files. These are the areas most likely to hide
   gaps.
2. **The measurement cannot see regressions in most low-SSIM files.**
   44 of the 68 are within tolerance of a reference at 300 dpi, differences that a single 96 dpi
   witness reports as low similarity. Without a second witness and a
   resolution check, a real regression in those files cannot be told
   from noise. The 23 open files are the real fidelity list.
3. **Text.** 15 files need annotation text that the API already
   produces but the CLI does not expose, and 11 have unresolvable
   codes whose cause is undetermined.
4. **Recovery mode** draws 18 of 43 blockers. Up to 12 more are
   reachable, each an upper bound:
   - by recovery rules: operand count, `/BBox`, function inputs and
     outputs, codestream sizes, a misplaced CID font program, the
     `/AS` choice;
   - or by a decimated decode. Every recovery rule also changes what `redact`
   parses: `redact.rs`, `redact_diag.rs`, `remove_region.rs`,
   `remove_text.rs`, `redact_type3.rs` and `redact_usage.rs` pass
   `doc.recovery_mode()` to `parse_content_stream`. No redaction test
   currently opens a document in recovery mode.
5. **Unresolved**:
   - the per-font cause of the 11 + 12 unresolved-text files;
   - the 8 small or undetermined text cases;
   - the 23 files outside tolerance of both references at 300 dpi
     (§ 6.2), of which 7 tone / colour, 3 annotation-layer and 1
     without a signal are undetermined;
   - the 5 files that recovery parse cannot read (xref / page-tree
     rebuild limits).
