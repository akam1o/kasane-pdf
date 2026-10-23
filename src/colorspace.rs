//! Colour spaces (§8.6): every family a content stream or image can
//! select, resolved once into a [`ColorSpace`] that converts
//! components → sRGB with a closed formula.
//!
//! * Device spaces: the existing Gray/RGB/CMYK rules.
//! * CIE-based: CalGray, CalRGB, Lab via XYZ → Bradford D65 → sRGB.
//!   ICCBased uses the embedded profile when [`crate::icc`] can
//!   evaluate it exactly (matrix/TRC RGB, gray); otherwise the
//!   `/Alternate` (or device space by `/N`) is used **and the
//!   approximation is recorded** so a render through it is never
//!   mistaken for a faithful one.
//! * Special: Indexed (lookup table), Separation and DeviceN (tint
//!   transform through [`crate::function`]). Pattern stays
//!   `Unsupported` — pattern painting is a different feature.
//!
//! Parsing is bounded: nesting depth, lookup-table size and function
//! sizes are capped; a colour space that references itself is a
//! `CycleDetected`/depth error, never a stack overflow.

use std::rc::Rc;

use crate::document::Document;
use crate::error::Error;
use crate::function::{parse_function, Function};
use crate::icc::{self, IccProfile};
use crate::limits::Limits;
use crate::object::{Dict, Object};

/// Deepest `[/Indexed [/ICCBased …]]`-style nesting accepted.
const MAX_CS_DEPTH: usize = 8;

/// A resolved colour space.
#[derive(Debug)]
pub(crate) enum ColorSpace {
    DeviceGray,
    DeviceRGB,
    DeviceCMYK,
    CalGray {
        gamma: f64,
        white: [f64; 3],
    },
    CalRGB {
        gamma: [f64; 3],
        /// Column-major per spec: XYZ = M × (A^Ga, B^Gb, C^Gc).
        matrix: [[f64; 3]; 3],
        white: [f64; 3],
    },
    Lab {
        white: [f64; 3],
        range: [f64; 4],
    },
    IccBased {
        n: usize,
        /// `Some` when the embedded profile is evaluated exactly.
        profile: Option<Rc<IccProfile>>,
        /// Used when `profile` is `None`.
        alternate: Rc<ColorSpace>,
        /// Why the alternate is used instead of the profile. `Some`
        /// means every colour through this space is an approximation.
        approximation: Option<String>,
    },
    Indexed {
        base: Rc<ColorSpace>,
        hival: u32,
        /// `(hival + 1) × base.ncomps()` bytes (zero-padded if short).
        lookup: Vec<u8>,
    },
    /// Separation (`n == 1`) or DeviceN (`n ≥ 1`).
    Separation {
        n: usize,
        alternate: Rc<ColorSpace>,
        tint: Rc<Function>,
        /// `/None` colorant: paints nothing (§8.6.6.4).
        is_none: bool,
    },
    /// `/Pattern` (§8.7.3): the "colour" is a pattern object selected
    /// by `scn`; `base` is the underlying space of an uncoloured
    /// tiling pattern's components.
    Pattern {
        base: Option<Rc<ColorSpace>>,
    },
}

impl ColorSpace {
    /// Number of components a colour in this space has.
    pub(crate) fn ncomps(&self) -> usize {
        match self {
            ColorSpace::DeviceGray | ColorSpace::CalGray { .. } | ColorSpace::Indexed { .. } => 1,
            ColorSpace::DeviceRGB | ColorSpace::CalRGB { .. } | ColorSpace::Lab { .. } => 3,
            ColorSpace::DeviceCMYK => 4,
            ColorSpace::IccBased { n, .. } => *n,
            ColorSpace::Separation { n, .. } => *n,
            ColorSpace::Pattern { base } => base.as_ref().map(|b| b.ncomps()).unwrap_or(0),
        }
    }

    /// Initial colour when the space is selected (§8.6.3 Table 62 /
    /// §8.6.6: all-ones for Separation/DeviceN, otherwise zeros, with
    /// CMYK black = `0 0 0 1`).
    pub(crate) fn initial_components(&self) -> Vec<f64> {
        match self {
            ColorSpace::DeviceCMYK => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::IccBased { n: 4, .. } => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Separation { n, .. } => vec![1.0; *n],
            other => vec![0.0; other.ncomps()],
        }
    }

    /// Default image `/Decode` ranges for `bpc` bits per component
    /// (§8.9.5.2 Table 90).
    pub(crate) fn default_decode(&self, bpc: u32) -> Vec<(f64, f64)> {
        match self {
            ColorSpace::Indexed { .. } => vec![(0.0, ((1u64 << bpc.min(16)) - 1) as f64)],
            ColorSpace::Lab { range, .. } => {
                vec![(0.0, 100.0), (range[0], range[1]), (range[2], range[3])]
            }
            other => vec![(0.0, 1.0); other.ncomps()],
        }
    }

    /// The approximation note, if colours through this space (or any
    /// space it is built on) are not exact.
    pub(crate) fn approximation(&self) -> Option<String> {
        match self {
            ColorSpace::IccBased {
                approximation,
                alternate,
                ..
            } => approximation.clone().or_else(|| alternate.approximation()),
            ColorSpace::Indexed { base, .. } => base.approximation(),
            ColorSpace::Separation { alternate, .. } => alternate.approximation(),
            ColorSpace::Pattern { base } => base.as_ref().and_then(|b| b.approximation()),
            _ => None,
        }
    }

    /// `true` for the `/Pattern` family.
    pub(crate) fn is_pattern(&self) -> bool {
        matches!(self, ColorSpace::Pattern { .. })
    }

    /// `true` for a `/None` Separation — painting in it marks nothing.
    pub(crate) fn paints_nothing(&self) -> bool {
        matches!(self, ColorSpace::Separation { is_none: true, .. })
    }

    /// Components → sRGB in 0..=1. Missing components read as 0;
    /// out-of-range values are clipped per §8.6.3.
    pub(crate) fn to_rgb(&self, c: &[f64]) -> Result<[f64; 3], Error> {
        let at = |i: usize| clamp01(c.get(i).copied().unwrap_or(0.0));
        Ok(match self {
            ColorSpace::DeviceGray => {
                let g = at(0);
                [g, g, g]
            }
            ColorSpace::DeviceRGB => [at(0), at(1), at(2)],
            ColorSpace::DeviceCMYK => {
                let (cy, m, y, k) = (at(0), at(1), at(2), at(3));
                [
                    1.0 - (cy + k).min(1.0),
                    1.0 - (m + k).min(1.0),
                    1.0 - (y + k).min(1.0),
                ]
            }
            ColorSpace::CalGray { gamma, white } => {
                // §8.6.5.2: X = Xw·A^G, Y = Yw·A^G, Z = Zw·A^G.
                let ag = at(0).powf(*gamma);
                let xyz = [white[0] * ag, white[1] * ag, white[2] * ag];
                icc::xyz_d65_to_srgb(icc::adapt_to_d65(xyz, *white))
            }
            ColorSpace::CalRGB {
                gamma,
                matrix,
                white,
            } => {
                // §8.6.5.3: XYZ = M · (A^Gr, B^Gg, C^Gb), M columns are
                // the colorants' XYZ.
                let abc = [
                    at(0).powf(gamma[0]),
                    at(1).powf(gamma[1]),
                    at(2).powf(gamma[2]),
                ];
                let xyz = [
                    matrix[0][0] * abc[0] + matrix[1][0] * abc[1] + matrix[2][0] * abc[2],
                    matrix[0][1] * abc[0] + matrix[1][1] * abc[1] + matrix[2][1] * abc[2],
                    matrix[0][2] * abc[0] + matrix[1][2] * abc[1] + matrix[2][2] * abc[2],
                ];
                icc::xyz_d65_to_srgb(icc::adapt_to_d65(xyz, *white))
            }
            ColorSpace::Lab { white, range } => {
                // §8.6.5.4 — inputs are L* 0..100, a*/b* within /Range.
                let l = c.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
                let a = c.get(1).copied().unwrap_or(0.0).clamp(range[0], range[1]);
                let b = c.get(2).copied().unwrap_or(0.0).clamp(range[2], range[3]);
                let m = (l + 16.0) / 116.0;
                let ll = m + a / 500.0;
                let n = m - b / 200.0;
                let g = |x: f64| {
                    if x >= 6.0 / 29.0 {
                        x * x * x
                    } else {
                        (108.0 / 841.0) * (x - 4.0 / 29.0)
                    }
                };
                let xyz = [white[0] * g(ll), white[1] * g(m), white[2] * g(n)];
                icc::xyz_d65_to_srgb(icc::adapt_to_d65(xyz, *white))
            }
            ColorSpace::IccBased {
                n,
                profile,
                alternate,
                ..
            } => {
                let comps: Vec<f64> = (0..*n).map(at).collect();
                match profile.as_deref().and_then(|p| p.to_srgb(&comps)) {
                    Some(rgb) => rgb,
                    None => alternate.to_rgb(&comps)?,
                }
            }
            ColorSpace::Indexed {
                base,
                hival,
                lookup,
            } => {
                // §8.6.6.3: the index is rounded and clipped to
                // 0..=hival; lookup bytes map to the base component
                // ranges (0..=1 for every base we accept, except Lab
                // whose decode we apply explicitly).
                let raw = c.first().copied().unwrap_or(0.0);
                let idx = if raw.is_finite() {
                    (raw.round().max(0.0) as u64).min(*hival as u64) as usize
                } else {
                    0
                };
                let nb = base.ncomps();
                let start = idx * nb;
                let bytes: Vec<f64> = (0..nb)
                    .map(|i| lookup.get(start + i).copied().unwrap_or(0) as f64 / 255.0)
                    .collect();
                let comps: Vec<f64> = match &**base {
                    ColorSpace::Lab { range, .. } => vec![
                        bytes[0] * 100.0,
                        range[0] + bytes[1] * (range[1] - range[0]),
                        range[2] + bytes[2] * (range[3] - range[2]),
                    ],
                    _ => bytes,
                };
                base.to_rgb(&comps)?
            }
            ColorSpace::Separation {
                n,
                alternate,
                tint,
                is_none,
            } => {
                if *is_none {
                    // Never painted; the caller checks `paints_nothing`.
                    return Ok([0.0, 0.0, 0.0]);
                }
                let comps: Vec<f64> = (0..*n).map(at).collect();
                let out = tint.eval(&comps)?;
                alternate.to_rgb(&out)?
            }
            // A pattern's colour comes from the pattern object; the
            // components (if any) belong to an uncoloured tiling
            // pattern, which the renderer refuses before painting.
            ColorSpace::Pattern { base } => match base {
                Some(b) => b.to_rgb(c)?,
                None => [0.0, 0.0, 0.0],
            },
        })
    }

    /// Parse a colour-space object. `resources` is the current
    /// `/Resources` (names other than the device/Pattern families are
    /// looked up in its `/ColorSpace` dictionary; `/DefaultGray` &c.
    /// remap the device spaces per §8.6.5.6).
    pub(crate) fn parse(
        doc: &Document,
        obj: &Object,
        resources: &Object,
        limits: &Limits,
    ) -> Result<ColorSpace, Error> {
        parse_depth(doc, obj, resources, limits, 0)
    }
}

fn parse_depth(
    doc: &Document,
    obj: &Object,
    resources: &Object,
    limits: &Limits,
    depth: usize,
) -> Result<ColorSpace, Error> {
    if depth > MAX_CS_DEPTH {
        return Err(Error::LimitExceeded {
            limit: "colour space nesting depth",
            max: MAX_CS_DEPTH as u64,
        });
    }
    let obj = doc.deref(obj)?;
    match &obj {
        Object::Name(n) => parse_name(doc, n, resources, limits, depth),
        Object::Array(a) => parse_array(doc, a, resources, limits, depth),
        // A bare dictionary/stream is not a colour space (ICC streams
        // appear only inside an /ICCBased array).
        _ => Err(Error::Structure(
            "colour space must be a name or an array".into(),
        )),
    }
}

fn parse_name(
    doc: &Document,
    name: &[u8],
    resources: &Object,
    limits: &Limits,
    depth: usize,
) -> Result<ColorSpace, Error> {
    let device = match name {
        b"DeviceGray" | b"G" => Some((ColorSpace::DeviceGray, &b"DefaultGray"[..])),
        b"DeviceRGB" | b"RGB" => Some((ColorSpace::DeviceRGB, &b"DefaultRGB"[..])),
        b"DeviceCMYK" | b"CMYK" => Some((ColorSpace::DeviceCMYK, &b"DefaultCMYK"[..])),
        _ => None,
    };
    if let Some((dev, default_key)) = device {
        // §8.6.5.6: a /Default* entry in the resources' /ColorSpace
        // dictionary overrides the device space. Only honoured at the
        // top level (depth 0) so a Default* that is itself built on a
        // device space does not recurse.
        if depth == 0 {
            if let Some(d) = lookup_resource(doc, resources, default_key)? {
                let cs = parse_depth(doc, &d, resources, limits, depth + 1)?;
                if cs.ncomps() == dev.ncomps() {
                    return Ok(cs);
                }
                return Err(Error::Structure(format!(
                    "/{} has {} components, expected {}",
                    String::from_utf8_lossy(default_key),
                    cs.ncomps(),
                    dev.ncomps()
                )));
            }
        }
        return Ok(dev);
    }
    match name {
        b"Pattern" => Ok(ColorSpace::Pattern { base: None }),
        // Bare family names are only valid inside an array; a few
        // producers still emit `/CalRGB cs`-style names. Refuse them
        // precisely rather than guessing parameters.
        b"CalGray" | b"CalRGB" | b"Lab" | b"ICCBased" | b"Indexed" | b"I" | b"Separation"
        | b"DeviceN" => Err(Error::Structure(format!(
            "/{} colour space needs its parameter array",
            String::from_utf8_lossy(name)
        ))),
        b"DefaultGray" => Ok(ColorSpace::DeviceGray),
        b"DefaultRGB" => Ok(ColorSpace::DeviceRGB),
        b"DefaultCMYK" => Ok(ColorSpace::DeviceCMYK),
        other => match lookup_resource(doc, resources, other)? {
            Some(o) => {
                // A named resource that is itself a bare name would
                // loop (`/CS0 → /CS0`); the depth cap ends that.
                parse_depth(doc, &o, resources, limits, depth + 1)
            }
            None => Err(Error::Structure(format!(
                "colour space /{} is not defined in the current resources",
                String::from_utf8_lossy(other)
            ))),
        },
    }
}

fn lookup_resource(
    doc: &Document,
    resources: &Object,
    name: &[u8],
) -> Result<Option<Object>, Error> {
    // An empty/absent /Resources is common for pages that only use
    // device colours — treat a non-dictionary as "no entry" here;
    // structural problems surface where the resource is required.
    match doc.resolve_resource(resources, "ColorSpace", name) {
        Ok(v) => Ok(v),
        Err(Error::Structure(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_array(
    doc: &Document,
    arr: &[Object],
    resources: &Object,
    limits: &Limits,
    depth: usize,
) -> Result<ColorSpace, Error> {
    let Some(first) = arr.first() else {
        return Err(Error::Structure("colour space array is empty".into()));
    };
    let first = doc.deref(first)?;
    let family = first
        .as_name()
        .ok_or_else(|| Error::Structure("colour space array must start with a name".into()))?;
    let param = |i: usize| -> Result<Option<Object>, Error> {
        match arr.get(i) {
            None => Ok(None),
            Some(o) => Ok(Some(doc.deref(o)?)),
        }
    };
    match family {
        // `[/DeviceRGB]` is legal shorthand.
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"G" | b"RGB" | b"CMYK" | b"Pattern"
            if arr.len() == 1 =>
        {
            parse_name(doc, family, resources, limits, depth)
        }
        b"Pattern" => {
            // `[/Pattern base]` — the base space is for uncoloured
            // tiling patterns' `scn` components.
            let base = match arr.get(1) {
                None => None,
                Some(b) => {
                    let bs = parse_depth(doc, b, resources, limits, depth + 1)?;
                    if matches!(bs, ColorSpace::Pattern { .. }) {
                        return Err(Error::Structure(
                            "/Pattern base space may not be Pattern".into(),
                        ));
                    }
                    Some(Rc::new(bs))
                }
            };
            Ok(ColorSpace::Pattern { base })
        }
        b"CalGray" => {
            let d = dict_param(param(1)?, "CalGray")?;
            let white = white_point(doc, &d)?;
            let gamma = match d.get(b"Gamma".as_slice()) {
                None => 1.0,
                Some(o) => positive_number(doc, o, "/Gamma")?,
            };
            Ok(ColorSpace::CalGray { gamma, white })
        }
        b"CalRGB" => {
            let d = dict_param(param(1)?, "CalRGB")?;
            let white = white_point(doc, &d)?;
            let gamma = match d.get(b"Gamma".as_slice()) {
                None => [1.0; 3],
                Some(o) => {
                    let v = number_array(doc, o, 3, "/Gamma")?;
                    for g in &v {
                        if *g <= 0.0 {
                            return Err(Error::Structure("/Gamma must be positive".into()));
                        }
                    }
                    [v[0], v[1], v[2]]
                }
            };
            let matrix = match d.get(b"Matrix".as_slice()) {
                None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                Some(o) => {
                    let v = number_array(doc, o, 9, "/Matrix")?;
                    [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]
                }
            };
            Ok(ColorSpace::CalRGB {
                gamma,
                matrix,
                white,
            })
        }
        b"Lab" => {
            let d = dict_param(param(1)?, "Lab")?;
            let white = white_point(doc, &d)?;
            let range = match d.get(b"Range".as_slice()) {
                None => [-100.0, 100.0, -100.0, 100.0],
                Some(o) => {
                    let v = number_array(doc, o, 4, "/Range")?;
                    if v[0] > v[1] || v[2] > v[3] {
                        return Err(Error::Structure("/Range bounds are inverted".into()));
                    }
                    [v[0], v[1], v[2], v[3]]
                }
            };
            Ok(ColorSpace::Lab { white, range })
        }
        b"ICCBased" => {
            let Some(Object::Stream { dict, data }) = param(1)? else {
                return Err(Error::Structure("/ICCBased needs a profile stream".into()));
            };
            let n = dict
                .get(b"N".as_slice())
                .and_then(|o| o.as_i64())
                .ok_or_else(|| Error::Structure("/ICCBased stream has no /N".into()))?;
            let n = match n {
                1 | 3 | 4 => n as usize,
                other => {
                    return Err(Error::Structure(format!(
                        "/ICCBased /N {other} is not 1, 3 or 4"
                    )))
                }
            };
            let alternate = match dict.get(b"Alternate".as_slice()) {
                Some(a) => {
                    let alt = parse_depth(doc, a, resources, limits, depth + 1)?;
                    if alt.ncomps() != n {
                        return Err(Error::Structure(format!(
                            "/ICCBased /Alternate has {} components, /N is {n}",
                            alt.ncomps()
                        )));
                    }
                    alt
                }
                None => match n {
                    1 => ColorSpace::DeviceGray,
                    3 => ColorSpace::DeviceRGB,
                    _ => ColorSpace::DeviceCMYK,
                },
            };
            // The profile bytes: a decode failure or an unreadable
            // profile falls back to the alternate (§8.6.5.5 allows
            // it) — but is recorded as an approximation.
            let (profile, approximation) = match doc.decode_stream(&dict, &data) {
                Ok(bytes) => match icc::parse(&bytes) {
                    Some(IccProfile::Unsupported { reason }) => (
                        None,
                        Some(format!(
                            "ICC profile not evaluated ({reason}); /Alternate used"
                        )),
                    ),
                    Some(p) => {
                        let pn = match &p {
                            IccProfile::Gray { .. } => 1,
                            IccProfile::MatrixTrc { .. } => 3,
                            IccProfile::Unsupported { .. } => 0,
                        };
                        if pn != n {
                            (
                                None,
                                Some(format!(
                                    "ICC profile has {pn} components but /N is {n}; /Alternate used"
                                )),
                            )
                        } else {
                            (Some(Rc::new(p)), None)
                        }
                    }
                    None => (
                        None,
                        Some("ICC stream is not a readable profile; /Alternate used".into()),
                    ),
                },
                Err(Error::LimitExceeded { limit, max }) => {
                    return Err(Error::LimitExceeded { limit, max })
                }
                Err(_) => (
                    None,
                    Some("ICC stream could not be decoded; /Alternate used".into()),
                ),
            };
            Ok(ColorSpace::IccBased {
                n,
                profile,
                alternate: Rc::new(alternate),
                approximation,
            })
        }
        b"Indexed" | b"I" => {
            if arr.len() != 4 {
                return Err(Error::Structure(
                    "/Indexed needs [base hival lookup]".into(),
                ));
            }
            let base = parse_depth(doc, &arr[1], resources, limits, depth + 1)?;
            if matches!(base, ColorSpace::Indexed { .. }) {
                return Err(Error::Structure(
                    "/Indexed base may not itself be Indexed".into(),
                ));
            }
            let hival = param(2)?
                .and_then(|o| o.as_i64())
                .ok_or_else(|| Error::Structure("/Indexed hival is not an integer".into()))?;
            if !(0..=255).contains(&hival) {
                return Err(Error::Structure(format!(
                    "/Indexed hival {hival} is outside 0..=255"
                )));
            }
            let need = (hival as usize + 1) * base.ncomps();
            let mut lookup = match param(3)? {
                Some(Object::String(s)) => s,
                Some(Object::Stream { dict, data }) => doc.decode_stream(&dict, &data)?,
                _ => {
                    return Err(Error::Structure(
                        "/Indexed lookup must be a string or a stream".into(),
                    ))
                }
            };
            // §8.6.6.3 requires exactly `need` bytes; real files are
            // often short (trailing entries implicitly black) — pad
            // rather than refuse, truncate anything beyond.
            lookup.resize(need, 0);
            Ok(ColorSpace::Indexed {
                base: Rc::new(base),
                hival: hival as u32,
                lookup,
            })
        }
        b"Separation" | b"DeviceN" => {
            let is_sep = family == b"Separation";
            let names_obj =
                param(1)?.ok_or_else(|| Error::Structure("colorant name(s) missing".into()))?;
            let (n, is_none) = if is_sep {
                let nm = names_obj.as_name().ok_or_else(|| {
                    Error::Structure("/Separation colorant must be a name".into())
                })?;
                (1usize, nm == b"None")
            } else {
                let names = names_obj
                    .as_array()
                    .ok_or_else(|| Error::Structure("/DeviceN names must be an array".into()))?;
                if names.is_empty() || names.len() > 32 {
                    return Err(Error::Structure(format!(
                        "/DeviceN has {} colorants (1..=32 allowed)",
                        names.len()
                    )));
                }
                let mut all_none = true;
                for nmo in names {
                    let nm = doc.deref(nmo)?;
                    let nm = nm.as_name().ok_or_else(|| {
                        Error::Structure("/DeviceN colorant must be a name".into())
                    })?;
                    if nm != b"None" {
                        all_none = false;
                    }
                }
                (names.len(), all_none)
            };
            let alt_obj = arr
                .get(2)
                .ok_or_else(|| Error::Structure("alternate colour space missing".into()))?;
            let alternate = parse_depth(doc, alt_obj, resources, limits, depth + 1)?;
            if matches!(
                alternate,
                ColorSpace::Indexed { .. } | ColorSpace::Separation { .. }
            ) {
                return Err(Error::Structure(
                    "alternate space must not be Indexed/Separation/DeviceN".into(),
                ));
            }
            let fn_obj = arr
                .get(3)
                .ok_or_else(|| Error::Structure("tint transform missing".into()))?;
            let tint = parse_function(doc, fn_obj, limits)?;
            if tint.n_inputs() != n {
                return Err(Error::Structure(format!(
                    "tint transform takes {} inputs, colour space has {n}",
                    tint.n_inputs()
                )));
            }
            if let Some(o) = tint.n_outputs() {
                if o != alternate.ncomps() {
                    return Err(Error::Structure(format!(
                        "tint transform yields {o} outputs, alternate space has {}",
                        alternate.ncomps()
                    )));
                }
            }
            Ok(ColorSpace::Separation {
                n,
                alternate: Rc::new(alternate),
                tint: Rc::new(tint),
                is_none,
            })
        }
        other => Err(Error::Unsupported {
            feature: "colour space family",
            detail: format!("/{}", String::from_utf8_lossy(other)),
        }),
    }
}

fn dict_param(o: Option<Object>, fam: &str) -> Result<Dict, Error> {
    match o {
        Some(Object::Dict(d)) => Ok(d),
        _ => Err(Error::Structure(format!(
            "/{fam} needs a parameter dictionary"
        ))),
    }
}

fn white_point(doc: &Document, d: &Dict) -> Result<[f64; 3], Error> {
    let o = d
        .get(b"WhitePoint".as_slice())
        .ok_or_else(|| Error::Structure("/WhitePoint is required".into()))?;
    let v = number_array(doc, o, 3, "/WhitePoint")?;
    if v[0] <= 0.0 || v[1] != 1.0 || v[2] <= 0.0 {
        return Err(Error::Structure(
            "/WhitePoint must be [Xw 1.0 Zw] with Xw, Zw positive".into(),
        ));
    }
    Ok([v[0], v[1], v[2]])
}

fn positive_number(doc: &Document, o: &Object, what: &str) -> Result<f64, Error> {
    let v = doc
        .deref(o)?
        .as_number()
        .ok_or_else(|| Error::Structure(format!("{what} must be a number")))?;
    if !(v.is_finite() && v > 0.0) {
        return Err(Error::Structure(format!("{what} must be positive")));
    }
    Ok(v)
}

fn number_array(doc: &Document, o: &Object, len: usize, what: &str) -> Result<Vec<f64>, Error> {
    let o = doc.deref(o)?;
    let arr = o
        .as_array()
        .ok_or_else(|| Error::Structure(format!("{what} must be an array")))?;
    if arr.len() != len {
        return Err(Error::Structure(format!(
            "{what} must have {len} numbers, has {}",
            arr.len()
        )));
    }
    let mut out = Vec::with_capacity(len);
    for e in arr {
        let v = doc
            .deref(e)?
            .as_number()
            .ok_or_else(|| Error::Structure(format!("{what} entries must be numbers")))?;
        if !v.is_finite() {
            return Err(Error::Structure(format!("{what} has a non-finite value")));
        }
        out.push(v);
    }
    Ok(out)
}

fn clamp01(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn device_spaces_match_existing_rules() {
        assert_eq!(ColorSpace::DeviceGray.to_rgb(&[0.5]).unwrap(), [0.5; 3]);
        assert_eq!(
            ColorSpace::DeviceCMYK
                .to_rgb(&[0.0, 0.0, 0.0, 1.0])
                .unwrap(),
            [0.0; 3]
        );
        assert_eq!(
            ColorSpace::DeviceRGB.to_rgb(&[2.0, -1.0, 0.25]).unwrap(),
            [1.0, 0.0, 0.25]
        );
    }

    #[test]
    fn calrgb_srgb_primaries_round_trip() {
        // sRGB colorants (D65) with gamma 2.2 ≈ identity through XYZ.
        let cs = ColorSpace::CalRGB {
            gamma: [2.2; 3],
            matrix: [
                [0.4124, 0.2126, 0.0193],
                [0.3576, 0.7152, 0.1192],
                [0.1805, 0.0722, 0.9505],
            ],
            white: [0.9505, 1.0, 1.089],
        };
        assert!(close(cs.to_rgb(&[1.0, 1.0, 1.0]).unwrap(), [1.0; 3], 0.01));
        assert!(close(
            cs.to_rgb(&[1.0, 0.0, 0.0]).unwrap(),
            [1.0, 0.0, 0.0],
            0.05
        ));
        assert!(close(cs.to_rgb(&[0.5, 0.5, 0.5]).unwrap(), [0.5; 3], 0.03));
        assert!(close(cs.to_rgb(&[0.0, 0.0, 0.0]).unwrap(), [0.0; 3], 0.001));
    }

    #[test]
    fn calgray_is_gamma_encoded() {
        let cs = ColorSpace::CalGray {
            gamma: 1.0,
            white: [0.9505, 1.0, 1.089],
        };
        // Linear 0.5 → sRGB ≈ 0.735, not 0.5.
        let g = cs.to_rgb(&[0.5]).unwrap();
        assert!(close(g, [0.735; 3], 0.01), "{g:?}");
        assert!(close(cs.to_rgb(&[1.0]).unwrap(), [1.0; 3], 0.01));
    }

    #[test]
    fn lab_white_and_black() {
        let cs = ColorSpace::Lab {
            white: [0.9643, 1.0, 0.8251],
            range: [-100.0, 100.0, -100.0, 100.0],
        };
        assert!(close(
            cs.to_rgb(&[100.0, 0.0, 0.0]).unwrap(),
            [1.0; 3],
            0.01
        ));
        assert!(close(cs.to_rgb(&[0.0, 0.0, 0.0]).unwrap(), [0.0; 3], 0.01));
        let red = cs.to_rgb(&[50.0, 70.0, 50.0]).unwrap();
        assert!(red[0] > red[1] && red[0] > red[2], "{red:?}");
    }

    #[test]
    fn indexed_and_defaults() {
        let cs = ColorSpace::Indexed {
            base: Rc::new(ColorSpace::DeviceRGB),
            hival: 1,
            lookup: vec![255, 0, 0, 0, 0, 255],
        };
        assert_eq!(cs.to_rgb(&[0.0]).unwrap(), [1.0, 0.0, 0.0]);
        assert_eq!(cs.to_rgb(&[1.0]).unwrap(), [0.0, 0.0, 1.0]);
        assert_eq!(
            cs.to_rgb(&[7.0]).unwrap(),
            [0.0, 0.0, 1.0],
            "clipped to hival"
        );
        assert_eq!(cs.default_decode(8), vec![(0.0, 255.0)]);
        assert_eq!(cs.default_decode(1), vec![(0.0, 1.0)]);
        assert_eq!(
            ColorSpace::DeviceCMYK.initial_components(),
            vec![0.0, 0.0, 0.0, 1.0]
        );
    }
}
