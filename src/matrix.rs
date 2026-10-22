//! 2D affine transformation matrix for PDF content streams
//! (ISO 32000-1:2008, §8.3.2–8.3.4).
//!
//! A PDF CTM is a 6-element matrix `[a b c d e f]` applied to a point
//! `(x, y)` as:
//!
//! ```text
//! x' = a·x + c·y + e
//! y' = b·x + d·y + f
//! ```
//!
//! The `cm` operator concatenates the new matrix to the *left* of the
//! existing CTM (post-multiplication in the common row-vector
//! convention): `CTM' = cm × CTM`. [`Matrix::concat`] implements this
//! order so that `existing.concat(new)` computes `new × existing`.

use std::fmt;

/// A 2×3 affine matrix `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    /// a — horizontal scale / cos θ
    pub a: f64,
    /// b — skew / sin θ
    pub b: f64,
    /// c — skew / −sin θ
    pub c: f64,
    /// d — vertical scale / cos θ
    pub d: f64,
    /// e — horizontal translation
    pub e: f64,
    /// f — vertical translation
    pub f: f64,
}

impl Matrix {
    /// The identity transform.
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Build from the six `cm` operator arguments `[a b c d e f]`.
    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Matrix {
        Matrix { a, b, c, d, e, f }
    }

    /// Concatenate `other` *after* `self` (i.e. apply `self` first, then
    /// `other`). In PDF's row-vector convention (`[x y 1] × M`) this is
    /// the product `self × other`.
    ///
    /// For a PDF `cm` operand (and a form `/Matrix`, Type3
    /// `/FontMatrix`, …) the spec says `CTM' = M × CTM` — the new
    /// matrix maps the *new* user space into the old one, so it is
    /// applied first. The caller therefore writes
    /// `ctm = m.concat(ctm)`, **not** `ctm.concat(m)`; the latter puts
    /// `m` after the CTM and misplaces everything drawn under a nested
    /// transform (see `tests/render_basic.rs::nested_cm_order`).
    pub fn concat(self, other: Matrix) -> Matrix {
        Matrix {
            a: other.a * self.a + other.c * self.b,
            b: other.b * self.a + other.d * self.b,
            c: other.a * self.c + other.c * self.d,
            d: other.b * self.c + other.d * self.d,
            e: other.a * self.e + other.c * self.f + other.e,
            f: other.b * self.e + other.d * self.f + other.f,
        }
    }

    /// Apply the transform to a point `(x, y)`.
    pub fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Translation matrix `[1 0 0 1 tx ty]`.
    pub fn translate(tx: f64, ty: f64) -> Matrix {
        Matrix::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Scale matrix `[sx 0 0 sy 0 0]`.
    pub fn scale(sx: f64, sy: f64) -> Matrix {
        Matrix::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Rotation matrix `[cos −sin sin cos 0 0]` for `deg` degrees.
    pub fn rotate(deg: f64) -> Matrix {
        let rad = deg.to_radians();
        let (s, c) = rad.sin_cos();
        Matrix::new(c, s, -s, c, 0.0, 0.0)
    }

    /// True if every component is finite (not NaN or infinite).
    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }
}

impl fmt::Display for Matrix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{} {} {} {} {} {}]",
            fmt_num(self.a),
            fmt_num(self.b),
            fmt_num(self.c),
            fmt_num(self.d),
            fmt_num(self.e),
            fmt_num(self.f)
        )
    }
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
