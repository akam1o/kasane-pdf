//! Error type returned by every fallible operation in kasane-pdf.
//!
//! Distinct variants are used so callers (and future layers) can tell apart
//! "this file is broken", "this feature is not implemented yet" and
//! "the file intentionally uses a feature we refuse to process".

use std::fmt;

/// An object that was referenced but does not exist, and the kind of
/// structure that was expected.
#[derive(Debug, Clone)]
pub struct MissingReference {
    /// The indirect reference that could not be resolved.
    pub reference: crate::IndirectRef,
    /// Human-readable context, e.g. `"page tree Kids entry"`.
    pub context: &'static str,
}

/// All errors produced by kasane-pdf.
///
/// `Display` messages include byte offsets or object numbers where known,
/// to aid debugging of malformed files.
#[derive(Debug)]
pub enum Error {
    /// Input is not recognized as a PDF file (bad or missing `%PDF-` header).
    NotPdf,
    /// The syntax is malformed (bad object, bad xref line, truncated input…).
    /// The payload describes where and what failed.
    Parse(String),
    /// The file ended before a complete construct could be read.
    UnexpectedEof {
        /// Byte offset at which more input was required.
        offset: usize,
        /// What was being read when the input ran out.
        context: &'static str,
    },
    /// A syntactically valid but semantically impossible structure was found
    /// (e.g. `/Type /Pages` with `/Count 0` and non-empty Kids, catalog
    /// without `/Pages`, kids count mismatch).
    Structure(String),
    /// The document declares encryption (`/Encrypt` in the trailer).
    /// Encrypted files are refused entirely in M0.
    Encrypted,
    /// The document uses a valid PDF feature that M0 does not implement.
    ///
    /// Current cases: xref streams / hybrid-reference files, object
    /// streams, and `/XRefStm` entries.
    Unsupported {
        /// The feature that is not implemented (e.g. `"xref stream"`).
        feature: &'static str,
        /// Additional detail (offset, trailer key, …).
        detail: String,
    },
    /// An indirect reference could not be resolved to a defined object.
    MissingReference(MissingReference),
    /// A reference cycle was detected while following the object graph
    /// (e.g. page tree node referencing its own ancestor, or two objects
    /// pointing at each other where a value was required).
    CycleDetected {
        /// Where the cycle was found.
        context: &'static str,
    },
    /// A configured [`Limits`](crate::Limits) bound was exceeded. Parsing
    /// stops rather than consuming unbounded memory or time.
    LimitExceeded {
        /// Which limit was exceeded.
        limit: &'static str,
        /// The configured maximum.
        max: u64,
    },
    /// I/O error from the caller-provided source (CLI only; the library
    /// parses from byte slices and does not perform I/O itself).
    Io(std::io::Error),
}

impl Error {
    /// Short, stable machine-readable category name.
    ///
    /// Useful for tests and for the CLI's error output so that message text
    /// can change without breaking automation.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::NotPdf => "not a PDF",
            Error::Parse(_) => "parse error",
            Error::UnexpectedEof { .. } => "unexpected end of input",
            Error::Structure(_) => "invalid document structure",
            Error::Encrypted => "encrypted document",
            Error::Unsupported { .. } => "unsupported feature",
            Error::MissingReference(_) => "missing reference",
            Error::CycleDetected { .. } => "reference cycle",
            Error::LimitExceeded { .. } => "limit exceeded",
            Error::Io(_) => "I/O error",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotPdf => write!(f, "input does not start with a %PDF- header"),
            Error::Parse(msg) => write!(f, "parse error: {msg}"),
            Error::UnexpectedEof { offset, context } => {
                write!(
                    f,
                    "unexpected end of input at byte {offset} while reading {context}"
                )
            }
            Error::Structure(msg) => write!(f, "invalid document structure: {msg}"),
            Error::Encrypted => write!(
                f,
                "document is encrypted (/Encrypt in trailer); encryption is not supported"
            ),
            Error::Unsupported { feature, detail } => {
                write!(f, "unsupported PDF feature: {feature} ({detail})")
            }
            Error::MissingReference(m) => write!(
                f,
                "referenced object {} {} R is not defined (needed as {})",
                m.reference.number, m.reference.generation, m.context
            ),
            Error::CycleDetected { context } => {
                write!(f, "indirect reference cycle detected in {context}")
            }
            Error::LimitExceeded { limit, max } => {
                write!(f, "resource limit exceeded: {limit} (max {max})")
            }
            Error::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
