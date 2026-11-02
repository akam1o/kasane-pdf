# Full-rewrite output — support & guarantees

## Rewrite output (ISO 32000-1 §7.5)

`rewrite(&Document)` rebuilds a loaded document as a fresh
single-revision PDF: DFS from trailer `/Root` (plus `/Info`), dense
renumbering 1..N, classic xref + trailer emitted. No original bytes,
`/Prev` history or ObjStm containers are carried over — only the
reachable object graph.

"Reachable" means objects the DFS can reach through `/Ref`s starting
at `/Root`/`/Info`. Keys *inside* a reachable dictionary are kept
verbatim — including keys the rewriter does not understand. That is distinct
from *unreferenced* objects elsewhere in the file: unreferenced
objects are simply never walked and never emitted (the only
deliberate exception is an indirect stream `/Length`, whose slot is
skipped at collection and recomputed at write time).

| Feature | Status | Notes |
|---|---|---|
| object collection & renumbering | ✅ | dense 1..N; shared refs share the new number; /Parent cycles terminate via the map |
| references to undefined objects | ✅ (0.5) | §7.3.10: no in-use or compressed entry for that number and generation (absent, free, another generation) — the reference is kept and points at a new indirect object whose body is `null` (one per undefined target, shared), so array positions and dictionary keys are unchanged and the output has no dangling reference; each recorded as `Warning::UndefinedReference`. A *defined* object that cannot be read (damaged body, object-stream member missing, reference cycle, limit) still fails the rewrite. Before 0.5 every such document was refused with `MissingReference` |
| `/Info` dictionary | ✅ | carried when trailer references it |
| `/Prev`, `/XRefStm` | — | dropped by design (single-revision file) |
| trailer `/ID` | ✅ (0.5) | written for PDF 2.0 output only, where it is required (ISO 32000-2 Table 15): the input's first (permanent) identifier when it is a string, the second the MD5 of the written body (§14.4: it changes with a modification; the same input gives the same output); both the digest when the input has none, or under `remove_document_metadata` (the identifier identifies the document too). 1.x output has none, as before |
| stream bodies | ✅ | encoded bytes verbatim; `/Length` recomputed from emitted bytes; `/Filter`/`/DecodeParms` kept consistent |
| object streams (input) | ✅ | objects inside `/ObjStm` are resolved and emitted as normal indirect objects |
| incremental input | ✅ | xref-chain merge already yields the latest revision |
| link annots | ✅ | `/Subtype /Link` `/Rect`, `/A` `/URI`/`/GoTo`, `/D` dest arrays — refs remapped |
| direct destination arrays | ✅ | `[page /XYZ …]`, `[page /Fit]` — refs remapped |
| **named destinations** | ✅ | preserved: the catalog `/Names` → `/Dests` name tree (`/Kids`, `/Names`, `/Limits`), the legacy catalog `/Dests` dictionary, destination arrays and `/D` dictionaries, and every link `/Dest`, `GoTo` `/D` and `OpenAction` `/D` name/string operand are copied verbatim with renumbered references — the name → page binding is identical before and after. `kasane_pdf::named_destinations` / `resolve_named_destination` (`src/dests.rs`) read the structure (bounded by `max_page_tree_depth` / `max_page_tree_nodes`, cyclic `/Kids` → `CycleDetected` before any output) and the round-trip tests assert input and output agree. A dangling name is the input's defect and is copied as-is, never repaired or dropped. No page-removal/reorder feature exists, so no destination needs remapping; region-removal edits go through the same `rewrite_overlay` and keep the tree untouched |
| encrypted input | ❌ | refused at `Document::from_bytes` |
| `/Perms` `/DocMDP`/`/UR`/`/UR3`, signed `/Sig` fields | ❌ | `Error::SignaturePolicy` (`SignatureRule::RewriteOfSignedDocument`) — a signature covers the original bytes (`/ByteRange`), so a rewrite (new objects, offsets and xref; the incremental-update history flattened) would invalidate it and is refused before anything is written. A field counts as signed when `/FT /Sig` (own or inherited) carries a `/V` dictionary, anywhere under `/AcroForm /Fields` → `/Kids` (depth ≤ 32) **or** as a page widget outside `/Fields`. Blank `/Sig` fields (no `/V`) pass through like any other widget. Changes a signer permits (form filling, annotations under `/DocMDP /P 2`/`3` or an approval signature) can be appended with `Edit::save_incremental()` instead — see [`editing-incremental.md`](editing-incremental.md) |
| `/AcroForm` `/XFA` | ✅ (0.5) | carried verbatim (§12.7.8): the single stream or the packet array — packet names, order and duplicates, every packet's stream bytes and `/Filter` — and the catalog's `/NeedsRendering`, with renumbered references; nothing is parsed, regenerated or reordered. A packet whose content holds an XML signature is `Error::SignaturePolicy`, like every signature: the XML-DSig namespace (`xmldsig`, any case) anywhere in a packet, after numeric character references (any number of digits) are expanded, in UTF-8, UTF-16 or another ASCII-compatible encoding — an element is only an XML signature in that namespace, and the namespace can only be bound by writing it. A packet that cannot be read completely to check — filter or limit failure, data in an external file (`/F`), UTF-32, EBCDIC, NUL bytes in no known encoding, a declared encoding that is not ASCII-compatible or contradicts the bytes, a DTD (its entities can build markup) — is `Error::Unsupported`. The packets form one XML document, so after each packet alone the same checks run on all of them together: a character reference, declaration, DTD or namespace split across packets counts. Conservative: a mention of the namespace in a comment or CDATA also refuses. Every reference these checks follow is followed through object streams too |
| `/OCProperties` | ✅ | carried when well formed (see below), else `Error::Unsupported` |
| PDF 2.0 structures | ✅ (0.5) | page-level `/Metadata`, associated files (`/AF` on the catalog, pages, annotations, structure elements, XObjects, marked-content property lists, DParts — ISO 32000-2 §14.13) with their file specifications and embedded files, document parts (`/DPartRoot`, the DPart tree, `/DPM`, pages' `/DPart`, §14.12) and every other 2.0 entry are ordinary reachable objects: copied as they are, references renumbered, sharing kept. Features 2.0 deprecates (XFA, `/NeedsRendering`, `/Info` entries other than the dates) are carried, not modernised; the refusals (signatures, scripts, `/RichMedia`, `/AA`) are the same as for 1.x |
| catalog `/RichMedia`, `/AA`; `/OpenAction` and `/Names` JavaScript | ❌ | `Error::Unsupported` — outside the profile |
| output xref format | — | classic table only; xref-stream/ObjStm output is not implemented |
| stream `/Length` | ✅ | always emitted as a direct integer recomputed from the emitted bytes; an *indirect* `/Length` ref is never walked from the stream dict (would emit an orphan), but the same object is still collected if referenced elsewhere |
| PDF version | ✅ | output = `max(header_version, catalog /Version)` (§7.2.2; at least 1.5 with optional content); `1.x` passes through, **`2.0` is written as 2.0 (0.5)** — never translated down — with the trailer `/ID` above; above 2.0 is `Error::Unsupported` (its rules are not known). A 1.x input that uses a 2.0 structure (`/AF` in a PDF/A-3 file) keeps its version. **Malformed input versions are refused, not guessed**: a `%PDF-M.m` header whose token is not `1.x`/`2.0` → `Unsupported`; a catalog `/Version` that is present but not a well-formed `M.m` name → `Unsupported` (never silently dropped, never treated as absent) |

**Not a sanitizer.** A full rewrite does *not* remove unreferenced
history in a guaranteed way (only reachable objects are emitted, but
anything reachable — including `/Info`, metadata and embedded data —
is preserved verbatim). Do not present a rewrite as redaction.

**Preserve ≠ guarantee ≠ inspect.** The rewrite preserves every
reachable dictionary key verbatim — including keys whose *feature*
the engine does not implement (colour management, transparency, patterns,
optional content inside content streams, etc.). "Preserved" means the
bytes are carried over; it does **not** mean the feature works, is
validated, or is safe. The refusal list above is exactly the set the rewriter
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

`scripts/verify-rewrite.sh` runs `qpdf --check` (v12.4.2) on the
rewrite output of every well-formed fixture and requires exit 0 with
zero warnings. Run it explicitly — `cargo test` does not depend on
qpdf. Fixtures are deterministic (`make_fixtures.py`); intentionally
broken inputs (`hybrid.pdf`, `xobject_image_dct.pdf`, `render_annot.pdf`,
`contents_none.pdf`, `flate_*`, `prev_chain`, `unsupported_filter`,
`bad_offset`, `missing_ref`, `cyclic_pages`, `truncated`) are kept for
load-path tests and excluded from the clean-validation set.
