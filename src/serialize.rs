//! Serialize [`Object`]s back into PDF syntax (ISO 32000-1:2008, §7.3).
//!
//! Deterministic: the same `Object` always produces the same bytes —
//! dictionary keys come from a `BTreeMap` (already sorted), names use
//! `#xx` hex escapes for every byte outside the regular set, strings
//! are emitted as `<hex>` so no byte is ever reinterpreted as UTF-8 or
//! locale-dependent. Stream `/Length` is recomputed from the emitted
//! data — the stored value is never trusted.
//!
//! Number formatting: integers via `i64`; reals via `format!("{:.10}")`
//! with trailing zeros trimmed — always finite and decimal, never
//! scientific notation (§7.3.3 requires a decimal point form). The
//! parser stores `f64`; sub-ulp precision loss on rewrite is inherent
//! to the in-memory model and documented as such.

use crate::error::Error;
use crate::object::{Dict, IndirectRef, Object};

/// Maximum nesting depth for array/dict literals when serializing.
const MAX_NESTING: usize = 256;

/// Write one object (not wrapped in `n g obj`) to `out`.
///
/// `remap` translates an old [`IndirectRef`] to its new object number;
/// a ref not present in the map is a `MissingReference`.
pub(crate) fn write_object(
    obj: &Object,
    remap: &dyn Fn(IndirectRef) -> Option<u32>,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), Error> {
    if depth > MAX_NESTING {
        return Err(Error::LimitExceeded {
            limit: "serialize nesting depth",
            max: MAX_NESTING as u64,
        });
    }
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => write_real(*r, out)?,
        Object::Name(n) => write_name(n, out),
        Object::String(s) => write_hex_string(s, out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(item, remap, out, depth + 1)?;
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(d, remap, out, depth)?,
        Object::Stream { dict, data } => {
            let mut d = dict.clone();
            // Length is recomputed from the emitted body — the source
            // /Length (and any reference it held) is discarded.
            d.insert(
                b"Length".as_slice().to_vec(),
                Object::Integer(data.len() as i64),
            );
            write_dict(&d, remap, out, depth)?;
            // `stream` is followed by CRLF or LF; `endstream` must be
            // preceded by an EOL that is *not* part of the stream data
            // (§7.3.8.1). We always emit LF after `stream` and an LF
            // before `endstream` — bytes inside the stream are opaque
            // so any `endstream`-looking sequence in `data` is safe.
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Ref(r) => {
            let new_num = remap(*r).ok_or_else(|| {
                Error::Structure(format!("reference {r} was not collected during rewrite"))
            })?;
            out.extend_from_slice(format!("{new_num} 0 R").as_bytes());
        }
    }
    Ok(())
}

/// `<< /k v /k v >>` — keys sorted (BTreeMap order).
fn write_dict(
    d: &Dict,
    remap: &dyn Fn(IndirectRef) -> Option<u32>,
    out: &mut Vec<u8>,
    depth: usize,
) -> Result<(), Error> {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        out.push(b' ');
        write_name(k, out);
        out.push(b' ');
        write_object(v, remap, out, depth + 1)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

/// Emit a name with `#xx` escapes for delimiter/whitespace/out-of-range
/// bytes (§7.3.5: "any character except null may be included… by
/// writing its 2-digit hexadecimal code preceded by #").
fn write_name(n: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in n {
        if is_regular_name_byte(b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

/// Regular name characters per §7.3.5 Table 5 note — anything except
/// whitespace, delimiters `()<>[]{}/%` and bytes outside `!`..`~`.
fn is_regular_name_byte(b: u8) -> bool {
    !(b <= 0x20
        || b >= 0x7F
        || matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
        ))
}

/// `<hex>` — hex strings never reinterpret bytes and never need
/// escaping; any byte sequence round-trips.
fn write_hex_string(s: &[u8], out: &mut Vec<u8>) {
    out.push(b'<');
    for &b in s {
        out.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    out.push(b'>');
}

/// §7.3.3 real numbers: must contain a decimal point, must not use
/// scientific notation, must be finite. `f64::is_finite` rejects
/// NaN/±inf outright — they can never appear in a valid PDF.
fn write_real(r: f64, out: &mut Vec<u8>) -> Result<(), Error> {
    if !r.is_finite() {
        return Err(Error::Structure(format!(
            "real number {r} is not finite — cannot serialize"
        )));
    }
    // `{:.10}` is enough precision for PDF coordinates (spec examples
    // use ≤4 decimals); trimming `0`s keeps determinism.
    let s = format!("{r:.10}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    // Always keep a decimal point (e.g. `5` → `5.0`) so the token is
    // unambiguously a real, not an integer (§7.3.3 note).
    let s = if trimmed.contains('.') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.0")
    };
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Write a complete indirect object `n 0 obj … endobj`.
pub(crate) fn write_indirect_object(
    number: u32,
    obj: &Object,
    remap: &dyn Fn(IndirectRef) -> Option<u32>,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
    write_object(obj, remap, out, 0)?;
    out.extend_from_slice(b"\nendobj\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Dict;
    use std::collections::BTreeMap;

    fn ser(obj: &Object) -> Vec<u8> {
        let mut out = Vec::new();
        write_object(obj, &|_| None, &mut out, 0).unwrap();
        out
    }

    #[test]
    fn name_special_bytes_escaped() {
        let mut d: Dict = BTreeMap::new();
        d.insert(b"weird key".to_vec(), Object::String(b"v".to_vec()));
        let out = ser(&Object::Dict(d));
        assert!(String::from_utf8_lossy(&out).contains("/weird#20key"));
    }

    #[test]
    fn real_never_exponent() {
        let out = ser(&Object::Real(1.5e20));
        let s = String::from_utf8_lossy(&out);
        assert!(!s.contains('e') && !s.contains('E'), "{s}");
        assert!(s.contains('.'), "{s}");
    }

    #[test]
    fn real_nan_rejected() {
        let mut out = Vec::new();
        assert!(write_object(&Object::Real(f64::NAN), &|_| None, &mut out, 0).is_err());
    }

    #[test]
    fn empty_containers() {
        let mut d: Dict = BTreeMap::new();
        d.insert(b"EmptyArr".to_vec(), Object::Array(vec![]));
        d.insert(b"EmptyDict".to_vec(), Object::Dict(BTreeMap::new()));
        let out = ser(&Object::Dict(d));
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/EmptyArr []"), "{s}");
        assert!(s.contains("/EmptyDict << >>"), "{s}");
    }

    #[test]
    fn string_is_hex_not_utf8() {
        // 0xFF is not valid UTF-8 — must survive verbatim as hex.
        let out = ser(&Object::String(vec![0xFF, 0x00, 0x41]));
        assert_eq!(out, b"<FF0041>");
    }

    #[test]
    fn stream_endstream_inside_data_safe() {
        let mut d: Dict = BTreeMap::new();
        d.insert(b"Length".to_vec(), Object::Integer(0)); // lie — recomputed
        let data = b"fake endstream inside";
        let mut out = Vec::new();
        write_object(
            &Object::Stream {
                dict: d,
                data: data.to_vec(),
            },
            &|_| None,
            &mut out,
            0,
        )
        .unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("/Length 21"), "{s}");
        assert!(out.windows(data.len()).any(|w| w == data));
    }
}
