//! # kasane-pdf
//!
//! A PDF engine written in Rust — parsing, document model, content
//! interpretation, rendering, text extraction, editing, and writing
//! are all implemented here (not a wrapper around PDFium/MuPDF/
//! Poppler).
//!
//! Scope (see `docs/features/` for the full per-area support matrix):
//!
//! - Parse PDF basic objects, classic `xref` tables, `/Prev` chains,
//!   xref streams, hybrid `/XRefStm`, and object streams.
//! - Decode `/FlateDecode` (+ PNG/TIFF predictors) and `/DCTDecode`
//!   (image XObjects / region editing).
//! - Load the document catalog and page tree with inherited page
//!   attributes; `page_contents`/`page_operations`/`page_resource`.
//! - Render pages to a premultiplied RGBA8 buffer via [`render_page`] (`RenderedPage::to_png`, `pixels`, `into_rgba8`):
//!   paths, clipping, Device colours, Form & Image XObjects, embedded
//!   TrueType and Type0/Identity-H/CIDFontType2 text.
//! - Extract positioned Unicode text via [`extract_text`]
//!   (`/ToUnicode` CMap + WinAnsi/AGL fallback).
//! - Full single-revision rewrite via [`rewrite`].
//! - Incremental save via `Edit::save_incremental()` (original bytes
//!   kept, a revision appended) for metadata, annotation and form-field
//!   edits, gated by the signed-document policy in [`signature`].
//! - `Document::edit()` sessions: `/Square` annotations plus the
//!   experimental scoped `remove_*` editing family and
//!   `diagnose_redaction`/`audit_residuals`.
//!
//! Explicitly **not** supported (typed `Error::Unsupported` or
//! `Error::Encrypted`, never a partial result): the `/Crypt` filter,
//! encrypted files, knockout transparency groups, `/Matte` soft-mask
//! images, predefined non-Identity CMaps and non-embedded CID fonts
//! (unless the `cjk-fallback` feature bundles the Adobe-Japan1 set
//! and a fallback face), singular CTMs, annotations
//! without an appearance stream other than form widgets and the
//! geometric markup subtypes. Colour spaces, image masks, inline
//! images, every byte filter (Flate, LZW, ASCII85, ASCIIHex,
//! RunLength) and the JPEG, JBIG2, CCITT and JPX image filters,
//! every shading type (axial, radial, function-based, triangle and
//! patch meshes), tiling patterns, every simple-font encoding,
//! CID-keyed CFF and OpenType CID fonts, embedded CMaps, vertical
//! writing, optional content (default configuration), `/ActualText`,
//! synthesised AcroForm widget and markup annotation appearances,
//! isolated and non-isolated transparency groups, ExtGState soft masks
//! and every standard blend mode, and `/UserUnit` render; an
//! `ICCBased` profile the engine cannot evaluate exactly falls back to
//! `/Alternate` and is reported on
//! [`RenderedPage::color_approximations`].
//!
//! Damaged files: [`Document::from_bytes`] is strict. A caller that
//! wants a best-effort reading uses [`Document::from_bytes_recovering`],
//! which rebuilds the cross-reference table from the object headers in
//! the file and reports every assumption on [`RecoveryReport`]
//! (`Document::recovery()`); a strictly parsed document reports `None`.
//!
//! # Safety-oriented design
//!
//! All parsing goes through [`Limits`], which bounds the number of
//! indirect objects followed, page-tree recursion depth and nodes,
//! collection sizes, and content-stream operation/operand counts.
//! Malformed input returns an [`Error`]; it must not panic or loop
//! without bound.
//!
//! # Example
//!
//! ```no_run
//! use kasane_pdf::Document;
//!
//! let data = std::fs::read("input.pdf")?;
//! let doc = Document::from_bytes(&data)?;
//! for page in doc.pages() {
//!     println!("page {}: {} (rotate {})", page.index() + 1, page.media_box(), page.rotate());
//! }
//! # Ok::<(), kasane_pdf::Error>(())
//! ```

mod acroform;
mod annot_appearance;
pub mod audit;
mod bilevel;
mod cjk;
mod cmap;
mod colorspace;
mod content;
pub mod dests;
mod document;
pub mod edit;
mod encodings;
mod error;
mod filter;
mod font;
mod fontsubst;
mod function;
mod icc;
mod incremental;
mod jpx;
mod lexer;
mod limits;
mod matrix;
mod object;
mod objstm;
mod optcontent;
mod page;
mod parser;
pub mod redact_diag;
pub mod remove_image;
pub mod remove_metadata;
pub mod remove_region;
pub mod remove_text;
mod render;
mod rewrite;
mod serialize;
mod shading;
pub mod signature;
pub mod text_extract;
mod tounicode;
mod type1;
pub mod warning;
mod xref;

pub use content::{Operation, TJItem};
pub use dests::{named_destinations, resolve_named_destination, DestSource, NamedDest};
pub use fontsubst::{substitution_for, FontSubstitution, SubstKind};
pub use matrix::Matrix;
/// PNG encoding of a raw `tiny-skia` pixmap (`RenderedPage::to_png`
/// is the backend-neutral form). `tiny-skia-interop` feature.
#[cfg(feature = "tiny-skia-interop")]
pub use render::pixmap_to_png;
pub use render::{render_page, Color, PixelFormat, RenderOptions, RenderedPage};
pub use text_extract::{extract_text, CharInfo, ExtractedText, ResolveKind, TextOptions};
/// The raster type behind `RenderedPage::pixmap()`, re-exported so it
/// can be named without depending on `tiny-skia` at a matching
/// version. `tiny-skia-interop` feature.
#[cfg(feature = "tiny-skia-interop")]
pub use tiny_skia::Pixmap;
pub use xref::RecoveryReport;

pub use audit::{
    audit_residuals, AuditFinding, AuditIncomplete, FindingKind, HistoryReport, PathStep,
    ReachClass, ReachabilityReport, ResidualAudit,
};
pub use cjk::{cjk_fallback_enabled, predefined_cmap_known, predefined_cmap_names};
pub use document::Document;
pub use edit::Edit;
pub use error::{Error, ErrorKind, MissingReference, SignatureRule};
pub use limits::Limits;
pub use object::{Dict, IndirectRef, Object};
pub use page::{Page, Rect};
pub use redact_diag::{diagnose_redaction, Diagnosis, Target, TargetDetail, TargetKind};
pub use remove_image::{remove_image_region, ImageRemoval};
pub use remove_metadata::{remove_document_metadata, MetadataRemoval};
pub use remove_region::{
    remove_region, remove_regions, remove_regions_and_document_metadata, PageSpec, RegionRemoval,
    RegionsAndMetadataRemoval, RegionsRemoval,
};
pub use remove_text::{remove_text, Removal};
pub use rewrite::{rewrite, rewrite_with_limits};
pub use signature::{signatures, ChangeKind, SignatureInfo};
pub use warning::Warning;

/// The README's code blocks are compiled as doctests (`no_run`), so
/// the examples a new user copies cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
