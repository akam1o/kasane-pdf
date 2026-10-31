# Semantic round trip

Status: implemented in the compatibility runner (`compat/runner/src/semantic.rs`,
2026-10-31); reported, not enforced. Aggregates only; per-file results
stay in the gitignored `compat/reports/`.

## Why

The round trip's structure check (`graph_diff`) compares the object
graph reachable from `/Root` and `/Info` after renumbering. It catches a
lost or changed object, but it is the wrong tool for *meaning*: storing
named destinations in a name tree instead of a `/Dests` dictionary,
splitting a number tree, or re-encoding a title as UTF-16 are graph
differences with no change of meaning — exactly what a future writer
(e.g. `/OCProperties` support) may do. The semantic snapshot is the
check such a writer is measured against.

## Snapshot

Each document is reduced, per category, to a multiset of normalised
lines; the original and the rewrite are compared line for line.

| category | what a line records |
|---|---|
| outlines | hierarchy path (order), title, open / closed / leaf, destination — resolved through named destinations to page index and view — or action (GoTo by destination, others by dictionary, `/Next` chains), colour and style flags |
| named destinations | `/Dests` dictionary and `/Names /Dests` name tree merged: key → page index and view |
| page labels | the `/PageLabels` number tree expanded to every page: label, style, prefix, start |
| structure tree | elements by path: `/S`, role through `/RoleMap`, page, `/Alt`, `/ActualText`, `/Lang`, `/E`, `/T`; MCIDs, MCRs (with `/Stm`), OBJRs (annotation by page and position); the `/ParentTree` entry of every page MCID and annotation `/StructParent`, mapped back to an element path (`orphan` when it is not in the tree) |
| embedded files | `/Names /EmbeddedFiles`, catalog `/AF`, FileAttachment annotations: `/F`, `/UF`, `/Desc`, `/AFRelationship`, MIME type, `/Params` `/Size` and `/ModDate`, decoded length and digest |
| optional content | OCGs by position in `/OCGs` (name, intent, usage); every configuration (base state, ON / OFF / Locked, `/Order`, `/RBGroups`, `/ListMode`, `/AS`, …); every use — page `/Properties`, XObject `/OC`, annotation `/OC` — with OCMDs by members, policy and visibility expression |
| AcroForm | `/NeedAppearances`, `/SigFlags`, `/DA`, `/Q`; terminal fields by fully qualified name with inherited `/FT`, `/Ff`, `/V`, `/DV`, `/Opt`, `/MaxLen`; their widgets by page and position with `/Rect`, `/AS`, `/MK`, appearance states, flags, `/DA`; the calculation order |

Normalisation: object numbers never appear (pages by index, annotations
by page and position, OCGs by index, structure elements by path);
dictionary keys sorted; name and number trees flattened; text strings
decoded (UTF-16 or UTF-8 with a BOM, else PDFDocEncoding) and NFC
normalised (not NFKC: a ligature is a different string); numbers in one
canonical form; streams by digest (embedded files decoded when the
filter is FlateDecode without parameters, otherwise `unsupported`).
The snapshot reads the object graph itself and does not reuse the
engine's readers, so a reader bug cannot hide a writer bug.

Limits: 200 000 object resolutions per document, tree depth 64, value
depth 12, 64 MiB per decoded embedded file. A cycle in an outline,
structure tree, field tree or name / number tree, or a limit, makes the
category `error`.

## Classes

| class | meaning | success |
|---|---|---|
| `pass` | the snapshots are equal | yes |
| `fail` | a line differs, is lost or is added | no |
| `unsupported` | the snapshot needs something not implemented (XFA, an embedded file under another filter) | no |
| `not_applicable` | neither side has the structure | — |
| `error` | the snapshot could not be taken (cycle, limit, malformed structure) | no |

Failure notes carry counts and the *names* of differing fields, never
their values: the run report can be a CI artifact.

## Verification

Synthetic documents (`semantic/tests.rs`): renumbered, destinations in
a name tree, a split number tree, a UTF-16 title and a deflated embedded
file all pass; a lost outline title, a changed outline target, a changed
page label, swapped MCID ownership in the ParentTree, changed and
detached embedded files, an OCG switched off and a lost field value each
fail in exactly their category; no structures → `not_applicable`;
XFA and an ASCIIHex embedded file → `unsupported`; cycles → `error`;
kasane's own rewrite of the synthetic document passes.

On the corpus (`kasane-compat --semantic-probe`), changes made through
the editing API and saved incrementally:

| probe | files | detected |
|---|---:|---:|
| a text field's value changed | 27 | 27 |
| the annotation an OBJR points at removed | 49 | 49 |
| a file-attachment annotation removed | 1 | 1 |
| control: an unrelated `/Info` edit — no category may fail | 446 | 446 pass |

## Corpus result (2026-10-31, macOS, nightly configuration)

413 rewrites reparsed and compared; overall: pass 189, not applicable
224, **fail 0, unsupported 0, error 0**.

| category | files with it | pass | fail | unsupported | error | lines |
|---|---:|---:|---:|---:|---:|---:|
| outlines | 103 | 103 | 0 | 0 | 0 | 745 |
| named destinations | 31 | 31 | 0 | 0 | 0 | 6 522 |
| page labels | 8 | 8 | 0 | 0 | 0 | 74 |
| structure tree | 57 | 57 | 0 | 0 | 0 | 119 798 |
| embedded files | 1 | 1 | 0 | 0 | 0 | 1 |
| optional content | 0 | 0 | 0 | 0 | 0 | 0 |
| AcroForm | 51 | 51 | 0 | 0 | 0 | 185 |

The writer carries these structures verbatim, so equality is expected;
the graph check agrees (413 pass). Coverage is thin for page labels (8),
embedded files (1) and optional content (0 — the rewriter refuses
`/OCProperties`, so no rewrite with optional content exists yet); the
synthetic tests carry those categories until the corpus grows.

## Roll-out

Reported in the run report (`roundtrip.semantic`), the baseline summary
(`semantic` per category) and the per-category tables; `check` lists
pass → other changes without failing. Enforce once a nightly baseline
carries the field.
