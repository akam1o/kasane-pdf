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
//! - `origin` — the page-space (PDF user-space) point at which the
//!   character's glyph is drawn, in PDF points. The origin is the
//!   *glyph* origin: the point where the character's baseline begins,
//!   before any glyph-specific offset.
//! - `advance` — the vector from this character's origin to the
//!   next character's origin, in page space. For horizontal text it
//!   points right (+x); its length is the glyph's advance width.
//! - `bounds` — the axis-aligned bounding box of the glyph's
//!   outline in page space, or `None` if the font has no outline for
//!   the glyph (e.g. space, or a `.notdef` with no shape). A
//!   zero-area glyph produces a degenerate bbox, not `None`.
//!
//! # Resolution
//!
//! `unicode` is `Some` when the character resolved via the font's
//! `/ToUnicode` CMap or, failing that, via the font's
//! `WinAnsiEncoding` glyph name → AGL → Unicode fallback
//! (§9.10.2). It is `None` when neither path produces a value —
//! for example, a `.notdef` glyph with no encoding entry. A
//! mapped-to-U+FFFD produces `Some("\u{FFFD}")` — distinct from
//! unresolved `None`.
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
use crate::font::SimpleFont;
use crate::limits::Limits;
use crate::page::Page;
use crate::render::Renderer;
use std::rc::Rc;

/// One extracted character — a single PDF character code with its
/// resolved Unicode value and page-space geometry.
#[derive(Debug, Clone)]
pub struct CharInfo {
    /// The raw character code (0–255 for simple fonts).
    pub code: u8,
    /// Resolved Unicode string, or `None` if unresolvable.
    /// May hold multiple scalars (e.g. a ligature mapping).
    pub unicode: Option<String>,
    /// How `unicode` was resolved.
    pub resolved_via: ResolveKind,
    /// Glyph origin in page space (PDF points, CropBox-relative
    /// after `/Rotate` and `/UserUnit` are applied).
    pub origin: (f64, f64),
    /// Advance vector — the displacement from this origin to the
    /// next character's origin, in page space.
    pub advance: (f64, f64),
    /// Bounding box of the glyph's outline in page space, or `None`
    /// if the glyph has no outline. Axis-aligned; not the selection
    /// rectangle.
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
/// Runs the same content interpreter as `render_page` but records
/// [`CharInfo`] instead of rasterising. The pixmap is still built
/// (M6 does not add a headless mode); call `render_page` for pixels.
pub fn extract_text(doc: &Document, page: &Page, limits: &Limits) -> Result<ExtractedText, Error> {
    // Reuse the render machinery: we run the content stream but
    // divert each character to the `extract` buffer.
    let opts = crate::render::RenderOptions {
        dpi: 72.0, // coordinates are page-space; dpi only affects pixmap size
        limits: limits.clone(),
    };
    // We need a Renderer with `extract` set — add a thin wrapper.
    let ops = doc.page_operations(page)?;
    let mut r = Renderer::for_extraction(doc, page, &opts)?;
    r.run(&ops)?;
    let chars = r.take_extracted().unwrap_or_default();
    Ok(ExtractedText { chars, omitted: 0 })
}

/// Resolve `code` through the font — used by the renderer's
/// extraction hook. Public for `render.rs`; not part of the
/// crate's public API.
pub(crate) fn resolve_unicode(font: &Rc<SimpleFont>, code: u8) -> (Option<String>, ResolveKind) {
    match font.unicode(code) {
        Some(s) if font.has_tounicode() => (Some(s), ResolveKind::ToUnicode),
        Some(s) => (Some(s), ResolveKind::EncodingFallback),
        None => (None, ResolveKind::Unresolved),
    }
}
