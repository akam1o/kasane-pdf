# Security & safety posture

kasane-pdf treats every input byte as hostile. Two independent
mechanisms apply: a **`Limits` bound** on every unbounded operation,
and a **typed-refusal policy** — a construct the engine cannot process
*exactly* produces `Error::Unsupported`/`Error::Structure`, never a
partial or best-effort result presented as success.

## Resource bounds

Every unbounded operation is capped by [`Limits`](../src/limits.rs)
(see `architecture.md` § Safety / hostile input for the full table):
input size, indirect-resolution depth, page-tree depth/nodes,
collection sizes, string/nesting depth, decoded-stream bytes (single
+ cumulative), filter-chain length, object-stream size, parsed-op and
operand-stack counts, render pixel/buffer size, path-element count,
xref section/entry counts, the diagnostic `max_diag_*` budgets, and
the edit `max_edit_*` budgets.

Additionally: cycle detection on reference chains, `/Prev` chains,
and page-tree node revisits; object-stream bodies decoded once per
Document; `unsafe` is never used.

## Typed errors, not partial results

`Error::kind()` gives a stable machine-readable category
(`parse`, `structure`, `unsupported`, `encrypted`, `limit`,
`not-pdf`, `io`). A malformed file and a valid file using an
unimplemented feature are *distinguishable* — automation should branch
on `kind()`, not on message text.

## What the engine is **not**

- **Not a redaction tool.** The `experimental-remove-*` commands and
  `remove_*` APIs are *content editors*: they rewrite selected
  operations/pixels into a new object but do **not** prove the
  removed data is gone. The original stream may survive via sharing;
  `pixels_changed`/`codes_removed` report what was edited, not a
  removal guarantee. See [`features/editing-*.md`](features/) and
  [`redaction-design.md`](redaction-design.md).
- **Not a sanitizer.** `rewrite`/`rewrite_with_limits` carry every
  *reachable* object verbatim — including keys and payloads the
  engine does not interpret. "Preserved" ≠ "validated" ≠ "safe".
  See [`features/rewriting.md`](features/rewriting.md).
- **Not a "safe" verdict.** `diagnose_redaction` /
  `preflight-redaction` deliberately return no boolean verdict. A
  clean report means only "the walk finished within the examined
  scope" — see [`features/diagnosis.md`](features/diagnosis.md).
- **Not guaranteed-durable output.** The CLI write path is atomic
  (temp-file + `sync_data` + rename) but does not fsync the parent
  directory — power-loss durability is out of scope. See
  [`features/rewriting.md`](features/rewriting.md) § Save guarantees.
