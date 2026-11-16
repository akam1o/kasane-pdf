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
