# kasane-pdf documentation

kasane-pdf is an independent PDF engine (parser, document model,
content interpreter, writer) — not a wrapper around PDFium/MuPDF/
Poppler. This directory is split into **reference** (what is
supported / how it behaves) and **rationale** (why it is built that
way).

## Reading order

1. [`architecture.md`](architecture.md) — goals/non-goals, module
   map, the core design decisions, the safety/`Limits` model, the
   licensing policy, and the long-term roadmap.
2. [`features/`](features/) — the support matrix, one file per area.
   Everything *not* listed as supported is a typed error — never a
   silent skip or a partial result.
3. [`security.md`](security.md) — the safety/`Limits` posture and the
   "not a redaction/sanitizer" guarantees in one place.
4. [`decisions/`](decisions/) — design-decision records (one per
   editing/diagnosis subsystem).
5. [`testing.md`](testing.md) — test layout, fixtures, and external
   verification.
6. [`compat-corpus.md`](compat-corpus.md) — the R2-backed real-world
   PDF compatibility corpus and runner.
7. [`redaction-design.md`](redaction-design.md) — the recorded
   constraints that shape document-model choices.

## Feature support matrix (`features/`)

| File | Area |
|---|---|
| [`parsing.md`](features/parsing.md) | file structure, object layer, stream decoding, document layer, content-stream ops |
| [`rendering.md`](features/rendering.md) | page→pixmap rendering, text rendering & `extract_text` |
| [`matrices.md`](features/matrices.md) | `Matrix` CTM semantics, singular/non-finite handling |
| [`rewriting.md`](features/rewriting.md) | full single-revision rewrite — what is carried, refused, and *not* a sanitizer |
| [`editing-annotations.md`](features/editing-annotations.md) | `/Square` annotation editing |
| [`inspect.md`](features/inspect.md) | `inspect` page/document structure dump |
| [`diagnosis.md`](features/diagnosis.md) | `diagnose_redaction` / `preflight-redaction` (read-only) |
| [`editing-text.md`](features/editing-text.md) | experimental scoped text removal |
| [`editing-image.md`](features/editing-image.md) | experimental image-region removal (raw/Flate) |
| [`editing-jpeg.md`](features/editing-jpeg.md) | JPEG `DCTDecode` image editing (M27) |
| [`editing-region.md`](features/editing-region.md) | experimental text+image region removal (M20) |
| [`editing-multipage.md`](features/editing-multipage.md) | multi-page region removal (M24) |
| [`editing-metadata.md`](features/editing-metadata.md) | combined region removal + metadata strip (M26) |
| [`metadata.md`](features/metadata.md) | document-metadata entry removal (M25) |
| [`audit.md`](features/audit.md) | residual-information audit (`audit-residuals`, M23) |

## Design decisions (`decisions/`)

Rationale for the experimental editing/diagnosis subsystems — kept as
one record per milestone so a decision's context and its superseded
form stay traceable.

| File | Decision |
|---|---|
| [`diagnosis.md`](decisions/diagnosis.md) | pre-redaction diagnosis walker (M16) |
| [`editing.md`](decisions/editing.md) | scoped text/image/region removal, JPEG, grid-aligned CTM (M17–M28) |
| [`audit.md`](decisions/audit.md) | residual-information audit (M23) |
| [`multi-page.md`](decisions/multi-page.md) | multi-page region removal (M24) |
| [`metadata.md`](decisions/metadata.md) | document-metadata removal (M25) |
| [`combined.md`](decisions/combined.md) | combined region + metadata strip (M26) |

## Conventions

- ISO 32000-1 section numbers (`§`) are cited inline throughout and
  refer to ISO 32000-1:2008 unless noted.
- "Spec guarantee" vs "implementation limit" is distinguished in each
  feature file: a ✅/❌ matrix row is the *contract*, prose under it is
  the *rationale*.
- The experimental `remove_*` / `experimental-*` commands are content
  editors, **not** a redaction tool and **not** a sanitizer — see the
  security note in [`architecture.md`](architecture.md) and each
  editing feature file.
