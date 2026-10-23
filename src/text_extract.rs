//! Text extraction — positioned Unicode characters in content order.
//!
//! `extract_text` replays the page's content stream (including
//! nested Form XObjects) and records one [`CharInfo`] per character
//! code processed by `Tj`/`TJ`/`'`/`\"`. This is *not* a reader-order
//! reconstruction: the output follows the order the characters were
//! emitted by the content stream, and it does not invent spaces,
//! line breaks, or paragraph boundaries.
//!
//! # Coordinates
//!
//! All geometry is in **PDF user space** — the coordinate system the
//! page's content stream operates in:
//!
//! - Unit: the default user-space unit, 1/72 inch ("PDF points").
//! - Origin and axes: the page's device-independent coordinate system;
//!   +x points right, +y points up.
//! - `MediaBox`/`CropBox`: coordinates are *not* rebased — a page
//!   cropped to `[100 100 300 200]` still reports a glyph at (120,140)
//!   as (120,140). The visible region simply starts at the CropBox's
//!   lower-left corner.
//! - `/Rotate`: **not applied**. The same page at Rotate 0/90/180/270
//!   yields identical coordinates — rotation only changes display.
//! - Form XObjects: the form's `/Matrix` is concatenated into the CTM,
//!   so text inside a form reports true user-space positions.
//! - `Trm`/`Tm`, `Tz` (horizontal scaling) and `Ts` (text rise) are all
//!   reflected in `origin`, `advance` and `bounds`.
//! - Independent of render DPI: no pixels are involved.
//!
//! Fields:
//!
//! - `origin` — the glyph's origin point (baseline start) in user
//!   space, i.e. the current text position when the code was shown.
//! - `advance` — the displacement vector from this glyph's origin to
//!   the next character's origin, in user space. It carries no
//!   translation of its own: it is the transform of the text-space
//!   advance point minus the transform of the origin, so it is already
//!   in user-space units and directions.
//! - `bounds` — the axis-aligned bounding box of the glyph's *outline*
//!   in user space, computed from the transformed outline's four
//!   corners (correct under rotation/shear), or `None` when the glyph
//!   has no outline (e.g. space). It is the ink boundary — **not** a
//!   selection rectangle, which this API does not provide.
//!
//! # Resolution
//!
//! `unicode` is `Some` when the character resolved via the font's
//! `/ToUnicode` CMap or, failing that and only for simple fonts, via
//! the font's `WinAnsiEncoding` glyph name → AGL → Unicode fallback
//! (§9.10.2). It is `None` when neither path produces a value —
//! including *every* code on a composite font without `/ToUnicode`
//! (Identity-H carries no Unicode guess). A mapped-to-U+FFFD produces
//! `Some("\u{FFFD}")` — distinct from unresolved `None`.
//!
//! # Scoping
//!
//! - One character code → one `CharInfo`, even when the code maps
//!   to multiple Unicode scalars (e.g. a ligature mapping). The
//!   `unicode` field holds the whole string; the code's *position*
//!   is not split.
//! - Rendering mode 3 (invisible) characters are included — the
//!   caller decides whether to show them.
//! - `ActualText` and Optional Content (`/OC`) are not consulted:
//!   a page that uses them produces whatever the ToUnicode/
//!   encoding path yields, which may not match visible text.
//!   This is a documented limitation — not silent wrong output.

use crate::document::Document;
use crate::error::Error;
use crate::font::Font;
use crate::limits::Limits;
use crate::page::Page;
use crate::render::Renderer;

/// One extracted character — a single PDF character code with its
/// resolved Unicode value and page-space geometry.
#[derive(Debug, Clone)]
pub struct CharInfo {
    /// The raw character code — 0–255 for simple fonts, 0–65535
    /// (2-byte big-endian) for composite fonts.
    pub code: u32,
    /// Resolved Unicode string, or `None` if unresolvable.
    /// May hold multiple scalars (e.g. a ligature mapping).
    pub unicode: Option<String>,
    /// How `unicode` was resolved.
    pub resolved_via: ResolveKind,
    /// Glyph origin in PDF user space (points; +x right, +y up;
    /// `/Rotate` not applied — see module docs).
    pub origin: (f64, f64),
    /// Advance vector — the displacement from this origin to the
    /// next character's origin, in user space (no translation).
    pub advance: (f64, f64),
    /// Axis-aligned bounding box of the glyph's *outline* in user
    /// space `(minX, minY, maxX, maxY)`, or `None` if the glyph has
    /// no outline (e.g. space). Ink boundary, not a selection rect.
    pub bounds: Option<(f64, f64, f64, f64)>,
    /// Text rendering mode in effect (0–7; 3 = invisible).
    pub render_mode: u8,
    /// Font size in text-space units (`Tfs`).
    pub font_size: f64,
    /// Text rise `Trise` in text-space units.
    pub rise: f64,
    /// TJ adjustment applied before this glyph (0 for `Tj`).
    pub tj_adjust: f64,
    /// `true` if shown inside `BT`/`ET` — always true in practice
    /// since `Tj` outside a text object is an error.
    pub in_text: bool,
}

/// How a `CharInfo`'s Unicode value was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveKind {
    /// Via `/ToUnicode` CMap (preferred).
    ToUnicode,
    /// Via the font's `Encoding` → AGL glyph name → Unicode
    /// (the spec's fallback for non-symbolic simple fonts).
    EncodingFallback,
    /// Not resolvable — `unicode` is `None`.
    Unresolved,
    /// Replaced by the enclosing marked content's `/ActualText`
    /// (§14.9.4): the first character of the span carries the whole
    /// replacement string, the rest carry `Some("")`. A span that
    /// shows no glyph at all yields one synthetic entry (`code` 0,
    /// zero advance, no bounds) so the text is not lost.
    ActualText,
}

/// The extracted text for one page.
#[derive(Debug)]
pub struct ExtractedText {
    /// Character entries in content-execution order.
    pub chars: Vec<CharInfo>,
    /// How many `CharInfo`s were skipped because they exceeded
    /// [`Limits::max_text_codes`] before extraction could record
    /// them. Non-zero on a `LimitExceeded` path — the `Err` is
    /// propagated separately, but on success this is always 0.
    pub omitted: u64,
}

/// Extract positioned text from `page` in content order.
///
/// Runs the same content interpreter as `render_page` in headless
/// mode: every operator executes (text state, resources, marked
/// content checks, limits behave identically) but nothing is
/// rasterised, clipped, or image-decoded.
///
/// Fails — returning no partial result — on malformed input,
/// unsupported constructs (e.g. `/ActualText` replacement or Optional
/// Content in the executed stream), and resource limits. Unresolved
/// characters are part of a *successful* result, distinct from `Err`.
pub fn extract_text(doc: &Document, page: &Page, limits: &Limits) -> Result<ExtractedText, Error> {
    extract_text_impl(doc, page, limits, false)
}

/// Like [`extract_text`] but also reports text inside optional content
/// that the document's default configuration hides (§8.11). The
/// default [`extract_text`] mirrors what a viewer shows — hidden
/// layers (alternate languages, print-only marks, collapsed answers)
/// are left out, as `pdftotext` does; this variant is for indexing
/// every string the page carries.
pub fn extract_text_all(
    doc: &Document,
    page: &Page,
    limits: &Limits,
) -> Result<ExtractedText, Error> {
    extract_text_impl(doc, page, limits, true)
}

fn extract_text_impl(
    doc: &Document,
    page: &Page,
    limits: &Limits,
    include_hidden: bool,
) -> Result<ExtractedText, Error> {
    let opts = crate::render::RenderOptions {
        dpi: 72.0, // unused — extraction reports user space, no pixels
        limits: limits.clone(),
    };
    let ops = doc.page_operations(page)?;
    let mut r = Renderer::for_extraction(doc, page, &opts)?;
    if include_hidden {
        r.include_hidden_content();
    }
    r.run(&ops)?;
    let chars = r.take_extracted().unwrap_or_default();
    Ok(ExtractedText { chars, omitted: 0 })
}

/// Resolve `code` through the font — used by the renderer's
/// extraction hook. Public for `render.rs`; not part of the
/// crate's public API.
pub(crate) fn resolve_unicode(font: &Font, code: u32) -> (Option<String>, ResolveKind) {
    match font.unicode(code) {
        Some(s) if font.has_tounicode() => (Some(s), ResolveKind::ToUnicode),
        Some(s) => (Some(s), ResolveKind::EncodingFallback),
        None => (None, ResolveKind::Unresolved),
    }
}
