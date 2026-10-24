# Decision — multi-page region removal

## Multi-page experimental region removal

`remove_regions` extends the `remove_region` edit across
several pages in **one `Edit` session and one `rewrite`**. It is
still an *experimental content editor* — not redaction — and the
per-page eligibility list is unchanged.

Key choices:

- **Input shape** — `&[PageSpec]` where `PageSpec =
  (usize /*0-based page*/, Vec<(llx,lly,urx,ury)>)`. A page that
  repeats merges its rect lists; pages are walked in ascending
  index order regardless of input order. All validation (empty
  specs, empty rect lists, out-of-range page indices, malformed
  rects) runs *before* any page is walked, so a bad spec never
  leaves a half-built `Edit`.
- **One `Edit` for everything** — `edit_guard` is `Option<Edit>`
  shared across pages: each image clone gets `edit.alloc()` and
  lands in `overlay`, each new `/Contents` stream likewise, and
  each edited page's dict (Contents + materialised Resources +
  updated XObject names) is staged into `page_dicts` and inserted
  into the same overlay *after* every page's edits are computed.
  `rewrite` runs exactly once at the end. The rule — Contents
  and materialised Resources live in *one* page dict — is applied
  per page before the dict is staged, so a page's contents edit can
  never be clobbered by another page's Resources materialisation.
- **Cumulative budgets carry forward** — `Limits` documents
  `max_diag_exec_ops`, `max_text_codes`, `max_xobject_invocations`,
  and `max_show_states` as whole-call caps. `DiagWalker` resets its
  counters per page, so `remove_regions` reads
  `exec_ops_consumed`/`text_codes_consumed`/
  `xobject_invocations_consumed`/`show_states_len` after each page
  and gives the *next* page the remaining budget (not a fresh
  `Limits`). `generated_bytes` is accumulated across pages the same
  way `remove_region` accumulates it within one page.
- **Resources materialisation once for the document** —
  `plan_resource_materialisation` runs over the whole `/Pages`
  tree after per-page edits are computed; only pages *not* already
  in `edited_refs` get a bare-Resources overlay insert (edited
  pages already carry their Resources inside the staged dict).
  `/Pages` stripping is unchanged — only when every descendant has
  been materialised.
- **Per-page independence** — each page's `DiagWalker` resolves
  `/XObject` names through *its* effective `/Resources`, so two
  pages that share the same image object each clone their own
  `KasIm{n}` and repoint only their own `Do`s. The unedited page
  keeps the original image stream and content; the writer's
  reachability scan drops the orphaned original only when nothing
  references it.
- **Per-page rect cap + panic-free errors.** The merged
  per-page rect count is checked against `max_diag_rects` inside
  the merge loop with `checked_add` — a page repeated across
  several specs accumulates toward the same limit, so splitting a
  page's rects across repeated specs cannot bypass it. The cap is
  *per page* (matching the single-page API), not a document-wide
  budget. Every `page_index + 1` in an error message routes through
  `page_label()`, which uses `checked_add` — a `usize::MAX` index
  produces a `Structure` refusal instead of overflowing.

CLI `experimental-remove-regions` mirrors `experimental-remove-region`
with `--page <n> --rect … [--page <m> --rect …] --output <out>`:
1-based pages, `--rect` attaches to the nearest preceding `--page`
(a `--rect` before any `--page` is refused, as is a `--page` with
no rects), pages may repeat to add rects. Same `--output`,
same-file refusal, atomic write, exit codes as `remove_region`.
