# kasane-pdf

An independent PDF engine written in Rust: a strict parser, a page
renderer, positioned text extraction, a document rewriter and an
incremental editor. It is **not** a wrapper around PDFium, MuPDF or
Poppler — the syntax parser, document model, content interpreter,
rasteriser and writer are implemented in this repository, and the
only external code is a handful of permissively licensed codec and
raster crates.

Status: 0.6 on [crates.io](https://crates.io/crates/kasane-pdf),
documented on [docs.rs](https://docs.rs/kasane-pdf). The API is 0.x:
additions arrive in minor releases, breaking changes are recorded in
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
- **Redaction that removes, and says what it proves.** `redact`
  removes what is drawn inside page rectangles — never paints over it
  — checks the written bytes, and refuses what it cannot process
  exactly. Its guarantee is positional and stated in three parts
  ([`docs/features/redaction.md`](docs/features/redaction.md)). The
  other editing APIs are content editors, not redaction, and nothing
  here is a sanitizer ([`docs/security.md`](docs/security.md)).

## Features

**Parsing** — object layer, classic and stream cross-reference
sections, `/Prev` chains, hybrid files, object streams, every
standard stream filter (Flate, LZW, ASCII85, ASCIIHex, RunLength with
predictors; DCT, JBIG2, CCITT and JPX for images), page tree with
inherited attributes, lazy indirect resolution with cycle detection,
named destinations, an explicit recovery mode.

**Encrypted documents** (read only) — the Standard security handler,
revisions 2–6: RC4, AES-128 and AES-256, crypt filters, the empty,
user or owner password (`Document::open` with `OpenOptions`), Unicode
passwords. Permissions are reported, not enforced; the writers refuse
encrypted documents. See [`docs/encryption.md`](docs/encryption.md).

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
mode and Unicode through `/ToUnicode`, the font encoding's glyph names
(Adobe Glyph List), the embedded font program's own Unicode `cmap`,
the Adobe-Japan1 collection (`cjk-fallback`) or `/ActualText` —
`ResolveKind` says which. Unresolved codes are `None`, never a guess.
Optionally (`TextOptions::with_annotations`) the text of annotations
and form fields, reported apart from the page content.

**Writing and editing** — `rewrite` produces a fresh single-revision
file from the reachable object graph (streams verbatim). An `Edit`
session adds `/Square` annotations, modifies or removes annotations,
sets form-field values and `/Info` entries, and can be written either
as a full rewrite or as an **incremental update** that keeps the
original bytes verbatim and appends a revision (`save_incremental`).
Signed documents are detected; edits a signer did not permit are
refused (see Limitations). `rewrite_with` takes explicit policies for
executable content and usage-rights signatures and reports what it
carried or removed (below).

**Redaction** — `redact` (CLI `redact`): inside the given page
rectangles, text codes in embedded fonts are deleted, image samples
zeroed, paths removed or cut at the rect's edge, links (and on request
markup annotations) removed; forms and shared images are edited in
per-use copies; `/Info` and XMP are removed. Before returning, the
output is parsed again and checked: the same rects find nothing left,
no edited original or removed annotation is still in the file unless
the output still uses it elsewhere (reported), one revision only. What
it cannot process exactly — non-embedded fonts (unless their
Standard 14 substitutes are embedded first, on request), Type 3 glyphs
drawn by more than paths and images,
curved fills crossing a rect, masked images, widgets meeting a rect and
more — is a typed error and no output. Copies of the information elsewhere in the
document are reported, not removed.

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
kasane-pdf = "0.7"
```

Or track the repository directly:

```toml
[dependencies]
kasane-pdf = { git = "https://github.com/akam1o/kasane-pdf", tag = "v0.7.0" }
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
| [`rewrite.rs`](examples/rewrite.rs) | full rewrite and the typed refusals (signed, scripts, …) |
| [`remove_usage_rights.rs`](examples/remove_usage_rights.rs) | `rewrite_with` + `UsageRightsPolicy::Remove`: usage-rights signatures removed, every other signature refused, the report |
| [`incremental_save.rs`](examples/incremental_save.rs) | `Edit` session saved as an appended revision |
| [`recovery.rs`](examples/recovery.rs) | strict parse first, explicit recovery parse with its report |
| [`open_encrypted.rs`](examples/open_encrypted.rs) | encrypted document: empty password, then a password from a file; what `/Encrypt` declares |
| [`cjk_fallback.rs`](examples/cjk_fallback.rs) | non-embedded Japanese CID fonts (`--features cjk-fallback`) |

```console
cargo run --example render_page -- tests/fixtures/render_rects.pdf 1 /tmp/out.png 144
cargo run --example inspect_objects -- tests/fixtures/hello.pdf
cargo run --example extract_text -- tests/fixtures/text_basic.pdf
cargo run --example rewrite -- tests/fixtures/hello.pdf /tmp/rewritten.pdf
cargo run --example incremental_save -- tests/fixtures/hello.pdf /tmp/annotated.pdf
cargo run --example recovery -- tests/fixtures/hello.pdf
cargo run --example open_encrypted -- tests/fixtures/encrypted/enc_r6_aes256_empty.pdf
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
kasane-pdf redact <in> --page N --rect … [--page M --rect …] [--remove-annotations] [--embed-substitute-fonts] [--remove-javascript] --output <out>
kasane-pdf preflight-redaction <pdf> <page> --rect … [--format text|json]
kasane-pdf audit-residuals <pdf> [--format text|json]
kasane-pdf experimental-remove-text <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-image-region <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-region <in> <page> --rect … --output <out>
kasane-pdf experimental-remove-regions <in> --rect … [--page N …] --output <out>
kasane-pdf remove-document-metadata <in> --output <out>
```

Every subcommand accepts `--password-file <path>` (`-`: standard
input) for an encrypted document; the first line is the password. A
password is never taken from the command line itself.

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
(`compat/baselines/corpus-full.json`). Linux nightly on the 0.5.0
code (2026-11-04), on the 425 files expected to be well-formed:

| stage | pass |
|---|---|
| strict parse | 408 (96.0%) |
| render, every page | 380 (89.4%) |
| text extraction | 392 (92.2%) |
| rewrite (default policy) | 372 (87.5%) |
| all four | 345 (81.2%) |

With `rewrite_with`'s options all four pass for 351
(`ExecutableContentPolicy::Carry`, Linux and macOS) and 378 (`Carry`
and `UsageRightsPolicy::Remove`, measured on macOS so far); the
options are opt-in, and the default is the figure above. Of the files rewritten by
default (every expectation tag), 436 reparse with an identical object
graph and 432 match on every comparison; 302 also pass the semantic
comparison (the others carry nothing it compares). All 44 adversarial
files are refused without crash or timeout. Exact renders are checked against Poppler by an enforced
quality gate and report-only placement and colour rules.
These numbers describe that corpus and the strict parser; they are
not a general compatibility claim. Definitions, per-category results
and the remaining blockers are in
[`docs/compat-corpus.md`](docs/compat-corpus.md).

## Limitations

- **Encrypted documents** are read through the Standard security
  handler (R2–R6) with the empty user password or a password passed to
  `Document::open`; they are never written (the writers refuse them).
  Public-key and third-party handlers are refused. Permissions are
  reported, not enforced, and an empty user password means the file has
  no confidentiality ([`docs/encryption.md`](docs/encryption.md)).
- **Digital signatures are detected, not verified.** The engine reads
  `/ByteRange`, `/DocMDP` and `/FieldMDP` to decide which incremental
  edits a signer permitted and refuses the rest; it never checks the
  digest or certificate and never reports a signature as valid. A full
  rewrite of a signed document is refused, except that
  `UsageRightsPolicy::Remove`, on request, deletes usage-rights
  signatures alone (never re-signs; costs more memory and time).
- **Strict mode fails on malformed files** (damaged cross-reference
  tables, stray `Q`, truncated streams); a reference to an undefined
  object is null as §7.3.10 requires, and an open `q`/`BT` block at the
  end of a stream is closed — both recorded as warnings. The explicit
  recovery parser
  handles some of them and reports what it assumed.
- **Rendering gaps:** a shading pattern's `/ExtGState` override,
  transfer functions other than `/Default`, non-embedded CJK fonts
  outside the Adobe-Japan1
  fallback, Type 3 fonts in text clipping modes, `/Matte`
  pre-blended soft masks, non-default transfer functions. ICC profiles
  the engine cannot evaluate exactly fall back to the alternate space
  and are reported.
- **No colour management** beyond exact matrix/TRC ICC profiles; no
  glyph hinting or shaping; no layout analysis, reading order or
  search in text extraction.
- **Rewrite** flattens the revision history (use the incremental save
  to keep it), refuses versions above PDF 2.0, and carries unknown
  structures verbatim — it is not a sanitizer. The incremental writer
  cannot delete bytes, refuses recovered documents and non-zero
  generation references.
- **`redact` is positional**: it removes what is drawn inside the
  rects and reports — does not remove — copies elsewhere (other pages,
  annotations, attachments, metadata it does not own). It refuses
  non-embedded fonts unless `embed_substitute_fonts` embeds the
  Standard 14 substitutes first, Type 3 glyphs drawn by more than
  paths and images, vertical fonts, curved fills crossing a rect, masked images,
  Indexed or four-component JPEGs, widgets and media
  meeting a rect, shadings and tiling patterns crossing a rect, soft
  masks, scripts unless `remove_javascript` removes them, strings
  readers disagree on (unescaped CRs, CMap codes written as strings)
  and more
  (`docs/features/redaction.md`); a stroke cut at a rect can lose a
  sliver of band next to it (oblique crossings, round caps at an
  original end).
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
- **Executable content.** `rewrite` (and `rewrite_with` with the
  default `ExecutableContentPolicy::Profile`) refuses the document-level
  entry points (catalog `/AA`, a JavaScript `/OpenAction`, `/Names
  /JavaScript`) and carries every other action verbatim.
  `rewrite_with` reports each one carried (class, owner, position, script
  length; never the text):

  ```rust,no_run
  use kasane_pdf::{rewrite_with, Document, ExecutableContentPolicy, RewriteOptions};

  fn main() -> Result<(), Box<dyn std::error::Error>> {
      let doc = Document::from_bytes(&std::fs::read("input.pdf")?)?;
      let opts = RewriteOptions::new()
          .with_executable_content(ExecutableContentPolicy::RefuseScriptsAndExternal);
      // Err (refusal_reason() Script / ExternalAction) if any is present.
      let out = rewrite_with(&doc, &opts)?;
      for item in out.report().executable_content() {
          println!("{} {} at {}", item.class.as_str(), item.kind.as_str(), item.owner);
      }
      std::fs::write("output.pdf", out.bytes())?;
      Ok(())
  }
  ```

  `ExecutableContentPolicy::Carry` keeps scripts, including the
  document-level ones. It executes nothing, and it is **not a
  sanitizer**: the output can run whatever the input could.
- **Usage-rights signatures.** `UsageRightsPolicy::Remove` (opt-in;
  the default refuses) deletes Reader-extension signatures (`/Perms
  /UR`, `/UR3`) and withdraws the rights they grant; any other
  signature, a field lock, `/DSS` or `/Legal`, or anything it cannot
  determine refuses, and the output is re-checked before it is
  returned. It never verifies or re-signs:

  ```rust,no_run
  use kasane_pdf::{
      rewrite_with, Document, ExecutableContentPolicy, RewriteOptions, UsageRightsOutcome,
      UsageRightsPolicy,
  };

  fn main() -> Result<(), Box<dyn std::error::Error>> {
      let doc = Document::from_bytes(&std::fs::read("form.pdf")?)?;
      let opts = RewriteOptions::new()
          .with_executable_content(ExecutableContentPolicy::Carry)
          .with_usage_rights(UsageRightsPolicy::Remove);
      let out = rewrite_with(&doc, &opts)?; // Err: refused, nothing written
      if let UsageRightsOutcome::Removed(r) = out.report().usage_rights() {
          println!("removed {} usage-rights signature(s)", r.entries.len());
      }
      println!("{}", out.report()); // counts per class
      std::fs::write("out.pdf", out.bytes())?;
      Ok(())
  }
  ```
- **Signed documents.** Signatures are detected, never verified.
  Before an incremental save the engine reads `/ByteRange`,
  `/DocMDP` and `/FieldMDP` and refuses edits the signer did not
  permit; a full rewrite of a signed file is refused. The one
  exception is opt-in: `UsageRightsPolicy::Remove` deletes `/Perms
  /UR` / `/UR3` usage-rights signatures (withdrawing the reader rights
  they grant) when no other signature-like structure exists anywhere,
  and reports it (`RewriteReport::usage_rights()`); any other signature
  still refuses ([`docs/security.md`](docs/security.md)). Keeping
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
- **Resource limits.** Every unbounded size, count and depth is capped
  by `Limits` (input size, decoded bytes, pixels, operator counts,
  depths, edit budgets). The defaults suit ordinary documents;
  tighten them for untrusted input in constrained environments with
  `Limits::default().with_<field>(value)`. Processing **time** is not
  bounded: a page within every limit can still take minutes to
  render, so run untrusted renders under your own time limit
  ([`docs/security.md`](docs/security.md)). Exceeding one is
  `Error::LimitExceeded { limit, max }`, distinct from a malformed
  file; a zero structural bound is `Error::InvalidArgument`.
- **Encryption is not a security property of the output.** Opening an
  encrypted file only means the password matched; RC4/MD5 revisions
  and empty user passwords protect nothing, and `/P` permissions are a
  request to the reader, not access control. kasane-pdf reports them
  and never writes encrypted output.
- **Only `redact` is meant for redaction**, and only within its stated
  guarantee. The `remove_*` editors and `diagnose_redaction` report
  what they changed or found; they do not prove data is gone.

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
- [`docs/encryption.md`](docs/encryption.md) — reading encrypted
  documents: passwords, errors, what encryption does not mean.
- [`docs/migration-0.3.md`](docs/migration-0.3.md) — upgrading from 0.2.
- [`docs/api-stability.md`](docs/api-stability.md) — what the public
  API promises and how it evolves.
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
