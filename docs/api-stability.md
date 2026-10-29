# Public API stability — the contract from 0.2 on

kasane-pdf is 0.x: by Cargo's SemVer rules a *minor* bump (0.2 → 0.3)
may break, a *patch* bump (0.2.0 → 0.2.1) may not. 0.2.0 made the
breaking changes that the 0.1 audit scheduled (recorded below and in
`docs/migration-0.2.md`), so that from here on evolution is additive:
`cargo semver-checks check-release --baseline-version <last>` is part
of the release checklist and must report no required update for a
patch release.

## The surface (0.2)

| area | items | notes |
|---|---|---|
| loading | `Document::from_bytes`, `from_bytes_with_limits`, `from_bytes_recovering`, `from_bytes_recovering_with_limits` | `&[u8]` in, owned `Document` out (copies once; the writers and the audit re-read the original bytes) |
| inspection | `pages()`, `page_count()`, `catalog()`, `trailer()`, `resolve()`, `deref()`, `header_version()`, `catalog_version()`, `page_contents()`, `page_operations*()`, `page_resource()`, `resolve_resource*()`; `Page` accessors; `Object` / `Dict` / `IndirectRef` | the object model is the engine's own |
| recovery | `Document::recovery() -> Option<&RecoveryReport>` | `None` for a strict parse, always |
| warnings | `Warning` (`#[non_exhaustive]`), `Document::take_warnings` / `warnings` / `warnings_dropped`, `warning::MAX_WARNINGS` | |
| rendering | `render_page(&Document, &Page, &RenderOptions) -> RenderedPage`; `RenderOptions::{new, with_dpi, with_limits, validate}` with `pub dpi`, `pub limits`; `RenderedPage::{width, height, stride, pixel_format, pixels, into_rgba8, to_png, dpi, page_size_pts, font_substitutions, color_approximations}`; `PixelFormat`; `Color`, `Matrix` | backend-neutral; `pixmap()`, `into_pixmap()`, `Pixmap`, `pixmap_to_png` only with the `tiny-skia-interop` feature |
| text | `extract_text(&Document, &Page, &TextOptions)`; `TextOptions::{new, with_limits, with_hidden_content, with_region}` with `pub` fields; `ExtractedText { chars, omitted }`, `CharInfo`, `ResolveKind` | |
| writing | `rewrite`, `rewrite_with_limits`, `Document::edit*() -> Edit`, `Edit::{add_square, set_info, remove_annotation, set_annotation_entries, set_field_value, rewrite, save_incremental}` | `Edit<'a>` borrows the document; the save methods consume it |
| signatures | `signatures() -> Vec<SignatureInfo>`, `ChangeKind`, `SignatureRule` | detection and policy only; nothing is verified |
| diagnostics / editors | `diagnose_redaction`, `audit_residuals`, `remove_text`, `remove_image_region`, `remove_region(s)`, `remove_document_metadata` and their report types; `named_destinations`, `substitution_for` | experimental editors are documented as such |
| errors | `Error` (12 variants) + `Error::kind() -> ErrorKind`; `ErrorKind::{as_str, is_malformed_input}`; `MissingReference`; `SignatureRule` | `std::error::Error + Send + Sync` |
| limits | `Limits` (49 `pub` fields, `Default`, `Clone`, `new`, `validate`, one `with_<field>` each) | |
| features | `cjk-fallback` (off), `tiny-skia-interop` (on) | `cjk_fallback_enabled()` reports the first at run time |

## Rules for evolving it

1. **Add, don't change.** Every option and result struct is
   `#[non_exhaustive]`; new fields go in with a `with_*` method (and a
   getter for result types) and a default that keeps old behaviour.
   Every classification enum (`Error`, `ErrorKind`, `Warning`,
   `SignatureRule`, `ChangeKind`, `ResolveKind`, `SubstKind`,
   `DestSource`, `TargetKind`, `PixelFormat`) is `#[non_exhaustive]`;
   new variants are additive.
2. **`ErrorKind::as_str()` strings never change.** They are the CLI's
   `error [<kind>]:` output and the compatibility runner's cause ids.
   Message text (`Display`) may change in any release.
3. **No third-party type in the stable surface.** The raster backend
   is reachable only behind `tiny-skia-interop`; the backend-neutral
   accessors are what examples and the README use.
4. **Validation happens at the entry point.** `Limits::validate`,
   `RenderOptions::validate` and the `TextOptions` checks run first and
   return `InvalidArgument`; a limit hit during processing is
   `LimitExceeded`; neither is ever reported as a file fault.
5. **Defaults are part of the contract.** `Limits::default()` values
   are listed in `src/limits.rs`; tightening a default is a breaking
   change (a document that loaded before may not load after).

## What 0.2.0 changed, and why

| change | class | rationale |
|---|---|---|
| `#[non_exhaustive]` on the enums and the option / result structs | breaking | the single change that makes everything after it additive |
| `Error::kind() -> ErrorKind` | breaking | an enum can be matched, hashed and extended; `as_str()` keeps the strings |
| `Error::InvalidArgument`, `Error::SignaturePolicy { rule }` | breaking (new variants were breaking on the exhaustive 0.1 enum) | a caller's mistake and a signature's rule were both reported as file / feature problems |
| `Limits::with_*` + `validate` | additive on its own; needed by `#[non_exhaustive]` | struct literals stop compiling downstream; the builder is the replacement |
| `TextOptions` replacing `&Limits` and `extract_text_all` | breaking | room for text options without a signature change each time |
| `RenderedPage` private fields + accessors; `tiny-skia-interop` | breaking | detaches the stable API from the rasteriser's major version |

## Added in 0.2.1 (additive)

- `Warning::{UndefinedReference, UnterminatedContentBlock,
  UnknownRenderingIntent, IgnoredFilterFragment, RecoveryApplied}`,
  `RecoveryRule` (`#[non_exhaustive]`), `Document::recovery_mode()`,
  `SubstKind::SymbolFallback`, `Limits::max_knockout_objects` +
  `with_max_knockout_objects`, `RenderedPage::image_approximations()`. Behaviour changes on previously refused
  input (undefined references → null, open blocks closed, knockout
  groups rendered, Symbol / ZapfDingbats substituted) are recorded in
  `CHANGELOG.md`; no signature or default changed.

## Open items (additive, any 0.2.x)

- `Document::from_vec(Vec<u8>)` to hand ownership over without a copy.
- Typed `RecoveryReport` notes and typed `color_approximations`
  (today `Vec<String>`); typed `Unsupported` features.
- `TextOptions`: reading-order and grouping modes if a layout stage is
  added; `ActualText` is deliberately always applied.
- `RenderOptions`: background colour, annotation visibility switches.
- A `RenderedPage` constructor for tests outside the crate, if a need
  appears.

## How this is checked

- `cargo semver-checks check-release --baseline-version <last>` on
  every release candidate (`--release-type patch` to force the full
  list when the version was already bumped).
- `tests/public_api.rs` compiles against the public paths only and
  pins the error classification, the recovery contract, the warning
  channel, the option builders and the `RenderedPage` accessors.
- The README's code blocks are doctests (`cargo test --doc`).
