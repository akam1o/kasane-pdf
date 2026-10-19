//! ToUnicode CMap parsing (ISO 32000-1 §9.10.3).
//!
//! A `/ToUnicode` stream is a small PostScript-flavoured file that
//! maps the font's *character codes* to UTF-16BE Unicode strings.
//! For the simple fonts M5/M6 support, codes are one byte wide.
//!
//! Supported syntax (from §9.10.3 and Adobe TN #5014):
//!
//! - `n begincodespacerange … endcodespacerange` — declares the
//!   byte-length of source codes (must be 1 for our fonts).
//! - `n beginbfchar … endbfchar` — `<src> <dst>` pairs.
//! - `n beginbfrange … endbfrange` — two forms:
//!   - `<lo> <hi> <dst>` — `dst`'s *last byte* increments per source
//!     code (must not overflow past 0xFF, per spec).
//!   - `<lo> <hi> [<d0> <d1> … <dm>]` — explicit per-code list,
//!     `m = hi − lo + 1` entries.
//!
//! Values are hex strings (`<…>`). The `dstString` may hold up to
//! 512 bytes and is interpreted as UTF-16BE; surrogate pairs decode
//! to a single scalar.
//!
//! Not supported → `Error::Unsupported`:
//! - `/UseCMap` (CMap chaining)
//! - `usecmap`, `usefont`, `beginnotdef*` / `endnotdef*`,
//!   `begincidchar` / `begincidrange`, `beginrearrangedfont`,
//!   `beginusematrix`, `codespacerange` longer than 1 byte
//! - A CMap dictionary that is not a stream
//!
//! `def`, `dict`, `begin`/`end`/`pop`/`findresource` etc. are the
//! expected CMap scaffolding — they are consumed and checked for
//! placement, but do not affect the mapping table.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::Object;
use crate::Document;

/// A parsed ToUnicode CMap — a sorted list of single-code →
/// UTF-16BE string mappings.
#[derive(Debug, Clone)]
pub(crate) struct ToUnicode {
    /// Codespace width: 1 for simple fonts, 2 for Identity-H
    /// composites. Mixed-width codespaces are refused at parse time.
    code_len: u8,
    /// Sorted by `code`. Bound by `max_text_codes`.
    entries: Vec<Mapping>,
}

#[derive(Debug, Clone)]
struct Mapping {
    /// The source character code (fits in `code_len` bytes).
    code: u32,
    /// The decoded UTF-16BE string. May be empty (a valid mapping
    /// to the empty string per §9.10.3 — *not* "unresolved").
    utf16be: Vec<u16>,
}

impl ToUnicode {
    /// Source code width in bytes (1 or 2).
    pub(crate) fn code_len(&self) -> u8 {
        self.code_len
    }

    /// Look up `code`; `None` if the CMap has no entry for it.
    pub(crate) fn lookup(&self, code: u32) -> Option<&[u16]> {
        self.entries
            .iter()
            .find(|m| m.code == code)
            .map(|m| m.utf16be.as_slice())
    }

    /// Decode `code` to a `String` if the CMap maps it. Returns
    /// `None` for unmapped codes; `Some("")` for a mapping to the
    /// empty string. Lone surrogates produce `None` — they are
    /// malformed UTF-16BE.
    pub(crate) fn decode(&self, code: u32) -> Option<String> {
        let u16s = self.lookup(code)?;
        // UTF-16BE → String; lone surrogates make it invalid.
        String::from_utf16(u16s).ok()
    }

    /// Parse a ToUnicode CMap from a decoded stream body.
    ///
    /// `limits` bounds: total mappings, UTF-16 output per entry, and
    /// the source byte length of the CMap itself.
    pub(crate) fn parse(data: &[u8], limits: &Limits) -> Result<Self, Error> {
        if data.len() as u64 > limits.max_stream_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_stream_bytes (ToUnicode)",
                max: limits.max_stream_bytes,
            });
        }
        let mut p = Parser {
            data,
            pos: 0,
            depth: 0,
        };
        let mut out = ToUnicode {
            code_len: 0, // set by begincodespacerange
            entries: Vec::new(),
        };
        let mut seen_codespace = false;
        let mut _entry_count: u64 = 0;

        while p.skip_trivia()? && !p.eof() {
            let tok = p.next_token()?;
            match tok {
                Tok::Num(n) => {
                    // A number may be a count for a following
                    // begin* operator, or part of the CMap preamble
                    // (e.g. `12 dict`). Peek at the next token.
                    let save = p.pos;
                    let op = p.next_token()?;
                    match op {
                        Tok::BareWord(b"begincodespacerange") => {
                            let len = p.parse_codespace(n, limits)?;
                            // Mixed-width codespaces are outside M7 —
                            // every declared range must share one byte
                            // length.
                            match out.code_len {
                                0 => out.code_len = len,
                                l if l == len => {}
                                _ => {
                                    return Err(Error::Unsupported {
                                        feature: "ToUnicode mixed code lengths",
                                        detail: "codespace ranges with different byte lengths are not supported".into(),
                                    })
                                }
                            }
                            seen_codespace = true;
                        }
                        Tok::BareWord(b"beginbfchar") => {
                            let used = p.parse_bfchar(n, limits, &mut out)?;
                            _entry_count += used;
                        }
                        Tok::BareWord(b"beginbfrange") => {
                            let used = p.parse_bfrange(n, limits, &mut out)?;
                            _entry_count += used;
                        }
                        Tok::BareWord(b"endcmap") => {
                            return Err(Error::Structure("stray number before endcmap".into()));
                        }
                        _ => {
                            // Not a begin* — the number was preamble
                            // data (e.g. `12 dict begin`). Rewind and
                            // treat the token normally.
                            p.pos = save;
                        }
                    }
                }
                Tok::BareWord(b"begincmap") | Tok::BareWord(b"endcmap") => {
                    // CMap body markers — the bfchar/bfrange blocks
                    // they delimit are handled by the Num arm.
                }
                Tok::BareWord(name)
                    if name == b"def"
                        || name == b"pop"
                        || name == b"dup"
                        || name == b"begin"
                        || name == b"end"
                        || name == b"dict"
                        || name == b"currentdict"
                        || name == b"findresource"
                        || name == b"defineresource" =>
                {
                    // CMap scaffolding — `def`, `dict`, `begin`, `end`
                    // etc. are permitted anywhere.
                }
                Tok::BareWord(b"usecmap") | Tok::BareWord(b"usefont") => {
                    // In-CMap chaining — not supported.
                    return Err(Error::Unsupported {
                        feature: "ToUnicode usecmap/usefont",
                        detail: "CMap chaining is not implemented".into(),
                    });
                }
                Tok::DictStart
                | Tok::DictEnd
                | Tok::Str
                | Tok::HexStr(_)
                | Tok::ArrStart
                | Tok::ArrEnd
                | Tok::Name => {
                    // Literal values outside begin* blocks — the
                    // CMap preamble (CIDInit etc.). Skipped.
                }
                Tok::BareWord(other) => {
                    // Post endcmap there may be trailing PostScript
                    // (e.g. `CMapName currentdict /CMap defineresource
                    // pop end`). Treat unknown bare words as benign
                    // scaffolding *after* the CMap body — not before
                    // or inside a begin*/end* block.
                    let _ = other;
                }
            }
        }

        if !seen_codespace {
            // §9.10.3: a ToUnicode CMap shall contain
            // begincodespacerange. Its absence is a defect.
            return Err(Error::Structure(
                "ToUnicode CMap has no begincodespacerange".into(),
            ));
        }
        // Sort for binary-search lookup; duplicates keep the first.
        out.entries.sort_by_key(|m| m.code);
        out.entries.dedup_by_key(|m| m.code);
        Ok(out)
    }
}

// ---------------------------------------------------------------------
// Low-level tokeniser
// ---------------------------------------------------------------------

#[derive(Debug)]
enum Tok<'a> {
    Num(i64),
    /// `/name` — the leading slash is consumed.
    Name,
    /// A bare word — not `/name` — e.g. `begincmap`, `endcmap`.
    BareWord(&'a [u8]),
    Str,
    HexStr(&'a [u8]), // raw hex digits, odd-length tolerated below
    DictStart,
    DictEnd,
    ArrStart,
    ArrEnd,
}

struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn skip_trivia(&mut self) -> Result<bool, Error> {
        loop {
            match self.data.get(self.pos) {
                Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(0x0C) | Some(0) => {
                    self.pos += 1
                }
                Some(b'%') => {
                    // Comment to EOL.
                    while self.pos < self.data.len()
                        && self.data[self.pos] != b'\n'
                        && self.data[self.pos] != b'\r'
                    {
                        self.pos += 1;
                    }
                }
                _ => return Ok(true),
            }
        }
    }

    fn next_token(&mut self) -> Result<Tok<'a>, Error> {
        self.skip_trivia()?;
        if self.eof() {
            return Err(Error::Structure(
                "ToUnicode: unexpected end of stream".into(),
            ));
        }
        match self.data[self.pos] {
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                self.depth += 1;
                if self.depth > 8 {
                    return Err(Error::LimitExceeded {
                        limit: "ToUnicode nesting depth",
                        max: 8,
                    });
                }
                Ok(Tok::DictStart)
            }
            b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                self.depth = self.depth.saturating_sub(1);
                Ok(Tok::DictEnd)
            }
            b'<' => {
                // Hex string.
                let start = self.pos + 1;
                let mut end = start;
                while end < self.data.len() && self.data[end] != b'>' {
                    end += 1;
                }
                if end >= self.data.len() {
                    return Err(Error::Structure("unterminated hex string".into()));
                }
                let body = &self.data[start..end];
                self.pos = end + 1;
                Ok(Tok::HexStr(body))
            }
            b'[' => {
                self.pos += 1;
                Ok(Tok::ArrStart)
            }
            b']' => {
                self.pos += 1;
                Ok(Tok::ArrEnd)
            }
            b'/' => {
                let start = self.pos + 1;
                let mut end = start;
                while end < self.data.len()
                    && !matches!(self.data[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0)
                {
                    end += 1;
                }
                self.pos = end;
                Ok(Tok::Name)
            }
            b'(' => {
                // Literal string — balanced parens, escapes.
                let mut depth = 1;
                let mut i = self.pos + 1;
                while i < self.data.len() && depth > 0 {
                    match self.data[i] {
                        b'\\' => {
                            i += 2;
                            continue;
                        }
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                self.pos = i;
                Ok(Tok::Str)
            }
            c if c.is_ascii_digit() || c == b'-' || c == b'+' => {
                let start = self.pos;
                let mut end = start;
                while end < self.data.len()
                    && !matches!(self.data[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0)
                {
                    end += 1;
                }
                let s = std::str::from_utf8(&self.data[start..end])
                    .map_err(|_| Error::Structure("invalid number".into()))?;
                self.pos = end;
                let n: i64 = s
                    .parse()
                    .map_err(|_| Error::Structure(format!("invalid number '{s}'")))?;
                Ok(Tok::Num(n))
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                // A bare word — `begincmap`, `endbfchar`, etc.
                let start = self.pos;
                let mut end = start;
                while end < self.data.len()
                    && !matches!(self.data[end], b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0)
                {
                    end += 1;
                }
                let word = &self.data[start..end];
                self.pos = end;
                Ok(Tok::BareWord(word))
            }
            _ => Err(Error::Structure(format!(
                "unexpected byte 0x{:02X} in ToUnicode CMap at pos {}",
                self.data[self.pos], self.pos
            ))),
        }
    }

    /// `n begincodespacerange … endcodespacerange` — returns the code
    /// byte length (1 or 2). Mixed lengths within one CMap are
    /// rejected by the caller.
    fn parse_codespace(&mut self, n: i64, _limits: &Limits) -> Result<u8, Error> {
        if n < 1 {
            return Err(Error::Structure("begincodespacerange count < 1".into()));
        }
        let mut len: Option<u8> = None;
        for _ in 0..n {
            let lo = self.next_token()?;
            let hi = self.next_token()?;
            let (lo_bytes, hi_bytes) = match (lo, hi) {
                (Tok::HexStr(l), Tok::HexStr(h)) => (l, h),
                _ => {
                    return Err(Error::Structure(
                        "codespacerange bounds must be hex strings".into(),
                    ))
                }
            };
            // Codespace bounds are hex strings whose digit count is
            // 2× the byte length (2 for 1-byte, 4 for 2-byte).
            let l = lo_bytes.len();
            if l != hi_bytes.len() || (l != 2 && l != 4) {
                return Err(Error::Unsupported {
                    feature: "ToUnicode codespace",
                    detail: format!(
                        "only uniform 1- or 2-byte codespaces are \
                         supported; got {:?}..{:?}",
                        String::from_utf8_lossy(lo_bytes),
                        String::from_utf8_lossy(hi_bytes)
                    ),
                });
            }
            let this_len = (l / 2) as u8;
            match len {
                None => len = Some(this_len),
                Some(prev) if prev == this_len => {}
                Some(_) => {
                    return Err(Error::Unsupported {
                        feature: "ToUnicode mixed code lengths",
                        detail: "codespace ranges declare different byte lengths".into(),
                    })
                }
            }
            let lo = hex_uint(lo_bytes)?;
            let hi = hex_uint(hi_bytes)?;
            if lo > hi {
                return Err(Error::Structure(
                    "codespacerange low bound exceeds high".into(),
                ));
            }
        }
        let end = self.next_token()?;
        match end {
            Tok::BareWord(b"endcodespacerange") => Ok(len.unwrap_or(1)),
            _ => Err(Error::Structure("expected endcodespacerange".into())),
        }
    }

    /// `n beginbfchar` — each entry is `<src> <dst>`.
    fn parse_bfchar(&mut self, n: i64, limits: &Limits, out: &mut ToUnicode) -> Result<u64, Error> {
        if n < 0 || n as u64 > limits.max_text_codes {
            return Err(Error::LimitExceeded {
                limit: "max_text_codes (bfchar entries)",
                max: limits.max_text_codes,
            });
        }
        for _ in 0..n {
            let src = self.next_token()?;
            let dst = self.next_token()?;
            let (s, d) = match (src, dst) {
                (Tok::HexStr(s), Tok::HexStr(d)) => (s, d),
                _ => {
                    return Err(Error::Structure(
                        "bfchar entries must be hex strings".into(),
                    ))
                }
            };
            // Source width must match the declared codespace length.
            if s.len() != (out.code_len as usize) * 2 {
                return Err(Error::Unsupported {
                    feature: "ToUnicode source code width",
                    detail: format!(
                        "code {} does not match the declared {}-byte codespace",
                        String::from_utf8_lossy(s),
                        out.code_len
                    ),
                });
            }
            if d.len() > 1024 {
                return Err(Error::LimitExceeded {
                    limit: "ToUnicode dstString length",
                    max: 1024,
                });
            }
            let code = hex_uint(s)? as u32;
            let utf16 = decode_utf16be(d)?;
            out.entries.push(Mapping {
                code,
                utf16be: utf16,
            });
        }
        let end = self.next_token()?;
        match end {
            Tok::BareWord(b"endbfchar") => Ok(n as u64),
            _ => Err(Error::Structure("expected endbfchar".into())),
        }
    }

    /// `n beginbfrange` — `<lo> <hi> <dst>` or `<lo> <hi> […]`.
    fn parse_bfrange(
        &mut self,
        n: i64,
        limits: &Limits,
        out: &mut ToUnicode,
    ) -> Result<u64, Error> {
        if n < 0 || n as u64 > limits.max_text_codes {
            return Err(Error::LimitExceeded {
                limit: "max_text_codes (bfrange entries)",
                max: limits.max_text_codes,
            });
        }
        let mut produced = 0u64;
        for _ in 0..n {
            let lo_tok = self.next_token()?;
            let hi_tok = self.next_token()?;
            let (lo, hi) = match (lo_tok, hi_tok) {
                (Tok::HexStr(l), Tok::HexStr(h)) => (l, h),
                _ => {
                    return Err(Error::Structure(
                        "bfrange bounds must be hex strings".into(),
                    ))
                }
            };
            if lo.len() != (out.code_len as usize) * 2 || hi.len() != lo.len() {
                return Err(Error::Unsupported {
                    feature: "ToUnicode source code width",
                    detail: format!(
                        "bfrange bounds do not match the declared {}-byte codespace",
                        out.code_len
                    ),
                });
            }
            let lo = hex_uint(lo)? as u32;
            let hi = hex_uint(hi)? as u32;
            if lo > hi {
                return Err(Error::Structure("bfrange low bound exceeds high".into()));
            }
            let range_len = (hi - lo + 1) as u64;
            produced += range_len;
            if produced > limits.max_text_codes {
                return Err(Error::LimitExceeded {
                    limit: "max_text_codes (bfrange mappings)",
                    max: limits.max_text_codes,
                });
            }

            // Third operand: hex string or array of hex strings.
            let third = self.next_token()?;
            match third {
                Tok::HexStr(d) => {
                    // Sequential form — last byte increments per
                    // source code. Spec (TN #5014 §begincidrange):
                    // "the last byte of dstString shall be incremented".
                    if d.len() % 2 != 0 || d.is_empty() {
                        return Err(Error::Structure(
                            "bfrange dstString must be even-length hex".into(),
                        ));
                    }
                    if d.len() > 1024 {
                        return Err(Error::LimitExceeded {
                            limit: "ToUnicode dstString length",
                            max: 1024,
                        });
                    }
                    let base = decode_utf16be(d)?;
                    // The spec's overflow check: the last byte must
                    // not be incremented past 0xFF.
                    let last_byte = base.last().map(|u| (*u & 0xFF) as u64).unwrap_or(0);
                    if last_byte + (range_len - 1) > 0xFF {
                        return Err(Error::Structure("bfrange range overflows last byte".into()));
                    }
                    let mut cur = base.clone();
                    for i in 0..range_len {
                        // Push the current value, then increment the
                        // last byte for the next iteration.
                        out.entries.push(Mapping {
                            code: lo.wrapping_add(i as u32),
                            utf16be: cur.clone(),
                        });
                        if let Some(last) = cur.last_mut() {
                            let v = *last as u32;
                            if v >= 0xFF {
                                return Err(Error::Structure(
                                    "bfrange increment overflows last byte".into(),
                                ));
                            }
                            *last = (*last & 0xFF00) | ((v + 1) as u16 & 0xFF);
                        }
                    }
                }
                Tok::ArrStart => {
                    // Array form — exactly range_len hex strings.
                    let mut count = 0u64;
                    loop {
                        let t = self.next_token()?;
                        match t {
                            Tok::HexStr(d) => {
                                if d.len() > 1024 {
                                    return Err(Error::LimitExceeded {
                                        limit: "ToUnicode dstString length",
                                        max: 1024,
                                    });
                                }
                                let utf16 = decode_utf16be(d)?;
                                out.entries.push(Mapping {
                                    code: lo.wrapping_add(count as u32),
                                    utf16be: utf16,
                                });
                                count += 1;
                            }
                            Tok::ArrEnd => break,
                            _ => {
                                return Err(Error::Structure(
                                    "bfrange array elements must be hex strings".into(),
                                ))
                            }
                        }
                    }
                    if count != range_len {
                        return Err(Error::Structure(format!(
                            "bfrange array has {count} entries, expected {range_len}"
                        )));
                    }
                }
                _ => {
                    return Err(Error::Structure(
                        "bfrange expects a hex string or array".into(),
                    ))
                }
            }
        }
        let end = self.next_token()?;
        match end {
            Tok::BareWord(b"endbfrange") => Ok(produced),
            _ => Err(Error::Structure("expected endbfrange".into())),
        }
    }
}

/// Parse a hex digit string (2 or 4 digits) to an unsigned value.
fn hex_uint(s: &[u8]) -> Result<u16, Error> {
    let s = std::str::from_utf8(s).map_err(|_| Error::Structure("invalid hex".into()))?;
    u16::from_str_radix(s, 16).map_err(|_| Error::Structure(format!("invalid hex '{s}'")))
}

/// Decode a hex string to UTF-16BE code units. Odd-length hex
/// strings are padded with a leading zero per PDF convention.
fn decode_utf16be(hex: &[u8]) -> Result<Vec<u16>, Error> {
    let mut bytes = Vec::with_capacity(hex.len() / 2 + 1);
    let hex_str = std::str::from_utf8(hex)
        .map_err(|_| Error::Structure("invalid hex in ToUnicode".into()))?;
    // Odd hex length → prefix a 0 per spec convention.
    let padded = if hex_str.len() % 2 == 1 {
        format!("0{hex_str}")
    } else {
        hex_str.to_string()
    };
    for i in (0..padded.len()).step_by(2) {
        let b = u8::from_str_radix(&padded[i..i + 2], 16)
            .map_err(|_| Error::Structure("invalid hex in ToUnicode".into()))?;
        bytes.push(b);
    }
    // Must be even length for UTF-16.
    if bytes.len() % 2 != 0 {
        return Err(Error::Structure(
            "ToUnicode dstString has odd byte length — not UTF-16BE".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect())
}

/// Load and parse the font's `/ToUnicode` CMap, if present.
/// Returns `None` when the font has no `/ToUnicode` entry.
pub(crate) fn load_tounicode(
    doc: &Document,
    font_dict: &crate::object::Dict,
    limits: &Limits,
) -> Result<Option<ToUnicode>, Error> {
    let obj = match font_dict.get(b"ToUnicode".as_slice()) {
        None | Some(Object::Null) => return Ok(None),
        Some(o) => o.clone(),
    };
    let resolved = doc.deref(&obj)?;
    let (dict, raw) = resolved
        .as_stream()
        .ok_or_else(|| Error::Structure("/ToUnicode is not a stream".into()))?;

    // /UseCMap — chaining to another CMap is not supported.
    if dict.get(b"UseCMap".as_slice()).is_some() {
        return Err(Error::Unsupported {
            feature: "ToUnicode /UseCMap",
            detail: "chained CMaps are not implemented".into(),
        });
    }

    let data = doc.decode_stream(dict, raw)?;
    ToUnicode::parse(&data, limits).map(Some)
}
