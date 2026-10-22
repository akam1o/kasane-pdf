# Decision — pre-redaction diagnosis (M16)

## M16 — pre-redaction diagnosis

`src/redact_diag.rs` implements `diagnose_redaction` +
`preflight-redaction` — a **read-only** preflight that walks a page's
content as a reader (not a renderer) and reports what a future
redaction would have to handle.

Why a separate walker instead of reusing `Renderer`: the render
interpreter skips path/image/clip work in extraction mode and
*refuses* on `gs`/`sh`/patterns — those exact constructs are the
blockers a diagnosis must *report*, not abort on. The walker
(`DiagWalker`) mirrors the render lifecycle (gs stack, CTM, text
state, `Do` recursion with `xobject_stack`, `/Resources` swap) but
records occurrences instead of painting.

Key semantics — every choice is conservative because a wrong "safe"
verdict is worse than a verbose report:

- `targets` vs `uncertain` vs `blockers` vs `incomplete` vs
  `unexamined` are kept distinct so "nothing found" is never confused
  with "could not look". `Diagnosis` has no boolean verdict.
- `Site { contents, op_index, form_path, code_index }` is the minimum
  identity needed to re-find an occurrence: `/Contents` element refs
  (every element when the array is used — `page_operations` already
  treats them as one logical stream), a parsed op index, the
  outermost-first `Do` chain (each [`FormFrame`] carries the
  call-site `op_index`, so two `Do`s of the same object are distinct
  sites), and `code_index` — the character's position in the op's
  code stream. For `Tj`/`'`/`"` it counts codes in that one string,
  restarting at 0; for `TJ` it counts *across the whole array* so
  `[(A) 100 (B)]` reports 'A' at index 0 and 'B' at index 1 —
  numeric elements produce no codes and do not advance it. No
  source map, no generic IR.
- `DiagGs` is the *whole* graphics state the diagnosis touches —
  CTM, clip, stroke state (`w`/`J`/`j`/`M`), and the text state
  (`Tc`/`Tw`/`Tz`/`TL`/`Ts`/`Tr`/`Tf` — Table 52 parameters). `q`/`Q`
  save and restore all of it, so an inner `w` or `Tf` never leaks;
  a `Form` `Do` pushes and pops the same struct for its inner ops.
  `Tm`/`Tlm`/`in_text` are *not* graphics state — they live on
  `DiagWalker` inside `BT`/`ET`.
- Text bounds are the glyph's *outline* bbox or an *estimated cell*
  (labelled `TextBounds::Cell` for space/`.notdef`/missing outlines)
  — never silently presented as a safe redaction region. A
  `glyph_id`/`outline`/`units_per_em` *error* (broken cmap, limit,
  unsupported encoding) reports `uncertain` — never a silent cell
  estimate; `LimitExceeded` propagates.
- Stroke-mode text (`Tr` 1, 2, 5, 6) inflates the glyph bounds by the
  same `stroke_reach` a path uses — a text stroke extends outside the
  ink bbox exactly like a path stroke. Clip-mode text (`Tr` 4–7) is
  a blocker (unsupported by the engine) + an `uncertain` entry — its
  extent is not estimated.
- Image bounds are the unit square under the `Do` CTM — the *whole*
  image extent, so rotation/shear/clip over-report rather than
  under-report.
- Path bounds inflate the construction points by the stroke reach —
  `w/2` for butt/round/square caps and joins, `miter_limit × w/2`
  when miter joins are in effect — scaled by `√2 × max(|col₁|,
  |col₂|)` of the CTM, the provable upper bound on a unit vector's
  stretch. `sqrt(|det|)` — the *geometric* mean of the singular
  values — is **not** conservative: under `10 0 0 0.1` it returns 1
  while the actual stretch is 10.
- A `w` of 0 is a device-pixel **hairline** — its coverage is
  resolution-dependent and cannot be bounded in user space, so a
  *stroked* path or stroke-mode text (`Tr` 1, 2, 5, 6) at `w == 0`
  reports as `uncertain`, never a certain `Target` whose bounds
  would be claimed "outside the rect". Fill-only ops (`f`, `f*`,
  `Tr` 0, 3) are unaffected — they do not stroke; fill+stroke ops
  (`B`, `b`, `B*`, `b*`) report the whole occurrence as uncertain so
  the stroke's un-evaluated reach is not dropped.
- Sharing analysis is bounded: `site_map` on this page + a
  `/Resources` scan of ≤`max_diag_share_pages` other pages, ≤
  `max_diag_share_refs` entries. `shared_scope.truncated` is reported;
  "not shared" is only claimed for *scanned* pages.
- Anything the walker cannot interpret (unsupported font, non-finite
  CTM, out-of-order path ops, inline images) stops the walk and
  records `incomplete` — partial results are never mistaken for
  "all clear". An *uninterpreted* state-changing op (`gs`, `sh`,
  Pattern colour space) does not stop the walk but marks subsequent
  geometry `uncertain` until `Q` or a form's end restores known
  state — the flag is saved and restored with `DiagGs`.

CLI: `kasane-pdf preflight-redaction in.pdf 1 --rect llx lly urx ury
[--rect …] [--format text|json]` — read-only, deterministic order,
capped output, exit 0 on a completed diagnosis (with or without
blockers), 1 on a typed error, 2 on usage errors. The name is
deliberately not `redact`.
