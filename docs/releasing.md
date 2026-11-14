# Releasing

Three things are always the same commit: the one the release gate
verified, the one the tag points at, and the source of the crate on
crates.io (its `.cargo_vcs_info.json` records it). crates.io publishes
are permanent — a version can be yanked, never replaced — and a pushed
tag is never moved or deleted, so everything is verified before those
steps. What is written after publication (a CHANGELOG *Security*
section, advisory regression tests) goes into a **new** commit on
`main`; the GitHub Release's body may be edited, the tag and the crate
never are.

- **A. Normal release** — no undisclosed vulnerability is fixed.
- **B. Security release** — the release fixes vulnerabilities that are
  not public yet. Same gate, but prepared privately and published so
  that the details become public only once the fixed version can be
  downloaded.

## The release gate

Every row must have passed **on the commit that will be tagged**; a
change after it (code or docs) means running it again on the new
commit. A run on another commit — last night's nightly on `main`, a
pull request's run — does not count. Never record a check as passed
that did not run on this commit.

| check | where it runs | counts when |
| --- | --- | --- |
| verify-redact (35 cases), advisory regressions (`--expect-safe`, every advisory's inputs `SAFE`), redaction model (160 seeds), fuzz smoke, engine tests, docs, wasm, audit | `pr.yml` fast gate (part of the full gate) | `gate` green on a push or dispatch of the commit |
| MSRV, semver-checks, redaction model at 10,000 seeds, packaged crate's tests, `cargo publish --dry-run` | `pr.yml` full gate | as above — `scripts/release-check.py <sha>` checks it |
| CodeQL, no new alert | `codeql-release.yml` on pushes to `release/**` (or `gh workflow run codeql-release.yml --ref <ref>`): it fails on any result that is not a reviewed alert. The default setup's analysis on `main` and pull requests does not count — its success means the analysis ran | green on the commit — `scripts/release-check.py` checks it |
| redaction corpus (117 runs × four option sets), 511-file compatibility run, external renderers (Poppler, MuPDF, pdf.js on every verify-redact case, every advisory input and the XFA / shading / pattern / widget cases), long fuzzing (three targets, 900 s each), redaction model at 10,000 seeds, verify-redact and advisory regressions | locally: `scripts/release-verify.py run --ref <sha>` (*Local release verification* below) | its `release` run proves `<sha>` passed — `scripts/release-check.py <sha> --local-results DIR` checks it |
| the same corpora in CI | `gh workflow run nightly.yml --ref <ref>` (A, before; B, after publication) | green on the commit: `--require-nightly` (A) |
| the advisory regressions against every published version (`scripts/check-advisory-regressions.py <old CLI> <dump dir>`, affected range `LEAK`, the candidate `SAFE`) | locally, by hand (old CLIs are built from their tags) | a security release; the commands and results recorded with the SHA |
| package contents | locally, from a clean checkout of the commit: `cargo publish --dry-run`, then `tar -tzf target/package/tmp-crate/kasane-pdf-<v>.crate` (no corpus, no reproduction material) and its `.cargo_vcs_info.json` (the commit) | on the commit |

## Branch protection on `main`

Required checks, strict (the branch must be up to date):
`test / fmt / clippy`, `compat + round-trip smoke (fixtures)`, `gate`,
`workflow guard` (all GitHub Actions). `gate` is `pr.yml`'s last job: it
passes only when every job of its level succeeded
(`scripts/ci-gate.py`); `workflow guard` (`pr-guard.yml`) checks a pull
request's workflows with the base branch's definition, so a pull
request cannot weaken `gate` and report it green (docs/testing.md, *CI
gates*). Force pushes and deletion are off. Administrators are not
included (`enforce_admins` off): the maintainer can still push to
`main` — used only to fast-forward it to a release commit whose full
gate is green (A.3, B.3), and to land a change the guard refuses by
design (a fork's change to the CI). Everything else is merged through
the pull request (`gh pr merge <n> --merge`), never by a local merge
pushed to `main`.

`pr-guard.yml` runs on `pull_request_target`, which GitHub blocks by
default in public repositories (from 2026-12-02). A repository Actions
policy allows it for that workflow only (Settings → Actions → Policies;
`gh api repos/akam1o/kasane-pdf/actions/policies`): workflow path
`.github/workflows/pr-guard.yml`, allowed events `pull_request_target`,
enforcement active. Without it the guard does not run and, being
required, blocks every merge — restore the policy, or drop `workflow
guard` from the required checks below until it is back.

If the protection itself is wrong (a check that never reports blocks
every merge, a check name changed):

```console
# what it is now
gh api repos/akam1o/kasane-pdf/branches/main/protection/required_status_checks
# back to the two original checks (the state before `gate` was required)
gh api -X PATCH repos/akam1o/kasane-pdf/branches/main/protection/required_status_checks \
  --input - <<'EOF'
{"strict": true, "checks": [
  {"context": "test / fmt / clippy", "app_id": 15368},
  {"context": "compat + round-trip smoke (fixtures)", "app_id": 15368}]}
EOF
# the current set: add {"context": "gate", "app_id": 15368} and
# {"context": "workflow guard", "app_id": 15368} to the list above
```

The repository owner can always change the protection, whatever it
requires; a single pull request blocked by a check that cannot report
can be merged with `gh pr merge <n> --merge --admin` once its other
checks are verified by hand.

## A. Normal release

### A.1 Release candidate

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

Push the branch: a push to `release/**` runs the full gate and CodeQL
on that commit. Dispatch the corpus on it:
`gh workflow run nightly.yml --ref release/v<version>-prep`. Run the
local verification on the same commit:
`scripts/release-verify.py run --ref <sha>` (about 30 minutes).

### A.2 Release gate and approval

Run the rows of *The release gate* on the branch's head `<sha>`, then

```console
scripts/release-check.py <sha> --require-nightly --local-results <DIR of the run>
```

Approval is an explicit decision: the SHA, the version, the crate's file
list and size, and that the crates.io token is available.

### A.3 main, tag, crates.io

```console
scripts/release-check.py <sha> --require-nightly --local-results <DIR>   # once more, right before
git push origin <sha>:main                           # a fast-forward: main = <sha>
git tag -a v<version> -m "kasane-pdf <version>" <sha>
git push origin v<version>
git rev-parse v<version>^{}                          # = <sha>
git worktree add --detach /tmp/rel v<version>
cd /tmp/rel && ./scripts/fetch-cjk-assets.sh --check && cargo publish
```

The push to `main` runs `pr.yml` once more on the same commit; it is not
waited for (the release branch's run is the gate), but a failure there
is handled as in *If something goes wrong*.

Then: `https://crates.io/api/v1/crates/kasane-pdf` lists `<version>`;
`curl -sL https://static.crates.io/crates/kasane-pdf/kasane-pdf-<version>.crate | tar -xzO kasane-pdf-<version>/.cargo_vcs_info.json`
records `<sha>`.

### A.4 GitHub Release, after publishing

After the crate is on crates.io (a Release never precedes the crate it
describes). Title `v<version>`; the body is the release notes with links
made absolute to the tag, then GitHub's generated list:

```console
gh release create v<version> --verify-tag --title "v<version>" \
  --notes-file <notes with absolute links> --generate-notes
```

No binary assets. Then:

- docs.rs (`https://docs.rs/crate/kasane-pdf/<version>/status.json`):
  the build succeeded; the `cjk-fallback` items appear.
- A clean consumer: `cargo new probe && cargo add kasane-pdf@<version>`,
  build and run with default features and with
  `--features kasane-pdf/cjk-fallback`.
- Delete the merged release branches.

## B. Security release

The goal: nothing that tells a reader how to exploit a vulnerability is
public before users can install the fix, and the time between the fix's
code becoming public and the advisories being published is the CI run
of one commit and the publish itself (minutes).

What becomes public when:

| information | where | public |
| --- | --- | --- |
| the fix's code | release commit R | when R is pushed (B.3), minutes before the crate |
| a general, true statement: the version fixes security issues in redaction, and advisories with the details will be published once it is on crates.io | R's CHANGELOG entry and release notes | with R |
| the advisory ids, affected versions, conditions of the leak, severity | the GHSAs | once the fixed crate is downloadable (B.4) |
| the GitHub Release's *Security* section (links to the advisories) | the Release body | right after the advisories |
| the CHANGELOG *Security* section in full, the advisory regression tests and their table rows | commit S on `main` | after the advisories |
| reproduction files, impact analysis, exploit steps | never in the repository | never (the advisory says what users need) |

The R entry is never false: it says that security issues are fixed and
that advisories with the details will follow; it does not describe the
fix as something else. The short window is a mitigation, not secrecy:
the fix's diff is public from B.3 on, and a careful reader may infer
from it what was wrong. The crate published from R carries that entry —
its CHANGELOG and docs do not list the advisories, and its tests do not
include the new regression tests (they come with S). Both are said in
the GitHub Release.

### B.1 Private preparation

- One GitHub Security Advisory **draft** per issue
  (`gh api -X POST repos/akam1o/kasane-pdf/security-advisories`, or the
  Security tab): affected range as published on crates.io
  (`>= <first>, < <version>`; versions never published are not listed),
  patched version `<version>`, CVSS 3.1 vector, CWE, the affected API
  and options, the conditions, workarounds, and references to the crate
  version and the GitHub Release (they resolve once published).
- The fix, its regression tests (synthetic inputs, one per advisory, in
  `tests/redact_advisories.rs` for
  `scripts/check-advisory-regressions.py`), the table rows of
  `docs/security/advisory-regressions.md`, the CHANGELOG *Security*
  section and the release notes' *Security* section are written in a
  **private** place: a local branch that is never pushed, or the
  advisory's **temporary private fork** (Security tab → the draft →
  *Start a temporary private fork*) when someone else must review or
  contribute. GitHub Actions do not run in a temporary private fork, so
  it is for review only — the verification is B.2, and the commit
  published is pushed from the local clone (B.3), never merged from the
  fork's page (that would publish a commit no gate verified).
- Reproduction material and the impact analysis stay outside the
  repository (`~/kasane-private/`).

Two commits come out of it, R on top of the current `main` and S on top
of R:

- **R — the release commit**: the fix (code only), the version bump,
  the CHANGELOG entry and release notes with the general statement, all
  non-security changes of the release. Its message describes the
  release, not the vulnerability ("Release 0.11.0", "redact: …" in
  general terms); private fix history is squashed into it so that no
  pushed commit message says more.
- **S — the disclosure commit**: the regression tests, the table rows,
  the full CHANGELOG *Security* section and release notes' *Security*
  section, with the advisory ids. `git diff R S --stat` must list only
  `tests/`, `docs/` and `CHANGELOG.md` — no source file: the code
  verified with S's tests is R's.

### B.2 Private verification

Three sets of checks, each recorded with the SHA it ran on. None is
reported as run on a commit it did not run on.

**(a) On R itself**, locally, before anything is pushed:

```console
scripts/release-verify.py run --ref R            # the release profile: corpora, renderers, fuzz, model, regressions
scripts/release-verify.py check --ref R --results <DIR>
```

and, by hand from a clean checkout of R, the CI's own commands of the
fast and full gate (the `run:` lines of `pr.yml`: fmt, clippy and the
tests per feature set, MSRV, semver-checks, `cargo publish --dry-run`
and the packaged crate's tests — CI runs them again on R in B.3) and the
package contents of R's crate: its file list (no corpus, no
reproduction material, no new advisory input) and its
`.cargo_vcs_info.json` (R).

**(b) On S**, for what only S contains — the new advisories'
regression inputs:

```console
scripts/release-verify.py run --ref S --profile disclosure   # verify-redact, every advisory input SAFE
```

and, by hand, every affected published version judged `LEAK` on S's
inputs. This is evidence about R's code only because the code is
identical: `release-check.py --disclosure S` refuses an S that changes
anything but `tests/`, `docs/` and `CHANGELOG.md` after R. It is the one
exception to "on the commit that will be tagged", and it is kept apart:
S's run is of S, under the `disclosure` profile; it does not count for
R, nor R's for S.

**(c) In public, on R**: the full gate and CodeQL when R is pushed
(B.3), and the nightly on the tag after publication (B.5). The corpus
row of the release gate is satisfied by (a) on R; the nightly on R
confirms it in CI.

### B.3 Final check and publication window

Before starting — everything ready, one sitting, no pause:

- approval: SHA of R, version, crate file list, each advisory's range,
  severity and text;
- the crates.io token works (`cargo owner --list kasane-pdf`);
- `main` has not moved since R was made (`git fetch origin && git
  merge-base --is-ancestor origin/main R`); if it has, rebase R and S
  and run B.2 again — finding out after R is public costs a second
  window.

Then:

```console
git push origin R:refs/heads/release/v<version>    # code public from here; full gate + CodeQL on R
gh run watch <the pr.yml run of R>                  # ~6 minutes
gh run watch <the codeql-release.yml run of R>      # ~5 minutes, in parallel
scripts/release-check.py R --local-results <DIR of R> --disclosure S --disclosure-results <DIR of S>
                                                    # full gate and CodeQL green on R; R and S verified; main fast-forwards
git push origin R:main
git tag -a v<version> -m "kasane-pdf <version>" R && git push origin v<version>
git worktree add --detach /tmp/rel v<version>
cd /tmp/rel && ./scripts/fetch-cjk-assets.sh --check && cargo publish
```

### B.4 Disclosure — once the crate can be downloaded

1. The fixed crate is downloadable:
   `cargo info kasane-pdf@<version>` from a clean directory, and the
   `.crate` from `static.crates.io` records R in
   `.cargo_vcs_info.json`.
2. Publish every advisory, one after another without a pause:
   `gh api -X PATCH repos/akam1o/kasane-pdf/security-advisories/<GHSA-id> -f state=published`;
   check each (state, severity and vector, affected range, patched
   version, references).
3. The GitHub Release (A.4's command), its body the release notes plus
   the *Security* section: each advisory's id and link, and that the
   crate's own CHANGELOG has the general entry only.
4. S: rebase it on `main` if needed (the diff stays tests/docs/
   CHANGELOG), open a pull request, merge it when green. The tag stays
   on R.

### B.5 After publishing

- `gh workflow run nightly.yml --ref v<version>` — the corpus on R in
  CI, confirming the local run.
- docs.rs, the clean consumer, branch clean-up as in A.4.
- The global GitHub Advisory Database lists the advisories after
  GitHub's review (hours to days); check that its entries match.

## Local release verification

`scripts/release-verify.py` runs the part of the gate CI cannot — the
private corpora need the local store, the long fuzzing and the external
renderers are too long or too tool-heavy for every push — on one commit,
records it, and checks the record.

```console
scripts/release-verify.py run --ref <sha> [--out DIR] [--profile release|disclosure] [--corpus DIR]
scripts/release-verify.py check --ref <sha> --results DIR [--profile release]
scripts/release-verify.py report --results DIR [--public OUT.json]
```

**What `run` does.** It refuses to start from a checkout with
uncommitted changes, checks the commit out into a fresh worktree
(`DIR/src`), and runs that commit's own `scripts/release-verify.py`
with that commit's `scripts/release-verify.json` (the profiles, each
check's timeout and parameters). Each check of the profile runs with its
own timeout; a failure does not stop the others; a check whose tools are
missing, or whose corpus is incomplete (every file of
`compat/manifest.yaml` must be in the corpus directory with its
SHA-256), is not run and says why. Before and after each check the
worktree must still be the commit, unmodified. The checks:

| check | what | passes when |
| --- | --- | --- |
| `security_regressions` | `scripts/verify-redact.py`; every advisory input with `--expect-safe` | all cases as expected, at least 35 and 44 |
| `noninterference` | `tests/redact_model.rs` at 10,000 seeds, release build | the test passes |
| `redact_corpus` | `scripts/eval-redact-corpus.py` in four option sets → projection → `scripts/check-redact-corpus.py --strict-pixels` against `compat/redaction/` | 117 runs each; no regression (an `ok` run no longer `ok`, an unexpected run, more words, characters or pixels lost outside the rect, a run missing or new); improvements and changed refusal reasons are listed separately and never make it pass |
| `compat_corpus` | `kasane-compat run` built with `cjk-fallback` as `nightly.yml` builds it, over the 511 files (Poppler witness, 96 dpi, 20 s per file, full mode, `--rewrite-options carry`) → `scripts/ci-regression.py check` against `compat/baselines/corpus-full.json` | 511 files on both sides (else `incomparable`), no crash or timeout, no regression |
| `external_renderers` | `scripts/verify-renderers.py` (Poppler, MuPDF, pdf.js from `tools/pdfjs`) | no leak or text loss; drawing outside the rect no worse than `compat/renderers/baseline.json` for the same renderer versions (other versions: `incomparable`) |
| `fuzz` | `fuzz/ci-run.sh` for render_content, render_graph and redact_model, 900 s each in parallel, from the committed seeds | no crash, timeout or false success; at least 95 % of the time ran (else `incomplete`); inputs beyond the seeds executed |

Statuses: `passed`, `failed`, `timeout`, `error` (could not run),
`incomplete`, `incomparable`, `interrupted`. Only `passed` counts.

On macOS the run holds off idle sleep (`caffeinate -i`); a machine that
sleeps anyway stretches the wall-clock time (a timeout or a corpus file's
20 s limit can then fail, which is a failure, never a pass). Every
`cargo` command runs `--locked`: a stale lock fails the check
instead of being rewritten in the worktree (which would fail the next
check as a modified worktree).

**Requirements.** git, Python 3, rustup with the stable toolchain and
the fuzz toolchain of the config (`nightly-2026-10-29`) with
`cargo-fuzz`, qpdf (with `fix-qdf`), Poppler (`pdftoppm`, `pdftotext`,
`pdfinfo`), MuPDF (`mutool`), Node.js with npm (`tools/pdfjs` is
installed with `npm ci`), and the corpus (`compat/corpus`, or
`--corpus`; docs/compat-corpus.md). macOS and Linux run the same
commands; the renderer baseline records the renderer versions it was
made with.

**Where results go.** `DIR` (default
`~/.cache/kasane-pdf/release-verify/<sha12>-<time>/`, never inside the
checkout; a new directory each run): `result.json` (written last),
`checks/<name>.json`, `logs/<name>*.log` (every log a check writes,
each recorded with its digest), `work/`. They are **private**: logs and
`work/` name corpus files and quote tool output. Only `report --public`
is publishable — statuses, counts, durations and version numbers, from a
closed vocabulary; no ids, paths, commands, rectangles or text. Nothing
of `DIR` is uploaded anywhere.

**What `check` verifies.** `result.json` and every
`checks/<name>.json` parse completely with exactly the expected fields
and known statuses; `commit` and `tree` are `<sha>`'s; the verifier and
config blobs are those in `<sha>`; the profile is the one asked for and
every one of its checks is there, `passed`, with the commit's
configuration; every file's SHA-256 matches the one recorded and every
file is one run's (one `run_id`); the run finished (no `RUNNING`) less
than `max_age_days` (30) ago; and the local ledger
(`~/.cache/kasane-pdf/release-verify/ledger.jsonl`, appended under a
lock; its order, not the clock, says which run is newer) has no newer
run of the same commit and profile — an older success does not count
once a later attempt failed or was interrupted (a `--only` run is
`partial` and supersedes nothing). Files are opened by fixed names only,
without following symbolic links.

**What it does not prove.** The digests catch accidents — another
commit's results, a partial or interrupted run, an edited or truncated
file, a stale run. They are not signatures: whoever can write `DIR`
and the ledger can make a consistent forgery. The evidence is as
trustworthy as the machine and the person who ran it.

**When it fails.** Read `report` and the check's log in `DIR/logs/`;
fix the cause (a tool, the corpus, the code); run it again into a new
directory — the new run supersedes the old one. A tool upgraded since
the renderer baseline makes `external_renderers` `incomparable`: check
the differences, then re-baseline in a commit of its own
(`scripts/verify-renderers.py <kasane> <dump> --json /tmp/x.json
--write-baseline compat/renderers/baseline.json`). A changed corpus
baseline is committed as projections only. `--only <check>` runs a
subset for trying things out; its results are `partial` and never
accepted.

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

`main` is not moved to it: `scripts/release-check.py <sha>
--require-nightly --no-main --local-results DIR` checks the gate without
the fast-forward condition. A tag older than `scripts/release-verify.py`
has no verifier of its own to run: its local gate is recorded by hand,
and `--legacy-local` (instead of `--local-results`) says so. The tag goes on the release branch's verified commit; the
branch's CHANGELOG section and release notes are then brought to `main`
by a pull request (the fix commits are already there).

## If something goes wrong

For each case: whether to go on, what can be retried, what cannot be
undone, when users must be told, and when it needs a person.

| case | go on? | retry | cannot be undone | tell users | by hand |
| --- | --- | --- | --- | --- | --- |
| **The full gate fails on R** (before the tag) | stop | an infrastructure failure (runner lost, download error): `gh run rerun <id> --failed`, then `release-check.py` again | in B, R's code is public — the advisories stay drafts | no (nothing released) | a real failure: fix, new R (and S), B.2 again, push the new R; keep the window short |
| **CI fails on `main` after the push** (same SHA, gate was green on the branch) | the gate already passed on R: a flake is re-run; a real difference (a cache, a runner change) is investigated | `gh run rerun --failed` | `main` = R stays (no force push) | only if the version is published and the failure is real | seen before the tag: stop and fix as above; tagged but not published: the version is skipped if the source must change (the tag stays); published: fix forward in a patch release |
| **The tag push fails** | stop until it is pushed | the push (network, permission) — after an unclear failure, `git ls-remote origin refs/tags/v<version>` first | — | no | a tag pushed on the wrong commit is never moved or deleted: that version is skipped (not published), the next patch version is released, the CHANGELOG says the skipped one was never released |
| **`cargo publish` fails** | stop the disclosure | before upload (a check, network, token): fix outside the source, publish again from the same tag | the tag (pushed), and the version if the upload went through | no | check `https://crates.io/api/v1/crates/kasane-pdf/<version>` before retrying (an upload may have succeeded and only the wait failed); a fix in the source means a new version (the tag stays, the version is skipped) |
| **The crate cannot be downloaded** (index or CDN delay, crates.io incident) | wait; advisories stay drafts | `cargo info`, the sparse index (`https://index.crates.io/ka/sa/kasane-pdf`) and the `.crate` URL, every few minutes | the version is published | no, while the details are private | beyond an hour: status.crates.io, then the crates.io team |
| **The GitHub Release fails** | go on (the advisories do not depend on it) | `gh release create` again, or `gh release edit` a half-made one | — | no | the advisories' Release link resolves once it exists |
| **Only some advisories are published** | go on at once | the PATCH for each draft one (or the Security tab) | a published advisory cannot return to draft | the Release and S list the published ones; add the rest as they are published | an API refusal (a missing field): fix the draft in the Security tab |
| **An advisory has a mistake** | go on | edit the published advisory (`gh api -X PATCH …` or the Security tab): description, ranges, severity, CWE, references | it was public with the mistake; GitHub's database copy updates after review | yes, when the mistake understates the affected range or the severity: a dated correction in the advisory, the Release and the CHANGELOG (new commit) — never an edit that hides the earlier text was wrong | a withdrawal (not a vulnerability): GitHub Support |
| **docs.rs fails** | go on | a transient failure: a rebuild (docs.rs owner rebuild, or its issue tracker) | — | no | a failure from the source: fixed in the next patch release (a rebuild uses the same source) |
| **A released version is wrong** (after `cargo publish`) | stop recommending it; a new patch release goes through the whole gate | — | the version | yes: the Release says what is wrong and which version fixes it; `cargo yank --version <v>` when using it is harmful (existing lockfiles keep working); the yank noted in the CHANGELOG | deciding to yank; never delete a GitHub Release a crate version points at — mark it superseded |
| **An advisory was published before the crate** | finish the release as fast as the gate allows — never by skipping a failing check | the publication steps, as above | the disclosure | yes: the advisory says the patched version is coming and, if there is one, the workaround; updated when it is out | if the crate cannot be published soon (a failing gate, crates.io down): the workaround in the advisory, by hand |
