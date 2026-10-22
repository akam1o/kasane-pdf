# Coordinate transforms (`Matrix`)

## Matrices (ISO 32000-1 §8.3)

`Matrix` is a `[a b c d e f]` affine transform with `transform`,
`concat` (PDF `cm` order: `existing.concat(cm)` = `cm × existing`),
`translate`/`scale`/`rotate` helpers, and `is_finite` checking. The
page-level `CropBox`/`Rotate` → pixmap mapping is built inside
`render_page` (see `src/render.rs`), not to be confused with the
content-level `cm` operator.

### Singular / non-finite transforms

- **Non-finite `cm` operands** → `Error::Structure` ("cm operands must
  be finite"). PDF number syntax cannot express `1e999`, so such
  input is rejected at the lexer before `cm` runs; the check is a
  defensive guard.
- **Singular CTM** (finite elements, determinant exactly 0 — e.g.
  `0 0 0 0 e f cm`): ISO 32000-1 does not define painting under a
  non-invertible transform. We **refuse the page** with
  `Error::Unsupported` at paint time rather than silently producing
  nothing — a degenerate transform cannot be honoured faithfully.
  Poppler warns "matrix not invertible" and paints a fallback; both
  are unspecified-behaviour choices.
- **Near-singular but invertible CTM** (e.g. det = 1e-9, or
  `1000 0 0 0.001` anisotropic): rendered normally — we only reject
  the exact `det == 0` case, no epsilon cutoff.
- The check runs on the content-level CTM (`cm` accumulation), not
  the fixed page→pixmap transform — page geometry is built
  separately and is always invertible for valid page boxes.
