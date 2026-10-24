# Parsing & document structure — support matrix

Covers file structure, the object layer, stream decoding, the
document layer (catalog/page tree/resources), and content-stream
parsing. Everything not listed as supported is a typed error.


## File structure

| Feature | Status | Behaviour / error |
|---|---|---|
| `%PDF-` header within first 1024 bytes | ✅ | `Error::NotPdf` otherwise |
| `startxref` near end of file | ✅ | `Error::Parse` if missing/invalid |
| **Recovery mode** (`Document::from_bytes_recovering`) | ✅ | opt-in, separate from the strict parse: when the strict parse fails for a *structural* reason (never `NotPdf`, `Encrypted`, `LimitExceeded`) the xref table is rebuilt by scanning the whole file for `n g obj` headers (a header needs a non-regular byte before the number and whitespace around the generation), the **last** definition of a number winning as in an incremental update; `/Root` comes from the merged `trailer` dictionaries, else the newest `/Type /XRef` dictionary, else the single (or last-defined) object declaring `/Type /Catalog`; `/Type /ObjStm` members fill gaps no direct definition covers; `/Encrypt` still refuses. Everything deviating from a clean read is listed on `RecoveryReport` (`Document::recovery()`, `None` for a strict parse): strict error, objects found, members from streams, `/Root` source, warnings. Bounded by `max_recovery_objects` (default 2²⁰) and the usual stream/objstm limits; page-tree cycle detection and reference budgets are unchanged, so a recovered document is as safe to walk as a strict one |
| Classic `xref` tables, incl. subsections | ✅ | `Error::Parse` on malformed lines |
| `trailer` dictionary | ✅ | merged across `/Prev` chain |
| `/Prev` chains (incremental updates in classic format) | ✅ | newest section wins; cycles → `Error::CycleDetected` |
| xref streams (`/Type /XRef`) | ✅ | entries read from the **decoded** stream (`/Filter` + predictors), `/W` (0–8 bytes/field), `/Index` (default `[0 /Size]`), types 0/1/2; bad type → `Error::Parse`, bad width → `Error::Unsupported` |
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
| `obj … endobj` | ✅ | a missing `endobj` is a parse error in the strict parse (§7.3.10); recovery mode (`from_bytes_recovering`) reads the object anyway, since its own syntax delimits it |

## Stream decoding (ISO 32000-1 §7.4)

| Feature | Status | Notes |
|---|---|---|
| No filter | ✅ | raw bytes returned |
| indirect references to undefined objects (§7.3.10) | ✅ | no cross-reference entry for the number and generation, or a free entry → the null object, recorded once as `Warning::UndefinedReference` (strict mode too — the standard requires it). An entry that *exists* but points at damage is still `Error::Parse` / `Error::MissingReference`; the writers and the audit treat an undefined reference as `MissingReference` so a rewrite never turns it into `null` silently |
| content stream ending inside `q`, `BT`, `BMC`/`BDC`, `BX` | ✅ | the block is closed at the end of the stream — nothing after it could be affected — and recorded as `Warning::UnterminatedContentBlock`; a stray `Q`, `ET`, `EMC`, `EX` is still `Error::Structure` in strict mode (recovery mode ignores a stray `Q`, `RecoveryRule::IgnoredRestoreWithoutSave`) |
| `/FlateDecode` (`/Fl`) | ✅ | zlib/DEFLATE via `flate2`/`miniz_oxide`; output capped at `max_stream_bytes` while it is produced. Data that ends exactly after a sync-flush block (`00 00 FF FF`) with the final block and Adler-32 missing is **accepted with `Warning::UnterminatedFlateStream`** — every block was complete, so the output is exact; only the checksum is unverifiable. Data that ends inside a block is `Error::Parse("… incomplete deflate stream")`; corrupt data is `"… corrupt deflate stream"` |
| `/LZWDecode` (`/LZW`) | ✅ | 9–12-bit codes MSB first, clear (256) / EOD (257), `/EarlyChange` 0 or 1 (default 1), KwKwK case, mid-stream clear; data ending inside a code with non-zero bits → `Error::Parse`, zero padding without EOD accepted; predictors as for Flate |
| `/ASCII85Decode` (`/A85`) | ✅ | `z`, partial final group, `~>` EOD (its absence accepted at a clean group boundary), an optional `<~` prefix skipped; `~` not followed by `>`, a one-character final group, `z` inside a group, a byte outside `!`..`u` → `Error::Parse`; a final group of one character (encodes no byte) is ignored and recorded as `Warning::IgnoredFilterFragment` |
| `/ASCIIHexDecode` (`/AHx`) | ✅ | white space ignored, odd final digit padded, `>` EOD (absence accepted); a non-hex byte → `Error::Parse` |
| `/RunLengthDecode` (`/RL`) | ✅ | literal and repeat runs, 128 EOD (absence at a run boundary accepted); a run cut off mid-way → `Error::Parse` |
| `/Filter` as name or array | ✅ | array order applied sequentially |
| `/DecodeParms` dict/array/null | ✅ | positional match with `/Filter` |
| `/Length` direct or indirect | ✅ | indirect `/Length` resolved at decode time |
| PNG predictors 10–15 | ✅ | `/Colors` 1–4, `/BitsPerComponent` 8 only; 10–14 fix the row filter, 15 = per-row optimum; other values → `Error::Unsupported` |
| TIFF predictor 2 | ✅ | `/BitsPerComponent` 8 only; other values → `Error::Unsupported` |
| `/Predictor` 1 | ✅ | no-op |
| `/Predictor` 3–9, 16+ | ❌ | `Error::Unsupported` (reserved/undefined in spec) |
| Image filters (`/DCTDecode`, `/JBIG2Decode`, `/CCITTFaxDecode`) | ◐ | they yield image samples, not bytes, so they are decoded by the image pipeline (see rendering.md, `Do`) and refused here with `Error::Unsupported` when they appear on a content stream, font program or other byte stream |
| `/JPXDecode`, `/Crypt`, unknown names | ❌ | `Error::Unsupported` when the stream is requested; document still loads |

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
| `SC` `sc` | ✅ | any number of numeric components; arity checked at colour-space resolution |
| `SCN` `scn` | ✅ | numeric components + optional trailing pattern name; patterns → `Error::Unsupported` at render |
| `BT` `ET` | ✅ | nesting and balance checked |
| `Tc` `Tw` `Tz` `TL` `Tf` `Tr` `Ts` `Td` `TD` `Tm` `T*` | ✅ | `Tf` keeps the font resource *name* |
| `Tj` `TJ` `'` `"` | ✅ | `TJ` items: `TJItem::Text` (raw bytes) / `TJItem::Kern` (number); non-string/number items → `Error::Parse` |
| `Do` | ✅ | resolved via the *current* `/XObject` resource dict; `/Form` → painted via §8.10 rules (own `/Matrix`, `/BBox` clip, own `/Resources`); `/Image` → §8.9 sampled image (8-bit DeviceGray/RGB, raw/Flate/DCT); other subtypes → `Error::Unsupported` |
| `sh` | ✅ | resource name kept; shading **not** executed |
| `MP` `DP` `BMC` `BDC` `EMC` | ✅ | `DP`/`BDC` property operand must be a dict or name; balance checked |
| `BX` `EX` | ✅ | inside, unknown operators are recorded as `Operation::Unknown` instead of failing (§7.8.2) |
| `BI` `ID` `EI` (inline images) | ✅ | `Operation::InlineImage { dict, data }` — the dictionary is read up to `ID`, the data end is *computed* (see `docs/features/rendering.md`, `Do`), never guessed from the first `EI`; a stray `ID`/`EI` or data that never closes → `Error::Parse`/`UnexpectedEof` |
| Unknown operator outside `BX`/`EX` | ❌ | `Error::Parse` |
| Wrong operand count / type, leftover operands at EOF | ❌ | `Error::Parse` |
| Rendering / executing operations | ❌ | out of the parser's scope — operations are data, not executed |
