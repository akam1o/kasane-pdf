//! Stream filter decoding (ISO 32000-1:2008, §7.4).
//!
//! Supported here: `/FlateDecode` (zlib/DEFLATE via `flate2`/`miniz_oxide`),
//! `/LZWDecode` (§7.4.4, `/EarlyChange`), `/ASCII85Decode` (§7.4.3),
//! `/ASCIIHexDecode` (§7.4.2) and `/RunLengthDecode` (§7.4.5). PNG
//! predictors (`/Predictor` 10–15) and the TIFF predictor (2) apply to
//! `/FlateDecode` and `/LZWDecode` output.
//!
//! Image-only filters — `/DCTDecode`, `/JBIG2Decode`, `/CCITTFaxDecode`
//! (and `/JPXDecode`, which is not implemented) — produce samples, not
//! bytes, so they are decoded by the image pipeline (`render.rs`,
//! `bilevel.rs`) and refused here with [`Error::Unsupported`], as are
//! `/Crypt` and unknown names. `/DecodeParms` entries for filters we do
//! not implement are never inspected — we refuse before that.
//!
//! Multiple filters are applied in order (`/Filter [/A /B]` means data
//! was encoded first by `/B` then by `/A`, so decoding applies `/A`
//! first). `/DecodeParms` is matched positionally; a `null` entry means
//! "no parameters" for that filter. Every filter's output is capped at
//! `max_stream_bytes` while it is produced, so a hostile stream cannot
//! force an allocation beyond the limit.
//!
//! Termination: a missing EOD marker (`~>`, `>`, LZW code 257,
//! RunLength byte 128) at the very end of the data is accepted when the
//! bytes decoded so far are unambiguous — common producer sloppiness —
//! but a code or run cut off mid-way, or any byte outside the filter's
//! alphabet, fails the whole decode.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use crate::warning::Warning;
use flate2::{Decompress, FlushDecompress, Status};

/// Decode `data` according to the stream dictionary's `/Filter` and
/// `/DecodeParms`, bounded by `limits`.
///
/// `raw_len` is the encoded length (for diagnostics). Returns the decoded
/// bytes. On any failure the whole decode fails — partial output is
/// never returned.
pub(crate) fn decode_stream(
    dict: &Dict,
    data: &[u8],
    limits: &Limits,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<u8>, Error> {
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
        buf = apply_one_filter(fname, parm, &buf, limits, warnings)?;
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
pub(crate) fn normalize_filters(obj: Option<&Object>) -> Result<Vec<Vec<u8>>, Error> {
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
pub(crate) fn normalize_decode_parms(
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
        // `[]` is written by some producers for "no parameters at all".
        Some(Object::Array(a)) if a.is_empty() => Ok(vec![None; n_filters]),
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
pub(crate) fn apply_one_filter(
    name: &[u8],
    parms: Option<&Dict>,
    buf: &[u8],
    limits: &Limits,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<u8>, Error> {
    match name {
        b"FlateDecode" | b"Fl" => flate_decode(buf, parms, limits, warnings),
        b"LZWDecode" | b"LZW" => lzw_decode(buf, parms, limits),
        b"ASCII85Decode" | b"A85" => ascii85_decode(buf, limits),
        b"ASCIIHexDecode" | b"AHx" => asciihex_decode(buf, limits),
        b"RunLengthDecode" | b"RL" => runlength_decode(buf, limits),
        b"DCTDecode" | b"DCT" | b"JBIG2Decode" | b"CCITTFaxDecode" | b"CCF" | b"JPXDecode" => {
            Err(Error::Unsupported {
                feature: "stream filter",
                detail: format!(
                    "/{} outside an image XObject (image-only filter)",
                    String::from_utf8_lossy(name)
                ),
            })
        }
        other => Err(Error::Unsupported {
            feature: "stream filter",
            detail: format!("/{}", String::from_utf8_lossy(other)),
        }),
    }
}

/// `true` for the filters that yield image samples rather than bytes
/// and are therefore decoded by the image pipeline, not here.
pub(crate) fn is_image_filter(name: &[u8]) -> bool {
    matches!(
        name,
        b"DCTDecode" | b"DCT" | b"JBIG2Decode" | b"CCITTFaxDecode" | b"CCF" | b"JPXDecode"
    )
}

fn over_cap(len: usize, limits: &Limits) -> Result<(), Error> {
    if len as u64 > limits.max_stream_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_stream_bytes (decoded)",
            max: limits.max_stream_bytes,
        });
    }
    Ok(())
}

fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

/// §7.4.2: pairs of hex digits, white space ignored, `>` is EOD; an odd
/// final digit is padded with 0.
fn asciihex_decode(buf: &[u8], limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(buf.len() / 2);
    let mut hi: Option<u8> = None;
    for (i, &b) in buf.iter().enumerate() {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b'>' => break,
            _ if is_pdf_whitespace(b) => continue,
            _ => {
                return Err(Error::Parse(format!(
                    "ASCIIHexDecode: byte 0x{b:02x} at offset {i} is not a hex digit"
                )))
            }
        };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => {
                over_cap(out.len() + 1, limits)?;
                out.push((h << 4) | v);
            }
        }
    }
    if let Some(h) = hi {
        over_cap(out.len() + 1, limits)?;
        out.push(h << 4);
    }
    Ok(out)
}

/// §7.4.3: groups of five characters `!`..`u` encode four bytes
/// (base 85, big-endian); `z` is four zero bytes; `~>` is EOD; a final
/// partial group of n characters yields n − 1 bytes. A leading `<~`
/// (not part of the PDF filter, but written by some producers) is
/// skipped.
fn ascii85_decode(buf: &[u8], limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(buf.len() * 4 / 5 + 4);
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    // Skip leading white space and an optional `<~`.
    while i < buf.len() && is_pdf_whitespace(buf[i]) {
        i += 1;
    }
    if buf[i..].starts_with(b"<~") {
        i += 2;
    }
    let flush = |group: &[u8; 5], n: usize, out: &mut Vec<u8>| -> Result<(), Error> {
        // Pad the partial group with `u` (84) per the spec.
        let mut v: u32 = 0;
        for (k, &g) in group.iter().enumerate() {
            let d = if k < n { g } else { 84 } as u32;
            v = v
                .checked_mul(85)
                .and_then(|v| v.checked_add(d))
                .ok_or_else(|| Error::Parse("ASCII85Decode: group value exceeds 2^32".into()))?;
        }
        over_cap(out.len() + n - 1, limits)?;
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
        Ok(())
    };
    while i < buf.len() {
        let b = buf[i];
        i += 1;
        match b {
            b'~' => {
                if buf.get(i) != Some(&b'>') {
                    return Err(Error::Parse(format!(
                        "ASCII85Decode: `~` at offset {} is not followed by `>`",
                        i - 1
                    )));
                }
                break;
            }
            b'z' if n == 0 => {
                over_cap(out.len() + 4, limits)?;
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            b'z' => {
                return Err(Error::Parse(format!(
                    "ASCII85Decode: `z` inside a group at offset {}",
                    i - 1
                )))
            }
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    flush(&group, 5, &mut out)?;
                    n = 0;
                }
            }
            _ if is_pdf_whitespace(b) => {}
            _ => {
                return Err(Error::Parse(format!(
                    "ASCII85Decode: byte 0x{b:02x} at offset {} is outside `!`..`u`",
                    i - 1
                )))
            }
        }
    }
    match n {
        0 => {}
        1 => {
            return Err(Error::Parse(
                "ASCII85Decode: final group has a single character (encodes no byte)".into(),
            ))
        }
        _ => flush(&group, n, &mut out)?,
    }
    Ok(out)
}

/// §7.4.5: a length byte L, then either L + 1 literal bytes (L ≤ 127)
/// or one byte repeated 257 − L times (L ≥ 129); 128 is EOD.
fn runlength_decode(buf: &[u8], limits: &Limits) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0usize;
    while i < buf.len() {
        let l = buf[i] as usize;
        i += 1;
        if l == 128 {
            return Ok(out);
        }
        if l < 128 {
            let end = i + l + 1;
            if end > buf.len() {
                return Err(Error::Parse(format!(
                    "RunLengthDecode: literal run of {} bytes at offset {} overruns the data",
                    l + 1,
                    i - 1
                )));
            }
            over_cap(out.len() + l + 1, limits)?;
            out.extend_from_slice(&buf[i..end]);
            i = end;
        } else {
            let Some(&b) = buf.get(i) else {
                return Err(Error::Parse(format!(
                    "RunLengthDecode: repeat run at offset {} has no byte to repeat",
                    i - 1
                )));
            };
            i += 1;
            let count = 257 - l;
            over_cap(out.len() + count, limits)?;
            out.extend(std::iter::repeat_n(b, count));
        }
    }
    // No EOD byte: the data ended on a run boundary, which is unambiguous.
    Ok(out)
}

/// §7.4.4: variable-length LZW (9–12 bit codes, MSB first), clear = 256,
/// EOD = 257. `/EarlyChange` (default 1) makes the code width grow one
/// code early. Predictors apply afterwards exactly as for Flate.
fn lzw_decode(buf: &[u8], parms: Option<&Dict>, limits: &Limits) -> Result<Vec<u8>, Error> {
    let early = match parms.and_then(|p| p.get(b"EarlyChange".as_slice())) {
        None | Some(Object::Null) => 1,
        Some(o) => match o.as_i64() {
            Some(v @ (0 | 1)) => v,
            Some(v) => {
                return Err(Error::Structure(format!(
                    "LZWDecode /EarlyChange {v} (must be 0 or 1)"
                )))
            }
            None => {
                return Err(Error::Structure(
                    "LZWDecode /EarlyChange is not an integer".into(),
                ))
            }
        },
    } as usize;

    // Dictionary entries are (prefix code, final byte); entry lengths
    // let a code be emitted by walking the chain backwards into `out`
    // without materialising every string.
    let mut prefix: Vec<u16> = Vec::with_capacity(4096);
    let mut suffix: Vec<u8> = Vec::with_capacity(4096);
    let mut length: Vec<u32> = Vec::with_capacity(4096);
    let reset = |prefix: &mut Vec<u16>, suffix: &mut Vec<u8>, length: &mut Vec<u32>| {
        prefix.clear();
        suffix.clear();
        length.clear();
        for i in 0..258u16 {
            prefix.push(u16::MAX);
            suffix.push(i as u8);
            length.push(1);
        }
    };
    reset(&mut prefix, &mut suffix, &mut length);

    let mut out: Vec<u8> = Vec::with_capacity(buf.len() * 3);
    let mut width = 9u32;
    let mut prev: Option<u16> = None;
    let mut bitpos: usize = 0;
    let total_bits = buf.len() * 8;
    loop {
        if bitpos + width as usize > total_bits {
            // Fewer than `width` bits remain: accept pure zero padding
            // (the producer forgot EOD), reject anything else.
            let rest = &buf[bitpos / 8..];
            let pad_ok = rest.iter().enumerate().all(|(k, &b)| {
                if k == 0 {
                    b & (0xFF >> (bitpos % 8)) == 0
                } else {
                    b == 0
                }
            });
            if pad_ok {
                break;
            }
            return Err(Error::Parse(format!(
                "LZWDecode: data ends inside a {width}-bit code at bit {bitpos} (no EOD)"
            )));
        }
        let mut code: u32 = 0;
        for _ in 0..width {
            let byte = buf[bitpos / 8];
            let bit = (byte >> (7 - (bitpos % 8))) & 1;
            code = (code << 1) | bit as u32;
            bitpos += 1;
        }
        let code = code as u16;
        if code == 256 {
            reset(&mut prefix, &mut suffix, &mut length);
            width = 9;
            prev = None;
            continue;
        }
        if code == 257 {
            break;
        }
        let next = prefix.len() as u16; // the code this step defines
        let (emit_from, emit_len) = if (code as usize) < prefix.len() {
            (code, length[code as usize])
        } else if code == next && prev.is_some() {
            // KwKwK case: prev string + its own first byte.
            let p = prev.unwrap();
            (p, length[p as usize] + 1)
        } else {
            return Err(Error::Parse(format!(
                "LZWDecode: code {code} at bit {} is not in the table (next free is {next})",
                bitpos - width as usize
            )));
        };
        over_cap(out.len() + emit_len as usize, limits)?;
        let start = out.len();
        out.resize(start + emit_len as usize, 0);
        // Fill backwards along the prefix chain.
        let mut c = emit_from;
        let mut k = start + length[emit_from as usize] as usize;
        let kwkwk = emit_len > length[emit_from as usize];
        while c != u16::MAX {
            k -= 1;
            out[k] = suffix[c as usize];
            c = prefix[c as usize];
        }
        if kwkwk {
            out[start + emit_len as usize - 1] = out[start];
        }
        if let Some(p) = prev {
            if prefix.len() < 4096 {
                prefix.push(p);
                suffix.push(out[start]);
                length.push(length[p as usize] + 1);
            }
        }
        prev = Some(code);
        // Width switches when the *next* code to be defined reaches
        // 512/1024/2048 (minus one with EarlyChange).
        width = match prefix.len() + early {
            d if d >= 2048 => 12,
            d if d >= 1024 => 11,
            d if d >= 512 => 10,
            _ => 9,
        };
    }
    apply_predictor(out, parms, limits)
}

/// zlib/DEFLATE with optional predictor post-processing.
fn flate_decode(
    buf: &[u8],
    parms: Option<&Dict>,
    limits: &Limits,
    warnings: &mut Vec<Warning>,
) -> Result<Vec<u8>, Error> {
    // Driven through the low-level decompressor so the three outcomes
    // stay distinguishable: a complete zlib stream (`StreamEnd`), data
    // that ends inside a deflate block (refused), and data that ends
    // on a block boundary after a sync flush with the final block and
    // checksum missing — a deviation that several producers ship and
    // every major reader accepts; accepted here and recorded as a
    // `Warning::UnterminatedFlateStream`, never silently.
    let mut d = Decompress::new(true);
    let mut out: Vec<u8> = Vec::new();
    let cap = limits.max_stream_bytes as usize;
    let mut status;
    loop {
        if out.capacity() - out.len() < 32 * 1024 {
            out.reserve(64 * 1024);
        }
        let before_out = out.len();
        let before_in = d.total_in();
        let rest = &buf[(d.total_in() as usize).min(buf.len())..];
        status = d
            .decompress_vec(rest, &mut out, FlushDecompress::None)
            .map_err(|_| Error::Parse("FlateDecode failed: corrupt deflate stream".into()))?;
        if out.len() > cap {
            return Err(Error::LimitExceeded {
                limit: "max_stream_bytes (decoded)",
                max: limits.max_stream_bytes,
            });
        }
        if status == Status::StreamEnd {
            break;
        }
        if out.len() == before_out && d.total_in() == before_in {
            // No progress: the input is exhausted without a final block.
            break;
        }
    }
    if status != Status::StreamEnd {
        // Z_SYNC_FLUSH ends with an empty stored block, `00 00 FF FF`;
        // when the data ends exactly there every earlier block was
        // complete and `out` is exact. Anything else ended mid-block.
        const SYNC_FLUSH: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF];
        let consumed_all = d.total_in() as usize >= buf.len();
        if consumed_all && buf.ends_with(&SYNC_FLUSH) {
            warnings.push(Warning::UnterminatedFlateStream {
                decoded_bytes: out.len(),
            });
        } else {
            return Err(Error::Parse(
                "FlateDecode failed: incomplete deflate stream".into(),
            ));
        }
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
    // Valid predictors: 2 (TIFF) and 10–15 (PNG).  3–9 are reserved.
    if predictor != 2 && !(10..=15).contains(&predictor) {
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
pub(crate) fn apply_predictor(
    data: Vec<u8>,
    parms: Option<&Dict>,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
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
    if !data.len().is_multiple_of(stride) {
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
