# Changelog

All notable changes to this project are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

Rendering robustness (plan: `docs/plans/v0.5.1.md`). No public API
change.

### Changed
- `Limits::default().max_xobject_depth` is **48** (was 32), from a
  stack measurement of every nesting path (forms, transparency groups,
  soft-mask groups, annotation appearances, Type 3 glyphs): at 48 each
  needs less stack than 0.5.0 needed at 32 — at most ≈ 1.2 MiB in a
  debug build, ≈ 0.25 MiB in release — so the default fits a 2 MiB
  thread. Documents nested 33–48 levels deep now render instead of
  `LimitExceeded` (`diagnose_redaction` and the region removers follow
  them too). A caller's own limit is unchanged; raising it costs
  ≈ 25 KiB (debug) / 5 KiB (release) of stack per level.
- The content interpreter dispatches the operators that re-enter it
  (`Do`, `gs`, text showing, path painting, inline images) from a small
  stack frame,
  cutting the stack per nesting level by 40–60 % in debug builds and
  10–30 % in release. No rendering change.
- The clip and soft masks held by the graphics state (one byte per
  page pixel each) are bounded: those alive at once may take at most
  `max_render_buffer_bytes`, an allowance of their own beside the page
  and its layers; beyond it, `LimitExceeded`. Shared masks (below) make
  this reachable only by many *distinct* nested clips at a high
  resolution — e.g. 32 nested forms with ever smaller BBoxes at 600 dpi,
  which 0.5.0 drew with ≈ 0.9 GB.

### Fixed
- Memory: every `q`, form, group and pattern cell copied the clip mask
  — `re W n` followed by 1000 `q` cost 1000 page-sized masks (2.2 GB at
  600 dpi on a 600 pt page; tens of GB at the default pixel cap), with
  no limit. A saved state now shares the masks, a clip that cuts
  nothing away (such as a form BBox enclosing the clip) keeps the mask,
  and the masks are bounded (above). The same 1000 `q`: 119 MB.
  Present since 0.1.
- **Dashed strokes could take minutes.** Round or square caps on a
  dash pattern along a long path made the rasteriser's work grow with
  the square of the edges per row: a 15 KB content stream (250 000
  round-capped dashes) took 63 s in 0.5.0 at width 100 and over 170 s
  at width 600 (release, 600 pt page at 72 dpi). A dashed stroke's work
  is now estimated before drawing — Σ over rows of (edges of the dash
  segments reaching the row)², per stroke and summed over the page, and
  all segments' edges — and a stroke over a bound is refused with
  `Error::Unsupported` ("dashed stroke"), never approximated; the
  slowest stroke still drawn takes ≈ 0.7 s, a page's dashed strokes
  together ≈ 10 s at most. A
  stroke that covers the page from every point through round or square
  caps draws one segment (every pixel fully covered, as by all of
  them). Other dashed strokes draw exactly as before. Present since
  0.1; found by the nightly fuzzing.
- A stroke far wider than the page (e.g. `99999999999999999999 w`)
  panicked inside the rasteriser. A stroke wider than 2^24 device
  pixels made of straight segments with round joins or miters within
  their limit, covering the page from every point, is now narrowed to
  a covering width (the same coverage, up to the rasteriser's rounding
  at a far edge); any other is refused with `Error::Unsupported`
  ("stroke width"). Present since 0.1; found by fuzzing.
- Geometry reaching about 5 × 10⁸ device pixels panicked inside the
  rasteriser. A path beyond ±2^24 device pixels is now skipped when it
  lies entirely off the page (a clip there leaves nothing visible) and
  refused with `Error::Unsupported` ("geometry extent") otherwise.
  Present since 0.1; found by fuzzing.

### Added
- Fuzz targets `render_content` (a valid page with every resource kind
  the renderer draws, its content stream generated from the input) and
  `render_graph` (page trees and form graphs with cycles, sharing,
  soft masks, patterns, broken references and object streams), with
  committed seeds (`fuzz/README.md`).
- CI: a render fuzz smoke job on every pull request (seeds plus 3000
  inputs per target, and a check that the seeds' pages paint), a
  Linux depth-and-stack job (debug and release), and a separate nightly
  workflow (`fuzz-nightly.yml`, 15 minutes per target, with a
  self-test that a crash, a timeout and an out-of-memory each fail it).
  All of them run the targets through `fuzz/ci-run.sh`, which fails on
  any input libFuzzer saves, not only on its exit status.

## [0.5.0] - 2026-11-05

`rewrite` now writes documents it refused before — PDF 2.0, XFA forms,
references to undefined objects (*Changed*). A document it wrote before
is written as before, unless a refusal check had missed it (*Fixed*: it
is now refused) or its trailer pushes it past `max_output_bytes`. The
new `rewrite_with` adds explicit policies: carrying executable content
and removing usage-rights signatures, each opt-in and reported. Every public API change is
additive, on `#[non_exhaustive]` types (`cargo semver-checks` against
0.4.1: no semver update required); the minor version is for the
behaviour changes under *Changed*. `rewrite_with` with the default
options gives exactly `rewrite`'s bytes and errors. Release notes:
`docs/releases/v0.5.0.md`.

### Added
- `rewrite_with(&Document, &RewriteOptions) -> Result<RewriteOutput, Error>`:
  the full rewrite under explicit policies, with a report of the
  executable content it carried. `RewriteOptions` (`#[non_exhaustive]`,
  `Default`, `with_*`) holds `ExecutableContentPolicy` (`Profile`, the
  default; `RefuseScriptsAndExternal`; `Carry`), `MultimediaPolicy`
  (`Refuse`), `UsageRightsPolicy` (`Refuse`, the default; `Remove`,
  below) and `Limits`. With the defaults the bytes and errors are
  exactly `rewrite`'s.
  - `Carry` carries document-level scripts (catalog `/AA`, a JavaScript
    `/OpenAction`, `/Names /JavaScript`) verbatim. It executes nothing
    and is **not sanitizing**.
  - `RefuseScriptsAndExternal` refuses every script, external or
    unknown action wherever it is, and XFA scripts.
  - The catalog `/RichMedia` and every signature refusal hold under
    every policy.
  - `RewriteOutput` (`#[must_use]`) gives the bytes and a
    `RewriteReport`. The report lists every carried action, multimedia
    annotation and XFA script (`ExecutableItem`: class by effect, kind,
    positional owner, `/Next` position, input and output object, script
    size, sharing), the structures that could not be classified, and
    the version change. It holds no script text, URLs, field names,
    name-tree keys or digests.
- `UsageRightsPolicy::Remove` (the default stays `Refuse`): `rewrite_with`
  deletes `/Perms /UR` and `/UR3` usage-rights signatures — and with
  them the extended rights they grant in a reader — dropping an empty
  `/Perms` and clearing `/AcroForm /SigFlags` bit 1 (every other bit
  kept). It refuses (`Error::SignaturePolicy`) a document with any
  other signature-like dictionary anywhere reachable (by structural
  markers, not handler names), a field `/Lock`, `/DSS` or `/Legal`, an
  XFA XML signature, a usage-rights signature that cannot be confirmed
  or is referenced from anywhere but `/Perms`, or a graph that cannot be
  read or searched completely. The output is reparsed and checked by
  walkers of its own before it is returned (no signature marker, no
  `/ByteRange`, every object reachable, exactly the declared removal);
  a failure returns no bytes. Nothing is verified or re-signed.
  - `RewriteReport::usage_rights() -> &UsageRightsOutcome`
    (`NotRequested`, `NonePresent`, `Removed(UsageRightsRemoval)`) and
    `signatures_removed()`. `UsageRightsRemoval` lists each removed
    signature (`RemovedSignature`: key, input object, `SubFilterKind`,
    the `UsageRight` categories, `/ByteRange` present), whether `/Perms`
    was dropped, the `SigFlagsChange`, seed values carried, the input's
    revisions and the `OutputCheck` counts. Content-free.
  - New `SignatureRule` variants: `UsageRightsNotRemovable`,
    `OtherSignaturePresent`, `SecurityStore`, `ResidualSignature`.
- `Error::refusal_reason() -> Option<RefusalReason>`: the policy
  refusals of the writers by class (signature, encryption,
  document-level script, script, external or unknown action,
  unclassified structure, multimedia), without reading messages.
- `examples/remove_usage_rights.rs`.

### Changed
- `rewrite`: a document that declares PDF 2.0 (header or catalog
  `/Version`) is written as 2.0 instead of refused, with the trailer
  `/ID` 2.0 requires (ISO 32000-2 Table 15): the input's permanent first
  identifier, the second the digest of the written body (deterministic;
  not carried by `remove_document_metadata`). Its structures — page
  `/Metadata`, associated files and embedded files, document parts —
  are carried as objects with references renumbered and sharing kept.
  A version above 2.0 is still refused; 1.x output is unchanged (no
  `/ID`). `max_output_bytes` now covers the whole written file, the
  trailer included.
- `rewrite`: a reference to an undefined object (ISO 32000-1 §7.3.10:
  no in-use or compressed cross-reference entry for that number and
  generation — absent, free, or another generation) is a reference to
  null. The reference is kept and points at a new indirect object
  whose body is `null` (one per undefined target), so array positions,
  dictionary keys and sharing are unchanged and the output has no
  dangling reference; each is recorded as
  `Warning::UndefinedReference`. Such documents were refused with
  `MissingReference`. A defined object that cannot be read (damaged
  body, missing object-stream member, reference cycle, limit) still
  fails the rewrite.
- `rewrite`: XFA forms are carried verbatim instead of refused — the
  `/XFA` stream or packet array (names, order, duplicates, stream bytes
  and filters) and `/NeedsRendering`, with renumbered references;
  nothing is parsed or regenerated. An XFA packet holding an XML
  signature — the XML-DSig namespace in a packet or in the packets
  together (a character reference, declaration or DTD can straddle
  two), character references expanded, UTF-8, UTF-16 or another
  ASCII-compatible encoding — is refused with `Error::SignaturePolicy`;
  a packet that cannot be read completely to check (filter, limits,
  external data, UTF-32 or another encoding that is not
  ASCII-compatible, a DTD) is `Error::Unsupported`. The `/Perms`
  (`/DocMDP`, `/UR`, `/UR3`) and signed-field refusals are unchanged.

### Fixed
- `rewrite`: the refusal checks (signatures, `/Perms`, scripts, XFA)
  missed an object reached through a compressed object whose value is
  a reference — `Document::deref` returns that reference — so, for
  example, a `/Perms /UR3` dictionary behind one was rewritten. They now
  follow such references to the object (64 hops at most, then a cycle
  error); the resolutions count against `max_reference_depth` like
  any other.
- `audit_residuals`: a reference chain's intermediate links (`5 0 obj
  6 0 R endobj`) were reported as unreachable objects; the reachability
  walk now resolves one hop at a time and reaches every link.
  A reference chain that closes on itself is still `CycleDetected`.

### Testing and tooling (no library change)
- Fuzz targets `rewrite_graph` (hostile object graphs: undefined, free
  and other-generation references, chains, cycles, XFA packet arrays,
  usage-rights signatures; model-based oracle), `rewrite_policy`
  (action graphs under every executable-content policy, with `Remove`
  too) and `signature_markers` (`Remove` against a ground truth of
  signature markers, near misses and placements).
- Compatibility runner: semantic round-trip categories for XFA,
  document / page / annotation / field actions, `/Names /JavaScript`,
  metadata, viewer preferences, PDF 2.0 (effective version, page
  `/Metadata`, associated files, document parts), multimedia and
  signatures; declared transformations (`transformed`, never counted as
  `pass`: T1 version raised, T2 usage rights removed); every rewrite
  configuration (`carry`, `remove-usage-rights`,
  `carry,remove-usage-rights`) measured in its own namespace, with the
  report checked against the runner's own inventory and signature
  reading; text agreement with annotation appearance text. Report only.
- `scripts/ci-regression.py`: figures per configuration; per-file
  changes of a configuration listed for review (the default stays the
  only enforced run). `scripts/usage-rights-audit.py`: qpdf and pypdf
  re-read input/output pairs for every signature marker (content-free;
  `--self-test` on a synthetic form).

## [0.4.1] - 2026-11-02

Patch release: cross-reference fixes only (`cargo semver-checks`
against 0.4.0: no semver update required). Release notes:
`docs/releases/v0.4.1.md`.

### Fixed
- **Cross-reference precedence across incremental updates** (ISO
  32000-1 §7.5.4, §7.5.6, §7.5.8; ISO 32000-2 §7.5.8.4). Per object
  number, the newest update section that lists it now decides — in use
  at a generation, compressed, or free — and older sections never
  override it. Before:
  - an update appended to a hybrid-reference file was ignored for every
    object the older section's `/XRefStm` stream listed (kasane read the
    old object while qpdf, Poppler and Quartz read the new one) — also
    after kasane's own `Edit::save_incremental`, and in `rewrite`;
  - an object freed by a newer section with the next generation (the
    §7.5.4 way to delete it) was still resolved from the older section;
  - xref-stream entries lost their generation (field 3), so `n g R` with
    `g > 0` did not resolve;
  - within one hybrid section the `/XRefStm` stream replaced the table's
    entries; the table is now searched first and the stream supplies
    only numbers the table does not list (qpdf, Poppler and Quartz agree
    when both list a number in use).
- Generations above 65535 on in-use entries, and xref-stream
  object-stream numbers or indices above 2³²−1, are refused instead of
  truncated onto another object; xref and object offsets are checked
  before conversion, so a 32-bit (wasm32) build cannot wrap an offset
  past 4 GiB onto an unrelated position. `/XRefStm` must point at a
  cross-reference stream and is honoured only in classic trailers.
- No public API change.

## [0.4.0] - 2026-11-01

Every public API change is additive on `#[non_exhaustive]` types
(`cargo semver-checks` against 0.3.1: no semver update required); the
minor version is for the behaviour changes below. Release notes:
`docs/releases/v0.4.0.md`.

### Added
- **Text without `/ToUnicode`** (text extraction only; glyph selection
  never uses it). Codes a font's `/ToUnicode` does not cover used to
  resolve only through a 216-name glyph table for simple fonts. Now:
  - every simple font, Type 3 included, maps encoding glyph names
    through the full Adobe Glyph List and Adobe's glyph-name rules
    (suffixes, ligature components, `uniXXXX…`, `uXXXX[XX]`; a name
    maps only when every part does; never private-use, controls or
    noncharacters) after the old table, which still decides for the
    names it knows — no earlier result changes;
  - a CFF program used through its built-in encoding names the glyph in
    its charset;
  - `ResolveKind::FontProgram`: the embedded program's Unicode `cmap`
    read backwards for the selected glyph (unambiguous glyphs only; a
    few equivalents such as space / no-break space decide), else its
    own glyph name (`post`, name-keyed CFF); never for a substituted
    face; bounded (`cmap`s covering more than 131 072 code points, or
    with unreadable headers, are not read backwards);
  - `ResolveKind::CharacterCollection`: Type 0 fonts of the Adobe
    registry map CIDs through `Adobe-<ordering>-UCS2` (§9.10.2 method
    3) — Adobe-Japan1 with the `cjk-fallback` feature; when the
    embedded program disagrees the code stays unresolved.
  `/ToUnicode`, when present, still decides alone. The Adobe Glyph List
  (BSD-3-Clause) is bundled as `src/agl.rs` (see NOTICE).
- **Annotation and form-field text, opt-in**:
  `TextOptions::with_annotations(true)` fills `ExtractedText::
  annotations` with one `AnnotationText` per annotation (`/Annots`
  order, each object once, popups with their parent): `/Contents`,
  `/RC` as stored, for widgets a `FieldText` (fully qualified name,
  `/FT`, `/V` and `/DV` as text — choices through `/Opt` to their
  display text — the value on the field's first widget only), and the
  characters the normal appearance draws, placed on the page.
  `AnnotationText::text()` picks one string by priority (field value,
  else `/Contents`, else the appearance) so nothing is counted twice.
  Default off: `chars` and everything else are unchanged.

### Changed
- **`rewrite` carries optional content.** A catalog `/OCProperties` was
  refused as outside the rewrite profile; a well-formed one is now
  carried verbatim with renumbered references — every group, the
  default and alternate configurations (`/BaseState`, `/ON`, `/OFF`,
  `/Order`, `/RBGroups`, `/Locked`, `/ListMode`, `/AS`, `/Intent`) and
  every content-level reference (page `/Properties`, XObject and
  annotation `/OC`, OCMDs with `/P` and `/VE`). Unknown keys are kept.
  A malformed one — missing `/OCGs` or `/D`, a group that is not an
  `/OCG` dictionary with a `/Name`, a wrong type, an `/Order` that
  contains itself, a usage application without `/Event` or
  `/Category` — is refused with `Error::Structure`; an `/Order` nested
  deeper than 32 levels or more than 262 144 configuration entries
  with `LimitExceeded`. Indirect values and `null` array entries are
  accepted (§7.3.10). A document that carries optional content is
  written with a header of at least `%PDF-1.5` (the larger of header
  and catalog `/Version`, then that floor; 2.0 is still refused); an
  incremental save never changes the header.
- **A new refusal: `max_pattern_tiles` on overlapping tiling patterns
  in Type 3 glyphs.** One corpus document (a Type 3 glyph showing text
  in a glyph-space tiling pattern) that rendered *wrongly* before now
  renders correctly only with 4 866 cell executions and is refused with
  `LimitExceeded { limit: "max_pattern_tiles" }` at the default 4 096;
  raise the limit to render it.
- **Pages that name a non-Normal blend mode** are composited on a
  transparent page group (below): their pixels may differ by ±1–2 levels
  where nothing blends.

### Fixed
- **Blend modes against the page were computed over white.** §11.4.7
  makes the page group isolated: objects composite against a
  *transparent* backdrop and the result is put on white paper at the
  end. Painting directly on white made every non-Normal blend mode
  wrong where nothing had been painted before (a group drawn with
  `/Difference` over the bare page came out in the complementary
  colour; `/Screen`, `/Lighten`, `/ColorDodge`, the non-separable modes
  … came out white). A page whose resources (forms, patterns, Type 3
  fonts, annotation appearances included) name a blend mode other than
  Normal is now rendered on a transparent backdrop and composited on
  white; other pages are painted exactly as before.
- **`/HardLight` on images, shadings and groups used a wrong formula**
  (the Screen half doubled the backdrop term). It is now
  Screen(b, 2s − 1) as §11.3.5 defines.
- **Stroked text had the wrong line width.** Glyph outlines in the
  stroking text modes (`Tr` 1, 2, 5, 6) were stroked with
  `line width × font size / 1000` in glyph space — about 7× too thin at
  12 pt, far too thick at large sizes or under a scaling text matrix —
  and without the caps, joins and dashes of the graphics state. They are
  now stroked like any path in user space (§9.3.6, §8.5.3.2).
- **A text object inside a Type 3 glyph moved the outer text.** A
  CharProc may hold its own `BT … ET`; its `BT` reset and its `Tj`
  advanced the text and line matrices of the text being shown, so the
  following glyphs of the outer string were drawn at the wrong place.
  Both matrices are now restored after the CharProc.
- **Tiling patterns counted cells that cannot reach the painted area.**
  The cell range of an overlapping (`/BBox` larger than the step)
  pattern was widened by up to two cells per side; those cells were run
  and counted against `max_pattern_tiles`. The range is now exact
  (no pixel changes; fewer cells executed).
- **Annotations with the NoRotate flag were turned with the page.** On a
  page with `/Rotate`, an annotation whose `/F` sets NoRotate (bit 5)
  is now drawn upright: its placed appearance is turned back by the page
  rotation about the upper-left corner of `/Rect` (Poppler's pivot).
  Pages without `/Rotate` and annotations without the flag are
  unchanged. NoZoom needs no change: a render is the page at
  magnification 1 at the requested resolution; there is no separate
  viewer zoom to compensate.
- **CFF fonts with a predefined charset drew `.notdef` for their
  glyphs.** A `/FontFile3 /Type1C` program whose Top DICT selects the
  ISOAdobe charset (operand 0, also the default) has no charset table;
  glyph names were not found in it, so each glyph was drawn as
  `.notdef` on pages reported as exact renders. Names now resolve
  through the predefined charset. The **Expert and ExpertSubset
  charsets are refused** (`Unsupported`) instead of being drawn as
  `.notdef`.
- **A simple font with `/FirstChar` or `/LastChar` outside 0–255
  panicked** (index out of bounds while reading `/Widths`, since 0.1;
  found by fuzzing): a malformed font dictionary crashed the caller
  instead of being handled. Widths for codes a simple font cannot show
  are now skipped and the count saturates; documents that loaded before
  load the same.
- **Every text extraction leaked a small allocation** (a 1 × 1
  placeholder pixmap, ~36 bytes per call, since extraction shared the
  renderer — 0.3.1 included). Freed with the call now.

### Known limitations
- DeviceCMYK is converted with the §10.3.5 formula, without colour
  management: CMYK pages differ from Poppler / Acrobat by ΔE00 ≈ 5–25
  on saturated colours (`docs/limitations.md`).
- Text without `/ToUnicode`: names without glyph-list meaning (`g12`),
  symbol-only `cmap`s, ambiguous glyphs and CID-keyed fonts outside the
  bundled Adobe-Japan1 collection stay unresolved — on purpose; GB1,
  CNS1 and Korea1 have no bundled UCS2 mapping.
- Annotation text: `/RC` is not interpreted; a field's value is read
  per page (a field on several pages reports it on each); appearance
  text requires the annotation's appearance to be interpretable.
- NoZoom has no effect (a render has no separate viewer zoom).
- The compat runner's colour comparison (CIEDE2000) is report-only and
  not part of the library.
- Thin strokes are drawn at their true coverage; Poppler and Acrobat
  widen them to a device pixel, so hairline drawings look lighter.
- Overlapping tiling patterns are executed cell by cell, bounded by
  `max_pattern_tiles` (one corpus document now refused; raise the
  limit to render it).
- Encrypted documents are read only; writers refuse them.

## [0.3.1] - 2026-10-31

Patch release (`cargo semver-checks` against 0.3.0: no semver update
required). Release notes: `docs/releases/v0.3.1.md`.

### Fixed
- **Type 1 fonts with a `/FontMatrix` other than `[0.001 0 0 0.001 0 0]`
  were drawn at the wrong scale and reported as exact renders** (since
  0.1). eexec (`/FontFile`) programs: the matrix was looked up in the
  encrypted Private dictionary, where it does not belong, instead of the
  cleartext font dictionary; CFF (`/FontFile3 /Type1C`) programs: the
  matrix was not read at all (1000 units/em assumed). A 2048-unit font
  came out about twice too large, glyphs overlapping. Glyph outlines now
  go through the program's full `FontMatrix` (scale, shear, non-uniform
  scale), for CID-keyed CFF including the Font DICT matrix `FDSelect`
  assigns (combined with the Top DICT matrix as FreeType does).
  Advances still come from `/Widths`. Text-extraction glyph bounds and
  the redaction diagnosis use the same matrix. A `FontMatrix` that is
  present but not six finite numbers forming an invertible matrix is
  `Error::Structure`; an absent one is still the conventional 0.001.

## [0.3.0] - 2026-10-31

Reading of encrypted documents (Standard security handler). No public
item was removed or changed in signature (`cargo semver-checks`
against 0.2.2: no semver update required); the minor version is for
the observable behaviour changes below. Upgrade notes:
`docs/migration-0.3.md`; user guide: `docs/encryption.md`; release
notes: `docs/releases/v0.3.0.md`.

### Added
- **Encrypted documents** (Standard security handler, ISO 32000-2
  §7.6): revisions 2–6 — RC4 40–128-bit, AES-128 (`/AESV2`), AES-256
  (`/AESV3`, R5 and R6) — with crypt filters, `/Identity`, the `/Crypt`
  stream filter, `/EncryptMetadata false` and unencrypted signature
  `/Contents`. Decryption only; ciphers and hashes from RustCrypto.
- `Document::open(&[u8], &OpenOptions)` with `OpenOptions`
  (`with_limits`, `with_password` — tried as user, then owner password —
  `with_recovery`); `Document::encryption() -> Option<EncryptionInfo>`
  with `CryptMethod`, `Authentication` and the advisory permission bits.
  Passwords are held in zeroed buffers and never appear in `Debug`,
  errors or warnings.
- Passwords as text: revision 5/6 passwords are normalised with
  SASLprep (RFC 4013, `stringprep` crate) and truncated to 127 bytes as
  Algorithm 2.A requires; a non-ASCII UTF-8 password for revisions 2–4
  is also tried in PDFDocEncoding. Every candidate is derived from the
  caller's password.
- `RecoveryRule::PaddedShortEncryptionKey`: a revision-4 key shorter
  than 128 bits whose data decrypts only when the key is zero-padded to
  16 bytes (Acrobat's undocumented reading) is read in recovery mode and
  recorded; strict mode refuses such a file (`Error::Unsupported`)
  instead of returning undecryptable data. Decided from the file's own
  data, so a file written as ISO 32000 specifies reads identically in
  both modes.
- `Warning::OverlongEncryptionEntry`: an `/O` `/U` `/OE` `/UE` `/Perms`
  string longer than its revision specifies (the leading bytes are
  used, as by every reader).

- CLI: `--password-file <path>` (`-`: standard input) on every
  subcommand; the document summary prints an `encryption:` line. No
  `--password <text>` option — arguments are visible to other users.
- `examples/open_encrypted.rs`; `docs/encryption.md` (user guide) and
  `docs/migration-0.3.md`, both compiled as doctests.

### Fixed
- Text extraction from a non-embedded Standard 14 font without
  `/Encoding` (drawn with a bundled substitute) now resolves codes
  through the font's built-in encoding (Table 114: StandardEncoding,
  or Symbol's / ZapfDingbats'), the same table rendering uses; they
  were reported `ResolveKind::Unresolved` before (since 0.1).

### Changed (observable behaviour)
- An encrypted document that opens with the empty user password is now
  read by `Document::from_bytes` instead of being refused with
  `Error::Encrypted`; check `doc.encryption()` to tell. `Error::Encrypted`
  now means "the password does not open it". A malformed `/Encrypt`
  dictionary is `Error::Structure`; an unknown handler, revision or crypt
  filter method is `Error::Unsupported`.
- The writers (`rewrite`, `Edit::rewrite`, `Edit::save_incremental`)
  refuse an encrypted document with `Error::Unsupported` (they would
  drop or break the encryption).
- A damaged encrypted file whose cross-reference table would have to be
  rebuilt is refused with `Error::Unsupported` (the scan would read
  ciphertext as plaintext); previously `Error::Encrypted`. The file
  counts as encrypted when *any* object the scan finds says so — an
  `/Encrypt` entry or an encryption dictionary, also inside an object
  stream — not only a surviving trailer: 0.2.1 rebuilt such a file
  with a lost trailer as an unencrypted document of ciphertext.
- A malformed `/Encrypt` dictionary never falls back to a default: a
  `/StmF` `/StrF` `/EFF` or `/Crypt` `/Name` that is present but not a
  name, an undefined crypt filter, RC4 crypt filters that disagree on
  the key length (or a short one beside `/AESV2`), a `/Length` that is
  not an integer, and an R6 dictionary without `/Perms` are
  `Error::Structure` — also with a wrong password, since no password
  could open them.

## [0.2.2] - 2026-10-31

Patch release (`cargo semver-checks` against 0.2.1: no semver update
required). Release notes: `docs/releases/v0.2.2.md`.

### Fixed
- **Panic on an empty stream without a usable `/Length`** (found by
  fuzzing; present since 0.1.0). With no direct or resolvable
  `/Length`, the parser scans for `endstream` and backs off the EOL
  before it; in `stream⏎endstream` that EOL ends the `stream` line, so
  the data end fell one byte before its start and slicing panicked —
  reachable from any input, in strict and recovery mode. The back-off
  now never passes the start of the data; such a stream is empty.

## [0.2.1] - 2026-10-30

Compatible release (`cargo semver-checks` against 0.2.0: no semver
update required). Several inputs refused by 0.2.0 now succeed; the
list is in `docs/releases/v0.2.1.md` § Behaviour changes.

### Added
- **§7.3.10 undefined references.** An indirect reference to an object
  the file does not define (no cross-reference entry for that number
  and generation, or a free entry) resolves to null in strict mode, as
  the standard requires, and is recorded once as
  `Warning::UndefinedReference`. The writers and the audit still see
  `Error::MissingReference` (`Document::resolve_defined`, crate-private)
  so a rewrite never turns a dangling reference into `null` silently.
- **Open content-stream blocks.** A stream ending inside `q`, `BT`,
  `BMC`/`BDC` or `BX` is closed at its end and recorded as
  `Warning::UnterminatedContentBlock` (nothing after the end could
  depend on the closing operator). Stray closing operators stay errors.
- **Recovery rules.** `Document::from_bytes_recovering` now also
  applies, and records as `Warning::RecoveryApplied { rule:
  RecoveryRule, .. }`, the reinterpretations every major reader makes
  and ISO 32000-1 does not sanction: US Letter for a page without
  `/MediaBox`, a stray `Q` ignored, an unknown operator dropped, a
  graphics-state or colour operator inside a path object applied, an
  invalid `/Annots` entry or appearance state skipped. Strict mode
  refuses every one of them unchanged. `Document::recovery_mode()`
  reports the mode; `recovery()` still reports only a rebuilt table.
- **Knockout transparency groups** (`/K true`, §11.4.8): each object is
  composited against the group's initial backdrop and replaces earlier
  objects within its shape; isolated and non-isolated, with the group's
  alpha, blend mode and soft mask. New bound
  `Limits::max_knockout_objects` (default 4 096).
- **Symbol and ZapfDingbats fallback.** Non-embedded Symbol /
  ZapfDingbats fonts draw with a bundled 414-glyph DejaVu Sans subset
  (`fonts/symbol-fallback/`, Bitstream Vera licence) through their
  built-in encodings (Annex D.5/D.6) and the Adobe AFM widths when the
  PDF has no `/Widths`; reported as `SubstKind::SymbolFallback`.
- `Warning::UnknownRenderingIntent` (an `/RI` or `ri` name outside
  §8.6.5.8, treated as RelativeColorimetric) and
  `Warning::IgnoredFilterFragment` (an ASCII85 final group of one
  character).
- Compatibility runner: strict / recovered / approximate render
  outcomes counted independently (a recovered render is never a strict
  one), a recovery-mode render attempt after a strict render error,
  warning and recovery-note totals.

- **Oversized images drawn small.** A JPEG whose `/Width` or `/Height`
  exceeds `max_image_dimension` (or the sample limits) while the size it
  is drawn at does not is decoded with a reduced inverse DCT (1/2, 1/4
  or 1/8: the coarsest that keeps one sample per device pixel on both
  axes, else the finest that fits the limits) and reported on the new
  `RenderedPage::image_approximations()`, which says whether the decode
  is below the drawn resolution.
  An oversized soft mask without an image filter is sampled directly
  at its compositing grid (exactly the samples a full decode would
  give). The limits are unchanged; anything else still fails with
  `LimitExceeded`.

### Fixed
- Knockout groups: opacity set inside a nested form (`ca`/`CA`, an
  ExtGState soft mask, an image's `/SMask`) reduced the object's shape
  and so under-knocked earlier objects; opacity is now kept out of the
  shape pass at any depth, and `/AIS` (previously ignored) decides
  whether those values are shape (§11.6.4.4). `/AIS` must be a boolean.
- DCT images: the JPEG frame size is checked against the image
  dictionary *before* decoding, and a progressive frame's coefficient
  memory (held at full resolution whatever the output scale) is bounded
  by `max_image_bytes` up front. Previously a frame header larger than
  the dictionary, or a large progressive frame, could allocate beyond
  `max_image_bytes` before the post-decode size check.
- A colour-keyed (`/Mask` array) JPEG is never decoded at reduced size:
  the key matches exact sample values.

### Changed
- An image's `/SMask` finer than the image is composited at the mask's
  resolution (capped at the drawn size and the image limits), with the
  image's samples repeated onto that grid — §11.6.5.3 maps the two
  independently. Previously the mask was resampled down to the image's
  grid.
- A `/SMask`, `/Contents` element, appearance state or `/AP /N` that
  is null (including through an undefined reference) is treated as
  absent rather than as a type error.

## [0.2.0] - 2026-10-24

The API-stability release: the breaking changes scheduled in
`docs/api-stability.md`, made once so that later releases can add
without breaking. Behaviour on PDF input is unchanged (the 511-file
corpus renders, extracts and rewrites identically). Every change is
mechanical; `docs/migration-0.2.md` has the one-line replacement for
each.

### Breaking
- `#[non_exhaustive]` on `Error`, `ChangeKind`, `ResolveKind`,
  `SubstKind`, `DestSource`, `TargetKind`, and on the option and
  result structs `Limits`, `RenderOptions`, `RenderedPage`,
  `ExtractedText`, `CharInfo`, `SignatureInfo`, `RecoveryReport`,
  `FontSubstitution`, `MissingReference`. Exhaustive matches need a
  wildcard arm; struct literals of `Limits` / `RenderOptions` become
  `Limits::default().with_<field>(v)` / `RenderOptions::default()
  .with_dpi(d).with_limits(l)` (fields stay `pub` for reading and
  assignment).
- `Error::kind()` returns the new `ErrorKind` enum instead of
  `&'static str`; `ErrorKind::as_str()` yields the 0.1 strings
  unchanged, so CLI output is identical.
- `extract_text(&Document, &Page, &TextOptions)`: the third argument
  is the new `TextOptions` (`with_limits`, `with_hidden_content`,
  `with_region`). `extract_text_all` is removed (=
  `with_hidden_content(true)`).
- `RenderedPage` fields are private: `width()`, `height()`, `dpi()`,
  `page_size_pts()`, `font_substitutions()`, `color_approximations()`;
  the pixel buffer through the backend-neutral `pixels()`, `stride()`,
  `pixel_format()`, `into_rgba8()`, `to_png()`; `pixmap()` /
  `into_pixmap()`, the `Pixmap` re-export and `pixmap_to_png` only
  with the new default feature `tiny-skia-interop`.
- A signature's refusal is `Error::SignaturePolicy { rule:
  SignatureRule, detail }` (`ByteRangeNotIntact`, `DocMdpNoChanges`,
  `DocMdpFormFillOnly`, `DocMdpFormFillAndAnnotations`,
  `FieldMdpNotEvaluated`, `RewriteOfSignedDocument`) instead of
  `Error::Unsupported { feature: "signed document" | "edit of a
  signed document" }`.
- An invalid `dpi` is `Error::InvalidArgument { argument: "dpi" }`
  instead of `Error::Structure`.

### Added
- `ErrorKind` (`as_str`, `is_malformed_input`), `SignatureRule`,
  `Error::InvalidArgument`, `MissingReference` re-exported.
- `Limits::new`, `Limits::validate` (rejects a zero *structural*
  bound; feature bounds may still be 0 to forbid the feature), one
  `with_<field>` per bound; `RenderOptions::new/with_dpi/with_limits/
  validate`; `TextOptions`; `PixelFormat`.
- Every entry point that takes limits or options validates them first.

### Changed
- Documentation: `docs/migration-0.2.md`, `docs/api-stability.md`
  rewritten for the 0.2 contract, README and examples on the new API.

## [0.1.1] - 2026-10-24

Additive release: no public item was removed or changed in signature
(`cargo semver-checks` against 0.1.0: no semver update required).

### Added
- `Warning` (`#[non_exhaustive]`) and the per-document warning
  channel: `Document::take_warnings`, `warnings`, `warnings_dropped`,
  `warning::MAX_WARNINGS`. A non-conforming input the engine can
  still process *exactly* is accepted and recorded instead of refused.
  Two variants: `UnterminatedFlateStream` (a `/FlateDecode` stream
  ending after a sync flush without the final block and Adler-32;
  data ending inside a block is still refused) and
  `MalformedToUnicodeEntry` (a `bfchar`/`bfrange` entry that cannot
  be read is skipped; its codes stay unmapped).
- `kasane_pdf::Pixmap` re-exports the `tiny-skia` type of
  `RenderedPage::pixmap`.
- The CLI `render` and `text` subcommands print each warning as
  `warning: …` on stderr.
- Examples `rewrite`, `recovery` and `cjk_fallback` (the last behind
  `--features cjk-fallback`); `tests/public_api.rs` exercises the
  public surface the README relies on; the README code blocks are
  compiled as doctests.
- `docs/api-stability.md` (SemVer classification of the 0.1 API),
  `docs/decisions/ttf-parser.md`, a RustSec audit job in CI.

### Changed
- `/ToUnicode` hex strings may contain white space between digits
  (§7.3.4.3); previously such a CMap failed with
  `Error::Structure("invalid hex in ToUnicode")`.
- Flate decoding failures are now distinguished: `"incomplete deflate
  stream"` (truncated inside a block) versus `"corrupt deflate
  stream"` (invalid data); both remain `Error::Parse`.
- Dependencies: `crc32fast` 1.5.3; the compatibility runner uses
  `sha2` 0.11.

### Dependencies
- `ttf-parser` 0.25.1 is kept despite RUSTSEC-2026-0192 (an
  *unmaintained* notice, not a vulnerability); the rationale and the
  migration conditions are in `docs/decisions/ttf-parser.md`, and
  `.cargo/audit.toml` ignores only that informational id.

## [0.1.0] - 2026-10-24

Initial release. The API is not yet stable (0.x): breaking changes
are possible between minor versions and are recorded here.

### Added
- Strict parser for the PDF object layer, classic and stream
  cross-reference sections, object streams, incremental updates and
  hybrid files; an explicit recovery parser (`from_bytes_recovering`)
  that reports every assumption it makes.
- Page renderer (`render_page`): paths, clipping, every colour-space
  family of ISO 32000-1 §8.6, images (Flate, LZW, ASCII, RunLength,
  DCT, JBIG2, CCITT, JPX), image masks and soft masks, transparency
  groups and blend modes, ExtGState soft masks, shadings of every
  type, tiling and shading patterns, inline images, text in TrueType,
  Type 1, CFF, OpenType, Type 0 (CID) and Type 3 fonts with rendering
  modes 0–7, optional content, annotation appearances and synthesised
  form-field / markup appearances, `/UserUnit`.
- Text extraction (`extract_text`): positioned characters with
  Unicode through `/ToUnicode`, encodings, `/ActualText`.
- Full rewrite (`rewrite`) and an `Edit` session with `/Square`
  annotations, annotation modification and removal, form-field
  values, `/Info` updates; **incremental save** that preserves the
  original bytes; signature detection and a safe-by-default policy
  for edits of signed documents (no cryptographic verification).
- Experimental content editors (`remove_text`, `remove_image_region`,
  `remove_region`, `remove_regions`, `remove_document_metadata`), the
  read-only redaction diagnosis and residual-information audit.
- Bundled Liberation fonts as substitutes for non-embedded Standard
  14-style fonts; optional `cjk-fallback` feature for non-embedded
  Adobe-Japan1 CID fonts.
- Compatibility runner (`compat/runner`) with Poppler/qpdf
  cross-checks, all-page round-trip verification and CI workflows.

### Fixed
- JPEG colour-transform tests no longer depend on the decoder's SIMD
  back-end: expectations are derived in-process (JFIF decode of the
  same coefficients, or the T.871 conversion of the decoder's own raw
  samples) instead of libjpeg-turbo sample values, which differ by
  the IDCT accuracy allowance between x86-64 and arm64.
- Nightly workflow calls `fetch-corpus.py download --dir` and passes
  the store location and credentials under the names the tool and
  boto3 read; actions pinned to commit SHAs; a consistency check runs
  on every pull request.
- CJK fallback assets are fetched from pinned commits and verified
  against `scripts/cjk-assets.sha256`.
- Test-fixture font subsets renamed internally so OFL Reserved Font
  Names are not used; provenance recorded.

### Not included
- Document creation from scratch (no page / content-stream builder),
  encryption, cryptographic signature verification, knockout
  transparency groups, non-embedded CJK fonts other than Adobe-Japan1
  (with `cjk-fallback`), page-level editing — see
  `docs/limitations.md`.

### Licence
- Apache License 2.0; bundled fonts under the SIL Open Font License
  1.1 and Adobe CMap resources under BSD-3-Clause (`NOTICE`,
  `docs/third-party.md`).

[Unreleased]: https://github.com/akam1o/kasane-pdf/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.5.0
[0.4.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.4.1
[0.4.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.4.0
[0.3.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.3.1
[0.3.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.3.0
[0.2.2]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.2
[0.2.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.1
[0.2.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.2.0
[0.1.1]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.1
[0.1.0]: https://github.com/akam1o/kasane-pdf/releases/tag/v0.1.0
