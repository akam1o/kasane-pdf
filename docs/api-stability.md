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
| loading | `Document::from_bytes`, `from_bytes_with_limits`, `from_bytes_recovering`, `from_bytes_recovering_with_limits`; since 0.3 `Document::open(&[u8], &OpenOptions)` | `&[u8]` in, owned `Document` out (copies once; the writers and the audit re-read the original bytes) |
| inspection | `pages()`, `page_count()`, `catalog()`, `trailer()`, `resolve()`, `deref()`, `header_version()`, `catalog_version()`, `page_contents()`, `page_operations*()`, `page_resource()`, `resolve_resource*()`; `Page` accessors; `Object` / `Dict` / `IndirectRef` | the object model is the engine's own |
| recovery | `Document::recovery() -> Option<&RecoveryReport>` | `None` for a strict parse, always |
| warnings | `Warning` (`#[non_exhaustive]`), `Document::take_warnings` / `warnings` / `warnings_dropped`, `warning::MAX_WARNINGS` | |
| rendering | `render_page(&Document, &Page, &RenderOptions) -> RenderedPage`; `RenderOptions::{new, with_dpi, with_limits, validate}` with `pub dpi`, `pub limits`; `RenderedPage::{width, height, stride, pixel_format, pixels, into_rgba8, to_png, dpi, page_size_pts, font_substitutions, color_approximations}`; `PixelFormat`; `Color`, `Matrix` | backend-neutral; `pixmap()`, `into_pixmap()`, `Pixmap`, `pixmap_to_png` only with the `tiny-skia-interop` feature |
| text | `extract_text(&Document, &Page, &TextOptions)`; `TextOptions::{new, with_limits, with_hidden_content, with_region}` with `pub` fields; `ExtractedText { chars, omitted }`, `CharInfo`, `ResolveKind` | |
| writing | `rewrite`, `rewrite_with_limits`; since 0.5 `rewrite_with(&Document, &RewriteOptions) -> RewriteOutput` with `RewriteOptions` (`#[non_exhaustive]`, `pub` fields, `with_*`), `ExecutableContentPolicy`, `MultimediaPolicy`, `UsageRightsPolicy` (`#[non_exhaustive]`; `Refuse`, `Remove`), `RewriteOutput::{bytes, into_bytes, report, into_parts}` (`#[must_use]`), `RewriteReport` (accessors, `usage_rights()`, `signatures_removed()`), `UsageRightsOutcome`, `UsageRightsRemoval`, `RemovedSignature`, `SigFlagsChange`, `UsageRightsKey`, `SubFilterKind`, `UsageRight`, `OutputCheck` (`#[non_exhaustive]`, read-only fields), `ExecutableItem`, `ExecutableSummary`, `ActionClass`, `ActionKind`, `ActionOwner`, `Trigger`, `ScriptSize`, `MultimediaSubtype`, `UnclassifiedStructure`, `UnclassifiedReason`, `VersionChange`, `VersionReason` (`#[non_exhaustive]`); `Document::edit*() -> Edit`, `Edit::{add_square, set_info, remove_annotation, set_annotation_entries, set_field_value, rewrite, save_incremental}` | `Edit<'a>` borrows the document; the save methods consume it |
| signatures | `signatures() -> Vec<SignatureInfo>`, `ChangeKind`, `SignatureRule` | detection and policy only; nothing is verified |
| diagnostics / editors | `diagnose_redaction`, `audit_residuals`, `remove_text`, `remove_image_region`, `remove_region(s)`, `remove_document_metadata` and their report types; `named_destinations`, `substitution_for` | experimental editors are documented as such |
| redaction | since 0.6 `redact(&Document, &[PageSpec], &RedactOptions) -> Result<Redaction, Error>`; `RedactOptions::{new, with_limits, remove_structure_tree, refuse_copies_elsewhere, remove_annotations}` (since 0.7 `embed_substitute_fonts`, `remove_javascript`; since 0.8 `refuse_uninspected`); `Redaction` (`pub` counts, `output`, `elsewhere`, `uninspected`, `residual`); since 0.8 `uninspected_data`, `DataInventory`; `Elsewhere`, `ElsewhereLocation`, `Removed` | the guarantee is the three statements in `docs/features/redaction.md` |
| errors | `Error` (12 variants) + `Error::kind() -> ErrorKind`; `ErrorKind::{as_str, is_malformed_input}`; `MissingReference`; `SignatureRule`; since 0.5 `Error::refusal_reason() -> Option<RefusalReason>` | `std::error::Error + Send + Sync` |
| limits | `Limits` (49 `pub` fields, `Default`, `Clone`, `new`, `validate`, one `with_<field>` each) | |
| features | `cjk-fallback` (off), `tiny-skia-interop` (on) | `cjk_fallback_enabled()` reports the first at run time |

## Rules for evolving it

1. **Add, don't change.** Every option and result struct is
   `#[non_exhaustive]`; new fields go in with a `with_*` method (and a
   getter for result types) and a default that keeps old behaviour.
   Every classification enum (`Error`, `ErrorKind`, `Warning`,
   `RecoveryRule`, `SignatureRule`, `ChangeKind`, `ResolveKind`,
   `SubstKind`, `DestSource`, `TargetKind`, `PixelFormat`,
   `CryptMethod`, `Authentication`) is `#[non_exhaustive]`;
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

## Added in 0.3.0 (additive; behaviour changes)

- `Document::open(&[u8], &OpenOptions)`, `Document::encryption()`,
  `OpenOptions` (`#[non_exhaustive]`: `new`, `with_limits`,
  `with_password`, `with_recovery`), `EncryptionInfo`
  (`#[non_exhaustive]`, `permission(bit)`), `CryptMethod` and
  `Authentication` (both `#[non_exhaustive]`),
  `Warning::OverlongEncryptionEntry`,
  `RecoveryRule::PaddedShortEncryptionKey`.
- `cargo semver-checks` against 0.2.2: no update required. The minor
  bump is for observable behaviour: encrypted files that open with the
  empty user password are returned instead of `Error::Encrypted`, and
  malformed or unsupported encryption is classified `Structure` /
  `Unsupported` ([`migration-0.3.md`](migration-0.3.md)).
- `Document::open` is the constructor that grows: new open-time options
  go on `OpenOptions`, not into new `from_bytes_*` variants.

## Added in 0.4.0 (additive; behaviour changes)

- `TextOptions::include_annotations` / `with_annotations`,
  `ExtractedText::annotations`, and the new `AnnotationText` and
  `FieldText` (both `#[non_exhaustive]`, re-exported at the crate root
  and from `text_extract`); `AnnotationText::text()`.
- `ResolveKind::FontProgram` and `ResolveKind::CharacterCollection`
  (the enum is `#[non_exhaustive]`; match with a wildcard arm).
- `cargo semver-checks` against 0.3.1: no update required. The minor
  bump is for observable behaviour: codes that were unresolved now
  resolve (never a code that already resolved), pages with a non-Normal
  blend mode and stroked text render differently (corrected), one more
  class of input is refused with `max_pattern_tiles`, and `rewrite`
  carries `/OCProperties`.

## Unreleased (0.9)

- Additive: `Redaction::fields_removed` (the struct is
  `#[non_exhaustive]`). `cargo semver-checks` against 0.8.0: no semver
  update required.

## 0.8.0

- Added: `uninspected_data`, `DataInventory`, `DataItem`, `DataRole`,
  `DataStatus` (all `#[non_exhaustive]`); `RedactOptions::refuse_uninspected`;
  `Redaction::{structure_texts_removed, structure_items_removed,
  uninspected, thumbnails_removed}`.
- Breaking (allowed by a minor bump): `Diagnosis` becomes
  `#[non_exhaustive]` (a struct literal no longer compiles; it is a
  result) and gains `data`. `cargo semver-checks` against 0.7.0 reports
  only that (`struct_marked_non_exhaustive`).
- Behaviour: a structure tree with alternate text is edited instead of
  refused; tagged annotations removed with a rect no longer refuse;
  edited pages lose `/Thumb`; Type 0 codes readers map differently are
  refused; CMap definition order and code length follow the readers;
  soft-masked, Separation and DeviceN images are redacted. See the
  CHANGELOG.

## 0.7.0

- Added: `RedactOptions::{embed_substitute_fonts, remove_javascript}`;
  `Redaction::{fonts_embedded, shadings_removed, javascript_removed}`;
  `TargetKind::Shading`; `Warning::LiteralStringCarriageReturn` (last
  variant, so no discriminant moves); `Limits: PartialEq + Eq + Hash`.
- Breaking (allowed by the 0.6 → 0.7 bump): `Redaction` gains fields
  and is now `#[non_exhaustive]` — a struct literal no longer compiles
  (it is a result: read its fields), and later fields are additive.
  `cargo semver-checks` against 0.6.2 reports only that.
- `Document` stays `Send` and `UnwindSafe`: the fonts it reuses are
  kept per thread for the duration of an operation, not in it.
- Behaviour: unescaped CR / CRLF in literal strings read as LF
  (§7.3.4.2); a rewrite that edits or removes anything refuses a
  document whose objects refer to an input object stream; `redact`
  refuses strings and CMaps readers disagree on, and documents running
  scripts unless `remove_javascript`; see the CHANGELOG.

## 0.6.0

- Added: `redact`, `RedactOptions`, `Redaction`, `Elsewhere`,
  `ElsewhereLocation`, `Removed` (above). Nothing removed or changed;
  `cargo semver-checks` against 0.5.2: no semver update required.
- Behaviour: glyph placement on the device grid, page pixel size
  rounded up, area-averaged minified images, real numbers written
  exactly, Standard 14 fonts without `/Widths` laid out with the AFM
  widths, `/MissingWidth` applied, `/ActualText` refused on any tag by
  the editors and the diagnosis (CHANGELOG, 0.6.0).

## 0.5.1 / 0.5.2 (no API change)

- No public item added, removed or changed. `Limits::default()`
  raises `max_xobject_depth` from 32 to 48: a loosening (a file nested
  33–48 levels deep now renders instead of `LimitExceeded`). Callers
  that set the limit themselves are unaffected.
- `max_render_buffer_bytes` now also bounds the clip and soft masks
  alive at once (an allowance of their own, beside the page and its
  layers). It was unbounded memory before; with masks now shared, only
  many distinct nested clips at a high resolution reach it, and none of
  the 511 corpus files does at 96 dpi. Recorded as a behaviour change,
  not a default change: no default value moves.
- Two inputs that panicked inside the rasteriser are now drawn or
  refused with `Error::Unsupported` (`feature` "stroke width" or
  "geometry extent"), an existing variant; a dashed stroke too costly to
  rasterise (0.5.0 took minutes) is refused with `Unsupported`
  ("dashed stroke").
- `rewrite`, `rewrite_with` and `rewrite_with_limits` do not read
  `max_xobject_depth`: their bytes and errors are unchanged. The
  editors' content walk (`diagnose_redaction`, `remove_region`,
  `remove_image_region`) shares the limit, so it too now follows forms
  up to 48 levels deep.

## Open items (additive)

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
