# Compatibility backlog

The corpus failures left after 0.5.1, ranked for the next work. Source:
the macOS full run at `4e9cf5e` (same library code as 0.5.1); 425
`expect: pass` files, 346 Full PASS in the default configuration.

## Not engine work (79 − 44 = 35 files)

Intended refusals of the default rewrite profile: signed documents 27,
JavaScript name tree 4, catalog entries outside the profile 2, writing
an encrypted document 2. The `carry` and `remove-usage-rights`
configurations already measure the policies that lift them.

## Strict refusals of malformed input (≈ 30 files)

Recovery mode already reads most of them: unbalanced `Q`, operators
inside a path object (§8.2), missing `startxref` / `endobj`, xref
offsets past the end, undefined `/Properties`, image dictionaries whose
size disagrees with the JPEG / JBIG2 frame, deliberately invalid
shadings. Full PASS counts strict results, so these stay failures by
design; changing what strict mode accepts is a policy decision, not a
compatibility fix. Recovery-mode gaps (no `/MediaBox` ×4, an inline
image without `EI`, a corrupt Flate stream, an unknown operator) belong
with the recovery-rule work (phase E).

## Feature gaps, by files unblocked

| item | files | notes |
|---|---:|---|
| non-embedded Chinese fonts (TrueType and CID, GB1 / Identity) | 4 + 2 `any` | needs a bundled GB1 fallback (like `cjk-fallback` for Japan1); Identity-ordering CID fonts cannot be substituted |
| annotation `/AP /N` states that are all `null` | 1 | §7.3.7: a null entry is absent, so there are no states and nothing to draw; currently refused as "states without /AS" |
| FreeText / field values outside Latin with the fallback font | 1 | needs a Unicode field font |
| `/BrotliDecode` | 1 | not in ISO 32000; pdfium-only |

## Fidelity (pages rendered as exact but differing from Poppler)

| item | files | status |
|---|---:|---|
| `/Link` borders without `/AP` | 7 | done: missing ink 0 on the 3 measured files (`file_url_link` SSIM 0.615 → 0.673); open: a border wider than its `/Rect` (`bug1552113`, Poppler strokes outside the rect) |
| Chrome-printed pages (`chrome-wiki-*`) at SSIM 0.6–0.7 | ~12 | to investigate: likely text anti-aliasing and hinting, not missing content |
| `jbig2_file_header`, `issue21878`, `devicen-none-zero-domain`, `bug1844583` | 4 | low SSIM; to investigate one by one |

Next: the null-state annotation fix (small, spec-exact, +1 Full PASS),
then the Chrome-print fidelity investigation (largest remaining group).
