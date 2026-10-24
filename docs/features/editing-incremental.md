# Incremental save and signed-document policy

## Incremental save (ISO 32000-1 §7.5.6)

`Edit::save_incremental()` is the second way out of an `Edit` session,
next to `Edit::rewrite()`. Where the full rewrite rebuilds the file as
one flat revision, the incremental save keeps the **original bytes
verbatim** and appends a new revision after them:

```text
┌──────────────────────────────┐
│ original file, byte for byte │  ← never touched (earlier revisions,
├──────────────────────────────┤     signed byte ranges survive)
│ updated objects              │  ← same object numbers / generations
│ new objects                  │  ← numbered from the old trailer /Size
│ new /Info (set_info)         │
│ xref section                 │  ← classic table *or* xref stream,
│ trailer  /Size /Root /Info   │     matching the file's newest section
│          /ID /Prev           │     (§7.5.8.4 forbids mixing kinds)
│ startxref  %%EOF             │
└──────────────────────────────┘
```

| Feature | Status | Notes |
|---|---|---|
| original bytes preserved | ✅ | output `starts_with(original)`; a trailing newline is added only when the file does not end in one |
| updated objects | ✅ | page dictionaries, `/Annots` arrays, annotation dictionaries, field dictionaries, `/AcroForm` (or the catalog when `/AcroForm` is direct) are rewritten under their **original** number and generation |
| new objects | ✅ | annotation dicts, appearance streams and copied `/Annots` arrays get fresh numbers starting at the newest trailer's `/Size`, skipping any number the file already defines; references between them are translated at write time |
| `/Prev` | ✅ | the previous `startxref` offset |
| `/Size` | ✅ | one past the highest number written |
| `/Root`, `/Info`, `/ID` | ✅ | carried from the merged trailer; a `set_info` replacement becomes a new `/Info` object |
| classic xref input | ✅ | appends `xref` subsections (one per run of consecutive numbers) + `trailer` |
| xref-stream input | ✅ | appends an uncompressed `/Type /XRef` stream (`/W [1 4 2]`, `/Index`) that also lists itself; offsets > 4 GiB are refused |
| multiple revisions | ✅ | a saved file can be loaded and saved again; each revision keeps the previous one verbatim (`tests/incremental_save.rs::remove_modify_and_field_value`) |
| recovered documents | ❌ | `Error::Unsupported` — the xref offsets are reconstructions, so `/Prev` would point at damage; use the full rewrite |
| encrypted documents | ❌ | `Error::Encrypted` |
| non-zero-generation references | ❌ | `Error::Unsupported` — the serialiser writes `n 0 R`; refusing is safer than mis-linking |
| deleted objects | — | an incremental save cannot remove bytes; `remove_annotation` drops the page's reference, the annotation object stays in the file (and is reachable to a forensic reader — use the full rewrite to drop it) |

### Edits that may be saved incrementally

| Operation | API | Change kind (signature policy) |
|---|---|---|
| metadata update | `Edit::set_info(Dict)` — merged over the current `/Info`, `null` removes a key | `Metadata` |
| annotation add | `Edit::add_square(page, rect, rgb, width)` | `Annotations` |
| annotation modify | `Edit::set_annotation_entries(page, index, Dict)` — sets/removes entries; an existing `/AP` is kept (remove it with `null` to have the appearance synthesised) | `Annotations` |
| annotation delete | `Edit::remove_annotation(page, index)` | `Annotations` |
| form field value | `Edit::set_field_value("parent.child", value)` — `/V`; for a name value also `/AS` on the widgets whose `/AP /N` has that state (`/Off` otherwise); `/NeedAppearances true` | `FormFill` |

The same session can also be written with `Edit::rewrite()`, in which
case `set_info` is **not** carried (use `remove_document_metadata` /
the rewrite overrides there).

### Tests

`tests/incremental_save.rs` covers: appending to a classic-xref and to
an xref-stream file, two revisions on top of each other, updated
objects read from the new revision while untouched ones are read from
the original offsets, page count and structure preserved, rendering
of the result, and `qpdf --check` on every produced file (skipped when
qpdf is not installed).

## Signed documents (§12.8) — safety policy

`kasane_pdf::signatures(&doc)` detects every signature value: `/FT
/Sig` fields with a `/V` dictionary under `/AcroForm /Fields` →
`/Kids` (depth ≤ 32) and signature widgets reachable only from page
`/Annots`. For each it reports the field name, `/ByteRange`, whether
that range is **intact** (`[0 a b c]` with `a ≤ b`, `b + c ≤` file
length, and the gap `[a, b)` holding exactly the `/Contents` hex
string), the end of the signed revision, `/DocMDP /P` (certification
signature; `/Perms /DocMDP` naming the value defaults `/P` to 2),
whether a `/FieldMDP` transform is present, and `/SubFilter`.

**Nothing here verifies a signature cryptographically.** The engine
never reports a signature as valid; it only decides whether an edit
can be appended without contradicting what the signer declared.

| Situation | Full rewrite | Incremental save |
|---|---|---|
| no signature | allowed | allowed |
| any signature | **refused** (`Error::SignaturePolicy { rule: RewriteOfSignedDocument }`) — every signed byte would move | see below |
| `/ByteRange` not intact (not four non-negative integers, does not start at 0, gap is not the `/Contents` string, extends past the file) | refused | **refused** — the claim cannot be reasoned about |
| `/FieldMDP` present | refused | **refused** — field locks are not evaluated |
| certification `/DocMDP /P 1` | refused | refused for every change |
| certification `/DocMDP /P 2` | refused | `FormFill` only |
| certification `/DocMDP /P 3` | refused | `FormFill` and `Annotations` |
| approval signature (no `/DocMDP`) | refused | `FormFill` and `Annotations` (what viewers treat as a non-invalidating post-signing change) |
| `Metadata` or any other change kind | refused | refused whenever a signature exists |

The policy runs before a byte is written; a refusal is a typed error
with the field name and the permission that blocked the change.

### Staged plan and what is not done

1. detection — done (`src/signature.rs`);
2. `/ByteRange` structural check — done (coverage and gap, not the
   digest);
3. revision structure — done implicitly: the incremental writer keeps
   the signed revision intact and chains `/Prev` to it;
4. `/DocMDP` evaluation — done; `/FieldMDP` — detected, **not
   evaluated** (refused);
5. permitted changes only — done through `ChangeKind`;
6. comparison with an external signature verifier — **not done**;
   neither is digest or certificate validation, `/UR`/`/UR3` usage
   rights, timestamp signatures, or PAdES/`/DSS` structures. A
   document whose signature is already broken is still treated as
   signed (refused for anything but permitted incremental changes),
   because the engine cannot tell a broken signature from a valid
   one.
