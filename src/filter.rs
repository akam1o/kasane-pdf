//! Stream filter decoding (ISO 32000-1:2008, §7.4).
//!
//! Supported: `/FlateDecode` (zlib/DEFLATE via `flate2`/`miniz_oxide`).
//! PNG predictors (`/Predictor` 10–15) and the TIFF predictor (2) are
//! implemented for `/FlateDecode` output.
//!
//! Not supported (reported as [`Error::Unsupported`]):
//! `/ASCIIHexDecode`, `/ASCII85Decode`, `/LZWDecode`, `/RunLengthDecode`,
//! `/CCITTFaxDecode`, `/JBIG2Decode`, `/DCTDecode`, `/JPXDecode`,
//! `/Crypt`, and any unknown name. `/DecodeParms` entries for filters
//! we do not implement are never inspected — we refuse before that.
//!
//! Multiple filters are applied in order (`/Filter [/A /B]` means data
//! was encoded first by `/B` then by `/A`, so decoding applies `/A`
//! first). `/DecodeParms` is matched positionally; a `null` entry means
//! "no parameters" for that filter.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use flate2::read::ZlibDecoder;
use std::io::Read;

/// Decode `data` according to the stream dictionary's `/Filter` and
/// `/DecodeParms`, bounded by `limits`.
///
/// `raw_len` is the encoded length (for diagnostics). Returns the decoded
/// bytes. On any failure the whole decode fails — partial output is
/// never returned.
pub(crate) fn decode_stream(dict: &Dict, data: &[u8], limits: &Limits) -> Result<Vec<u8>, Error> {
    let filters = normalize_filters(dict.get(b"Filter".as_slice()))?;
    if filters.is_empty() {
        return Ok(data.to_vec());
    }
    if filters.len() > limits.max_filter_chain {
        return Err(Error::LimitExceeded {
            limit: "max_filter_chain",
            max: limits.max_filter_chain as u64,
        });
    }

    let parms = normalize_decode_parms(dict.get(b"DecodeParms".as_slice()), filters.len())?;

    let mut buf = data.to_vec();
    for (i, fname) in filters.iter().enumerate() {
        let parm = parms.get(i).and_then(|o| o.as_ref());
        buf = apply_one_filter(fname, parm, &buf, limits)?;
        if buf.len() as u64 > limits.max_stream_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_stream_bytes (decoded)",
                max: limits.max_stream_bytes,
            });
        }
    }
    Ok(buf)
}

/// `/Filter` may be a name, an array of names, or absent. Returns the
/// list of filter names in application order.
fn normalize_filters(obj: Option<&Object>) -> Result<Vec<Vec<u8>>, Error> {
    match obj {
        None | Some(Object::Null) => Ok(Vec::new()),
        Some(Object::Name(n)) => Ok(vec![n.clone()]),
        Some(Object::Array(a)) => {
            let mut out = Vec::with_capacity(a.len());
            for o in a {
                match o {
                    Object::Name(n) => out.push(n.clone()),
                    _ => {
                        return Err(Error::Structure(
                            "/Filter array must contain only names".into(),
                        ))
                    }
                }
            }
            Ok(out)
        }
        _ => Err(Error::Structure("/Filter must be a name or array".into())),
    }
}

/// `/DecodeParms` may be a dict (single filter), an array aligned with
/// the filter array, or absent. Each element may be `null` or a dict.
/// Returns a Vec of `Option<&Dict>` aligned with `filters`.
fn normalize_decode_parms(
    obj: Option<&Object>,
    n_filters: usize,
) -> Result<Vec<Option<Dict>>, Error> {
    match obj {
        None | Some(Object::Null) => Ok(vec![None; n_filters]),
        Some(Object::Dict(d)) => {
            if n_filters == 1 {
                Ok(vec![Some(d.clone())])
            } else {
                // A single dict is only unambiguous with one filter;
                // with multiple filters the spec expects an array.
                Err(Error::Structure(
                    "/DecodeParms dict given but /Filter is an array".into(),
                ))
            }
        }
        Some(Object::Array(a)) => {
            if a.len() != n_filters {
                return Err(Error::Structure(format!(
                    "/DecodeParms array has {} entries but /Filter has {}",
                    a.len(),
                    n_filters
                )));
            }
            let mut out = Vec::with_capacity(a.len());
            for o in a {
                match o {
                    Object::Null => out.push(None),
                    Object::Dict(d) => out.push(Some(d.clone())),
                    _ => {
                        return Err(Error::Structure(
                            "/DecodeParms entries must be dict or null".into(),
                        ))
                    }
                }
            }
            Ok(out)
        }
        _ => Err(Error::Structure(
            "/DecodeParms must be a dict, array, or null".into(),
        )),
    }
}

/// Apply one named filter to `buf` with its decode parameters.
fn apply_one_filter(
    name: &[u8],
    parms: Option<&Dict>,
    buf: &[u8],
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    match name {
        b"FlateDecode" | b"Fl" => flate_decode(buf, parms, limits),
        other => Err(Error::Unsupported {
            feature: "stream filter",
            detail: format!("/{}", String::from_utf8_lossy(other)),
        }),
    }
}

/// zlib/DEFLATE with optional predictor post-processing.
fn flate_decode(buf: &[u8], parms: Option<&Dict>, limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut decoder = ZlibDecoder::new(buf);
    let mut out = Vec::new();
    // Read in bounded chunks; check the cap as we go so a hostile stream
    // cannot force an allocation beyond max_stream_bytes.
    let cap = limits.max_stream_bytes as usize;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = decoder
            .read(&mut chunk)
            .map_err(|e| Error::Parse(format!("FlateDecode failed: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len() + n > cap {
            return Err(Error::LimitExceeded {
                limit: "max_stream_bytes (decoded)",
                max: limits.max_stream_bytes,
            });
        }
        out.extend_from_slice(&chunk[..n]);
    }

    apply_predictor(out, parms, limits)
}

/// Predictor parameters from `/DecodeParms`.
struct PredictorParms {
    predictor: u32,
    colors: usize,
    bits_per_component: u32,
    columns: usize,
}

fn predictor_parms(parms: Option<&Dict>) -> Result<Option<PredictorParms>, Error> {
    let dict = match parms {
        None => return Ok(None),
        Some(d) => d,
    };
    let get_i64 = |key: &[u8], default: i64| -> Result<i64, Error> {
        match dict.get(key) {
            None | Some(Object::Null) => Ok(default),
            Some(o) => o.as_i64().ok_or_else(|| {
                Error::Structure(format!(
                    "/DecodeParms /{} is not an integer",
                    String::from_utf8_lossy(key)
                ))
            }),
        }
    };
    let predictor = get_i64(b"Predictor", 1)?;
    if predictor == 1 {
        return Ok(None);
    }
    if !(2..=15).contains(&predictor) {
        return Err(Error::Unsupported {
            feature: "FlateDecode predictor",
            detail: format!("/Predictor {predictor}"),
        });
    }
    let colors = get_i64(b"Colors", 1)?;
    let bits = get_i64(b"BitsPerComponent", 8)?;
    let columns = get_i64(b"Columns", 1)?;
    if !(1..=4).contains(&colors) {
        return Err(Error::Unsupported {
            feature: "FlateDecode Colors",
            detail: format!("/Colors {colors}"),
        });
    }
    if ![1, 2, 4, 8, 16].contains(&bits) {
        return Err(Error::Unsupported {
            feature: "FlateDecode BitsPerComponent",
            detail: format!("/BitsPerComponent {bits}"),
        });
    }
    if columns < 1 {
        return Err(Error::Structure("/Columns must be >= 1".into()));
    }
    Ok(Some(PredictorParms {
        predictor: predictor as u32,
        colors: colors as usize,
        bits_per_component: bits as u32,
        columns: columns as usize,
    }))
}

/// Apply TIFF (2) or PNG (10–15) predictor reconstruction.
fn apply_predictor(data: Vec<u8>, parms: Option<&Dict>, limits: &Limits) -> Result<Vec<u8>, Error> {
    let p = match predictor_parms(parms)? {
        None => return Ok(data),
        Some(p) => p,
    };

    if p.predictor == 2 {
        return tiff_predictor(&data, &p, limits);
    }
    png_predictor(&data, &p, limits)
}

/// TIFF Predictor 2 (horizontal differencing). Valid only for 8-bit
/// components in this implementation.
fn tiff_predictor(data: &[u8], p: &PredictorParms, limits: &Limits) -> Result<Vec<u8>, Error> {
    if p.bits_per_component != 8 {
        return Err(Error::Unsupported {
            feature: "TIFF predictor BitsPerComponent",
            detail: format!(
                "/BitsPerComponent {} (only 8 supported)",
                p.bits_per_component
            ),
        });
    }
    let bytes_per_pixel = p.colors;
    let row_len = p.columns.saturating_mul(bytes_per_pixel);
    if row_len == 0 {
        return Err(Error::Structure("TIFF predictor row length is zero".into()));
    }
    let mut out = data.to_vec();
    for row in out.chunks_exact_mut(row_len) {
        for i in bytes_per_pixel..row.len() {
            row[i] = row[i].wrapping_add(row[i - bytes_per_pixel]);
        }
    }
    if out.len() as u64 > limits.max_stream_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_stream_bytes (decoded)",
            max: limits.max_stream_bytes,
        });
    }
    Ok(out)
}

/// PNG predictors (10–15). Each row starts with a filter byte (0–4).
fn png_predictor(data: &[u8], p: &PredictorParms, limits: &Limits) -> Result<Vec<u8>, Error> {
    if p.bits_per_component != 8 {
        return Err(Error::Unsupported {
            feature: "PNG predictor BitsPerComponent",
            detail: format!(
                "/BitsPerComponent {} (only 8 supported)",
                p.bits_per_component
            ),
        });
    }
    let bytes_per_pixel = p.colors;
    let row_len = p.columns.saturating_mul(bytes_per_pixel);
    let stride = row_len + 1; // +1 for the filter byte
    if row_len == 0 {
        return Err(Error::Structure("PNG predictor row length is zero".into()));
    }
    if data.len() % stride != 0 {
        return Err(Error::Parse(format!(
            "PNG predictor data length {} is not a multiple of row size {}",
            data.len(),
            stride
        )));
    }
    let mut out = Vec::with_capacity(data.len() - data.len() / stride);
    let mut prev: Vec<u8> = vec![0u8; row_len];
    for row in data.chunks_exact(stride) {
        let (filter, row_data) = (row[0], &row[1..]);
        let mut cur = row_data.to_vec();
        match filter {
            0 => {} // None
            1 => {
                // Sub
                for i in bytes_per_pixel..cur.len() {
                    cur[i] = cur[i].wrapping_add(cur[i - bytes_per_pixel]);
                }
            }
            2 => {
                // Up
                for i in 0..cur.len() {
                    cur[i] = cur[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                // Average
                for i in 0..cur.len() {
                    let a = if i >= bytes_per_pixel {
                        cur[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    let b = prev[i];
                    cur[i] = cur[i].wrapping_add(((a as u16 + b as u16) / 2) as u8);
                }
            }
            4 => {
                // Paeth
                for i in 0..cur.len() {
                    let a = if i >= bytes_per_pixel {
                        cur[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    let b = prev[i];
                    let c = if i >= bytes_per_pixel {
                        prev[i - bytes_per_pixel]
                    } else {
                        0
                    };
                    cur[i] = cur[i].wrapping_add(paeth(a, b, c));
                }
            }
            other => {
                return Err(Error::Parse(format!(
                    "PNG predictor row filter {other} is invalid"
                )))
            }
        }
        prev = cur.clone();
        out.extend_from_slice(&cur);
        if out.len() as u64 > limits.max_stream_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_stream_bytes (decoded)",
                max: limits.max_stream_bytes,
            });
        }
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}
