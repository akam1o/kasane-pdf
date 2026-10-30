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

Numbers above refer to the baseline committed alongside this file;
update both when the fixtures or the engine change
(`python3 scripts/ci-regression.py summarize /tmp/smoke/latest.json > compat/baselines/fixtures-smoke.json`).
