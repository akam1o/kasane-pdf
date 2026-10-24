//! Recursive-descent parser for PDF objects and indirect-object
//! definitions (ISO 32000-1:2008, §7.3).
//!
//! [`Parser`] wraps the lexer and produces [`Object`] values. It also
//! locates the `%PDF-` header and the `startxref` marker, and parses
//! trailer dictionaries at a given offset.
//!
//! Stream objects: when a parsed dictionary is immediately followed by
//! the `stream` keyword, the raw (still-encoded) stream bytes are
//! captured. `/Filter` decoding is intentionally left to the document layer.
//! The stream extent is determined from a direct-integer `/Length` when
//! it is present and consistent (keyword `endstream` follows the data);
//! otherwise we fall back to scanning for the `endstream` keyword, which
//! is conservative for streams whose data contains those bytes.

use crate::error::Error;
use crate::lexer::{Lexer, Token};
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};

/// Parse `M.m` after a `%PDF-` marker. Returns `Some((major, minor))`
/// for a well-formed version (`1.0` … `2.0`, up to two-digit minors
/// per §7.5.2); `None` when the bytes after the marker are not a
/// valid `digit(s).digit(s)` token — the file may still be a PDF the
/// *reader* can load, but the *writer* must not guess a version.
fn parse_version(after: &[u8]) -> Option<(u8, u8)> {
    let mut i = 0;
    let mut major: u32 = 0;
    let mut saw_major_digit = false;
    while i < after.len() && after[i].is_ascii_digit() {
        major = major
            .saturating_mul(10)
            .saturating_add((after[i] - b'0') as u32);
        saw_major_digit = true;
        i += 1;
    }
    if !saw_major_digit || i >= after.len() || after[i] != b'.' {
        return None;
    }
    i += 1;
    let mut minor: u32 = 0;
    let mut saw_minor_digit = false;
    while i < after.len() && after[i].is_ascii_digit() {
        minor = minor
            .saturating_mul(10)
            .saturating_add((after[i] - b'0') as u32);
        saw_minor_digit = true;
        i += 1;
    }
    if !saw_minor_digit {
        return None;
    }
    // Only 1.x and 2.0 are real PDF versions; anything else is a
    // malformed header the writer must not fabricate a version for.
    match (major, minor) {
        (1, m) if m <= 9 => Some((1, m as u8)),
        (2, 0) => Some((2, 0)),
        _ => None,
    }
}

/// A parsed indirect object definition (`n g obj … endobj`).
#[derive(Debug)]
pub(crate) struct IndirectObject {
    pub(crate) object_ref: IndirectRef,
    pub(crate) object: Object,
}

pub(crate) struct Parser<'a> {
    lexer: Lexer<'a>,
    limits: &'a Limits,
    /// Strict mode (the default) requires every indirect object to be
    /// closed by `endobj` (§7.3.10); recovery tolerates its absence.
    require_endobj: bool,
    /// Resolves an *indirect* stream `/Length` (`n g R`) to a byte
    /// count while a stream is captured; without it the capture falls
    /// back to scanning for `endstream`.
    length_resolver: Option<&'a dyn Fn(IndirectRef) -> Option<usize>>,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(input: &'a [u8], limits: &'a Limits) -> Self {
        Parser {
            require_endobj: true,
            lexer: Lexer::new(input, limits),
            limits,
            length_resolver: None,
        }
    }

    /// Resolve indirect `/Length` values through `f` when capturing
    /// stream data (see [`Parser::find_stream_end`]).
    pub(crate) fn with_length_resolver(&mut self, f: &'a dyn Fn(IndirectRef) -> Option<usize>) {
        self.length_resolver = Some(f);
    }

    /// Byte offset the lexer is currently positioned at.
    pub(crate) fn pos(&self) -> usize {
        self.lexer.pos()
    }

    /// Seek the lexer to an absolute byte offset.
    pub(crate) fn seek(&mut self, pos: usize) {
        self.lexer.seek(pos);
    }

    // ---------------------------------------------------------------
    // File-level structure
    // ---------------------------------------------------------------

    /// Locate the `%PDF-` header. Per spec it starts the file; real-world
    /// files may carry a short binary prefix, so we accept it within the
    /// first 1024 bytes (common reader behaviour).
    ///
    /// Returns `Some((major, minor))` for a well-formed `M.m` token
    /// after the marker — `%PDF-1.7` → `Some((1, 7))` — and `None`
    /// when the token is malformed. `None` is *not* a parse failure:
    /// the file still loads, but a rewrite must refuse rather than
    /// guess a version to declare (see `rewrite::effective_version`).
    pub(crate) fn check_header(&mut self) -> Result<Option<(u8, u8)>, Error> {
        const SEARCH_WINDOW: usize = 1024;
        let input = self.lexer.input;
        let end = SEARCH_WINDOW.min(input.len());
        if end < b"%PDF-".len() {
            return Err(Error::NotPdf);
        }
        for i in 0..end - b"%PDF-".len() {
            if &input[i..i + b"%PDF-".len()] == b"%PDF-" {
                let v = parse_version(&input[i + b"%PDF-".len()..]);
                return Ok(v);
            }
        }
        Err(Error::NotPdf)
    }

    /// Locate the last `startxref` keyword near the end of the file and
    /// parse the byte offset it names (§7.5.5).
    pub(crate) fn find_startxref(&mut self) -> Result<u64, Error> {
        const TAIL_WINDOW: usize = 1024;
        let input = self.lexer.input;
        let len = input.len();
        let window_start = len.saturating_sub(TAIL_WINDOW);
        let tail = &input[window_start..];

        let mut idx = None;
        for (i, w) in tail.windows(b"startxref".len()).enumerate() {
            if w == b"startxref" {
                idx = Some(window_start + i);
            }
        }

        let kw =
            idx.ok_or_else(|| Error::Parse("no startxref marker in last 1024 bytes".to_string()))?;

        self.lexer.seek(kw + b"startxref".len());
        match self.lexer.next_token()? {
            Some(Token::Word { bytes, offset }) => {
                let s = std::str::from_utf8(&bytes).map_err(|_| {
                    Error::Parse(format!("startxref value is not ASCII at byte {offset}"))
                })?;
                s.parse::<u64>().map_err(|_| {
                    Error::Parse(format!("invalid startxref offset '{s}' at byte {offset}"))
                })
            }
            Some(other) => Err(Error::Parse(format!(
                "expected startxref offset, found token at byte {}",
                other.offset()
            ))),
            None => Err(Error::UnexpectedEof {
                offset: kw,
                context: "startxref value",
            }),
        }
    }

    // ---------------------------------------------------------------
    // Object parsing
    // ---------------------------------------------------------------

    /// Parse a single object at the current position. Returns `None` at
    /// end of input. `depth` bounds nested container recursion.
    pub(crate) fn parse_object(&mut self, depth: u32) -> Result<Option<Object>, Error> {
        if depth > self.limits.max_nesting_depth {
            return Err(Error::LimitExceeded {
                limit: "max_nesting_depth",
                max: self.limits.max_nesting_depth as u64,
            });
        }
        let tok = match self.lexer.next_token()? {
            None => return Ok(None),
            Some(t) => t,
        };
        let obj = match tok {
            Token::ArrayOpen { .. } => self.parse_array(depth)?,
            Token::DictOpen { .. } => {
                let dict = self.parse_dict_body(depth)?;
                self.maybe_stream(dict)?
            }
            Token::ArrayClose { offset } | Token::DictClose { offset } => {
                return Err(Error::Parse(format!(
                    "unexpected closing delimiter at byte {offset}"
                )));
            }
            Token::LiteralString { value, .. } | Token::HexString { value, .. } => {
                Object::String(value)
            }
            Token::Name { value, .. } => Object::Name(value),
            Token::Word { bytes, offset } => self.parse_word_object(&bytes, offset)?,
        };
        Ok(Some(obj))
    }

    /// Parse the body of a dictionary (positioned just after `<<`).
    fn parse_dict_body(&mut self, depth: u32) -> Result<Dict, Error> {
        let mut dict = Dict::new();
        loop {
            if dict.len() >= self.limits.max_dict_entries {
                return Err(Error::LimitExceeded {
                    limit: "max_dict_entries",
                    max: self.limits.max_dict_entries as u64,
                });
            }
            match self.lexer.next_token()? {
                None => {
                    return Err(Error::UnexpectedEof {
                        offset: self.lexer.pos(),
                        context: "dictionary",
                    })
                }
                Some(Token::DictClose { .. }) => return Ok(dict),
                Some(Token::Name { value: key, offset }) => {
                    let value = self.parse_object(depth + 1)?.ok_or(Error::UnexpectedEof {
                        offset,
                        context: "dictionary value",
                    })?;
                    // §7.3.7: a null value is equivalent to an absent key.
                    if value != Object::Null {
                        dict.insert(key, value);
                    }
                }
                Some(other) => {
                    return Err(Error::Parse(format!(
                        "expected name key in dictionary, found token at byte {}",
                        other.offset()
                    )))
                }
            }
        }
    }

    /// Parse `[ … ]` elements (positioned after `[`).
    fn parse_array(&mut self, depth: u32) -> Result<Object, Error> {
        let mut items = Vec::new();
        loop {
            if items.len() >= self.limits.max_array_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_array_elements",
                    max: self.limits.max_array_elements as u64,
                });
            }
            let save = self.lexer.clone();
            match self.lexer.next_token()? {
                None => {
                    return Err(Error::UnexpectedEof {
                        offset: self.lexer.pos(),
                        context: "array",
                    })
                }
                Some(Token::ArrayClose { .. }) => return Ok(Object::Array(items)),
                Some(_) => {
                    self.lexer = save;
                    let obj = self.parse_object(depth + 1)?.ok_or(Error::UnexpectedEof {
                        offset: self.lexer.pos(),
                        context: "array element",
                    })?;
                    items.push(obj);
                }
            }
        }
    }

    /// Interpret a word token: boolean, null, integer, real, or the start
    /// of an indirect reference `n g R` (requires lookahead).
    fn parse_word_object(&mut self, bytes: &[u8], offset: usize) -> Result<Object, Error> {
        match bytes {
            b"true" => return Ok(Object::Bool(true)),
            b"false" => return Ok(Object::Bool(false)),
            b"null" => return Ok(Object::Null),
            _ => {}
        }

        if let Some(int_val) = parse_i64(bytes) {
            // Lookahead for `n g R`.
            let save = self.lexer.clone();
            let gen_and_r = (|| -> Result<Option<i64>, Error> {
                if let Some(Token::Word { bytes: b2, .. }) = self.lexer.next_token()? {
                    if let Some(g) = parse_i64(&b2) {
                        if let Some(Token::Word { bytes: b3, .. }) = self.lexer.next_token()? {
                            if b3 == b"R" {
                                return Ok(Some(g));
                            }
                        }
                    }
                }
                Ok(None)
            })();
            match gen_and_r {
                Ok(Some(gen)) => {
                    if !(0..=u32::MAX as i64).contains(&int_val) {
                        return Err(Error::Parse(format!(
                            "object number {int_val} out of range at byte {offset}"
                        )));
                    }
                    if !(0..=u16::MAX as i64).contains(&gen) {
                        return Err(Error::Parse(format!(
                            "generation {gen} out of range at byte {offset}"
                        )));
                    }
                    return Ok(Object::Ref(IndirectRef::new(int_val as u32, gen as u16)));
                }
                Ok(None) => {
                    self.lexer = save;
                    return Ok(Object::Integer(int_val));
                }
                Err(e) => return Err(e),
            }
        }

        if let Some(f) = parse_f64(bytes) {
            return Ok(Object::Real(f));
        }

        Err(Error::Parse(format!(
            "unexpected keyword '{}' at byte {offset}",
            String::from_utf8_lossy(bytes)
        )))
    }

    /// After a dictionary, check whether the `stream` keyword follows; if
    /// so, capture raw stream data up to `endstream`.
    fn maybe_stream(&mut self, dict: Dict) -> Result<Object, Error> {
        let save = self.lexer.clone();
        self.lexer.skip_ws();
        let pos = self.lexer.pos();
        let input = self.lexer.input;

        if !input[pos..].starts_with(b"stream") {
            self.lexer = save;
            return Ok(Object::Dict(dict));
        }
        // `stream` must be followed by an EOL marker (§7.3.8.1), not e.g.
        // by another word character ("streamline").
        let after_kw = pos + b"stream".len();
        let data_start = match input.get(after_kw) {
            Some(0x0D) if input.get(after_kw + 1) == Some(&0x0A) => after_kw + 2,
            Some(0x0D) | Some(0x0A) => after_kw + 1,
            _ => {
                // Not a real stream keyword; leave the dictionary as-is.
                self.lexer = save;
                return Ok(Object::Dict(dict));
            }
        };

        // Determine the end of stream data. Prefer a consistent direct
        // /Length; otherwise scan for the "endstream" keyword.
        let data_end = self.find_stream_end(&dict, data_start)?;

        self.lexer.seek(data_end);
        // Consume the endstream keyword; an EOL may precede it.
        self.lexer.skip_ws();
        match self.lexer.next_token()? {
            Some(Token::Word { bytes, .. }) if bytes == b"endstream" => {}
            other => {
                return Err(Error::Parse(format!(
                    "expected 'endstream' after stream data, found {:?}",
                    other.map(|t| t.offset())
                )))
            }
        }
        Ok(Object::Stream {
            dict,
            data: input[data_start..data_end].to_vec(),
        })
    }

    /// Determine where stream data ends (exclusive offset).
    ///
    /// If `/Length` is a non-negative integer — direct, or indirect and
    /// resolvable through the length resolver — and `endstream`
    /// follows at that position (after optional EOL), trust it.
    /// Otherwise scan for the first `endstream` keyword and back off the
    /// EOL that precedes it. The scan is a heuristic: data whose last
    /// byte is itself CR or LF loses it, which is why a resolvable
    /// `/Length` always wins.
    fn find_stream_end(&self, dict: &Dict, data_start: usize) -> Result<usize, Error> {
        let input = self.lexer.input;

        let declared: Option<i64> = match dict.get(b"Length".as_slice()) {
            Some(Object::Ref(r)) => self
                .length_resolver
                .and_then(|f| f(*r))
                .and_then(|n| i64::try_from(n).ok()),
            Some(o) => o.as_i64(),
            None => None,
        };
        if let Some(len) = declared {
            if len >= 0 {
                let end = data_start.saturating_add(len as usize);
                if end <= input.len() && self.endstream_follows(end) {
                    return Ok(end);
                }
                // Inconsistent /Length: fall through to keyword scan
                // (robust readers recover from a wrong Length this way).
            }
        }

        match find_subslice(&input[data_start.min(input.len())..], b"endstream") {
            Some(rel) => {
                let abs = data_start + rel;
                // The EOL immediately preceding endstream belongs to
                // the stream terminator, not to the data (§7.3.8.1).
                let mut end = abs;
                if end >= 2 && input[end - 2] == 0x0D && input[end - 1] == 0x0A {
                    end -= 2;
                } else if end >= 1 && (input[end - 1] == 0x0A || input[end - 1] == 0x0D) {
                    end -= 1;
                }
                Ok(end)
            }
            None => Err(Error::UnexpectedEof {
                offset: data_start,
                context: "endstream",
            }),
        }
    }

    /// True if `endstream` follows `pos` after at most whitespace.
    fn endstream_follows(&self, pos: usize) -> bool {
        let input = self.lexer.input;
        let mut p = pos;
        while p < input.len() && crate::lexer::is_white(input[p]) {
            p += 1;
        }
        input[p..].starts_with(b"endstream")
    }

    /// Accept an indirect object that is not closed by `endobj`
    /// (recovery mode).
    pub(crate) fn tolerate_missing_endobj(&mut self) {
        self.require_endobj = false;
    }

    /// Parse an indirect object definition `n g obj … endobj` at the
    /// current position. Returns `Ok(None)` (without consuming input) if
    /// the position does not start with `<int> <int> obj`; a missing
    /// `endobj` is a parse error unless [`Self::tolerate_missing_endobj`]
    /// was called.
    pub(crate) fn parse_indirect_object(&mut self) -> Result<Option<IndirectObject>, Error> {
        let save = self.lexer.clone();
        let header = (|| -> Result<Option<(i64, i64, usize)>, Error> {
            let t1 = self.lexer.next_token()?;
            let t2 = self.lexer.next_token()?;
            let t3 = self.lexer.next_token()?;
            match (t1, t2, t3) {
                (
                    Some(Token::Word { bytes: b1, offset }),
                    Some(Token::Word { bytes: b2, .. }),
                    Some(Token::Word { bytes: b3, .. }),
                ) if b3 == b"obj" => match (parse_i64(&b1), parse_i64(&b2)) {
                    (Some(n), Some(g)) => Ok(Some((n, g, offset))),
                    _ => Ok(None),
                },
                _ => Ok(None),
            }
        })();

        let (num, gen, num_off) = match header? {
            Some(h) => h,
            None => {
                self.lexer = save;
                return Ok(None);
            }
        };

        if !(0..=u32::MAX as i64).contains(&num) {
            return Err(Error::Parse(format!(
                "object number {num} out of range at byte {num_off}"
            )));
        }
        if !(0..=u16::MAX as i64).contains(&gen) {
            return Err(Error::Parse(format!(
                "generation {gen} out of range at byte {num_off}"
            )));
        }

        let object = self.parse_object(0)?.ok_or(Error::UnexpectedEof {
            offset: self.lexer.pos(),
            context: "indirect object body",
        })?;

        // `endobj` is required (§7.3.10). The object was delimited by
        // its own syntax, so a damaged file can still be read without
        // it — but only when the caller opted into recovery; a strict
        // parse reports the file as malformed.
        let save2 = self.lexer.clone();
        let end_pos = self.lexer.pos();
        match self.lexer.next_token()? {
            Some(Token::Word { bytes, .. }) if bytes == b"endobj" => {}
            _ => {
                if self.require_endobj {
                    return Err(Error::Parse(format!(
                        "object {num} {gen} is not closed by endobj (at byte {end_pos})"
                    )));
                }
                self.lexer = save2;
            }
        }

        Ok(Some(IndirectObject {
            object_ref: IndirectRef::new(num as u32, gen as u16),
            object,
        }))
    }

    /// Parse a dictionary at the current position (used for trailers).
    /// Streams are not accepted here.
    pub(crate) fn parse_dict_at_pos(&mut self) -> Result<Dict, Error> {
        match self.lexer.next_token()? {
            Some(Token::DictOpen { .. }) => self.parse_dict_body(0),
            Some(other) => Err(Error::Parse(format!(
                "expected '<<' dictionary, found token at byte {}",
                other.offset()
            ))),
            None => Err(Error::UnexpectedEof {
                offset: self.lexer.pos(),
                context: "dictionary",
            }),
        }
    }

    // ---------------------------------------------------------------
    // xref-section helpers (word-level access used by the xref module)
    // ---------------------------------------------------------------

    /// Read the next token if it is a plain word (number, keyword).
    /// Returns `Ok(None)` at end of input and an error on any
    /// non-word token.
    pub(crate) fn read_word_token(&mut self) -> Result<Option<Vec<u8>>, Error> {
        match self.lexer.next_token()? {
            None => Ok(None),
            Some(Token::Word { bytes, .. }) => Ok(Some(bytes)),
            Some(other) => Err(Error::Parse(format!(
                "expected word token, found non-word token at byte {}",
                other.offset()
            ))),
        }
    }

    /// Read one xref entry line: `<offset> <generation> <n|f>` as three
    /// word tokens. Returns `Ok(None)` only if the input ends cleanly
    /// before the first field.
    pub(crate) fn read_xref_entry_line(&mut self) -> Result<Option<XrefLine>, Error> {
        let offset_tok = match self.read_word_token()? {
            None => return Ok(None),
            Some(w) => w,
        };
        let gen_tok = self.read_word_token()?.ok_or(Error::UnexpectedEof {
            offset: self.pos(),
            context: "xref entry generation",
        })?;
        let kind_tok = self.read_word_token()?.ok_or(Error::UnexpectedEof {
            offset: self.pos(),
            context: "xref entry type",
        })?;

        let offset = parse_u64_digits(&offset_tok).ok_or_else(|| {
            Error::Parse(format!(
                "bad xref entry offset '{}'",
                String::from_utf8_lossy(&offset_tok)
            ))
        })?;
        let generation = parse_u64_digits(&gen_tok)
            .filter(|&g| g <= u32::MAX as u64)
            .ok_or_else(|| {
                Error::Parse(format!(
                    "bad xref entry generation '{}'",
                    String::from_utf8_lossy(&gen_tok)
                ))
            })? as u32;
        if kind_tok.len() != 1 {
            return Err(Error::Parse(format!(
                "bad xref entry type '{}'",
                String::from_utf8_lossy(&kind_tok)
            )));
        }
        Ok(Some(XrefLine {
            offset,
            generation,
            kind: kind_tok[0],
        }))
    }
}

/// One line of a classic xref subsection body.
#[derive(Debug, Clone, Copy)]
pub(crate) struct XrefLine {
    pub(crate) offset: u64,
    /// Free entries use the full 5-digit range (0–65535) per spec, but
    /// some writers emit 65536; accept u32 and let the caller decide.
    pub(crate) generation: u32,
    /// `b'n'` (in-use) or `b'f'` (free); validated by the caller.
    pub(crate) kind: u8,
}

/// Unsigned integer made of ASCII digits only.
fn parse_u64_digits(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u64>().ok()
}

/// Strict integer token: optional sign followed by ASCII digits only.
fn parse_i64(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() {
        return None;
    }
    let rest = match bytes[0] {
        b'+' | b'-' => &bytes[1..],
        _ => bytes,
    };
    if rest.is_empty() || !rest.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<i64>().ok()
}

/// Real-number token: digits plus at most '.', '+' or '-' characters
/// (§7.3.3 numbers do not use exponent notation).
fn parse_f64(bytes: &[u8]) -> Option<f64> {
    if bytes.is_empty() {
        return None;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_digit() || *b == b'.' || *b == b'+' || *b == b'-')
    {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<f64>().ok()
}

/// Find `needle` in `hay`; returns the index relative to `hay`'s start.
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}
