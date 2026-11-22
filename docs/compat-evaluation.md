# Compatibility evaluation: witnesses, 300 dpi re-check, regressions

How a compatibility run is judged, beyond the runner's own stage
classes and the exact-render quality gate
([`plans/quality-gate.md`](plans/quality-gate.md)). This is M1 of
[`compatibility-roadmap.md`](compatibility-roadmap.md): M1-a (a second
witness), M1-b (the 300 dpi re-check) and M1-c (numeric regressions
inside a class), with the baseline protection that goes with them.

The roadmap placed M1-a in the runner. It is implemented instead as a
stage over the runner's report (`scripts/compat-attribute.py`), which
the roadmap allowed ("logic shared or ported"). The reasons:
- the runner and its report format stay as they are, so the committed
  baselines and `release-verify` keep working;
- the stage needs only the CLI and the renderers.

## 1. The measures

| measure | where | what it sees |
|---|---|---|
| page SSIM, content SSIM | runner, against Poppler at 96 dpi | structure, overall and where ink is |
| page-1 text F1 | runner, against `pdftotext` | the characters extracted |
| `dpx` | `compat-attribute.py` | share of pixels whose grey levels differ by more than 48 |
| `mad` | `compat-attribute.py` | mean absolute grey difference (0–255): broad tone differences that stay under 48 everywhere |

`compat-attribute.py` renders the page again with kasane (the CLI
built with the runner's features), Poppler (`pdftoppm -cropbox`, the
runner's page box) and MuPDF (`mutool draw`). Every render is in RGB,
converted to grey by one formula (ITU-R 601, Pillow). The renderers'
own greyscale modes use different formulas, and that alone made MuPDF
differ from the others by 25–53 % of the pixels on coloured fixture
pages. Renders up to 1 px apart per axis (page sizes are rounded
differently) are cropped to their common size; anything larger is not
comparable.

## 2. Multiple witnesses

Every page of an `expect: pass` file is examined when:
- its strict render passed;
- its runner SSIM is below **0.95** (the threshold of the v0.11.0
  analysis).

All such pages are examined (M2). M1 examined the file's 8 worst only:
which pages those were depended on their ranks, so a fix that improved
pages pushed a page with a known difference into the set, and the check
read it as a new one (v0.11 Q1: 12 pages of one file improved, page 8
left the 8 worst, page 10 — outside both witnesses before as well —
entered them). Counted on the corpus (Linux nightly): 143 pages with the
cap, 152 without it, of the same 65 files; two files have more than 8.
`--max-pages-total` (1000) bounds the work; pages beyond it are recorded
as `truncated`, and the check fails on them.

At **300 dpi**, box-downsampled to the 72 dpi grid, kasane is **within
tolerance** of a witness when `dpx < 0.5 %` **and** `mad < 2`
(`TOLERANCE`, the values of the v0.11.0 analysis). The page's
`agreement` is one of:

| agreement | meaning |
|---|---|
| `both` | within tolerance of Poppler and of MuPDF |
| `poppler` | of Poppler only |
| `mupdf` | of MuPDF only |
| `neither` | of neither: the page is **open** |
| `not_comparable` | a render failed, timed out, wrote nothing, could not be read, or the sizes differ; `reason` says which and by whom |

`references_agree` records separately whether Poppler is within
tolerance of MuPDF. It is `None` when the page is not comparable.

No majority is taken, and no agreement proves anything:
- **Agreement between the references is not a proof that they are
  right.** Both draw a JBIG2 page of the corpus wrongly, in different
  ways.
- **Agreement of kasane with a witness at 300 dpi is an observation.**
  It is consistent with a rasterisation difference at 96 dpi but does
  not prove it.
- **`not_comparable` is never agreement.** It counts on its own, and
  in the regression check a page that becomes not comparable is a
  regression.

A descriptive class from the 72 dpi renders is recorded beside the
verdict: `blank`, `missing_ink`, `extra_ink`, `annotation_layer`,
`reference_kasane_mupdf`, `reference_three_way` or `kasane`. kasane's
own defect signals (missing or extra ink) are tested before any
reference disagreement, so that a disagreement cannot hide them.

pdf.js is not a witness here; it stays a judge of redacted outputs
(`verify-renderers.py`), where its text layer is what matters.

## 3. Regressions (`ci-regression.py check`)

Every input is validated first. A report or baseline is rejected
(exit 2, `INVALID`) and never compared when:
- it is not the expected shape (or not JSON, or empty);
- it has duplicate ids, or a key twice anywhere;
- it holds `NaN` or `Infinity`, an SSIM or content SSIM outside -1..1,
  or a text F1 outside 0..1;
- it records a page twice, or a page that is not a number from 1;
- it is a baseline whose recorded agreement its witness measures do not
  give;
- it is a baseline with page measures but no environment;
- it is an attribution of another schema or tolerance, without its
  threshold or page cap (a number from 1, or null for every page), with a page twice, with a measure missing or
  out of range (dpx outside 0–1, mad outside 0–255), or with an
  agreement that its measures do not give. The gate recomputes every
  agreement (`scripts/compat_verdict.py`) and never trusts a recorded
  label.

A file of the baseline that is missing from the run is a regression,
in every mode: a removed case is not an improvement.

The stage, round-trip, crash / timeout and quality-gate rules are
unchanged. Against a baseline that records numeric measures, measured
in the **same environment**, each of the following is a regression:

| change | threshold |
|---|---|
| a page's SSIM drops | more than 0.002 |
| a page's content SSIM drops, or is no longer measured | more than 0.01 |
| page-1 text F1 (with or without annotations) drops, or is no longer measured | more than 0.02 |
| a page compared in the baseline is no longer compared | — |
| a page within a witness's tolerance (`both` / `poppler` / `mupdf`) becomes `neither` or `not_comparable` | — |
| an open page becomes `not_comparable` (hidden) | — |
| an open page's difference from either witness grows (each witness judged on its own) | `dpx` +0.002 or `mad` +0.5 |
| a page of the run below the threshold is not attributed, or a page is attributed that is not | — |
| the attribution is of another commit or environment than the run, or of another threshold or tolerance than the baseline's | — |
| a page below the threshold that the baseline did not attribute, and that was not below it in the baseline (or is new), is `neither` or `not_comparable` (also in a file new to the baseline) | — |
| a page the baseline attributed leaves either witness's tolerance (`both` → `poppler` / `mupdf` / `neither`, `poppler` → `neither`, …), each witness on its own | — |
| the attribution has a cap per file (whatever the baseline's), or is `truncated` | — |
| the run's or the attribution's environment lacks the commit, OS, architecture, Poppler or MuPDF version | — |
| a page below the threshold in the baseline that the baseline should have attributed but did not (it is incomplete) | — |

How the rules are applied:
- **Per page, with no netting.** Each page is judged on its own; an
  improvement elsewhere (listed as `improved:`) never offsets a
  regression.
- **By (case, page), never by rank (M2).** The check builds a ledger of
  every page the baseline attributes, the run attributes, or the run
  has below the threshold — which side has it, whether it is a baseline
  target or a new candidate, why it is measured, whether it is
  comparable — and judges each record (`check --ledger FILE` writes it:
  ids, page numbers and decisions). A baseline target still below the
  threshold must be measured again and is compared with itself; one
  that rose above it is an improvement; one whose page is gone is lost
  measures (a regression). A page the baseline did not attribute is new
  when it was not below the threshold there (the baseline records
  `below`, from the unrounded SSIM): outside both witnesses, a
  regression. When it was below it already but beyond an older (M1)
  baseline's cap of 8 per file, the baseline has no witness result for
  it, so anything short of `both` now (`poppler`, `mupdf`, `neither`,
  `not_comparable`) cannot be told from a page that was `both` and lost
  a witness: it fails as `first attributed: … — unverified` until the
  baseline attributes every page. That migration is a reviewed
  `update-baseline --write --accept-unverified`, which accepts these
  first measurements (as the ledger records them, never by their text)
  and nothing else: any other regression, and a first measurement that
  is `not_comparable` (it would become a quiet gap), still needs
  `--accept-regressions`. A baseline written by M2 has no
  such pages: `update-baseline` refuses an attribution with a cap per
  file, a `truncated` one, or one that misses a page below the
  threshold. Below the threshold in a baseline that had room to
  attribute it, yet not attributed: the baseline is incomplete, a
  regression whatever the run's verdict.
- **The rules hold in every class.** They apply in every quality-gate
  class (`ok`, `report`, `exempt`, `n/a`, `flag`), so a worsening that
  stays inside its class is still caught.

**Noise.** Runs of one environment are deterministic. Three Linux
nightlies (`c593bb8`, `09a8e78`, `e7d52aa`) agree exactly on every SSIM,
content SSIM and text F1 of 1147 pages, so any move is an engine
change. The thresholds sit above the baseline's 4-decimal rounding and
below the smallest change worth a review. Text F1 drops were
report-only before (listed at 0.05); they are enforced at 0.02 because
the same runs show no noise.

**Environments.** Renders differ between platforms and renderer
versions: 230 of 1147 pages differ in SSIM between the Linux nightly and
a macOS run of the same commit. A baseline records its environment
(`scripts/compat_env.py`):
- commit;
- OS and architecture;
- Poppler, MuPDF and qpdf versions;
- features;
- the reference's fonts (`reference`): Poppler draws a non-embedded font
  with what fontconfig gives it, so the profile (`system`, or a reference
  directory's declared profile such as `courier-nimbus-mono-ps`,
  docs/courier-reference.md), the effective fontconfig configuration
  (the top-level file and every file it loads, in order, by content),
  the font files fontconfig lists (by content), the fontconfig version
  and the FreeType library Poppler loads. Paths inside a reference
  directory are recorded relative to it. Schema 2 adds the isolation
  (`system`, `inherits-system` — E1, which includes the machine's
  configuration — or `isolated` — E2, which does not), the SHA-256 of
  the lock an isolated reference was built from (checked against its
  directory), and the fontconfig library by content beside FreeType's
  (ldd on Linux; on macOS `otool -L`, `@rpath` resolved). The
  fingerprint fails closed: a `pdftoppm` that is neither a binary nor
  exactly setup-e1.sh's or setup-e2.py's wrapper of one, a binary run
  with `FONTCONFIG_PATH` or `FONTCONFIG_SYSROOT` set, an isolated
  reference whose fontconfig loads or lists a file outside it, a listed
  file that cannot be read, an empty listing, or a reference of another
  schema (an older report) leaves it incomplete, and an incomplete
  reference equals none and is refused by `update-baseline`.

The numeric measures are compared only when OS, architecture, Poppler,
MuPDF, features and the reference are equal; a side without a complete
reference fingerprint (an older report) is never equal. Otherwise:
- **without `--require-same-env`** (`release-verify` on a workstation,
  against the Linux baseline): the run prints *"numeric measures not
  compared: the environment differs …"*. The stage checks still run;
  the numeric ones are reported as not compared, never as passed. Where
  the commit has a baseline of the workstation's own platform
  (`compat/baselines/corpus-full-<os>-<arch>.json`, measured there with
  E2), `release-verify`'s `compat_corpus` runs the corpus with E2 and
  also compares it strictly with that baseline (environment, second
  witness, `--require-same-env`): its `strict` result. Without one,
  `strict` is `incomparable` — the Linux nightly remains the only strict
  comparison.
- **with `--require-same-env`** (the nightly): the difference is a
  regression. So are a run without `--env`, and a run without
  `--attribution` when the baseline has attribution. An update of the
  runner image's Poppler therefore fails the nightly until the baseline
  is measured again.

A baseline without numeric measures is reported as *"the baseline
records none"* — a note for `fixtures-smoke.json` and `release-verify`,
a regression with `--require-same-env`, so the nightly cannot pass
against an unarmed baseline.

### Reviewed differences

`compat/reviewed-differences.json` lists a few changes of **one
measure on one page** (an open page's `mad_kp` / `mad_km` / `dpx_kp` /
`dpx_km`, or a page's content SSIM) that were investigated and accepted
in a review. Each entry records the case id, the input's SHA-256, the
page, the measure and how it is taken, the environment it was reviewed
in (OS, architecture, Poppler, MuPDF, features, reference fingerprint),
the baseline value, the reviewed value, the reason and the review.

`check` still finds the regression under the original rule and prints
it as `regression under the original rule, ACCEPTED (reviewed,
limited)` — it is not a regression only when all of these hold:
the run's file has the entry's SHA-256, the run's environment equals
the entry's, the baseline value equals the entry's, and the new value
is not past the reviewed one (a `mad` / `dpx` at most it, a content SSIM
at least it). Otherwise it stays a `REGRESSION`, with a note why the
entry did not apply (another input, an input of unknown identity,
another environment, another baseline value, past the reviewed value,
evidence missing). Every other measure of the page — the other witness,
`dpx` beside `mad`, page SSIM, the agreement, a page that becomes
`not_comparable` — and every other page stay gated. An entry with
evidence missing or blank (any field, an environment field of either
side, a SHA-256 that is not one) never applies. The ledger (`--ledger`)
records the acceptance beside the page's other decisions. The baseline
itself is not changed: `update-baseline` still counts an accepted
difference as a regression (`--accept-regressions` after review), and a
baseline measured again makes an entry's baseline value stale, so the
entry no longer applies.

## 4. Baselines

Baselines change only by a person, with a commit:

```sh
# 1. A run, its environment and its attribution, from the same machine
#    (the nightly artifact `compat-full-report`: latest.json, env.json,
#    attribution.json).
python3 scripts/ci-regression.py update-baseline latest.json compat/baselines/corpus-full.json \
    --env env.json --attribution attribution.json            # dry run: the diff, counts only
python3 scripts/ci-regression.py update-baseline … --ids     # the same, with file ids (local review only)
python3 scripts/ci-regression.py update-baseline … --write   # writes it
```

`update-baseline` prints the environment and its change, the changes
per category (stage moves, quality classes, pages SSIM lower / higher /
no longer compared, agreement moves, added and removed files), and the
`check` of the run against the current baseline. With `--write` it
refuses (exit 1) when:
- files would be removed, without `--allow-removed`;
- the check finds regressions, without `--accept-regressions`;
- the environment is incomplete (exit 2): no commit, OS, architecture
  or Poppler version, or no MuPDF version with an attribution;
- the attribution was measured in another environment than the
  environment file.

Each of these flags is a reviewed decision, recorded in the commit
message. `scripts/check-workflows.py` fails on any workflow that runs
`update-baseline --write`, or that redirects, copies or moves anything
into `compat/baselines/`. `summarize` still prints a summary, for
tools; it is not the update procedure.

## 5. CI

| where | what |
|---|---|
| PR (Fast Gate), `compat-smoke` | `scripts/test_compat_evaluation.py`, with Pillow required (`KASANE_REQUIRE_PILLOW=1`): the decisions, every regression rule and every invalid input; the attribution end to end with fake renderers that fail, hang, draw nothing, draw at another size, at another resolution, shifted or in another tone |
| Full Gate, `renderers-linux` | `test_compat_evaluation.py --real`: the attribution with the real Poppler and MuPDF on committed fixtures (every page `both`, a page past the end `not_comparable`, both versions recorded) |
| nightly, `full-corpus` | `ci-regression.py env`, `compat-attribute.py` on the run, and `check --env --attribution --require-same-env` against `corpus-full.json`; `env.json`, `attribution.json` and its summary are uploaded with the report (ids and measures, no PDFs, no images); the agreement counts go to the job summary |

`scripts/check-workflows.py` requires each nightly command with its
options, in a step nothing switches off.

The corpus never reaches a pull request. The nightly reads it from the
private store, as before.

## 6. Known limitations

- `fixtures-smoke.json` records no numeric measures: the PR smoke run
  compares stages only.
- The attribution covers at most `--max-pages-total` pages (1000; 152
  today); a run with more is truncated and fails rather than passing
  unmeasured. A baseline written before M2 has no `below` flag: a page
  whose SSIM rounds to the threshold reads as not below it there, so
  its first measurement can fail as new until the baseline is updated
  (the safe side).
- A page that crosses the threshold by less than the SSIM drop rule
  (0.002) is still measured, and fails when it is outside both
  witnesses; one that stays above the threshold is screened by the SSIM,
  content SSIM and text F1 rules only, not by the witnesses.
- The witnesses are Poppler and MuPDF at the versions of the runner
  image. A defect both share is invisible here, and so is one in a
  page they both cannot draw.
- `mad < 2` and `dpx < 0.5 %` are averages over the page. A small wrong
  region on a large page can stay within them; the quality gate's tile
  rules (`displaced_ink`, `colour_tile`) look at regions.

## 7. Reproduce

```sh
python3 scripts/ci-regression.py env --features cjk-fallback > env.json
.venv-corpus/bin/python scripts/compat-attribute.py compat/reports/latest.json target/release/kasane-pdf \
    --out compat/reports/attribution.json --summary /tmp/attribution-summary.json --features cjk-fallback
python3 scripts/ci-regression.py check compat/reports/latest.json compat/baselines/corpus-full.json \
    --env env.json --attribution compat/reports/attribution.json      # on a workstation: no --require-same-env
.venv-corpus/bin/python scripts/test_compat_evaluation.py
.venv-corpus/bin/python scripts/test_compat_evaluation.py --real target/release/kasane-pdf
```
