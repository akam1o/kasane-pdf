//! Non-fatal findings: input that deviates from ISO 32000-1 in a way
//! the engine can process *exactly* anyway, recorded instead of
//! refused.
//!
//! The strict parser still refuses everything it cannot process
//! exactly (`Error::Structure`, `Error::Unsupported`, …). A
//! [`Warning`] is the third outcome between a clean pass and a
//! refusal: the result is complete and correct for the bytes that are
//! there, but the file was not conforming and a reader that wants to
//! know should look. Warnings accumulate on the [`Document`] that
//! produced them (every operation — render, extract, rewrite — appends
//! to the same list) and are read with [`Document::take_warnings`] or
//! [`Document::warnings`]. The list is bounded by
//! [`MAX_WARNINGS`]; overflow is counted, never lost silently
//! ([`Document::warnings_dropped`]).
//!
//! What is *not* a warning: an approximation the renderer had to make
//! (a substituted font, an ICC profile it could not evaluate) — those
//! are reported on the result itself (`RenderedPage::font_substitutions`,
//! `RenderedPage::color_approximations`); a damaged cross-reference
//! table rebuilt by `Document::from_bytes_recovering` — that is a
//! `RecoveryReport`.
//!
//! [`Document`]: crate::Document
//! [`Document::take_warnings`]: crate::Document::take_warnings
//! [`Document::warnings`]: crate::Document::warnings
//! [`Document::warnings_dropped`]: crate::Document::warnings_dropped

use std::fmt;

/// Most warnings a [`Document`](crate::Document) retains; further ones
/// are counted in `warnings_dropped()`.
pub const MAX_WARNINGS: usize = 1024;

/// A non-fatal deviation from the specification that the engine
/// processed exactly and recorded.
///
/// New variants may be added in minor releases; match with a wildcard
/// arm.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// A `/FlateDecode` stream ended after a complete sync-flush block
    /// (`00 00 FF FF`) without the final block and the Adler-32
    /// checksum (§7.4.4.2 requires a complete zlib stream). Every
    /// deflate block before the end was complete, so `decoded_bytes`
    /// bytes were decoded exactly as written; what cannot be verified
    /// is the checksum. A stream that ends *inside* a block is still
    /// refused with `Error::Parse`.
    UnterminatedFlateStream {
        /// Bytes the stream decoded to.
        decoded_bytes: usize,
    },
    /// A `bfchar` / `bfrange` entry of a `/ToUnicode` CMap (§9.10.3)
    /// could not be read — a hex string that is not hexadecimal, a
    /// destination that is not UTF-16BE — and was skipped. Codes it
    /// would have mapped stay unmapped (`CharInfo::unicode == None`);
    /// nothing is guessed in their place.
    MalformedToUnicodeEntry {
        /// What was wrong, as the parser saw it.
        detail: String,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::UnterminatedFlateStream { decoded_bytes } => write!(
                f,
                "FlateDecode stream ends after a sync flush without a final block or checksum \
                 ({decoded_bytes} bytes decoded)"
            ),
            Warning::MalformedToUnicodeEntry { detail } => {
                write!(f, "ToUnicode entry skipped: {detail}")
            }
        }
    }
}

/// Bounded sink the engine appends to.
#[derive(Debug, Default)]
pub(crate) struct WarningSink {
    pub(crate) list: Vec<Warning>,
    pub(crate) dropped: u64,
}

impl WarningSink {
    pub(crate) fn push(&mut self, w: Warning) {
        if self.list.len() < MAX_WARNINGS {
            self.list.push(w);
        } else {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    pub(crate) fn extend(&mut self, ws: Vec<Warning>) {
        for w in ws {
            self.push(w);
        }
    }
}
