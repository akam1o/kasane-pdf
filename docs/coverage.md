# kasane-pdf support matrix

Milestone: **M8**. Everything not listed as supported below is
unsupported and reported as a typed error — never silently skipped or
returned as a partial result.

Legend: ✅ supported · ❌ unsupported (explicit error) · ⚠️ partial /
restricted

## File structure

| Feature | Status | Behaviour / error |
|---|---|---|
| `%PDF-` header within first 1024 bytes | ✅ | `Error::NotPdf` otherwise |
| `startxref` near end of file | ✅ | `Error::Parse` if missing/invalid |
| Classic `xref` tables, incl. subsections | ✅ | `Error::Parse` on malformed lines |
| `trailer` dictionary | ✅ | merged across `/Prev` chain |
| `/Prev` chains (incremental updates in classic format) | ✅ | newest section wins; cycles → `Error::CycleDetected` |
| xref streams (`/Type /XRef`) | ✅ | `/W` (0–8 bytes/field), `/Index` (default `[0 /Size]`), types 0/1/2; bad type → `Error::Parse`, bad width → `Error::Unsupported` |
| Hybrid-reference files (`/XRefStm`) | ✅ | within one update section the stream *replaces* classic entries (§7.5.8.4); across `/Prev` sections the newer section wins |
| Object streams (`/Type /ObjStm`) | ✅ | `/N`/`/First` validated; index↔number consistency checked; decoded bodies cached per Document |
| Encrypted files (`/Encrypt`) | ❌ | `Error::Encrypted` |
| Linearized ("fast web view") files | ⚠️ | parsed like normal files; the linearization dict is ignored, not validated |

## Object layer (ISO 32000-1 §7.3)

| Type | Status | Notes |
|---|---|---|
| null, boolean | ✅ | |
| integer, real | ✅ | kept distinct; `as_number()` accepts either |
| names, `#xx` escapes | ✅ | malformed escapes are `Error::Parse` |
| literal strings `(…)` | ✅ | escapes + balanced nesting; unbalanced → `UnexpectedEof` |
| hex strings `<…>` | ✅ | odd nibble count handled per spec |
| arrays, dictionaries | ✅ | `null` dict values treated as absent (§7.3.7) |
| indirect references `n g R` | ✅ | chained refs followed; loops → `Error::CycleDetected` |
| `stream … endstream` | ✅ | raw bytes captured; extent via consistent direct `/Length`, else `endstream` scan |
| `obj … endobj` | ✅ | missing `endobj` tolerated (offset-driven parse) |

## Stream decoding (ISO 32000-1 §7.4)

| Feature | Status | Notes |
|---|---|---|
| No filter | ✅ | raw bytes returned |
| `/FlateDecode` (`/Fl`) | ✅ | zlib/DEFLATE via `flate2`/`miniz_oxide` |
| `/Filter` as name or array | ✅ | array order applied sequentially |
| `/DecodeParms` dict/array/null | ✅ | positional match with `/Filter` |
| `/Length` direct or indirect | ✅ | indirect `/Length` resolved at decode time |
| PNG predictors 10–15 | ✅ | `/Colors` 1–4, `/BitsPerComponent` 8 only; 10–14 fix the row filter, 15 = per-row optimum; other values → `Error::Unsupported` |
| TIFF predictor 2 | ✅ | `/BitsPerComponent` 8 only; other values → `Error::Unsupported` |
| `/Predictor` 1 | ✅ | no-op |
| `/Predictor` 3–9, 16+ | ❌ | `Error::Unsupported` (reserved/undefined in spec) |
| Other filters (`/LZWDecode`, `/DCTDecode`, `/ASCIIHexDecode`, `/ASCII85Decode`, `/RunLengthDecode`, `/CCITTFaxDecode`, `/JBIG2Decode`, `/JPXDecode`, `/Crypt`, unknown) | ❌ | `Error::Unsupported` when the stream is requested; document still loads |

## Document layer

| Feature | Status | Notes |
|---|---|---|
| trailer `/Root` → catalog | ✅ | missing → `Error::Structure`; `/Type` checked if present |
| page tree walk | ✅ | depth ≤ `max_page_tree_depth`, nodes ≤ `max_page_tree_nodes`; revisited nodes → `Error::CycleDetected` |
| inherited `MediaBox`, `CropBox`, `Rotate`, `Resources` | ✅ | per Table 30; `CropBox` defaults to `MediaBox`, `Rotate` defaults to 0 |
| root `/Count` consistency | ✅ | mismatch → `Error::Structure` |
| `/Contents` absent | ✅ | `page_contents()` returns `[]` |
| `/Contents` single stream | ✅ | decoded via `Document::page_contents()` |
| `/Contents` array of streams | ✅ | order preserved; each decoded |
| `/Contents` wrong type / missing ref | ✅ | `Error::Structure` / `Error::MissingReference` at call time |
| `/Resources` | ✅ | inherited per Table 30; `/Font` entries resolved via `load_font` — `/TrueType` simple fonts and `/Type0` composite fonts (CIDFontType2 + Identity-H + `/FontFile2` only; other subtypes/encodings or missing program → `Error::Unsupported`) (nearest ancestor's dict used *whole* — no per-key merge); named entries in `Font`, `XObject`, `ExtGState`, `ColorSpace`, `Pattern`, `Shading`, `Properties`, … resolvable via `Document::page_resource()`; wrong type → `Error::Structure`, missing → `Ok(None)`; targets not interpreted |
| annotations, outlines, names, metadata, forms | ❌ | not read; keys are preserved in the parsed dictionaries only |

## Content streams (ISO 32000-1 §7.8)

`Document::page_operations()` parses the decoded `/Contents` into a
`Vec<Operation>` of typed instructions. A `/Contents` **array** is
parsed as one logical stream — operand stack and structural state are
not reset between elements (§7.8.1). Only *direct* objects may appear
as operands: `n g R` references, `obj` bodies and `stream` data are
rejected (e.g. `10 0 R` → unknown operator `R`). Any error discards
all parsed operations — no partial results.

| Feature | Status | Notes |
|---|---|---|
| operand collection (numbers, names, strings, arrays, dicts, bools, null) | ✅ | string bytes kept raw (no UTF-8 assumption); arrays/dicts nest with `max_nesting_depth` |
| `q` `Q` `cm` `w` `J` `j` `M` `d` `ri` `i` `gs` | ✅ | `q`/`Q` depth bounded by `max_gsave_depth`; `Q` underflow / unclosed `q` → `Error::Structure` |
| `m` `l` `c` `v` `y` `h` `re` | ✅ | operands taken in postfix order |
| `S` `s` `f` `F` `f*` `B` `B*` `b` `b*` `n` `W` `W*` | ✅ | |
| `G` `g` `RG` `rg` `K` `k` `CS` `cs` | ✅ | `CS`/`cs` keep the colour-space *name* |
| `SC` `sc` | ✅ | any number of numeric components; arity checked at colour-space resolution (M3: DeviceN parsing supported, rendering `Unsupported`) |
| `SCN` `scn` | ✅ | numeric components + optional trailing pattern name; patterns → `Error::Unsupported` at render |
| `BT` `ET` | ✅ | nesting and balance checked |
| `Tc` `Tw` `Tz` `TL` `Tf` `Tr` `Ts` `Td` `TD` `Tm` `T*` | ✅ | `Tf` keeps the font resource *name* |
| `Tj` `TJ` `'` `"` | ✅ | `TJ` items: `TJItem::Text` (raw bytes) / `TJItem::Kern` (number); non-string/number items → `Error::Parse` |
| `Do` | ✅ | resolved via the *current* `/XObject` resource dict; `/Form` → painted via §8.10 rules (own `/Matrix`, `/BBox` clip, own `/Resources`); `/Image` → §8.9 sampled image (8-bit DeviceGray/RGB, raw/Flate/DCT); other subtypes → `Error::Unsupported` |
| `sh` | ✅ | resource name kept; shading **not** executed |
| `MP` `DP` `BMC` `BDC` `EMC` | ✅ | `DP`/`BDC` property operand must be a dict or name; balance checked |
| `BX` `EX` | ✅ | inside, unknown operators are recorded as `Operation::Unknown` instead of failing (§7.8.2) |
| `BI` `ID` `EI` (inline images) | ❌ | `Error::Unsupported` — the binary data between `ID` and `EI` cannot be tokenized without guessing |
| Unknown operator outside `BX`/`EX` | ❌ | `Error::Parse` |
| Wrong operand count / type, leftover operands at EOF | ❌ | `Error::Parse` |
| Rendering / executing operations | ❌ | out of M2 scope — operations are data, not executed |

## Rendering (ISO 32000-1 §8, §9)

`render_page` executes supported operations into a `Pixmap`
(`tiny_skia`) over a white background; `pixmap_to_png` encodes it.
Only pages whose operations all fall inside this subset render —
anything else fails with `Error::Unsupported` and produces no output.

| Feature | Status | Notes |
|---|---|---|
| `q`/`Q`/`cm` | ✅ | *special graphics state* ops (§8.2 Table 51), allowed at page description level only — inside a path object they are `Error::Structure` (§8.2 Figure 9). Paths are built in user space; `CTM × page→pixmap` is applied at paint time |
| `m` `l` `c` `v` `y` `h` `re` | ✅ | accumulate in the current (user-space) path; `m`/`re` begin a path object |
| `S` `s` `f` `F` `f*` `B` `B*` `b` `b*` `n` | ✅ | nonzero-winding and even-odd rules; open subpaths are implicitly closed for filling |
| `W` `W*` | ✅ | mark the current path as the clip operand (must follow the *last* construction op — §8.5.4); the clip installs when the path is *consumed*: a paint op draws under the **pre-W** clip first, then the clip updates; `W`+`n` installs without painting; `W` on an empty path empties the clip; `q`/`Q` save/restore |

**Placement enforcement** (§8.2 Figure 9, §8.5.4): inside a path
object only path-construction ops, `W`/`W*`, and the terminating
painting/`n` op are allowed. The renderer rejects out-of-order
operators with `Error::Structure` — e.g. `cm`, `q`/`Q`, colour ops,
`m`/`l`/`re` after `W`/`W*`, a second `W`/`W*` in the same path, or
`l`/`c`/`v`/`y` without a current point. The parse-only
`page_operations()` API still returns these sequences; placement is
a *render-time* restriction, not a parsing one.
| `w` `J` `j` `M` `d` `i` | ✅ | line width 0 → 1-device-pixel hairline (§8.4.3.2) |
| `G` `g` `RG` `rg` `K` `k` | ✅ | DeviceGray/RGB/CMYK |
| `CS` `cs` + `SC` `sc` | ✅ | `DeviceGray`/`DeviceRGB`/`DeviceCMYK` and `DefaultGray`/`DefaultRGB`/`DefaultCMYK` |
| `SCN` `scn` pattern name | ❌ | `Error::Unsupported` — patterns not rendered |
| `CS`/`cs` non-device space | ❌ | `Error::Unsupported` — no ICCBased/Separation/DeviceN/Pattern |
| text ops `Tf` `Tj` `TJ` `'` `"` | ✅ | embedded `/TrueType` + `/FontFile2` + `WinAnsiEncoding` (M5) *or* `/Type0` + Identity-H + CIDFontType2 + `/FontFile2` (M7); `BT`/`ET` init `Tm`/`Tlm`; `Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts` honoured — `Tw` applies to single-byte code 32 only (never to 2-byte Identity-H codes, §9.3.3); modes 0–3 painted (4–7 → `Unsupported`); PDF widths (`/Widths`, `/DW`/`/W`) used for advance — font-internal `hmtx` never consulted for positioning |
| text-state ops `Tc` `Tw` `Tz` `TL` `Tr` `Ts` `Td` `TD` `Tm` `T*` | ✅ | state persists across text objects; `Tm`/`Tlm` reinitialized by `BT` |
| `extract_text` (§9.10) | ✅ | one `CharInfo` per character code (1- or 2-byte per font), content order incl. Form XObjects; `/ToUnicode` CMap (1- or 2-byte codespace, `bfchar`, `bfrange` sequential + array, UTF-16BE/surrogates/multi-scalar) → WinAnsi glyph-name→AGL→Unicode fallback for simple fonts / `Unresolved` for Identity-H composites (no guessing); `/Span` `/ActualText` and `/OC` membership → `Error::Unsupported`; headless — no rasterizing, clip masks, or image decoding; user-space coords (`/Rotate` not applied); budgets: `max_text_codes`, `max_extracted_unicode_bytes` |
| `Do` | ✅ | Form XObject (§8.10): gs saved, `/Matrix` concatenated, clipped to `/BBox`, form content runs under its own `/Resources`, gs restored; nested forms and recursion detection bounded by `max_xobject_depth`/`max_xobject_invocations`. Image XObject (§8.9): `/DeviceGray`/`/DeviceRGB` 8-bit only, raw/`FlateDecode`/`DCTDecode`, `/Decode`, `/Interpolate` (nearest-neighbour — spec permits ignoring interpolation). `/ImageMask`, `/Mask`, `/SMask`, `/SMaskInData` ≠ 0, `/OC`, `/Group`, `/Ref`, non-8-bit, non-device colour spaces → `Error::Unsupported` |
| `sh` | ❌ | `Error::Unsupported` — shading not rendered |
| `gs` | ❌ | `Error::Unsupported` |
| `BMC` `BDC` `EMC` `MP` `DP` | ✅ | plain marked content — metadata only (§10.5); `/Span` property lists carrying `/ActualText` (§14.9.4) are refused in render and extraction |
| `/OC` `/OCMD` marked content | ❌ | `Error::Unsupported` — OCG/OCMD visibility evaluation is not implemented; only `/OC` tags whose property list is an OCG/OCMD dict count (spec 8.11.2); `/OCMD` without a membership dict is plain marked content; unresolvable `/Properties` names error |
| `BX` `EX` `Unknown` | ✅ | §7.8.2 compatibility — ignored |
| page annotations (`/Annots`) | ❌ | `Error::Unsupported` — page is refused, not drawn without them |
| `/UserUnit` ≠ 1 | ❌ | `Error::Unsupported` |
| `MediaBox`/`CropBox`/`Rotate` | ✅ | `CropBox` defaults to `MediaBox`; Rotate 0/90/180/270; non-zero origins; `/Rotate` is clockwise on display |
| DPI scaling | ✅ | any positive finite dpi; pixel dims = pts × dpi/72 |

### Test fixtures (font provenance)

| File | Provenance | Licence |
|---|---|---|
| `KasaneTest.ttf` | 5-glyph synthetic font drawn via fontTools by `make_fixtures.py` — no real typeface data | none (generated) |
| `KasaneCIDTest.ttf` | 6-glyph synthetic font (`make_cid_font.py`) — .notdef/box/tri/bar/circle/hole at non-identity GIDs | none (generated) |
| `KasaneJP.ttf` | 17-glyph subset of **Noto Sans JP** (`NotoSansJP-VF.ttf`, notofonts/noto-cjk `Sans2.004`) via `make_jp_font.py`; TrueType `glyf`, UPM=1000, renamed to avoid the OFL Reserved Font Name 'Source' | SIL Open Font License 1.1 — © 2014-2021 Adobe; subsetting/redistribution permitted |

### Colour conversion notes

- `DeviceCMYK` → RGB uses the naive subtractive form
  `1 − min(1, c + k)` (§10.4.3 note). It is **not** colour-managed;
  ICC profile conversion is out of scope.
- Colour values are clamped to [0,1] for output encoding; out-of-range
  input saturates rather than erroring (the spec clamps at the device).

## Matrices (ISO 32000-1 §8.3)

`Matrix` is a `[a b c d e f]` affine transform with `transform`,
`concat` (PDF `cm` order: `existing.concat(cm)` = `cm × existing`),
`translate`/`scale`/`rotate` helpers, and `is_finite` checking. The
page-level `CropBox`/`Rotate` → pixmap mapping is built inside
`render_page` (see `src/render.rs`), not to be confused with the
content-level `cm` operator.

### Singular / non-finite transforms

- **Non-finite `cm` operands** → `Error::Structure` ("cm operands must
  be finite"). PDF number syntax cannot express `1e999`, so such
  input is rejected at the lexer before `cm` runs; the check is a
  defensive guard.
- **Singular CTM** (finite elements, determinant exactly 0 — e.g.
  `0 0 0 0 e f cm`): ISO 32000-1 does not define painting under a
  non-invertible transform. We **refuse the page** with
  `Error::Unsupported` at paint time rather than silently producing
  nothing — a degenerate transform cannot be honoured faithfully.
  Poppler warns "matrix not invertible" and paints a fallback; both
  are unspecified-behaviour choices.
- **Near-singular but invertible CTM** (e.g. det = 1e-9, or
  `1000 0 0 0.001` anisotropic): rendered normally — we only reject
  the exact `det == 0` case, no epsilon cutoff.
- The check runs on the content-level CTM (`cm` accumulation), not
  the fixed page→pixmap transform — page geometry is built
  separately and is always invertible for valid page boxes.

## Rewrite output (M8, ISO 32000-1 §7.5)

`rewrite(&Document)` rebuilds a loaded document as a fresh
single-revision PDF: DFS from trailer `/Root` (plus `/Info`), dense
renumbering 1..N, classic xref + trailer emitted. No original bytes,
`/Prev` history or ObjStm containers are carried over — only the
reachable object graph.

"Reachable" means objects the DFS can reach through `/Ref`s starting
at `/Root`/`/Info`. Keys *inside* a reachable dictionary are kept
verbatim — including keys M8 does not understand. That is distinct
from *unreferenced* objects elsewhere in the file: unreferenced
objects are simply never walked and never emitted (the only
deliberate exception is an indirect stream `/Length`, whose slot is
skipped at collection and recomputed at write time).

| Feature | Status | Notes |
|---|---|---|
| object collection & renumbering | ✅ | dense 1..N; shared refs share the new number; /Parent cycles terminate via the map |
| `/Info` dictionary | ✅ | carried when trailer references it |
| `/ID`, `/Prev`, `/XRefStm` | — | dropped by design (single-revision file) |
| stream bodies | ✅ | encoded bytes verbatim; `/Length` recomputed from emitted bytes; `/Filter`/`/DecodeParms` kept consistent |
| object streams (input) | ✅ | objects inside `/ObjStm` are resolved and emitted as normal indirect objects |
| incremental input | ✅ | xref-chain merge already yields the latest revision |
| link annots | ✅ | `/Subtype /Link` `/Rect`, `/A` `/URI`/`/GoTo`, `/D` dest arrays — refs remapped |
| direct destination arrays | ✅ | `[page /XYZ …]`, `[page /Fit]` — refs remapped |
| **named destinations** | ❌ | `Error::Unsupported` — `/Dests`, `/Names /Dests`, link `/Dest` name/string, `GoTo` `/D` name/string, `OpenAction` `/D` name/string are all refused (name trees are not resolved) |
| encrypted input | ❌ | refused at `Document::from_bytes` |
| `/Perms` `/DocMDP`/`/UR`/`/UR3`, filled `/Sig` fields | ❌ | `Error::Unsupported` — a signature cannot survive a rewrite; blank unsigned fields pass |
| `/AcroForm` `/XFA`, `/OCProperties`, `/RichMedia`, `/AA` | ❌ | `Error::Unsupported` — outside the profile |
| output xref format | — | classic table only; xref-stream/ObjStm output is not implemented |
| stream `/Length` | ✅ | always emitted as a direct integer recomputed from the emitted bytes; an *indirect* `/Length` ref is never walked from the stream dict (would emit an orphan), but the same object is still collected if referenced elsewhere |
| PDF version | ✅ | output = `max(header_version, catalog /Version)`; `1.x` passes through, `2.0`+ is `Error::Unsupported`. **Malformed input versions are refused, not guessed**: a `%PDF-M.m` header whose token is not `1.x`/`2.0` → `Unsupported`; a catalog `/Version` that is present but not a well-formed `M.m` name → `Unsupported` (never silently dropped, never treated as absent) |

**Not a sanitizer.** A full rewrite does *not* remove unreferenced
history in a guaranteed way (only reachable objects are emitted, but
anything reachable — including `/Info`, metadata and embedded data —
is preserved verbatim). Do not present a rewrite as redaction.

**Preserve ≠ guarantee ≠ inspect.** The rewrite preserves every
reachable dictionary key verbatim — including keys whose *feature*
M8 does not implement (colour management, transparency, patterns,
optional content inside content streams, etc.). "Preserved" means the
bytes are carried over; it does **not** mean the feature works, is
validated, or is safe. The refusal list above is exactly the set M8
inspects — a payload hidden in a key not listed is preserved, not
removed. Do not widen this to a sanitizing claim.

### Save guarantees (CLI `rewrite`)

- Output is written to a temp file in the same directory via
  `OpenOptions::create_new(true)` — never follows a symlink, never
  overwrites a leftover tmp. Then `write_all` + `sync_data` +
  `rename` over the target.
- *Atomic replace* (rename) and *content sync* (`sync_data`) are
  implemented; **power-loss durability is not** — the parent
  directory is not fsynced after rename. Input is only read, never
  written.
- Same-file refusal uses `canonicalize` + Unix `(dev, ino)`; non-Unix
  relies on `canonicalize` alone (documented limit).

### Independent structural validation

`scripts/verify_rewrite.sh` runs `qpdf --check` (v12.4.2) on the
rewrite output of every well-formed fixture and requires exit 0 with
zero warnings. Run it explicitly — `cargo test` does not depend on
qpdf. Fixtures are deterministic (`make_fixtures.py`); intentionally
broken inputs (`hybrid.pdf`, `xobject_image_dct.pdf`, `render_annot.pdf`,
`contents_none.pdf`, `flate_*`, `prev_chain`, `unsupported_filter`,
`bad_offset`, `missing_ref`, `cyclic_pages`, `truncated`) are kept for
load-path tests and excluded from the clean-validation set.

## Annotation edit — `/Square` (M9, ISO 32000-1 §12.5.6.9, §8.4, §12.5.2)

`Document::edit()` returns an [`Edit`] session that accumulates
`/Subtype /Square` annotations as **synthetic objects** in a reserved
indirect-number space (`u32::MAX − 1 000 000` upward). The source
bytes are never modified; `Edit::rewrite()` feeds the session's
overlay plus the live document through the normal rewrite path.

| Feature | Status | Notes |
|---|---|---|
| `/Subtype /Square` on a page | ✅ | `add_square(page, rect, rgb, width)` |
| Rect meaning | ✅ | **the annotation's outer bounding box** — the stroke's outer edge, not the path centreline (§12.5.5 + "Border Styles": border drawn *inside* the Rect) |
| Rect space | ✅ | un-rotated PDF user space (+x right, +y up); CropBox/Rotate not applied — same space M6.1 reports |
| Stroke colour | ✅ | `/C` `[r g b]`, each channel validated finite `0..=1` — out-of-range is an error, never clamped |
| Line width | ✅ | `/BS << /S /S /W w >>`; finite and `> 0`, and `< min(rect w,h)` so the stroke can't invert |
| Fill | — | interior is unfilled (`S` stroke only); no `/IC` emitted |
| Appearance stream | ✅ | generated `/AP << /N >>` Form XObject — `BBox [0 0 w h]`, `Matrix [1 0 0 1 llx lly]`, body `q RG w re S Q` stroking the *inner* rect `[w/2 w/2 w-w/2 h-w/2]`; transformed BBox coincides with Rect so the annotation mapping (`AA = Matrix × A`) is a pure translate and the stroke width is never rescaled or clipped |
| Flags | ✅ | `/F 4` (Print) — visible on screen and on paper |
| Page link | ✅ | `/P` set to the page's indirect ref |
| Multiple annots | ✅ | per page and across pages; each page's `/Annots` array is materialised once |
| `/Annots` absent / direct / indirect | ✅ | a fresh array object is always created — a shared `/Annots` is never mutated in place, so annotating one page can't alter another |
| Existing annots / links | ✅ | preserved verbatim in the new array |
| `/UserUnit` ≠ 1 | ❌ | `Error::Unsupported` — the rect is in default user units and a scaled page would misplace it |
| Refusals | ✅ | all M8 rewrite refusals still apply (encryption, signatures, version, named dests, malformed structure, limits) |
| Synthetic object numbers | ✅ | allocated downward from `u32::MAX` skipping any number *reserved* by the input xref at **any generation or entry type** (in-use, free, compressed), any already-allocated synthetic, or any overlay key — a synthetic ref can never alias a real, reserved, or prior synthetic number; exhaustion and `max_edit_objects` are typed errors |
| Input-ref integrity | ✅ | a reference found in an *input* body that lands on a synthetic number is treated as a dangling input ref and resolved via the document only — never silently resolved to a synthetic body; overlay (edit) refs may resolve to synthetics |
| Out-of-range page / non-finite rect / bad colour / bad width | ❌ | `Error::Structure`, no partial state |
| Reversed rect (llx>urx or lly>ury) | ❌ | refused — not silently normalized |

Not in scope: fill/IC, dashes, transparency, other annotation
subtypes, editing or deleting existing annotations, page edits,
flattening, redaction, a general-purpose edit API, or kasane's own
renderer painting the annotation (it still refuses pages with
`/Annots` — that is M6 behaviour, unchanged).

External check: `qpdf --check` clean; `pdftoppm -hide-annotations`
output is byte-identical to the un-annotated input (only the
annotation differs); the change is confined to the Rect bounds.

## Page structure inspection (M10, CLI `inspect`)

`kasane-pdf inspect <file.pdf> <page>` — a read-only diagnostic dump
of a single page's structure. Fixed line order, no rendering, no
editing, no text extraction. The file must contain exactly one page;
`page` selects it (1-based).

| Line | Source | Default when absent |
|---|---|---|
| `page <n>` | 1-based page index | — |
| `mediabox <llx> <lly> <urx> <ury>` | inherited `/MediaBox` | — (required) |
| `cropbox <llx> <lly> <urx> <ury>` | inherited `/CropBox` | `/MediaBox` |
| `rotation <deg>` | inherited `/Rotate` | `0` |
| `annotations <count>` | `/Annots` array length | `0` |
| `resources fonts <count>` + `font <name> <subtype> [/<BaseFont>]` | `/Resources /Font` entries — `resources` opens the section (this is the only line with that prefix) | `0` |
| `xobjects <count>` + `xobject <name> <subtype> [extra]` | `/Resources /XObject` entries — `Form` shows `/BBox=[…]`, `Image` shows `/Width=… /Height=…` | `0` |
| `colorspaces <count>` + `colorspace <name> <csname>` | `/Resources /ColorSpace` entries — the space's name (`/DeviceRGB`), array's first name, or dict `/Subtype`/`/Base` | `0` |
| `patterns <count>` + `pattern <name> <type>` | `/Resources /Pattern` entries — `/PatternType` (1 tiling, 2 shading) | `0` |
| `shadings <count>` + `shading <name> <type>` | `/Resources /Shading` entries — `/ShadingType` (1–7) | `0` |
| `extgstates <count>` + `extgstate <name> <type>` | `/Resources /ExtGState` entries — `/Type` (usually `/ExtGState`) | `0` |
| `procsets` `properties` | direct entry counts (not enumerated) | `0` |
| `tokens <count>` | lexer tokens across `/Contents` (operands + operators) | `0` when `/Contents` absent |
| `operators <count>` | parsed operations | `0` when `/Contents` absent |
| `opstats [<name>:<count> ...]` | per-operator tallies using the *actual* stream token (`SC` vs `SCN`, `sc` vs `scn`, or the real name an `Unknown` op parsed — never a grouped label) | `opstats` (bare) when absent |
| `contents <ref>` `contents [<ref>, <ref>, ...]` `contents absent` | `/Contents` | `contents absent` |
| `userunit <v>` `userunit absent` | `/UserUnit` | `userunit absent` |
| `annot <i> <subtype> rect <llx> <lly> <urx> <ury> [/<k>=<v> ...]` | one line per `/Annots` element, in array order | — |
| `document` `version <major>.<minor>` `info <count>` `key <name> <type>` | document section — `version` reports the effective declared PDF version (the larger of the `%PDF-M.m` header and the catalog `/Version` name; malformed header or malformed `/Version` is a typed error, never a guess); `info`/`key` list the trailer `/Info` dictionary — values are never shown | `info 0` when `/Info` absent |

Each `annot` line dumps the annotation dictionary: `/Subtype` and
`/Rect` first (validated — the element may be an indirect ref *or*
a direct dictionary, both are resolved to the dict; anything else —
a number, string, stream — is a typed error, as is a non-dict
element, a missing/malformed `/Rect`, or a non-name `/Subtype`),
then every remaining key in byte order. Values are formatted
recursively: numbers decimal, names `/name`, strings `(text)` with
non-ASCII/non-printing bytes escaped `\xNN` (`(` `)` `\` too),
arrays `[v0 v1 …]`, dicts `<< /k v … >>`, refs verbatim `n g R`,
streams as `<stream>` (dict not expanded). Refs are shown, not
resolved — an `annot` value never re-fetches another object.

`tokens` counts every lexer token consumed (operands *and*
operators), so `tokens >= operators` always. `opstats` aggregates
by the *actual* operator token as it appears in the stream — `SC`
and `SCN` are counted separately (never the grouped `SC/SCN`
label), `sc`/`scn` likewise, and an `Unknown` op is reported under
its real name, not `unknown`. Order is byte order. A malformed
content stream — a `/Contents` that isn't a ref or ref array, a
stream that fails to decode, or a syntax/balance error the M2
parser rejects — is a typed error, not silently skipped.

Errors are typed (`Error::Structure`/`MissingReference`/`Parse`): a
non-1-page file (the count is reported), a non-array/unresolvable
`/Annots`, a non-dict/unresolvable `/Resources`, a `/Contents` that is
neither a ref nor an array of refs, an undecodable or unbalanced
content stream, a non-numeric `/UserUnit`, or a malformed annotation
element. Nothing is silently coerced — inspect explains the
structure as it is, or fails.

## Pre-redaction diagnosis (M16, `redact_diag`, CLI `preflight-redaction`)

`diagnose_redaction(&doc, page_index, rects, limits) -> Diagnosis`
inspects one page's content streams *as a reader* and reports what a
future redaction would face. **It never edits the file, and a clean
result is not a "safe to redact" verdict** — it only means the walk
finished and saw no obstacle in the examined scope.

### Input contract

- `rects` — 1..=`max_diag_rects` (256) user-space rectangles
  `(llx, lly, urx, ury)`. Every component must be finite and
  `llx < urx`, `lly < ury`; reversed or degenerate rects are
  `Error::Structure`, never silently normalised or clipped.
- `page_index` — 0-based; out of range is `Error::Structure` naming
  the count.
- `/UserUnit` ≠ 1 on the page is a *blocker* (coordinates aren't the
  default unit); a non-numeric `/UserUnit` is `Error::Structure`.

### What the result means

| Field | Contents |
|---|---|
| `targets` | Painted occurrences localised to page space: each shown character (`Text`), each `/Image` `Do` call (`Image`), each `/Form` `Do` invocation (`Form`), each path painted by a fill/stroke op (`Path`). `bounds` is the conservative axis-aligned bbox; `hits` lists which input rects it intersects (touching edges count). |
| `uncertain` | Occurrences that could not be localised — e.g. a non-finite text/image/form transform. Their position is not guessed. |
| `blockers` | Structures that make exact processing impossible: `/OC` marked content, `/ActualText`, `/ExtGState`, `sh`, `/Pattern` colour space, unsupported `/XObject` subtype, `/UserUnit`, dangling/non-dict annotations. The walk continues past most of them so the caller sees the whole picture. |
| `shared` | Objects invoked from ≥2 sites on this page (e.g. two `Do` names for the same form) *or* listed in another page's `/Resources` — editing them would affect every call site. `shared_scope` reports `pages_scanned`/`pages_total`/`truncated`; a truncated scan is *not* a "not shared" guarantee. |
| `annotations` | `/Annots` entries with `/Subtype`, `/Rect`, `hits` against the input rects, and extra payloads (`/Contents` `/A` `/AP` `/AA` `/Popup` `/Dest`). The `/Rect` is the annotation's frame, not the extent of all data it carries. |
| `unexamined` | Fixed labels for document areas never evaluated: `/Info`/XMP, attachments, form values, structure tree/alt text, JavaScript/actions, font-embedded data, duplicated content on other pages, annotation appearances, link-target text. "Not examined" ≠ "absent". |
| `incomplete` | `Some` when the walk stopped mid-stream — e.g. an unsupported font subtype, a non-finite CTM, an out-of-place path op. `targets`/`blockers` are then *partial*: "the stream could not be fully scanned", never "nothing was found". |

`Diagnosis` deliberately carries no boolean "safe" flag. A clean
report means "walk completed, no blockers in the examined scope",
and nothing more.

### Per-kind bounds semantics (ISO 32000-1 §8, §9)

- **Text** — origin from the same `Trm × CTM` math the renderer
  uses; bounds are the glyph *outline* bbox under that transform
  (`TextBounds::Outline` — ink extent only) or, for
  space/`.notdef`/missing outlines, an estimated character *cell*
  (`TextBounds::Cell` — an estimate, never ink extent). Stroke-mode
  text (`Tr` 1, 2, 5, 6) inflates the bounds by the same stroke
  reach a path uses — a text stroke extends outside the ink bbox
  exactly like a path stroke. Clip-mode text (`Tr` 4–7) is a blocker
  plus `uncertain` — its extent is not estimated. The raw character
  code is reported verbatim even when Unicode is unresolved;
  multi-scalar mappings are not split. A `glyph_id`/`outline`/
  `units_per_em` *error* reports `uncertain` — never a silent cell
  estimate; `LimitExceeded` propagates. Stroke-mode text at `w == 0`
  is a device-pixel hairline — `uncertain`, never a certain target.
- **Image** — the unit square `[0,1]×[0,1]` under the `Do` CTM,
  axis-aligned. This is the *whole image extent* — a conservative
  candidate, never the painted pixels — so rotation/shear/clip
  over-report coverage rather than under-report.
- **Form** — `/BBox` under `Matrix × CTM`; inner occurrences report
  with `Site::form_path` extended so a shared form's each call is
  distinguishable.
- **Path** — the construction ops' points under the CTM, inflated by
  the *stroke reach*: `w/2` for butt/round/square caps and joins,
  `miter_limit × w/2` when miter joins are in effect — scaled by
  `√2 × max(|col₁|, |col₂|)` of the CTM, the provable upper bound on
  a unit vector's stretch. `sqrt(|det|)` — the geometric mean of the
  singular values — **under-reports** non-uniform scaling (a `10 0 0
  0.1` CTM stretches by 10, not 1).
  
  **Hairline** — a `w` of 0 is a *device-pixel* stroke: its coverage
  is resolution-dependent and cannot be bounded in user space, so a
  stroked path (S, s, B, b, B*, b*) at `w == 0` reports as
  `uncertain` — never a certain `Target` whose bounds we would call
  "outside the rect". A fill-only path (`f`, `f*`) is unaffected —
  it does not stroke. For fill+stroke ops (`B`, `b`, `B*`, `b*`) the
  *whole* occurrence reports uncertain: the fill's bounds are known
  but the stroke's un-evaluated reach must not be dropped from the
  same occurrence.

### Graphics-state tracking

`DiagWalker` mirrors the renderer's lifecycle but the state it
tracks is the *whole* graphics state the diagnosis needs — CTM,
clip rect, stroke parameters (`w`/`J`/`j`/`M`), and the text state
parameters (`Tc`/`Tw`/`Tz`/`TL`/`Ts`/`Tr`/`Tf`, Table 52). `q`/`Q`
save and restore all of it; a `Form` `Do` pushes and pops the same
struct so `w`/`Tf`/`Tc` changes inside a form never leak to the
page level. `Tm`/`Tlm`/`in_text` are *not* graphics state — they
live on the walker inside `BT`/`ET`.

An uninterpreted state-changing construct (`gs` ExtGState, `sh`
shading, a Pattern colour space) is recorded as a `blocker` **and**
marks `uncertain_state`: every geometry op after it is reported as
`uncertain` rather than a certain `Target`, until `Q` or a form's
end restores known state. (`BX…EX` `Unknown` ops do *not* taint
state — §7.8.2 says they are ignored by definition.)

### Site identity

`Site` carries `contents` (the `/Contents` element refs — every
element when the array is used, since `page_operations` treats it
as one logical stream), `op_index` (the parsed op index in that
logical stream — for form-nested ops, the index *inside* the form's
stream), `form_path` (outermost-first [`FormFrame`] chain — each
frame has the resource `name`, resolved `xobj` ref, and the
call-site `op_index`, so two `Do`s of the same object or the same
name for two objects are distinct sites), and `code_index`.

`code_index` is the character's position in the *code stream of the
op*, counted per shown character code (a 1- or 2-byte code per the
font's CMap — not a Unicode scalar count and not a byte offset). For
a `Tj`/`'`/`"` op it counts the codes in that one string, restarting
at 0. For a `TJ` op it counts across the *whole array* —
`[(A) 100 (B)]` reports 'A' at index 0 and 'B' at index 1 — so two
characters that share one `op_index` stay distinct sites. Numeric
`TJ` elements produce no codes and do not advance the index. A code
that maps to several Unicode scalars still reports under its own
single `code_index` — the code, not its expansion, is the identity.

### CLI

```
kasane-pdf preflight-redaction <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] [--format text|json]
```

`--rect` is lower-left x,y then upper-right x,y in PDF user space —
the same un-rotated, un-cropped convention as `inspect` and
`extract_text`. Exit is `0` on a completed diagnosis *with or
without* blockers, `1` on a typed error (bad input, unreadable
content), `2` on usage errors. Output lines/state are deterministic
and capped at 1_000 entries per category; `incomplete`/`truncated`
are always stated, never silent.

The name is `preflight-redaction` precisely because this is not a
redaction engine and produces no safety guarantee — see
`src/redact_diag.rs`'s module doc.

## Experimental scoped text removal (M17, `remove_text`, CLI `experimental-remove-text`)

**Not a redaction tool** — the name and docs say so up front. M17
*deletes* the character codes whose filled-ink bounds overlap a
caller rectangle and writes a new single-revision PDF; it does not
paint over anything, does not touch other copies of the same data
(metadata, annotations, structure tree, other pages, font internals,
the original `/Contents` when it is shared), and does not guarantee
irreversibility. Think of it as an experiment in character-level
content editing on the smallest safe scope.

What it does — API `remove_text(doc, page_index, rects, limits)`
returning `Removal { output, codes_removed, sites, scope }`:

- Rects are un-rotated, un-cropped user space — the same convention
  as `preflight-redaction`/`inspect`. A code is removed when its
  *filled* glyph-outline bounds overlap any rect (edge-touching
  counts); a code is one removal unit even when it expands to several
  Unicode characters.
- Reuses the M16 `DiagWalker` so the graphics/text state and hit
  tests are exactly the diagnosis's — any `blocker`/`uncertain`
  entry, or a mid-stream walk failure, is a typed refusal, not a
  best-effort skip. Walk failures keep their typed error
  (`LimitExceeded`, `Structure`, `Parse`…) — they are never
  flattened into `Unsupported`.
- **Op-time state (M17.1)**: the walker records the text state at
  each page-level show op — resolved font, `Tfs`, `Th`, `Tc`, `Tw`
  — keyed by `op_index`. Selection and the rewrite read the *same*
  snapshot, so a later `Tf`/`Tc`/`Tw`/`Tz` change, `q`/`Q` restore,
  or second `BT`/`ET` can never rewrite an earlier show under the
  wrong parameters. `"`'s snapshot is taken *after* its `Tw`/`Tc`
  update.
- **Page-level eligibility (M17.1, extended M18)**: a page is
  eligible only when *every* shown text occurrence is `Tr 0` fill —
  an out-of-rect `Tr 3`/`Tr 1`/`Tr 2` glyph still refuses the page,
  whether it's at page level or inside a form. `Tr 4–7` and `w = 0`
  hairlines arrive via `blocker`/`uncertain` and refuse the same
  way. A `Tr` value *set* but never used to show text is harmless
  (the mode only matters when a glyph paints under it).
- **One level of `/Form` (M18)**: a page-level `Do` whose target is
  a `/Form` XObject is editable per-invocation — the form's inner
  text is rewritten into a *clone* of its stream (new object,
  `/Filter`/`/DecodeParms` dropped since the clone body is emitted
  uncompressed, `/Resources`/`/Matrix`/`/BBox` and every other key
  preserved verbatim), registered under a fresh `/XObject` name
  (`KasFm{n}`, deterministic and collision-checked against the
  page's effective `/XObject` dict), and only that `Do` is repointed
  in the rewritten page stream. Two `Do`s of the same form — or two
  resource names for the same object — are *distinct invocations*
  (keyed by the call-site `op_index` in `form_path`), so each clones
  its own copy; the unedited call keeps the original object, and
  every other page sharing it keeps the unmodified bytes. A form's
  own `/Resources` replaces the caller's per §8.10.1; a form
  *without* `/Resources` inherits them, and the clone reaches the
  same fonts because the names live in the page's effective dict.
- **Do-level eligibility (M18)**: a `Do` inside a form (`form_path`
  depth ≥ 2 anywhere in the walk) refuses — nested XObjects are out
  of scope. `Image` and `Other` targets refuse outright; `Form`
  targets are the invocation records themselves (depth 0, allowed).
  `/Group`/`/OC`/`/Ref` on a form is already a `blocker` from the
  walk and refuses the same way.
- Rewrites `Tj`/`TJ`/`'`/`"` only: a `Tj` losing codes becomes a
  `TJ` whose numeric elements restore the removed advance
  (`-(w0 + (Tc+Tw)·1000/Tfs)` — `Tz` cancels, so `Tfs = 0` or
  `Tz = 0` is refused); a `TJ` keeps its original numeric elements
  and splits string elements around removed runs; `'`/`"` expand to
  `T*` + `Tj` (and `Tw`/`Tc` for `"`) so their side effects survive
  even when the text goes to zero.
- Writes a *new* `/Contents` stream through the same
  `Edit`/`rewrite_overlay` pipeline as `annotate-rect` — the input
  document and its bytes are never modified, and other pages that
  share the original stream keep it untouched. The page's effective
  `/Resources` dict is materialised into the page object when a form
  is cloned, so an inherited or shared resources dictionary is never
  mutated in place.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, any `sh`/inline image/`gs`/Pattern colour
space/`ActualText`/OC, a `Do` inside a form, an `Image` or other
non-Form XObject, any rendering mode other than `Tr 0` fill,
`w = 0` hairlines, unsupported or unresolvable fonts, non-finite
CTM, `/UserUnit` ≠ 1, vertical text, or `Tfs`/`Tz` = 0. Every one of
those was already a `blocker`/`uncertain` in the diagnosis — removal
refuses on the first rather than continue past unknown state.

Limitations — by design and explicitly *not* done:

- Nested XObjects (a `Do` inside a form), images, annotation content,
  and forms carrying `/Group`/`/OC`/`/Ref` are out of scope (refused).
- A form's unedited invocations keep the *original* stream — the
  removed bytes still exist in that shared object (and on every
  other page that calls it).
- The original `/Contents` stays in the file when another page
  shares it (page 2's view is unchanged; the removed bytes still
  exist in the shared stream).
- Removed text can still exist elsewhere: `/Info`, XMP, structure
  tree, `ActualText`, embedded files, other pages, or the font's
  own cmap/name tables.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-text <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`; `--output` is
required, input and output must differ (same-file check is the
`refuse_same_file` helper), and the output is written atomically —
a refusal leaves any existing output untouched.

## Experimental image-region removal (M19/M19.1, `remove_image`, CLI `experimental-remove-image-region`)

**Not a redaction tool** — same guarantee boundary as `remove_text`.
M19 *replaces* the image samples whose painted positions land inside
caller-supplied rectangles with opaque black (all-zero samples) in a
*new* image object; the original stream is never mutated, and only
the `Do` call sites that hit a rect are repointed to the clone. It
does not remove other copies of the same image data (metadata,
annotations, structure tree, other pages, thumbnails), does not
guarantee irreversibility, and does not reason about what the image
depicts.

What it does — API `remove_image_region(doc, page_index, rects,
limits)` returning `ImageRemoval { output, invocations_edited,
pixels_changed, sites, scope }`:

- Rects are un-rotated, un-cropped user space — same convention as
  `preflight-redaction`/`inspect`/`remove_text`.
- Reuses the M16 `DiagWalker` (`new_diag_only` — no text snapshots)
  so the graphics state and hit tests are the diagnosis's; any
  `blocker`/`uncertain` entry or a mid-stream walk failure is a
  typed refusal.
- **Exact pixel union, not a bounding box (M19.2)**: each rect
  produces its own `(x0,y0,x1,y1)` pixel range via inverse-CTM
  mapping. The union is computed by splitting into horizontal bands
  at every distinct y-boundary, then merging x-intervals within each
  band — a band is a maximal y-strip whose x-interval set is
  constant. Corner-touching rects and L-shaped overlaps do not fill
  the pixels between them; overlapping ranges are merged so no pixel
  is visited twice. Outward rounding (`floor`/`ceil`) and the y-flip
  (image rows top-down) are unchanged. The intermediate `Vec` is
  bounded by `rects.len()` (`max_diag_rects`), never by image height.
- **Pixel count, not component count (M19.1)**: `pixels_changed`
  counts *pixels* — a pixel is one unit regardless of how many of
  its channels were non-zero. RGB `[r,g,b]` → `[0,0,0]` counts as 1;
  `[0,128,0]` → `[0,0,0]` also counts as 1; an already-black pixel
  counts as 0. Gray follows the same definition.
- **Colour-space resolution + Default* substitution (M19.2)**:
  `/ColorSpace` names are resolved through the page's effective
  `/Resources /ColorSpace` dict — `/CS1` → `/DeviceGray` works.
  A *device* name (`/DeviceGray`, `/DeviceRGB`) is then checked
  against the `Default*` substitution table in the same resource
  dict: `/DefaultRGB` → `/DeviceRGB` is accepted, but `/DefaultRGB`
  → `/CalRGB` (or any non-device space) is refused. Indirect refs
  are dereferenced, then the same rules apply. A missing
  `/ColorSpace` resource entry is `Error::Structure`, not a guess.
  Substitution does not recurse — a `Default*` that is itself a
  device name is ignored.
- **Pre-decode budget check (M19.2)**: `max_image_bytes` and
  `max_edit_content_bytes` are both checked *before* decoding —
  an image whose expected `Width × Height × ncomps` size exceeds
  the remaining cumulative budget is refused without allocating
  its buffer. A non-intersecting image is skipped before decode
  entirely (no budget consumed). `charge_generated` runs before
  the decoded buffer is held, so the clone is never built when
  the budget is insufficient. The decoded `Vec` is then moved
  (not cloned) into the edited body — no extra copy.
- Per-invocation cloning: fresh `/XObject` names (`KasIm{n}`,
  deterministic, collision-checked), only the hit `Do` is repointed,
  the page's effective `/Resources` is materialised, shared objects
  are never mutated in place.
- The final PDF is still bounded by `max_output_bytes` at serialize
  time — `BoundedWriter` checks the remaining budget before every
  `push`/`extend_from_slice`.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, `/UserUnit` ≠ 1, any `Do` to a `/Form`, any
inline image, any `gs`/`sh`/Pattern/`ActualText`/OC, any
`blocker`/`uncertain` from the walk, an image that is not 8-bit
DeviceGray/DeviceRGB (post-resolution), a `/Mask`/`SMask`/
`SMaskInData`/`OC`/`ImageMask`/`Interpolate`, a non-default
`/Decode`, a `/Filter` other than none or `/FlateDecode`, or a
non-axis-aligned/negative-scale/non-finite CTM.

Limitations — by design and explicitly *not* done:

- Nested XObjects (a `Do` inside a form), inline images, and all
  non-device colour spaces beyond the resolved name are out of scope
  (refused).
- The original image stream stays in the file when another page or
  an unedited invocation shares it — the changed pixels exist only
  in the clone.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-image-region <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`; `--output` is
required, input and output must differ, and the output is written
atomically — a refusal leaves any existing output untouched.

External check: `qpdf --check` clean; `pdfimages` extraction
confirms disjoint rects leave middle pixels unchanged and only the
selected pixels' components are zeroed; `pixels_changed` counts
pixels (not components) and matches the CLI report.

## Experimental scoped region removal (M20, `remove_region`, CLI `experimental-remove-region`)

**Not a redaction tool** — same guarantee boundary as `remove_text`
and `remove_image_region`. M20 performs a *single* integrated edit
pass over one page: removes character codes whose filled-ink bounds
hit a caller rect *and* blackens image samples whose painted
positions hit the same rects, then writes one new PDF.

What it does — API `remove_region(doc, page_index, rects, limits)`
returning `RegionRemoval { output, codes_removed,
invocations_edited, pixels_changed, sites, scope }`:

- One `DiagWalker::new` walk over the original content stream —
  every target is keyed by its *original* `op_index`. Text ops are
  rewritten by `remove_text::rewrite_ops_for` (same TJ/kern logic);
  image `Do`s are repointed to cloned image objects via `KasIm{n}`
  (same per-invocation cloning as M19). Both edits are applied to
  the *same* output ops list in one pass — a `Tj` that loses codes
  becomes a `TJ` at the same `op_index`, and a `Do` that hits a
  rect gets a new name at the same `op_index`. The page's
  `/Contents` is serialised once, and `rewrite` runs once.
- **M20.1** — the walker records the CTM at each `Image` `Do` call
  site into `TargetDetail::Image.ctm` (method A: no re-walk). This
  removes the O(images × ops) rescan that `ctm_at_op` used to
  perform — the budget `max_diag_exec_ops` now covers the *entire*
  CTM acquisition cost. `remove_image_region` benefits identically
  (it shares `DiagWalker`).
- **M21** — XObject names that were edited and are no longer
  referenced by any remaining `Do` are removed from the page's
  `/Resources /XObject` dict. The image object itself is left to
  the writer's reachability scan: if nothing else references it
  (another page, another `Do` via a different name, an inherited
  `/Resources`), it is not emitted. A name is removed only when
  the *edited* ops no longer use it — `new_ops` is scanned, not
  the original ops, so text expansion does not confuse the check.
  Names that were already unused before the edit are left alone.
- Text eligibility: `Tr 0` fill only, page-level (an out-of-rect
  `Tr 3`/`Tr 1` occurrence still refuses). `Tj`/`TJ`/`'`/`"` are
  rewritten with the same advance-preserving logic as `remove_text`.
- Image eligibility: 8-bit DeviceGray/DeviceRGB (post-resolution
  and `Default*` substitution), no `/Mask`/`SMask`/`SMaskInData`/
  `OC`/`ImageMask`/`Interpolate`, default `/Decode`, no filter or
  `/FlateDecode`, axis-aligned positive-scale CTM.
- **No Form `Do` in M20** — a `Do` to a `/Form` is refused even
  when it contains no text or images (M18's form handling is not
  inherited).
- Shared cumulative `max_edit_content_bytes` — the same budget
  bounds the cloned image bodies plus the new `/Contents` (not
  per-type). `max_image_bytes` is checked *before* each image
  decodes. `max_diag_exec_ops` bounds the walk (including the CTM
  recorded at each `Image` `Do` — M20.1). `max_operations`
  bounds the output op count. The final PDF is bounded by
  `max_output_bytes`.
- `pixels_changed` counts *pixels* (≥1 channel changed), not
  components — same definition as M19.1/M19.2.
- A rect that hits nothing produces an empty edit set — `Ok` with
  `codes_removed = 0`, `invocations_edited = 0`, `pixels_changed = 0`.
- Any failure — validation, unsupported construct, or limit — emits
  nothing; a partial rewrite is never returned as success.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, `/UserUnit` ≠ 1, any `Do` to a `/Form`, any
inline image, any `gs`/`sh`/Pattern/`ActualText`/OC, any
`blocker`/`uncertain` from the walk, non-`Tr 0` text, an image that
is not 8-bit DeviceGray/DeviceRGB (post-resolution), a mask/non-default
`/Decode`/non-Flate filter/non-axis-aligned CTM, or an `Image`
target at `form_path` depth ≥ 1.

A name removed from `/XObject` does **not** mean the object was
deleted — the writer's reachability scan decides that. If another
page, another `Do` under a different name, or an inherited
`/Resources` still references the object, it is emitted. The
`orphaned_names_removed` counter reports names removed from the
page's `/XObject`, not objects deleted.

An image *outside* every rect is never touched — its `hits` list is
empty so `to_edit_image` skips it entirely, and `validate_image` is
never reached. A JPEG outside the rect therefore leaves the call
`Ok` (the image is unedited); a JPEG *inside* the rect refuses the
whole call via `validate_image`. The same policy applies to text:
`Tr 3` outside the rect is a `blocker` (refused), `Tr 3` inside the
rect is a `hits`-non-empty `Target` (also refused).

Limitations — by design and explicitly *not* done:

- Form XObjects, inline images, JPEG, rotated/sheared images — all
  refused.
- The original image stream and `/Contents` stay in the file when
  another page or an unedited invocation shares them.
- Removed text can still exist elsewhere: `/Info`, XMP, structure
  tree, `ActualText`, embedded files, other pages, font internals.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-region <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`/`remove_text`/
`remove_image_region`; `--output` is required, input and output
must differ, and the output is written atomically — a refusal
leaves any existing output untouched.

External check: `qpdf --check` clean; `pdftotext` confirms removed
codes are absent and survivors keep positions; `pdfimages`
extraction confirms only selected pixels are zeroed and the
original image is preserved elsewhere in the file.

## Robustness

Guaranteed (tested): bad xref offset, missing referenced objects, cyclic
page tree, indirect reference loops, truncated input, freed-object reuse,
generation mismatch, bad xref-stream entry types, truncated Flate data,
oversized decoded streams → typed errors, no panic, no unbounded work.

Bounded by `Limits` (`src/limits.rs`): input bytes, reference-resolution
budget, page-tree depth/nodes, array/dict sizes, string/name length,
container nesting, xref entry count, `/Prev` chain length, decoded
stream size (per stream and cumulative), filter chain length, object
stream entry count and byte range, parsed-operation count
(`max_operations`), pending operand stack (`max_operand_stack`),
`q`/`Q` depth (`max_gsave_depth`), rendered output dimensions and
total pixels (`max_render_pixels`, `max_render_total_pixels`,
`max_render_buffer_bytes`), and path complexity
(`max_path_elements`). The M1 decoded-byte budget counts decode
*work*; the M2 operation limits bound the retained structure; the M3
render limits bound the output. M16 diagnosis adds input/output
bounds — `max_diag_rects`, `max_diag_items` (reported targets +
uncertain + blockers), `max_diag_share_pages` (cross-page sharing
scan), `max_diag_share_refs` (resource entries inspected), and a
CLI output cap of 1_000 lines per category — so a diagnosis never
grows without bound and its partial-result markers (`incomplete`,
`shared_scope.truncated`) are always stated.

M18.1 adds three cumulative budgets that the per-stream parse cap
(`max_operations`) cannot express:

- `max_diag_exec_ops` (200,000) — every content-stream *execution*
  inside one `diagnose_redaction`/`remove_text` walk. A page op,
  an op inside a `Do`'d form, an empty `Tj`, a state change, or an
  unknown op all count once. The same form invoked N times costs
  N× its op count; the per-stream parse cap sees each stream once.
- `max_show_states` (200,000) — entries in the walker's
  `show_states` snapshot map, one per page-level or form-level show
  op that ran, keyed by `(form_path, op_index)`. The map only
  records when `remove_text` asks for it; `diagnose_redaction`
  walks with snapshots off and never fills it.
- `max_edit_content_bytes` (64 MiB) — bytes of *generated* stream
  bodies held in the `Edit` overlay at once: each cloned form body
  plus the new page `/Contents`. `serialize_ops` receives the
  cumulative count so a `BoundedWriter` can refuse the *next* byte
  before it lands — a long `Tj` string or huge `TJ` array stops
  mid-write, never allocating past the bound. `charge_generated`
  then counts each finished body; the writer's `max_output_bytes`
  still caps the final PDF independently.
