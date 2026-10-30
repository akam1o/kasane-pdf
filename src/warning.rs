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
    /// An indirect reference to an object the file does not define —
    /// no cross-reference entry for that object number and generation,
    /// or a free entry. §7.3.10 requires a conforming reader to treat
    /// such a reference as the null object, so the engine does, and
    /// records it once per reference. A reference whose entry *exists*
    /// but points at damage is still `Error::Parse` /
    /// `Error::MissingReference` (that is corruption, not an undefined
    /// object).
    UndefinedReference {
        /// The reference that resolved to null.
        reference: crate::IndirectRef,
    },
    /// A content stream ended with a block still open — `q` without
    /// `Q`, `BT` without `ET`, `BMC`/`BDC` without `EMC`, `BX` without
    /// `EX`. The closing operators were supplied at the end of the
    /// stream, where nothing further could depend on them, so the
    /// result is exact; the file is not conforming (§7.8.2).
    UnterminatedContentBlock {
        /// Which block(s), as the parser saw them.
        detail: String,
    },
    /// A rendering intent that is not one of the four names of
    /// §8.6.5.8. Treated as `RelativeColorimetric`, the intent a
    /// reader falls back to; this engine applies no colour management
    /// that would depend on it, so the output is unaffected.
    UnknownRenderingIntent {
        /// The name as written.
        name: String,
    },
    /// A stream filter's input ended with a fragment that encodes no
    /// data — an `ASCII85Decode` final group of one character (§7.4.3
    /// allows 2 to 4). Every byte the stream encodes was produced; the
    /// fragment was ignored.
    IgnoredFilterFragment {
        /// The filter.
        filter: &'static str,
        /// What the fragment was.
        detail: String,
    },
    /// A recovery-mode rule was applied: the input was reinterpreted
    /// the way every major reader does, without a basis in the
    /// specification. Recorded only on a document opened with
    /// `Document::from_bytes_recovering` (see
    /// `Document::recovery_mode`); a strict document refuses the same
    /// input with an error.
    RecoveryApplied {
        /// Which rule.
        rule: RecoveryRule,
        /// What was found.
        detail: String,
    },
    /// A string of the encryption dictionary is longer than its
    /// revision specifies (`/O` `/U`: 32 bytes for revisions 2–4, 48 for
    /// 5–6; `/OE` `/UE`: 32; `/Perms`: 16 — ISO 32000-2 Tables 21–22).
    /// Some writers pad them; the specified leading bytes are used and
    /// the rest is ignored, as every reader does.
    OverlongEncryptionEntry {
        /// The entry (`O`, `U`, `OE`, `UE` or `Perms`).
        key: &'static str,
        /// Its length in bytes.
        length: usize,
        /// The length the revision specifies.
        specified: usize,
    },
}

/// The recovery-mode reinterpretations (see
/// [`Warning::RecoveryApplied`]). Each has one reading shared by the
/// major readers and none in ISO 32000-1; none is applied in strict
/// mode. New rules may be added in minor releases; match with a
/// wildcard arm.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoveryRule {
    /// A page without `/MediaBox` on itself or any ancestor (required
    /// by Table 30) is given US Letter, 612 × 792 points.
    MediaBoxDefault,
    /// A `Q` with no saved state to restore is ignored.
    IgnoredRestoreWithoutSave,
    /// An operator the engine does not know, outside a `BX`/`EX`
    /// section, is dropped with its operands.
    IgnoredUnknownOperator,
    /// A graphics-state or colour operator between path construction
    /// and the painting operator (§8.2 Figure 9 forbids it) is applied
    /// where it stands, before the path is painted.
    OperatorInsidePathObject,
    /// An `/Annots` entry that is not a dictionary, or an appearance
    /// state that is not a stream, is skipped — nothing is drawn for
    /// it (Table 164 / §12.5.5 require a dictionary and a stream).
    IgnoredInvalidAnnotation,
    /// A font resource that is missing or not a dictionary is replaced
    /// by Helvetica (drawn with the bundled substitute, reported as a
    /// substitution) so the text is shown rather than lost.
    DefaultFontSubstituted,
    /// A `BDC` property name with no entry in `/Properties` is treated
    /// as a marked-content sequence without properties (visible).
    IgnoredUndefinedProperty,
    /// A `/Differences` element that is neither an integer nor a name
    /// is skipped (§9.6.6.1).
    IgnoredInvalidEncodingEntry,
    /// An image `/SMask` that is not a stream (typically the name
    /// `/None`, which only an ExtGState may carry) is treated as no
    /// soft mask.
    IgnoredInvalidSoftMask,
    /// A revision-4 file encryption key shorter than 128 bits is
    /// extended to 16 bytes with zeros before the object keys are
    /// derived (Algorithm 1). Acrobat does this and pdf.js follows it;
    /// ISO 32000 uses the key as is, so a strict document derives the
    /// specified object keys and such a file's data does not decrypt.
    PaddedShortEncryptionKey,
}

impl fmt::Display for RecoveryRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RecoveryRule::MediaBoxDefault => "page without /MediaBox given US Letter",
            RecoveryRule::IgnoredRestoreWithoutSave => "Q without a saved state ignored",
            RecoveryRule::IgnoredUnknownOperator => "unknown operator dropped",
            RecoveryRule::OperatorInsidePathObject => {
                "operator inside a path object applied before painting"
            }
            RecoveryRule::IgnoredInvalidAnnotation => "invalid annotation entry skipped",
            RecoveryRule::DefaultFontSubstituted => "missing font resource replaced by Helvetica",
            RecoveryRule::IgnoredUndefinedProperty => {
                "undefined /Properties name treated as plain marked content"
            }
            RecoveryRule::IgnoredInvalidEncodingEntry => "invalid /Differences entry skipped",
            RecoveryRule::IgnoredInvalidSoftMask => "image /SMask that is not a stream ignored",
            RecoveryRule::PaddedShortEncryptionKey => {
                "revision-4 encryption key shorter than 128 bits zero-padded to 16 bytes"
            }
        })
    }
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
            Warning::UndefinedReference { reference } => write!(
                f,
                "reference {} {} R to an undefined object treated as null (§7.3.10)",
                reference.number, reference.generation
            ),
            Warning::UnterminatedContentBlock { detail } => {
                write!(
                    f,
                    "content stream ended with an open block, closed at the end: {detail}"
                )
            }
            Warning::UnknownRenderingIntent { name } => {
                write!(f, "rendering intent /{name} is not one of §8.6.5.8; treated as RelativeColorimetric")
            }
            Warning::IgnoredFilterFragment { filter, detail } => {
                write!(
                    f,
                    "{filter}: trailing fragment that encodes no data ignored ({detail})"
                )
            }
            Warning::OverlongEncryptionEntry {
                key,
                length,
                specified,
            } => write!(
                f,
                "/Encrypt /{key} is {length} bytes; the first {specified} are used"
            ),
            Warning::RecoveryApplied { rule, detail } => {
                write!(f, "recovery: {rule} ({detail})")
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
        // One record per undefined reference: the same dangling
        // reference is resolved again on every page that uses it.
        if matches!(w, Warning::UndefinedReference { .. }) && self.list.contains(&w) {
            return;
        }
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
