# Experimental multi-page region removal

## Experimental multi-page region removal (`remove_regions`, CLI `experimental-remove-regions`)

**Not a redaction tool** — the same experiment boundary as
`remove_region`, extended across pages in one edit session and one
rewrite. API `remove_regions(doc, specs, limits) -> RegionsRemoval`
where `specs` is `(0-based_page_index, rects)` pairs:

- **One `Edit`, one rewrite** — every page's edits are accumulated
  into a single `Edit::overlay` (image clones + new `/Contents`
  streams + materialised page dicts + stripped `/Pages` resources),
  then `rewrite` runs once. No per-page PDF is emitted, no chained
  rewrites, no per-page budget resets.
- **Merged page specs** — a page listed twice merges its rect lists
  into one plan; pages are processed in ascending order regardless
  of input order. Empty specs, empty rect lists, out-of-range page
  indices and malformed rects are all refused *before* any walk.
- **Per-page `max_diag_rects`** — the merged per-page rect
  count is bounded by the same `max_diag_rects` limit as the
  single-page API; a page repeated across several specs cannot
  bypass it. The count is per page, not a document-wide rect
  budget — different pages each within the limit pass even when
  their sum exceeds it. Out-of-range page indices refuse without
  panicking — a `usize::MAX` index never overflows the error
  message (`page_label` uses `checked_add`).
- **Per-page rules unchanged** — every `remove_region` refusal
  (`/Annots`, forms, masks/JPEG/non-device colour, non-`Tr 0` text,
  `/UserUnit` ≠ 1, `blocker`/`uncertain`) applies per page. A
  failure on any page fails the whole call — no partial output.
- **Shared budgets across pages** — the cumulative limits
  (`max_diag_exec_ops`, `max_text_codes`, `max_show_states`,
  `max_edit_content_bytes`, `max_edit_objects`, `max_output_bytes`)
  bound the *whole call*, not each page. Each page's spend is
  measured from its `DiagWalker` (`exec_ops_consumed`,
  `text_codes_consumed`, `xobject_invocations_consumed`,
  `show_states_len`) and subtracted from the budget given to the
  next page — a page that fits alone but not cumulatively is a
  `LimitExceeded` error.
- **Contents + Resources integration** — each edited page's dict
  gets its new `/Contents` *and* materialised `/Resources` in the
  same `page_dict` before the overlay insert, so the rule
  (never write `Contents` separately from materialised `Resources`)
  holds across pages. `/Resources` materialisation and `/Pages`
  stripping run once over the whole document after all per-page
  edits are collected — a parent can only drop `/Resources` when
  *every* descendant page has been materialised.
- **Shared image independence** — two pages sharing the same image
  object each clone their own `KasIm{n}`; the unedited page keeps
  the original stream. `pdfimages` confirms the unedited page's
  image samples are untouched while the edited page's clone is
  zeroed.
- **Result** — `RegionsRemoval { output, codes_removed,
  invocations_edited, pixels_changed, orphaned_names_removed,
  per_page: Vec<(page_index, Vec<Site>)>, scope }`; `per_page`
  carries the 0-based page index alongside the `Site`s so a
  multi-page caller can attribute each edit.

CLI:

```sh
kasane-pdf experimental-remove-regions <file.pdf> \
  --page <n> --rect <llx> <lly> <urx> <ury> [--rect ...] \
  [--page <m> --rect ...] --output <out.pdf>
```

`--page` is 1-based; each `--rect` attaches to the nearest
preceding `--page`; `--rect` before any `--page` is refused, as is
a `--page` with no rects. Same `--output`, same-file refusal, and
atomic-write rules as `experimental-remove-region`.

External check: `qpdf --check` clean; `pdftotext` shows the edited
page's removed codes gone while the other page's text is intact;
`pdfimages` shows the edited page's image pixels zeroed while the
shared original survives on the unedited page.
