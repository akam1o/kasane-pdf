# Decision — residual-information audit (M23)

## Residual-information audit (M23)

`audit_residuals(doc, limits) -> ResidualAudit` is a **read-only**
inventory of where a PDF can still carry information. It exists so a
caller deciding what a "distributable" file must contain has an
enumerated list to work from — the audit deliberately does *not*
decide sensitivity, never deletes anything, and a clean report is
not a safety verdict.

Design choices:

- **Three passes, one structure.** (1) a BFS reachability walk from
  the audit roots (trailer `/Root`, trailer `/Info`, every page
  object) builds `reachable` and `referrers`; (2) structure-keyed
  passes read the trailer/catalog/page dicts, annotations, form
  fields, structure tree, name trees and every live xref object;
  (3) the xref/history pass re-reads each `/Prev` section so
  superseded bodies are reported *as superseded*, not merged away.
- **Completeness is explicit.** `walk_complete` is `true` only when
  the BFS drained its queue — an edges-cap, element-cap or depth
  stop leaves it `false`. Only then is `unreachable_live` a
  complete count; otherwise unvisited live objects are
  `unevaluated_live`. This is *separate* from `findings` output
  truncation (`max_audit_findings`), which never claims anything
  about reachability.
- **Missing references are findings, not objects.** An edge whose
  target is not in the xref is recorded as a `MissingReference`
  finding carrying the referrer path; the target counts as
  `missing_references`, never as `objects_examined` or `reachable`.
  It does not itself mark the walk incomplete (the queue still
  drains) — but it is never silently absent either.
- **Findings carry no content.** `/Info` contributes key names only;
  annot `/Contents`, field `/V`, JavaScript and stream bodies never
  enter a `note`. Streams report `(encoded_len, declared_/Length)`.
- **Reachability is explicit.** `ReachClass` distinguishes
  `Reachable` / `UnreachableLive` / `PreviousRevision` /
  `Unevaluated`. "No history" (one xref section) is reported as a
  single section — *not* as proof of no old bytes; physical slack
  stays under `unexamined`.
- **Partial coverage is stated.** `incomplete` records every scope
  stopped by a cap or an unreadable construct (with `limit: bool`);
  `unexamined` lists scopes never entered. A truncated audit never
  reads as a clean one.
- **Budgets are additive, not reusing parse limits silently.** New
  `Limits` fields: `max_audit_findings`, `max_audit_edges`,
  `max_audit_paths` (per-object referrer cap), `max_audit_walk_depth`
  (one object's container scan depth), `max_audit_content_ops`
  (per-page ops scanned for `/ActualText`). Document-level
  `max_reference_depth`/`max_total_decoded_bytes` still apply.
- **Path representation.** `PathStep::{TrailerKey, Key, Index,
  NameTreeEntry, ResourceName}` covers dict keys, array indices,
  name-tree leaf names and `/Resources` category+name; binary names
  are escaped at print time (`json_escape_bytes` / `escape_bytes`).
- **Name-tree traversal is iterative** (explicit stack, no call-stack
  growth) with two distinct guards: `seen` — nodes already fully
  expanded (shared kids walk exactly once and are reported as
  shared, not silently skipped) — and `path_refs` — the refs on the
  current root-to-node path (a kid whose ref is already an ancestor
  is a cycle, reported as `cycle:` in `incomplete`). Depth is
  `max_audit_walk_depth`; total work is `max_audit_elements` shared
  with the other traversals. Missing `/Kids` refs and resolution
  failures are recorded, never `if let Ok`-dropped.
- **Depth counts nesting, not breadth.** `walk_object_refs` carries
  the real container depth per frame; a 100-element array at depth
  1 does not trip a depth cap, while a 100-deep nest does — and
  only that branch is dropped (siblings still walk). Container
  *elements* are a separate `max_audit_elements` budget so wide
  structures are still bounded.

- **Incremental traversal (M23.2).** `walk_object_refs` no longer
  expands a container's children into the stack up front. A frame
  is one of:
  ```text
  enum Frame<'a> {
      Leaf(&'a Object),
      Array { it: slice::Iter<'a, Object>, idx: u32, path: Vec<PathStep> },
      Dict  { it: btree_map::Iter<'a, Vec<u8>, Object>, path: Vec<PathStep> },
  }
  ```
  Each `stack.pop()` yields the *next* element of the top
  container — `it.next()` — and pushes the (still-live) frame back,
  so a wide array keeps exactly one frame for its whole breadth.
  `max_audit_elements` is consumed at yield time, before any
  sibling paths are built, and the `Vec<PathStep>` is cloned only
  when the yielded element is a `Ref` (for the referrer record) or
  a container being descended into — never for a leaf element.
  The `path` inside each frame is the path *to the container*,
  extended with the yielded `Index`/`Key` step when a child is
  descended into. `enum Frame` holds no per-element state beyond
  the iterator position, so a budget stop never held the whole
  container in memory.
- **Name-tree entry failures are recorded (M23.2).** `audit_name_trees`
  used to `let Ok(..) = deref(..)` on the `/Names` root and on each
  tree root (`/EmbeddedFiles`, `/JavaScript`) — a missing or
  unresolvable ref silently produced "no attachments". Now each
  entry-point `Err` pushes `AuditIncomplete { scope, reason:
  "<key> does not resolve: <err-kind>", limit: matches!(e,
  LimitExceeded{..}) }` and the *sibling* tree continues. `/Names`
  resolving to a non-dict is likewise recorded. A legitimately
  absent `/Names` adds nothing — absence is not incompleteness.
  The same `Err → incomplete` rule applies to `audit_structure_tree`'s
  `StructTreeRoot`/`/K`/`/K[i]` derefs for consistency.
- **ObjStm wording corrected.** An earlier draft described objects
  "inside ObjStm (e.g. Image)" as undetected; streams cannot
  legally appear inside `/ObjStm` (only non-stream dicts can), so
  the actual gap is narrower: a compressed *dict* (annot,
  Filespec, etc.) goes unclassified because the audit reads xref
  entries, not ObjStm member bodies. The docs now state this
  distinction and that no classification feature was added.

- **Stack-height probe moved into production (M23.3).** M23.2's
  `Probe` test duplicated `walk_object_refs` to record
  `stack.len()` — a copy that could not detect a regression in the
  real walker. M23.3 replaces it with a `#[cfg(test)]`-only field
  `max_walk_stack` on the `Audit` struct plus a `push_frame!`
  macro that records the high-water mark at every stack push.
  Tests call the production `walk_object_refs` through a small
  `test_audit()` builder; the field and macro compile out in
  non-test builds, and no public API gains a measurement field.
  Wide-array, wide-dict, deep-nest, and budget-stop inputs all
  assert the frame count stays proportional to open containers
  (never to breadth), and `element_budget_stop_keeps_stack_small_and_incomplete`
  checks that a budget stop reports `walk_complete = false` with
  unvisited objects as `Unevaluated`, not `UnreachableLive`.
- **CLI-level incomplete verification (M23.3).** `cli_audit_incomplete_text_names_scope_and_reason`
  and `cli_audit_incomplete_json_structure` run the real binary
  (`CARGO_BIN_EXE_kasane-pdf`) against a PDF whose
  `/EmbeddedFiles` root is a dangling ref: text output shows the
  failing `scope` and `reason` (never a bare "incomplete"), JSON
  output shows the `{"scope","reason","limit":false}` object, the
  good `/JavaScript` sibling finding survives, the input bytes are
  unchanged, no PDF is written, and a `/Contents` secret never
  reaches stdout. `cli_audit_no_name_tree_no_incomplete` asserts a
  file with no `/Names` produces no name-tree incomplete.

CLI: `kasane-pdf audit-residuals in.pdf [--format text|json]` —
prints to stdout, writes no file, exit 0 on a completed audit.
