# Testing & verification

## Layout

Tests are organised by **feature**, not by milestone — see
[`../tests/README.md`](../tests/README.md) for the authoritative map
of `tests/*.rs` → the area it exercises and where to add a new case.

- `tests/parse_*.rs`, `render_*.rs`, `text_*.rs`, `rewrite_roundtrip.rs`,
  `edit_annotations.rs` — engine-level integration tests.
- `tests/cli_*.rs` — CLI integration tests, one file per subcommand
  group (`rewrite`, `annotate`, `inspect`, `preflight`, `remove`,
  `audit`/multi-page/metadata, `remove_jpeg`).
- `tests/common/mod.rs` — shared CLI helpers (`bin`, `tmpdir`,
  `fixture`, `minimal_pdf`, `blocker_page_pdf`).
- `tests/fixtures/` — hand-crafted, deterministic PDFs
  (`make_fixtures.py`) plus a few byte-pinned binaries.

## Running

```sh
cargo test                 # unit + integration tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Fixture tiers

Two fixture regimes are kept distinct so a future real-world corpus
drops in cleanly:

- **Deterministic fixtures** (`tests/fixtures/`): small, generated
  PDFs for specific features/edge cases, committed to Git, run on
  every `cargo test`.
- **Real-world corpus** (planned): large PDFs produced by real
  applications, held out-of-repo behind a manifest for compatibility
  testing. Not yet introduced — when it lands it goes under
  `tests/compatibility/` + a manifest, separate from `fixtures/`.

## External verification

The engine's own tests never depend on another PDF implementation,
but external tools are used to *confirm* behaviour on real files:

- [`../scripts/verify-rewrite.sh`](../scripts/verify-rewrite.sh) —
  `qpdf --check` on the rewrite output of every well-formed fixture;
  requires exit 0 and zero warnings.
- `pdfimages`, `pdftoppm`, `djpeg` — used in M27/M28 verification to
  confirm extracted samples and rendered edit positions; the clone's
  decoded bytes are verified in-process so decoder differences are
  not confused with real edits.
