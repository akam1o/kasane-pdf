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
difference — conservative, and what a verbatim carry must keep). XFA
packet names are text strings compared as exact Unicode (another
encoding of the same name is equal, a canonically equivalent sequence
is not). Inside these categories only `/Parent` and a stream's
`/Length` are skipped as bookkeeping — not `/P`, `/Next`, `/Prev`, which
the earlier categories skip as back-pointers but which are content
inside an action value (a GoToE target's page, a Launch action's
parameters). An
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
| `transformed` | every difference is a transformation the run declared, and every declared change happened (V4, `semantic/transform.rs`) | no — reported separately, never counted as `pass` |
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
structure destination names its element by path; a GoToE target's `/P`
and a Launch action's `/Win /P` are compared; XFA packet names are exact
Unicode; ASCII85 / ASCIIHex chains decode (a stray `~` is an error, not
an end); notes carry no value or digest; kasane's rewrite of the
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

The probe counts a revision it could not write or reparse as skipped
(none on the corpus). In 9 further files the appended revision was not
what the engine read back (the probe checks this before comparing): files with a
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

After the cross-reference fix (X1), Linux nightly 41315396310 and the
macOS run agree: 428 rewrites compared (one more: a document whose
generation-2 objects now resolve), pass 295, not applicable 133, fail /
unsupported / error 0; doc_actions 68 (a hybrid-reference file's page
annotations are now read), metadata 274 (one from the newly compared document), viewer preferences 125. The
baseline records the categories per file (report only).

## PDF 2.0 categories, report only (v0.5.0 W2, 2026-11-02)

Four categories (`semantic/pdf20.rs`) taken after the others with their
own node budget and reported in `roundtrip.semantic.report_only`. They
never enter the overall class or the `semantic` map of the baseline
summary (`ci-regression.py` records them as `semantic_report` and lists
a lost pass without failing), so adding them changes no existing
result.

| category | what is compared | what is not a difference |
|---|---|---|
| `version` | the effective version: the header's, or the catalog `/Version` when it is later (ISO 32000-2 §7.2.2), read from the file bytes and the catalog by the snapshot | how it is declared — header only, catalog only, an indirect `/Version` — recorded as *declared form* and named in the notes when it changes |
| `page_metadata` | each page's own `/Metadata` (not inheritable) by page position: state (present, empty, undecodable, not a stream, unreadable), other dictionary entries, decoded content, sharing between pages | compression; object numbers. The catalog's `/Metadata` stays in `metadata` |
| `associated_files` | every `/AF` owner — catalog, page, annotation, annotation appearance stream (states included), structure element, XObject and marked-content property list in a page's or an appearance stream's resources (forms followed, each once), document part — with its count and each file specification whole (names, description, `/AFRelationship`, every other key; `/EF` streams with `/Params` by decoded content); a file specification or embedded-file stream reached again named by its first owner and position | compression; object numbers; key order. `embedded_files` (unchanged) keeps its catalog `/AF` lines |
| `dparts` | the DPart tree by path: the root's entries, each node's `/Start` and `/End` pages, `/DPM`, number of children, whether `/Parent` names its parent, every other key; a node reached twice named by its first path, one on its own ancestry by the path it returns to; each page's `/DPart` as the node path it names | object numbers. A node's `/AF` is in `associated_files` |

The document-level rules apply: decoded content by length and digest,
an unsupported filter is `unsupported`, a value past the depth limit, a
line past 1 MiB, the decoded budget or the node budget are `error`, an
unreadable object is `#unresolved` (not null) and an undefined one is
null (§7.3.10). An XObject named outside a resource dictionary — by a
structure element's `/OBJR` or as a page's `/Thumb` — is an owner too
(named by the first owner when a page listed it first).
**Not covered** — a change there is not seen, so such a structure is
not claimed as preserved: `/AF` reached only through the resources of a
pattern, a Type 3 font, a soft mask or the AcroForm `/DR`, through a
widget that is in no page's `/Annots`, and pages that are not indirect
objects.

Synthetic tests (`semantic/tests_pdf20.rs`) on one base document with
all four (two pages, a DPart tree, `/AF` on eight kinds of owner,
shared file specifications and embedded files): renumbered, keys
reordered and recompressed pass; the version declared differently
passes with a note, lowered fails; page metadata changed, removed,
added, swapped, shared, its dictionary changed or unreadable fail in
`page_metadata` only; an embedded file's content, relationship,
description or parameters changed, an entry removed, added or
reordered, a file specification or embedded file unshared, an entry
changed on a structure element, a nested form, a property list, an
annotation, an appearance stream or a document part fail in
`associated_files` only; an XObject reached only through
`/OBJR` changed fails too; a part's pages, `/DPM`, parent, page link,
root entry, sharing, an added or removed node fail in `dparts` (a
moved node with an `/AF` also in `associated_files`); cycles end with
the path they return to; an over-deep tree, an over-deep value and an
over-long line are `error`; an unsupported filter is `unsupported`;
notes carry no content; the overall class is untouched; kasane's
rewrite of the base document passes every category as 1.7 and as 2.0.

Corpus (macOS and Linux arm64, identical by category), with `main`'s
library: version 432 pass and 2 fail — inputs declaring 1.4 that carry
`/OCProperties`, which the rewriter raises to 1.5 by design —, page
metadata 1 pass, associated files and document parts not applicable;
every other result unchanged. With the W2 rewriter: version 434 pass
and the same 2 fail, page metadata 1 pass, associated files 2 pass (the
two 2.0 documents), document parts none in the corpus.

## V4: strict actions, multimedia, signatures, transformations (v0.5.0, 2026-11-03)

Runner only; the library is unchanged. Design: `rewrite-policy.md`
§3.5, §5.7, §6.

**Strict action comparison.** Outline `/A` used an older `action()`:
a `/Next` chain cut at depth 8 (`…`, compared as equal past it),
scripts by raw bytes, a GoTo's `/SD` ignored. It now goes through the
document-level `chain()` of `doc_actions`:
- depth 32, a value cut at the value depth is `error`;
- scripts by decoded content;
- every key of the action, `/SD` included;
- action objects named by first-visit position in the category
  (`{#n …}`, a repeat `{=#n}`), so sharing is compared and a shared
  chain is expanded once. This applies to `doc_actions` too.

`outlines` is taken after `struct_tree`, so a structure destination
names its element by tree path (`elem#…`), as in `doc_actions`.
Section order is unchanged.

**`multimedia`** (report only): RichMedia, Screen, Movie, Sound and 3D
annotations by page and position (whole dictionary but `/AP` and
`/Popup`; streams — assets, embedded files, media — by decoded length
and digest), the catalog `/RichMedia`, and multimedia actions
(Rendition, Movie, Sound, RichMediaExecute) by owner and chain position.
A referenced value met again is named by its first owner and key
(`shared=`). An unreadable annotation or action is a line
(`unresolved`), never "absent"; an undecodable stream is `unsupported`.

**`signatures`** (report only), for D1:
- every *signature-like* dictionary in the graph reachable from the
  trailer's `/Root` and `/Info` (breadth first; `/Parent`, `/P` and a
  stream's `/Length` counted or skipped, not followed): `/ByteRange`;
  `/Type /Sig` or `/DocTimeStamp`; `/Contents` with any `/Filter` or
  `/SubFilter`; `/Reference` (array or dictionary) with
  `/TransformMethod`; `/Cert` with `/Contents`. Each by first path and
  number of references, with type, filter, sub-filter, `/ByteRange`
  length, `/Contents` *length*, each reference's transform method and
  parameters, and which of `/M /Name /Reason /Location /ContactInfo
  /Prop_Build /Cert /Changes` are present — never their values;
- the catalog `/Perms` (path, number of entries) and each entry's
  target path, `/AcroForm /SigFlags`, signature fields and fields with
  `/Lock` or `/SV` (path, `/V` kind and path; `/Lock` and `/SV` by keys
  and digest), `/DSS` (entry counts), `/Legal` (keys and digest), XFA
  packets naming the XML-DSig namespace;
- unreadable objects met by the walk (a count).

The walk keeps one node per value (parent and step), so memory is
linear in the graph; it shares the node budget, and a path past 1 MiB
is `error`.

**Declared transformations.** The comparison is unchanged and finds
every difference. Afterwards, a category's difference (as a multiset
of lines) is compared with what the run's declarations predict from the
*original* snapshot:
- no declaration for the category: the comparison stands;
- exactly the predicted lines removed and added: `transformed`, with
  the rule names;
- anything more, anything less (a declared change that did not happen)
  or a declaration that does not fit the original (`invalid
  declaration`): `fail`;
- `error` and `unsupported` stay as they are.

| rule | declared by | category | predicted change |
|---|---|---|---|
| T1 version raised | the runner, for every rewrite: `/OCProperties` present and an effective version below 1.5 | `version` | `effective version=1.x` → `1.5`; only to 1.5, only from the document's own version |
| T2 usage rights removed | the D1 configurations (`--rewrite-options remove-usage-rights`), from the runner's reading of `/Perms` and `/SigFlags` | `signatures` | the `/UR` / `/UR3` entries and their signature dictionary (which must be referenced once) go; `/Perms` goes when nothing else is left, otherwise its entry count drops; `/SigFlags` changes only by clearing a set bit 1 |
| | | `acroform` | the `form` line's `sig_flags` only, bit 1 cleared |
| | | graph (`structure`) | exactly `/Root/Perms` (or its removed entries) and, with bit 1, `/Root/AcroForm/SigFlags` — recorded as `structure_declared`, the check itself stays `fail` |

Each rule has the eight tests: the transformation, undeclared,
declared but absent, out of scope, a near miss (one unrelated change),
two or more extra changes, invalid declarations, an incomplete
comparison. T2 on the corpus: all 27 usage-rights forms (refused by the
default rewrite, so not compared in a run) yield a valid declaration
from their own snapshot — `/Perms` holds only `/UR3`, one signature
dictionary referenced once, `/SigFlags 2` (no bit-1 change) — and the
exact removal applied to their lines classifies `transformed`.

**Action inventory** (`kasane-compat inventory FILE`, `v4.rs`): every
action reachable from its owners — catalog `/OpenAction` (an array is a
destination), catalog, page, annotation and field `/AA` triggers,
annotation and outline `/A`, `/Names /JavaScript` — with class (script,
external, state, navigation, multimedia, unknown), kind, owner, chain
position, the first item it shares, and a script's length. An
unreadable or malformed action is an `unknown` item, never dropped.
`scripts/pdf-inventory.py` reads the same with pypdf. Corpus: the two
agree on 445 of the 447 files both read (15 283 actions: 8 120
navigation, 7 051 external, 112 script); the two others are malformed
inputs read differently (a stream without `/Length`, whose end-of-line
pypdf counts; an object pypdf reads and the library does not). The
D2 report will be checked against this inventory, not trusted.

**Tests** (`semantic/tests_v4.rs`, 18 tests; `configs.rs`, 3): equal, renumbered, keys
reordered and recompressed pass; content, settings, removal, addition,
reorder, kind and sharing changes fail; cycles end; undefined and
unreadable references differ; depth, node and decode budgets are
`error`; an unsupported filter is `unsupported`; notes and lines carry
no script text or signature value. 21 deliberate breakages of the
implementation (sharing ids, the strict outline cut, three signature
markers, reference counts, multimedia sharing, actions and coverage,
five transformation checks, the T2 guards, the inventory's sharing,
the graph allowance, the `/DSS` line, the unresolved count, the
evaluation order, the stream `/Length` skip) each make a test fail.

**Corpus** (macOS, nightly configuration; numbers in `v0.5.0.md` §19).
Every existing figure is unchanged; existing semantic classes are
unchanged (`outlines` lines changed in form — the strict chain's
`{#n …}` — for every file with outline actions, with the same class);
`version` moves its two known fails to `transformed` (T1);
`multimedia` and `signatures` are report only.

**Not covered:** scripts inside embedded files; RichMedia/3D stream
content beyond the digest; a signature with none of the markers (by
ISO 32000 not a PDF signature); `/#unresolved` as a literal name is
indistinguishable from an unreadable object (all categories).

## Roll-out

Reported in the run report (`roundtrip.semantic`), the baseline summary
(`semantic` per category) and the per-category tables; `check` lists
pass → other changes without failing. Enforce once a nightly baseline
carries the field. The document-level categories follow the same path:
the Linux nightly and a baseline regenerated from it come with the
merge of phase A; enforcement only after that.
