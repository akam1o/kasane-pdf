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
| content SSIM after the best shift | structure where something is drawn, a whole-page ±1–3 px offset removed | a page-wide mean: a small wrong region on a full page barely moves it | report (`content_ssim_shift` < 0.3) |
| displaced ink (worst 32-px tile, after the best shift) | our ink with no reference mark within 2 px — content drawn where the witness has none | sees what Poppler fails to draw as well | report (`displaced_ink`: tile share > 0.5, ≥ 30 px) |
| missing ink (the reverse) | reference ink with nothing of ours near | lighter thin strokes and content only Poppler draws reach 1.0 on legitimate pages | recorded, no rule |
| colour: CIEDE2000 over pixels both renders draw flat (after the best shift) | hue, chroma and lightness of filled regions | blind to colour on edges, hairlines and text thinner than 3 px; needs ≥ 64 comparable pixels | report (`colour_tile`: a 32-px tile mean ΔE00 > 15; `colour_hue`: a tile mostly ΔE00 > 10 at \|ΔL\*\| < 5) |
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

## Placement rules (report only, 2026-10-31)

The NoRotate misrender passed the gate (ink ratio 0.98, best-shift
difference 2.4 %, content SSIM 0.26 — the existing `content_ssim` rule
requires > 5 % difference). Before choosing a rule, the 8 exact pages
with content SSIM < 0.3 on the Linux nightly were classified (side by
side, also against Quartz where Poppler was in doubt):

| class | pages |
|---|---:|
| kasane-pdf misrender (NoRotate, since fixed) | 1 |
| thin-stroke rasterisation (hairline grid, thin rectangle, 0.4-pt pattern-filled boxes) | 3 |
| whole-page 1-px offset (text, a scanned image) — SSIM low at zero shift, fine at the best shift | 2 |
| image resampling of a dithered 1-bit image | 1 |
| a hairline on the page edge plus a 1-px page-height rounding | 1 |

Content SSIM alone would list 6 legitimate pages per run. Candidates
were then measured on the 791 exact macOS pages with ink (the fixed
engine), on the three historic misrenders (two FontMatrix, one
NoRotate, rendered by the old engine) and on 1 222 synthetic injections
into those same pages (a 24–64 px patch of our render moved by 40 px,
turned by 90°, or deleted):

| rule | historic bugs | moved | turned | deleted | legitimate pages flagged |
|---|---:|---:|---:|---:|---:|
| content SSIM < 0.3 | 3/3 | 7/398 | 12/412 | 9/412 | 6 |
| existing `content_ssim` (< 0.3 and diff > 5 %) | 0/3 | 0 | 0 | 0 | 0 |
| content SSIM after the best shift < 0.3 | 3/3 | 6/398 | 7/412 | 6/412 | 1 |
| displaced ink, tile > 0.5, ≥ 30 px | 3/3 | 209/398 | 147/412 | 2/412 | 3 |
| missing ink, tile > 0.5, ≥ 30 px | 2/3 | 290/398 | 104/412 | 304/412 | 6 |

The page-wide SSIM rules cannot see a small wrong region on a full
page (≈ 2 % of injections); the tile rule finds half of them (a patch
moved onto other ink is invisible to it by construction) and all three
historic bugs. Two rules were adopted, **report only**:
`content_ssim_shift` and `displaced_ink`. Missing ink stays a recorded
metric: lighter thin strokes and content only Poppler draws make it fire
on legitimate pages. Deletions are therefore left to the enforced ink
ratio (large losses) and remain a blind spot for small ones.

The legitimate pages the tile rule flagged were inspected: one where
Poppler drops a CJK glyph of an embedded OpenType CFF font that Quartz
and we draw (a witness defect), and two where we drew `.notdef` boxes
for spaces — **a real engine bug** (a CFF program with the predefined
ISOAdobe charset; glyph names were not resolved) fixed in this series
(`font: resolve glyph names through a CFF predefined charset`; 5 corpus
files carry such a font, pixels changed on 5 pages, all up). On the
full macOS run after that fix (848 exact pages): `content_ssim_shift`
fires on 2 pages (the known grid; the page-edge hairline), and
`displaced_ink` on 1 (the Poppler-dropped glyph) — 0.2 % and 0.1 %.
The runner computing the metrics reproduces the prototype exactly
(0.264 / 1.0 / 250 px on the NoRotate page).

Linux (nightly 41230437264, Poppler 26.01, 847 exact pages): the
runner measured every page; `content_ssim_shift` fires on the same 2
pages as on macOS; `displaced_ink` on 2 — the macOS page (a glyph
Poppler drops) and the vertical-CJK page, where Poppler 26.01 draws
only part of the text (macOS Poppler 26.10 and kasane-pdf agree; our
render is identical on both platforms). No new kasane-pdf defect.
The historic misrenders cannot be replayed on Linux (the nightly runs
the current engine); their detection is pinned by the metric values in
`scripts/test_ci_regression.py`. Promotion to enforced needs a period
of nightly runs with the report-only findings reviewed; not now.

## Colour rules (report only, 2026-11-01)

Every metric above compares luma; a region painted in the wrong hue
at the right lightness passed them all. The runner now also renders
Poppler in RGB (`pdftoppm` without `-gray`, same page, resolution and
crop; the file is deleted after use) and compares colours
(`compat/runner/src/colour.rs`):

- 8-bit sRGB → linear (IEC 61966-2-1) → XYZ (sRGB matrix, D65) →
  CIE L\*a\*b\* (white = the matrix row sums, white is exactly L\* 100)
  → CIEDE2000 (k = 1). All 34 pairs of Sharma, Wu & Dalal (2005) are
  reproduced to 5 · 10⁻⁵; the conversion matches colour-science 0.4.7
  to 10⁻³. Our premultiplied RGBA is composited on white first.
- Pixel selection: both renders *draw* there (a channel < 245) and both
  are *flat* there (3 × 3 luma range ≤ 24) after the best placement
  shift — the interiors of shapes, not anti-aliased edges. A pixel more
  than ΔE00 2 off is compared once more with the reference's 3 × 3
  neighbourhood (a 1-px boundary offset); a wrongly filled region stays
  wrong, its neighbours are wrong too. Disjoint shapes share no pixel
  and give no statistic rather than a false colour error.
- Per page: compared pixels, median / p95 / p99, share > 5, share > 10,
  share > 10 at |ΔL\*| < 5, worst 32-px tile mean, hue tiles (more than
  half the tile's compared pixels > 10 at |ΔL\*| < 5). `None` below 64
  comparable pixels. Histogram percentiles (0.1 steps); memory is the
  two RGB buffers of the page.

Population: exact renders, and renders whose only approximations are
font substitutions (they change glyph shapes, not colours). Colour and
image approximations (ICC / CMYK fallbacks, reduced JPEG decoding) stay
out; the existing exemptions (blank reference, page size) apply first.

Calibration (`kasane-compat --colour-probe`, macOS, 96 dpi, page 1 of
every file that renders strictly): 303 pages, 174 with colour
statistics, 90 with a 48-px patch colourful in both renders, into which
one error was injected at a time.

| rule | hue rotation | R↔B swap | CMYK black dropped | desaturated | exact pages hit (154) | font-only pages hit (19) |
|---|---:|---:|---:|---:|---:|---:|
| p95 > 10 | 50/90 | 49/90 | 28/90 | 50/90 | 3 | 2 |
| tile mean > 10 | 89/90 | 80/90 | 39/90 | 86/90 | 3 | 3 |
| **tile mean > 15 (`colour_tile`)** | 88/90 | 75/90 | 37/90 | 84/90 | 2 | 3 |
| tile mean > 20 | 88/90 | 72/90 | 16/90 | 75/90 | 2 | 3 |
| **hue tiles ≥ 1 (`colour_hue`)** | 21/90 | 26/90 | 1/90 | 36/90 | 1 | 0 |

A dropped black is mostly a lightness error at low chroma and is the
hardest case; `colour_hue` is deliberately narrow (equal lightness) and
reads as a qualifier on `colour_tile`, not as a detector of its own.

Calibration itself found the page-group / blend-mode bug (two files;
fixed before the full run). Every hit on the full corpus after that fix
(macOS, 1 142 compared pages, 564 with colour statistics; 7 files) was
inspected against Poppler and Quartz and the specification:

| class | files | finding |
|---|---:|---|
| kasane-pdf bug, fixed | 1 | a text object inside a Type 3 glyph moved the outer text (now drawn correctly, and refused for `max_pattern_tiles`) |
| DeviceCMYK conversion | 1 | ours is the §10.3.5 conversion; Poppler and Quartz use colour-managed CMYK. Not a defect (no OutputIntent, no profile) |
| Poppler defect or choice | 4 | a zero-width function `/Domain` (legal, `min ≤ max`) Poppler rejects and paints black; out-of-range `/Indexed` values Poppler paints black where §8.6.6.3 clamps (the file's own reference row agrees with us); a pattern phase on text Poppler offsets (Quartz agrees with us); an image Poppler smooths for `/Interpolate`, a hint we do not apply |
| rasterisation | 1 | 0.12-pt dotted strokes without stroke adjustment: Poppler and Quartz widen them to a device pixel; we draw their true coverage |

On exact renders `colour_tile` fires on 4 files (CMYK 1, Poppler 3) and
`colour_hue` on 1 (CMYK); on font-only renders on 2 (Poppler 1,
rasterisation 1). None is a kasane-pdf defect.

Cost (macOS, full corpus): colour comparison 63.5 s of 603 s (median
32 ms, p95 87 ms per page, mostly the second Poppler render; worst
7.6 s on a page Poppler takes 7.5 s to draw); runner total +12 %
(537 → 603 s summed per file). Peak RSS of a single heavy page was
unchanged (2 042 vs 2 065 MB, the engine dominates); the RGB buffers
are 6 bytes per pixel at 96 dpi (≈ 3 MB for a Letter page).

Not enforced: the Linux nightly must first run the rules (Poppler 26.01
draws some patterns and images differently) and every finding be
reviewed for a period, as for the placement rules.

## Blind spots

- Small deletions (a missing glyph or icon on a full page): ink ratio
  barely moves, the shifted SSIM is a page mean, missing ink is too
  noisy to rule on.
- A misplaced element moved onto other content (no unmatched ink).
- Colour on thin features (text, hairlines, edges): only flat regions
  of at least 3 × 3 px are compared. A wrong text colour is invisible
  to the colour rules.
- A colour error shared by both engines (both naive, or a witness
  defect we reproduced) cannot be seen against a witness.

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

The first nightly on this code (ubuntu-26.04, poppler-utils 26.01.0 vs
26.10.0 locally) confirmed it on the v0.4.0 runner: 847 pages compared
(848 locally), the same single enforced flag, 4 report-only files (3),
10 exempt files on 16 pages (7 on 7) — the extra exemptions are pages
Poppler 26.01 leaves blank, and one JPX page it does not render
(`compat/baselines/README.md`). Linux ink-ratio distribution over 780
pages with ink on both sides: p1 0.79, median 1.02, p99 1.39, extremes
0.25 (the known grid) and 2.05 — the closest legitimate page to the
enforced 2.5. Pinning Poppler would need a source build in CI; the
witness version is recorded in the baseline notes instead, and the
baseline is regenerated when the runner image changes it.

## Roll-out

1. (done) measure the distribution on v0.3.0 and after the fix; confirm
   the FontMatrix pages are flagged before and pass after;
2. (done) the fixtures-smoke baseline regenerated with the new fields —
   the gate is armed for pull requests (no fixture is flagged);
3. (done) `compat/baselines/corpus-full.json` from the first Linux
   nightly on this code, after a file-by-file comparison with the
   provisional local baseline (all differences in the witness, none in
   an enforced rule); `reviewed_known_flags` unchanged at 1; the
   nightly re-run against it enforces the gate.

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

### With annotation text (v0.5.0, report only)

`pdftotext` draws annotation appearances into the page text; the
default extraction does not report them, so a page whose text lives in
form fields scores 0 against a reference that has it.
`text_char_f1_with_annots` is the same F1 with the text drawn by each
visible annotation's normal appearance added to ours
(`TextOptions::with_annotations`), counted once per annotation as Poppler
draws it — Popup, Hidden and NoView annotations draw nothing; each
widget of a field draws its own appearance. Field values and
`/Contents` are counted by source (`text_with_annots`:
`appearance_chars`, `contents_chars`, `field_value_chars`) but never
added: a field's value is normally also drawn by its appearance, and a
note's `/Contents` is not drawn, so adding either would count text twice
or credit text the reference does not contain. `text_char_f1`, its
denominators and every gate are unchanged. Population: the same
documents as `text_char_f1`, with content characters on either side
after the addition; a page whose extraction with annotations fails (a
malformed appearance) is counted, not scored (`text_with_annots.error`).
A higher score is agreement with Poppler, not proof of correctness: the
two engines generate missing appearances differently (a list box without
`/AP`: Poppler draws every option, kasane the selected values).

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
