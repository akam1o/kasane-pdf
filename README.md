# kasane-pdf

An independent PDF engine written in Rust: a strict parser, a page
renderer, positioned text extraction, a document rewriter and an
incremental editor. It is **not** a wrapper around PDFium, MuPDF or
Poppler — the syntax parser, document model, content interpreter,
rasteriser and writer are implemented in this repository, and the
only external code is a handful of permissively licensed codec and
raster crates.

Status: pre-1.0, under active development. The API is not stable and
the crate is not yet published on crates.io.

## Design goals

- **Independent implementation.** Other PDF engines are used only as
  development-time verification tools (`qpdf`, Poppler) and are never
  linked.
- **Everything is bounded.** Every unbounded operation is capped by a
  [`Limits`](src/limits.rs) field: input size, reference depth,
  decoded bytes per operation, operator counts, pixel buffers, mesh
  subdivision, edit budgets. Hostile input must never panic or run
  without bound.
- **No partial results.** A construct the engine cannot process
  exactly produces a typed [`Error`](src/error.rs)
  (`Unsupported`, `Structure`, `LimitExceeded`, …) with a stable
  machine-readable `kind()`. Nothing is skipped silently; unavoidable
  approximations (colour profiles, substituted fonts) are reported
  on the result.
- **Strict by default, recovery by choice.** `Document::from_bytes`
  follows ISO 32000-1; `Document::from_bytes_recovering` rebuilds
  damaged files and reports every assumption it made.
- **Verified against other implementations.** A compatibility runner
  renders, extracts and rewrites a real-world corpus and compares the
  results with Poppler and qpdf on every page.
- **Not a redaction tool, not a sanitizer.** The editing APIs rewrite
  content; they do not prove the old data is gone. See
  [`docs/security.md`](docs/security.md).

## Features

**Parsing** — object layer, classic and stream cross-reference
sections, `/Prev` chains, hybrid files, object streams, every
standard stream filter (Flate, LZW, ASCII85, ASCIIHex, RunLength with
predictors; DCT, JBIG2, CCITT and JPX for images), page tree with
inherited attributes, lazy indirect resolution with cycle detection,
named destinations, an explicit recovery mode.

**Rendering** (`render_page` → RGBA pixmap, `pixmap_to_png`) — paths,
clipping, every colour-space family of §8.6 (Device, Cal, Lab,
ICC-based with exact matrix/TRC profiles, Indexed, Separation,
DeviceN, Pattern), ExtGState parameters including soft masks and all
16 blend modes, isolated and non-isolated transparency groups, image
XObjects with image masks, soft masks and colour-key masks, inline
images, shadings of all seven types, tiling and shading patterns,
text in TrueType, Type 1 (eexec and CFF), OpenType, Type 0 / CID and
Type 3 fonts with rendering modes 0–7, vertical writing, optional
content, annotation appearance streams and synthesised appearances
for form fields and markup annotations, `/UserUnit`. Non-embedded
Standard 14-style fonts draw with bundled Liberation substitutes
(reported, never silent).

**Text extraction** (`extract_text`) — one positioned `CharInfo` per
character code: origin, advance, glyph bounds, font size, rendering
mode and Unicode through `/ToUnicode`, the font encoding or
`/ActualText`. Unresolved codes are `None`, never a guess.

**Writing and editing** — `rewrite` produces a fresh single-revision
file from the reachable object graph (streams verbatim). An `Edit`
session adds `/Square` annotations, modifies or removes annotations,
sets form-field values and `/Info` entries, and can be written either
as a full rewrite or as an **incremental update** that keeps the
original bytes verbatim and appends a revision (`save_incremental`).
Signed documents are detected; edits a signer did not permit are
refused (see Limitations).

**Inspection and audit** — `inspect` structural dumps,
`diagnose_redaction` (what is painted inside a rectangle, with
blockers and uncertainties, deliberately without a "safe" verdict),
`audit_residuals` (an inventory of residual information), and the
experimental scoped content editors (`remove_text`,
`remove_image_region`, `remove_region`, `remove_regions`,
`remove_document_metadata`).

**CLI** — `kasane-pdf` exposes the above as subcommands (below).

## Installation

Rust 1.92 or newer. The crate is not on crates.io yet; depend on the
repository:

```toml
[dependencies]
kasane-pdf = { git = "https://github.com/akam1o/kasane-pdf" }
```

Build from a checkout:

```console
cargo build --release                 # library + CLI
cargo install --path .                # installs the `kasane-pdf` binary
```

## Quick start

Render a page to PNG ([`examples/render_page.rs`](examples/render_page.rs)):

```rust
use kasane_pdf::{pixmap_to_png, render_page, Document, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    let opts = RenderOptions { dpi: 144.0, ..RenderOptions::default() };
    let page = render_page(&doc, &doc.pages()[0], &opts)?;
    std::fs::write("page-1.png", pixmap_to_png(&page.pixmap)?)?;
    for s in &page.font_substitutions {
        eprintln!("substituted font: {} -> {}", s.base_font, s.substitute);
    }
    Ok(())
}
```

Extract text with positions ([`examples/extract_text.rs`](examples/extract_text.rs)):

```rust
use kasane_pdf::{extract_text, Document, Limits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    let text = extract_text(&doc, &doc.pages()[0], &Limits::default())?;
    for c in &text.chars {
        if let Some(u) = &c.unicode {
            println!("{u} at ({:.1}, {:.1})", c.origin.0, c.origin.1);
        }
    }
    Ok(())
}
```

Edit and save incrementally ([`examples/incremental_save.rs`](examples/incremental_save.rs)):

```rust
use kasane_pdf::{Dict, Document, Object, Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    let mut edit = doc.edit()?;
    let mut info = Dict::new();
    info.insert(b"Title".to_vec(), Object::String(b"Annotated copy".to_vec()));
    edit.set_info(info)?;
    edit.add_square(0, Rect { llx: 72.0, lly: 72.0, urx: 200.0, ury: 144.0 }, [1.0, 0.0, 0.0], 2.0)?;
    let out = edit.save_incremental()?;   // original bytes + appended revision
    assert!(out.starts_with(&data));
    std::fs::write("annotated.pdf", out)?;
    Ok(())
}
```

Run the examples against a fixture:

```console
cargo run --example render_page -- tests/fixtures/render_rects.pdf 1 /tmp/out.png 144
cargo run --example extract_text -- tests/fixtures/text_basic.pdf
cargo run --example incremental_save -- tests/fixtures/hello.pdf /tmp/annotated.pdf
```

CLI:

```text
kasane-pdf <file.pdf>                                   page list and document info
kasane-pdf contents <pdf> <page>                        decoded content stream
kasane-pdf operations <pdf> <page>                      typed operator dump
kasane-pdf render <pdf> <page> --output out.png [--dpi N]
kasane-pdf text <pdf> <page> [--format json|lines]
kasane-pdf rewrite <in.pdf> --output <out.pdf>
kasane-pdf annotate-rect <in> <page> --rect llx lly urx ury --color r g b --width w --output <out>
kasane-pdf inspect <pdf> <page>                         structural dump
kasane-pdf preflight-redaction <pdf> <page> --rect … [--format text|json]
kasane-pdf audit-residuals <pdf> [--format text|json]
kasane-pdf experimental-remove-text <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-image-region <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-region <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-regions <in> --rect … [--page N …] --output <out>
kasane-pdf remove-document-metadata <in> --output <out>
```

Errors are printed with their stable category, e.g.
`error [unsupported feature]: …`.

## Feature flags

| feature | default | effect |
|---|---|---|
| `cjk-fallback` | off | Embeds the Adobe-Japan1 predefined CMaps (`90ms-RKSJ-*`, `UniJIS-*`, `EUC-*`, …), the Adobe-Japan1 CID → Unicode table and Noto Sans JP Regular (about 4.3 MB) so that **non-embedded Japanese CID fonts** render. Glyph shapes come from Noto Sans, metrics from the PDF; every such render is reported in `font_substitutions`. The assets are fetched by `scripts/fetch-cjk-assets.sh` (Adobe resources BSD-3-Clause, Noto SIL OFL 1.1) and are not committed. |

Without the feature, a non-embedded CID font or a predefined
non-Identity CMap is a typed `Error::Unsupported`. No build ever
consults fonts installed on the host system.

## Compatibility

Support is measured, not asserted. The compatibility runner
(`compat/runner`) processes a corpus of 511 PDFs drawn from public
test suites (pdf.js, PDFium, veraPDF, Poppler, pdfbox) and real-world
documents (arXiv papers, government forms, browser prints, scans,
Japanese documents), and cross-checks against Poppler and qpdf. The
corpus itself is not in the repository; the aggregate baseline is
(`compat/baselines/corpus-full.json`). Latest run, on the 427 files
expected to be well-formed:

| stage | pass |
|---|---|
| strict parse | 404 (94.6%) |
| render, every page | 365 (85.5%) |
| text extraction | 381 (89.2%) |
| rewrite | 348 (81.5%) |
| all four | 315 (73.8%) |

Of the 383 files that were rewritten, 383 reparse with an identical
object graph, 350 render identically on all 1,308 compared pages,
and the rewrite introduced no new qpdf or Poppler diagnostic in any
of them; 43 adversarial files are refused without crash or timeout.
These numbers describe that corpus and the strict parser; they are
not a general compatibility claim. Definitions, per-category results
and the remaining blockers are in
[`docs/compat-corpus.md`](docs/compat-corpus.md).

## Limitations

- **Encrypted documents** are refused (`Error::Encrypted`); no
  security handler is implemented.
- **Digital signatures are detected, not verified.** The engine reads
  `/ByteRange`, `/DocMDP` and `/FieldMDP` to decide which incremental
  edits a signer permitted and refuses the rest; it never checks the
  digest or certificate and never reports a signature as valid. A full
  rewrite of a signed document is always refused.
- **Strict mode fails on malformed files** (dangling references,
  unbalanced `q`/`Q`, truncated streams); the explicit recovery parser
  handles some of them and reports what it assumed.
- **Rendering gaps:** knockout transparency groups, a shading
  pattern's `/ExtGState` override, non-embedded Symbol and
  ZapfDingbats, non-embedded CJK fonts outside the Adobe-Japan1
  fallback, Type 3 fonts in text clipping modes, `/Matte`
  pre-blended soft masks, non-default transfer functions. ICC profiles
  the engine cannot evaluate exactly fall back to the alternate space
  and are reported.
- **No colour management** beyond exact matrix/TRC ICC profiles; no
  glyph hinting or shaping; no layout analysis, reading order or
  search in text extraction.
- **Rewrite** flattens the revision history (use the incremental save
  to keep it), refuses PDF 2.0 declarations, and carries unknown
  structures verbatim — it is not a sanitizer. The incremental writer
  cannot delete bytes, refuses recovered documents and non-zero
  generation references.
- **The experimental `remove_*` editors are content editors**, not
  redaction: they report what they changed, not that data is gone.
- `Document` is single-threaded (interior caches); use one instance
  per thread.

The complete per-feature contract, including every refusal, is in
[`docs/features/`](docs/features/) and summarised in
[`docs/limitations.md`](docs/limitations.md).

## Documentation

- [`docs/README.md`](docs/README.md) — index by topic.
- [`docs/getting-started.md`](docs/getting-started.md) — install,
  CLI tour, library usage.
- [`docs/architecture.md`](docs/architecture.md) — goals, module map,
  key decisions, `Limits`, licensing policy, roadmap.
- [`docs/features/`](docs/features/) — support matrices (parsing,
  rendering, rewriting, editing, incremental save, diagnosis, audit).
- [`docs/compat-corpus.md`](docs/compat-corpus.md) — the compatibility
  corpus, the runner, round-trip verification, results.
- [`docs/security.md`](docs/security.md) — safety posture.
- [`docs/third-party.md`](docs/third-party.md) — dependencies and
  bundled assets with their licences.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the build, test, lint and
compatibility-test workflow, and [`SECURITY.md`](SECURITY.md) for
reporting security problems privately.

## License

Copyright 2026 Akira Kamio. kasane-pdf is licensed under the
[Apache License, Version 2.0](LICENSE).
The bundled Liberation fonts and the font subsets among the test
fixtures are under the SIL Open Font License 1.1 and are listed in
[`NOTICE`](NOTICE); all dependencies and their licences are in
[`docs/third-party.md`](docs/third-party.md).
