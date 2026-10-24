# Migrating from kasane-pdf 0.1 to 0.2

0.2.0 is the API-stability release: it makes the breaking changes
that `docs/api-stability.md` scheduled so that later 0.2.x and 0.3
releases can *add* without breaking. Behaviour on PDF input is
unchanged — the compatibility corpus renders, extracts and rewrites
identically before and after — and every change below is mechanical.
`cargo semver-checks check-release --baseline-version 0.1.1
--release-type patch` lists the detectable ones; the undetectable ones
(signature changes of `extract_text` and `Error::kind`) are called out
here.

## Checklist

| you wrote in 0.1 | write in 0.2 | why |
|---|---|---|
| `Limits { max_input_bytes: n, ..Limits::default() }` | `Limits::default().with_max_input_bytes(n)` | `Limits` is `#[non_exhaustive]`; one `with_<field>` per field; fields are still `pub` for reading and `limits.max_x = v` still works |
| `RenderOptions { dpi: 144.0, ..RenderOptions::default() }` | `RenderOptions::default().with_dpi(144.0)` (`.with_limits(l)`) | same; `render_page` now validates (`InvalidArgument`) |
| `extract_text(&doc, page, &Limits::default())` | `extract_text(&doc, page, &TextOptions::default())` | options struct with room to grow |
| `extract_text(&doc, page, &limits)` | `extract_text(&doc, page, &TextOptions::default().with_limits(limits))` | |
| `extract_text_all(&doc, page, &limits)` | `extract_text(&doc, page, &TextOptions::default().with_limits(limits).with_hidden_content(true))` | removed; it was one switch |
| `rendered.pixmap` | `rendered.pixels()` (borrow), `rendered.into_rgba8()` (take), `rendered.pixmap()` / `into_pixmap()` (tiny-skia interop, default feature `tiny-skia-interop`) | the raster backend is no longer part of the stable surface |
| `rendered.width`, `.height`, `.dpi`, `.page_size_pts`, `.font_substitutions`, `.color_approximations` | the same names as methods: `rendered.width()`, … (`font_substitutions()` returns a slice) | fields are private; new accessors can be added |
| `pixmap_to_png(&rendered.pixmap)` | `rendered.to_png()` (`pixmap_to_png(rendered.pixmap())` still exists behind `tiny-skia-interop`) | |
| `e.kind() == "unsupported feature"` | `e.kind() == ErrorKind::Unsupported` or `e.kind().as_str() == "unsupported feature"` | `kind()` returns the `ErrorKind` enum; `as_str()` keeps the 0.1 strings and the CLI output is unchanged |
| `match e { Error::Parse(_) => …, Error::Structure(_) => …, /* every variant */ }` | add `_ => …` | `Error`, `ErrorKind`, `ChangeKind`, `ResolveKind`, `SubstKind`, `DestSource`, `TargetKind`, `Warning`, `PixelFormat`, `SignatureRule` are `#[non_exhaustive]` |
| `Err(Error::Unsupported { feature: "signed document", .. })` / `"edit of a signed document"` | `Err(Error::SignaturePolicy { rule, .. })` with a `SignatureRule` | a refusal on the signature's declared terms is its own kind, distinct from an unimplemented feature |
| `Err(Error::Structure(_))` for a bad `dpi` | `Err(Error::InvalidArgument { argument: "dpi", .. })` | caller mistakes are no longer reported as file faults |
| constructing `CharInfo`, `ExtractedText`, `SignatureInfo`, `RecoveryReport`, `FontSubstitution` by literal | not possible outside the crate (they are results) | `#[non_exhaustive]` so fields can be added |

Nothing else changed: `Document`, `Page`, `Object`, `Dict`, `Edit`,
`rewrite`, `signatures`, the warnings channel, the diagnostics and the
experimental editors have the 0.1 signatures.

## Errors in 0.2

```text
Error                      ErrorKind            meaning
NotPdf                     NotPdf               not a PDF
Parse(_)                   Parse                malformed syntax          ┐
UnexpectedEof{..}          UnexpectedEof        truncated                 │ is_malformed_input()
Structure(_)               Structure            impossible structure      │
MissingReference(_)        MissingReference     dangling reference        │
CycleDetected{..}          CycleDetected        reference cycle           ┘
Encrypted                  Encrypted            /Encrypt present
Unsupported{feature,..}    Unsupported          valid PDF, not implemented
LimitExceeded{limit,max}   LimitExceeded        a Limits bound
InvalidArgument{argument}  InvalidArgument      the caller's argument (new)
SignaturePolicy{rule,..}   SignaturePolicy      refused on the signature's terms (new)
Io(_)                      Io                   CLI I/O
```

Not errors: `Warning` (processed exactly, recorded), `RecoveryReport`
(structure guessed, reported), `RenderedPage::font_substitutions()` /
`color_approximations()` (approximations on a successful render).

## Limits in 0.2

- Read fields directly; set them with `with_<field>(value)` or by
  assignment on a `mut` value; construct only through
  `Limits::default()` / `Limits::new()`.
- `Limits::validate()` is called by every entry point. It rejects a
  zero *structural* bound (input size, depths, collection sizes,
  stream and output bytes, operator counts, render buffer) with
  `InvalidArgument`; *feature* bounds (`max_xobject_depth`,
  `max_image_dimension`, `max_pattern_tiles`, the edit, diagnostic and
  audit budgets, …) may be 0 to forbid the feature, exactly as in 0.1.
- Defaults are unchanged.

## RenderedPage in 0.2

```rust,no_run
use kasane_pdf::{render_page, Document, PixelFormat, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Document::from_bytes(&std::fs::read("in.pdf")?)?;
    let page = render_page(&doc, &doc.pages()[0], &RenderOptions::default().with_dpi(96.0))?;
    assert_eq!(page.pixel_format(), PixelFormat::Rgba8Premultiplied);
    let (w, h, stride) = (page.width(), page.height(), page.stride());
    let row0: &[u8] = &page.pixels()[..stride];      // borrowed, no copy
    let _ = (w, h, row0);
    let rgba: Vec<u8> = page.into_rgba8();            // moved, no copy
    let _ = rgba;
    Ok(())
}
```

`pixels()` borrows the buffer and `into_rgba8()` moves it; neither
copies. `pixmap()` / `into_pixmap()` and the `Pixmap` re-export exist
only with the `tiny-skia-interop` feature (on by default) so that a
future rasteriser change can retire them without touching the rest.

## Signatures in 0.2

`signatures()` and `SignatureInfo` are unchanged in content;
`SignatureInfo` is `#[non_exhaustive]` and its documentation now states
on every field that it is a *declaration read from the file*, never a
verification result. `Edit::save_incremental` on a signed document and
`rewrite` of a signed document return `Error::SignaturePolicy { rule,
detail }` where `rule` is one of `ByteRangeNotIntact`,
`DocMdpNoChanges`, `DocMdpFormFillOnly`,
`DocMdpFormFillAndAnnotations`, `FieldMdpNotEvaluated`,
`RewriteOfSignedDocument`. kasane-pdf still does not compute digests
or check certificates: a successful incremental save means the signed
bytes were left in place, not that the signature is valid.
