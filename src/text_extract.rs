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
//! `/ToUnicode` CMap or, without one, via an encoding glyph name and
//! the Adobe glyph-name rules, the embedded program's Unicode `cmap`,
//! or the Adobe-Japan1 collection (§9.10.2; `resolved_via` names the
//! source — see [`ResolveKind`]). It is `None` when no source answers
//! unambiguously; a `.notdef` glyph (CID 0, GID 0) has no text. A
//! `/ToUnicode` mapping to U+FFFD produces `Some("\u{FFFD}")` —
//! distinct from unresolved `None`.
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

/// Options for [`extract_text`].
///
/// Construct with [`TextOptions::default`] and the `with_*` methods;
/// the struct is `#[non_exhaustive]` so options can be added without
/// a breaking change. What is deliberately *not* an option: the
/// `/ActualText` replacement (§14.9.4) is always applied — it is the
/// producer's statement of what the text is; Unicode normalisation,
/// reading order, word and line grouping are out of scope (the
/// result is one entry per character code in content order).
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
    /// Resource bounds for the extraction.
    pub limits: Limits,
    /// Also report text inside optional content (§8.11) that is OFF in
    /// the default configuration. Off by default, so the result mirrors
    /// what a viewer shows; an OFF layer is still *interpreted* either
    /// way (its operators and resources must be valid).
    pub include_hidden_content: bool,
    /// Keep only characters whose glyph origin lies inside this
    /// rectangle (user space, PDF points, `/Rotate` not applied).
    /// `None` keeps every character. The filter is applied after
    /// extraction, so limits and errors are unaffected by it.
    pub region: Option<crate::page::Rect>,
    /// Also report the page's annotations and form fields in
    /// [`ExtractedText::annotations`] (off by default: `chars` and
    /// everything else are unchanged either way). Each annotation's
    /// appearance is interpreted like a render would: a malformed
    /// appearance stream fails the extraction; an annotation with no
    /// appearance the renderer can draw (a note without `/AP`, an
    /// appearance-state dictionary with no state selected) simply has
    /// no appearance text.
    pub include_annotations: bool,
}

impl TextOptions {
    /// The defaults: default limits, hidden content excluded, no region.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the resource bounds.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Include (`true`) or exclude (`false`, the default) text in
    /// optional content that is OFF.
    #[must_use]
    pub fn with_hidden_content(mut self, include: bool) -> Self {
        self.include_hidden_content = include;
        self
    }

    /// Keep only characters whose origin lies inside `region`.
    #[must_use]
    pub fn with_region(mut self, region: crate::page::Rect) -> Self {
        self.region = Some(region);
        self
    }

    /// Report annotation and form-field text in
    /// [`ExtractedText::annotations`] (`true`), or not (`false`, the
    /// default).
    #[must_use]
    pub fn with_annotations(mut self, include: bool) -> Self {
        self.include_annotations = include;
        self
    }
}

pub use crate::annot_text::{AnnotationText, FieldText};

/// One extracted character — a single PDF character code with its
/// resolved Unicode value and page-space geometry.
///
/// Fields may be added in minor releases (`#[non_exhaustive]`).
#[non_exhaustive]
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
///
/// New kinds may be added in minor releases; match with a wildcard arm.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveKind {
    /// Via `/ToUnicode` CMap (preferred).
    ToUnicode,
    /// Via a glyph name of the font's encoding (the PDF `/Encoding` or
    /// the program's built-in one) and the Adobe glyph-name rules
    /// (§9.10.2 method 2; also for Type 3 character names).
    EncodingFallback,
    /// Not resolvable — `unicode` is `None`.
    Unresolved,
    /// Replaced by the enclosing marked content's `/ActualText`
    /// (§14.9.4): the first character of the span carries the whole
    /// replacement string, the rest carry `Some("")`. A span that
    /// shows no glyph at all yields one synthetic entry (`code` 0,
    /// zero advance, no bounds) so the text is not lost.
    ActualText,
    // New variants go last: the implicit discriminants of the earlier
    // ones are part of the public contract.
    /// No `/ToUnicode` and no meaningful encoding name: through the
    /// embedded font program — its Unicode `cmap` read backwards
    /// (unambiguous glyphs only) or its own glyph name (`post`, CFF
    /// charset). Not a method of ISO 32000; never used for a
    /// substituted face.
    FontProgram,
    /// No `/ToUnicode`: the CID through the font's Adobe character
    /// collection, `Adobe-<ordering>-UCS2` (§9.10.2 method 3). Only the
    /// collections bundled with the `cjk-fallback` feature (Japan1).
    CharacterCollection,
}

/// The extracted text for one page.
///
/// Fields may be added in minor releases (`#[non_exhaustive]`).
#[non_exhaustive]
#[derive(Debug)]
pub struct ExtractedText {
    /// Character entries in content-execution order.
    pub chars: Vec<CharInfo>,
    /// How many `CharInfo`s were skipped because they exceeded
    /// [`Limits::max_text_codes`] before extraction could record
    /// them. Non-zero on a `LimitExceeded` path — the `Err` is
    /// propagated separately, but on success this is always 0.
    pub omitted: u64,
    /// The page's annotations and form fields, in `/Annots` order —
    /// only with [`TextOptions::with_annotations`], else empty. Their
    /// text is never in `chars`. `region` filters their `appearance`
    /// characters like `chars`.
    pub annotations: Vec<AnnotationText>,
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
pub fn extract_text(
    doc: &Document,
    page: &Page,
    opts: &TextOptions,
) -> Result<ExtractedText, Error> {
    opts.limits.validate()?;
    if let Some(r) = &opts.region {
        if !(r.llx.is_finite() && r.lly.is_finite() && r.urx.is_finite() && r.ury.is_finite()) {
            return Err(Error::invalid_argument(
                "region",
                "rectangle has non-finite values",
            ));
        }
    }
    let mut out = extract_text_impl(
        doc,
        page,
        &opts.limits,
        opts.include_hidden_content,
        opts.include_annotations,
        true,
    )?;
    if let Some(r) = &opts.region {
        let (x0, x1) = (r.llx.min(r.urx), r.llx.max(r.urx));
        let (y0, y1) = (r.lly.min(r.ury), r.lly.max(r.ury));
        let inside = |c: &CharInfo| {
            let (x, y) = c.origin;
            x >= x0 && x <= x1 && y >= y0 && y <= y1
        };
        out.chars.retain(inside);
        for a in &mut out.annotations {
            a.appearance.retain(inside);
        }
    }
    Ok(out)
}

/// Everything a page shows as text, for a search: hidden optional
/// content and annotation appearances included, characters without
/// their `bounds` (no glyph outline is evaluated).
pub(crate) fn extract_text_for_search(
    doc: &Document,
    page: &Page,
    limits: &Limits,
    annotations: bool,
) -> Result<ExtractedText, Error> {
    limits.validate()?;
    extract_text_impl(doc, page, limits, true, annotations, false)
}

fn extract_text_impl(
    doc: &Document,
    page: &Page,
    limits: &Limits,
    include_hidden: bool,
    include_annotations: bool,
    glyph_bounds: bool,
) -> Result<ExtractedText, Error> {
    let _budget = doc.decoded_budget_scope();
    let opts = crate::render::RenderOptions {
        dpi: 72.0, // unused — extraction reports user space, no pixels
        limits: limits.clone(),
    };
    let ops = doc.page_operations(page)?;
    // The renderer's placeholder pixmap (nothing is painted), owned
    // here so it is freed with the call.
    let mut pixmap = tiny_skia::Pixmap::new(1, 1)
        .ok_or_else(|| Error::Structure("cannot allocate extraction pixmap".into()))?;
    let mut r = Renderer::for_extraction(doc, page, &opts, &mut pixmap)?;
    if include_hidden {
        r.include_hidden_content();
    }
    if !glyph_bounds {
        r.skip_glyph_bounds();
    }
    r.run(&ops)?;
    let mut annotations = Vec::new();
    let chars = if include_annotations {
        let n = r.extracted_len();
        let page_resources = page
            .resources()
            .cloned()
            .unwrap_or(crate::object::Object::Dict(Default::default()));
        r.draw_annotations(page, &page_resources)?;
        let (spans, hidden) = r.take_annotation_spans();
        let used = r.extracted_unicode_bytes();
        let mut all = r.take_extracted().unwrap_or_default();
        let drawn: Vec<(usize, Vec<CharInfo>)> = spans
            .into_iter()
            .map(|(i, range)| (i, all[range].to_vec()))
            .collect();
        all.truncate(n);
        annotations = crate::annot_text::collect(
            doc,
            page,
            drawn,
            &hidden,
            used,
            limits.max_extracted_unicode_bytes,
        )?;
        all
    } else {
        r.take_extracted().unwrap_or_default()
    };
    Ok(ExtractedText {
        chars,
        omitted: 0,
        annotations,
    })
}

/// Resolve `code` through the font — used by the renderer's
/// extraction hook. Public for `render.rs`; not part of the
/// crate's public API.
pub(crate) fn resolve_unicode(font: &Font, code: u32) -> (Option<String>, ResolveKind) {
    use crate::font::UnicodeSource as S;
    match font.unicode(code) {
        Some((s, S::ToUnicode)) => (Some(s), ResolveKind::ToUnicode),
        Some((s, S::GlyphName)) => (Some(s), ResolveKind::EncodingFallback),
        Some((s, S::FontProgram)) => (Some(s), ResolveKind::FontProgram),
        Some((s, S::CharacterCollection)) => (Some(s), ResolveKind::CharacterCollection),
        None => (None, ResolveKind::Unresolved),
    }
}
