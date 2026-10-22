# Real-world PDF compatibility corpus

kasane-pdf measures engine maturity against real, in-the-wild PDFs —
not just the hand-crafted `tests/fixtures/`. The corpus lives in an
**S3-compatible object store** (Cloudflare R2, AWS S3, MinIO,
Backblaze B2, …) and is downloaded on demand; PDFs are never committed
to git, and no endpoint, bucket, or credential is hard-coded in the
repo.

- Layout: `<prefix>/<category>/<sha256>.pdf` (prefix defaults to `pdfs`)
- Manifest: [`../compat/manifest.yaml`](../compat/manifest.yaml) —
  the single source of truth (id, sha256, category, source, license,
  `expect` for the Compatibility/Robustness split). The manifest names
  sources and licences but **no** endpoint or credentials.

## Configuration

Copy [`../compat/corpus.env.example`](../compat/corpus.env.example) to
`.env` (git-ignored) or export the variables:

| variable | meaning |
|---|---|
| `KASANE_CORPUS_ENDPOINT` | S3 endpoint URL for your store |
| `KASANE_CORPUS_BUCKET` | bucket holding the corpus objects |
| `KASANE_CORPUS_PREFIX` | object-key prefix (default `pdfs`) |

Back-compat: `R2_ENDPOINT`/`CLOUDFLARE_R2_ENDPOINT`/`R2_BUCKET` (and
generic `AWS_ENDPOINT_URL`/`S3_BUCKET`) are also honoured.

## Credentials

Credentials resolve through the **standard AWS/boto3 provider
chain** — nothing is stored in the repo:

- `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, or
- `AWS_PROFILE` → `~/.aws/credentials`, or
- an IAM role / instance-metadata / SSO.

Scope the token to the corpus bucket only.

## No store? Use a local directory

The runner reads a plain directory of PDFs. Build one from files you
already have (matched to the manifest by sha256):

```console
$ .venv-corpus/bin/python compat/fetch-corpus.py seed \
    --from /path/to/my/pdfs --dir compat/corpus
```

…or run the runner straight at any directory:

```console
$ compat/runner/target/release/kasane-compat run \
    --corpus /path/to/my/pdfs --out compat/reports
```

## Setup

```console
$ python3 -m venv .venv-corpus
$ .venv-corpus/bin/pip install boto3 pyyaml pillow
```

## Commands

```console
# list manifest entries
$ .venv-corpus/bin/python compat/fetch-corpus.py list

# download everything → compat/corpus/
$ .venv-corpus/bin/python compat/fetch-corpus.py download

# add a local file (computes sha256, uploads, appends to manifest)
$ .venv-corpus/bin/python compat/fetch-corpus.py add file.pdf \
      --category office --source <url> --license <tag> [--pages N]

# add straight from a URL
$ .venv-corpus/bin/python compat/fetch-corpus.py add-url <url> \
      --category latex --source <url> --license <tag>

# re-hash local files against the manifest
$ .venv-corpus/bin/python compat/fetch-corpus.py verify
```

## Running the compatibility suite

Build once:

```console
$ cargo build --manifest-path compat/runner/Cargo.toml
```

Run the whole corpus (writes `compat/reports/run-<ts>.{json,md}` +
`latest.*`):

```console
$ compat/runner/target/debug/kasane-compat run \
      --corpus compat/corpus --out compat/reports --with-poppler
```

- `--with-poppler` — cross-check page count, render page 1 with
  `pdftoppm` (MAE vs our raster), and text-match with `pdftotext`.
  Requires `pdfinfo`, `pdftoppm`, `pdftotext`, `qpdf` on PATH.
- `--dpi N` (default 96), `--max-pages N` (default 3),
  `--only <id>` for a single file.

Re-render a saved result without re-running:

```console
$ kasane-compat report compat/reports/latest.json --format md|html
```

Diff the current run against a previous one (regression analysis —
newly PASS/FAIL files, verdict changes, cause-set and MAE deltas):

```console
$ kasane-compat report compat/reports/latest.json \
      --compare compat/reports/run-<prev-ts>.json
```

## Compatibility vs Robustness

The report splits the corpus into two independent rates, driven by a
per-file `expect:` tag in `manifest.yaml`:

| `expect` | meaning | feeds |
|---|---|---|
| `pass` | well-formed document the engine should handle | **Compatibility** rate |
| `reject` | adversarial / malformed / fuzzed / encrypted — must be *refused safely* | **Robustness** score |
| `any` | valid doc using out-of-scope features | neither rate |

- **Compatibility** = per-stage pass rate over the `pass` files.
- **Robustness** = a `reject` file is OK when the engine refuses it
  *without crashing or hanging* (a classified `Error`/`Unsupported`);
  only `CRASH`/`TIMEOUT` fail the robustness check. A `reject` file
  that happens to render is still "safe" — refusal is the goal, not
  mandatory failure.

Tag files by *document intent*, never by the engine's current result —
a well-formed PDF our parser can't read yet is `expect: pass` (a
compatibility gap), not `reject`.

## ExtGState usage audit

`kasane-compat run --corpus … --gs-audit` walks every corpus PDF and
prints which `/ExtGState` keys appear, on how many files, and which gs
names are actually `gs`-invoked — the data behind the phased
implementation decision in `docs/decisions/`.

## Font usage audit

`kasane-compat run --corpus … --font-audit` (`runner/src/fontaudit.rs`)
enumerates every `/Font` resource per PDF — split by `/Subtype`,
embedding (`/FontFile`/`/FontFile2`/`/FontFile3` in the descriptor),
`/Encoding`, `/ToUnicode`, `/Widths` — and marks which fonts are
actually `Tf`-invoked (vs merely present in resources). Add `--only ID`
to dump one file's fonts. This drove the Type1 implementation split:

| program | status | corpus hit |
|---|---|---|
| `/FontFile3` raw CFF (`/Type1` + `/MMType1`) | **implemented** — outlines via `ttf_parser::cff`, glyph ids via CFF charset + `/Encoding` | few (`pattern_text_embedded_font`, parts of `tamreview`) |
| non-embedded Type1 (Standard-14-style) | **metrics-only** — `/Widths`+encoding feed text extraction (`bounds`→`None`); render refuses | ~21 files |
| `/FontFile` Adobe Type 1 (eexec) | **implemented** — `src/type1.rs` eexec-decrypts + interprets the charstring VM (paths, hints, subrs, flex, `seac`) | ~7 files (the arXiv family) — all *also* blocked by `/Annots` + non-embedded + a `bfrange` bug, so eexec alone unblocks 0 renders today |
| `/FontFile3` CIDFontType0C | **refused** — CID font, not simple Type1 | — |
| `/Type3` (`/CharProcs` streams) | **implemented** — each glyph runs like a mini Form XObject under `ctm×trm×/FontMatrix`; `d0`/`d1` parsed; `in_text` reset allows nested `BT…ET` (Type3-in-Type3) bounded by `max_xobject_depth` | 4 files (2 now pass; 2 blocked on `/Pattern`) |

Measured after Type1 + Type3 (vs the 13/67 render, 17/67 text,
13 full-PASS ExtGState baseline): render **15/67**, text **25/67**,
full PASS **15** — zero regressions.

Measured after eexec (`/FontFile`) Type 1 (run-1792678862): the
`render.font.type1-eexec`/`text.font.type1-eexec` cause is gone from
all 7 affected files and `attention-is-all-you-need` progressed past
font load to a separate `bfrange` ToUnicode `Structure` bug — but
**0 new PASSes**, because every eexec file is *also* blocked by
`/Annots` (render+text) and non-embedded fonts. eexec was never the
sole blocker; the other 6 remain `/Annots`-blocked before fonts are
reached. Correctness is carried by the `src/type1.rs` unit tests and
the real `eexec_cmsy10.bin` fixture.

## Failure-cause analysis

Every stage failure is also classified into a **stable cause id** of
the form `<stage>.<category>[.<specific>].<class>` — e.g.
`render.extgstate.unsupported`, `parse.eof.error`,
`rewrite.named-dest.unsupported`. The id is derived from
`Error::kind()` plus the engine's `feature:` tag, **never** from
free-text messages, so rewording an error string can't re-bucket the
count. Unknown errors still produce an id (`*.unknown.*`) rather than
being dropped.

A read-only **probe** (`runner/src/probe.rs`) walks each page's
content ops and `/XObject` / `/Font` / `/ColorSpace` / `/ExtGState`
resources and records *every* blocker it would hit — not just the
first one the engine returns. This powers three distinct per-feature
counts in the report:

| count | meaning |
|---|---|
| **affected** | PDFs the feature appears on at all |
| **sole-blocker** | PDFs where it is the *only* known blocker |
| **potentially unblocked** | PDFs that could newly pass if it's implemented — an **estimate**, since the probe only sees `--max-pages` pages and doesn't recurse into Form XObject streams |

The **Development priorities** table ranks features by
`potentially_unblocked / effort`, where `effort` is a person-day
*guess* in `analysis.rs` (`PrioritiesConfig`) — shown as an estimate,
never a measured value. The probe deliberately does **not** change
engine failure behaviour; it only enumerates what *would* block.

## Result classification

Each file gets one verdict — the **worst** class across its stages
(crash > timeout > error > unsupported > mismatch > pass > skip):

| class | meaning |
|---|---|
| `PASS` | stage produced the expected result |
| `MISMATCH` | succeeded but output differs from the reference |
| `UNSUPPORTED` | a documented unimplemented feature (typed `Unsupported`) |
| `ERROR` | parse/structure/limit failure or reparse failure |
| `CRASH` | panic / abnormal exit |
| `TIMEOUT` | exceeded `--timeout-secs` |
| `SKIP` | could not run (e.g. earlier stage failed) |

Unsupported features are **never counted as success** — the report
keeps `UNSUPPORTED` separate so "we can't do it" is visible, not
hidden inside a pass rate.

## Render execution vs visual fidelity

A page can *execute* (no error) yet draw the wrong thing, so the runner
keeps two independent measures:

- **Render Execution Success** — the `render` stage class: did
  `render_page` return without `Unsupported`/`Error`/`Crash`?
- **Render Visual Fidelity** — when `--with-poppler` is on, every
  rendered page is compared to `pdftoppm`'s reference at the same DPI
  (grayscale, nearest-resampled to match size):

  | metric | meaning |
  |---|---|
  | `render_ssim` | Structural Similarity (Wang et al.), 8×8 windows, stride 4 — 1.0 = identical |
  | `render_mae` | mean absolute luminance error, 0.0–1.0 |
  | `render_diff_px_ratio` | fraction of pixels differing by >8/255 |
  | `render_pages_ok/total` | per-page render success |

  A file **meets the fidelity bar** when its worst page has SSIM ≥ 0.98
  *and* diff-pixels ≤ 2% *and* MAE ≤ 0.05 — loose enough to absorb
  legitimate anti-aliasing/rasteriser differences between us and
  Poppler, tight enough to catch a genuinely broken glyph/stream.
  Files below the bar are listed with a saved diff image. For each
  measured file the runner writes `{id}-p{n}-ours.pgm`,
  `-ref.pgm`, `-diff.pgm` under `reports/diff/` so a divergent page can
  be eyeballed without re-running.

  Current run (run-1792678862): 15 rendered files measured, **12 meet
  the bar**; mean SSIM 0.976, worst 0.733 (`type3wordspacing`), plus
  `cidfont_cmap_overflow`/`complex_ttf_font` at SSIM 0.962 — all three
  flagged with diff images for follow-up.

  The reference engine is Poppler as an *external* oracle only — its
  code is never linked or embedded; the comparison is process-level
  (`pdftoppm` subprocess) so the renderer stays self-contained.

## Sources & licences

The corpus mixes permissively-licensed and internal-use files
(nothing is redistributed — R2 is private):

- **pdf.js `test/pdfs`** — Apache-2.0, ~80 files: fonts, images,
  annotations, malformed/fuzzed, CID, xref/ObjStm edge cases.
- **veraPDF `veraPDF-corpus`** — GPLv3+MPL (internal only):
  graphics, transparency, ICCBased colour, forms.
- **arXiv papers** — author-submitted LaTeX PDFs (fonts, math,
  figures, large).
- **US Gov / IRS forms** — public-domain fillable AcroForm PDFs.
- **Adobe / W3C samples** — real generator variety.

Add more via `add`/`add-url`; keep `license` accurate per entry.

## Directory layout

```
compat/
  manifest.yaml     # id, sha256, category, source, license
  fetch-corpus.py   # R2 sync (boto3) — boto3/pyyaml in .venv-corpus
  runner/
    src/main.rs     # CLI, stage orchestration, report rendering
    src/cause.rs    # error → stable cause-id taxonomy (+ tests)
    src/probe.rs    # read-only multi-blocker page walker
    src/analysis.rs # aggregation, priorities, effort config (+ tests)
  corpus/           # downloaded PDFs (gitignored)
  reports/          # run-*.json/.md + diff images (gitignored)
```
