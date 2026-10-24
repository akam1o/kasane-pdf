# Public API stability — the 0.1 surface audited

kasane-pdf is 0.x: by Cargo's SemVer rules a *minor* bump (0.1 → 0.2)
may break, a *patch* bump (0.1.0 → 0.1.1) may not. This file records
what the audit of the published 0.1.0 API found, what was changed in
0.1.x (additive only), and what is reserved for 0.2.0 with the
migration for each item. `cargo semver-checks check-release
--baseline-version 0.1.0` is part of the release checklist; the 0.1.x
line must report "no semver update required" or additions only.

## The surface

| area | items | notes |
|---|---|---|
| loading | `Document::from_bytes`, `from_bytes_with_limits`, `from_bytes_recovering`, `from_bytes_recovering_with_limits` | `&[u8]` in, owned `Document` out (copies the input once; needed because the writers and the audit re-read the original bytes) |
| inspection | `pages()`, `page_count()`, `catalog()`, `trailer()`, `resolve()`, `deref()`, `header_version()`, `catalog_version()`, `page_contents()`, `page_operations*()`, `page_resource()`, `resolve_resource*()`; `Page` accessors; `Object` / `Dict` / `IndirectRef` | the object model is the engine's own; `Dict` is a `BTreeMap<Vec<u8>, Object>` alias-like newtype with byte-slice keys |
| recovery | `Document::recovery() -> Option<&RecoveryReport>` | `None` for a strict parse, always |
| warnings (new in 0.1.1) | `Warning` (`#[non_exhaustive]`), `Document::take_warnings` / `warnings` / `warnings_dropped`, `warning::MAX_WARNINGS` | additive |
| rendering | `render_page(&Document, &Page, &RenderOptions) -> RenderedPage`, `pixmap_to_png`, `RenderOptions { dpi, limits }`, `RenderedPage { pixmap, width, height, dpi, page_size_pts, font_substitutions, color_approximations }`, `Color`, `Matrix` | `RenderedPage::pixmap` is a `tiny_skia::Pixmap` (re-exported as `kasane_pdf::Pixmap` since 0.1.1) |
| text | `extract_text`, `extract_text_all`, `ExtractedText { chars, omitted }`, `CharInfo`, `ResolveKind` | |
| writing | `rewrite`, `rewrite_with_limits`, `Document::edit*() -> Edit`, `Edit::{add_square, set_info, remove_annotation, set_annotation_entries, set_field_value, rewrite, save_incremental}` | `Edit<'a>` borrows the document; `rewrite`/`save_incremental` consume it |
| signatures | `signatures() -> Vec<SignatureInfo>`, `ChangeKind` | detection and policy only |
| diagnostics / editors | `diagnose_redaction`, `audit_residuals`, `remove_text`, `remove_image_region`, `remove_region(s)`, `remove_document_metadata` and their report types; `named_destinations`, `substitution_for` | experimental editors are documented as such |
| errors | `Error` (10 variants) + `Error::kind() -> &'static str`, `MissingReference` | `std::error::Error + Send + Sync` |
| limits | `Limits` (49 `pub` fields, `Default`, `Clone`) | |
| features | `cjk-fallback` (additive assets; `cjk_fallback_enabled()` reports it at run time) | |

## What the audit found

**Sound as published**

- Errors are *classifiable*: a malformed file (`Parse`, `Structure`,
  `UnexpectedEof`, `MissingReference`, `CycleDetected`), an
  unimplemented feature (`Unsupported { feature, detail }`), a
  resource bound (`LimitExceeded { limit, max }`), encryption
  (`Encrypted`) and I/O are distinct variants with distinct `kind()`
  strings. Recovery is not an error at all but a report on the
  document; approximations are fields on `RenderedPage`; a signature
  policy refusal is `Unsupported` with `feature` naming the signature
  rule — distinguishable, though a dedicated variant would be cleaner
  (see 0.2.0).
- Ownership is simple: everything takes `&Document`; only `Edit` holds
  a borrow and consumes itself on save. No `Rc`/lifetime puzzles leak
  out. `Document` is `!Sync` by design (interior caches) and documented
  as such.
- No internal type is exposed by accident: the `pub mod`s (`audit`,
  `dests`, `edit`, `signature`, `text_extract`, `remove_*`,
  `redact_diag`, now `warning`) contain only report and option types.
  `unsafe` is confined to four documented sites in `src/font.rs`.
- Nothing in the API clones large data on the hot path; `pages()`
  returns a slice, `resolve()` returns an owned `Object` because
  object-stream members are decoded lazily (a borrowed return would
  need a self-referential cache).

**Weak spots** (fixable only with a breaking change, scheduled for
0.2.0)

1. `Limits`, `RenderOptions`, `RenderedPage`, `ExtractedText`,
   `CharInfo`, `SignatureInfo`, `RecoveryReport` have all-`pub` fields
   and no `#[non_exhaustive]`. Adding a field — the most common
   evolution — is a breaking change today, which is why the 0.1.1
   warning channel lives on `Document` instead of on
   `RenderedPage`/`ExtractedText` where it would read more naturally.
2. `Error` is not `#[non_exhaustive]`: a new variant breaks exhaustive
   matches. Nor are `ChangeKind`, `ResolveKind`, `SubstKind`,
   `TargetKind`, `DestSource`.
3. `RenderedPage::pixmap` ties the public API to `tiny-skia`'s major
   version. A `tiny-skia` bump is a kasane-pdf breaking change.
4. `extract_text(&doc, page, &Limits)` takes a bare `Limits` while
   `render_page` takes `RenderOptions { limits, .. }` — inconsistent;
   text extraction has no room for options.
5. `RecoveryReport::warnings: Vec<String>` and
   `RenderedPage::color_approximations: Vec<String>` are strings, not
   typed; `Error::Parse(String)` / `Structure(String)` carry
   diagnostics only as text.
6. `Document::from_bytes(&[u8])` copies; a `from_vec(Vec<u8>)` would
   let callers hand ownership over (additive, can be done in 0.1.x).

## SemVer classification of pending changes

| change | class | status |
|---|---|---|
| `Warning` enum + `Document::take_warnings/warnings/warnings_dropped` + `pub mod warning` | **minor (additive)** — 0.x patch-compatible per `cargo semver-checks` | done in 0.1.1 |
| `pub use tiny_skia::Pixmap` so the type of `RenderedPage::pixmap` is nameable without a second dependency | additive | done in 0.1.1 |
| `/ToUnicode` hex strings tolerate white space; Flate sync-flush acceptance; Flate error text split into incomplete/corrupt | **patch** (behaviour on previously refused input; error *variant* unchanged, message text is not part of the contract) | done in 0.1.1 |
| `Document::from_vec(Vec<u8>)` | additive | candidate 0.1.x |
| `Error::SignaturePolicy { rule, detail }` variant | **breaking** (new variant on an exhaustive enum) | 0.2.0 |
| `#[non_exhaustive]` on `Error`, `Warning` (already), `ChangeKind`, `ResolveKind`, `SubstKind`, `TargetKind`, `DestSource` | **breaking** (exhaustive matches downstream stop compiling) | 0.2.0 |
| `#[non_exhaustive]` + builders / `..Default::default()` for `Limits`, `RenderOptions`; `#[non_exhaustive]` + getters for `RenderedPage`, `ExtractedText`, `CharInfo`, `SignatureInfo`, `RecoveryReport` | **breaking** (struct literals downstream stop compiling) | 0.2.0 |
| `RenderedPage` returns an owned RGBA buffer (`Vec<u8>` + stride) with `pixmap()` behind a `tiny-skia` feature, or `tiny-skia` pinned as a documented public dependency | **breaking** | 0.2.0 |
| `extract_text(&doc, page, &TextOptions)` with `TextOptions { limits, .. }` | **breaking** | 0.2.0 |
| typed `RecoveryReport` notes and `color_approximations` | **breaking** | 0.2.0 |
| `ttf-parser` → `skrifa` | internal, **patch** (no public type involved) | when the conditions in `decisions/ttf-parser.md` hold |

## Migration notes for the 0.2.0 items

- Struct literals `Limits { max_input_bytes: n, ..Limits::default() }`
  keep working under `#[non_exhaustive]` *inside* the crate only; a
  downstream crate must switch to a builder or a `with_*` method —
  both will be provided before the attribute lands, so that code can
  migrate in 0.1.x and compile unchanged on 0.2.0.
- Exhaustive `match` on `Error` gains a `_ =>` arm; `Error::kind()`
  strings stay stable across the change.
- `rendered.pixmap.width()` becomes `rendered.width()`; PNG output
  stays `pixmap_to_png(&rendered)`.

## How this is checked

- `cargo semver-checks check-release --baseline-version <last>` on
  every release candidate.
- `tests/public_api.rs` compiles against the public paths only and
  pins the error classification, the recovery contract and the warning
  channel.
- The README's code blocks are doctests (`cargo test --doc`).
