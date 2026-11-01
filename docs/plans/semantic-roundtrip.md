# Semantic round trip

Status: implemented in the compatibility runner (`compat/runner/src/semantic.rs`,
2026-10-31; document-level categories in `semantic/doclevel.rs`,
2026-11-01); reported, not enforced. Aggregates only; per-file results
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
| named destinations | every binding of the `/Dests` dictionary and the `/Names /Dests` name tree, untagged: key → page index and view (a key bound in both stores is two lines; a name resolves through `/Dests` first, a string through the tree first) |
| page labels | the `/PageLabels` number tree expanded to every page: label, style, prefix, start |
| structure tree | elements by path: `/S`, role through `/RoleMap`, page, `/Alt`, `/ActualText`, `/Lang`, `/E`, `/T`; an element reachable from two parents is recorded once and referenced (`shared`); MCIDs, MCRs (with `/Stm`), OBJRs (annotation by page and position); the `/ParentTree` entry of every page MCID and annotation `/StructParent`, mapped back to an element path (`orphan` when it is not in the tree) |
| embedded files | `/Names /EmbeddedFiles`, catalog `/AF`, FileAttachment annotations: `/F`, `/UF`, `/Desc`, `/AFRelationship`, MIME type, `/Params` `/Size` and `/ModDate`, decoded length and digest |
| optional content | OCGs by position in `/OCGs` (name, intent, usage); every configuration (base state, ON / OFF / Locked, `/Order`, `/RBGroups`, `/ListMode`, `/AS`, …); every use — page `/Properties`, XObject `/OC`, annotation `/OC` — with OCMDs by members, policy and visibility expression |
| AcroForm | `/NeedAppearances`, `/SigFlags`, `/DA`, `/Q`; terminal fields by fully qualified name (unnamed intermediate nodes walked) with inherited `/FT`, `/Ff`, `/V`, `/DV`, `/Opt`, `/MaxLen`; their widgets by page and position with `/Rect`, `/AS`, `/MK`, the names of the appearance states, flags, `/DA`; the calculation order. The AcroForm half of an XFA form is compared here too (v0.5.0; before, an XFA form made this category `unsupported`) |
| XFA (`xfa`, v0.5.0) | `/AcroForm /XFA` as one stream or as the packet array: the form (`stream` / `array`, never equated), the packet count, every packet by position and name with the length and digest of its decoded content (order and duplicate names kept); the catalog's `/NeedsRendering` |
| document actions (`doc_actions`, v0.5.0) | catalog `/OpenAction` (destination or action) and `/AA` by trigger; page `/AA`; every annotation's `/A`, `/AA` and `/Dest` by page and position; `/AA` of field-tree nodes that are not on a page, by qualified name. An action is its type, every other key (a script by digest, a GoTo destination resolved, a structure destination's element by its structure-tree path) and its `/Next` actions in order, branches kept |
| JavaScript (`javascript`, v0.5.0) | the `/Names /JavaScript` tree flattened: each entry by its key (exact bytes) and its action chain |
| metadata (`metadata`, v0.5.0) | trailer `/Info`: every entry (text strings decoded); catalog `/Metadata`: state (`present`, `empty`, `not-a-stream`, `undecodable`), stream dictionary, length and digest of the decoded XMP |
| viewer preferences (`viewer_prefs`, v0.5.0) | every `/ViewerPreferences` entry (extensions included), `/PageMode`, `/PageLayout`; an absent entry is not filled in with its default |

Normalisation: object numbers never appear (pages by index, annotations
by page and position, OCGs by index, structure elements by path);
dictionary keys sorted; name and number trees flattened; text strings
decoded (UTF-16 or UTF-8 with a BOM, else PDFDocEncoding) and NFC
normalised (not NFKC: a ligature is a different string); numbers in one
canonical form; streams by digest (embedded files decoded when the
filter is FlateDecode without parameters, otherwise `unsupported`).
The snapshot reads the object graph itself and does not reuse the
engine's readers, so a reader bug cannot hide a writer bug.

The document-level categories (v0.5.0) add: content — XFA packets, XMP,
scripts — compared by the length and SHA-256 (96 bits) of its *decoded*
bytes, decoded by the snapshot's own FlateDecode (no predictor),
ASCII85 and ASCIIHex decoders, so recompression is not a difference and
any byte of content is; another filter or a predictor is `unsupported`,
data that does not decode is compared by its raw digest (`undecodable`);
a script (`/JS`) is a text string or a text stream (§12.6.4.17), decoded
to Unicode without normalisation before its digest, so the two storage
forms of one script are equal; an XML document is compared as bytes (no
XML canonicalisation: two serialisations of the same XML are a
difference — conservative, and what a verbatim carry must keep). An
entry whose value is null or a reference to an undefined object is
absent (§7.3.7, §7.3.10); an object the engine cannot read is
`#unresolved`, never null. A `/Next` reference back onto its own chain
is named by the position it returns to.

Limits: 200 000 object resolutions per document, tree depth 64, value
depth 12, 64 MiB per decoded embedded file. A cycle in an outline,
structure tree, field tree or name / number tree, or a limit, makes the
category `error`. The document-level categories also stop at action
chain depth 32, 64 MiB per decoded stream, 256 MiB decoded per snapshot
and 1 MiB per line, and treat a value cut at the value-depth limit as
`error` — never a silently shortened line. They come after the seven
earlier categories, so the shared node budget cannot change those.

Not in the snapshot: the content of appearance streams (only their
state names — the render round trip compares how they draw), the
marked-content sequences inside page content (the snapshot sees the
MCIDs the structure tree and ParentTree claim, not which content
carries them), and annotations other than widgets, file attachments
and OBJR targets (the existing annotation check covers their summary).
A reference to an undefined object is null (§7.3.10); a reference the
engine cannot resolve is a distinct `#unresolved` value.

## Classes

| class | meaning | success |
|---|---|---|
| `pass` | the snapshots are equal | yes |
| `fail` | a line differs, is lost or is added | no |
| `unsupported` | the snapshot needs something not implemented (a stream under another filter or a predictor) | no |
| `not_applicable` | neither side has the structure | — |
| `error` | the snapshot could not be taken (cycle, limit, malformed structure) | no |

Failure notes carry counts and the *names* of differing fields, never
their values: the run report can be a CI artifact.

## Verification

An independent review (another model) found four normalisation gaps
before the corpus run, all fixed with tests: a key bound in both
destination stores collapsed to one binding; a structure element shared
by two parents read as a cycle (`error`); fields under an unnamed
intermediate node were not walked; resolution failures read as null.

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
embedded files (1) and optional content (0 — the rewriter refused
`/OCProperties`); the synthetic tests carry those categories until the
corpus grows. The Linux nightly (2026-11-01) gives the same table.

Since the rewriter carries `/OCProperties` (2026-11-01): 427 compared,
pass 203, not applicable 224, fail / unsupported / error 0; optional
content 14 / 14 pass (58 lines: groups, configurations, uses).

## Document-level categories (v0.5.0 phase A, 2026-11-01)

Synthetic documents (`semantic/tests_doclevel.rs`), one base document
carrying all five categories: renumbered, keys reordered, XFA packets,
a script and XMP compressed, a script stored as a text stream instead
of a string, the JavaScript tree split into two leaves, the `/Info`
title as UTF-16 and indirect viewer preferences all pass; each of these
fails in exactly its category — an XFA packet changed, dropped,
renamed, swapped with another, duplicated, the array replaced by one
stream, XFA dropped, `/NeedsRendering` changed; the open action's target
changed, its `/Next` dropped, its `/Next` branches swapped, the catalog
script changed, page `/AA` dropped, a link target changed, a field's
keystroke script changed; a script changed, an entry dropped or renamed,
a chained `/Next` script changed; the `/Info` title changed, `/Info`
dropped, XMP changed, dropped or emptied; a preference changed, a vendor
extension key dropped, `/PageMode` changed. Also: an XFA change is not
hidden by the AcroForm half passing; absent structures are
`not_applicable`; an unsupported filter or predictor is `unsupported`;
malformed structures are `error` or recorded as malformed, never a
panic; `/Next` cycles end with the position they return to; cycles in
the JavaScript tree and the field tree are `error`; an action chain past
its depth, a value past the value depth, a line past 1 MiB and a packet
decoding past 64 MiB are `error`; a dangling `/Next` equals no `/Next`,
a lost body is a difference and an unreadable object is not null; a
structure destination names its element by path; ASCII85 / ASCIIHex
chains decode; notes carry no value or digest; kasane's rewrite of the
base document (without the entries the 0.4.0 rewriter refuses) passes.
Five deliberate breakages of the implementation (packet position,
`/Next`, `/NeedsRendering`, content digest, unknown preference keys
dropped) each make tests fail.

On the corpus (`--semantic-probe`): one object replaced by an appended
revision written by the probe itself (independent of the engine's
writers), compared with the original:

| probe | files | detected |
|---|---:|---:|
| xfa: a packet's content changed (+1 byte) | 29 | 29 |
| xfa control: the packet recompressed | 29 | 29 pass |
| javascript: a script changed | 31 | 31 |
| javascript control: the script recompressed | 30 | 30 pass |
| doc_actions: the open action removed | 42 | 42 |
| metadata: XMP changed | 174 | 174 |
| metadata control: XMP recompressed | 174 | 174 pass |
| metadata: `/Info` replaced | 446 | 446 |
| viewer_prefs: a preference changed | 441 | 441 |
| control: the catalog re-stored unchanged — no category fails | 446 | 446 |
| control: the `/Info` edit — no category but metadata fails | 446 | 446 |

In 9 further files the appended revision was not what the engine read
back (the probe checks this before comparing): files with a
hybrid-reference section, whose `/XRefStm` entries override the newer
section — an engine defect (v0.5.0 plan, X1), counted apart rather than
as misses.

Corpus result (macOS, nightly configuration, 427 rewrites compared):
overall pass 294, not applicable 133, **fail / unsupported / error 0**;
the seven earlier categories identical to 0.4.0 (class and line count
on every file); 91 files move from not applicable to pass because they
carry document-level structures.

| category | files with it | pass | fail | unsupported | error | lines |
|---|---:|---:|---:|---:|---:|---:|
| xfa | 0 | 0 | 0 | 0 | 0 | 0 |
| doc_actions | 67 | 67 | 0 | 0 | 0 | 15 724 |
| javascript | 0 | 0 | 0 | 0 | 0 | 0 |
| metadata | 273 | 273 | 0 | 0 | 0 | 1 417 |
| viewer_prefs | 125 | 125 | 0 | 0 | 0 | 235 |

XFA and JavaScript are 0 because the 0.4.0 rewriter refuses those
documents. On the rewrite spike (not merged), whose rewriter lifts those
refusals, 72 rewrites compare: xfa 29 / 29 pass, javascript 32 / 32,
doc_actions 15 / 15, metadata 49 / 49, viewer preferences 32 / 32,
AcroForm 39 / 39 — every one of the 29 XFA forms that were `unsupported`
before is now verified.

## Roll-out

Reported in the run report (`roundtrip.semantic`), the baseline summary
(`semantic` per category) and the per-category tables; `check` lists
pass → other changes without failing. Enforce once a nightly baseline
carries the field. The document-level categories follow the same path:
the Linux nightly and a baseline regenerated from it come with the
merge of phase A; enforcement only after that.
