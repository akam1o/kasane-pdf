# Annotation text

`kasane-pdf text --annotations` and
`TextOptions::with_annotations(true)` report the text a page's
annotations and form fields carry. It is reported **beside** the page's
own text, never mixed into it (T1 in
[`compatibility-roadmap.md`](compatibility-roadmap.md)).

## Default behaviour

Without the flag (or the option), nothing changes:
- `kasane-pdf text` prints the page content's characters only. The
  output is byte-for-byte what it was before T1.
- `extract_text` returns `annotations` empty.

The page content's `chars` are the same with or without the option:
annotation text is never added to them.

## What is reported

Every annotation of the page's `/Annots` gives one `AnnotationText`, in
`/Annots` order. An annotation object listed twice gives one record.
Two different annotations with the same words give two records:
nothing is removed because its words equal another annotation's.

Text comes in two kinds, chosen by what each annotation type means
(ISO 32000-1 § 12.5):

- **shown** (`AnnotationText::shown_text`): the characters the
  annotation's normal appearance draws on the page. This is what a
  viewer shows, and what Poppler and MuPDF extract. Where an annotation
  has no appearance stream, it is the appearance kasane draws for it:
  - a form field's value, laid out with its `/DA`;
  - a free-text annotation's `/Contents`.
- **note** (`AnnotationText::note`): the comment a *markup* annotation
  carries in `/Contents` (§ 12.5.6.2). A viewer shows it in the pop-up,
  not on the page.

Both are `None` for an annotation that is not shown:
- the Hidden flag (bit 2) or the NoView flag (bit 6);
- optional content that is OFF.

NoPrint concerns printing only, so a NoPrint annotation is shown.

| annotation | shown | note |
|---|---|---|
| `Text` (sticky note) | its appearance (usually an icon, no text) | `/Contents` |
| `FreeText` | its appearance, or the one drawn from `/Contents` and `/DA` | none: `/Contents` is the text it draws, so it is in shown |
| `Highlight`, `Underline`, `Squiggly`, `StrikeOut` | their appearance (usually no text) | `/Contents` |
| `Stamp`, `Square`, `Circle`, `Line`, `Polygon`, `PolyLine`, `Ink`, `Caret`, `FileAttachment`, `Sound`, `Redact`, `Projection` | their appearance | `/Contents` |
| `Widget` (form field) | its appearance, or the one drawn from the value | none |
| `Link` | none: a link has no appearance of its own | none: `/Contents` is an alternate description |
| `Popup` | not reported: it shows its parent's `/Contents`, reported on the parent | — |
| any other (`Screen`, `Movie`, `PrinterMark`, `Watermark`, `3D`, unknown) | its appearance | none: `/Contents` is an alternate description |

### Form fields

A widget's field is reported apart, in `AnnotationText::field` (and the
CLI's `field`):
- the fully qualified name, from the `/T` chain;
- the type `/FT`;
- the value `/V`;
- the default value `/DV`.

A choice field's options are given through `/Opt` as their display
text. A field with several widgets reports its value on the first one
only.

The value and the default value are never mixed up, and neither is
shown text. **Shown text is the appearance**, even when it differs from
`/V` (a stale appearance). That is what a viewer shows, and Poppler and
MuPDF extract it too. The value stays in `field.value`.

`AnnotationText::text()` keeps its earlier rule (the field value, else
`/Contents`, else the appearance), unchanged.

### Never text

These are identifiers or metadata. Some are reported as labelled
values, but none is ever shown or a note:
- `/T`: an annotation's author, or a form field's partial name. A
  field's name is in `field.name`; an author is not reported.
- `/Subj` and `/NM` (the annotation's unique name).
- `/TU` (a field's tooltip).
- `/DV`, the default value, which is reported in `field.default_value`.
- `/RC`: the rich-text form of `/Contents`, reported as stored (XHTML,
  not interpreted) in `rich_text`.
- A link's or widget's `/Contents`: an alternate description, reported
  as `contents`.

### Strings

Text strings are decoded as § 7.9.2.2 defines:
- UTF-16BE or UTF-8 with its byte-order mark, with language escapes
  removed;
- else PDFDocEncoding.

Undefined or invalid sequences become U+FFFD: an odd UTF-16 byte, an
unpaired surrogate, invalid UTF-8, an undefined PDFDocEncoding code. No
Unicode normalisation is applied: text is reported as stored. A
decomposed é stays two code points.

## Appearance streams and duplicates

Duplicates are removed by what the strings *are*, not by their bytes:

- **A free-text annotation's `/Contents` and its appearance** are the
  same text, once as source and once drawn. It is reported once, as
  shown, whether the two are equal or not.
- **A widget's `/V` and its appearance** are the same value, once as
  source and once drawn. It is shown once. The value is the field's,
  reported in `field`.
- **A markup annotation's `/Contents` and its appearance** are
  different texts (a comment, and what is drawn). Both are reported,
  as note and as shown.
- **Different annotations** are never merged, even when they hold the
  same words.

| case | shown | note |
|---|---|---|
| no appearance stream | what kasane draws for it (a field, a free text), else none | as above |
| an appearance stream that does not parse, or whose state is not a stream | strict mode: the extraction fails | |
| | recovery mode: none, with `recovered` set | as above |
| a hidden annotation | none | none |

## Recovery mode

In a document opened in recovery mode (`OpenOptions::with_recovery`),
an annotation can be read under a recovery rule:
- its appearance skipped: an `/AP /N` state that is not a stream, or an
  appearance stream without `/BBox`;
- its appearance drawn under a rule of
  [`recovery-mode.md`](recovery-mode.md) (an operator with the wrong
  operand count, say).

Such an annotation has `AnnotationText::recovered` set. Its shown text
may then be empty or partial, and not what another reader shows. Its
dictionary values are read as usual. An `/Annots` entry that is not a
dictionary is skipped, with `Warning::RecoveryApplied`, and is not
reported. The CLI opens documents in strict mode.

Annotation text is extraction only. Redaction never uses it to decide
anything: it reads annotations itself and refuses what it cannot
remove.

## The CLI

```text
kasane-pdf text <file.pdf> <page> [--format json|lines] [--annotations]
```

### `--format json --annotations`

An object:

```json
{
"chars": [ … the page's characters … ],
"annotations": [
  {"index": 0, "subtype": "FreeText", "rect": [10.00, 100.00, 110.00, 120.00],
   "hidden": false, "recovered": false, "shown": "…", "note": null,
   "contents": "…", "rich_text": null, "field": null}
]
}
```

- `chars` has the same entries as the default output, written as JSON
  (RFC 8259).
- `field` is `null`, or
  `{"name", "type", "value": […], "default_value": […], "first_widget"}`.
- A string longer than 65,536 characters is cut, and the annotation
  carries `"truncated": true`.
- At most 50,000 annotations are listed; the rest are counted in an
  `_omitted` entry.

### `--format lines --annotations`

The page's lines, as without the flag, then one line per shown text and
per note:

```text
annotation 0 FreeText shown "…"
annotation 1 Text note "…"
```

A line ends with ` (recovered)` or ` (truncated)` when that applies.

## Known limits

- **`/RC` is not interpreted.** A markup annotation with `/RC` but no
  `/Contents` has no note.
- **The default JSON is not strictly JSON.** The output of `text
  --format json` without the flag writes `via` unquoted, and its
  strings with Rust escapes. It is kept as it was. With `--annotations`
  the whole output is JSON.
- **Shown text exists only where kasane draws an appearance.**
  Synthesised field appearances use the field's `/DA` font. A Unicode
  field font is F1 in the roadmap, not done, so a field value in a
  script that font does not cover may not be shown as other readers
  show it.
- **A cycle in a field's `/Parent` chain fails the extraction.** It is
  caught as a cycle, or by the 32-level depth bound of the
  inherited-attribute walk that meets it first.
