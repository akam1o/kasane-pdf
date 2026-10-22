# `inspect` — page/document structure dump

## Page structure inspection (M10, CLI `inspect`)

`kasane-pdf inspect <file.pdf> <page>` — a read-only diagnostic dump
of a single page's structure. Fixed line order, no rendering, no
editing, no text extraction. The file must contain exactly one page;
`page` selects it (1-based).

| Line | Source | Default when absent |
|---|---|---|
| `page <n>` | 1-based page index | — |
| `mediabox <llx> <lly> <urx> <ury>` | inherited `/MediaBox` | — (required) |
| `cropbox <llx> <lly> <urx> <ury>` | inherited `/CropBox` | `/MediaBox` |
| `rotation <deg>` | inherited `/Rotate` | `0` |
| `annotations <count>` | `/Annots` array length | `0` |
| `resources fonts <count>` + `font <name> <subtype> [/<BaseFont>]` | `/Resources /Font` entries — `resources` opens the section (this is the only line with that prefix) | `0` |
| `xobjects <count>` + `xobject <name> <subtype> [extra]` | `/Resources /XObject` entries — `Form` shows `/BBox=[…]`, `Image` shows `/Width=… /Height=…` | `0` |
| `colorspaces <count>` + `colorspace <name> <csname>` | `/Resources /ColorSpace` entries — the space's name (`/DeviceRGB`), array's first name, or dict `/Subtype`/`/Base` | `0` |
| `patterns <count>` + `pattern <name> <type>` | `/Resources /Pattern` entries — `/PatternType` (1 tiling, 2 shading) | `0` |
| `shadings <count>` + `shading <name> <type>` | `/Resources /Shading` entries — `/ShadingType` (1–7) | `0` |
| `extgstates <count>` + `extgstate <name> <type>` | `/Resources /ExtGState` entries — `/Type` (usually `/ExtGState`) | `0` |
| `procsets` `properties` | direct entry counts (not enumerated) | `0` |
| `tokens <count>` | lexer tokens across `/Contents` (operands + operators) | `0` when `/Contents` absent |
| `operators <count>` | parsed operations | `0` when `/Contents` absent |
| `opstats [<name>:<count> ...]` | per-operator tallies using the *actual* stream token (`SC` vs `SCN`, `sc` vs `scn`, or the real name an `Unknown` op parsed — never a grouped label) | `opstats` (bare) when absent |
| `contents <ref>` `contents [<ref>, <ref>, ...]` `contents absent` | `/Contents` | `contents absent` |
| `userunit <v>` `userunit absent` | `/UserUnit` | `userunit absent` |
| `annot <i> <subtype> rect <llx> <lly> <urx> <ury> [/<k>=<v> ...]` | one line per `/Annots` element, in array order | — |
| `document` `version <major>.<minor>` `info <count>` `key <name> <type>` | document section — `version` reports the effective declared PDF version (the larger of the `%PDF-M.m` header and the catalog `/Version` name; malformed header or malformed `/Version` is a typed error, never a guess); `info`/`key` list the trailer `/Info` dictionary — values are never shown | `info 0` when `/Info` absent |

Each `annot` line dumps the annotation dictionary: `/Subtype` and
`/Rect` first (validated — the element may be an indirect ref *or*
a direct dictionary, both are resolved to the dict; anything else —
a number, string, stream — is a typed error, as is a non-dict
element, a missing/malformed `/Rect`, or a non-name `/Subtype`),
then every remaining key in byte order. Values are formatted
recursively: numbers decimal, names `/name`, strings `(text)` with
non-ASCII/non-printing bytes escaped `\xNN` (`(` `)` `\` too),
arrays `[v0 v1 …]`, dicts `<< /k v … >>`, refs verbatim `n g R`,
streams as `<stream>` (dict not expanded). Refs are shown, not
resolved — an `annot` value never re-fetches another object.

`tokens` counts every lexer token consumed (operands *and*
operators), so `tokens >= operators` always. `opstats` aggregates
by the *actual* operator token as it appears in the stream — `SC`
and `SCN` are counted separately (never the grouped `SC/SCN`
label), `sc`/`scn` likewise, and an `Unknown` op is reported under
its real name, not `unknown`. Order is byte order. A malformed
content stream — a `/Contents` that isn't a ref or ref array, a
stream that fails to decode, or a syntax/balance error the M2
parser rejects — is a typed error, not silently skipped.

Errors are typed (`Error::Structure`/`MissingReference`/`Parse`): a
non-1-page file (the count is reported), a non-array/unresolvable
`/Annots`, a non-dict/unresolvable `/Resources`, a `/Contents` that is
neither a ref nor an array of refs, an undecodable or unbalanced
content stream, a non-numeric `/UserUnit`, or a malformed annotation
element. Nothing is silently coerced — inspect explains the
structure as it is, or fails.
