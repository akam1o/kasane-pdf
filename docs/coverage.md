# kasane-pdf support matrix

Milestone: **M1**. Everything not listed as supported below is
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
| Hybrid-reference files (`/XRefStm`) | ✅ | classic chain primary; xref-stream entries fill gaps only |
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
| PNG predictors 10–15 | ✅ | `/Colors` 1–4, `/BitsPerComponent` 8 only; other values → `Error::Unsupported` |
| TIFF predictor 2 | ✅ | `/BitsPerComponent` 8 only; other values → `Error::Unsupported` |
| `/Predictor` 1 | ✅ | no-op |
| Other `/Predictor` values | ❌ | `Error::Unsupported` |
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
| `/Resources` | ⚠️ | resolved one level (direct dict returned); not interpreted |
| annotations, outlines, names, metadata, forms | ❌ | not read; keys are preserved in the parsed dictionaries only |

## Rendering / text / editing / writing

All ❌ — out of M1 scope by design. The redaction (墨消し) feature is a
long-term goal with its own design constraints, recorded in
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
stream entry count and byte range.
