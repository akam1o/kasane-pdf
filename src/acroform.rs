//! AcroForm widget appearance synthesis (ISO 32000-1 §12.7): when a
//! Widget annotation has no `/AP` — or the document sets
//! `/NeedAppearances true` for its variable-text fields — the viewer
//! must construct the normal appearance from the field's value and
//! the widget's `/MK` characteristics. This module builds that
//! appearance as a Form XObject (dictionary + content bytes) which
//! the renderer paints through the ordinary annotation path, so
//! placement, clipping, fonts and colours all go through the same
//! code as an author-supplied stream.
//!
//! Covered: text fields (`/Tx` — single-line, multi-line, comb,
//! password, quadding), check boxes and radio buttons (`/Btn`, drawn
//! as paths from the `/MK /CA` style rather than ZapfDingbats, which
//! is not bundled), push buttons (caption), choice fields (`/Ch` —
//! combo shows the value, list shows the options with the selection
//! highlighted), `/MK` background, border colour and rotation,
//! `/BS /W` border width, `/DA` font / size / colour (size 0 = auto),
//! field attribute inheritance through `/Parent`, `/AcroForm /DA`
//! `/Q` `/DR` defaults.
//!
//! Not covered, deliberately: XFA (`/AcroForm /XFA` — a different
//! form model; when present the AcroForm widgets are the printable
//! fallback and are rendered as such), rich-text values (`/RV`),
//! JavaScript-computed values, signature appearances (`/Sig` fields
//! without `/AP` draw nothing — there is no value to show), and
//! bevelled/inset 3-D borders (drawn solid). A value that cannot be
//! encoded in the `/DA` font's single-byte Latin range is refused
//! rather than drawn with wrong glyphs.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};

/// Deepest `/Parent` chain followed for inherited attributes.
const MAX_PARENT_DEPTH: usize = 32;
/// Characters of a value we lay out (the rest is truncated).
const MAX_VALUE_CHARS: usize = 4096;
/// Options of a list box we draw.
const MAX_OPTIONS: usize = 1024;

/// Document-level form defaults (`/AcroForm`).
#[derive(Debug, Default, Clone)]
pub(crate) struct FormDefaults {
    pub(crate) need_appearances: bool,
    /// `/DR` default resources (a dictionary), if any.
    pub(crate) dr: Option<Object>,
    pub(crate) da: Option<Vec<u8>>,
    pub(crate) q: i64,
}

impl FormDefaults {
    pub(crate) fn from_catalog(doc: &Document) -> Result<FormDefaults, Error> {
        let Some(af) = doc.catalog().get(b"AcroForm".as_slice()) else {
            return Ok(FormDefaults::default());
        };
        let af = doc.deref(af)?;
        let Some(d) = af.as_dict() else {
            return Err(Error::Structure("/AcroForm is not a dictionary".into()));
        };
        Ok(FormDefaults {
            need_appearances: d
                .get(b"NeedAppearances".as_slice())
                .and_then(|o| o.as_bool())
                .unwrap_or(false),
            dr: d.get(b"DR".as_slice()).map(|o| doc.deref(o)).transpose()?,
            da: match d.get(b"DA".as_slice()).map(|o| doc.deref(o)).transpose()? {
                Some(Object::String(s)) => Some(s),
                _ => None,
            },
            q: d.get(b"Q".as_slice()).and_then(|o| o.as_i64()).unwrap_or(0),
        })
    }
}

/// Field kind after `/FT` + `/Ff` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    CheckBox,
    Radio,
    PushButton,
    Combo,
    List,
    Signature,
}

/// Look `key` up on the widget, then up its `/Parent` chain (§12.7.3.1
/// inheritable attributes), bounded.
fn inherited(doc: &Document, widget: &Dict, key: &[u8]) -> Result<Option<Object>, Error> {
    let mut cur = widget.clone();
    for _ in 0..MAX_PARENT_DEPTH {
        if let Some(v) = cur.get(key) {
            return Ok(Some(doc.deref(v)?));
        }
        match cur.get(b"Parent".as_slice()) {
            None => return Ok(None),
            Some(p) => {
                let p = doc.deref(p)?;
                match p.as_dict() {
                    Some(d) => cur = d.clone(),
                    None => return Ok(None),
                }
            }
        }
    }
    Err(Error::LimitExceeded {
        limit: "form field /Parent depth",
        max: MAX_PARENT_DEPTH as u64,
    })
}

fn num_array(doc: &Document, o: &Object) -> Result<Vec<f64>, Error> {
    let o = doc.deref(o)?;
    Ok(o.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_number()).collect())
        .unwrap_or_default())
}

/// `/MK /BG` or `/BC` → a colour operator (`g`/`rg`/`k`) with the
/// given operator family, or `None` when transparent (empty array).
fn color_op(comps: &[f64], fill: bool) -> Option<String> {
    let f = |v: &f64| format!("{:.4}", v.clamp(0.0, 1.0));
    match comps.len() {
        1 => Some(format!("{} {}", f(&comps[0]), if fill { "g" } else { "G" })),
        3 => Some(format!(
            "{} {} {} {}",
            f(&comps[0]),
            f(&comps[1]),
            f(&comps[2]),
            if fill { "rg" } else { "RG" }
        )),
        4 => Some(format!(
            "{} {} {} {} {}",
            f(&comps[0]),
            f(&comps[1]),
            f(&comps[2]),
            f(&comps[3]),
            if fill { "k" } else { "K" }
        )),
        _ => None,
    }
}

/// Parsed `/DA`: font resource name, size (0 = auto) and the colour
/// operator string (only `g`/`rg`/`k` with numeric operands survive).
struct Da {
    font: Vec<u8>,
    size: f64,
    color: String,
}

fn parse_da(da: Option<&[u8]>) -> Da {
    let mut out = Da {
        font: b"Helv".to_vec(),
        size: 0.0,
        color: "0 g".into(),
    };
    let Some(da) = da else { return out };
    let text = String::from_utf8_lossy(da);
    let toks: Vec<&str> = text.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        match *t {
            "Tf" if i >= 2 => {
                if let Some(name) = toks[i - 2].strip_prefix('/') {
                    if !name.is_empty() {
                        out.font = name.as_bytes().to_vec();
                    }
                }
                out.size = toks[i - 1].parse::<f64>().unwrap_or(0.0).max(0.0);
                if !out.size.is_finite() {
                    out.size = 0.0;
                }
            }
            "g" | "rg" | "k" => {
                let n = match *t {
                    "g" => 1,
                    "rg" => 3,
                    _ => 4,
                };
                if i >= n {
                    let nums: Option<Vec<f64>> = toks[i - n..i]
                        .iter()
                        .map(|x| x.parse::<f64>().ok())
                        .collect();
                    if let Some(v) = nums {
                        if let Some(c) = color_op(&v, true) {
                            out.color = c;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Encode a value for `Tj` under a WinAnsi-style single-byte font.
/// Characters outside the Latin range the bundled substitutes can
/// draw are refused — drawing `?` would silently misreport the form.
fn encode_latin(s: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars().take(MAX_VALUE_CHARS) {
        let c = ch as u32;
        match c {
            0x20..=0x7E | 0xA0..=0xFF => out.push(c as u8),
            0x2019 => out.push(0x92),
            0x2018 => out.push(0x91),
            0x201C => out.push(0x93),
            0x201D => out.push(0x94),
            0x2013 => out.push(0x96),
            0x2014 => out.push(0x97),
            0x2022 => out.push(0x95),
            0x20AC => out.push(0x80),
            0x09 => out.push(b' '),
            _ => {
                return Err(Error::Unsupported {
                    feature: "form field value outside the Latin range",
                    detail: format!(
                        "U+{c:04X} cannot be drawn with the field's single-byte font; \
                         a CJK/Unicode field font is not synthesised"
                    ),
                })
            }
        }
    }
    Ok(out)
}

fn literal(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() + 2);
    s.push('(');
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(b as char);
            }
            0x20..=0x7E => s.push(b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s.push(')');
    s
}

/// The synthesised appearance: a Form XObject dictionary (with
/// `/BBox`, `/Matrix`, `/Resources`) and its content bytes.
pub(crate) struct Appearance {
    pub(crate) dict: Dict,
    pub(crate) content: Vec<u8>,
}

/// Decide whether `widget` needs a synthesised appearance given the
/// form defaults and whether it already has `/AP`.
pub(crate) fn needs_synthesis(
    doc: &Document,
    widget: &Dict,
    defaults: &FormDefaults,
    has_ap: bool,
) -> Result<bool, Error> {
    if !has_ap {
        return Ok(true);
    }
    if !defaults.need_appearances {
        return Ok(false);
    }
    // NeedAppearances regenerates variable text; buttons keep theirs.
    Ok(matches!(
        kind_of(doc, widget)?,
        Some(Kind::Text) | Some(Kind::Combo) | Some(Kind::List)
    ))
}

fn kind_of(doc: &Document, widget: &Dict) -> Result<Option<Kind>, Error> {
    let ft = inherited(doc, widget, b"FT")?;
    let ff = inherited(doc, widget, b"Ff")?
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    Ok(match ft.as_ref().and_then(|o| o.as_name()) {
        Some(b"Tx") => Some(Kind::Text),
        Some(b"Btn") => Some(if ff & (1 << 16) != 0 {
            Kind::PushButton
        } else if ff & (1 << 15) != 0 {
            Kind::Radio
        } else {
            Kind::CheckBox
        }),
        Some(b"Ch") => Some(if ff & (1 << 17) != 0 {
            Kind::Combo
        } else {
            Kind::List
        }),
        Some(b"Sig") => Some(Kind::Signature),
        _ => None,
    })
}

/// Build the appearance for `widget` whose `/Rect` is `rect`
/// (normalised `[x0 y0 x1 y1]`). Returns `None` when the field kind
/// draws nothing (signature, unknown `/FT` without `/MK`).
pub(crate) fn synthesize(
    doc: &Document,
    widget: &Dict,
    rect: [f64; 4],
    defaults: &FormDefaults,
    limits: &Limits,
) -> Result<Option<Appearance>, Error> {
    let kind = kind_of(doc, widget)?;
    let ff = inherited(doc, widget, b"Ff")?
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let mk: Option<Dict> = match widget.get(b"MK".as_slice()) {
        Some(o) => doc.deref(o)?.as_dict().cloned(),
        None => None,
    };
    let rotation = mk
        .as_ref()
        .and_then(|m| m.get(b"R".as_slice()))
        .and_then(|o| o.as_i64())
        .unwrap_or(0)
        .rem_euclid(360);
    let (rw, rh) = (rect[2] - rect[0], rect[3] - rect[1]);
    // Appearance space: width/height swap for 90/270 (§12.5.5 note on /MK /R).
    let (w, h) = if rotation == 90 || rotation == 270 {
        (rh, rw)
    } else {
        (rw, rh)
    };
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return Ok(None);
    }
    let matrix = match rotation {
        90 => format!("[0 1 -1 0 {rw:.4} 0]"),
        180 => format!("[-1 0 0 -1 {rw:.4} {rh:.4}]"),
        270 => format!("[0 -1 1 0 0 {rh:.4}]"),
        _ => "[1 0 0 1 0 0]".into(),
    };

    // ---- border & background (§12.5.4 /MK, §12.5.4 /BS) --------------
    let bg = mk
        .as_ref()
        .and_then(|m| m.get(b"BG".as_slice()))
        .map(|o| num_array(doc, o))
        .transpose()?
        .and_then(|c| color_op(&c, true));
    let bc = mk
        .as_ref()
        .and_then(|m| m.get(b"BC".as_slice()))
        .map(|o| num_array(doc, o))
        .transpose()?
        .and_then(|c| color_op(&c, false));
    let bw = match widget.get(b"BS".as_slice()) {
        Some(o) => doc
            .deref(o)?
            .as_dict()
            .and_then(|b| b.get(b"W".as_slice()))
            .and_then(|o| o.as_number())
            .unwrap_or(1.0),
        None => 1.0,
    };
    let bw = if bc.is_some() && bw.is_finite() && bw >= 0.0 {
        bw.min(w / 2.0).min(h / 2.0)
    } else {
        0.0
    };
    let mut c = String::new();
    if let Some(bg) = &bg {
        c.push_str(&format!("q {bg} 0 0 {w:.4} {h:.4} re f Q\n"));
    }
    if let (Some(bc), true) = (&bc, bw > 0.0) {
        c.push_str(&format!(
            "q {bc} {bw:.4} w {:.4} {:.4} {:.4} {:.4} re S Q\n",
            bw / 2.0,
            bw / 2.0,
            w - bw,
            h - bw
        ));
    }

    let Some(kind) = kind else {
        return Ok(if c.is_empty() {
            None
        } else {
            Some(finish(c, w, h, &matrix, None))
        });
    };

    // ---- defaults for variable text ----------------------------------
    let da_bytes = match inherited(doc, widget, b"DA")? {
        Some(Object::String(s)) => Some(s),
        _ => defaults.da.clone(),
    };
    let da = parse_da(da_bytes.as_deref());
    let q = inherited(doc, widget, b"Q")?
        .and_then(|o| o.as_i64())
        .unwrap_or(defaults.q);
    let resources = text_resources(doc, defaults, &da.font)?;
    let font = crate::font::load_font(doc, &resources, &da.font, limits)?;
    let text_width = |bytes: &[u8], size: f64| -> f64 {
        bytes.iter().map(|&b| font.width(b as u32)).sum::<f64>() / 1000.0 * size
    };
    let pad = bw + 2.0;

    match kind {
        Kind::Signature => {}
        Kind::CheckBox | Kind::Radio => {
            // On when /AS names a non-Off state; without /AS, when /V
            // (the field value) names the widget's on-state.
            let on = match widget.get(b"AS".as_slice()).and_then(|o| o.as_name()) {
                Some(s) => s != b"Off",
                None => match inherited(doc, widget, b"V")? {
                    Some(Object::Name(v)) => {
                        v != b"Off" && on_state_name(doc, widget)?.map(|s| s == v).unwrap_or(true)
                    }
                    _ => false,
                },
            };
            if on {
                let style = mk
                    .as_ref()
                    .and_then(|m| m.get(b"CA".as_slice()))
                    .and_then(|o| o.as_string().map(|s| s.to_vec()))
                    .and_then(|s| s.first().copied())
                    .unwrap_or(if kind == Kind::Radio { b'l' } else { b'4' });
                c.push_str(&format!(
                    "q {} {}\nQ\n",
                    da.color,
                    mark_path(style, w, h, pad)
                ));
            }
        }
        Kind::PushButton => {
            let caption = mk
                .as_ref()
                .and_then(|m| m.get(b"CA".as_slice()))
                .and_then(|o| o.as_string().map(|s| crate::render::pdf_text_string(s)))
                .unwrap_or_default();
            if !caption.is_empty() {
                let bytes = encode_latin(&caption)?;
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    auto_size(h, pad, |s| text_width(&bytes, s), w - 2.0 * pad)
                };
                let tw = text_width(&bytes, size);
                let x = ((w - tw) / 2.0).max(pad);
                let y = (h - size * 0.72) / 2.0;
                c.push_str(&format!(
                    "q {pad:.4} {pad:.4} {:.4} {:.4} re W n BT /{} {size:.4} Tf {} {x:.4} {y:.4} Td {} Tj ET Q\n",
                    w - 2.0 * pad,
                    h - 2.0 * pad,
                    String::from_utf8_lossy(&da.font),
                    da.color,
                    literal(&bytes)
                ));
            }
        }
        Kind::Text | Kind::Combo => {
            let value = match inherited(doc, widget, b"V")? {
                Some(Object::String(s)) => crate::render::pdf_text_string(&s),
                Some(Object::Name(n)) => String::from_utf8_lossy(&n).into_owned(),
                _ => String::new(),
            };
            // Combo: the value may be an export value — show its display text.
            let value = if kind == Kind::Combo {
                display_for_export(doc, widget, &value)?.unwrap_or(value)
            } else {
                value
            };
            let value = if ff & (1 << 13) != 0 {
                // Password: one bullet per character.
                "*".repeat(value.chars().count().min(MAX_VALUE_CHARS))
            } else {
                value
            };
            if value.trim().is_empty() {
                return Ok(Some(finish(c, w, h, &matrix, Some(resources))));
            }
            let multiline = kind == Kind::Text && ff & (1 << 12) != 0;
            let comb = kind == Kind::Text && ff & (1 << 24) != 0;
            let max_len = inherited(doc, widget, b"MaxLen")?
                .and_then(|o| o.as_i64())
                .unwrap_or(0);
            let inner_w = (w - 2.0 * pad).max(1.0);
            let inner_h = (h - 2.0 * pad).max(1.0);
            c.push_str(&format!(
                "/Tx BMC q {pad:.4} {pad:.4} {inner_w:.4} {inner_h:.4} re W n BT {} ",
                da.color
            ));
            let fname = String::from_utf8_lossy(&da.font).into_owned();
            if comb && max_len > 0 {
                let bytes = encode_latin(&value)?;
                let cell = w / max_len as f64;
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    (inner_h / 1.35).min(cell * 1.4).max(1.0)
                };
                c.push_str(&format!("/{fname} {size:.4} Tf "));
                let y = (h - size * 0.72) / 2.0;
                let mut prev_x = 0.0;
                c.push_str(&format!("1 0 0 1 0 {y:.4} Tm "));
                for (i, &b) in bytes.iter().enumerate().take(max_len as usize) {
                    let cw = text_width(&[b], size);
                    let x = cell * i as f64 + (cell - cw) / 2.0;
                    c.push_str(&format!("{:.4} 0 Td {} Tj ", x - prev_x, literal(&[b])));
                    prev_x = x;
                }
            } else if multiline {
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    12.0_f64.min(inner_h / 1.35).max(4.0)
                };
                let lead = size * 1.15;
                let lines = wrap_lines(&value, inner_w, |s| text_width(s, size))?;
                c.push_str(&format!("/{fname} {size:.4} Tf {lead:.4} TL "));
                let top = h - pad - size * 0.78;
                c.push_str(&format!("1 0 0 1 {pad:.4} {top:.4} Tm "));
                let mut first = true;
                for line in lines {
                    let tw = text_width(&line, size);
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
            } else {
                let bytes = encode_latin(value.lines().next().unwrap_or(""))?;
                let size = if da.size > 0.0 {
                    da.size
                } else {
                    auto_size(h, pad, |s| text_width(&bytes, s), inner_w)
                };
                let tw = text_width(&bytes, size);
                let x = pad
                    + match q {
                        1 => (inner_w - tw) / 2.0,
                        2 => inner_w - tw,
                        _ => 0.0,
                    }
                    .max(0.0);
                let y = (h - size * 0.72) / 2.0;
                c.push_str(&format!(
                    "/{fname} {size:.4} Tf {x:.4} {y:.4} Td {} Tj ",
                    literal(&bytes)
                ));
            }
            c.push_str("ET Q EMC\n");
        }
        Kind::List => {
            let opts = options(doc, widget)?;
            let selected: Vec<String> = match inherited(doc, widget, b"V")? {
                Some(Object::String(s)) => vec![crate::render::pdf_text_string(&s)],
                Some(Object::Array(a)) => a
                    .iter()
                    .filter_map(|o| o.as_string().map(crate::render::pdf_text_string))
                    .collect(),
                _ => Vec::new(),
            };
            let top = inherited(doc, widget, b"TI")?
                .and_then(|o| o.as_i64())
                .unwrap_or(0)
                .max(0) as usize;
            let inner_w = (w - 2.0 * pad).max(1.0);
            let inner_h = (h - 2.0 * pad).max(1.0);
            let size = if da.size > 0.0 {
                da.size
            } else {
                12.0_f64.min(inner_h / 1.35).max(4.0)
            };
            let lead = size * 1.15;
            let fname = String::from_utf8_lossy(&da.font).into_owned();
            c.push_str(&format!(
                "/Tx BMC q {pad:.4} {pad:.4} {inner_w:.4} {inner_h:.4} re W n "
            ));
            let mut y = h - pad;
            for (export, display) in opts.iter().skip(top) {
                if y - lead < pad - lead {
                    break;
                }
                if selected.iter().any(|s| s == export || s == display) {
                    c.push_str(&format!(
                        "q 0.6 0.75 0.85 rg {pad:.4} {:.4} {inner_w:.4} {lead:.4} re f Q ",
                        y - lead
                    ));
                }
                let bytes = encode_latin(display)?;
                c.push_str(&format!(
                    "BT {} /{fname} {size:.4} Tf {:.4} {:.4} Td {} Tj ET ",
                    da.color,
                    pad + 2.0,
                    y - lead + size * 0.28,
                    literal(&bytes)
                ));
                y -= lead;
            }
            c.push_str("Q EMC\n");
        }
    }
    Ok(Some(finish(c, w, h, &matrix, Some(resources))))
}

/// The field value as an appearance-state name (`/V /Yes`), for a
/// widget whose `/AP /N` has several states but no `/AS`.
pub(crate) fn field_value_name(doc: &Document, widget: &Dict) -> Result<Option<Vec<u8>>, Error> {
    Ok(match inherited(doc, widget, b"V")? {
        Some(Object::Name(n)) => Some(n),
        Some(Object::String(s)) => Some(s),
        _ => None,
    })
}

/// The on-state name of a check box / radio widget: the single
/// non-`Off` key of `/AP /N`, when an `/AP` exists.
fn on_state_name(doc: &Document, widget: &Dict) -> Result<Option<Vec<u8>>, Error> {
    let Some(ap) = widget.get(b"AP".as_slice()) else {
        return Ok(None);
    };
    let ap = doc.deref(ap)?;
    let Some(n) = ap.as_dict().and_then(|d| d.get(b"N".as_slice())) else {
        return Ok(None);
    };
    let n = doc.deref(n)?;
    Ok(n.as_dict()
        .and_then(|d| d.keys().find(|k| k.as_slice() != b"Off").cloned()))
}

/// `/Opt` → `(export, display)` pairs (§12.7.4.4).
fn options(doc: &Document, widget: &Dict) -> Result<Vec<(String, String)>, Error> {
    let Some(opt) = inherited(doc, widget, b"Opt")? else {
        return Ok(Vec::new());
    };
    let Some(arr) = opt.as_array() else {
        return Err(Error::Structure("/Opt is not an array".into()));
    };
    let mut out = Vec::new();
    for o in arr.iter().take(MAX_OPTIONS) {
        match doc.deref(o)? {
            Object::String(s) => {
                let t = crate::render::pdf_text_string(&s);
                out.push((t.clone(), t));
            }
            Object::Array(pair) => {
                let e = pair
                    .first()
                    .and_then(|o| o.as_string())
                    .map(crate::render::pdf_text_string)
                    .unwrap_or_default();
                let d = pair
                    .get(1)
                    .and_then(|o| o.as_string())
                    .map(crate::render::pdf_text_string)
                    .unwrap_or_else(|| e.clone());
                out.push((e, d));
            }
            _ => {
                return Err(Error::Structure(
                    "/Opt entries must be strings or pairs".into(),
                ))
            }
        }
    }
    Ok(out)
}

fn display_for_export(doc: &Document, widget: &Dict, value: &str) -> Result<Option<String>, Error> {
    Ok(options(doc, widget)?
        .into_iter()
        .find(|(e, _)| e == value)
        .map(|(_, d)| d))
}

/// Auto font size (`/DA` size 0): fit the height, then shrink until
/// the text fits the width.
fn auto_size(h: f64, pad: f64, width_at: impl Fn(f64) -> f64, avail: f64) -> f64 {
    let mut size = ((h - 2.0 * pad) / 1.35).clamp(1.0, 48.0);
    let tw = width_at(size);
    if tw > avail && tw > 0.0 {
        size = (size * avail / tw).max(1.0);
    }
    size
}

/// Greedy word wrap of `value` into lines of at most `avail` points.
fn wrap_lines(
    value: &str,
    avail: f64,
    width: impl Fn(&[u8]) -> f64,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut lines = Vec::new();
    for para in value.split(['\n', '\r']).take(256) {
        let mut line: Vec<u8> = Vec::new();
        for word in para.split(' ') {
            let wb = encode_latin(word)?;
            let candidate: Vec<u8> = if line.is_empty() {
                wb.clone()
            } else {
                [line.as_slice(), b" ", wb.as_slice()].concat()
            };
            if width(&candidate) <= avail || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::take(&mut line));
                line = wb;
            }
            if lines.len() >= 512 {
                return Ok(lines);
            }
        }
        lines.push(line);
    }
    Ok(lines)
}

/// Check-mark style glyphs drawn as paths (ZapfDingbats codes: `4`
/// check, `l` circle, `8` cross, `n` square, `u` diamond, `H` star).
fn mark_path(style: u8, w: f64, h: f64, pad: f64) -> String {
    let cx = w / 2.0;
    let cy = h / 2.0;
    let r = ((w.min(h) - 2.0 * pad) / 2.0).max(0.5);
    match style {
        b'l' => {
            // Circle via four Béziers.
            let k = 0.5523 * r * 0.6;
            let rr = r * 0.6;
            format!(
                "{:.4} {:.4} m {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c \
                 {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} c f",
                cx + rr, cy,
                cx + rr, cy + k, cx + k, cy + rr, cx, cy + rr,
                cx - k, cy + rr, cx - rr, cy + k, cx - rr, cy,
                cx - rr, cy - k, cx - k, cy - rr, cx, cy - rr,
                cx + k, cy - rr, cx + rr, cy - k, cx + rr, cy
            )
        }
        b'8' => format!(
            "{:.4} w {:.4} {:.4} m {:.4} {:.4} l {:.4} {:.4} m {:.4} {:.4} l S",
            (r / 4.0).max(0.8),
            cx - r * 0.7,
            cy - r * 0.7,
            cx + r * 0.7,
            cy + r * 0.7,
            cx - r * 0.7,
            cy + r * 0.7,
            cx + r * 0.7,
            cy - r * 0.7
        ),
        b'n' => format!(
            "{:.4} {:.4} {:.4} {:.4} re f",
            cx - r * 0.65,
            cy - r * 0.65,
            r * 1.3,
            r * 1.3
        ),
        b'u' | b'H' => format!(
            "{:.4} {:.4} m {:.4} {:.4} l {:.4} {:.4} l {:.4} {:.4} l h f",
            cx,
            cy + r * 0.8,
            cx + r * 0.8,
            cy,
            cx,
            cy - r * 0.8,
            cx - r * 0.8,
            cy
        ),
        _ => format!(
            "{:.4} w 1 J 1 j {:.4} {:.4} m {:.4} {:.4} l {:.4} {:.4} l S",
            (r / 3.5).max(0.8),
            cx - r * 0.7,
            cy + r * 0.05,
            cx - r * 0.2,
            cy - r * 0.55,
            cx + r * 0.75,
            cy + r * 0.7
        ),
    }
}

/// Resources for the text: `/DR` with the `/DA` font guaranteed to
/// exist — a missing one gets a Standard 14 Helvetica entry, which
/// the font loader draws with its bundled substitute (and reports).
fn text_resources(doc: &Document, defaults: &FormDefaults, font: &[u8]) -> Result<Object, Error> {
    let mut res: Dict = match &defaults.dr {
        Some(Object::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    let mut fonts: Dict = match res
        .get(b"Font".as_slice())
        .map(|o| doc.deref(o))
        .transpose()?
    {
        Some(Object::Dict(d)) => d,
        _ => Dict::new(),
    };
    if !fonts.contains_key(font) {
        let mut helv = Dict::new();
        helv.insert(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        helv.insert(b"Subtype".to_vec(), Object::Name(b"Type1".to_vec()));
        helv.insert(b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec()));
        helv.insert(
            b"Encoding".to_vec(),
            Object::Name(b"WinAnsiEncoding".to_vec()),
        );
        fonts.insert(font.to_vec(), Object::Dict(helv));
    }
    res.insert(b"Font".to_vec(), Object::Dict(fonts));
    Ok(Object::Dict(res))
}

fn finish(content: String, w: f64, h: f64, matrix: &str, resources: Option<Object>) -> Appearance {
    let mut dict = Dict::new();
    dict.insert(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
    dict.insert(b"Subtype".to_vec(), Object::Name(b"Form".to_vec()));
    dict.insert(
        b"BBox".to_vec(),
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(w),
            Object::Real(h),
        ]),
    );
    let m: Vec<Object> = matrix
        .trim_matches(|c| c == '[' || c == ']')
        .split_whitespace()
        .map(|t| Object::Real(t.parse().unwrap_or(0.0)))
        .collect();
    dict.insert(b"Matrix".to_vec(), Object::Array(m));
    if let Some(r) = resources {
        dict.insert(b"Resources".to_vec(), r);
    }
    Appearance {
        dict,
        content: content.into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da_parsing() {
        let d = parse_da(Some(b"/Helv 0 Tf 0 0 1 rg"));
        assert_eq!(d.font, b"Helv");
        assert_eq!(d.size, 0.0);
        assert_eq!(d.color, "0.0000 0.0000 1.0000 rg");
        let d = parse_da(Some(b"0.5 g /F1 9.5 Tf"));
        assert_eq!(d.font, b"F1");
        assert_eq!(d.size, 9.5);
        assert_eq!(d.color, "0.5000 g");
        let d = parse_da(None);
        assert_eq!(d.font, b"Helv");
        assert_eq!(d.color, "0 g");
    }

    #[test]
    fn latin_encoding_and_literals() {
        assert_eq!(encode_latin("a(b)\\").unwrap(), b"a(b)\\");
        assert_eq!(literal(b"a(b)\\"), "(a\\(b\\)\\\\)");
        assert_eq!(encode_latin("\u{2019}").unwrap(), vec![0x92]);
        assert!(matches!(
            encode_latin("日本"),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn wrapping_and_auto_size() {
        let width = |s: &[u8]| s.len() as f64 * 5.0;
        let lines = wrap_lines("aa bb cc dd", 26.0, width).unwrap();
        assert_eq!(lines, vec![b"aa bb".to_vec(), b"cc dd".to_vec()]);
        let lines = wrap_lines("x\ny", 100.0, width).unwrap();
        assert_eq!(lines.len(), 2);
        let s = auto_size(20.0, 2.0, |sz| sz * 10.0, 40.0);
        assert!((s - 4.0).abs() < 1e-9, "{s}");
        let s = auto_size(20.0, 2.0, |sz| sz * 1.0, 400.0);
        assert!((s - 16.0 / 1.35).abs() < 1e-9);
    }

    #[test]
    fn colour_ops_and_marks() {
        assert_eq!(color_op(&[1.0], true).unwrap(), "1.0000 g");
        assert_eq!(color_op(&[], true), None);
        assert!(mark_path(b'4', 10.0, 10.0, 1.0).ends_with(" S"));
        assert!(mark_path(b'l', 10.0, 10.0, 1.0).ends_with(" f"));
    }
}
