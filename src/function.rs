//! PDF functions (§7.10): sampled (type 0), exponential (type 2),
//! stitching (type 3) and PostScript calculator (type 4), plus the
//! "array of 1-out functions" form colour spaces and shadings use.
//!
//! Every evaluation is bounded: inputs are clipped to `/Domain`,
//! outputs to `/Range` when present, sampled tables are size-capped at
//! parse time, stitching nesting is depth-capped, and the PostScript
//! calculator runs with a fixed operand-stack ceiling and an
//! instruction budget — a hostile function cannot loop or blow the
//! stack (there is no loop operator; `if`/`ifelse` only nest).

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::Object;

/// Parsed, ready-to-evaluate function.
#[derive(Debug, Clone)]
pub(crate) enum Function {
    Sampled {
        domain: Vec<(f64, f64)>,
        range: Vec<(f64, f64)>,
        size: Vec<usize>,
        encode: Vec<(f64, f64)>,
        decode: Vec<(f64, f64)>,
        /// Samples normalised to 0..=1, `n_out` per grid point, first
        /// input varying fastest.
        samples: Vec<f32>,
        n_out: usize,
    },
    Exponential {
        domain: Vec<(f64, f64)>,
        range: Option<Vec<(f64, f64)>>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitching {
        domain: Vec<(f64, f64)>,
        range: Option<Vec<(f64, f64)>>,
        functions: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<(f64, f64)>,
    },
    PostScript {
        domain: Vec<(f64, f64)>,
        range: Vec<(f64, f64)>,
        program: Vec<PsOp>,
    },
    /// `[f1 f2 …]` — one 1-output function per output component.
    Array(Vec<Function>),
}

/// Largest sampled-function table we will materialise (grid points ×
/// outputs). 2^22 f32 = 16 MiB.
const MAX_SAMPLE_VALUES: usize = 1 << 22;
/// Nesting depth for stitching / array functions.
const MAX_FUNCTION_DEPTH: usize = 8;
/// PostScript calculator: operand stack ceiling (§7.10.5 says 100).
const PS_STACK_MAX: usize = 100;
/// PostScript calculator: instructions per evaluation.
const PS_STEP_BUDGET: usize = 100_000;
/// PostScript calculator: program length cap.
const PS_PROGRAM_MAX: usize = 65_536;

/// One PostScript-calculator instruction.
#[derive(Debug, Clone)]
pub(crate) enum PsOp {
    Num(f64),
    Op(PsName),
    /// A `{ … }` procedure operand for `if` / `ifelse`.
    Block(Vec<PsOp>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsName {
    Abs,
    Add,
    Atan,
    Ceiling,
    Cos,
    Cvi,
    Cvr,
    Div,
    Exp,
    Floor,
    Idiv,
    Ln,
    Log,
    Mod,
    Mul,
    Neg,
    Round,
    Sin,
    Sqrt,
    Sub,
    Truncate,
    And,
    Bitshift,
    Eq,
    False,
    Ge,
    Gt,
    Le,
    Lt,
    Ne,
    Not,
    Or,
    True,
    Xor,
    If,
    IfElse,
    Copy,
    Dup,
    Exch,
    Index,
    Pop,
    Roll,
}

fn pairs(
    obj: Option<&Object>,
    doc: &Document,
    what: &str,
) -> Result<Option<Vec<(f64, f64)>>, Error> {
    let Some(o) = obj else { return Ok(None) };
    let o = doc.deref(o)?;
    let arr = o
        .as_array()
        .ok_or_else(|| Error::Structure(format!("function /{what} is not an array")))?;
    if arr.len() % 2 != 0 || arr.is_empty() {
        return Err(Error::Structure(format!(
            "function /{what} must hold an even, non-zero number of values"
        )));
    }
    let mut out = Vec::with_capacity(arr.len() / 2);
    for p in arr.as_chunks::<2>().0 {
        let a = p[0]
            .as_number()
            .ok_or_else(|| Error::Structure(format!("function /{what} entries must be numbers")))?;
        let b = p[1]
            .as_number()
            .ok_or_else(|| Error::Structure(format!("function /{what} entries must be numbers")))?;
        if !(a.is_finite() && b.is_finite()) {
            return Err(Error::Structure(format!(
                "function /{what} has non-finite values"
            )));
        }
        out.push((a, b));
    }
    Ok(Some(out))
}

fn numbers(obj: Option<&Object>, doc: &Document, what: &str) -> Result<Option<Vec<f64>>, Error> {
    let Some(o) = obj else { return Ok(None) };
    let o = doc.deref(o)?;
    let arr = o
        .as_array()
        .ok_or_else(|| Error::Structure(format!("function /{what} is not an array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for e in arr {
        let v = doc.deref(e)?;
        let v = v
            .as_number()
            .ok_or_else(|| Error::Structure(format!("function /{what} entries must be numbers")))?;
        if !v.is_finite() {
            return Err(Error::Structure(format!(
                "function /{what} has non-finite values"
            )));
        }
        out.push(v);
    }
    Ok(Some(out))
}

/// Parse a function object (dictionary, stream, or array of them).
pub(crate) fn parse_function(
    doc: &Document,
    obj: &Object,
    limits: &Limits,
) -> Result<Function, Error> {
    parse_function_depth(doc, obj, limits, 0)
}

fn parse_function_depth(
    doc: &Document,
    obj: &Object,
    limits: &Limits,
    depth: usize,
) -> Result<Function, Error> {
    if depth > MAX_FUNCTION_DEPTH {
        return Err(Error::LimitExceeded {
            limit: "function nesting depth",
            max: MAX_FUNCTION_DEPTH as u64,
        });
    }
    let resolved = doc.deref(obj)?;
    if let Some(arr) = resolved.as_array() {
        if arr.is_empty() {
            return Err(Error::Structure("function array is empty".into()));
        }
        let mut fs = Vec::with_capacity(arr.len());
        for f in arr {
            let f = parse_function_depth(doc, f, limits, depth + 1)?;
            if f.n_outputs() != Some(1) {
                return Err(Error::Structure(
                    "each function in a function array must produce one output".into(),
                ));
            }
            fs.push(f);
        }
        return Ok(Function::Array(fs));
    }
    let (dict, stream) = match &resolved {
        Object::Dict(d) => (d, None),
        Object::Stream { dict, data } => (dict, Some(data)),
        _ => {
            return Err(Error::Structure(
                "function is neither a dictionary nor a stream".into(),
            ))
        }
    };
    let ftype = dict
        .get(b"FunctionType".as_slice())
        .and_then(|o| o.as_i64())
        .ok_or_else(|| Error::Structure("function has no /FunctionType".into()))?;
    let domain = pairs(dict.get(b"Domain".as_slice()), doc, "Domain")?
        .ok_or_else(|| Error::Structure("function has no /Domain".into()))?;
    let range = pairs(dict.get(b"Range".as_slice()), doc, "Range")?;
    match ftype {
        0 => {
            let raw = stream
                .ok_or_else(|| Error::Structure("type 0 function must be a stream".into()))?;
            let range =
                range.ok_or_else(|| Error::Structure("type 0 function needs /Range".into()))?;
            let n_out = range.len();
            let size: Vec<usize> = numbers(dict.get(b"Size".as_slice()), doc, "Size")?
                .ok_or_else(|| Error::Structure("type 0 function needs /Size".into()))?
                .iter()
                .map(|v| *v as i64)
                .map(|v| {
                    if v >= 1 {
                        Ok(v as usize)
                    } else {
                        Err(Error::Structure("/Size entries must be ≥ 1".into()))
                    }
                })
                .collect::<Result<_, _>>()?;
            if size.len() != domain.len() {
                return Err(Error::Structure(
                    "/Size length must match /Domain inputs".into(),
                ));
            }
            let bps = dict
                .get(b"BitsPerSample".as_slice())
                .and_then(|o| o.as_i64())
                .ok_or_else(|| Error::Structure("type 0 function needs /BitsPerSample".into()))?;
            if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
                return Err(Error::Structure(format!(
                    "/BitsPerSample {bps} is not valid"
                )));
            }
            let total = size
                .iter()
                .try_fold(1usize, |acc, s| acc.checked_mul(*s))
                .and_then(|g| g.checked_mul(n_out))
                .ok_or(Error::LimitExceeded {
                    limit: "sampled function table",
                    max: MAX_SAMPLE_VALUES as u64,
                })?;
            if total > MAX_SAMPLE_VALUES {
                return Err(Error::LimitExceeded {
                    limit: "sampled function table",
                    max: MAX_SAMPLE_VALUES as u64,
                });
            }
            let encode = pairs(dict.get(b"Encode".as_slice()), doc, "Encode")?
                .unwrap_or_else(|| size.iter().map(|s| (0.0, (*s as f64) - 1.0)).collect());
            let decode = pairs(dict.get(b"Decode".as_slice()), doc, "Decode")?
                .unwrap_or_else(|| range.clone());
            if encode.len() != size.len() || decode.len() != n_out {
                return Err(Error::Structure(
                    "/Encode or /Decode length mismatch".into(),
                ));
            }
            let data = crate::filter::decode_stream(dict, raw, limits)?;
            let needed_bits = (total as u64) * bps as u64;
            if (data.len() as u64) * 8 < needed_bits {
                return Err(Error::Structure(format!(
                    "sampled function needs {} bits of samples, stream has {}",
                    needed_bits,
                    data.len() * 8
                )));
            }
            let max = if bps == 32 {
                u32::MAX as f64
            } else {
                ((1u64 << bps) - 1) as f64
            };
            let mut samples = Vec::with_capacity(total);
            let mut bitpos: u64 = 0;
            for _ in 0..total {
                let mut v: u64 = 0;
                for _ in 0..bps {
                    let byte = data[(bitpos / 8) as usize];
                    let bit = (byte >> (7 - (bitpos % 8))) & 1;
                    v = (v << 1) | bit as u64;
                    bitpos += 1;
                }
                samples.push((v as f64 / max) as f32);
            }
            Ok(Function::Sampled {
                domain,
                range,
                size,
                encode,
                decode,
                samples,
                n_out,
            })
        }
        2 => {
            let c0 = numbers(dict.get(b"C0".as_slice()), doc, "C0")?.unwrap_or_else(|| vec![0.0]);
            let c1 = numbers(dict.get(b"C1".as_slice()), doc, "C1")?.unwrap_or_else(|| vec![1.0]);
            if c0.len() != c1.len() || c0.is_empty() {
                return Err(Error::Structure(
                    "/C0 and /C1 must have the same non-zero length".into(),
                ));
            }
            let n = dict
                .get(b"N".as_slice())
                .and_then(|o| o.as_number())
                .ok_or_else(|| Error::Structure("type 2 function needs /N".into()))?;
            if !n.is_finite() {
                return Err(Error::Structure("/N must be finite".into()));
            }
            if domain.len() != 1 {
                return Err(Error::Structure(
                    "type 2 function takes exactly one input".into(),
                ));
            }
            Ok(Function::Exponential {
                domain,
                range,
                c0,
                c1,
                n,
            })
        }
        3 => {
            if domain.len() != 1 {
                return Err(Error::Structure(
                    "type 3 function takes exactly one input".into(),
                ));
            }
            let funcs_obj = dict
                .get(b"Functions".as_slice())
                .ok_or_else(|| Error::Structure("type 3 function needs /Functions".into()))?;
            let funcs_obj = doc.deref(funcs_obj)?;
            let farr = funcs_obj
                .as_array()
                .ok_or_else(|| Error::Structure("/Functions is not an array".into()))?;
            if farr.is_empty() {
                return Err(Error::Structure("/Functions is empty".into()));
            }
            let mut functions = Vec::with_capacity(farr.len());
            for f in farr {
                functions.push(parse_function_depth(doc, f, limits, depth + 1)?);
            }
            let bounds =
                numbers(dict.get(b"Bounds".as_slice()), doc, "Bounds")?.unwrap_or_default();
            if bounds.len() + 1 != functions.len() {
                return Err(Error::Structure(
                    "/Bounds must have one fewer entry than /Functions".into(),
                ));
            }
            let (d0, d1) = domain[0];
            let mut prev = d0;
            for b in &bounds {
                if *b < prev || *b > d1 {
                    return Err(Error::Structure(
                        "/Bounds must be increasing within /Domain".into(),
                    ));
                }
                prev = *b;
            }
            let encode = pairs(dict.get(b"Encode".as_slice()), doc, "Encode")?
                .ok_or_else(|| Error::Structure("type 3 function needs /Encode".into()))?;
            if encode.len() != functions.len() {
                return Err(Error::Structure(
                    "/Encode must have one pair per function".into(),
                ));
            }
            Ok(Function::Stitching {
                domain,
                range,
                functions,
                bounds,
                encode,
            })
        }
        4 => {
            let raw = stream
                .ok_or_else(|| Error::Structure("type 4 function must be a stream".into()))?;
            let range =
                range.ok_or_else(|| Error::Structure("type 4 function needs /Range".into()))?;
            let src = crate::filter::decode_stream(dict, raw, limits)?;
            if src.len() > PS_PROGRAM_MAX {
                return Err(Error::LimitExceeded {
                    limit: "PostScript calculator program bytes",
                    max: PS_PROGRAM_MAX as u64,
                });
            }
            let program = parse_ps(&src)?;
            Ok(Function::PostScript {
                domain,
                range,
                program,
            })
        }
        other => Err(Error::Unsupported {
            feature: "function type",
            detail: format!("/FunctionType {other}"),
        }),
    }
}

fn clip_to(v: f64, (lo, hi): (f64, f64)) -> f64 {
    if lo <= hi {
        v.max(lo).min(hi)
    } else {
        v.max(hi).min(lo)
    }
}

fn interpolate(x: f64, (xmin, xmax): (f64, f64), (ymin, ymax): (f64, f64)) -> f64 {
    if xmax == xmin {
        ymin
    } else {
        ymin + (x - xmin) * (ymax - ymin) / (xmax - xmin)
    }
}

impl Function {
    /// Number of outputs when it is fixed by the definition.
    pub(crate) fn n_outputs(&self) -> Option<usize> {
        match self {
            Function::Sampled { n_out, .. } => Some(*n_out),
            Function::Exponential { c0, range, .. } => {
                Some(range.as_ref().map(|r| r.len()).unwrap_or(c0.len()))
            }
            Function::Stitching {
                range, functions, ..
            } => range
                .as_ref()
                .map(|r| r.len())
                .or_else(|| functions.first().and_then(|f| f.n_outputs())),
            Function::PostScript { range, .. } => Some(range.len()),
            Function::Array(fs) => Some(fs.len()),
        }
    }

    pub(crate) fn n_inputs(&self) -> usize {
        match self {
            Function::Sampled { domain, .. }
            | Function::Exponential { domain, .. }
            | Function::Stitching { domain, .. }
            | Function::PostScript { domain, .. } => domain.len(),
            Function::Array(fs) => fs.first().map(|f| f.n_inputs()).unwrap_or(1),
        }
    }

    /// Evaluate. Inputs are clipped to the domain; outputs to the
    /// range when one is declared.
    pub(crate) fn eval(&self, inputs: &[f64]) -> Result<Vec<f64>, Error> {
        match self {
            Function::Array(fs) => {
                let mut out = Vec::with_capacity(fs.len());
                for f in fs {
                    let r = f.eval(inputs)?;
                    out.push(r.first().copied().unwrap_or(0.0));
                }
                Ok(out)
            }
            Function::Exponential {
                domain,
                range,
                c0,
                c1,
                n,
            } => {
                let x = clip_to(inputs.first().copied().unwrap_or(0.0), domain[0]);
                let t = if *n == 1.0 {
                    x
                } else if x < 0.0 {
                    // x^N for non-integer N is undefined below 0; the
                    // domain normally forbids it — clamp defensively.
                    0.0
                } else {
                    x.powf(*n)
                };
                let mut out: Vec<f64> = c0.iter().zip(c1).map(|(a, b)| a + t * (b - a)).collect();
                if let Some(r) = range {
                    for (v, rr) in out.iter_mut().zip(r) {
                        *v = clip_to(*v, *rr);
                    }
                }
                Ok(out)
            }
            Function::Stitching {
                domain,
                range,
                functions,
                bounds,
                encode,
            } => {
                let (d0, d1) = domain[0];
                let x = clip_to(inputs.first().copied().unwrap_or(0.0), domain[0]);
                let k = bounds.iter().position(|b| x < *b).unwrap_or(bounds.len());
                let lo = if k == 0 { d0 } else { bounds[k - 1] };
                let hi = if k == bounds.len() { d1 } else { bounds[k] };
                let e = interpolate(x, (lo, hi), encode[k]);
                let mut out = functions[k].eval(&[e])?;
                if let Some(r) = range {
                    for (v, rr) in out.iter_mut().zip(r) {
                        *v = clip_to(*v, *rr);
                    }
                }
                Ok(out)
            }
            Function::Sampled {
                domain,
                range,
                size,
                encode,
                decode,
                samples,
                n_out,
            } => {
                let m = size.len();
                // Encoded, clipped sample coordinate per input.
                let mut coord = Vec::with_capacity(m);
                for i in 0..m {
                    let x = clip_to(inputs.get(i).copied().unwrap_or(0.0), domain[i]);
                    let e = interpolate(x, domain[i], encode[i]);
                    coord.push(e.max(0.0).min((size[i] - 1) as f64));
                }
                // Multilinear interpolation over the 2^m surrounding
                // grid points (m is small in practice; capped by the
                // table size limit anyway).
                let mut out = vec![0.0f64; *n_out];
                let corners = 1usize << m.min(16);
                for c in 0..corners {
                    let mut weight = 1.0;
                    let mut index = 0usize;
                    let mut stride = 1usize;
                    for i in 0..m {
                        let f = coord[i].floor();
                        let frac = coord[i] - f;
                        let hi = (c >> i) & 1 == 1;
                        let (idx, w) = if hi {
                            if frac == 0.0 {
                                (f as usize, 0.0)
                            } else {
                                ((f as usize + 1).min(size[i] - 1), frac)
                            }
                        } else {
                            (f as usize, 1.0 - frac)
                        };
                        weight *= w;
                        index += idx * stride;
                        stride *= size[i];
                    }
                    if weight == 0.0 {
                        continue;
                    }
                    for (j, o) in out.iter_mut().enumerate() {
                        let s = samples.get(index * n_out + j).copied().unwrap_or(0.0) as f64;
                        *o += weight * s;
                    }
                }
                for (j, o) in out.iter_mut().enumerate() {
                    let v = interpolate(*o, (0.0, 1.0), decode[j]);
                    *o = clip_to(v, range[j]);
                }
                Ok(out)
            }
            Function::PostScript {
                domain,
                range,
                program,
            } => {
                let mut stack: Vec<f64> = Vec::with_capacity(PS_STACK_MAX);
                for (i, d) in domain.iter().enumerate() {
                    stack.push(clip_to(inputs.get(i).copied().unwrap_or(0.0), *d));
                }
                let mut budget = PS_STEP_BUDGET;
                exec_ps(program, &mut stack, &mut budget)?;
                let n = range.len();
                if stack.len() < n {
                    return Err(Error::Structure(format!(
                        "PostScript function left {} values, /Range needs {n}",
                        stack.len()
                    )));
                }
                let tail = &stack[stack.len() - n..];
                Ok(tail
                    .iter()
                    .zip(range)
                    .map(|(v, r)| clip_to(*v, *r))
                    .collect())
            }
        }
    }
}

// ---- PostScript calculator -------------------------------------------

fn parse_ps(src: &[u8]) -> Result<Vec<PsOp>, Error> {
    let text = String::from_utf8_lossy(src);
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '{' | '}' => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
                tokens.push(ch.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            '%' => {
                // comment to end of line — rare in calculator code
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
                cur.push('%');
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut pos = 0usize;
    // The whole program is one outer `{ … }`.
    if tokens.first().map(|t| t.as_str()) != Some("{") {
        return Err(Error::Structure(
            "PostScript function must start with '{'".into(),
        ));
    }
    pos += 1;
    let body = parse_ps_block(&tokens, &mut pos, 0)?;
    Ok(body)
}

fn parse_ps_block(tokens: &[String], pos: &mut usize, depth: usize) -> Result<Vec<PsOp>, Error> {
    if depth > 64 {
        return Err(Error::LimitExceeded {
            limit: "PostScript calculator block nesting",
            max: 64,
        });
    }
    let mut out = Vec::new();
    while *pos < tokens.len() {
        let t = tokens[*pos].as_str();
        *pos += 1;
        match t {
            "}" => return Ok(out),
            "{" => {
                let inner = parse_ps_block(tokens, pos, depth + 1)?;
                out.push(PsOp::Block(inner));
            }
            _ => {
                if let Ok(v) = t.parse::<f64>() {
                    if !v.is_finite() {
                        return Err(Error::Structure(
                            "PostScript function literal is not finite".into(),
                        ));
                    }
                    out.push(PsOp::Num(v));
                } else if let Some(op) = ps_name(t) {
                    out.push(PsOp::Op(op));
                } else if t.starts_with('%') {
                    continue;
                } else {
                    return Err(Error::Structure(format!(
                        "PostScript function: unknown operator '{t}'"
                    )));
                }
            }
        }
    }
    Err(Error::Structure(
        "PostScript function: unbalanced '{'".into(),
    ))
}

fn ps_name(t: &str) -> Option<PsName> {
    use PsName::*;
    Some(match t {
        "abs" => Abs,
        "add" => Add,
        "atan" => Atan,
        "ceiling" => Ceiling,
        "cos" => Cos,
        "cvi" => Cvi,
        "cvr" => Cvr,
        "div" => Div,
        "exp" => Exp,
        "floor" => Floor,
        "idiv" => Idiv,
        "ln" => Ln,
        "log" => Log,
        "mod" => Mod,
        "mul" => Mul,
        "neg" => Neg,
        "round" => Round,
        "sin" => Sin,
        "sqrt" => Sqrt,
        "sub" => Sub,
        "truncate" => Truncate,
        "and" => And,
        "bitshift" => Bitshift,
        "eq" => Eq,
        "false" => False,
        "ge" => Ge,
        "gt" => Gt,
        "le" => Le,
        "lt" => Lt,
        "ne" => Ne,
        "not" => Not,
        "or" => Or,
        "true" => True,
        "xor" => Xor,
        "if" => If,
        "ifelse" => IfElse,
        "copy" => Copy,
        "dup" => Dup,
        "exch" => Exch,
        "index" => Index,
        "pop" => Pop,
        "roll" => Roll,
        _ => return None,
    })
}

fn exec_ps(program: &[PsOp], stack: &mut Vec<f64>, budget: &mut usize) -> Result<(), Error> {
    fn pop(stack: &mut Vec<f64>) -> Result<f64, Error> {
        stack
            .pop()
            .ok_or_else(|| Error::Structure("PostScript function: stack underflow".into()))
    }
    fn push(stack: &mut Vec<f64>, v: f64) -> Result<(), Error> {
        if stack.len() >= PS_STACK_MAX {
            return Err(Error::LimitExceeded {
                limit: "PostScript calculator stack",
                max: PS_STACK_MAX as u64,
            });
        }
        stack.push(if v.is_finite() { v } else { 0.0 });
        Ok(())
    }
    let mut i = 0usize;
    while i < program.len() {
        if *budget == 0 {
            return Err(Error::LimitExceeded {
                limit: "PostScript calculator steps",
                max: PS_STEP_BUDGET as u64,
            });
        }
        *budget -= 1;
        match &program[i] {
            PsOp::Num(v) => push(stack, *v)?,
            PsOp::Block(_) => {
                // Blocks are consumed by the following if/ifelse.
                let b1 = match &program[i] {
                    PsOp::Block(b) => b,
                    _ => unreachable!(),
                };
                match program.get(i + 1) {
                    Some(PsOp::Op(PsName::If)) => {
                        let cond = pop(stack)? != 0.0;
                        if cond {
                            exec_ps(b1, stack, budget)?;
                        }
                        i += 2;
                        continue;
                    }
                    Some(PsOp::Block(b2)) => {
                        if !matches!(program.get(i + 2), Some(PsOp::Op(PsName::IfElse))) {
                            return Err(Error::Structure(
                                "PostScript function: two procedures must be followed by ifelse"
                                    .into(),
                            ));
                        }
                        let cond = pop(stack)? != 0.0;
                        if cond {
                            exec_ps(b1, stack, budget)?;
                        } else {
                            exec_ps(b2, stack, budget)?;
                        }
                        i += 3;
                        continue;
                    }
                    _ => {
                        return Err(Error::Structure(
                            "PostScript function: procedure not followed by if/ifelse".into(),
                        ))
                    }
                }
            }
            PsOp::Op(op) => {
                use PsName::*;
                match op {
                    If | IfElse => {
                        return Err(Error::Structure(
                            "PostScript function: if/ifelse without a procedure".into(),
                        ))
                    }
                    Abs => {
                        let a = pop(stack)?;
                        push(stack, a.abs())?
                    }
                    Add => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        push(stack, a + b)?
                    }
                    Sub => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        push(stack, a - b)?
                    }
                    Mul => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        push(stack, a * b)?
                    }
                    Div => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        push(stack, if b == 0.0 { 0.0 } else { a / b })?
                    }
                    Idiv => {
                        let b = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        push(stack, if b == 0 { 0.0 } else { (a / b) as f64 })?
                    }
                    Mod => {
                        let b = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        push(stack, if b == 0 { 0.0 } else { (a % b) as f64 })?
                    }
                    Neg => {
                        let a = pop(stack)?;
                        push(stack, -a)?
                    }
                    Atan => {
                        let den = pop(stack)?;
                        let num = pop(stack)?;
                        let mut deg = num.atan2(den).to_degrees();
                        if deg < 0.0 {
                            deg += 360.0;
                        }
                        push(stack, deg)?
                    }
                    Ceiling => {
                        let a = pop(stack)?;
                        push(stack, a.ceil())?
                    }
                    Floor => {
                        let a = pop(stack)?;
                        push(stack, a.floor())?
                    }
                    Round => {
                        let a = pop(stack)?;
                        push(stack, (a + 0.5).floor())?
                    }
                    Truncate => {
                        let a = pop(stack)?;
                        push(stack, a.trunc())?
                    }
                    Cos => {
                        let a = pop(stack)?;
                        push(stack, a.to_radians().cos())?
                    }
                    Sin => {
                        let a = pop(stack)?;
                        push(stack, a.to_radians().sin())?
                    }
                    Sqrt => {
                        let a = pop(stack)?;
                        push(stack, a.max(0.0).sqrt())?
                    }
                    Exp => {
                        let e = pop(stack)?;
                        let b = pop(stack)?;
                        push(stack, b.powf(e))?
                    }
                    Ln => {
                        let a = pop(stack)?;
                        push(stack, if a > 0.0 { a.ln() } else { 0.0 })?
                    }
                    Log => {
                        let a = pop(stack)?;
                        push(stack, if a > 0.0 { a.log10() } else { 0.0 })?
                    }
                    Cvi => {
                        let a = pop(stack)?;
                        push(stack, a.trunc())?
                    }
                    Cvr => {}
                    And => {
                        let b = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        push(stack, (a & b) as f64)?
                    }
                    Or => {
                        let b = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        push(stack, (a | b) as f64)?
                    }
                    Xor => {
                        let b = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        push(stack, (a ^ b) as f64)?
                    }
                    Not => {
                        let a = pop(stack)?;
                        // Boolean not for 0/1, bitwise for other ints.
                        let v = if a == 0.0 {
                            1.0
                        } else if a == 1.0 {
                            0.0
                        } else {
                            (!(a as i64)) as f64
                        };
                        push(stack, v)?
                    }
                    Bitshift => {
                        let s = pop(stack)? as i64;
                        let a = pop(stack)? as i64;
                        let v = if s >= 0 {
                            a.checked_shl(s.min(63) as u32).unwrap_or(0)
                        } else {
                            a >> ((-s).min(63) as u32)
                        };
                        push(stack, v as f64)?
                    }
                    Eq | Ne | Gt | Ge | Lt | Le => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        let r = match op {
                            Eq => a == b,
                            Ne => a != b,
                            Gt => a > b,
                            Ge => a >= b,
                            Lt => a < b,
                            _ => a <= b,
                        };
                        push(stack, if r { 1.0 } else { 0.0 })?
                    }
                    True => push(stack, 1.0)?,
                    False => push(stack, 0.0)?,
                    Dup => {
                        let a = *stack.last().ok_or_else(|| {
                            Error::Structure("PostScript function: stack underflow".into())
                        })?;
                        push(stack, a)?
                    }
                    Pop => {
                        pop(stack)?;
                    }
                    Exch => {
                        let b = pop(stack)?;
                        let a = pop(stack)?;
                        push(stack, b)?;
                        push(stack, a)?
                    }
                    Copy => {
                        let n = pop(stack)? as i64;
                        if n < 0 || n as usize > stack.len() {
                            return Err(Error::Structure(
                                "PostScript function: bad copy count".into(),
                            ));
                        }
                        let n = n as usize;
                        let base = stack.len() - n;
                        for k in 0..n {
                            let v = stack[base + k];
                            push(stack, v)?;
                        }
                    }
                    Index => {
                        let n = pop(stack)? as i64;
                        if n < 0 || n as usize >= stack.len() {
                            return Err(Error::Structure("PostScript function: bad index".into()));
                        }
                        let v = stack[stack.len() - 1 - n as usize];
                        push(stack, v)?
                    }
                    Roll => {
                        let j = pop(stack)? as i64;
                        let n = pop(stack)? as i64;
                        if n < 0 || n as usize > stack.len() {
                            return Err(Error::Structure(
                                "PostScript function: bad roll count".into(),
                            ));
                        }
                        let n = n as usize;
                        if n > 0 {
                            let base = stack.len() - n;
                            let j = j.rem_euclid(n as i64) as usize;
                            stack[base..].rotate_right(j);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(src: &str, inputs: &[f64], domain: usize, range: usize) -> Vec<f64> {
        let program = parse_ps(src.as_bytes()).unwrap();
        let f = Function::PostScript {
            domain: vec![(-1e9, 1e9); domain],
            range: vec![(-1e9, 1e9); range],
            program,
        };
        f.eval(inputs).unwrap()
    }

    #[test]
    fn postscript_arithmetic_and_control() {
        assert_eq!(ps("{ add 2 mul }", &[1.0, 2.0], 2, 1), vec![6.0]);
        assert_eq!(
            ps("{ dup 0.5 gt { pop 1 } { pop 0 } ifelse }", &[0.7], 1, 1),
            vec![1.0]
        );
        assert_eq!(
            ps("{ dup 0.5 gt { pop 1 } { pop 0 } ifelse }", &[0.2], 1, 1),
            vec![0.0]
        );
        assert_eq!(
            ps("{ 3 1 roll }", &[1.0, 2.0, 3.0], 3, 3),
            vec![3.0, 1.0, 2.0]
        );
        assert_eq!(ps("{ 1 index }", &[4.0, 5.0], 2, 3), vec![4.0, 5.0, 4.0]);
        assert_eq!(ps("{ 2 copy add }", &[1.0, 2.0], 2, 3), vec![1.0, 2.0, 3.0]);
        assert_eq!(ps("{ 90 sin }", &[], 0, 1), vec![1.0]);
        assert_eq!(ps("{ 1 exch sub }", &[0.25], 1, 1), vec![0.75]);
    }

    #[test]
    fn postscript_is_bounded() {
        // Stack overflow via dup is refused, never a panic.
        let src = format!("{{ {} }}", "dup ".repeat(200));
        let program = parse_ps(src.as_bytes()).unwrap();
        let f = Function::PostScript {
            domain: vec![(0.0, 1.0)],
            range: vec![(0.0, 1.0)],
            program,
        };
        assert!(matches!(f.eval(&[0.5]), Err(Error::LimitExceeded { .. })));
        // Underflow and unknown operators are structure errors.
        assert!(parse_ps(b"{ frobnicate }").is_err());
        let program = parse_ps(b"{ add }").unwrap();
        let f = Function::PostScript {
            domain: vec![],
            range: vec![(0.0, 1.0)],
            program,
        };
        assert!(matches!(f.eval(&[]), Err(Error::Structure(_))));
    }

    #[test]
    fn exponential_and_stitching() {
        let e = Function::Exponential {
            domain: vec![(0.0, 1.0)],
            range: None,
            c0: vec![0.0, 1.0],
            c1: vec![1.0, 0.0],
            n: 1.0,
        };
        assert_eq!(e.eval(&[0.25]).unwrap(), vec![0.25, 0.75]);
        let s = Function::Stitching {
            domain: vec![(0.0, 1.0)],
            range: None,
            functions: vec![e.clone(), e],
            bounds: vec![0.5],
            encode: vec![(0.0, 1.0), (1.0, 0.0)],
        };
        assert_eq!(s.eval(&[0.25]).unwrap(), vec![0.5, 0.5]);
        assert_eq!(s.eval(&[0.75]).unwrap(), vec![0.5, 0.5]);
        assert_eq!(s.eval(&[1.0]).unwrap(), vec![0.0, 1.0]);
    }

    #[test]
    fn sampled_interpolates() {
        // 1 input, 1 output, 3 samples 0, 0.5, 1 (8-bit).
        let f = Function::Sampled {
            domain: vec![(0.0, 1.0)],
            range: vec![(0.0, 1.0)],
            size: vec![3],
            encode: vec![(0.0, 2.0)],
            decode: vec![(0.0, 1.0)],
            samples: vec![0.0, 0.5, 1.0],
            n_out: 1,
        };
        assert!((f.eval(&[0.25]).unwrap()[0] - 0.25).abs() < 1e-6);
        assert!((f.eval(&[1.0]).unwrap()[0] - 1.0).abs() < 1e-6);
        assert!(
            (f.eval(&[5.0]).unwrap()[0] - 1.0).abs() < 1e-6,
            "clipped to domain"
        );
    }
}
