# Real-world PDF compatibility corpus

kasane-pdf measures engine maturity against real, in-the-wild PDFs —
not just the hand-crafted `tests/fixtures/`. The corpus lives in an
**S3-compatible object store** (Cloudflare R2, AWS S3, MinIO,
Backblaze B2, …) and is downloaded on demand; PDFs are never committed
to git, and no endpoint, bucket, or credential is hard-coded in the
repo.

- Layout: `<prefix>/<category>/<sha256>.pdf` (prefix defaults to `pdfs`)
- Manifest: [`../compat/manifest.yaml`](../compat/manifest.yaml) —
  the single source of truth (id, sha256, category, source, license,
  `expect` for the Compatibility/Robustness split). The manifest names
  sources and licences but **no** endpoint or credentials.

## Configuration

Copy [`../compat/corpus.env.example`](../compat/corpus.env.example) to
`.env` (git-ignored) or export the variables:

| variable | meaning |
|---|---|
| `KASANE_CORPUS_ENDPOINT` | S3 endpoint URL for your store |
| `KASANE_CORPUS_BUCKET` | bucket holding the corpus objects |
| `KASANE_CORPUS_PREFIX` | object-key prefix (default `pdfs`) |

Back-compat: `R2_ENDPOINT`/`CLOUDFLARE_R2_ENDPOINT`/`R2_BUCKET` (and
generic `AWS_ENDPOINT_URL`/`S3_BUCKET`) are also honoured.

## Credentials

Credentials resolve through the **standard AWS/boto3 provider
chain** — nothing is stored in the repo:

- `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, or
- `AWS_PROFILE` → `~/.aws/credentials`, or
- an IAM role / instance-metadata / SSO.

Scope the token to the corpus bucket only.

## No store? Use a local directory

The runner reads a plain directory of PDFs. Build one from files you
already have (matched to the manifest by sha256):

```console
$ .venv-corpus/bin/python compat/fetch-corpus.py seed \
    --from /path/to/my/pdfs --dir compat/corpus
```

…or run the runner straight at any directory:

```console
$ compat/runner/target/release/kasane-compat run \
    --corpus /path/to/my/pdfs --out compat/reports
```

## Setup

```console
$ python3 -m venv .venv-corpus
$ .venv-corpus/bin/pip install boto3 pyyaml pillow
```

## Commands

```console
# list manifest entries
$ .venv-corpus/bin/python compat/fetch-corpus.py list

# download everything → compat/corpus/
$ .venv-corpus/bin/python compat/fetch-corpus.py download

# add a local file (computes sha256, uploads, appends to manifest)
$ .venv-corpus/bin/python compat/fetch-corpus.py add file.pdf \
      --category office --source <url> --license <tag> [--pages N]

# add straight from a URL
$ .venv-corpus/bin/python compat/fetch-corpus.py add-url <url> \
      --category latex --source <url> --license <tag>

# re-hash local files against the manifest
$ .venv-corpus/bin/python compat/fetch-corpus.py verify
```

## Running the compatibility suite

Build once:

```console
$ cargo build --manifest-path compat/runner/Cargo.toml
```

Run the whole corpus (writes `compat/reports/run-<ts>.{json,md}` +
`latest.*`):

```console
$ compat/runner/target/debug/kasane-compat run \
      --corpus compat/corpus --out compat/reports --with-poppler
```

- `--with-poppler` — cross-check page count, render page 1 with
  `pdftoppm` (MAE vs our raster), and text-match with `pdftotext`.
  Requires `pdfinfo`, `pdftoppm`, `pdftotext`, `qpdf` on PATH.
- `--dpi N` (default 96), `--max-pages N` (default 3),
  `--only <id>` for a single file.

Re-render a saved result without re-running:

```console
$ kasane-compat report compat/reports/latest.json --format md|html
```

Diff the current run against a previous one (regression analysis —
newly PASS/FAIL files, verdict changes, cause-set and MAE deltas):

```console
$ kasane-compat report compat/reports/latest.json \
      --compare compat/reports/run-<prev-ts>.json
```

## Compatibility vs Robustness

The report splits the corpus into two independent rates, driven by a
per-file `expect:` tag in `manifest.yaml`:

| `expect` | meaning | feeds |
|---|---|---|
| `pass` | well-formed document the engine should handle | **Compatibility** rate |
| `reject` | adversarial / malformed / fuzzed / encrypted — must be *refused safely* | **Robustness** score |
| `any` | valid doc using out-of-scope features | neither rate |

- **Compatibility** = per-stage pass rate over the `pass` files.
- **Robustness** = a `reject` file is OK when the engine refuses it
  *without crashing or hanging* (a classified `Error`/`Unsupported`);
  only `CRASH`/`TIMEOUT` fail the robustness check. A `reject` file
  that happens to render is still "safe" — refusal is the goal, not
  mandatory failure.

Tag files by *document intent*, never by the engine's current result —
a well-formed PDF our parser can't read yet is `expect: pass` (a
compatibility gap), not `reject`.

## ExtGState usage audit

`kasane-compat run --corpus … --gs-audit` walks every corpus PDF and
prints which `/ExtGState` keys appear, on how many files, and which gs
names are actually `gs`-invoked — the data behind the phased
implementation decision in `docs/decisions/`.

## Font usage audit

`kasane-compat run --corpus … --font-audit` (`runner/src/fontaudit.rs`)
enumerates every `/Font` resource per PDF — split by `/Subtype`,
embedding (`/FontFile`/`/FontFile2`/`/FontFile3` in the descriptor),
`/Encoding`, `/ToUnicode`, `/Widths` — and marks which fonts are
actually `Tf`-invoked (vs merely present in resources). Add `--only ID`
to dump one file's fonts. This drove the Type1 implementation split:

| program | status | corpus hit |
|---|---|---|
| `/FontFile3` raw CFF (`/Type1` + `/MMType1`) | **implemented** — outlines via `ttf_parser::cff`, glyph ids via CFF charset + `/Encoding` | few (`pattern_text_embedded_font`, parts of `tamreview`) |
| non-embedded Type1 (Standard-14-style) | **substituted** — bundled Liberation faces (OFL) by name/flags, PDF `/Widths` kept, substitute `hmtx` only when `/Widths` is absent; reported in `RenderedPage::font_substitutions` and listed in the fidelity section; Symbol/ZapfDingbats/unknown-symbolic/non-ASCII names still refuse | ~21 files; 7 render only because of this |
| `/FontFile` Adobe Type 1 (eexec) | **implemented** — `src/type1.rs` eexec-decrypts + interprets the charstring VM (paths, hints, subrs, flex, hint replacement, `seac`; `-|`/`|-` RD aliases; 32-bit integer operands) — every glyph of all 19 real arXiv fonts (CM, cm-super, Nimbus) interprets | ~7 files (the arXiv family): text now passes on `arxiv-2204.06125`, `arxiv-2305.14334`, `attention-is-all-you-need`; render stays blocked by `/Annots` + non-embedded fonts |
| `/FontFile3` CIDFontType0C | **refused** — CID font, not simple Type1 | — |
| `/Type3` (`/CharProcs` streams) | **implemented** — each glyph runs like a mini Form XObject under `/FontMatrix × Trm × CTM`; `d0`/`d1` parsed; `in_text` reset allows nested `BT…ET` (Type3-in-Type3) bounded by `max_xobject_depth` | 4 files (2 now pass; 2 blocked on `/Pattern`) |

Measured after Type1 + Type3 (vs the 13/67 render, 17/67 text,
13 full-PASS ExtGState baseline): render **15/67**, text **25/67**,
full PASS **15** — zero regressions.

Measured after eexec (`/FontFile`) Type 1 (run-1792678862): the
`render.font.type1-eexec`/`text.font.type1-eexec` cause is gone from
all 7 affected files and `attention-is-all-you-need` progressed past
font load to a separate `bfrange` ToUnicode `Structure` bug — but
**0 new PASSes**, because every eexec file is *also* blocked by
`/Annots` (render+text) and non-embedded fonts. eexec was never the
sole blocker; the other 6 remain `/Annots`-blocked before fonts are
reached. Correctness is carried by the `src/type1.rs` unit tests and
the real `eexec_cmsy10.bin` fixture.

Measured after the fidelity-driven fixes (run-1792685132, same
conditions): render **17/67**, text **34/67**, full PASS **17**
(+`truetype_without_cmap`, +`arial_unicode_ab_cidfont`), 0 newly
failing, 0 cause-set changes, 0 crash/timeout, robustness 13/13. The
fixes came from reading the saved ours/ref/diff images rather than
from the pass counts:

| root cause | symptom in the corpus | fix |
|---|---|---|
| `cm`, form `/Matrix` and Type3 `/FontMatrix` composed *after* the CTM instead of before (`ctm.concat(m)`) | every Type3 glyph drawn at the page origin (`simpletype3font` SSIM 0.997 yet completely misplaced; `type3wordspacing` SSIM 0.733); a synthetic nested-`cm` oracle put a square at (200,200) where Poppler put it at (100,100) | `m.concat(ctm)` at all three sites; `nested_cm_order`, `form_matrix_under_cm_order`, `type3_glyph_lands_at_text_position` tests |
| TrueType outlines ignored the `hmtx` bearing when it disagrees with the `glyf` xMin | `cidfont_cmap_overflow` glyphs 80 units right of Poppler (SSIM 0.962) | FreeType phantom-point rule (`lsb − xMin`, `USE_MY_METRICS` for composites) → SSIM 0.999 |
| `callothersubr` popped `N` before `othersubr#`; OtherSubrs 3 dropped the Subr index; 5-byte numbers read as 16.16; only literal `RD` accepted | `type1 callothersubr underflow` on 6 arXiv files once text extraction reached their eexec fonts | operand order, hint-replacement push-back, 32-bit ints, positional RD token |
| `/Widths 12 0 R` not dereferenced | `/Widths is not an array` on 2 files | `doc.deref` |
| `bfrange` checked the whole UTF-16 unit against 0xFF, and incremented once past the last entry | `bfrange increment overflows last byte` on 4 files (any destination above U+00FF) | low-byte check, no increment after the final entry |
| `/Annots` refused in *extraction* | 10 files' text blocked by a layer that is not page text | extraction ignores `/Annots` (render still refuses) |

Remaining fidelity gaps after this run: `type3wordspacing` (SSIM
0.921) is Poppler's minimum-stroke-width policy on 0.5 pt dashed
strokes, `complex_ttf_font` (0.962) is rasteriser/anti-aliasing
difference with no systematic shift (best shift (−1,0) does not lower
the diff), and `truetype_without_cmap` (0.971) sits one pixel low
(best shift (0,−1)) — a baseline rounding question, not a placement
bug.

Measured after non-embedded font substitution, annotation appearance
streams and named-destination-preserving rewrite (run-1792687472,
same conditions): render **24/67**, text **35/67**, rewrite
**60/67**, full PASS **24** (+`arxiv-2204.06125`, `basicapi`,
`mlp-mixer`, `helloworld`, `bad-pagelabels`, `standard_fonts`,
`text_rise_eol_bug`), 0 newly failing, 0 crash/timeout, robustness
13/13. The 17 files measured in both runs have identical fidelity
(mean SSIM 0.9906 → 0.9906); the 7 newly measured files draw
Standard 14 text with Liberation substitutes and Poppler draws them
with its own substitutes, so their SSIM (0.886–0.998) is bounded by
the two substitute choices, not by the engine — the report lists
which fonts were substituted per file. Reading those new diffs also
exposed a Type 1 interpreter bug: `hvcurveto` ended its curve
horizontally instead of vertically, distorting every round CM glyph
(`attention-is-all-you-need` 0.793 → 0.901, `mlp-mixer` 0.767 → 0.870
after the fix). What still blocks the watch-list files: image
`/SMask` (`attention-is-all-you-need`, `arxiv-2305.14334`,
`arxiv-2307.06435`, `arxiv-2310.06825`, `blendmode`), non-device
colour spaces (`arxiv-2303.18223`), inline images (`tamreview`),
the `UniJIS-UTF16-H` predefined CMap (`mixedfonts`).

Measured after image soft masks, non-device colour spaces and
offscreen transparency-group layers (run-1792710240, same
conditions): render **30/67**, text **38/67**, rewrite **60/67**,
full PASS **30** (+`attention-is-all-you-need`, `arxiv-2305.14334`,
`arxiv-2310.06825`, `colorspace_atan`, `indexedcs_negative_and_high`,
`verapdf_test_suite_6-2-3-2-t01-pass-a`), 0 newly failing, 0 lost
renders, 0 crash/timeout, robustness 13/13, the same 5 render
`error` files as before. The 24 files measured in both runs have
identical fidelity (mean SSIM 0.9734 → 0.9734; `standard_fonts`
0.8859 → 0.8857 is sub-pixel anti-aliasing noise from the new image
compositor, not a content change). Newly measured: `colorspace_atan`
(DeviceN + PostScript tint transform) SSIM 0.995,
`indexedcs_negative_and_high` 0.982, `verapdf …6-2-3-2-t01` 0.9998;
the three arXiv papers now render all 3 probed pages (the previously
blocked soft-masked figure pages score 0.958 on
`attention-is-all-you-need` p3, bounded by Computer Modern
substitution exactly like their text pages). No file in the corpus
needed an ICC approximation (every embedded profile is matrix/TRC
sRGB). The cause-set lost `render.colorspace.unsupported` on 5 files;
what still blocks the watch list: `blendmode` (a `/Luminosity`
blend on an image — non-separable), `arxiv-2307.06435` (simple font
without `/Encoding`), `arxiv-2303.18223` (`MacRomanEncoding`),
`tamreview` (inline images), `mixedfonts` (`UniJIS-UTF16-H`), and
`images`/`smaskdim`/`stamps`/`highlights`/`inks` which still fail
at parse time ("unexpected end of input while reading xref stream
data"), so their soft masks were never reached. The soft-masked
figures themselves (e.g. the Transformer diagram on
`attention-is-all-you-need` p3) show no alpha-edge halo in the diff
image; the residual diff on those pages is the missing Computer
Modern math glyphs, a pre-existing font gap.

## Corpus composition and the road to 500–1,000 files

The current 106 files by manifest category (`compat/manifest.yaml`):
graphics 25, annotations 21, misc 15, malformed 12, images 11,
japanese 8, latex 7, encrypted 3, incremental 2, office 2. By origin:
82 from the pdf.js test suite (Apache-2.0), 13 from the veraPDF
corpus (GPLv3+MPL, internal use), 7 arXiv papers (author-submitted,
not redistributable), 2 IRS forms (US public domain), 1 Adobe sample,
1 W3C test document. 79 are `expect: pass`, 13 `reject`, 14 `any`.

That is a *feature-test* corpus: 77% of it is engine test suites
built to exercise one PDF construct each, and the categories a user
actually meets are thin — 2 office exports, 0 browser-printed pages,
0 scanned documents, 0 interactive forms with field values, 8
Japanese files that are all pdf.js CMap tests. The expansion target
and the sampling bias to correct, per category (target counts for a
600-file corpus, weighted toward producer diversity rather than
spec coverage):

| category | now | target | what to sample | candidate sources (licence) |
|---|---:|---:|---|---|
| office exports | 2 | 100 | Word/Excel/PowerPoint, LibreOffice, Google Docs, Pages; several versions of each producer | self-generated from CC0/own documents (no licence issue); GovDocs1 subset (US public records, free use); LibreOffice `sw/qa` test docs (MPL-2.0) |
| browser print | 0 | 80 | Chrome/Skia, Firefox/Cairo, Safari/Quartz “print to PDF” of real pages, incl. CSS transforms, web fonts, SVG, emoji | self-printed Wikipedia/CC-BY pages (CC BY-SA 4.0, attribution in manifest); W3C/WHATWG spec pages (W3C licence) |
| Japanese | 8 | 80 | vertical text, predefined CMaps (90ms-RKSJ, UniJIS-UCS2/UTF16), non-embedded CJK (MS Mincho/Gothic, Hiragino), Word-ja, 一太郎, e-Gov/city-hall forms, LaTeX pLaTeX/LuaTeX-ja | e-Gov 法令・各省庁 PDF (公共データ利用規約 PDL 1.0 = CC BY 4.0 compatible); 国立国会図書館 digital collections (public domain); J-STAGE open-access papers (CC BY); self-generated |
| academic papers | 7 | 80 | LaTeX via pdfTeX/XeTeX/LuaTeX/dvipdfmx, Word→PDF, with figures (SMask, shadings, patterns, Type3/Type1C fonts) | arXiv **only** papers under CC BY/CC0 (filter by licence field in the OAI metadata — the current 7 are author-submitted and must not be redistributed); PLOS/PMC OA subset (CC BY); DOAJ |
| scanned | 0 | 60 | CCITT G4, JBIG2, DCT, JPX, mixed-raster, OCR text layers, skewed pages, A3/A4 | Internet Archive public-domain books (PD); GovDocs1; NDL; self-scanned |
| forms (AcroForm/XFA) | 2 | 40 | field appearance streams, `/NeedAppearances`, filled values, checkboxes/radios, XFA foreground | IRS (PD), US federal forms (PD), e-Gov forms (PDL), pdf.js/pdfium form tests (Apache/BSD) |
| annotations | 21 | 40 | real-world markup from Acrobat/Preview/Edge/Drawboard: highlights, ink, stamps, popups, redactions, rich-text free text | self-annotated CC0 docs with several viewers; pdf.js tests already cover the synthetic cases |
| images & transparency | 11 | 60 | SMask/Matte, ICC-CMYK images, Indexed+SMask, 16-bit, JPX, soft masks in ExtGState, knockout groups, blend modes from InDesign/Illustrator/Inkscape | Inkscape/Scribus exports (self-generated, GPL test files are fine as data), pdfium test corpus (BSD-3), Ghostscript bug attachments (case-by-case — many have no licence), PDF Association sample files |
| malformed / fuzzed | 12 | 60 | truncated, bad xref, missing `endobj`, object streams with wrong offsets, recursion bombs, decompression bombs, giant `/Width` | Ghostscript/poppler/pdfium/pdf.js bug-tracker fuzz cases (already used; licence-free as they are not creative works but keep them *out of the public repo* when the tracker says so); self-mutated copies of the CC0 files (radamsa/zzuf) |

Three constraints that shape the manifest schema rather than the
runner: (1) **licence is per file**, so `license` and `source` stay
mandatory and a `redistribute: yes|no` flag should be added so a
public bucket can be built from the `yes` subset only — today the
7 arXiv papers and the Adobe sample are `no`; (2) **producer** is
the axis users care about, so add `producer:` (from `/Producer`,
normalised) and report per-producer pass rates next to per-category;
(3) the bucket stays private and nothing under `compat/corpus/` is
committed — the manifest (hashes, sources, licences) is the public
artefact, and `fetch-corpus.py` re-materialises the corpus from the
listed sources where `redistribute: no`.

## Pre-publication audit (repository and history)

Scanned the tracked tree and all 91 commits for credentials and
environment-specific identifiers (patterns: AWS/R2 key IDs,
`SECRET`/`token`/`password`, private-key headers, the R2 endpoint
host, the Cloudflare account ID, the bucket name, local user paths,
the maintainer's employer and e-mail). Findings, classified:

| item | kind | where | action before going public |
|---|---|---|---|
| R2 account ID (32-hex) inside the endpoint URL | **identifier**, not a secret — it does not grant access by itself, but it pins the repo to one Cloudflare account and is unwanted in a public history | 11 historical commits of `compat/fetch-corpus.py` and `docs/compat-corpus.md`; **absent from HEAD** (the example config uses `<ACCOUNT_ID>`) | either accept it (identifier only) or rewrite history once, right before the first public push, with `git filter-repo --replace-text`; **not done in this change** — rewriting now would invalidate every open branch and the commit IDs quoted in the run reports |
| bucket name `kasane-pdf-corpus` | identifier | same 11 commits; HEAD uses the `CORPUS_BUCKET` variable | same decision as above; a bucket name is not sensitive, but the bucket must stay private (it holds non-redistributable files) |
| `*.r2.cloudflarestorage.com` host | identifier (vendor) | 19 commits + HEAD example/docs | fine to publish |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | **variable names only**; no value has ever been committed (0 hits for key-ID or secret patterns in any commit) | docs + example env file | nothing to rotate; keep `.env`/`compat/corpus.env` ignored |
| local paths (`/Users/…`), employer domain, personal e-mail | — | 0 hits in tree or history (commit author is the noreply address) | nothing |
| `compat/corpus.env` (the real local config) | gap in `.gitignore` — `.env.*` does not match `corpus.env` | not present in tree or history | add `compat/corpus.env` to `.gitignore` (done in this change) |

So: **no secret has ever been committed and no rotation is needed.**
The only pre-publication decision is whether to scrub the account
ID and bucket name from 11 historical commits; if yes, do it as a
single `filter-repo` pass on a fresh clone, re-tag, and force-push
*once* before the repository is public, never afterwards.
Configuration layout is already in the recommended shape — code
reads `KASANE_CORPUS_ENDPOINT`/`KASANE_CORPUS_BUCKET`/
`KASANE_CORPUS_PREFIX` and the `AWS_*` credentials from the
environment or an ignored env file, and `compat/corpus.env.example`
documents the variables with placeholders.

## Failure-cause analysis

Every stage failure is also classified into a **stable cause id** of
the form `<stage>.<category>[.<specific>].<class>` — e.g.
`render.extgstate.unsupported`, `parse.eof.error`,
`rewrite.named-dest.unsupported`. The id is derived from
`Error::kind()` plus the engine's `feature:` tag, **never** from
free-text messages, so rewording an error string can't re-bucket the
count. Unknown errors still produce an id (`*.unknown.*`) rather than
being dropped.

A read-only **probe** (`runner/src/probe.rs`) walks each page's
content ops and `/XObject` / `/Font` / `/ColorSpace` / `/ExtGState`
resources and records *every* blocker it would hit — not just the
first one the engine returns. This powers three distinct per-feature
counts in the report:

| count | meaning |
|---|---|
| **affected** | PDFs the feature appears on at all |
| **sole-blocker** | PDFs where it is the *only* known blocker |
| **potentially unblocked** | PDFs that could newly pass if it's implemented — an **estimate**, since the probe only sees `--max-pages` pages and doesn't recurse into Form XObject streams |

The **Development priorities** table ranks features by
`potentially_unblocked / effort`, where `effort` is a person-day
*guess* in `analysis.rs` (`PrioritiesConfig`) — shown as an estimate,
never a measured value. The probe deliberately does **not** change
engine failure behaviour; it only enumerates what *would* block.

### Feature-set priorities (combinations)

Most failing files carry *several* blockers, so a single-feature
ranking undersells the work that actually turns files green. The
**Feature-set priorities** table (`analysis::combo_analysis`) evaluates
sets of 1–4 features:

- A file's blocker set is the probe's causes **plus** the cause id of
  every failing stage (a rewrite-only blocker such as named
  destinations still prevents a full PASS). Files whose parse failed
  contribute nothing — their blockers are *unknown*, never "none" —
  and `expect: reject` files are excluded (refusing them is the goal).
- Candidate sets come from the data (each failing file's own set and
  pairwise unions of those sets) plus a fixed hypothesis list
  (non-embedded fonts + annotations, OC + annotations, Pattern + Type3,
  Vertical/CJK + CIDFont, colour spaces + transparency, ToUnicode +
  fonts), which are always reported even when they unblock nothing.
- Sets are closed over a dependency table (`PrioritiesConfig::deps`:
  Pattern colours need Shading, the ExtGState residue needs
  transparency-group XObjects) and charged the prerequisites' effort.
- **Expected newly PASS** = failing well-formed files whose *entire
  known* blocker set lies in the closure. The bracketed
  **fully probed** figure additionally requires every page walked, no
  walk errors and no Form XObjects (the probe cannot see inside a
  form); even that is an estimate — the probe mirrors the renderer's
  rules, it does not run them. **Confidence** is driven by that
  coverage ratio. Data-derived sets that add nothing over a strict
  subset are dropped as dominated.
- Each set lists the candidate file ids with their full blocker sets
  and coverage so the claim can be checked by hand.

Current ranking (run-1792685132): non-embedded font rendering alone
→ 7 (6 fully probed); + annotations + named destinations → 14 (10);
adding non-device colour spaces → 21 (14). Annotations are never a
sole blocker in this corpus; they only pay off together with
non-embedded fonts.

## Result classification

Each file gets one verdict — the **worst** class across its stages
(crash > timeout > error > unsupported > mismatch > pass > skip):

| class | meaning |
|---|---|
| `PASS` | stage produced the expected result |
| `MISMATCH` | succeeded but output differs from the reference |
| `UNSUPPORTED` | a documented unimplemented feature (typed `Unsupported`) |
| `ERROR` | parse/structure/limit failure or reparse failure |
| `CRASH` | panic / abnormal exit |
| `TIMEOUT` | exceeded `--timeout-secs` |
| `SKIP` | could not run (e.g. earlier stage failed) |

Unsupported features are **never counted as success** — the report
keeps `UNSUPPORTED` separate so "we can't do it" is visible, not
hidden inside a pass rate.

## Render execution vs visual fidelity

A page can *execute* (no error) yet draw the wrong thing, so the runner
keeps two independent measures:

- **Render Execution Success** — the `render` stage class: did
  `render_page` return without `Unsupported`/`Error`/`Crash`?
- **Render Visual Fidelity** — when `--with-poppler` is on, every
  rendered page is compared to `pdftoppm`'s reference at the same DPI
  (grayscale, nearest-resampled to match size):

  | metric | meaning |
  |---|---|
  | `render_ssim` | Structural Similarity (Wang et al.), 8×8 windows, stride 4 — 1.0 = identical |
  | `render_mae` | mean absolute luminance error, 0.0–1.0 |
  | `render_diff_px_ratio` | fraction of pixels differing by >8/255 |
  | `render_pages_ok/total` | per-page render success |
  | `render_pages[]` | every compared page with all metrics below (the file-level values are the worst page's) |
  | `render_quality.content_ssim` | SSIM averaged over 8×8 windows that contain ink in *either* image — blank area cannot inflate it |
  | `render_quality.diff_px_tol1` | different-pixel ratio where a pixel counts only if no 3×3 neighbour in the other image matches — tolerant of ±1 px anti-aliasing/hinting |
  | `render_quality.ink_ours` / `ink_ref` | ink coverage (luma < 200) — a ratio far from 1 is missing or extra content |
  | `render_quality.best_shift` / `diff_px_at_best_shift` | the integer shift within ±3 px that minimises diff-px; a non-zero shift with a sharp drop is a systematic position offset (metrics, bearing, matrix) |
  | `render_quality.size_mismatch` | reference dimensions differed before resampling |

  Two bars are reported side by side, and neither changes the render
  stage's PASS: a page that executes but diverges is a render PASS
  and a fidelity failure.

  - **Comparability bar** (unchanged, so runs stay comparable): worst
    page SSIM ≥ 0.98 *and* diff-pixels ≤ 2% *and* MAE ≤ 0.05.
  - **Content bar**: ink coverage within ×0.7–1.4 of the reference
    *and* (content-SSIM ≥ 0.85 *or* ±1 px-tolerant diff ≤ 0.5%).

  Why the second bar exists: `simpletype3font` scored whole-page SSIM
  0.997 while every glyph sat at the page origin — on a mostly blank
  page the blank windows dominate. Its content-SSIM was 0.009. The
  same run's `type3wordspacing` had ink coverage 0.002 vs 0.048 in the
  reference: content was missing, which no SSIM threshold expresses.
  The tolerant diff and best-shift figures separate rasteriser
  differences (`complex_ttf_font`: same ink, no shift that helps) from
  placement bugs (`cidfont_cmap_overflow` before the fix: best shift
  (−3,0) cut diff-px from 0.021 to 0.008).

  Files below either bar are listed with the bar they failed and a
  saved diff image. For each measured file the runner writes
  `{id}-p{n}-ours.pgm`, `-ref.pgm`, `-diff.pgm` under `reports/diff/`
  so a divergent page can be eyeballed without re-running. The
  comparison is deterministic: same DPI, grayscale, nearest-neighbour
  resample to our pixmap size.

  Current run (run-1792685132): 17 rendered files measured; mean SSIM
  0.991 (was 0.976), worst 0.921 (`type3wordspacing`, was 0.733);
  `cidfont_cmap_overflow` 0.962 → 0.999, `simpletype3font` 0.997 →
  1.000. Three files remain below a bar, all explained above.

  The reference engine is Poppler as an *external* oracle only — its
  code is never linked or embedded; the comparison is process-level
  (`pdftoppm` subprocess) so the renderer stays self-contained.

## Sources & licences

The corpus mixes permissively-licensed and internal-use files
(nothing is redistributed — R2 is private):

- **pdf.js `test/pdfs`** — Apache-2.0, ~80 files: fonts, images,
  annotations, malformed/fuzzed, CID, xref/ObjStm edge cases.
- **veraPDF `veraPDF-corpus`** — GPLv3+MPL (internal only):
  graphics, transparency, ICCBased colour, forms.
- **arXiv papers** — author-submitted LaTeX PDFs (fonts, math,
  figures, large).
- **US Gov / IRS forms** — public-domain fillable AcroForm PDFs.
- **Adobe / W3C samples** — real generator variety.

Add more via `add`/`add-url`; keep `license` accurate per entry.

## Directory layout

```
compat/
  manifest.yaml     # id, sha256, category, source, license
  fetch-corpus.py   # R2 sync (boto3) — boto3/pyyaml in .venv-corpus
  runner/
    src/main.rs     # CLI, stage orchestration, report rendering
    src/cause.rs    # error → stable cause-id taxonomy (+ tests)
    src/probe.rs    # read-only multi-blocker page walker
    src/analysis.rs # aggregation, priorities, effort config (+ tests)
  corpus/           # downloaded PDFs (gitignored)
  reports/          # run-*.json/.md + diff images (gitignored)
```
