# Combined region removal + document-metadata removal

## Combined region removal + metadata entry removal (M26, `remove_regions_and_document_metadata`, CLI `experimental-remove-regions --remove-document-metadata`)

**Not redaction or sanitizing.** Runs the full `remove_regions`
plan *and* the M25 metadata-entry strip in one `Edit` session and
one `rewrite` — no intermediate PDF is produced or re-parsed.

- Same input rules, per-page eligibility, and cumulative budgets
  as `remove_regions`; a failure on any page (or an inherited
  rewrite refusal) emits nothing and leaves the input untouched.
- The trailer `/Info` and catalog `/Metadata` entries are removed
  in the *same* rewrite via `RewriteOverrides` — the catalog is
  overlaid minus `/Metadata` (merging with any existing overlay
  body, never rolling back other edits) and `/Info` is skipped as
  a collection root. Page/XObject `/Metadata` and shared-referenced
  values are kept exactly as in `remove_document_metadata`.
- Result `RegionsAndMetadataRemoval` adds `info_removed` /
  `metadata_removed` to the region-removal tallies; flags mean
  "the entry was present and removed", not "the data is gone".

CLI: `experimental-remove-regions <file> --page … --rect …
[--remove-document-metadata] --output <out>`. Without the flag the
metadata entries are kept; with it the two entry-removal flags are
also printed. `remove-document-metadata` remains a standalone
command.
