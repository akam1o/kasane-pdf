//! Error type returned by every fallible operation in kasane-pdf.
//!
//! Distinct variants are used so callers (and future layers) can tell
//! apart "this file is broken", "this feature is not implemented yet",
//! "the file intentionally uses a feature we refuse to process", "a
//! resource bound stopped the operation", "the caller passed a bad
//! argument" and "a signature's policy forbids this edit". Branch on
//! [`Error::kind`] (an [`ErrorKind`]) rather than on message text.
//!
//! What is *not* an error: a non-conforming input the engine could
//! still process exactly is a [`Warning`](crate::Warning) recorded on
//! the document; a damaged cross-reference table rebuilt by
//! `Document::from_bytes_recovering` is a
//! [`RecoveryReport`](crate::RecoveryReport); an approximation the
//! renderer had to make is reported on the
//! [`RenderedPage`](crate::RenderedPage). Errors are refusals: no
//! partial result accompanies them.

use std::fmt;

/// An object that was referenced but does not exist, and the kind of
/// structure that was expected.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MissingReference {
    /// The indirect reference that could not be resolved.
    pub reference: crate::IndirectRef,
    /// Human-readable context, e.g. `"page tree Kids entry"`.
    pub context: &'static str,
}

/// The category of an [`Error`], for classification without matching
/// on the full variant.
///
/// New kinds may be added in minor releases; match with a wildcard
/// arm. [`ErrorKind::as_str`] (and `Display`) gives the stable string
/// the CLI prints in `error [<kind>]: …`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Not a PDF file at all.
    NotPdf,
    /// Malformed syntax.
    Parse,
    /// The input ended inside a construct.
    UnexpectedEof,
    /// Syntactically valid, semantically impossible.
    Structure,
    /// `/Encrypt` in the trailer.
    Encrypted,
    /// A valid PDF feature the engine does not implement.
    Unsupported,
    /// A reference to an undefined object.
    MissingReference,
    /// A cycle in the object graph.
    CycleDetected,
    /// A [`Limits`](crate::Limits) bound was hit.
    LimitExceeded,
    /// An argument the caller supplied (an option, a page index, a
    /// rectangle) is invalid; the file is not at fault.
    InvalidArgument,
    /// A signature's declared policy forbids the requested edit.
    SignaturePolicy,
    /// I/O (CLI only).
    Io,
}

impl ErrorKind {
    /// Short, stable machine-readable name — the same strings 0.1
    /// returned from `Error::kind()`.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::NotPdf => "not a PDF",
            ErrorKind::Parse => "parse error",
            ErrorKind::UnexpectedEof => "unexpected end of input",
            ErrorKind::Structure => "invalid document structure",
            ErrorKind::Encrypted => "encrypted document",
            ErrorKind::Unsupported => "unsupported feature",
            ErrorKind::MissingReference => "missing reference",
            ErrorKind::CycleDetected => "reference cycle",
            ErrorKind::LimitExceeded => "limit exceeded",
            ErrorKind::InvalidArgument => "invalid argument",
            ErrorKind::SignaturePolicy => "signature policy",
            ErrorKind::Io => "I/O error",
        }
    }

    /// The file is malformed (as opposed to valid but unsupported,
    /// bounded, or refused by policy).
    pub fn is_malformed_input(self) -> bool {
        matches!(
            self,
            ErrorKind::NotPdf
                | ErrorKind::Parse
                | ErrorKind::UnexpectedEof
                | ErrorKind::Structure
                | ErrorKind::MissingReference
                | ErrorKind::CycleDetected
        )
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which signature rule refused an edit (see
/// [`Error::SignaturePolicy`]). Every rule is *structural*: the engine
/// reads what the signature declares and never verifies the
/// signature cryptographically.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureRule {
    /// `/ByteRange` does not cover the signed revision from byte 0 with
    /// only `/Contents` left out, so what the signature covers cannot
    /// be reasoned about.
    ByteRangeNotIntact,
    /// `/DocMDP` `/P 1`: no change of any kind is permitted.
    DocMdpNoChanges,
    /// `/DocMDP` `/P 2`: only form filling is permitted.
    DocMdpFormFillOnly,
    /// `/DocMDP` `/P 3`: form filling and annotations are permitted.
    DocMdpFormFillAndAnnotations,
    /// A `/FieldMDP` field lock is present and is not evaluated.
    FieldMdpNotEvaluated,
    /// A full rewrite would re-serialise the signed bytes and so
    /// invalidate every signature; only an incremental update can
    /// keep them.
    RewriteOfSignedDocument,
}

impl fmt::Display for SignatureRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SignatureRule::ByteRangeNotIntact => "/ByteRange does not cover the signed revision",
            SignatureRule::DocMdpNoChanges => "/DocMDP /P 1 permits no changes",
            SignatureRule::DocMdpFormFillOnly => "/DocMDP /P 2 permits form filling only",
            SignatureRule::DocMdpFormFillAndAnnotations => {
                "/DocMDP /P 3 permits form filling and annotations only"
            }
            SignatureRule::FieldMdpNotEvaluated => "/FieldMDP lock is not evaluated",
            SignatureRule::RewriteOfSignedDocument => {
                "a full rewrite would invalidate the signature"
            }
        })
    }
}

/// All errors produced by kasane-pdf.
///
/// `Display` messages include byte offsets or object numbers where known,
/// to aid debugging of malformed files. New variants may be added in
/// minor releases; match with a wildcard arm or classify through
/// [`Error::kind`].
#[non_exhaustive]
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
    /// The document is encrypted and the supplied password (none: the
    /// empty user password) authenticates neither as user nor as owner
    /// password. No part of the document is returned. A malformed
    /// `/Encrypt` dictionary is [`Error::Structure`]; an unimplemented
    /// security handler or method is [`Error::Unsupported`].
    Encrypted,
    /// The document uses a valid PDF feature that the engine does not
    /// implement. The operation that needed it fails as a whole; no
    /// partial result is produced.
    Unsupported {
        /// The feature that is not implemented (e.g. `"knockout group"`).
        feature: &'static str,
        /// Additional detail (offset, dictionary key, …).
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
    /// A configured [`Limits`](crate::Limits) bound was exceeded. The
    /// operation stops rather than consuming unbounded memory or time.
    LimitExceeded {
        /// Which limit was exceeded.
        limit: &'static str,
        /// The configured maximum.
        max: u64,
    },
    /// An argument supplied by the caller is invalid — a non-finite
    /// DPI, a limit of zero, a page index past the end. The file is
    /// not at fault and the same file succeeds with a valid argument.
    InvalidArgument {
        /// Which argument (e.g. `"dpi"`, `"max_input_bytes"`).
        argument: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// A signature in the document declares a policy that forbids the
    /// requested edit, or the edit (a full rewrite) would invalidate
    /// the signature. Distinct from [`Error::Unsupported`]: the
    /// engine understood the request and refused it on the
    /// signature's terms. The refusal is structural — the signature
    /// itself is never verified cryptographically.
    SignaturePolicy {
        /// Which rule refused.
        rule: SignatureRule,
        /// The signature field and what it declared.
        detail: String,
    },
    /// I/O error from the caller-provided source (CLI only; the library
    /// parses from byte slices and does not perform I/O itself).
    Io(std::io::Error),
}

impl Error {
    /// The category of this error, for classification.
    ///
    /// `kind().as_str()` is the stable string the 0.1 API returned
    /// and the CLI prints.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::NotPdf => ErrorKind::NotPdf,
            Error::Parse(_) => ErrorKind::Parse,
            Error::UnexpectedEof { .. } => ErrorKind::UnexpectedEof,
            Error::Structure(_) => ErrorKind::Structure,
            Error::Encrypted => ErrorKind::Encrypted,
            Error::Unsupported { .. } => ErrorKind::Unsupported,
            Error::MissingReference(_) => ErrorKind::MissingReference,
            Error::CycleDetected { .. } => ErrorKind::CycleDetected,
            Error::LimitExceeded { .. } => ErrorKind::LimitExceeded,
            Error::InvalidArgument { .. } => ErrorKind::InvalidArgument,
            Error::SignaturePolicy { .. } => ErrorKind::SignaturePolicy,
            Error::Io(_) => ErrorKind::Io,
        }
    }

    /// Build an [`Error::InvalidArgument`].
    pub(crate) fn invalid_argument(argument: &'static str, detail: impl Into<String>) -> Self {
        Error::InvalidArgument {
            argument,
            detail: detail.into(),
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
            Error::Encrypted => {
                write!(f, "document is encrypted and the password does not open it")
            }
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
            Error::InvalidArgument { argument, detail } => {
                write!(f, "invalid argument {argument}: {detail}")
            }
            Error::SignaturePolicy { rule, detail } => {
                write!(f, "refused by signature policy: {rule} ({detail})")
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
