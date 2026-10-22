//! Minimal ICC profile reader (ICC.1 v2/v4) for the profile classes
//! that can be converted *exactly* with a closed formula:
//!
//! * RGB **matrix/TRC** profiles (`rXYZ gXYZ bXYZ` + `rTRC gTRC bTRC`,
//!   PCS XYZ) — every sRGB / Adobe RGB / display profile embedded by
//!   common producers;
//! * **Gray** profiles (`kTRC`).
//!
//! LUT-based profiles (`A2B0` multi-dimensional tables, all CMYK
//! profiles, Lab PCS) are reported as [`IccProfile::Unsupported`] so
//! the colour space falls back to its `/Alternate` *and says so*
//! (§8.6.5.5 permits the alternate when the profile cannot be
//! processed; the renderer records the approximation rather than
//! calling it a faithful render).
//!
//! Conversion path: device → TRC → linear → colorant matrix → XYZ
//! (D50 PCS, as the colorant tags are already adapted) → Bradford
//! D50→D65 → linear sRGB → sRGB transfer. Deterministic, no host CMS.

/// Parsed profile.
#[derive(Debug, Clone)]
pub(crate) enum IccProfile {
    /// RGB matrix/TRC profile. `matrix` rows are X, Y, Z (D50 PCS).
    MatrixTrc {
        trc: [Trc; 3],
        matrix: [[f64; 3]; 3],
    },
    /// Gray profile — a single tone curve.
    Gray { trc: Trc },
    /// A profile we read but cannot process exactly (LUT-based, CMYK,
    /// Lab PCS, …). Carries a reason for the report.
    Unsupported { reason: String },
}

/// A tone reproduction curve.
#[derive(Debug, Clone)]
pub(crate) enum Trc {
    Identity,
    Gamma(f64),
    /// Sampled curve, values 0..=1, evenly spaced over the input.
    Table(Vec<f32>),
    /// ICC v4 parametric curve: function type 0–4 with parameters
    /// `g a b c d e f` (unused ones zero).
    Parametric {
        kind: u16,
        p: [f64; 7],
    },
}

impl Trc {
    /// Apply to a device value in 0..=1 → linear 0..=1.
    pub(crate) fn apply(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Trc::Identity => x,
            Trc::Gamma(g) => x.powf(*g),
            Trc::Table(t) => {
                if t.len() < 2 {
                    return t.first().copied().unwrap_or(0.0) as f64;
                }
                let pos = x * (t.len() - 1) as f64;
                let i = pos.floor() as usize;
                let frac = pos - i as f64;
                let a = t[i.min(t.len() - 1)] as f64;
                let b = t[(i + 1).min(t.len() - 1)] as f64;
                a + (b - a) * frac
            }
            Trc::Parametric { kind, p } => {
                let [g, a, b, c, d, e, f] = *p;
                let v = match kind {
                    0 => x.powf(g),
                    1 => {
                        if x >= -b / a {
                            (a * x + b).powf(g)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if x >= -b / a {
                            (a * x + b).powf(g) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if x >= d {
                            (a * x + b).powf(g)
                        } else {
                            c * x
                        }
                    }
                    4 => {
                        if x >= d {
                            (a * x + b).powf(g) + e
                        } else {
                            c * x + f
                        }
                    }
                    _ => x,
                };
                if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
        }
    }
}

fn be32(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be16(d: &[u8], off: usize) -> Option<u16> {
    d.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn s15f16(d: &[u8], off: usize) -> Option<f64> {
    be32(d, off).map(|v| v as i32 as f64 / 65536.0)
}

/// Parse `data`. Returns `None` only when the bytes are not an ICC
/// profile at all (bad header); unsupported-but-valid profiles are
/// `Some(IccProfile::Unsupported)`.
pub(crate) fn parse(data: &[u8]) -> Option<IccProfile> {
    if data.len() < 132 || &data[36..40] != b"acsp" {
        return None;
    }
    let space = &data[16..20];
    let pcs = &data[20..24];
    let tag_count = be32(data, 128)? as usize;
    if tag_count > 1024 || 132 + tag_count * 12 > data.len() {
        return None;
    }
    let mut tags: Vec<([u8; 4], usize, usize)> = Vec::with_capacity(tag_count);
    for i in 0..tag_count {
        let o = 132 + i * 12;
        let sig = [data[o], data[o + 1], data[o + 2], data[o + 3]];
        let off = be32(data, o + 4)? as usize;
        let len = be32(data, o + 8)? as usize;
        if off
            .checked_add(len)
            .map(|e| e <= data.len())
            .unwrap_or(false)
        {
            tags.push((sig, off, len));
        }
    }
    let find = |sig: &[u8; 4]| {
        tags.iter()
            .find(|(s, _, _)| s == sig)
            .map(|(_, o, l)| &data[*o..*o + *l])
    };

    let unsupported = |reason: &str| {
        Some(IccProfile::Unsupported {
            reason: reason.to_string(),
        })
    };
    if pcs != b"XYZ " {
        return unsupported("Lab PCS");
    }
    match space {
        b"GRAY" => {
            let Some(k) = find(b"kTRC") else {
                return unsupported("gray profile without kTRC");
            };
            let trc = parse_curve(k)?;
            Some(IccProfile::Gray { trc })
        }
        b"RGB " => {
            let (Some(r), Some(g), Some(b)) = (find(b"rXYZ"), find(b"gXYZ"), find(b"bXYZ")) else {
                return unsupported(if find(b"A2B0").is_some() {
                    "LUT-based RGB profile (A2B0)"
                } else {
                    "RGB profile without colorant tags"
                });
            };
            let (Some(rt), Some(gt), Some(bt)) = (find(b"rTRC"), find(b"gTRC"), find(b"bTRC"))
            else {
                return unsupported("RGB profile without TRC tags");
            };
            let rx = parse_xyz(r)?;
            let gx = parse_xyz(g)?;
            let bx = parse_xyz(b)?;
            let trc = [parse_curve(rt)?, parse_curve(gt)?, parse_curve(bt)?];
            let matrix = [
                [rx[0], gx[0], bx[0]],
                [rx[1], gx[1], bx[1]],
                [rx[2], gx[2], bx[2]],
            ];
            Some(IccProfile::MatrixTrc { trc, matrix })
        }
        b"CMYK" => unsupported("CMYK profile (LUT-based)"),
        other => unsupported(&format!(
            "colour space '{}'",
            String::from_utf8_lossy(other).trim()
        )),
    }
}

fn parse_xyz(tag: &[u8]) -> Option<[f64; 3]> {
    if tag.len() < 20 || &tag[0..4] != b"XYZ " {
        return None;
    }
    Some([s15f16(tag, 8)?, s15f16(tag, 12)?, s15f16(tag, 16)?])
}

fn parse_curve(tag: &[u8]) -> Option<Trc> {
    match tag.get(0..4)? {
        b"curv" => {
            let n = be32(tag, 8)? as usize;
            match n {
                0 => Some(Trc::Identity),
                1 => Some(Trc::Gamma(be16(tag, 12)? as f64 / 256.0)),
                n if n <= 65536 && 12 + n * 2 <= tag.len() => {
                    let mut t = Vec::with_capacity(n);
                    for i in 0..n {
                        t.push(be16(tag, 12 + i * 2)? as f32 / 65535.0);
                    }
                    Some(Trc::Table(t))
                }
                _ => None,
            }
        }
        b"para" => {
            let kind = be16(tag, 8)?;
            let np = match kind {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return None,
            };
            let mut p = [0.0f64; 7];
            for (i, slot) in p.iter_mut().enumerate().take(np) {
                *slot = s15f16(tag, 12 + i * 4)?;
            }
            Some(Trc::Parametric { kind, p })
        }
        _ => None,
    }
}

// ---- colorimetry helpers shared with the Cal*/Lab spaces ---------------

/// Bradford chromatic adaptation of XYZ from white point `src` to D65.
pub(crate) fn adapt_to_d65(xyz: [f64; 3], src: [f64; 3]) -> [f64; 3] {
    const D65: [f64; 3] = [0.9505, 1.0, 1.0890];
    const M: [[f64; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    const MI: [[f64; 3]; 3] = [
        [0.9869929, -0.1470543, 0.1599627],
        [0.4323053, 0.5183603, 0.0492912],
        [-0.0085287, 0.0400428, 0.9684867],
    ];
    if src[1] <= 0.0 || !src.iter().all(|v| v.is_finite()) {
        return xyz;
    }
    let s = mul(&M, src);
    let d = mul(&M, D65);
    let c = mul(&M, xyz);
    let scaled = [
        if s[0] != 0.0 {
            c[0] * d[0] / s[0]
        } else {
            c[0]
        },
        if s[1] != 0.0 {
            c[1] * d[1] / s[1]
        } else {
            c[1]
        },
        if s[2] != 0.0 {
            c[2] * d[2] / s[2]
        } else {
            c[2]
        },
    ];
    mul(&MI, scaled)
}

/// XYZ (D65) → sRGB 0..=1 with the sRGB transfer function.
pub(crate) fn xyz_d65_to_srgb(xyz: [f64; 3]) -> [f64; 3] {
    const M: [[f64; 3]; 3] = [
        [3.2404542, -1.5371385, -0.4985314],
        [-0.9692660, 1.8760108, 0.0415560],
        [0.0556434, -0.2040259, 1.0572252],
    ];
    let lin = mul(&M, xyz);
    [
        srgb_encode(lin[0]),
        srgb_encode(lin[1]),
        srgb_encode(lin[2]),
    ]
}

pub(crate) fn srgb_encode(v: f64) -> f64 {
    let v = if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn mul(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

impl IccProfile {
    /// Device components (0..=1) → sRGB 0..=1. Only valid for the
    /// supported variants; `Unsupported` returns `None`.
    pub(crate) fn to_srgb(&self, c: &[f64]) -> Option<[f64; 3]> {
        const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
        match self {
            IccProfile::MatrixTrc { trc, matrix } => {
                let lin = [
                    trc[0].apply(c.first().copied().unwrap_or(0.0)),
                    trc[1].apply(c.get(1).copied().unwrap_or(0.0)),
                    trc[2].apply(c.get(2).copied().unwrap_or(0.0)),
                ];
                let xyz = mul(matrix, lin);
                Some(xyz_d65_to_srgb(adapt_to_d65(xyz, D50)))
            }
            IccProfile::Gray { trc } => {
                let y = trc.apply(c.first().copied().unwrap_or(0.0));
                let g = srgb_encode(y);
                Some([g, g, g])
            }
            IccProfile::Unsupported { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny v2 RGB matrix/TRC profile with sRGB-like colorants
    /// (D50-adapted) and a gamma 2.2 curve.
    pub(crate) fn srgb_like_profile() -> Vec<u8> {
        fn xyz_tag(x: f64, y: f64, z: f64) -> Vec<u8> {
            let mut t = b"XYZ \0\0\0\0".to_vec();
            for v in [x, y, z] {
                t.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
            }
            t
        }
        let curv = {
            let mut t = b"curv\0\0\0\0".to_vec();
            t.extend_from_slice(&1u32.to_be_bytes());
            t.extend_from_slice(&((2.2f64 * 256.0) as u16).to_be_bytes());
            t.extend_from_slice(&[0, 0]);
            t
        };
        let tags: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"rXYZ", xyz_tag(0.4361, 0.2225, 0.0139)),
            (*b"gXYZ", xyz_tag(0.3851, 0.7169, 0.0971)),
            (*b"bXYZ", xyz_tag(0.1431, 0.0606, 0.7139)),
            (*b"rTRC", curv.clone()),
            (*b"gTRC", curv.clone()),
            (*b"bTRC", curv),
        ];
        let mut data = vec![0u8; 128];
        data[16..20].copy_from_slice(b"RGB ");
        data[20..24].copy_from_slice(b"XYZ ");
        data[36..40].copy_from_slice(b"acsp");
        data.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut off = 132 + tags.len() * 12;
        let mut bodies = Vec::new();
        for (sig, body) in &tags {
            data.extend_from_slice(sig);
            data.extend_from_slice(&(off as u32).to_be_bytes());
            data.extend_from_slice(&(body.len() as u32).to_be_bytes());
            off += body.len();
            bodies.extend_from_slice(body);
        }
        data.extend_from_slice(&bodies);
        let len = data.len() as u32;
        data[0..4].copy_from_slice(&len.to_be_bytes());
        data
    }

    #[test]
    fn srgb_like_profile_round_trips_primaries() {
        let p = parse(&srgb_like_profile()).expect("parse");
        let white = p.to_srgb(&[1.0, 1.0, 1.0]).unwrap();
        assert!(white.iter().all(|v| (v - 1.0).abs() < 0.02), "{white:?}");
        let red = p.to_srgb(&[1.0, 0.0, 0.0]).unwrap();
        assert!(red[0] > 0.95 && red[1] < 0.15 && red[2] < 0.15, "{red:?}");
        let mid = p.to_srgb(&[0.5, 0.5, 0.5]).unwrap();
        // gamma 2.2 in, sRGB curve out ≈ 0.5
        assert!(mid.iter().all(|v| (v - 0.5).abs() < 0.03), "{mid:?}");
    }

    #[test]
    fn unsupported_profiles_say_why() {
        let mut d = srgb_like_profile();
        d[16..20].copy_from_slice(b"CMYK");
        match parse(&d) {
            Some(IccProfile::Unsupported { reason }) => assert!(reason.contains("CMYK")),
            other => panic!("{other:?}"),
        }
        assert!(parse(b"not a profile").is_none());
    }

    #[test]
    fn parametric_and_table_curves() {
        let t = Trc::Parametric {
            kind: 3,
            p: [
                2.4,
                1.0 / 1.055,
                0.055 / 1.055,
                1.0 / 12.92,
                0.04045,
                0.0,
                0.0,
            ],
        };
        assert!((t.apply(0.5) - 0.214).abs() < 0.01);
        let tab = Trc::Table(vec![0.0, 0.5, 1.0]);
        assert!((tab.apply(0.25) - 0.25).abs() < 1e-6);
    }
}
