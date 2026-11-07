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

struct Subpath {
    /// Page-space points.
    pts: Vec<(f64, f64)>,
    /// The same points in user space.
    upts: Vec<(f64, f64)>,
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
                upts: vec![(*x, *y)],
                closed: false,
                curved: false,
            }),
            O::LineTo(x, y) => match subs.last_mut() {
                Some(s) if !s.closed => {
                    s.pts.push(tp(*x, *y));
                    s.upts.push((*x, *y));
                }
                _ => return Err(unsupported("a segment without a current point")),
            },
            O::CurveTo(x1, y1, x2, y2, x3, y3) => {
                let s = subs
                    .last_mut()
                    .ok_or_else(|| unsupported("a curve without a current point"))?;
                s.curved = true;
                s.pts.extend([tp(*x1, *y1), tp(*x2, *y2), tp(*x3, *y3)]);
                s.upts.extend([(*x1, *y1), (*x2, *y2), (*x3, *y3)]);
            }
            O::CurveToV(x2, y2, x3, y3) | O::CurveToY(x2, y2, x3, y3) => {
                let s = subs
                    .last_mut()
                    .ok_or_else(|| unsupported("a curve without a current point"))?;
                s.curved = true;
                s.pts.extend([tp(*x2, *y2), tp(*x3, *y3)]);
                s.upts.extend([(*x2, *y2), (*x3, *y3)]);
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
                upts: vec![(*x, *y), (*x + *w, *y), (*x + *w, *y + *h), (*x, *y + *h)],
                closed: true,
                curved: false,
            }),
            _ => return Err(unsupported(format!("{} in a path", op.name()))),
        }
    }
    let closes = matches!(
        paint,
        O::CloseAndStroke | O::CloseFillAndStroke | O::CloseFillAndStrokeEvenOdd
    );
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
    // Worked in user space, where the stroke is defined (§8.5.3.2):
    // each rect maps to a parallelogram there, the band, caps and
    // joins are exact.
    let mut stroke_lines: Option<Lines> = None;
    if stroke {
        let half = st.width.abs() / 2.0;
        if !(half.is_finite() && half > 0.0) {
            return Err(unsupported("a hairline or non-finite line width"));
        }
        // How far a cap reaches past an end along the segment: none for
        // butt; a square's half side for square; a round cap is bounded
        // by that square.
        let ext = if st.cap == 0 { 0.0 } else { half };
        let limit = if st.miter_limit.is_finite() && st.miter_limit >= 1.0 {
            st.miter_limit
        } else {
            1.0
        };
        let quad = |r: &Rect| -> Vec<(f64, f64)> {
            [
                (r.llx, r.lly),
                (r.urx, r.lly),
                (r.urx, r.ury),
                (r.llx, r.ury),
            ]
            .iter()
            .map(|&(x, y)| inv.transform(x, y))
            .collect()
        };
        let quads_test: Vec<Vec<(f64, f64)>> = tested.iter().map(quad).collect();
        let quads_cut: Vec<Vec<(f64, f64)>> =
            rects.iter().map(|r| quad(&grow(r, 2.0 * SLACK))).collect();
        let quads_exact: Vec<Vec<(f64, f64)>> = rects.iter().map(quad).collect();
        // The reach of the join at `pts[i]` between its neighbours, as a
        // distance from the vertex: the miter tip when within the limit,
        // else the bevel / round extent (half the width).
        let join_reach = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> f64 {
            if st.join != 0 {
                return half;
            }
            match (norm((b.0 - a.0, b.1 - a.1)), norm((c.0 - b.0, c.1 - b.1))) {
                (Some(u), Some(v)) => {
                    let cos = -(u.0 * v.0 + u.1 * v.1);
                    let sin_half = ((1.0 - cos) / 2.0).max(0.0).sqrt();
                    if sin_half > 0.0 && 1.0 / sin_half <= limit {
                        half / sin_half
                    } else {
                        half
                    }
                }
                _ => half,
            }
        };
        let clear_of = |p: (f64, f64), r: f64| quads_exact.iter().all(|q| poly_dist(p, q) >= r);
        let mut out: Lines = Vec::new();
        let mut changed = false;
        for s in &subs {
            if s.curved {
                // Keep it only when everything it can paint (the page
                // hull of its control points, grown by the largest reach)
                // clears every rect.
                let r = half * limit.max(std::f64::consts::SQRT_2) * max_stretch(&m);
                let h = bbox(&s.pts);
                let grown_hull = [h[0] - r, h[1] - r, h[2] + r, h[3] + r];
                if rects.iter().any(|q| overlaps_open(&grown_hull, q)) {
                    return Err(unsupported("a curve near a rect"));
                }
                out.push((s.upts.clone(), s.closed));
                continue;
            }
            // Repeated points draw nothing and hide the real joins:
            // drop them, and a closing point equal to the start.
            let mut pts: Vec<(f64, f64)> = Vec::with_capacity(s.upts.len());
            for &p in &s.upts {
                if pts.last() != Some(&p) {
                    pts.push(p);
                }
            }
            if s.closed && pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            let n = pts.len();
            if n < 2 {
                // A dot (round or square caps): within the cap reach.
                if !clear_of(pts[0], half * std::f64::consts::SQRT_2) {
                    return Err(unsupported("a degenerate subpath near a rect"));
                }
                out.push((s.upts.clone(), s.closed));
                continue;
            }
            let seg_count = if s.closed { n } else { n - 1 };
            let seg = |k: usize| (pts[k], pts[(k + 1) % n]);
            // The centreline positions whose stroke cross-section (and
            // cap) would reach into a rect: the rect's parallelogram
            // grown by the cross-section, a convex polygon.
            let forbidden = |a: (f64, f64), b: (f64, f64), quads: &[Vec<(f64, f64)>]| {
                let u = norm((b.0 - a.0, b.1 - a.1)).expect("no repeated points");
                let nrm = (-u.1, u.0);
                let k: Vec<(f64, f64)> = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                    .iter()
                    .map(|&(sa, sb)| {
                        (
                            sa * ext * u.0 + sb * half * nrm.0,
                            sa * ext * u.1 + sb * half * nrm.1,
                        )
                    })
                    .collect();
                quads
                    .iter()
                    .map(|q| {
                        let sum: Vec<(f64, f64)> = q
                            .iter()
                            .flat_map(|p| k.iter().map(move |d| (p.0 + d.0, p.1 + d.1)))
                            .collect();
                        convex_hull(sum)
                    })
                    .collect::<Vec<_>>()
            };
            let cut_here = (0..seg_count).any(|k| {
                let (a, b) = seg(k);
                kept_intervals_poly(a, b, &forbidden(a, b, &quads_test)).as_slice() != [(0.0, 1.0)]
            });
            if !cut_here {
                // Untouched, but every join must clear the rects.
                for i in 0..n {
                    let prev = if i == 0 {
                        if s.closed {
                            n - 1
                        } else {
                            continue;
                        }
                    } else {
                        i - 1
                    };
                    let next = if i + 1 == n {
                        if s.closed {
                            0
                        } else {
                            continue;
                        }
                    } else {
                        i + 1
                    };
                    if !clear_of(pts[i], join_reach(pts[prev], pts[i], pts[next])) {
                        return Err(unsupported("a corner whose join reaches a rect"));
                    }
                }
                out.push((s.upts.clone(), s.closed));
                continue;
            }
            changed = true;
            let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
            let mut cur: Vec<(f64, f64)> = Vec::new();
            for k in 0..seg_count {
                let (a, b) = seg(k);
                for (t0, t1) in kept_intervals_poly(a, b, &forbidden(a, b, &quads_cut)) {
                    let p1 = lerp(a, b, t1);
                    if t0 == 0.0 && cur.last() == Some(&a) {
                        cur.push(p1);
                    } else {
                        if cur.len() >= 2 {
                            pieces.push(std::mem::take(&mut cur));
                        }
                        cur = vec![lerp(a, b, t0), p1];
                    }
                }
            }
            if cur.len() >= 2 {
                pieces.push(cur);
            }
            if s.closed && pieces.len() >= 2 {
                let start = pts[0];
                if pieces[0][0] == start && pieces.last().and_then(|p| p.last()) == Some(&start) {
                    let last = pieces.pop().expect("two or more");
                    let mut merged = last;
                    merged.extend_from_slice(&pieces[0][1..]);
                    pieces[0] = merged;
                }
            }
            for p in pieces {
                for i in 1..p.len() - 1 {
                    if !clear_of(p[i], join_reach(p[i - 1], p[i], p[i + 1])) {
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
        match &stroke_lines {
            Some(lines) => {
                let before = ops.len();
                for (pts, closed) in lines {
                    ops.push(O::MoveTo(pts[0].0, pts[0].1));
                    for p in &pts[1..] {
                        ops.push(O::LineTo(p.0, p.1));
                    }
                    if *closed {
                        ops.push(O::ClosePath);
                    }
                }
                if ops.len() > before {
                    ops.push(O::Stroke);
                }
            }
            None => {
                // The stroke is untouched: paint it as before.
                ops.extend(construction.iter().cloned());
                ops.push(if closes { O::CloseAndStroke } else { O::Stroke });
            }
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

/// Convex hull (counter-clockwise, Andrew's monotone chain).
fn convex_hull(mut p: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    p.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut h: Vec<(f64, f64)> = Vec::new();
    for pass in 0..2 {
        let start = h.len();
        let iter: Box<dyn Iterator<Item = &(f64, f64)>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in iter {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    h
}

/// The parameter intervals of `a`→`b` outside the interior of every
/// convex polygon in `polys` (counter-clockwise), in order.
fn kept_intervals_poly(a: (f64, f64), b: (f64, f64), polys: &[Vec<(f64, f64)>]) -> Vec<(f64, f64)> {
    let mut kept = vec![(0.0f64, 1.0f64)];
    for poly in polys {
        let Some((t0, t1)) = clip_convex(a, b, poly) else {
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

/// The parameter range of `a`→`b` inside a convex polygon's interior
/// (Cyrus–Beck), or `None`.
fn clip_convex(a: (f64, f64), b: (f64, f64), poly: &[(f64, f64)]) -> Option<(f64, f64)> {
    if poly.len() < 3 {
        return None;
    }
    let d = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        // Inward normal of a counter-clockwise edge.
        let nrm = (-(q.1 - p.1), q.0 - p.0);
        let num = nrm.0 * (a.0 - p.0) + nrm.1 * (a.1 - p.1);
        let den = nrm.0 * d.0 + nrm.1 * d.1;
        if den == 0.0 {
            if num <= 0.0 {
                return None;
            }
        } else {
            let t = -num / den;
            if den > 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 < t1).then_some((t0, t1))
}

/// Euclidean distance from `p` to a convex polygon (0 inside).
fn poly_dist(p: (f64, f64), poly: &[(f64, f64)]) -> f64 {
    let n = poly.len();
    let mut inside = n >= 3;
    let mut best = f64::INFINITY;
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let e = (b.0 - a.0, b.1 - a.1);
        let w = (p.0 - a.0, p.1 - a.1);
        let cross = e.0 * w.1 - e.1 * w.0;
        let orient = {
            // The polygon may be clockwise (a mirroring CTM).
            let c = poly[(i + 2) % n];
            e.0 * (c.1 - a.1) - e.1 * (c.0 - a.0)
        };
        if cross * orient < 0.0 {
            inside = false;
        }
        let len2 = e.0 * e.0 + e.1 * e.1;
        let t = if len2 > 0.0 {
            ((w.0 * e.0 + w.1 * e.1) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min(dist(p, (a.0 + e.0 * t, a.1 + e.1 * t)));
    }
    if inside {
        0.0
    } else {
        best
    }
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
