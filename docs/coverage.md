# kasane-pdf support matrix

Milestone: **M0**. Everything not listed as supported below is
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
| xref streams (`/Type /XRef`) | ❌ | `Error::Unsupported { feature: "xref stream" }` |
| Hybrid-reference files (`/XRefStm`) | ❌ | `Error::Unsupported { feature: "hybrid-reference file" }` |
| Object streams (`/ObjStm`) | ❌ | any file needing them for the page tree fails when the target object can't be resolved; explicit detection arrives with xref streams |
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
| `stream … endstream` | ⚠️ | raw bytes captured; `/Filter` decoding unsupported; extent via consistent direct `/Length`, else `endstream` scan; indirect `/Length` without marker → `Error::Unsupported` |
| `obj … endobj` | ✅ | missing `endobj` tolerated (offset-driven parse) |

## Document layer

| Feature | Status | Notes |
|---|---|---|
| trailer `/Root` → catalog | ✅ | missing → `Error::Structure`; `/Type` checked if present |
| page tree walk | ✅ | depth ≤ `max_page_tree_depth`, nodes ≤ `max_page_tree_nodes`; revisited nodes → `Error::CycleDetected` |
| inherited `MediaBox`, `CropBox`, `Rotate`, `Resources` | ✅ | per Table 30; `CropBox` defaults to `MediaBox`, `Rotate` defaults to 0 |
| root `/Count` consistency | ✅ | mismatch → `Error::Structure` |
| `/Contents` | ⚠️ | exposed unresolved (reference or array); not interpreted |
| `/Resources` | ⚠️ | resolved one level (direct dict returned); not interpreted |
| annotations, outlines, names, metadata, forms | ❌ | not read; keys are preserved in the parsed dictionaries only |

## Rendering / text / editing / writing

All ❌ — out of M0 scope by design. The redaction (墨消し) feature is a
long-term goal with its own design constraints, recorded in
[`design.md`](design.md#redaction-design-constraints).

## Robustness

Guaranteed (tested): bad xref offset, missing referenced objects, cyclic
page tree, indirect reference loops, truncated input → typed errors, no
panic, no unbounded work.

Bounded by `Limits` (`src/limits.rs`): input bytes, reference-resolution
budget, page-tree depth/nodes, array/dict sizes, string/name length,
container nesting, xref entry count, `/Prev` chain length.
