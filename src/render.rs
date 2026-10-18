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
//! Path model (§8.4.3.1): path elements are stored in *user space*
//! (untransformed) and the paint-time `CTM × page→pixmap` is applied
//! when the path is consumed — so a `cm` between construction and
//! painting moves the whole current path, matching poppler and the
//! spec's wording that a path is transformed by the CTM "in effect
//! when it is painted".
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
use crate::object::{Name, Object};
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
    stroke_space: DeviceSpace,
    fill_space: DeviceSpace,
    stroke_color: Color,
    fill_color: Color,
    /// Current clip as a pixmap-space alpha mask; `None` = page bounds
    /// only. Stacked clips intersect via `Mask::intersect_path`.
    clip: Option<Mask>,
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
        }
    }
}

/// A pending path being built by path-construction operators.
///
/// Stored in *user space* (untransformed): per §8.4.3.1 and ISO
/// 32000-1 §8.4.3.1's note, a `cm` between path construction and
/// painting applies to the whole current path — elements are not
/// fixed at append time. The path is transformed once by
/// `CTM_paint × page→pixmap` when it is consumed.
#[derive(Default)]
struct CurrentPath {
    builder: PathBuilder,
    has_open_subpath: bool,
    elements: usize,
    /// `W`/`W*` mark the path as a clip operand; the clip is installed
    /// at the *next* painting operator per §8.5.4.
    pending_clip: Option<FillRule>,
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
    }
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
    let mut renderer = Renderer {
        limits,
        doc,
        page,
        pixmap: &mut pixmap,
        user_to_pix,
        gs: GraphicsState::new(),
        gs_stack: Vec::new(),
        path: CurrentPath::default(),
        in_text: false,
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

struct Renderer<'a> {
    limits: &'a Limits,
    doc: &'a Document,
    page: &'a Page,
    pixmap: &'a mut Pixmap,
    /// page→pixmap transform (crop offset, rotate, dpi scale, y flip).
    user_to_pix: Transform,
    gs: GraphicsState,
    gs_stack: Vec<GraphicsState>,
    path: CurrentPath,
    in_text: bool,
}

impl<'a> Renderer<'a> {
    fn run(&mut self, ops: &[Operation]) -> Result<(), Error> {
        for op in ops {
            self.execute(op)?;
        }
        Ok(())
    }

    fn execute(&mut self, op: &Operation) -> Result<(), Error> {
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
                self.path.bump_elements(self.limits)?;
                self.path.builder.move_to(*x as f32, *y as f32);
                self.path.has_open_subpath = true;
            }
            Operation::LineTo(x, y) => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.line_to(*x as f32, *y as f32);
                self.path.has_open_subpath = true;
            }
            Operation::CurveTo(x1, y1, x2, y2, x3, y3) => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x1 as f32, *y1 as f32, *x2 as f32, *y2 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::CurveToV(x2, y2, x3, y3) => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x2 as f32, *y2 as f32, *x2 as f32, *y2 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::CurveToY(x1, y1, x3, y3) => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.cubic_to(
                    *x1 as f32, *y1 as f32, *x3 as f32, *y3 as f32, *x3 as f32, *y3 as f32,
                );
                self.path.has_open_subpath = true;
            }
            Operation::ClosePath => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.close();
                self.path.has_open_subpath = false;
            }
            Operation::Rectangle(x, y, w, h) => {
                self.path.bump_elements(self.limits)?;
                self.path.builder.move_to(*x as f32, *y as f32);
                self.path.builder.line_to((*x + *w) as f32, *y as f32);
                self.path
                    .builder
                    .line_to((*x + *w) as f32, (*y + *h) as f32);
                self.path.builder.line_to(*x as f32, (*y + *h) as f32);
                self.path.builder.close();
                self.path.has_open_subpath = false;
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
            Operation::Clip => self.path.pending_clip = Some(FillRule::Winding),
            Operation::ClipEvenOdd => self.path.pending_clip = Some(FillRule::EvenOdd),

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
            Operation::BeginText => self.in_text = true,
            Operation::EndText => self.in_text = false,
            // State-only text ops are harmless until a show op runs.
            Operation::SetCharSpacing(_)
            | Operation::SetWordSpacing(_)
            | Operation::SetHorizontalScaling(_)
            | Operation::SetTextLeading(_)
            | Operation::SetTextRenderingMode(_)
            | Operation::SetTextRise(_)
            | Operation::MoveTextPosition(_, _)
            | Operation::MoveTextPositionAndSetLeading(_, _)
            | Operation::SetTextMatrix(_)
            | Operation::NextLine => {
                let _ = self.in_text;
            }
            Operation::SetTextFont { .. } => {
                return Err(Error::Unsupported {
                    feature: "text font selection (Tf)",
                    detail: "font loading and text rendering are not implemented in M3".into(),
                })
            }
            Operation::ShowText(_)
            | Operation::ShowTextAdjusted(_)
            | Operation::NextLineShowText(_)
            | Operation::SetSpacingNextLineShowText { .. } => {
                return Err(Error::Unsupported {
                    feature: "text drawing (Tj/TJ/'/\" )",
                    detail: "text rendering is not implemented in M3".into(),
                })
            }

            // ---- XObject / shading / ExtGState --------------------
            Operation::Do(n) => {
                return Err(Error::Unsupported {
                    feature: "XObject invocation (Do)",
                    detail: format!(
                        "/{} — Form XObject and image drawing are not implemented in M3",
                        show_name(n)
                    ),
                })
            }
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
                let props = self.doc.page_resource(self.page, "Properties", n)?;
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
    fn paint_ts(&self) -> Result<Transform, Error> {
        let ctm_ts = ctm_to_transform(&self.gs.ctm)
            .ok_or_else(|| Error::Structure("non-finite CTM".into()))?;
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

fn show_name(n: &Name) -> String {
    String::from_utf8_lossy(n).into_owned()
}

fn ctm_to_transform(m: &Matrix) -> Option<Transform> {
    if !m.is_finite() {
        return None;
    }
    Some(Transform::from_row(
        m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32,
    ))
}

/// PDF line width 0 is a hairline — "as thin as can be rendered"
/// (§8.4.3.2). tiny-skia treats `Stroke.width == 0` as a hairline
/// automatically, so the value here is a nominal width that will be
/// replaced by the hairline path anyway.
fn hairline_width() -> f32 {
    0.0
}
