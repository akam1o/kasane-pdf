//! M3: render a page to a pixel buffer (basic paths, clipping, Device
//! colours) and encode it as PNG.
//!
//! This is the first execution layer on top of the M2 `Operation`
//! stream. It is intentionally narrow:
//!
//! **Rendered**: `q`/`Q`/`cm`, path construction (`m l c v y h re`),
//! painting (`S s f F f* B B* b b* n`), clipping (`W W*`), line state
//! (`w J j M d i`), Device colours (`G g RG rg K k` and `CS/cs +
//! SC/sc` selecting `DeviceGray`/`DeviceRGB`/`DeviceCMYK`, plus
//! `DefaultGray`/`DefaultRGB`/`DefaultCMYK` substitution), `ri`
//! (stored; no effect on device-space output).
//!
//! **Refused as `Error::Unsupported`** (never silently skipped):
//! text (`Tf`, `Tj`, `TJ`, `'`, `"`), `Do`/`sh`, `gs` (any ExtGState —
//! M3 interprets none), `SCN`/`scn` pattern names, `CS`/`cs` naming a
//! non-device colour space, page annotations (`/Annots` non-empty),
//! `/UserUnit` ≠ 1, inline images (rejected at M2 parse).
//!
//! Ignored per spec (no effect on output): `BMC`/`BDC`/`EMC`,
//! `MP`/`DP`, `BX`/`EX` and `Operation::Unknown` inside them, and the
//! text-state operators (`Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts`/`Td`/`TD`/`Tm`/
//! `T*`) — they only matter once a text-showing op runs.
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
    /// Non-stroking alpha `ca` (§11.3.7.2) — multiplies the fill
    /// colour's alpha at paint time. 1.0 = opaque.
    fill_alpha: f64,
    /// Stroking alpha `CA` — multiplies the stroke colour's alpha.
    stroke_alpha: f64,
    /// Current blend mode `BM` (§11.3.5) — mapped to tiny-skia's
    /// `BlendMode` at paint time. `SourceOver` = PDF `/Normal`.
    blend_mode: tiny_skia::BlendMode,
    /// Current clip as a pixmap-space alpha mask; `None` = page bounds
    /// only. Stacked clips intersect via `Mask::intersect_path`.
    clip: Option<Mask>,
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
    /// invisible, 4–7 clip variants (M5: refused).
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
    /// Opaque white (the M3 page background).
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
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            blend_mode: tiny_skia::BlendMode::SourceOver,
            clip: None,
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

    // ---- Reject pages we cannot faithfully draw ---------------------
    // (`/Annots` are drawn after the content — see `draw_annotations`.)
    if let Some(uu) = page_dict_entry(doc, page, b"UserUnit")? {
        let v = uu
            .as_number()
            .ok_or_else(|| Error::Structure("/UserUnit is not a number".into()))?;
        if (v - 1.0).abs() > f64::EPSILON {
            return Err(Error::Unsupported {
                feature: "/UserUnit other than 1",
                detail: format!("UserUnit {v}"),
            });
        }
    }

    // ---- Page geometry ----------------------------------------------
    let crop = page.crop_box();
    if !rect_is_finite(&crop) {
        return Err(Error::Structure("CropBox has non-finite values".into()));
    }
    let rotate = page.rotate().rem_euclid(360);

    let scale = opts.dpi / 72.0;
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
        blend_restricted: 0,
        layer_bytes: 0,
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
    /// Nesting depth of transparency groups whose *contents* must use
    /// Normal blending for our rendering of them to be exact: an
    /// isolated group painted inline (a non-Normal mode inside would
    /// see the page instead of a transparent backdrop) and a
    /// non-isolated group painted through an offscreen layer (the
    /// reverse). `gs` refuses a non-Normal `BM` while this is non-zero.
    blend_restricted: u32,
    /// Bytes of offscreen group layers currently alive — bounded by
    /// `max_render_buffer_bytes` together with the page pixmap.
    layer_bytes: u64,
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
            blend_restricted: 0,
            layer_bytes: 0,
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
                if *m >= 4 {
                    return Err(Error::Unsupported {
                        feature: "text rendering mode",
                        detail: format!("{m} — clipping modes 4–7 are not implemented"),
                    });
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
            Operation::Shading(n) => {
                return Err(Error::Unsupported {
                    feature: "shading (sh)",
                    detail: format!(
                        "/{} — shading patterns are not implemented in M3",
                        show_name(n)
                    ),
                })
            }
            Operation::SetExtGState(n) => self.apply_extgstate(n)?,

            // ---- marked content / compatibility -----------------
            Operation::MarkedContentPoint(_) => {
                // §10.5.2: MP takes a tag only; it is metadata with no
                // rendering effect.
            }
            Operation::DesignatedMarkedContentPoint { tag, properties }
            | Operation::BeginMarkedContentWithProps { tag, properties } => {
                self.marked_content(tag, properties)?;
            }
            Operation::BeginMarkedContent(_) | Operation::EndMarkedContent => {
                // Tag-only BMC/EMC have no rendering effect.
            }
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

    /// A `DP`/`BDC` tag+properties operand: decide whether this is
    /// ordinary marked content (ignored), Optional Content we cannot
    /// evaluate (`/OC` + an OCG/OCMD property list — §8.11), or a
    /// `/Span` sequence carrying `/ActualText` replacement text
    /// (§14.9.4), which would change what text extraction reports.
    /// All other tags and property lists are plain marked content.
    ///
    /// The `properties` operand may be a name (resolved against the
    /// current `/Properties` resource subdictionary) or a direct
    /// dictionary. An unresolvable name is a structure error, never
    /// silently ignored.
    fn marked_content(&self, tag: &[u8], properties: &Object) -> Result<(), Error> {
        // Resolve a `/Properties` name through the current resources.
        // Names under tags we don't inspect are plain marked content.
        let resolved = match properties {
            Object::Name(n) => {
                if tag != b"OC" && tag != b"Span" {
                    return Ok(());
                }
                let props = self
                    .doc
                    .resolve_resource(&self.resources, "Properties", n)?;
                match props {
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
            _ => return Ok(()), // other operand types: treated as inert
        };

        let dict = resolved.as_dict().ok_or_else(|| {
            Error::Structure("marked-content properties is not a dictionary".into())
        })?;
        if tag == b"OC" {
            // OCG/OCMD membership is indicated by a /Type of /OCG or /OCMD.
            match dict.get(b"Type".as_slice()).and_then(|t| t.as_name()) {
                Some(b"OCG") | Some(b"OCMD") => return Err(Error::Unsupported {
                    feature: "optional content (/OC or /OCMD marked content)",
                    detail:
                        "OCG/OCMD membership requires visibility evaluation, not implemented in M3"
                            .into(),
                }),
                _ => return Ok(()),
            }
        }
        // §14.9.4: an /ActualText entry in a marked-content property
        // list (attached via a /Span tag) substitutes the shown
        // characters. Extraction cannot honour it, so refuse rather
        // than return text that may not match the document's meaning.
        if tag == b"Span" && dict.contains_key(b"ActualText".as_slice()) {
            return Err(Error::Unsupported {
                feature: "replacement text (/Span <</ActualText …>>)",
                detail: "ActualText substitutes extracted text and is not implemented".into(),
            });
        }
        Ok(())
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
        } else {
            self.gs.fill_space = cs;
            self.gs.fill_color = color;
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
            return Err(Error::Unsupported {
                feature: "pattern colour (SCN/scn name operand)",
                detail: format!("/{} — pattern painting is not implemented", show_name(n)),
            });
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
                    let bm = parse_blend_mode(v)?;
                    if bm != tiny_skia::BlendMode::SourceOver && self.blend_restricted > 0 {
                        return Err(Error::Unsupported {
                            feature: "group XObject (/Group in a form dictionary)",
                            detail: "a non-Normal blend mode inside this transparency group \
                                     would composite against a backdrop (the group's own, \
                                     or the page's) that our rendering of the group does \
                                     not provide"
                                .into(),
                        });
                    }
                    self.gs.blend_mode = bm;
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
                    if !matches!(v.as_name(), Some(b"None")) {
                        return Err(Error::Unsupported {
                            feature: "soft mask (/SMask)",
                            detail:
                                "luminosity/alpha soft masks need transparency-group compositing"
                                    .into(),
                        });
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
                // `/Font [ref size]` sets the text font via gs. It is
                // an *object reference*, not a resource name, so it
                // can't go through load_font's name lookup; rather than
                // hand-roll a parallel loader we refuse — it does not
                // appear in the corpus.
                b"Font" => {
                    return Err(Error::Unsupported {
                        feature: "ExtGState /Font",
                        detail: "font selection via gs is not implemented".into(),
                    });
                }

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
            self.gs.clip = Some(
                Mask::new(self.pixmap.width(), self.pixmap.height())
                    .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?,
            );
            return Ok(());
        };
        match &mut self.gs.clip {
            None => {
                let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height())
                    .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?;
                mask.fill_path(&new_path, rule, true, ts);
                self.gs.clip = Some(mask);
            }
            Some(existing) => existing.intersect_path(&new_path, rule, true, ts),
        }
        Ok(())
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
        // group is composited as a whole. Which rendering is *exact*:
        //  * opaque (CA = ca = 1), Normal blend, not knockout, not
        //    isolated → painting the contents inline on the page;
        //  * opaque, Normal, isolated → inline too, provided nothing
        //    inside uses a non-Normal blend mode (it would see the
        //    page where the spec gives it a transparent backdrop) —
        //    `blend_restricted` makes `gs` refuse that;
        //  * non-opaque or non-Normal → an offscreen layer: the
        //    contents are painted onto transparency with alpha and
        //    blend reset (§11.6.6), then the layer is composited once
        //    with the group's `ca` and `BM`. Exact for an isolated
        //    group; for a non-isolated one only while its contents
        //    blend Normal (then the backdrop makes no difference) —
        //    again enforced through `blend_restricted`;
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
            if !is_separable_blend(self.gs.blend_mode) {
                return Err(Error::Unsupported {
                    feature: "group XObject (/Group in a form dictionary)",
                    detail: "a non-separable blend mode on a transparency group needs \
                             per-pixel colour conversion"
                        .into(),
                });
            }
            let isolated = gd.get(b"I".as_slice()).and_then(|o| o.as_bool()) == Some(true);
            let opaque_normal = self.gs.fill_alpha >= 1.0
                && self.gs.stroke_alpha >= 1.0
                && self.gs.blend_mode == tiny_skia::BlendMode::SourceOver;
            group = if opaque_normal {
                GroupMode::Inline { restrict: isolated }
            } else {
                GroupMode::Layer {
                    restrict: !isolated,
                }
            };
        }
        // A form may carry /OC (optional content membership) or /Ref
        // (reference XObject); both change what's painted — refuse
        // rather than guess.
        if dict.contains_key(b"OC".as_slice()) {
            return Err(Error::Unsupported {
                feature: "optional content on a form XObject (/OC)",
                detail: "OC visibility evaluation is not implemented".into(),
            });
        }
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
                GroupMode::None => self.run(&ops),
                GroupMode::Inline { restrict } => self.run_blend_restricted(&ops, restrict),
                GroupMode::Layer { restrict } => self.run_in_layer(&ops, restrict),
            };
            self.xobject_depth -= 1;
            r
        })();

        self.swap_resources(saved_resources);
        if pop_ref {
            self.xobject_stack.pop();
        }
        self.gs = self.gs_stack.pop().expect("pushed above");
        run_result
    }

    /// Run `ops`, refusing non-Normal blend modes inside when
    /// `restrict` (see `blend_restricted`).
    fn run_blend_restricted(&mut self, ops: &[Operation], restrict: bool) -> Result<(), Error> {
        if restrict {
            self.blend_restricted += 1;
        }
        let r = self.run(ops);
        if restrict {
            self.blend_restricted -= 1;
        }
        r
    }

    /// Paint `ops` into a fresh, fully transparent layer the size of
    /// the page, then composite that layer onto the current pixmap
    /// with the group's constant alpha (`ca`) and blend mode. Inside
    /// the layer the alpha constants are 1 and the blend mode Normal
    /// (§11.6.6); the caller's `gs` pop restores them afterwards.
    fn run_in_layer(&mut self, ops: &[Operation], restrict: bool) -> Result<(), Error> {
        if self.extraction_only {
            return self.run_blend_restricted(ops, restrict);
        }
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let bytes = w as u64 * h as u64 * 4;
        // The page pixmap plus every live layer must fit the budget.
        if self.layer_bytes.saturating_add(bytes).saturating_add(bytes)
            > self.limits.max_render_buffer_bytes
        {
            return Err(Error::LimitExceeded {
                limit: "max_render_buffer_bytes",
                max: self.limits.max_render_buffer_bytes,
            });
        }
        let mut layer = Pixmap::new(w, h)
            .ok_or_else(|| Error::Structure("cannot allocate transparency-group layer".into()))?;
        let alpha = self.gs.fill_alpha;
        let bm = self.gs.blend_mode;
        self.gs.fill_alpha = 1.0;
        self.gs.stroke_alpha = 1.0;
        self.gs.blend_mode = tiny_skia::BlendMode::SourceOver;
        std::mem::swap(&mut *self.pixmap, &mut layer);
        self.layer_bytes += bytes;
        let r = self.run_blend_restricted(ops, restrict);
        self.layer_bytes -= bytes;
        std::mem::swap(&mut *self.pixmap, &mut layer);
        r?;
        self.composite_layer(&layer, alpha, bm);
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
        let entries = match annots {
            Object::Array(a) => a,
            Object::Null => return Ok(()),
            _ => return Err(Error::Structure("/Annots is not an array".into())),
        };
        for entry in &entries {
            let entry_ref = entry.as_ref();
            let annot = self.doc.deref(entry)?;
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
            let ap = match ad.get(b"AP".as_slice()) {
                Some(o) => self.doc.deref(o)?,
                None => {
                    if subtype == b"Link" {
                        continue;
                    }
                    return Err(Error::Unsupported {
                        feature: "annotation without appearance stream",
                        detail: format!(
                            "/Subtype /{} has no /AP — synthesising an appearance is not \
                             implemented",
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
                    // Appearance sub-dictionary: one stream per state,
                    // selected by /AS. A missing /AS is only acceptable
                    // when there is exactly one state to choose from.
                    let key: Vec<u8> = match ad.get(b"AS".as_slice()).and_then(|o| o.as_name()) {
                        Some(k) => k.to_vec(),
                        None if states.len() == 1 => states.keys().next().unwrap().clone(),
                        None => {
                            return Err(Error::Unsupported {
                                feature: "annotation appearance states without /AS",
                                detail: format!(
                                    "/AP /N has {} states and the annotation names none",
                                    states.len()
                                ),
                            })
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
            let (sdict, sraw) = stream
                .as_stream()
                .ok_or_else(|| Error::Structure("appearance state is not a stream".into()))?;
            let sdict = sdict.clone();
            let sraw = sraw.to_vec();

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
            self.gs.clip = Some(
                Mask::new(self.pixmap.width(), self.pixmap.height())
                    .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?,
            );
            return Ok(());
        };
        match &mut self.gs.clip {
            None => {
                let mut mask = Mask::new(self.pixmap.width(), self.pixmap.height())
                    .ok_or_else(|| Error::Structure("clip mask allocation failed".into()))?;
                mask.fill_path(&path, FillRule::Winding, true, ts);
                self.gs.clip = Some(mask);
            }
            Some(existing) => existing.intersect_path(&path, FillRule::Winding, true, ts),
        }
        Ok(())
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
        // on an image needs a per-pixel colour-space conversion our
        // raster path doesn't perform — rather than silently draw it
        // as Normal (a mis-render) we refuse. Separable modes are
        // applied per-channel in `composite_px`.
        if !is_separable_blend(self.gs.blend_mode) {
            return Err(Error::Unsupported {
                feature: "non-separable blend mode on image",
                detail: "Hue/Saturation/Color/Luminosity blend on an image needs per-pixel colour conversion".into(),
            });
        }
        if dict.contains_key(b"OC".as_slice()) {
            return Err(Error::Unsupported {
                feature: "optional content on an image XObject (/OC)",
                detail: format!(
                    "/OC on /{} — OC visibility evaluation is not implemented",
                    show_name(name)
                ),
            });
        }
        // `SMaskInData 0` is the defined default; non-zero means the
        // alpha lives inside JPX data we do not decode.
        if let Some(v) = dict.get(b"SMaskInData".as_slice()) {
            if v.as_i64().unwrap_or(0) != 0 {
                return Err(Error::Unsupported {
                    feature: "image attribute",
                    detail: format!(
                        "/SMaskInData on /{} (JPX-embedded alpha) is not implemented",
                        show_name(name)
                    ),
                });
            }
        }
        let src = self.decode_image_source(dict, raw, name, false)?;
        self.composite_image(&src)
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

        // ---- bit depth & colour space ------------------------------
        let bpc_entry = dict
            .get(b"BitsPerComponent".as_slice())
            .and_then(|o| o.as_i64());
        let bpc: u32 = if is_mask {
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
        let cs: Option<Rc<ColorSpace>> =
            if is_mask {
                None
            } else {
                let cs_obj = dict.get(b"ColorSpace".as_slice()).ok_or_else(|| {
                    Error::Structure(format!("image /{} has no /ColorSpace", show_name(name)))
                })?;
                let cs = ColorSpace::parse(self.doc, cs_obj, &self.resources, self.limits)
                    .map_err(|e| match e {
                        Error::Unsupported {
                            feature: "pattern colour space",
                            detail,
                        } => Error::Unsupported {
                            feature: "image colour space",
                            detail,
                        },
                        other => other,
                    })?;
                self.note_approximation(&cs);
                Some(Rc::new(cs))
            };
        let ncomps = cs.as_ref().map(|c| c.ncomps()).unwrap_or(1);
        let default_decode = cs
            .as_ref()
            .map(|c| c.default_decode(bpc))
            .unwrap_or_else(|| vec![(0.0, 1.0)]);
        let decode = self.image_decode_array(dict, ncomps, &default_decode)?;

        // ---- sample data -------------------------------------------
        let filters = crate::filter::normalize_filters(dict.get(b"Filter".as_slice()))?;
        let is_dct = filters
            .last()
            .map(|f| f == b"DCTDecode" || f == b"DCT")
            .unwrap_or(false);
        let data = self.decode_image_data(dict, raw)?;
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
        let row_bytes = (w as u64 * ncomps as u64 * bpc as u64 + 7) / 8;
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
                for p in a.chunks_exact(2) {
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
        // §8.9.6.4: when both are present /SMask wins.
        if let Some(sm) = dict.get(b"SMask".as_slice()) {
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
        let bpc = match sd
            .get(b"BitsPerComponent".as_slice())
            .and_then(|o| o.as_i64())
        {
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
        let cs_obj = sd
            .get(b"ColorSpace".as_slice())
            .ok_or_else(|| Error::Structure("image /SMask has no /ColorSpace".into()))?;
        let cs = ColorSpace::parse(self.doc, cs_obj, &self.resources, self.limits)?;
        if cs.ncomps() != 1 {
            return Err(Error::Structure(format!(
                "image /SMask colour space has {} components (must be DeviceGray)",
                cs.ncomps()
            )));
        }
        let decode = self.image_decode_array(sd, 1, &[(0.0, 1.0)])?;
        let data = self.decode_image_data(sd, sraw)?;
        let row_bytes = (sw as u64 * bpc as u64 + 7) / 8;
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
        for (i, fname) in filters.iter().enumerate() {
            let parm = parms.get(i).and_then(|o| o.as_ref());
            buf = match fname.as_slice() {
                b"FlateDecode" | b"Fl" => {
                    let mut dec = flate2::read::ZlibDecoder::new(buf.as_slice());
                    let mut out = Vec::new();
                    std::io::Read::read_to_end(&mut dec, &mut out)
                        .map_err(|e| Error::Parse(format!("FlateDecode failed: {e}")))?;
                    if out.len() as u64 > self.limits.max_image_bytes {
                        return Err(Error::LimitExceeded {
                            limit: "max_image_bytes",
                            max: self.limits.max_image_bytes,
                        });
                    }
                    // Predictor post-processing (PNG/TIFF) rides on
                    // /DecodeParms — same rules as the stream decoder.
                    crate::filter::apply_predictor(out, parm, self.limits)?
                }
                b"DCTDecode" | b"DCT" => self.dct_decode(&buf, parm)?,
                other => {
                    return Err(Error::Unsupported {
                        feature: "image stream filter",
                        detail: format!("/{}", String::from_utf8_lossy(other)),
                    })
                }
            };
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
                for pair in arr.chunks_exact(2) {
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

    /// Composite a finished transparency-group layer (premultiplied,
    /// same size as the page pixmap) onto the current pixmap with the
    /// group's constant alpha and blend mode (§11.4.7, §11.6.6).
    fn composite_layer(&mut self, layer: &Pixmap, alpha: f64, bm: tiny_skia::BlendMode) {
        let alpha = clamp01(alpha);
        let src = layer.data();
        let dst = self.pixmap.data_mut();
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            let la = s[3] as f64 / 255.0;
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

        let gs = &mut self.gs;
        let t = &mut gs.text;

        // Resolve code → glyph.
        let gid = font.glyph_id(code)?;

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

        if let Some(path) = &glyph_path {
            if !self.extraction_only {
                // Text→render matrix (§9.4.4 note 2).
                let trm = Matrix::new(t.font_size * t.h_scale, 0.0, 0.0, t.font_size, 0.0, t.rise)
                    .concat(self.text_tm)
                    .concat(gs.ctm);
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
                    0 | 2 => {
                        // fill (mode 2 also strokes below)
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
                if t.render_mode == 1 || t.render_mode == 2 {
                    // stroke
                    let mut p = Paint {
                        anti_alias: true,
                        blend_mode: gs.blend_mode,
                        ..Default::default()
                    };
                    let c = gs.stroke_color.with_alpha(gs.stroke_alpha);
                    p.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a));
                    let stroke = tiny_skia::Stroke {
                        width: (gs.line_width * t.font_size / 1000.0) as f32,
                        ..Default::default()
                    };
                    self.pixmap
                        .stroke_path(path, &p, &stroke, full, gs.clip.as_ref());
                }
                // mode 3: invisible — paint nothing, still advance.
            }
        }

        // Record the extraction event before advancing — the char's
        // origin is the *current* Tm translation, and its advance is
        // what we are about to add.
        if let Some(buf) = &mut self.extract {
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
            let (ax, ay) = trm.transform(tx_text, 0.0);
            let advance = (ax - origin.0, ay - origin.1);
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
                code,
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
        let tx =
            ((w0 / 1000.0 - tj_adjust / 1000.0) * t.font_size + t.char_spacing + tw) * t.h_scale;
        let adv = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
        self.text_tm = adv.concat(self.text_tm);
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
            if !self.extraction_only {
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
                // A CharProc is its own scope: it may legitimately
                // contain `BT … ET` (nested Type3, Type3-in-Type3), so
                // the parent's text state must not leak in. Save and
                // clear `in_text` for the nested run, restore after.
                let saved_in_text = self.in_text;
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
                self.swap_resources(saved_resources);
                self.gs = self.gs_stack.pop().expect("pushed above");
                run_result?;
            }
        }

        // ---- extraction record -------------------------------------
        if let Some(buf) = &mut self.extract {
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
                code,
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
                paint.set_color(ts_color(
                    self.gs.stroke_color.with_alpha(self.gs.stroke_alpha),
                ));
                self.pixmap
                    .stroke_path(&path, &paint, &stroke, ts, self.gs.clip.as_ref());
            }
            PaintKind::Fill(rule) => {
                paint.set_color(ts_color(self.gs.fill_color.with_alpha(self.gs.fill_alpha)));
                self.pixmap
                    .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
            }
            PaintKind::FillStroke(rule) => {
                paint.set_color(ts_color(self.gs.fill_color.with_alpha(self.gs.fill_alpha)));
                self.pixmap
                    .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
                paint.set_color(ts_color(
                    self.gs.stroke_color.with_alpha(self.gs.stroke_alpha),
                ));
                self.pixmap
                    .stroke_path(&path, &paint, &stroke, ts, self.gs.clip.as_ref());
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
    Inline { restrict: bool },
    /// Painted into an offscreen layer, composited afterwards.
    Layer { restrict: bool },
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
    for ch in 0..3 {
        let cb = if ab > 0.0 {
            (dst[ch] as f64 / 255.0 / ab).min(1.0)
        } else {
            0.0
        };
        let cs = clamp01(rgb[ch]);
        let blended = if bm == tiny_skia::BlendMode::SourceOver {
            cs
        } else {
            blend_mode_channel(bm, to_u8(cs), to_u8(cb)) as f64 / 255.0
        };
        let pr = (1.0 - a_s) * ab * cb + a_s * ((1.0 - ab) * cs + ab * blended);
        dst[ch] = to_u8(pr);
    }
    dst[3] = to_u8(ar);
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
/// (§10.4.3 note). Not colour-managed; ICC is out of M3 scope.
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
