//! Cutting painted paths at redaction rects (`redact`).
//!
//! A path that crosses a rect's edge is rewritten so that nothing it
//! paints reaches inside the rect, while what it paints outside stays:
//!
//! - **Fills** of axis-aligned rectangles (in page space) become the
//!   rectangles' difference with the rects — exact. Several rectangles
//!   are cut only when they do not overlap (so the fill rule does not
//!   matter).
//! - **Strokes** of straight segments are cut along their centreline:
//!   the part within the rect grown by the stroke's reach (half the
//!   line width under the CTM's largest stretch, √2 times that for a
//!   square cap) is removed, so the remaining band, caps included,
//!   stays outside the rect. A corner kept next to a rect must clear
//!   it by its join's own reach (a miter's tip), or the path refuses.
//!   Dashed strokes refuse (the remaining pieces would restart the
//!   dash pattern).
//!
//! Anything else that comes near a rect — curves, other fill shapes,
//! overlapping rectangles — refuses. A path that keeps clear of every
//! rect is left exactly as it was.

use crate::content::Operation;
use crate::error::Error;
use crate::matrix::Matrix;
use crate::page::Rect;

/// The stroke and transform in effect when a path was painted.
#[derive(Debug, Clone)]
pub(crate) struct PathPaintState {
    pub(crate) ctm: Matrix,
    pub(crate) width: f64,
    pub(crate) cap: i64,
    pub(crate) join: i64,
    pub(crate) miter_limit: f64,
    pub(crate) dashed: bool,
}

/// The outcome of [`cut_path`].
pub(crate) enum Cut {
    /// Nothing the path paints reaches inside a rect.
    Unchanged,
    /// Replace the path object (its construction ops and painting op)
    /// with these ops.
    Replace(Vec<Operation>),
}

/// Page-space polylines with their closed flag.
type Lines = Vec<(Vec<(f64, f64)>, bool)>;

/// A segment's ends and its kept parameter intervals.
type Segment = ((f64, f64), (f64, f64), Vec<(f64, f64)>);

struct Subpath {
    /// Page-space points.
    pts: Vec<(f64, f64)>,
    closed: bool,
    curved: bool,
}

fn unsupported(detail: impl Into<String>) -> Error {
    Error::Unsupported {
        feature: "redaction of a path crossing a rect's edge",
        detail: detail.into(),
    }
}

/// Cut the path built by `construction` and painted by `paint` at
/// `rects` (page space).
pub(crate) fn cut_path(
    construction: &[Operation],
    paint: &Operation,
    st: &PathPaintState,
    rects: &[Rect],
) -> Result<Cut, Error> {
    use Operation as O;
    let m = st.ctm;
    let inv = invert(&m).ok_or_else(|| unsupported("the CTM is not invertible"))?;
    let tp = |x: f64, y: f64| m.transform(x, y);

    // ---- parse the construction into page-space subpaths -----------
    let mut subs: Vec<Subpath> = Vec::new();
    for op in construction {
        match op {
            O::MoveTo(x, y) => subs.push(Subpath {
                pts: vec![tp(*x, *y)],
                closed: false,
                curved: false,
            }),
            O::LineTo(x, y) => match subs.last_mut() {
                Some(s) if !s.closed => s.pts.push(tp(*x, *y)),
                _ => return Err(unsupported("a segment without a current point")),
            },
            O::CurveTo(x1, y1, x2, y2, x3, y3) => {
                let s = subs
                    .last_mut()
                    .ok_or_else(|| unsupported("a curve without a current point"))?;
                s.curved = true;
                s.pts.extend([tp(*x1, *y1), tp(*x2, *y2), tp(*x3, *y3)]);
            }
            O::CurveToV(x2, y2, x3, y3) | O::CurveToY(x2, y2, x3, y3) => {
                let s = subs
                    .last_mut()
                    .ok_or_else(|| unsupported("a curve without a current point"))?;
                s.curved = true;
                s.pts.extend([tp(*x2, *y2), tp(*x3, *y3)]);
            }
            O::ClosePath => {
                if let Some(s) = subs.last_mut() {
                    s.closed = true;
                }
            }
            O::Rectangle(x, y, w, h) => subs.push(Subpath {
                pts: vec![
                    tp(*x, *y),
                    tp(*x + *w, *y),
                    tp(*x + *w, *y + *h),
                    tp(*x, *y + *h),
                ],
                closed: true,
                curved: false,
            }),
            _ => return Err(unsupported(format!("{} in a path", op.name()))),
        }
    }
    let (fill, even_odd, stroke) = match paint {
        O::Stroke => (false, false, true),
        O::CloseAndStroke => {
            if let Some(s) = subs.last_mut() {
                s.closed = true;
            }
            (false, false, true)
        }
        O::Fill => (true, false, false),
        O::FillEvenOdd => (true, true, false),
        O::FillAndStroke => (true, false, true),
        O::FillAndStrokeEvenOdd => (true, true, true),
        O::CloseFillAndStroke | O::CloseFillAndStrokeEvenOdd => {
            if let Some(s) = subs.last_mut() {
                s.closed = true;
            }
            (true, matches!(paint, O::CloseFillAndStrokeEvenOdd), true)
        }
        _ => return Err(unsupported(format!("painting op {}", paint.name()))),
    };
    if !m.is_finite() || rects.is_empty() {
        return Err(unsupported("non-finite geometry"));
    }

    // A path is touched when it comes within SLACK of a rect (or of its
    // reach), and then cut at twice that: the cut pieces, mapped to
    // user space and back, stay clear of the touch test, so running
    // the cut again on the output changes nothing.
    let tested: Vec<Rect> = rects.iter().map(|r| grow(r, SLACK)).collect();

    // ---- fill --------------------------------------------------------
    let mut fill_rects: Option<Vec<[f64; 4]>> = None; // page-space pieces
    if fill {
        let hull = |s: &Subpath| bbox(&s.pts);
        let near: Vec<bool> = subs
            .iter()
            .map(|s| tested.iter().any(|r| overlaps_open(&hull(s), r)))
            .collect();
        if near.iter().any(|&n| n) {
            let boxes: Vec<[f64; 4]> = subs
                .iter()
                .map(|s| {
                    axis_rect(s).ok_or_else(|| {
                        unsupported("a filled shape that is not an axis-aligned rectangle")
                    })
                })
                .collect::<Result<_, _>>()?;
            for (i, a) in boxes.iter().enumerate() {
                for b in &boxes[i + 1..] {
                    if a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3] {
                        return Err(unsupported("overlapping filled rectangles"));
                    }
                }
            }
            let _ = even_odd; // disjoint pieces fill alike under either rule
            let mut pieces = Vec::new();
            for b in boxes {
                let mut parts = vec![b];
                for r in rects {
                    parts = parts
                        .into_iter()
                        .flat_map(|p| subtract(p, &grow(r, 2.0 * SLACK)))
                        .collect();
                }
                pieces.extend(parts);
            }
            fill_rects = Some(pieces);
        }
    }

    // ---- stroke --------------------------------------------------------
    let mut stroke_lines: Option<Lines> = None;
    if stroke {
        let half = st.width.abs() / 2.0 * max_stretch(&m);
        if !(half.is_finite() && half > 0.0) {
            return Err(unsupported("a hairline or non-finite line width"));
        }
        let reach = if st.cap == 2 {
            half * std::f64::consts::SQRT_2
        } else {
            half
        };
        let join_reach = |pts: &[(f64, f64)], i: usize, n: usize, closed: bool| -> f64 {
            // The join at vertex i between the segments into and out
            // of it.
            let prev = if i == 0 {
                if closed {
                    n - 1
                } else {
                    return reach;
                }
            } else {
                i - 1
            };
            let next = if i + 1 == n {
                if closed {
                    0
                } else {
                    return reach;
                }
            } else {
                i + 1
            };
            if st.join != 0 {
                return half.max(reach);
            }
            let (a, b, c) = (pts[prev], pts[i], pts[next]);
            let u = norm((b.0 - a.0, b.1 - a.1));
            let v = norm((c.0 - b.0, c.1 - b.1));
            match (u, v) {
                (Some(u), Some(v)) => {
                    // Angle between the incoming and outgoing directions.
                    let cos = -(u.0 * v.0 + u.1 * v.1);
                    let sin_half = ((1.0 - cos) / 2.0).max(0.0).sqrt();
                    let miter = if sin_half > 0.0 {
                        half / sin_half
                    } else {
                        f64::INFINITY
                    };
                    let limit = if st.miter_limit.is_finite() && st.miter_limit >= 1.0 {
                        st.miter_limit
                    } else {
                        1.0
                    };
                    if miter <= limit * half {
                        miter.max(reach)
                    } else {
                        half.max(reach)
                    }
                }
                _ => half.max(reach),
            }
        };
        let box_of = |r: &Rect, d: f64| [r.llx - d, r.lly - d, r.urx + d, r.ury + d];
        let grown_test: Vec<[f64; 4]> = rects.iter().map(|r| box_of(r, reach + SLACK)).collect();
        let grown: Vec<[f64; 4]> = rects
            .iter()
            .map(|r| box_of(r, reach + 2.0 * SLACK))
            .collect();
        let mut out: Lines = Vec::new();
        let mut changed = false;
        let limit = if st.miter_limit.is_finite() && st.miter_limit >= 1.0 {
            st.miter_limit
        } else {
            1.0
        };
        for s in &subs {
            let n = s.pts.len();
            if s.curved || n < 2 {
                // Keep it only when everything it can paint (control
                // hull grown by the largest join or cap reach) clears.
                let r = half * limit.max(std::f64::consts::SQRT_2);
                let h = bbox(&s.pts);
                let grown_hull = [h[0] - r, h[1] - r, h[2] + r, h[3] + r];
                if rects.iter().any(|q| overlaps_open(&grown_hull, q)) {
                    return Err(unsupported(if s.curved {
                        "a curve near a rect"
                    } else {
                        "a degenerate subpath near a rect"
                    }));
                }
                out.push((s.pts.clone(), s.closed));
                continue;
            }
            // Segments, with the closing one for a closed subpath.
            let seg_count = if s.closed { n } else { n - 1 };
            let segs: Vec<Segment> = (0..seg_count)
                .map(|k| {
                    let (a, b) = (s.pts[k], s.pts[(k + 1) % n]);
                    (a, b, kept_intervals(a, b, &grown))
                })
                .collect();
            let cut_here = segs
                .iter()
                .any(|(a, b, _)| kept_intervals(*a, *b, &grown_test).as_slice() != [(0.0, 1.0)]);
            if !cut_here {
                // Untouched, but every join must clear the rects by
                // its own reach.
                for i in 0..n {
                    let r = join_reach(&s.pts, i, n, s.closed);
                    if rects.iter().any(|q| linf_dist(s.pts[i], q) < r) {
                        return Err(unsupported("a corner whose join reaches a rect"));
                    }
                }
                out.push((s.pts.clone(), s.closed));
                continue;
            }
            changed = true;
            let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
            let mut cur: Vec<(f64, f64)> = Vec::new();
            for (a, b, kept) in &segs {
                for &(t0, t1) in kept {
                    let p1 = lerp(*a, *b, t1);
                    if t0 == 0.0 && cur.last() == Some(a) {
                        cur.push(p1);
                    } else {
                        if cur.len() >= 2 {
                            pieces.push(std::mem::take(&mut cur));
                        }
                        cur = vec![lerp(*a, *b, t0), p1];
                    }
                }
            }
            if cur.len() >= 2 {
                pieces.push(cur);
            }
            // A closed subpath: a last piece ending at the start vertex
            // runs on into a first piece starting there.
            if s.closed && pieces.len() >= 2 {
                let start = s.pts[0];
                if pieces[0][0] == start && pieces.last().and_then(|p| p.last()) == Some(&start) {
                    let last = pieces.pop().expect("two or more");
                    let mut merged = last;
                    merged.extend_from_slice(&pieces[0][1..]);
                    pieces[0] = merged;
                }
            }
            for p in pieces {
                // Interior vertices of a piece are original corners
                // with their joins: each must clear the rects.
                for i in 1..p.len() - 1 {
                    let r = join_reach(&p, i, p.len(), false);
                    if rects.iter().any(|q| linf_dist(p[i], q) < r) {
                        return Err(unsupported("a corner whose join reaches a rect"));
                    }
                }
                if p.windows(2).map(|w| dist(w[0], w[1])).sum::<f64>() > 0.0 {
                    out.push((p, false));
                }
            }
        }
        if changed && subs.iter().any(|s| s.curved) {
            return Err(unsupported("a curve in a path cut at a rect"));
        }
        if changed {
            if st.dashed {
                return Err(unsupported("a dashed stroke near a rect"));
            }
            stroke_lines = Some(out);
        }
    }

    if fill_rects.is_none() && stroke_lines.is_none() {
        return Ok(Cut::Unchanged);
    }

    // ---- emit, in user space -----------------------------------------
    let back = |p: (f64, f64)| inv.transform(p.0, p.1);
    let mut ops = Vec::new();
    if fill {
        match &fill_rects {
            Some(rs) => {
                for r in rs {
                    let a = back((r[0], r[1]));
                    let b = back((r[2], r[1]));
                    let c = back((r[2], r[3]));
                    let d = back((r[0], r[3]));
                    ops.push(O::MoveTo(a.0, a.1));
                    ops.push(O::LineTo(b.0, b.1));
                    ops.push(O::LineTo(c.0, c.1));
                    ops.push(O::LineTo(d.0, d.1));
                    ops.push(O::ClosePath);
                }
                if rs.is_empty() {
                    // Nothing left to fill.
                } else {
                    ops.push(O::Fill);
                }
            }
            None => {
                // The fill is untouched: paint it as before (rule kept).
                ops.extend(construction.iter().cloned());
                ops.push(if even_odd { O::FillEvenOdd } else { O::Fill });
            }
        }
    }
    if stroke {
        let lines: Lines = match stroke_lines {
            Some(l) => l,
            None => subs.iter().map(|s| (s.pts.clone(), s.closed)).collect(),
        };
        if lines.iter().any(|_| true) && subs.iter().all(|s| !s.curved) {
            let before = ops.len();
            for (pts, closed) in &lines {
                let p0 = back(pts[0]);
                ops.push(O::MoveTo(p0.0, p0.1));
                for p in &pts[1..] {
                    let q = back(*p);
                    ops.push(O::LineTo(q.0, q.1));
                }
                if *closed {
                    ops.push(O::ClosePath);
                }
            }
            if ops.len() > before {
                ops.push(O::Stroke);
            }
        } else if !lines.is_empty() {
            // Untouched curved stroke: as before.
            ops.extend(construction.iter().cloned());
            ops.push(O::Stroke);
        }
    }
    Ok(Cut::Replace(ops))
}

/// Page-space margin, far above rounding error and far below anything
/// visible (a millionth of a point).
const SLACK: f64 = 1e-6;

fn grow(r: &Rect, d: f64) -> Rect {
    Rect {
        llx: r.llx - d,
        lly: r.lly - d,
        urx: r.urx + d,
        ury: r.ury + d,
    }
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m.a * m.d - m.b * m.c;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let a = m.d / det;
    let b = -m.b / det;
    let c = -m.c / det;
    let d = m.a / det;
    let e = -(m.e * a + m.f * c);
    let f = -(m.e * b + m.f * d);
    Some(Matrix::new(a, b, c, d, e, f))
}

/// The largest stretch of `m`'s linear part (its largest singular
/// value).
fn max_stretch(m: &Matrix) -> f64 {
    let (a, b, c, d) = (m.a, m.b, m.c, m.d);
    let s = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    ((s + (s * s - 4.0 * det * det).max(0.0).sqrt()) / 2.0).sqrt()
}

fn bbox(p: &[(f64, f64)]) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for &(x, y) in p {
        b[0] = b[0].min(x);
        b[1] = b[1].min(y);
        b[2] = b[2].max(x);
        b[3] = b[3].max(y);
    }
    b
}

/// Whether box `b` and rect `r` share interior points.
fn overlaps_open(b: &[f64; 4], r: &Rect) -> bool {
    b[0] < r.urx && b[2] > r.llx && b[1] < r.ury && b[3] > r.lly
}

/// A closed four-point subpath (or `re`) that is an axis-aligned
/// rectangle in page space.
fn axis_rect(s: &Subpath) -> Option<[f64; 4]> {
    if s.curved {
        return None;
    }
    let mut p = s.pts.clone();
    if p.len() == 5 && p[0] == p[4] {
        p.pop();
    }
    if p.len() != 4 {
        return None;
    }
    let [a, b, c, d] = [p[0], p[1], p[2], p[3]];
    let ok = (a.0 == b.0 && c.0 == d.0 && b.1 == c.1 && d.1 == a.1)
        || (a.1 == b.1 && c.1 == d.1 && b.0 == c.0 && d.0 == a.0);
    ok.then(|| bbox(&p))
}

/// `b` minus `r`: up to four boxes (below, above, left, right).
fn subtract(b: [f64; 4], r: &Rect) -> Vec<[f64; 4]> {
    if !(b[0] < r.urx && b[2] > r.llx && b[1] < r.ury && b[3] > r.lly) {
        return vec![b];
    }
    let mut out = Vec::new();
    if b[1] < r.lly {
        out.push([b[0], b[1], b[2], r.lly]);
    }
    if b[3] > r.ury {
        out.push([b[0], r.ury, b[2], b[3]]);
    }
    let (y0, y1) = (b[1].max(r.lly), b[3].min(r.ury));
    if b[0] < r.llx {
        out.push([b[0], y0, r.llx, y1]);
    }
    if b[2] > r.urx {
        out.push([r.urx, y0, b[2], y1]);
    }
    out.retain(|q| q[2] > q[0] && q[3] > q[1]);
    out
}

/// The parameter intervals of segment `a`→`b` outside every box of
/// `grown` (each box's interior removed), in order.
fn kept_intervals(a: (f64, f64), b: (f64, f64), grown: &[[f64; 4]]) -> Vec<(f64, f64)> {
    let mut kept = vec![(0.0f64, 1.0f64)];
    for g in grown {
        let Some((t0, t1)) = clip_segment(a, b, g) else {
            continue;
        };
        let mut next = Vec::new();
        for (s0, s1) in kept {
            if t1 <= s0 || t0 >= s1 {
                next.push((s0, s1));
                continue;
            }
            if s0 < t0 {
                next.push((s0, t0));
            }
            if t1 < s1 {
                next.push((t1, s1));
            }
        }
        kept = next;
    }
    kept.retain(|(s0, s1)| s1 > s0);
    kept
}

/// The parameter range of `a`→`b` inside box `g` (Liang–Barsky), or
/// `None` when the segment misses its interior.
fn clip_segment(a: (f64, f64), b: (f64, f64), g: &[f64; 4]) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;
    for (p, q) in [
        (-dx, a.0 - g[0]),
        (dx, g[2] - a.0),
        (-dy, a.1 - g[1]),
        (dy, g[3] - a.1),
    ] {
        if p == 0.0 {
            if q <= 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    (t0 < t1).then_some((t0, t1))
}

fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    if t == 0.0 {
        a
    } else if t == 1.0 {
        b
    } else {
        (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
    }
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn norm(v: (f64, f64)) -> Option<(f64, f64)> {
    let l = (v.0 * v.0 + v.1 * v.1).sqrt();
    (l > 0.0 && l.is_finite()).then(|| (v.0 / l, v.1 / l))
}

/// L∞ distance from `p` to rect `r` (0 inside).
fn linf_dist(p: (f64, f64), r: &Rect) -> f64 {
    let dx = (r.llx - p.0).max(p.0 - r.urx).max(0.0);
    let dy = (r.lly - p.1).max(p.1 - r.ury).max(0.0);
    dx.max(dy)
}
