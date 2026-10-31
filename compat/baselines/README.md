# Committed compatibility baselines

Aggregates only — never PDFs, images or logs. Produced by
`scripts/ci-regression.py summarize` from a runner report and checked
by `scripts/ci-regression.py check` (pull requests: the fixtures
smoke run; nightly: the full corpus).

| file | source run | population |
|---|---|---|
| `fixtures-smoke.json` | `kasane-compat run --corpus tests/fixtures --mode smoke --with-poppler` | the 66 deterministic fixtures under `tests/fixtures/` |
| `corpus-full.json` | the nightly job: runner built with `--features cjk-fallback` on ubuntu-latest, `kasane-compat run --corpus compat/corpus --mode full --with-poppler --dpi 96 --timeout-secs 20` | the 511-file real-world corpus (`compat/manifest.yaml`) |

## How to read the fixtures smoke result

The fixtures are not a compatibility population: most are built to
exercise one refusal path. A verdict other than `pass` is therefore
usually the *expected* outcome, and the regression check only fails
when a file gets **worse** than the baseline (a stage that was `pass`
and no longer is, a round-trip check that was PASS and is now FAIL,
any crash or timeout). Classification of the current baseline:

| class | files | meaning |
|---|---|---|
| PASS | 46 | well-formed fixture; every stage passes |
| Expected rejection | 15 | the fixture is deliberately malformed or uses a feature outside the contract, and the engine refuses it with the typed error its test asserts: `bad_offset`, `cyclic_pages`, `missing_ref`, `truncated`, `xref_stream_bad_type`, `hybrid` (strict parse), `encrypted`, `content_ops`, `contents_array`, `flate_bad_data`, `flate_contents`, `flate_predictor`, `text_missing_font_ref`, `text_no_font_tj`, `xobject_form_self_recursive`, `xobject_missing` (render) |
| Unsupported by design | 5 | a valid construct the renderer refuses rather than approximates: `flate_predictor_bad` (`/Predictor 16`), `render_annot` (annotation without appearance, subtype outside the synthesised set), `unsupported_filter` (`/JPXDecode` on a content stream), `xobject_unknown_subtype` (`/PS`) — plus `encrypted` counted above |
| Known limitation | 1 | `xref_stream_bad_type`: the strict parser refuses the file (expected), the recovery parser reads it, and the *rewrite of the recovered document* draws one qpdf warning (`Resources is missing or invalid; repairing`) that the original does not draw because qpdf cannot read the original's page tree at all. Round-trip `external_no_new` is FAIL for this file in the baseline; it is a property of repairing a broken input, not of the rewriter, and it is tracked here so the regression check keeps flagging any *new* file that behaves this way |
| Unexpected failure | 0 | — |

## Notes on the full-corpus baseline

The baseline is taken from the nightly configuration itself
(`cjk-fallback` enabled, Linux), so it must be regenerated from a
nightly artifact — not from a default-feature local run — whenever
support changes: download `compat-full-report`, then
`python3 scripts/ci-regression.py summarize latest.json > compat/baselines/corpus-full.json`.
Linux and macOS results are identical for every file except the
effect of the feature flag (the first nightly run confirmed this).

The current `corpus-full.json` (2026-10-31) is the summary of the
first nightly on the quality-gate code (run 41193210017, commit
`cb6447f`: ubuntu-26.04, poppler-utils 26.01.0, qpdf 12.3.2, runner
with `cjk-fallback`). It replaced a provisional baseline from a local
run in the same configuration (macOS, Poppler 26.10.0). Before the
replacement every difference was traced: stage, round-trip check,
robustness and incremental classes are identical; our renders are
identical (ink of ours Linux / macOS = 1.00 on every page); all
differences are in the witness —

| difference | files | cause |
|---|---:|---|
| quality `ok` → `exempt:reference-blank` | 3 (9 pages) | Poppler 26.01 draws an empty page where 26.10 draws the content (embedded CID / CFF fonts); not reproduced locally (no container), so the defect inside Poppler is not identified |
| quality `ok` → `n/a` | 1 | Poppler 26.01 produced no reference for a JPX page |
| quality `ok` → `report:ink_ratio_band` | 1 | Poppler 26.01 draws thinner strokes on a vector-only form (ratio inside the enforced band) |
| `roundtrip_full` true → false | 1 | Poppler 26.01 renders a malformed original at another page size |

No enforced flag changed: the one flagged file (reviewed) is the same
with the same page. The exemptions cost the gate 9 of 848 pages on
Linux; they cannot hide a misrender on another page. Quality-gate
flags the baseline may hold are capped by `reviewed_known_flags` in
`compat/quality-gate.json`.
With the feature, `90ms_rksj_h_sample` (`expect: any`) is classified
`error` instead of `unsupported`: its predefined CMap now loads and
the strict parser then refuses the CIDFont for lacking the required
`/FontDescriptor` — the correct outcome for that sample, recorded
here so it does not read as a regression.

Regenerated for 0.2.1 from the nightly run on the release candidate.
The previous baseline predated the recovery work; against it the
nightly reported 16 stage changes as regressions, none of which lost a
pass: five files whose strict parse fails are now also taken through
render, text and rewrite on the recovered document, so those stages
moved from `skip` to `error` (with the reason recorded); an appearance
state that is not a stream is now a structural error rather than an
unsupported feature (the file renders in recovery mode); a form
nesting deeper than `max_xobject_depth` is now reported as the limit
error. In the same run 29 stages improved (parse 444 → 446, render
423 → 440, text 446 → 458; rewrite 413 and round trip 410 unchanged,
no crash or timeout).

Regenerated for 0.3.0 from the nightly run on the release candidate
(894a2eb). Four encrypted files now parse; the writers refuse
encrypted documents, so their rewrite stage moved from `skip` to
`unsupported` — reported by the check as a change, not a lost pass.
Aggregates: parse 446 → 449, render 440 → 444, text 458 → 462;
rewrite 413 and round trip 410 unchanged; no crash or timeout.

Numbers above refer to the baseline committed alongside this file;
update both when the fixtures or the engine change
(`python3 scripts/ci-regression.py summarize /tmp/smoke/latest.json > compat/baselines/fixtures-smoke.json`).
