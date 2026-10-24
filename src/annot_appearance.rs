//! Appearance synthesis for markup annotations without `/AP`
//! (ISO 32000-1 §12.5.6). A conforming reader "may" construct an
//! appearance from the annotation dictionary when the author supplied
//! none; this module does so for the geometric subtypes whose look the
//! specification actually describes — Square, Circle, Line (with line
//! endings and leader lines), Polygon, PolyLine, Ink, the four text
//! markups (Highlight, Underline, StrikeOut, Squiggly) and FreeText —
//! and produces a Form XObject the renderer paints through the ordinary
//! annotation path, so `/Rect` fitting, clipping and fonts are shared
//! with author-supplied streams. An existing `/AP` always wins; this
//! code is only consulted when there is none.
//!
//! Inputs honoured: `/Rect` (`/RD` for Square/Circle/FreeText), `/C`
//! (stroke; FreeText background), `/IC` (interior), `/BS` `/W` `/S`
//! `/D` or `/Border` (width, dash), `/CA` (constant alpha, applied to
//! the whole annotation as a transparency group so overlapping strokes
//! do not double), `/QuadPoints`, `/InkList`, `/Vertices`, `/L`, `/LE`,
//! `/LL`, `/LLE`, `/DA`, `/Q`, `/Contents`. Highlights blend with
//! `/Multiply` as every viewer does. A degenerate `/Rect` (zero area —
//! seen on Line annotations) is replaced by the geometry's bounds.
//!
//! Not synthesised, deliberately: `/Redact` marks (the pre-redaction
//! appearance has no normative look; nothing is drawn, as in Poppler),
//! and every other subtype without `/AP` — `/Text` and `/Stamp` icons,
//! `/FileAttachment`, `/Sound`, `/Caret`, `/Screen`, 3D … — which stays
//! [`Error::Unsupported`] rather than being skipped. FreeText contents
//! outside the Latin range of the fallback font are refused for the
//! same reason as form fields.

use crate::acroform::{
    color_op, finish, literal, num_array, parse_da, text_resources, wrap_lines, Appearance,
    FormDefaults,
};
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};

/// Most quadrilaterals of a text-markup annotation we draw.
const MAX_QUADS: usize = 4096;
/// Most points across `/InkList` / `/Vertices` we draw.
const MAX_POINTS: usize = 65_536;

/// A synthesised markup appearance: the Form XObject, the rectangle
/// its `/BBox` describes (normally `/Rect`, else the geometry bounds)
/// and the constant alpha to composite it with.
pub(crate) struct Markup {
    pub(crate) appearance: Appearance,
    pub(crate) rect: [f64; 4],
    pub(crate) alpha: Option<f64>,
    /// Highlights composite with `/Multiply` (§12.5.6.10 note).
    pub(crate) multiply: bool,
}

/// Build the appearance of `annot` (`/Subtype` `subtype`, no `/AP`)
/// inside `rect` (normalised `/Rect`). `Ok(None)` means the subtype is
/// handled and draws nothing; an unhandled subtype is `Unsupported`.
pub(crate) fn synthesize(
    doc: &Document,
    annot: &Dict,
    subtype: &[u8],
    rect: [f64; 4],
    limits: &Limits,
) -> Result<Option<Markup>, Error> {
    let stroke = color(doc, annot, b"C", false)?.map(|c| c + " ");
    let interior = color(doc, annot, b"IC", true)?.map(|c| c + " ");
    let (bw, dash) = border(doc, annot)?;
    let alpha = match annot.get(b"CA".as_slice()) {
        None | Some(Object::Null) => None,
        Some(o) => {
            let v = o
                .as_number()
                .ok_or_else(|| Error::Structure("annotation /CA is not a number".into()))?;
            if !v.is_finite() {
                return Err(Error::Structure("annotation /CA is not finite".into()));
            }
            let v = v.clamp(0.0, 1.0);
            (v < 1.0).then_some(v)
        }
    };
    let dash_op = match &dash {
        Some(d) if !d.is_empty() => format!(
            "[{}] 0 d ",
            d.iter()
                .map(|v| format!("{v:.4}"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        _ => String::new(),
    };
    let mut c = String::new();
    let mut bounds: Option<[f64; 4]> = None;
    let mut resources: Option<Object> = None;
    let mut blend_multiply = false;

    match subtype {
        b"Square" | b"Circle" => {
            let [x0, y0, x1, y1] = inset_rd(doc, annot, rect)?;
            let hw = bw / 2.0;
            let (ix0, iy0, ix1, iy1) = (x0 + hw, y0 + hw, x1 - hw, y1 - hw);
            if ix1 <= ix0 || iy1 <= iy0 {
                return Ok(None);
            }
            let path = if subtype == b"Square" {
                format!("{ix0:.4} {iy0:.4} {:.4} {:.4} re", ix1 - ix0, iy1 - iy0)
            } else {
                ellipse(ix0, iy0, ix1, iy1)
            };
            if let Some(op) = paint_op(&stroke, &interior, bw) {
                c.push_str(&format!(
                    "q {}{}{bw:.4} w {dash_op}{path} {op}\nQ\n",
                    stroke.clone().unwrap_or_default(),
                    interior.clone().unwrap_or_default()
                ));
            }
        }
        b"Line" => {
            let l = numbers(doc, annot, b"L", 4)?;
            let Some(l) = l else {
                return Err(Error::Structure("/Line annotation has no /L".into()));
            };
            let ll = number(doc, annot, b"LL")?.unwrap_or(0.0);
            let lle = number(doc, annot, b"LLE")?.unwrap_or(0.0).max(0.0);
            let (x1, y1, x2, y2) = (l[0], l[1], l[2], l[3]);
            let (dx, dy) = (x2 - x1, y2 - y1);
            let len = (dx * dx + dy * dy).sqrt();
            let (ux, uy) = if len > 1e-9 {
                (dx / len, dy / len)
            } else {
                (1.0, 0.0)
            };
            // Leader lines run perpendicular to the line (§12.5.6.7:
            // positive /LL is to the left of the direction of travel).
            let (px, py) = (-uy, ux);
            let (ox, oy) = (px * ll, py * ll);
            let (ax, ay, bx, by) = (x1 + ox, y1 + oy, x2 + ox, y2 + oy);
            let Some(sc) = &stroke else {
                return Ok(None);
            };
            let ic = interior.clone().unwrap_or_default();
            c.push_str(&format!("q {sc}{ic}{bw:.4} w {dash_op}"));
            let mut pts = vec![(ax, ay), (bx, by)];
            if ll != 0.0 {
                let ext = ll.signum() * lle;
                for (sx, sy) in [(x1, y1), (x2, y2)] {
                    c.push_str(&format!(
                        "{sx:.4} {sy:.4} m {:.4} {:.4} l S\n",
                        sx + ox + px * ext,
                        sy + oy + py * ext
                    ));
                    pts.push((sx, sy));
                    pts.push((sx + ox + px * ext, sy + oy + py * ext));
                }
            }
            c.push_str(&format!("{ax:.4} {ay:.4} m {bx:.4} {by:.4} l S\n"));
            let le = names(doc, annot, b"LE")?;
            let (le1, le2) = (
                le.first().cloned().unwrap_or_else(|| b"None".to_vec()),
                le.get(1).cloned().unwrap_or_else(|| b"None".to_vec()),
            );
            let size = ending_size(bw);
            c.push_str(&line_ending(
                &le1,
                ax,
                ay,
                -ux,
                -uy,
                size,
                interior.is_some(),
            ));
            c.push_str(&line_ending(&le2, bx, by, ux, uy, size, interior.is_some()));
            c.push_str("Q\n");
            bounds = Some(expand(points_bounds(&pts), bw.max(size)));
        }
        b"Polygon" | b"PolyLine" => {
            let v = numbers_any(doc, annot, b"Vertices")?;
            if v.len() < 4 {
                return Ok(None);
            }
            let pts: Vec<(f64, f64)> = v.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).collect();
            check_points(pts.len())?;
            let Some(op) = paint_op(&stroke, &interior, bw) else {
                return Ok(None);
            };
            c.push_str(&format!(
                "q {}{}{bw:.4} w {dash_op}",
                stroke.clone().unwrap_or_default(),
                interior.clone().unwrap_or_default()
            ));
            c.push_str(&polyline(&pts));
            if subtype == b"Polygon" {
                c.push_str(&format!(" h {op}\n"));
            } else {
                c.push_str(" S\n");
                let le = names(doc, annot, b"LE")?;
                let size = ending_size(bw);
                if let (Some(a), Some(b)) = (pts.first(), pts.get(1)) {
                    let (ux, uy) = unit(b.0 - a.0, b.1 - a.1);
                    let style = le.first().cloned().unwrap_or_else(|| b"None".to_vec());
                    c.push_str(&line_ending(
                        &style,
                        a.0,
                        a.1,
                        -ux,
                        -uy,
                        size,
                        interior.is_some(),
                    ));
                }
                if let (Some(a), Some(b)) = (pts.get(pts.len() - 2), pts.last()) {
                    let (ux, uy) = unit(b.0 - a.0, b.1 - a.1);
                    let style = le.get(1).cloned().unwrap_or_else(|| b"None".to_vec());
                    c.push_str(&line_ending(
                        &style,
                        b.0,
                        b.1,
                        ux,
                        uy,
                        size,
                        interior.is_some(),
                    ));
                }
            }
            c.push_str("Q\n");
            bounds = Some(expand(points_bounds(&pts), bw.max(ending_size(bw))));
        }
        b"Ink" => {
            let Some(sc) = &stroke else {
                return Ok(None);
            };
            let list = match annot.get(b"InkList".as_slice()) {
                Some(o) => doc.deref(o)?,
                None => return Err(Error::Structure("/Ink annotation has no /InkList".into())),
            };
            let Some(list) = list.as_array() else {
                return Err(Error::Structure("/InkList is not an array".into()));
            };
            let mut all: Vec<(f64, f64)> = Vec::new();
            c.push_str(&format!("q {sc}{bw:.4} w 1 J 1 j {dash_op}"));
            for stroke_obj in list {
                let v = num_array(doc, stroke_obj)?;
                if v.iter().any(|x| !x.is_finite()) {
                    return Err(Error::Structure("/InkList has non-finite values".into()));
                }
                let pts: Vec<(f64, f64)> =
                    v.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])).collect();
                check_points(all.len() + pts.len())?;
                match pts.len() {
                    0 => {}
                    // A single point: a dot of the line width.
                    1 => c.push_str(&format!(
                        "{:.4} {:.4} m {:.4} {:.4} l S\n",
                        pts[0].0, pts[0].1, pts[0].0, pts[0].1
                    )),
                    _ => {
                        c.push_str(&polyline(&pts));
                        c.push_str(" S\n");
                    }
                }
                all.extend(pts);
            }
            c.push_str("Q\n");
            if all.is_empty() {
                return Ok(None);
            }
            bounds = Some(expand(points_bounds(&all), bw));
        }
        b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly" => {
            let Some(sc) = &stroke else {
                return Ok(None);
            };
            let quads = quadpoints(doc, annot)?;
            if quads.is_empty() {
                return Ok(None);
            }
            // The markup colour is a fill for highlights and a stroke
            // for the three line styles; /C is one colour either way.
            let fill_c = sc
                .replace(" RG", " rg")
                .replace(" G", " g")
                .replace(" K", " k");
            let mut all = Vec::new();
            match subtype {
                b"Highlight" => {
                    blend_multiply = true;
                    c.push_str(&format!("q {fill_c}"));
                    for q in &quads {
                        // Order (§12.5.6.10): upper-left, upper-right,
                        // lower-left, lower-right.
                        c.push_str(&format!(
                            "{:.4} {:.4} m {:.4} {:.4} l {:.4} {:.4} l {:.4} {:.4} l h f\n",
                            q[0], q[1], q[2], q[3], q[6], q[7], q[4], q[5]
                        ));
                    }
                    c.push_str("Q\n");
                }
                b"Underline" | b"StrikeOut" => {
                    c.push_str(&format!("q {sc}"));
                    for q in &quads {
                        let h = quad_height(q);
                        let lw = (h / 14.0).max(0.5);
                        // Along the bottom edge (lower-left → lower-right),
                        // raised by half the line width; strike-out through
                        // the middle.
                        let t = if subtype == b"Underline" {
                            lw / 2.0 / h.max(1e-9)
                        } else {
                            0.5
                        };
                        let (x0, y0) = lerp((q[4], q[5]), (q[0], q[1]), t);
                        let (x1, y1) = lerp((q[6], q[7]), (q[2], q[3]), t);
                        c.push_str(&format!(
                            "{lw:.4} w {x0:.4} {y0:.4} m {x1:.4} {y1:.4} l S\n"
                        ));
                    }
                    c.push_str("Q\n");
                }
                _ => {
                    c.push_str(&format!("q {sc}1 w 1 J 1 j "));
                    for q in &quads {
                        let h = quad_height(q);
                        let amp = (h / 4.0).max(1.0);
                        let (bx0, by0, bx1, by1) = (q[4], q[5], q[6], q[7]);
                        let (ux, uy) = unit(bx1 - bx0, by1 - by0);
                        let (px, py) = (-uy, ux); // "up" along the quad
                        let w = ((bx1 - bx0).powi(2) + (by1 - by0).powi(2)).sqrt();
                        let n = ((w / amp).ceil() as usize).clamp(1, 4096);
                        let step = w / n as f64;
                        let mut path = format!("{:.4} {:.4} m", bx0 + px * 0.5, by0 + py * 0.5);
                        for i in 1..=n {
                            let up = if i % 2 == 1 { amp } else { 0.5 };
                            let (x, y) = (
                                bx0 + ux * step * i as f64 + px * up,
                                by0 + uy * step * i as f64 + py * up,
                            );
                            path.push_str(&format!(" {x:.4} {y:.4} l"));
                        }
                        c.push_str(&path);
                        c.push_str(" S\n");
                    }
                    c.push_str("Q\n");
                }
            }
            for q in &quads {
                all.extend(q.as_chunks::<2>().0.iter().map(|p| (p[0], p[1])));
            }
            bounds = Some(expand(points_bounds(&all), 1.0));
        }
        b"FreeText" => {
            let [x0, y0, x1, y1] = inset_rd(doc, annot, rect)?;
            let (w, h) = (x1 - x0, y1 - y0);
            if w <= 0.0 || h <= 0.0 {
                return Ok(None);
            }
            let da_bytes = match annot
                .get(b"DA".as_slice())
                .map(|o| doc.deref(o))
                .transpose()?
            {
                Some(Object::String(s)) => Some(s),
                _ => None,
            };
            let da = parse_da(da_bytes.as_deref());
            // Background from /C (Table 164 — the annotation colour),
            // border in the text colour (as Poppler draws it).
            let fill_bg = stroke.as_ref().map(|s| {
                s.replace(" RG", " rg")
                    .replace(" G", " g")
                    .replace(" K", " k")
            });
            let border_w = bw;
            let hw = border_w / 2.0;
            if fill_bg.is_some() || border_w > 0.0 {
                let op = match (&fill_bg, border_w > 0.0) {
                    (Some(_), true) => "B",
                    (Some(_), false) => "f",
                    (None, _) => "S",
                };
                c.push_str(&format!(
                    "q {}{} {border_w:.4} w {dash_op}{:.4} {:.4} {:.4} {:.4} re {op} Q\n",
                    fill_bg.clone().unwrap_or_default(),
                    da.color
                        .replace(" rg", " RG")
                        .replace(" g", " G")
                        .replace(" k", " K"),
                    x0 + hw,
                    y0 + hw,
                    (w - border_w).max(0.0),
                    (h - border_w).max(0.0)
                ));
            }
            let text = match annot
                .get(b"Contents".as_slice())
                .map(|o| doc.deref(o))
                .transpose()?
            {
                Some(Object::String(s)) => crate::render::pdf_text_string(&s),
                _ => String::new(),
            };
            if !text.trim().is_empty() {
                let defaults = FormDefaults::from_catalog(doc)?;
                let res = text_resources(doc, &defaults, &da.font)?;
                let font = crate::font::load_font(doc, &res, &da.font, limits)?;
                let size = if da.size > 0.0 { da.size } else { 12.0 };
                let width = |s: &[u8]| -> f64 {
                    s.iter().map(|&b| font.width(b as u32)).sum::<f64>() / 1000.0 * size
                };
                let pad = border_w + 2.0;
                let inner_w = (w - 2.0 * pad).max(1.0);
                let q = number(doc, annot, b"Q")?.unwrap_or(0.0) as i64;
                let lines = wrap_lines(&text, inner_w, width)?;
                let lead = size * 1.15;
                let fname = String::from_utf8_lossy(&da.font).into_owned();
                c.push_str(&format!(
                    "q {:.4} {:.4} {inner_w:.4} {:.4} re W n BT {} /{fname} {size:.4} Tf {lead:.4} TL ",
                    x0 + pad,
                    y0 + pad,
                    (h - 2.0 * pad).max(1.0),
                    da.color
                ));
                let top = y1 - pad - size * 0.78;
                c.push_str(&format!("1 0 0 1 {:.4} {top:.4} Tm ", x0 + pad));
                let mut first = true;
                for line in lines {
                    let tw = width(&line);
                    let x = match q {
                        1 => (inner_w - tw) / 2.0,
                        2 => inner_w - tw,
                        _ => 0.0,
                    }
                    .max(0.0);
                    if !first {
                        c.push_str("T* ");
                    }
                    first = false;
                    c.push_str(&format!(
                        "{x:.4} 0 Td {} Tj {:.4} 0 Td ",
                        literal(&line),
                        -x
                    ));
                }
                c.push_str("ET Q\n");
                resources = Some(res);
            }
        }
        b"Redact" => {
            // A redaction *mark* has no normative pre-redaction look;
            // Poppler and Acrobat draw nothing for it without /AP.
            return Ok(None);
        }
        other => {
            return Err(Error::Unsupported {
                feature: "annotation without appearance stream",
                detail: format!(
                    "/Subtype /{} has no /AP — synthesising an appearance is implemented for \
                     form widgets and the geometric markup subtypes only",
                    String::from_utf8_lossy(other)
                ),
            })
        }
    }
    if c.is_empty() {
        return Ok(None);
    }

    // The appearance lives in page space: /BBox is the rectangle, the
    // /Matrix identity, so Algorithm 8.1 maps it 1:1. A degenerate
    // /Rect (zero area) cannot hold anything — the geometry's bounds
    // stand in for it.
    let degenerate = rect[2] - rect[0] <= 1e-9 || rect[3] - rect[1] <= 1e-9;
    let eff = match (degenerate, bounds) {
        (true, Some(b)) if b[2] > b[0] && b[3] > b[1] => b,
        (true, _) => return Ok(None),
        (false, _) => rect,
    };
    let mut appearance = finish(c, 0.0, 0.0, "[1 0 0 1 0 0]", resources);
    appearance.dict.insert(
        b"BBox".to_vec(),
        Object::Array(eff.iter().map(|v| Object::Real(*v)).collect()),
    );
    if alpha.is_some() || blend_multiply {
        let mut g = Dict::new();
        g.insert(b"S".to_vec(), Object::Name(b"Transparency".to_vec()));
        g.insert(b"I".to_vec(), Object::Bool(true));
        appearance.dict.insert(b"Group".to_vec(), Object::Dict(g));
    }
    Ok(Some(Markup {
        appearance,
        rect: eff,
        alpha,
        multiply: blend_multiply,
    }))
}

// ---- dictionary helpers --------------------------------------------

/// `/C` or `/IC` as a colour operator (`RG`/`rg` family). An empty
/// array is "transparent" → `None`; absent → `None` as well.
fn color(doc: &Document, annot: &Dict, key: &[u8], fill: bool) -> Result<Option<String>, Error> {
    let Some(o) = annot.get(key) else {
        return Ok(None);
    };
    let o = doc.deref(o)?;
    if matches!(o, Object::Null) {
        return Ok(None);
    }
    let Some(arr) = o.as_array() else {
        return Err(Error::Structure(format!(
            "annotation /{} is not an array",
            String::from_utf8_lossy(key)
        )));
    };
    let comps: Vec<f64> = arr
        .iter()
        .map(|x| {
            x.as_number().filter(|v| v.is_finite()).ok_or_else(|| {
                Error::Structure("annotation colour component is not a number".into())
            })
        })
        .collect::<Result<_, _>>()?;
    match comps.len() {
        0 | 1 | 3 | 4 => Ok(color_op(&comps, fill)),
        n => Err(Error::Structure(format!(
            "annotation /{} has {n} components (0, 1, 3 or 4 allowed)",
            String::from_utf8_lossy(key)
        ))),
    }
}

/// Border width and dash from `/BS` (preferred) or `/Border`.
fn border(doc: &Document, annot: &Dict) -> Result<(f64, Option<Vec<f64>>), Error> {
    if let Some(bs) = annot.get(b"BS".as_slice()) {
        let bs = doc.deref(bs)?;
        let Some(bs) = bs.as_dict() else {
            return Err(Error::Structure(
                "annotation /BS is not a dictionary".into(),
            ));
        };
        let w = match bs.get(b"W".as_slice()) {
            None | Some(Object::Null) => 1.0,
            Some(o) => o
                .as_number()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| Error::Structure("/BS /W is not a non-negative number".into()))?,
        };
        let dash = if bs.get(b"S".as_slice()).and_then(|o| o.as_name()) == Some(b"D") {
            Some(match bs.get(b"D".as_slice()) {
                Some(o) => dash_array(doc, o)?,
                None => vec![3.0],
            })
        } else {
            None
        };
        return Ok((w, dash));
    }
    if let Some(b) = annot.get(b"Border".as_slice()) {
        let b = doc.deref(b)?;
        if let Some(arr) = b.as_array() {
            let w = match arr.get(2) {
                Some(o) => o
                    .as_number()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| {
                        Error::Structure("/Border width is not a non-negative number".into())
                    })?,
                None => 1.0,
            };
            let dash = match arr.get(3) {
                Some(o) => Some(dash_array(doc, o)?),
                None => None,
            };
            return Ok((w, dash));
        }
        return Err(Error::Structure(
            "annotation /Border is not an array".into(),
        ));
    }
    Ok((1.0, None))
}

fn dash_array(doc: &Document, o: &Object) -> Result<Vec<f64>, Error> {
    let v = num_array(doc, o)?;
    if v.iter().any(|x| !x.is_finite() || *x < 0.0) || v.iter().all(|x| *x == 0.0) && !v.is_empty()
    {
        return Err(Error::Structure("annotation dash array is invalid".into()));
    }
    Ok(v.into_iter().take(16).collect())
}

fn number(doc: &Document, annot: &Dict, key: &[u8]) -> Result<Option<f64>, Error> {
    match annot.get(key) {
        None | Some(Object::Null) => Ok(None),
        Some(o) => doc
            .deref(o)?
            .as_number()
            .filter(|v| v.is_finite())
            .map(Some)
            .ok_or_else(|| {
                Error::Structure(format!(
                    "annotation /{} is not a finite number",
                    String::from_utf8_lossy(key)
                ))
            }),
    }
}

/// `key` as exactly `n` finite numbers.
fn numbers(doc: &Document, annot: &Dict, key: &[u8], n: usize) -> Result<Option<Vec<f64>>, Error> {
    let v = numbers_any(doc, annot, key)?;
    if v.is_empty() && !annot.contains_key(key) {
        return Ok(None);
    }
    if v.len() != n {
        return Err(Error::Structure(format!(
            "annotation /{} must hold {n} numbers (has {})",
            String::from_utf8_lossy(key),
            v.len()
        )));
    }
    Ok(Some(v))
}

fn numbers_any(doc: &Document, annot: &Dict, key: &[u8]) -> Result<Vec<f64>, Error> {
    let Some(o) = annot.get(key) else {
        return Ok(Vec::new());
    };
    let o = doc.deref(o)?;
    let Some(arr) = o.as_array() else {
        return Err(Error::Structure(format!(
            "annotation /{} is not an array",
            String::from_utf8_lossy(key)
        )));
    };
    arr.iter()
        .map(|x| {
            x.as_number().filter(|v| v.is_finite()).ok_or_else(|| {
                Error::Structure(format!(
                    "annotation /{} has a non-numeric entry",
                    String::from_utf8_lossy(key)
                ))
            })
        })
        .collect()
}

fn names(doc: &Document, annot: &Dict, key: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let Some(o) = annot.get(key) else {
        return Ok(Vec::new());
    };
    let o = doc.deref(o)?;
    Ok(o.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_name().map(|n| n.to_vec()))
                .collect()
        })
        .unwrap_or_default())
}

/// `/Rect` shrunk by `/RD` (§12.5.6.5), when present and sane.
fn inset_rd(doc: &Document, annot: &Dict, rect: [f64; 4]) -> Result<[f64; 4], Error> {
    let Some(rd) = numbers(doc, annot, b"RD", 4)? else {
        return Ok(rect);
    };
    if rd.iter().any(|v| *v < 0.0) {
        return Err(Error::Structure(
            "annotation /RD has negative entries".into(),
        ));
    }
    let out = [
        rect[0] + rd[0],
        rect[1] + rd[1],
        rect[2] - rd[2],
        rect[3] - rd[3],
    ];
    if out[2] <= out[0] || out[3] <= out[1] {
        return Err(Error::Structure(
            "annotation /RD leaves no room inside /Rect".into(),
        ));
    }
    Ok(out)
}

/// `/QuadPoints` as groups of eight finite numbers.
fn quadpoints(doc: &Document, annot: &Dict) -> Result<Vec<[f64; 8]>, Error> {
    let v = numbers_any(doc, annot, b"QuadPoints")?;
    if !v.len().is_multiple_of(8) {
        return Err(Error::Structure(format!(
            "/QuadPoints has {} numbers (must be a multiple of 8)",
            v.len()
        )));
    }
    if v.len() / 8 > MAX_QUADS {
        return Err(Error::LimitExceeded {
            limit: "annotation /QuadPoints",
            max: MAX_QUADS as u64,
        });
    }
    Ok(v.as_chunks::<8>().0.to_vec())
}

fn check_points(n: usize) -> Result<(), Error> {
    if n > MAX_POINTS {
        return Err(Error::LimitExceeded {
            limit: "annotation geometry points",
            max: MAX_POINTS as u64,
        });
    }
    Ok(())
}

// ---- geometry ------------------------------------------------------

/// Fill/stroke operator for a closed shape, or `None` when neither
/// colour paints.
fn paint_op(stroke: &Option<String>, interior: &Option<String>, bw: f64) -> Option<&'static str> {
    match (stroke.is_some() && bw > 0.0, interior.is_some()) {
        (true, true) => Some("B"),
        (true, false) => Some("S"),
        (false, true) => Some("f"),
        (false, false) => None,
    }
}

fn polyline(pts: &[(f64, f64)]) -> String {
    let mut s = format!("{:.4} {:.4} m", pts[0].0, pts[0].1);
    for p in &pts[1..] {
        s.push_str(&format!(" {:.4} {:.4} l", p.0, p.1));
    }
    s
}

/// Four Bézier arcs approximating the ellipse inscribed in the box.
fn ellipse(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    const K: f64 = 0.5522847498;
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let (kx, ky) = (rx * K, ry * K);
    format!(
        "{:.4} {cy:.4} m {:.4} {:.4} {:.4} {:.4} {cx:.4} {y1:.4} c \
         {:.4} {y1:.4} {x1:.4} {:.4} {x1:.4} {cy:.4} c \
         {x1:.4} {:.4} {:.4} {y0:.4} {cx:.4} {y0:.4} c \
         {:.4} {y0:.4} {x0:.4} {:.4} {x0:.4} {cy:.4} c h",
        x0,
        x0,
        cy + ky,
        cx - kx,
        y1,
        cx + kx,
        cy + ky,
        cy - ky,
        cx + kx,
        cx - kx,
        cy - ky
    )
}

fn unit(dx: f64, dy: f64) -> (f64, f64) {
    let len = (dx * dx + dy * dy).sqrt();
    if len > 1e-9 {
        (dx / len, dy / len)
    } else {
        (1.0, 0.0)
    }
}

fn lerp(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Height of a quadrilateral: distance from its lower-left to its
/// upper-left corner.
fn quad_height(q: &[f64; 8]) -> f64 {
    ((q[0] - q[4]).powi(2) + (q[1] - q[5]).powi(2)).sqrt()
}

fn points_bounds(pts: &[(f64, f64)]) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in pts {
        b[0] = b[0].min(*x);
        b[1] = b[1].min(*y);
        b[2] = b[2].max(*x);
        b[3] = b[3].max(*y);
    }
    b
}

fn expand(b: [f64; 4], by: f64) -> [f64; 4] {
    [b[0] - by, b[1] - by, b[2] + by, b[3] + by]
}

/// Size of a line ending for border width `bw` (Acrobat draws them
/// roughly three widths long, never below 4 pt).
fn ending_size(bw: f64) -> f64 {
    (bw * 3.0).max(4.0)
}

/// Line ending `style` (Table 176) at `(x, y)` with the line arriving
/// along the unit vector `(ux, uy)` (pointing *out* of the line).
/// Closed shapes are filled when the annotation has an `/IC`.
fn line_ending(style: &[u8], x: f64, y: f64, ux: f64, uy: f64, size: f64, fill: bool) -> String {
    let (px, py) = (-uy, ux);
    let closed = |pts: &[(f64, f64)]| -> String {
        format!("{} h {}\n", polyline(pts), if fill { "B" } else { "S" })
    };
    let half = size / 2.0;
    match style {
        b"Square" => closed(&[
            (x - ux * half - px * half, y - uy * half - py * half),
            (x + ux * half - px * half, y + uy * half - py * half),
            (x + ux * half + px * half, y + uy * half + py * half),
            (x - ux * half + px * half, y - uy * half + py * half),
        ]),
        b"Circle" => {
            let e = ellipse(x - half, y - half, x + half, y + half);
            format!("{e} {}\n", if fill { "B" } else { "S" })
        }
        b"Diamond" => closed(&[
            (x - ux * half, y - uy * half),
            (x + px * half, y + py * half),
            (x + ux * half, y + uy * half),
            (x - px * half, y - py * half),
        ]),
        b"OpenArrow" | b"ClosedArrow" | b"ROpenArrow" | b"RClosedArrow" => {
            // 30° half-angle arrow heads; the R variants point backwards.
            let (dx, dy) = if style.starts_with(b"R") {
                (-ux, -uy)
            } else {
                (ux, uy)
            };
            let (bx, by) = (x - dx * size, y - dy * size);
            let spread = size * 0.5774; // tan 30°
            let a = (bx + px * spread, by + py * spread);
            let b = (bx - px * spread, by - py * spread);
            if style.ends_with(b"ClosedArrow") {
                closed(&[a, (x, y), b])
            } else {
                format!(
                    "{:.4} {:.4} m {x:.4} {y:.4} l {:.4} {:.4} l S\n",
                    a.0, a.1, b.0, b.1
                )
            }
        }
        b"Butt" => format!(
            "{:.4} {:.4} m {:.4} {:.4} l S\n",
            x + px * half,
            y + py * half,
            x - px * half,
            y - py * half
        ),
        b"Slash" => {
            // 30° from the line direction.
            let (sx, sy) = (ux * 0.5 + px * 0.866, uy * 0.5 + py * 0.866);
            format!(
                "{:.4} {:.4} m {:.4} {:.4} l S\n",
                x + sx * half,
                y + sy * half,
                x - sx * half,
                y - sy * half
            )
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_helpers() {
        assert_eq!(ending_size(1.0), 4.0);
        assert_eq!(ending_size(4.0), 12.0);
        let b = points_bounds(&[(1.0, 2.0), (-3.0, 5.0)]);
        assert_eq!(b, [-3.0, 2.0, 1.0, 5.0]);
        assert!(ellipse(0.0, 0.0, 10.0, 10.0).ends_with(" c h"));
        assert!(line_ending(b"OpenArrow", 0.0, 0.0, 1.0, 0.0, 6.0, false).ends_with("l S\n"));
        assert!(line_ending(b"ClosedArrow", 0.0, 0.0, 1.0, 0.0, 6.0, true).ends_with("h B\n"));
        assert_eq!(line_ending(b"None", 0.0, 0.0, 1.0, 0.0, 6.0, true), "");
        assert_eq!(paint_op(&None, &None, 1.0), None);
        assert_eq!(paint_op(&Some("0 G".into()), &None, 0.0), None);
    }
}
