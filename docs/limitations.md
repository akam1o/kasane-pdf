# Limitations

What kasane-pdf does **not** do today, collected from the feature
files under [`features/`](features/) (each refusal there is a typed
error — nothing in this list is a silent skip). Items marked *planned*
are on the [roadmap](architecture.md#roadmap).

## Documents

- **Encryption**: any document with `/Encrypt` in its trailer is
  refused at load (`Error::Encrypted`); the `/Crypt` filter is
  refused as well. *Planned.*
- **Strict parsing**: `Document::from_bytes` follows ISO 32000-1.
  Dangling references, a wrong `startxref`, a damaged cross-reference
  table, objects not closed by `endobj` and similar faults are errors.
  `Document::from_bytes_recovering` rebuilds the cross-reference table
  by scanning and reports every assumption. A reference to an
  *undefined* object (no cross-reference entry, or a free one) is null
  in both modes, as §7.3.10 requires, and recorded as
  `Warning::UndefinedReference`; a reference whose entry points at
  damage is still an error. Recovery mode additionally applies the
  `RecoveryRule`s (US Letter for a page without `/MediaBox`, a stray
  `Q` ignored, an unknown operator dropped, a state or colour operator
  inside a path object applied, an invalid `/Annots` entry skipped),
  each recorded as `Warning::RecoveryApplied`; strict mode refuses
  every one of them.
- **Accepted with warnings** is a narrow third outcome, not a lenient
  mode: only deviations the engine can process *exactly* are
  recorded on the `Document` as a `Warning` instead of refused
  (today: a Flate stream missing its final block after a sync flush,
  a malformed `/ToUnicode` entry). Everything else that is not
  conforming is still an error in strict mode.
- **PDF 2.0**: files are read; features new in 2.0 are not interpreted
  and the rewriter refuses a document that declares version 2.0.
- `Document` holds interior caches (`Cell`/`RefCell`) and is not
  `Sync`; use one instance per thread.

## Rendering

- **Knockout transparency groups** (`/K true`) are rendered per
  §11.4.8 (each object against the group's initial backdrop); the one
  inexactness is a nested form whose own ExtGState changes its opacity
  — its shape is taken with those changes. Bounded by
  `max_knockout_objects`.
- **Shading pattern `/ExtGState`** override — refused.
- **Non-embedded fonts**: Symbol and ZapfDingbats draw with a bundled
  DejaVu Sans subset through their built-in encodings and the Adobe
  AFM widths (`SubstKind::SymbolFallback`) — glyph shapes differ from
  Adobe's and the substitution is always reported; non-embedded CID fonts
  are refused unless the `cjk-fallback` feature is enabled and the
  ordering is Adobe-Japan1 (GB1 / CNS1 / Korea1 are not bundled;
  `/Ordering (Identity)` can never be substituted soundly). Symbolic
  simple fonts with unknown names are refused. Substitutes change
  glyph shapes and are always reported.
- **Type 3 fonts in text clipping modes** (`Tr 4`–`7`) — refused.
- **Images**: `/Matte` (pre-blended soft masks), `/SMaskInData` on a
  non-JPX image, `/OC` on an image, `/Ref` — refused.
- **Colour**: no colour management beyond exact matrix/TRC ICC
  profiles; LUT, CMYK and Lab-PCS ICC profiles fall back to the
  `/Alternate` space and are reported in `color_approximations`.
  Non-`/Default` transfer functions, black generation and undercolour
  removal are refused; halftones and smoothness are accepted and
  ignored (no effect on this rasteriser). DeviceCMYK uses the naive
  §10.4.3 formula.
- **Text**: no hinting, no shaping (ligature substitution, kerning
  from GPOS); glyphs are placed exactly where the content stream
  puts them.
- Mesh shadings interpolate colours in sRGB after conversion, not in
  the source space; function-based shadings are sampled on a 128×128
  grid. Both are recorded as approximations in the feature file.

## Text extraction

- Positioned characters only: no layout analysis, reading order,
  word or line grouping, or search.
- A code the font cannot map to Unicode is reported as `None`, never
  guessed — including codes whose `/ToUnicode` entry was malformed
  and skipped (recorded as `Warning::MalformedToUnicodeEntry`). Text
  drawn through optional content that is OFF is excluded;
  `/ActualText` replaces the drawn codes.

## Writing and editing

- **Rewrite** produces one flat revision: superseded objects, earlier
  trailers and the incremental-update history are not carried; a
  signed document is refused; XFA, `/OCProperties`, RichMedia and
  document-level actions are refused rather than carried. Preserved
  objects are carried verbatim — the rewriter is **not a sanitizer**.
- **Incremental save** cannot delete bytes (a removed annotation's
  object stays in the file), refuses recovered documents, encrypted
  documents and non-zero-generation references, and only appends the
  edits the `Edit` session offers (metadata, annotations, form-field
  values).
- **Signatures** are detected, not verified: no digest or certificate
  check, no `/FieldMDP` evaluation (a document with one is refused),
  no `/UR` usage rights, no timestamp or PAdES structures. The engine
  never reports a signature as valid.
- **Page-level editing** (insert, delete, reorder), link editing and
  other annotation subtypes — *planned*.
- The **experimental `remove_*` editors** are content editors: they
  rewrite selected operators and pixels into new objects and report
  what they changed. They do not prove the data is gone and are not
  a redaction tool; see [`security.md`](security.md).
- Output durability: the CLI writes atomically but does not fsync the
  parent directory.

## Platform

- Pure Rust library and CLI. No C API, no mobile or desktop
  application packaging — *planned, not implemented*.
