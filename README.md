# kasane-pdf

A PDF parsing/processing engine written in Rust. This is **not** a
wrapper around an existing PDF engine (PDFium, MuPDF, Poppler are not
used) — the syntax parser, document model, renderer, and writer are
implemented here.

Early development; the API is not yet stable.

## Design policy

- **Independent implementation** — the only external PDF code is a few
  permissively-licensed codec/raster crates (`flate2`, `tiny-skia`,
  `png`, `jpeg-decoder`, `ttf-parser`); all PDF interpretation is ours.
- **Everything is bounded** — every unbounded operation is capped by
  [`Limits`](src/limits.rs) (input size, reference depth, decoded
  bytes, op counts, pixel buffers, edit budgets, …).
- **No partial results** — a construct the engine cannot process
  *exactly* produces a typed [`Error`](src/error.rs)
  (`Error::Unsupported`/`Error::Structure`), never a silent skip or a
  best-effort guess. `Error::kind()` gives a stable machine-readable
  category.
- **Never panics** on hostile input; no `unsafe`.
- **Not a redaction tool, not a sanitizer** — the `experimental-*`
  editing commands rewrite selected content into a new object but do
  not prove the old data is gone. See [`docs/security.md`](docs/security.md).

## Features

- **Parsing** — full object layer (names/`#xx` escapes, hex/literal
  strings, arrays, dicts), classic `xref` tables + `/Prev` chains,
  xref streams (`/W`/`/Index`, types 0/1/2), hybrid `/XRefStm`
  references, object streams (`/ObjStm`), `FlateDecode` + PNG/TIFF
  predictors, indirect resolution with cycle detection. Document
  catalog + page tree with inherited `MediaBox`/`CropBox`/`Rotate`/
  `Resources`; `page_contents`/`page_operations`/`page_resource` APIs.
- **Rendering** — `render_page` → pixmap + `pixmap_to_png`: paths,
  clipping (nonzero/even-odd), DeviceGray/RGB/CMYK, Form & Image
  XObjects (`/Matrix`, `/BBox`, `/Decode`, raw + Flate + `DCTDecode`),
  embedded TrueType text (`/FontFile2`, WinAnsi), Type0/Identity-H/
  CIDFontType2, honouring `MediaBox`/`CropBox`/`Rotate`/DPI. Operator
  placement is checked per ISO 32000-1 §8.2 Figure 9.
- **Text extraction** — `extract_text` → positioned `CharInfo` per
  code (origin, advance, bounds): `/ToUnicode` CMap (`bfchar`/`bfrange`,
  UTF-16BE, surrogate pairs) with WinAnsi/AGL fallback; unresolved
  codes return `None`, never a guess. Headless — `/ActualText`/OC
  content is refused, not silently ignored.
- **Rewrite** — `rewrite(&Document)` → fresh single-revision PDF:
  DFS from trailer `/Root`+`/Info`, dense renumbering, `/Length`
  recomputed, stream bytes verbatim, links/annots carried.
  Signed/XFA/OCProperties/RichMedia/AA input is refused.
- **Editing** — `Document::edit()` session: add `/Square` annotations
  (`add_square`, generated `/AP`), and the experimental scoped
  `remove_text`/`remove_image_region`/`remove_region`/`remove_regions`
  (multi-page, inherited-`/Resources` materialisation) family plus
  `remove_document_metadata` — all through the atomic-overlay +
  rewrite pipeline, never mutating the input.
- **Diagnosis & audit** — `diagnose_redaction`/`preflight-redaction`
  (read-only report of painted content inside rects: targets,
  blockers, uncertain, shared, unexamined — deliberately *no* "safe"
  verdict) and `audit_residuals`/`audit-residuals` (read-only
  inventory of residual information).

## Current limitations

Encrypted documents; stream filters other than `/FlateDecode` and
`/DCTDecode` (images only); inline images (`BI`/`ID`/`EI`); text
extraction beyond TrueType+WinAnsi and Type0+Identity-H+CIDFontType2;
non-embedded/Type1/Type3/CFF fonts, non-Identity-H Type0 encodings,
`gs`/`sh`/patterns, non-device colour, `/ImageMask`/`/Mask`/`/SMask`,
transparency, `/UserUnit` ≠ 1, singular CTMs; annotation subtypes
other than `/Square`; `/UseCMap`-chained or multi-byte-codespace
ToUnicode CMaps. Each is a distinct typed error — never a partial
result. Full list: [`docs/features/`](docs/features/).

## Build

- Rust 1.70+ (`edition = "2021"`)

```console
$ cargo build
```

## Usage

Library:

```rust
use kasane_pdf::Document;

let data = std::fs::read("input.pdf")?;
let doc = Document::from_bytes(&data)?;
for page in doc.pages() {
    println!("page {}: {}", page.index() + 1, page.media_box());
}
# Ok::<(), kasane_pdf::Error>(())
```

CLI (subcommands):

```text
kasane-pdf <file.pdf>                          # info: page list
kasane-pdf contents <pdf> <page>               # decoded content stream
kasane-pdf operations <pdf> <page>             # typed op dump
kasane-pdf render <pdf> <page> --output out.png [--dpi N]
kasane-pdf text <pdf> <page> [--format text|json|lines]
kasane-pdf rewrite <in.pdf> --output <out.pdf>
kasane-pdf annotate-rect <in> <page> --rect .. --color .. --width .. --output ..
kasane-pdf inspect <pdf> <page>                # structural dump
kasane-pdf preflight-redaction <pdf> <page> --rect .. [--format json]
kasane-pdf audit-residuals <pdf> [--format json]
kasane-pdf experimental-remove-text <in> <page> --rect .. --output ..
kasane-pdf experimental-remove-image-region <in> <page> --rect .. --output ..
kasane-pdf experimental-remove-region <in> <page> --rect .. --output ..
kasane-pdf experimental-remove-regions <in> --rect .. [--pages ..] --output ..
kasane-pdf remove-document-metadata <in> --output ..
```

Example:

```console
$ kasane-pdf render tests/fixtures/render_rects.pdf 1 --output out.png --dpi 144
rendered page 1 -> out.png (400x400 px, 200 pts, dpi 144)

$ kasane-pdf experimental-remove-text tests/fixtures/text_basic.pdf 1 \
    --rect 35 60 38 80 --output /tmp/out.pdf
experimental-remove-text ... codes removed: 1
# output Contents: BT /F1 24 Tf 20 60 Td [(A) -600 (C)] TJ ET
# — 'B' gone, 'C' keeps its origin. (Content edit, not redaction.)
```

Errors carry a stable category: `error [unsupported feature]: …`.

## Testing

```console
$ cargo test            # unit + integration tests
$ cargo fmt --check
$ cargo clippy --all-targets
```

Tests are feature-organised (see [`tests/README.md`](tests/README.md));
fixtures in `tests/fixtures/` are deterministic — regenerate with
`python3 tests/fixtures/make_fixtures.py`. External checks:
`scripts/verify-rewrite.sh` (`qpdf --check` on rewrite output),
`scripts/verify-audit.sh`, `scripts/verify-resources-materialize.sh`.
See [`docs/testing.md`](docs/testing.md).

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — goals, module map,
  core decisions, `Limits`, licensing, roadmap.
- [`docs/features/`](docs/features/) — support matrix per area
  (parsing, rendering, rewriting, editing, diagnosis, audit).
- [`docs/security.md`](docs/security.md) — safety/`Limits` posture and
  the not-a-redaction/sanitizer guarantees.
- [`docs/decisions/`](docs/decisions/) — design-decision records.
- [`docs/testing.md`](docs/testing.md) — test layout & verification.
- [`docs/redaction-design.md`](docs/redaction-design.md) — recorded
  redaction constraints.

## License

Not yet decided. Until it is, all rights reserved. The dependency
policy (permissive only, no AGPL/GPL engine cores) is in
[`docs/architecture.md`](docs/architecture.md). The bundled
substitute fonts in [`fonts/liberation/`](fonts/liberation/) are the
Liberation Fonts under the SIL Open Font License 1.1 (see the README
and LICENSE there); they are used for non-embedded Standard 14-style
fonts only and never looked up on the host system.

## References

- ISO 32000-1:2008 — *PDF 1.7* (and errata).
- ISO 32000-2:2020 — PDF 2.0.

Section numbers cited in source comments refer to ISO 32000-1 unless
noted otherwise.

## Compatibility corpus

The engine is continuously measured against a real-world PDF corpus
(~100+ files: arXiv/LaTeX, IRS forms, veraPDF, pdf.js test files)
fetched from an **S3-compatible object store you configure** (or a
local directory) and never committed to git. See
[`docs/compat-corpus.md`](docs/compat-corpus.md).

```console
$ scripts/compat-run.sh --with-poppler   # sync, build, run, report
```

Results land in `compat/reports/` — JSON + Markdown with per-stage
PASS/MISMATCH/UNSUPPORTED/ERROR/CRASH/TIMEOUT classification.
