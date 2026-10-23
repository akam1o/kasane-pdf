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
//! - Render pages to a pixmap via [`render_page`] / [`pixmap_to_png`]:
//!   paths, clipping, Device colours, Form & Image XObjects, embedded
//!   TrueType and Type0/Identity-H/CIDFontType2 text.
//! - Extract positioned Unicode text via [`extract_text`]
//!   (`/ToUnicode` CMap + WinAnsi/AGL fallback).
//! - Full single-revision rewrite via [`rewrite`].
//! - `Document::edit()` sessions: `/Square` annotations plus the
//!   experimental scoped `remove_*` editing family and
//!   `diagnose_redaction`/`audit_residuals`.
//!
//! Explicitly **not** supported (typed `Error::Unsupported` or
//! `Error::Encrypted`, never a partial result): stream filters other
//! than `/FlateDecode`/`/DCTDecode`, inline images, encrypted files,
//! `/Pattern` colour and shadings, knockout transparency groups and
//! ExtGState soft masks, `/Matte` soft-mask images, optional content,
//! non-Identity-H Type0 encodings, `/UserUnit` ≠ 1, singular CTMs.
//! Colour spaces (`CalGray`/`CalRGB`/`Lab`/`ICCBased`/`Indexed`/
//! `Separation`/`DeviceN`), image `/SMask`/`/Mask`/`/ImageMask` and
//! non-knockout transparency groups render; an `ICCBased` profile
//! the engine cannot evaluate exactly falls back to `/Alternate` and
//! is reported on [`RenderedPage::color_approximations`].
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

pub mod audit;
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
mod lexer;
mod limits;
mod matrix;
mod object;
mod objstm;
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
pub mod text_extract;
mod tounicode;
mod type1;
mod xref;

pub use content::{Operation, TJItem};
pub use dests::{named_destinations, resolve_named_destination, DestSource, NamedDest};
pub use fontsubst::{substitution_for, FontSubstitution, SubstKind};
pub use matrix::Matrix;
pub use render::{pixmap_to_png, render_page, Color, RenderOptions, RenderedPage};
pub use text_extract::{extract_text, CharInfo, ExtractedText, ResolveKind};
pub use xref::RecoveryReport;

pub use audit::{
    audit_residuals, AuditFinding, AuditIncomplete, FindingKind, HistoryReport, PathStep,
    ReachClass, ReachabilityReport, ResidualAudit,
};
pub use document::Document;
pub use edit::Edit;
pub use error::Error;
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
