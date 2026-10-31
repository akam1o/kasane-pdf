//! Colour agreement between our render and the reference: CIEDE2000
//! (ΔE00) over the pixels both images draw in a flat colour.
//!
//! Conversion: 8-bit sRGB → linear (IEC 61966-2-1 transfer function) →
//! XYZ (sRGB matrix, D65) → CIE L\*a\*b\* (reference white the matrix'
//! row sums, so white is exactly L\* 100, a\* = b\* = 0) → ΔE00
//! (Sharma, Wu, Dalal 2005, verified against their 34 published pairs).
//!
//! What is compared, and why: whole-page means drown a wrong colour in
//! white background, and comparing pixels of differently placed shapes
//! measures position, not colour (the placement metrics do that). So a
//! pixel counts only when both images *draw* there (some channel below
//! `MARK`) and both are *flat* there (3 × 3 luma range at most `FLAT`,
//! after the best placement shift) — the interiors of shapes, not their
//! anti-aliased edges. A pixel that differs by more than `NEAR_DE` is
//! compared once more against the reference's 3 × 3 neighbourhood
//! (the smallest ΔE wins), so a 1-px placement difference at a colour
//! boundary does not read as a colour change; a region filled with the
//! wrong colour stays wrong, because every neighbour is wrong too.

use serde::{Deserialize, Serialize};

/// Below this in some channel a pixel is drawn (not white paper).
const MARK: u8 = 245;
/// Largest 3 × 3 luma range of a flat pixel.
const FLAT: u8 = 24;
/// ΔE00 under which the neighbourhood search is skipped.
const NEAR_DE: f64 = 2.0;
/// Tile edge for the local statistics (24 pt at 96 dpi).
const TILE: usize = 32;
/// Compared pixels a tile needs to be judged.
const TILE_MIN: usize = 32;
/// Compared pixels a page needs for its statistics to mean anything.
pub const MIN_COMPARED: usize = 64;

/// The colour statistics of one compared page (see the module docs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColourStats {
    /// Pixels compared (drawn and flat in both images).
    pub compared_px: usize,
    pub de_median: f64,
    pub de_p95: f64,
    pub de_p99: f64,
    /// Share of compared pixels with ΔE00 > 5 and > 10.
    pub share_de5: f64,
    pub share_de10: f64,
    /// Share with ΔE00 > 10 at |ΔL\*| < 5: a hue or chroma difference at
    /// equal lightness — invisible to every luma-based metric.
    pub share_hue10: f64,
    /// Largest mean ΔE00 of a tile with at least `TILE_MIN` compared px.
    pub worst_tile_mean: f64,
    /// Tiles where more than half the compared pixels are hue / chroma
    /// differences (ΔE00 > 10 at |ΔL\*| < 5).
    pub hue_tiles: usize,
}

/// Linear-light value of an 8-bit sRGB component.
fn linear(c: u8) -> f64 {
    let v = c as f64 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Lookup table of `linear`.
fn linear_lut() -> [f64; 256] {
    let mut t = [0.0; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = linear(i as u8);
    }
    t
}

const WHITE: [f64; 3] = [0.950_47, 1.0, 1.088_83];

/// CIE L\*a\*b\* of an sRGB pixel.
pub fn lab(rgb: [u8; 3], lut: &[f64; 256]) -> [f64; 3] {
    let (r, g, b) = (
        lut[rgb[0] as usize],
        lut[rgb[1] as usize],
        lut[rgb[2] as usize],
    );
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    let f = |t: f64| {
        const D: f64 = 6.0 / 29.0;
        if t > D * D * D {
            t.cbrt()
        } else {
            t / (3.0 * D * D) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(x / WHITE[0]), f(y / WHITE[1]), f(z / WHITE[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 colour difference (kL = kC = kH = 1).
pub fn ciede2000(p: [f64; 3], q: [f64; 3]) -> f64 {
    let (l1, a1, b1) = (p[0], p[1], p[2]);
    let (l2, a2, b2) = (q[0], q[1], q[2]);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cbar = (c1 + c2) / 2.0;
    let c7 = cbar.powi(7);
    let g = 0.5 * (1.0 - (c7 / (c7 + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1.0 + g) * a1, (1.0 + g) * a2);
    let (c1p, c2p) = ((a1p * a1p + b1 * b1).sqrt(), (a2p * a2p + b2 * b2).sqrt());
    let hue = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            0.0
        } else {
            let h = b.atan2(a).to_degrees();
            if h < 0.0 {
                h + 360.0
            } else {
                h
            }
        }
    };
    let (h1p, h2p) = (hue(b1, a1p), hue(b2, a2p));
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > 180.0 {
            d - 360.0
        } else if d < -180.0 {
            d + 360.0
        } else {
            d
        }
    };
    let dhp_big = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lbarp = (l1 + l2) / 2.0;
    let cbarp = (c1p + c2p) / 2.0;
    let hbarp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hbarp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * hbarp).to_radians().cos()
        + 0.32 * (3.0 * hbarp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hbarp - 63.0).to_radians().cos();
    let dtheta = 30.0 * (-((hbarp - 275.0) / 25.0).powi(2)).exp();
    let cbarp7 = cbarp.powi(7);
    let rc = 2.0 * (cbarp7 / (cbarp7 + 25f64.powi(7))).sqrt();
    let l50 = (lbarp - 50.0).powi(2);
    let sl = 1.0 + 0.015 * l50 / (20.0 + l50).sqrt();
    let sc = 1.0 + 0.045 * cbarp;
    let sh = 1.0 + 0.015 * cbarp * t;
    let rt = -(2.0 * dtheta).to_radians().sin() * rc;
    let (x, y, z) = (dlp / sl, dcp / sc, dhp_big / sh);
    let v = (x * x + y * y + z * z + rt * y * z).sqrt();
    if v.is_finite() {
        v
    } else {
        f64::NAN
    }
}

fn luma(p: [u8; 3]) -> u8 {
    ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8
}

/// Flat: the 3 × 3 luma range around (x, y) is at most `FLAT`.
fn flat(img: &[u8], w: usize, h: usize, x: usize, y: usize) -> bool {
    let (mut lo, mut hi) = (255u8, 0u8);
    for yy in y.saturating_sub(1)..(y + 2).min(h) {
        for xx in x.saturating_sub(1)..(x + 2).min(w) {
            let i = (yy * w + xx) * 3;
            let l = luma([img[i], img[i + 1], img[i + 2]]);
            lo = lo.min(l);
            hi = hi.max(l);
        }
    }
    hi - lo <= FLAT
}

fn px(img: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * w + x) * 3;
    [img[i], img[i + 1], img[i + 2]]
}

/// Compare two RGB images of the same size (`w × h`, 3 bytes per
/// pixel, opaque — alpha already composited on white). `None` when the
/// sizes disagree with the buffers or fewer than `MIN_COMPARED` pixels
/// are comparable.
pub fn compare(ours: &[u8], reference: &[u8], w: usize, h: usize) -> Option<ColourStats> {
    let n = w.checked_mul(h)?.checked_mul(3)?;
    if ours.len() != n || reference.len() != n || w == 0 || h == 0 {
        return None;
    }
    let lut = linear_lut();
    let (tw, th) = (w.div_ceil(TILE), h.div_ceil(TILE));
    let mut tiles: Vec<(usize, f64, usize)> = vec![(0, 0.0, 0); tw * th];
    // ΔE00 histogram in 0.1 steps up to 200 (the largest ΔE00 between
    // sRGB colours is about 120).
    let mut hist = vec![0usize; 2001];
    let (mut count, mut d5, mut d10, mut hue10) = (0usize, 0usize, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (px(ours, w, x, y), px(reference, w, x, y));
            let drawn = |p: [u8; 3]| p.iter().any(|c| *c < MARK);
            if !(drawn(a) && drawn(b)) || !flat(ours, w, h, x, y) || !flat(reference, w, h, x, y) {
                continue;
            }
            let la = lab(a, &lut);
            let mut lb = lab(b, &lut);
            let mut d = ciede2000(la, lb);
            if d > NEAR_DE {
                for yy in y.saturating_sub(1)..(y + 2).min(h) {
                    for xx in x.saturating_sub(1)..(x + 2).min(w) {
                        let ln = lab(px(reference, w, xx, yy), &lut);
                        let dn = ciede2000(la, ln);
                        if dn < d {
                            d = dn;
                            lb = ln;
                        }
                    }
                }
            }
            if !d.is_finite() {
                continue;
            }
            count += 1;
            hist[((d * 10.0).round() as usize).min(2000)] += 1;
            let hue = d > 10.0 && (la[0] - lb[0]).abs() < 5.0;
            d5 += (d > 5.0) as usize;
            d10 += (d > 10.0) as usize;
            hue10 += hue as usize;
            let t = &mut tiles[(y / TILE) * tw + x / TILE];
            t.0 += 1;
            t.1 += d;
            t.2 += hue as usize;
        }
    }
    if count < MIN_COMPARED {
        return None;
    }
    let pct = |q: f64| {
        let target = ((count as f64) * q).ceil().max(1.0) as usize;
        let mut acc = 0usize;
        for (i, c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return i as f64 / 10.0;
            }
        }
        200.0
    };
    let judged = tiles.iter().filter(|t| t.0 >= TILE_MIN);
    let worst_tile_mean = judged.clone().map(|t| t.1 / t.0 as f64).fold(0.0, f64::max);
    let hue_tiles = judged.filter(|t| t.2 * 2 > t.0).count();
    let c = count as f64;
    Some(ColourStats {
        compared_px: count,
        de_median: pct(0.5),
        de_p95: pct(0.95),
        de_p99: pct(0.99),
        share_de5: d5 as f64 / c,
        share_de10: d10 as f64 / c,
        share_hue10: hue10 as f64 / c,
        worst_tile_mean,
        hue_tiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sharma, Wu, Dalal (2005), Table 1: all 34 pairs (also checked
    /// against colour-science 0.4.7, `delta_E(method="CIE 2000")`).
    #[test]
    fn ciede2000_matches_the_published_pairs() {
        #[rustfmt::skip]
        let pairs: [([f64; 3], [f64; 3], f64); 34] = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, -1.3802, -84.2814], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, -1.1848, -84.8006], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, -0.9009, -85.5211], [50.0, 0.0, -82.7485], 1.0000),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, -1.0, 2.0], [50.0, 0.0, 0.0], 2.3669),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0009], 7.1792),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0010], 7.1792),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0011], 7.2195),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0012], 7.2195),
            ([50.0, -0.001, 2.49], [50.0, 0.0009, -2.49], 4.8045),
            ([50.0, -0.001, 2.49], [50.0, 0.0010, -2.49], 4.8045),
            ([50.0, -0.001, 2.49], [50.0, 0.0011, -2.49], 4.7461),
            ([50.0, 2.5, 0.0], [50.0, 0.0, -2.5], 4.3065),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([50.0, 2.5, 0.0], [61.0, -5.0, 29.0], 22.8977),
            ([50.0, 2.5, 0.0], [56.0, -27.0, -3.0], 31.9030),
            ([50.0, 2.5, 0.0], [58.0, 24.0, 15.0], 19.4535),
            ([50.0, 2.5, 0.0], [50.0, 3.1736, 0.5854], 1.0000),
            ([50.0, 2.5, 0.0], [50.0, 3.2972, 0.0], 1.0000),
            ([50.0, 2.5, 0.0], [50.0, 1.8634, 0.5757], 1.0000),
            ([50.0, 2.5, 0.0], [50.0, 3.2592, 0.3350], 1.0000),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([63.0109, -31.0961, -5.8663], [62.8187, -29.7946, -4.0864], 1.2630),
            ([61.2901, 3.7196, -5.3901], [61.4292, 2.2480, -4.9620], 1.8731),
            ([35.0831, -44.1164, 3.7933], [35.0232, -40.0716, 1.5901], 1.8645),
            ([22.7233, 20.0904, -46.6940], [23.0331, 14.9730, -42.5619], 2.0373),
            ([36.4612, 47.8580, 18.3852], [36.2715, 50.5065, 21.2231], 1.4146),
            ([90.8027, -2.0831, 1.4410], [91.1528, -1.6435, 0.0447], 1.4441),
            ([90.9257, -0.5406, -0.9208], [88.6381, -0.8985, -0.7239], 1.5381),
            ([6.7747, -0.2908, -2.4247], [5.8714, -0.0985, -2.2286], 0.6377),
            ([2.0776, 0.0795, -1.1350], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (i, (p, q, want)) in pairs.iter().enumerate() {
            let got = ciede2000(*p, *q);
            assert!((got - want).abs() < 5e-5, "pair {}: {got} vs {want}", i + 1);
            // Symmetric.
            assert!(
                (ciede2000(*q, *p) - got).abs() < 1e-9,
                "pair {} not symmetric",
                i + 1
            );
        }
        assert_eq!(ciede2000([50.0, 0.0, 0.0], [50.0, 0.0, 0.0]), 0.0);
    }

    /// sRGB → L*a*b* against colour-science with the same white point.
    #[test]
    fn srgb_to_lab_matches_the_reference() {
        let lut = linear_lut();
        for (rgb, want) in [
            ([255, 255, 255], [100.0, 0.0, 0.0]),
            ([255, 0, 0], [53.2408, 80.0925, 67.2032]),
            ([0, 255, 0], [87.7347, -86.1827, 83.1793]),
            ([0, 0, 255], [32.2970, 79.1875, -107.8602]),
            ([128, 128, 128], [53.5850, 0.0, 0.0]),
            ([0, 255, 255], [91.1132, -48.0875, -14.1312]),
            ([10, 20, 30], [5.9485, -0.6687, -8.1375]),
        ] {
            let got = lab(rgb, &lut);
            for k in 0..3 {
                assert!(
                    (got[k] - want[k]).abs() < 1e-3,
                    "{rgb:?}: {got:?} vs {want:?}"
                );
            }
        }
        // Known differences (colour-science): red vs green, pure red vs
        // a SWOP-like red, a channel swap.
        let d = |a: [u8; 3], b: [u8; 3]| ciede2000(lab(a, &lut), lab(b, &lut));
        assert!((d([255, 0, 0], [0, 255, 0]) - 86.6082).abs() < 1e-3);
        assert!((d([255, 0, 0], [237, 28, 36]) - 5.4857).abs() < 1e-3);
        assert!((d([200, 50, 30], [30, 50, 200]) - 46.3939).abs() < 1e-3);
    }

    /// A `w × h` image filled with `bg`, and a `[x0, y0, x1, y1)`
    /// rectangle of `fg`.
    fn img(w: usize, h: usize, bg: [u8; 3], rect: [usize; 4], fg: [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3];
                v.extend_from_slice(if inside { &fg } else { &bg });
            }
        }
        v
    }

    const W: [u8; 3] = [255, 255, 255];
    const R: [usize; 4] = [8, 8, 56, 56];

    fn stats(a: [u8; 3], b: [u8; 3]) -> ColourStats {
        compare(&img(64, 64, W, R, a), &img(64, 64, W, R, b), 64, 64).unwrap()
    }

    #[test]
    fn identical_hue_lightness_and_chroma_changes() {
        let same = stats([200, 40, 40], [200, 40, 40]);
        assert_eq!(
            (same.de_p95, same.share_de10, same.hue_tiles),
            (0.0, 0.0, 0)
        );
        // Hue only: red → green-ish at similar lightness.
        let hue = stats([200, 60, 60], [62, 124, 62]);
        assert!(
            hue.de_median > 10.0 && hue.share_hue10 > 0.9 && hue.hue_tiles > 0,
            "{hue:?}"
        );
        // Lightness only: same hue, darker — large ΔE, not a hue change.
        let light = stats([200, 60, 60], [100, 30, 30]);
        assert!(
            light.de_median > 10.0 && light.share_hue10 == 0.0,
            "{light:?}"
        );
        // Chroma only: a red against a grey of the same lightness.
        let chroma = stats([200, 60, 60], [111, 111, 111]);
        assert!(chroma.share_hue10 > 0.9, "{chroma:?}");
        // RGB channel swap.
        let swap = stats([200, 50, 30], [30, 50, 200]);
        assert!(swap.de_median > 40.0, "{swap:?}");
    }

    #[test]
    fn grey_images_and_white_pages() {
        let g = stats([90, 90, 90], [90, 90, 90]);
        assert_eq!(g.de_p99, 0.0);
        // Nothing drawn: not comparable.
        assert!(compare(&img(64, 64, W, R, W), &img(64, 64, W, R, W), 64, 64).is_none());
        // A tiny mark (3 × 3): fewer than MIN_COMPARED pixels.
        let a = img(64, 64, W, [10, 10, 13, 13], [255, 0, 0]);
        assert!(compare(&a, &a, 64, 64).is_none());
    }

    #[test]
    fn placement_differences_are_not_colour_differences() {
        // The same red square one pixel to the right: edges are not
        // flat and the neighbourhood absorbs the offset.
        let a = img(64, 64, W, R, [220, 30, 30]);
        let b = img(64, 64, W, [9, 8, 57, 56], [220, 30, 30]);
        let s = compare(&a, &b, 64, 64).unwrap();
        assert_eq!((s.share_de5, s.hue_tiles), (0.0, 0), "{s:?}");
        // An anti-aliased coloured edge (a 1-px blended border on one
        // side only) is not flat and is not compared.
        let mut c = img(64, 64, W, R, [30, 30, 220]);
        for y in 8..56 {
            let i = (y * 64 + 56) * 3;
            c[i..i + 3].copy_from_slice(&[142, 142, 237]);
        }
        let s = compare(&img(64, 64, W, R, [30, 30, 220]), &c, 64, 64).unwrap();
        assert_eq!(s.share_de5, 0.0, "{s:?}");
        // A different region (shape) entirely: shares nothing, so no
        // colour statistics at all rather than a false colour error.
        let d = img(64, 64, W, [0, 0, 20, 20], [220, 30, 30]);
        let e = img(64, 64, W, [40, 40, 64, 64], [220, 30, 30]);
        assert!(compare(&d, &e, 64, 64).is_none());
    }

    #[test]
    fn malformed_buffers_are_not_compared() {
        let a = img(4, 4, W, [0, 0, 4, 4], [0, 0, 0]);
        assert!(compare(&a, &a[..10], 4, 4).is_none());
        assert!(compare(&a, &a, 5, 4).is_none());
        assert!(compare(&[], &[], 0, 0).is_none());
        assert!(compare(&a, &a, usize::MAX, 2).is_none());
        // NaN never comes out of a valid conversion; a NaN input is
        // reported as NaN, not as a small difference.
        assert!(ciede2000([f64::NAN, 0.0, 0.0], [50.0, 0.0, 0.0]).is_nan());
    }
}
