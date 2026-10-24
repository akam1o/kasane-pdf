# kasane-pdf

An independent PDF engine written in Rust: a strict parser, a page
renderer, positioned text extraction, a document rewriter and an
incremental editor. It is **not** a wrapper around PDFium, MuPDF or
Poppler — the syntax parser, document model, content interpreter,
rasteriser and writer are implemented in this repository, and the
only external code is a handful of permissively licensed codec and
raster crates.

Status: 0.1.x on [crates.io](https://crates.io/crates/kasane-pdf),
documented on [docs.rs](https://docs.rs/kasane-pdf); this branch is
the unreleased 0.2 line. The API is 0.x: additions arrive in minor
releases, breaking changes are recorded in
[`CHANGELOG.md`](CHANGELOG.md), classified in
[`docs/api-stability.md`](docs/api-stability.md), and 0.1 → 0.2 is a
mechanical migration ([`docs/migration-0.2.md`](docs/migration-0.2.md)).

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
  machine-readable `kind()`. Nothing is skipped silently: unavoidable
  approximations (colour profiles, substituted fonts) are reported
  on the result, and a non-conforming input the engine could still
  process exactly is recorded as a [`Warning`](src/warning.rs) on
  the `Document` (`take_warnings`), never folded into a clean pass.
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

**Rendering** (`render_page` → premultiplied RGBA8 buffer, `to_png`) — paths,
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

```toml
[dependencies]
kasane-pdf = "0.1"
```

Or track the repository directly:

```toml
[dependencies]
kasane-pdf = { git = "https://github.com/akam1o/kasane-pdf", tag = "v0.1.1" }
```

Supported Rust versions: 1.92 or newer (`rust-version` in
`Cargo.toml`, checked in CI with the stable toolchain and verified
with 1.92). No build dependencies, no C code, no network access at
build time.

Build from a checkout:

```console
cargo build --release                 # library + CLI
cargo install --path .                # installs the `kasane-pdf` binary
```

## Quick start

Render a page to PNG ([`examples/render_page.rs`](examples/render_page.rs)):

```rust,no_run
use kasane_pdf::{render_page, Document, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    let opts = RenderOptions::default().with_dpi(144.0);
    let page = render_page(&doc, &doc.pages()[0], &opts)?;
    std::fs::write("page-1.png", page.to_png()?)?;
    // `page.pixels()` is the premultiplied RGBA8 buffer, `page.stride()`
    // its row length; `page.into_rgba8()` takes it without a copy.
    for s in page.font_substitutions() {
        eprintln!("substituted font: {} -> {}", s.base_font, s.substitute);
    }
    // Non-fatal deviations the engine processed exactly (a Flate stream
    // missing its final block, a malformed /ToUnicode entry, …).
    for w in doc.take_warnings() {
        eprintln!("warning: {w}");
    }
    Ok(())
}
```

Walk the document structure — every object is reachable through the
same typed `Object` model the engine uses ([`examples/inspect_objects.rs`](examples/inspect_objects.rs)):

```rust,no_run
use kasane_pdf::{Document, Object};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: inspect_objects <input.pdf>")?;
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?; // strict; `from_bytes_recovering` for damaged files
    println!(
        "version {:?}, {} pages",
        doc.catalog_version()?,
        doc.page_count()
    );
    for page in doc.pages() {
        let dict = doc.resolve(page.object_ref().ok_or("direct page")?)?;
        let annots = match dict.as_dict().and_then(|d| d.get(b"Annots".as_slice())) {
            Some(a) => doc.deref(a)?.as_array().map(|a| a.len()).unwrap_or(0),
            None => 0,
        };
        let mb = page.media_box();
        println!(
            "page {}: {} x {} pt, {} annotation(s)",
            page.index() + 1,
            mb.urx - mb.llx,
            mb.ury - mb.lly,
            annots
        );
        for op in doc.page_operations(page)?.iter().take(3) {
            println!("  {op:?}");
        }
    }
    if let Some(info) = doc.trailer().get(b"Info".as_slice()) {
        if let Object::Dict(d) = doc.deref(info)? {
            println!("info keys: {}", d.len());
        }
    }
    Ok(())
}
```

Extract text with positions ([`examples/extract_text.rs`](examples/extract_text.rs)):

```rust,no_run
use kasane_pdf::{extract_text, Document, TextOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    // `TextOptions` carries the limits and the switches (hidden
    // optional content, a region filter); the defaults mirror a viewer.
    let text = extract_text(&doc, &doc.pages()[0], &TextOptions::default())?;
    for c in &text.chars {
        if let Some(u) = &c.unicode {
            println!("{u} at ({:.1}, {:.1})", c.origin.0, c.origin.1);
        }
    }
    Ok(())
}
```

Edit and save incrementally ([`examples/incremental_save.rs`](examples/incremental_save.rs)):

```rust,no_run
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

kasane-pdf does **not** create documents from scratch: there is no
page or content-stream builder in 0.1. Editing starts from an existing
file — annotations, form-field values and `/Info` through `Edit`,
written as a full rewrite or as an incremental update.

All examples, runnable against the fixtures in `tests/fixtures/`:

| example | shows |
|---|---|
| [`render_page.rs`](examples/render_page.rs) | open, render a page at a DPI, write PNG, report substitutions |
| [`inspect_objects.rs`](examples/inspect_objects.rs) | page info (size, rotation, annotations), object model, operators |
| [`extract_text.rs`](examples/extract_text.rs) | positioned text with Unicode |
| [`rewrite.rs`](examples/rewrite.rs) | full rewrite and the typed refusals (signed, PDF 2.0, …) |
| [`incremental_save.rs`](examples/incremental_save.rs) | `Edit` session saved as an appended revision |
| [`recovery.rs`](examples/recovery.rs) | strict parse first, explicit recovery parse with its report |
| [`cjk_fallback.rs`](examples/cjk_fallback.rs) | non-embedded Japanese CID fonts (`--features cjk-fallback`) |

```console
cargo run --example render_page -- tests/fixtures/render_rects.pdf 1 /tmp/out.png 144
cargo run --example inspect_objects -- tests/fixtures/hello.pdf
cargo run --example extract_text -- tests/fixtures/text_basic.pdf
cargo run --example rewrite -- tests/fixtures/hello.pdf /tmp/rewritten.pdf
cargo run --example incremental_save -- tests/fixtures/hello.pdf /tmp/annotated.pdf
cargo run --example recovery -- tests/fixtures/hello.pdf
cargo run --example cjk_fallback --features cjk-fallback -- path/to/japanese.pdf /tmp/cjk.png
```

The public surface these examples use is also exercised by
[`tests/public_api.rs`](tests/public_api.rs).

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
| `tiny-skia-interop` | on | Exposes the `tiny-skia` `Pixmap` behind `RenderedPage` (`pixmap()`, `into_pixmap()`, the `Pixmap` re-export, `pixmap_to_png`). The rasteriser is always tiny-skia; the flag only decides whether its type is part of kasane-pdf's public API. The backend-neutral accessors (`pixels()`, `stride()`, `pixel_format()`, `into_rgba8()`, `to_png()`) are always available. |
| `cjk-fallback` | off | Embeds the Adobe-Japan1 predefined CMaps (`90ms-RKSJ-*`, `UniJIS-*`, `EUC-*`, …), the Adobe-Japan1 CID → Unicode table and Noto Sans JP Regular (about 4.3 MB) so that **non-embedded Japanese CID fonts** render. Glyph shapes come from Noto Sans, metrics from the PDF; every such render is reported in `font_substitutions`. The assets ship in the crate under `assets/cjk/` (Adobe resources BSD-3-Clause, Noto SIL OFL 1.1; pinned upstream commits, SHA-256 lock in `scripts/cjk-assets.sha256`), so `--features cjk-fallback` needs no extra step. |

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
| strict parse | 406 (95.1%) |
| render, every page | 368 (86.2%) |
| text extraction | 384 (89.9%) |
| rewrite | 350 (82.0%) |
| all four | 318 (74.5%) |

Two of the passing files were *accepted with warnings* (a Flate
stream missing its final block, processed exactly and recorded); the
runner counts them apart from a clean pass. Of the 385 files that
were rewritten, 385 reparse with an identical object graph, 352
render identically on all their compared pages, and the rewrite
introduced no new qpdf or Poppler diagnostic in any of them; 43
adversarial files are refused without crash or timeout.
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

## Security notes

Read these before processing untrusted files or files whose integrity
matters. [`docs/security.md`](docs/security.md) has the full posture.

- **Rewrite vs incremental save.** `rewrite` / `Edit::rewrite` write a
  *new* file: the reachable object graph, renumbered, old revisions
  dropped, unreachable objects gone — but every reachable object is
  carried verbatim, including payloads the engine does not interpret,
  so a rewrite is not a sanitizer. `Edit::save_incremental` keeps the
  input bytes exactly and appends a revision: nothing is removed, the
  previous state remains readable by anyone with the file.
- **Signed documents.** Signatures are detected, never verified.
  Before an incremental save the engine reads `/ByteRange`,
  `/DocMDP` and `/FieldMDP` and refuses edits the signer did not
  permit; a full rewrite of a signed file is always refused. Keeping
  the signed byte range intact is a *structural* property —
  it says nothing about whether the signature is cryptographically
  valid, whether the certificate is trusted, or whether the
  signer's other constraints were honoured. Use a signature
  validator for that.
- **Recovery mode means guessing, recorded.** `from_bytes_recovering`
  rebuilds the cross-reference table from whatever `n g obj` headers
  and trailers a scan finds. The result is a best effort over a
  damaged file; `Document::recovery()` lists what was assumed, and
  the writers refuse to save a recovered document incrementally. A
  strict parse never produces a report.
- **Unsupported is an error, not a blank.** A feature the engine does
  not implement is `Error::Unsupported { feature, detail }` for the
  operation that needs it; the page is not rendered partially. What
  *is* reported on a successful result: font substitutions and
  colour approximations (`RenderedPage`), and non-conforming input
  processed exactly (`Document::take_warnings`).
- **Resource limits.** Every unbounded operation is capped by
  `Limits` (input size, decoded bytes, pixels, operator counts,
  depths, edit budgets). The defaults suit ordinary documents;
  tighten them for untrusted input in constrained environments with
  `Limits::default().with_<field>(value)`. Exceeding one is
  `Error::LimitExceeded { limit, max }`, distinct from a malformed
  file; a zero structural bound is `Error::InvalidArgument`.
- **Not a redaction tool.** The `remove_*` editors and
  `diagnose_redaction` report what they changed or found; they do not
  prove data is gone.

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
- [`docs/api-stability.md`](docs/api-stability.md) — what the 0.1
  API promises, what may change in 0.2.
- [`docs/decisions/`](docs/decisions/) — decision records, including
  the `ttf-parser` maintenance notice.
- [`docs/third-party.md`](docs/third-party.md) — dependencies and
  bundled assets with their licences.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the build, test, lint and
compatibility-test workflow, and [`SECURITY.md`](SECURITY.md) for
reporting security problems privately.

## License

Copyright 2026 akam1o. kasane-pdf is licensed under the
[Apache License, Version 2.0](LICENSE).
The bundled Liberation fonts and the font subsets among the test
fixtures are under the SIL Open Font License 1.1 and are listed in
[`NOTICE`](NOTICE); all dependencies and their licences are in
[`docs/third-party.md`](docs/third-party.md).
