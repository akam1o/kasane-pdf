# kasane-pdf support matrix

Milestone: **M2**. Everything not listed as supported below is
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
| `/Resources` | ✅ | inherited per Table 30 (nearest ancestor's dict used *whole* — no per-key merge); named entries in `Font`, `XObject`, `ExtGState`, `ColorSpace`, `Pattern`, `Shading`, `Properties`, … resolvable via `Document::page_resource()`; wrong type → `Error::Structure`, missing → `Ok(None)`; targets not interpreted |
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
| `SC` `SCN` `sc` `scn` | ✅ | 0–4 numeric components + optional trailing pattern name (`SCN`/`scn`); the real arity depends on the current colour space, which is not resolved at this stage |
| `BT` `ET` | ✅ | nesting and balance checked |
| `Tc` `Tw` `Tz` `TL` `Tf` `Tr` `Ts` `Td` `TD` `Tm` `T*` | ✅ | `Tf` keeps the font resource *name* |
| `Tj` `TJ` `'` `"` | ✅ | `TJ` items: `TJItem::Text` (raw bytes) / `TJItem::Kern` (number); non-string/number items → `Error::Parse` |
| `Do` `sh` | ✅ | resource name kept; XObject/shading **not** executed or decoded |
| `MP` `DP` `BMC` `BDC` `EMC` | ✅ | `DP`/`BDC` property operand must be a dict or name; balance checked |
| `BX` `EX` | ✅ | inside, unknown operators are recorded as `Operation::Unknown` instead of failing (§7.8.2) |
| `BI` `ID` `EI` (inline images) | ❌ | `Error::Unsupported` — the binary data between `ID` and `EI` cannot be tokenized without guessing |
| Unknown operator outside `BX`/`EX` | ❌ | `Error::Parse` |
| Wrong operand count / type, leftover operands at EOF | ❌ | `Error::Parse` |
| Rendering / executing operations | ❌ | out of M2 scope — operations are data, not executed |

## Matrices (ISO 32000-1 §8.3)

`Matrix` is a `[a b c d e f]` affine transform with `transform`,
`concat` (PDF `cm` order: `existing.concat(cm)` = `cm × existing`),
`translate`/`scale`/`rotate` helpers, and `is_finite` checking. Page
`CropBox`/`Rotate` → device-space mapping is *not* built yet and is
unrelated to the content-level `cm` operator.

## Rendering / text / editing / writing

Rendering is ❌ — M2 parses operations but does not execute them. Text
extraction (Unicode mapping, `ToUnicode` CMaps) is also future work.
The redaction (墨消し) feature is a long-term goal with its own design
constraints, recorded in
[`design.md`](design.md#redaction-design-constraints).

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
(`max_operations`), pending operand stack (`max_operand_stack`), and
`q`/`Q` depth (`max_gsave_depth`). The M1 decoded-byte budget counts
decode *work*; the M2 operation limits bound the retained structure.
