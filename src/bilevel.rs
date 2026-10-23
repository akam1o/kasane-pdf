//! Bilevel image filters: `/JBIG2Decode` (ISO 32000-1 §7.4.7, ITU-T
//! T.88) and `/CCITTFaxDecode` (§7.4.6, ITU-T T.4/T.6), decoded through
//! the pure-Rust `hayro-jbig2` / `hayro-ccitt` crates into packed
//! 1-bit-per-component rows that the image pipeline then treats like
//! any other 1 bpc `DeviceGray` or stencil-mask data.
//!
//! Output convention (what the PDF filters are specified to deliver):
//! a **0 bit is black**. JBIG2 itself codes black as 1, so the filter
//! inverts (§7.4.7: "1 bits shall be interpreted as black" *in the
//! encoded data*, and the filter "shall produce" `DeviceGray`-style
//! samples); CCITT follows `/BlackIs1` (default `false` → 0 = black).
//! `/Decode [1 0]` and `/ImageMask` then apply to these samples exactly
//! as they would to raw 1 bpc data.
//!
//! Safety: both crates are `#![forbid(unsafe_code)]` and bound every
//! internal bitmap to 65 535 × 65 535; on top of that the page bitmap
//! here is allocated only after `/Width` × `/Height` pass the
//! `max_image_dimension` / `max_image_total_samples` / `max_image_bytes`
//! limits, the JBIG2 page size must equal the dictionary's, every pixel
//! write is clipped to the buffer, and a stream that does not deliver
//! every row fails rather than yielding a partial image.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};

/// Packed MSB-first 1 bpc rows, `(width + 7) / 8` bytes each, initially
/// all 1 (white); decoders clear bits for black pixels. Writes beyond
/// the row or past the last row are ignored, never out of bounds.
struct PackedRows {
    width: u32,
    height: u32,
    row_bytes: usize,
    data: Vec<u8>,
    x: u32,
    y: u32,
}

impl PackedRows {
    fn new(width: u32, height: u32, limits: &Limits) -> Result<Self, Error> {
        if width == 0
            || height == 0
            || width > limits.max_image_dimension
            || height > limits.max_image_dimension
        {
            return Err(Error::LimitExceeded {
                limit: "max_image_dimension",
                max: limits.max_image_dimension as u64,
            });
        }
        if width as u64 * height as u64 > limits.max_image_total_samples {
            return Err(Error::LimitExceeded {
                limit: "max_image_total_samples",
                max: limits.max_image_total_samples,
            });
        }
        let row_bytes = (width as usize).div_ceil(8);
        let total = row_bytes as u64 * height as u64;
        if total > limits.max_image_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_image_bytes",
                max: limits.max_image_bytes,
            });
        }
        Ok(Self {
            width,
            height,
            row_bytes,
            data: vec![0xFF; total as usize],
            x: 0,
            y: 0,
        })
    }

    /// Clear `count` bits starting at the cursor (black run), clipped
    /// to the current row, then advance.
    fn black(&mut self, count: u32) {
        if self.y < self.height && self.x < self.width {
            let x0 = self.x as usize;
            let x1 = (self.x.saturating_add(count)).min(self.width) as usize;
            let row = &mut self.data[self.y as usize * self.row_bytes..][..self.row_bytes];
            let (b0, b1) = (x0 / 8, x1 / 8);
            if b0 == b1 {
                let mask = (0xFFu16 >> (x0 % 8)) as u8 & !((0xFFu16 >> (x1 % 8)) as u8);
                row[b0] &= !mask;
            } else {
                row[b0] &= !((0xFFu16 >> (x0 % 8)) as u8);
                for b in row.iter_mut().take(b1).skip(b0 + 1) {
                    *b = 0;
                }
                if b1 < self.row_bytes {
                    row[b1] &= (0xFFu16 >> (x1 % 8)) as u8;
                }
            }
        }
        self.x = self.x.saturating_add(count);
    }

    fn white(&mut self, count: u32) {
        self.x = self.x.saturating_add(count);
    }

    fn next_line(&mut self) {
        self.y = self.y.saturating_add(1);
        self.x = 0;
    }
}

impl hayro_jbig2::Decoder for PackedRows {
    fn push_pixel(&mut self, black: bool) {
        if black {
            self.black(1);
        } else {
            self.white(1);
        }
    }
    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let n = chunk_count.saturating_mul(8);
        if black {
            self.black(n);
        } else {
            self.white(n);
        }
    }
    fn next_line(&mut self) {
        PackedRows::next_line(self);
    }
}

impl hayro_ccitt::Decoder for PackedRows {
    fn push_pixels(&mut self, white: bool, count: u32) {
        if white {
            self.white(count);
        } else {
            self.black(count);
        }
    }
    fn next_line(&mut self) {
        PackedRows::next_line(self);
    }
}

/// `/JBIG2Decode`: `data` is the PDF-embedded segment sequence (Annex
/// D.3 of T.88), `globals` the decoded `/JBIG2Globals` stream. Returns
/// `width × height` packed rows with 0 = black.
pub(crate) fn jbig2_decode(
    data: &[u8],
    globals: Option<&[u8]>,
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    // §7.4.7 says the embedded stream carries segments only, yet some
    // producers prepend the T.88 Annex D.4 file header (`97 4A 42 32 0D
    // 0A 1A 0A`). Its signature is unambiguous, so such a stream is read
    // in the file organisation instead of being refused as a bad segment.
    const FILE_HEADER: &[u8] = b"\x97JB2\r\n\x1a\n";
    let image = if data.starts_with(FILE_HEADER) {
        if globals.is_some() {
            return Err(Error::Structure(
                "JBIG2Decode: stream has a file header and /JBIG2Globals (the globals cannot be placed)".into(),
            ));
        }
        hayro_jbig2::Image::new(data)
    } else {
        hayro_jbig2::Image::new_embedded(data, globals)
    }
    .map_err(|e| Error::Parse(format!("JBIG2Decode: {e}")))?;
    if image.width() != width || image.height() != height {
        return Err(Error::Structure(format!(
            "JBIG2 page is {}x{} but the image dictionary says {width}x{height}",
            image.width(),
            image.height()
        )));
    }
    let mut rows = PackedRows::new(width, height, limits)?;
    image
        .decode(&mut rows)
        .map_err(|e| Error::Parse(format!("JBIG2Decode: {e}")))?;
    if rows.y < height {
        return Err(Error::Parse(format!(
            "JBIG2Decode: page delivered {} of {height} rows",
            rows.y
        )));
    }
    Ok(rows.data)
}

/// `/CCITTFaxDecode` parameters (Table 11) with their defaults.
struct CcittParms {
    k: i64,
    columns: u32,
    rows: u32,
    black_is_1: bool,
    end_of_line: bool,
    byte_align: bool,
    end_of_block: bool,
}

fn ccitt_parms(parms: Option<&Dict>) -> Result<CcittParms, Error> {
    let int = |key: &[u8], default: i64| -> Result<i64, Error> {
        match parms.and_then(|p| p.get(key)) {
            None | Some(Object::Null) => Ok(default),
            Some(o) => o.as_i64().ok_or_else(|| {
                Error::Structure(format!(
                    "CCITTFaxDecode /{} is not an integer",
                    String::from_utf8_lossy(key)
                ))
            }),
        }
    };
    let flag = |key: &[u8], default: bool| -> Result<bool, Error> {
        match parms.and_then(|p| p.get(key)) {
            None | Some(Object::Null) => Ok(default),
            Some(o) => o.as_bool().ok_or_else(|| {
                Error::Structure(format!(
                    "CCITTFaxDecode /{} is not a boolean",
                    String::from_utf8_lossy(key)
                ))
            }),
        }
    };
    let columns = int(b"Columns", 1728)?;
    let rows = int(b"Rows", 0)?;
    let damaged = int(b"DamagedRowsBeforeError", 0)?;
    if columns <= 0 || columns > u32::MAX as i64 {
        return Err(Error::Structure(format!(
            "CCITTFaxDecode /Columns {columns} is out of range"
        )));
    }
    if rows < 0 || rows > u32::MAX as i64 {
        return Err(Error::Structure(format!(
            "CCITTFaxDecode /Rows {rows} is out of range"
        )));
    }
    if damaged < 0 {
        return Err(Error::Structure(format!(
            "CCITTFaxDecode /DamagedRowsBeforeError {damaged} is negative"
        )));
    }
    // `/DamagedRowsBeforeError` is a *tolerance*: how many damaged rows
    // a decoder may repair before it must report an error. This decoder
    // repairs none, so every damaged row is reported — never more
    // lenient than the file asked for.
    Ok(CcittParms {
        k: int(b"K", 0)?,
        columns: columns as u32,
        rows: rows as u32,
        black_is_1: flag(b"BlackIs1", false)?,
        end_of_line: flag(b"EndOfLine", false)?,
        byte_align: flag(b"EncodedByteAlign", false)?,
        end_of_block: flag(b"EndOfBlock", true)?,
    })
}

/// `/CCITTFaxDecode` into `width × height` packed rows following
/// `/BlackIs1` (default: 0 = black). `/Columns` must equal the image
/// width — the filter's rows *are* the image's rows — and the stream
/// must deliver every row; data after the last needed row is ignored.
pub(crate) fn ccitt_decode(
    data: &[u8],
    parms: Option<&Dict>,
    width: u32,
    height: u32,
    limits: &Limits,
) -> Result<Vec<u8>, Error> {
    let p = ccitt_parms(parms)?;
    if p.columns != width {
        return Err(Error::Structure(format!(
            "CCITTFaxDecode /Columns {} does not match the image /Width {width}",
            p.columns
        )));
    }
    let encoding = match p.k {
        k if k < 0 => hayro_ccitt::EncodingMode::Group4,
        0 => hayro_ccitt::EncodingMode::Group3_1D,
        k => hayro_ccitt::EncodingMode::Group3_2D { k: k as u32 },
    };
    let rows_to_decode = if p.rows > 0 { p.rows } else { height };
    let mut rows = PackedRows::new(width, height, limits)?;
    let mut ctx = hayro_ccitt::DecoderContext::new(hayro_ccitt::DecodeSettings {
        columns: width,
        rows: rows_to_decode,
        end_of_block: p.end_of_block,
        end_of_line: p.end_of_line,
        rows_are_byte_aligned: p.byte_align,
        encoding,
        invert_black: false,
    });
    let result = hayro_ccitt::decode(data, &mut rows, &mut ctx);
    // A fault after every row the image needs has been delivered (e.g.
    // garbage past the EOFB, or /Rows larger than /Height) changes no
    // pixel; a fault before that would leave rows missing.
    if let Err(e) = result {
        if rows.y < height {
            return Err(Error::Parse(format!(
                "CCITTFaxDecode: {e} after {} of {height} rows",
                rows.y
            )));
        }
    }
    if rows.y < height {
        return Err(Error::Parse(format!(
            "CCITTFaxDecode: stream ended after {} of {height} rows",
            rows.y
        )));
    }
    if p.black_is_1 {
        for b in rows.data.iter_mut() {
            *b = !*b;
        }
    }
    Ok(rows.data)
}
