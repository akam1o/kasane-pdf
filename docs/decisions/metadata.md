# Decision — document-metadata entry removal

## Document-metadata entry removal

`remove_document_metadata` removes exactly two *entries* — the
trailer `/Info` key and the catalog `/Metadata` key — and runs one
`rewrite`. It is not a metadata scrubber and not redaction: the
values are never parsed, and anything still referenced through
another key stays reachable (and is emitted) by design.

Key choices:

- **Trailer `/Info`** — the writer collects roots from the trailer
  (`/Root` and `/Info`). `rewrite_overlay_with` takes an internal
  `RewriteOverrides { strip_info, strip_metadata }`: with
  `strip_info` the `/Info` root is simply never added to
  `root_refs`, so no `/Info` object is collected and no `/Info`
  key is written to the output trailer. The `/Info` *value* is
  never resolved — a direct dict, a wrong-typed value, or a
  dangling ref all just get dropped with the key.
- **Catalog `/Metadata`** — the catalog is an indirect object
  referenced by trailer `/Root`; `strip_metadata` clones the
  catalog dict, removes the `Metadata` key, and inserts the result
  into the overlay keyed by the catalog's ref. The collector
  resolves the catalog through the overlay (it wins over
  `doc.resolve`), so the emitted catalog has no `/Metadata`; the
  XMP stream stays reachable only if some other key references it.
  Other catalog keys are untouched.
- **One rewrite** — both removals land in the same collection pass
  and the same `rewrite_overlay` call; there is no per-entry
  output. All existing refusals and output budgets are inherited
  unchanged, and the plain `rewrite`/`Edit` paths are unaffected
  (`rewrite_overlay` delegates with default `RewriteOverrides`).
- **Result** — `MetadataRemoval { output, info_removed,
  metadata_removed, scope }`. The flags mean "the entry was present
  and removed"; `scope` states the boundary (removed entries vs.
  data that may remain via other references or copies).

CLI `remove-document-metadata <file> --output <out>` reuses
`refuse_same_file`/`write_atomic`; on success it prints only which
of the two entries were removed — never metadata values, and never
claims "all metadata removed".
