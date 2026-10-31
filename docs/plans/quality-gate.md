# Exact-render quality gate and quality metrics

Status: implemented in the compatibility runner and
`scripts/ci-regression.py`; enforced in the pull-request smoke run and
in the nightly against baselines that carry the gate fields (§ Roll-out,
2026-10-31). Aggregates only; per-file evidence stays in the gitignored
`compat/reports/`.

## Why

A render counted *exact* used only constructs the specification allows
and reported no approximation. That says nothing about whether the
pixels are right: the v0.4.0 analysis found Type 1 fonts drawn at the
wrong scale on pages reported exact (`v0.4.0.md` § 2). The gate compares
every exact page with an independent renderer (Poppler) and fails CI on
a *new* gross difference.

Poppler is a witness, not the truth: it ignores `/UserUnit`, leaves some
JBIG2 images blank, rounds page sizes differently and rasterises thin
strokes differently. The gate therefore uses a coarse, robust metric for
its failing rule, exempts comparisons that are not meaningful, and fails
only on changes against a reviewed baseline.

## Metrics

| metric | what it sees | limits | use |
|---|---|---|---|
| whole-page SSIM | structure over the page | margins dominate: a page with garbled text scored 0.94 | report |
| content SSIM (8 × 8 windows with ink in either image) | structure where something is drawn | rasteriser differences (anti-aliasing, hinting) put 10 % of exact pages below 0.41 and 5 % below 0.36 | report (`content_ssim`) |
| ink ratio (pixels with luma < 200, ours / Poppler's) | content missing, duplicated or at the wrong scale | sparse pages of hairlines vary with stroke rasterisation (0.5–2.0 seen) | **enforced** outside [0.4, 2.5]; report outside [0.5, 2.0] |
| pixel difference ratio, MAE | magnitude of change | position noise of ±1 px counts in full | report |
| difference at best ±3 px shift | shape vs placement | a shift hides real metric errors | report (paired with content SSIM) |
| text bounding boxes, glyph widths / positions | font metrics directly | needs a per-glyph witness (Poppler's `-bbox` is per word) | not yet — the FontMatrix fixtures check them exactly in unit tests |

## Rules (`compat/quality-gate.json`)

Population: files whose strict render passed without a reported
approximation, every page with a Poppler reference.

Exemptions, decided per page from the measurement itself (no list of
files):

| exemption | condition | why |
|---|---|---|
| `page-size` | page pixel sizes differ by > 2 % | the reference was resampled; an observed case is `/UserUnit`, which Poppler ignores and we honour |
| `reference-blank` | Poppler's page has < 0.05 % ink, ours ≥ 0.1 % | the witness drew nothing (observed: JBIG2 images Poppler leaves blank — ours are correct) |
| (below `min_ink`) | either side < 0.1 % ink | the ratio is unstable; no rule is evaluated |

1-px rounding of the page size is inside the 2 % and compared normally.

## Measurements behind the thresholds

v0.3.0 (before the FontMatrix fix), 511 files, 848 exact pages compared:

- the two visibly garbled FontMatrix pages had ink ratios 3.18 and
  2.98 (content SSIM 0.07 and 0.04);
- after the fix the same pages score 1.02 and 1.08, and every other
  page is unchanged;
- distribution after the fix over the 790 pages with ink on both
  sides: p1 0.75, p5 0.97, median 1.02, p95 1.19, p99 1.35, extremes
  0.25 and 2.00; 17 pages lie outside [0.75, 1.33]. The six outside
  [0.6, 1.6] were inspected side by side: stroke width / anti-aliasing
  of hairlines and thin borders (four — lighter or heavier than
  Poppler's), the thin rule lines of a form drawn heavier (one), a 1-px
  resampled edge on a near-empty page (one) — no misrender, but thin
  strokes are a fidelity topic of their own (`v0.4.0.md` § 2). The extremes are a hairline grid
  that Poppler draws darker (0.25) and a single thin rectangle (2.00).

So [0.5, 2.0] is too tight to enforce (two legitimate pages sit on its
edges); [0.4, 2.5] keeps a 20 % margin from every legitimate page except
the hairline grid at 0.25, and still catches both FontMatrix pages.

| code | enforced flags | report-only | exempt |
|---|---:|---:|---:|
| v0.3.0 | 3 (2 FontMatrix, 1 hairline grid) | 3 | 5 |
| after the fix | 1 (hairline grid) | 3 | 7 |

The hairline grid is a fidelity difference, not a misrender (hairline
and thin strokes are drawn lighter than Poppler draws them, see
`v0.4.0.md` § 2); it stays a known flag in the baseline.

## CI semantics

`ci-regression.py check` fails on a new silent misrender:

- a file flagged by an enforced rule that the baseline records as not
  flagged, or does not contain at all;
- a known flagged file with more flagged pages than the baseline
  records;
- a baseline holding more flagged files than `reviewed_known_flags` in
  `compat/quality-gate.json` — regenerating a baseline cannot absorb a
  new flag; raising the number is a reviewed change with its reason in
  § Known flags.

Flags already in the baseline are listed as known, not failed; a file
that stops being flagged is an improvement. Report-only rules never
fail. A baseline without the gate fields prints flags as *not enforced*.
`--summary-md` appends the counts (pages compared, exact renders,
enforced / report-only / exempt files and pages, new and known flags,
result) to the GitHub job summary — never file names.

`scripts/test_ci_regression.py` (run by the PR workflow) covers each
case with synthetic records: the FontMatrix page before (flagged) and
after (passes) the fix, the known grid and a second flagged page on it,
the reviewed-flags cap, `/UserUnit` page size and a 1-px rounding, a
blank JBIG2 reference, a near-empty page, a new misrender (changed file
and file absent from the baseline), report-only findings, approximate
and recovered renders (not judged), a pre-gate baseline, and the
summary carrying no names.

On the corpus: replaying the v0.3.0 run against the current baseline
fails exactly on the two FontMatrix files with a reference (besides
four `expect: any` stage differences that come from the `cjk-fallback`
feature, not the gate); the current run passes with the one known flag.

## Known flags

| flag | files | reason |
|---|---:|---|
| ink ratio 0.25 | 1 | a grid of hairlines Poppler draws darker; hairline / thin-stroke fidelity (`v0.4.0.md` § 2, P2). The page is otherwise right. |

`reviewed_known_flags` = 1.

## Environment

Poppler is the witness and its version differs between the nightly
(Ubuntu) and a developer machine (Homebrew). Measured on the same engine
and runner (v0.3.1) on both: our renders are identical (ink ratio of
ours Linux / macOS = 1.00 on every page), while Poppler's ink differs —
|log ratio| p99 0.09, one page by a factor 2.1 (ratio 2.05 on Linux,
1.23 on macOS); Poppler leaves three more pages blank on Linux. The
enforced flag set is the same on both. This is why the enforced band is
wide and why the full-corpus baseline is taken from the nightly
configuration.

## Roll-out

1. (done) measure the distribution on v0.3.0 and after the fix; confirm
   the FontMatrix pages are flagged before and pass after;
2. (done) the fixtures-smoke baseline regenerated with the new fields —
   the gate is armed for pull requests (no fixture is flagged);
3. (done locally) `compat/baselines/corpus-full.json` regenerated with
   the gate fields from a full run in the nightly configuration
   (`cjk-fallback`, `--mode full`) of the v0.3.1 engine and this runner,
   and checked against the Linux v0.3.1 nightly artifact (no regression,
   same enforced flag; one round-trip `full` differs because Linux
   Poppler sizes one malformed original differently — not a gated
   check). Once this code is on `main`, the first nightly's artifact
   replaces it (`ci-regression.py summarize latest.json`), keeping
   `reviewed_known_flags` unless a new flag is reviewed.

## Text agreement

`text_match_ratio` (existing) is order-sensitive: the longest common
subsequence with `pdftotext` over the longer string, white space
removed. It now compares page 1 with page 1 — it used to compare the
whole document's text with Poppler's page 1, understating every
multi-page file. New, order-independent, also page 1:

- both texts NFKC-normalised (ligatures expanded, compatibility,
  half-width / full-width and vertical presentation forms unified,
  combining sequences composed); white space (including line breaks),
  controls and default-ignorable characters (soft hyphen, zero-width
  characters, BOM) removed;
- compared as character multisets: precision = shared / ours, recall =
  shared / Poppler's, F1.

Denominators: one value per document (page 1), over files whose
strict text stage passed and that `pdftotext` read. A page with no
content character on either side scores 1 on every metric by
definition; the runner records both character counts
(`text_chars_ours`, `text_chars_ref`) and the tables report such pages
separately (164 of 427 compared documents) — quoting the share of
F1 ≥ 0.9 over all compared documents overstated it (88.7 % → 82.8 %
over `expect: pass` documents with text).

Character agreement says the right characters came out; the order score
says they came out in the same order. Neither judges which reading order
is correct — for multi-column and vertical text Poppler's order is one
choice among several.

## Per-category tables

`scripts/quality-by-category.py RUN.json` prints a table per dimension —
generator (`/Producer` family, generators with fewer than three files
pooled), script of the extracted text, font kind, font embedding, text
layer on page 1, encryption, forms / annotations / signatures,
transparency, shading, image compression, file size, page count — with
strict parse, strict render, exact, approximate, recovered, text,
rewrite, incremental save and full PASS over each category's
`expect: pass` files (the denominator `n` is printed), and robustness
(no crash / timeout) and strict refusal over its `expect: reject` files
(definitions in `compat-corpus.md` § Compatibility vs Robustness), then
the text-agreement table. Every category comes from a measurement
(the runner's content profile, its features, the file's `/Producer`);
what could not be measured is `unknown`. The nightly writes the tables
to the job summary.
