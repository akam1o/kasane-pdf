# Residual-information audit

## Residual-information audit (`audit`, CLI `audit-residuals`)

**Read-only.** `audit_residuals(&Document, &Limits)` returns a
`ResidualAudit`; nothing is written, nothing is deleted, and the
input `Document`/bytes are never mutated. The CLI prints the same
result as text or `--format json` and writes no output file.

Detected, per finding — *existence and location only*; a finding is
**not** a "sensitive data" verdict and a clean report is **not** a
"safe to distribute" one:

- **Document info** — trailer `/Info` (key names only, never
  values; distinguished as `PreviousRevision` when the newest
  trailer has no `/Info` and the entry arrived via the merged
  trailer). Catalog `/Metadata` (XMP stream). Page `/Thumb`.
- **Attachments** — `/Names /EmbeddedFiles` name tree, `/Filespec`
  dicts and `/EF` streams (including annotation `/FS`), catalog
  `/AF`, `/Subtype /FileAttachment` annots.
- **Annotations / forms** — annot text-bearing keys (`/Contents`,
  `/RC`, `/T`, `/Subj`, `/Title`) by *presence* only, `/AP`, `/A` /
  `/AA` action keys, `/Subtype /Widget`, field `/V`/`/DV` on widget
  dicts, catalog `/AcroForm`, `/AcroForm /XFA`.
- **Structure / alternate text** — `/StructTreeRoot`; `/ActualText`,
  `/Alt`, `/E` inside structure elements (via `/K`) *and* inside
  page-content `BDC`/`DP` property lists (both inline dicts and
  `/Properties` name lookups).
- **Actions / optional content** — catalog `/OpenAction` and `/AA`,
  any dict carrying `/S` (with `/S /JavaScript` reported as
  `JavaScript`, other `/S` values as `Action`), `/Names /JavaScript`,
  catalog `/OCProperties` and its `/OCGs`.
- **Resources & streams** — each page's effective `/Resources`,
  every registered `/Font`/`/XObject` name (as
  `ResourceUsageUnexamined` — *not* "unused"), every live xref stream
  classified as `Image`, `FormXObject`, `ToUnicode` (via a font
  dict's `/ToUnicode`), or `EmbeddedFont` (a `/FontFile*` inside a
  `/FontDescriptor`).

**Reachability classes** — the audit walks a root set of trailer
`/Root`, trailer `/Info`, and every page object. Each object-bearing
finding is `Reachable`, `UnreachableLive` (in the merged xref and
*confirmed* unreachable — reported only when the walk drained its
queue), `PreviousRevision` (a superseded body from an older xref
section still present), or `Unevaluated` (the walk was cut short or
the ref was missing — never reported as "unreachable" while the
walk is incomplete). `referrers` records the bounded set of
`(object, key/index)` paths that point at an object; `via` carries a
representative `PathStep` list (dict key, array index, name-tree
entry, resource name). A reference edge whose target has no xref
entry is a `MissingReference` finding — the referrer path is
recorded, the target is not an object and counts as neither
examined nor reachable.

**History / physical data** — `xref_sections`, `incremental_history`
(`Some(n)` = n prior `/Prev` sections; `None` = single section —
*not* proof that no old bytes exist), `free_entries`,
`compressed_entries`, `objects_in_old_region` (superseded bodies
found by re-reading each `/Prev` section and comparing offsets —
the merged xref's newest-wins shadow is undone). Physical slack
(between objects, after `%%EOF`, unreferenced regions) is listed
under `unexamined`, never as "absent".

**ObjStm note** — stream objects cannot live inside `/ObjStm` (they
must appear as top-level indirect objects), so "an Image inside an
ObjStm" is impossible by construction. What the audit may still
miss is a *non-stream* dictionary inside an `/ObjStm` — e.g. a
`Filespec` or an annot dict — because the compressed-objects pass
reads only xref entries, not the ObjStm member bodies. This is
listed under `unexamined` as `compressed-objects`, not counted as
"absent".

**Partial results** — `incomplete` records each scope that stopped
early (limit name, or a non-limit reason like an unparsable content
stream, a name-tree cycle, a missing `/Kids` ref, or a name-tree
*entry point* that fails to resolve), so truncated coverage is never
reported as "none found". A name-tree entry-point failure (`/Names`
itself or a tree root ref) is recorded under its own scope —
unresolved does not mean "no attachments". `unexamined` lists
scopes not evaluated at all. The walk's completeness is reported
separately: `walk_complete` is `false` whenever the BFS stopped
early, and in that case unvisited live objects count as
`unevaluated_live`, never `unreachable_live`.

**Budgets** (all in `Limits`): `max_audit_findings`,
`max_audit_edges` (reference edges followed), `max_audit_paths`
(referrers kept per object), `max_audit_walk_depth` (container
nesting depth inside one object — **not** element count),
`max_audit_elements` (container elements expanded across every
traversal — name-tree nodes, `/Names` pairs, `/Kids` entries, array
items, dict entries), `max_audit_content_ops` (per-page ops
scanned for marked content). The document's own
`max_reference_depth` and `max_total_decoded_bytes` still apply to
resolutions/decodes.

**Incremental traversal** — `walk_object_refs` keeps one frame per
*open container* (a `Vec` of `enum Frame` holding a `slice::Iter`
for arrays or a `btree_map::Iter` for dicts/stream-dicts) rather
than one frame per child element. Stack height is proportional to
*nesting depth*, never breadth — a 200-element array is one frame
that yields its children lazily, so a low `max_audit_elements`
budget stops the walk before all children are expanded into frames
or paths. A `Vec<PathStep>` is cloned only when a `Ref` is found or
a container is descended into, not for every element visited.
(the test-only `max_walk_stack` field on `Audit` plus a
`push_frame!` macro let tests assert this against the *production*
walker — no duplicated scan.)

**Leakage** — notes carry keys, types, counts and refs only: Info
values, annot contents, field values, JavaScript, XMP and stream
bodies never appear in findings, `Debug`, or CLI output. External
check: `scripts/verify-audit.sh` compares findings against qpdf's
view on representative fixtures.

CLI:

```sh
kasane-pdf audit-residuals <file.pdf> [--format text|json]
```

Exit code 0 on a completed audit (findings or not), 1 on a hard
error — it is an *execution status*, not a safety verdict.


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
(`max_path_elements`). The decoded-byte budget counts decode
*work*; the operation limits bound the retained structure; the
render limits bound the output. the diagnosis adds input/output
bounds — `max_diag_rects`, `max_diag_items` (reported targets +
uncertain + blockers), `max_diag_share_pages` (cross-page sharing
scan), `max_diag_share_refs` (resource entries inspected), and a
CLI output cap of 1_000 lines per category — so a diagnosis never
grows without bound and its partial-result markers (`incomplete`,
`shared_scope.truncated`) are always stated.

The text editor adds three cumulative budgets that the per-stream parse cap
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
