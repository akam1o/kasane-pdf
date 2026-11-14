# Testing & verification

## Layout

Tests are organised by **feature**, not by when they were written — see
[`../tests/README.md`](../tests/README.md) for the authoritative map
of `tests/*.rs` → the area it exercises and where to add a new case.

- `tests/parse_*.rs`, `render_*.rs`, `text_*.rs`, `rewrite_roundtrip.rs`,
  `edit_annotations.rs` — engine-level integration tests.
- `tests/cli_*.rs` — CLI integration tests, one file per subcommand
  group (`rewrite`, `annotate`, `inspect`, `preflight`, `remove`,
  `audit`/multi-page/metadata, `remove_jpeg`).
- `tests/common/mod.rs` — shared CLI helpers (`bin`, `tmpdir`,
  `fixture`, `minimal_pdf`, `blocker_page_pdf`).
- `tests/fixtures/` — hand-crafted, deterministic PDFs
  (`make_fixtures.py`) plus a few byte-pinned binaries.

## Running

```sh
cargo test                 # unit + integration tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Fixture tiers

Two fixture regimes are kept distinct:

- **Deterministic fixtures** (`tests/fixtures/`): small, generated
  PDFs for specific features/edge cases, committed to Git, run on
  every `cargo test`.
- **Real-world corpus** (`compat/corpus/`): ~100+ PDFs produced by
  real applications (arXiv/LaTeX, IRS forms, veraPDF, pdf.js test
  files), fetched from a user-configured S3-compatible store (or a
  local directory) behind `compat/manifest.yaml` — never committed.
  See [`compat-corpus.md`](compat-corpus.md).

## External verification

The engine's own tests never depend on another PDF implementation,
but external tools are used to *confirm* behaviour on real files:

- [`../scripts/verify-rewrite.sh`](../scripts/verify-rewrite.sh) —
  `qpdf --check` on the rewrite output of every well-formed fixture;
  requires exit 0 and zero warnings.
- [`../scripts/verify-audit.sh`](../scripts/verify-audit.sh) —
  `audit-residuals` findings cross-checked against `qpdf`.
- [`../scripts/verify-resources-materialize.sh`](../scripts/verify-resources-materialize.sh)
  — inherited-`/Resources` materialisation checked with
  `qpdf`/`pdftotext`/`pdftoppm`/`pdfimages`.
- `pdfimages`, `pdftoppm`, `djpeg` — used in the JPEG / grid-aligned-CTM verification to
  confirm extracted samples and rendered edit positions; the clone's
  decoded bytes are verified in-process so decoder differences are
  not confused with real edits.

### Redaction

- [`../scripts/verify-redact.py`](../scripts/verify-redact.py) —
  synthetic cases (including tagged documents, whose output's structure
  tree is read with qpdf: present, every reference resolving, the
  untouched elements' text kept), each successful output checked with `qpdf`
  (structure, and every decoded stream searched for the secret text
  and image samples), `pdftotext` and `pdftoppm`; refusals as declared.
- [`../scripts/eval-redact-corpus.py`](../scripts/eval-redact-corpus.py)
  — the same three rects (a word, its line, a central block) on page 1
  of each Corpus file, with and without the opt-ins
  (`--redact-arg --embed-substitute-fonts --redact-arg
  --remove-annotations`), `--json` for the per-run results. A success
  is checked for words still extracted inside the rect, unchanged ink
  inside it, words lost and pixels changed outside it; character by
  character with MuPDF (`mutool draw -F stext`, coordinates scaled back
  from `/UserUnit`): no character extracted inside the rect, each kept
  character found again at its origin (lost and moved counted apart
  from words pdftotext only regrouped); an image reported edited must
  not leave every page image unchanged (`pdfimages -all`); each run's
  time is recorded. A redactor that copies its input is flagged on
  every run whose rect holds ink. Known false signals, checked by hand
  rather than relaxed: zeroed image samples that were already black
  count as unchanged ink (reported `unexpected`, marked
  `check_by_hand`, then checked mechanically: rendered again with no
  image drawn — each image XObject an empty form — no unchanged ink may
  stay inside, so it is all image samples, which `redact` verifies are
  zero inside the rect; `unchanged_ink_not_from_images`); a word "lost" outside can be pdftotext joining a
  kept glyph to its neighbour once the glyph between them is removed
  (per-glyph words in vertical CJK text); MuPDF rounds simple fonts'
  widths to integers, so a kept glyph after removed ones on its line
  "moves" by up to a few hundredths of a point in MuPDF's reading only
  (Poppler places it identically); a Type 3 code is removed when its
  advance cell meets a rect, so MuPDF can count a character whose ink
  lies just outside as lost.
- [`../scripts/analyze-redact-refusals.py`](../scripts/analyze-redact-refusals.py)
  `<kasane-pdf> <eval.json> [--redact-arg …] --json out.json` — for
  each refused run, every blocker of the redaction walk (through the
  undocumented `redact --explain`), grouped into categories, with the
  number of runs each category alone blocks (an upper bound for what
  supporting it gains: a later stage may still refuse). Results of a
  round are kept in [`../compat/redaction/`](../compat/redaction/) as
  projections (`scripts/redact-results-projection.py`: ids, categories,
  counts — never the rectangles or refusal details).
- [`../scripts/bench-redact.py`](../scripts/bench-redact.py)
  `<kasane-pdf> [<kasane-pdf> …] [--runs N]` — fixed redaction timing
  benchmark: generated inputs (400/800/1,600 fonts, 200 pages in one
  font or 1,600 font dictionaries, 5,000-entry resources, forms nested
  15 deep, 50 pages of 20,000 shows, 4,000 annotations), one rect on
  page 1, best of N sequential runs per binary.
- Reader agreement on literal strings: `tests/literal_strings.rs`
  (TrueType, Type 1, Type 0 and Type 3 pages with raw and escaped
  end-of-lines; CMap codes as strings; object strings); the same
  outputs were compared with Poppler, MuPDF and pdf.js by rendering
  and extraction.
- Independence of the output from what is removed: build two inputs
  that differ only in the data meeting the rects, redact both, require
  byte-identical outputs (this rules out any route — interpolation,
  anti-aliasing, a kept operand — by which removed data could reach a
  reader); check the harness with a control that keeps some varied data.
  The image and path cases were run this way; the results are in the
  redaction docs' known limits.
- [`../tests/redact_security.rs`](../tests/redact_security.rs) — that
  check as a regression suite: text under nested `cm` and a form
  `/Matrix`, removed kerns, `Tm`/`Td` positioning, `/ActualText`
  `/Alt` `/E`, Type 3 programs, clipped and optional content, image
  samples (Gray, RGB with `/Interpolate`, CMYK, ICCBased, Indexed, in a
  form), `/SA` and hairline strokes, a shading and a tiling pattern,
  an annotation appearance, soft masks, Separation / DeviceN images, the
  structure tree's text of touched elements (`/Alt`, `/ActualText`,
  `/E`, `/T`, indirect, on an ancestor, a tagged link); refusals must
  be alike for both inputs.
- [`../tests/redact_structure.rs`](../tests/redact_structure.rs) — the
  structure tree after `redact`: which elements lose their text and
  attribute strings, `/ParentTree` and `/K` still resolving, shared
  elements, `/IDTree`, forms, annotations, malformed and large trees.
  `verify-redact.py`'s `structure-*` cases read the output's tree with
  qpdf; the same outputs were read with pdf.js (`getStructTree`) and
  MuPDF during development.
- [`../tests/redact_inventory.rs`](../tests/redact_inventory.rs) — the
  inventory of uninspected data and `refuse_uninspected`.

### Redaction: generated documents and advisories (0.10)

- `tests/redact_model.rs` (generator: `tests/model/mod.rs`) builds
  documents in two variants that differ only in what lies inside the
  rect on page 1 — text (visible, invisible, under an optional content
  group that is off), image samples, glyph procedures, appearance
  drawings, alternate text, annotation text (of hidden annotations
  too), form field values, a thumbnail, the path of a filled shape
  inside the rect — and filled shapes, an axial shading band and a
  tiling fill crossing it, with
  shared and nested forms, inherited resources, annotations, form
  fields, a structure tree, Type 3 fonts, references to secret objects
  from places that draw nothing, identical copies, dangling,
  wrong-generation and cyclic references, object streams and incremental
  updates. Both variants must redact to the same bytes (or be refused
  alike), and a success must keep every public text. 160 seeds by
  default; `KASANE_MODEL_SEEDS=N` for more. Equal bytes cover at once
  what is drawn, the extractable text, every object and the reference
  graph. What the model does not establish: secrets of a kind or in a
  place the generator does not produce (it is not a proof over all
  PDFs); data that differs outside the rect (kept by design, and data
  kept objects carry whole, such as a cut shading's function, is
  identical in both variants by construction); how other readers draw
  the output (the external checks and the advisory tests do that).
  Passing it is evidence for the verification, not a proof that every
  copy of the removed content is gone.
- `fuzz/fuzz_targets/redact_model.rs` drives the same generator from the
  fuzzer's bytes: a false success fails like a crash (PR smoke, nightly).
- `tests/redact_advisories.rs`: one test per published advisory;
  `KASANE_ADVISORY_DUMP=dir` writes its inputs for
  `scripts/check-advisory-regressions.py`, which judges any CLI with
  qpdf and pdftotext (PR workflow, `--expect-safe`). Results per version:
  `docs/security/advisory-regressions.md`.

## CI gates

Three levels, by when they run; the security checks of redaction are in
the first.

| gate | when | what |
| --- | --- | --- |
| **fast** (`pr.yml`) | every pull request, and every push of the full gate | `cargo fmt`; clippy in each feature set; docs without warnings; the engine tests with nextest and the doctests in the default features and with `cjk-fallback` (the library's unit tests and doctests also without default features); `compat/runner` tests; wasm32 build; `cargo audit`; the crate package and every target of the extracted crate compiling; the fixture compatibility + round-trip smoke against its baseline; **`scripts/verify-redact.py` (qpdf, pdftotext) and the advisory regressions (`tests/redact_advisories.rs` judged by `scripts/check-advisory-regressions.py --expect-safe`)**; the redaction model (`tests/redact_model.rs`, 160 seeds); the fuzz smoke in four parallel jobs (each render target's seeds replayed with the paint report and 3000 new inputs; `redact_model`'s 2,800 seeds replayed and 3000 new inputs, in eight shards run four at a time in two jobs); the R2 depth and stack tests |
| **full** (`pr.yml`) | pushes to `main` and `release/**`, `workflow_dispatch` (the release gate) | the fast gate, plus MSRV (`cargo +1.92 check`), `cargo semver-checks` against the newest release, the redaction model at 10,000 seeds (release build), `cargo publish --dry-run` and the packaged crate's own tests (nextest, doctests, docs) |
| **corpus and long fuzz** (`nightly.yml`, `fuzz-nightly.yml`) | nightly on `main`; dispatched on a release commit | the 511-file compatibility run and its baselines; the redaction corpus (117 runs × four option sets, `scripts/eval-redact-corpus.py`) compared with `compat/redaction/` by `scripts/check-redact-corpus.py`; 15 minutes of fuzzing per target from the accumulated corpus |

`gate` (the last job of `pr.yml`) passes only when every job of its
level ran and succeeded (`scripts/ci-gate.py`): every job of its
`EXPECTED` list must report a result, and a failed, cancelled or
timed-out job, a missing or unknown result, or a job it does not expect
fails it; a full-gate job may be skipped only on a pull request. `gate`
runs `if: always()`, so it is never skipped while its run exists;
cancelling a job it needs fails it (an `always()` job still runs when
the run is cancelled, and passes only if everything it needs had
already succeeded).

Required checks on `main` (docs/releasing.md, *Branch protection*):
`test / fmt / clippy`, `compat + round-trip smoke (fixtures)`, `gate`
and `workflow guard`. `pr.yml` runs on `pull_request`, so it is the
pull request's own copy of the workflow: a pull request could edit
`gate` or `ci-gate.py` to pass. `workflow guard` (`pr-guard.yml`, on
`pull_request_target`) closes that for forks: it runs the **base
branch's** definition with a read-only token, reads the pull request's
workflows and `ci-gate.py` from git objects as data (never checked out,
never run), and checks them with the base branch's
`scripts/check-workflows.py --data-only`; and it fails a pull request
**from a fork** that changes a CI path (`CI_PATHS`: `.github/`,
`scripts/`, `tools/`, the fuzz runners, `.config/`, `.cargo/`, the
baselines in `compat/` (corpus, redaction, renderers),
`tests/redact_advisories.rs`) — its CI results would come
from what it changed. Paths are compared as git writes them with `-z`
and `--no-renames` (unusual names, and a file renamed out of a CI path,
count). The maintainer lands such a change from a branch of their own
after review (or merges it as administrator, docs/releasing.md).

The trust model: a branch of this repository (write access) is trusted
to change the CI; the static checks catch an accidental weakening, not
a determined one — they check the workflows' structure and commands,
not what a changed script does. A fork is not trusted: it cannot change
the CI paths, and its workflows are checked by the base branch's
definition. `pull_request_target` must be allowed for `pr-guard.yml` by
the repository's Actions policy (GitHub blocks it by default in public
repositories from 2026-12-02); if it is not, the guard does not run
and, being required, blocks every merge (docs/releasing.md, *Branch
protection*).

`scripts/check-workflows.py` (run by the fast gate and by the guard)
keeps the workflows so: each required check is the name of exactly its
job (`REQUIRED_JOBS`: `pr.yml`'s `engine`, `compat-smoke` and `gate`,
`pr-guard.yml`'s `guard`) and of no other job of any workflow (`.yml`
or `.yaml`), and no name built from an expression can produce one — a
second job of the same name could answer for it; `gate` runs
`always()`, needs every job of `pr.yml` and runs exactly
`python3 scripts/ci-gate.py` with the job results, unconditionally;
`ci-gate.py`'s `EXPECTED` (read as data) is exactly `pr.yml`'s jobs and
its `FULL` exactly the jobs pull requests skip — so a new job left out
of `gate` or of `EXPECTED` fails; `pull_request` is unfiltered (no
paths or branches leave a pull request without `gate`); the commands of
the security checks, the engine tests, the doctests and the fuzz smoke
run on every pull request, in the repository root, in steps no
condition switches off, on lines that are not printed, commented out or
made to pass (`|| true`); every job has a timeout and none continues on
error; the workflow runs on pull requests, pushes to `main` and
`release/**`, and dispatch; every cache is written from `main` only
(pull requests restore `main`'s and cannot write one it reads); the
guard runs on `pull_request_target` only, with `contents: read` only,
has no condition on its job or steps, checks out only the base branch
(full history, no credentials) with `actions/checkout` pinned, uses no
other action, and runs both of its checks.

`scripts/test_ci_scripts.py` (fast gate) tests these decisions: the
gate over every job, result and event, and missing, unknown and
unexpected results; each weakening of the workflows the check must
catch (a printed or `|| true` command, a duplicate `gate` in another
workflow or a `.yaml` one, `pr.yml` replaced, each weakening of the
guard); the guard's fork decision on the paths git reports for renames
and unusual names; `scripts/release-check.py`'s decision (only a
successful full gate, CodeQL and — when required — a nightly whose
corpus job really ran, on the exact commit); and
`fuzz/ci-run-sharded.sh`'s failures (a failing or killed shard, no
seeds, an empty shard, bad arguments) and its dealing of every seed.
The redaction corpus results leave the runner only as projections (ids,
categories, counts).

### Release verification

The part of the release gate that CI cannot run on every push — the
private corpora (redaction 117 × 4, compatibility 511), the external
renderers, 900 s of fuzzing per target, the 10,000-seed model with the
security regressions — runs locally with `scripts/release-verify.py run
--ref <sha>`, which records each check against the commit (full SHA and
tree, the verifier's and config's git blobs, tool versions, commands,
exit status, duration, peak memory, log digests), and
`scripts/release-check.py --local-results DIR` refuses a release whose
evidence is missing, of another commit, partial, failed, edited or
superseded (docs/releasing.md, *Local release verification*).

`scripts/verify-renderers.py` is its renderer check: every
verify-redact case, every advisory input (F, G, H, I and earlier) and
synthetic XFA, shading, tiling-pattern and widget-appearance cases,
redacted and then rendered and text-extracted by Poppler, MuPDF and
pdf.js (`tools/pdfjs`, pinned). Ink left inside the rect, the secret in
the extracted text or the public text lost fail always; changed pixels
outside the rect are classified (anti-aliasing, edge snap, font
substitution, changed) and only `changed` is compared, with
`compat/renderers/baseline.json` and only for the renderer versions it
was made with.

`scripts/test_release_verify.py` (fast gate) runs the verifier on
throwaway repositories and checks each way its evidence can be wrong:
another commit or tree, an older run superseded by a newer one, edited
or truncated JSON, a missing check, a forged status, a missing or
changed log, a run killed half-way, a timeout, a crash, a missing tool,
another renderer version, an incomplete corpus, private text in the
public projection, paths and symbolic links out of the results, the
worktree changed during a check, R's and S's evidence swapped.

### Advisory history and Linux

`scripts/advisory-history.py` runs every advisory input on every
published version (docs/security/advisory-regressions.md): each version
built from its crates.io package, checked against its git tag, with
toolchain 1.92 and the default features; affected versions must LEAK,
the candidate must be removed, refused or kept as
`compat/advisories/history.json` expects (a refusal where removal is
expected fails, and the reverse), and each advisory's affected range is
checked against what is observed. Text cases are judged by Poppler,
MuPDF and pdf.js — text extracted and ink left inside the rect — never
by kasane's own reading. A build that fails, a package that is not its
tag, a cached binary whose digest changed, a crash, a timeout or a
witness that fails is never LEAK, removed or refused.

Where each runs:

| check | inputs | when | platform |
| --- | --- | --- | --- |
| advisory regressions on the candidate (`--expect-safe`), the history and baseline scripts' own tests | synthetic | every pull request (fast gate) | Linux (CI) |
| external renderers on synthetic cases, against `baseline-linux-x86_64.json` | synthetic | full gate (pushes to `main`, `release/**`, dispatch) | Linux (CI) |
| advisory history: every published version and the candidate | synthetic | nightly and dispatch (`nightly.yml`) | Linux (CI) |
| the release profile (corpora, renderers, fuzz, model, regressions) | private corpus + synthetic | before a release (`release-verify.py`) | the maintainer's machine |
| `security-history` with private reproduction material | private + synthetic | before a security advisory (`release-verify.py --profile security-history --private-inputs`) | the maintainer's machine |

Private inputs never reach GitHub Actions: the corpus jobs fetch the
corpus only in `nightly.yml`'s gated job and upload projections, and
`advisory-history.py` refuses `--private-inputs` when `GITHUB_ACTIONS`
is set. `scripts/test_advisory_history.py` (fast gate) checks the
judgement on fake CLIs and hand-made packages: a failed build, a
package unlike its tag, an unexpected feature or toolchain, missing or
unknown inputs, an old version not leaking, the candidate leaking, a
crash, a timeout, a missing output, an unexpected refusal, a changed
binary, another platform's baseline, a baseline written over a safety
failure or with unaccepted increases, private names in the public
summary.

What a pull request's green fast gate guarantees: the change compiles
and passes every engine test in both feature sets, every redaction
security check (the 35 verify-redact cases, every advisory's regression
inputs judged by tools that share no code with kasane, the generated
non-interference model, the fuzz smoke of the redaction model), and the
fixture compatibility smoke. What it leaves to later gates, and why:

| check | runs | if it fails after merging |
| --- | --- | --- |
| MSRV, semver-checks | full gate, on the merge commit | `main` is red before any release; fix forward (a release requires a green full gate on its commit) |
| redaction model at 10,000 seeds (160 on pull requests) | full gate | as above; the failing seed is printed and reproduces locally |
| the packaged crate's tests (pull requests compile every target of it) | full gate | only fixtures read at run time and missing from the package are not caught earlier; fix the `include` list |
| redaction corpus, 511-file compatibility | nightly (credentials; not on pull requests) | the nightly fails the next morning; the release gate requires it green on the release commit |

## Compatibility suite

`compat/runner` (`kasane-compat`) runs the real-world corpus through
parse / render / text / rewrite, classifies each file
(`pass`/`mismatch`/`unsupported`/`error`/`crash`/`timeout`/`skip`),
and emits JSON + Markdown reports. With `--with-poppler` it
cross-checks page count, page-1 render MAE, and text-match against
`pdftotext`/`pdfinfo`/`pdftoppm`. See
[`compat-corpus.md`](compat-corpus.md).
