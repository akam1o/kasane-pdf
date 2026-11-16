# Recovery mode

`Document::from_bytes_recovering` (or `OpenOptions::with_recovery(true)`)
reads files that ISO 32000 forbids. It rebuilds a damaged
cross-reference table and applies the recovery rules
(`RecoveryRule`, recorded as `Warning::RecoveryApplied`) to the
constructs the standard does not allow. Strict mode (`Document::from_bytes`)
applies none of them, and refuses the same files exactly as before any
rule existed.

Drawing a file in recovery mode and redacting it safely are different
claims. Every rule states what redaction does with what it recovered;
the policy as a whole is
[`redaction-safety-model.md`](redaction-safety-model.md) § 2.8, and
`tests/redact_recovery.rs` holds a case for each rule. A rule is added
together with its cases there (R0 in
[`compatibility-roadmap.md`](compatibility-roadmap.md)).

## Rules

| rule | the construct | read as | region redaction |
|---|---|---|---|
| `MediaBoxDefault` | a page without `/MediaBox` on itself or any ancestor | US Letter | refused (the page box is inferred) |
| `IgnoredRestoreWithoutSave` | a `Q` with no saved state | ignored | allowed: every major reader ignores it, and the walker sees the whole page |
| `IgnoredUnknownOperator` | an operator kasane does not know, outside `BX`/`EX` | kept as `Operation::Unknown`, not drawn | refused (what it draws is unknown) |
| `OperatorInsidePathObject` | a state or colour operator between path construction and painting | applied where it stands | refused |
| `IgnoredInvalidAnnotation` | an `/Annots` entry that is not a dictionary, an appearance state that is not a stream | skipped | refused when the annotation meets a rect |
| `DefaultFontSubstituted` | a font resource that is missing or not a dictionary | Helvetica | refused |
| `IgnoredUndefinedProperty` | a `BDC` property name not in `/Properties` | plain marked content | refused |
| `IgnoredInvalidEncodingEntry` | a `/Differences` element that is neither an integer nor a name | skipped | allowed only when the font draws nothing that stays in the output |
| `IgnoredInvalidSoftMask` | an image `/SMask` that is not a stream | no soft mask | refused |
| `PaddedShortEncryptionKey` | a revision-4 key shorter than 128 bits | zero-padded, as Acrobat does | refused (no writer encrypts) |
| `IgnoredWrongOperandCount` (R1, v0.11) | an operator with the wrong number of operands | see below | refused |
| `UnclippedFormWithoutBBox` (R2, v0.11) | a form XObject without `/BBox`, or with `/BBox null` | drawn without the clip | refused |
| `FunctionExtraInputsIgnored` (R3, v0.11) | a type 2 or 3 function whose `/Domain` gives more than one input | the first input used | as strict mode (see below) |
| `ShadingOutputsTruncated` (R3, v0.11) | a shading function with more outputs than the colour space has components | the first outputs used | as strict mode (see below) |

## R1 — operators with the wrong number of operands

**The construct.** An operator with a fixed number of operands, given
too few or too many (`0 0 10 re`, `9 1 0 0 1 0 0 cm`, `BT 12 Tf`).
Strict mode refuses the content stream: `Error::Parse("operator 're'
expects 4 operand(s), found 3")`.

**Readers disagree.** Measured on synthetic pages with Poppler 26.10
and MuPDF 1.28:
- **Too many operands.** Poppler uses the last operands and MuPDF the
  first: `7 60 100 Td` places the text at x 60 in Poppler and x 7 in
  MuPDF.
- **Too few operands.** Poppler skips the operator, and its text
  output for the page is empty. MuPDF draws with zeros for the missing
  operands: `2 0 0 2 0 cm` collapses what follows.

No reading is shared, so none is adopted.

**Recovery mode.** The operator is kept, uninterpreted, as
`Operation::Unknown { operator, operands }`, with every operand it was
given; nothing is completed or truncated. The renderer and the text
extractor skip it, and `IgnoredWrongOperandCount` records the operator,
the expected and found counts, and the byte offset. What depends on it
is not invented either:
- an `l` after a dropped `m` has no current point;
- a `Tj` after a dropped `Tf` has no font.

Such a page still fails to render with that error. It is not drawn
under a guessed state.

**Not recovered** (an error in both modes):
- **Block operators.** `q Q BT ET BMC BDC EMC BX EX` with any operand:
  dropping one would change the nesting of everything that follows.
- **Variable-arity colour operators.** `SC SCN sc scn`, whose arity
  depends on the colour space.
- **The right number of operands of the wrong type**, e.g. `(a) 2 m`.
- **Operands left over** before an inline image (`BI`) or at the end of
  the stream.

**Inside `BX … EX`** the same rule applies in recovery mode. Strict mode
still counts the operands of a known operator there.

**Region redaction** refuses every page with such an operator
(*region removal on a page with uncertain content*). This holds even far
from the rects: what an uninterpreted operator would have drawn or set
cannot be bounded. Some pages are refused earlier, by the error that
follows a dropped operator.

**Known limits.**
- A page whose content needs the dropped operator to render (a path
  without its `m`, text without its `Tf`) still does not render.
- The drawing of a page that does render may differ from every other
  reader's, which disagree among themselves.

## R2 — form XObjects without `/BBox`

**The construct.** A form XObject without the required `/BBox`
(ISO 32000-1 Table 93), or with `/BBox null`. Strict mode refuses the
page: `Error::Structure("form XObject has no /BBox")`.

**Readers disagree.** Measured on synthetic pages with Poppler 26.10
and MuPDF 1.28:
- MuPDF draws the form without a clip, with its `/Matrix`.
- Poppler reports *Bad form bounding box* and draws nothing of the
  form.

**Recovery mode.** A form painted by `Do` (from a page, another form, a
pattern or a Type 3 glyph) or used as a soft-mask group is drawn
without the clip a `/BBox` would set, as MuPDF draws it. Its `/Matrix` still applies, and so does every
clip around the `Do`, including an enclosing form's `/BBox`.
`UnclippedFormWithoutBBox` records the form's object number. Nothing
of the form is dropped, and no box is invented. An annotation
appearance without `/BBox` is not covered by this rule: it has no
placement on the page (Algorithm 8.1 maps the box to `/Rect`), and it
is skipped as before (`IgnoredInvalidAnnotation`).

**Not recovered** (an error in both modes): a `/BBox` that is present
but is not a rectangle of four finite numbers. MuPDF does not draw such
a form either.

**Region redaction** refuses every page that draws such a form, in both
modes and wherever the form draws. A form painted by `Do` is refused
with the strict cause (*form XObject has no /BBox*). A soft-mask group
or a Type 3 glyph's form is refused earlier, by the refusal those
constructs already have. The redaction geometry bounds a form by its `/BBox`. It
does not invent one, so the form's extent stays unknown.

**Known limits.** A form drawn unclipped may show more than a reader
that clips would show. Poppler shows nothing of it.

## R3 — function input and shading output counts

**The constructs.**
- **Extra inputs.** A type 2 (exponential) or type 3 (stitching)
  function takes one input (§7.10.3, § 7.10.4), but its `/Domain` gives
  more.
- **Extra outputs.** A shading whose function yields more outputs than
  its colour space has components (§ 8.7.4.5).

Strict mode refuses both (*type 2 function takes exactly one input*,
*shading /Function yields 3 outputs, colour space has 1 components*).

**Readers disagree.** Measured on synthetic pages with Poppler 26.10
and MuPDF 1.28:
- **Extra inputs.** MuPDF uses the first input. Poppler reports
  *function with more than one input* and draws nothing of the shading.
- **Extra outputs.** MuPDF uses the first outputs. Poppler reports
  *function with wrong output size* and draws nothing.
- **Too few outputs.** Poppler draws nothing. MuPDF fills the missing
  components with each function type's own defaults (for a type 2
  function, the defaults of `/C0` and `/C1`).

**Recovery mode.**
- **Extra inputs.** The first input is used and the rest of `/Domain`
  is ignored (`FunctionExtraInputsIgnored`).
- **Extra outputs.** The first outputs are used, as many as the colour
  space has components (`ShadingOutputsTruncated`).

Both are what MuPDF draws.

**Not recovered** (an error in both modes): a shading function with
*fewer* outputs than components. The roadmap proposed padding with 0.
No value is defined for the missing components, and MuPDF does not pad
with 0 either, so no reading is shared and none is invented. A function
with too few inputs for its use (a function-based shading needs two)
stays an error too.

**Region redaction** is unchanged. It does not evaluate these
functions to place anything. A shading meeting a rect is removed or cut
by its geometry, and strict mode already redacts such a page (the
render it cannot do is not needed). Both modes give the same output,
and `tests/redact_recovery.rs` checks that the two variants of each
case are byte-identical.

**Known limits.** The colours drawn are MuPDF's reading. Poppler draws
nothing of such a shading.
