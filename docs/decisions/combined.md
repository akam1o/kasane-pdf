# Decision — combined region removal + metadata strip

## Combined region removal + metadata entry removal

`remove_regions_and_document_metadata` composes `remove_regions` and `remove_document_metadata` in
**one `Edit` session and one `rewrite`** — not by chaining the two
public APIs (which would produce an intermediate PDF).

Key choices:

- **Shared session builder.** `remove_regions`'s body is extracted
  into `regions_edit_session(doc, specs, limits)` which returns a
  `RegionsEdit { edit, totals…, per_page }` — the accumulated
  `Edit` (image clones, new `/Contents`, staged page dicts,
  materialised `/Resources`) plus the counters — without writing.
  `remove_regions` calls `session.edit.rewrite()`; the combined
  entry point calls `session.edit.rewrite_with_overrides(...)`.
  No logic is duplicated between the two paths.
- **`Edit::rewrite_with_overrides`** (`pub(crate)`) runs the same
  `commit` + `rewrite_overlay` pipeline but forwards a
  `rewrite::RewriteOverrides` to `rewrite_overlay_with`. `Edit::rewrite`
  is now a thin wrapper with default overrides — the public
  rewrite path is unchanged.
- **Metadata strip merges, not replaces.** In
  `rewrite_overlay_with`, `strip_metadata` builds the catalog
  overlay from the *existing* overlay body for the catalog ref
  when one is present (removing `/Metadata` from that dict), and
  only falls back to `doc.catalog()` when absent — so an earlier
  edit to the catalog can never be silently rolled back. Other
  overlay entries are preserved via `entry().or_insert`.
- **Single rewrite, cumulative budgets.** All multi-page cumulative
  limits still bound the whole call; the metadata strip adds no
  new limits and never resets a budget. Any failure (per-page
  eligibility, cumulative budget, rewrite refusal, output cap)
  returns `Err` before output — the `Edit` and its overlay are
  dropped, so the input `Document` is untouched.
- **Result** `RegionsAndMetadataRemoval` = the region tallies +
  `info_removed`/`metadata_removed` (entry-present-and-removed
  flags) + `scope` stating the boundary. `Debug` prints no output
  bytes, site content, or metadata values.

CLI: `experimental-remove-regions` gains `--remove-document-metadata`;
without it behaviour is identical to `remove_regions`. The standalone
`remove-document-metadata` command is kept.
