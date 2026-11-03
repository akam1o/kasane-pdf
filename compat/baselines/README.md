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
| PASS | 47 | well-formed fixture; every stage passes (`hybrid` since the cross-reference fix: its table and its own `/XRefStm` disagree about the page, and the table — searched first, ISO 32000-2 §7.5.8.4 — names it) |
| Expected rejection | 14 | the fixture is deliberately malformed or uses a feature outside the contract, and the engine refuses it with the typed error its test asserts: `bad_offset`, `cyclic_pages`, `missing_ref`, `truncated`, `xref_stream_bad_type`, `encrypted`, `content_ops`, `contents_array`, `flate_bad_data`, `flate_contents`, `flate_predictor`, `text_missing_font_ref`, `text_no_font_tj`, `xobject_form_self_recursive`, `xobject_missing` (render) |
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
effect of the feature flag (the first nightly run confirmed this) and
the witness — and, measured again for 0.5.0, our renders' ink on 47
pages of 19 files, by at most 0.064 % (anti-aliasing; the same pages in
0.4.0 and after the cross-reference fix).

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
with the same page.

Updated 2026-11-01 from nightly run 41230437264 (commit `41215fe`,
same environment) to add the semantic round trip per category and the
report-only rule set per file. Nothing else changed per file except two
quality classes `ok` → `report:…`, both witness defects reviewed side by
side: a CJK glyph of an embedded OpenType CFF font that Poppler does
not draw (Quartz and kasane-pdf do) — `displaced_ink`; and vertical CJK
text that Poppler 26.01 draws only partly (26.10 locally draws it like
kasane-pdf, whose render is identical on both platforms) —
`displaced_ink` and `content_ssim_shift`. Report-only findings never
fail; recording them lets `check` list *new* ones. Semantic round trip:
413 compared, 189 pass, 224 not applicable, 0 fail / unsupported /
error — identical to the macOS run. The exemptions cost the gate 9 of 848 pages on
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

Updated 2026-11-01 from nightly run 41241175195 (commit `eb1a4a9`,
`/OCProperties` carried by the rewriter). Per file, exactly the 14
documents that were refused for `/OCProperties` changed: rewrite
`unsupported` → `pass`, round trip full, semantic round trip `pass`
(optional content included); 13 of them change verdict to `pass`
(full PASS 325 → 338), one keeps its verdict for an unrelated render
error. No page's pixels, text, quality class or report-only finding
changed; robustness 44/44, 0 crash / timeout.

Updated 2026-11-01 from nightly run 41254250037 (commit `b6f8aef`:
blend-mode page group, HardLight, Type 3 text state, stroked text,
exact tiling cell range, the CIEDE2000 colour rules). Per file:

- one `expect: any` document is now refused with `max_pattern_tiles`
  (render `pass` → `error`) — an intended refusal: it was drawn wrongly
  before (a Type 3 glyph's nested text moved the outer text; the
  stroke width was wrong); drawn correctly its glyph-space tiling
  pattern needs 4 866 cell executions (counted exactly) against the
  default 4 096. Verified independently (pattern space §8.7.2, cell
  range, the limit's unit = cell executions);
- six documents carry new report-only colour findings (a new
  measurement, not a change of output): DeviceCMYK conversion 1,
  Poppler differences 4 (zero-width function domain, out-of-range
  Indexed values, pattern phase on text, `/Interpolate`),
  rasterisation of 0.12-pt dotted strokes 1 — classified in
  `docs/plans/quality-gate.md` § Colour rules.

No other stage, verdict, text, round-trip or warning change; pixels
changed on 26 pages of 20 documents, all intended (blend-mode pages,
stroked text, the transparency group).

Updated 2026-11-01 from nightly run 41267856113 (commit `589dc83`:
text without ToUnicode, annotation text, the security fixes, and the
witness now installed with `poppler-data`). Per file, only quality
fields changed, all on the witness side (no page of ours changed):
three Japanese documents that Poppler 26.01 left blank without the CJK
data (exempt `reference-blank`) are now drawn and pass the gate
(content SSIM 0.95-0.99); on the vertical-writing document the
`displaced_ink` report (Poppler drawing only part of the text)
disappears, `content_ssim_shift` stays. With the same witness data the
text agreement is now identical on Linux and macOS (page-1 F1 >= 0.9
on 213 of 250 `expect: pass` documents with text; CJK 17 / 17).

Updated 2026-11-01 from nightly run 41315396310 (commit `984548b`:
the cross-reference precedence fix X1 and the v0.5.0 phase A runner;
same environment as the 0.4.0 nightly — ubuntu, Poppler 26.01.0 with
`poppler-data` 0.4.12, qpdf 12.3.2). Every per-file difference against
the previous baseline was classified before the update:

| difference | files | cause |
|---|---:|---|
| semantic round trip: five document-level categories recorded | 428 | new measurement (phase A); no earlier category changed on any file |
| `text_f1` / `text_f1_annots` recorded | 455 / 450 | new fields: page-1 F1 and the annotation F1, report only (`check` lists a drop ≥ 0.05, never fails on it) |
| rewrite `error` → `pass`, verdict → `pass`, round trip full, warning gone | 1 | X1: the objects its xref stream declares at generation 2 now resolve (it was refused as a dangling reference) — verified against qpdf |
| round trip `annotations` not applicable → pass | 1 | X1: the page and catalog of the newest revision are read (an older hybrid section's `/XRefStm` overrode them); the page's annotations are now visible — verified against qpdf |
| render approximate, quality `ok` → `n/a` (3 pages leave the exact-render gate), warning gone | 1 | X1: an appearance stream declared at generation 1 resolves; it uses a non-embedded font, drawn with its declared substitute, so the render is a reported approximation and the gate no longer judges it |

Aggregates: rewrite 427 → 428, round trip full 424 → 425, quality
pages 847 → 844; every other count unchanged; no enforced flag,
report-only finding, stage, verdict or round-trip check changed
otherwise. Robustness 44 / 44, crash / timeout 0. The fixtures smoke
baseline is regenerated from the pull-request run on the same commit
(41315385267): `hybrid` now passes every stage (parse, incremental
`error` / `skip` → `pass`; its page enters the exact-render gate,
`ok`), and the fields added since the smoke baseline was last written
(report-only rules, semantic categories, text F1) are recorded.


Updated 2026-11-02 from nightly run 41377950881 (commit `f921627`, the
merge of v0.5.0 phase B: W1 references to undefined objects, W3 XFA
carried; same environment as the previous update). Population unchanged
(511 files, 425 `expect: pass`). Compared file by file with the previous
nightly (41315396310): exactly 7 files changed, all in the rewrite
stage and its round trip; text, pixels, quality classes, report-only
findings, warnings, recovery, incremental and robustness are identical
on every file.

| difference | files | cause |
|---|---:|---|
| rewrite `error` → `pass`, verdict → `pass`, round trip full, semantic round trip pass | 4 | W1: the dangling reference is written as a reference to an indirect null |
| rewrite `error` → `pass`, round trip full, semantic not applicable; verdict stays `error` | 1 | W1; render and text still fail on a font resource that is not a dictionary |
| rewrite `error` → `pass` on the recovered document, semantic pass, `external_no_new` fails; verdict stays `error` (`expect: reject`) | 1 | W1 on a damaged file: qpdf warnings 9 → 0; Poppler, now able to read past the broken cross-reference table, reports the fonts that point at the 4 undefined objects |
| rewrite `error` → `unsupported` | 1 | W1 lifts the dangling reference; the next refusal is a `/Names /JavaScript` tree (D2) |

No change comes from W3 (the 30 XFA and usage-rights forms stay
refused for `/Perms /UR3` or a JavaScript tree) or from the XFA
signature check, and none is specific to Linux. The Linux / macOS
differences (109 render pages, text on 1 file, 1 full round trip) are
the same as before the merge. Aggregates: rewrite 365 → 370, Full PASS
339 → 343, graph round trip 428 → 434, full round trip 425 → 430;
semantic round trip 434 compared, 300 pass, 134 not applicable, 0 fail
/ unsupported / error; robustness 44 / 44, crash / timeout 0. The
fixtures smoke baseline is unchanged: the pull-request run on the same
commit (41377935675) summarises to it exactly.

Updated 2026-11-03 from nightly run 41480198364 (commit `9962fd0`, the
merge of v0.5.0 W2: PDF 2.0 documents are rewritten as 2.0, and the
runner reports four report-only PDF 2.0 semantic categories; same
environment: ubuntu x86_64, Poppler 26.01.0 with `poppler-data`
0.4.12, qpdf 12.3.2). Population unchanged (511 files, 425
`expect: pass`). Compared file by file with the previous nightly
(41400411935, commit `617444a`; the scheduled nightly 41433320638 on
the same commit is identical to it):

| difference | files | cause |
|---|---:|---|
| rewrite `unsupported` → `pass`, verdict → `pass`, round trip full, semantic pass in every category the document has | 2 | W2: the two documents that declare PDF 2.0, refused before for their version; qpdf exit 0, Poppler SSIM 1.0 |
| `semantic_report` recorded | 436 | new field: the report-only categories of every compared rewrite |

Every other field of every file is identical: stages, verdicts,
round-trip checks, the existing `semantic` map, text and annotation F1,
render values, quality classes, enforced flags and report-only
findings, warnings, recovery and incremental classes. Aggregates:
rewrite 370 → 372, Full PASS 343 → 345, `rewrite_pass` (graph round
trip) 434 → 436, full round trip 430 → 432; robustness 44 / 44, crash /
timeout 0.

Report-only semantic categories (not enforced; `check` lists a lost
pass for review):

| category | pass | fail | unsupported | error | not applicable |
|---|---:|---:|---:|---:|---:|
| version | 434 | 2 | 0 | 0 | 0 |
| page_metadata | 1 | 0 | 0 | 0 | 435 |
| associated_files | 2 | 0 | 0 | 0 | 434 |
| dparts | 0 | 0 | 0 | 0 | 436 |

The two `version` fails are recorded as fails, deliberately. Both
inputs declare 1.4 but carry `/OCProperties`, a 1.5 feature, and the
rewriter raises the output to 1.5 so that it declares what it contains.
The effective version changes, and the category reports that
(`plans/v0.5.0.md` §16 proposes classifying such a permitted
transformation separately before any of these categories is
enforced). The fixtures smoke baseline is regenerated from the
pull-request run on the same commit (41480171819; the previous one
came from 41377935675): it
differs only by the new `semantic_report` field (61 fixtures: version
pass, the other three not applicable).

Updated 2026-11-03 from nightly run 41571647011 (commit `67c01d9`, the
merge of v0.5.0 V4: the runner's strict action comparison, the
report-only `multimedia` and `signatures` categories, the
`transformed` class with the version rule T1, figures by rewrite
configuration; the library is unchanged; same environment). Population
unchanged. Compared field by field, every file, with the previous
nightly 41497592731 (commit `ad76fda`), which summarises exactly to the
previous baseline. Process ids in temporary file names and the output
directory were normalised.

| difference | files | cause |
|---|---:|---|
| `semantic_report.multimedia` recorded (not applicable) | 436 | new report-only category |
| `semantic_report.signatures` recorded (435 not applicable, 1 pass) | 436 | new report-only category |
| `semantic_report.version` `fail` → `transformed`, `semantic_rules` = T1 | 2 | the two inputs that declare 1.4 and carry `/OCProperties`; the rewriter raises them to 1.5, and T1 now accepts exactly that change |
| `rewrite_refusal` recorded (run report only) | 40 | new field: 27 signature, 8 script, 5 other unsupported |

Every other field of every file is identical:
- stages, verdicts and round-trip checks;
- the existing `semantic` map, with every category's class, items and
  notes (`outlines` lines change form, `{#n …}`, but no class or count
  changes);
- text and annotation F1;
- per-page render values, quality classes and findings;
- warnings, recovery and incremental classes.

Aggregates are unchanged:
- Parse 408, Render 380, Text 392, Rewrite 372;
- Full PASS 345 / 425;
- graph round trip 436, full round trip 432;
- semantic 302 pass, 134 not applicable;
- robustness 44 / 44, crash / timeout 0.

The summary adds `by_config` (only `default`, which reproduces these
figures; `transformed` 0 in the enforced map, never counted as pass) and
per-file `semantic_rules`.

The fixtures smoke baseline is regenerated from the pull-request run on
the same commit (41571589747). It differs only by the two new
report-only categories (61 fixtures, not applicable) and `by_config`.

With this update `check` lists a semantic move into `fail`, `error`
or `unsupported` from any recorded class, and a lost `transformed`.
Before, only a lost pass was listed, so once a baseline records
`not_applicable` a structure *added* by the rewrite would have gone
unlisted. It is still not enforced.
