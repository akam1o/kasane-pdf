# Releasing

The tagged commit and the crate published on crates.io must contain
the same source. crates.io publishes are permanent (a version can be
yanked, never replaced), so everything is verified before the
irreversible step, and the tag is created only after the package has
been verified from the exact commit it will point at.

Keep it short: run the release gate once on the candidate, and reuse
the latest green nightly (corpus and fuzz) and review results when the
code they covered has not changed since. Changes after the gate that
touch only the CHANGELOG date, release notes, README, comments or links
need CI and the package check, not a new gate; a code change re-runs
the checks it affects. A patch release needs no extra review or corpus
run beyond what its changes call for (`CONTRIBUTING.md`, *Change risk
and verification*). Minimal path: version + CHANGELOG + release notes →
CI and `cargo publish --dry-run` → approval → tag, publish, GitHub
Release → check crates.io and docs.rs.

## 1. Release preparation commit

- `Cargo.toml`: `version`, `rust-version` (verified with that
  toolchain: `cargo +<msrv> check --all-targets --features cjk-fallback`),
  metadata, the `include` list; no `publish = false`.
- `CHANGELOG.md`: move *Unreleased* into a dated version section.
- `README.md`: install line `kasane-pdf = "<major.minor>"`, tag in the
  git alternative.
- `docs/releases/v<version>.md`: the release notes.
- `scripts/fetch-cjk-assets.sh --check` (committed assets match the
  lock).

## 2. CI green

Push the preparation commit; `pr.yml` (test / fmt / clippy, feature
matrix + docs, wasm32 build, dependency audit, compat + round-trip
smoke, crate package check, render fuzz smoke, R2 depth and stack on
Linux) and CodeQL must pass; the latest corpus nightly (`nightly.yml`)
and render fuzz nightly (`fuzz-nightly.yml`) must be green on the same
code (dispatch either on the branch if the scheduled run is older).

### Patch releases from a release branch

A patch for an earlier version (0.4.1 from `v0.4.0` while `main` had
moved on) is cut as `release/v<version>` from that tag, with only the
fix commits and the preparation commit — never `main` — and verified on
its own commit:

- `pr.yml` runs on pushes to `release/**` too (a pull request would
  test a merge with `main`, not the release commit);
- `codeql-release.yml` runs the CodeQL analysis of the repository's
  default setup (actions, python, rust; default suite) on pushes to
  `release/**`, without uploading, and fails on any result that is not
  a reviewed (dismissed) alert. Default setup analyses `main` and pull
  requests only and rejects advanced uploads while enabled, so no ref is
  analysed twice;
- the nightly: `gh workflow run nightly.yml --ref release/v<version>`
  — it checks against the baseline committed on that branch (the one of
  the version being patched), not `main`'s.

The tag goes on the release branch's verified commit; the branch's
CHANGELOG section and release notes are then brought to `main` as a
separate commit (the fix commits are already there).

## 3. Package verification (local, from the clean tree)

```console
cargo package                       # refuses a dirty tree; verifies the build
cargo publish --dry-run
tar -tzf target/package/kasane-pdf-<v>.crate | sort   # review the file list
```

Then, from the extracted `.crate` only (not the working tree): build a
throwaway consumer with `path = <extracted>` for default features and
`--features kasane-pdf/cjk-fallback`, run `cargo test` and
`cargo doc --all-features` inside the extracted crate.
`.cargo_vcs_info.json` inside the crate records the commit; it must
be the commit that gets tagged.

## 4. Approvals

Publishing and tagging are explicit decisions: confirm the commit SHA,
the version, the crate file list and size, and that the crates.io
token is available to the person publishing.

## 5. Tag

```console
git tag -a v<version> -m "kasane-pdf <version>" <sha>
git push origin v<version>
```

Verify `git rev-parse v<version>^{}` equals the SHA recorded in the
crate's `.cargo_vcs_info.json`.

## 6. crates.io publish

```console
cargo publish            # from the tagged commit; token via `cargo login` or CARGO_REGISTRY_TOKEN
```

Confirm on crates.io that the version, the README render and the
dependency list are right.

## 7. GitHub Release

After the crate is on crates.io (a Release never precedes the crate
it describes). The release title is the tag name only (`v<version>`), and the body
is GitHub's generated notes (the commit/PR list and the *Full
Changelog* compare link) with a pointer to `CHANGELOG.md` and the
detailed notes in `docs/releases/v<version>.md`:

```console
gh release create v<version> --title "v<version>" --generate-notes \
  --notes "Release notes: https://github.com/akam1o/kasane-pdf/blob/v<version>/docs/releases/v<version>.md (changelog: https://github.com/akam1o/kasane-pdf/blob/v<version>/CHANGELOG.md)"
```

No binary assets; the source archive is generated by GitHub.

## 8. docs.rs and external verification

- docs.rs builds with `all-features = true` (`[package.metadata.docs.rs]`);
  check that the build succeeded and the `cjk-fallback` items appear.
- From a clean machine or container: `cargo new probe && cargo add
  kasane-pdf@<version>`, build with default features and with
  `--features kasane-pdf/cjk-fallback`, run an example.

## If something goes wrong

- Before `cargo publish`: fix, commit, re-run steps 2–3; move the
  tag only if it has not been pushed (a pushed tag stays; release the
  fix as the next patch version).
- After `cargo publish`: the version cannot be changed. `cargo yank
  --version <v>` hides it from new resolutions (existing lockfiles
  keep working); publish a fixed `<v+patch>`, tag it, and note the
  yank in `CHANGELOG.md`. Never delete a GitHub Release that a crate
  version points at — mark it as superseded instead.
