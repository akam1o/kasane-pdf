# Contributing to kasane-pdf

Thank you for your interest. This page covers how to build, test and
submit changes. Design principles and the module map are in
[`docs/architecture.md`](docs/architecture.md); the feature contract
(what is supported, what is refused and why) is under
[`docs/features/`](docs/features/).

## Ground rules

- **Independent implementation.** No PDF engine (PDFium, MuPDF,
  Poppler, …) may be linked into the crate. They are used only as
  development-time verification tools through their command lines.
- **Permissive licences only.** A new dependency must be MIT /
  Apache-2.0 / BSD / Zlib-style, recorded in
  [`docs/third-party.md`](docs/third-party.md), and justified in the
  pull request. Copyleft code is not accepted.
- **No partial results.** A construct the engine cannot process
  exactly produces a typed `Error`, never a silent skip or a guess.
  Approximations that are unavoidable (colour spaces, font
  substitution) are reported on the result (`color_approximations`,
  `font_substitutions`) and documented.
- **Everything is bounded.** Any new unbounded operation needs a
  `Limits` field with a documented default.
- **No `unsafe`** except the two documented self-referential font
  borrows in `src/font.rs`; hostile input must never panic.
- **Spec references.** Cite ISO 32000-1 section numbers (`§x.y.z`) in
  comments for behaviour taken from the specification.
- Contributions are accepted under the Apache License 2.0
  (`LICENSE`); by submitting a pull request you agree to license your
  contribution under it, including the patent grant in section 3.

## Building

Rust 1.92 or newer (`rust-version` in `Cargo.toml`).

```console
cargo build                      # library + `kasane-pdf` CLI
cargo build --examples           # examples/*.rs
cargo build --features cjk-fallback   # needs assets fetched first (below)
```

The optional `cjk-fallback` feature embeds licence-clean assets that
are not committed:

```console
scripts/fetch-cjk-assets.sh      # → assets/cjk/ (ignored by git), verified against scripts/cjk-assets.sha256
```

The script fetches from commit pins and fails on any checksum
mismatch; `docs/third-party.md` describes how to update the pins and
the lock.

## Tests and lints

Every pull request must pass:

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features cjk-fallback      # after fetching the assets
```

and the same three for the compatibility runner:

```console
cd compat/runner && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`cargo test` runs unit tests, the feature-organised integration tests
under `tests/` (see [`tests/README.md`](tests/README.md)) and the
CLI tests. Fixtures in `tests/fixtures/` are deterministic — regenerate
with `python3 tests/fixtures/make_fixtures.py`; do not hand-edit a
generated fixture.

External verification scripts (need `qpdf`, Poppler tools):

```console
scripts/verify-rewrite.sh                 # qpdf --check on every rewrite output
scripts/verify-audit.sh
scripts/verify-resources-materialize.sh
```

## Compatibility testing

The real-world corpus is not in the repository. The runner works on
any directory of PDFs:

```console
cargo build --release --manifest-path compat/runner/Cargo.toml
compat/runner/target/release/kasane-compat run \
    --corpus tests/fixtures --out /tmp/smoke --with-poppler --mode smoke
python3 scripts/ci-regression.py check /tmp/smoke/latest.json compat/baselines/fixtures-smoke.json
```

`--mode smoke` is what the pull-request workflow runs (a few pages,
low dpi); `--mode full` compares every page with Poppler and is the
nightly configuration. How to set up a corpus store, the manifest
format, and how results are classified are in
[`docs/compat-corpus.md`](docs/compat-corpus.md). A change that
affects parsing, rendering, extraction or writing should come with
the before/after numbers of a full run when the contributor has a
corpus, and must not turn a previously passing file into a failure
without an explanation in the pull request.

## Pull requests

- One logical change per pull request; keep refactors separate from
  behaviour changes.
- Add or extend a test for every behaviour change, in the test file
  that owns the feature (`tests/README.md`).
- Update the feature file under `docs/features/` when support changes,
  and `CHANGELOG.md` under *Unreleased*.
- Commit messages: a short imperative summary line, optionally
  prefixed with the area (`feat(render):`, `fix(parser):`, `docs:`),
  and a body that says *why*.
- Do not commit PDFs, rendered images, reports or logs from the
  compatibility corpus — only aggregates under `compat/baselines/`.

## Reporting bugs

Use the issue templates. For a parsing or rendering problem a
minimal PDF that reproduces it is the most useful thing you can
attach — only attach files you are allowed to redistribute. Security
issues go through [`SECURITY.md`](SECURITY.md), not the public
tracker.
