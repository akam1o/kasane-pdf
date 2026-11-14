# Advisory regressions

Every published advisory has a permanent regression test in
`tests/redact_advisories.rs`, on minimal synthetic inputs with variants
of the construction it was about (no real document's content). The
tests assert the patched behaviour with kasane's own reader; the same
inputs are also judged with tools that share no code with kasane:

```console
KASANE_ADVISORY_DUMP=/tmp/adv cargo test --test redact_advisories
scripts/check-advisory-regressions.py <kasane-pdf CLI> /tmp/adv [--expect-safe]
```

Every published version is judged mechanically, from its crates.io
package (never from this checkout), by `scripts/advisory-history.py`:

```console
KASANE_ADVISORY_DUMP=/tmp/adv cargo test --release --test redact_advisories
scripts/advisory-history.py run --dump /tmp/adv --candidate target/release/kasane-pdf --json /tmp/history.json
```

It downloads each version of `compat/advisories/history.json`, checks
the package's SHA-256 against the crates.io index and its contents
against the git tag (`.cargo_vcs_info.json` must name the tag's commit,
every file must be the tag's), builds it `--locked` with toolchain 1.92
and the default features, and records the binary's SHA-256. Each input
then gets one state per version:

| state | meaning |
| --- | --- |
| `LEAK` | the CLI wrote an output from which the removed data is recovered: text extracted by `pdftotext`, MuPDF or pdf.js, ink left unchanged inside the rect in a Poppler, MuPDF or pdf.js render (text cases), the bytes still in the file after `qpdf --qdf --object-streams=disable --decode-level=all`; or it accepted an input it must refuse |
| `SLOW` | the render took longer than 15 s (`fast` inputs) |
| `removed` | the output exists and none of those witnesses finds the data |
| `refused` | a typed refusal (exit 1, an `error [kind]` message) and no output file |
| `kept` | data that must stay (an action a kept link shares) is still there |
| `N/A` | that version's CLI has no such command (`redact` exists from 0.6.0) |

A crash, a timeout, a witness that fails (`ERROR`) is never any of
these. `removed` and `refused` are not interchangeable: the table
records which one is the expected safe behaviour of each input, and
the run fails on the other (`unexpected_refusal`, `unexpected_success`).
Independently of the table, each advisory's affected range is checked
against the observations: every affected version has an input that
leaks, no other version and not the candidate has one. GHSA-vph3's
range starts at 0.1.0, but before 0.6.0 there is no `redact` command:
those versions are listed as untestable, as is GHSA-9mw8 (an API-level
property, judged by the Rust test only). The table changes only with
`scripts/advisory-history.py update-expected`, which prints every cell
that changes and refuses a failure as an expectation.

Last measured on 2026-11-14 (macOS arm64; the nightly runs it on Linux).
Every affected range matches its advisory; `removed` or `refused` before
the fix in a few rows means that variant was handled by that version
(it is kept as a guard for later ones). `candidate` is the build of the
commit (0.10.0 and later).

| input | v0.5.0 | v0.5.1 | v0.5.2 | v0.6.0 | v0.6.1 | v0.6.2 | v0.7.0 | v0.8.0 | candidate |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `2337-cm` | LEAK | LEAK | removed | removed | removed | removed | removed | removed | removed |
| `2337-form` | LEAK | LEAK | removed | removed | removed | removed | removed | removed | removed |
| `2r8v-default-rgb-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-devicen-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-palette-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-palette-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-palette-string-catalog` | N/A | N/A | N/A | removed | removed | removed | removed | removed | refused |
| `2r8v-tint-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-tint-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `2r8v-transfer-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `3f9j-after-endcmap` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `3f9j-bfchar` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `3f9j-length` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `3f9j-order` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | removed | removed |
| `6pgh-goto-A-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-goto-A-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-goto-A-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-goto-AA-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-goto-AA-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-goto-AA-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-A-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-A-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-A-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-AA-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-AA-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-javascript-AA-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-A-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-A-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-A-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-AA-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-AA-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-launch-AA-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-A-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-A-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-A-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-AA-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-AA-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-named-AA-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-shared-kept` | N/A | N/A | N/A | kept | kept | kept | kept | kept | kept |
| `6pgh-uri-A-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-uri-A-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-uri-A-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-uri-AA-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-uri-AA-next` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `6pgh-uri-AA-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | refused |
| `92wc-link-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-hide` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-nm-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-pieceinfo` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-rc-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-subj-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `92wc-markup-t-catalog` | N/A | N/A | N/A | refused | refused | refused | LEAK | LEAK | refused |
| `cjhc-contents-attachment` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `cjhc-contents-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `cjhc-contents-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `cjhc-link-appearance` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | refused | refused |
| `cjhc-stamp-appearance` | N/A | N/A | N/A | refused | refused | refused | refused | refused | refused |
| `f228-crlf` | N/A | N/A | N/A | LEAK | LEAK | LEAK | refused | refused | refused |
| `f228-shown` | N/A | N/A | N/A | LEAK | LEAK | LEAK | refused | refused | refused |
| `fm2j-dash` | SLOW | fast | fast | fast | fast | fast | fast | fast | fast |
| `fqgr-cff-notdef` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | removed |
| `fqgr-cidfonttype2-bearing` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | removed |
| `fqgr-truetype-bearing` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | removed |
| `fqgr-type1-absent-name` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | LEAK | removed |
| `frr2-tm-cancelled-by-cm` | N/A | N/A | N/A | LEAK | refused | refused | refused | refused | refused |
| `frr2-tm` | N/A | N/A | N/A | LEAK | refused | refused | refused | refused | refused |
| `vph3-objstm` | N/A | N/A | N/A | LEAK | LEAK | LEAK | refused | refused | refused |
| `x455-image` | N/A | N/A | N/A | LEAK | removed | removed | removed | removed | removed |
| `x455-text-invisible-clip` | N/A | N/A | N/A | refused | refused | refused | removed | removed | removed |
| `x455-text` | N/A | N/A | N/A | LEAK | removed | removed | removed | removed | removed |
| `xpph-literal` | N/A | N/A | N/A | LEAK | LEAK | LEAK | refused | refused | refused |
