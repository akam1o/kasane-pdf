//! Embedded CMaps for Type 0 fonts (ISO 32000-1 §9.7.5, Adobe TN
//! #5014): `/Encoding` given as a stream. The CMap decides how many
//! bytes each character code takes (`begincodespacerange`) and which
//! CID each code maps to (`begincidchar`, `begincidrange`); `/WMode 1`
//! selects vertical writing.
//!
//! `usecmap` is honoured only for the two Identity CMaps (they need no
//! resource file). Any other predefined CMap name — `90ms-RKSJ-H`,
//! `UniJIS-UCS2-H`, … — needs the Adobe CMap resources, which are not
//! bundled: `Error::Unsupported`, never a guess at the mapping.
//!
//! Bounded by the content limits: entries are capped by
//! `max_text_codes`, token count by the lexer's own bounds.

use std::collections::HashMap;

use crate::error::Error;
use crate::lexer::{Lexer, Token};
use crate::limits::Limits;

/// A parsed embedded CMap.
#[derive(Debug, Clone, Default)]
pub(crate) struct CMap {
    /// `(byte length, low, high)` codespace ranges.
    codespaces: Vec<(u8, u32, u32)>,
    /// Single-code mappings.
    single: HashMap<u32, u32>,
    /// `(byte length, low, high, first cid)` range mappings, in file
    /// order (later entries override — searched last-to-first).
    ranges: Vec<(u8, u32, u32, u32)>,
    /// Codes not mapped explicitly fall through to Identity (the CMap
    /// `usecmap`s Identity-H/V).
    identity_base: bool,
    /// `/WMode 1`.
    pub(crate) vertical: bool,
}

impl CMap {
    /// Parse the decoded stream bytes. `use_cmap` is the stream
    /// dictionary's `/UseCMap` name, if any.
    pub(crate) fn parse(
        data: &[u8],
        use_cmap: Option<&[u8]>,
        limits: &Limits,
    ) -> Result<CMap, Error> {
        let mut cm = CMap::default();
        match use_cmap {
            None => {}
            Some(b"Identity-H") => cm.identity_base = true,
            Some(b"Identity-V") => {
                cm.identity_base = true;
                cm.vertical = true;
            }
            Some(other) => {
                return Err(Error::Unsupported {
                    feature: "Type 0 font encoding",
                    detail: format!(
                        "/UseCMap /{} — predefined CMaps other than Identity are not bundled",
                        String::from_utf8_lossy(other)
                    ),
                })
            }
        }
        let mut lx = Lexer::new(data, limits);
        // Operand stack of the few token kinds a CMap body uses.
        let mut stack: Vec<Token> = Vec::new();
        let mut entries: u64 = 0;
        let cap = limits.max_text_codes;
        let count = |n: u64, entries: &mut u64| -> Result<(), Error> {
            *entries += n;
            if *entries > cap {
                return Err(Error::LimitExceeded {
                    limit: "max_text_codes (CMap entries)",
                    max: cap,
                });
            }
            Ok(())
        };
        while let Some(tok) = lx.next_token()? {
            match &tok {
                Token::Word { bytes, .. } => match bytes.as_slice() {
                    b"begincodespacerange" => {
                        stack.clear();
                        loop {
                            let lo = match lx.next_token()? {
                                Some(Token::Word { bytes, .. })
                                    if bytes == b"endcodespacerange" =>
                                {
                                    break
                                }
                                Some(Token::HexString { value, .. }) => value,
                                Some(_) => continue,
                                None => return Err(eof("codespacerange")),
                            };
                            let hi = match lx.next_token()? {
                                Some(Token::HexString { value, .. }) => value,
                                _ => {
                                    return Err(Error::Structure(
                                        "codespacerange needs a high bound".into(),
                                    ))
                                }
                            };
                            if lo.is_empty() || lo.len() > 4 || lo.len() != hi.len() {
                                return Err(Error::Structure(
                                    "codespacerange bounds must be 1–4 bytes of equal length"
                                        .into(),
                                ));
                            }
                            count(1, &mut entries)?;
                            cm.codespaces.push((lo.len() as u8, be(&lo), be(&hi)));
                        }
                    }
                    b"begincidrange" => {
                        stack.clear();
                        loop {
                            let lo = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) if bytes == b"endcidrange" => break,
                                Some(Token::HexString { value, .. }) => value,
                                Some(_) => continue,
                                None => return Err(eof("cidrange")),
                            };
                            let hi = match lx.next_token()? {
                                Some(Token::HexString { value, .. }) => value,
                                _ => {
                                    return Err(Error::Structure(
                                        "cidrange needs a high bound".into(),
                                    ))
                                }
                            };
                            let cid = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) => int(&bytes)?,
                                _ => return Err(Error::Structure("cidrange needs a CID".into())),
                            };
                            if lo.is_empty() || lo.len() > 4 || lo.len() != hi.len() {
                                return Err(Error::Structure(
                                    "cidrange bounds must be 1–4 equal-length bytes".into(),
                                ));
                            }
                            let (l, h) = (be(&lo), be(&hi));
                            if h < l {
                                return Err(Error::Structure(
                                    "cidrange low bound exceeds high".into(),
                                ));
                            }
                            count(1, &mut entries)?;
                            cm.ranges.push((lo.len() as u8, l, h, cid));
                        }
                    }
                    b"begincidchar" => {
                        stack.clear();
                        loop {
                            let code = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) if bytes == b"endcidchar" => break,
                                Some(Token::HexString { value, .. }) => value,
                                Some(_) => continue,
                                None => return Err(eof("cidchar")),
                            };
                            let cid = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) => int(&bytes)?,
                                _ => return Err(Error::Structure("cidchar needs a CID".into())),
                            };
                            if code.is_empty() || code.len() > 4 {
                                return Err(Error::Structure(
                                    "cidchar code must be 1–4 bytes".into(),
                                ));
                            }
                            count(1, &mut entries)?;
                            if cm.single.len() < cap as usize {
                                cm.single.insert(be(&code), cid);
                            }
                        }
                    }
                    b"usecmap" => {
                        // `/Name usecmap`
                        match stack.last() {
                            Some(Token::Name { value, .. }) if value == b"Identity-H" => {
                                cm.identity_base = true
                            }
                            Some(Token::Name { value, .. }) if value == b"Identity-V" => {
                                cm.identity_base = true;
                                cm.vertical = true;
                            }
                            Some(Token::Name { value, .. }) => {
                                return Err(Error::Unsupported {
                                    feature: "Type 0 font encoding",
                                    detail: format!(
                                        "usecmap /{} — predefined CMaps other than Identity are not bundled",
                                        String::from_utf8_lossy(value)
                                    ),
                                })
                            }
                            _ => return Err(Error::Structure("usecmap without a CMap name".into())),
                        }
                        stack.clear();
                    }
                    b"def" => {
                        // `/WMode 1 def`
                        if stack.len() >= 2 {
                            if let (Token::Name { value, .. }, Token::Word { bytes, .. }) =
                                (&stack[stack.len() - 2], &stack[stack.len() - 1])
                            {
                                if value == b"WMode" {
                                    cm.vertical = int(bytes)? == 1;
                                }
                            }
                        }
                        stack.clear();
                    }
                    b"begin" | b"end" | b"pop" | b"findresource" | b"dict" | b"currentdict"
                    | b"defineresource" | b"CMapName" | b"endcmap" | b"begincmap" => stack.clear(),
                    b"beginbfchar" | b"beginbfrange" | b"beginnotdefrange" | b"beginnotdefchar" => {
                        // Not meaningful in an encoding CMap; skip to the end marker.
                        let end: &[u8] = match bytes.as_slice() {
                            b"beginbfchar" => b"endbfchar",
                            b"beginbfrange" => b"endbfrange",
                            b"beginnotdefrange" => b"endnotdefrange",
                            _ => b"endnotdefchar",
                        };
                        loop {
                            match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) if bytes == end => break,
                                Some(_) => {}
                                None => return Err(eof("CMap block")),
                            }
                        }
                    }
                    _ => {
                        stack.push(tok);
                        if stack.len() > 64 {
                            stack.remove(0);
                        }
                    }
                },
                Token::DictOpen { .. } => {
                    // Skip `<< … >>` (CIDSystemInfo).
                    let mut depth = 1;
                    while depth > 0 {
                        match lx.next_token()? {
                            Some(Token::DictOpen { .. }) => depth += 1,
                            Some(Token::DictClose { .. }) => depth -= 1,
                            Some(_) => {}
                            None => return Err(eof("CMap dictionary")),
                        }
                    }
                }
                _ => {
                    stack.push(tok);
                    if stack.len() > 64 {
                        stack.remove(0);
                    }
                }
            }
        }
        if cm.codespaces.is_empty() && !cm.identity_base {
            return Err(Error::Structure(
                "embedded CMap declares no codespace range".into(),
            ));
        }
        Ok(cm)
    }

    /// The code starting at `bytes[pos]` and its byte length, per the
    /// codespace ranges (§9.7.6.3): the shortest range whose bounds
    /// contain the bytes wins; bytes matching no range consume the
    /// shortest declared length (2 for a pure Identity base) and map
    /// to CID 0.
    pub(crate) fn next_code(&self, bytes: &[u8], pos: usize) -> (u32, usize) {
        for len in 1..=4usize {
            if pos + len > bytes.len() {
                break;
            }
            let code = be(&bytes[pos..pos + len]);
            if self
                .codespaces
                .iter()
                .any(|(l, lo, hi)| *l as usize == len && code >= *lo && code <= *hi)
            {
                return (code, len);
            }
        }
        let len = self
            .codespaces
            .iter()
            .map(|(l, _, _)| *l as usize)
            .min()
            .unwrap_or(2)
            .min(bytes.len() - pos)
            .max(1);
        (be(&bytes[pos..pos + len]), len)
    }

    /// CID for `code` (of `len` bytes).
    pub(crate) fn cid(&self, code: u32, len: usize) -> u32 {
        if let Some(c) = self.single.get(&code) {
            return *c;
        }
        for (l, lo, hi, cid) in self.ranges.iter().rev() {
            if *l as usize == len && code >= *lo && code <= *hi {
                return cid + (code - lo);
            }
        }
        if self.identity_base {
            code
        } else {
            0
        }
    }

    /// `true` when every code is two bytes (so a `/ToUnicode` CMap
    /// should use two-byte codes as well).
    pub(crate) fn is_two_byte(&self) -> bool {
        !self.codespaces.is_empty() && self.codespaces.iter().all(|(l, _, _)| *l == 2)
    }
}

fn be(b: &[u8]) -> u32 {
    b.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32)
}

fn int(b: &[u8]) -> Result<u32, Error> {
    std::str::from_utf8(b)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| (0..=0xFFFF).contains(v))
        .map(|v| v as u32)
        .ok_or_else(|| Error::Structure("CMap CID must be an integer 0..=65535".into()))
}

fn eof(ctx: &'static str) -> Error {
    Error::UnexpectedEof {
        offset: 0,
        context: ctx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_width_codespace_and_ranges() {
        let src = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
            /CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> def\n\
            /CMapName /Test def /WMode 0 def\n\
            2 begincodespacerange <00> <80> <8140> <9ffc> endcodespacerange\n\
            1 begincidrange <8140> <8150> 633 endcidrange\n\
            1 begincidchar <41> 34 endcidchar\n\
            endcmap CMapName currentdict /CMap defineresource pop end end";
        let cm = CMap::parse(src, None, &Limits::default()).unwrap();
        assert_eq!(cm.next_code(b"\x41\x81\x42", 0), (0x41, 1));
        assert_eq!(cm.next_code(b"\x41\x81\x42", 1), (0x8142, 2));
        assert_eq!(cm.cid(0x41, 1), 34);
        assert_eq!(cm.cid(0x8142, 2), 635);
        assert_eq!(cm.cid(0x50, 1), 0, "unmapped → CID 0");
        assert!(!cm.vertical);
    }

    #[test]
    fn identity_base_and_wmode() {
        let src = b"/Identity-H usecmap /WMode 1 def 1 begincidchar <0005> 9 endcidchar";
        let cm = CMap::parse(src, None, &Limits::default()).unwrap();
        assert!(cm.vertical);
        assert_eq!(cm.cid(5, 2), 9);
        assert_eq!(cm.cid(7, 2), 7);
        assert_eq!(cm.next_code(b"\x00\x07", 0), (7, 2));
        assert!(matches!(
            CMap::parse(b"/90ms-RKSJ-H usecmap", None, &Limits::default()),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            CMap::parse(
                b"1 begincidchar <41> 1 endcidchar",
                None,
                &Limits::default()
            ),
            Err(Error::Structure(_))
        ));
    }
}
