//! # kasane-pdf
//!
//! A PDF engine written in Rust (early development, milestone M0).
//!
//! M0 scope (see `docs/coverage.md` for the full support matrix):
//!
//! - Parse PDF basic objects (null, booleans, numbers, names, literal/hex
//!   strings, arrays, dictionaries, `stream` markers).
//! - Read classic (non-stream) cross-reference tables, including `/Prev`
//!   chains, and the file trailer.
//! - Resolve indirect references.
//! - Load the document catalog and page tree, with inherited page
//!   attributes (`MediaBox`, `CropBox`, `Rotate`, `Resources`).
//!
//! Explicitly **not** supported yet (reported as [`Error::Unsupported`] or
//! [`Error::Encrypted`]): xref streams, object streams, stream decoding,
//! encrypted files, content stream interpretation, rendering, text
//! extraction, editing.
//!
//! # Safety-oriented design
//!
//! All parsing goes through [`Limits`], which bounds the number of
//! indirect objects followed, page-tree recursion depth and nodes, and
//! collection sizes. Malformed input returns an [`Error`]; it must not
//! panic or loop without bound.
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

mod document;
mod error;
mod filter;
mod lexer;
mod limits;
mod object;
mod objstm;
mod page;
mod parser;
mod xref;

pub use document::Document;
pub use error::Error;
pub use limits::Limits;
pub use object::{IndirectRef, Object};
pub use page::{Page, Rect};
