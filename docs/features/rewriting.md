# Full-rewrite output — support & guarantees

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

`scripts/verify-rewrite.sh` runs `qpdf --check` (v12.4.2) on the
rewrite output of every well-formed fixture and requires exit 0 with
zero warnings. Run it explicitly — `cargo test` does not depend on
qpdf. Fixtures are deterministic (`make_fixtures.py`); intentionally
broken inputs (`hybrid.pdf`, `xobject_image_dct.pdf`, `render_annot.pdf`,
`contents_none.pdf`, `flate_*`, `prev_chain`, `unsupported_filter`,
`bad_offset`, `missing_ref`, `cyclic_pages`, `truncated`) are kept for
load-path tests and excluded from the clean-validation set.
