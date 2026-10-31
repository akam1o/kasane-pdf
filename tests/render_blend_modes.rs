//! Blend modes (ISO 32000-1 §11.3.5) and the page group (§11.4.7).
//!
//! Every mode is checked on three painting paths — a filled path, an
//! image and a transparency group (form XObject) — against the
//! specification's formulas evaluated here, independently of the
//! renderer: B(cb, cs) for the separable modes (Table 136) and the
//! SetLum / SetSat / ClipColor construction for the non-separable ones
//! (§11.3.5.3). The backdrop is an opaque colour, so the result is
//! B(cb, cs) itself.
//!
//! The page group is isolated with a *transparent* backdrop and only
//! then composited on white (§11.4.7): an object blended where nothing
//! was painted shows its own colour, whatever the blend mode.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, RenderOptions};

const MODES: [&str; 16] = [
    "Normal",
    "Multiply",
    "Screen",
    "Overlay",
    "Darken",
    "Lighten",
    "ColorDodge",
    "ColorBurn",
    "HardLight",
    "SoftLight",
    "Difference",
    "Exclusion",
    "Hue",
    "Saturation",
    "Color",
    "Luminosity",
];

fn lum(c: [f64; 3]) -> f64 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f64; 3]) -> [f64; 3] {
    let l = lum(c);
    let n = c.iter().cloned().fold(f64::MAX, f64::min);
    let x = c.iter().cloned().fold(f64::MIN, f64::max);
    let mut c = c;
    if n < 0.0 {
        c = c.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 {
        c = c.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    c
}

fn set_lum(c: [f64; 3], l: f64) -> [f64; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f64; 3]) -> f64 {
    c.iter().cloned().fold(f64::MIN, f64::max) - c.iter().cloned().fold(f64::MAX, f64::min)
}

fn set_sat(c: [f64; 3], s: f64) -> [f64; 3] {
    let mut i = [0, 1, 2];
    i.sort_by(|a, b| c[*a].partial_cmp(&c[*b]).unwrap());
    let (lo, mid, hi) = (i[0], i[1], i[2]);
    let mut out = [0.0; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out
}

/// §11.3.5, Table 136 / 137.
fn blend(mode: &str, cb: [f64; 3], cs: [f64; 3]) -> [f64; 3] {
    let sep = |f: &dyn Fn(f64, f64) -> f64| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    let screen = |b: f64, s: f64| b + s - b * s;
    let hard = |b: f64, s: f64| {
        if s <= 0.5 {
            b * 2.0 * s
        } else {
            screen(b, 2.0 * s - 1.0)
        }
    };
    match mode {
        "Normal" => cs,
        "Multiply" => sep(&|b, s| b * s),
        "Screen" => sep(&screen),
        "Overlay" => sep(&|b, s| hard(s, b)),
        "Darken" => sep(&|b, s| b.min(s)),
        "Lighten" => sep(&|b, s| b.max(s)),
        "ColorDodge" => sep(&|b, s| {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        "ColorBurn" => sep(&|b, s| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        "HardLight" => sep(&hard),
        "SoftLight" => sep(&|b, s| {
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
        }),
        "Difference" => sep(&|b, s| (b - s).abs()),
        "Exclusion" => sep(&|b, s| b + s - 2.0 * b * s),
        "Hue" => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        "Saturation" => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        "Color" => set_lum(cs, lum(cb)),
        "Luminosity" => set_lum(cb, lum(cs)),
        _ => unreachable!(),
    }
}

const CB: [f64; 3] = [0.2, 0.6, 0.9];
const CS: [f64; 3] = [0.8, 0.3, 0.55];

/// A 30 × 10 page: the backdrop rectangle covers x 0..20; three 6-pt
/// squares paint `CS` in `mode` — a path at x 1, a 1 × 1 image at x 8,
/// a group form at x 22 (outside the backdrop: over nothing).
fn pdf(mode: &str) -> Vec<u8> {
    let c = |v: [f64; 3]| format!("{} {} {}", v[0], v[1], v[2]);
    let content = format!(
        "{} rg 0 0 20 10 re f /GS gs {} rg 1 2 6 6 re f \
         q 6 0 0 6 8 2 cm /Im Do Q /Fm Do \
         /GS0 gs q 6 0 0 6 15 2 cm /Fm1 Do Q",
        c(CB),
        c(CS)
    );
    let px = CS.map(|v| (v * 255.0).round() as u8);
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 30 10] /Contents 4 0 R \
              /Resources << /ExtGState << /GS 5 0 R /GS0 8 0 R >> \
              /XObject << /Im 6 0 R /Fm 7 0 R /Fm1 9 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (5, format!("<< /Type /ExtGState /BM /{mode} >>").into_bytes()),
        (
            6,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceRGB",
                &px,
            ),
        ),
        (
            7,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 30 10] /Group << /S /Transparency >>",
                format!("{} rg 22 2 6 6 re f", c(CS)).as_bytes(),
            ),
        ),
        (8, b"<< /Type /ExtGState /BM /Normal >>".to_vec()),
        // A group drawn with the mode in force inside, over the backdrop.
        (
            9,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /ExtGState << /G 5 0 R >> >>",
                format!("/G gs {} rg 0 0 1 1 re f", c(CS)).as_bytes(),
            ),
        ),
    ])
}

fn rgb_at(doc: &Document, x: u32, y: u32) -> [f64; 3] {
    let r = render_page(doc, &doc.pages()[0], &RenderOptions::new().with_dpi(72.0)).unwrap();
    let p = r.pixmap().pixel(x, y).unwrap();
    [
        p.red() as f64 / 255.0,
        p.green() as f64 / 255.0,
        p.blue() as f64 / 255.0,
    ]
}

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2.5 / 255.0)
}

#[test]
fn every_mode_follows_the_specification_on_every_painting_path() {
    let mut bad = Vec::new();
    for mode in MODES {
        let doc = Document::from_bytes(&pdf(mode)).unwrap();
        let want = blend(mode, CB, CS);
        // y = 5 (page y 5) is inside every square.
        for (what, x) in [("path", 4), ("image", 11), ("form", 18)] {
            let got = rgb_at(&doc, x, 5);
            if !close(got, want) {
                bad.push(format!("{mode} {what}: got {got:.3?}, want {want:.3?}"));
            }
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}

#[test]
fn the_page_group_backdrop_is_transparent() {
    // A group composited with a blend mode where the page has nothing:
    // α_b = 0, so the result is the group's own colour (§11.3.6, §11.4.7)
    // — on white paper, CS — not B(white, CS).
    let mut bad = Vec::new();
    for mode in MODES {
        let doc = Document::from_bytes(&pdf(mode)).unwrap();
        let got = rgb_at(&doc, 25, 5);
        if !close(got, CS) {
            bad.push(format!("{mode}: got {got:.3?}, want {CS:.3?}"));
        }
    }
    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}
