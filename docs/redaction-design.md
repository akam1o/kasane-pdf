# Redaction — recorded design constraints

## Redaction design constraints (recorded, not implemented)

These requirements shape document-model choices being made now:

- Painting a black rectangle over content is **not** redaction; the
  underlying text/images must be removed from the content stream.
- Invisible/OCR text and image soft masks (`/SMask`) must be handled.
- Shared resources: removing an image or Form XObject that is also used
  outside the redacted area must not corrupt the retained content —
  object-level analysis of all referencers is required.
- Metadata surfaces: annotations, form fields, alternate text
  (`/Alt`, `/ActualText`), document metadata, embedded files.
- Distribution output is a **full rewrite**: no incremental update, no
  leftover objects — which M0's parse-into-owned-model approach is
  aligned with.
- Link policy must be explicit: links overlapping the redaction area
  are removed; links fully outside must be preserved where possible.
- Structures we cannot safely process must produce a clear error, not a
  best-effort result.
- Distinguish "remove the occurrence at this position" from "remove all
  occurrences of this information" (e.g. a repeated footer string).
- Safety is verified externally: text/image extraction by an
  independent engine plus internal-structure inspection.
