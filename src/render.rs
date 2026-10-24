//! Render a page to a pixel buffer and encode it as PNG; the same
//! interpreter runs headless for text extraction.
//!
//! This is the execution layer on top of the `Operation` stream: the
//! graphics state (`q`/`Q`/`cm`, line state, colour spaces of §8.6,
//! ExtGState parameters including soft masks and blend modes), path
//! construction, painting and clipping, shadings and tiling/shading
//! patterns, Form and Image XObjects (with transparency groups), inline
//! images, text in every font type the crate loads (rendering modes
//! 0–7), optional content, and annotation appearances. The feature
//! matrix with every refusal is `docs/features/rendering.md`; a
//! construct the renderer cannot draw exactly is a typed
//! `Error::Unsupported`/`Error::Structure`, never a silent skip.
//!
//! Ignored per spec (no effect on output): `BMC`/`BDC`/`EMC` without
//! optional-content meaning, `MP`/`DP`, `BX`/`EX` and
//! `Operation::Unknown` inside them.
//!
//! Path model: path elements are stored in *user space*
//! (untransformed) and the paint-time `CTM × page→pixmap` is applied
//! when the path is consumed. Keeping the path in user space is an
//! implementation choice; the observable result must match the CTM
//! in effect at paint time.
//!
//! Placement rules (§8.2 Figure 9): inside a *path object* only
//! path-construction ops, `W`/`W*`, and the terminating painting op
//! are allowed — `cm`/`q`/`Q`, colour ops, marked content, text ops
//! etc. are out of order there and are rejected with
//! `Error::Structure`. `W`/`W*` additionally must come after the
//! *last* path-construction op (§8.5.4), so a construction op after
//! `W`/`W*` and a second `W`/`W*` in the same path are also rejected.
//! The parse-only API (`page_operations`) still accepts these
//! sequences — placement is a *render-time* restriction.
//!
//! Clipping (§8.5.4): `W`/`W*` mark the path as a clip operand; the
//! clip is installed when the path is consumed — at a painting op
//! (where the path is painted *under the pre-W clip* first, then the
//! clip updates) or at `n`. The clip is a pixmap-sized `tiny_skia::Mask`;
//! `q`/`Q` save/restore it; nested clips intersect via
//! `Mask::intersect_path`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::colorspace::ColorSpace;
use crate::content::Operation;
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, IndirectRef, Name, Object};
use crate::page::Page;
use crate::shading::{parse_shading, Shading};
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

/// Graphics-state parameters saved/restored by `q`/`Q` (§8.4.1).
/// The path under construction is *not* part of the state.
#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    line_width: f64,
    line_cap: u8,
    line_join: u8,
    miter_limit: f64,
    dash: Option<(Vec<f64>, f64)>,
    flatness: f64,
    /// Current colour spaces (`CS`/`cs`, or the device space the
    /// `G`/`RG`/`K` family selects). Shared `Rc` so `q`/`Q` cloning
    /// never re-parses an ICC profile or tint transform.
    stroke_space: Rc<ColorSpace>,
    fill_space: Rc<ColorSpace>,
    stroke_color: Color,
    fill_color: Color,
    /// The shading pattern selected by `scn`/`SCN` while the colour
    /// space is `/Pattern` (§8.7.3.3); `None` = no pattern selected
    /// (paints nothing) or a non-pattern space.
    fill_pattern: Option<Rc<PatternPaint>>,
    stroke_pattern: Option<Rc<PatternPaint>>,
    /// Non-stroking alpha `ca` (§11.3.7.2) — multiplies the fill
    /// colour's alpha at paint time. 1.0 = opaque.
    fill_alpha: f64,
    /// Stroking alpha `CA` — multiplies the stroke colour's alpha.
    stroke_alpha: f64,
    /// Current blend mode `BM` (§11.3.5) — mapped to tiny-skia's
    /// `BlendMode` at paint time. `SourceOver` = PDF `/Normal`.
    blend_mode: tiny_skia::BlendMode,
    /// Current clip as a pixmap-space alpha mask; `None` = page bounds
    /// only. Stacked clips intersect via `Mask::intersect_path`. While
    /// a soft mask is set this is the *effective* mask — hard clip ×
    /// soft mask (§11.6.4.3: the mask multiplies the source shape).
    clip: Option<Mask>,
    /// The ExtGState `/SMask` in force (§11.6.5), already in device
    /// space; `None` = `/SMask /None`.
    soft_mask: Option<Rc<Mask>>,
    /// The clip without the soft mask, kept while `soft_mask` is set
    /// so a later `/SMask /None` can drop the mask and nothing else.
    hard_clip: Option<Mask>,
    /// Text state (§9.3) — per-object, not part of `q`/`Q` save.
    text: TextState,
}

/// Text-state parameters (Table 104) — the parts that live in the
/// graphics state and are saved/restored by `q`/`Q`.
///
/// `Tm` and `Tlm` are **not** here: they belong to the *text object*
/// (§9.4.1), are reinitialized by `BT`, and are not part of the
/// graphics state — `q`/`Q` inside a text object does not save them.
#[derive(Clone)]
struct TextState {
    /// Character spacing `Tc` (unscaled text-space units).
    char_spacing: f64,
    /// Word spacing `Tw` — applied only to the space character (0x20)
    /// for the single-byte fonts we support.
    word_spacing: f64,
    /// Horizontal scaling `Th` as a *factor* (`Tz / 100`).
    h_scale: f64,
    /// Leading `Tl` (unscaled text-space units).
    leading: f64,
    /// Current font — `None` until `Tf` selects one.
    font: Option<crate::font::Font>,
    /// Font resource name of `font`, kept for error messages.
    font_name: Vec<u8>,
    /// Font size `Tfs` (unscaled text-space units).
    font_size: f64,
    /// Rendering mode `Tmode` — 0 fill, 1 stroke, 2 fill+stroke, 3
    /// invisible, 4–7 clip variants.
    render_mode: i64,
    /// Text rise `Trise` (unscaled text-space units).
    rise: f64,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            font: None,
            font_name: Vec::new(),
            font_size: 0.0,
            render_mode: 0,
            rise: 0.0,
        }
    }
}

/// An RGBA8 colour produced from a device colour space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Red channel 0–255.
    pub r: u8,
    /// Green channel 0–255.
    pub g: u8,
    /// Blue channel 0–255.
    pub b: u8,
    /// Alpha 0–255 (255 = opaque).
    pub a: u8,
}

impl Color {
    /// Opaque black (the PDF initial colour).
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// Opaque white (the page background).
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    /// This colour with its alpha multiplied by `a` (0..=1) — applies
    /// the `ca`/`CA` ExtGState parameters at paint time (§11.3.7.2:
    /// the constant alpha is *multiplied* with any intrinsic alpha).
    pub fn with_alpha(self, a: f64) -> Color {
        let na = (self.a as f64 / 255.0) * clamp01(a);
        Color {
            a: to_u8(na),
            ..self
        }
    }
}

impl GraphicsState {
    fn new() -> Self {
        GraphicsState {
            ctm: Matrix::IDENTITY,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash: None,
            flatness: 1.0,
            stroke_space: Rc::new(ColorSpace::DeviceGray),
            fill_space: Rc::new(ColorSpace::DeviceGray),
            stroke_color: Color::BLACK,
            fill_color: Color::BLACK,
            fill_pattern: None,
            stroke_pattern: None,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            clip: None,
            soft_mask: None,
            hard_clip: None,
            text: TextState::default(),
        }
    }
}

/// A pending path being built by path-construction operators.
///
/// Stored in *user space* (untransformed): the path is transformed
/// once by `CTM_paint × page→pixmap` when it is consumed (an
/// implementation choice — the spec does not mandate an internal
/// representation, only that the painted result match the CTM in
/// effect at painting time).
///
/// Placement rules (§8.2 Figure 9, §8.5.4): inside a *path object*
/// only path-construction operators and `W`/`W*` are allowed; `W`/`W*`
/// may appear only after the last path-construction operator.
/// `in_progress` tracks whether we're inside a path object so the
/// renderer can reject out-of-place operators with a typed error
/// rather than guessing at unspecified behaviour.
#[derive(Default)]
struct CurrentPath {
    builder: PathBuilder,
    has_open_subpath: bool,
    elements: usize,
    /// `W`/`W*` mark the path as a clip operand; the clip is installed
    /// when the path is consumed per §8.5.4.
    pending_clip: Option<FillRule>,
    /// True while a path object is open (first construction op seen,
    /// not yet painted/`n`-ed).
    in_progress: bool,
}

impl CurrentPath {
    fn bump_elements(&mut self, limits: &Limits) -> Result<(), Error> {
        self.elements += 1;
        if self.elements > limits.max_path_elements {
            return Err(Error::LimitExceeded {
                limit: "max_path_elements",
                max: limits.max_path_elements as u64,
            });
        }
        Ok(())
    }

    /// Whether the path is empty (no elements).
    fn is_empty(&self) -> bool {
        self.elements == 0
    }

    /// Reset after a painting op or `n`. A pending `W`/`W*` clip is
    /// installed by the caller before this runs (§8.5.4), so resetting
    /// here just drops the path elements.
    fn reset(&mut self) {
        self.builder = PathBuilder::new();
        self.has_open_subpath = false;
        self.elements = 0;
        self.in_progress = false;
        self.pending_clip = None;
    }
}

/// Reject an operator that §8.2 Figure 9 does not allow inside a path
/// object. Only path-construction operators, `W`/`W*`, and the
/// painting/`n` ops that end the path are permitted there.
fn placement_err(what: &str) -> Error {
    Error::Structure(format!(
        "{what} is not allowed inside a path object (§8.2 Figure 9)"
    ))
}

/// Options for [`render_page`].
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Output resolution in dots per inch. 72 = 1 px per PDF point.
    /// Must be finite and > 0.
    pub dpi: f64,
    /// Limits for the render.
    pub limits: Limits,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions {
            dpi: 72.0,
            limits: Limits::default(),
        }
    }
}

/// The rendered page plus the geometry that produced it.
#[derive(Debug)]
pub struct RenderedPage {
    /// Premultiplied RGBA8 pixel buffer.
    pub pixmap: Pixmap,
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// DPI used for the conversion.
    pub dpi: f64,
    /// Rendered rectangle size in PDF points (CropBox, Rotate applied).
    pub page_size_pts: (f64, f64),
    /// Non-embedded fonts that were drawn with a bundled substitute
    /// (see [`crate::fontsubst`]). Empty means every glyph on the page
    /// came from an embedded program; non-empty means shapes are
    /// approximations of the named fonts and the image must not be
    /// treated as a faithful render of them.
    pub font_substitutions: Vec<crate::fontsubst::FontSubstitution>,
    /// Colour spaces that were converted through an *approximation*
    /// (an `/ICCBased` profile we cannot evaluate, so its `/Alternate`
    /// was used — §8.6.5.5). Empty means every colour on the page went
    /// through an exact conversion; non-empty means hues may differ
    /// from a CMS-backed viewer and the image must not be treated as
    /// a faithful render of those colours.
    pub color_approximations: Vec<String>,
}

/// Render `page` of `doc` to a pixel buffer. On failure no
/// `RenderedPage` is produced; callers must not write output on `Err`.
pub fn render_page(
    doc: &Document,
    page: &Page,
    opts: &RenderOptions,
) -> Result<RenderedPage, Error> {
    let limits = &opts.limits;
    let _budget = doc.decoded_budget_scope();

    // ---- Reject pages we cannot faithfully draw ---------------------
    // (`/Annots` are drawn after the content — see `draw_annotations`.)
    // §14.11.5 /UserUnit: the size of a default user-space unit in
    // multiples of 1/72 inch (1 by default, 1 ≤ value ≤ 75 000). At a
    // given dpi the page is drawn that many times larger; the pixel
    // limits below bound the result.
    let user_unit = match page_dict_entry(doc, page, b"UserUnit")? {
        None => 1.0,
        Some(uu) => {
            let v = uu
                .as_number()
                .ok_or_else(|| Error::Structure("/UserUnit is not a number".into()))?;
            if !(v.is_finite() && (1.0..=75_000.0).contains(&v)) {
                return Err(Error::Structure(format!(
                    "/UserUnit {v} is outside the 1..=75000 range"
                )));
            }
            v
        }
    };

    // ---- Page geometry ----------------------------------------------
    let crop = page.crop_box();
    if !rect_is_finite(&crop) {
        return Err(Error::Structure("CropBox has non-finite values".into()));
    }
    let rotate = page.rotate().rem_euclid(360);

    let scale = opts.dpi / 72.0 * user_unit;
    if !(scale.is_finite() && scale > 0.0) {
        return Err(Error::Structure(format!(
            "invalid DPI {} (must be positive and finite)",
            opts.dpi
        )));
    }
    let (w_pts, h_pts) = crop_size_after_rotate(&crop, rotate);
    if !(w_pts.is_finite() && h_pts.is_finite() && w_pts >= 0.0 && h_pts >= 0.0) {
        return Err(Error::Structure("CropBox dimensions are invalid".into()));
    }
    let width = checked_px(w_pts, scale, limits)?;
    let height = checked_px(h_pts, scale, limits)?;
    if width as u64 * height as u64 > limits.max_render_total_pixels {
        return Err(Error::LimitExceeded {
            limit: "max_render_total_pixels",
            max: limits.max_render_total_pixels,
        });
    }
    let buf_bytes = (width as u64)
        .checked_mul(height as u64)
        .and_then(|p| p.checked_mul(4))
        .ok_or(Error::LimitExceeded {
            limit: "max_render_buffer_bytes",
            max: limits.max_render_buffer_bytes,
        })?;
    if buf_bytes > limits.max_render_buffer_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_render_buffer_bytes",
            max: limits.max_render_buffer_bytes,
        });
    }

    let user_to_pix = build_page_transform(&crop, rotate, scale)?;

    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| Error::Structure(format!("cannot allocate {width}x{height} pixmap")))?;
    pixmap.fill(ts_color(Color::WHITE));

    let ops = doc.page_operations(page)?;
    let resources = page
        .resources()
        .cloned()
        .unwrap_or(Object::Dict(Default::default()));
    let mut renderer = Renderer {
        limits,
        doc,
        pixmap: &mut pixmap,
        user_to_pix,
        gs: GraphicsState::new(),
        gs_stack: Vec::new(),
        path: CurrentPath::default(),
        in_text: false,
        text_clip: None,
        resources,
        xobject_depth: 0,
        xobject_invocations: 0,
        xobject_stack: Vec::new(),
        text_tm: Matrix::IDENTITY,
        text_tlm: Matrix::IDENTITY,
        text_codes: 0,
        extract: None,
        extract_unicode_bytes: 0,
        extraction_only: false,
        substitutions: Vec::new(),
        color_approximations: Vec::new(),
        default_spaces: None,
        layer_bytes: 0,
        pattern_base: Matrix::IDENTITY,
        pattern_depth: 0,
        pattern_stack: Vec::new(),
        pattern_tiles: 0,
        uncolored_depth: 0,
        oc: crate::optcontent::OcConfig::from_catalog(doc)?,
        oc_include_hidden: false,
        mc_stack: Vec::new(),
        hidden: 0,
    };
    renderer.run(&ops)?;
    // Annotation appearance streams paint *over* the page content, in
    // /Annots order (§12.5.5), each under a fresh graphics state.
    let page_resources = renderer.resources.clone();
    renderer.draw_annotations(page, &page_resources)?;
    let font_substitutions = std::mem::take(&mut renderer.substitutions);
    let color_approximations = std::mem::take(&mut renderer.color_approximations);

    Ok(RenderedPage {
        pixmap,
        width,
        height,
        dpi: opts.dpi,
        page_size_pts: (w_pts, h_pts),
        font_substitutions,
        color_approximations,
    })
}

/// Encode a rendered pixmap as PNG bytes (RGBA8).
pub fn pixmap_to_png(pixmap: &Pixmap) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| Error::Structure(format!("PNG encode header: {e}")))?;
        writer
            .write_image_data(pixmap.data())
            .map_err(|e| Error::Structure(format!("PNG encode data: {e}")))?;
    }
    Ok(out)
}

// --------------------------------------------------------------------
// Page geometry helpers
// --------------------------------------------------------------------

/// Re-read a single key from the (indirect) page object. `/Annots` and
/// `/UserUnit` are not part of the inherited attributes stored on
/// `Page`, so they are fetched lazily here.
fn page_dict_entry(doc: &Document, page: &Page, key: &[u8]) -> Result<Option<Object>, Error> {
    let pref = match page.object_ref() {
        Some(r) => r,
        None => return Ok(None),
    };
    let obj = doc.resolve(pref)?;
    let dict = obj
        .as_dict()
        .ok_or_else(|| Error::Structure("page object is not a dictionary".into()))?;
    match dict.get(key) {
        None => Ok(None),
        Some(v) => Ok(Some(doc.deref(v)?)),
    }
}

fn rect_is_finite(r: &crate::page::Rect) -> bool {
    [r.llx, r.lly, r.urx, r.ury].iter().all(|v| v.is_finite())
}

/// CropBox width/height after `/Rotate` (90/270 swap them).
fn crop_size_after_rotate(crop: &crate::page::Rect, rotate: i64) -> (f64, f64) {
    let w = (crop.urx - crop.llx).abs();
    let h = (crop.ury - crop.lly).abs();
    match rotate {
        90 | 270 => (h, w),
        _ => (w, h),
    }
}

fn checked_px(pts: f64, scale: f64, limits: &Limits) -> Result<u32, Error> {
    let px = pts * scale;
    if !(px.is_finite() && px >= 0.0) {
        return Err(Error::Structure(format!(
            "non-finite or negative pixel dimension ({px})"
        )));
    }
    let rounded = px.round();
    if rounded > limits.max_render_pixels as f64 {
        return Err(Error::LimitExceeded {
            limit: "max_render_pixels",
            max: limits.max_render_pixels as u64,
        });
    }
    Ok(rounded as u32)
}

/// Map PDF user space to pixmap space.
///
/// `/Rotate` rotates the displayed page counterclockwise in user space
/// (§7.7.3.3). The CropBox is moved to the origin, rotated about it,
/// shifted so the rotated bounds start at (0,0), then scaled and
/// y-flipped into pixmap coordinates.
fn build_page_transform(
    crop: &crate::page::Rect,
    rotate: i64,
    scale: f64,
) -> Result<Transform, Error> {
    let (_w_pts, h_pts) = crop_size_after_rotate(crop, rotate);

    // Step 1: CropBox.ll -> origin.
    let t1 = Matrix::translate(-crop.llx, -crop.lly);
    // Step 2: rotate about the origin; the rotated CropBox bounds can
    // extend negative, so shift them back to start at (0,0).
    // /Rotate is *clockwise* when the page is displayed (§7.7.3.3),
    // which is a negative angle in y-up user space.
    let (cw, ch) = (crop.urx - crop.llx, crop.ury - crop.lly);
    let rot = Matrix::rotate(-rotate as f64);
    let mut minx = f64::INFINITY;
    let mut miny = f64::INFINITY;
    for (x, y) in [(0.0, 0.0), (cw, 0.0), (0.0, ch), (cw, ch)] {
        let (rx, ry) = rot.transform(x, y);
        minx = minx.min(rx);
        miny = miny.min(ry);
    }
    let t2 = Matrix::translate(-minx, -miny);
    // Step 3: scale and flip y; after the flip the top of the box is
    // at pixmap height.
    let flip = Matrix::scale(scale, -scale).concat(Matrix::translate(0.0, h_pts * scale));

    let m = t1.concat(rot).concat(t2).concat(flip);
    if !m.is_finite() {
        return Err(Error::Structure("page transform is not finite".into()));
    }
    Ok(Transform::from_row(
        m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32,
    ))
}

// --------------------------------------------------------------------
// The interpreter
// --------------------------------------------------------------------

pub(crate) struct Renderer<'a> {
    limits: &'a Limits,
    doc: &'a Document,
    pixmap: &'a mut Pixmap,
    /// page→pixmap transform (crop offset, rotate, dpi scale, y flip).
    user_to_pix: Transform,
    gs: GraphicsState,
    gs_stack: Vec<GraphicsState>,
    path: CurrentPath,
    in_text: bool,
    /// Glyph outlines (device space) accumulated by the text clipping
    /// modes `Tr 4`–`7` inside the current text object; intersected
    /// with the clip at `ET` (§9.3.6). `None` = no clipping mode used.
    text_clip: Option<PathBuilder>,
    /// Current effective `/Resources` dictionary — the page's at top
    /// level, the form's own while executing a Form XObject.
    /// §7.8.3: a form without `/Resources` inherits the caller's
    /// (obsolete but encountered in ≤1.1 files).
    resources: Object,
    /// Current `Do` nesting depth (forms containing forms).
    xobject_depth: u32,
    /// Cumulative `Do` invocations this page (bounds sibling fan-out).
    xobject_invocations: u64,
    /// References currently being painted — for direct/indirect form
    /// recursion. Same form used at different spots is fine; a form
    /// that (transitively) invokes itself is not.
    xobject_stack: Vec<IndirectRef>,
    /// Text-object matrices (§9.4.1) — *not* part of `q`/`Q`; reset by
    /// `BT` and discarded by `ET`. Kept on the renderer, not in
    /// `GraphicsState`.
    text_tm: Matrix,
    text_tlm: Matrix,
    /// Cumulative count of character codes processed this page —
    /// bounds total glyph work regardless of how strings are split
    /// across `Tj`/`TJ`/forms.
    text_codes: u64,
    /// If `Some`, text-showing ops also record an [`ExtractedChar`]
    /// per character code — used by [`crate::text_extract`].
    extract: Option<Vec<crate::text_extract::CharInfo>>,
    /// Cumulative UTF-8 bytes of resolved Unicode stored in `extract`.
    extract_unicode_bytes: u64,
    /// `true` while serving `extract_text`: painting work (path
    /// rasterisation, clip masks, image decoding) is skipped — the
    /// interpreter still walks every operator so text state, resources
    /// and limits behave exactly as during a render.
    extraction_only: bool,
    /// Non-embedded fonts drawn with a bundled substitute this page
    /// (deduplicated) — surfaced on [`RenderedPage`].
    substitutions: Vec<crate::fontsubst::FontSubstitution>,
    /// Colour-space approximations recorded this page (deduplicated)
    /// — surfaced on [`RenderedPage::color_approximations`].
    color_approximations: Vec<String>,
    /// `/DefaultGray`, `/DefaultRGB`, `/DefaultCMYK` of the current
    /// resources (§8.6.5.6), resolved lazily and dropped whenever
    /// `resources` changes — see [`Self::device_space`].
    default_spaces: Option<[Rc<ColorSpace>; 3]>,
    /// Bytes of offscreen group layers currently alive — bounded by
    /// `max_render_buffer_bytes` together with the page pixmap.
    layer_bytes: u64,
    /// Pattern space → page space for the content stream being run
    /// (§8.7.3.1): identity for the page, the form's CTM (after its
    /// `/Matrix`) inside a Form XObject or Type 3 glyph.
    pattern_base: Matrix,
    /// Nesting depth of tiling-pattern cells being painted.
    pattern_depth: u32,
    /// Tiling patterns (by indirect reference) whose cells are being
    /// painted — a cell that paints with its own pattern is a cycle.
    pattern_stack: Vec<IndirectRef>,
    /// Tiling cells executed individually on this page
    /// (`max_pattern_tiles`).
    pattern_tiles: u64,
    /// Depth of `/PaintType 2` (uncoloured) cells being painted: colour
    /// operators inside are ignored (§8.7.3.3).
    uncolored_depth: u32,
    /// Optional-content configuration of the document (§8.11).
    oc: crate::optcontent::OcConfig,
    /// Extraction only: report text inside hidden optional content too
    /// (`extract_text_all`). Never set while painting.
    oc_include_hidden: bool,
    /// One frame per open `BMC`/`BDC` (§14.6): whether it hides its
    /// content and whether it carries `/ActualText`.
    mc_stack: Vec<McFrame>,
    /// Open frames that hide their content. While non-zero nothing is
    /// painted or extracted; the graphics state keeps evolving.
    hidden: u32,
}

/// A marked-content frame (see `Renderer::mc_stack`).
#[derive(Clone)]
struct McFrame {
    hidden: bool,
    /// `/ActualText` replacement for the span; `emitted` once the
    /// first shown character carried it.
    actual: Option<(String, bool)>,
}

/// A `/PatternType 2` pattern: a shading plus its `/Matrix`.
struct ShadingPattern {
    shading: Rc<Shading>,
    matrix: Matrix,
}

/// A `/PatternType 1` tiling pattern (§8.7.3): the cell content stream
/// and the parameters that place its copies.
struct TilingPattern {
    dict: Dict,
    raw: Vec<u8>,
    matrix: Matrix,
    /// Normalised `/BBox` (cell clip).
    bbox: crate::page::Rect,
    xstep: f64,
    ystep: f64,
    /// `/PaintType 2`: the cell paints in this colour (the `scn`
    /// components through the base space), ignoring its own colour
    /// operators.
    under: Option<Color>,
    resources: Object,
    entry_ref: Option<IndirectRef>,
}

/// What a `/Pattern` colour currently selects.
enum PatternPaint {
    Shading(Rc<ShadingPattern>),
    Tiling(Rc<TilingPattern>),
}

/// Deepest nesting of tiling-pattern cells (a cell filling with a
/// pattern whose cell fills with a pattern …).
const MAX_PATTERN_DEPTH: u32 = 8;
/// Up to this many cells a fill is executed cell by cell (crisp,
/// exact); denser tilings go through the cached cell.
const DIRECT_TILES: u64 = 64;

/// Device bounding box (inclusive pixels) of the non-zero coverage in
/// `mask`, or `None` when it is empty.
fn mask_bounds(mask: &Mask) -> Option<(i32, i32, i32, i32)> {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let data = mask.data();
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut any = false;
    for y in 0..h {
        let row = &data[y * w..(y + 1) * w];
        let Some(first) = row.iter().position(|&v| v != 0) else {
            continue;
        };
        let last = row.iter().rposition(|&v| v != 0).unwrap_or(first);
        any = true;
        x0 = x0.min(first);
        x1 = x1.max(last);
        y0 = y0.min(y);
        y1 = y;
    }
    any.then_some((x0 as i32, y0 as i32, x1 as i32, y1 as i32))
}

/// Interpreter state a pattern cell replaces while it runs.
struct CellSaved {
    gs: GraphicsState,
    gs_stack: Vec<GraphicsState>,
    resources: Object,
    pattern_base: Matrix,
    path: CurrentPath,
    in_text: bool,
    text_clip: Option<PathBuilder>,
    text_tm: Matrix,
    text_tlm: Matrix,
    pushed_ref: bool,
    uncolored: bool,
}

impl<'a> Renderer<'a> {
    /// Build a renderer that records extracted characters instead of
    /// painting them. The pixmap is still allocated (to satisfy the
    /// struct's invariants) but `extract` captures the text stream.
    pub(crate) fn for_extraction(
        doc: &'a Document,
        page: &Page,
        opts: &'a RenderOptions,
    ) -> Result<Renderer<'a>, Error> {
        let limits = &opts.limits;
        // `/Annots` is deliberately *not* a blocker here. Extraction
        // reports the page's content-stream text (plus Form XObjects);
        // annotation appearance streams are a separate layer that is
        // never part of that text — `pdftotext` makes the same cut.
        // Rendering still refuses annotated pages (see `render_page`)
        // because there the appearance streams *would* change the
        // picture.
        // Extraction reports PDF user-space (CropBox) coordinates, so
        // no page→pixmap transform and no page-sized pixel buffer are
        // needed. The struct still requires a live pixmap; 1×1 is a
        // placeholder only — nothing is painted in extraction mode.
        let pixmap = Pixmap::new(1, 1)
            .ok_or_else(|| Error::Structure("cannot allocate extraction pixmap".into()))?;
        let resources = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Default::default()));
        Ok(Renderer {
            limits,
            doc,
            pixmap: Box::leak(Box::new(pixmap)),
            user_to_pix: Transform::default(),
            gs: GraphicsState::new(),
            gs_stack: Vec::new(),
            path: CurrentPath::default(),
            in_text: false,
            text_clip: None,
            resources,
            xobject_depth: 0,
            xobject_invocations: 0,
            xobject_stack: Vec::new(),
            text_tm: Matrix::IDENTITY,
            text_tlm: Matrix::IDENTITY,
            text_codes: 0,
            extract: Some(Vec::new()),
            extract_unicode_bytes: 0,
            extraction_only: true,
            substitutions: Vec::new(),
            color_approximations: Vec::new(),
            default_spaces: None,
            layer_bytes: 0,
            pattern_base: Matrix::IDENTITY,
            pattern_depth: 0,
            pattern_stack: Vec::new(),
            pattern_tiles: 0,
            uncolored_depth: 0,
            oc: crate::optcontent::OcConfig::from_catalog(doc)?,
            oc_include_hidden: false,
            mc_stack: Vec::new(),
            hidden: 0,
        })
    }

    /// Replace the effective `/Resources`, returning the previous one.
    /// Every switch goes through here so the `/Default*` colour-space
    /// cache never outlives the dictionary it was read from.
    fn swap_resources(&mut self, resources: Object) -> Object {
        self.default_spaces = None;
        std::mem::replace(&mut self.resources, resources)
    }

    /// Take the extracted character buffer (extraction mode only).
    pub(crate) fn take_extracted(&mut self) -> Option<Vec<crate::text_extract::CharInfo>> {
        self.extract.take()
    }

    /// Extraction mode: report text in hidden optional content too.
    pub(crate) fn include_hidden_content(&mut self) {
        debug_assert!(self.extraction_only);
        self.oc_include_hidden = true;
    }

    pub(crate) fn run(&mut self, ops: &[Operation]) -> Result<(), Error> {
        for op in ops {
            self.execute(op)?;
        }
        Ok(())
    }

    fn execute(&mut self, op: &Operation) -> Result<(), Error> {
        // §8.2 Figure 9 placement: inside a path object (after a path
        // construction op, before the painting/`n` that ends it) only
        // path-construction ops, W/W*, and the terminating painting
        // op are allowed. Everything else — graphics-state ops,
        // colour ops, marked content, text, XObjects — is out of
        // order and rejected rather than interpreted under undefined
        // semantics.
        if self.path.in_progress && !op.is_path_level() {
            return Err(placement_err(op.name()));
        }
        // §8.7.3.3: an uncoloured pattern cell "shall not" set colour;
        // the colour comes from the `scn` that selected the pattern,
        // so colour operators inside are ignored, as every viewer does.
        if self.uncolored_depth > 0
            && matches!(
                op,
                Operation::SetGrayStroke(_)
                    | Operation::SetGrayFill(_)
                    | Operation::SetRgbStroke(..)
                    | Operation::SetRgbFill(..)
                    | Operation::SetCmykStroke(..)
                    | Operation::SetCmykFill(..)
                    | Operation::SetColorSpaceStroke(_)
                    | Operation::SetColorSpaceFill(_)
                    | Operation::SetColorStroke(_)
                    | Operation::SetColorFill(_)
            )
        {
            return Ok(());
        }
        match op {
            // ---- graphics state stack ---------------------------
            Operation::SaveState => {
                if self.gs_stack.len() >= self.limits.max_gsave_depth {
                    return Err(Error::LimitExceeded {
                        limit: "max_gsave_depth",
                        max: self.limits.max_gsave_depth as u64,
                    });
                }
                self.gs_stack.push(self.gs.clone());
            }
            Operation::RestoreState => {
                self.gs = self
                    .gs_stack
                    .pop()
                    .ok_or_else(|| Error::Structure("Q without matching q".into()))?;
                // NB: the current path is NOT part of graphics state.
            }
            Operation::ConcatMatrix(m) => {
                if !m.is_finite() {
                    return Err(Error::Structure(
                        "cm operands must be finite numbers".into(),
                    ));
                }
                // PDF: CTM' = cm × CTM — the operand maps the *new*
                // user space into the old one, so it applies first.
                // `concat` runs `self` before its argument ⇒ `cm.concat(ctm)`.
                self.gs.ctm = m.concat(self.gs.ctm);
            }
            Operation::SetLineWidth(w) => self.gs.line_width = *w,
            Operation::SetLineCap(c) => self.gs.line_cap = (*c).clamp(0, 2) as u8,
            Operation::SetLineJoin(j) => self.gs.line_join = (*j).clamp(0, 2) as u8,
            Operation::SetMiterLimit(m) => self.gs.miter_limit = *m,
            Operation::SetDash { dashes, phase } => self.gs.dash = Some((dashes.clone(), *phase)),
            Operation::SetFlatness(f) => self.gs.flatness = *f,
            Operation::SetRenderingIntent(_) => {
                // Stored; no effect on device-space output.
            }

            // ---- path construction ------------------------------
            // Path elements are stored in user space and transformed by
            // `CTM_paint × page→pixmap` when consumed (§8.4.3.1).
            Operation::MoveTo(x, y) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("m after W/W*"));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.move_to(*x as f32, *y as f32);
                self.path.has_open_subpath = true;
                self.path.in_progress = true;
            }
            Operation::LineTo(x, y) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("l after W/W*"));
                }
                if !self.path.has_open_subpath {
                    return Err(Error::Structure(
                        "l without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.line_to(*x as f32, *y as f32);
            }
            Operation::CurveTo(x1, y1, x2, y2, x3, y3) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("c after W/W*"));
                }
                if !self.path.has_open_subpath {
                    return Err(Error::Structure(
                        "c without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x1 as f32, *y1 as f32, *x2 as f32, *y2 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::CurveToV(x2, y2, x3, y3) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("v after W/W*"));
                }
                if !self.path.has_open_subpath {
                    return Err(Error::Structure(
                        "v without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x2 as f32, *y2 as f32, *x2 as f32, *y2 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::CurveToY(x1, y1, x3, y3) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("y after W/W*"));
                }
                if !self.path.has_open_subpath {
                    return Err(Error::Structure(
                        "y without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x1 as f32, *y1 as f32, *x3 as f32, *y3 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::ClosePath => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("h after W/W*"));
                }
                // `h` with no open subpath is a no-op (nothing to
                // close); most readers accept it.
                if !self.path.has_open_subpath {
                    return Ok(());
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.close();
                self.path.has_open_subpath = false;
            }
            Operation::Rectangle(x, y, w, h) => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("re after W/W*"));
                }
                self.path.bump_elements(self.limits)?;
                self.path.builder.move_to(*x as f32, *y as f32);
                self.path.builder.line_to((*x + *w) as f32, *y as f32);
                self.path
                    .builder
                    .line_to((*x + *w) as f32, (*y + *h) as f32);
                self.path.builder.line_to(*x as f32, (*y + *h) as f32);
                self.path.builder.close();
                self.path.has_open_subpath = false;
                self.path.in_progress = true;
            }

            // ---- painting ---------------------------------------
            Operation::Stroke => self.paint_path(PaintKind::Stroke)?,
            Operation::CloseAndStroke => {
                self.close_open_subpath();
                self.paint_path(PaintKind::Stroke)?
            }
            Operation::Fill => self.paint_path(PaintKind::Fill(FillRule::Winding))?,
            Operation::FillEvenOdd => self.paint_path(PaintKind::Fill(FillRule::EvenOdd))?,
            Operation::FillAndStroke => {
                self.paint_path(PaintKind::FillStroke(FillRule::Winding))?
            }
            Operation::FillAndStrokeEvenOdd => {
                self.paint_path(PaintKind::FillStroke(FillRule::EvenOdd))?
            }
            Operation::CloseFillAndStroke => {
                self.close_open_subpath();
                self.paint_path(PaintKind::FillStroke(FillRule::Winding))?
            }
            Operation::CloseFillAndStrokeEvenOdd => {
                self.close_open_subpath();
                self.paint_path(PaintKind::FillStroke(FillRule::EvenOdd))?
            }
            Operation::EndPath => {
                // `n` ends the path without painting; a pending W/W*
                // clip is still installed (the path was the clip
                // operand).
                if self.path.pending_clip.is_some() {
                    self.install_clip()?;
                }
                self.path.reset();
            }

            // ---- clipping -----------------------------------------
            // §8.5.4: W/W* mark the current path as a clip operand;
            // the clip is *installed* when the path is consumed —
            // at a painting op or at `n`. The path itself is not
            // consumed by W, so a following paint op paints it under
            // the *pre-W* clip and then the new clip takes effect for
            // subsequent painting. A `W`+`n` sequence installs the clip
            // without painting.
            Operation::Clip => {
                if self.path.pending_clip.is_some() {
                    // §8.5.4: one clipping op per path object — a
                    // second W in the same path is out of order.
                    return Err(placement_err("second W/W* in the same path object"));
                }
                self.path.pending_clip = Some(FillRule::Winding);
            }
            Operation::ClipEvenOdd => {
                if self.path.pending_clip.is_some() {
                    return Err(placement_err("second W/W* in the same path object"));
                }
                self.path.pending_clip = Some(FillRule::EvenOdd);
            }

            // ---- colour --------------------------------------------
            // §8.6.8: G/RG/K select the device space (or its /Default*
            // replacement) *and* set the colour, like `cs` + `sc`.
            Operation::SetGrayStroke(g) => self.select_device_color(0, &[*g], true)?,
            Operation::SetGrayFill(g) => self.select_device_color(0, &[*g], false)?,
            Operation::SetRgbStroke(r, g, b) => self.select_device_color(1, &[*r, *g, *b], true)?,
            Operation::SetRgbFill(r, g, b) => self.select_device_color(1, &[*r, *g, *b], false)?,
            Operation::SetCmykStroke(c, m, y, k) => {
                self.select_device_color(2, &[*c, *m, *y, *k], true)?
            }
            Operation::SetCmykFill(c, m, y, k) => {
                self.select_device_color(2, &[*c, *m, *y, *k], false)?
            }
            Operation::SetColorSpaceStroke(n) => self.select_colorspace(n, true)?,
            Operation::SetColorSpaceFill(n) => self.select_colorspace(n, false)?,
            Operation::SetColorStroke(ops) => self.set_color(ops, true)?,
            Operation::SetColorFill(ops) => self.set_color(ops, false)?,

            // ---- text ---------------------------------------------
            Operation::BeginText => {
                if self.in_text {
                    return Err(Error::Structure("BT inside a text object".into()));
                }
                self.in_text = true;
                // §9.4.1: BT resets Tm and Tlm to identity.
                self.text_tm = Matrix::IDENTITY;
                self.text_tlm = Matrix::IDENTITY;
            }
            Operation::EndText => {
                if !self.in_text {
                    return Err(Error::Structure("ET without a matching BT".into()));
                }
                self.in_text = false;
                // §9.3.6: the glyphs shown in a clipping mode become
                // the clip once the text object ends — an empty set
                // (a clipping mode with no glyph outline) clips
                // everything away.
                if let Some(b) = self.text_clip.take() {
                    if !self.extraction_only {
                        match b.finish() {
                            Some(p) => {
                                self.clip_intersect(&p, FillRule::Winding, Transform::identity())?
                            }
                            None => self.clip_empty()?,
                        }
                    }
                }
            }
            // Text-state operators — usable both inside and outside
            // BT/ET (their values persist across text objects).
            Operation::SetCharSpacing(v) => self.gs.text.char_spacing = *v,
            Operation::SetWordSpacing(v) => self.gs.text.word_spacing = *v,
            Operation::SetHorizontalScaling(v) => self.gs.text.h_scale = *v / 100.0,
            Operation::SetTextLeading(v) => self.gs.text.leading = *v,
            Operation::SetTextRenderingMode(m) => {
                if *m < 0 || *m > 7 {
                    return Err(Error::Structure(format!("Tr {m} out of range 0..=7")));
                }
                self.gs.text.render_mode = *m;
            }
            Operation::SetTextRise(v) => self.gs.text.rise = *v,
            Operation::SetTextFont { font, size } => {
                if !(size.is_finite() && *size > 0.0) {
                    return Err(Error::Structure(format!(
                        "Tf size {size} is not a positive finite number"
                    )));
                }
                let f = crate::font::load_font(self.doc, &self.resources, font, self.limits)?;
                // Record a substituted (non-embedded) font once per page
                // so the caller can tell this render from a faithful one.
                if let Some(s) = f.substitution() {
                    if !self.substitutions.contains(&s) {
                        self.substitutions.push(s);
                    }
                }
                self.gs.text.font = Some(f);
                self.gs.text.font_name = font.clone();
                self.gs.text.font_size = *size;
            }
            // Positioning — Tm/Tlm updates per §9.4.2.
            Operation::MoveTextPosition(tx, ty) => {
                // Tm = Tlm = [1 0 0 1 tx ty] × Tlm
                let t = Matrix::new(1.0, 0.0, 0.0, 1.0, *tx, *ty);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            Operation::MoveTextPositionAndSetLeading(tx, ty) => {
                // −ty TL ; tx ty Td
                self.gs.text.leading = -*ty;
                let t = Matrix::new(1.0, 0.0, 0.0, 1.0, *tx, *ty);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            Operation::SetTextMatrix(m) => {
                self.text_tm = *m;
                self.text_tlm = *m;
            }
            Operation::NextLine => {
                // T* ≡ 0 -Tl Td
                let t = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.gs.text.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            // Showing.
            Operation::ShowText(s) => self.show_text(s)?,
            Operation::ShowTextAdjusted(items) => self.show_text_adjusted(items)?,
            Operation::NextLineShowText(s) => {
                // ' ≡ T* string Tj
                let t = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.gs.text.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
                self.show_text(s)?;
            }
            Operation::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => {
                // " ≡ aw Tw ac Tc string '
                self.gs.text.word_spacing = *word_spacing;
                self.gs.text.char_spacing = *char_spacing;
                let t = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.gs.text.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
                self.show_text(text)?;
            }

            // ---- XObject / shading / ExtGState --------------------
            Operation::Do(n) => self.do_xobject(n)?,
            Operation::Shading(n) => self.paint_shading_op(n)?,
            Operation::InlineImage { dict, data } => {
                if !self.extraction_only && self.hidden == 0 {
                    self.paint_inline_image(dict, data)?;
                }
            }
            Operation::SetExtGState(n) => self.apply_extgstate(n)?,

            // ---- marked content / compatibility -----------------
            Operation::MarkedContentPoint(_) => {
                // §10.5.2: MP takes a tag only; it is metadata with no
                // rendering effect.
            }
            Operation::DesignatedMarkedContentPoint { .. } => {
                // §10.5.2: DP is a point — properties (even /OC) have
                // no span to hide.
            }
            Operation::BeginMarkedContentWithProps { tag, properties } => {
                self.begin_marked(tag, properties)?;
            }
            Operation::BeginMarkedContent(_) => self.push_marked(McFrame {
                hidden: false,
                actual: None,
            })?,
            Operation::EndMarkedContent => self.end_marked()?,
            Operation::BeginCompatibility | Operation::EndCompatibility => {}
            Operation::Type3SetWidth { .. } | Operation::Type3SetWidthAndBox { .. } => {
                // d0/d1 only have meaning inside a Type3 /CharProcs
                // stream (handled by the glyph executor). At page level
                // they are a malformed-file quirk — ignore.
            }
            Operation::Unknown { .. } => {
                // §7.8.2: inside BX/EX unknown ops may be ignored.
            }
        }
        Ok(())
    }

    /// `BDC tag props`: open a marked-content frame. Two property
    /// kinds matter (§8.11.3.2, §14.9.4):
    ///
    /// * `/OC` — the properties name an OCG or OCMD; the frame hides
    ///   its content when the document's default configuration says
    ///   so (`OcConfig::visible`). Painting, image/form invocation and
    ///   text extraction stop until the matching `EMC`; the graphics
    ///   state (including clipping) keeps evolving, exactly as for
    ///   visible content.
    /// * `/ActualText` on any tag — the replacement text for the
    ///   span; extraction reports it instead of the glyphs' Unicode.
    ///
    /// The `properties` operand may be a name (resolved against the
    /// current `/Properties` resource subdictionary — kept as the raw
    /// reference so an OCG can be identified) or a direct dictionary.
    /// An unresolvable name is a structure error, never ignored.
    fn begin_marked(&mut self, tag: &[u8], properties: &Object) -> Result<(), Error> {
        let raw = match properties {
            Object::Name(n) => {
                match self
                    .doc
                    .resolve_resource_raw(&self.resources, "Properties", n)?
                {
                    Some(o) => o,
                    None => {
                        return Err(Error::Structure(format!(
                            "marked-content /Properties entry /{} is undefined",
                            show_name(n)
                        )))
                    }
                }
            }
            obj @ Object::Dict(_) => obj.clone(),
            _ => {
                return self.push_marked(McFrame {
                    hidden: false,
                    actual: None,
                })
            }
        };
        let dict_obj = self.doc.deref(&raw)?;
        let dict = dict_obj.as_dict().ok_or_else(|| {
            Error::Structure("marked-content properties is not a dictionary".into())
        })?;
        let mut frame = McFrame {
            hidden: false,
            actual: None,
        };
        if tag == b"OC" {
            let visible = self.oc.visible(self.doc, &raw)?;
            frame.hidden = !visible && !self.oc_include_hidden;
        }
        if let Some(at) = dict.get(b"ActualText".as_slice()) {
            let at = self.doc.deref(at)?;
            let text = match at {
                Object::String(bytes) => pdf_text_string(&bytes),
                _ => return Err(Error::Structure("/ActualText is not a string".into())),
            };
            // An enclosing span is superseded for this range: it must
            // not emit its own text again when it closes.
            for f in self.mc_stack.iter_mut().rev() {
                if let Some((_, emitted)) = &mut f.actual {
                    *emitted = true;
                    break;
                }
            }
            frame.actual = Some((text, false));
        }
        self.push_marked(frame)
    }

    fn push_marked(&mut self, frame: McFrame) -> Result<(), Error> {
        if self.mc_stack.len() >= self.limits.max_nesting_depth as usize {
            return Err(Error::LimitExceeded {
                limit: "max_nesting_depth (marked content)",
                max: self.limits.max_nesting_depth as u64,
            });
        }
        if frame.hidden {
            self.hidden += 1;
        }
        self.mc_stack.push(frame);
        Ok(())
    }

    /// `EMC`: close the innermost frame. A span whose `/ActualText`
    /// never reached a shown character still contributes its text to
    /// extraction as one synthetic, advance-less entry at the current
    /// text position.
    fn end_marked(&mut self) -> Result<(), Error> {
        // The content parser guarantees balance; a stray EMC here
        // would be its bug, so tolerate rather than fail.
        let Some(frame) = self.mc_stack.pop() else {
            return Ok(());
        };
        if frame.hidden {
            self.hidden -= 1;
        }
        if let Some((text, false)) = frame.actual {
            if self.hidden == 0 && self.extract.is_some() && !text.is_empty() {
                let t = &self.gs.text;
                let origin = if self.in_text {
                    Matrix::new(t.font_size * t.h_scale, 0.0, 0.0, t.font_size, 0.0, t.rise)
                        .concat(self.text_tm)
                        .concat(self.gs.ctm)
                        .transform(0.0, 0.0)
                } else {
                    (self.gs.ctm.e, self.gs.ctm.f)
                };
                self.extract_unicode_bytes =
                    self.extract_unicode_bytes.saturating_add(text.len() as u64);
                if self.extract_unicode_bytes > self.limits.max_extracted_unicode_bytes {
                    return Err(Error::LimitExceeded {
                        limit: "max_extracted_unicode_bytes",
                        max: self.limits.max_extracted_unicode_bytes,
                    });
                }
                let (font_size, rise, render_mode) = (t.font_size, t.rise, t.render_mode as u8);
                let in_text = self.in_text;
                if let Some(buf) = self.extract.as_mut() {
                    buf.push(crate::text_extract::CharInfo {
                        code: 0,
                        unicode: Some(text),
                        resolved_via: crate::text_extract::ResolveKind::ActualText,
                        origin,
                        advance: (0.0, 0.0),
                        bounds: None,
                        render_mode,
                        font_size,
                        rise,
                        tj_adjust: 0.0,
                        in_text,
                    });
                }
            }
        }
        Ok(())
    }

    /// The `/ActualText` that replaces the character about to be
    /// shown: the whole string for the span's first character, `""`
    /// for the rest, `None` outside any such span.
    fn take_actual_text(&mut self) -> Option<String> {
        for f in self.mc_stack.iter_mut().rev() {
            if let Some((text, emitted)) = &mut f.actual {
                if *emitted {
                    return Some(String::new());
                }
                *emitted = true;
                return Some(text.clone());
            }
        }
        None
    }

    /// The combined user→pixmap transform used at paint time:
    /// `CTM × page→pixmap`. Path elements were built in user space and
    /// are transformed once by this when the path is consumed
    /// (§8.4.3.1).
    ///
    /// A CTM that is finite but *singular* (determinant 0 — e.g.
    /// `0 0 0 0 e f cm`) collapses every point onto a line or a point;
    /// ISO 32000-1 does not define the behaviour of a non-invertible
    /// CTM and tiny-skia would silently rasterize a degenerate path.
    /// Rather than emit nothing for a page that asked to be drawn, we
    /// refuse it: painting under a singular CTM is a transform we
    /// cannot honour.
    fn paint_ts(&self) -> Result<Transform, Error> {
        let ctm = &self.gs.ctm;
        if !ctm.is_finite() {
            return Err(Error::Structure("non-finite CTM".into()));
        }
        // det = a·d − b·c. Exact zero means non-invertible; we do not
        // epsilon-reject near-singular matrices (anisotropic and
        // tiny-scale transforms are still renderable).
        let det = ctm.a * ctm.d - ctm.b * ctm.c;
        if det == 0.0 {
            return Err(Error::Unsupported {
                feature: "singular CTM (determinant 0)",
                detail: format!(
                    "CTM {ctm} is not invertible — painting under a degenerate \
                     transform is not supported"
                ),
            });
        }
        let ctm_ts = Transform::from_row(
            ctm.a as f32,
            ctm.b as f32,
            ctm.c as f32,
            ctm.d as f32,
            ctm.e as f32,
            ctm.f as f32,
        );
        Ok(ctm_ts.post_concat(self.user_to_pix))
    }

    fn close_open_subpath(&mut self) {
        if self.path.has_open_subpath {
            self.path.builder.close();
            self.path.has_open_subpath = false;
        }
    }

    /// The device space the `G`/`RG`/`K` family selects (`idx` 0 =
    /// Gray, 1 = RGB, 2 = CMYK), honouring a `/DefaultGray`-style
    /// override in the current resources (§8.6.5.6). Resolved once per
    /// resources dictionary and cached in `default_spaces`.
    fn device_space(&mut self, idx: usize) -> Result<Rc<ColorSpace>, Error> {
        if self.default_spaces.is_none() {
            let mut parsed: Vec<Rc<ColorSpace>> = Vec::with_capacity(3);
            for name in [&b"DeviceGray"[..], b"DeviceRGB", b"DeviceCMYK"] {
                let cs = ColorSpace::parse(
                    self.doc,
                    &Object::Name(name.to_vec()),
                    &self.resources,
                    self.limits,
                )?;
                parsed.push(Rc::new(cs));
            }
            let cmyk = parsed.pop().expect("three parsed");
            let rgb = parsed.pop().expect("three parsed");
            let gray = parsed.pop().expect("three parsed");
            self.default_spaces = Some([gray, rgb, cmyk]);
        }
        let cs = self.default_spaces.as_ref().expect("filled above")[idx].clone();
        self.note_approximation(&cs);
        Ok(cs)
    }

    /// `G`/`RG`/`K` and their lowercase forms: select the device
    /// space and set the colour in one step.
    fn select_device_color(
        &mut self,
        idx: usize,
        comps: &[f64],
        stroke: bool,
    ) -> Result<(), Error> {
        let cs = self.device_space(idx)?;
        let color = color_in(&cs, comps)?;
        if stroke {
            self.gs.stroke_space = cs;
            self.gs.stroke_color = color;
        } else {
            self.gs.fill_space = cs;
            self.gs.fill_color = color;
        }
        Ok(())
    }

    /// `CS`/`cs`: select a colour space by name and reset the colour
    /// to the space's initial value (§8.6.8).
    fn select_colorspace(&mut self, name: &Name, stroke: bool) -> Result<(), Error> {
        let cs = ColorSpace::parse(
            self.doc,
            &Object::Name(name.clone()),
            &self.resources,
            self.limits,
        )?;
        self.note_approximation(&cs);
        let cs = Rc::new(cs);
        let color = color_in(&cs, &cs.initial_components())?;
        if stroke {
            self.gs.stroke_space = cs;
            self.gs.stroke_color = color;
            self.gs.stroke_pattern = None;
        } else {
            self.gs.fill_space = cs;
            self.gs.fill_color = color;
            self.gs.fill_pattern = None;
        }
        Ok(())
    }

    /// Record that colours through `cs` are approximated (ICC profile
    /// not evaluated), once per distinct reason.
    fn note_approximation(&mut self, cs: &ColorSpace) {
        if let Some(a) = cs.approximation() {
            if !self.color_approximations.contains(&a) {
                self.color_approximations.push(a);
            }
        }
    }

    /// `SC`/`SCN`/`sc`/`scn` operands → colour in the active colour
    /// space. A trailing pattern name (the `N` forms) needs pattern
    /// support → `Error::Unsupported`.
    fn set_color(&mut self, ops: &[Object], stroke: bool) -> Result<(), Error> {
        let space = if stroke {
            self.gs.stroke_space.clone()
        } else {
            self.gs.fill_space.clone()
        };
        let (comps, name) = match ops.split_last() {
            Some((last @ Object::Name(_), rest)) => (rest, Some(last)),
            _ => (ops, None),
        };
        if let Some(Object::Name(n)) = name {
            if !space.is_pattern() {
                return Err(Error::Parse(format!(
                    "/{} — a pattern name operand needs the /Pattern colour space",
                    show_name(n)
                )));
            }
            // Components before the name colour an uncoloured tiling
            // pattern (through the `[/Pattern base]` space).
            let nums: Vec<f64> = comps
                .iter()
                .map(|o| o.as_number())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    Error::Parse("scn operands before the pattern name must be numbers".into())
                })?;
            let pat = self.load_pattern(n, &space, &nums)?;
            if stroke {
                self.gs.stroke_pattern = Some(pat);
            } else {
                self.gs.fill_pattern = Some(pat);
            }
            return Ok(());
        }
        if space.is_pattern() {
            return Err(Error::Parse(
                "scn in the /Pattern colour space needs a pattern name".into(),
            ));
        }
        let nums: Vec<f64> = comps
            .iter()
            .map(|o| o.as_number())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::Parse("SC/sc operands must be numbers".into()))?;
        if nums.len() != space.ncomps() {
            return Err(Error::Parse(format!(
                "colour space expects {} component(s), got {}",
                space.ncomps(),
                nums.len()
            )));
        }
        let color = color_in(&space, &nums)?;
        if stroke {
            self.gs.stroke_color = color;
        } else {
            self.gs.fill_color = color;
        }
        Ok(())
    }

    /// Apply an `/ExtGState` resource to the current graphics state
    /// (`gs` operator, §8.4.5 / Table 58).
    ///
    /// Every key in the dictionary is dispatched: parameters we model
    /// are written into [`GraphicsState`]; parameters that have no
    /// effect on our raster output (overprint flags, stroke-adjust
    /// hint, `/None` soft masks, `/Default` transfer functions, text
    /// knockout) are accepted and dropped; parameters that *would*
    /// visibly change output but are out of scope return
    /// `Error::Unsupported` rather than being silently mis-rendered.
    ///
    /// The `/Type` entry (`/ExtGState`) is metadata, not a parameter.
    fn apply_extgstate(&mut self, name: &Name) -> Result<(), Error> {
        let obj = self
            .doc
            .resolve_resource(&self.resources, "ExtGState", name)?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/ExtGState /{} not in the resource dictionary",
                    show_name(name)
                ))
            })?;
        let dict = obj.as_dict().ok_or_else(|| {
            Error::Structure(format!(
                "/ExtGState /{} is not a dictionary",
                show_name(name)
            ))
        })?;

        for (k, v) in dict {
            let key: &[u8] = k;
            match key {
                b"Type" => {
                    // `/Type /ExtGState` — declarative, no effect.
                }

                // ---- line parameters (already-modelled gs) ---------
                b"LW" => {
                    self.gs.line_width = num_param(v, "LW")?;
                }
                b"LC" => {
                    let c = int_param(v, "LC")?;
                    if !(0..=2).contains(&c) {
                        return Err(Error::Structure(format!("LC {c} out of range 0..=2")));
                    }
                    self.gs.line_cap = c as u8;
                }
                b"LJ" => {
                    let j = int_param(v, "LJ")?;
                    if !(0..=2).contains(&j) {
                        return Err(Error::Structure(format!("LJ {j} out of range 0..=2")));
                    }
                    self.gs.line_join = j as u8;
                }
                b"ML" => {
                    let m = num_param(v, "ML")?;
                    if m < 1.0 {
                        return Err(Error::Structure(format!("ML {m} must be >= 1")));
                    }
                    self.gs.miter_limit = m;
                }
                b"D" => {
                    // `/D [dash array] phase` — same shape as the `d`
                    // operator's operands.
                    let arr = v
                        .as_array()
                        .ok_or_else(|| Error::Structure("/D must be an array".into()))?;
                    let dash = arr
                        .first()
                        .and_then(|o| o.as_array())
                        .ok_or_else(|| Error::Structure("/D[0] must be the dash array".into()))?;
                    let phase = arr
                        .get(1)
                        .and_then(|o| o.as_number())
                        .ok_or_else(|| Error::Structure("/D[1] must be the phase".into()))?;
                    let dashes: Vec<f64> = dash
                        .iter()
                        .map(|x| x.as_number())
                        .collect::<Option<_>>()
                        .ok_or_else(|| Error::Structure("/D dash array must be numeric".into()))?;
                    if dashes.iter().all(|d| *d == 0.0) {
                        // §8.4.3.6: an all-zero dash array is a solid line.
                        self.gs.dash = None;
                    } else {
                        self.gs.dash = Some((dashes, phase));
                    }
                }
                b"RI" => {
                    // Rendering intent selects the ICC conversion
                    // intent; with no ICC support it has no visible
                    // effect on device-space output — accepted.
                    match v.as_name() {
                        Some(
                            b"AbsoluteColorimetric"
                            | b"RelativeColorimetric"
                            | b"Saturation"
                            | b"Perceptual",
                        ) => {}
                        _ => {
                            return Err(Error::Structure(
                                "/RI must be a rendering-intent name".into(),
                            ))
                        }
                    }
                }

                // ---- transparency (implemented) --------------------
                b"ca" => {
                    self.gs.fill_alpha = alpha_param(v, "ca")?;
                }
                b"CA" => {
                    self.gs.stroke_alpha = alpha_param(v, "CA")?;
                }
                b"BM" => {
                    self.gs.blend_mode = parse_blend_mode(v)?;
                }

                // ---- accepted no-ops (spec-tolerable) ---------------
                // Overprint (OP/op/OPM) only affects CMYK-separation
                // output; in device-RGB compositing it has no effect.
                // All corpus values are OP/op=false, OPM=1.
                b"OP" | b"op" | b"OPM" => {}
                // SA = stroke adjustment hint; a quality-of-rendering
                // hint, not a correctness parameter. Ignoring it is a
                // spec-permitted approximation (§11.7).
                b"SA" => {}
                // AIS = alpha-is-shape flag; only meaningful inside a
                // transparency group / soft mask, which we refuse when
                // actually required. `false` is the default.
                b"AIS" => {}
                // TK = text knockout; only relevant with multiple
                // glyphs in one showing op under advanced blending.
                // `true` is the default and the common corpus value.
                b"TK" => {}
                // SMask `/None` is an explicit "no soft mask" — a no-op.
                // A real mask (dict / `/Luminosity`/`/Alpha` name) is a
                // genuine soft mask → Unsupported.
                b"SMask" => {
                    let v = self.doc.deref(v)?;
                    match &v {
                        Object::Name(n) if n == b"None" => self.clear_soft_mask(),
                        Object::Null => self.clear_soft_mask(),
                        Object::Dict(sm) => {
                            let mask = self.render_soft_mask(sm)?;
                            if !self.extraction_only {
                                self.set_soft_mask(mask);
                            }
                        }
                        _ => {
                            return Err(Error::Structure(
                                "/SMask must be a dictionary or /None".into(),
                            ))
                        }
                    }
                }
                // Transfer functions TR/TR2 — accepted only as the
                // name `/Default` (no adjustment). A real function
                // (dict / array of functions) changes tone → refuse.
                b"TR" | b"TR2" | b"BG" | b"BG2" | b"UCR" | b"UCR2" => {
                    if !matches!(v.as_name(), Some(b"Default")) {
                        return Err(Error::Unsupported {
                            feature: "transfer/black-generation function",
                            detail: format!(
                                "/{} requires a tint transform not implemented",
                                show_name(key)
                            ),
                        });
                    }
                }
                // FL = flatness tolerance — the same curve-flattening
                // knob the `i` operator sets (§8.4.3.8); apply it like
                // the op does rather than dropping it.
                b"FL" => {
                    self.gs.flatness = num_param(v, "FL")?;
                }
                // HT halftone & SM smoothness are device-rendering
                // knobs with no PDF-semantics change on our rasteriser
                // → accepted.
                b"HT" | b"HTP" | b"SM" => {}
                // `/Font [ref size]` (§8.4.5, Table 58) sets the text
                // font and size like `Tf`, except that the font is an
                // object reference, not a resource name.
                b"Font" => {
                    let arr = self.doc.deref(v)?;
                    let arr = arr.as_array().ok_or_else(|| {
                        Error::Structure("ExtGState /Font must be [font size]".into())
                    })?;
                    if arr.len() != 2 {
                        return Err(Error::Structure(
                            "ExtGState /Font must be [font size]".into(),
                        ));
                    }
                    let size = self.doc.deref(&arr[1])?.as_number().ok_or_else(|| {
                        Error::Structure("ExtGState /Font size must be a number".into())
                    })?;
                    if !(size.is_finite() && size > 0.0) {
                        return Err(Error::Structure(format!(
                            "ExtGState /Font size {size} is not a positive finite number"
                        )));
                    }
                    let fobj = self.doc.deref(&arr[0])?;
                    let fdict = fobj.as_dict().ok_or_else(|| {
                        Error::Structure("ExtGState /Font entry is not a font dictionary".into())
                    })?;
                    let label = format!("gs:{}", show_name(name)).into_bytes();
                    let f = crate::font::load_font_dict(self.doc, fdict, &label, self.limits)?;
                    if let Some(s) = f.substitution() {
                        if !self.substitutions.contains(&s) {
                            self.substitutions.push(s);
                        }
                    }
                    self.gs.text.font = Some(f);
                    self.gs.text.font_name = label;
                    self.gs.text.font_size = size;
                }

                // Apple Quartz private keys (`/AAPL:AA` anti-aliasing,
                // `/AAPL:ST` …): second-class names of a registered
                // producer that carry viewer hints, not graphics-state
                // semantics — a conforming reader ignores them (§7.3.5,
                // Annex E).
                k if k.starts_with(b"AAPL:") => {}
                // Anything else is an unknown parameter — be strict so
                // a silent mis-render can't masquerade as support.
                other => {
                    return Err(Error::Unsupported {
                        feature: "ExtGState parameter",
                        detail: format!(
                            "/{} — unhandled ExtGState key",
                            String::from_utf8_lossy(other)
                        ),
                    })
                }
            }
        }
        Ok(())
    }

    /// Install the pending `W`/`W*` clip (§8.5.4). The clip is
    /// rasterized under the paint-time CTM into a pixmap-sized mask and
    /// intersected with the existing clip. `W` on an empty path makes
    /// the clip empty. The path is *not* consumed here — the caller
    /// (`n` or a painting op) resets it separately.
    fn install_clip(&mut self) -> Result<(), Error> {
        let rule = self.path.pending_clip.take().unwrap();
        if self.extraction_only {
            // No pixels — a pending clip is just consumed.
            return Ok(());
        }
        let ts = self.paint_ts()?;
        let Some(new_path) = self.path.builder.clone().finish() else {
            // W on an empty path empties the clip.
            return self.clip_empty();
        };
        self.clip_intersect(&new_path, rule, ts)
    }

    /// Intersect the clip with `path` — the effective clip and, while a
    /// soft mask is set, the hard clip underneath it.
    fn clip_intersect(
        &mut self,
        path: &tiny_skia::Path,
        rule: FillRule,
        ts: Transform,
    ) -> Result<(), Error> {
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let apply = |slot: &mut Option<Mask>| -> Result<(), Error> {
            match slot {
                None => {
                    let mut mask = Mask::new(w, h)
                        .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?;
                    mask.fill_path(path, rule, true, ts);
                    *slot = Some(mask);
                }
                Some(existing) => existing.intersect_path(path, rule, true, ts),
            }
            Ok(())
        };
        apply(&mut self.gs.clip)?;
        if self.gs.soft_mask.is_some() {
            apply(&mut self.gs.hard_clip)?;
        }
        Ok(())
    }

    /// Clip everything away (`W n` on an empty path, a degenerate BBox).
    fn clip_empty(&mut self) -> Result<(), Error> {
        let empty = || {
            Mask::new(self.pixmap.width(), self.pixmap.height())
                .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))
        };
        self.gs.clip = Some(empty()?);
        if self.gs.soft_mask.is_some() {
            self.gs.hard_clip = Some(empty()?);
        }
        Ok(())
    }

    /// Install `mask` as the soft mask: the effective clip becomes the
    /// hard clip multiplied by it.
    fn set_soft_mask(&mut self, mask: Mask) {
        self.clear_soft_mask();
        let mut eff = match self.gs.clip.take() {
            Some(c) => c,
            None => Mask::new(self.pixmap.width(), self.pixmap.height())
                .map(|mut m| {
                    m.data_mut().fill(255);
                    m
                })
                .expect("page-sized mask"),
        };
        self.gs.hard_clip = Some(eff.clone());
        for (e, m) in eff.data_mut().iter_mut().zip(mask.data()) {
            *e = mul255(*e, *m);
        }
        self.gs.clip = Some(eff);
        self.gs.soft_mask = Some(Rc::new(mask));
    }

    /// `/SMask /None`: back to the hard clip.
    fn clear_soft_mask(&mut self) {
        if self.gs.soft_mask.take().is_some() {
            self.gs.clip = self.gs.hard_clip.take();
        }
    }

    // ----------------------------------------------------------------
    // XObject invocation (§8.8 / §8.9 / §8.10)
    // ----------------------------------------------------------------

    /// `Do name`: resolve `name` in the current `/XObject` resource
    /// dictionary and paint the XObject it refers to. Names that do
    /// not resolve to a stream are `Error::Structure`; a stream with an
    /// unsupported `/Subtype` is `Error::Unsupported`.
    fn do_xobject(&mut self, name: &[u8]) -> Result<(), Error> {
        if self.xobject_invocations >= self.limits.max_xobject_invocations {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_invocations",
                max: self.limits.max_xobject_invocations,
            });
        }
        self.xobject_invocations += 1;

        // Fetch the (still-indirect) XObject entry so the form's own
        // reference can be tracked for cycle detection — resolve it
        // only after recording which object the name points at.
        let entry = self
            .doc
            .resolve_resource_raw(&self.resources, "XObject", name)?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/XObject /{} is not defined in the current resources",
                    String::from_utf8_lossy(name)
                ))
            })?;
        let entry_ref = entry.as_ref();
        let obj = self.doc.deref(&entry)?;
        let (dict, raw) = obj.as_stream().ok_or_else(|| {
            Error::Structure(format!(
                "/XObject /{} is not a stream",
                String::from_utf8_lossy(name)
            ))
        })?;
        let dict = dict.clone();
        let raw = raw.to_vec();
        // Hidden by an enclosing /OC span, or by the XObject's own /OC
        // membership (§8.11.3.3): not painted, text not extracted —
        // unless extraction was asked to include hidden content.
        if self.hidden > 0 {
            return Ok(());
        }
        if let Some(oc) = dict.get(b"OC".as_slice()) {
            if !self.oc.visible(self.doc, oc)? && !self.oc_include_hidden {
                return Ok(());
            }
        }
        match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
            Some(b"Form") => self.do_form(&dict, &raw, name, entry_ref),
            // Images carry no text — in extraction mode the dict is
            // still validated/refused above but nothing is decoded.
            Some(b"Image") => {
                if self.extraction_only {
                    Ok(())
                } else {
                    self.do_image(&dict, &raw, name)
                }
            }
            Some(other) => Err(Error::Unsupported {
                feature: "XObject subtype",
                detail: format!(
                    "/{} — only /Form and /Image XObjects are supported",
                    String::from_utf8_lossy(other)
                ),
            }),
            None => Err(Error::Structure(format!(
                "/XObject /{} has no /Subtype",
                String::from_utf8_lossy(name)
            ))),
        }
    }

    /// Paint a Form XObject (§8.10): save gs, concat `/Matrix`, clip
    /// to `/BBox`, run the form's operations under its own
    /// `/Resources`, restore gs. A form that (transitively) invokes
    /// itself is `Error::CycleDetected`; nesting is bounded by
    /// `max_xobject_depth`.
    fn do_form(
        &mut self,
        dict: &Dict,
        raw: &[u8],
        _name: &[u8],
        entry_ref: Option<IndirectRef>,
    ) -> Result<(), Error> {
        if self.xobject_depth >= self.limits.max_xobject_depth {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                max: self.limits.max_xobject_depth as u64,
            });
        }

        // Required geometry: BBox in form space.
        let bbox = dict
            .get(b"BBox".as_slice())
            .ok_or_else(|| Error::Structure("form XObject has no /BBox".into()))
            .and_then(|o| {
                crate::page::Rect::from_object(o)
                    .ok_or_else(|| Error::Structure("/BBox is not a 4-number rectangle".into()))
            })?;
        if !rect_is_finite(&bbox) {
            return Err(Error::Structure("/BBox has non-finite values".into()));
        }

        // Optional matrix: default identity.
        let form_matrix = match dict.get(b"Matrix".as_slice()) {
            None | Some(Object::Null) => Matrix::IDENTITY,
            Some(o) => matrix_from_array(o)
                .ok_or_else(|| Error::Structure("/Matrix is not a 6-number array".into()))?,
        };
        if !form_matrix.is_finite() {
            return Err(Error::Structure("/Matrix has non-finite values".into()));
        }

        // Group XObjects (§8.10.3, §11.4.7, §11.6.6): a transparency
        // group is composited as a whole.
        //  * isolated → an offscreen layer that starts transparent (the
        //    backdrop the spec prescribes), composited once with the
        //    group's `ca`, `BM` and soft mask;
        //  * non-isolated, opaque (CA = ca = 1), Normal, no soft mask
        //    → painted inline on the page (identical result);
        //  * non-isolated otherwise → painted over a copy of the page
        //    (so blend modes inside see the real backdrop), the
        //    backdrop's contribution removed afterwards per §11.4.7,
        //    then composited once like an isolated group;
        //  * knockout groups need per-object backdrop replacement →
        //    refused.
        let mut group = GroupMode::None;
        if let Some(g) = dict.get(b"Group".as_slice()) {
            let g = self.doc.deref(g)?;
            let gd = g
                .as_dict()
                .ok_or_else(|| Error::Structure("/Group is not a dictionary".into()))?;
            let s = gd.get(b"S".as_slice()).and_then(|o| o.as_name());
            if s != Some(b"Transparency") {
                return Err(Error::Unsupported {
                    feature: "group XObject (/Group in a form dictionary)",
                    detail: format!(
                        "/Group /S /{} is not a transparency group",
                        String::from_utf8_lossy(s.unwrap_or(b"?"))
                    ),
                });
            }
            let knockout = gd.get(b"K".as_slice()).and_then(|o| o.as_bool()) == Some(true);
            if knockout {
                return Err(Error::Unsupported {
                    feature: "group XObject (/Group in a form dictionary)",
                    detail: "knockout transparency group (/K true) needs per-object backdrop \
                             replacement, which is not implemented"
                        .into(),
                });
            }
            let isolated = gd.get(b"I".as_slice()).and_then(|o| o.as_bool()) == Some(true);
            let opaque_normal = self.gs.fill_alpha >= 1.0
                && self.gs.stroke_alpha >= 1.0
                && self.gs.blend_mode == tiny_skia::BlendMode::SourceOver
                && self.gs.soft_mask.is_none();
            // An isolated group composites against a transparent
            // backdrop, which an offscreen layer *is* — exact whatever
            // its contents blend with. A non-isolated group sees the
            // page: exact inline when opaque-Normal; through a layer
            // otherwise, provided its contents blend Normal.
            group = if isolated {
                GroupMode::Layer { isolated: true }
            } else if opaque_normal {
                GroupMode::Inline
            } else {
                GroupMode::Layer { isolated: false }
            };
        }
        // /OC was evaluated by `do_xobject`. A /Ref (reference
        // XObject) imports an external page — refuse rather than guess.
        if dict.contains_key(b"Ref".as_slice()) {
            return Err(Error::Unsupported {
                feature: "reference XObject (/Ref in a form dictionary)",
                detail: "importing external pages is not implemented".into(),
            });
        }

        // Self-reference guard: a form that invokes itself (directly
        // or transitively) is rejected. Track by the indirect ref the
        // resource name resolves to — same form used at different
        // spots is fine, recursion is not.
        if let Some(r) = entry_ref {
            if self.xobject_stack.contains(&r) {
                return Err(Error::CycleDetected {
                    context: "form XObject recursion",
                });
            }
            self.xobject_stack.push(r);
        }
        let pop_ref = entry_ref.is_some();

        // The form's own resource dictionary; §7.8.3 says a form
        // *without* /Resources inherits the caller's (obsolete but
        // encountered). A present /Resources replaces it entirely —
        // never merge per-key.
        let form_resources = match dict.get(b"Resources".as_slice()) {
            None | Some(Object::Null) => self.resources.clone(),
            Some(r) => r.clone(),
        };
        let saved_resources = self.swap_resources(form_resources);

        // §8.10.1 Do(): (a) save gs, (b) concat Matrix into CTM,
        // (c) clip to BBox, (d) run form ops, (e) restore gs.
        if self.gs_stack.len() >= self.limits.max_gsave_depth {
            return Err(Error::LimitExceeded {
                limit: "max_gsave_depth",
                max: self.limits.max_gsave_depth as u64,
            });
        }
        self.gs_stack.push(self.gs.clone());
        // CTM' = Matrix × CTM: form-space points go through /Matrix
        // *first*, then the existing CTM. `concat` applies `self` before
        // its argument, so `form_matrix` is the receiver here.
        self.gs.ctm = form_matrix.concat(self.gs.ctm);
        // §8.7.3.1: patterns used inside the form map to the form's
        // coordinate space, not the page's.
        let saved_pattern_base = std::mem::replace(&mut self.pattern_base, self.gs.ctm);

        // Clip to BBox in *form space* — build the BBox path in user
        // space and intersect via the same mechanism as W.
        // Clip to BBox — skipped in extraction mode (no raster clip).
        let clip_result = if self.extraction_only {
            Ok(())
        } else {
            self.intersect_clip_rect(&bbox)
        };

        // Decode and run the form's content stream.
        let run_result = (|| {
            clip_result?;
            let decoded = self.doc.decode_stream(dict, raw)?;
            let ops = crate::content::parse_content_stream(&[decoded], self.limits)?;
            self.xobject_depth += 1;
            let r = match group {
                GroupMode::None | GroupMode::Inline => self.run(&ops),
                GroupMode::Layer { isolated } => self.run_in_layer(&ops, isolated),
            };
            self.xobject_depth -= 1;
            r
        })();

        self.swap_resources(saved_resources);
        self.pattern_base = saved_pattern_base;
        if pop_ref {
            self.xobject_stack.pop();
        }
        self.gs = self.gs_stack.pop().expect("pushed above");
        run_result
    }

    /// Paint a transparency group's `ops` offscreen and composite the
    /// result once with the group's constant alpha (`ca`), blend mode
    /// and soft mask. Inside the group the alpha constants are 1, the
    /// blend mode Normal and the soft mask None (§11.6.6); the
    /// caller's `gs` pop restores them afterwards.
    ///
    /// An isolated group is painted onto a transparent layer. A
    /// non-isolated group is painted onto a *copy of the current page*
    /// so blend modes inside see the real backdrop, and onto a
    /// transparent layer to obtain the group's own alpha αgn; the
    /// backdrop's contribution is then removed per §11.4.7,
    /// C = Cn + (Cn − C0)·(α0/αgn − α0), leaving the group's colour and
    /// αgn to composite exactly like an isolated group.
    fn run_in_layer(&mut self, ops: &[Operation], isolated: bool) -> Result<(), Error> {
        if self.extraction_only {
            return self.run(ops);
        }
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let bytes = w as u64 * h as u64 * 4;
        let layers = if isolated { 1 } else { 2 };
        // The page pixmap plus every live layer must fit the budget.
        if self
            .layer_bytes
            .saturating_add(bytes)
            .saturating_add(bytes * layers)
            > self.limits.max_render_buffer_bytes
        {
            return Err(Error::LimitExceeded {
                limit: "max_render_buffer_bytes",
                max: self.limits.max_render_buffer_bytes,
            });
        }
        let alpha = self.gs.fill_alpha;
        let bm = self.gs.blend_mode;
        let mask = self.gs.soft_mask.clone();
        self.clear_soft_mask();
        self.gs.fill_alpha = 1.0;
        self.gs.stroke_alpha = 1.0;
        self.gs.blend_mode = tiny_skia::BlendMode::SourceOver;

        // Pass 1: onto transparency → the group's own colour (isolated
        // semantics) and alpha αgn.
        let mut layer = Pixmap::new(w, h)
            .ok_or_else(|| Error::Structure("cannot allocate transparency-group layer".into()))?;
        let saved = (self.gs.clone(), self.gs_stack.len());
        std::mem::swap(&mut *self.pixmap, &mut layer);
        self.layer_bytes += bytes;
        let r = self.run(ops);
        self.layer_bytes -= bytes;
        std::mem::swap(&mut *self.pixmap, &mut layer);
        r?;

        if !isolated {
            // Pass 2: onto a copy of the backdrop, then remove the
            // backdrop's contribution.
            self.gs = saved.0;
            self.gs_stack.truncate(saved.1);
            let mut over = self.pixmap.clone();
            std::mem::swap(&mut *self.pixmap, &mut over);
            self.layer_bytes += bytes;
            let r = self.run(ops);
            self.layer_bytes -= bytes;
            std::mem::swap(&mut *self.pixmap, &mut over);
            r?;
            let backdrop = self.pixmap.data();
            for ((g, n), b) in layer
                .data_mut()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(over.data().as_chunks::<4>().0)
                .zip(backdrop.as_chunks::<4>().0)
            {
                let agn = g[3] as f64 / 255.0;
                let a0 = b[3] as f64 / 255.0;
                let an = n[3] as f64 / 255.0;
                if agn <= 0.0 || a0 <= 0.0 || an <= 0.0 {
                    continue; // nothing painted, or no backdrop: pass 1 is exact
                }
                let f = a0 / agn - a0;
                for ch in 0..3 {
                    let cn = n[ch] as f64 / 255.0 / an;
                    let c0 = b[ch] as f64 / 255.0 / a0;
                    let c = clamp01(cn + (cn - c0) * f);
                    g[ch] = to_u8(c * agn);
                }
            }
        }
        self.composite_layer(&layer, alpha, bm, mask.as_deref());
        Ok(())
    }

    /// Draw the page's annotation appearance streams (§12.5.5).
    ///
    /// For each entry of `/Annots`, in array order (the painting
    /// order): skip annotations flagged *Hidden* or *NoView*, skip
    /// `/Popup` (a viewer-side pop-up window, never part of the page
    /// image), skip a `/Link` without `/AP` (links have no appearance
    /// unless one is supplied). Every other annotation must carry an
    /// `/AP` with a normal appearance `/N` — a stream, or a
    /// sub-dictionary of states selected by `/AS`. An annotation whose
    /// appearance we would have to *synthesise* (no `/AP`) is refused
    /// with `Unsupported`, never silently dropped.
    ///
    /// Placement: the form's `/BBox` transformed by its `/Matrix` is
    /// fitted to `/Rect` (scale + translate — Algorithm 8.1), then the
    /// stream is painted exactly like a Form XObject (`do_form`: gs
    /// saved, `/Matrix` applied, clipped to `/BBox`, own `/Resources`,
    /// gs restored) under a *fresh* graphics state — annotation
    /// appearances never inherit the page content's state.
    fn draw_annotations(&mut self, page: &Page, page_resources: &Object) -> Result<(), Error> {
        let Some(annots) = page_dict_entry(self.doc, page, b"Annots")? else {
            return Ok(());
        };
        let form = crate::acroform::FormDefaults::from_catalog(self.doc)?;
        let entries = match annots {
            Object::Array(a) => a,
            Object::Null => return Ok(()),
            _ => return Err(Error::Structure("/Annots is not an array".into())),
        };
        for entry in &entries {
            let entry_ref = entry.as_ref();
            let annot = self.doc.deref(entry)?;
            if matches!(annot, Object::Null) {
                // §7.3.9: null in an array is the absence of a value.
                continue;
            }
            let Some(ad) = annot.as_dict() else {
                return Err(Error::Structure(
                    "/Annots entry is not an annotation dictionary".into(),
                ));
            };
            let subtype: Vec<u8> = ad
                .get(b"Subtype".as_slice())
                .and_then(|o| o.as_name())
                .map(|n| n.to_vec())
                .unwrap_or_default();
            // /F flags (Table 165): bit 2 Hidden, bit 6 NoView.
            let flags = ad
                .get(b"F".as_slice())
                .and_then(|o| o.as_i64())
                .unwrap_or(0);
            if flags & 0x02 != 0 || flags & 0x20 != 0 {
                continue;
            }
            if subtype == b"Popup" {
                continue;
            }
            // §12.5.2 /OC: an annotation in a hidden group is not drawn.
            if let Some(oc) = ad.get(b"OC".as_slice()) {
                if !self.oc.visible(self.doc, oc)? {
                    continue;
                }
            }
            // /Rect, normalised (§12.5.2: corners may be in any order).
            let rect_obj = ad
                .get(b"Rect".as_slice())
                .ok_or_else(|| Error::Structure("annotation has no /Rect".into()))?;
            let rect_obj = self.doc.deref(rect_obj)?;
            let r = crate::page::Rect::from_object(&rect_obj)
                .ok_or_else(|| Error::Structure("/Rect is not a 4-number rectangle".into()))?;
            if !rect_is_finite(&r) {
                return Err(Error::Structure("/Rect has non-finite values".into()));
            }
            let (rx0, rx1) = (r.llx.min(r.urx), r.llx.max(r.urx));
            let (ry0, ry1) = (r.lly.min(r.ury), r.lly.max(r.ury));

            // The normal appearance: the file's /AP /N stream, or —
            // for a form widget without one (or under
            // /NeedAppearances) — one synthesised from the field
            // (§12.7.3.3, `crate::acroform`).
            let has_ap = ad.contains_key(b"AP".as_slice());
            let mut fit_rect = [rx0, ry0, rx1, ry1];
            let mut group_alpha: Option<f64> = None;
            let mut multiply = false;
            let synth = if subtype == b"Widget"
                && crate::acroform::needs_synthesis(self.doc, ad, &form, has_ap)?
            {
                match crate::acroform::synthesize(
                    self.doc,
                    ad,
                    [rx0, ry0, rx1, ry1],
                    &form,
                    self.limits,
                )? {
                    Some(a) => Some(a),
                    // Nothing to draw for this field (signature, no
                    // value, no /MK): the widget is simply blank.
                    None => continue,
                }
            } else if !has_ap && subtype != b"Widget" && subtype != b"Link" {
                // Markup annotations without /AP (§12.5.6,
                // `crate::annot_appearance`); unhandled subtypes are
                // refused there.
                match crate::annot_appearance::synthesize(
                    self.doc,
                    ad,
                    &subtype,
                    [rx0, ry0, rx1, ry1],
                    self.limits,
                )? {
                    Some(m) => {
                        fit_rect = m.rect;
                        group_alpha = m.alpha;
                        multiply = m.multiply;
                        Some(m.appearance)
                    }
                    None => continue,
                }
            } else {
                None
            };
            let [rx0, ry0, rx1, ry1] = fit_rect;
            let (sdict, sraw, stream_ref): (Dict, Vec<u8>, Option<IndirectRef>) = match synth {
                Some(a) => (a.dict, a.content, None),
                None => {
                    let ap = match ad.get(b"AP".as_slice()) {
                        Some(o) => self.doc.deref(o)?,
                        None => {
                            if subtype == b"Link" {
                                continue;
                            }
                            return Err(Error::Unsupported {
                                feature: "annotation without appearance stream",
                                detail: format!(
                                    "/Subtype /{} has no /AP — synthesising an appearance is \
                                     implemented for form widgets only",
                                    String::from_utf8_lossy(&subtype)
                                ),
                            });
                        }
                    };
                    let ap = ap
                        .as_dict()
                        .ok_or_else(|| Error::Structure("/AP is not a dictionary".into()))?;
                    let Some(n_obj) = ap.get(b"N".as_slice()) else {
                        return Err(Error::Structure("/AP has no normal appearance (/N)".into()));
                    };
                    let mut stream_ref = n_obj.as_ref();
                    let n = self.doc.deref(n_obj)?;
                    let stream = match n {
                        Object::Stream { .. } => n,
                        Object::Dict(states) => {
                            // Appearance sub-dictionary: one stream per
                            // state, selected by /AS. Without /AS: a
                            // single state is unambiguous; a widget's
                            // field value (/V) names its state; else
                            // refuse.
                            let key: Vec<u8> = match ad
                                .get(b"AS".as_slice())
                                .and_then(|o| o.as_name())
                            {
                                Some(k) => k.to_vec(),
                                None if states.len() == 1 => states.keys().next().unwrap().clone(),
                                None => {
                                    let v = if subtype == b"Widget" {
                                        crate::acroform::field_value_name(self.doc, ad)?
                                    } else {
                                        None
                                    };
                                    match v {
                                            Some(v) if states.contains_key(v.as_slice()) => v,
                                            Some(_) => b"Off".to_vec(),
                                            None => {
                                                return Err(Error::Unsupported {
                                                    feature:
                                                        "annotation appearance states without /AS",
                                                    detail: format!(
                                                        "/AP /N has {} states and the annotation names none",
                                                        states.len()
                                                    ),
                                                })
                                            }
                                        }
                                }
                            };
                            match states.get(key.as_slice()) {
                                // A state with no stream (e.g. /Off absent)
                                // draws nothing — that is the file's choice.
                                None => continue,
                                Some(o) => {
                                    stream_ref = o.as_ref();
                                    self.doc.deref(o)?
                                }
                            }
                        }
                        _ => {
                            return Err(Error::Structure(
                                "/AP /N is neither a stream nor a dictionary".into(),
                            ))
                        }
                    };
                    let (sdict, sraw) = stream.as_stream().ok_or_else(|| {
                        Error::Structure("appearance state is not a stream".into())
                    })?;
                    (sdict.clone(), sraw.to_vec(), stream_ref)
                }
            };

            // Algorithm 8.1: transform /BBox by /Matrix, fit the result's
            // bounds to /Rect. A degenerate transformed box (zero width
            // or height) gets scale 1 on that axis.
            let bbox = sdict
                .get(b"BBox".as_slice())
                .ok_or_else(|| Error::Structure("appearance stream has no /BBox".into()))
                .and_then(|o| {
                    crate::page::Rect::from_object(o)
                        .ok_or_else(|| Error::Structure("/BBox is not a 4-number rectangle".into()))
                })?;
            if !rect_is_finite(&bbox) {
                return Err(Error::Structure("/BBox has non-finite values".into()));
            }
            let matrix = match sdict.get(b"Matrix".as_slice()) {
                None | Some(Object::Null) => Matrix::IDENTITY,
                Some(o) => matrix_from_array(o)
                    .ok_or_else(|| Error::Structure("/Matrix is not a 6-number array".into()))?,
            };
            if !matrix.is_finite() {
                return Err(Error::Structure("/Matrix has non-finite values".into()));
            }
            let corners = [
                matrix.transform(bbox.llx, bbox.lly),
                matrix.transform(bbox.urx, bbox.lly),
                matrix.transform(bbox.urx, bbox.ury),
                matrix.transform(bbox.llx, bbox.ury),
            ];
            let tx0 = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
            let tx1 = corners
                .iter()
                .map(|c| c.0)
                .fold(f64::NEG_INFINITY, f64::max);
            let ty0 = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
            let ty1 = corners
                .iter()
                .map(|c| c.1)
                .fold(f64::NEG_INFINITY, f64::max);
            let sx = if tx1 - tx0 > 1e-9 {
                (rx1 - rx0) / (tx1 - tx0)
            } else {
                1.0
            };
            let sy = if ty1 - ty0 > 1e-9 {
                (ry1 - ry0) / (ty1 - ty0)
            } else {
                1.0
            };
            let a = Matrix::new(sx, 0.0, 0.0, sy, rx0 - tx0 * sx, ry0 - ty0 * sy);
            if !a.is_finite() {
                return Err(Error::Structure(
                    "annotation placement matrix is not finite".into(),
                ));
            }

            // Fresh state per annotation: page content state, text
            // matrices and any leftover gsave stack do not leak in.
            self.gs = GraphicsState::new();
            self.gs.ctm = a;
            // A synthesised markup appearance is a transparency group
            // composited once with the annotation's /CA (and /Multiply
            // for highlights).
            if let Some(ca) = group_alpha {
                self.gs.fill_alpha = ca;
                self.gs.stroke_alpha = ca;
            }
            if multiply {
                self.gs.blend_mode = tiny_skia::BlendMode::Multiply;
            }
            self.gs_stack.clear();
            self.in_text = false;
            self.text_tm = Matrix::IDENTITY;
            self.text_tlm = Matrix::IDENTITY;
            self.path.reset();
            self.swap_resources(page_resources.clone());
            if self.xobject_invocations >= self.limits.max_xobject_invocations {
                return Err(Error::LimitExceeded {
                    limit: "max_xobject_invocations",
                    max: self.limits.max_xobject_invocations,
                });
            }
            self.xobject_invocations += 1;
            self.do_form(
                &sdict,
                &sraw,
                b"annotation appearance",
                stream_ref.or(entry_ref),
            )?;
        }
        Ok(())
    }

    /// Intersect the current clip with `rect` (in user space under the
    /// current CTM) — the BBox clip of a Form XObject.
    fn intersect_clip_rect(&mut self, rect: &crate::page::Rect) -> Result<(), Error> {
        let ts = self.paint_ts()?;
        let mut pb = PathBuilder::new();
        pb.move_to(rect.llx as f32, rect.lly as f32);
        pb.line_to(rect.urx as f32, rect.lly as f32);
        pb.line_to(rect.urx as f32, rect.ury as f32);
        pb.line_to(rect.llx as f32, rect.ury as f32);
        pb.close();
        let Some(path) = pb.finish() else {
            // Degenerate BBox → empty clip.
            return self.clip_empty();
        };
        self.clip_intersect(&path, FillRule::Winding, ts)
    }

    /// Render an ExtGState `/SMask` dictionary (§11.6.5) into a device
    /// mask. The group `/G` is painted with the CTM in force now, under
    /// a fresh graphics state and no clip, into an offscreen layer:
    /// for `/S /Luminosity` the layer starts as the opaque backdrop
    /// `/BC` (in the group's colour space, black by default) and the
    /// mask is the luminosity of the result — so outside the group's
    /// `/BBox` it is the backdrop's luminosity; for `/S /Alpha` the
    /// layer starts transparent and the mask is its alpha. `/TR` maps
    /// the values through a function (`/Identity` = none).
    fn render_soft_mask(&mut self, sm: &Dict) -> Result<Mask, Error> {
        let luminosity = match sm.get(b"S".as_slice()).and_then(|o| o.as_name()) {
            Some(b"Luminosity") => true,
            Some(b"Alpha") => false,
            other => {
                return Err(Error::Structure(format!(
                    "/SMask /S must be /Alpha or /Luminosity (is {:?})",
                    other.map(String::from_utf8_lossy)
                )))
            }
        };
        let g = sm
            .get(b"G".as_slice())
            .ok_or_else(|| Error::Structure("/SMask has no /G group".into()))?;
        let g_ref = g.as_ref();
        let g = self.doc.deref(g)?;
        let (gdict, graw) = g
            .as_stream()
            .ok_or_else(|| Error::Structure("/SMask /G is not a form XObject stream".into()))?;
        let group = gdict
            .get(b"Group".as_slice())
            .map(|o| self.doc.deref(o))
            .transpose()?;
        let group = group.as_ref().and_then(|o| o.as_dict());
        if group
            .and_then(|d| d.get(b"S".as_slice()))
            .and_then(|o| o.as_name())
            != Some(b"Transparency")
        {
            return Err(Error::Structure(
                "/SMask /G must be a transparency group XObject".into(),
            ));
        }
        // Backdrop colour in the group's colour space (default black).
        let mut backdrop = [0u8, 0, 0];
        if luminosity {
            if let Some(bc) = sm.get(b"BC".as_slice()) {
                let comps: Vec<f64> = self
                    .doc
                    .deref(bc)?
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_number()).collect())
                    .unwrap_or_default();
                let cs = match group.and_then(|d| d.get(b"CS".as_slice())) {
                    Some(cs) => ColorSpace::parse(self.doc, cs, &self.resources, self.limits)?,
                    None => match comps.len() {
                        1 => ColorSpace::DeviceGray,
                        4 => ColorSpace::DeviceCMYK,
                        _ => ColorSpace::DeviceRGB,
                    },
                };
                if comps.len() != cs.ncomps() {
                    return Err(Error::Structure(format!(
                        "/SMask /BC has {} components, the group colour space {}",
                        comps.len(),
                        cs.ncomps()
                    )));
                }
                let rgb = cs.to_rgb(&comps)?;
                backdrop = [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])];
            }
        }
        // Transfer function: 256-entry table.
        let tr: Option<Vec<u8>> = match sm.get(b"TR".as_slice()) {
            None => None,
            Some(o) => {
                let o = self.doc.deref(o)?;
                match o.as_name() {
                    Some(b"Identity") => None,
                    Some(other) => {
                        return Err(Error::Structure(format!(
                            "/SMask /TR /{} is not a function",
                            String::from_utf8_lossy(other)
                        )))
                    }
                    None => {
                        let f = crate::function::parse_function(self.doc, &o, self.limits)?;
                        if f.n_inputs() != 1 {
                            return Err(Error::Structure("/SMask /TR must take one input".into()));
                        }
                        let mut t = Vec::with_capacity(256);
                        for i in 0..256 {
                            let out = f.eval(&[i as f64 / 255.0])?;
                            let v = out.first().copied().unwrap_or(0.0);
                            t.push(to_u8(clamp01(v)));
                        }
                        Some(t)
                    }
                }
            }
        };
        if self.extraction_only {
            return Mask::new(1, 1).ok_or_else(|| Error::Structure("mask allocation".into()));
        }

        // ---- paint the group into a layer ------------------------
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let bytes = w as u64 * h as u64 * 4;
        if self.layer_bytes.saturating_add(bytes).saturating_add(bytes)
            > self.limits.max_render_buffer_bytes
        {
            return Err(Error::LimitExceeded {
                limit: "max_render_buffer_bytes",
                max: self.limits.max_render_buffer_bytes,
            });
        }
        let mut layer = Pixmap::new(w, h)
            .ok_or_else(|| Error::Structure("cannot allocate soft-mask layer".into()))?;
        if luminosity {
            layer.fill(tiny_skia::Color::from_rgba8(
                backdrop[0],
                backdrop[1],
                backdrop[2],
                255,
            ));
        }
        // Fresh state: the CTM of the moment the ExtGState is applied
        // (§11.6.5.1), no clip, full alpha, Normal blend, no mask.
        let mut fresh = GraphicsState::new();
        fresh.ctm = self.gs.ctm;
        let saved_gs = std::mem::replace(&mut self.gs, fresh);
        let saved_stack = std::mem::take(&mut self.gs_stack);
        let saved_path = std::mem::take(&mut self.path);
        let saved_text_clip = self.text_clip.take();
        let saved_text = (
            std::mem::replace(&mut self.in_text, false),
            std::mem::replace(&mut self.text_tm, Matrix::IDENTITY),
            std::mem::replace(&mut self.text_tlm, Matrix::IDENTITY),
        );
        std::mem::swap(&mut *self.pixmap, &mut layer);
        self.layer_bytes += bytes;
        let r = self.do_form(gdict, graw, b"soft mask group", g_ref);
        self.layer_bytes -= bytes;
        std::mem::swap(&mut *self.pixmap, &mut layer);
        self.gs = saved_gs;
        self.gs_stack = saved_stack;
        self.path = saved_path;
        self.in_text = saved_text.0;
        self.text_clip = saved_text_clip;
        self.text_tm = saved_text.1;
        self.text_tlm = saved_text.2;
        r?;

        // ---- layer → mask ---------------------------------------
        let mut mask = Mask::new(w, h)
            .ok_or_else(|| Error::Structure("soft mask allocation failed".into()))?;
        let src = layer.data();
        for (m, px) in mask.data_mut().iter_mut().zip(src.as_chunks::<4>().0) {
            let v = if luminosity {
                // The layer is opaque everywhere (backdrop fill), so the
                // premultiplied bytes are the colour itself.
                let lum = 0.3 * px[0] as f64 + 0.59 * px[1] as f64 + 0.11 * px[2] as f64;
                lum.round().clamp(0.0, 255.0) as u8
            } else {
                px[3]
            };
            *m = match &tr {
                Some(t) => t[v as usize],
                None => v,
            };
        }
        Ok(mask)
    }

    /// Paint an Image XObject (§8.9): `/BitsPerComponent` 1/2/4/8/16,
    /// every colour space [`ColorSpace`] resolves, `/Decode`,
    /// `/ImageMask` stencils in the fill colour, `/SMask` soft masks
    /// (straight alpha; nearest-resampled when the mask size differs)
    /// and stencil / colour-key `/Mask`. Refused: `/OC`,
    /// `/SMaskInData ≠ 0`, `/Matte` (pre-blended soft masks) and
    /// non-separable blend modes.
    fn do_image(&mut self, dict: &Dict, raw: &[u8], name: &[u8]) -> Result<(), Error> {
        self.paint_image(dict, raw, name)
    }

    /// Decode and paint an image. The image is drawn into the unit
    /// square of the *current* user space (§8.9.4 — the CTM maps the
    /// unit square to the destination parallelogram).
    fn paint_image(&mut self, dict: &Dict, raw: &[u8], name: &[u8]) -> Result<(), Error> {
        // A non-separable blend mode (Hue/Saturation/Color/Luminosity)
        // on an image is composited per pixel like the separable ones.
        let src = self.decode_image_source(dict, raw, name, false)?;
        let is_mask = dict.get(b"ImageMask".as_slice()).and_then(|o| o.as_bool()) == Some(true);
        if is_mask && self.gs.fill_space.is_pattern() {
            // §8.9.6.2: a stencil mask paints the *current fill colour*,
            // and in /Pattern space that colour is the pattern — the
            // stencil becomes a coverage mask for the pattern paint.
            return self.composite_stencil_with_pattern(&src);
        }
        self.composite_image(&src)
    }

    /// Paint the fill pattern through a stencil image: the stencil is
    /// resampled into a device-space coverage mask exactly as
    /// `composite_image` resamples colours, then the pattern is
    /// painted under that mask (× the clip). No pattern selected in
    /// /Pattern space paints nothing (§8.7.3.3).
    fn composite_stencil_with_pattern(&mut self, src: &ImageSource) -> Result<(), Error> {
        let Some(pat) = self.gs.fill_pattern.clone() else {
            return Ok(());
        };
        let ts = self.paint_ts()?;
        let Some(inv) = ts.invert() else {
            return Err(Error::Unsupported {
                feature: "image transform",
                detail: "CTM is not invertible".into(),
            });
        };
        let (w, h) = (src.w as usize, src.h as usize);
        let corners = [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        let (mut minx, mut miny, mut maxx, mut maxy) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for (x, y) in corners {
            let mut p = tiny_skia::Point { x, y };
            ts.map_point(&mut p);
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
        }
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let x0 = minx.floor().max(0.0) as i32;
        let y0 = miny.floor().max(0.0) as i32;
        let x1 = maxx.ceil().min(pw as f32) as i32;
        let y1 = maxy.ceil().min(ph as f32) as i32;
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let ex = (ts.sx * ts.sx + ts.ky * ts.ky).sqrt().max(1e-6);
        let ey = (ts.kx * ts.kx + ts.sy * ts.sy).sqrt().max(1e-6);
        let density = (w as f32 / ex).max(h as f32 / ey);
        let k = if density.is_finite() {
            (density.ceil() as usize).clamp(1, 4)
        } else {
            1
        };
        let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height())
            .ok_or_else(|| Error::Structure("stencil mask allocation failed".into()))?;
        let data = mask.data_mut();
        for py in y0..y1 {
            for px in x0..x1 {
                let mut covered = 0u32;
                for j in 0..k {
                    for i in 0..k {
                        let mut up = tiny_skia::Point {
                            x: px as f32 + (i as f32 + 0.5) / k as f32,
                            y: py as f32 + (j as f32 + 0.5) / k as f32,
                        };
                        inv.map_point(&mut up);
                        let sx = (up.x * w as f32).floor();
                        let sy = ((1.0 - up.y) * h as f32).floor();
                        if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                            continue;
                        }
                        if src.rgba[(sy as usize * w + sx as usize) * 4 + 3] != 0 {
                            covered += 1;
                        }
                    }
                }
                if covered > 0 {
                    data[(py as u64 * pw as u64 + px as u64) as usize] =
                        (covered * 255 / (k * k) as u32) as u8;
                }
            }
        }
        if let Some(clip) = &self.gs.clip {
            for (m, c) in mask.data_mut().iter_mut().zip(clip.data()) {
                *m = mul255(*m, *c);
            }
        }
        self.paint_pattern_under_mask(&pat, mask)
    }

    /// Paint an inline image (§8.9.7) through the image XObject path:
    /// abbreviated keys and filter names are expanded (Tables 93/94)
    /// and the result is treated exactly like an image dictionary.
    /// `/L` (length) is dropped — the parser already delimited the
    /// data — and anything an image XObject refuses is refused here.
    fn paint_inline_image(&mut self, dict: &Dict, data: &[u8]) -> Result<(), Error> {
        let mut full = Dict::new();
        for (k, v) in dict {
            let key = crate::content::inline_key(k);
            if key == b"Length" {
                continue;
            }
            let value = match (key, v) {
                (b"Filter", Object::Name(n)) => {
                    Object::Name(crate::content::inline_filter(n).to_vec())
                }
                (b"Filter", Object::Array(a)) => Object::Array(
                    a.iter()
                        .map(|o| match o {
                            Object::Name(n) => {
                                Object::Name(crate::content::inline_filter(n).to_vec())
                            }
                            other => other.clone(),
                        })
                        .collect(),
                ),
                _ => v.clone(),
            };
            full.insert(key.to_vec(), value);
        }
        self.paint_image(&full, data, b"inline image")
    }

    /// `/Width` × `/Height` with the image limits applied.
    fn image_dims(&self, dict: &Dict, name: &[u8]) -> Result<(u32, u32), Error> {
        let width = dict
            .get(b"Width".as_slice())
            .and_then(|o| o.as_i64())
            .ok_or_else(|| Error::Structure(format!("image /{} has no /Width", show_name(name))))?;
        let height = dict
            .get(b"Height".as_slice())
            .and_then(|o| o.as_i64())
            .ok_or_else(|| {
                Error::Structure(format!("image /{} has no /Height", show_name(name)))
            })?;
        if width <= 0 || height <= 0 {
            return Err(Error::Structure(format!(
                "image dimensions {width}x{height} are not positive"
            )));
        }
        if width as u64 > self.limits.max_image_dimension as u64
            || height as u64 > self.limits.max_image_dimension as u64
        {
            return Err(Error::LimitExceeded {
                limit: "max_image_dimension",
                max: self.limits.max_image_dimension as u64,
            });
        }
        let samples = (width as u64)
            .checked_mul(height as u64)
            .ok_or(Error::LimitExceeded {
                limit: "max_image_total_samples",
                max: self.limits.max_image_total_samples,
            })?;
        if samples > self.limits.max_image_total_samples {
            return Err(Error::LimitExceeded {
                limit: "max_image_total_samples",
                max: self.limits.max_image_total_samples,
            });
        }
        Ok((width as u32, height as u32))
    }

    /// Decode an image XObject into a straight-alpha RGBA8 buffer
    /// (`w × h × 4`): samples unpacked per `/BitsPerComponent`,
    /// `/Decode` applied, converted through the image colour space,
    /// then `/SMask` or `/Mask` folded into the alpha channel. Image
    /// masks (`/ImageMask true`) take their colour from the current
    /// fill colour and their alpha from the stencil bits.
    ///
    /// `as_stencil` is set when decoding a `/Mask` stream: the stream
    /// must be an image mask and its own `/Mask`/`/SMask` entries are
    /// never consulted (so the recursion is one level deep at most).
    fn decode_image_source(
        &mut self,
        dict: &Dict,
        raw: &[u8],
        name: &[u8],
        as_stencil: bool,
    ) -> Result<ImageSource, Error> {
        let (width, height) = self.image_dims(dict, name)?;
        let (w, h) = (width as usize, height as usize);
        let is_mask = dict.get(b"ImageMask".as_slice()).and_then(|o| o.as_bool()) == Some(true);
        if as_stencil && !is_mask {
            return Err(Error::Structure(format!(
                "/Mask stream /{} is not an image mask (/ImageMask true)",
                show_name(name)
            )));
        }

        // ---- JPX: the codestream decides depth and channels ---------
        let filters = crate::filter::normalize_filters(dict.get(b"Filter".as_slice()))?;
        let is_jpx = filters.last().map(|f| f == b"JPXDecode").unwrap_or(false);
        let smask_in_data = match dict.get(b"SMaskInData".as_slice()) {
            None | Some(Object::Null) => 0,
            Some(o) => match o.as_i64() {
                Some(v @ 0..=2) => v,
                _ => {
                    return Err(Error::Structure(format!(
                        "image /{} has /SMaskInData outside 0..=2",
                        show_name(name)
                    )))
                }
            },
        };
        if smask_in_data != 0 && !is_jpx {
            return Err(Error::Structure(format!(
                "image /{} has /SMaskInData but is not JPX-encoded",
                show_name(name)
            )));
        }
        let jpx = if is_jpx {
            if is_mask {
                return Err(Error::Structure(format!(
                    "image /{} is a JPX image mask (§7.4.9 forbids it)",
                    show_name(name)
                )));
            }
            let img = self.decode_jpx_stream(dict, raw, &filters)?;
            if img.width != width || img.height != height {
                return Err(Error::Structure(format!(
                    "JPX codestream in /{} is {}x{} but the dictionary says {width}x{height}",
                    show_name(name),
                    img.width,
                    img.height
                )));
            }
            if smask_in_data != 0 && img.alpha.is_none() {
                return Err(Error::Structure(format!(
                    "image /{} has /SMaskInData {smask_in_data} but the codestream has no alpha channel",
                    show_name(name)
                )));
            }
            Some(img)
        } else {
            None
        };

        // ---- bit depth & colour space ------------------------------
        let bpc_entry = dict
            .get(b"BitsPerComponent".as_slice())
            .and_then(|o| o.as_i64());
        let bpc: u32 = if let Some(j) = &jpx {
            let _ = j;
            8
        } else if is_mask {
            match bpc_entry {
                None | Some(1) => 1,
                Some(other) => {
                    return Err(Error::Structure(format!(
                        "image mask /{} has /BitsPerComponent {other} (must be 1)",
                        show_name(name)
                    )))
                }
            }
        } else {
            match bpc_entry {
                Some(b @ (1 | 2 | 4 | 8 | 16)) => b as u32,
                Some(other) => {
                    return Err(Error::Unsupported {
                        feature: "image BitsPerComponent",
                        detail: format!("{other} on /{}", show_name(name)),
                    })
                }
                None => {
                    return Err(Error::Structure(format!(
                        "image /{} has no /BitsPerComponent",
                        show_name(name)
                    )))
                }
            }
        };
        let cs: Option<Rc<ColorSpace>> = if is_mask {
            None
        } else if let (Some(j), None) = (&jpx, dict.get(b"ColorSpace".as_slice())) {
            // §7.4.9: no /ColorSpace → the codestream's declaration.
            let cs = match j.space {
                crate::jpx::JpxSpace::Gray => ColorSpace::DeviceGray,
                crate::jpx::JpxSpace::Rgb => ColorSpace::DeviceRGB,
                crate::jpx::JpxSpace::Cmyk => ColorSpace::DeviceCMYK,
                crate::jpx::JpxSpace::Icc(n) | crate::jpx::JpxSpace::Unknown(n) => {
                    let note = format!(
                        "JPX image /{}: embedded colour declaration ({} channels) not evaluated, device space used",
                        show_name(name),
                        n
                    );
                    if !self.color_approximations.contains(&note) {
                        self.color_approximations.push(note);
                    }
                    match n {
                        1 => ColorSpace::DeviceGray,
                        3 => ColorSpace::DeviceRGB,
                        4 => ColorSpace::DeviceCMYK,
                        other => {
                            return Err(Error::Unsupported {
                                feature: "JPX colour space",
                                detail: format!("{other} channels without a /ColorSpace entry"),
                            })
                        }
                    }
                }
            };
            Some(Rc::new(cs))
        } else {
            let cs_obj = dict.get(b"ColorSpace".as_slice()).ok_or_else(|| {
                Error::Structure(format!("image /{} has no /ColorSpace", show_name(name)))
            })?;
            let cs = ColorSpace::parse(self.doc, cs_obj, &self.resources, self.limits)?;
            if cs.is_pattern() {
                return Err(Error::Structure(format!(
                    "image /{} has a /Pattern colour space (§8.9.5: not allowed)",
                    show_name(name)
                )));
            }
            self.note_approximation(&cs);
            Some(Rc::new(cs))
        };
        let ncomps = cs.as_ref().map(|c| c.ncomps()).unwrap_or(1);
        if let Some(j) = &jpx {
            if j.ncomps != ncomps {
                return Err(Error::Structure(format!(
                    "JPX codestream in /{} has {} channels, /ColorSpace has {ncomps}",
                    show_name(name),
                    j.ncomps
                )));
            }
        }
        let default_decode = cs
            .as_ref()
            .map(|c| c.default_decode(bpc))
            .unwrap_or_else(|| vec![(0.0, 1.0)]);
        // §7.4.9: /Decode is ignored for JPX images.
        let decode = if jpx.is_some() {
            default_decode.clone()
        } else {
            self.image_decode_array(dict, ncomps, &default_decode)?
        };

        // ---- sample data -------------------------------------------
        let is_dct = filters
            .last()
            .map(|f| f == b"DCTDecode" || f == b"DCT")
            .unwrap_or(false);
        let is_bilevel = filters
            .last()
            .map(|f| f == b"JBIG2Decode" || f == b"CCITTFaxDecode" || f == b"CCF")
            .unwrap_or(false);
        if is_bilevel && (bpc != 1 || ncomps != 1) {
            // §7.4.6/§7.4.7: these filters deliver 1-bit monochrome
            // samples; the dictionary has to describe exactly that.
            return Err(Error::Structure(format!(
                "/{} image /{} must be 1 bit per component with one colour component (has {bpc} bpc, {ncomps} components)",
                String::from_utf8_lossy(filters.last().unwrap()),
                show_name(name)
            )));
        }
        let data = match &jpx {
            Some(j) => j.samples.clone(),
            None => self.decode_image_data(dict, raw)?,
        };
        if is_dct {
            // JPEG always yields 8-bit interleaved samples; the dict's
            // declared depth and component count must agree.
            if bpc != 8 || is_mask {
                return Err(Error::Structure(format!(
                    "DCTDecode image /{} must be an 8-bit colour image",
                    show_name(name)
                )));
            }
            let got = data.len() / (w * h).max(1);
            if got != ncomps || data.len() != w * h * ncomps {
                return Err(Error::Structure(format!(
                    "JPEG in /{} decodes to {got} components, /ColorSpace has {ncomps}",
                    show_name(name)
                )));
            }
        }
        let row_bytes = (w as u64 * ncomps as u64 * bpc as u64).div_ceil(8);
        let need = row_bytes * h as u64;
        if (data.len() as u64) < need {
            return Err(Error::Structure(format!(
                "image /{} data length {} is shorter than {}x{}x{}@{}bpc = {}",
                show_name(name),
                data.len(),
                w,
                h,
                ncomps,
                bpc,
                need
            )));
        }
        let px_bytes = (w as u64 * h as u64)
            .checked_mul(4)
            .ok_or(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: self.limits.max_image_bytes,
            })?;
        if px_bytes > self.limits.max_image_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: self.limits.max_image_bytes,
            });
        }
        let mut rgba = vec![0u8; w * h * 4];

        if is_mask {
            // §8.9.6.2: with the default Decode [0 1] a sample of 0
            // paints; Decode [1 0] flips that. Stencil `/Mask` streams
            // share the rule (§8.9.6.4: 1 = masked out = not painted).
            let paint_sample: u32 = if decode[0].0 >= 0.5 { 1 } else { 0 };
            let fill = if as_stencil {
                Color::BLACK
            } else {
                self.gs.fill_color
            };
            for y in 0..h {
                let row_bit = y as u64 * row_bytes * 8;
                for x in 0..w {
                    let s = read_bits(&data, row_bit + x as u64, 1);
                    let i = (y * w + x) * 4;
                    rgba[i] = fill.r;
                    rgba[i + 1] = fill.g;
                    rgba[i + 2] = fill.b;
                    rgba[i + 3] = if s == paint_sample { fill.a } else { 0 };
                }
            }
            return Ok(ImageSource {
                w: width,
                h: height,
                rgba,
            });
        }

        let cs = cs.expect("colour image has a colour space");
        // Colour-key masking (§8.9.6.4, `/Mask` array): ranges are in
        // raw sample units, a pixel is masked when every component is
        // inside its range.
        let mask_entry = match dict.get(b"Mask".as_slice()) {
            Some(o) => Some(self.doc.deref(o)?),
            None => None,
        };
        let color_key: Option<Vec<(u32, u32)>> = match &mask_entry {
            Some(Object::Array(a)) => {
                if a.len() != ncomps * 2 {
                    return Err(Error::Structure(format!(
                        "/Mask colour-key array has {} entries, expected {}",
                        a.len(),
                        ncomps * 2
                    )));
                }
                let mut ranges = Vec::with_capacity(ncomps);
                for p in a.as_chunks::<2>().0 {
                    let lo = p[0].as_i64().ok_or_else(|| {
                        Error::Structure("/Mask colour-key entries must be integers".into())
                    })?;
                    let hi = p[1].as_i64().ok_or_else(|| {
                        Error::Structure("/Mask colour-key entries must be integers".into())
                    })?;
                    ranges.push((
                        lo.clamp(0, u32::MAX as i64) as u32,
                        hi.clamp(0, u32::MAX as i64) as u32,
                    ));
                }
                Some(ranges)
            }
            _ => None,
        };
        let paints_nothing = cs.paints_nothing();
        let mut conv = SampleConverter::new(cs.clone(), bpc, &decode)?;
        let mut comps = vec![0u32; ncomps];
        for y in 0..h {
            let row_bit = y as u64 * row_bytes * 8;
            for x in 0..w {
                let base = row_bit + x as u64 * ncomps as u64 * bpc as u64;
                for (c, slot) in comps.iter_mut().enumerate() {
                    *slot = read_bits(&data, base + c as u64 * bpc as u64, bpc);
                }
                let rgb = conv.convert(&comps)?;
                let i = (y * w + x) * 4;
                rgba[i] = rgb[0];
                rgba[i + 1] = rgb[1];
                rgba[i + 2] = rgb[2];
                let masked = color_key
                    .as_ref()
                    .map(|r| {
                        comps
                            .iter()
                            .zip(r)
                            .all(|(v, (lo, hi))| *v >= *lo && *v <= *hi)
                    })
                    .unwrap_or(false);
                rgba[i + 3] = if masked || paints_nothing { 0 } else { 255 };
            }
        }

        // ---- soft mask / stencil mask → alpha ----------------------
        // §8.9.6.4: when both are present /SMask wins; a JPX alpha
        // channel counts only with /SMaskInData (§7.4.9), 2 meaning
        // the colour is premultiplied by it.
        if let (Some(j), true) = (&jpx, smask_in_data != 0) {
            let alpha = j.alpha.as_ref().expect("checked above");
            for (i, a) in alpha.iter().enumerate() {
                let p = &mut rgba[i * 4..i * 4 + 4];
                if smask_in_data == 2 && *a != 0 && *a != 255 {
                    for c in p.iter_mut().take(3) {
                        *c = ((*c as u32 * 255 / *a as u32).min(255)) as u8;
                    }
                }
                p[3] = mul255(p[3], *a);
            }
        } else if let Some(sm) = dict.get(b"SMask".as_slice()) {
            let alpha = self.decode_soft_mask(sm, w, h)?;
            for (i, a) in alpha.iter().enumerate() {
                rgba[i * 4 + 3] = mul255(rgba[i * 4 + 3], *a);
            }
        } else if let Some(Object::Stream {
            dict: md,
            data: mraw,
        }) = &mask_entry
        {
            let (md, mraw) = (md.clone(), mraw.clone());
            let stencil = self.decode_image_source(&md, &mraw, b"Mask", true)?;
            let (sw, sh) = (stencil.w as usize, stencil.h as usize);
            for y in 0..h {
                let sy = y * sh / h;
                for x in 0..w {
                    let sx = x * sw / w;
                    let a = stencil.rgba[(sy * sw + sx) * 4 + 3];
                    let i = (y * w + x) * 4 + 3;
                    rgba[i] = mul255(rgba[i], a);
                }
            }
        }
        Ok(ImageSource {
            w: width,
            h: height,
            rgba,
        })
    }

    /// Run the byte filters that precede `/JPXDecode` (the last entry
    /// of `filters`) and decode the codestream.
    fn decode_jpx_stream(
        &self,
        dict: &Dict,
        raw: &[u8],
        filters: &[Vec<u8>],
    ) -> Result<crate::jpx::JpxImage, Error> {
        let mut pre = dict.clone();
        let n = filters.len() - 1;
        pre.insert(
            b"Filter".to_vec(),
            Object::Array(
                filters[..n]
                    .iter()
                    .map(|f| Object::Name(f.clone()))
                    .collect(),
            ),
        );
        let parms = crate::filter::normalize_decode_parms(
            dict.get(b"DecodeParms".as_slice()),
            filters.len(),
        )?;
        pre.insert(
            b"DecodeParms".to_vec(),
            Object::Array(
                parms[..n]
                    .iter()
                    .map(|p| p.clone().map(Object::Dict).unwrap_or(Object::Null))
                    .collect(),
            ),
        );
        let bytes = self.decode_image_data(&pre, raw)?;
        crate::jpx::decode(&bytes, self.limits)
    }

    /// Decode an image `/SMask` (§11.6.5.3) into a `w × h` alpha
    /// plane (0–255), nearest-resampled when the mask's own size
    /// differs. The mask must be a one-component image; `/Matte`
    /// (pre-blended colour) is refused rather than approximated.
    fn decode_soft_mask(&mut self, obj: &Object, w: usize, h: usize) -> Result<Vec<u8>, Error> {
        let sm = self.doc.deref(obj)?;
        let Some((sd, sraw)) = sm.as_stream() else {
            return Err(Error::Structure("image /SMask is not a stream".into()));
        };
        if let Some(st) = sd.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
            if st != b"Image" {
                return Err(Error::Structure(
                    "image /SMask is not an image XObject".into(),
                ));
            }
        }
        if sd.contains_key(b"Matte".as_slice()) {
            return Err(Error::Unsupported {
                feature: "pre-blended soft-mask image (/Matte)",
                detail: "un-premultiplying against /Matte is not implemented".into(),
            });
        }
        let (sw, sh) = self.image_dims(sd, b"SMask")?;
        let (sw, sh) = (sw as usize, sh as usize);
        // A JPX-encoded soft mask: one 8-bit channel from the codestream.
        let sm_filters = crate::filter::normalize_filters(sd.get(b"Filter".as_slice()))?;
        let jpx = if sm_filters
            .last()
            .map(|f| f == b"JPXDecode")
            .unwrap_or(false)
        {
            let img = self.decode_jpx_stream(sd, sraw, &sm_filters)?;
            if img.width as usize != sw || img.height as usize != sh {
                return Err(Error::Structure(format!(
                    "JPX /SMask codestream is {}x{} but the dictionary says {sw}x{sh}",
                    img.width, img.height
                )));
            }
            if img.ncomps != 1 {
                return Err(Error::Structure(format!(
                    "JPX /SMask has {} channels (must be one)",
                    img.ncomps
                )));
            }
            Some(img)
        } else {
            None
        };
        let bpc = match sd
            .get(b"BitsPerComponent".as_slice())
            .and_then(|o| o.as_i64())
        {
            _ if jpx.is_some() => 8,
            Some(b @ (1 | 2 | 4 | 8 | 16)) => b as u32,
            Some(other) => {
                return Err(Error::Unsupported {
                    feature: "image BitsPerComponent",
                    detail: format!("{other} on /SMask"),
                })
            }
            None => {
                return Err(Error::Structure(
                    "image /SMask has no /BitsPerComponent".into(),
                ))
            }
        };
        let cs = match (sd.get(b"ColorSpace".as_slice()), &jpx) {
            (None, Some(_)) => ColorSpace::DeviceGray,
            (cs_obj, _) => {
                let cs_obj = cs_obj
                    .ok_or_else(|| Error::Structure("image /SMask has no /ColorSpace".into()))?;
                ColorSpace::parse(self.doc, cs_obj, &self.resources, self.limits)?
            }
        };
        if cs.ncomps() != 1 {
            return Err(Error::Structure(format!(
                "image /SMask colour space has {} components (must be DeviceGray)",
                cs.ncomps()
            )));
        }
        let decode = if jpx.is_some() {
            vec![(0.0, 1.0)]
        } else {
            self.image_decode_array(sd, 1, &[(0.0, 1.0)])?
        };
        let data = match jpx {
            Some(img) => img.samples,
            None => self.decode_image_data(sd, sraw)?,
        };
        let row_bytes = (sw as u64 * bpc as u64).div_ceil(8);
        if (data.len() as u64) < row_bytes * sh as u64 {
            return Err(Error::Structure(format!(
                "image /SMask data length {} is shorter than {sw}x{sh}@{bpc}bpc",
                data.len()
            )));
        }
        let maxv = ((1u64 << bpc) - 1) as f64;
        let (dmin, dmax) = decode[0];
        let mut plane = vec![0u8; sw * sh];
        for y in 0..sh {
            let row_bit = y as u64 * row_bytes * 8;
            for x in 0..sw {
                let s = read_bits(&data, row_bit + x as u64 * bpc as u64, bpc) as f64;
                let v = dmin + s / maxv * (dmax - dmin);
                plane[y * sw + x] = to_u8(v);
            }
        }
        if sw == w && sh == h {
            return Ok(plane);
        }
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let sy = y * sh / h;
            for x in 0..w {
                out[y * w + x] = plane[sy * sw + x * sw / w];
            }
        }
        Ok(out)
    }

    /// Decode the image's byte stream — applies `/Filter` chain in
    /// order. `DCTDecode` is handled here (not in the generic stream
    /// decoder) because it produces *pixels*, not bytes; the PDF
    /// dict's Width/Height/ColorSpace are authoritative for the
    /// result's shape, the JPEG's own headers must agree.
    fn decode_image_data(&self, dict: &Dict, raw: &[u8]) -> Result<Vec<u8>, Error> {
        let filters = crate::filter::normalize_filters(dict.get(b"Filter".as_slice()))?;
        if filters.is_empty() {
            return Ok(raw.to_vec());
        }
        if filters.len() > self.limits.max_filter_chain {
            return Err(Error::LimitExceeded {
                limit: "max_filter_chain",
                max: self.limits.max_filter_chain as u64,
            });
        }
        let parms = crate::filter::normalize_decode_parms(
            dict.get(b"DecodeParms".as_slice()),
            filters.len(),
        )?;
        let mut buf = raw.to_vec();
        let last = filters.len() - 1;
        for (i, fname) in filters.iter().enumerate() {
            let parm = parms.get(i).and_then(|o| o.as_ref());
            let name = fname.as_slice();
            // An image filter yields samples, so nothing can follow it
            // (§7.4: the byte filters come first in the chain).
            if crate::filter::is_image_filter(name) && i != last {
                return Err(Error::Structure(format!(
                    "/{} must be the last filter of an image (followed by /{})",
                    String::from_utf8_lossy(name),
                    String::from_utf8_lossy(&filters[i + 1])
                )));
            }
            buf = match name {
                b"DCTDecode" | b"DCT" => self.dct_decode(&buf, parm)?,
                b"JBIG2Decode" => {
                    let (w, h) = image_dims_u32(dict)?;
                    let globals = match parm.and_then(|p| p.get(b"JBIG2Globals".as_slice())) {
                        None | Some(Object::Null) => None,
                        Some(g) => {
                            let g = self.doc.deref(g)?;
                            let (gd, graw) = g.as_stream().ok_or_else(|| {
                                Error::Structure("/JBIG2Globals is not a stream".into())
                            })?;
                            Some(self.doc.decode_stream(gd, graw)?)
                        }
                    };
                    crate::bilevel::jbig2_decode(&buf, globals.as_deref(), w, h, self.limits)?
                }
                b"CCITTFaxDecode" | b"CCF" => {
                    let (w, h) = image_dims_u32(dict)?;
                    crate::bilevel::ccitt_decode(&buf, parm, w, h, self.limits)?
                }
                b"JPXDecode" => {
                    // Image colour data is decoded by `decode_image_source`
                    // (which needs the codestream's channel layout); a
                    // JPX-encoded /SMask or /Mask stream lands here.
                    return Err(Error::Unsupported {
                        feature: "image stream filter",
                        detail: "/JPXDecode on a soft-mask or stencil stream".into(),
                    });
                }
                // Byte filters (Flate, LZW, ASCII85, ASCIIHex, RunLength)
                // and their predictors share the stream decoder.
                _ => crate::filter::apply_one_filter(name, parm, &buf, self.limits)?,
            };
            if buf.len() as u64 > self.limits.max_image_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_image_bytes",
                    max: self.limits.max_image_bytes,
                });
            }
        }
        Ok(buf)
    }

    /// JPEG via `jpeg-decoder`. Returns raw 8-bit samples in the
    /// decoder's component order (grayscale or RGB). `/ColorTransform`
    /// in the PDF's `/DecodeParms` and the JPEG's internal markers
    /// decide whether YCbCr→RGB conversion is applied — we trust the
    /// decoder's output format and verify it against `/ColorSpace`
    /// afterwards.
    fn dct_decode(&self, data: &[u8], parms: Option<&Dict>) -> Result<Vec<u8>, Error> {
        // DecodeParms for DCTDecode: only /ColorTransform is defined.
        if let Some(p) = parms {
            match p.get(b"ColorTransform".as_slice()) {
                None | Some(Object::Null) => {}
                Some(o) => {
                    let v = o.as_i64().ok_or_else(|| {
                        Error::Structure("/ColorTransform is not an integer".into())
                    })?;
                    if v != 0 && v != 1 {
                        return Err(Error::Unsupported {
                            feature: "DCTDecode /ColorTransform",
                            detail: format!("{v} (only 0 or 1 defined)"),
                        });
                    }
                }
            }
        }
        let mut dec = jpeg_decoder::Decoder::new(data);
        dec.set_max_decoding_buffer_size(self.limits.max_image_bytes as usize);
        // Read the header up front so a malformed JPEG fails before
        // the decoder allocates its frame buffers.
        dec.read_info()
            .map_err(|e| Error::Parse(format!("DCTDecode (JPEG) header: {e}")))?;
        let mut pixels = dec
            .decode()
            .map_err(|e| Error::Parse(format!("DCTDecode (JPEG) failed: {e}")))?;
        // `jpeg-decoder` hands back four-component data *inverted*
        // (255 − value), as a viewer of Adobe "Inverted CMYK" JPEGs
        // would want. PDF semantics are the stored sample values with
        // the dictionary's `/Decode` applied (Photoshop-produced files
        // carry `/Decode [1 0 1 0 1 0 1 0]` for exactly this reason),
        // so undo the decoder's inversion.
        if dec.info().map(|i| i.pixel_format) == Some(jpeg_decoder::PixelFormat::CMYK32) {
            for b in &mut pixels {
                *b = 255 - *b;
            }
        }
        Ok(pixels)
    }

    /// The image's `/Decode` array; `default` is the colour space's
    /// per-component default (§8.9.5.2 Table 90).
    fn image_decode_array(
        &self,
        dict: &Dict,
        ncomps: usize,
        default: &[(f64, f64)],
    ) -> Result<Vec<(f64, f64)>, Error> {
        match dict.get(b"Decode".as_slice()) {
            None | Some(Object::Null) => Ok(default.to_vec()),
            Some(o) => {
                let o = self.doc.deref(o)?;
                let arr = o
                    .as_array()
                    .ok_or_else(|| Error::Structure("/Decode is not an array".into()))?;
                if arr.len() != ncomps * 2 {
                    return Err(Error::Structure(format!(
                        "/Decode has {} entries, expected {}",
                        arr.len(),
                        ncomps * 2
                    )));
                }
                let mut out = Vec::with_capacity(ncomps);
                for pair in arr.as_chunks::<2>().0 {
                    let lo = pair[0].as_number().ok_or_else(|| {
                        Error::Structure("/Decode entries must be numbers".into())
                    })?;
                    let hi = pair[1].as_number().ok_or_else(|| {
                        Error::Structure("/Decode entries must be numbers".into())
                    })?;
                    if !(lo.is_finite() && hi.is_finite()) {
                        return Err(Error::Structure("/Decode entries must be finite".into()));
                    }
                    out.push((lo, hi));
                }
                Ok(out)
            }
        }
    }

    /// Composite a decoded image (straight RGBA) into the current
    /// pixmap. The CTM maps the unit square of user space to the
    /// destination; each covered device pixel is inverse-mapped to the
    /// source. Magnified images sample nearest-neighbour (as before —
    /// `/Interpolate` is a hint the spec lets us ignore); minified ones
    /// average a `k × k` grid of sub-samples per device pixel so that
    /// down-scaled photos and the image's own edges are area-weighted
    /// rather than aliased. The clip mask's coverage, the image alpha
    /// and `ca` multiply into one source alpha, then the pixel is
    /// composited with the active blend mode via [`composite_px`].
    fn composite_image(&mut self, src: &ImageSource) -> Result<(), Error> {
        let ts = self.paint_ts()?;
        let Some(inv) = ts.invert() else {
            return Err(Error::Unsupported {
                feature: "image transform",
                detail: "CTM is not invertible".into(),
            });
        };
        let (w, h) = (src.w as usize, src.h as usize);
        // Bounding box of the unit square under `ts`.
        let corners = [(0.0f32, 0.0f32), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        let mut minx = f32::INFINITY;
        let mut miny = f32::INFINITY;
        let mut maxx = f32::NEG_INFINITY;
        let mut maxy = f32::NEG_INFINITY;
        for (x, y) in corners {
            let mut p = tiny_skia::Point { x, y };
            ts.map_point(&mut p);
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
        }
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let x0 = minx.floor().max(0.0) as i32;
        let y0 = miny.floor().max(0.0) as i32;
        let x1 = maxx.ceil().min(pw as f32) as i32;
        let y1 = maxy.ceil().min(ph as f32) as i32;
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        // Device-pixel length of the image's unit edges → source
        // samples per device pixel → sub-sample grid size.
        let ex = (ts.sx * ts.sx + ts.ky * ts.ky).sqrt().max(1e-6);
        let ey = (ts.kx * ts.kx + ts.sy * ts.sy).sqrt().max(1e-6);
        let density = (w as f32 / ex).max(h as f32 / ey);
        let k = if density.is_finite() {
            (density.ceil() as usize).clamp(1, 4)
        } else {
            1
        };
        let kk = (k * k) as f64;
        let fill_alpha = clamp01(self.gs.fill_alpha);
        let bm = self.gs.blend_mode;
        let clip = self.gs.clip.take();
        for py in y0..y1 {
            for px in x0..x1 {
                let coverage = match &clip {
                    Some(mask) => {
                        let idx = (py as u64 * mask.width() as u64 + px as u64) as usize;
                        mask.data().get(idx).copied().unwrap_or(0) as f64 / 255.0
                    }
                    None => 1.0,
                };
                if coverage <= 0.0 {
                    continue;
                }
                // Accumulate premultiplied sub-samples; sub-samples
                // that fall outside the image count as transparent.
                let (mut sr, mut sg, mut sb, mut sa) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for j in 0..k {
                    for i in 0..k {
                        let mut up = tiny_skia::Point {
                            x: px as f32 + (i as f32 + 0.5) / k as f32,
                            y: py as f32 + (j as f32 + 0.5) / k as f32,
                        };
                        inv.map_point(&mut up);
                        // Image space (§8.9.4): sample (0,0) is the
                        // top-left and sits at user-space (0,1).
                        let sx = (up.x * w as f32).floor();
                        let sy = ((1.0 - up.y) * h as f32).floor();
                        if sx < 0.0 || sy < 0.0 || sx >= w as f32 || sy >= h as f32 {
                            continue;
                        }
                        let si = (sy as usize * w + sx as usize) * 4;
                        let a = src.rgba[si + 3] as f64 / 255.0;
                        if a == 0.0 {
                            continue;
                        }
                        sr += src.rgba[si] as f64 / 255.0 * a;
                        sg += src.rgba[si + 1] as f64 / 255.0 * a;
                        sb += src.rgba[si + 2] as f64 / 255.0 * a;
                        sa += a;
                    }
                }
                if sa <= 0.0 {
                    continue;
                }
                let rgb = [sr / sa, sg / sa, sb / sa];
                let alpha = (sa / kk) * fill_alpha * coverage;
                let i = (py as u64 * pw as u64 + px as u64) as usize * 4;
                let buf = self.pixmap.data_mut();
                composite_px(&mut buf[i..i + 4], rgb, alpha, bm);
            }
        }
        self.gs.clip = clip;
        Ok(())
    }

    /// `sh name` (§8.7.4.2): paint the shading over the current clip
    /// region, in current user space. `/Background` is ignored for
    /// `sh` (it applies to pattern use only); `/BBox` clips.
    fn paint_shading_op(&mut self, name: &Name) -> Result<(), Error> {
        if self.extraction_only || self.hidden > 0 {
            return Ok(());
        }
        let obj = self
            .doc
            .resolve_resource(&self.resources, "Shading", name)?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/Shading /{} is not defined in the current resources",
                    show_name(name)
                ))
            })?;
        let sh = parse_shading(self.doc, &obj, &self.resources, self.limits)?;
        if let Some(a) = &sh.approximation {
            if !self.color_approximations.contains(a) {
                self.color_approximations.push(a.clone());
            }
        }
        let ctm = self.gs.ctm;
        self.paint_shading(&sh, &ctm, None, false)
    }

    /// Resolve `/Pattern name` in the current resources as a shading
    /// pattern (`/PatternType 2`). Tiling patterns (`/PatternType 1`)
    /// need their cell content tiled across the fill — not
    /// implemented, refused.
    /// Resolve `/Pattern name` (§8.7.3): a shading pattern, or a tiling
    /// pattern whose uncoloured variant takes its colour from `comps`
    /// through the base space of `space`.
    fn load_pattern(
        &mut self,
        name: &[u8],
        space: &ColorSpace,
        comps: &[f64],
    ) -> Result<Rc<PatternPaint>, Error> {
        let entry = self
            .doc
            .resolve_resource_raw(&self.resources, "Pattern", name)?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/Pattern /{} is not defined in the current resources",
                    show_name(name)
                ))
            })?;
        let entry_ref = entry.as_ref();
        let obj = self.doc.deref(&entry)?;
        let dict = obj
            .as_dict_like()
            .ok_or_else(|| Error::Structure("pattern is not a dictionary".into()))?;
        let matrix = match dict.get(b"Matrix".as_slice()) {
            None | Some(Object::Null) => Matrix::IDENTITY,
            Some(o) => {
                let o = self.doc.deref(o)?;
                matrix_from_array(&o).ok_or_else(|| {
                    Error::Structure("pattern /Matrix is not a 6-number array".into())
                })?
            }
        };
        if !matrix.is_finite() {
            return Err(Error::Structure(
                "pattern /Matrix has non-finite values".into(),
            ));
        }
        match dict.get(b"PatternType".as_slice()).and_then(|o| o.as_i64()) {
            Some(2) => {}
            Some(1) => {
                return Ok(Rc::new(PatternPaint::Tiling(Rc::new(
                    self.load_tiling_pattern(name, &obj, matrix, space, comps, entry_ref)?,
                ))))
            }
            other => {
                return Err(Error::Structure(format!(
                    "/Pattern /{} has /PatternType {other:?}",
                    show_name(name)
                )))
            }
        }
        if dict.contains_key(b"ExtGState".as_slice()) {
            return Err(Error::Unsupported {
                feature: "shading pattern /ExtGState",
                detail: "a graphics-state override on a shading pattern is not implemented".into(),
            });
        }
        let sh_obj = dict
            .get(b"Shading".as_slice())
            .ok_or_else(|| Error::Structure("shading pattern has no /Shading".into()))?;
        let shading = parse_shading(self.doc, sh_obj, &self.resources, self.limits)?;
        if !self.extraction_only {
            if let Some(a) = &shading.approximation {
                if !self.color_approximations.contains(a) {
                    self.color_approximations.push(a.clone());
                }
            }
        }
        Ok(Rc::new(PatternPaint::Shading(Rc::new(ShadingPattern {
            shading: Rc::new(shading),
            matrix,
        }))))
    }

    /// `/PatternType 1` (Table 75): `/PaintType` 1 coloured / 2
    /// uncoloured, `/TilingType` 1–3 (spacing hints — the cells are
    /// placed exactly either way), `/BBox`, `/XStep`/`/YStep`
    /// (non-zero; the sign is irrelevant since tiles extend both ways),
    /// `/Resources`. The stream is kept and parsed when painted.
    fn load_tiling_pattern(
        &mut self,
        name: &[u8],
        obj: &Object,
        matrix: Matrix,
        space: &ColorSpace,
        comps: &[f64],
        entry_ref: Option<IndirectRef>,
    ) -> Result<TilingPattern, Error> {
        let (pdict, raw) = obj.as_stream().ok_or_else(|| {
            Error::Structure(format!(
                "tiling pattern /{} is not a stream",
                show_name(name)
            ))
        })?;
        let under = match pdict.get(b"PaintType".as_slice()).and_then(|o| o.as_i64()) {
            Some(1) => None,
            Some(2) => {
                let base = match space {
                    ColorSpace::Pattern { base: Some(b) } => b.clone(),
                    _ => {
                        return Err(Error::Structure(format!(
                            "uncoloured tiling pattern /{} needs a [/Pattern base] colour space",
                            show_name(name)
                        )))
                    }
                };
                if comps.len() != base.ncomps() {
                    return Err(Error::Parse(format!(
                        "uncoloured pattern /{} expects {} colour component(s) before the name, got {}",
                        show_name(name),
                        base.ncomps(),
                        comps.len()
                    )));
                }
                Some(color_in(&base, comps)?)
            }
            other => {
                return Err(Error::Structure(format!(
                    "tiling pattern /{} has /PaintType {other:?} (must be 1 or 2)",
                    show_name(name)
                )))
            }
        };
        match pdict.get(b"TilingType".as_slice()).and_then(|o| o.as_i64()) {
            None | Some(1..=3) => {}
            Some(other) => {
                return Err(Error::Structure(format!(
                    "tiling pattern /{} has /TilingType {other} (must be 1, 2 or 3)",
                    show_name(name)
                )))
            }
        }
        let bbox_obj = pdict
            .get(b"BBox".as_slice())
            .ok_or_else(|| Error::Structure("tiling pattern has no /BBox".into()))?;
        let bbox_obj = self.doc.deref(bbox_obj)?;
        let b = crate::page::Rect::from_object(&bbox_obj).ok_or_else(|| {
            Error::Structure("tiling pattern /BBox is not a 4-number rectangle".into())
        })?;
        if !rect_is_finite(&b) {
            return Err(Error::Structure(
                "tiling pattern /BBox has non-finite values".into(),
            ));
        }
        let bbox = crate::page::Rect {
            llx: b.llx.min(b.urx),
            lly: b.lly.min(b.ury),
            urx: b.llx.max(b.urx),
            ury: b.lly.max(b.ury),
        };
        let step = |key: &[u8]| -> Result<f64, Error> {
            let v = pdict.get(key).and_then(|o| o.as_number()).ok_or_else(|| {
                Error::Structure(format!(
                    "tiling pattern /{} is missing or not a number",
                    String::from_utf8_lossy(key)
                ))
            })?;
            if !v.is_finite() || v == 0.0 {
                return Err(Error::Structure(format!(
                    "tiling pattern /{} must be a non-zero finite number",
                    String::from_utf8_lossy(key)
                )));
            }
            Ok(v.abs())
        };
        let xstep = step(b"XStep")?;
        let ystep = step(b"YStep")?;
        let resources = match pdict.get(b"Resources".as_slice()) {
            None | Some(Object::Null) => Object::Dict(Dict::new()),
            Some(r) => self.doc.deref(r)?,
        };
        Ok(TilingPattern {
            dict: pdict.clone(),
            raw: raw.to_vec(),
            matrix,
            bbox,
            xstep,
            ystep,
            under,
            resources,
            entry_ref,
        })
    }

    /// Fill `path` with `pat`: the path (under the current clip)
    /// becomes the pattern's coverage mask; a shading is drawn in
    /// pattern space (`/Matrix` × the parent stream's base CTM —
    /// §8.7.3.1) with `/Background` where its geometry leaves the path
    /// unpainted (§8.7.4.3); a tiling pattern's cells are stamped
    /// across it.
    fn fill_path_with_pattern(
        &mut self,
        pat: Rc<PatternPaint>,
        path: &tiny_skia::Path,
        rule: FillRule,
        ts: Transform,
    ) -> Result<(), Error> {
        let mask = match &self.gs.clip {
            Some(m) => {
                let mut m = m.clone();
                m.intersect_path(path, rule, true, ts);
                m
            }
            None => {
                let mut m = Mask::new(self.pixmap.width(), self.pixmap.height())
                    .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?;
                m.fill_path(path, rule, true, ts);
                m
            }
        };
        self.paint_pattern_under_mask(&pat, mask)
    }

    /// Stroke `path` with the stroke pattern: the stroke outline is
    /// computed in user space (so line width, joins and dashes are in
    /// user units, §8.4.3) and filled with the pattern.
    fn stroke_path_with_pattern(
        &mut self,
        path: &tiny_skia::Path,
        stroke: &Stroke,
        ts: Transform,
    ) -> Result<(), Error> {
        let Some(pat) = self.gs.stroke_pattern.clone() else {
            return Ok(());
        };
        let scale = (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt().max(0.01);
        let Some(outline) = path.stroke(stroke, scale) else {
            return Ok(());
        };
        self.fill_path_with_pattern(pat, &outline, FillRule::Winding, ts)
    }

    /// Paint `pat` wherever `mask` has coverage (the mask already
    /// includes the clip).
    fn paint_pattern_under_mask(&mut self, pat: &PatternPaint, mask: Mask) -> Result<(), Error> {
        match pat {
            PatternPaint::Shading(sp) => {
                let to_page = sp.matrix.concat(self.pattern_base);
                let shading = sp.shading.clone();
                let saved = self.gs.clip.replace(mask);
                let r = self.paint_shading(&shading, &to_page, None, true);
                self.gs.clip = saved;
                r
            }
            PatternPaint::Tiling(tp) => self.paint_tiling(tp.clone(), mask),
        }
    }

    /// Paint tiling pattern `tp` under `mask`. Pattern space maps to
    /// the page through `/Matrix` × `pattern_base` (§8.7.3.1); tile
    /// (i, j) is the cell translated by (i·XStep, j·YStep) in pattern
    /// space, clipped to `/BBox`, and only the tiles that can touch the
    /// mask's device bounds are considered. Two strategies, one result:
    /// a fill with at most [`DIRECT_TILES`] cells — or whose `/BBox`
    /// exceeds the step, so tiles overlap — executes the cell content
    /// once per tile (bounded by `max_pattern_tiles`); a dense
    /// non-overlapping tiling renders one cell into an offscreen
    /// pixmap of its device size and stamps it with a repeating
    /// bilinear shader (cost per device pixel, so no tile bound).
    /// The fill alpha and blend mode apply once to the stamped result,
    /// as for a transparency group.
    fn paint_tiling(&mut self, tp: Rc<TilingPattern>, mask: Mask) -> Result<(), Error> {
        if self.pattern_depth >= MAX_PATTERN_DEPTH {
            return Err(Error::LimitExceeded {
                limit: "tiling pattern nesting depth",
                max: MAX_PATTERN_DEPTH as u64,
            });
        }
        if let Some(r) = tp.entry_ref {
            if self.pattern_stack.contains(&r) {
                return Err(Error::CycleDetected {
                    context: "tiling pattern cell painting with its own pattern",
                });
            }
        }
        let Some(dev) = mask_bounds(&mask) else {
            return Ok(());
        };
        let to_page = tp.matrix.concat(self.pattern_base);
        if !to_page.is_finite() {
            return Err(Error::Structure(
                "tiling pattern matrix is not finite".into(),
            ));
        }
        let pat_ts = Transform::from_row(
            to_page.a as f32,
            to_page.b as f32,
            to_page.c as f32,
            to_page.d as f32,
            to_page.e as f32,
            to_page.f as f32,
        )
        .post_concat(self.user_to_pix);
        let Some(inv) = pat_ts.invert() else {
            return Err(Error::Unsupported {
                feature: "singular CTM (determinant 0)",
                detail: "tiling pattern under a degenerate transform is not supported".into(),
            });
        };
        // Pattern-space bounds of the device box to cover.
        let (mut px0, mut py0, mut px1, mut py1) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for (x, y) in [
            (dev.0, dev.1),
            (dev.2 + 1, dev.1),
            (dev.0, dev.3 + 1),
            (dev.2 + 1, dev.3 + 1),
        ] {
            let mut p = tiny_skia::Point {
                x: x as f32,
                y: y as f32,
            };
            inv.map_point(&mut p);
            px0 = px0.min(p.x);
            py0 = py0.min(p.y);
            px1 = px1.max(p.x);
            py1 = py1.max(p.y);
        }
        if !(px0.is_finite() && py0.is_finite() && px1.is_finite() && py1.is_finite()) {
            return Err(Error::Structure(
                "tiling pattern transform overflows pattern space".into(),
            ));
        }
        let bb = tp.bbox;
        let i0 = ((px0 as f64 - bb.urx) / tp.xstep).floor();
        let i1 = ((px1 as f64 - bb.llx) / tp.xstep).ceil();
        let j0 = ((py0 as f64 - bb.ury) / tp.ystep).floor();
        let j1 = ((py1 as f64 - bb.lly) / tp.ystep).ceil();
        let tiles = (i1 - i0 + 1.0).max(0.0) * (j1 - j0 + 1.0).max(0.0);
        if !tiles.is_finite() || tiles > u64::MAX as f64 / 4.0 {
            return Err(Error::Structure(
                "tiling pattern tile count overflows".into(),
            ));
        }
        let cell_w = (pat_ts.sx * tp.xstep as f32).hypot(pat_ts.ky * tp.xstep as f32);
        let cell_h = (pat_ts.kx * tp.ystep as f32).hypot(pat_ts.sy * tp.ystep as f32);
        let overlap =
            bb.urx - bb.llx > tp.xstep * (1.0 + 1e-6) || bb.ury - bb.lly > tp.ystep * (1.0 + 1e-6);
        let cached_ok = !overlap
            && cell_w.is_finite()
            && cell_h.is_finite()
            && cell_w >= 1e-3
            && cell_h >= 1e-3;
        let direct = !cached_ok || tiles <= DIRECT_TILES as f64;
        if direct {
            let n = tiles as u64;
            if n > self.limits.max_pattern_tiles
                || self.pattern_tiles.saturating_add(n) > self.limits.max_pattern_tiles
            {
                return Err(Error::LimitExceeded {
                    limit: "max_pattern_tiles",
                    max: self.limits.max_pattern_tiles,
                });
            }
            self.pattern_tiles += n;
        }
        let decoded = self.doc.decode_stream(&tp.dict, &tp.raw)?;
        let ops = crate::content::parse_content_stream(&[decoded], self.limits)?;
        let alpha = (self.gs.fill_alpha, self.gs.stroke_alpha, self.gs.blend_mode);
        let saved = self.enter_cell(&tp);
        let r = if direct {
            self.paint_tiles_direct(
                &tp,
                &ops,
                &to_page,
                &mask,
                [i0 as i64, i1 as i64, j0 as i64, j1 as i64],
                dev,
                alpha,
            )
        } else {
            self.paint_tiles_cached(&tp, &ops, pat_ts, &mask, cell_w, cell_h, dev, alpha)
        };
        self.leave_cell(saved);
        r
    }

    /// Swap in the interpreter state a pattern cell runs under
    /// (§8.7.3.1: the cell's own resources, pattern space as its base,
    /// no text object, no path in progress).
    fn enter_cell(&mut self, tp: &TilingPattern) -> CellSaved {
        let pushed_ref = match tp.entry_ref {
            Some(r) => {
                self.pattern_stack.push(r);
                true
            }
            None => false,
        };
        self.pattern_depth += 1;
        let uncolored = tp.under.is_some();
        if uncolored {
            self.uncolored_depth += 1;
        }
        CellSaved {
            gs: std::mem::replace(&mut self.gs, GraphicsState::new()),
            gs_stack: std::mem::take(&mut self.gs_stack),
            resources: self.swap_resources(tp.resources.clone()),
            pattern_base: self.pattern_base,
            path: std::mem::take(&mut self.path),
            in_text: std::mem::replace(&mut self.in_text, false),
            text_clip: self.text_clip.take(),
            text_tm: std::mem::replace(&mut self.text_tm, Matrix::IDENTITY),
            text_tlm: std::mem::replace(&mut self.text_tlm, Matrix::IDENTITY),
            pushed_ref,
            uncolored,
        }
    }

    fn leave_cell(&mut self, saved: CellSaved) {
        self.gs = saved.gs;
        self.gs_stack = saved.gs_stack;
        self.swap_resources(saved.resources);
        self.pattern_base = saved.pattern_base;
        self.path = saved.path;
        self.in_text = saved.in_text;
        self.text_clip = saved.text_clip;
        self.text_tm = saved.text_tm;
        self.text_tlm = saved.text_tlm;
        if saved.pushed_ref {
            self.pattern_stack.pop();
        }
        self.pattern_depth -= 1;
        if saved.uncolored {
            self.uncolored_depth -= 1;
        }
    }

    /// Fresh graphics state for one cell: `ctm` as the CTM, the outer
    /// fill colour for an uncoloured pattern, `clip` as the clip.
    fn reset_cell_state(&mut self, tp: &TilingPattern, ctm: Matrix, clip: Option<Mask>) {
        self.gs = GraphicsState::new();
        self.gs.ctm = ctm;
        if let Some(u) = tp.under {
            self.gs.fill_color = u;
            self.gs.stroke_color = u;
        }
        self.gs.clip = clip;
        self.gs_stack.clear();
        self.pattern_base = ctm;
        self.in_text = false;
        self.text_tm = Matrix::IDENTITY;
        self.text_tlm = Matrix::IDENTITY;
        self.path = CurrentPath::default();
    }

    /// Execute the cell content once per tile, each under `mask` ∩
    /// its translated `/BBox`.
    #[allow(clippy::too_many_arguments)]
    fn paint_tiles_direct(
        &mut self,
        tp: &TilingPattern,
        ops: &[Operation],
        to_page: &Matrix,
        mask: &Mask,
        range: [i64; 4],
        dev: (i32, i32, i32, i32),
        alpha: (f64, f64, tiny_skia::BlendMode),
    ) -> Result<(), Error> {
        let bb = tp.bbox;
        for j in range[2]..=range[3] {
            for i in range[0]..=range[1] {
                let cell =
                    Matrix::new(1.0, 0.0, 0.0, 1.0, i as f64 * tp.xstep, j as f64 * tp.ystep)
                        .concat(*to_page);
                // Cull cells whose /BBox misses the device box.
                let ts = Transform::from_row(
                    cell.a as f32,
                    cell.b as f32,
                    cell.c as f32,
                    cell.d as f32,
                    cell.e as f32,
                    cell.f as f32,
                )
                .post_concat(self.user_to_pix);
                let (mut cx0, mut cy0, mut cx1, mut cy1) = (
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                );
                for (x, y) in [
                    (bb.llx, bb.lly),
                    (bb.urx, bb.lly),
                    (bb.llx, bb.ury),
                    (bb.urx, bb.ury),
                ] {
                    let mut p = tiny_skia::Point {
                        x: x as f32,
                        y: y as f32,
                    };
                    ts.map_point(&mut p);
                    cx0 = cx0.min(p.x);
                    cy0 = cy0.min(p.y);
                    cx1 = cx1.max(p.x);
                    cy1 = cy1.max(p.y);
                }
                if cx1 < dev.0 as f32
                    || cx0 > (dev.2 + 1) as f32
                    || cy1 < dev.1 as f32
                    || cy0 > (dev.3 + 1) as f32
                {
                    continue;
                }
                self.reset_cell_state(tp, cell, Some(mask.clone()));
                self.gs.fill_alpha = alpha.0;
                self.gs.stroke_alpha = alpha.1;
                self.gs.blend_mode = alpha.2;
                self.intersect_clip_rect(&bb)?;
                self.run(ops)?;
            }
        }
        Ok(())
    }

    /// Render one cell into a pixmap of its device size and stamp it
    /// across the mask with a repeating shader.
    #[allow(clippy::too_many_arguments)]
    fn paint_tiles_cached(
        &mut self,
        tp: &TilingPattern,
        ops: &[Operation],
        pat_ts: Transform,
        mask: &Mask,
        cell_w: f32,
        cell_h: f32,
        dev: (i32, i32, i32, i32),
        alpha: (f64, f64, tiny_skia::BlendMode),
    ) -> Result<(), Error> {
        let cw = (cell_w.ceil() as u32).clamp(1, self.limits.max_render_pixels);
        let ch = (cell_h.ceil() as u32).clamp(1, self.limits.max_render_pixels);
        let bytes = cw as u64 * ch as u64 * 4;
        let page_bytes = self.pixmap.width() as u64 * self.pixmap.height() as u64 * 4;
        if self
            .layer_bytes
            .saturating_add(page_bytes)
            .saturating_add(bytes)
            > self.limits.max_render_buffer_bytes
        {
            return Err(Error::LimitExceeded {
                limit: "max_render_buffer_bytes",
                max: self.limits.max_render_buffer_bytes,
            });
        }
        let mut cell = Pixmap::new(cw, ch)
            .ok_or_else(|| Error::Structure("pattern cell allocation failed".into()))?;
        // Pattern space → cell pixels: one step along each axis fills
        // the cell exactly; (llx, lly) sits at the origin.
        let sx = cw as f32 / tp.xstep as f32;
        let sy = ch as f32 / tp.ystep as f32;
        let cell_ts = Transform::from_row(
            sx,
            0.0,
            0.0,
            sy,
            -(tp.bbox.llx as f32) * sx,
            -(tp.bbox.lly as f32) * sy,
        );
        std::mem::swap(&mut *self.pixmap, &mut cell);
        let saved_u2p = std::mem::replace(&mut self.user_to_pix, cell_ts);
        self.layer_bytes += bytes;
        let r = (|| {
            // Alpha and blend are reset inside the cell and applied once
            // when the tiles are stamped.
            self.reset_cell_state(tp, Matrix::IDENTITY, None);
            self.intersect_clip_rect(&tp.bbox)?;
            self.run(ops)
        })();
        self.layer_bytes -= bytes;
        self.user_to_pix = saved_u2p;
        std::mem::swap(&mut *self.pixmap, &mut cell);
        r?;
        let Some(cell_inv) = cell_ts.invert() else {
            return Ok(());
        };
        let shader_ts = cell_inv.post_concat(pat_ts);
        let paint = Paint {
            shader: tiny_skia::Pattern::new(
                cell.as_ref(),
                tiny_skia::SpreadMode::Repeat,
                tiny_skia::FilterQuality::Bilinear,
                clamp01(alpha.0) as f32,
                shader_ts,
            ),
            blend_mode: alpha.2,
            anti_alias: false,
            ..Default::default()
        };
        let Some(rect) = tiny_skia::Rect::from_ltrb(
            dev.0 as f32,
            dev.1 as f32,
            (dev.2 + 1) as f32,
            (dev.3 + 1) as f32,
        ) else {
            return Ok(());
        };
        self.pixmap
            .fill_rect(rect, &paint, Transform::identity(), Some(mask));
        Ok(())
    }

    /// Rasterise `sh` whose space maps to *page* space through
    /// `to_page`, over the current clip (and `extra`, multiplied in),
    /// with the fill alpha and blend mode. Each device pixel's centre
    /// is inverse-mapped to shading space and coloured by the
    /// parametric position; `with_background` paints `/Background`
    /// where the geometry does not reach.
    fn paint_shading(
        &mut self,
        sh: &Shading,
        to_page: &Matrix,
        extra: Option<&Mask>,
        with_background: bool,
    ) -> Result<(), Error> {
        if !to_page.is_finite() {
            return Err(Error::Structure("shading matrix is not finite".into()));
        }
        let det = to_page.a * to_page.d - to_page.b * to_page.c;
        if det == 0.0 {
            return Err(Error::Unsupported {
                feature: "singular CTM (determinant 0)",
                detail: "shading under a degenerate transform is not supported".into(),
            });
        }
        let ts = Transform::from_row(
            to_page.a as f32,
            to_page.b as f32,
            to_page.c as f32,
            to_page.d as f32,
            to_page.e as f32,
            to_page.f as f32,
        )
        .post_concat(self.user_to_pix);
        let Some(inv) = ts.invert() else {
            return Err(Error::Unsupported {
                feature: "singular CTM (determinant 0)",
                detail: "shading transform is not invertible".into(),
            });
        };
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        // Device bounds: the page, narrowed to /BBox when present.
        let (mut x0, mut y0, mut x1, mut y1) = (0i32, 0i32, pw, ph);
        if let Some(bb) = &sh.bbox {
            let corners = [
                (bb.llx as f32, bb.lly as f32),
                (bb.urx as f32, bb.lly as f32),
                (bb.llx as f32, bb.ury as f32),
                (bb.urx as f32, bb.ury as f32),
            ];
            let (mut minx, mut miny, mut maxx, mut maxy) = (
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            );
            for (x, y) in corners {
                let mut p = tiny_skia::Point { x, y };
                ts.map_point(&mut p);
                minx = minx.min(p.x);
                miny = miny.min(p.y);
                maxx = maxx.max(p.x);
                maxy = maxy.max(p.y);
            }
            x0 = x0.max(minx.floor() as i32);
            y0 = y0.max(miny.floor() as i32);
            x1 = x1.min(maxx.ceil() as i32);
            y1 = y1.min(maxy.ceil() as i32);
        }
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let alpha = clamp01(self.gs.fill_alpha);
        let bm = self.gs.blend_mode;
        let bg = if with_background { sh.background } else { None };
        // Mesh shadings are rasterised as a whole in device space and
        // read back per pixel; the other kinds are evaluated per pixel.
        let raster = match sh.mesh() {
            Some(m) => {
                let map = |x: f64, y: f64| {
                    let mut p = tiny_skia::Point {
                        x: x as f32,
                        y: y as f32,
                    };
                    ts.map_point(&mut p);
                    (p.x as f64, p.y as f64)
                };
                let r = m.rasterize(&map, sh.lut(), (x0, y0, x1, y1), self.limits)?;
                if bg.is_none() {
                    x0 = x0.max(r.x0);
                    y0 = y0.max(r.y0);
                    x1 = x1.min(r.x0 + r.w as i32);
                    y1 = y1.min(r.y0 + r.h as i32);
                    if x0 >= x1 || y0 >= y1 {
                        return Ok(());
                    }
                }
                Some(r)
            }
            None => None,
        };
        let clip = self.gs.clip.take();
        for py in y0..y1 {
            for px in x0..x1 {
                let idx = (py as u64 * pw as u64 + px as u64) as usize;
                let mut coverage = match &clip {
                    Some(m) => m.data().get(idx).copied().unwrap_or(0) as f64 / 255.0,
                    None => 1.0,
                };
                if let Some(e) = extra {
                    coverage *= e.data().get(idx).copied().unwrap_or(0) as f64 / 255.0;
                }
                if coverage <= 0.0 {
                    continue;
                }
                let mut p = tiny_skia::Point {
                    x: px as f32 + 0.5,
                    y: py as f32 + 0.5,
                };
                inv.map_point(&mut p);
                let (x, y) = (p.x as f64, p.y as f64);
                if let Some(bb) = &sh.bbox {
                    if x < bb.llx.min(bb.urx)
                        || x > bb.llx.max(bb.urx)
                        || y < bb.lly.min(bb.ury)
                        || y > bb.lly.max(bb.ury)
                    {
                        continue;
                    }
                }
                let painted = match &raster {
                    Some(r) => r.get(px, py),
                    None => sh.rgb_at(x, y),
                };
                let rgb8 = match painted {
                    Some(c) => c,
                    None => match bg {
                        Some(b) => b,
                        None => continue,
                    },
                };
                let rgb = [
                    rgb8[0] as f64 / 255.0,
                    rgb8[1] as f64 / 255.0,
                    rgb8[2] as f64 / 255.0,
                ];
                let i = idx * 4;
                let buf = self.pixmap.data_mut();
                composite_px(&mut buf[i..i + 4], rgb, alpha * coverage, bm);
            }
        }
        self.gs.clip = clip;
        Ok(())
    }

    /// Composite a finished transparency-group layer (premultiplied,
    /// same size as the page pixmap) onto the current pixmap with the
    /// group's constant alpha and blend mode (§11.4.7, §11.6.6).
    fn composite_layer(
        &mut self,
        layer: &Pixmap,
        alpha: f64,
        bm: tiny_skia::BlendMode,
        mask: Option<&Mask>,
    ) {
        let alpha = clamp01(alpha);
        let src = layer.data();
        let dst = self.pixmap.data_mut();
        for (i, (d, s)) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(src.as_chunks::<4>().0)
            .enumerate()
        {
            let mut la = s[3] as f64 / 255.0;
            if let Some(m) = mask {
                la *= m.data().get(i).copied().unwrap_or(0) as f64 / 255.0;
            }
            if la == 0.0 {
                continue;
            }
            let rgb = [
                s[0] as f64 / 255.0 / la,
                s[1] as f64 / 255.0 / la,
                s[2] as f64 / 255.0 / la,
            ];
            composite_px(d, rgb, la * alpha, bm);
        }
    }

    // ----------------------------------------------------------------
    // Text rendering (§9.4)
    // ----------------------------------------------------------------

    /// `Tj string`: paint each byte's glyph at the current text
    /// position, advancing `Tm` per §9.4.4.
    fn show_text(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if !self.in_text {
            return Err(Error::Structure("Tj outside a text object".into()));
        }
        let font = self
            .gs
            .text
            .font
            .clone()
            .ok_or_else(|| Error::Structure("Tj before Tf — no font selected".into()))?;
        if !(self.gs.text.font_size.is_finite() && self.gs.text.font_size > 0.0) {
            return Err(Error::Structure("Tj before Tf — no font size".into()));
        }
        let mut pos = 0;
        while pos < bytes.len() {
            let (code, len) = font.read_code(bytes, pos)?;
            pos += len;
            self.show_glyph(&font, code, 0.0)?;
        }
        Ok(())
    }

    /// `TJ [item …]`: strings and numeric adjustments interleaved.
    /// A number shifts the *next* glyph — the sign is such that a
    /// positive number moves left (§9.4.3). Codes never span array
    /// items — a 2-byte code cut by a `TJ` boundary is a structure
    /// error, never silently joined.
    fn show_text_adjusted(&mut self, items: &[crate::content::TJItem]) -> Result<(), Error> {
        if !self.in_text {
            return Err(Error::Structure("TJ outside a text object".into()));
        }
        let font = self
            .gs
            .text
            .font
            .clone()
            .ok_or_else(|| Error::Structure("TJ before Tf — no font selected".into()))?;
        if !(self.gs.text.font_size.is_finite() && self.gs.text.font_size > 0.0) {
            return Err(Error::Structure("TJ before Tf — no font size".into()));
        }
        for item in items {
            match item {
                crate::content::TJItem::Text(bytes) => {
                    let mut pos = 0;
                    while pos < bytes.len() {
                        let (code, len) = font.read_code(bytes, pos)?;
                        pos += len;
                        self.show_glyph(&font, code, 0.0)?;
                    }
                }
                crate::content::TJItem::Kern(n) => {
                    // A number shifts the *next* glyph: §9.4.4's tx
                    // term is −Tj/1000 in glyph-space (1/1000-em)
                    // units — mapped through Trm it scales by Tfs·Th
                    // before landing on Tm's page-space e. We keep
                    // Tm's e in the same units a Td operand uses
                    // (unscaled), so the shift added here must be
                    // pre-multiplied by Tfs·Th — matching the
                    // glyph-advance path in show_glyph.
                    let t = &self.gs.text;
                    let tx = -*n / 1000.0 * t.font_size * t.h_scale;
                    let t = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
                    self.text_tm = t.concat(self.text_tm);
                }
            }
        }
        Ok(())
    }

    /// Paint a single char code's glyph at the current Tm origin,
    /// then advance Tm per §9.4.4:
    ///   tx = ((w0 − Tj/1000) × Tfs + Tc + Tw) × Th
    /// where w0 is the glyph's PDF width (from `/Widths`), `Tj` the
    /// TJ-array number for this glyph (0 for plain `Tj`), `Tw` applies
    /// only to the space character.
    fn show_glyph(
        &mut self,
        font: &crate::font::Font,
        code: u32,
        tj_adjust: f64,
    ) -> Result<(), Error> {
        // Cumulative per-page budget for character codes — a long
        // string, many strings, and Form-embedded text all count
        // here. Checked before any glyph work so even .notdef and
        // outline-less glyphs are accounted for.
        self.text_codes = self.text_codes.checked_add(1).ok_or(Error::LimitExceeded {
            limit: "max_text_codes",
            max: self.limits.max_text_codes,
        })?;
        if self.text_codes > self.limits.max_text_codes {
            return Err(Error::LimitExceeded {
                limit: "max_text_codes",
                max: self.limits.max_text_codes,
            });
        }

        // Type3 glyphs are CharProc *content streams*, not outline
        // programs — a completely different paint path. Hand them to a
        // dedicated executor and skip the outline machinery entirely.
        if font.as_type3().is_some() {
            return self.show_type3_glyph(font, code, tj_adjust);
        }

        let record = self.hidden == 0;
        let actual = self.take_actual_text();
        // A glyph filled with a /Pattern colour is deferred past the
        // `gs` borrow below.
        let mut pattern_fill: Option<(tiny_skia::Path, Transform)> = None;
        let mut pattern_stroke: Option<(tiny_skia::Path, Transform, Stroke)> = None;
        let gs = &mut self.gs;
        let t = &mut gs.text;

        // Resolve code → glyph.
        let gid = font.glyph_id(code)?;
        // Vertical writing (§9.7.4.3): the glyph is positioned by its
        // vertical origin `v` (vx, vy — 1/1000 em) relative to the
        // horizontal one, and the advance is `w1` along −y.
        let vertical = font.vertical();
        let (w1y, vx, vy) = if vertical {
            font.vertical_metrics(code)
        } else {
            (0.0, 0.0, 0.0)
        };
        let vshift = Matrix::new(1.0, 0.0, 0.0, 1.0, -vx / 1000.0, -vy / 1000.0);

        // Build the glyph path in glyph space, then transform to
        // device space via:
        //   glyph → text:   scale(1/1000 em)
        //   text → render:  Trm = [Tfs·Th 0 0 Tfs 0 Trise] × Tm
        //   render → page:  CTM
        //   page → pixmap:  user_to_pix
        // Glyph outlines serve two purposes: painting, and the
        // per-glyph `bounds` reported by text extraction. Extraction
        // still wants real bounds, so attempt the outline there too —
        // but a font that *cannot* produce one (non-embedded Type1,
        // metrics-only) must not fail extraction: it just reports
        // `bounds: None`. In render mode the same refusal propagates.
        let units = font.units_per_em()?;
        let mut gp = crate::font::GlyphPath::new(units);
        let has_outline = match font.outline(gid, &mut gp) {
            Ok(h) => h,
            Err(e) if self.extraction_only => {
                // Tolerated only when not painting — a metrics-only
                // font yields no ink shape, so bounds degrade to None.
                if matches!(e, Error::Unsupported { .. }) {
                    false
                } else {
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        };
        // The finished path is consumed by painting or by the bounds
        // computation below — exactly one of the two per call.
        let glyph_path = if has_outline { gp.finish() } else { None };

        if t.render_mode >= 4 && !self.extraction_only && self.hidden == 0 {
            // A glyph shown in a clipping mode marks the mode as used
            // even when it has no outline (a space): the clip at ET is
            // then the union of whatever outlines were added — empty
            // when none were.
            self.text_clip.get_or_insert_with(PathBuilder::new);
        }
        if let Some(path) = &glyph_path {
            if !self.extraction_only && self.hidden == 0 {
                // Text→render matrix (§9.4.4 note 2).
                let trm = Matrix::new(t.font_size * t.h_scale, 0.0, 0.0, t.font_size, 0.0, t.rise)
                    .concat(self.text_tm)
                    .concat(gs.ctm);
                let trm = if vertical { vshift.concat(trm) } else { trm };
                let trm_ts = Transform::from_row(
                    trm.a as f32,
                    trm.b as f32,
                    trm.c as f32,
                    trm.d as f32,
                    trm.e as f32,
                    trm.f as f32,
                );
                // tiny-skia `post_concat` composes `other` *around*
                // self: `a.post_concat(b)` builds `b × a` — `b` runs
                // first for points. Pipeline order glyph → Trm → page
                // → pixmap therefore wants `trm.post_concat(upx)`.
                let full = trm_ts.post_concat(self.user_to_pix);
                match t.render_mode {
                    0 | 2 | 4 | 6 if gs.fill_space.is_pattern() => {
                        // Filled with the pattern once the gs borrow ends.
                        pattern_fill = Some((path.clone(), full));
                    }
                    0 | 2 | 4 | 6 => {
                        // fill (modes 2 and 6 also stroke below)
                        let mut p = Paint {
                            anti_alias: true,
                            blend_mode: gs.blend_mode,
                            ..Default::default()
                        };
                        let c = gs.fill_color.with_alpha(gs.fill_alpha);
                        p.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a));
                        self.pixmap
                            .fill_path(path, &p, FillRule::Winding, full, gs.clip.as_ref());
                    }
                    _ => {}
                }
                if matches!(t.render_mode, 1 | 2 | 5 | 6) {
                    // stroke
                    let stroke = tiny_skia::Stroke {
                        width: (gs.line_width * t.font_size / 1000.0) as f32,
                        ..Default::default()
                    };
                    if gs.stroke_space.is_pattern() {
                        pattern_stroke = Some((path.clone(), full, stroke));
                    } else {
                        let mut p = Paint {
                            anti_alias: true,
                            blend_mode: gs.blend_mode,
                            ..Default::default()
                        };
                        let c = gs.stroke_color.with_alpha(gs.stroke_alpha);
                        p.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a));
                        self.pixmap
                            .stroke_path(path, &p, &stroke, full, gs.clip.as_ref());
                    }
                }
                // mode 3: invisible — paint nothing, still advance.
                if t.render_mode >= 4 {
                    // Clipping modes 4–7: the outline joins the text
                    // clip (device space) applied at ET.
                    if let Some(dev) = path.clone().transform(full) {
                        self.text_clip
                            .get_or_insert_with(PathBuilder::new)
                            .push_path(&dev);
                    }
                }
            }
        }
        // Record the extraction event before advancing — the char's
        // origin is the *current* Tm translation, and its advance is
        // what we are about to add.
        if let Some(buf) = self.extract.as_mut().filter(|_| record) {
            let w0 = font.width(code);
            let tw = if font.is_space_code(code) {
                t.word_spacing
            } else {
                0.0
            };
            // §9.4.4's tx mapped into Trm's glyph-space units. The
            // advance matrix applies ×Tfs·Th, so unscaled spacing
            // (Tc, Tw — which join tx *after* the ×Tfs) must be
            // divided by Tfs here to land at the right page-space
            // distance:
            //   page_tx = (w0/1000 − Tj/1000)·Tfs·Th + (Tc + Tw)·Th
            //           = (w0/1000 − Tj/1000 + (Tc + Tw)/Tfs)·Tfs·Th
            let tx_text = w0 / 1000.0 - tj_adjust / 1000.0 + (t.char_spacing + tw) / t.font_size;
            let trm = Matrix::new(t.font_size * t.h_scale, 0.0, 0.0, t.font_size, 0.0, t.rise)
                .concat(self.text_tm)
                .concat(gs.ctm);
            let origin = trm.transform(0.0, 0.0);
            let (ax, ay) = if vertical {
                // ty = (w1 − Tj/1000)·Tfs + Tc + Tw, no horizontal scale.
                let ty_text =
                    w1y / 1000.0 - tj_adjust / 1000.0 + (t.char_spacing + tw) / t.font_size;
                let unscaled = Matrix::new(t.font_size, 0.0, 0.0, t.font_size, 0.0, t.rise)
                    .concat(self.text_tm)
                    .concat(gs.ctm);
                unscaled.transform(0.0, ty_text)
            } else {
                trm.transform(tx_text, 0.0)
            };
            let advance = (ax - origin.0, ay - origin.1);
            let trm = if vertical { vshift.concat(trm) } else { trm };
            // Glyph bounds: the outline's user-space axis-aligned bbox,
            // or None if the glyph has no outline (e.g. space). Reuses
            // the outline already built above — a second parse of the
            // glyph program would only duplicate work.
            let bounds = glyph_path.as_ref().map(|p| {
                let b = p.bounds();
                let (x0, y0) = trm.transform(b.left() as f64, b.top() as f64);
                let (x1, y1) = trm.transform(b.right() as f64, b.bottom() as f64);
                let (x2, y2) = trm.transform(b.left() as f64, b.bottom() as f64);
                let (x3, y3) = trm.transform(b.right() as f64, b.top() as f64);
                (
                    x0.min(x1).min(x2).min(x3),
                    y0.min(y1).min(y2).min(y3),
                    x0.max(x1).max(x2).max(x3),
                    y0.max(y1).max(y2).max(y3),
                )
            });
            let (unicode, resolved_via) = crate::text_extract::resolve_unicode(font, code);
            // §14.9.4: inside an /ActualText span the replacement text
            // stands for the shown characters.
            let (unicode, resolved_via) = match &actual {
                Some(t) => (
                    Some(t.clone()),
                    crate::text_extract::ResolveKind::ActualText,
                ),
                None => (unicode, resolved_via),
            };
            if let Some(s) = &unicode {
                self.extract_unicode_bytes = self
                    .extract_unicode_bytes
                    .checked_add(s.len() as u64)
                    .ok_or(Error::LimitExceeded {
                        limit: "max_extracted_unicode_bytes",
                        max: self.limits.max_extracted_unicode_bytes,
                    })?;
                if self.extract_unicode_bytes > self.limits.max_extracted_unicode_bytes {
                    return Err(Error::LimitExceeded {
                        limit: "max_extracted_unicode_bytes",
                        max: self.limits.max_extracted_unicode_bytes,
                    });
                }
            }
            buf.push(crate::text_extract::CharInfo {
                code: crate::font::unpack_code_for_font(font, code),
                unicode,
                resolved_via,
                origin,
                advance,
                bounds,
                render_mode: t.render_mode as u8,
                font_size: t.font_size,
                rise: t.rise,
                tj_adjust,
                in_text: true,
            });
        }

        // Advance the text matrix (§9.4.4). w0 is the char's width in
        // 1/1000-em units → glyph displacement in text space is
        // w0/1000 × Tfs (plus spacing) × Th.
        let w0 = font.width(code);
        let tw = if font.is_space_code(code) {
            t.word_spacing
        } else {
            0.0
        };
        let adv = if vertical {
            let ty = (w1y / 1000.0 - tj_adjust / 1000.0) * t.font_size + t.char_spacing + tw;
            Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, ty)
        } else {
            let tx = ((w0 / 1000.0 - tj_adjust / 1000.0) * t.font_size + t.char_spacing + tw)
                * t.h_scale;
            Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0)
        };
        self.text_tm = adv.concat(self.text_tm);
        // Paint a pattern-filled glyph now that the text-state borrow
        // is over (the path and its transform were captured above).
        if let Some((p, full)) = pattern_fill {
            if let Some(pat) = self.gs.fill_pattern.clone() {
                self.fill_path_with_pattern(pat, &p, FillRule::Winding, full)?;
            }
        }
        if let Some((p, full, stroke)) = pattern_stroke {
            self.stroke_path_with_pattern(&p, &stroke, full)?;
        }
        Ok(())
    }

    /// Show a `/Type3` glyph — execute its `/CharProcs` content stream
    /// like a miniature Form XObject (§9.6.4).
    ///
    /// The CharProc paints in *glyph space*, transformed into text
    /// space by `/FontMatrix` and then by the same text-rendering
    /// matrix the outline path uses:
    ///   glyph → text:    /FontMatrix
    ///   text → render:   Trm = [Tfs·Th 0 0 Tfs 0 Trise] × Tm
    ///   render → page:   CTM
    /// So the CharProc runs under `ctm × trm × font_matrix`.
    ///
    /// `d0`/`d1` inside the stream set the glyph's width/bbox — we take
    /// the authoritative advance from `/Widths` regardless (§9.6.4.1
    /// says they must agree; the font dict wins on conflict) and use
    /// `d1`'s bbox (or the font's `/FontBBox`) for extraction bounds.
    fn show_type3_glyph(
        &mut self,
        font: &crate::font::Font,
        code: u32,
        tj_adjust: f64,
    ) -> Result<(), Error> {
        let t3 = font.as_type3().expect("checked by caller");
        let font_matrix = t3.font_matrix();

        // Copy the text-state scalars we need up front — `self.gs` is
        // mutated below (pushed/run/restored), so a live &mut borrow of
        // `gs.text` can't be held across the CharProc execution.
        let font_size = self.gs.text.font_size;
        let h_scale = self.gs.text.h_scale;
        let rise = self.gs.text.rise;
        let render_mode = self.gs.text.render_mode;
        if render_mode >= 4 && !self.extraction_only {
            // A CharProc paints arbitrary content, not an outline;
            // there is no glyph shape to clip with (§9.3.6 leaves it
            // to the implementation, so refuse rather than guess).
            return Err(Error::Unsupported {
                feature: "text rendering mode",
                detail: format!("Tr {render_mode} (clipping) with a Type 3 font"),
            });
        }
        let char_spacing = self.gs.text.char_spacing;
        let word_spacing = self.gs.text.word_spacing;

        // Resolve the CharProc for this code (encoding → name → stream).
        let charproc = t3.charproc(self.doc, code as u8)?;

        // Text→render matrix, identical to the outline path's Trm.
        let trm = Matrix::new(font_size * h_scale, 0.0, 0.0, font_size, 0.0, rise)
            .concat(self.text_tm)
            .concat(self.gs.ctm);

        // The transform under which the CharProc's glyph-space
        // coordinates land on the page. `font_matrix` maps glyph →
        // text space and is applied *first*; `trm` then maps text →
        // page (§9.6.5: glyph → text → device). `concat` applies
        // `self` before its argument, so the order here is
        // `font_matrix` then `trm` — composing them the other way
        // round scales the Tm/CTM translation by the FontMatrix and
        // draws every glyph at the page origin.
        let glyph_ctm = font_matrix.concat(trm);

        // ---- paint the CharProc -----------------------------------
        if let Some((cdict, craw)) = charproc {
            if !self.extraction_only && self.hidden == 0 {
                // Bound nesting — a Type3 CharProc may itself contain a
                // `Tf` + `Tj` invoking another (or the same) Type3
                // font. Count it against the XObject depth budget so a
                // self-referential CharProc can't recurse forever.
                if self.xobject_depth >= self.limits.max_xobject_depth {
                    return Err(Error::LimitExceeded {
                        limit: "max_xobject_depth",
                        max: self.limits.max_xobject_depth as u64,
                    });
                }
                if self.gs_stack.len() >= self.limits.max_gsave_depth {
                    return Err(Error::LimitExceeded {
                        limit: "max_gsave_depth",
                        max: self.limits.max_gsave_depth as u64,
                    });
                }
                self.gs_stack.push(self.gs.clone());
                let saved_resources = match t3.resources() {
                    Some(r) => self.swap_resources(r.clone()),
                    None => self.resources.clone(), // inherit caller's
                };
                self.gs.ctm = glyph_ctm;
                let saved_pattern_base = std::mem::replace(&mut self.pattern_base, glyph_ctm);
                // A CharProc is its own scope: it may legitimately
                // contain `BT … ET` (nested Type3, Type3-in-Type3), so
                // the parent's text state must not leak in. Save and
                // clear `in_text` for the nested run, restore after.
                let saved_in_text = self.in_text;
                let saved_text_clip = self.text_clip.take();
                self.in_text = false;
                let run_result = (|| {
                    let decoded = self.doc.decode_stream(&cdict, &craw)?;
                    let ops = crate::content::parse_content_stream(&[decoded], self.limits)?;
                    self.xobject_depth += 1;
                    let r = self.run(&ops);
                    self.xobject_depth -= 1;
                    r
                })();
                self.in_text = saved_in_text;
                self.text_clip = saved_text_clip;
                self.pattern_base = saved_pattern_base;
                self.swap_resources(saved_resources);
                self.gs = self.gs_stack.pop().expect("pushed above");
                run_result?;
            }
        }

        // ---- extraction record -------------------------------------
        let record = self.hidden == 0;
        let actual = self.take_actual_text();
        if let Some(buf) = self.extract.as_mut().filter(|_| record) {
            let w0 = font.width(code);
            let tw = if font.is_space_code(code) {
                word_spacing
            } else {
                0.0
            };
            let tx_text = w0 / 1000.0 - tj_adjust / 1000.0 + (char_spacing + tw) / font_size;
            let origin = trm.transform(0.0, 0.0);
            let (ax, ay) = trm.transform(tx_text, 0.0);
            let advance = (ax - origin.0, ay - origin.1);
            // Bounds: a CharProc's ink extent isn't cheaply known
            // without running it — the extraction record leaves it
            // None (callers treat it like a space/.notdef bound).
            let bounds = None;
            let (unicode, resolved_via) = crate::text_extract::resolve_unicode(font, code);
            // §14.9.4: inside an /ActualText span the replacement text
            // stands for the shown characters.
            let (unicode, resolved_via) = match &actual {
                Some(t) => (
                    Some(t.clone()),
                    crate::text_extract::ResolveKind::ActualText,
                ),
                None => (unicode, resolved_via),
            };
            if let Some(s) = &unicode {
                self.extract_unicode_bytes = self
                    .extract_unicode_bytes
                    .checked_add(s.len() as u64)
                    .ok_or(Error::LimitExceeded {
                        limit: "max_extracted_unicode_bytes",
                        max: self.limits.max_extracted_unicode_bytes,
                    })?;
                if self.extract_unicode_bytes > self.limits.max_extracted_unicode_bytes {
                    return Err(Error::LimitExceeded {
                        limit: "max_extracted_unicode_bytes",
                        max: self.limits.max_extracted_unicode_bytes,
                    });
                }
            }
            buf.push(crate::text_extract::CharInfo {
                code: crate::font::unpack_code_for_font(font, code),
                unicode,
                resolved_via,
                origin,
                advance,
                bounds,
                render_mode: render_mode as u8,
                font_size,
                rise,
                tj_adjust,
                in_text: true,
            });
        }

        // ---- advance -----------------------------------------------
        let w0 = font.width(code);
        let tw = if font.is_space_code(code) {
            word_spacing
        } else {
            0.0
        };
        let tx = ((w0 / 1000.0 - tj_adjust / 1000.0) * font_size + char_spacing + tw) * h_scale;
        let adv = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
        self.text_tm = adv.concat(self.text_tm);
        Ok(())
    }

    fn paint_path(&mut self, kind: PaintKind) -> Result<(), Error> {
        if self.extraction_only {
            // Nothing is rasterised; only a pending W/W* needs to be
            // consumed so path-object bookkeeping stays consistent.
            self.path.pending_clip = None;
            self.path.reset();
            return Ok(());
        }
        if self.hidden > 0 {
            // Hidden optional content paints nothing; its clipping
            // path still takes effect (§8.11.3.1 — only painting is
            // suppressed).
            if self.path.pending_clip.is_some() {
                self.install_clip()?;
            }
            self.path.reset();
            return Ok(());
        }
        // §8.5.4: the clip is intersected *after* the path is painted.
        // So we paint first (under the pre-W clip), then install the
        // pending clip for subsequent painting.
        if self.path.is_empty() && self.path.pending_clip.is_none() {
            self.path.reset();
            return Ok(());
        }
        // The path is in user space; the combined `CTM × page→pixmap`
        // transform is passed to fill/stroke so the stroke outline and
        // the path are transformed together (§8.4.3.2/§8.4.4: line
        // width and dash lengths are in user units).
        let ts = self.paint_ts()?;
        let Some(path) = self.path.builder.clone().finish() else {
            // Empty path — still consume a pending clip if present.
            if self.path.pending_clip.is_some() {
                self.install_clip()?;
            }
            self.path.reset();
            return Ok(());
        };

        let mut paint = Paint {
            anti_alias: true,
            blend_mode: self.gs.blend_mode,
            ..Default::default()
        };

        let stroke = Stroke {
            width: if self.gs.line_width == 0.0 {
                hairline_width()
            } else {
                self.gs.line_width as f32
            },
            miter_limit: self.gs.miter_limit as f32,
            line_cap: match self.gs.line_cap {
                1 => tiny_skia::LineCap::Round,
                2 => tiny_skia::LineCap::Square,
                _ => tiny_skia::LineCap::Butt,
            },
            line_join: match self.gs.line_join {
                1 => tiny_skia::LineJoin::Round,
                2 => tiny_skia::LineJoin::Bevel,
                _ => tiny_skia::LineJoin::Miter,
            },
            dash: self.gs.dash.as_ref().and_then(|(arr, phase)| {
                StrokeDash::new(arr.iter().map(|v| *v as f32).collect(), *phase as f32)
            }),
        };

        match kind {
            PaintKind::Stroke => {
                if self.gs.stroke_space.is_pattern() {
                    self.stroke_path_with_pattern(&path, &stroke, ts)?;
                } else {
                    paint.set_color(ts_color(
                        self.gs.stroke_color.with_alpha(self.gs.stroke_alpha),
                    ));
                    self.pixmap
                        .stroke_path(&path, &paint, &stroke, ts, self.gs.clip.as_ref());
                }
            }
            PaintKind::Fill(rule) => {
                if self.gs.fill_space.is_pattern() {
                    if let Some(pat) = self.gs.fill_pattern.clone() {
                        self.fill_path_with_pattern(pat, &path, rule, ts)?;
                    }
                } else {
                    paint.set_color(ts_color(self.gs.fill_color.with_alpha(self.gs.fill_alpha)));
                    self.pixmap
                        .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
                }
            }
            PaintKind::FillStroke(rule) => {
                if self.gs.fill_space.is_pattern() {
                    if let Some(pat) = self.gs.fill_pattern.clone() {
                        self.fill_path_with_pattern(pat, &path, rule, ts)?;
                    }
                } else {
                    paint.set_color(ts_color(self.gs.fill_color.with_alpha(self.gs.fill_alpha)));
                    self.pixmap
                        .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
                }
                if self.gs.stroke_space.is_pattern() {
                    self.stroke_path_with_pattern(&path, &stroke, ts)?;
                } else {
                    paint.set_color(ts_color(
                        self.gs.stroke_color.with_alpha(self.gs.stroke_alpha),
                    ));
                    self.pixmap
                        .stroke_path(&path, &paint, &stroke, ts, self.gs.clip.as_ref());
                }
            }
        }
        // §8.5.4: the clip is updated *after* the path is painted.
        // Install a pending W/W* clip now that the path has been
        // painted under the pre-W clip.
        if self.path.pending_clip.is_some() {
            self.install_clip()?;
        }
        self.path.reset();
        Ok(())
    }
}

enum PaintKind {
    Stroke,
    Fill(FillRule),
    FillStroke(FillRule),
}

/// How a form's transparency group is rendered (see `do_form`).
#[derive(Clone, Copy)]
enum GroupMode {
    /// Not a transparency group.
    None,
    /// Painted directly on the current pixmap.
    Inline,
    /// Painted offscreen, composited afterwards (see `run_in_layer`).
    Layer { isolated: bool },
}

/// Colour components → RGBA8 through `cs`. A `/None` Separation
/// yields alpha 0 — painting with it marks nothing, exactly as
/// §8.6.6.4 requires.
fn color_in(cs: &ColorSpace, comps: &[f64]) -> Result<Color, Error> {
    let [r, g, b] = cs.to_rgb(comps)?;
    let mut c = rgb_to_rgba(r, g, b);
    if cs.paints_nothing() {
        c.a = 0;
    }
    Ok(c)
}

/// A decoded image: straight (non-premultiplied) RGBA8, `w × h`.
struct ImageSource {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// Read `bpc` bits (1/2/4/8/16) at bit offset `bit` (MSB first, as
/// PDF packs samples — §8.9.3). Out-of-range reads yield 0; callers
/// validate the data length up front.
fn read_bits(data: &[u8], bit: u64, bpc: u32) -> u32 {
    let byte = (bit / 8) as usize;
    match bpc {
        8 => data.get(byte).copied().unwrap_or(0) as u32,
        16 => {
            ((data.get(byte).copied().unwrap_or(0) as u32) << 8)
                | data.get(byte + 1).copied().unwrap_or(0) as u32
        }
        1 | 2 | 4 => {
            let b = data.get(byte).copied().unwrap_or(0) as u32;
            let shift = 8 - bpc - (bit % 8) as u32;
            (b >> shift) & ((1 << bpc) - 1)
        }
        _ => 0,
    }
}

/// `a × b / 255` for two 0–255 alphas.
fn mul255(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// Only the separable blend modes (§11.3.5.2) can be applied per
/// channel by `composite_px`; the non-separable ones need a colour-
/// space conversion we do not perform.
fn is_separable_blend(bm: tiny_skia::BlendMode) -> bool {
    use tiny_skia::BlendMode as B;
    matches!(
        bm,
        B::SourceOver
            | B::Multiply
            | B::Screen
            | B::Overlay
            | B::Darken
            | B::Lighten
            | B::ColorDodge
            | B::ColorBurn
            | B::HardLight
            | B::SoftLight
            | B::Difference
            | B::Exclusion
    )
}

/// Composite one straight-colour source pixel (`rgb` 0..=1, alpha
/// `a_s`) onto a premultiplied RGBA8 destination pixel with a
/// separable blend mode — the basic compositing formula of §11.3.8
/// including a non-opaque backdrop (transparency-group layers):
///
/// ```text
/// αr = αb + αs − αb·αs
/// Cr·αr = (1 − αs)·αb·Cb + αs·[(1 − αb)·Cs + αb·B(Cb, Cs)]
/// ```
fn composite_px(dst: &mut [u8], rgb: [f64; 3], a_s: f64, bm: tiny_skia::BlendMode) {
    let a_s = clamp01(a_s);
    if a_s == 0.0 {
        return;
    }
    let ab = dst[3] as f64 / 255.0;
    let ar = ab + a_s - ab * a_s;
    let cb: [f64; 3] = std::array::from_fn(|ch| {
        if ab > 0.0 {
            (dst[ch] as f64 / 255.0 / ab).min(1.0)
        } else {
            0.0
        }
    });
    let cs: [f64; 3] = std::array::from_fn(|ch| clamp01(rgb[ch]));
    let blended: [f64; 3] = if bm == tiny_skia::BlendMode::SourceOver {
        cs
    } else if is_separable_blend(bm) {
        std::array::from_fn(|ch| {
            blend_mode_channel(bm, to_u8(cs[ch]), to_u8(cb[ch])) as f64 / 255.0
        })
    } else {
        blend_non_separable(bm, cs, cb)
    };
    for ch in 0..3 {
        let pr = (1.0 - a_s) * ab * cb[ch] + a_s * ((1.0 - ab) * cs[ch] + ab * blended[ch]);
        dst[ch] = to_u8(pr);
    }
    dst[3] = to_u8(ar);
}

/// The non-separable blend functions of §11.3.5.3 (Hue, Saturation,
/// Color, Luminosity) on RGB triples in 0..=1.
fn blend_non_separable(bm: tiny_skia::BlendMode, cs: [f64; 3], cb: [f64; 3]) -> [f64; 3] {
    fn lum(c: [f64; 3]) -> f64 {
        0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
    }
    fn clip_color(c: [f64; 3]) -> [f64; 3] {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        let mut out = c;
        if n < 0.0 {
            for v in out.iter_mut() {
                *v = l + (*v - l) * l / (l - n);
            }
        }
        if x > 1.0 {
            for v in out.iter_mut() {
                *v = l + (*v - l) * (1.0 - l) / (x - l);
            }
        }
        out
    }
    fn set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
        let d = l - lum(c);
        clip_color([c[0] + d, c[1] + d, c[2] + d])
    }
    fn sat(c: [f64; 3]) -> f64 {
        c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
    }
    fn set_sat(c: [f64; 3], s: f64) -> [f64; 3] {
        let mut idx = [0usize, 1, 2];
        idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
        let (imin, imid, imax) = (idx[0], idx[1], idx[2]);
        let mut out = [0.0; 3];
        if c[imax] > c[imin] {
            out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
            out[imax] = s;
        }
        out
    }
    use tiny_skia::BlendMode as B;
    match bm {
        B::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        B::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        B::Color => set_lum(cs, lum(cb)),
        B::Luminosity => set_lum(cb, lum(cs)),
        _ => cs,
    }
}

/// Converts raw integer samples of one pixel to sRGB8 through a
/// colour space, with `/Decode` applied. One-component spaces at ≤ 8
/// bits use a full lookup table; everything else goes through a
/// bounded per-image cache keyed by the raw sample tuple (tint
/// transforms and ICC curves are far too slow to run per pixel).
struct SampleConverter {
    cs: Rc<ColorSpace>,
    maxv: f64,
    decode: Vec<(f64, f64)>,
    lut: Option<Vec<[u8; 3]>>,
    direct_rgb8: bool,
    cache: HashMap<Vec<u32>, [u8; 3]>,
    vals: Vec<f64>,
}

impl SampleConverter {
    const CACHE_MAX: usize = 1 << 16;

    fn new(cs: Rc<ColorSpace>, bpc: u32, decode: &[(f64, f64)]) -> Result<Self, Error> {
        let maxv = ((1u64 << bpc) - 1) as f64;
        let ncomps = cs.ncomps();
        let mut conv = SampleConverter {
            cs,
            maxv,
            decode: decode.to_vec(),
            lut: None,
            direct_rgb8: false,
            cache: HashMap::new(),
            vals: vec![0.0; ncomps],
        };
        if ncomps == 1 && bpc <= 8 {
            let n = 1usize << bpc;
            let mut lut = Vec::with_capacity(n);
            for v in 0..n {
                lut.push(conv.compute(&[v as u32])?);
            }
            conv.lut = Some(lut);
        } else if matches!(&*conv.cs, ColorSpace::DeviceRGB)
            && bpc == 8
            && decode == [(0.0, 1.0); 3]
        {
            conv.direct_rgb8 = true;
        }
        Ok(conv)
    }

    fn compute(&mut self, comps: &[u32]) -> Result<[u8; 3], Error> {
        for (i, c) in comps.iter().enumerate() {
            let (lo, hi) = self.decode[i];
            self.vals[i] = lo + (*c as f64 / self.maxv) * (hi - lo);
        }
        let rgb = self.cs.to_rgb(&self.vals)?;
        Ok([to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])])
    }

    fn convert(&mut self, comps: &[u32]) -> Result<[u8; 3], Error> {
        if let Some(l) = &self.lut {
            return Ok(l[(comps[0] as usize).min(l.len() - 1)]);
        }
        if self.direct_rgb8 {
            return Ok([comps[0] as u8, comps[1] as u8, comps[2] as u8]);
        }
        if let Some(v) = self.cache.get(comps) {
            return Ok(*v);
        }
        let v = self.compute(comps)?;
        if self.cache.len() < Self::CACHE_MAX {
            self.cache.insert(comps.to_vec(), v);
        }
        Ok(v)
    }
}

// --------------------------------------------------------------------
// Colour helpers (device spaces → RGBA8)
// --------------------------------------------------------------------

fn clamp01(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    v.clamp(0.0, 1.0)
}

fn to_u8(v: f64) -> u8 {
    (clamp01(v) * 255.0).round() as u8
}

fn rgb_to_rgba(r: f64, g: f64, b: f64) -> Color {
    Color {
        r: to_u8(r),
        g: to_u8(g),
        b: to_u8(b),
        a: 255,
    }
}

/// DeviceCMYK → RGBA, naive subtractive form `1 − min(1, c + k)`
/// (§10.4.3 note). Not colour-managed.
fn ts_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

/// One separable blend-mode function `B(cb, cs)` (§11.3.5) evaluated
/// on a single 8-bit channel. `cb` = backdrop, `cs` = source, both in
/// [0,255]; returns the blended channel [0,255]. Only the separable
/// modes (each channel independent) are handled — non-separable modes
/// are refused at `paint_image` and never reach here.
fn blend_mode_channel(mode: tiny_skia::BlendMode, cs: u8, cb: u8) -> u8 {
    let s = cs as f64 / 255.0;
    let b = cb as f64 / 255.0;
    let v = match mode {
        tiny_skia::BlendMode::Multiply => b * s,
        tiny_skia::BlendMode::Screen => b + s - b * s,
        tiny_skia::BlendMode::Overlay => {
            // §11.3.5: Overlay = HardLight with source/backdrop swapped.
            if b <= 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        tiny_skia::BlendMode::Darken => b.min(s),
        tiny_skia::BlendMode::Lighten => b.max(s),
        tiny_skia::BlendMode::ColorDodge => {
            if b >= 1.0 || s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        tiny_skia::BlendMode::ColorBurn => {
            if b <= 0.0 || s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }
        tiny_skia::BlendMode::HardLight => {
            if s <= 0.5 {
                b * 2.0 * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - (2.0 * s - 1.0))
            }
        }
        tiny_skia::BlendMode::SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        tiny_skia::BlendMode::Difference => (b - s).abs(),
        tiny_skia::BlendMode::Exclusion => b + s - 2.0 * b * s,
        // SourceOver handled by the caller's composite; anything else
        // (non-separable) is refused upstream — treat as Normal.
        _ => s,
    };
    (clamp01(v) * 255.0).round() as u8
}

// --------------------------------------------------------------------
// ExtGState parameter helpers
// --------------------------------------------------------------------

/// A numeric ExtGState value; `Err` on the wrong type (§8.4.5 — gs
/// operands are typed, malformed types are structure errors).
fn num_param(v: &Object, key: &str) -> Result<f64, Error> {
    v.as_number()
        .ok_or_else(|| Error::Structure(format!("/{key} must be a number")))
}

/// An integer ExtGState value (line cap/join are enumerated ints).
fn int_param(v: &Object, key: &str) -> Result<i64, Error> {
    v.as_i64()
        .ok_or_else(|| Error::Structure(format!("/{key} must be an integer")))
}

/// An alpha value `ca`/`CA` clamped to [0,1] (§11.3.7.2 — out-of-range
/// alphas clamp, non-numeric is a structure error).
fn alpha_param(v: &Object, key: &str) -> Result<f64, Error> {
    Ok(clamp01(num_param(v, key)?))
}

/// Map a `/BM` value (a name, or an array whose first supported member
/// is used per §11.3.5) to a tiny-skia `BlendMode`. `/Normal` maps to
/// `SourceOver`; every PDF separable/non-separable mode has a direct
/// tiny-skia equivalent, so coverage is complete. `Compatible` is an
/// alias for `Normal` (§11.3.5 note).
fn parse_blend_mode(v: &Object) -> Result<tiny_skia::BlendMode, Error> {
    use tiny_skia::BlendMode as B;
    // An array of names: §11.3.5 says a conforming reader uses the
    // first name it recognises; we recognise all standard modes, so
    // the first element decides.
    let name = match v {
        Object::Name(n) => n.clone(),
        Object::Array(a) => match a.first() {
            Some(Object::Name(n)) => n.clone(),
            _ => return Err(Error::Structure("/BM array must start with a name".into())),
        },
        _ => return Err(Error::Structure("/BM must be a name or name array".into())),
    };
    Ok(match name.as_slice() {
        b"Normal" | b"Compatible" => B::SourceOver,
        b"Multiply" => B::Multiply,
        b"Screen" => B::Screen,
        b"Overlay" => B::Overlay,
        b"Darken" => B::Darken,
        b"Lighten" => B::Lighten,
        b"ColorDodge" => B::ColorDodge,
        b"ColorBurn" => B::ColorBurn,
        b"HardLight" => B::HardLight,
        b"SoftLight" => B::SoftLight,
        b"Difference" => B::Difference,
        b"Exclusion" => B::Exclusion,
        b"Hue" => B::Hue,
        b"Saturation" => B::Saturation,
        b"Color" => B::Color,
        b"Luminosity" => B::Luminosity,
        other => {
            return Err(Error::Unsupported {
                feature: "blend mode",
                detail: format!(
                    "/{} — not a standard PDF blend mode",
                    String::from_utf8_lossy(other)
                ),
            })
        }
    })
}

/// Decode a PDF *text string* (§7.9.2.2): UTF-16BE with BOM, UTF-8
/// with BOM (PDF 2.0), else PDFDocEncoding — whose printable range
/// coincides with Latin-1 for everything a form value or
/// `/ActualText` carries in practice.
pub(crate) fn pdf_text_string(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let units: Vec<u16> = b[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if b.len() >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    b.iter().map(|&c| c as char).collect()
}

/// `/Width` × `/Height` as `u32` for the bilevel filters; the
/// dictionary has already passed `image_dims`, so this only re-reads.
fn image_dims_u32(dict: &Dict) -> Result<(u32, u32), Error> {
    let get = |key: &[u8]| -> Result<u32, Error> {
        dict.get(key)
            .and_then(|o| o.as_i64())
            .filter(|v| *v > 0)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| {
                Error::Structure(format!(
                    "image /{} is missing or not a positive integer",
                    String::from_utf8_lossy(key)
                ))
            })
    };
    Ok((get(b"Width")?, get(b"Height")?))
}

fn show_name(n: &[u8]) -> String {
    String::from_utf8_lossy(n).into_owned()
}

/// Parse a 6-number array into a `Matrix`; `None` for any other shape.
fn matrix_from_array(o: &Object) -> Option<Matrix> {
    let arr = o.as_array()?;
    if arr.len() != 6 {
        return None;
    }
    let v: Vec<f64> = arr.iter().map(|x| x.as_number()).collect::<Option<_>>()?;
    Some(Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]))
}

/// PDF line width 0 is a hairline — "as thin as can be rendered"
/// (§8.4.3.2). tiny-skia treats `Stroke.width == 0` as a hairline
/// automatically, so the value here is a nominal width that will be
/// replaced by the hairline path anyway.
fn hairline_width() -> f32 {
    0.0
}
