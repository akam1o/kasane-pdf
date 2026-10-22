# Pre-redaction diagnosis (`diagnose_redaction`)

## Pre-redaction diagnosis (M16, `redact_diag`, CLI `preflight-redaction`)

`diagnose_redaction(&doc, page_index, rects, limits) -> Diagnosis`
inspects one page's content streams *as a reader* and reports what a
future redaction would face. **It never edits the file, and a clean
result is not a "safe to redact" verdict** — it only means the walk
finished and saw no obstacle in the examined scope.

### Input contract

- `rects` — 1..=`max_diag_rects` (256) user-space rectangles
  `(llx, lly, urx, ury)`. Every component must be finite and
  `llx < urx`, `lly < ury`; reversed or degenerate rects are
  `Error::Structure`, never silently normalised or clipped.
- `page_index` — 0-based; out of range is `Error::Structure` naming
  the count.
- `/UserUnit` ≠ 1 on the page is a *blocker* (coordinates aren't the
  default unit); a non-numeric `/UserUnit` is `Error::Structure`.

### What the result means

| Field | Contents |
|---|---|
| `targets` | Painted occurrences localised to page space: each shown character (`Text`), each `/Image` `Do` call (`Image`), each `/Form` `Do` invocation (`Form`), each path painted by a fill/stroke op (`Path`). `bounds` is the conservative axis-aligned bbox; `hits` lists which input rects it intersects (touching edges count). |
| `uncertain` | Occurrences that could not be localised — e.g. a non-finite text/image/form transform. Their position is not guessed. |
| `blockers` | Structures that make exact processing impossible: `/OC` marked content, `/ActualText`, `/ExtGState`, `sh`, `/Pattern` colour space, unsupported `/XObject` subtype, `/UserUnit`, dangling/non-dict annotations. The walk continues past most of them so the caller sees the whole picture. |
| `shared` | Objects invoked from ≥2 sites on this page (e.g. two `Do` names for the same form) *or* listed in another page's `/Resources` — editing them would affect every call site. `shared_scope` reports `pages_scanned`/`pages_total`/`truncated`; a truncated scan is *not* a "not shared" guarantee. |
| `annotations` | `/Annots` entries with `/Subtype`, `/Rect`, `hits` against the input rects, and extra payloads (`/Contents` `/A` `/AP` `/AA` `/Popup` `/Dest`). The `/Rect` is the annotation's frame, not the extent of all data it carries. |
| `unexamined` | Fixed labels for document areas never evaluated: `/Info`/XMP, attachments, form values, structure tree/alt text, JavaScript/actions, font-embedded data, duplicated content on other pages, annotation appearances, link-target text. "Not examined" ≠ "absent". |
| `incomplete` | `Some` when the walk stopped mid-stream — e.g. an unsupported font subtype, a non-finite CTM, an out-of-place path op. `targets`/`blockers` are then *partial*: "the stream could not be fully scanned", never "nothing was found". |

`Diagnosis` deliberately carries no boolean "safe" flag. A clean
report means "walk completed, no blockers in the examined scope",
and nothing more.

### Per-kind bounds semantics (ISO 32000-1 §8, §9)

- **Text** — origin from the same `Trm × CTM` math the renderer
  uses; bounds are the glyph *outline* bbox under that transform
  (`TextBounds::Outline` — ink extent only) or, for
  space/`.notdef`/missing outlines, an estimated character *cell*
  (`TextBounds::Cell` — an estimate, never ink extent). Stroke-mode
  text (`Tr` 1, 2, 5, 6) inflates the bounds by the same stroke
  reach a path uses — a text stroke extends outside the ink bbox
  exactly like a path stroke. Clip-mode text (`Tr` 4–7) is a blocker
  plus `uncertain` — its extent is not estimated. The raw character
  code is reported verbatim even when Unicode is unresolved;
  multi-scalar mappings are not split. A `glyph_id`/`outline`/
  `units_per_em` *error* reports `uncertain` — never a silent cell
  estimate; `LimitExceeded` propagates. Stroke-mode text at `w == 0`
  is a device-pixel hairline — `uncertain`, never a certain target.
- **Image** — the unit square `[0,1]×[0,1]` under the `Do` CTM,
  axis-aligned. This is the *whole image extent* — a conservative
  candidate, never the painted pixels — so rotation/shear/clip
  over-report coverage rather than under-report.
- **Form** — `/BBox` under `Matrix × CTM`; inner occurrences report
  with `Site::form_path` extended so a shared form's each call is
  distinguishable.
- **Path** — the construction ops' points under the CTM, inflated by
  the *stroke reach*: `w/2` for butt/round/square caps and joins,
  `miter_limit × w/2` when miter joins are in effect — scaled by
  `√2 × max(|col₁|, |col₂|)` of the CTM, the provable upper bound on
  a unit vector's stretch. `sqrt(|det|)` — the geometric mean of the
  singular values — **under-reports** non-uniform scaling (a `10 0 0
  0.1` CTM stretches by 10, not 1).
  
  **Hairline** — a `w` of 0 is a *device-pixel* stroke: its coverage
  is resolution-dependent and cannot be bounded in user space, so a
  stroked path (S, s, B, b, B*, b*) at `w == 0` reports as
  `uncertain` — never a certain `Target` whose bounds we would call
  "outside the rect". A fill-only path (`f`, `f*`) is unaffected —
  it does not stroke. For fill+stroke ops (`B`, `b`, `B*`, `b*`) the
  *whole* occurrence reports uncertain: the fill's bounds are known
  but the stroke's un-evaluated reach must not be dropped from the
  same occurrence.

### Graphics-state tracking

`DiagWalker` mirrors the renderer's lifecycle but the state it
tracks is the *whole* graphics state the diagnosis needs — CTM,
clip rect, stroke parameters (`w`/`J`/`j`/`M`), and the text state
parameters (`Tc`/`Tw`/`Tz`/`TL`/`Ts`/`Tr`/`Tf`, Table 52). `q`/`Q`
save and restore all of it; a `Form` `Do` pushes and pops the same
struct so `w`/`Tf`/`Tc` changes inside a form never leak to the
page level. `Tm`/`Tlm`/`in_text` are *not* graphics state — they
live on the walker inside `BT`/`ET`.

An uninterpreted state-changing construct (`gs` ExtGState, `sh`
shading, a Pattern colour space) is recorded as a `blocker` **and**
marks `uncertain_state`: every geometry op after it is reported as
`uncertain` rather than a certain `Target`, until `Q` or a form's
end restores known state. (`BX…EX` `Unknown` ops do *not* taint
state — §7.8.2 says they are ignored by definition.)

### Site identity

`Site` carries `contents` (the `/Contents` element refs — every
element when the array is used, since `page_operations` treats it
as one logical stream), `op_index` (the parsed op index in that
logical stream — for form-nested ops, the index *inside* the form's
stream), `form_path` (outermost-first [`FormFrame`] chain — each
frame has the resource `name`, resolved `xobj` ref, and the
call-site `op_index`, so two `Do`s of the same object or the same
name for two objects are distinct sites), and `code_index`.

`code_index` is the character's position in the *code stream of the
op*, counted per shown character code (a 1- or 2-byte code per the
font's CMap — not a Unicode scalar count and not a byte offset). For
a `Tj`/`'`/`"` op it counts the codes in that one string, restarting
at 0. For a `TJ` op it counts across the *whole array* —
`[(A) 100 (B)]` reports 'A' at index 0 and 'B' at index 1 — so two
characters that share one `op_index` stay distinct sites. Numeric
`TJ` elements produce no codes and do not advance the index. A code
that maps to several Unicode scalars still reports under its own
single `code_index` — the code, not its expansion, is the identity.

### CLI

```
kasane-pdf preflight-redaction <file.pdf> <page> \
  --rect <llx> <lly> <urx> <ury> [--rect ...] [--format text|json]
```

`--rect` is lower-left x,y then upper-right x,y in PDF user space —
the same un-rotated, un-cropped convention as `inspect` and
`extract_text`. Exit is `0` on a completed diagnosis *with or
without* blockers, `1` on a typed error (bad input, unreadable
content), `2` on usage errors. Output lines/state are deterministic
and capped at 1_000 entries per category; `incomplete`/`truncated`
are always stated, never silent.

The name is `preflight-redaction` precisely because this is not a
redaction engine and produces no safety guarantee — see
`src/redact_diag.rs`'s module doc.
