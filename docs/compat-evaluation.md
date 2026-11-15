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
  analysis);
- it is among the file's 8 worst such pages.

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
- it is not the expected shape;
- it has duplicate ids;
- it holds `NaN` or `Infinity`;
- it is an attribution of another schema;
- it is a baseline with page measures but no environment.

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
| an open page's difference from the nearer witness grows | `dpx` +0.002 or `mad` +0.5 |
| a page attributed in the baseline is not attributed, while still below the threshold | — |
| a page below the threshold that the baseline did not attribute is `neither` or `not_comparable` (also in a file new to the baseline) | — |

How the rules are applied:
- **Per page, with no netting.** Each page is judged on its own; an
  improvement elsewhere (listed as `improved:`) never offsets a
  regression.
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
- features.

The numeric measures are compared only when OS, architecture, Poppler,
MuPDF and features are equal. Otherwise:
- **without `--require-same-env`** (`release-verify` on a workstation,
  against the Linux baseline): the run prints *"numeric measures not
  compared: the environment differs …"*. The stage checks still run;
  the numeric ones are reported as not compared, never as passed.
- **with `--require-same-env`** (the nightly): the difference is a
  regression. So are a run without `--env`, and a run without
  `--attribution` when the baseline has attribution. An update of the
  runner image's Poppler therefore fails the nightly until the baseline
  is measured again.

A baseline without numeric measures (`fixtures-smoke.json`, and
`corpus-full.json` before it was armed) is reported as *"the baseline
records none"*.

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
- Pages past the 8 worst of a file below the threshold are not
  attributed.
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
