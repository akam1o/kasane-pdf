# Compatibility backlog

The corpus failures left, ranked for the next work. Source: the macOS
full run at `cdad273` (all four rewrite configurations); 425
`expect: pass` files, Full PASS 347 / 353 / 347 / 380 (default / carry
/ remove-usage-rights / both), Semantic fail 0, crash and timeout 0.
Render SSIM against Poppler over 427 compared files: median 0.9964,
mean 0.9672; below 0.7: 10, below 0.8: 13, below 0.9: 37.

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
| non-embedded fonts named for Chinese system fonts (`simfang-variant`, `xiaobiaosong`, `issue20489`) | 3 | not a missing GB1 fallback: simple TrueType fonts, `/WinAnsiEncoding`, GBK-encoded names (宋体, 仿宋_GB2312), the strings GBK bytes. Per the spec they are single-byte Latin codes; Poppler draws them as mojibake with a substitute. Only an OS font lookup with Windows-style charset behaviour would show the intended text — a policy decision, not adopted |
| non-embedded `Identity-H` CID TrueType (`issue11578_reduced`, `issue12418_reduced`: Times New Roman) | 2 | CIDs are glyph ids of the exact system font; no bundled font can stand in. Needs an opt-in OS / user font source (exact font required) |
| CIDFontType0 with a Type 1 `/FontFile` (`issue11740_reduced`) | 1 | malformed (needs `/FontFile3`); reported as non-embedded; recovery-rule work |
| GB1 / CNS1 / Korea1 fallback fonts | 0 | no corpus file needs one. Noto Sans SC/TC/KR (SIL OFL 1.1) may be bundled, but each is several MB and the crate already carries 4.5 MB of Noto Sans JP (crates.io limit 10 MB): a companion crate or a user-supplied font path |
| ~~annotation `/AP /N` states that are all `null`~~ | 0 | done for real null entries (§7.3.7); `issue15557` has the *name* `/null` as its state values — malformed, still refused |
| FreeText / field values outside Latin with the fallback font | 1 | `freetext_no_appearance` (Arabic): needs a Unicode field font |
| ~~`/BrotliDecode`~~ | 0 | done (EXTN-BROTLI-1; `pdfium-many_rectangles_brotli` passes) |

## Fidelity (pages rendered as exact but differing from Poppler)

| item | files | status |
|---|---:|---|
| glyph placement on the device grid | 140 improved | done: Tr 0 / Tr 4 fill y floored, cached Type 3 glyphs x and y floored, origins in f64 (`chrome-wiki-*` 0.60–0.70 → 0.85–0.96). The "glyph weight" gap once blamed for `chrome-wiki-*` was this offset |
| `bug1844583` | 1 | 0.680 → 0.664. Not the snap's fault: the widget has `/MK /BC` but no `/BS` or `/Border`; Poppler draws no border then (AnnotWidget only builds a border from `/BS`), kasane draws the 1 pt default (§12.5.2). Text position then differs too. Kept: the spec default |
| `bug1552113` (`/Border [0 0 112]` on a 20 pt high link) | 1 | Poppler strokes far outside `/Rect` and covers the page text the file says "should be visible"; pdf.js falls back to width 1; kasane draws nothing. A degenerate border — not adopted |
| `/Link` borders without `/AP` | 7 | done |
| stroke adjustment without `/SA` (IRS forms ×19 at 0.73–0.89, `vertical`, `bug1743245`, `mlp-mixer` chart rules, dotted rules in `issue12963`) | ~25 | Poppler adjusts every stroke and rectangle whatever `/SA` says ("Acrobat apparently hardwires it to true"): thin rules become crisp 1 px black lines. kasane now honours `/SA true` (§10.7.5; 32 corpus files set it, mostly Chrome — little measured change) but keeps the spec default false, so hairlines stay anti-aliased. Adopting Poppler's behaviour is a policy decision. `vertical` is now flagged by the ink-ratio gate: since page sizes round up, Poppler's two 1 px page-edge hairlines are no longer resampled away — the render itself did not change; recorded as a reviewed known flag (`quality-gate.md` § Known flags). Measured with stroke adjustment forced on everywhere (not adopted): `irs-f990` pages 1/3 0.84 → 0.88 / 0.97 → 1.00, `vertical` 0.99 → 1.00 |
| tiling patterns with tiny cells (`issue16038`, `scan-issue16038`, `issue21878`) | 3 | investigated, no kasane defect: on 97 × 52 and 81 × 49 px pages the gap is Poppler's stroke adjustment (the square borders and the 0.4 pt in-cell lines come out as solid 1 px rows) and its pattern-bitmap sampling. kasane's coverage is the analytic one (`issue16038` lines 0.53 px each; `issue21878` stripes 1.77 px per period against 1.89 exact, Poppler 2.0). Tried and dropped: whole-pixel cell spacing for TilingType 1/3 (spec-permitted distortion) — every tiling file got worse against Poppler, which also resamples a ceil-sized cell |
| glyph coverage (`chrome-wiki-*` residual, 0.85–0.96) | many | placement now matches Splash (y floor, x quarter pixel); the rest is FreeType's exact-area coverage against tiny-skia's anti-aliasing |
| Poppler renders wrongly (`bug1552113`, `jbig2_file_header`, `devicen-none-zero-domain`, `bug1743245`, `issue19484_1`) | 5 | reference defects (overflowing border, JBIG2 file header not decoded, invalid function domain, hairlines thickened, page not rendered); kept |

Next: most files below 0.9 now differ from Poppler by its always-on
stroke adjustment, its own defects, or font substitutes and glyph
coverage. Candidates: an opt-in Poppler-compatible stroke-adjustment
mode (default unchanged); exact-coverage glyph rasterisation; an opt-in font
source (system or user-supplied fonts) for the Identity CID and
GBK-named fonts (a decision).
