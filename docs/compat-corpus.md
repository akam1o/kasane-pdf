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

# register without a store: keep the file in compat/corpus/ and append
# to the manifest; upload later with `sync` once credentials exist
$ .venv-corpus/bin/python compat/fetch-corpus.py add-url <url> --local \
      --category papers --license "CC BY 4.0" --redistribute yes --expect pass
$ .venv-corpus/bin/python compat/fetch-corpus.py sync

# grow the corpus from the public sources in one go (pdf.js, pdfium,
# veraPDF, IRS, arXiv CC BY, Chrome/CUPS/sips-generated, mutations)
$ .venv-corpus/bin/python compat/collect-corpus.py [--only pdfjs irs …] [--max-pdfjs N …]
```

Every manifest entry carries `id`, `sha256`, `category`, `source`,
`license`, `expect` (`pass`/`reject`/`any`), `pages`, `notes`, and —
since the 500-file expansion — `redistribute` (`yes`/`no`: may the
file itself be published with the corpus), `producer` (the
`/Producer` string normalised to a product family by
`fetch-corpus.py::normalise_producer`), `producer_raw`, `version`
(`%PDF` header) and `encrypted`. `add`/`add-url` refuse a file whose
SHA-256 is already listed. The manifest is the source of truth; the
store and `compat/corpus/` are caches of it.

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
- `--mode smoke|full` — presets for the round-trip stage. `smoke`
  (pull requests): 2 pages per file, 24 dpi, Poppler on page 1, a
  10 s per-file render budget and a 10 s stage timeout. `full`
  (nightly / release): every page of every file with our renderer
  *and* with Poppler, 48 dpi, a 120 s per-file render budget. The
  individual knobs are `--rt-pages N`, `--rt-dpi N`, `--poppler-pages
  N`, `--rt-budget-secs N`. A file whose budget runs out before every
  page was compared is classified PARTIAL — attempted, not passed.

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
| `any` | valid doc using out-of-scope features, or a file whose only fault is one the strict parse reports and recovery reads (named in `notes`) | neither rate |

- **Compatibility** = per-stage pass rate over the `pass` files.
- **Robustness** = a `reject` file is OK when the engine refuses it
  *without crashing or hanging* (a classified `Error`/`Unsupported`);
  only `CRASH`/`TIMEOUT` fail the robustness check. A `reject` file
  that happens to render is still "safe" — refusal is the goal, not
  mandatory failure.

Tag files by *document intent*, never by the engine's current result —
a well-formed PDF our parser can't read yet is `expect: pass` (a
compatibility gap), not `reject`.

### Strict parse vs recovery parse

`parse` is always the strict `Document::from_bytes`. When it fails,
the runner tries `Document::from_bytes_recovering` as a separate
`recover` stage: on success the render/text/rewrite stages run on the
rebuilt document and the file is listed in the report's *Recovery
parse* section with the strict error, the recovery summary (objects
scanned, members from object streams, where `/Root` came from,
warnings) and those stage outcomes — but its verdict stays the strict
parse's, and none of the stage tables, category/producer tables or
the compatibility rate count a recovered file's downstream stages
(`analysis::rated_class` turns them into *skip*). A recovered
document is a best-effort reading of a damaged file and is reported
as exactly that.

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

Measured after simple-font encodings, axial/radial shadings, inline
images, the xref-stream decoding fix and the recovery parse
(run-1792713726, same conditions, 106 files): strict parse **92/106**
(was 67 — the xref-stream fix alone: xref-stream entries were being
read from the *encoded* bytes, so every file whose newest section was
a Flate-compressed xref stream failed), render **54/67 → 54/92
attempted**, text **70**, rewrite **79**, full PASS **53** (was 30),
0 newly failing, 0 lost renders, 0 crash/timeout, robustness 13/13.
Newly passing: the 13 pdf.js `annotation-*` files, `alphatrans`,
`radial_gradients`, `shading_extend` (shadings), `tamreview` (inline
images), `arxiv-2303.18223` and `arxiv-2307.06435` (encodings:
MacRoman TrueType, Type 1 built-in encodings), `w3c-dummy` (symbolic
TrueType), `font_ascent_descent`, `image-rotated-black-white-ratio`,
`smaskdim`, `stamps` (xref streams). The 27 files measured in both
runs are unchanged (mean SSIM 0.9755 → 0.9756). The recovery parse
rebuilt 5 of the 14 strict-parse failures (`helloworld-bad`,
`xref_command_missing`, `scan-bad`, `ghostscript-698804-1-fuzzed` —
all `expect: reject`, all then render/extract/rewrite cleanly — and
`highlights`, whose truncated content stream still stops the render);
the other 9 are 5 encrypted files, 2 whose referenced objects do not
exist anywhere in the file, a page-tree cycle and a page without a
MediaBox — none of which a scan can repair, and none of which should
be invented.

## Corpus composition (511 files, run-1792715480)

The corpus grew from 106 to **511** files with `compat/collect-corpus.py`
(sources and licences below; every file registered through
`fetch-corpus.py --local`, de-duplicated by SHA-256, described by
producer family / `%PDF` version / encryption, and kept out of git).
All 511 files are in the object store (`fetch-corpus.py sync`
uploaded the 405 new ones plus one re-keyed by its refreshed
category; a fresh `download` into an empty directory reproduces the
corpus and `verify` reports every hash intact). Redistribution flags: 0 `yes`,
0 `no` (the veraPDF suite) — a public bucket can be
built from the `yes` subset.

By category (strict counts; a recovered file's downstream stages are
not counted — see *Strict parse vs recovery parse*):

| category | files | parse | render | text | rewrite | full PASS |
|---|---:|---:|---:|---:|---:|---:|
| misc | 159 | 141 | 67 | 95 | 127 | 65 |
| graphics | 61 | 61 | 17 | 50 | 59 | 17 |
| annotations | 52 | 50 | 23 | 44 | 42 | 22 |
| forms | 38 | 38 | 3 | 21 | 13 | 3 |
| images | 17 | 16 | 8 | 14 | 15 | 8 |
| malformed | 45 | 10 | 6 | 8 | 9 | 6 |
| japanese | 16 | 16 | 3 | 4 | 15 | 3 |
| latex | 13 | 13 | 9 | 9 | 12 | 9 |
| office | 17 | 16 | 8 | 12 | 13 | 7 |
| browser | 19 | 19 | 10 | 10 | 19 | 10 |
| papers | 10 | 10 | 1 | 1 | 9 | 1 |
| transparency | 5 | 5 | 1 | 3 | 5 | 1 |
| encrypted | 4 | 0 | 0 | 0 | 0 | 0 |
| incremental | 2 | 0 | 0 | 0 | 0 | 0 |
| scan | 13 | 13 | 12 | 13 | 13 | 12 |

`category` is the collector's name-based guess refined by producer
evidence (`fetch-corpus.py refresh --recategorise` moves `misc` files
whose `/Producer` names an office suite, browser, TeX engine or
scanner); `misc` is still the largest bucket because most pdf.js /
pdfium reproducers carry no `/Producer` at all (239 of 511 files are
`producer: unknown`). The report's *By producer* table is therefore
the better origin axis for the files that have one: Microsoft Office
18/22 render, pdfTeX 7/9, TCPDF 7/10, Quartz 21/40, Acrobat 17/36,
Skia/Chrome 14/42 (the Chrome prints embed CFF-based CID fonts),
LiveCycle Designer 1/29 (XFA/AcroForm widgets without appearance
streams), LibreOffice 0/8 (CIDFontType0).

By source and licence:

| source | files |
|---|---:|
| github.com/mozilla | 282 |
| pdfium.googlesource.com | 60 |
| www.irs.gov | 27 |
| arxiv.org | 17 |
| github.com/veraPDF | 13 |
| en.wikipedia.org | 4 |
| ja.wikipedia.org | 3 |
| www.adobe.com | 1 |
| www.w3.org | 1 |
| de.wikipedia.org | 1 |
| fr.wikipedia.org | 1 |
| ko.wikipedia.org | 1 |

| licence | files |
|---|---:|
| Apache-2.0 | 282 |
| BSD-3-Clause | 60 |
| US Gov public domain | 27 |
| GPLv3+MPL (internal) | 13 |
| CC BY-SA 4.0 (Wikipedia text; printed with Chrome) | 12 |
| CC BY (http://creativecommons.org/licenses/by/4.0/) | 10 |
| arXiv (author-submitted) | 7 |
| Adobe sample (internal use) | 1 |
| W3C test doc | 1 |

Self-generated files are CC0: Chrome headless `--print-to-pdf` of
local HTML (columns/tables/SVG/transforms/emoji, Japanese vertical
text with Hiragino, form controls, code listings) and of Wikipedia
articles in six scripts (CC BY-SA 4.0 text, attribution in `notes`),
CUPS/Quartz text-to-PDF, `sips` image-only pages rasterised from
corpus files (scan-like, no text layer), and deterministic mutations
(truncation, byte flips, zeroed xref, bad `startxref`, bad `/Length`,
dropped `endobj`, renumbered objects, giant `/Count`, stray bytes;
`expect: reject`) of those files and of the test fixtures. The
`drop-endobj` mutations are the reason the strict parser now requires
`endobj` (§7.3.10): until it did, three of them parsed, rendered and
rewrote cleanly — "processed" without being valid PDF — and were the
only `expect: reject` files to score a full PASS. They are recovered
now, as are four pdfium test files whose hand-written objects also
lack `endobj` (retagged `expect: any` with the fault in `notes`).

What the expansion did **not** reach, and why: arXiv added 10 papers
(licence verified on each abstract page; the OAI-PMH endpoint
rate-limited the bulk listing), no Japanese government/office
originals (no vetted URL list — the Japanese set is Chrome prints of
Japanese Wikipedia pages plus the pdf.js CJK tests), no real scanner
output beyond one Canon file (the scan bucket is `sips`-generated),
no real office exports beyond the pdf.js/pdfium ones (no office suite
on this machine). Those four categories remain the next collection
targets; the mechanism (manifest fields, `--local`, `sync`, the
collector's per-source caps) is in place.

### Results on 511 files

Compatibility (431 `expect: pass` files): strict parse **408 (95%)**,
render **177 (41%)**, text **290 (67%)**, rewrite **352 (82%)**;
full PASS **198/511**. Robustness (43 `expect: reject`): 43 safely
handled, 40 cleanly refused, **0 crash / 0 timeout** — across all 511
files no stage crashed or timed out. The 106 files of the previous
corpus are unchanged (full PASS 53 → 53, mean SSIM over the 53
measured files 0.9795 → 0.9795), so the new baseline is additive.
Recovery parse rebuilt 30 of the 63 strict failures (26 of them
`expect: reject` mutations; the 4 `expect: pass` ones are damaged
real files); 33 stay refused: 6 encrypted, 4 pages without a
MediaBox, 5 with references to objects that exist nowhere in the
file, the rest truncated beyond any object header.

Visual fidelity on the 166 newly measured files: median SSIM 0.996,
mean 0.937; 32 files below 0.9. The three worst are **reference
disagreements, not engine errors**: `copy_paste_ligatures` (SSIM
0.005 — Poppler draws almost nothing for a cairo ligature font that
we draw), `bug1552113` (0.22 — the pdf.js reproducer whose caption is
"this text should be visible"; Poppler paints the page black, we
show the text) and `issue21068` (0.50 — widget annotations with
appearance streams that Poppler suppresses). The content-aware
quality bar (`render_quality`) exists for exactly this: per-page
`ink_ours`/`ink_ref` make the disagreement visible instead of
averaging it away.

Remaining render blockers by first-error count (`expect: pass`,
strict parse ok): JBIG2 images **45**, annotations without an
appearance stream **42** (AcroForm widgets needing synthesis —
LiveCycle/IRS forms), CIDFontType0 (CFF-based CID fonts) **34**
(every Chrome/LibreOffice/Google Docs print with a CJK or ligature
font), `/ActualText` spans **17**, optional content **11 + 3**,
tiling patterns **9**, ASCII85/ASCIIHex/LZW content filters **7**,
CCITT **4**, JPX **2**, `/UserUnit` **2**, function-based shading
**2**. Text extraction is blocked by the same CFF CID fonts (38) and
`/ActualText` (17); rewrite by the deliberate profile (27 signed
documents, 14 `/OCProperties`, 3 XFA, 3 `/AA`, 1 JavaScript tree)
and 6 dangling references.

Performance: median 31 ms per file (parse + probe + render of up to
3 pages + text + rewrite), p90 124 ms, p99 536 ms, max 1.05 s
(`arxiv-2610.24216`, 26 pages); nothing approached the 20 s timeout.
Peak RSS is a process-wide high-water mark (381 MB at the end of the
run, reached on the 41 MB arXiv files), so per-file memory is an
upper bound; the limits that bound it (`max_image_bytes`,
`max_render_buffer_bytes`, `max_total_decoded_bytes`) were not hit by
any well-formed file — the two `max_image_dimension` hits are
deliberate refusals of > 16384-pixel images.

### Results after CID CFF fonts, ActualText, optional content and AcroForm synthesis (run-1792769430)

Same 511 files, same conditions, measured against the
run-1792715480 baseline above. The headline now comes from one
table whose rows are separate populations (`analysis::Tally`): the
earlier report's "render 177" was the `expect: pass` population and
its "full PASS 198" the whole corpus, which is why the second
number could exceed the first; each row below carries its own
denominator.

| files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 448 | 296 | 389 | 390 | **254** | 30 | 0 |
| expect: pass | 431 | 408 | 267 | 357 | 352 | **226** | 10 | 0 |
| expect: reject | 43 | 4 | 3 | 3 | 3 | 3 | 20 | 0 |
| expect: any | 37 | 36 | 26 | 29 | 35 | 25 | 0 | 0 |

Against the baseline (all 511): render **203 → 296**, text **320 →
389**, rewrite 390 → 390, full PASS **198 → 254** (+56), 0 newly
failing, 0 lost in any stage, 0 crash / 0 timeout; robustness 43/43
safely handled (40 cleanly refused). The 219 files with a fidelity
measurement in both runs average SSIM 0.9476 → 0.9465; the only
changes above 0.005 are `irs-f1096` (0.958 → 0.852) and `irs-fw2`
(0.941 → 0.818), whose `/NeedAppearances true` fields are now
regenerated from their values while Poppler keeps the stale
author-supplied streams — the page content is identical on
inspection, the field text differs in size. Newly measured: 80
files, median SSIM 0.952; the lowest (`chrome-wiki-ja-tategaki`
0.65, the IRS 1040 family 0.72–0.74) are heavier CFF glyph
rendering of CJK text and synthesised form fields that Poppler does
not draw, not missing or misplaced content.

What each feature unblocked (expect: pass, first-error counts):

| feature | before | after | newly full PASS (examples) |
|---|---:|---:|---|
| CIDFontType0 / CFF CID fonts (+ OpenType `/FontFile3`, embedded CMaps, vertical) | render 34, text 38 | 0 | all 8 CC BY arXiv papers, 13 Chrome prints incl. 6 Japanese, `embedded_font`, `extract_link`, `vertical` |
| `/ActualText` | text 17 | 0 | `chrome-form-controls-*`, `chrome-wiki-*` (Chrome emits it for every ligature) |
| optional content | render 11 + 3, text 11 | 0 | `cid_cff`, `cmykjpeg`, `s2`, `transparency_group`, `inks` now reach their next blocker |
| AcroForm appearance synthesis | annotations without `/AP` 42 | 14 (all non-widget: `/Ink`, `/Square`, `/Line` … test files) | `irs-*` forms, `pdfium-text_form`, `pdfium-multiple_form_types`, `checkbox_no_appearance`, `annotation-tx`, `acroform_calculation_order` |

Per category (expect: pass; strict counts):

| category | n | parse | render | text | rewrite | full PASS | what still blocks |
|---|---:|---:|---:|---:|---:|---:|---|
| japanese | 9 | 9 | 8 | 9 | 8 | 7 | 1 JBIG2 image, 1 `/OCProperties` outside the rewrite profile |
| office | 16 | 16 | 11 | 14 | 13 | 8 | dangling references, a signed document (rewrite refused by design), a content stream ending inside `BT`, one `/AS`-less two-state widget without a value |
| browser | 19 | 19 | 16 | 16 | 19 | 16 | non-Normal blend inside a group, an ExtGState luminosity soft mask, a `bfrange` overflow |
| scan | 13 | 13 | 12 | 13 | 13 | 12 | 1 JBIG2 |
| forms | 38 | 38 | 30 | 31 | 13 | 11 | rewrite refuses 19 **signed** documents (`/Perms`) and 3 XFA — deliberate profile limits, not failures; render: `/UserUnit 3`, a `bfrange` overflow, a malformed number |
| papers | 10 | 10 | 10 | 10 | 9 | 9 | 1 dangling reference in rewrite |
| graphics | 61 | 61 | 21 | 54 | 59 | 19 | JBIG2 (the `bitmap-*` suite), tiling patterns, function-based shading |
| annotations | 52 | 50 | 33 | 49 | 42 | 28 | non-widget annotations without `/AP`, `/OCProperties` in rewrite |

Remaining render blockers (expect: pass, strict parse): image
filters **53** (JBIG2 45, CCITT 4, JPX 2, …), non-widget annotations
without `/AP` **14**, tiling patterns **9**, ASCII85/ASCIIHex/LZW
content filters **7**, ExtGState soft masks **3**, non-embedded CID
fonts **3**, `/UserUnit` **2**, function-based shading **2**, image
dimension limit **2**. Text is blocked by the same filters, tiling
patterns and soft masks plus 5 files with a truncated dictionary;
rewrite by the deliberate profile (27 signed, 14 `/OCProperties`,
3 XFA, 3 `/AA`) and 6 dangling references. Performance: median 41
ms per file, p90 236 ms, p99 879 ms, max 1.34 s (`chrome-wiki-zh-hanzi`,
CFF CID text on 30+ pages); no file approached the 20 s timeout and
no limit was hit except the two deliberate `max_image_dimension`
refusals.

### Results after JBIG2/CCITT, byte filters, markup annotations and tiling patterns (run-1792801802)

Same 511 files, measured against run-1792769430 above. Four pdfium
test files whose hand-written objects lack `endobj` were retagged
`expect: any` (see the mutation note), so the compatibility population
is 427.

| files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 444 | 372 | 399 | 383 | **329** | 34 | 0 |
| expect: pass | 427 | 404 (94.6%) | **342 (80.1%)** | 368 (86.2%) | 348 (81.5%) | **301 (70.5%)** | 10 | 0 |
| expect: reject | 43 | 1 | 0 | 0 | 0 | **0** | 23 | 0 |
| expect: any | 41 | 39 | 30 | 31 | 35 | 28 | 1 | 0 |

Against run-1792769430 (all 511): render **296 → 372**, text 389 →
399, rewrite 390 → 383, full PASS **254 → 329** (+80 newly, −5), 0
crash / 0 timeout; robustness 43/43 safely handled, 43 refused, and
for the first time **no `expect: reject` file scores a full PASS**.
The five files that left full PASS all lack `endobj` on at least one
object (three `drop-endobj` mutations, `pdfium-bug_459580`,
`pdfium-gotoe_action` — the latter also has a stray `>>`): the strict
parser now reports that (§7.3.10) and recovery reads them, which is
the classification they should always have had. Two more pdfium files
(`bug_779`, `hello_world_2_pages_split_streams`) keep render/text but
lose the strict rewrite walk for the same reason.

What each feature unblocked (newly full PASS, by first-error cause):

| feature | newly full PASS | files |
|---|---:|---|
| JBIG2 (`hayro-jbig2`) | 45 | the 44 pdf.js `bitmap-*` files (generic, refinement, symbol/text with Huffman variants, halftone, composition ops, TPGDON) and `jbig2_file_header` |
| CCITT (`hayro-ccitt`) | 5 | `ccitt_endofblock_false`, `images_1bit_grayscale`, `issue13372` (stencil painted with a shading pattern), `issue14999_reduced` (Type 3 glyphs as inline `/CCF` images — also fixed `/Widths` through a `[1 0 0 -1 0 0]` `/FontMatrix`), `issue19532` (inline `/CCF` running straight into `EI`) |
| ASCII85 / ASCIIHex / LZW / RunLength | 6 | `bug1337429`, `bug864847`, `pdfium-bug_1646`, `pdfium-bug_402562387`, `pdfium-bug_444176962`, `pdfium-jpeg_reduced_size_with_smask` |
| markup annotations without `/AP` | 13 | the nine `annotation-*-without-appearance` files (Square/Circle, Line, Line with empty `/Rect`, Polygon/PolyLine, Ink, Highlight, Underline, StrikeOut, Squiggly), `issue12337`, `issue22124`, `pdfium-freetext_annotation_without_da`, `pdfium-redact_annot` |
| tiling patterns (+ pattern strokes and text) | 11 | `tiling_patterns_variations`, `tiling-pattern-box`, `pdfium-pattern_fill_uncolored`, `pdfium-pattern_stroke_uncolored`, `colorkeymask`, `issue16038`, `issue21878`, `pattern_text_embedded_font`, `showtext-shadingpattern`, `contentstreamnocycletype3insidetype3`, `verapdf…6-2-3-3-t01-fail-o` |

Per category (expect: pass; render and full PASS as before → after):

| category | n | parse | render | text | rewrite | full PASS |
|---|---:|---:|---:|---:|---:|---:|
| misc | 154 | 136 | 85 → 112 | 119 | 122 | 77 → 104 |
| graphics | 61 | 61 | 21 → **52** | 57 | 59 | 19 → **50** |
| annotations | 52 | 50 | 33 → **44** | 49 | 42 | 28 → **39** |
| forms | 38 | 38 | 30 | 31 | 13 | 11 |
| browser | 19 | 19 | 16 | 16 | 19 | 16 |
| pdfa | 17 | 17 | 16 | 16 | 17 | 16 |
| office | 16 | 16 | 11 | 14 | 13 | 8 |
| images | 14 | 14 | 9 → 11 | 14 | 13 | 8 → 10 |
| latex | 13 | 13 | 12 → 13 | 13 | 12 | 11 → 12 |
| scan | 13 | 13 | 12 → **13** | 13 | 13 | 12 → 13 |
| papers | 10 | 10 | 10 | 10 | 9 | 9 |
| japanese | 9 | 9 | 8 → **9** | 9 | 8 | 7 → 8 |
| transparency | 5 | 5 | 1 → 4 | 5 | 5 | 1 → 4 |

Fidelity (render success and quality are separate counts). Common
pages measured in both runs: 299 files, mean SSIM 0.9344 → 0.9406; the
nine files that moved (`copy_paste_ligatures` 0.005 → 0.765,
`issue21068` 0.50 → 0.95, `issue18072`, `highlight_popup`, …) did so
because the runner now renders the Poppler reference with
`pdftoppm -cropbox` — our page is the `/CropBox`, Poppler's default is
the `/MediaBox`, and the two pixmaps could not be aligned before. No
common page regressed. Newly measured: 80 files, mean 0.9575, median
0.986; below 0.8: `issue21878` (0.45 — a 60 × 36 pt page of
LaTeX/pgf hatching whose tile seams dominate at 96 dpi; identical
to Poppler at 288 dpi, see below), `jbig2_file_header` (0.46 —
Poppler's `jbig2dec` *fails* on the T.88 file header and renders a
blank page, so the reference is empty while ours shows the bitmap),
`issue13372` (0.65 — 646 × 761 halftone stencil through a rainbow
shading, identical placement and colours; the dot pattern is
resampled differently), `issue16038` (0.67 — 73 × 39 pt pgf hatch,
same seam effect). Mean MAE over all 379 measured files is 0.018.

Visually inspected against Poppler at 72 dpi unless noted (page 1 in
each case): `bitmap-symbol-textrefine` (smiley; identical, no
inversion), `ccitt_endofblock_false` (six `/K -1 / 0 / 1` labels on a
blue card; identical), `issue13372` (balloons stencil; identical
layout), `issue19532` and `issue14999_reduced` (Type 3 text made of
CCITT glyphs; identical after the `/Widths` fix — before it the
glyphs piled up at one origin), `bug1337429` (LZW text; identical),
`jbig2_file_header` (ours: two black shapes, Poppler: blank),
`annotation-square-circle-without-appearance`,
`annotation-ink-without-appearance`,
`annotation-polyline-polygon-without-appearance`,
`annotation-line-without-appearance`, `…-empty-rect`,
`annotation-squiggly-without-appearance` (all identical in position,
line width, colour),
`pdfium-freetext_annotation_without_da` (red box, "Hello!" in the
Helvetica fallback; identical), `issue12337` (yellow highlight over
an ACM abstract; the `/Multiply` blend keeps the text black as in
Poppler), `tiling_patterns_variations` (nine panels: coloured and
uncoloured, matrix, gaps, rotation; identical),
`tiling-pattern-box` (dashed grid — ours slightly lighter at the
left/bottom edge cells), `pdfium-pattern_stroke_uncolored` and
`colorkeymask` (identical), `issue16038` and `issue21878` at 288 dpi
and a replica of the `issue16038` cell at 576 dpi (the faint seams at
every tile edge appear in Poppler's output as well — they are the
anti-aliased `/BBox` clip of each cell, not a stamping error). Not
inspected: the remaining 43 `bitmap-*` files beyond the four viewed
(all SSIM ≥ 0.908 with ink ratios within 1 % of the reference).

Remaining render blockers (expect: pass, strict parse, first error):
`/X is not a stream` 3, ExtGState luminosity soft masks 3,
non-embedded CID fonts 3, `/Annots` entries that are not
dictionaries 2, `/UserUnit` 3 2, `bfrange` overflow 2,
function-based shading 2, `max_image_dimension` 2, JPX 2, dangling
references 4, malformed content streams (unbalanced `q`/`Q`, operators
inside a path object) 4, non-separable blend on an image 1,
non-Normal blend inside a group 1, a Type 3 glyph whose pattern cell
paints with its own font (`contentstreamcycletype3insidetype3`,
refused as a cycle — Poppler breaks the cycle and draws the rest) 1,
Arabic FreeText contents (`freetext_no_appearance`, no CJK/Arabic
fallback font) 1, `/JBIG2Decode` on a *content* stream
(`bomb_giant`, an image-only filter applied to page content) 1.
Rewrite is bounded by the deliberate profile (27 signed, 14
`/OCProperties`, 3 XFA, 3 `/AA`, 2 PDF-2.0) plus 6 dangling
references. Performance: median 43 ms per file, p90 232 ms, p99 902
ms, max 1.34 s; no limit was hit except the two deliberate
`max_image_dimension` refusals.

### Results after soft masks, group compositing, JPX, parser fixes and the round-trip pipeline (run-1792806451)

Same 511 files, same conditions, measured against run-1792801802.
Render blockers were re-classified first (62 `expect: pass` files
that parsed strictly but did not render): 23 engine faults (content
arrays split inside a dictionary, CMap tokens glued to `>>`, indirect
`/DecodeParms`, bfrange carry, `/Identity` ToUnicode names, null
`/Annots` entries), 14 unimplemented features (soft masks, isolated
groups with blend modes inside, non-separable blends, JPX, UserUnit,
knockout, function-based and mesh shadings, text clip modes,
non-embedded CJK fonts), 15 faults in the PDF itself (dangling
references, unbalanced `q`/`Q`, operators inside path objects,
truncated Flate streams, a JBIG2 filter on a content stream), 2
resource limits (`max_image_dimension`), 3 deliberate refusals
(unsigned Latin-only fallback for Arabic FreeText, ZapfDingbats
without a licence-clean substitute, a genuine Type 3/pattern cycle)
and no reference-implementation disagreement. The first two groups
were addressed where the fix is exact (see the feature docs); the
third is left to recovery mode by design.

| files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 444 | 393 | 411 | 383 | **340** | 34 | 0 |
| expect: pass | 427 | 404 (94.6%) | **362 (84.8%)** | 380 (89.0%) | 348 (81.5%) | **311 (72.8%)** | 10 | 0 |
| expect: reject | 43 | 1 | 0 | 0 | 0 | 0 | 23 | 0 |
| expect: any | 41 | 39 | 31 | 31 | 35 | 29 | 1 | 0 |

Against run-1792801802 (all 511): render **372 → 393**, text 399 →
411, rewrite 383 → 383, full PASS **329 → 340**; 0 newly failing, 0
lost in any stage, 0 crash / 0 timeout; robustness 43/43 handled,
43 refused, 0 `expect: reject` full PASS. Newly rendering (21):

| cause removed | files |
|---|---|
| `/Contents` array split inside a `<< >>` (§7.8.2) | `irs-f1040es` `irs-f1040s2` `irs-f1040sd` `irs-f1120s` `irs-f990` `irs-fss4` |
| ExtGState `/SMask` (luminosity / alpha) | `chrome-wiki-zh-hanzi` `issue14297` |
| isolated group with a blend mode inside, non-separable blends | `chrome-wiki-de-schriftart` `blendmode` |
| JPX (`hayro-jpeg2000`), JPX soft mask | `issue19326` `bug_jpx` `s2` |
| `/UserUnit` honoured | `bug1947248_forms` `bug1947248_text` |
| bfrange carry (Chrome Hangul), CMap delimiter lexing, `/ToUnicode /Identity`, indirect `/DecodeParms` | `chrome-wiki-ko-hangul` `issue12963` `bug920426` `identitytounicodemap_charcodeof` `bug1650302_reduced` `issue14814` |
| null `/Annots` entry skipped | `annotation-text-without-popup` |

Per category (expect: pass; render / full PASS before → after):
misc 112 → 118 / 104 → 108, graphics 52 → 55 / 50 → 53, annotations
44 → 45 / 39 → 40, **forms 30 → 37** / 11 → 12 (the IRS forms;
rewrite still refuses the 25 signed ones), **browser 16 → 19** / 16
→ 17 (every Chrome print now renders), japanese 9 / 8, scan 13 / 13,
papers 10 / 9, office 11 / 8, images 11 / 10, latex 13 / 12,
transparency 4 / 4, pdfa 16 / 16.

Fidelity (separate from render success): 379 files measured in both
runs, mean SSIM 0.9442 → 0.9442, no file moved by more than 0.005.
Newly measured: 21 files, mean 0.868, median 0.862, 7 below 0.8 —
the six IRS forms (0.70–0.86: `/NeedAppearances` fields regenerated
by us, stale appearances kept by Poppler, as with the earlier IRS
set) and the three Chrome prints (0.71–0.77: CJK/Latin glyph weight
through CFF hinting, plus the Hangul page's many small glyphs);
`bug1947248_*` are rendered three times Poppler's size because
Poppler ignores `/UserUnit`, so their metric is resampled.
Inspected against Poppler at 72 dpi: `blendmode` (all 16 modes,
identical including Hue/Saturation/Color/Luminosity),
`issue14297` (soft-masked header gradients, identical),
`chrome-wiki-zh-hanzi` (language-switch dialog with a luminosity
shadow mask over Chinese text; renders, Poppler reference not
produced for comparison), `irs-f1040s2` (identical),
`bug1947248_text` (same content at 3× size), `issue19326` (JPX
"JPX" text, identical), `s2` (JPX photo with JPX soft mask, matches
the reference colours and masking).

Remaining render blockers (expect: pass, strict parse, 42 files):
non-embedded CJK CID fonts 3 (+ non-embedded TrueType 1, ZapfDingbats
1), dangling references 4, function-based shading 2, tensor-patch
mesh 1, `max_image_dimension` 2, malformed content streams 7
(unbalanced `q`/`Q`, operators inside path objects, `BT` without
`ET`, truncated Flate, bad inline-image `EI`, undefined `/Properties`
name, bad `/Differences`), knockout group 1, text clip modes 4–7 1,
Arabic FreeText 1, a genuine cycle 1, multi-state `/AP` without `/AS`
on a non-widget 1, JBIG2 on a content stream 1, zero-size page 1.
Performance: the slowest is `bomb_giant` at 40 s, which is two
20-second kills of external tools (Poppler and qpdf) on the rewritten
JBIG2 bomb — our own stages take milliseconds on it. Peak RSS 710
MB (page-sized layers and the two-pass non-isolated groups on a large
transparency page). Median 136 ms per file including the round
trip, p90 554 ms, p99 4.5 s.

### Round-trip results (run-1792806451)

444 strictly parsed files; rates are PASS over attempted, *full*
over the 444 (a refused rewrite is not a round trip).

| group | files | rewrite P/F/U | reparse | structure | render P/N/A | text P/N/A | annots P/N/A | qpdf clean/warn/err | **full** | size ×mean/max |
|---|---:|---|---|---|---|---|---|---|---:|---|
| all | 444 | 383 / 10 / 51 | 383 / 0 | 383 / 0 | 342 / 41 | 354 / 29 | 122 / 261 | 365 / 16 / 2 | **383 (86.3%)** | 0.99 / 1.64 |
| expect: pass | 404 | 348 / 6 / 50 | 348 / 0 | 348 / 0 | 313 / 35 | 325 / 23 | 119 / 229 | 330 / 16 / 2 | **348 (86.1%)** | 0.99 / 1.64 |
| feature: acroform | 89 | 50 / 2 / 37 | 50 / 0 | 50 / 0 | 46 / 4 | 48 / 2 | **50 / 0** | 46 / 4 / 0 | 50 | 0.99 / 1.63 |
| feature: annots | 163 | 120 / 6 / 37 | 120 / 0 | 120 / 0 | 111 / 9 | 114 / 6 | **118 / 2** | 108 / 12 / 0 | 120 | 1.02 / 1.64 |
| feature: dests | 80 | 44 / 1 / 35 | 44 / 0 | 44 / 0 | 41 / 3 | 41 / 3 | 43 / 1 | 43 / 1 / 0 | 44 | 1.00 / 1.21 |
| feature: multipage | 97 | 65 / 5 / 27 | 65 / 0 | 65 / 0 | 61 / 4 | 61 / 4 | 40 / 25 | 60 / 5 / 0 | 65 | 1.01 / 1.50 |
| feature: incremental | 151 | 111 / 2 / 38 | 111 / 0 | 111 / 0 | 105 / 6 | 107 / 4 | 43 / 68 | 107 / 4 / 0 | 111 | 1.01 / 1.64 |
| feature: embedded-fonts | 210 | 169 / 3 / 38 | 169 / 0 | 169 / 0 | 155 / 14 | 157 / 12 | 73 / 96 | 165 / 4 / 0 | 169 | 1.02 / 1.64 |
| feature: nonembedded-fonts | 70 | 66 / 3 / 1 | 66 / 0 | 66 / 0 | 53 / 13 | 54 / 12 | 32 / 34 | 63 / 2 / 1 | 66 | 0.98 / 1.64 |
| feature: images | 118 | 107 / 1 / 10 | 107 / 0 | 107 / 0 | 100 / 7 | 102 / 5 | 29 / 78 | 106 / 1 / 0 | 107 | 0.99 / 1.64 |
| feature: transparency | 127 | 113 / 2 / 12 | 113 / 0 | 113 / 0 | 103 / 10 | 104 / 9 | 61 / 52 | 112 / 1 / 0 | 113 | 0.99 / 1.64 |
| feature: signed | 27 | 0 / 0 / **27** | – | – | – | – | – | – | 0 | – |
| feature: optional-content | 17 | 0 / 0 / **17** | – | – | – | – | – | – | 0 | – |

Reading: **every rewrite that the engine performed survived
reparsing and is the same object graph** (383/383), renders
identically under our renderer wherever the original renders
(342/342 attempted; 41 N/A are files our renderer refuses), extracts
the same characters at the same positions (354/354), keeps every
annotation, field value and named destination (122/122), and
Poppler agrees: page-1 SSIM between original and rewritten averages
1.0000 over the 394 pairs it could render (lowest 0.993, Poppler's
own anti-aliasing noise). The 10 rewrite *failures* are all
files with dangling references or objects without `endobj` — the
strict walker reports the fault the file has (recovery mode would
read them). The 51 refusals are the deliberate profile: 27 signed
documents, 17 `/OCProperties`, 3 XFA, 3 `/AA`, 2 PDF 2.0. qpdf finds
no errors in 365 of 383 outputs; the 16 warnings are properties of the
inputs that the rewrite copies verbatim — pages without a
`/Resources` dictionary ("kid … Resources is missing or invalid;
repairing") and Flate streams qpdf flags as "complete but output may
still be valid" — and the 2 errors are the two
decompression-bomb inputs whose content qpdf cannot decode either. Output size: mean 0.99× the input (object
streams are written as plain objects, which costs up to 1.64× on
`smaskdim`/`issue14307`; unreferenced objects and dead revisions are
dropped, which is why `filled-background` shrinks to 0.3 %).
Per-category rows (annotations 42/42, forms 13/13 of 38 — 25 signed
—, browser 19/19, graphics 59/59, japanese 15/15, scan 13/13, papers
9/9, office 13/13, images 15/15, latex 12/12 full) are in the report.

### Results after shadings 1/4–7, text clip modes, CJK fallback, incremental save and the all-page round-trip (run-1792812028)

Same 511 files, measured against run-1792806451 — with one change of
conditions that matters for every comparison below: the run used
`--mode full`, so **every page** of every file was rendered and
extracted (1 491 pages), where the baseline evaluated the first 3
pages. The two runs are therefore compared twice: like for like
(pages 1–3) for the regression check, and on the new all-page basis
for the baseline.

| files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 444 | 397 | 413 | 383 | **345** | 34 | 0 |
| expect: pass | 427 | 404 (94.6%) | **365 (85.5%)** | 381 (89.2%) | 348 (81.5%) | **315 (73.8%)** | 10 | 0 |
| expect: reject | 43 | 1 | 0 | 0 | 0 | 0 | 23 | 0 |
| expect: any | 41 | 39 | 32 | 32 | 35 | 30 | 1 | 0 |

Against run-1792806451 (all 511): render **393 → 397**, text 411 →
413, rewrite 383 → 383, full PASS **340 → 345**; 0 crash / 0
timeout; robustness 43/43 handled. Render on `expect: pass`:
84.8 % → **85.5 %** on the all-page basis; on the like-for-like
(pages 1–3) basis the same files give **86.2 %** (368/427) — the 90 %
target was not reached (see the blocker list below for what the
remaining 59 files need).

Newly rendering (6, `expect: pass`):

| cause removed | files |
|---|---|
| function-based shading (type 1) | `function_based_shading` `function_based_shading_cmyk` |
| tensor-product patch mesh (type 7) | `issue13520` |
| text clipping mode `Tr 4` | `glyph_accent` |
| `/ExtGState /Font [ref size]` | `verapdf_verapdf-test-suite-6-1-12-t02-pass-l` |
| indirect stream `/Length` clipped by the `endstream` scan (parser fix) | `images` |

Regressions: **none like for like** — no file that rendered in the
baseline fails on pages 1–3. Three files now fail on pages the
baseline never evaluated: `arxiv-2303.18223` (page 17, invalid hex
in a `/ToUnicode` CMap — a structure fault of the file that does not
affect drawing; candidate for "report, don't fail" in the next
phase), `mlp-mixer` (page 42, knockout group) and `standard_fonts`
(page 13, non-embedded Symbol font). They are reclassified from
PASS to FAIL by the wider denominator (3 pages → all pages), not by
a code change; three more all-page failures found by the first
full run (`max_total_decoded_bytes` exhausted on pages 4–13 of
`arxiv-2610.24216`, `chrome-wiki-ko-hangul`, `chrome-wiki-zh-hanzi`)
were engine limitations and were fixed before this run.

Per category (expect: pass; render / full PASS before → after, raw
stage classes): misc 123 → 123 / 108 → 107, graphics 55 → **57** /
53 → 55, annotations 45 / 40, forms 37 / 12, browser 19 / **17 → 19**,
images 11 → **12** / 10 → 11, japanese 9 / 8, latex 13 → 11 / 12 → 10
(the two all-page failures above), scan 13 / 13, papers 10 / 9,
office 11 / 8, transparency 4 → **5** / 4 → 5, pdfa 16 → **17** /
16 → 17.

Fidelity (separate from render success): measured on 407 files /
**1 087 pages** (was 400 files / 540 pages) with a Poppler
reference; 245 (was 241) meet the comparability bar, 255 (was 249)
the content bar. 66 files draw with a bundled Latin substitute. The
three new shading files land at SSIM 0.95 (`function_based_shading`:
a 128-sample grid against Poppler's per-pixel evaluation — visible
only in the sharp-edged checkerboard panel), 0.97 and 0.99.

Round-trip (444 strictly parsed files; the full table is in
`compat/reports/run-1792812028.md`): rewrite 383 PASS / 10 FAIL /
51 UNSUPPORTED (the 10 failures are dangling references and
unterminated objects the strict parser accepts lazily — unchanged),
reparse 383/383, structure 383/383, render **350/350 over every
page** (33 N/A: our renderer refuses the original), text 360/360,
annotations/forms/dests 122/122; **pages compared 1 308, skipped 0
(no file hit the 120 s budget), failed 0**; Poppler witnessed 382
files on every page with **0 divergent**, mean SSIM 0.99997, worst
0.9931 (`mut-*` recovered inputs that Poppler renders differently
are outside the strict set); qpdf on the rewritten file: 369 clean /
12 warning / 2 error — **external validity 369/383**, **no newly
introduced external diagnostic 383/383** (the four warnings the
rewrite used to add are gone with the `/Length` fix; the 12
remaining and both errors are present on the originals too, see the
findings table). *full* (all eight judgements) **383/444**.

CJK fallback (measured separately with a `cjk-fallback` runner
build, `--only`, since the baseline binary is the default build):
`noembed-eucjp` (`EUC-H`, non-embedded MS-Gothic, `expect: any`)
UNSUPPORTED → **PASS**, Poppler SSIM **0.9966**, diff-px 0.17 %,
reported as `MS-Gothic → NotoSansJP-Regular`; `90ms_rksj_h_sample`
(`expect: any`) now loads its CMap and fails on the next fault — the
CIDFont has no `/FontDescriptor` (required by Table 117) — a
structure error of the sample, not a fallback gap; the Japanese
files with embedded fonts render identically with and without the
feature (SSIM unchanged to the last digit). The three `expect: pass`
non-embedded CID files (`issue11578_reduced`, `issue11740_reduced`,
`issue12418_reduced`) use `/Ordering (Identity)` — CIDs are glyph
indices of an unknown font, so no fallback can be sound and they
stay refused; `simfang-variant` / `xiaobiaosong` are non-embedded
Chinese TrueType (`expect: any`, GB1 not bundled).

Performance (all pages, Poppler and image comparison included):
median 192 ms per file, p90 1.1 s, p99 29.8 s, max 44.8 s
(`chrome-wiki-ko-hangul`, 23 pages); engine stage totals over the
corpus: parse 0.06 s, render 204 s, text 49 s, rewrite 0.5 s;
process high-water mark 949 MB (cumulative over the run, an upper
bound per file). Robustness: 43/43 `expect: reject` files refused
without crash or timeout.

Remaining render blockers on `expect: pass` (59 files, 13 of them
strict-parse failures): non-embedded CID fonts with Identity
ordering 3, truncated / unterminated Flate streams 3, dangling
references 4, operators inside path objects or unbalanced `q`/`Q` 7,
`max_image_dimension` 2, knockout groups 2, non-embedded Symbol /
ZapfDingbats 2, `/Annots` entries that are not dictionaries 2,
`/AP /N` state dictionaries without `/AS` 2, invalid `/ToUnicode`
hex 1, Arabic FreeText fallback 1, Type 3 / pattern cycle 1, JBIG2 on
a content stream 1, `/BrotliDecode` 1, others 1 each.

### Results after the warnings channel: unterminated Flate streams and malformed `/ToUnicode` entries (run-1792845977)

Same 511 files, `--mode full` (every page), default features, measured
against run-1792812028 like for like. The code change is narrow — see
[`security.md`](security.md) § Three outcomes: a `/FlateDecode`
stream that ends exactly after a sync-flush block without the final
block and Adler-32 is now decoded (every block was complete; only the
checksum is unverifiable) and recorded as
`Warning::UnterminatedFlateStream`; hex strings in a `/ToUnicode`
CMap may contain white space (§7.3.4.3); a `bfchar`/`bfrange` entry
that cannot be read is skipped with `Warning::MalformedToUnicodeEntry`
and its codes stay unmapped. A stream cut inside a block, a corrupt
stream, and a malformed codespace range are still errors.

| files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 446 | 400 | 416 | 385 | **348** | 32 | 0 |
| expect: pass | 427 | 406 (95.1%) | **368 (86.2%)** | 384 (89.9%) | 350 (82.0%) | **318 (74.5%)** | 8 | 0 |
| expect: reject | 43 | 1 | 0 | 0 | 0 | 0 | 23 | 0 |
| expect: any | 41 | 39 | 32 | 32 | 35 | 30 | 1 | 0 |

Against run-1792812028: parse **404 → 406**, render **365 → 368**,
text 381 → 384, rewrite 348 → 350, full PASS **315 → 318** on
`expect: pass`; all 511: full PASS 345 → 348; 0 crash / 0 timeout;
robustness 43/43 refused.

Newly passing (3, `expect: pass`):

| cause removed | files | how |
|---|---|---|
| xref stream and annotation appearance streams end after a sync flush (70 / 851 bytes decoded exactly) | `comments` `highlights` | **accepted with warnings** — the only two warned files in the corpus; both are full PASS, so the clean full PASS is 316 on `expect: pass` and 346 on all files |
| white space inside `/ToUnicode` hex strings (`<00 41>`) on page 17 | `arxiv-2303.18223` | conforming per §7.3.4.3 — a parser gap, not a deviation; no warning |

Regressions: **none** — no file and no stage that passed in
run-1792812028 fails now; `issue13316_reduced` (corrupt Flate data)
is still refused with the new, more specific message
`FlateDecode failed: corrupt deflate stream`. Round trip: 385 strictly
parsed files rewritten (10 rewrite failures, 51 unsupported), 385
reparse with an identical object graph, render and text identical on
every compared page (352 / 362 files; the rest have no comparable
page), external validation introduced no new qpdf or Poppler
diagnostic in any file (`external_no_new` 0 failures), Poppler
mean SSIM 0.99997 on 1 488 pages.

Remaining render blockers on `expect: pass` (59 files, 13 of them
strict-parse failures) after this run, in the order the next phase
should take them: dangling references in strict mode 4 (a recovery
`null` policy — the fix is in the recovery parser, not the strict
one), operators inside path objects or unbalanced `q`/`Q` 7 (file
faults; candidates for warnings only where the drawing is provably
unaffected), non-embedded CID fonts with `/Ordering (Identity)` 3
(no sound fallback exists), knockout groups 2, `max_image_dimension`
2 (a `Limits` decision, not a gap), non-embedded Symbol /
ZapfDingbats 2 (no licence-clean metric clone), `/Annots` entries
that are not dictionaries 2, `/AP /N` state dictionaries without
`/AS` 2, Arabic FreeText fallback 1, Type 3 / pattern cycle 1, JBIG2
on a content stream 1, `/BrotliDecode` 1, others 1 each. Chinese and
Korean predefined CMaps (`GB1`, `CNS1`, `Korea1`) are not bundled:
the crate is 7.2 MiB compressed against the 10 MiB crates.io limit,
so they would need a separate asset crate.

### 0.2 API refactor: results identical (run-1792850880)

The 0.2.0 public-API changes (`docs/migration-0.2.md`) touch no
engine logic. Same 511 files, `--mode full`, default features,
measured against run-1792845977 with the runner migrated to the new
API: every stage class, every verdict, every per-file warning list,
every round-trip class, every Poppler SSIM value and every cause id
are identical (0 differences across all four comparisons); the
headline table, the round-trip table and the accepted-with-warnings
count are the same to the digit. Robustness 43/43, 0 crash / 0
timeout. The signature-policy refusals now arrive as
`Error::SignaturePolicy` and are mapped by the runner to the same
`rewrite.signature.unsupported` cause id as before.

### Results after §7.3.10 null references, recovery rules, knockout groups and the symbol fallback (run-1792865059)

Same 511 files, `--mode full`, default features, measured against
run-1792853774 (the 0.2.0 engine). The runner now reports three
independent render attributes (see § *Result classification* below):
**strict**, **approximate** (part of strict) and **recovered** (never
part of strict). Per-file findings are in the gitignored diagnosis
report written by `scripts/diag-render-blockers.py`; only category
counts appear here.

| files | n | parse | render (strict) | text | rewrite | full PASS | recovered | crash/timeout |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | 446 | 408 | 422 | 385 | **353** | 37 | 0 |
| expect: pass | 427 | 406 (95.1%) | **376 (88.1%)** | 390 (91.3%) | 350 (82.0%) | **323 (75.6%)** | 12 | 0 |
| expect: reject | 43 | 1 | 0 | 0 | 0 | 0 | 24 | 0 |
| expect: any | 41 | 39 | 32 | 32 | 35 | 30 | 1 | 0 |

| render outcomes | n | strict | of which approximate | recovered | unsupported | rejected | warnings | recovery notes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| all | 511 | **408** | 68 | 40 | 14 | 49 | 50 | 36 |
| expect: pass | 427 | **376** | 67 | 18 | 9 | 24 | 39 | 27 |
| expect: reject | 43 | **0** | 0 | 20 | 0 | 23 | 10 | 9 |
| expect: any | 41 | **32** | 1 | 2 | 5 | 2 | 1 | 0 |

Against the 0.2.0 engine on `expect: pass`: strict render **368 → 376**
(+8; the 90 % line is 385), text 384 → 390, rewrite 350 → 350, full
PASS 318 → 323; recovered render 8 → 18; no file or stage that passed
strictly before fails now; 0 crash / 0 timeout; robustness 43/43
refused in strict mode (20 of the 43 now render in recovery mode, where
they are reported as such). Round trip: 385 strictly parsed files
rewritten, 385 reparse with an identical object graph, render and text
identical on every compared page, 411 full round trips (unchanged), no
new qpdf or Poppler diagnostic; Poppler SSIM did not drop on any file.
27 files are accepted with warnings (5 before), 6 of them full PASS.

Strict gains by cause (10 new strict renders):

| cause removed | files | how |
|---|---:|---|
| reference to an undefined object | 5 | null per §7.3.10, `Warning::UndefinedReference` (also fixes an annotation whose `/AP /N` was undefined and a font whose `/SMask` was) |
| content stream ending inside `q` / `BT` | 2 | closed at the end, `Warning::UnterminatedContentBlock` |
| unrecognised rendering intent | 1 | RelativeColorimetric fallback (§8.6.5.8), `Warning::UnknownRenderingIntent` |
| knockout transparency group | 1 | §11.4.8 implemented; the second candidate is now blocked by `max_xobject_depth` |
| non-embedded Symbol | 1 | the bundled symbol fallback face, `SubstKind::SymbolFallback` (approximate, reported) |

Recovered renders by rule (10 new; `Warning::RecoveryApplied`, strict
mode refuses each): page-box defaults 4, operator inside a path object
3, stray `Q` 1, unknown operator 0 (the file then hits an ambiguous
operand count), invalid annotation entry / appearance 2, undefined
`/Properties` 1, missing font resource 1, malformed `/Differences` 1,
image `/SMask` that is not a stream 1 (several files apply more than
one rule).

Classification changes that are not regressions: two files moved from
*unsupported* to *error* because an earlier refusal was removed and a
later fault is reached (an appearance state that is not a stream; form
nesting deeper than `max_xobject_depth`); both still do not render
strictly, and the first renders in recovery mode.

Remaining strict blockers on `expect: pass` (51), by category: damaged
file structure 5 (recovery parse, then the content decides), page
without `/MediaBox` 4 (recovered), operator inside a path object 3
(recovered), encryption 3, Identity-ordering CID fonts 3, `/AP /N`
without `/AS` and several states 2, `max_image_dimension` 2, invalid
page tree 2, single files 27 (nine of them recovered). Performance:
median 168 ms per file, p90 1.2 s, max 43.9 s; render total 148 s over
the corpus (173 s before — the knockout per-object passes cost nothing
measurable on this corpus). The plan to 385 is in
[`plans/render-90.md`](plans/render-90.md).

### Results after oversized-image decoding (run-1793285439)

Same 511 files, `--mode full`, default features, against
run-1792865059. Two image paths changed (`features/rendering.md` §
`Do`): a JPEG stored larger than `max_image_dimension` is decoded with a
reduced inverse DCT when drawn smaller, reported in
`RenderedPage::image_approximations()`; a soft mask finer than its
image is composited at the mask's resolution (capped at the drawn
size), and an oversized plain soft mask is sampled directly. The limits
themselves are unchanged.

| expect: pass (427) | before | after |
|---|---:|---:|
| strict render | 376 (88.1%) | **378 (88.5%)** |
| of which approximate | 67 | 69 |
| recovered render | 18 | 18 |
| text | 390 | 390 |
| rewrite | 350 | 350 |
| full PASS | 323 | **325** |

All files: strict render 408 → 410, full PASS 353 → 355. Both new
strict renders are the two files previously blocked by
`max_image_dimension` (one JPEG, one soft mask); the JPEG is counted
approximate. No file or stage that passed strictly before fails now;
round trip 411 unchanged; Poppler SSIM did not drop on any file; 0 crash
/ 0 timeout; robustness 43/43. Wall time per file: median 178 ms; the
render total rose because the run shared the machine with the gate
suite — the affected files time identically on an idle machine against
the previous build.

### Results after the Standard security handler (run-1793311936)

Same 511 files, `--mode full`, default features, against run-1793287891
(the v0.2.1 audit run). Encrypted documents are read through the
Standard security handler (R2–R6; `plans/encryption.md`) with the empty
user password — the runner supplies no password. **Observable behaviour
change:** files refused with `Error::Encrypted` before now open when the
empty user password authenticates; the writers refuse them
(`Error::Unsupported`), so they cannot reach full PASS or round trip.

| expect: pass (427) | before | after |
|---|---:|---:|
| strict parse | 406 | **407** |
| strict render | 378 (88.5%) | **379 (88.8%)** |
| of which approximate | 69 | 69 |
| recovered render | 18 | 18 |
| text | 390 | **391** |
| rewrite | 350 | 350 |
| full PASS | 325 | 325 |

All files: strict render 410 → 413, rejected 47 → 44. Round trip 411
unchanged; Poppler SSIM dropped on no file; 0 crash / 0 timeout;
robustness 43/43. The six encrypted corpus files, by outcome:

| outcome | files | expect | note |
|---|---:|---|---|
| opens with the empty user password, renders and extracts strictly | 3 | pass, any, reject | SSIM against Poppler 0.92–1.0, in the corpus's normal range; extracted codes are text, not ciphertext |
| `Error::Encrypted` — a non-empty user password is required | 2 | pass, reject | qpdf refuses both without a password too; a correct refusal |
| authenticates, streams do not decode (`Error::Parse`) | 1 | pass | R4 with an RC4 crypt filter shorter than 128 bits: Acrobat zero-pads the key to 16 bytes, an undocumented reading (pdf.js implements it). Not adopted in strict mode; see `plans/encryption.md` |

One `expect: reject` file now renders strictly (identically to Poppler):
it was classed reject only because encryption was unsupported. The
manifest is left as is so that the denominator does not move inside
this change; reclassifying it as `pass` is recommended separately.

Wall time per file: median 191 ms (183 before). The increase sits in
the heaviest files' Poppler-inclusive totals; an A/B of the slowest
files against the previous build renders them in identical time, and
parse time across the corpus moved by 5 ms in total.

### Results after the encryption review fixes (run-1793344091)

Same 511 files, `--mode full`, default features, against
run-1793311936. Changes since: the independent-review fixes (recovery
detects encryption from every scanned object), SASLprep / PDFDocEncoding
passwords, the short-key decision, the empty-stream parser fix and the
built-in-encoding text fix (`plans/encryption.md` § Review, § Short
keys, § Fuzzing).

| expect: pass (427) | before | after |
|---|---:|---:|
| strict parse | 407 | 407 |
| strict render | 379 (88.8%) | 379 (88.8%) |
| of which approximate | 69 | 69 |
| recovered render | 18 | **19** |
| text | 391 | 391 |
| rewrite | 350 | 350 |
| full PASS | 325 | 325 |

One file changed class: the revision-4 file with a 40-bit RC4 key that
needs Acrobat's zero-padding. Before, it authenticated and then failed
on a corrupt Flate stream (`Error::Parse`) — the specified key decrypts
it to garbage. Now strict mode refuses it up front
(`Error::Unsupported`, the data does not decrypt as ISO 32000
specifies) and recovery mode renders it, recorded as
`RecoveryRule::PaddedShortEncryptionKey`; it counts as recovered, not
strict (SSIM against Poppler 0.81, within the corpus's spread). No
other file changed in any stage or warning count. Round trip 411
unchanged; Poppler SSIM dropped on no file; 0 crash / 0 timeout;
robustness 43/43. Wall time per file: median 190 ms.

### Results for the 0.3.0 release candidate (run-1793402300)

Same 511 files, `--mode full`, default features, against
run-1793344091, after the final encryption audit (malformed
crypt-filter entries are errors instead of defaults; `/Perms` shape
before authentication) and the CLI password option. Every stage of
every file, the approximation flag, the warning count and the recovery
notes are identical to the previous run: strict parse 407, strict
render 379 of 427 (88.8 %), of which approximate 69, recovered 19,
text 391, rewrite 350, full PASS 325, round trip 411, Poppler SSIM
dropped on no file (median 0.986 over the 369 strict renders with a
reference), 0 crash / 0 timeout, robustness 43/43. Against 0.2.2 (the
published release): strict render +1, recovered +1, text +1, all from
the encrypted files. Wall time per file: median 176 ms.

### Results after the FontMatrix fix, with the quality gate (run-1793419385)

Same 511 files, `--mode full`, default features, against run-1793406122
(the v0.3.0 release). Engine change: Type 1 glyph outlines go through
the program's own `FontMatrix` (`plans/v0.4.0.md` § 7). Runner changes:
the exact-render quality gate, order-independent text scores,
an incremental-save stage and a content profile
(`plans/quality-gate.md`).

| expect: pass (427) | v0.3.0 | now |
|---|---:|---:|
| strict parse | 407 | 407 |
| strict render | 379 (88.8 %) | 379 (88.8 %) |
| of which approximate | 69 | 69 |
| recovered render | 19 | 19 |
| text | 391 | 391 |
| rewrite | 350 | 350 |
| full PASS | 325 | 325 |

Counts are unchanged — the fix changes *what* is drawn on pages already
counted as exact. Rendered pixels changed on exactly three files, all
three intended (the garbled Type 1 pages): content SSIM 0.07 → 0.88 and
0.04 → 0.71, whole-page SSIM up on both, the third has no Poppler
reference. No other page's SSIM moved by more than 0.0005; no stage,
approximation flag or warning count changed on any file; round trip
411; 0 crash / 0 timeout; robustness 43/43; median wall time per file
177 ms (176).

Quality gate: 1 exact file flagged by the enforced rule (a hairline grid
Poppler draws darker — known, fidelity), 3 report-only, 7 exempt
(5 blank Poppler pages, 2 `/UserUnit` page sizes); on v0.3.0 the same
rules flag 3 (the two garbled pages with a reference, and the grid).

Text, page 1 against `pdftotext` page 1, 425 strict extractions:
character-multiset F1 median 1.00, ≥ 0.9 on 377 (88.7 %); the
order-sensitive score ≥ 0.9 on 328. On the 16 files whose text is
Chinese, Japanese or Korean: F1 median 1.00, order score median 0.93
(vertical writing and column order). Low recall (< 0.5, 46 files) is
mostly text only in annotations / form fields or codes without a
Unicode mapping (`plans/v0.4.0.md` § 4).

Incremental save of a one-entry `/Info` edit on every strictly parsed
file: 446 pass, 3 refused (encrypted — the writers do not encrypt);
none produced a file that failed to reparse or lost the edit.

## External verification findings (this phase)

What the independent parsers said about the rewrites, file by file,
and what was done about it:

| finding | files | origin | action |
|---|---|---|---|
| `qpdf --check` **error** on the rewritten file | `bomb_giant`, `pdfium-many_rectangles_brotli` | **input-derived**: the original fails identically (`/JBIG2Decode` on a page content stream; `/BrotliDecode`, a filter outside ISO 32000). Both are `expect: pass` for the strict parser (the syntax is fine) and refused at render with a typed error | recorded; `external_no_new` passes for both (no *new* diagnostic), `external_valid` fails — the two judgements are reported separately for exactly this case |
| `qpdf --check` **warning** on the rewritten file only (`input stream is complete but output may still be valid` at one image stream) | `images`, `issue12337`, `issue18911`, `issue19239` | **rewrite-introduced**: the stream's `/Length` is *indirect*; the parser trusted only direct lengths and otherwise scanned for `endstream`, backing off the EOL before it — which also removed the stream's genuine last byte when that byte was `\r`. The rewritten file therefore carried a Flate stream one byte short; the same bug made page 11 of the three `issue*` files fail to render | **fixed** — indirect `/Length` values are resolved (re-entrancy-guarded) and trusted when `endstream` follows; `tests/parse_stream_length.rs`. The four files are clean for qpdf afterwards |
| Poppler renders the rewritten file but not the original | `mut-truncate-tail-chrome-article-columns-1`, `mut-truncate-tail-scan-issue10301` | `expect: reject` mutations with the tail cut off: Poppler finds no page in the original (3 syntax errors) while our recovery parse plus rewrite produces a readable file. Reported as a Poppler divergence (page-count mismatch) because the comparison cannot tell a repair from a change | recorded; both stay outside the compatibility rate (`reject`) |
| Flate streams ending in a sync flush (`00 00 ff ff`) with no final block | `comments`, `highlights` and the three `issue*` files above (Apple annotation appearance streams) | **input-derived**: qpdf warns on the originals too; the decoded output is complete but the stream is not terminated. Our decoder refuses an unterminated stream (`FlateDecode failed: incomplete deflate stream`), other viewers use what was decoded | left strict in this phase: accepting it silently would be error suppression, and the engine has no per-render diagnostics channel yet to report it; proposed for the next phase as "accept + report" |
| rendering every page exhausted `max_total_decoded_bytes` | `arxiv-2610.24216`, `chrome-wiki-ko-hangul`, `chrome-wiki-zh-hanzi` (pages 4–13) | an engine limitation surfaced by the all-page run: the budget was per `Document` lifetime, so page after page of decoded images drained it | **fixed** — the budget is per operation (render / extract / rewrite) plus the retained ObjStm cache; `tests/limits_decoded_budget.rs` |

## Round-trip verification (rewrite → reparse → compare)

A rewrite that *succeeds* says nothing about whether the document
survived it. `compat/runner/src/roundtrip.rs` runs, for every file
whose strict parse succeeded:

```text
original ──strict parse──▶ Document A
                              │ rewrite()            (refusal → UNSUPPORTED, error → FAIL)
                              ▼
                           bytes B ──strict parse──▶ Document B   (reparse PASS/FAIL)
                              │
          ┌───────────────────┼─────────────────────┬────────────────────┐
          ▼                   ▼                     ▼                    ▼
   object graph          render A vs B         text A vs B      annotations/forms/dests
   (/Root, /Info)       kasane, every page    chars, Unicode,    per-annotation summary,
   renumbering-          (budgeted), 48 dpi:   origin/advance,    field tree (name, FT, V,
   independent walk      size, MAE, SSIM,      code/ActualText    Ff, DA, Opt), named
   with an old→new       diff-px per page;     resolution         destinations → page
   reference map         Poppler on every page
                         of both as witness
          └───────────────────┴─────────────────────┴────────────────────┘
                              ▼
         qpdf --check on A *and* B (warnings and errors counted separately);
         Poppler `Syntax Error`/`Syntax Warning` lines counted for A and B
                              ▼
   full = rewrite PASS ∧ every attempted comparison PASS ∧ no page skipped
          ∧ no new external diagnostic ∧ no Poppler-divergent page
```

Eight judgements are recorded and tallied separately, so "matched
inside kasane" is never confused with "valid for an independent
implementation":

| # | judgement | field | PASS means |
|---|---|---|---|
| 1 | rewrite execution | `rewrite` | `rewrite()` returned bytes (UNSUPPORTED = a documented refusal such as a signature) |
| 2 | reparse | `reparse` | the strict parser reads B |
| 3 | structural equivalence | `structure` | object-graph walk finds no difference |
| 4 | render equivalence | `render` + `pages` | every compared page agrees (SSIM ≥ 0.995, diff-px ≤ 0.05 %, same size); `pages` records total / compared / skipped (budget) / unsupported (our render of A fails) / failed pages, worst and mean SSIM, first divergent page |
| 5 | text equivalence | `text` | characters, Unicode and positions agree |
| 6 | annotation / form / destination equivalence | `annotations` | derived summaries agree |
| 7 | external parser validity | `external_valid` | `qpdf --check` on B exits 0 with **no warning** |
| 8 | no newly introduced external error | `external_no_new` | B has no more qpdf warnings/errors than A and no more Poppler syntax diagnostics than A (inherited diagnostics are listed in the notes, never hidden) |

The Poppler witness (`poppler`) renders every compared page of A and
B with `pdftoppm` and records pages compared, size mismatches, mean /
worst SSIM, mean MAE, worst diff-px ratio and the first divergent
page (SSIM < 0.99 or diff-px > 0.1 %). A kasane-vs-kasane match with
a Poppler divergence is a file to investigate, and it fails `full`.

Outcomes per check are PASS / FAIL / UNSUPPORTED (the engine refused
the rewrite — a profile limit such as a signature, not damage) / N/A
(nothing of that kind in the file: no annotations, or our renderer
or extractor fails on the *original*, so there is nothing to compare)
/ not attempted. Rates are PASS over *attempted*; N/A never counts as
a pass, and a file without forms is never credited with "forms
preserved". Every `FileResult` records the per-check notes (first
differences with their object path), per-page render metrics, the
qpdf diagnostics, the size ratio and the time; the report tallies
per group — all strictly parsed files, by `expect`, by category and
by document feature (multipage, acroform, annots, dests, outlines,
signed, incremental, embedded/non-embedded fonts, images,
transparency, …).

What the comparisons can and cannot show:

* the object-graph walk is the strongest check: it compares every
  key, value, stream byte and reference *target* reachable from the
  catalog and `/Info`, tolerating only renumbering and `/Length`
  recomputation. A rewrite that drops a key, inlines something,
  re-encodes a stream or points a reference elsewhere is reported
  with the path (`/Root/Pages/Kids[0]/Annots[2]/AP/N: stream bytes
  differ`);
* kasane-vs-kasane rendering detects damage *added* by the rewrite
  even where our rendering of the original is itself wrong — but a
  match only proves the rewrite changed nothing this renderer shows.
  Two renderers agreeing on both files does not prove either is
  right; the Poppler witness is the independent second opinion on
  page 1;
* text and annotation comparisons are derived views of the same
  graph; they exist so a failure is attributed to what a user would
  notice (characters moved, a field value lost) rather than to a path.

### Signed, incrementally updated and encrypted files (safety)

* **Signed** (`/ByteRange` in a `/Sig` value): a signature covers a
  byte range of the original revision. Any rewrite — new object
  numbers, offsets, a single xref — breaks it, so the rewriter
  refuses before writing (`/Perms /DocMDP`/`/UR`/`/UR3`, and any
  `/FT /Sig` field with a `/V` dictionary under `/Fields` → `/Kids`
  or as a page widget). Safe handling = read-only: render, extract,
  audit — plus, since this phase, `Edit::save_incremental()`, which
  appends a revision after the signed bytes and is allowed only for
  the changes the signer permitted (`/DocMDP /P 2` form filling,
  `/P 3` and approval signatures also annotations; `/P 1`, a
  `/FieldMDP` lock, a `/ByteRange` that does not cover the file
  intact, or any metadata change → refused). No cryptographic
  verification is performed and no signature is ever reported valid;
  see `docs/features/editing-incremental.md`.
* **Incremental updates** (`/Prev` chains, `/XRefStm`): the parser
  merges them newest-first; the rewrite emits one flat revision with
  the *current* objects. The history (superseded objects, earlier
  trailers) is intentionally not carried — a round trip therefore
  compares the current revision only, and the `incremental` feature
  row shows how many such files pass.
* **Encrypted** (`/Encrypt` in the trailer): refused at
  `Document::from_bytes` (`Error::Encrypted`), so no stage — rewrite
  included — can emit plaintext from a file it could not decrypt.
  Decryption is not implemented; when it is, the rewriter must
  re-encrypt or refuse, never write the decrypted objects as-is.

## Non-embedded CJK CID fonts: options (decision: implemented behind `cjk-fallback`)

*Outcome:* the second row below was implemented in this phase —
`src/cjk.rs`, feature `cjk-fallback`, assets fetched by
`scripts/fetch-cjk-assets.sh` (Adobe-Japan1 CMaps + `Adobe-Japan1-UCS2`
from Adobe's BSD-3 resources, Noto Sans JP Regular under the OFL),
`usecmap` chains in `src/cmap.rs`, `SubstKind::CjkFallback` in
`font_substitutions`. The default build is unchanged (refusal). The
analysis that led there is kept for the record:

Three `expect: pass` files (`issue11578_reduced`, `issue11740_reduced`,
`issue12418_reduced`) and two more with non-embedded TrueType fonts
render-fail on "non-embedded CIDFont: /FontFile2 or /FontFile3 is
required". Drawing them needs (a) a CJK glyph source, (b) the
predefined CMaps the fonts name (`90ms-RKSJ-H`, `UniGB-UCS2-H`, …) to
turn codes into CIDs, and (c) the CID → Unicode tables
(`Adobe-Japan1-UCS2` etc.) to pick glyphs from a font that is not
CID-keyed the same way. The options:

| option | licence | size | WASM / mobile | fidelity | verdict |
|---|---|---|---|---|---|
| bundle a CJK font (Noto Sans CJK / Source Han Sans, OFL 1.1; or the smaller Noto Sans CJK subsets) + Adobe CMap resources (BSD-3) | compatible | 15–20 MB per weight for pan-CJK; CMaps ~2 MB uncompressed | yes, but every consumer pays the size | glyph widths come from the PDF `/W`; shapes differ from the intended font (Mincho vs Gothic) | too heavy for a library crate's default build |
| optional feature `cjk-fallback` bundling one subset font (Japanese-only Gothic, ~4 MB) + the Adobe-Japan1 CMaps | compatible | ~5 MB behind a feature flag | yes when enabled | Japanese only | **next phase**: implement behind a feature, Japanese first, `font_substitutions` reporting the substitute and the ordering; Chinese/Korean later |
| caller-supplied font path (`RenderOptions::cjk_font`) | caller's | none | n/a | depends on the caller | possible as an addition, but an *implicit* host-font dependency is ruled out |
| refuse (status quo) | — | — | — | — | keeps the 5 files blocked; text extraction of these files already works |

The CMaps are needed regardless of the glyph source and are the
larger engineering item (the parser exists — `src/cmap.rs` reads
embedded CMap streams — the predefined ones only need to be shipped
and `usecmap` chains resolved). Estimated 3 person-days for the
Japanese feature flag including the width/position checks against
Poppler; the decision is deferred to the next phase so this one could
concentrate on the round-trip basis.

## JBIG2: implementation options (decision, implemented in run-1792801802)

JBIG2 (ITU-T T.88) is the single largest render blocker on the 511
files (45 first-error hits, all `expect: pass`, 48 files carry the
filter in total — the pdf.js `bitmap-*` suite plus real scans). It is
also the filter with the worst security history in PDF readers
(generic-region arithmetic decoding, symbol dictionaries and
refinement all index into caller-sized buffers). The options:

| option | licence | unsafe / FFI | WASM & mobile | coverage | effort | verdict |
|---|---|---|---|---|---|---|
| **`hayro-jbig2` 0.3** (pure Rust, `#![forbid(unsafe_code)]`, `no_std` + alloc, Apache-2.0 OR MIT, MSRV 1.92, 4.5 M downloads, actively maintained by the `hayro` PDF renderer) | compatible with ours | none (optional `simd` feature uses `fearless_simd`; disable it) | yes — no system deps | generic, refinement, symbol/text, halftone, pattern regions; MMR via `hayro-ccitt` (which we also need for CCITTFax — 4 blockers) | **1 day** to integrate: `/JBIG2Globals` concatenation, PDF-embedded stream organisation, 1-bit output → our `DeviceGray` 1 bpc image path, `max_image_*` limits; 1 day of corpus/fuzz testing | **adopt** |
| `jbig2dec` / `nipdf-jbig2dec` (C library via FFI) | GPL-3.0 / AGPL-3.0 — incompatible with a permissive publication | C code, FFI, memory-safety record of the C decoder is poor | needs a C toolchain per target; no clean WASM | complete | 2 days | reject (licence, safety) |
| own implementation | ours | none | yes | would start with generic regions only (covers scans, not the symbol-coded text pages that dominate real scanners) | 5–7 days for generic + symbol + refinement + halftone, plus the MQ arithmetic decoder; the security surface is exactly where readers have had CVEs | reject for now — no advantage over a maintained safe crate |
| no JBIG2 (status quo) | — | — | — | 0 | 0 | 45 files stay blocked; every bilevel scan from a modern scanner fails |

Decision (since carried out — `src/bilevel.rs`): integrate
`hayro-jbig2` 0.3.1 (and `hayro-ccitt` 0.4.0 for CCITT G3/G4
through the same crate family) with the `simd` and `image` features
off, behind the existing image pipeline. The MSRV jump (1.70 → 1.92)
is the only cost worth flagging; the crate's dependency tree is two
crates deep and contains no `unsafe`. Expected effect: JBIG2 45 +
CCITT 4 render blockers removed, text/rewrite unaffected (they do not
decode images). Estimated 2 person-days including corpus
verification against Poppler's `jbig2dec` output.

## Remaining render features: investigation notes

Investigated in this phase alongside the shading work; what was found
and why each stays where it is:

| feature | corpus impact (run-1792806451) | finding | decision |
|---|---|---|---|
| knockout transparency groups (`/K true`, §11.6.6) | 1 file | needs per-object backdrop replacement inside the group: every object composites against the group's *initial* backdrop, not against what earlier objects painted. tiny-skia composites against the current pixmap only, so a correct implementation renders each object into its own layer and merges with the group's initial backdrop (cost: one page-sized layer per object, bounded by `max_render_buffer_bytes`). Approximating with a normal group would silently change overlapping semi-transparent strokes | still `Error::Unsupported`; ≈ 2 days |
| text clipping modes `Tr 4`–`7` (§9.3.6) | 1 file | implemented: outlines accumulated in device space, intersected at `ET`, released by `Q`; Type 3 in a clipping mode refused | **done** (`tests/render_text_clip.rs`) |
| shading pattern `/ExtGState` (§8.7.3.3, Table 76) | 0 files blocked by it alone | the override applies the dictionary's `ca`/`CA`/`BM`/`SMask` while the pattern paints. Feasible through the existing `apply_extgstate` under a save/restore around the shading paint; not done because no corpus file needs it and a soft mask inside a pattern interacts with the pattern-space clip in a way that needs its own tests | still `Error::Unsupported`; ≈ 0.5 day |
| `/UserUnit` vs external renderers | 0 files | the engine draws a `UserUnit` page at `UserUnit × dpi / 72` pixels per unit (§14.11.5); Poppler/pdftoppm ignore `/UserUnit`, so a fidelity comparison on such a page sees a size mismatch. The runner resamples to the smaller raster before comparing and records the size mismatch; the engine's behaviour follows the specification | no change; documented in the fidelity section |
| `/ExtGState /Font [ref size]` (§8.4.5) | 1 `expect: pass` file (+1 `any`) | the font object is referenced directly instead of through `/Resources /Font`; implemented by splitting the loader into a dictionary-level entry point (`font::load_font_dict`) | **done** (`tests/render_text_clip.rs::extgstate_font_selects_the_font_like_tf`) |
| function-based and mesh shadings (types 1, 4–7) | 3 files | implemented — see `docs/features/rendering.md` (`sh` row) for the sampling/subdivision trade-offs and bounds | **done** |

## Continuous integration

Two workflows under `.github/workflows/`:

| workflow | trigger | what runs | needs |
|---|---|---|---|
| `pr.yml` | pull requests, pushes to `main` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (engine and `compat/runner`); then the runner in `--mode smoke --with-poppler` over `tests/fixtures` (66 committed, licence-clean fixtures) and `scripts/ci-regression.py check` against `compat/baselines/fixtures-smoke.json` | qpdf + poppler-utils from apt; **no credentials, no corpus** |
| `nightly.yml` | daily cron, manual dispatch | `scripts/fetch-cjk-assets.sh`, runner built with `cjk-fallback`, corpus fetched from the private store, `--mode full --with-poppler` over the 511 files, regression check against `compat/baselines/corpus-full.json`; a second job runs clippy + tests with the `cjk-fallback` feature | repository secrets `KASANE_R2_*` and the variable `KASANE_CORPUS_ENABLED=true`; the job is skipped when they are absent (forks, public mirrors) |

Rules the workflows follow: the store credentials are only read by
the nightly job and never by pull-request runs (secrets are not
exposed to PRs from forks in any case); the corpus is fetched into
the runner's scratch space and never uploaded; artifacts are the
markdown and json reports only — no PDFs, no rendered or diff images.
`scripts/ci-regression.py summarize` reduces a run report to
aggregates plus per-file verdicts, stage classes, round-trip classes
and cause ids, which is the only form of result committed under
`compat/baselines/`. `check` fails the gate when a file present in
both got worse (a stage that was `pass`, a round-trip check that was
PASS and is now FAIL, any crash or timeout); improvements and
newly added files are reported, not failed.

## Pre-publication audit (repository and history)

Re-run before the Apache-2.0 release preparation over the tracked tree
and all 118 commits (patterns: AWS/R2 key IDs, `SECRET`/`token`/
`password`, private-key headers, the R2 endpoint host, 32-hex
Cloudflare account IDs, bucket-name assignments, local user paths,
employer domains, personal e-mail). Findings, classified:

| item | kind | where | action before going public |
|---|---|---|---|
| R2 account ID (32-hex) inside an endpoint URL, and the bucket name | **identifiers**, not secrets — they grant no access by themselves, but pin the repository to one Cloudflare account and name a private bucket | **2 historical commits** of 2026-10-22 (`compat/fetch-corpus.py`, `docs/compat-corpus.md`); **absent from HEAD** — the example config uses `<ACCOUNT_ID>` and `your-corpus-bucket`, code reads `KASANE_CORPUS_ENDPOINT` / `KASANE_CORPUS_BUCKET` | decision for the maintainer: accept (identifier only) or rewrite history once, right before the first public push (`git filter-repo --replace-text` on a fresh clone, then re-tag and force-push *once*, never afterwards). **Not done** in this preparation — the values are not reproduced in any report |
| `*.r2.cloudflarestorage.com` host | identifier (vendor) | docs, example config, history | fine to publish |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`, `KASANE_R2_*` | **variable / secret names only**; no value has ever been committed (0 hits for key-ID, secret or token patterns in any commit) | docs, example env file, `.github/workflows/nightly.yml` | nothing to rotate; keep `.env` / `compat/corpus.env` ignored |
| local paths (`/Users/…`), employer domain, personal e-mail | — | 0 hits in tree or history (commit author is the GitHub noreply address) | nothing |
| generated PDFs, rendered images, diff images, logs | — | none tracked; `compat/corpus/`, `compat/reports/`, `assets/cjk/`, `target*/`, `.venv-corpus/` are ignored; the only committed results are the aggregate baselines under `compat/baselines/` | nothing |
| `tests/fixtures/eexec_cmsy10.bin`, `type1_cff.bin` | font subsets extracted from corpus PDFs | tracked | **resolved**: both programs carry their own copyright and licence statements (AMS 1997/2009 + OFL 1.1 with Reserved Font Name CMBSY10; Adobe 2010/2012 Source Sans Pro, OFL with Reserved Font Name 'Source'); the subsets are renamed inside the files so the reserved names are not used, and the notices are in `NOTICE` and `tests/fixtures/README.md` |

So: **no secret has ever been committed and no rotation is needed.**
The one pre-publication decision is whether to scrub the account ID
and bucket name from two historical commits.

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

### Accepted with warnings

A `PASS` whose document recorded a `Warning` (`Document::take_warnings`
after the render and text stages; `metrics.warnings` in the JSON,
deduplicated, at most 64 per file) is a pass — the output is exact —
but the input was not conforming. The report keeps the two apart:
the headline states how many files were *accepted with warnings* and
how many of the full passes are *clean*, and a **Warnings** section
lists each warned file with its findings. `scripts/ci-regression.py
check` prints a `note:` (not a regression) when a file that passed
cleanly in the baseline now passes with a warning, so the shift is
visible in CI without blocking it. The engine records a warning only
for the enumerated deviations it can process exactly (see
[`security.md`](security.md) § Three outcomes); the runner never
turns an `ERROR` or `UNSUPPORTED` into a warned pass.

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
