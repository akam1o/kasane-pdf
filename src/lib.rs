//! # kasane-pdf
//!
//! A PDF engine written in Rust (early development, milestone M2).
//!
//! Scope so far (see `docs/coverage.md` for the full support matrix):
//!
//! - Parse PDF basic objects (null, booleans, numbers, names, literal/hex
//!   strings, arrays, dictionaries, `stream` markers).
//! - Read classic cross-reference tables, `/Prev` chains, xref streams,
//!   hybrid-reference files, and object streams.
//! - Decode `/FlateDecode` streams with PNG/TIFF predictors (8-bit).
//! - Resolve indirect references.
//! - Load the document catalog and page tree, with inherited page
//!   attributes (`MediaBox`, `CropBox`, `Rotate`, `Resources`).
//! - Parse page `/Contents` into typed [`Operation`]s and look up
//!   named entries in page `/Resources`.
//!
//! Explicitly **not** supported yet (reported as [`Error::Unsupported`]
//! or [`Error::Encrypted`]): stream filters other than `/FlateDecode`,
//! inline images, encrypted files, rendering (operations are parsed,
//! not executed), Unicode text mapping, text extraction, editing.
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

mod content;
mod document;
mod error;
mod filter;
mod lexer;
mod limits;
mod matrix;
mod object;
mod objstm;
mod page;
mod parser;
mod xref;

pub use content::{Operation, TJItem};
pub use matrix::Matrix;

pub use document::Document;
pub use error::Error;
pub use limits::Limits;
pub use object::{IndirectRef, Object};
pub use page::{Page, Rect};
