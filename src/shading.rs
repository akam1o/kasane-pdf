//! Shadings (§8.7.4.5): axial (type 2) and radial (type 3), evaluated
//! per device pixel by the renderer. The colour along the axis is
//! sampled once into a 256-entry table through the shading's
//! `/Function` and `/ColorSpace` (so a PostScript tint transform
//! runs 256 times, not once per pixel); `/Extend`, `/Domain`,
//! `/BBox` and `/Background` are kept verbatim for the painter.
//!
//! Types 1 and 4–7 are refused as `Unsupported` — they need a
//! different rasterisation (function sampling over a 2-D domain,
//! mesh triangles, Coons patches), not an approximation of this one.

use std::rc::Rc;

use crate::colorspace::ColorSpace;
use crate::document::Document;
use crate::error::Error;
use crate::function::parse_function;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use crate::page::Rect;

/// Geometry of a supported shading, in shading space.
#[derive(Debug, Clone, Copy)]
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
}

/// A parsed, sampled shading.
#[derive(Debug)]
pub(crate) struct Shading {
    pub(crate) kind: ShadingKind,
    /// Extend beyond `s < 0` / `s > 1`.
    pub(crate) extend: (bool, bool),
    /// Shading-space clip for both `sh` and pattern use.
    pub(crate) bbox: Option<Rect>,
    /// Colour painted outside the geometry when used as a *pattern*
    /// (never by `sh`), sRGB8.
    pub(crate) background: Option<[u8; 3]>,
    /// Colour at `s = i/255`, sRGB8.
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
        self.lut[i.min(LUT_SIZE - 1)]
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

/// Parse a shading dictionary (or stream — types 4–7 are streams and
/// refused either way).
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
    let kind = match stype {
        2 | 3 => {
            let coords = numbers(
                doc,
                dict.get(b"Coords".as_slice())
                    .ok_or_else(|| Error::Structure("shading has no /Coords".into()))?,
                "/Coords",
            )?;
            if stype == 2 {
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
            }
        }
        1 | 4 | 5 | 6 | 7 => {
            return Err(Error::Unsupported {
                feature: "shading type",
                detail: format!(
                    "/ShadingType {stype} ({}) is not implemented",
                    match stype {
                        1 => "function-based",
                        4 => "free-form triangle mesh",
                        5 => "lattice-form triangle mesh",
                        6 => "Coons patch mesh",
                        _ => "tensor-product patch mesh",
                    }
                ),
            })
        }
        other => {
            return Err(Error::Structure(format!(
                "/ShadingType {other} is not a shading type"
            )))
        }
    };
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
    let fn_obj = dict
        .get(b"Function".as_slice())
        .ok_or_else(|| Error::Structure("axial/radial shading needs a /Function".into()))?;
    let func = parse_function(doc, fn_obj, limits)?;
    if func.n_inputs() != 1 {
        return Err(Error::Structure(
            "shading /Function must take one input (t)".into(),
        ));
    }
    if let Some(n) = func.n_outputs() {
        if n != cs.ncomps() {
            return Err(Error::Structure(format!(
                "shading /Function yields {n} outputs, colour space has {} components",
                cs.ncomps()
            )));
        }
    }
    let mut lut = Vec::with_capacity(LUT_SIZE);
    for i in 0..LUT_SIZE {
        let t = domain.0 + (domain.1 - domain.0) * i as f64 / (LUT_SIZE - 1) as f64;
        let out = func.eval(&[t])?;
        if out.len() != cs.ncomps() {
            return Err(Error::Structure(format!(
                "shading /Function yields {} outputs, colour space has {} components",
                out.len(),
                cs.ncomps()
            )));
        }
        let rgb = cs.to_rgb(&out)?;
        lut.push([to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2])]);
    }
    Ok(Shading {
        kind,
        extend,
        bbox,
        background,
        lut,
        approximation,
    })
}

fn to_u8(v: f64) -> u8 {
    if v.is_finite() {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0
    }
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
        let e = gray_ramp(s.kind, (false, true));
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
}
