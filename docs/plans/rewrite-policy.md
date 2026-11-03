# Plan — rewrite policy: executable content (D2) and usage-rights removal (D1)

Status: **design**, v0.5.0 phase C. Nothing here is implemented yet.
The decision record is [`../decisions/rewrite-policy.md`](../decisions/rewrite-policy.md);
the roadmap is [`v0.5.0.md`](v0.5.0.md) (§4 D1 / D2, §18).

## 0. Principles

1. `rewrite`, `rewrite_with_limits` and every function built on them
   keep their behaviour and their output bytes.
2. The current refusals stay the default.
3. A new behaviour needs an explicit option; there is no "relaxed"
   default and no feature flag that changes defaults.
4. Whatever an option changes in the document is reported as typed
   data, not only as a log line or a warning.
5. *Carry* keeps content verbatim. It is not sanitizing, and it never
   executes anything.
6. *Remove* deletes usage-rights signatures. It never verifies a
   signature, never re-signs, and never removes any other signature.
7. DocMDP (certification), approval and timestamp signatures, XFA XML
   signatures and every signature-like structure that is not exactly
   a usage-rights signature keep the document refused.
8. Unknown or unverifiable structure is refused, never assumed safe.
9. The public API changes additively only (semver minor).
10. Semantic correctness and safety come before Full PASS: a rewrite
    the semantic round trip cannot verify is not counted.

## 1. What the rewriter does today (audit, `main` `ad76fda`)

`check_refusals` (`src/rewrite.rs`) refuses, before any output:

| structure | today |
|---|---|
| encrypted input | refused (`encrypted_write_refused`) |
| `/Perms` with `/DocMDP`, `/UR` or `/UR3` (key present) | `SignaturePolicy` (`RewriteOfSignedDocument`) |
| a field or page widget with `/FT /Sig` (inherited too) and a `/V` dictionary | `Unsupported` ("signed signature field") |
| an XFA packet with the XML-DSig namespace, or one that cannot be read completely | `SignaturePolicy` / `Unsupported` |
| catalog `/AA`, catalog `/RichMedia` | `Unsupported` |
| `/OpenAction` whose own `/S` is `/JavaScript` | `Unsupported` |
| `/Names /JavaScript` | `Unsupported` |

and **carries without any report**:
- page `/AA`, annotation `/A` and `/AA`, field `/AA`, outline `/A`;
- JavaScript reached through an action's `/Next` chain, including
  under a non-script `/OpenAction`;
- Launch, URI, GoToR, GoToE, SubmitForm, ImportData, ResetForm, Hide,
  SetOCGState, Rendition, Movie, Sound, Trans, GoTo3DView and
  RichMediaExecute actions anywhere below the catalog;
- page-level RichMedia, Screen, Movie, Sound and 3D annotations;
- XFA scripts (W3 carries XFA packets verbatim);
- signature-like dictionaries that are not under an `/FT /Sig` field
  (a `/ByteRange` in a field without `/FT`, a `/DocTimeStamp` reachable
  only from elsewhere), `/DSS` and `/Legal`.

So the default rewrite's executable-content profile is
*document-level entry points refused, everything else carried
silently*, and its signature profile is *the declared entry points
refused*. D2 and D1 must not change that default: `rewrite` and
`rewrite_with(default)` keep exactly today's bytes and errors. The new
API reports what the default carries. The signature gaps are closed
for `Remove` (§5.3: its walker runs only under that policy). Whether
the default should refuse stray signature-like dictionaries too is
open decision 7: it would change `rewrite`'s results, so it is not
part of this design.

## 2. Public API

### 2.1 Candidates

**A — the draft.** `rewrite_with(&Document, &RewriteOptions) ->
Result<RewriteOutput, Error>`, with plain public fields on
`RewriteOptions` and `RewriteOutput`, and exhaustive two-variant enums
(`Refuse`/`Carry`, `Refuse`/`Remove`).
- For: obvious, constructible with a struct literal.
- Against: adding a field or variant is a breaking change; struct
  literals lock users into today's set; "Refuse" misnames the default,
  which refuses only document-level entry points and carries the rest.

**B — options builder, report accessors (recommended).** The crate's
existing convention (`Limits`, `OpenOptions`, `RenderOptions`):
`#[non_exhaustive]` structs with `Default` and `with_*` methods,
`#[non_exhaustive]` enums, a result type that owns the bytes and
exposes the report through methods.

**C — a stateful `Rewriter` builder** (`Rewriter::new(&doc).carry_executable().run()`).
- For: fluent API.
- Against: a second style next to `Limits`/`OpenOptions`, a lifetime
  on the builder, and no gain over B.

**D — one function per policy** (`rewrite_carrying_scripts`,
`rewrite_removing_usage_rights`, …).
- For: maximally explicit.
- Against: combinations multiply (2 × 2 × limits × overlay), and the
  report types diverge.

B is the recommendation: it matches the crate, every addition is
semver-minor, and the explicitness the draft wanted is kept by the
named variants and by the report being part of the return type.

### 2.2 Recommended types

```rust
/// Rewrite `document` under `options`. With `RewriteOptions::default()`
/// the bytes are exactly `rewrite(document)`'s.
pub fn rewrite_with(document: &Document, options: &RewriteOptions)
    -> Result<RewriteOutput, Error>;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RewriteOptions {
    pub executable_content: ExecutableContentPolicy, // default: Profile
    pub multimedia: MultimediaPolicy,                // default: Refuse
    pub usage_rights: UsageRightsPolicy,             // default: Refuse
    pub limits: Limits,                              // default: Limits::default()
}
impl RewriteOptions {
    pub fn new() -> Self;
    pub fn with_executable_content(self, p: ExecutableContentPolicy) -> Self;
    pub fn with_multimedia(self, p: MultimediaPolicy) -> Self;
    pub fn with_usage_rights(self, p: UsageRightsPolicy) -> Self;
    pub fn with_limits(self, l: Limits) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ExecutableContentPolicy {
    /// The 0.4 profile, unchanged: document-level entry points (catalog
    /// /AA, a JavaScript /OpenAction, /Names /JavaScript) refused,
    /// everything else carried — and, unlike `rewrite`, reported.
    #[default]
    Profile,
    /// Refuse any script or external action anywhere (classes in §3.1);
    /// navigation and document-state actions are carried and reported.
    RefuseScriptsAndExternal,
    /// Carry every action and script verbatim, never executed, all
    /// reported. Not sanitizing: the output can run whatever the input
    /// could.
    Carry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum MultimediaPolicy {
    /// Catalog /RichMedia refused (as today); page-level multimedia
    /// annotations carried and reported (as today, now reported).
    #[default]
    Refuse,
    // `Carry` is not part of v0.5 (§3.4).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum UsageRightsPolicy {
    #[default]
    Refuse,
    /// Delete /Perms /UR and /UR3 usage-rights signatures when they are
    /// the only signatures (§5). Never verifies, never re-signs.
    Remove,
}

#[must_use]
#[derive(Debug)]
#[non_exhaustive]
pub struct RewriteOutput { /* bytes, report */ }
impl RewriteOutput {
    pub fn bytes(&self) -> &[u8];
    pub fn into_bytes(self) -> Vec<u8>;
    pub fn report(&self) -> &RewriteReport;
    pub fn into_parts(self) -> (Vec<u8>, RewriteReport);
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RewriteReport {
    pub options: RewriteOptionsSummary,           // the policies applied
    pub executable_content: Vec<ExecutableItem>,  // §3.3; every carried item, under every policy
    pub usage_rights: UsageRightsOutcome,         // §5.5
    pub version: VersionChange,                   // declared → written, with the reason
    pub warnings: Vec<RewriteNote>,               // typed, no document content
}
```

- **Relation to `rewrite`.** `rewrite(d)` stays and is not deprecated.
  Defined as `rewrite_with(d, &RewriteOptions::default())?.into_bytes()`,
  with a test of byte identity across the corpus.
  `rewrite_with_limits(d, l)` equals `rewrite_with(d, &default.with_limits(l))`.
  `Edit::rewrite` and the `remove_*` editors keep the defaults; they do
  not take the new options in v0.5.
- **`Default`.** Every default is today's behaviour. There is no
  `Default` that relaxes anything, and no constructor that picks
  `Carry` or `Remove` implicitly.
- **Enums.** `#[non_exhaustive]`, so `Strict` variants, per-class
  policies and `MultimediaPolicy::Carry` can be added later. A
  per-class policy struct (`ExecutablePolicy { scripts, external, … }`)
  was considered and deferred: three named profiles cover the use
  cases, and a struct can be added later as another variant.
- **Errors.** No new `Error` variant is needed: refusals are
  `Error::SignaturePolicy { rule, .. }` and `Error::Unsupported { feature, .. }`.
  New `SignatureRule` variants (non-exhaustive, minor):
  `UsageRightsNotRemovable` (a removal target is not a usage-rights
  signature, or is reachable elsewhere), `OtherSignaturePresent`
  (another signature-like structure), `SecurityStore` (`/DSS`, `/Legal`),
  and `ResidualSignature` (the output check found one, an internal
  failure that is never silent).
- **`RewriteOutput`.** It owns the bytes and the report; nothing else.
  It is `#[must_use]`, so a caller cannot drop the report unnoticed.
- **Other output forms.** A future `rewrite_to(&Document, &RewriteOptions,
  impl Write)` can return a report-only result. The `RewriteOutput`
  methods (`bytes()`, not a public field) leave room for a
  writer-backed variant without breaking callers.
- **Memory.** Same as today (one output `Vec`). The report holds
  counts, locations and object numbers, never content.
- **semver.** `cargo semver-checks`: additions only, so minor. No
  feature flag: the defaults change nothing, and gating a type behind
  a feature would complicate docs.rs and the API without making
  anything safer.

### 2.3 Reports carry no document content

The report types derive `Debug` but hold no script text, no strings
from the document, no field names, no name-tree keys and no digests:
- locations are positional (page index, annotation index, field index
  path, name-tree entry index);
- object numbers are input and output numbers;
- sizes are byte lengths.

A digest was rejected: it lets anyone with a candidate script confirm
it was present, and it is computable from the output by a caller who
needs it. `Display` impls print counts only. Field names are omitted
because they are often personal data; a caller who needs them looks
them up through the output object number.

## 3. D2 — executable content

### 3.1 Classification

Actions are classified by effect, not by "is it JavaScript":

| class | members (ISO 32000-2 §12.6.4, §12.6.3, §13) | effect when the file is viewed |
|---|---|---|
| **Script** | `/S /JavaScript` actions anywhere; `/Names /JavaScript`; field `/AA` `/K` `/F` `/V` `/C` scripts; XFA script content (below) | runs code in the viewer |
| **External** | `Launch`, `URI`, `GoToR`, `GoToE`, `SubmitForm`, `ImportData`; `/URI` base | opens files or applications, network access, sends form data |
| **State** | `ResetForm`, `Hide`, `SetOCGState`, `Trans`, `Named` (other than page navigation) | changes the document's state in the viewer |
| **Navigation** | `GoTo`, `GoToDp` (PDF 2.0), `Thread`, `Named` `NextPage`/`PrevPage`/`FirstPage`/`LastPage`, `GoTo3DView` | moves the view |
| **Multimedia** | `Rendition`, `Movie`, `Sound`, `RichMediaExecute`; annotations `/RichMedia`, `/Screen`, `/Movie`, `/Sound`, `/3D`; catalog `/RichMedia` | plays media, may embed active content (Flash, 3D JavaScript) |
| **Unknown** | an `/S` not in the tables of ISO 32000-2, a missing `/S` | refused under `RefuseScriptsAndExternal` and `Carry`; carried and reported under `Profile` (as today) |

XFA scripts are detected on the text that W3's signature check already
produces: packets decoded, read completely (UTF-8/UTF-16, no DTD,
declarations checked), and character references expanded. A script is
an element whose local name is `script`, under any prefix: `<`, an
optional `NCName:`, `script`, then white space, `>` or `/`. The packets
are checked together, as one document. A packet that cannot be read
completely is refused, as in W3. A match that cannot be classified
(a `script` local name the rule cannot delimit) refuses under every
policy, so no carried script can be missing from the report. A FormCalc
`<script contentType=…>` is a script too.

Owners (where an action or trigger lives):
- catalog `/OpenAction` (a dictionary, or an array of action
  dictionaries, which is non-standard but seen: each member reported),
  catalog `/AA` (document triggers), catalog `/URI` (a base URI,
  External);
- page `/AA`;
- annotation `/A`, annotation `/AA`;
- field `/AA` (including non-widget field nodes);
- outline item `/A`;
- `/Names /JavaScript` entries;
- the `/Next` chain of any action (single or array);
- XFA packets (scripts);
- multimedia annotations and the catalog `/RichMedia`.

Not owners: `/Threads` and `/PresSteps` hold beads and navigation nodes
(destinations), not actions. `/AA` on a form XObject is not a PDF
structure (Table 95 has none); an `/AA` key there is carried like any
unknown key and not classified.

### 3.2 Policies

| class | `Profile` (default) | `RefuseScriptsAndExternal` | `Carry` |
|---|---|---|---|
| Script at document level (catalog `/AA`, JavaScript `/OpenAction`, `/Names /JavaScript`) | refuse | refuse | carry |
| Script below document level, or through `/Next` | carry, report | refuse | carry |
| External | carry, report | refuse | carry |
| State, Navigation | carry, report | carry, report | carry |
| Multimedia actions (Rendition, Movie, Sound, RichMediaExecute) and page-level multimedia annotations | carry, report | carry, report | carry, report |
| catalog `/RichMedia` | `MultimediaPolicy` (`Refuse`) | `MultimediaPolicy` | `MultimediaPolicy` |
| Unknown | carry, report | refuse | refuse |
| XFA scripts | carry, report (W3) | refuse | carry |

The defaults:
- `Profile` refuses exactly what `rewrite` refuses today, so
  `rewrite_with(default)` gives the same bytes and the same errors.
- `Carry` never opens multimedia. `/RichMedia` stays a separate,
  undecided question (§3.4).
- A catalog `/AA` that holds only navigation actions is still refused
  by `Profile`, as today: the default must not change.

*Carry* is the identity on the action subgraph:
- no key is added, removed or changed. Keys are written in sorted
  order, as every rewrite does (dictionaries are ordered maps), which
  is not a semantic difference;
- script strings and streams are copied byte-for-byte (the collector
  already does this);
- `/Next` order and sharing are kept, references renumbered.

Undefined references in an action graph are null (§7.3.10, W1).
Unreadable ones refuse, as in every rewrite.

### 3.3 Report — `ExecutableItem`

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecutableItem {
    pub class: ActionClass,           // Script | External | State | Navigation | Multimedia | Unknown
    pub kind: ActionKind,             // the /S (or annotation subtype) as an enum, Other for unknown
    pub owner: ActionOwner,           // Catalog{OpenAction{member: Option<u32>}|AA(trigger)|Uri} | Page{index, trigger} | Annotation{page, index, A|AA(trigger)}
                                      // | Field{path: Vec<u32>, trigger} | Outline{path: Vec<u32>} | NameTree{index} | Xfa{packet}
    pub chain_position: u32,          // 0 for the owner's action, n for the n-th /Next step (depth-first)
    pub input_object: Option<IndirectRef>,
    pub output_object: Option<IndirectRef>,
    pub script_bytes: Option<u64>,    // length of a script string or decoded stream; never the text
    pub shared_with: Option<usize>,   // index of the first item with the same action object
    // No disposition field: a report exists only for a rewrite that
    // succeeded, so every listed item was carried; a refusal is an Err.
}
```

- Counts per class come from `RewriteReport::executable_summary()`.
- Items are listed for every policy, so a caller of the default
  finally sees what is being carried.
- A refusal names the class, kind and owner in `Error::Unsupported`'s
  `detail`. The detail stays content-free.
- Bounded without a new limit: an item is one action object or one
  owner entry, so the report is at most a small constant times the
  number of written objects. That number is already bounded by
  `max_output_objects` and `max_rewrite_walk_depth`, which every
  rewrite enforces. A separate report cap would make `rewrite_with(default)`
  fail where `rewrite` succeeds, so there is none. The report is never
  truncated.

### 3.4 `/RichMedia` and multimedia

Not lifted in v0.5. RichMedia (ISO 32000-2 §13.7) embeds active
content (Flash SWF, 3D JavaScript) whose execution model is outside
PDF actions; carrying it is a different risk statement from carrying a
JavaScript action. `MultimediaPolicy` exists now so that
`ExecutableContentPolicy::Carry` cannot be read as "everything active".
A `Carry` variant needs its own design, tests and decision. The corpus
has no document blocked only by it (§7).

### 3.5 Verification — semantic round trip

D2 counts a document only when the semantic round trip passes in
`doc_actions` and `javascript` (and `xfa` when present). Both
categories exist and are enforced-path categories. Additions required
before D2 is measured (V4):
1. **Outline actions in strict mode.** `outlines` compares `/A`
   through the older `action()`: it cuts a `/Next` chain at depth 8
   (`…`) and hashes stream scripts by raw bytes. Move outline actions
   to `doc_actions`' strict `chain()` (an `outline_action` line with
   the outline path), or make `outlines` strict. Without this an
   outline script past depth 8 is not verified.
2. **Multimedia annotations.** A report-only `multimedia` category:
   subtype, `/RichMediaContent`/rendition dictionaries by canonical
   value, streams by decoded digest, by page and position. Needed
   because `Carry` reports them, even though v0.5 does not lift the
   catalog refusal.
3. **The report against the snapshot.** The runner derives its own
   action inventory from the snapshot lines (class by `/S`) and
   compares it with `RewriteReport::executable_content` (counts per
   class and owner). A disagreement is `error`. Both sides use the
   library's parser, so the corpus acceptance adds a pypdf inventory
   of actions for the D2 gains.

Already verified by the existing categories:
- action type and every key;
- `/Next` order and branches;
- name-tree keys and scripts (decoded, string or stream);
- the owner by page, annotation position or field name;
- sharing (named by position);
- cycles (`<cycle@k>`);
- renumbering and compression.

Not verified, and so not claimed: scripts inside embedded files (a
nested PDF's own actions), and content of RichMedia/3D streams beyond
the digest.

## 4. D2 — what changes for the default

Nothing in bytes or errors. The new API's report lists what `rewrite`
has always carried. `docs/limitations.md` gets the table of §1
(already true today) so the default's behaviour is stated, not implied.

## 5. D1 — usage-rights removal

### 5.1 What a usage-rights signature is

Catalog `/Perms` (ISO 32000-2 §12.8.6, Table 263) maps `/DocMDP` to a
certification signature and `/UR3` (and the PDF 1.5 `/UR`) to a
*usage-rights* signature dictionary.
- Its `/Reference` array holds a signature-reference dictionary with
  `/TransformMethod /UR3` (§12.8.2.3, Table 257) and `/TransformParams`
  listing the rights it grants.
- It is not in `/AcroForm /Fields`: it is not a field signature.
- It covers the whole file through `/ByteRange` and `/Contents`, so a
  rewrite invalidates it, and a viewer that checks it then withdraws the
  rights it granted.

Removing it keeps the document's content and loses those extended
rights. It is never "safe" to keep it: the 0.4 rewriter refuses for
that reason.

### 5.2 What is removed (semantics) and what remains (bytes)

**Removed:** the `/UR` and `/UR3` entries of `/Perms`, and `/Perms`
itself when nothing else is left in it. Through the rewrite's
reachability, the signature dictionaries they named are then not
written.

`/AcroForm /SigFlags` stays consistent with what remains (the D1
acceptance in v0.5.0.md §4). Bit 1 (SignaturesExist) is cleared when it
is set, because after the removal no signature remains (the walker has
established that). Bit 2 (AppendOnly) is carried and reported.
Clearing bit 1 is part of the declared D1 change (§5.7). Nothing else
is modified: `/AcroForm`, form fields and every other object are
carried as they are.

**Remains:** the output must contain no usage-rights signature. That
is checked on the *output* independently (§5.4), not inferred from the
input edit.

### 5.3 Refusals (every one of these refuses under `Remove` too)

D1 refuses unless exactly the following holds; anything it cannot
establish refuses.

1. **Removal targets.** Each of `/Perms /UR`, `/Perms /UR3` is a
   signature dictionary:
   - `/Type /Sig` or absent;
   - `/Filter` and `/Contents` present;
   - `/Reference` a non-empty array whose every element has an explicit
     `/TransformMethod /UR3` (for `/Perms /UR3`) or `/UR` or `/UR3`
     (for `/Perms /UR`: `/UR` is the PDF 1.5 name, and 1.6+ writers use
     `/UR3` under either key);
   - an element without `/TransformMethod` means `/DocMDP` (§12.8.2.3,
     Table 257) and refuses; so do an absent, empty or non-array
     `/Reference` (a usage-rights claim cannot be established) and any
     `/DocMDP` or `/FieldMDP` element.

   Anything else (`UsageRightsNotRemovable`) refuses.
2. **No other signature.** A walk over the input's reachable graph,
   from the trailer `/Root` and `/Info` and every page, through every
   reference with `deref_chain` (compressed reference values included),
   bounded by the limits, finds no *signature-like* dictionary other
   than the removal targets. Signature-like means any of:
   - `/ByteRange` present;
   - `/Type /Sig` or `/Type /DocTimeStamp`;
   - `/Contents` together with `/Filter` or `/SubFilter`, whatever
     their values. Handler names are open-ended (vendor handlers,
     private sub-filters), so no name list decides. A stream has
     `/Filter` but never `/Contents`; an annotation with `/Contents`
     and a stray `/Filter` refuses, which is conservative;
   - a `/Reference` containing `/TransformMethod`;
   - `/Cert` together with `/Contents`.

   `/FT /Sig` is not required. A field without `/FT` holding a
   `/ByteRange`, a timestamp reachable only from an annotation, and
   `/Perms /DocMDP` all refuse (`OtherSignaturePresent`). A walk that
   hits a limit or an unreadable object refuses; it is never cut short.
3. **Removal targets only under `/Perms`.** If a target is reachable
   from anywhere else (a field `/V`, another dictionary) or shared,
   refuse (`UsageRightsNotRemovable`).
4. **Security stores.** `/DSS` or `/Legal` in the catalog refuses
   (`SecurityStore`, §5.6).
5. **Field signatures, field locks.** `/FT /Sig` fields with `/V`
   refuse (as today). A field's own `/Lock` entry (Table 233) refuses,
   on a signature field or on any other field (conservative). `/SV`
   seed values on an unsigned field, including a `/Lock` nested in the
   seed-value dictionary, are open decision 2: recommended carry and
   report. `/FieldMDP`
   is a transform method in a signature's `/Reference`, covered by
   rule 1 and by the marker walk.
5a. **Shared edited dictionaries.** Every dictionary D1 edits must be
   reachable only from the catalog: `/Perms`, and `/AcroForm` when a
   `/SigFlags` edit is needed. Otherwise refuse: editing it would
   change the other referrer's view.
6. **XFA XML signatures** refuse (as today, W3).
7. **Incremental history.** A document with earlier revisions is read
   at its latest state (as every rewrite). A usage-rights or other
   signature in a superseded revision is not reachable and not
   written. The input check runs on the latest state, and the output
   check (§5.4) is the guarantee. The report states the number of
   input revisions.
8. **Generations.** A target or signature reached at another
   generation follows §7.3.10 (W1): an undefined object is null and
   not a signature. An unreadable one refuses.

### 5.4 Independent check of the output (under `Remove`)

It runs under `UsageRightsPolicy::Remove` only, for both outcomes
(`Removed` and `NonePresent`: it is cheap and also guards a collector
bug when nothing was removed). Under `Refuse` the output may carry the
signature-like leftovers that today's default carries (open decision
7), so the check would break the default's identity there.

After the bytes are produced and before they are returned:
1. Reparse the output strictly.
2. Walk its whole reachable graph with the residual-audit walker
   (`audit_residuals`' BFS, its own reader path, completeness flag).
   The walk must be complete; an incomplete one refuses.
3. Require:
   - no signature-like dictionary (§5.3.2) anywhere;
   - no `/ByteRange` key anywhere;
   - no `/Perms` `/UR`, `/UR3` or `/DocMDP`;
   - the same number of pages;
   - every other catalog key present.
4. Any failure is `SignaturePolicy { rule: ResidualSignature }`, and
   no bytes are returned.

The check is on the bytes the caller receives: a separate walker over a
fresh parse, so a collector bug cannot hide a residual signature. It
shares the library's parser, so a parser blind spot could hide a
marker from both the input and the output check. Third-party readers
compensate in the acceptance (§10): for every D1 gain, the runner
re-reads the output with qpdf (`--check`, and a JSON object dump
searched for the full §5.3.2 marker set: `/ByteRange`; `/Type /Sig`
or `/DocTimeStamp`; `/Contents` together with `/Filter` or `/SubFilter`
in one dictionary; `/Reference` with `/TransformMethod`; `/Cert` with
`/Contents`). The corpus acceptance does the same with pypdf.

### 5.5 Report — `UsageRightsOutcome`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum UsageRightsOutcome {
    /// No usage-rights signature in the input.
    NonePresent,
    /// Removed — the output carries no usage-rights signature and the
    /// rights it granted are withdrawn.
    Removed(UsageRightsRemoval),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UsageRightsRemoval {
    pub entries: Vec<RemovedSignature>,    // one per /UR, /UR3
    pub perms_removed: bool,               // /Perms dropped (it held nothing else)
    pub sig_flags: Option<SigFlagsChange>, // before / after: bit 1 cleared when set, bit 2 kept
    pub input_revisions: u32,
    pub output_check: OutputCheck,         // what §5.4 verified (counts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SigFlagsChange {
    pub before: i64,                       // /AcroForm /SigFlags as read
    pub after: i64,                        // as written: bit 1 cleared, every other bit kept
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemovedSignature {
    pub key: UsageRightsKey,               // UR | UR3
    pub input_object: Option<IndirectRef>,
    pub sub_filter: Option<SubFilterKind>, // enum (adbe.pkcs7.detached, …, Other); no strings
    pub rights: Vec<UsageRight>,           // from /TransformParams: Document, Annots, Form, Signature, EF, FormEx (enum, Other)
    pub byte_range_present: bool,
}
```

- `RewriteReport::usage_rights` is not optional. With `Remove` and a
  usage-rights signature present, the outcome is
  `Removed(..)`: the type, not a log line, says that the signature was
  deleted and the rights withdrawn.
- `RewriteReport::signatures_removed() -> bool` is a convenience
  accessor.
- With `Refuse`, a usage-rights signature is an error (as today), so
  `Removed` never appears without the option.

### 5.6 `/DSS` and `/Legal`

`/DSS` (Document Security Store, ISO 32000-2 §12.8.4.3) holds
validation material for the document's signatures:
- certificates, CRLs and OCSP responses;
- `/VRI` entries keyed by the SHA-1 of a signature's `/Contents`.

`/Legal` (§12.8.7) is the legal-content attestation that accompanies
a certification signature.

| option | safety | compatibility | complexity |
|---|---|---|---|
| 1. `/DSS` or `/Legal` present → refuse | highest. Their presence means a signature workflow (PAdES LTV, certification) the removal cannot reason about | refuses files that only carry stale validation data | trivial |
| 2. relate `/DSS` to the usage-rights signature (match `/VRI` keys to the target's `/Contents` hash, check no other signature) and drop the matched entries | correct only if every `/VRI` key and the certificate pools are matched exactly; a partial match leaves orphan material or removes another signature's evidence | handles LTV-extended usage-rights files | high: hashing, partial edits of `/DSS` arrays, more refusal paths |
| 3. a separate explicit option (`SecurityStorePolicy::Carry`) to carry `/DSS` unchanged after removal | the caller accepts carrying validation data for a removed signature | covers the case on request | moderate |

**Recommendation: option 1 for v0.5.** No corpus document carries
`/DSS` or `/Legal` with only a usage-rights signature (§7), so the
cost is nil. Option 3 is the extension point if a real need appears.
Option 2 is rejected: it edits signature evidence, which this design
must never do.

### 5.7 Verification — semantic round trip with declared transformations

D1 changes the document on purpose. The comparison must:
1. accept exactly the declared change;
2. fail on any other change.

This is §6's mechanism, with a new snapshot category:
- **`signatures`** (enforced-path, after its report-only period): one
  line for each of
  - catalog `/Perms` entry (key, kind by `/TransformMethod`, sub-filter,
    granted rights);
  - signature field (field path, `/V` kind);
  - signature-like dictionary anywhere (§5.3.2 rule, owner path);
  - `/DSS`, `/Legal`, `/SigFlags`.

  It has no values, only kinds and positions.
- **The D1 transformation rule:**
  - only `perms` lines with kind `UR` or `UR3` may disappear;
  - nothing in any category may appear;
  - nothing else may disappear or change;
  - the `structure` (graph) comparison may report exactly `/Root/Perms`
    dropped, or `/Root/Perms/UR3` or `/Root/Perms/UR` dropped, and —
    when bit 1 was set — `/Root/AcroForm/SigFlags` changed with only
    bit 1 cleared; nothing else;
  - in `acroform`, only the `sig_flags` line may change, and only by
    bit 1 being cleared;
  - in `signatures`, only the `/SigFlags` line may change, and only by
    bit 1 being cleared (near-miss test: bit 1 cleared while any other
    `signatures` line appears or disappears must `fail`);
  - the runner checks its own inventory against
    `RewriteReport::usage_rights`.

## 6. Semantic transformation model

Today a category is `pass` (snapshots equal) or `fail`. W2 already
found an intended change reported as `fail` (the version raised to
1.5 for optional content).

**Design:**
1. **Snapshots are unchanged.** The comparison still finds every
   difference.
2. **Transformation rules** live in the runner. They form a table in
   code (`semantic/transform.rs`), each rule with:
   - `name`;
   - the option or writer behaviour that produces it;
   - the category;
   - a predicate over (original snapshot, rewrite snapshot, input
     facts the runner reads itself, the library's report): which lines
     may disappear, appear or change, and how;
   - a precondition on the input.
3. **Activation is declared.** A rule applies only when the run
   declares it:
   - the runner's rewrite mode (`--rewrite-options carry,remove-usage-rights`)
     declares D1 and D2 rules;
   - writer rules such as the version floor are declared by the
     library's report (`VersionChange::Raised { reason: OptionalContent }`)
     and confirmed by the runner's own reading of the input (it has
     `/OCProperties`, declares < 1.5).
4. **Classification per category:**
   - `pass`: equal;
   - `transformed`: every difference is explained by an active rule,
     and every active rule's expected change happened (a declared
     removal that did not happen is a failure too);
   - `fail`: anything else.

   `transformed` names the rules. It is reported, never counted as
   `pass`.
5. **No D1-only escape.**
   - a rule is generic (category + predicate), so D1 adds one rule,
     not a code path;
   - each rule needs synthetic tests: it applies; it does not apply
     (precondition false); and the near miss (the same removal plus
     one unrelated change) must still `fail`;
   - adding a rule is a reviewed change, not configuration.
6. **Full PASS.**
   - The default Full PASS (enforced) counts only `pass`, as today.
   - Option runs report `transformed` separately and count a document
     as an option gain only when its categories are `pass` or
     `transformed` by that option's rules.
7. **Baseline compatibility.**
   - `transformed` is a new class string in `semantic` /
     `semantic_report` maps.
   - `ci-regression.py` treats it like `pass` for "lost pass" checks
     only when the rule set is unchanged, and lists any new
     `transformed` for review.
   - Old baselines have no `transformed` and read as before.
   - The default run's figures do not change unless a writer rule
     (the version floor) is declared. That rule moves the two known
     `version` fails to `transformed`, and the README records it.
8. **Report only and enforced.** A rule's category keeps its status.
   The version rule applies to the report-only `version` category, the
   D1 rule to `signatures` (report only until promoted).
   Promotion conditions (v0.5.0.md §16): rule tests, no unexplained
   `fail`, `error` or `unsupported` on the corpus, consecutive clean
   nightlies, an independent review.

## 7. Corpus — measured on a spike

A temporary build of `main` (never committed) lifted the refusals
behind environment switches and ran the full corpus (macOS, nightly
configuration):
- D2: catalog `/AA`, a JavaScript `/OpenAction` and `/Names /JavaScript`
  carried;
- D1: `/Perms /UR` and `/UR3` dropped by a catalog overlay, `DocMDP`
  still refused, `/RichMedia` still refused.

Base: the default rewrite, 345 / 425, measured on Linux x86_64 and
macOS. The spike runs are measured on macOS.

| evaluation | Full PASS | gain | round trip and semantic of the gains | status |
|---|---:|---:|---|---|
| 1. default rewrite | 345 | — | — | measured (baseline) |
| 2. D2 `Carry` only | 351 | +6 | 6 / 6 full round trip, semantic pass (`doc_actions`, `javascript`) | measured on the spike; to confirm with the implementation |
| 3. D1 `Remove` only | 345 | **+0** | the 27 usage-rights forms are no longer refused for the signature, but every one then stops at its `/Names /JavaScript` tree | measured on the spike |
| 4. D1 + D2 | **378** | +33 | the 6 of row 2, plus the 27 forms: every existing semantic category passes, and the graph comparison flags exactly `/Root/Perms` dropped (the declared change) | measured on the spike. The 27 count only once `signatures` and the D1 rule (§5.7, V4) classify them `transformed` |

- D2's six gains are the documents blocked only by a `/Names /JavaScript`
  tree (4) or a catalog `/AA` (2).
- Two further documents rewrite under D2 but stay short of Full PASS
  (render unsupported; a recovered parse).
- The gains do not add: D1's 27 need D2, and D2 is a subset of D1 + D2.

The 27 usage-rights forms, read independently with pypdf:
- `/Perms` holds only `/UR3`, with `/TransformMethod /UR3`;
- exactly one signature-like dictionary (the `/UR3` dictionary itself:
  one `/ByteRange`, one `/Type /Sig`), referenced once, only from
  `/Perms`;
- no `/Sig` fields;
- `/AcroForm /SigFlags 2` (AppendOnly);
- no `/DSS` or `/Legal` anywhere in the corpus.

So the rules of §5.3 remove all 27.

What remains after D1 + D2 (`expect: pass`, not Full PASS):
- 17 fail every stage (parse);
- 16 fail render and text, and 12 render only;
- 2 encrypted documents are refused by every writer.

None of these is a rewrite-policy question.

**Target ≥ 365 / 425:** reachable with both options (378 measured on
the spike), and only once V4 lets the semantic round trip verify the
removal. With the default rewrite alone the ceiling stays 345 plus
whatever non-policy work adds. Planned figures:

| runner report | figure | status |
|---|---|---|
| default rewrite | 345 | measured, enforced baseline |
| with `Carry` | 351 | spike, to measure with C1 |
| with `Remove` | 345 | spike |
| with both | 378 (351 verified + 27 needing the D1 rule) | spike, to measure with C2 |

**Runner design for the four figures:**
- `kasane-compat run --rewrite-options <set>` runs the round trip
  under `rewrite_with`, with that option set;
- the nightly runs the default and `carry,remove-usage-rights` (two
  passes, about 25 minutes);
- the summary records `full_pass_by_options` per set, and per file
  the set under which it is Full PASS;
- `check` enforces only the default set. The others are report only
  until they have their own baseline.

## 8. Threat model

**Assets:**
- the user's belief about what the output contains: no signature it
  does not claim, every script the input had, and nothing executed;
- the integrity of the signature policy (never invalidate a
  signature silently);
- the host process (CPU, memory, stack).

**Adversary:**
- supplies the input PDF: malicious, malformed or crafted to evade a
  check;
- may know the policy;
- cannot change the library or the caller's options.

| threat | where | defence | residual risk |
|---|---|---|---|
| signature not seen (missed refusal) | D1 | signature-like rule over the whole reachable graph (not `/FT /Sig`, no handler-name list); `deref_chain` for compressed values; fail closed on limits, unreadable objects, incomplete walks; output check on a fresh parse; third-party re-read in the acceptance | a signature with none of the structural markers (`/ByteRange`, `/Type /Sig` or `/DocTimeStamp`, `/Contents` with `/Filter` or `/SubFilter`, `/Reference` with `/TransformMethod`, `/Cert` with `/Contents`) is not a PDF signature by ISO 32000 |
| signature removed that was not a usage-rights one | D1 | targets must carry `/TransformMethod /UR*` only, and be reachable only from `/Perms`; DocMDP, `/FieldMDP`, `/DSS`, `/Legal` refuse | none known |
| the user does not notice removal | D1 | `UsageRightsOutcome::Removed` in the return type, `#[must_use]` output, option name | a caller ignoring the report entirely |
| a script carried unexpectedly | D2 | `Carry` only by explicit option; `Profile` reports what it carries; `RefuseScriptsAndExternal` for callers who need none | `Profile` carries below-catalog scripts as 0.4 did (documented, reported) |
| an action lost or altered | D2 | the collector copies verbatim; enforced `doc_actions` / `javascript` comparison; outline actions moved to strict comparison (V4) | actions inside embedded documents (not inspected, documented) |
| the report incomplete | D1, D2 | the runner's independent inventory against the report; a report limit refuses rather than truncates | — |
| reference confusion (shared or cyclic actions, generations) | D1, D2 | identity by object, sharing named by first position, cycles named, §7.3.10 for undefined, refusal for unreadable | — |
| semantic snapshot misses a change | both | categories by decoded content; rule near-miss tests; `fuzz` oracle on graph equality | structures listed as not covered (semantic-roundtrip.md) |
| CPU or memory DoS (huge scripts, many actions, deep `/Next`, huge XFA) | both | existing limits (`max_stream_bytes`, decode budget, `max_rewrite_walk_depth`, output limits); the report bounded by `max_output_objects` (no limit of its own); the residual walk shares the audit's bounded BFS | — |
| stack overflow | both | every walk iterative or depth-bounded (512 rewrite walk, 32 action chain in the snapshot) | — |
| signatures inside embedded files (a signed PDF in an `/EF` stream) | D1 | carried byte for byte, so the embedded signature's own byte range is untouched; not inspected (documented) | none for integrity; it is not reported |
| a `/Perms` dictionary shared with another owner | D1 | refuse (§5.3 rule 5a) | — |
| information exposure through reports or errors | both | reports and errors positional and content-free (§2.3) | object numbers point to content in the output; the output is the caller's anyway |

**Safety boundaries:**
- `Carry` never executes anything, and it does not sanitize.
- `Remove` neither verifies nor re-signs. It says it removed.
- Unknown structures refuse.

## 9. Tests and fuzzing

The synthetic documents are generated in tests, as for W1–W3. Each row
is one test, run under every policy where the outcome differs.

**D2:**

| input structure | policy | expected | report | semantic |
|---|---|---|---|---|
| `/Names /JavaScript` (2 entries, string and Flate stream) | Profile / Carry | refuse / bytes carried | 2 × Script, NameTree{0,1}, lengths | `javascript` pass |
| JavaScript `/OpenAction` with `/Next` [URI, GoTo] | Profile / RefuseSE / Carry | refuse / refuse / carry | Script(0), External(1), Navigation(2) | `doc_actions` pass |
| page, annotation, field `/AA` (each trigger) | each | carry (Profile, Carry), refuse (RefuseSE) | owner + trigger | pass |
| outline `/A` JavaScript with a 12-step `/Next` | Carry | carry | 13 items | pass only after V4; before, a known gap test that fails |
| shared action (one object in two `/AA`) | Carry | carry, one object | `shared_with` | pass |
| `/Next` cycle | Carry | carry | cycle positions | pass (`<cycle@k>`) |
| action behind a compressed reference value | each | as the uncompressed case | same | pass |
| undefined action reference | each | null kept (W1) | none for it | pass |
| unreadable action object | each | refuse | — | — |
| `/S` unknown, `/S` missing | each | carry (Profile), refuse (RefuseSE, Carry) | Unknown | — |
| 1 MiB script, 100 000 actions | Carry | carry within time | lengths | — |
| catalog `/RichMedia` | Carry | refuse (MultimediaPolicy) | — | — |
| `GoToDp` action | Carry | carry (Navigation) | Navigation | pass |
| `/OpenAction` array holding a JavaScript action | Profile / Carry | carry (as today: the top value is not a dictionary) / carry | each member reported | pass |
| XFA template with `<x:script xmlns:x=…>` and `<script contentType="application/x-formcalc">` | RefuseSE / Carry | refuse / carry | 2 × Script, Xfa | `xfa` pass |
| 200 000 actions | Profile | as `rewrite` (no report cap) | 200 000 items | — |
| default options vs `rewrite` | default | identical bytes and errors (every case above, and the corpus) | — | — |

**D1:**

| input structure | expected under `Remove` |
|---|---|
| `/Perms /UR3` (usage rights only), direct and indirect | removed; outcome `Removed`, rights listed; output check passes; `signatures` `transformed`; qpdf `--check` exit 0; Poppler renders equal |
| `/Perms /UR` (1.5) | as above |
| `/Perms /DocMDP` (with or without `/UR3`) | refuse `OtherSignaturePresent` |
| approval `/FT /Sig` field with `/V` | refuse |
| `/DocTimeStamp` field | refuse |
| `/ByteRange` dictionary under a field without `/FT` | refuse (the open P2 from W1) |
| signature-like dictionary only on an annotation, behind a compressed reference | refuse |
| `/UR3` target also a field `/V` | refuse `UsageRightsNotRemovable` |
| `/UR3` whose `/Reference` says `/DocMDP` | refuse |
| `/DSS` / `/Legal` present | refuse `SecurityStore` |
| XFA XML signature with `/UR3` | refuse |
| incremental update adding `/UR3` to a signed revision | refuse if the signature is reachable at the latest state; otherwise removed, `input_revisions` 2 |
| hybrid-reference file whose `/XRefStm` hides a `/UR3` | the latest state decides (X1); output check |
| undefined `/UR3` reference | `NonePresent` (null) |
| unreadable `/UR3` object | refuse |
| cycle through `/Reference` | refuse or remove per the rules, no loop |
| `/Perms` holding an unknown key besides `/UR3` | removed, `/Perms` kept with the unknown key; `perms_removed false` |
| a planted collector bug (test-only hook) leaving the target reachable | the output check refuses `ResidualSignature` |
| `/UR3` and a dictionary with `/Contents` and `/SubFilter /VendorCorp.SignV2` (an unlisted name) on an annotation | refuse `OtherSignaturePresent` |
| `/UR3` whose `/Reference` element has no `/TransformMethod` (means DocMDP); absent, empty or non-array `/Reference` | refuse `UsageRightsNotRemovable` |
| `/SigFlags 3` (bit 1 set) with `/UR3` | removed; `/SigFlags 2`; `sig_flags` 3 → 2 in the report; `acroform` `transformed` |
| `/Perms` dictionary object also referenced from another catalog key | refuse |
| a signed PDF as an embedded file next to `/UR3` | removed; the embedded file carried byte-identical (its own signature is untouched; not inspected) |

Every D1 test reparses the output and walks it for signature markers
with the test's own walker (not the library's).

**Fuzzing:**
- `rewrite_graph` grows two bits, so each policy set has its own
  oracle:
  - bit 6: options `Carry` (the oracle checks that the action
    subgraph is equal and that the report count equals its own count);
  - bit 2 reused with `Remove`: generates `/Perms` with `/UR3` and,
    by input byte, a second signature-like dictionary in a random
    place;
  - the oracle: `Removed` exactly when no other signature-like
    dictionary exists, and the output walk has no marker.
- A new target `signature_markers` mutates signature-like dictionaries
  (marker subsets, `/Reference` forms, compressed placements) and
  requires that D1 refuse unless the only markers are valid
  `/Perms /UR*` targets.
- 20 min per target before each merge, plus the existing targets.

## 10. Phases and acceptance

| phase | content | depends on | acceptance | estimate |
|---|---|---|---|---|
| V4 (runner) | outline actions strict; `multimedia` report-only category; `signatures` report-only category; the transformation model with the version rule; option runs in the runner (`--rewrite-options`) | — | synthetic tests per category (equal, changed, missing, near miss); corpus with the current library: existing categories unchanged, new ones explained; version rule turns exactly the two known `version` fails into `transformed` | 3–4 d |
| C1 D2 | `rewrite_with`, options, report, classifier; `Profile` byte identity; `RefuseScriptsAndExternal`; `Carry` | V4 | §9 D2 tests fail first; corpus identity under default options; Carry run: every gain passes `doc_actions`, `javascript` (and `xfa`), report = runner inventory = pypdf action inventory; fuzz | 3–4 d |
| C2 D1 | signature-like walker, removal, output check, outcome, `signatures` rule | C1 (API), V4 | §9 D1 tests; Remove run: every gain `transformed` only by the D1 rule, qpdf 0, Poppler equal, the library's output check and the qpdf JSON and pypdf marker scans (full §5.3.2 set) clean; fuzz | 3–4 d |
| review | independent review of each phase before merge, re-review after fixes | — | no open P0 / P1 | — |

Order: V4 → C1 (D2) → C2 (D1). D1 has no corpus value without D2 (§7),
and both need V4 to be measured.

## 11. Open decisions

1. **Name of the default executable policy** (`Profile`) and whether
   `RefuseScriptsAndExternal` ships in v0.5. Recommended: ship it. It
   is the one policy security-minded callers ask for, and it costs
   only the classifier, which `Profile` needs for its report.
2. **Seed values** (`/SV`) on unsigned signature fields under
   `Remove`. Carry (no signature exists) or refuse (signing is
   anticipated). Recommended: carry and report.
3. **`/SigFlags`**: resolved after review. Bit 1 is cleared as part of
   the declared change (the §4 acceptance); bit 2 is carried and
   reported.
4. **`/DSS` with usage rights only:** option 1 (refuse) now, option 3
   later on demand.
5. **`MultimediaPolicy::Carry`:** not in v0.5. Revisit with a use case.
6. **The `transformed` class in the enforced map:** introduce it with
   the version rule in V4 (moves two report-only fails), or keep it
   report-only until D1. Recommended: introduce in V4, report only.
7. **Tightening the default.** Should `rewrite` (and `Profile`) refuse
   stray signature-like dictionaries that the current checks miss (a
   `/ByteRange` under a field without `/FT`, a timestamp outside
   fields, `/DSS`, `/Legal`)? Doing so is safer, but it changes
   `rewrite`'s results (a behaviour change for 0.5, to be measured on
   the corpus). Recommended: decide after C2, with the walker in hand
   and its corpus effect measured. Until then `rewrite` is unchanged
   and the gap is documented in `docs/limitations.md`.

## 12. Independent review of the design

First review (a different model, read-only): approve with changes.

| # | severity | finding | decision |
|---|---|---|---|
| F1 | P0 | the signature marker required a known handler or sub-filter name; a dictionary with `/Contents` and an unlisted `/SubFilter` escaped both the input and the output check | adopted: `/Contents` with any `/Filter` or `/SubFilter` is a marker; test |
| F2 | P1 | "both phases close the signature gaps under every policy" and a report cap both contradicted byte and error identity of the default | adopted: the walker runs under `Remove` only; tightening the default is open decision 7; no report cap (bounded by the existing object limits) |
| F3 | P1 | `GoToDp` (PDF 2.0) in no class, so refused under `Carry` | adopted: Navigation; test |
| F4 | P1 | XFA script detection unspecified | adopted: local name `script` under any prefix, on W3's checked text, packets together; tests |
| F5 | P1 | `/SigFlags` "carry" contradicted the accepted D1 criterion (bit 1 must not claim a removed signature) | adopted: bit 1 cleared as part of the declared change, bit 2 carried and reported; rule and test |
| F6 | P1 | the "independent" checks share the library's parser | adopted: worded as a separate walker over a fresh parse; qpdf JSON and pypdf re-reads in the acceptance |
| F7 | P2 | target-check edge cases (`/Reference` without `/TransformMethod` means DocMDP; empty or absent `/Reference`; `/UR` vs `/UR3`) | adopted |
| F8 | P2 | `/Lock` / `/SV` / `/FieldMDP` conflated | adopted: reworded |
| F9 | P2 | "no key reordered" is false byte-wise (sorted maps) | adopted: reworded |
| F10 | P2 | multimedia actions under `MultimediaPolicy::Refuse` ambiguous | adopted: carried and reported; the policy applies to catalog `/RichMedia` |
| F11 | P2 | `Disposition::Refused` could never appear | adopted: field removed |
| F12 | P2 | `/OpenAction` arrays, catalog `/URI`, `/Threads` / `/PresSteps` | adopted |
| F13 | P2 | `/Root/Perms/UR` missing from the graph allowance | adopted |
| F14 | P2 | security.md overclaimed "every signature" | adopted: reworded |
| F15 | P2 | test gaps for the above | adopted: rows added to §9 |
| F16 | P2 | signed embedded files; a shared `/Perms` dictionary | adopted: threat rows, refusal rule 5a, test |

Re-review: the P0 and the P1s above confirmed resolved; one new P1 and
eight P2s, all adopted.

| # | severity | finding | decision |
|---|---|---|---|
| G1 | P1 | clearing `/SigFlags` bit 1 also changes the `signatures` category's `/SigFlags` line, which the D1 rule did not allow, so every such removal would `fail` | adopted: allowed in `signatures` too, only bit 1; near-miss test |
| G2 | P2 | the output check's policy scope unstated | adopted: `Remove` only, both outcomes |
| G3 | P2 | a stale `max_report_items` in the threat table | adopted |
| G4 | P2 | the third-party marker scan narrower than §5.3.2; acceptance rows not naming the re-reads | adopted: full marker set; named in §10 |
| G5 | P2 | `/AcroForm` edited for `/SigFlags` has the shared-dictionary hazard | adopted: rule 5a covers every edited dictionary |
| G6 | P2 | `/Lock` vs an `/SV`-nested `/Lock` read as contradictory | adopted: reworded; a field's own `/Lock` refuses on any field |
| G7 | P2 | `SigFlagsChange` not defined | adopted |
| G8 | P2 | an unclassifiable XFA script match carried unreported under `Carry`/`Profile` | adopted: refuses under every policy |
| G9 | P2 | `/OpenAction` array members indistinguishable in the report | adopted: member index |

