# Courier reference environments (evaluation, not a gate)

The Courier substitute (Kasane Courier, #58, released in 0.12.0)
changes what kasane draws for non-embedded Courier and Courier New.
Whether that is "better" depends on what the reference draws for the
same fonts. When this evaluation was made, the nightly's Poppler drew
them with the runner's own fonts, Liberation Mono — the face #58
replaces (E0 below). This page separates the reference environment from
the implementation; the 2×2 and its results are that evaluation, made
before the nightly moved.

Today the nightly's quality comparison (the 511-file corpus, the second
witness and the strict check against `compat/baselines/corpus-full.json`)
uses E1 (*The nightly on E1*, below). The redaction corpus in the same
job, and the external-renderer and Courier redaction checks of the full
gate (`scripts/verify-renderers.py`, `scripts/verify-courier-redaction.py`),
keep the system's fonts: what they check — that nothing redacted is
drawn or extracted, that readers draw the embedded program — does not
depend on which face a reader picks for a non-embedded Courier.

## Environments

- **E0** — the nightly runner (`ubuntu-latest`, Ubuntu 26.04 x86_64)
  with the nightly's verifiers (`qpdf poppler-utils poppler-data
  python3-pip mupdf-tools python3-pil`): Poppler 26.01.0, MuPDF 1.27.0,
  fontconfig 2.17.1, FreeType 2.14.2, 53 font files (DejaVu, Lato,
  Liberation, Noto Color Emoji, Ubuntu console). fontconfig resolves
  Courier to Liberation Mono (its metric aliases), and the Courier New
  PostScript names Poppler asks for to DejaVu Sans Mono.
- **E1** — E0 with Poppler, and nothing else, resolving Courier and
  Courier New to **Nimbus Mono PS** (Regular, Bold, Italic, BoldItalic;
  URW base35, from the pinned `fonts-urw-base35 20200910-8build1`
  package — downloaded, checked by SHA-256 and unpacked into a directory,
  never installed): `scripts/courier-reference/setup-e1.sh` writes a
  `fonts.conf` (a private cache, that directory, a strong preference for
  Nimbus Mono PS for the family names Poppler asks for Courier and
  Courier New, tested before the system rules, then the system
  configuration) and wrappers running the Poppler tools with it. MuPDF
  (built-in Nimbus Mono PS) and pdf.js (its Foxit Fixed) are unchanged;
  kasane reads no fontconfig.

`probe.py` and `reference-diff.py` refuse to run when E0 would not be
the system's Poppler (E1's wrappers first on PATH, or FONTCONFIG_FILE
set); `run-2x2.sh` refuses a source tree with changes and records, per
cell, the implementation's and the harness's commits and E1's
configuration hashes. `courier-reference.yml` records both on the runner (`env.py`: packages,
every font file's SHA-256, E1's configuration and fonts) and runs
`probe.py`: for probe fonts — the four Courier styles, Courier New
names, Helvetica, Times, Arial, Times New Roman, Symbol, ZapfDingbats,
other fixed-pitch and proportional names — the file fontconfig gave
Poppler, read from its trace (not `fc-match`), and the pixels, in E0 and
E1. It fails unless Courier and Courier New, and only they, change, each
to its style's Nimbus Mono PS file, and E0's font list is untouched.

## The 2×2

| | E0 | E1 |
|---|---|---|
| **A** — main with #57, Liberation Mono for Courier | today's reference | the reference changed alone |
| **C** — #58, Kasane Courier | today's comparison | the implementation in the new reference |

Each cell is `scripts/courier-reference/run-2x2.sh`: the nightly's corpus
run (`--with-poppler --dpi 96 --timeout-secs 20 --mode full
--rewrite-options carry`) and second-witness step
(`compat-attribute.py`), with E1's Poppler first on PATH for those
processes only. `compare-2x2.py` gives the three deltas;
`reference-diff.py` renders every page with Poppler in E0 and E1 and
compares the pixels; `compare-synth.py` runs the synthetic cases
(`synth.py`) against the three references.

The cells were run in an Ubuntu 26.04 container with the runner's
packages and font files (identical SHA-256 to the runner's `env.py`
record, the same E1 configuration hash) on aarch64. That container
reproduces the nightly: A in E0 against the CI nightly of main — every
file's stage classes and text F1 identical, 1,033 of 1,148 strict-render
pages with identical SSIM and the rest within 0.0005, the second
witness's summary identical; C in E0 against #58's CI nightly — the
same; and against a baseline built from A/E0 in the same container,
C/E0 gives exactly the 21 regressions that nightly reported.

## Results (implementations `19a2d00` A, `2e3a518` C; harness `8c342cd`)

**Reference pixels, E0 → E1** (`reference-diff.py`, all 511 files):
Poppler draws every page of 487 files in both environments (1,777 pages,
their pdfinfo page counts reached, status 0) and no page of the other 24
in either, with the same status in both (14 exit 1, 8 exit 99, one
killed by a signal 9, one crashing with signal 11) — files the nightly
has no Poppler reference for either (none has an SSIM in the runner's
report); they are not compared, and not counted as such. Of the 1,777
pages, 11 of 6 files change: the 9 pages of the 5 files kasane draws
with its Courier substitute, and 2 pages of a file kasane rejects (no
render to compare). Every changed file lost a Liberation Mono or DejaVu
Sans Mono face for Nimbus Mono PS and gained no other font (fontconfig's
trace per file, not per page); the other 1,766 pages are pixel-identical.

| delta | pages SSIM better / worse / same | content SSIM better / worse | classes, text F1, crashes, timeouts, missing |
|---|---|---|---|
| A/E0 → A/E1 (reference) | 0 / 8 / 1140 (worse by 0.006–0.070, median 0.050) | 0 / 9 | none changed |
| A/E1 → C/E1 (implementation, new reference) | **8** / 0 / 1140 (better by 0.004–0.052, median 0.024) | **8** / 0 | none changed |
| A/E0 → C/E0 (implementation, today's reference) | 0 / 8 / 1140 (worse by 0.005–0.059, median 0.041) | 0 / 8 | none changed |

Second witness (pages below SSIM 0.95, re-checked at 72 and 300 dpi
against Poppler and MuPDF): E0 → E1 turns four pages of A from
`reference_three_way` to `kasane` (the references now agree; Liberation
Mono is the outlier); in E1, C turns two of them back to
`reference_three_way` and one page from `kasane` to
`reference_kasane_mupdf`. Against an E1 baseline built from A/E1, C/E1
has no regression and 8 improvements; against an E0 baseline built from
A/E0, C/E0 has today's 21.

Synthetic (25 cases, 96 and 150 dpi: 23 in Courier or Courier New and
two controls — another fixed-pitch font and Helvetica — that A and C
draw identically), C against A, SSIM of the text area, differences
within 0.001 counted as the same: MuPDF and pdf.js
better on 22–23 cases, worse on none, in both environments (their
references do not depend on E0/E1); Poppler worse on 23 in E0 and better
on 22 in E1, worse on one — an invalid-name case where the references
disagree with each other (−0.006 / −0.003; MuPDF better there).

## What it does not settle

- Nimbus Mono PS is one Courier clone, not Adobe's Courier; E1 measures
  agreement with it, as E0 measures agreement with Liberation Mono.
- The evaluation ran on aarch64 against an x86_64 runner; the
  reproduction above bounds the difference.

## The nightly on E1

The nightly's reference is E1. The environment fingerprint
(`scripts/compat_env.py`, docs/compat-evaluation.md § Environments)
includes the reference — profile, effective fontconfig configuration,
font files, fontconfig and FreeType versions — so an E0 run is never
compared with the E1 baseline as the same environment.

`compat/baselines/corpus-full.json` was measured again from the
nightly's x86_64 run of the implementation without Kasane Courier in E1
(run 44675455806, commit `f6ddcdd`). It differs from the E0 baseline in
what E1 changes, and nothing else:
- the environment (commit, reference);
- the 9 pages of the 5 Courier files: SSIM and content SSIM, all lower
  (kasane's current Courier substitute is closer to Liberation Mono
  than to Nimbus Mono PS);
- the second witness's Kasane–Poppler measures of the 4 such pages it
  had (MuPDF's unchanged), and one more page below 0.95 attributed
  (agreement *neither*).

The rejected file's 2 Poppler pages are in no baseline (kasane renders
none of it). The 24 files Poppler draws no page of in either environment
are compared in neither.

What bounds E1 and what does not:
- The wrappers clear `FONTCONFIG_PATH` and `FONTCONFIG_SYSROOT` and set
  `FONTCONFIG_FILE`; everything else (the system configuration E1
  includes, a user configuration it includes in turn, fontconfig's
  caches) is inherited. The fingerprint is taken with the same
  environment, so it covers what fontconfig then loads and lists, not
  what a different environment would.
- `reference-diff.py` attributes changes per file, by the faces Poppler
  reports: a changed file must lose a Courier substitute and only such
  faces, and gain Nimbus Mono PS and nothing else. That is evidence the
  change is Courier's, not a proof for every pixel of the file.
