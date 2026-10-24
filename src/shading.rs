//! Shadings (§8.7.4.5), all seven types.
//!
//! * Axial (type 2) and radial (type 3) are evaluated per device pixel
//!   by the renderer. The colour along the axis is sampled once into a
//!   256-entry table through the shading's `/Function` and
//!   `/ColorSpace` (so a PostScript tint transform runs 256 times, not
//!   once per pixel); `/Extend`, `/Domain`, `/BBox` and `/Background`
//!   are kept verbatim for the painter.
//! * Function-based (type 1): the 2-in function is sampled on a
//!   [`FN_GRID`]² grid over `/Domain` at parse time and read back with
//!   bilinear interpolation; `/Matrix` maps that domain into shading
//!   space. The grid is the same trade-off as the axial table — a
//!   bounded number of function evaluations per shading instead of one
//!   per pixel.
//! * Mesh shadings (types 4–7) are decoded from their stream (flags,
//!   coordinates and colour components at `/BitsPerFlag`,
//!   `/BitsPerCoordinate`, `/BitsPerComponent`, mapped through
//!   `/Decode`) into triangles (4, 5) or patches (6, 7) in shading
//!   space. The renderer rasterises them in device space
//!   ([`Mesh::rasterize`]): triangles are filled with Gouraud
//!   interpolation of the vertex colours — of the parametric value `t`
//!   when the shading has a `/Function`, which is then looked up in
//!   the table, otherwise of the vertex colours converted to sRGB
//!   (interpolating in sRGB rather than in the source space is the
//!   approximation every raster renderer makes here) — and patches are
//!   split into an N×N grid of bicubic-surface samples whose colours
//!   vary bilinearly in (u, v), with N chosen from the patch's device
//!   size (≤ [`MAX_PATCH_SPLIT`]) and the triangle budget.
//!
//! Bounds: `Limits::max_shading_triangles` caps the triangles a mesh
//! may hold (types 4/5 refuse beyond it; patch splitting is reduced to
//! stay under it) and the rasteriser stops after visiting
//! `8 × max_render_total_pixels` device pixels, so a mesh of many huge
//! overlapping triangles cannot run unbounded.

use std::rc::Rc;

use crate::colorspace::ColorSpace;
use crate::document::Document;
use crate::error::Error;
use crate::function::{parse_function, Function};
use crate::limits::Limits;
use crate::matrix::Matrix;
use crate::object::{Dict, Object};
use crate::page::Rect;

/// Geometry of a shading, in shading space.
#[derive(Debug, Clone)]
pub(crate) enum ShadingKind {
    /// `/ShadingType 2`: colour varies along the axis `(x0,y0)→(x1,y1)`.
    Axial { x0: f64, y0: f64, x1: f64, y1: f64 },
    /// `/ShadingType 3`: colour varies between circle 0 and circle 1.
    Radial {
        x0: f64,
        y0: f64,
        r0: f64,
        x1: f64,
        y1: f64,
        r1: f64,
    },
    /// `/ShadingType 1`: colour is a function of position.
    Function(FunctionShading),
    /// `/ShadingType 4`–`7`: triangles and patches.
    Mesh(Mesh),
}

/// Samples of a function-based shading over its `/Domain`.
#[derive(Debug, Clone)]
pub(crate) struct FunctionShading {
    /// `[x0 x1 y0 y1]`.
    domain: [f64; 4],
    /// Shading space → function domain (inverse of `/Matrix`).
    inv: Matrix,
    /// `FN_GRID × FN_GRID` sRGB8 samples, x fastest.
    grid: Vec<[u8; 3]>,
}

/// Samples per axis of a function-based shading.
const FN_GRID: usize = 128;

impl FunctionShading {
    fn rgb_at(&self, x: f64, y: f64) -> Option<[u8; 3]> {
        let (u, v) = self.inv.transform(x, y);
        let [x0, x1, y0, y1] = self.domain;
        if !(u >= x0 && u <= x1 && v >= y0 && v <= y1) {
            return None;
        }
        let n = (FN_GRID - 1) as f64;
        let fx = ((u - x0) / (x1 - x0) * n).clamp(0.0, n);
        let fy = ((v - y0) / (y1 - y0) * n).clamp(0.0, n);
        let (ix, iy) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - ix as f64, fy - iy as f64);
        let at = |i: usize, j: usize| self.grid[j.min(FN_GRID - 1) * FN_GRID + i.min(FN_GRID - 1)];
        let (c00, c10, c01, c11) = (
            at(ix, iy),
            at(ix + 1, iy),
            at(ix, iy + 1),
            at(ix + 1, iy + 1),
        );
        let mut out = [0u8; 3];
        for k in 0..3 {
            let top = c00[k] as f64 * (1.0 - tx) + c10[k] as f64 * tx;
            let bot = c01[k] as f64 * (1.0 - tx) + c11[k] as f64 * tx;
            out[k] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
        }
        Some(out)
    }
}

/// One mesh vertex in shading space. `c` is the sRGB colour (0..=1) or,
/// for a parametric mesh, `[s, 0, 0]` with `s` the normalised position
/// in the colour table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Vertex {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) c: [f32; 3],
}

/// A Coons / tensor patch: 4×4 Bézier control points `p[i][j]` (`i`
/// along u, `j` along v) and the corner colours at (0,0), (0,1), (1,1),
/// (1,0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Patch {
    p: [[(f64, f64); 4]; 4],
    c: [[f32; 3]; 4],
}

/// Decoded mesh geometry (types 4–7).
#[derive(Debug, Clone, Default)]
pub(crate) struct Mesh {
    tris: Vec<[Vertex; 3]>,
    patches: Vec<Patch>,
    /// Vertex colours are table positions, not colours.
    parametric: bool,
}

/// Most splits per side of a patch.
const MAX_PATCH_SPLIT: usize = 32;

/// Device-space raster of a mesh: `px` holds `0` for unpainted and
/// `0xFF_RRGGBB` for painted pixels, row-major over `w × h` starting
/// at `(x0, y0)`.
pub(crate) struct MeshRaster {
    pub(crate) x0: i32,
    pub(crate) y0: i32,
    pub(crate) w: usize,
    pub(crate) h: usize,
    px: Vec<u32>,
}

impl MeshRaster {
    pub(crate) fn get(&self, x: i32, y: i32) -> Option<[u8; 3]> {
        let (dx, dy) = (x - self.x0, y - self.y0);
        if dx < 0 || dy < 0 || dx as usize >= self.w || dy as usize >= self.h {
            return None;
        }
        let v = self.px[dy as usize * self.w + dx as usize];
        (v != 0).then_some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
    }

    fn put(&mut self, x: i32, y: i32, c: [u8; 3], only_if_empty: bool) {
        let (dx, dy) = (x - self.x0, y - self.y0);
        if dx < 0 || dy < 0 || dx as usize >= self.w || dy as usize >= self.h {
            return;
        }
        let slot = &mut self.px[dy as usize * self.w + dx as usize];
        if only_if_empty && *slot != 0 {
            return;
        }
        *slot = 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
    }
}

impl Mesh {
    /// Rasterise into the device region `[x0, x1) × [y0, y1)`; `map`
    /// takes shading space to device pixels, `lut` is the colour table
    /// of a parametric mesh.
    pub(crate) fn rasterize(
        &self,
        map: &dyn Fn(f64, f64) -> (f64, f64),
        lut: &[[u8; 3]],
        region: (i32, i32, i32, i32),
        limits: &Limits,
    ) -> Result<MeshRaster, Error> {
        // Device bounds of everything, narrowed to the region.
        let mut bb = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        let mut grow = |x: f64, y: f64| {
            bb.0 = bb.0.min(x);
            bb.1 = bb.1.min(y);
            bb.2 = bb.2.max(x);
            bb.3 = bb.3.max(y);
        };
        for t in &self.tris {
            for v in t {
                let (x, y) = map(v.x, v.y);
                grow(x, y);
            }
        }
        for p in &self.patches {
            for row in &p.p {
                for &(x, y) in row {
                    let (dx, dy) = map(x, y);
                    grow(dx, dy);
                }
            }
        }
        let clampi = |v: f64, lo: i32, hi: i32| -> i32 {
            if !v.is_finite() {
                return lo;
            }
            v.clamp(lo as f64, hi as f64) as i32
        };
        let x0 = clampi(bb.0.floor(), region.0, region.2);
        let y0 = clampi(bb.1.floor(), region.1, region.3);
        let x1 = clampi(bb.2.ceil() + 1.0, region.0, region.2);
        let y1 = clampi(bb.3.ceil() + 1.0, region.1, region.3);
        let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
        let mut raster = MeshRaster {
            x0,
            y0,
            w,
            h,
            px: vec![0; w * h],
        };
        if w == 0 || h == 0 {
            return Ok(raster);
        }
        let mut budget = limits.max_render_total_pixels.saturating_mul(8);
        let mut tri_budget = limits.max_shading_triangles;
        // Split count cap per patch side from the share of the triangle
        // budget left after the explicit triangles.
        let n_cap = if self.patches.is_empty() {
            1
        } else {
            let left = tri_budget.saturating_sub(self.tris.len() as u64);
            let per_patch = (left / self.patches.len() as u64).max(2);
            (((per_patch / 2) as f64).sqrt().floor() as usize).clamp(1, MAX_PATCH_SPLIT)
        };
        let colour = |c: [f32; 3]| -> [u8; 3] {
            if self.parametric {
                let i = (c[0].clamp(0.0, 1.0) * (lut.len().max(1) - 1) as f32).round() as usize;
                lut.get(i.min(lut.len().saturating_sub(1)))
                    .copied()
                    .unwrap_or([0; 3])
            } else {
                [to_u8(c[0] as f64), to_u8(c[1] as f64), to_u8(c[2] as f64)]
            }
        };
        let mut draw = |tri: &[Vertex; 3], raster: &mut MeshRaster| -> Result<(), Error> {
            if tri_budget == 0 {
                return Err(Error::LimitExceeded {
                    limit: "max_shading_triangles",
                    max: limits.max_shading_triangles,
                });
            }
            tri_budget -= 1;
            let d: Vec<(f64, f64)> = tri.iter().map(|v| map(v.x, v.y)).collect();
            if d.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
                return Ok(());
            }
            fill_triangle(&d, tri, &colour, raster, &mut budget, limits)
        };
        for tri in &self.tris {
            draw(tri, &mut raster)?;
        }
        if !self.patches.is_empty() {
            for patch in &self.patches {
                let mut ext = 0.0f64;
                let mut pts = [[(0.0, 0.0); 4]; 4];
                for (row, prow) in pts.iter_mut().zip(&patch.p) {
                    for (slot, p) in row.iter_mut().zip(prow) {
                        *slot = map(p.0, p.1);
                    }
                }
                for i in 0..4 {
                    for j in 0..4 {
                        for k in 0..4 {
                            for l in 0..4 {
                                ext = ext.max((pts[i][j].0 - pts[k][l].0).abs());
                                ext = ext.max((pts[i][j].1 - pts[k][l].1).abs());
                            }
                        }
                    }
                }
                if !ext.is_finite() {
                    continue;
                }
                let n = ((ext / 3.0).ceil() as usize).clamp(1, n_cap);
                // Rows of surface samples, two at a time.
                let row = |i: usize| -> Vec<Vertex> {
                    let u = i as f64 / n as f64;
                    (0..=n)
                        .map(|j| {
                            let v = j as f64 / n as f64;
                            let (x, y) = bezier_surface(&patch.p, u, v);
                            Vertex {
                                x,
                                y,
                                c: bilinear(&patch.c, u as f32, v as f32),
                            }
                        })
                        .collect()
                };
                let mut prev = row(0);
                for i in 1..=n {
                    let cur = row(i);
                    for j in 0..n {
                        draw(&[prev[j], prev[j + 1], cur[j]], &mut raster)?;
                        draw(&[prev[j + 1], cur[j + 1], cur[j]], &mut raster)?;
                    }
                    prev = cur;
                }
            }
        }
        Ok(raster)
    }
}

/// Fill one device-space triangle (`d` = device vertices) with
/// Gouraud-interpolated colour at pixel centres. Shared edges are
/// painted by both neighbours (inclusive test with a small tolerance)
/// so adjacent triangles leave no seam; a triangle too small to cover a
/// pixel centre marks the pixel under its centroid unless something
/// larger already painted it.
fn fill_triangle(
    d: &[(f64, f64)],
    tri: &[Vertex; 3],
    colour: &dyn Fn([f32; 3]) -> [u8; 3],
    raster: &mut MeshRaster,
    budget: &mut u64,
    limits: &Limits,
) -> Result<(), Error> {
    let (ax, ay) = d[0];
    let (bx, by) = d[1];
    let (cx, cy) = d[2];
    let area = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
    let minx = ax.min(bx).min(cx);
    let maxx = ax.max(bx).max(cx);
    let miny = ay.min(by).min(cy);
    let maxy = ay.max(by).max(cy);
    let px0 = (minx - 0.5).ceil().max(raster.x0 as f64) as i64;
    let px1 = (maxx - 0.5)
        .floor()
        .min((raster.x0 + raster.w as i32 - 1) as f64) as i64;
    let py0 = (miny - 0.5).ceil().max(raster.y0 as f64) as i64;
    let py1 = (maxy - 0.5)
        .floor()
        .min((raster.y0 + raster.h as i32 - 1) as f64) as i64;
    if area.abs() < 1e-12 || px0 > px1 || py0 > py1 {
        // Degenerate or sub-pixel: the centroid's pixel, colour averaged.
        let (gx, gy) = ((ax + bx + cx) / 3.0, (ay + by + cy) / 3.0);
        let c = [
            (tri[0].c[0] + tri[1].c[0] + tri[2].c[0]) / 3.0,
            (tri[0].c[1] + tri[1].c[1] + tri[2].c[1]) / 3.0,
            (tri[0].c[2] + tri[1].c[2] + tri[2].c[2]) / 3.0,
        ];
        if gx.is_finite() && gy.is_finite() && gx.abs() < 1e9 && gy.abs() < 1e9 {
            raster.put(gx.floor() as i32, gy.floor() as i32, colour(c), true);
        }
        return Ok(());
    }
    let n = (px1 - px0 + 1) as u64 * (py1 - py0 + 1) as u64;
    if *budget < n {
        return Err(Error::LimitExceeded {
            limit: "shading mesh raster pixels",
            max: limits.max_render_total_pixels.saturating_mul(8),
        });
    }
    *budget -= n;
    let inv = 1.0 / area;
    let eps = 1e-3 * area.abs().max(1.0) * inv.abs(); // ~1e-3 of a pixel
    for py in py0..=py1 {
        let y = py as f64 + 0.5;
        for px in px0..=px1 {
            let x = px as f64 + 0.5;
            let w0 = ((bx - x) * (cy - y) - (cx - x) * (by - y)) * inv;
            let w1 = ((cx - x) * (ay - y) - (ax - x) * (cy - y)) * inv;
            let w2 = 1.0 - w0 - w1;
            if w0 < -eps || w1 < -eps || w2 < -eps {
                continue;
            }
            let (w0, w1, w2) = (w0 as f32, w1 as f32, w2 as f32);
            let c = [
                tri[0].c[0] * w0 + tri[1].c[0] * w1 + tri[2].c[0] * w2,
                tri[0].c[1] * w0 + tri[1].c[1] * w1 + tri[2].c[1] * w2,
                tri[0].c[2] * w0 + tri[1].c[2] * w1 + tri[2].c[2] * w2,
            ];
            raster.put(px as i32, py as i32, colour(c), false);
        }
    }
    Ok(())
}

/// Point of the bicubic Bézier surface with control net `p` at (u, v).
fn bezier_surface(p: &[[(f64, f64); 4]; 4], u: f64, v: f64) -> (f64, f64) {
    let bu = bernstein(u);
    let bv = bernstein(v);
    let (mut x, mut y) = (0.0, 0.0);
    for i in 0..4 {
        for j in 0..4 {
            let w = bu[i] * bv[j];
            x += w * p[i][j].0;
            y += w * p[i][j].1;
        }
    }
    (x, y)
}

fn bernstein(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t]
}

/// Bilinear blend of the corner colours (0,0), (0,1), (1,1), (1,0).
fn bilinear(c: &[[f32; 3]; 4], u: f32, v: f32) -> [f32; 3] {
    let w = [(1.0 - u) * (1.0 - v), (1.0 - u) * v, u * v, u * (1.0 - v)];
    let mut out = [0.0f32; 3];
    for k in 0..3 {
        out[k] = w[0] * c[0][k] + w[1] * c[1][k] + w[2] * c[2][k] + w[3] * c[3][k];
    }
    out
}

/// A parsed, sampled shading.
#[derive(Debug)]
pub(crate) struct Shading {
    pub(crate) kind: ShadingKind,
    /// Extend beyond `s < 0` / `s > 1` (axial and radial).
    pub(crate) extend: (bool, bool),
    /// Shading-space clip for both `sh` and pattern use.
    pub(crate) bbox: Option<Rect>,
    /// Colour painted outside the geometry when used as a *pattern*
    /// (never by `sh`), sRGB8.
    pub(crate) background: Option<[u8; 3]>,
    /// Colour at `s = i/255`, sRGB8 (axial, radial, parametric mesh).
    lut: Vec<[u8; 3]>,
    /// Colour-space approximation note (ICC profile not evaluated).
    pub(crate) approximation: Option<String>,
}

const LUT_SIZE: usize = 256;

impl Shading {
    /// Colour for the parametric position `s` (already clamped by the
    /// caller to `0..=1`).
    pub(crate) fn color(&self, s: f64) -> [u8; 3] {
        let i = (s.clamp(0.0, 1.0) * (LUT_SIZE - 1) as f64).round() as usize;
        self.lut.get(i.min(LUT_SIZE - 1)).copied().unwrap_or([0; 3])
    }

    /// The colour table of a parametric mesh.
    pub(crate) fn lut(&self) -> &[[u8; 3]] {
        &self.lut
    }

    /// The mesh of a type 4–7 shading.
    pub(crate) fn mesh(&self) -> Option<&Mesh> {
        match &self.kind {
            ShadingKind::Mesh(m) => Some(m),
            _ => None,
        }
    }

    /// Colour of shading-space point `(x, y)` for every kind but a
    /// mesh (which is rasterised as a whole), or `None` when the point
    /// is not painted.
    pub(crate) fn rgb_at(&self, x: f64, y: f64) -> Option<[u8; 3]> {
        match &self.kind {
            ShadingKind::Function(f) => f.rgb_at(x, y),
            ShadingKind::Mesh(_) => None,
            _ => self.param_at(x, y).map(|s| self.color(s)),
        }
    }

    /// The parametric position of shading-space point `(x, y)`, or
    /// `None` when the point is not painted (outside the axis without
    /// `/Extend`, outside every circle, or a degenerate geometry).
    pub(crate) fn param_at(&self, x: f64, y: f64) -> Option<f64> {
        match self.kind {
            ShadingKind::Axial { x0, y0, x1, y1 } => {
                let dx = x1 - x0;
                let dy = y1 - y0;
                let denom = dx * dx + dy * dy;
                if denom == 0.0 {
                    return None;
                }
                let s = ((x - x0) * dx + (y - y0) * dy) / denom;
                self.extend_param(s)
            }
            ShadingKind::Radial {
                x0,
                y0,
                r0,
                x1,
                y1,
                r1,
            } => {
                // §8.7.4.5.4: the circle at parameter s has centre
                // c(s) = c0 + s·(c1−c0) and radius r(s) = r0 + s·(r1−r0);
                // a point is painted by the largest s with |p − c(s)| =
                // r(s), r(s) ≥ 0, that /Extend admits.
                let cdx = x1 - x0;
                let cdy = y1 - y0;
                let dr = r1 - r0;
                let pdx = x - x0;
                let pdy = y - y0;
                let a = cdx * cdx + cdy * cdy - dr * dr;
                let b = pdx * cdx + pdy * cdy + r0 * dr;
                let c = pdx * pdx + pdy * pdy - r0 * r0;
                let roots: [Option<f64>; 2] = if a.abs() < 1e-9 {
                    if b.abs() < 1e-12 {
                        [None, None]
                    } else {
                        [Some(c / (2.0 * b)), None]
                    }
                } else {
                    let disc = b * b - a * c;
                    if disc < 0.0 {
                        [None, None]
                    } else {
                        let sq = disc.sqrt();
                        let s1 = (b + sq) / a;
                        let s2 = (b - sq) / a;
                        [Some(s1.max(s2)), Some(s1.min(s2))]
                    }
                };
                for s in roots.into_iter().flatten() {
                    if r0 + s * dr < 0.0 {
                        continue;
                    }
                    if let Some(v) = self.extend_param(s) {
                        return Some(v);
                    }
                }
                None
            }
            ShadingKind::Function(_) | ShadingKind::Mesh(_) => None,
        }
    }

    fn extend_param(&self, s: f64) -> Option<f64> {
        if !s.is_finite() {
            return None;
        }
        if s < 0.0 {
            return if self.extend.0 { Some(0.0) } else { None };
        }
        if s > 1.0 {
            return if self.extend.1 { Some(1.0) } else { None };
        }
        Some(s)
    }
}

fn numbers(doc: &Document, o: &Object, what: &str) -> Result<Vec<f64>, Error> {
    let o = doc.deref(o)?;
    let arr = o
        .as_array()
        .ok_or_else(|| Error::Structure(format!("shading {what} must be an array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        let v = doc
            .deref(e)?
            .as_number()
            .ok_or_else(|| Error::Structure(format!("shading {what} entries must be numbers")))?;
        if !v.is_finite() {
            return Err(Error::Structure(format!(
                "shading {what} has a non-finite value"
            )));
        }
        out.push(v);
    }
    Ok(out)
}

fn int_entry(dict: &Dict, key: &[u8], allowed: &[u32]) -> Result<u32, Error> {
    let name = String::from_utf8_lossy(key).into_owned();
    let v = dict
        .get(key)
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure(format!("mesh shading has no integer /{name}")))?;
    if v < 0 || !allowed.contains(&(v as u32)) {
        return Err(Error::Structure(format!(
            "mesh shading /{name} {v} is not one of {allowed:?}"
        )));
    }
    Ok(v as u32)
}

/// Parse a shading dictionary or stream.
pub(crate) fn parse_shading(
    doc: &Document,
    obj: &Object,
    resources: &Object,
    limits: &Limits,
) -> Result<Shading, Error> {
    let obj = doc.deref(obj)?;
    let dict: &Dict = obj
        .as_dict_like()
        .ok_or_else(|| Error::Structure("shading is not a dictionary".into()))?;
    let stype = dict
        .get(b"ShadingType".as_slice())
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure("shading has no /ShadingType".into()))?;
    if !(1..=7).contains(&stype) {
        return Err(Error::Structure(format!(
            "/ShadingType {stype} is not a shading type"
        )));
    }
    let cs_obj = dict
        .get(b"ColorSpace".as_slice())
        .ok_or_else(|| Error::Structure("shading has no /ColorSpace".into()))?;
    let cs = ColorSpace::parse(doc, cs_obj, resources, limits)?;
    if matches!(cs, ColorSpace::Indexed { .. } | ColorSpace::Pattern { .. }) {
        return Err(Error::Structure(
            "shading /ColorSpace may not be Indexed or Pattern".into(),
        ));
    }
    let approximation = cs.approximation();
    let cs = Rc::new(cs);
    let bbox = match dict.get(b"BBox".as_slice()) {
        None | Some(Object::Null) => None,
        Some(o) => {
            let o = doc.deref(o)?;
            let r = Rect::from_object(&o)
                .ok_or_else(|| Error::Structure("shading /BBox is not a rectangle".into()))?;
            if !(r.llx.is_finite() && r.lly.is_finite() && r.urx.is_finite() && r.ury.is_finite()) {
                return Err(Error::Structure(
                    "shading /BBox has non-finite values".into(),
                ));
            }
            Some(r)
        }
    };
    let background = match dict.get(b"Background".as_slice()) {
        None | Some(Object::Null) => None,
        Some(o) => {
            let comps = numbers(doc, o, "/Background")?;
            if comps.len() != cs.ncomps() {
                return Err(Error::Structure(format!(
                    "shading /Background has {} components, colour space has {}",
                    comps.len(),
                    cs.ncomps()
                )));
            }
            let rgb = cs.to_rgb(&comps)?;
            Some([to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])])
        }
    };
    let func = match dict.get(b"Function".as_slice()) {
        Some(f) => Some(parse_function(doc, f, limits)?),
        None => None,
    };
    if let Some(f) = &func {
        if let Some(n) = f.n_outputs() {
            if n != cs.ncomps() {
                return Err(Error::Structure(format!(
                    "shading /Function yields {n} outputs, colour space has {} components",
                    cs.ncomps()
                )));
            }
        }
    }
    let mut lut = Vec::new();
    let kind = match stype {
        1 => {
            let func = func.ok_or_else(|| {
                Error::Structure("function-based shading needs a /Function".into())
            })?;
            if func.n_inputs() != 2 {
                return Err(Error::Structure(
                    "function-based shading /Function must take two inputs (x, y)".into(),
                ));
            }
            let domain = match dict.get(b"Domain".as_slice()) {
                None => [0.0, 1.0, 0.0, 1.0],
                Some(o) => {
                    let d = numbers(doc, o, "/Domain")?;
                    if d.len() != 4 || d[0] >= d[1] || d[2] >= d[3] {
                        return Err(Error::Structure(
                            "function-based shading /Domain must be [x0 x1 y0 y1] with x0 < x1, y0 < y1".into(),
                        ));
                    }
                    [d[0], d[1], d[2], d[3]]
                }
            };
            let matrix = match dict.get(b"Matrix".as_slice()) {
                None | Some(Object::Null) => Matrix::IDENTITY,
                Some(o) => {
                    let m = numbers(doc, o, "/Matrix")?;
                    if m.len() != 6 {
                        return Err(Error::Structure(
                            "shading /Matrix must have six numbers".into(),
                        ));
                    }
                    Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
                }
            };
            let inv = invert(&matrix).ok_or_else(|| {
                Error::Structure("function-based shading /Matrix is singular".into())
            })?;
            let mut grid = Vec::with_capacity(FN_GRID * FN_GRID);
            for j in 0..FN_GRID {
                let y = domain[2] + (domain[3] - domain[2]) * j as f64 / (FN_GRID - 1) as f64;
                for i in 0..FN_GRID {
                    let x = domain[0] + (domain[1] - domain[0]) * i as f64 / (FN_GRID - 1) as f64;
                    let out = func.eval(&[x, y])?;
                    grid.push(rgb8(&cs, &out)?);
                }
            }
            ShadingKind::Function(FunctionShading { domain, inv, grid })
        }
        2 | 3 => {
            let coords = numbers(
                doc,
                dict.get(b"Coords".as_slice())
                    .ok_or_else(|| Error::Structure("shading has no /Coords".into()))?,
                "/Coords",
            )?;
            let kind = if stype == 2 {
                if coords.len() != 4 {
                    return Err(Error::Structure(
                        "axial shading /Coords needs 4 numbers".into(),
                    ));
                }
                ShadingKind::Axial {
                    x0: coords[0],
                    y0: coords[1],
                    x1: coords[2],
                    y1: coords[3],
                }
            } else {
                if coords.len() != 6 {
                    return Err(Error::Structure(
                        "radial shading /Coords needs 6 numbers".into(),
                    ));
                }
                if coords[2] < 0.0 || coords[5] < 0.0 {
                    return Err(Error::Structure("radial shading radii must be ≥ 0".into()));
                }
                ShadingKind::Radial {
                    x0: coords[0],
                    y0: coords[1],
                    r0: coords[2],
                    x1: coords[3],
                    y1: coords[4],
                    r1: coords[5],
                }
            };
            let domain = match dict.get(b"Domain".as_slice()) {
                None => (0.0, 1.0),
                Some(o) => {
                    let d = numbers(doc, o, "/Domain")?;
                    if d.len() != 2 || d[0] >= d[1] {
                        return Err(Error::Structure(
                            "shading /Domain must be [t0 t1] with t0 < t1".into(),
                        ));
                    }
                    (d[0], d[1])
                }
            };
            let func = func
                .ok_or_else(|| Error::Structure("axial/radial shading needs a /Function".into()))?;
            if func.n_inputs() != 1 {
                return Err(Error::Structure(
                    "shading /Function must take one input (t)".into(),
                ));
            }
            lut = sample_lut(&func, &cs, domain)?;
            kind
        }
        _ => {
            let (sdict, raw) = obj.as_stream().ok_or_else(|| {
                Error::Structure(format!("/ShadingType {stype} must be a stream"))
            })?;
            let data = doc.decode_stream(sdict, raw)?;
            let bpc = int_entry(dict, b"BitsPerCoordinate", &[1, 2, 4, 8, 12, 16, 24, 32])?;
            let bpcomp = int_entry(dict, b"BitsPerComponent", &[1, 2, 4, 8, 12, 16])?;
            let bpf = if stype == 5 {
                0
            } else {
                int_entry(dict, b"BitsPerFlag", &[2, 4, 8])?
            };
            let ncomp = if func.is_some() { 1 } else { cs.ncomps() };
            let decode = numbers(
                doc,
                dict.get(b"Decode".as_slice())
                    .ok_or_else(|| Error::Structure("mesh shading has no /Decode".into()))?,
                "/Decode",
            )?;
            if decode.len() != 4 + 2 * ncomp {
                return Err(Error::Structure(format!(
                    "mesh shading /Decode has {} numbers, needs {}",
                    decode.len(),
                    4 + 2 * ncomp
                )));
            }
            let decode: Vec<(f64, f64)> = decode.chunks(2).map(|c| (c[0], c[1])).collect();
            if let Some(f) = &func {
                if f.n_inputs() != 1 {
                    return Err(Error::Structure(
                        "mesh shading /Function must take one input (t)".into(),
                    ));
                }
                let (t0, t1) = decode[2];
                if t0 >= t1 {
                    return Err(Error::Structure(
                        "mesh shading /Decode range for t must be increasing".into(),
                    ));
                }
                lut = sample_lut(f, &cs, (t0, t1))?;
            }
            let params = MeshParams {
                bpc,
                bpcomp,
                bpf,
                decode,
                parametric: func.is_some(),
                cs: &cs,
                max_tris: limits.max_shading_triangles,
            };
            let mut reader = BitReader {
                data: &data,
                pos: 0,
            };
            let mesh = match stype {
                4 => decode_type4(&params, &mut reader)?,
                5 => {
                    let vpr = dict
                        .get(b"VerticesPerRow".as_slice())
                        .and_then(|o| o.as_i64())
                        .filter(|v| *v >= 2)
                        .ok_or_else(|| {
                            Error::Structure(
                                "lattice-form shading /VerticesPerRow must be ≥ 2".into(),
                            )
                        })?;
                    decode_type5(&params, &mut reader, vpr as usize)?
                }
                _ => decode_patches(&params, &mut reader, stype == 7)?,
            };
            ShadingKind::Mesh(mesh)
        }
    };
    let extend = match dict.get(b"Extend".as_slice()) {
        None => (false, false),
        Some(o) => {
            let o = doc.deref(o)?;
            let a = o
                .as_array()
                .ok_or_else(|| Error::Structure("shading /Extend must be an array".into()))?;
            if a.len() != 2 {
                return Err(Error::Structure(
                    "shading /Extend needs two booleans".into(),
                ));
            }
            let e0 = a[0].as_bool().ok_or_else(|| {
                Error::Structure("shading /Extend entries must be booleans".into())
            })?;
            let e1 = a[1].as_bool().ok_or_else(|| {
                Error::Structure("shading /Extend entries must be booleans".into())
            })?;
            (e0, e1)
        }
    };
    Ok(Shading {
        kind,
        extend,
        bbox,
        background,
        lut,
        approximation,
    })
}

fn sample_lut(func: &Function, cs: &ColorSpace, domain: (f64, f64)) -> Result<Vec<[u8; 3]>, Error> {
    let mut lut = Vec::with_capacity(LUT_SIZE);
    for i in 0..LUT_SIZE {
        let t = domain.0 + (domain.1 - domain.0) * i as f64 / (LUT_SIZE - 1) as f64;
        let out = func.eval(&[t])?;
        lut.push(rgb8(cs, &out)?);
    }
    Ok(lut)
}

fn rgb8(cs: &ColorSpace, comps: &[f64]) -> Result<[u8; 3], Error> {
    if comps.len() != cs.ncomps() {
        return Err(Error::Structure(format!(
            "shading /Function yields {} outputs, colour space has {} components",
            comps.len(),
            cs.ncomps()
        )));
    }
    let rgb = cs.to_rgb(comps)?;
    Ok([to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])])
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m.a * m.d - m.b * m.c;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    Some(Matrix::new(
        m.d / det,
        -m.b / det,
        -m.c / det,
        m.a / det,
        (m.c * m.f - m.d * m.e) / det,
        (m.b * m.e - m.a * m.f) / det,
    ))
}

fn to_u8(v: f64) -> u8 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0
    }
}

// ------------------------------------------------------------------
// mesh stream decoding
// ------------------------------------------------------------------

struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl BitReader<'_> {
    fn remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    fn read(&mut self, bits: u32) -> Option<u64> {
        if bits == 0 {
            return Some(0);
        }
        if self.remaining() < bits as usize {
            return None;
        }
        let mut v: u64 = 0;
        for _ in 0..bits {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u64;
            self.pos += 1;
        }
        Some(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

struct MeshParams<'a> {
    bpc: u32,
    bpcomp: u32,
    bpf: u32,
    /// `(min, max)` for x, y, then each colour component (or t).
    decode: Vec<(f64, f64)>,
    parametric: bool,
    cs: &'a ColorSpace,
    max_tris: u64,
}

impl MeshParams<'_> {
    fn ncomp(&self) -> usize {
        self.decode.len() - 2
    }

    fn vertex_bits(&self) -> usize {
        2 * self.bpc as usize + self.ncomp() * self.bpcomp as usize
    }

    fn dec(&self, raw: u64, bits: u32, i: usize) -> f64 {
        let (lo, hi) = self.decode[i];
        let max = if bits >= 64 {
            u64::MAX as f64
        } else {
            ((1u64 << bits) - 1) as f64
        };
        lo + raw as f64 * (hi - lo) / max
    }

    fn point(&self, r: &mut BitReader) -> Option<(f64, f64)> {
        let x = r.read(self.bpc)?;
        let y = r.read(self.bpc)?;
        Some((self.dec(x, self.bpc, 0), self.dec(y, self.bpc, 1)))
    }

    fn colour(&self, r: &mut BitReader) -> Result<Option<[f32; 3]>, Error> {
        let mut comps = Vec::with_capacity(self.ncomp());
        for i in 0..self.ncomp() {
            let Some(raw) = r.read(self.bpcomp) else {
                return Ok(None);
            };
            comps.push(self.dec(raw, self.bpcomp, 2 + i));
        }
        if self.parametric {
            let (t0, t1) = self.decode[2];
            let s = ((comps[0] - t0) / (t1 - t0)).clamp(0.0, 1.0);
            return Ok(Some([s as f32, 0.0, 0.0]));
        }
        let rgb = self.cs.to_rgb(&comps)?;
        Ok(Some([rgb[0] as f32, rgb[1] as f32, rgb[2] as f32]))
    }

    fn vertex(&self, r: &mut BitReader) -> Result<Option<Vertex>, Error> {
        if r.remaining() < self.vertex_bits() {
            return Ok(None);
        }
        let Some((x, y)) = self.point(r) else {
            return Ok(None);
        };
        let Some(c) = self.colour(r)? else {
            return Ok(None);
        };
        Ok(Some(Vertex { x, y, c }))
    }

    fn check_tris(&self, n: usize) -> Result<(), Error> {
        if n as u64 > self.max_tris {
            return Err(Error::LimitExceeded {
                limit: "max_shading_triangles",
                max: self.max_tris,
            });
        }
        Ok(())
    }
}

/// §8.7.4.5.5: each vertex carries an edge flag; 0 starts a triangle
/// (two more vertices follow), 1 and 2 reuse the previous triangle's
/// (vb, vc) or (va, vc). Every vertex starts on a byte boundary.
fn decode_type4(p: &MeshParams, r: &mut BitReader) -> Result<Mesh, Error> {
    let mut mesh = Mesh {
        parametric: p.parametric,
        ..Default::default()
    };
    let mut prev: Option<[Vertex; 3]> = None;
    while r.remaining() >= p.bpf as usize + p.vertex_bits() {
        let flag = r.read(p.bpf).unwrap_or(0);
        let Some(v) = p.vertex(r)? else { break };
        r.align();
        let tri = match (flag, prev) {
            (0, _) => {
                let mut rest = [v, v];
                let mut complete = true;
                for slot in rest.iter_mut() {
                    if r.remaining() < p.bpf as usize + p.vertex_bits() {
                        complete = false;
                        break;
                    }
                    r.read(p.bpf);
                    match p.vertex(r)? {
                        Some(w) => *slot = w,
                        None => {
                            complete = false;
                            break;
                        }
                    }
                    r.align();
                }
                if !complete {
                    break;
                }
                [v, rest[0], rest[1]]
            }
            (1, Some(t)) => [t[1], t[2], v],
            (2, Some(t)) => [t[0], t[2], v],
            (1 | 2, None) => {
                return Err(Error::Structure(
                    "free-form mesh: edge flag 1/2 before any triangle".into(),
                ))
            }
            (f, _) => {
                return Err(Error::Structure(format!(
                    "free-form mesh: edge flag {f} is not 0, 1 or 2"
                )))
            }
        };
        p.check_tris(mesh.tris.len() + 1)?;
        mesh.tris.push(tri);
        prev = Some(tri);
    }
    Ok(mesh)
}

/// §8.7.4.5.6: vertices in rows of `vpr`, no flags, packed without
/// per-vertex padding; adjacent rows form two triangles per cell.
fn decode_type5(p: &MeshParams, r: &mut BitReader, vpr: usize) -> Result<Mesh, Error> {
    let mut verts = Vec::new();
    while let Some(v) = p.vertex(r)? {
        verts.push(v);
        if verts.len() > 3 * p.max_tris as usize + vpr {
            return Err(Error::LimitExceeded {
                limit: "max_shading_triangles",
                max: p.max_tris,
            });
        }
    }
    let rows = verts.len() / vpr;
    let mut mesh = Mesh {
        parametric: p.parametric,
        ..Default::default()
    };
    if rows >= 2 {
        p.check_tris((rows - 1) * (vpr - 1) * 2)?;
    }
    for i in 1..rows {
        let (a, b) = (
            &verts[(i - 1) * vpr..i * vpr],
            &verts[i * vpr..(i + 1) * vpr],
        );
        for j in 0..vpr - 1 {
            mesh.tris.push([a[j], a[j + 1], b[j]]);
            mesh.tris.push([a[j + 1], b[j + 1], b[j]]);
        }
    }
    Ok(mesh)
}

/// §8.7.4.5.7 / .8: one flag per patch; 12 (Coons) or 16 (tensor)
/// points and 4 colours for a new patch, 8 / 12 points and 2 colours
/// when an edge is shared with the previous patch (`f` = 1, 2, 3 picks
/// which). Each patch starts on a byte boundary.
fn decode_patches(p: &MeshParams, r: &mut BitReader, tensor: bool) -> Result<Mesh, Error> {
    let mut mesh = Mesh {
        parametric: p.parametric,
        ..Default::default()
    };
    // Boundary points in stream order: p11 p12 p13 p14 p24 p34 p44 p43
    // p42 p41 p31 p21; colours c1..c4 at p11, p14, p44, p41.
    let mut b = [(0.0f64, 0.0f64); 12];
    let mut c = [[0.0f32; 3]; 4];
    let mut have_prev = false;
    loop {
        if r.remaining() < p.bpf as usize {
            break;
        }
        let Some(flag) = r.read(p.bpf) else { break };
        let (npts, ncol) = match flag {
            0 => (if tensor { 16 } else { 12 }, 4),
            1..=3 => (if tensor { 12 } else { 8 }, 2),
            f => {
                return Err(Error::Structure(format!(
                    "patch mesh: edge flag {f} is not 0–3"
                )))
            }
        };
        let need = npts * 2 * p.bpc as usize + ncol * p.ncomp() * p.bpcomp as usize;
        if r.remaining() < need {
            break;
        }
        if flag != 0 && !have_prev {
            return Err(Error::Structure(
                "patch mesh: edge flag 1–3 before any patch".into(),
            ));
        }
        let (prev_b, prev_c) = (b, c);
        let start = match flag {
            0 => 0,
            1 => {
                b[..4].copy_from_slice(&[prev_b[3], prev_b[4], prev_b[5], prev_b[6]]);
                c[0] = prev_c[1];
                c[1] = prev_c[2];
                4
            }
            2 => {
                b[..4].copy_from_slice(&[prev_b[6], prev_b[7], prev_b[8], prev_b[9]]);
                c[0] = prev_c[2];
                c[1] = prev_c[3];
                4
            }
            _ => {
                b[..4].copy_from_slice(&[prev_b[9], prev_b[10], prev_b[11], prev_b[0]]);
                c[0] = prev_c[3];
                c[1] = prev_c[0];
                4
            }
        };
        for slot in b.iter_mut().skip(start) {
            *slot = p
                .point(r)
                .ok_or_else(|| Error::Structure("patch mesh: truncated".into()))?;
        }
        let mut interior = [(0.0f64, 0.0f64); 4];
        if tensor {
            for slot in interior.iter_mut() {
                *slot = p
                    .point(r)
                    .ok_or_else(|| Error::Structure("patch mesh: truncated".into()))?;
            }
        }
        for slot in c.iter_mut().skip(start / 2) {
            *slot = p
                .colour(r)?
                .ok_or_else(|| Error::Structure("patch mesh: truncated".into()))?;
        }
        r.align();
        have_prev = true;
        // Control net p[i][j], i along u (p11→p41), j along v (p11→p14).
        let mut g = [[(0.0f64, 0.0f64); 4]; 4];
        g[0][0] = b[0];
        g[0][1] = b[1];
        g[0][2] = b[2];
        g[0][3] = b[3];
        g[1][3] = b[4];
        g[2][3] = b[5];
        g[3][3] = b[6];
        g[3][2] = b[7];
        g[3][1] = b[8];
        g[3][0] = b[9];
        g[2][0] = b[10];
        g[1][0] = b[11];
        if tensor {
            // Stream order p22 p23 p33 p32.
            g[1][1] = interior[0];
            g[1][2] = interior[1];
            g[2][2] = interior[2];
            g[2][1] = interior[3];
        } else {
            // Coons interior points (§8.7.4.5.7).
            let lin = |terms: &[(f64, (f64, f64))]| -> (f64, f64) {
                let (mut x, mut y) = (0.0, 0.0);
                for (k, (px, py)) in terms {
                    x += k * px;
                    y += k * py;
                }
                (x / 9.0, y / 9.0)
            };
            g[1][1] = lin(&[
                (-4.0, g[0][0]),
                (6.0, g[0][1]),
                (6.0, g[1][0]),
                (-2.0, g[0][3]),
                (-2.0, g[3][0]),
                (3.0, g[3][1]),
                (3.0, g[1][3]),
                (-1.0, g[3][3]),
            ]);
            g[1][2] = lin(&[
                (-4.0, g[0][3]),
                (6.0, g[0][2]),
                (6.0, g[1][3]),
                (-2.0, g[0][0]),
                (-2.0, g[3][3]),
                (3.0, g[3][2]),
                (3.0, g[1][0]),
                (-1.0, g[3][0]),
            ]);
            g[2][1] = lin(&[
                (-4.0, g[3][0]),
                (6.0, g[3][1]),
                (6.0, g[2][0]),
                (-2.0, g[3][3]),
                (-2.0, g[0][0]),
                (3.0, g[0][1]),
                (3.0, g[2][3]),
                (-1.0, g[0][3]),
            ]);
            g[2][2] = lin(&[
                (-4.0, g[3][3]),
                (6.0, g[3][2]),
                (6.0, g[2][3]),
                (-2.0, g[3][0]),
                (-2.0, g[0][3]),
                (3.0, g[0][2]),
                (3.0, g[2][0]),
                (-1.0, g[0][0]),
            ]);
        }
        // Each patch yields at least two triangles when rasterised.
        p.check_tris((mesh.patches.len() + 1) * 2)?;
        mesh.patches.push(Patch { p: g, c });
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_ramp(kind: ShadingKind, extend: (bool, bool)) -> Shading {
        Shading {
            kind,
            extend,
            bbox: None,
            background: None,
            lut: (0..LUT_SIZE).map(|i| [i as u8; 3]).collect(),
            approximation: None,
        }
    }

    #[test]
    fn axial_param_and_extend() {
        let s = gray_ramp(
            ShadingKind::Axial {
                x0: 0.0,
                y0: 0.0,
                x1: 10.0,
                y1: 0.0,
            },
            (false, true),
        );
        assert_eq!(s.param_at(5.0, 3.0), Some(0.5));
        assert_eq!(s.param_at(-1.0, 0.0), None);
        assert_eq!(s.param_at(11.0, 0.0), Some(1.0));
        assert_eq!(s.color(0.5), [128; 3]);
    }

    #[test]
    fn radial_concentric() {
        let s = gray_ramp(
            ShadingKind::Radial {
                x0: 0.0,
                y0: 0.0,
                r0: 0.0,
                x1: 0.0,
                y1: 0.0,
                r1: 10.0,
            },
            (false, false),
        );
        assert!((s.param_at(5.0, 0.0).unwrap() - 0.5).abs() < 1e-9);
        assert!((s.param_at(0.0, 10.0).unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(s.param_at(20.0, 0.0), None);
        let e = gray_ramp(s.kind.clone(), (false, true));
        assert_eq!(e.param_at(20.0, 0.0), Some(1.0));
    }

    #[test]
    fn degenerate_axis_paints_nothing() {
        let s = gray_ramp(
            ShadingKind::Axial {
                x0: 1.0,
                y0: 1.0,
                x1: 1.0,
                y1: 1.0,
            },
            (true, true),
        );
        assert_eq!(s.param_at(0.0, 0.0), None);
    }

    #[test]
    fn bit_reader_msb_first_and_align() {
        let mut r = BitReader {
            data: &[0b1011_0000, 0xFF],
            pos: 0,
        };
        assert_eq!(r.read(2), Some(0b10));
        assert_eq!(r.read(2), Some(0b11));
        r.align();
        assert_eq!(r.read(8), Some(0xFF));
        assert_eq!(r.read(1), None);
    }

    #[test]
    fn triangle_raster_covers_interior_and_shared_edges() {
        let v = |x: f64, y: f64, c: f32| Vertex { x, y, c: [c, c, c] };
        let mesh = Mesh {
            tris: vec![
                [v(0.0, 0.0, 0.0), v(10.0, 0.0, 1.0), v(0.0, 10.0, 1.0)],
                [v(10.0, 0.0, 1.0), v(10.0, 10.0, 1.0), v(0.0, 10.0, 1.0)],
            ],
            patches: vec![],
            parametric: false,
        };
        let r = mesh
            .rasterize(&|x, y| (x, y), &[], (0, 0, 20, 20), &Limits::default())
            .unwrap();
        // Every pixel of the 10×10 square is painted, nothing outside.
        for y in 0..10 {
            for x in 0..10 {
                assert!(r.get(x, y).is_some(), "({x},{y})");
            }
        }
        assert!(r.get(10, 5).is_none());
        assert!(r.get(5, 10).is_none());
        // Pixel centre (0.5, 0.5): weights 0.9/0.05/0.05 → 0.1.
        assert_eq!(r.get(0, 0), Some([26, 26, 26]));
        assert_eq!(r.get(9, 9), Some([255, 255, 255]));
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn coons_square_patch_interpolates_corner_colours() {
        // Straight-edged unit square (control points at thirds).
        let lerp =
            |a: (f64, f64), b: (f64, f64), t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let (p00, p03, p33, p30) = ((0.0, 0.0), (0.0, 90.0), (90.0, 90.0), (90.0, 0.0));
        let mut g = [[(0.0, 0.0); 4]; 4];
        for j in 0..4 {
            g[0][j] = lerp(p00, p03, j as f64 / 3.0);
            g[3][j] = lerp(p30, p33, j as f64 / 3.0);
        }
        for i in 0..4 {
            g[i][0] = lerp(p00, p30, i as f64 / 3.0);
            g[i][3] = lerp(p03, p33, i as f64 / 3.0);
        }
        // Coons interior of a bilinear patch is the bilinear point.
        g[1][1] = (30.0, 30.0);
        g[1][2] = (30.0, 60.0);
        g[2][1] = (60.0, 30.0);
        g[2][2] = (60.0, 60.0);
        let mesh = Mesh {
            tris: vec![],
            patches: vec![Patch {
                p: g,
                c: [[0.0; 3], [1.0, 0.0, 0.0], [1.0; 3], [0.0, 0.0, 1.0]],
            }],
            parametric: false,
        };
        let r = mesh
            .rasterize(&|x, y| (x, y), &[], (0, 0, 100, 100), &Limits::default())
            .unwrap();
        assert_eq!(bezier_surface(&g, 0.5, 0.5), (45.0, 45.0));
        let c = r.get(0, 0).unwrap();
        assert!(c.iter().all(|v| *v <= 3), "{c:?}");
        let c = r.get(45, 45).unwrap();
        assert!(
            (c[0] as i32 - 128).abs() <= 6 && (c[1] as i32 - 64).abs() <= 6,
            "{c:?}"
        );
        assert!(r.get(95, 95).is_none());
        // Near (0, 1) in (u, v) — x≈0, y≈89 — red.
        let c = r.get(1, 88).unwrap();
        assert!(c[0] > 220 && c[1] < 30 && c[2] < 30, "{c:?}");
    }
}
