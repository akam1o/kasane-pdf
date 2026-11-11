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
  round are kept in [`../compat/redaction/`](../compat/redaction/).
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

### Redaction: generated documents and advisories (0.9)

- `tests/redact_model.rs` (generator: `tests/model/mod.rs`) builds
  documents in two variants that differ only in what lies inside the
  rect on page 1 — text, image samples, glyph procedures, appearance
  drawings, alternate text, annotation text, form field values — with
  shared and nested forms, inherited resources, annotations, form
  fields, a structure tree, Type 3 fonts, references to secret objects
  from places that draw nothing, identical copies, dangling,
  wrong-generation and cyclic references, object streams and incremental
  updates. Both variants must redact to the same bytes (or be refused
  alike), and a success must keep every public text. 160 seeds by
  default; `KASANE_MODEL_SEEDS=N` for more.
- `fuzz/fuzz_targets/redact_model.rs` drives the same generator from the
  fuzzer's bytes: a false success fails like a crash (PR smoke, nightly).
- `tests/redact_advisories.rs`: one test per published advisory;
  `KASANE_ADVISORY_DUMP=dir` writes its inputs for
  `scripts/check-advisory-regressions.py`, which judges any CLI with
  qpdf and pdftotext (PR workflow, `--expect-safe`). Results per version:
  `docs/security/advisory-regressions.md`.

## Compatibility suite

`compat/runner` (`kasane-compat`) runs the real-world corpus through
parse / render / text / rewrite, classifies each file
(`pass`/`mismatch`/`unsupported`/`error`/`crash`/`timeout`/`skip`),
and emits JSON + Markdown reports. With `--with-poppler` it
cross-checks page count, page-1 render MAE, and text-match against
`pdftotext`/`pdfinfo`/`pdftoppm`. See
[`compat-corpus.md`](compat-corpus.md).
