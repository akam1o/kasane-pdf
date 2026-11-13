# Releasing

The tagged commit and the crate published on crates.io must contain
the same source. crates.io publishes are permanent (a version can be
yanked, never replaced), so everything is verified before the
irreversible step, and the tag is created only after the package has
been verified from the exact commit it will point at.

The procedure below is the one used for v0.10.0. In short: candidate
branch → release gate (CI full gate + corpus nightly + local package
checks) on the exact commit → main → tag → `cargo publish` → GitHub
Release → security advisories → checks after publishing.

## 1. Release candidate

Work on a branch `release/v<version>-prep` from `main`:

- `Cargo.toml`: `version`, `rust-version`, metadata, the `include` list;
  no `publish = false`. `cargo update -p kasane-pdf` (and in `fuzz/`,
  whose lock is not committed).
- `CHANGELOG.md`: move *Unreleased* into `## [<version>] - <date>`; keep
  an empty *Unreleased*; update the compare links at the bottom. A
  version prepared but never published is folded into the next one and
  said so ("0.9.0 was prepared but never released"); it is never listed
  as released.
- `README.md`: install line `kasane-pdf = "<major.minor>"`, tag in the
  git alternative.
- `docs/releases/v<version>.md`: the release notes (overview, newly
  supported, fixes, verification, tests, performance, corpus —
  stating that the corpus is one evaluation set, not a success rate —,
  compatibility).
- `docs/api-stability.md`, `docs/limitations.md`, the feature files and
  `docs/redaction-safety-model.md` brought up to date.
- `scripts/fetch-cjk-assets.sh --check`.

Open a pull request; its CI is the fast gate. Merge with `--no-ff` once
it is green.

## 2. Security advisories (when the release fixes vulnerabilities)

Before the release commit:

- one GitHub Security Advisory **draft** per issue
  (`gh api -X POST repos/akam1o/kasane-pdf/security-advisories`):
  affected range as published on crates.io (`>= <first>, < <version>`;
  versions never published are not listed), patched version
  `<version>`, CVSS 3.1 vector, CWE, the affected API and options,
  conditions, workarounds, references to the release notes, CHANGELOG
  and crate version (they resolve once the tag exists);
- one regression test per advisory in `tests/redact_advisories.rs`
  (synthetic inputs, written for `scripts/check-advisory-regressions.py`)
  and its rows in `docs/security/advisory-regressions.md`, measured
  against every published version (the affected range must show `LEAK`,
  the candidate `SAFE`);
- the reproduction material stays outside the repository.

The **security commit** — the CHANGELOG `### Security` section, the
release notes' *Security* section, the advisory tests and table rows —
is made last, on its own branch from the release candidate on `main`.
Pushing it **discloses the vulnerabilities**: their ids, descriptions,
affected versions and the regression inputs that reproduce them become
public before the fixed version is on crates.io. So it is pushed only
once everything else is ready — the release gate passed locally on the
same tree, approval given — and the release then proceeds without a
pause: CI on the pushed commit (minutes), `main`, tag, `cargo publish`,
advisories. Material beyond what the advisories themselves publish
(reproduction files, impact analysis) never enters the repository.

## 3. Release gate (on the exact release commit)

Everything below runs on the commit that will be tagged; a change after
it (code or docs) means running it again on the new commit.

- **CI full gate** — pushing the commit to `release/**` or `main`, or
  `gh workflow run pr.yml --ref <ref>`, runs `pr.yml`'s full gate
  (docs/testing.md, *CI gates*): fast gate, MSRV, semver-checks, the
  redaction model at 10,000 seeds, the packaged crate's own tests and
  `cargo publish --dry-run`. `gate` must be green; CodeQL with no new
  alert (`codeql-release.yml` on `release/**`).
- **Corpus** — `gh workflow run nightly.yml --ref <ref>`: the 511-file
  compatibility run and the redaction corpus in four option sets
  compared with `compat/redaction/` (`scripts/check-redact-corpus.py`).
  When the counts improve, commit the new results as projections only
  (`scripts/redact-results-projection.py`; the uploaded artifact is
  one).
- **Fuzz** — the latest `fuzz-nightly.yml` must be green on the same
  code (dispatch it on the branch if older).
- **Local package checks** (from a clean checkout of the commit):

  ```console
  cargo publish --dry-run
  tar -tzf target/package/tmp-crate/kasane-pdf-<v>.crate | sort   # file list: no corpus, no reproduction material
  tar -xzOf target/package/tmp-crate/kasane-pdf-<v>.crate kasane-pdf-<v>/.cargo_vcs_info.json   # the commit
  ```

- For a release with redaction changes, the external renderer checks
  (MuPDF, Poppler, pdf.js on the synthetic cases of the change) and the
  security regressions against every published version
  (`scripts/check-advisory-regressions.py <old CLI> <dump dir>`).

## 4. Approval

Publishing, tagging and publishing advisories are explicit decisions:
confirm the commit SHA, the version, the crate file list and size, the
advisories' ranges and severities, and that the crates.io token is
available.

## 5. main

Bring the verified commit to `main` without creating a new commit, so
that the tag, `main` and the gated commit are the same:

```console
git merge-base --is-ancestor origin/main <sha>   # must hold
git push origin <sha>:main
```

Wait for `pr.yml` (full gate) on `main` to be green.

## 6. Tag

```console
git tag -a v<version> -m "kasane-pdf <version>" <sha>
git push origin v<version>
git rev-parse v<version>^{}    # = <sha>
```

## 7. crates.io

From a clean checkout of the tag (a worktree, not the working tree):

```console
git worktree add --detach /tmp/rel v<version>
cd /tmp/rel && ./scripts/fetch-cjk-assets.sh --check && cargo publish
```

Then check that `https://crates.io/api/v1/crates/kasane-pdf` lists
`<version>` as the newest and that the downloaded `.crate`'s
`.cargo_vcs_info.json` records `<sha>`.

## 8. GitHub Release

After the crate is on crates.io (a Release never precedes the crate it
describes). Title `v<version>`; the body is the release notes
(`docs/releases/v<version>.md` with links made absolute to the tag),
then GitHub's generated list:

```console
gh release create v<version> --verify-tag --title "v<version>" \
  --notes-file <notes with absolute links> --generate-notes
```

No binary assets; the source archive is generated by GitHub.

## 9. Publish the advisories

Only once `<version>` can be downloaded from crates.io — users must be
able to upgrade when an advisory appears:

```console
gh api -X PATCH repos/akam1o/kasane-pdf/security-advisories/<GHSA-id> -f state=published
```

Check each: state published, severity and vector, affected range,
patched `<version>`, and that every reference opens. The global GitHub
Advisory Database lists them after GitHub's review (hours to days).

## 10. After publishing

- docs.rs (`https://docs.rs/crate/kasane-pdf/<version>/status.json`):
  the build succeeded; the `cjk-fallback` items appear.
- A clean consumer: `cargo new probe && cargo add kasane-pdf@<version>`,
  build and run with default features and with
  `--features kasane-pdf/cjk-fallback`.
- The tag, `main` and the crate's recorded commit are the same SHA.
- Delete the merged release branches.

## Patch releases from a release branch

A patch for an earlier version (0.4.1 from `v0.4.0` while `main` had
moved on) is cut as `release/v<version>` from that tag, with only the
fix commits and the preparation commit — never `main` — and verified on
its own commit:

- `pr.yml` runs its full gate on pushes to `release/**` (a pull request
  would test a merge with `main`, not the release commit);
- `codeql-release.yml` runs the CodeQL analysis of the repository's
  default setup (actions, python, rust; default suite) on pushes to
  `release/**`, without uploading, and fails on any result that is not
  a reviewed (dismissed) alert. Default setup analyses `main` and pull
  requests only and rejects advanced uploads while enabled, so no ref is
  analysed twice;
- the nightly: `gh workflow run nightly.yml --ref release/v<version>`
  — it checks against the baselines committed on that branch (those of
  the version being patched), not `main`'s.

The tag goes on the release branch's verified commit; the branch's
CHANGELOG section and release notes are then brought to `main` as a
separate commit (the fix commits are already there).

## If something goes wrong

- **A gate fails before anything is public**: fix on the branch, commit,
  run the release gate again on the new commit.
- **The security commit was pushed but the gate then fails**: the
  vulnerabilities are public (the Security sections and the regression
  inputs), the fix is not yet on crates.io. Fix and publish the fixed
  version as fast as safely possible; keep the advisories as drafts
  until it is downloadable, then publish them.
- **The tag was pushed but `cargo publish` fails before uploading**
  (network, token, a check): fix the cause outside the source (token,
  network) and publish from the same tag. If the source must change, the
  pushed tag stays — release the fix as the next patch version, tag that,
  and leave the unpublished version out of the CHANGELOG's released
  list.
- **After `cargo publish`**: the version cannot be changed. `cargo yank
  --version <v>` hides it from new resolutions (existing lockfiles keep
  working); publish a fixed `<v+patch>`, tag it, and note the yank in
  `CHANGELOG.md`. Never delete a GitHub Release that a crate version
  points at — mark it as superseded instead.
- **The GitHub Release or an advisory fails to publish**: the crate is
  out, so finish these promptly (they are not ordered against each
  other); an advisory published by mistake before the crate cannot be
  returned to a draft — publish the crate at once.
- **docs.rs fails to build**: a transient failure can be retried
  (docs.rs rebuild request); a failure in the source is fixed in the
  next patch release. The crate itself is unaffected.
