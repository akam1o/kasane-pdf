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

use crate::content::Operation;
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, IndirectRef, Name, Object};
use crate::page::Page;
use std::rc::Rc;
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
    stroke_space: DeviceSpace,
    fill_space: DeviceSpace,
    stroke_color: Color,
    fill_color: Color,
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
    font: Option<Rc<crate::font::SimpleFont>>,
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

/// Device colour space actually selected (after `Default*` mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceSpace {
    Gray,
    Rgb,
    Cmyk,
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
            stroke_space: DeviceSpace::Gray,
            fill_space: DeviceSpace::Gray,
            stroke_color: Color::BLACK,
            fill_color: Color::BLACK,
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
    if let Some(annots) = page_dict_entry(doc, page, b"Annots")? {
        if !is_empty_array(&annots) {
            return Err(Error::Unsupported {
                feature: "page annotations (/Annots)",
                detail: "page has annotations; annotation rendering is not implemented".into(),
            });
        }
    }
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
    };
    renderer.run(&ops)?;

    Ok(RenderedPage {
        pixmap,
        width,
        height,
        dpi: opts.dpi,
        page_size_pts: (w_pts, h_pts),
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

fn is_empty_array(o: &Object) -> bool {
    matches!(o, Object::Array(a) if a.is_empty())
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
        // We still validate page geometry — extraction runs the same
        // code path as painting.
        let limits = &opts.limits;
        if let Some(annots) = page_dict_entry(doc, page, b"Annots")? {
            if !is_empty_array(&annots) {
                return Err(Error::Unsupported {
                    feature: "page annotations (/Annots)",
                    detail: "page has annotations; annotation rendering is not implemented".into(),
                });
            }
        }
        let crop = page.crop_box();
        if !rect_is_finite(&crop) {
            return Err(Error::Structure("CropBox has non-finite values".into()));
        }
        let rotate = page.rotate().rem_euclid(360);
        let scale = opts.dpi / 72.0;
        let w_pts = crop.width();
        let h_pts = crop.height();
        let width = checked_px(w_pts, scale, limits)?;
        let height = checked_px(h_pts, scale, limits)?;
        if width as u64 * height as u64 > limits.max_render_total_pixels {
            return Err(Error::LimitExceeded {
                limit: "max_render_total_pixels",
                max: limits.max_render_total_pixels,
            });
        }
        let user_to_pix = build_page_transform(&crop, rotate, scale)?;
        // Minimal pixmap — content isn't painted, but the struct
        // requires a live buffer. 1×1 suffices.
        let mut pixmap = Pixmap::new(1, 1)
            .ok_or_else(|| Error::Structure("cannot allocate extraction pixmap".into()))?;
        pixmap.fill(ts_color(Color::WHITE));
        let resources = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Default::default()));
        Ok(Renderer {
            limits,
            doc,
            pixmap: Box::leak(Box::new(pixmap)),
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
            extract: Some(Vec::new()),
        })
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
                // PDF: CTM' = cm × CTM ⇒ existing.concat(cm).
                self.gs.ctm = self.gs.ctm.concat(*m);
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
            Operation::SetGrayStroke(g) => {
                self.gs.stroke_space = DeviceSpace::Gray;
                self.gs.stroke_color = gray_to_rgba(*g);
            }
            Operation::SetGrayFill(g) => {
                self.gs.fill_space = DeviceSpace::Gray;
                self.gs.fill_color = gray_to_rgba(*g);
            }
            Operation::SetRgbStroke(r, g, b) => {
                self.gs.stroke_space = DeviceSpace::Rgb;
                self.gs.stroke_color = rgb_to_rgba(*r, *g, *b);
            }
            Operation::SetRgbFill(r, g, b) => {
                self.gs.fill_space = DeviceSpace::Rgb;
                self.gs.fill_color = rgb_to_rgba(*r, *g, *b);
            }
            Operation::SetCmykStroke(c, m, y, k) => {
                self.gs.stroke_space = DeviceSpace::Cmyk;
                self.gs.stroke_color = cmyk_to_rgba(*c, *m, *y, *k);
            }
            Operation::SetCmykFill(c, m, y, k) => {
                self.gs.fill_space = DeviceSpace::Cmyk;
                self.gs.fill_color = cmyk_to_rgba(*c, *m, *y, *k);
            }
            Operation::SetColorSpaceStroke(n) => {
                self.gs.stroke_space = self.resolve_colorspace(n)?;
            }
            Operation::SetColorSpaceFill(n) => {
                self.gs.fill_space = self.resolve_colorspace(n)?;
            }
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
                let f =
                    crate::font::load_simple_font(self.doc, &self.resources, font, self.limits)?;
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
            Operation::SetExtGState(n) => {
                return Err(Error::Unsupported {
                    feature: "extended graphics state (gs)",
                    detail: format!("/{} — ExtGState is not implemented in M3", show_name(n)),
                })
            }

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
            Operation::Unknown { .. } => {
                // §7.8.2: inside BX/EX unknown ops may be ignored.
            }
        }
        Ok(())
    }

    /// A `DP`/`BDC` tag+properties operand: decide whether this is
    /// ordinary marked content (ignored) or Optional Content that we
    /// cannot evaluate. `/OC` and `/OCMD` mark Optional Content
    /// membership (§8.11); other tags are plain marked content.
    ///
    /// The `properties` operand may be a name (resolved against the
    /// page's `/Properties` resource subdictionary) or a direct
    /// dictionary. Names that resolve to an OCG/OCMD dictionary make
    /// the whole block optional-content — we refuse to render the
    /// page rather than silently draw members that may be hidden.
    fn marked_content(&self, tag: &[u8], properties: &Object) -> Result<(), Error> {
        // Resolve a `/Properties` name through the page's resources.
        let resolved = match properties {
            Object::Name(n) => {
                // /OC/OCMD tags are only meaningful when they actually
                // resolve to an optional-content dictionary; other
                // names are treated as plain marked content.
                if tag != b"OC" && tag != b"OCMD" {
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
        // OCG/OCMD membership is indicated by a /Type of /OCG or /OCMD.
        match dict.get(b"Type".as_slice()).and_then(|t| t.as_name()) {
            Some(b"OCG") | Some(b"OCMD") => Err(Error::Unsupported {
                feature: "optional content (/OC or /OCMD marked content)",
                detail: "OCG/OCMD membership requires visibility evaluation, not implemented in M3"
                    .into(),
            }),
            _ => Ok(()),
        }
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

    fn resolve_colorspace(&self, name: &Name) -> Result<DeviceSpace, Error> {
        match name.as_slice() {
            b"DeviceGray" | b"DefaultGray" => Ok(DeviceSpace::Gray),
            b"DeviceRGB" | b"DefaultRGB" => Ok(DeviceSpace::Rgb),
            b"DeviceCMYK" | b"DefaultCMYK" => Ok(DeviceSpace::Cmyk),
            other => Err(Error::Unsupported {
                feature: "non-device colour space",
                detail: format!(
                    "/{} — only DeviceGray/DeviceRGB/DeviceCMYK and Default* are supported in M3",
                    String::from_utf8_lossy(other)
                ),
            }),
        }
    }

    /// `SC`/`SCN`/`sc`/`scn` operands → colour in the active device
    /// space. A trailing pattern name (the `N` forms) needs pattern
    /// support → `Error::Unsupported`.
    fn set_color(&mut self, ops: &[Object], stroke: bool) -> Result<(), Error> {
        let space = if stroke {
            self.gs.stroke_space
        } else {
            self.gs.fill_space
        };
        let (comps, name) = match ops.split_last() {
            Some((last @ Object::Name(_), rest)) => (rest, Some(last)),
            _ => (ops, None),
        };
        if let Some(Object::Name(n)) = name {
            return Err(Error::Unsupported {
                feature: "pattern colour (SCN/scn name operand)",
                detail: format!(
                    "/{} — pattern painting is not implemented in M3",
                    show_name(n)
                ),
            });
        }
        let nums: Vec<f64> = comps
            .iter()
            .map(|o| o.as_number())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::Parse("SC/sc operands must be numbers".into()))?;
        let color = match space {
            DeviceSpace::Gray => {
                if nums.len() != 1 {
                    return Err(Error::Parse(format!(
                        "DeviceGray expects 1 component, got {}",
                        nums.len()
                    )));
                }
                gray_to_rgba(nums[0])
            }
            DeviceSpace::Rgb => {
                if nums.len() != 3 {
                    return Err(Error::Parse(format!(
                        "DeviceRGB expects 3 components, got {}",
                        nums.len()
                    )));
                }
                rgb_to_rgba(nums[0], nums[1], nums[2])
            }
            DeviceSpace::Cmyk => {
                if nums.len() != 4 {
                    return Err(Error::Parse(format!(
                        "DeviceCMYK expects 4 components, got {}",
                        nums.len()
                    )));
                }
                cmyk_to_rgba(nums[0], nums[1], nums[2], nums[3])
            }
        };
        if stroke {
            self.gs.stroke_color = color;
        } else {
            self.gs.fill_color = color;
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
            Some(b"Image") => self.do_image(&dict, &raw, name),
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

        // Group XObjects (§8.10.3) carry a /Group dictionary; the only
        // defined subtype is /Transparency, which needs the
        // transparency model — refuse rather than render wrong.
        if dict.contains_key(b"Group".as_slice()) {
            return Err(Error::Unsupported {
                feature: "group XObject (/Group in a form dictionary)",
                detail: "group attributes (e.g. /Transparency) are not implemented".into(),
            });
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
        let saved_resources = std::mem::replace(&mut self.resources, form_resources);

        // §8.10.1 Do(): (a) save gs, (b) concat Matrix into CTM,
        // (c) clip to BBox, (d) run form ops, (e) restore gs.
        if self.gs_stack.len() >= self.limits.max_gsave_depth {
            return Err(Error::LimitExceeded {
                limit: "max_gsave_depth",
                max: self.limits.max_gsave_depth as u64,
            });
        }
        self.gs_stack.push(self.gs.clone());
        self.gs.ctm = self.gs.ctm.concat(form_matrix);

        // Clip to BBox in *form space* — build the BBox path in user
        // space and intersect via the same mechanism as W.
        let clip_result = self.intersect_clip_rect(&bbox);

        // Decode and run the form's content stream.
        let run_result = (|| {
            clip_result?;
            let decoded = self.doc.decode_stream(dict, raw)?;
            let ops = crate::content::parse_content_stream(&[decoded], self.limits)?;
            self.xobject_depth += 1;
            let r = self.run(&ops);
            self.xobject_depth -= 1;
            r
        })();

        self.resources = saved_resources;
        if pop_ref {
            self.xobject_stack.pop();
        }
        self.gs = self.gs_stack.pop().expect("pushed above");
        run_result
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

    /// Paint an Image XObject (§8.9). Supports 8-bit
    /// `/DeviceGray`/`/DeviceRGB` with no filter, `/FlateDecode`, or
    /// `/DCTDecode` (baseline JPEG via `jpeg-decoder`). Image masks,
    /// `/Mask`, `/SMask`, `/SMaskInData`, `/OC`, and non-8-bit or
    /// non-device colour spaces are refused.
    fn do_image(&mut self, dict: &Dict, raw: &[u8], name: &[u8]) -> Result<(), Error> {
        self.paint_image(dict, raw, name)
    }

    /// Decode and paint an image. The image is drawn into the unit
    /// square of the *current* user space (§8.9.4 — the CTM maps the
    /// unit square to the destination parallelogram).
    fn paint_image(&mut self, dict: &Dict, raw: &[u8], name: &[u8]) -> Result<(), Error> {
        // ---- required geometry & format --------------------------
        let width = dict
            .get(b"Width".as_slice())
            .and_then(|o| o.as_i64())
            .ok_or_else(|| Error::Structure("image has no /Width".into()))?;
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

        // ---- optional attributes we must not silently ignore -------
        if dict.get(b"ImageMask".as_slice()).and_then(|o| o.as_bool()) == Some(true) {
            return Err(Error::Unsupported {
                feature: "image mask (/ImageMask true)",
                detail: "stencil-masked images are not implemented".into(),
            });
        }
        for key in [&b"Mask"[..], &b"SMask"[..], &b"SMaskInData"[..], &b"OC"[..]] {
            if let Some(v) = dict.get(key) {
                // `SMaskInData 0` is the defined default — not a
                // refusal; the others have no "inert" value.
                if key == b"SMaskInData" && v.as_i64() == Some(0) {
                    continue;
                }
                return Err(Error::Unsupported {
                    feature: "image attribute",
                    detail: format!(
                        "/{} on /{} is not implemented",
                        String::from_utf8_lossy(key),
                        show_name(name)
                    ),
                });
            }
        }

        // ---- colour space & bit depth -----------------------------
        let bpc = dict
            .get(b"BitsPerComponent".as_slice())
            .and_then(|o| o.as_i64())
            .ok_or_else(|| Error::Structure("image has no /BitsPerComponent".into()))?;
        if bpc != 8 {
            return Err(Error::Unsupported {
                feature: "image BitsPerComponent",
                detail: format!("{bpc} (only 8 supported)"),
            });
        }
        let cs_obj = dict
            .get(b"ColorSpace".as_slice())
            .ok_or_else(|| Error::Structure("image has no /ColorSpace".into()))?;
        let cs = self.resolve_image_colorspace(cs_obj)?;
        let ncomps = match cs {
            DeviceSpace::Gray => 1usize,
            DeviceSpace::Rgb => 3usize,
            DeviceSpace::Cmyk => {
                return Err(Error::Unsupported {
                    feature: "image colour space",
                    detail: "DeviceCMYK images are not implemented".into(),
                })
            }
        };

        // ---- decode the sample data --------------------------------
        // /Filter is applied to the raw stream bytes by the image
        // pipeline; predictors ride along via /DecodeParms.
        let decoded = self.decode_image_data(dict, raw)?;
        let expected = (width as u64)
            .checked_mul(height as u64)
            .and_then(|s| s.checked_mul(ncomps as u64))
            .ok_or(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: self.limits.max_image_bytes,
            })?;
        if expected > self.limits.max_image_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: self.limits.max_image_bytes,
            });
        }
        if decoded.len() as u64 != expected {
            return Err(Error::Structure(format!(
                "image /{} data length {} does not match {}x{}x{} = {}",
                show_name(name),
                decoded.len(),
                width,
                height,
                ncomps,
                expected
            )));
        }

        // ---- Decode array -----------------------------------------
        let decode = self.image_decode_array(dict, ncomps)?;

        // ---- paint -------------------------------------------------
        // The image fills the unit square of current user space; the
        // CTM (plus page transform) maps it to the destination. We
        // rasterize by inverse-mapping each covered pixel.
        self.paint_decoded_image(&decoded, width as u32, height as u32, ncomps, &decode)
    }

    /// Resolve an image `/ColorSpace` — a name in the current
    /// `/Resources /ColorSpace` or a direct device-space name.
    fn resolve_image_colorspace(&self, obj: &Object) -> Result<DeviceSpace, Error> {
        match obj {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"DefaultGray" => Ok(DeviceSpace::Gray),
                b"DeviceRGB" | b"DefaultRGB" => Ok(DeviceSpace::Rgb),
                b"DeviceCMYK" | b"DefaultCMYK" => Ok(DeviceSpace::Cmyk),
                other => {
                    // Try the current resources' /ColorSpace dict.
                    let resolved =
                        self.doc
                            .resolve_resource(&self.resources, "ColorSpace", other)?;
                    match resolved {
                        Some(o) => self.resolve_image_colorspace(&o),
                        None => Err(Error::Unsupported {
                            feature: "image colour space",
                            detail: format!(
                                "/{} — only DeviceGray/DeviceRGB are supported",
                                String::from_utf8_lossy(other)
                            ),
                        }),
                    }
                }
            },
            _ => Err(Error::Unsupported {
                feature: "image colour space",
                detail: "non-name colour spaces (arrays, ICCBased, …) are not implemented".into(),
            }),
        }
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
        let pixels = dec
            .decode()
            .map_err(|e| Error::Parse(format!("DCTDecode (JPEG) failed: {e}")))?;
        Ok(pixels)
    }

    /// The image's `/Decode` array (default [0 1] per component).
    fn image_decode_array(&self, dict: &Dict, ncomps: usize) -> Result<Vec<(f64, f64)>, Error> {
        match dict.get(b"Decode".as_slice()) {
            None | Some(Object::Null) => Ok(vec![(0.0, 1.0); ncomps]),
            Some(o) => {
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
                    out.push((lo, hi));
                }
                Ok(out)
            }
        }
    }

    /// Rasterize decoded image samples into the pixmap. The CTM maps
    /// the unit square of user space to the destination; we invert it
    /// to find each covered pixel's source sample (nearest-neighbour —
    /// `/Interpolate` is ignored, which the spec explicitly allows).
    fn paint_decoded_image(
        &mut self,
        data: &[u8],
        w: u32,
        h: u32,
        ncomps: usize,
        decode: &[(f64, f64)],
    ) -> Result<(), Error> {
        let ts = self.paint_ts()?;
        let Some(inv) = ts.invert() else {
            return Err(Error::Unsupported {
                feature: "image transform",
                detail: "CTM is not invertible".into(),
            });
        };
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

        for py in y0..y1 {
            for px in x0..x1 {
                // Pixel centre → user space → image space.
                let mut up = tiny_skia::Point {
                    x: px as f32 + 0.5,
                    y: py as f32 + 0.5,
                };
                inv.map_point(&mut up);
                // Image space (§8.9.4): sample (0,0) is the top-left
                // and sits at user-space (0,1); user-space y runs
                // *up*, image-space y runs *down* — hence 1 - y.
                let sx = (up.x * w as f32) as i64;
                let sy = ((1.0 - up.y) * h as f32) as i64;
                if sx < 0 || sx >= w as i64 || sy < 0 || sy >= h as i64 {
                    continue;
                }
                let si = ((sy as u64 * w as u64 + sx as u64) * ncomps as u64) as usize;
                if si + ncomps > data.len() {
                    continue;
                }
                let comps = &data[si..si + ncomps];
                let (r, g, b) = match ncomps {
                    1 => {
                        let v = apply_decode(comps[0], decode[0]);
                        (v, v, v)
                    }
                    3 => (
                        apply_decode(comps[0], decode[0]),
                        apply_decode(comps[1], decode[1]),
                        apply_decode(comps[2], decode[2]),
                    ),
                    _ => unreachable!("ncomps checked above"),
                };
                // Honour the current clip: only write where the clip
                // mask allows.
                if let Some(mask) = &self.gs.clip {
                    let idx = (py as u64 * mask.width() as u64 + px as u64) as usize;
                    if mask.data().get(idx).copied().unwrap_or(0) == 0 {
                        continue;
                    }
                }
                let i = (py as u64 * pw as u64 + px as u64) as usize * 4;
                let buf = self.pixmap.data_mut();
                buf[i] = r;
                buf[i + 1] = g;
                buf[i + 2] = b;
                buf[i + 3] = 255;
            }
        }
        Ok(())
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
        for &b in bytes {
            self.show_glyph(&font, b, 0.0)?;
        }
        Ok(())
    }

    /// `TJ [item …]`: strings and numeric adjustments interleaved.
    /// A number shifts the *next* glyph — the sign is such that a
    /// positive number moves left (§9.4.3).
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
                    for &b in bytes {
                        self.show_glyph(&font, b, 0.0)?;
                    }
                }
                crate::content::TJItem::Kern(n) => {
                    // A number shifts the *next* glyph: t_j = n/1000 in
                    // text-space units — applied to Tm *before* the
                    // following glyph.
                    let tx = -*n / 1000.0;
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
        font: &Rc<crate::font::SimpleFont>,
        code: u8,
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

        let gs = &mut self.gs;
        let t = &mut gs.text;

        // Resolve code → glyph.
        let glyph = font.glyph_id(code)?;
        let Some(gid) = glyph else { return Ok(()) }; // truly absent

        // Build the glyph path in glyph space, then transform to
        // device space via:
        //   glyph → text:   scale(1/1000 em)
        //   text → render:  Trm = [Tfs·Th 0 0 Tfs 0 Trise] × Tm
        //   render → page:  CTM
        //   page → pixmap:  user_to_pix
        let units = font.units_per_em()?;
        let mut gp = crate::font::GlyphPath::new(units);
        let has_outline = font.outline(gid, &mut gp)?;

        if has_outline {
            if let Some(path) = gp.finish() {
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
                let full = trm_ts.post_concat(self.user_to_pix);
                match t.render_mode {
                    0 | 2 => {
                        // fill (mode 2 also strokes below)
                        let mut p = Paint::default();
                        let c = gs.fill_color;
                        p.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a));
                        p.anti_alias = true;
                        self.pixmap
                            .fill_path(&path, &p, FillRule::Winding, full, gs.clip.as_ref());
                    }
                    _ => {}
                }
                if t.render_mode == 1 || t.render_mode == 2 {
                    // stroke
                    let mut p = Paint::default();
                    let c = gs.stroke_color;
                    p.set_color(tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a));
                    p.anti_alias = true;
                    let stroke = tiny_skia::Stroke {
                        width: (gs.line_width * t.font_size / 1000.0) as f32,
                        ..Default::default()
                    };
                    self.pixmap
                        .stroke_path(&path, &p, &stroke, full, gs.clip.as_ref());
                }
                // mode 3: invisible — paint nothing, still advance.
            }
        }

        // Record the extraction event before advancing — the char's
        // origin is the *current* Tm translation, and its advance is
        // what we are about to add.
        if let Some(buf) = &mut self.extract {
            let w0 = font.width(code);
            let tw = if code == 0x20 { t.word_spacing } else { 0.0 };
            // §9.4.4's tx in *text space* — the value that gets
            // multiplied by Trm's Tfs·Th when mapped to page space:
            //   tx = (w0/1000 − Tj/1000) + Tc + Tw   [text units]
            // Trm then applies × Tfs·Th horizontally, × Tfs vertically.
            let tx_text = w0 / 1000.0 - tj_adjust / 1000.0 + t.char_spacing + tw;
            let trm = Matrix::new(t.font_size * t.h_scale, 0.0, 0.0, t.font_size, 0.0, t.rise)
                .concat(self.text_tm)
                .concat(gs.ctm);
            let origin = trm.transform(0.0, 0.0);
            let (ax, ay) = trm.transform(tx_text, 0.0);
            let advance = (ax - origin.0, ay - origin.1);
            // Glyph bounds: outline in page space, or None if the
            // glyph has no outline (e.g. space).
            let bounds = if has_outline {
                let mut gp2 = crate::font::GlyphPath::new(units);
                if font.outline(gid, &mut gp2).unwrap_or(false) {
                    gp2.finish().map(|p| {
                        let b = p.bounds();
                        let trm2 = Matrix::new(
                            t.font_size * t.h_scale,
                            0.0,
                            0.0,
                            t.font_size,
                            0.0,
                            t.rise,
                        )
                        .concat(self.text_tm)
                        .concat(gs.ctm);
                        let (x0, y0) = trm2.transform(b.left() as f64, b.top() as f64);
                        let (x1, y1) = trm2.transform(b.right() as f64, b.bottom() as f64);
                        let (x2, y2) = trm2.transform(b.left() as f64, b.bottom() as f64);
                        let (x3, y3) = trm2.transform(b.right() as f64, b.top() as f64);
                        (
                            x0.min(x1).min(x2).min(x3),
                            y0.min(y1).min(y2).min(y3),
                            x0.max(x1).max(x2).max(x3),
                            y0.max(y1).max(y2).max(y3),
                        )
                    })
                } else {
                    None
                }
            } else {
                None
            };
            let (unicode, resolved_via) = crate::text_extract::resolve_unicode(font, code);
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
        let tw = if code == 0x20 { t.word_spacing } else { 0.0 };
        let tx =
            ((w0 / 1000.0 - tj_adjust / 1000.0) * t.font_size + t.char_spacing + tw) * t.h_scale;
        let adv = Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
        self.text_tm = adv.concat(self.text_tm);
        Ok(())
    }

    fn paint_path(&mut self, kind: PaintKind) -> Result<(), Error> {
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
                paint.set_color(ts_color(self.gs.stroke_color));
                self.pixmap
                    .stroke_path(&path, &paint, &stroke, ts, self.gs.clip.as_ref());
            }
            PaintKind::Fill(rule) => {
                paint.set_color(ts_color(self.gs.fill_color));
                self.pixmap
                    .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
            }
            PaintKind::FillStroke(rule) => {
                paint.set_color(ts_color(self.gs.fill_color));
                self.pixmap
                    .fill_path(&path, &paint, rule, ts, self.gs.clip.as_ref());
                paint.set_color(ts_color(self.gs.stroke_color));
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

fn gray_to_rgba(g: f64) -> Color {
    let v = to_u8(g);
    Color {
        r: v,
        g: v,
        b: v,
        a: 255,
    }
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
fn cmyk_to_rgba(c: f64, m: f64, y: f64, k: f64) -> Color {
    let c = clamp01(c);
    let m = clamp01(m);
    let y = clamp01(y);
    let k = clamp01(k);
    rgb_to_rgba(
        1.0 - (c + k).min(1.0),
        1.0 - (m + k).min(1.0),
        1.0 - (y + k).min(1.0),
    )
}

fn ts_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
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

/// Apply a `/Decode` range to an 8-bit sample. `x` in [0,255] maps
/// linearly onto [dmin, dmax], clamped to [0,1] for output.
fn apply_decode(x: u8, (dmin, dmax): (f64, f64)) -> u8 {
    let v = dmin + (x as f64 / 255.0) * (dmax - dmin);
    (clamp01(v) * 255.0).round() as u8
}

/// PDF line width 0 is a hairline — "as thin as can be rendered"
/// (§8.4.3.2). tiny-skia treats `Stroke.width == 0` as a hairline
/// automatically, so the value here is a nominal width that will be
/// replaced by the hairline path anyway.
fn hairline_width() -> f32 {
    0.0
}
