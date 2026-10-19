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
render limits bound the output.
