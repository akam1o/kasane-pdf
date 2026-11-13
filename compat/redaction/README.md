# Redaction Corpus results

The last round's results of `redact` on the Corpus (page 1 of each file,
three rects: a word, its line, a central block), kept so the next round
starts from them. See [`../../docs/testing.md`](../../docs/testing.md)
(Redaction) for the procedure.

Each file is the **projection** of a result
(`scripts/redact-results-projection.py IN OUT`): the manifest id, the
run's kind and category, its outcome, named counts, and for refusals
the reason and walk categories from a closed vocabulary (kasane's
refusal features and error kinds, the analysis script's categories;
anything else is `other`). The raw results — the rectangles (derived from the
page's words), refusal details and walk errors (they quote PDF names) —
are never committed; run the projection before committing a new round.
`scripts/check-redact-corpus.py` compares a run with these
(`nightly.yml`); `ci/`, when present, holds the projections measured on
the CI runner, whose Poppler and MuPDF shift word and pixel counts.

| file | what |
|---|---|
| `eval-default.json` | `scripts/eval-redact-corpus.py`, default options |
| `eval-optin.json` | the same with `--embed-substitute-fonts --remove-annotations` |
| `eval-optin-javascript.json` | the same with `--remove-javascript` added |
| `eval-optin-strict.json` | the same with `--refuse-uninspected` added (the strict mode) |
| `refusals-default.json`, `refusals-optin.json` | `scripts/analyze-redact-refusals.py` on those: every walk blocker of each refused run, by category, and the runs each category alone blocks |

Reading them: a run is `ok`, `refused` (a typed refusal, nothing
written) or `unexpected` (a success failing a check, or an untyped
failure). The `unexpected` runs — `scan-bitmap-composite-and-xnor-text`
block and `ccitt_endofblock_false` block, and with the opt-ins
`blendmode` block and `alphatrans` block — are the known false signal:
zeroed image samples that were already black (or grey under a soft
mask) look like unchanged ink. Each is checked mechanically
(`check_by_hand` true, `unchanged_ink_not_from_images` 0): with no image of
the output drawn, no unchanged ink stays inside the rect. A
"lost" word can be pdftotext regrouping kept glyphs once a neighbour is
removed (per-glyph words in vertical CJK text). MuPDF characters
"moved" are its integer-rounded widths (Poppler places those glyphs
identically); the 6 MuPDF characters "lost" are Type 3 codes whose
advance cell meets the rect (removed by design). The category counts are
upper bounds for what supporting a category gains: a later stage may
still refuse.
