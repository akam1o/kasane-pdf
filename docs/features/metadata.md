# Document-metadata entry removal

## Document-metadata entry removal (`remove_document_metadata`, CLI `remove-document-metadata`)

**Not a metadata scrubber or redaction tool.** This removes exactly
two *entries* — the trailer `/Info` key and the catalog `/Metadata`
key — and writes one new single-revision PDF. It does not parse,
edit, or partially redact their values; it does not touch
`/Metadata` on pages, XObjects, or any other object; and it does
not remove the same information wherever else it may live
(annotations, structure tree, embedded files, page content).

- **Removed**: the trailer `/Info` *entry* (it is dropped as a
  collection root and from the output trailer) and the catalog
  `/Metadata` *entry* (the catalog is overlaid with a copy that
  lacks the key).
- **Kept**: page/XObject `/Metadata`, `/Info` values reachable
  through other keys, the `/Metadata` stream when it is still
  referenced, annotations, structure tree, `ActualText`/`Alt`/`E`,
  attachments, JavaScript (when not a refusal), fonts/`ToUnicode`,
  page content, and every other catalog key.
- **Shared-data boundary** — when the removed entry's *value* is
  referenced by another key it stays reachable and is still
  emitted; `info_removed`/`metadata_removed` report that the
  *entry* was present and removed, never that the data is gone.
- **Refusals** — the full `rewrite` refusal set is inherited
  (encrypted, signed, named destinations, out-of-profile catalog
  entries, …) and the usual output budgets still apply. An
  `/Info`/`/Metadata` *value* that can't be resolved is not a
  reason to fail — the entry is removed without resolving it.
- **Result** — `MetadataRemoval { output, info_removed,
  metadata_removed, scope }`; flags report entry removal, not data
  disappearance.

CLI:

```sh
kasane-pdf remove-document-metadata <file.pdf> --output <out.pdf>
```

Reports which of the two entries were removed; never prints
metadata values, XMP bodies, or "all metadata removed".

External check: `qpdf --check` clean; output trailer has no `/Info`
and the catalog has no `/Metadata` while `/Pages`, `/PageMode`,
page content and other catalog keys are unchanged.
