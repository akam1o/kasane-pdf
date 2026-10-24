# kasane-pdf documentation

kasane-pdf is an independent PDF engine (parser, document model,
content interpreter, renderer, writer) — not a wrapper around
PDFium / MuPDF / Poppler. The documentation is split into
**reference** (what is supported and how it behaves), **rationale**
(why it is built that way) and **internal notes** (how it got here).

## Getting started

| File | Contents |
|---|---|
| [`getting-started.md`](getting-started.md) | requirements, build and install, CLI tour, library usage, `Limits`, errors, the `cjk-fallback` feature |
| [`../README.md`](../README.md) | project overview, feature summary, compatibility numbers, limitations in brief |
| [`../examples/`](../examples/) | runnable examples: open and inspect, render a page, extract text, rewrite, incremental save, recovery mode, `cjk-fallback` |

## Architecture

| File | Contents |
|---|---|
| [`architecture.md`](architecture.md) | goals and non-goals, module map, key design decisions, the `Limits` table, licensing policy, roadmap |
| [`security.md`](security.md) | the safety posture: resource bounds, typed refusals, strict pass vs accepted-with-warnings vs recovery, signatures, what the engine is *not* |
| [`api-stability.md`](api-stability.md) | the public API contract: what is stable, how additions are made without breaking, what the 0.2 line promises |
| [`migration-0.2.md`](migration-0.2.md) | migrating from 0.1 to 0.2: every breaking change with its one-line replacement |
| [`third-party.md`](third-party.md) | every dependency and bundled asset with its licence |

## Rendering

| File | Contents |
|---|---|
| [`features/rendering.md`](features/rendering.md) | the render support matrix: operators, colour spaces, images, transparency, shadings, patterns, fonts, text rendering modes, annotations, `/UserUnit`; font-fixture provenance and colour-conversion notes |
| [`features/matrices.md`](features/matrices.md) | `Matrix` / CTM semantics, singular and non-finite handling |
| [`features/parsing.md`](features/parsing.md) | file structure, object layer, stream filters, content-stream operators |

## Text extraction

| File | Contents |
|---|---|
| [`features/rendering.md`](features/rendering.md) § text | `extract_text`: fonts, encodings, `/ToUnicode`, `/ActualText`, coordinates (the extractor is the renderer running headless) |

## Editing

| File | Contents |
|---|---|
| [`features/rewriting.md`](features/rewriting.md) | the full rewrite: what is carried, what is refused, save guarantees, external validation |
| [`features/editing-annotations.md`](features/editing-annotations.md) | `/Square` annotation editing |
| [`features/inspect.md`](features/inspect.md) | the `inspect` structural dump |
| [`features/diagnosis.md`](features/diagnosis.md) | `diagnose_redaction` / `preflight-redaction` (read-only) |
| [`features/audit.md`](features/audit.md) | residual-information audit (`audit-residuals`) |
| [`features/metadata.md`](features/metadata.md) | document-metadata entry removal |
| [`features/editing-text.md`](features/editing-text.md), [`editing-image.md`](features/editing-image.md), [`editing-jpeg.md`](features/editing-jpeg.md), [`editing-region.md`](features/editing-region.md), [`editing-multipage.md`](features/editing-multipage.md), [`editing-metadata.md`](features/editing-metadata.md) | the **experimental** scoped content editors (`remove_text`, `remove_image_region`, `remove_region`, `remove_regions`, combined with metadata removal) — content editing, not redaction |
| [`redaction-design.md`](redaction-design.md) | the recorded redaction constraints that shape the document model |

## Incremental save

| File | Contents |
|---|---|
| [`features/editing-incremental.md`](features/editing-incremental.md) | `Edit::save_incremental`: original bytes preserved, appended revision, xref table / stream, multiple revisions; the signed-document policy (`/ByteRange`, `/DocMDP`, `/FieldMDP`) and its explicit non-goals |

## Compatibility

| File | Contents |
|---|---|
| [`compat-corpus.md`](compat-corpus.md) | the real-world corpus (manifest, store configuration, licences), the runner and its modes, result classification, round-trip and external verification, CI, results per run, failure-cause analysis, pre-publication audit |

## Limitations

| File | Contents |
|---|---|
| [`limitations.md`](limitations.md) | everything the engine does not do, collected in one place, with pointers to the roadmap |
| [`plans/render-90.md`](plans/render-90.md) | the plan for 90 % render on the corpus: the 59 blockers re-aggregated, candidates ranked by effort, gain, spec position, risk, WASM and licence |

## Development

| File | Contents |
|---|---|
| [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | build, test, lint, compatibility-test workflow, pull-request rules |
| [`testing.md`](testing.md) | test layout, fixture tiers, external verification scripts |
| [`../tests/README.md`](../tests/README.md) | which test file owns which feature |
| [`../tests/fixtures/README.md`](../tests/fixtures/README.md) | fixture generation and provenance |
| [`../SECURITY.md`](../SECURITY.md) | reporting security problems |

## Internal historical notes

| File | Contents |
|---|---|
| [`decisions/`](decisions/) | design-decision records for the diagnosis and experimental-editing subsystems, the development history, and dependency decisions ([`decisions/ttf-parser.md`](decisions/ttf-parser.md)) — the *why*, not the *what*; not a statement of current support |

## Conventions

- ISO 32000-1 section numbers (`§`) are cited inline throughout and
  refer to ISO 32000-1:2008 unless noted.
- "Spec guarantee" vs "implementation limit" is distinguished in each
  feature file: a ✅/◐/❌ matrix row is the *contract*, prose under it
  is the *rationale*.
- The experimental `remove_*` / `experimental-*` commands are content
  editors, **not** a redaction tool and **not** a sanitizer — see
  [`security.md`](security.md) and each editing feature file.
