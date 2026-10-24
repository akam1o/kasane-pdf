## What this changes

<!-- One logical change. Say *why*, not only what. -->

## Checklist

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` pass (engine and `compat/runner`)
- [ ] tests added or extended in the file that owns the feature (`tests/README.md`)
- [ ] `docs/features/` updated when support changed; `CHANGELOG.md` *Unreleased* entry added
- [ ] no new dependency, or it is permissively licensed and recorded in `docs/third-party.md`
- [ ] no PDFs, images, reports or logs from the compatibility corpus are committed
- [ ] behaviour on hostile input stays bounded (new unbounded work has a `Limits` field)

## Compatibility impact

<!-- If parsing / rendering / extraction / writing changed: the smoke
run result (`compat/runner … --mode smoke` + `scripts/ci-regression.py
check`), and full-corpus before/after numbers if you have a corpus.
State explicitly if any previously passing file now fails and why. -->
