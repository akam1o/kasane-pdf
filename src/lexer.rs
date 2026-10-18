//! Low-level tokenization of the PDF lexical layer
//! (ISO 32000-1:2008, §7.2 lexical conventions, §7.3 object types).
//!
//! The [`Lexer`] operates on the whole input byte slice and produces
//! [`Token`]s with byte offsets. It handles whitespace, `%` comments,
//! `#xx` name escapes, literal-string escapes/nesting, and hexadecimal
//! strings. It does not understand higher-level constructs (`obj`,
//! `stream`, xref tables); that is the parser/xref modules' job.

use crate::error::Error;
use crate::limits::Limits;

/// Whitespace characters per §7.2.2 Table 1.
pub(crate) fn is_white(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// End-of-line characters (LF, CR).
fn is_eol(b: u8) -> bool {
    b == 0x0A || b == 0x0D
}

/// Delimiter characters per §7.2.2 Table 2.
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// A lexical token with the byte offset where it starts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token {
    /// A regular (non-delimiter, non-keyword) byte run, e.g. `12`, `+3.0`,
    /// `obj`, `R`, `true`. Interpretation is left to the parser.
    Word { offset: usize, bytes: Vec<u8> },
    /// `/Name` with `#xx` escapes decoded; excludes the leading `/`.
    Name { offset: usize, value: Vec<u8> },
    /// Literal string `(…)`; escapes resolved, raw bytes.
    LiteralString { offset: usize, value: Vec<u8> },
    /// Hexadecimal string `<…>`; decoded bytes.
    HexString { offset: usize, value: Vec<u8> },
    /// `[`
    ArrayOpen { offset: usize },
    /// `]`
    ArrayClose { offset: usize },
    /// `<<`
    DictOpen { offset: usize },
    /// `>>`
    DictClose { offset: usize },
}

impl Token {
    /// Byte offset where this token begins.
    pub(crate) fn offset(&self) -> usize {
        match self {
            Token::Word { offset, .. }
            | Token::Name { offset, .. }
            | Token::LiteralString { offset, .. }
            | Token::HexString { offset, .. }
            | Token::ArrayOpen { offset }
            | Token::ArrayClose { offset }
            | Token::DictOpen { offset }
            | Token::DictClose { offset } => *offset,
        }
    }
}

/// Byte-slice lexer. Cloneable to support bounded lookahead.
#[derive(Clone)]
pub(crate) struct Lexer<'a> {
    /// The whole input being lexed (exposed for offset-based scans).
    pub(crate) input: &'a [u8],
    pos: usize,
    limits: &'a Limits,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(input: &'a [u8], limits: &'a Limits) -> Self {
        Lexer {
            input,
            pos: 0,
            limits,
        }
    }

    /// Current byte position (after skipped whitespace/comments).
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Seek to an absolute byte offset.
    pub(crate) fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.input.len());
    }

    /// True when positioned at or past end of input.
    pub(crate) fn at_eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// Skip whitespace and `%` comments (comments run to end of line).
    pub(crate) fn skip_ws(&mut self) {
        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                // Comment extends to but not including the EOL marker.
                while self.pos < self.input.len() && !is_eol(self.input[self.pos]) {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Produce the next token, skipping leading whitespace/comments.
    /// Returns `Ok(None)` at end of input.
    pub(crate) fn next_token(&mut self) -> Result<Option<Token>, Error> {
        self.skip_ws();
        if self.at_eof() {
            return Ok(None);
        }
        let offset = self.pos;
        let b = self.input[self.pos];
        let token = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen { offset }
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose { offset }
            }
            b'<' => {
                if self.peek_byte(1) == Some(b'<') {
                    self.pos += 2;
                    Token::DictOpen { offset }
                } else {
                    Token::HexString {
                        offset,
                        value: self.read_hex_string()?,
                    }
                }
            }
            b'>' => {
                if self.peek_byte(1) == Some(b'>') {
                    self.pos += 2;
                    Token::DictClose { offset }
                } else {
                    return Err(Error::Parse(format!(
                        "stray '>' delimiter at byte {offset}"
                    )));
                }
            }
            b'(' => Token::LiteralString {
                offset,
                value: self.read_literal_string()?,
            },
            b'/' => Token::Name {
                offset,
                value: self.read_name()?,
            },
            b')' => {
                return Err(Error::Parse(format!("unbalanced ')' at byte {offset}")));
            }
            _ => Token::Word {
                offset,
                bytes: self.read_regular_word(),
            },
        };
        Ok(Some(token))
    }

    fn peek_byte(&self, ahead: usize) -> Option<u8> {
        self.input.get(self.pos + ahead).copied()
    }

    /// Read a run of regular (non-white, non-delimiter) bytes.
    fn read_regular_word(&mut self) -> Vec<u8> {
        let start = self.pos;
        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if is_white(b) || is_delimiter(b) {
                break;
            }
            self.pos += 1;
        }
        self.input[start..self.pos].to_vec()
    }

    /// Read a name starting at `/`; decodes `#xx` escapes (§7.3.5).
    /// Position must be at the `/` on entry.
    fn read_name(&mut self) -> Result<Vec<u8>, Error> {
        debug_assert_eq!(self.input[self.pos], b'/');
        self.pos += 1;
        let mut out = Vec::new();
        while self.pos < self.input.len() {
            let b = self.input[self.pos];
            if is_white(b) || is_delimiter(b) {
                break;
            }
            if b == b'#' {
                // #xx hexadecimal escape. A malformed escape is an error,
                // not a literal '#': silently accepting it could change
                // the name's identity.
                let h = self.input.get(self.pos + 1);
                let l = self.input.get(self.pos + 2);
                match (h, l) {
                    (Some(&hi), Some(&lo)) => {
                        let hi = hex_val(hi).ok_or_else(|| {
                            Error::Parse(format!("invalid '#' escape in name at byte {}", self.pos))
                        })?;
                        let lo = hex_val(lo).ok_or_else(|| {
                            Error::Parse(format!("invalid '#' escape in name at byte {}", self.pos))
                        })?;
                        out.push(hi << 4 | lo);
                        self.pos += 3;
                    }
                    _ => {
                        return Err(Error::Parse(format!(
                            "truncated '#' escape in name at byte {}",
                            self.pos
                        )))
                    }
                }
            } else {
                out.push(b);
                self.pos += 1;
            }
            if out.len() > self.limits.max_string_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_string_bytes (name)",
                    max: self.limits.max_string_bytes as u64,
                });
            }
        }
        Ok(out)
    }

    /// Read a literal string `(…)` starting at `(`, resolving escapes
    /// and nested balanced parentheses (§7.3.4.2).
    fn read_literal_string(&mut self) -> Result<Vec<u8>, Error> {
        debug_assert_eq!(self.input[self.pos], b'(');
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth: u32 = 1;
        loop {
            if self.pos >= self.input.len() {
                return Err(Error::UnexpectedEof {
                    offset: self.pos,
                    context: "literal string",
                });
            }
            let b = self.input[self.pos];
            self.pos += 1;
            match b {
                b'\\' => {
                    // Escape sequence (Table 3).
                    if self.pos >= self.input.len() {
                        return Err(Error::UnexpectedEof {
                            offset: self.pos,
                            context: "literal string escape",
                        });
                    }
                    let e = self.input[self.pos];
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        b'0'..=b'7' => {
                            // Up to three octal digits; first already read.
                            let mut val: u16 = (e - b'0') as u16;
                            for _ in 0..2 {
                                match self.input.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        val = val * 8 + (d - b'0') as u16;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((val & 0xFF) as u8);
                        }
                        0x0D => {
                            // Backslash + line ending: line continuation,
                            // swallow a following LF of a CRLF pair.
                            if self.input.get(self.pos) == Some(&0x0A) {
                                self.pos += 1;
                            }
                        }
                        0x0A => { /* line continuation */ }
                        other => {
                            // Unknown escapes denote the character itself.
                            out.push(other)
                        }
                    }
                }
                b'(' => {
                    depth += 1;
                    if depth > self.limits.max_nesting_depth {
                        return Err(Error::LimitExceeded {
                            limit: "max_nesting_depth (string parentheses)",
                            max: self.limits.max_nesting_depth as u64,
                        });
                    }
                    out.push(b'(');
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b')');
                }
                _ => out.push(b),
            }
            if out.len() > self.limits.max_string_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_string_bytes (literal string)",
                    max: self.limits.max_string_bytes as u64,
                });
            }
        }
        Ok(out)
    }

    /// Read a hexadecimal string `<…>` starting at `<` (single angle).
    fn read_hex_string(&mut self) -> Result<Vec<u8>, Error> {
        debug_assert_eq!(self.input[self.pos], b'<');
        self.pos += 1;
        let mut nibbles: Vec<u8> = Vec::new();
        loop {
            if self.pos >= self.input.len() {
                return Err(Error::UnexpectedEof {
                    offset: self.pos,
                    context: "hexadecimal string",
                });
            }
            let b = self.input[self.pos];
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if is_white(b) {
                continue;
            }
            match hex_val(b) {
                Some(v) => nibbles.push(v),
                None => {
                    return Err(Error::Parse(format!(
                        "invalid character 0x{b:02X} in hexadecimal string at byte {}",
                        self.pos - 1
                    )))
                }
            }
            if nibbles.len() / 2 > self.limits.max_string_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_string_bytes (hex string)",
                    max: self.limits.max_string_bytes as u64,
                });
            }
        }
        // Odd nibble count: final nibble is the high half of the last byte.
        if nibbles.len() % 2 == 1 {
            nibbles.push(0);
        }
        Ok(nibbles.chunks(2).map(|c| (c[0] << 4) | c[1]).collect())
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
