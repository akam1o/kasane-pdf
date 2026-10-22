# Design decisions

One record per editing/diagnosis subsystem, kept so the *reason* for
each shape stays traceable to the milestone that introduced it. The
feature/contract lives in [`../features/`](../features/) — these files
are the *why*, not the *what*.

| File | Decision | Scope |
|---|---|---|
| [`diagnosis.md`](diagnosis.md) | `diagnose_redaction` / `preflight-redaction` (M16) | read-only preflight walker — why it is a separate `DiagWalker` and why it never returns a "safe" verdict |
| [`editing.md`](editing.md) | experimental scoped editing (M17–M28) | text removal, image-region removal, region removal, JPEG decode, grid-aligned CTM, budgets, per-invocation cloning |
| [`audit.md`](audit.md) | `audit_residuals` (M23) | residual-information inventory — read-only, reports presence/location only |
| [`multi-page.md`](multi-page.md) | `remove_regions` (M24) | multi-page walk, inherited-`/Resources` materialisation, post-order strip decision |
| [`metadata.md`](metadata.md) | `remove_document_metadata` (M25) | `/Info`/`/Metadata`/`/PieceInfo`/`/MarkInfo`/`/Perms` removal |
| [`combined.md`](combined.md) | `remove_regions_and_document_metadata` (M26) | region removal + metadata strip in one atomic pass |

New milestone-scale design choices should be added as a new file here
and indexed — not folded back into a single monolithic design doc.
