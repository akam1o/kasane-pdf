# Real-world PDF compatibility corpus

kasane-pdf measures engine maturity against real, in-the-wild PDFs —
not just the hand-crafted `tests/fixtures/`. The corpus lives in
**Cloudflare R2** (private bucket, S3 API) and is downloaded on
demand; PDFs are never committed to git.

- Bucket: `kasane-pdf-corpus`
- Endpoint: `https://9dffe78260d2c3d85a616c9bc35a8485.r2.cloudflarestorage.com`
- Layout: `pdfs/<category>/<sha256>.pdf`
- Manifest: [`../compat/manifest.yaml`](../compat/manifest.yaml) —
  the single source of truth (id, sha256, category, source, license).

## Credentials

Read/write keys live in `~/.cloudflare/` (not in the repo):

```
~/.cloudflare/r2-kasane-pdf-corpus.access_key_id
~/.cloudflare/r2-kasane-pdf-corpus.secret_access_key
```

or as `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`. The token is
scoped to this bucket only.

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
  runner/           # Rust compat runner (path-dep on the crate)
  corpus/           # downloaded PDFs (gitignored)
  reports/          # run-*.json/.md + diff images (gitignored)
```
