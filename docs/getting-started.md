# Getting started

## Requirements

- Rust 1.92 or newer (`rust-version` in `Cargo.toml`).
- Nothing else for the library and CLI. The optional external tools
  (`qpdf`, Poppler) are only needed for the verification scripts and
  the compatibility runner.

## Build and install

```console
git clone https://github.com/akam1o/kasane-pdf
cd kasane-pdf
cargo build --release
cargo install --path .          # puts `kasane-pdf` on your PATH
```

The crate is not published on crates.io yet. To use it as a
dependency, point Cargo at the repository:

```toml
[dependencies]
kasane-pdf = { git = "https://github.com/akam1o/kasane-pdf" }
```

## A tour of the CLI

```console
$ kasane-pdf tests/fixtures/hello.pdf
```

prints the page list (media box, rotation) and document information.

```console
$ kasane-pdf render tests/fixtures/render_rects.pdf 1 --output out.png --dpi 144
rendered page 1 -> out.png (400x400 px, 200 pts, dpi 144)

$ kasane-pdf text tests/fixtures/text_basic.pdf 1 --format lines
$ kasane-pdf inspect tests/fixtures/hello.pdf 1
$ kasane-pdf rewrite tests/fixtures/hello.pdf --output /tmp/rewritten.pdf
```

Every failure is a typed error with a stable category in brackets,
e.g. `error [unsupported feature]: …`, `error [invalid document
structure]: …`; scripts should branch on the category, not the
message. The full subcommand list is in the README.

## Using the library

The three entry points most programs need:

```rust
use kasane_pdf::{extract_text, pixmap_to_png, render_page, Document, Limits, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Load — strict by default. `Document::from_bytes_recovering`
    //    accepts damaged files and reports what it assumed.
    let data = std::fs::read("input.pdf")?;
    let doc = Document::from_bytes(&data)?;
    println!("{} pages", doc.page_count());

    // 2. Render a page. `RenderOptions::limits` bounds the work.
    let opts = RenderOptions { dpi: 96.0, ..RenderOptions::default() };
    let page = render_page(&doc, &doc.pages()[0], &opts)?;
    std::fs::write("page-1.png", pixmap_to_png(&page.pixmap)?)?;

    // 3. Extract positioned text.
    let text = extract_text(&doc, &doc.pages()[0], &Limits::default())?;
    let plain: String = text.chars.iter().filter_map(|c| c.unicode.clone()).collect();
    println!("{plain}");
    Ok(())
}
```

Editing goes through an `Edit` session obtained from
`Document::edit()`; it can be written as a fresh file
(`Edit::rewrite`) or appended to the original as an incremental
update (`Edit::save_incremental`) — see
[`features/editing-incremental.md`](features/editing-incremental.md)
and [`examples/incremental_save.rs`](../examples/incremental_save.rs).

### Limits

Everything the engine does is bounded by a [`Limits`](../src/limits.rs)
value: `Document::from_bytes_with_limits`, `RenderOptions::limits`,
and the `limits` argument of `extract_text` / `rewrite_with_limits`.
The defaults are generous for ordinary documents (1 GiB of decoded
bytes per operation, 64 Mi rendered pixels, …); tighten them when
processing untrusted input in a constrained environment. Exceeding a
limit is `Error::LimitExceeded { limit, max }`.

### Errors

`Error::kind()` returns one of a small set of stable strings
(`"parse"`, `"structure"`, `"unsupported"`, `"encrypted"`, `"limit"`,
…). `Error::Unsupported { feature, detail }` names the PDF feature
the engine declined to process — a valid file using an unimplemented
feature is distinguishable from a malformed one.

### Warnings

A third outcome sits between a clean result and a refusal: input that
deviates from ISO 32000-1 in a way the engine could still process
*exactly* is accepted and recorded as a `Warning` on the `Document`
(`doc.take_warnings()` drains the list, `doc.warnings()` peeks,
`doc.warnings_dropped()` counts overflow past
`warning::MAX_WARNINGS`). Every operation — render, extraction,
rewrite, incremental save — appends to the same list, so read it
after the operation you care about. The CLI prints each one as
`warning: …` on stderr. `Warning` is `#[non_exhaustive]`; match with
a wildcard arm.

## Feature flag: `cjk-fallback`

Non-embedded Japanese CID fonts render only when the fallback assets
are compiled in. The assets ship with the crate (`assets/cjk/`), so
the feature needs nothing but the flag:

```console
cargo build --release --features cjk-fallback
```

The assets (Adobe-Japan1 CMaps, CID → Unicode table, Noto Sans JP
Regular) and their licences are described in
[`third-party.md`](third-party.md); the behaviour in
[`features/rendering.md`](features/rendering.md).

## Where next

- What is supported and what is refused, feature by feature:
  [`features/`](features/) and [`limitations.md`](limitations.md).
- How the engine is built and why: [`architecture.md`](architecture.md).
- How it is measured against real documents:
  [`compat-corpus.md`](compat-corpus.md).
- Contributing: [`../CONTRIBUTING.md`](../CONTRIBUTING.md).
