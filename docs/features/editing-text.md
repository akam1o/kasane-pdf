# Experimental scoped text removal

## Experimental scoped text removal (`remove_text`, CLI `experimental-remove-text`)

**Not a redaction tool** — the name and docs say so up front. `remove_text`
*deletes* the character codes whose filled-ink bounds overlap a
caller rectangle and writes a new single-revision PDF; it does not
paint over anything, does not touch other copies of the same data
(metadata, annotations, structure tree, other pages, font internals,
the original `/Contents` when it is shared), and does not guarantee
irreversibility. Think of it as an experiment in character-level
content editing on the smallest safe scope.

What it does — API `remove_text(doc, page_index, rects, limits)`
returning `Removal { output, codes_removed, sites, scope }`:

- Rects are un-rotated, un-cropped user space — the same convention
  as `preflight-redaction`/`inspect`. A code is removed when its
  *filled* glyph-outline bounds overlap any rect (edge-touching
  counts); a code is one removal unit even when it expands to several
  Unicode characters.
- Reuses the diagnosis `DiagWalker` so the graphics/text state and hit
  tests are exactly the diagnosis's — any `blocker`/`uncertain`
  entry, or a mid-stream walk failure, is a typed refusal, not a
  best-effort skip. Walk failures keep their typed error
  (`LimitExceeded`, `Structure`, `Parse`…) — they are never
  flattened into `Unsupported`.
- **Op-time state**: the walker records the text state at
  each page-level show op — resolved font, `Tfs`, `Th`, `Tc`, `Tw`
  — keyed by `op_index`. Selection and the rewrite read the *same*
  snapshot, so a later `Tf`/`Tc`/`Tw`/`Tz` change, `q`/`Q` restore,
  or second `BT`/`ET` can never rewrite an earlier show under the
  wrong parameters. `"`'s snapshot is taken *after* its `Tw`/`Tc`
  update.
- **Page-level eligibility**: a page is
  eligible only when *every* shown text occurrence is `Tr 0` fill —
  an out-of-rect `Tr 3`/`Tr 1`/`Tr 2` glyph still refuses the page,
  whether it's at page level or inside a form. `Tr 4–7` and `w = 0`
  hairlines arrive via `blocker`/`uncertain` and refuse the same
  way. A `Tr` value *set* but never used to show text is harmless
  (the mode only matters when a glyph paints under it).
- **One level of `/Form`**: a page-level `Do` whose target is
  a `/Form` XObject is editable per-invocation — the form's inner
  text is rewritten into a *clone* of its stream (new object,
  `/Filter`/`/DecodeParms` dropped since the clone body is emitted
  uncompressed, `/Resources`/`/Matrix`/`/BBox` and every other key
  preserved verbatim), registered under a fresh `/XObject` name
  (`KasFm{n}`, deterministic and collision-checked against the
  page's effective `/XObject` dict), and only that `Do` is repointed
  in the rewritten page stream. Two `Do`s of the same form — or two
  resource names for the same object — are *distinct invocations*
  (keyed by the call-site `op_index` in `form_path`), so each clones
  its own copy; the unedited call keeps the original object, and
  every other page sharing it keeps the unmodified bytes. A form's
  own `/Resources` replaces the caller's per §8.10.1; a form
  *without* `/Resources` inherits them, and the clone reaches the
  same fonts because the names live in the page's effective dict.
- **Do-level eligibility**: a `Do` inside a form (`form_path`
  depth ≥ 2 anywhere in the walk) refuses — nested XObjects are out
  of scope. `Image` and `Other` targets refuse outright; `Form`
  targets are the invocation records themselves (depth 0, allowed).
  `/Group`/`/OC`/`/Ref` on a form is already a `blocker` from the
  walk and refuses the same way.
- Rewrites `Tj`/`TJ`/`'`/`"` only: a `Tj` losing codes becomes a
  `TJ` whose numeric elements restore the removed advance
  (`-(w0 + (Tc+Tw)·1000/Tfs)` — `Tz` cancels, so `Tfs = 0` or
  `Tz = 0` is refused); a `TJ` keeps its original numeric elements
  and splits string elements around removed runs; `'`/`"` expand to
  `T*` + `Tj` (and `Tw`/`Tc` for `"`) so their side effects survive
  even when the text goes to zero.
- Writes a *new* `/Contents` stream through the same
  `Edit`/`rewrite_overlay` pipeline as `annotate-rect` — the input
  document and its bytes are never modified, and other pages that
  share the original stream keep it untouched. The page's effective
  `/Resources` dict is materialised into the page object when a form
  is cloned, so an inherited or shared resources dictionary is never
  mutated in place.

Scope — a page is *refused* outright rather than half-edited when it
carries: `/Annots`, any `sh`/inline image/`gs`/Pattern colour
space/`ActualText`/OC, a `Do` inside a form, an `Image` or other
non-Form XObject, any rendering mode other than `Tr 0` fill,
`w = 0` hairlines, unsupported or unresolvable fonts, non-finite
CTM, `/UserUnit` ≠ 1, vertical text, or `Tfs`/`Tz` = 0. Every one of
those was already a `blocker`/`uncertain` in the diagnosis — removal
refuses on the first rather than continue past unknown state.

Limitations — by design and explicitly *not* done:

- Nested XObjects (a `Do` inside a form), images, annotation content,
  and forms carrying `/Group`/`/OC`/`/Ref` are out of scope (refused).
- A form's unedited invocations keep the *original* stream — the
  removed bytes still exist in that shared object (and on every
  other page that calls it).
- The original `/Contents` stays in the file when another page
  shares it (page 2's view is unchanged; the removed bytes still
  exist in the shared stream).
- Removed text can still exist elsewhere: `/Info`, XMP, structure
  tree, `ActualText`, embedded files, other pages, or the font's
  own cmap/name tables.
- No guarantee of irreversibility — this is *content editing*, not
  secure redaction.

CLI:

```sh
kasane-pdf experimental-remove-text <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] --output <out.pdf>
```

Same `--rect` convention as `preflight-redaction`; `--output` is
required, input and output must differ (same-file check is the
`refuse_same_file` helper), and the output is written atomically —
a refusal leaves any existing output untouched.
