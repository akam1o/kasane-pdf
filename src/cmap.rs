//! Embedded CMaps for Type 0 fonts (ISO 32000-1 §9.7.5, Adobe TN
//! #5014): `/Encoding` given as a stream. The CMap decides how many
//! bytes each character code takes (`begincodespacerange`) and which
//! CID each code maps to (`begincidchar`, `begincidrange`); `/WMode 1`
//! selects vertical writing.
//!
//! A code in the codespace with no defined mapping takes its notdef
//! mapping (`beginnotdefchar`, `beginnotdefrange`: every code of a range
//! maps to the same CID), else CID 0 (§9.7.6.3); bytes in no codespace
//! range map to CID 0. Poppler, MuPDF and pdf.js ignore notdef mappings
//! and differ on such codes among themselves, so redaction refuses
//! showing any of them (`CMap::lookup` tells which).
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
    /// Single-code mappings: `(byte length, code)` → `(cid, order)`.
    single: HashMap<(u8, u32), (u32, u32)>,
    /// `(byte length, low, high, first cid, order)` range mappings, in
    /// file order. `order` counts definitions of either kind in file
    /// order (a base CMap's first): the latest definition of a code
    /// wins, as Poppler, MuPDF and pdf.js read it.
    ranges: Vec<(u8, u32, u32, u32, u32)>,
    /// The next definition's `order`.
    order: u32,
    /// `(byte length, low, high, cid)` notdef mappings (`beginnotdefchar`
    /// entries have `low == high`): every code in a range maps to the
    /// same CID. Searched last-to-first, after the defined mappings.
    notdef: Vec<(u8, u32, u32, u32)>,
    /// Codes not mapped explicitly fall through to Identity (the CMap
    /// `usecmap`s Identity-H/V).
    identity_base: bool,
    /// `/WMode 1`.
    pub(crate) vertical: bool,
    /// Some code is written as a literal string: read like a hex one
    /// here and by pdf.js, rejected by Poppler — readers place glyphs
    /// differently.
    pub(crate) literal_codes: bool,
    /// A construction Poppler, MuPDF and pdf.js read differently, when
    /// the CMap has one: bf blocks in an encoding CMap, definitions after
    /// `endcmap` or a second `begincmap`, an Identity base with
    /// codespace ranges of its own, a `/WMode` other than 0 or 1, an odd-length hex
    /// code, a stray token in a block, codespace ranges of different
    /// lengths sharing leading bytes. Redaction refuses such a font.
    pub(crate) reader_dependent: Option<&'static str>,
}

impl CMap {
    /// Parse the decoded stream bytes. `use_cmap` is the stream
    /// dictionary's `/UseCMap` name, if any.
    pub(crate) fn parse(
        data: &[u8],
        use_cmap: Option<&[u8]>,
        limits: &Limits,
    ) -> Result<CMap, Error> {
        Self::parse_depth(data, use_cmap, limits, 0)
    }

    /// A predefined CMap by name (§9.7.5.2): the Identity CMaps always;
    /// the Adobe-Japan1 set when the `cjk-fallback` feature bundles
    /// them. `Ok(None)` when the name is not available.
    pub(crate) fn predefined(name: &[u8], limits: &Limits) -> Result<Option<CMap>, Error> {
        match name {
            b"Identity-H" => {
                return Ok(Some(CMap {
                    identity_base: true,
                    ..Default::default()
                }))
            }
            b"Identity-V" => {
                return Ok(Some(CMap {
                    identity_base: true,
                    vertical: true,
                    ..Default::default()
                }))
            }
            _ => {}
        }
        let Some(data) = crate::cjk::predefined_cmap(name) else {
            return Ok(None);
        };
        // Parsed once per process; the 300 KB UniJIS files lex slowly.
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static CACHE: OnceLock<Mutex<HashMap<Vec<u8>, CMap>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(c) = cache.lock() {
            if let Some(cm) = c.get(name) {
                return Ok(Some(cm.clone()));
            }
        }
        let cm = Self::parse_depth(data, None, limits, 1)?;
        if let Ok(mut c) = cache.lock() {
            c.insert(name.to_vec(), cm.clone());
        }
        Ok(Some(cm))
    }

    /// Resolve a `usecmap` / `/UseCMap` base into `cm`: Identity sets
    /// the fall-through; a bundled predefined CMap is parsed (depth
    /// bounded) and its mappings become the base of `cm`'s.
    fn use_base(cm: &mut CMap, name: &[u8], limits: &Limits, depth: usize) -> Result<(), Error> {
        match name {
            b"Identity-H" => cm.identity_base = true,
            b"Identity-V" => {
                cm.identity_base = true;
                cm.vertical = true;
            }
            other => {
                if depth > 4 {
                    return Err(Error::Structure("usecmap chain deeper than 4 CMaps".into()));
                }
                let Some(data) = crate::cjk::predefined_cmap(other) else {
                    return Err(Error::Unsupported {
                        feature: "Type 0 font encoding",
                        detail: format!(
                            "usecmap /{} — predefined CMaps other than Identity are not bundled \
                             (build with the `cjk-fallback` feature for the Adobe-Japan1 set)",
                            String::from_utf8_lossy(other)
                        ),
                    });
                };
                let base = Self::parse_depth(data, None, limits, depth + 1)?;
                // Base mappings go first: later entries override.
                let mut merged = base;
                merged.codespaces.extend(std::mem::take(&mut cm.codespaces));
                // Definitions made before the `usecmap` (rare) still
                // follow the base's.
                let shift = merged.order;
                merged.ranges.extend(
                    cm.ranges
                        .drain(..)
                        .map(|(l, lo, hi, c, o)| (l, lo, hi, c, o + shift)),
                );
                merged
                    .single
                    .extend(cm.single.drain().map(|(k, (c, o))| (k, (c, o + shift))));
                merged.order += cm.order;
                merged.notdef.extend(std::mem::take(&mut cm.notdef));
                merged.vertical |= cm.vertical;
                merged.identity_base |= cm.identity_base;
                merged.literal_codes |= cm.literal_codes;
                merged.reader_dependent = cm.reader_dependent.or(merged.reader_dependent);
                *cm = merged;
            }
        }
        Ok(())
    }

    fn parse_depth(
        data: &[u8],
        use_cmap: Option<&[u8]>,
        limits: &Limits,
        depth: usize,
    ) -> Result<CMap, Error> {
        let mut cm = CMap::default();
        if let Some(name) = use_cmap {
            Self::use_base(&mut cm, name, limits, depth)?;
        }
        let mut lx = Lexer::new(data, limits);
        let (mut begun, mut ended) = (false, false);
        let mut wmode_seen: Option<u32> = None;
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
                        if ended {
                            cm.reader_dependent
                                .get_or_insert("definitions after endcmap");
                        }
                        stack.clear();
                        loop {
                            let lo = match lx.next_token()? {
                                Some(Token::Word { bytes, .. })
                                    if bytes == b"endcodespacerange" =>
                                {
                                    break
                                }
                                Some(Token::HexString { value, .. }) => value,
                                Some(Token::LiteralString { value, .. }) => {
                                    cm.literal_codes = true;
                                    value
                                }
                                Some(_) => {
                                    // A stray token: readers skip it differently.
                                    cm.reader_dependent
                                        .get_or_insert("a stray token in a block");
                                    continue;
                                }
                                None => return Err(eof("codespacerange")),
                            };
                            let hi = match lx.next_token()? {
                                Some(Token::HexString { value, .. }) => value,
                                Some(Token::LiteralString { value, .. }) => {
                                    cm.literal_codes = true;
                                    value
                                }
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
                    b"begincidrange" | b"beginnotdefrange" => {
                        let notdef = bytes.as_slice() == b"beginnotdefrange";
                        if ended {
                            cm.reader_dependent
                                .get_or_insert("definitions after endcmap");
                        }
                        let (end, kind): (&[u8], &'static str) = if notdef {
                            (b"endnotdefrange", "notdefrange")
                        } else {
                            (b"endcidrange", "cidrange")
                        };
                        stack.clear();
                        loop {
                            let lo = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) if bytes == end => break,
                                Some(Token::HexString { value, .. }) => value,
                                Some(Token::LiteralString { value, .. }) => {
                                    cm.literal_codes = true;
                                    value
                                }
                                Some(_) => {
                                    // A stray token: readers skip it differently.
                                    cm.reader_dependent
                                        .get_or_insert("a stray token in a block");
                                    continue;
                                }
                                None => return Err(eof(kind)),
                            };
                            let hi = match lx.next_token()? {
                                Some(Token::HexString { value, .. }) => value,
                                Some(Token::LiteralString { value, .. }) => {
                                    cm.literal_codes = true;
                                    value
                                }
                                _ => {
                                    return Err(Error::Structure(format!(
                                        "{kind} needs a high bound"
                                    )))
                                }
                            };
                            let cid = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) => int(&bytes)?,
                                _ => return Err(Error::Structure(format!("{kind} needs a CID"))),
                            };
                            if lo.is_empty() || lo.len() > 4 || lo.len() != hi.len() {
                                return Err(Error::Structure(format!(
                                    "{kind} bounds must be 1–4 equal-length bytes"
                                )));
                            }
                            let (l, h) = (be(&lo), be(&hi));
                            if h < l {
                                return Err(Error::Structure(format!(
                                    "{kind} low bound exceeds high"
                                )));
                            }
                            count(1, &mut entries)?;
                            if notdef {
                                cm.notdef.push((lo.len() as u8, l, h, cid));
                            } else {
                                cm.ranges.push((lo.len() as u8, l, h, cid, cm.order));
                                cm.order += 1;
                            }
                        }
                    }
                    b"begincidchar" | b"beginnotdefchar" => {
                        let notdef = bytes.as_slice() == b"beginnotdefchar";
                        if ended {
                            cm.reader_dependent
                                .get_or_insert("definitions after endcmap");
                        }
                        let (end, kind): (&[u8], &'static str) = if notdef {
                            (b"endnotdefchar", "notdefchar")
                        } else {
                            (b"endcidchar", "cidchar")
                        };
                        stack.clear();
                        loop {
                            let code = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) if bytes == end => break,
                                Some(Token::HexString { value, .. }) => value,
                                Some(Token::LiteralString { value, .. }) => {
                                    cm.literal_codes = true;
                                    value
                                }
                                Some(_) => {
                                    // A stray token: readers skip it differently.
                                    cm.reader_dependent
                                        .get_or_insert("a stray token in a block");
                                    continue;
                                }
                                None => return Err(eof(kind)),
                            };
                            let cid = match lx.next_token()? {
                                Some(Token::Word { bytes, .. }) => int(&bytes)?,
                                _ => return Err(Error::Structure(format!("{kind} needs a CID"))),
                            };
                            if code.is_empty() || code.len() > 4 {
                                return Err(Error::Structure(format!(
                                    "{kind} code must be 1–4 bytes"
                                )));
                            }
                            count(1, &mut entries)?;
                            if notdef {
                                let c = be(&code);
                                cm.notdef.push((code.len() as u8, c, c, cid));
                            } else if cm.single.len() < cap as usize {
                                cm.single
                                    .insert((code.len() as u8, be(&code)), (cid, cm.order));
                                cm.order += 1;
                            }
                        }
                    }
                    b"usecmap" => {
                        // `/Name usecmap`
                        match stack.last() {
                            Some(Token::Name { value, .. }) => {
                                let name = value.clone();
                                Self::use_base(&mut cm, &name, limits, depth)?;
                            }
                            _ => {
                                return Err(Error::Structure("usecmap without a CMap name".into()))
                            }
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
                                    let w = int(bytes)?;
                                    if w > 1 {
                                        cm.reader_dependent
                                            .get_or_insert("a /WMode other than 0 or 1");
                                    }
                                    if wmode_seen.is_some_and(|v| v != w) {
                                        cm.reader_dependent.get_or_insert("/WMode defined twice");
                                    }
                                    wmode_seen = Some(w);
                                    cm.vertical = w == 1;
                                }
                            }
                        }
                        stack.clear();
                    }
                    b"endcmap" => {
                        ended = true;
                        stack.clear();
                    }
                    b"begincmap" => {
                        if begun || ended {
                            cm.reader_dependent.get_or_insert("a second begincmap");
                        }
                        begun = true;
                        stack.clear();
                    }
                    b"begin" | b"end" | b"pop" | b"findresource" | b"dict" | b"currentdict"
                    | b"defineresource" | b"CMapName" => stack.clear(),
                    b"beginbfchar" | b"beginbfrange" => {
                        cm.reader_dependent.get_or_insert(
                            "bf mappings in an encoding CMap (MuPDF and pdf.js apply them)",
                        );
                        // Not meaningful in an encoding CMap; skip to the end marker.
                        let end: &[u8] = match bytes.as_slice() {
                            b"beginbfchar" => b"endbfchar",
                            _ => b"endbfrange",
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
        // A code written as a literal string with an unescaped CR is a
        // line feed here (§7.3.4.2) and a CR to other readers: which
        // codes the CMap maps, and so where glyphs fall, would differ.
        if lx.odd_hex_strings != 0 {
            cm.reader_dependent.get_or_insert("an odd-length hex code");
        }
        // An Identity base with codespace ranges of its own: readers
        // split codes by one or the other. (Any code passed through the
        // base is reader-dependent: `Mapping::Fallthrough`.)
        if cm.identity_base && !cm.codespaces.is_empty() {
            cm.reader_dependent
                .get_or_insert("an Identity base with codespace ranges of its own");
        }
        // Codespace ranges of different lengths where a shorter one
        // holds the leading bytes of a longer one: readers split codes
        // by the shortest or the longest match.
        for (la, loa, hia) in &cm.codespaces {
            for (lb, lob, hib) in &cm.codespaces {
                if la < lb {
                    let shift = 8 * (*lb - *la) as u32;
                    let (plo, phi) = (lob >> shift, hib >> shift);
                    if plo <= *hia && phi >= *loa {
                        cm.reader_dependent.get_or_insert(
                            "codespace ranges of different lengths sharing leading bytes",
                        );
                    }
                }
            }
        }
        if lx.raw_cr_strings != 0 {
            return Err(Error::Unsupported {
                feature: "CMap code with an unescaped carriage return",
                detail: "readers do not agree on the bytes of such a string".into(),
            });
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
            if self.in_codespace(code, len) {
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

    /// `code` (of `len` bytes) lies in a codespace range — Identity's
    /// `<0000> <FFFF>` included when it is the base.
    fn in_codespace(&self, code: u32, len: usize) -> bool {
        (self.identity_base && len == 2)
            || self
                .codespaces
                .iter()
                .any(|(l, lo, hi)| *l as usize == len && code >= *lo && code <= *hi)
    }

    /// CID for `code` (of `len` bytes).
    pub(crate) fn cid(&self, code: u32, len: usize) -> u32 {
        self.lookup(code, len).0
    }

    /// CID for `code` (of `len` bytes) and how it was found (§9.7.6.3):
    /// a defined mapping (`cidchar`, `cidrange`), a code passed through
    /// an Identity base;
    /// else, for a code in the codespace, a notdef mapping; else CID 0.
    pub(crate) fn lookup(&self, code: u32, len: usize) -> (u32, Mapping) {
        // Bytes in no codespace range are invalid whatever a mapping
        // says of them: readers split and skip them differently.
        let valid = self.in_codespace(code, len);
        let found = |cid| {
            (
                cid,
                if valid {
                    Mapping::Defined
                } else {
                    Mapping::Invalid
                },
            )
        };
        if let Some(cid) = self.defined(code, len) {
            return found(cid);
        }
        // Defined only for another length: read by value, as pdf.js and
        // MuPDF do (Poppler does not; redaction refuses such a code,
        // `length_ambiguous`).
        if let Some(cid) = (1..=4usize)
            .filter(|l| *l != len)
            .find_map(|l| self.defined(code, l))
        {
            return found(cid);
        }
        if self.identity_base {
            // A code an embedded CMap passes through an Identity base:
            // MuPDF and pdf.js give it the code, Poppler CID 0 (or, with
            // no definitions of its own, reads one-byte codes).
            let m = if valid {
                Mapping::Fallthrough
            } else {
                Mapping::Invalid
            };
            return (code, m);
        }
        if !valid {
            return (0, Mapping::Invalid);
        }
        for (l, lo, hi, cid) in self.notdef.iter().rev() {
            if *l as usize == len && code >= *lo && code <= *hi {
                return (*cid, Mapping::Notdef);
            }
        }
        (0, Mapping::Unmapped)
    }

    /// The latest `cidchar` / `cidrange` definition of the `len`-byte
    /// `code`.
    fn defined(&self, code: u32, len: usize) -> Option<u32> {
        let single = self.single.get(&(len as u8, code)).copied();
        let range = self
            .ranges
            .iter()
            .rev()
            .find(|(l, lo, hi, _, _)| *l as usize == len && code >= *lo && code <= *hi)
            .map(|(_, lo, _, cid, o)| (cid + (code - lo), *o));
        match (single, range) {
            (Some(s), Some(r)) => Some(if s.1 > r.1 { s.0 } else { r.0 }),
            (s, r) => s.or(r).map(|x| x.0),
        }
    }

    /// Whether the same code value is also defined for another byte
    /// length with another CID: pdf.js and MuPDF key mappings by value,
    /// Poppler by bytes, so readers may take either.
    pub(crate) fn length_ambiguous(&self, code: u32, len: usize) -> bool {
        let mine = self.defined(code, len);
        (1..=4usize)
            .filter(|l| *l != len && (code as u64) < (1u64 << (8 * *l)))
            .any(|l| matches!(self.defined(code, l), Some(c) if Some(c) != mine))
    }

    /// `true` when every code is two bytes (so a `/ToUnicode` CMap
    /// should use two-byte codes as well).
    pub(crate) fn is_two_byte(&self) -> bool {
        !self.codespaces.is_empty() && self.codespaces.iter().all(|(l, _, _)| *l == 2)
    }
}

/// How [`CMap::lookup`] found a code's CID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mapping {
    /// Passed through an Identity base by an embedded CMap (readers
    /// disagree).
    Fallthrough,
    /// `cidchar` or `cidrange`.
    Defined,
    /// A notdef mapping: a code in the codespace with no defined one.
    Notdef,
    /// In the codespace, mapped by nothing: CID 0.
    Unmapped,
    /// In no codespace range: CID 0.
    Invalid,
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
        // A non-Identity predefined base: bundled with `cjk-fallback`,
        // refused otherwise.
        let r = CMap::parse(b"/90ms-RKSJ-H usecmap", None, &Limits::default());
        if cfg!(feature = "cjk-fallback") {
            let cm = r.unwrap();
            assert_eq!(cm.cid(0x8140, 2), 633, "U+3000 through 90ms-RKSJ-H");
            assert!(!cm.vertical);
        } else {
            assert!(matches!(r, Err(Error::Unsupported { .. })));
        }
        assert!(matches!(
            CMap::parse(
                b"1 begincidchar <41> 1 endcidchar",
                None,
                &Limits::default()
            ),
            Err(Error::Structure(_))
        ));
    }

    const CS: &str =
        "1 begincodespacerange <00> <7F> endcodespacerange 1 begincidchar <41> 36 endcidchar ";

    fn notdef(body: &str) -> Result<CMap, Error> {
        CMap::parse(format!("{CS}{body}").as_bytes(), None, &Limits::default())
    }

    #[test]
    fn notdef_char_and_range_map_to_one_cid() {
        let cm = notdef(
            "1 beginnotdefchar <42> 37 endnotdefchar \
             1 beginnotdefrange <50> <5F> 38 endnotdefrange",
        )
        .unwrap();
        assert_eq!(cm.lookup(0x41, 1), (36, Mapping::Defined));
        assert_eq!(cm.lookup(0x42, 1), (37, Mapping::Notdef));
        // Every code of a notdef range maps to the same CID.
        for c in [0x50, 0x57, 0x5F] {
            assert_eq!(cm.lookup(c, 1), (38, Mapping::Notdef));
        }
        assert_eq!(cm.lookup(0x43, 1), (0, Mapping::Unmapped));
        // Bytes in no codespace range: CID 0, never a notdef CID.
        assert_eq!(cm.next_code(b"\x90", 0), (0x90, 1));
        assert_eq!(cm.lookup(0x90, 1), (0, Mapping::Invalid));
        // A notdef range applies to codes of its own length only.
        assert_eq!(cm.lookup(0x0050, 2), (0, Mapping::Invalid));
    }

    #[test]
    fn defined_mappings_beat_notdef_and_later_notdef_wins() {
        let cm = notdef(
            "1 beginnotdefrange <40> <4F> 38 endnotdefrange \
             1 begincidrange <44> <45> 50 endcidrange \
             1 beginnotdefchar <46> 39 endnotdefchar",
        )
        .unwrap();
        assert_eq!(cm.cid(0x41, 1), 36);
        assert_eq!(cm.cid(0x45, 1), 51);
        assert_eq!(cm.lookup(0x46, 1), (39, Mapping::Notdef));
        assert_eq!(cm.lookup(0x47, 1), (38, Mapping::Notdef));
        // An Identity base maps every two-byte code: no notdef applies
        // here — but readers do not agree on a code passed through a base
        // the CMap adds mappings to (Poppler gives CID 0).
        let cm = CMap::parse(
            b"/Identity-H usecmap 1 beginnotdefrange <0000> <00FF> 7 endnotdefrange",
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(cm.lookup(0x0041, 2), (0x41, Mapping::Fallthrough));
        assert_eq!(cm.next_code(b"\x00\x41", 0), (0x41, 2));
    }

    #[test]
    fn notdef_through_usecmap() {
        // 90ms-RKSJ-H: `<00> <1f>` notdef → 231, `<20> <7d>` → 231…
        let r = CMap::parse(
            b"/90ms-RKSJ-H usecmap 1 beginnotdefchar <05> 9 endnotdefchar \
              1 beginnotdefrange <20> <20> 8 endnotdefrange",
            None,
            &Limits::default(),
        );
        if !cfg!(feature = "cjk-fallback") {
            assert!(matches!(r, Err(Error::Unsupported { .. })));
            return;
        }
        let cm = r.unwrap();
        assert_eq!(cm.lookup(0x01, 1), (231, Mapping::Notdef), "inherited");
        assert_eq!(cm.lookup(0x05, 1), (9, Mapping::Notdef), "overridden");
        // The base's defined mapping beats the derived notdef one.
        assert_eq!(cm.lookup(0x20, 1), (231, Mapping::Defined));
        assert_eq!(cm.lookup(0x41, 1), (264, Mapping::Defined));
    }

    #[test]
    fn malformed_notdef_blocks() {
        for body in [
            "1 beginnotdefrange <50> <4F> 38 endnotdefrange",
            "1 beginnotdefrange <50> <0060> 38 endnotdefrange",
            "1 beginnotdefrange <50> 38 endnotdefrange",
            "1 beginnotdefrange <50> <5F> /X endnotdefrange",
            "1 beginnotdefrange <50> <5F> 70000 endnotdefrange",
            "1 beginnotdefchar <42> <43> endnotdefchar",
            "1 beginnotdefchar <0102030405> 1 endnotdefchar",
            "1 beginnotdefchar <> 1 endnotdefchar",
        ] {
            assert!(matches!(notdef(body), Err(Error::Structure(_))), "{body}");
        }
        assert!(matches!(
            notdef("1 beginnotdefchar <42> 37"),
            Err(Error::UnexpectedEof { .. })
        ));
        // Codes written as literal strings are flagged like elsewhere.
        assert!(
            notdef("1 beginnotdefchar (B) 37 endnotdefchar")
                .unwrap()
                .literal_codes
        );
    }

    #[test]
    fn notdef_entries_count_against_the_limit() {
        let limits = Limits {
            max_text_codes: 3,
            ..Limits::default()
        };
        let src = format!("{CS}1 beginnotdefrange <50> <5F> 38 endnotdefrange");
        assert!(CMap::parse(src.as_bytes(), None, &limits).is_ok());
        let src = format!("{src} 2 beginnotdefchar <42> 1 <43> 2 endnotdefchar");
        assert!(matches!(
            CMap::parse(src.as_bytes(), None, &limits),
            Err(Error::LimitExceeded { .. })
        ));
    }

    /// Constructions readers read differently are flagged; a plain CMap
    /// and the bundled predefined ones are not.
    #[test]
    fn reader_dependent_constructions() {
        let l = Limits::default();
        let flag = |src: &str| {
            CMap::parse(src.as_bytes(), None, &l)
                .unwrap()
                .reader_dependent
        };
        let cs = "1 begincodespacerange <00> <7F> endcodespacerange ";
        assert_eq!(
            flag(&format!(
                "begincmap {cs} 1 begincidchar <41> 36 endcidchar endcmap"
            )),
            None
        );
        for src in [
            format!("{cs} 1 beginbfchar <41> <0042> endbfchar"),
            format!("begincmap {cs} endcmap 1 begincidchar <41> 36 endcidchar"),
            format!("begincmap {cs} endcmap begincmap endcmap"),
            "/Identity-H usecmap 1 begincodespacerange <0000> <FFFF> endcodespacerange".into(),
            format!("/WMode 2 def {cs}"),
            format!("/WMode 0 def /WMode 1 def {cs}"),
            format!("{cs} 1 begincidchar <4> 36 endcidchar"),
            format!("{cs} 1 begincidchar <41> 36 /x endcidchar"),
            "2 begincodespacerange <00> <7F> <4100> <41FF> endcodespacerange".into(),
        ] {
            assert!(flag(&src).is_some(), "{src}");
        }
        if cfg!(feature = "cjk-fallback") {
            for name in crate::cjk::predefined_cmap_names() {
                if let Some(cm) = CMap::predefined(name.as_bytes(), &l).unwrap() {
                    assert_eq!(cm.reader_dependent, None, "{name}");
                }
            }
        }
    }
}
