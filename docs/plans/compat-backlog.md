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
| ~~annotation `/AP /N` states that are all `null`~~ | 0 | done for real null entries (§7.3.7); the suspected file, `issue15557`, has the *name* `/null` as its state values — malformed, still refused |
| FreeText / field values outside Latin with the fallback font | 1 | needs a Unicode field font |
| `/BrotliDecode` | 1 | not in ISO 32000; pdfium-only |

## Fidelity (pages rendered as exact but differing from Poppler)

| item | files | status |
|---|---:|---|
| `/Link` borders without `/AP` | 7 | done: missing ink 0 on the 3 measured files (`file_url_link` SSIM 0.615 → 0.673); open: a border wider than its `/Rect` (`bug1552113`, Poppler strokes outside the rect) |
| Chrome-printed and text pages, glyph baselines | 15 improved | done: Splash floors each glyph origin's device y; kasane now does for unrotated text (35 text files: mean SSIM 0.8035 → 0.8268, none worse) |
| Chrome pages, glyph weight | ~10 | the largest remaining gap on `chrome-wiki-*` (SSIM 0.6–0.7, same ink, no offset): FreeType draws stems thinner and lighter than tiny-skia's coverage. A gamma or hinting choice, not a placement error; no exact fix |
| Type 3 glyph baselines | 3 + (`chrome-japanese-vertical-*` use Type 3 only) | Type 3 glyphs are run as content and not snapped; extending the snap to their origin is the next small step |
| axis-aligned rectangles | many, small | Splash snaps rectangle edges to whole pixels (stroke adjustment); kasane anti-aliases them exactly. Not adopted: a heuristic, not a PDF rule (only `/SA` strokes) |
| `jbig2_file_header`, `issue21878`, `devicen-none-zero-domain`, `bug1844583` | 4 | low SSIM; to investigate one by one |

Next: Type 3 glyph baselines (small, extends the snap), then the
non-embedded Chinese fonts (largest Full PASS gain left, needs a GB1
fallback font and its licensing) and the wide link border. Lower: the
FreeText non-Latin font, `/BrotliDecode`.
