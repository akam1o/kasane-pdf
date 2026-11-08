# Redaction Corpus results

The last round's results of `redact` on the Corpus (page 1 of each file,
three rects: a word, its line, a central block), kept so the next round
starts from them. See [`../../docs/testing.md`](../../docs/testing.md)
(Redaction) for the procedure.

| file | what |
|---|---|
| `eval-default.json` | `scripts/eval-redact-corpus.py`, default options |
| `eval-optin.json` | the same with `--embed-substitute-fonts --remove-annotations` |
| `refusals-default.json`, `refusals-optin.json` | `scripts/analyze-redact-refusals.py` on those: every walk blocker of each refused run, by category, and the runs each category alone blocks |

Reading them: a run is `ok`, `refused` (a typed refusal, nothing
written) or `unexpected` (a success failing a check, or an untyped
failure). The one `unexpected` run, `scan-bitmap-composite-and-xnor-text`
block, is a known false signal (zeroed image samples that were already
black look like unchanged ink); its output was checked by hand. A
"lost" word can be pdftotext regrouping kept glyphs once a neighbour is
removed (per-glyph words in vertical CJK text). The category counts are
upper bounds for what supporting a category gains: a later stage may
still refuse.
