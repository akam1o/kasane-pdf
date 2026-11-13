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

`LEAK`: the CLI returned an output in which the removed text is still
extracted by `pdftotext` or the removed bytes are still in the file
after `qpdf --qdf --object-streams=disable --decode-level=all`, or it
accepted an input it must refuse. `SLOW`: the render took longer than
15 s. `SAFE`: removed, refused, or fast. `N/A`: the CLI of that
version has no such command (`redact` exists from 0.6.0; for GHSA-2337
0.5.x runs `experimental-remove-text`). The PR workflow runs the check
on the current build with `--expect-safe`.

Measured on 2026-11-12 (release builds of each tag; `v0.10.0` is the
release candidate). Every affected range matches the advisory;
`SAFE` before the fix in a few rows means that variant was refused or
handled by that version (it is kept as a guard for later ones).

| advisory | input | v0.5.0 | v0.5.1 | v0.5.2 | v0.6.0 | v0.6.1 | v0.6.2 | v0.7.0 | v0.8.0 | v0.10.0 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| GHSA-2337-4h5g-jqwx | `2337-cm` | LEAK | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-2337-4h5g-jqwx | `2337-form` | LEAK | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-3f9j-35f2-6659 | `3f9j-after-endcmap` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-3f9j-35f2-6659 | `3f9j-bfchar` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-3f9j-35f2-6659 | `3f9j-length` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-3f9j-35f2-6659 | `3f9j-order` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-cjhc-76x8-crrq | `cjhc-contents-attachment` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-cjhc-76x8-crrq | `cjhc-contents-catalog` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-cjhc-76x8-crrq | `cjhc-contents-pieceinfo` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-cjhc-76x8-crrq | `cjhc-link-appearance` | N/A | N/A | N/A | LEAK | LEAK | LEAK | LEAK | SAFE | SAFE |
| GHSA-cjhc-76x8-crrq | `cjhc-stamp-appearance` | N/A | N/A | N/A | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-f228-qvw7-4x37 | `f228-crlf` | N/A | N/A | N/A | LEAK | LEAK | LEAK | SAFE | SAFE | SAFE |
| GHSA-f228-qvw7-4x37 | `f228-shown` | N/A | N/A | N/A | LEAK | LEAK | LEAK | SAFE | SAFE | SAFE |
| GHSA-fm2j-3w72-p7x3 | `fm2j-dash` | SLOW | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-frr2-h56x-mr4h | `frr2-tm` | N/A | N/A | N/A | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-frr2-h56x-mr4h | `frr2-tm-cancelled-by-cm` | N/A | N/A | N/A | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-vph3-9p4m-58fh | `vph3-objstm` | N/A | N/A | N/A | LEAK | LEAK | LEAK | SAFE | SAFE | SAFE |
| GHSA-x455-j82p-6jmv | `x455-image` | N/A | N/A | N/A | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-x455-j82p-6jmv | `x455-text` | N/A | N/A | N/A | LEAK | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-x455-j82p-6jmv | `x455-text-invisible-clip` | N/A | N/A | N/A | SAFE | SAFE | SAFE | SAFE | SAFE | SAFE |
| GHSA-xpph-r5fc-83rj | `xpph-literal` | N/A | N/A | N/A | LEAK | LEAK | LEAK | SAFE | SAFE | SAFE |

| advisory | severity | affected | fixed |
| --- | --- | --- | --- |
| GHSA-fm2j-3w72-p7x3 | Medium | < 0.5.1 | 0.5.1 |
| GHSA-2337-4h5g-jqwx | Low | ≤ 0.5.1 | 0.5.2 |
| GHSA-x455-j82p-6jmv | Medium | 0.6.0 | 0.6.1 |
| GHSA-frr2-h56x-mr4h | Low | 0.6.0 | 0.6.1 |
| GHSA-9mw8-m2cr-vxqm | Low | 0.6.0 – 0.6.1 | 0.6.2 |
| GHSA-f228-qvw7-4x37 | Low | 0.6.0 – 0.6.2 | 0.7.0 |
| GHSA-xpph-r5fc-83rj | Low | 0.6.0 – 0.6.2 | 0.7.0 |
| GHSA-vph3-9p4m-58fh | Medium | 0.1.0 – 0.6.2 | 0.7.0 |
| GHSA-3f9j-35f2-6659 | Low | 0.6.0 – 0.7.0 | 0.8.0 |
| GHSA-cjhc-76x8-crrq | Medium | 0.6.0 – 0.7.0 | 0.8.0 |

GHSA-9mw8 (positions of removed glyphs) is checked by non-interference
in the test itself (two inputs differing only in those positions give
the same bytes); it has no CLI row. The generated non-interference
tests (`tests/redact_model.rs`, the `redact_model` fuzz target) cover
the classes of these advisories at large.
