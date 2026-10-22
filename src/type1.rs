//! Adobe Type 1 (eexec `/FontFile`) font support — decoding and
//! charstring interpretation.
//!
//! A `/FontFile` stream is a Type 1 program: an ASCII font dict
//! followed by an `eexec`-encrypted binary section that holds the
//! `/Private` dict, `/Subrs`, and `/CharStrings`. Each charstring is
//! itself encrypted and is a tiny bytecode program that builds the
//! glyph's outline with moves/lines/curves plus stem hints and
//! subroutine calls (flex, dotsection, …).
//!
//! Layout (ISO 32000-1 §9.6.2 + Adobe "Type 1 Font Format" spec):
//!   [Length1 bytes]  ASCII font dict — cleartext, ends `currentfile eexec`
//!   [Length2 bytes]  eexec-encrypted — Private dict, Subrs, CharStrings
//!   [Length3 bytes]  trailer (`mark cleartomark` + zeroes)
//!
//! Security: the program and every charstring are untrusted input, so
//! parsing is bounded — eexec length, subr nesting depth, total
//! instructions, operand stack, and produced path elements all check
//! against [`Limits`]-style caps before any allocation happens.

use crate::error::Error;
use std::collections::HashMap;

/// `char name → decrypted charstring bytecode`.
type CharStringMap = HashMap<Vec<u8>, Vec<u8>>;

// --------------------------------------------------------------------
// Ciphers
// --------------------------------------------------------------------

/// The eexec cipher key (Type 1 spec).
const EEXEC_KEY: u16 = 55_665;
/// The CharString cipher key.
const CHARSTRING_KEY: u16 = 4_330;
/// `/lenIV` default: this many leading bytes of each charstring are
/// random padding and skipped. `-1` (from `/lenIV -1 def`) disables
/// encryption entirely.
const DEFAULT_LEN_IV: isize = 4;

/// Decrypt `data` in place with the Type 1 cipher: `c1 = c0 ^ (r>>8)`,
/// `r = (c0 + r) * 52845 + 22719` (mod 2^16). The key schedule feeds on
/// the *cipher* byte, so decryption is involutive — applying it to
/// ciphertext yields plaintext, and (because `r` only depends on the
/// input byte) applying it to plaintext does NOT round-trip. For tests
/// that need to *encrypt*, use [`encrypt`].
fn decrypt(data: &mut [u8], mut r: u16) {
    for c in data.iter_mut() {
        let c0 = *c;
        *c = c0 ^ (r >> 8) as u8;
        r = (c0 as u16)
            .wrapping_add(r)
            .wrapping_mul(52845)
            .wrapping_add(22719);
    }
}

/// Encrypt `data` in place — the inverse of [`decrypt`]. The key
/// schedule advances on the produced *cipher* byte.
#[cfg(test)]
fn encrypt(data: &mut [u8], mut r: u16) {
    for c in data.iter_mut() {
        let plain = *c;
        *c = plain ^ (r >> 8) as u8;
        r = (*c as u16)
            .wrapping_add(r)
            .wrapping_mul(52845)
            .wrapping_add(22719);
    }
}

// --------------------------------------------------------------------
// Font program (dict + subrs + charstrings)
// --------------------------------------------------------------------

/// A parsed Type 1 program ready for charstring interpretation.
/// Charstrings/subrs are kept *decrypted* as raw bytecode; outlines
/// are produced on demand by [`Type1Program::charstring`].
pub(crate) struct Type1Program {
    /// `char name → decrypted charstring bytecode`.
    charstrings: CharStringMap,
    /// `subr index → decrypted bytecode`.
    subrs: Vec<Vec<u8>>,
    /// `/FontMatrix` — glyph space → text space (default 0.001).
    font_matrix: crate::matrix::Matrix,
}

/// Parse a `/FontFile` Type 1 program (`Length1`/`Length2`/`Length3`
/// PFB-style layout). Returns `Err` for malformed structure or
/// `Error::Unsupported` for shapes we deliberately don't handle
/// (e.g. `/CharStrings` that aren't simple `name len RD bytes` pairs).
pub(crate) fn parse_type1_program(program: &[u8]) -> Result<Type1Program, Error> {
    // ---- split Length1 / Length2 / Length3 -----------------------
    // The boundaries come from /Length1-3 in the font descriptor, but
    // many producers leave them unreliable; we locate the cleartext
    // "currentfile eexec" sentinel which is the robust delimiter.
    let (dict_end, eexec_start) = find_eexec_boundary(program)?;
    let _dict = &program[..dict_end];
    let mut eexec = program[eexec_start..].to_vec();

    // ---- eexec decrypt -------------------------------------------
    decrypt(&mut eexec, EEXEC_KEY);
    // The decrypted Private section begins with up-to-`lenIV` random
    // bytes then `dup /Private ...`. The lenIV random prefix is part
    // of eexec too — but the eexec stream's own first bytes are the
    // lenIV padding; we find the real `dup` start.
    let priv_start = find_private_start(&eexec)?;
    let private = &eexec[priv_start..];

    // ---- scan the Private dict for the fields we need ------------
    // The Private section contains binary CharStrings data — only the
    // textual *header* (up to the first binary blob) is valid UTF-8.
    // Parse fields from a lossy view; CharStrings are located by bytes.
    let priv_txt = String::from_utf8_lossy(private);
    let len_iv = parse_isize(&priv_txt, "/lenIV", DEFAULT_LEN_IV);
    let font_matrix = parse_matrix(&priv_txt, "/FontMatrix")
        .unwrap_or(crate::matrix::Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0));
    // `/FontBBox` is parsed for completeness of the header but the
    // renderer derives bounds from the outlines themselves.
    let _font_bbox = parse_bbox(&priv_txt, "/FontBBox");

    // ---- /Subrs --------------------------------------------------
    let subrs = parse_subrs(private, len_iv)?;

    // ---- /CharStrings ---------------------------------------------
    let (charstrings, _n_glyphs) = parse_charstrings(private, len_iv)?;
    // A program without a single CharString cannot draw anything — it
    // is a broken (or fake) font, not an "all glyphs empty" one. Refuse
    // rather than silently render blanks for every code.
    if charstrings.is_empty() {
        return Err(Error::Structure(
            "Type 1 program has no /CharStrings entries".into(),
        ));
    }

    Ok(Type1Program {
        charstrings,
        subrs,
        font_matrix,
    })
}

/// Locate `currentfile eexec` — returns `(dict_end, eexec_start)`.
/// The encrypted section begins right after the `eexec` keyword plus
/// its line terminator (which is *not* part of the cipher stream).
fn find_eexec_boundary(program: &[u8]) -> Result<(usize, usize), Error> {
    // PFB: the binary part starts with 0x80 after the ascii dict.
    // PFA: "currentfile eexec" is the marker. Handle both.
    if let Some(i) = find_sub(program, b"eexec") {
        // advance past "eexec" and one line terminator
        let mut j = i + 5;
        if j < program.len() && program[j] == b'\r' {
            j += 1;
        }
        if j < program.len() && program[j] == b'\n' {
            j += 1;
        }
        // PFB: a 0x80 magic byte may follow
        if j < program.len() && program[j] == 0x80 {
            j += 1;
        }
        return Ok((i, j));
    }
    Err(Error::Structure(
        "Type1 program has no 'eexec' boundary — not a /FontFile program".into(),
    ))
}

/// Find where the decrypted Private dict really starts. The first
/// bytes are `lenIV` random padding (usually 4, often 0x00), then
/// `dup` or `/Private`.
fn find_private_start(eexec: &[u8]) -> Result<usize, Error> {
    // Scan the first 64 bytes for `dup`/`Private`/`dict`.
    for kw in [b"dup".as_slice(), b"Private".as_slice(), b"dict".as_slice()] {
        if let Some(i) = eexec.get(..64).and_then(|w| find_sub(w, kw)) {
            return Ok(i);
        }
    }
    // Fallback: skip a small padding run of NULs.
    let i = eexec.iter().position(|&b| b != 0).unwrap_or(0);
    if i < eexec.len() {
        Ok(i)
    } else {
        Err(Error::Structure("Type1 eexec section is empty".into()))
    }
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Index just past the `n`-th whitespace-delimited token of `seg`
/// (tokens are maximal runs of non-whitespace bytes). `None` when the
/// slice runs out first. Used to step over `dup idx len RD` / `len RD`
/// without assuming the RD procedure is literally spelled `RD`.
fn skip_tokens(seg: &[u8], n: usize) -> Option<usize> {
    let mut i = 0usize;
    for _ in 0..n {
        while i < seg.len() && seg[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= seg.len() {
            return None;
        }
        while i < seg.len() && !seg[i].is_ascii_whitespace() {
            i += 1;
        }
    }
    Some(i)
}

// --------------------------------------------------------------------
// Private-dict field scanners
// --------------------------------------------------------------------

/// `/NAME <int>` — first integer after the name. `/lenIV` etc.
fn parse_isize(s: &str, key: &str, default: isize) -> isize {
    if let Some(i) = s.find(key) {
        let rest = &s[i + key.len()..];
        let tok: String = rest
            .chars()
            .skip_while(|c| c.is_whitespace() || *c == '[' || *c == ']')
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '+')
            .collect();
        tok.parse().unwrap_or(default)
    } else {
        default
    }
}

/// `/NAME [a b c d]` — array of 4 numbers → bbox.
fn parse_bbox(s: &str, key: &str) -> Option<[f64; 4]> {
    let i = s.find(key)?;
    let rest = &s[i + key.len()..];
    let l = rest.find('[')?;
    let r = rest[l..].find(']')?;
    let inner = &rest[l + 1..l + r];
    let nums: Vec<f64> = inner
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    if nums.len() >= 4 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

/// `/FontMatrix [a b c d e f]` — 6 numbers.
fn parse_matrix(s: &str, key: &str) -> Option<crate::matrix::Matrix> {
    let i = s.find(key)?;
    let rest = &s[i + key.len()..];
    let l = rest.find('[')?;
    let r = rest[l..].find(']')?;
    let inner = &rest[l + 1..l + r];
    let nums: Vec<f64> = inner
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    if nums.len() >= 6 {
        Some(crate::matrix::Matrix::new(
            nums[0], nums[1], nums[2], nums[3], nums[4], nums[5],
        ))
    } else {
        None
    }
}

// --------------------------------------------------------------------
// /Subrs  &  /CharStrings  — `name len RD <encrypted bytes> ND` pairs
// --------------------------------------------------------------------

/// Walk a `/Subrs N array ... dup index len RD bytes ...` region and
/// decrypt each charstring-bytecode blob.
fn parse_subrs(private: &[u8], len_iv: isize) -> Result<Vec<Vec<u8>>, Error> {
    let start = match find_sub(private, b"/Subrs") {
        Some(i) => i,
        None => return Ok(Vec::new()),
    };
    // /Subrs N array — read N from the (ASCII) header
    let after = String::from_utf8_lossy(&private[start + 6..start + 6 + 32]);
    let n: usize = after
        .split_whitespace()
        .next()
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    let mut out: Vec<Vec<u8>> = vec![Vec::new(); n];
    // Stop at /CharStrings so we don't run into the glyph section.
    let end = find_sub(&private[start..], b"/CharStrings")
        .map(|i| start + i)
        .unwrap_or(private.len());
    // All offsets are absolute into `private`. Find "dup <i> <len> RD".
    let mut pos = start;
    let mut count = 0;
    while count < n {
        let d = match find_sub(&private[pos..end], b"dup") {
            Some(d) => pos + d,
            None => break,
        };
        // parse "dup <i> <len> RD" — read the ASCII prefix
        let seg = &private[d..end];
        let text = String::from_utf8_lossy(&seg[..seg.len().min(64)]);
        let mut parts = text.split_whitespace();
        let _dup = parts.next(); // "dup"
        let idx: usize = match parts.next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => break,
        };
        let len: usize = match parts.next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => break,
        };
        // The 4th token (`dup idx len RD`) is the RD procedure — `RD`,
        // `-|` (cm-super and other t1utils output) or whatever the font
        // named it — followed by exactly one space, then `len` bytes.
        let Some(after_rd) = skip_tokens(seg, 4) else {
            break;
        };
        let data_start = d + after_rd + 1;
        if data_start + len > end {
            break;
        }
        let mut bytes = private[data_start..data_start + len].to_vec();
        decrypt(&mut bytes, CHARSTRING_KEY);
        // strip lenIV random prefix
        let skip = if len_iv >= 0 { len_iv as usize } else { 0 };
        let body = if skip <= bytes.len() {
            bytes.split_off(skip)
        } else {
            Vec::new()
        };
        if idx < out.len() {
            out[idx] = body;
        }
        count += 1;
        pos = data_start + len;
        if pos >= end {
            break;
        }
    }
    Ok(out)
}

/// Walk `/CharStrings N dict dup begin /name len RD bytes ND ...` and
/// decrypt each glyph program. Returns `(map, n_declared)`.
fn parse_charstrings(private: &[u8], len_iv: isize) -> Result<(CharStringMap, usize), Error> {
    let start = match find_sub(private, b"/CharStrings") {
        Some(i) => i,
        None => return Ok((HashMap::new(), 0)),
    };
    let region = &private[start..];
    let mut map = HashMap::new();
    let mut pos = 0;
    // entries look like:  /name 44 RD <binary> ND
    while let Some(slash) = find_sub(&region[pos..], b"/") {
        let name_start = pos + slash + 1;
        // name end: whitespace
        let name_end = region[name_start..]
            .iter()
            .position(|c| c.is_ascii_whitespace())
            .map(|i| name_start + i);
        let Some(ne) = name_end else { break };
        let name = region[name_start..ne].to_vec();
        // skip /CharStrings itself / known dict words
        if name == b"CharStrings" || name == b"ND" || name == b"RD" {
            pos = ne;
            continue;
        }
        // expect: <len> RD <bytes> ND — read the ASCII length prefix
        let seg = &region[ne..];
        let head = String::from_utf8_lossy(&seg[..seg.len().min(64)]);
        let len: usize = match head.split_whitespace().next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => {
                // not a "/name len RD" entry — keep scanning
                pos = ne;
                continue;
            }
        };
        // `len RD` — the 2nd token is the RD procedure under whatever
        // name the font gave it (`RD`, `-|`, …); one space, then bytes.
        let Some(after_rd) = skip_tokens(seg, 2) else {
            break;
        };
        let ds = ne + after_rd + 1;
        if ds + len > region.len() {
            break;
        }
        let mut bytes = region[ds..ds + len].to_vec();
        if len_iv >= 0 {
            decrypt(&mut bytes, CHARSTRING_KEY);
        }
        let skip = if len_iv >= 0 { len_iv as usize } else { 0 };
        let body = if skip <= bytes.len() {
            bytes.split_off(skip)
        } else {
            Vec::new()
        };
        map.insert(name, body);
        pos = ds + len;
        if pos >= region.len() {
            break;
        }
    }
    let n = map.len();
    Ok((map, n))
}

// --------------------------------------------------------------------
// Accessors used by the Font model
// --------------------------------------------------------------------

impl Type1Program {
    pub(crate) fn font_matrix(&self) -> crate::matrix::Matrix {
        self.font_matrix
    }
    /// Number of decoded charstrings — used by tests and as a rough
    /// `/CharStrings` sanity check.
    #[cfg(test)]
    pub(crate) fn n_glyphs(&self) -> usize {
        self.charstrings.len()
    }
    /// `true` if a charstring exists for this glyph name.
    pub(crate) fn has_glyph(&self, name: &[u8]) -> bool {
        self.charstrings.contains_key(name)
    }

    /// Interpret the charstring for `name`, producing outline moves in
    /// `builder` (glyph space units). Returns `(had_outline, width)`.
    /// `Ok((false,_))` = empty/.notdef. `Err` = malformed program or a
    /// feature (seac/flex we reject) — the caller maps to Unsupported.
    pub(crate) fn charstring(
        &self,
        name: &[u8],
        builder: &mut dyn ttf_parser::OutlineBuilder,
    ) -> Result<(bool, f64), Error> {
        let code = match self.charstrings.get(name) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return Ok((false, 0.0)),
        };
        let mut st = Interp::new();
        let had = st.run(&code, self, builder)?;
        Ok((had, st.width))
    }
}

// --------------------------------------------------------------------
// Type 1 charstring interpreter
// --------------------------------------------------------------------

/// Flex (`callothersubr` subrs 0/1/2 + `setcurrentpoint`) — a flex is
/// drawn as two back-to-back cubic curves that flatten a serif/thin
/// region. The charstring calls `1` (begin), then `2` for each control
/// point, then `0` (end) and `setcurrentpoint` — the interpreter must
/// accumulate the two control points of each curve while the flex
/// `othersubr` machinery just feeds back the *current point* at each
/// step (see the `pop`/`setcurrentpoint` handling).
#[derive(Default)]
enum Flex {
    #[default]
    None,
    /// Collecting flex curve points. `n` = number of (x,y) pairs seen
    /// so far (the spec emits 7 pairs: begin + 6 curve coords). The
    /// coords themselves are absolute (relative to the start point),
    /// accumulated here so `0`/`setcurrentpoint` can emit the curves.
    Active,
}

struct Interp {
    /// The charstring operand stack (numbers). Operands are consumed
    /// *from the bottom* (stack[0..n]) per the Type 1 spec — only
    /// `div`, `callsubr`, `callothersubr`, `pop`, `return`, and
    /// `setcurrentpoint` read them differently.
    stack: Vec<f64>,
    call_depth: u32,
    ops: u64,
    /// Current point in glyph space.
    x: f64,
    y: f64,
    /// Left sidebearing (`hsbw`/`sbw` set it). The current point is
    /// `(pos_x + lsb.x, pos_y + lsb.y)` after width ops.
    lsb: (f64, f64),
    /// Advance width (from `hsbw`/`sbw`).
    width: f64,
    /// `(pos_x, pos_y)` — the origin the current point is offset from.
    pos: (f64, f64),
    /// Declared stem-hint pair count (each `hstem`/`vstem` = 1 pair,
    /// `hstem3`/`vstem3` = 3). `hintmask`/`cntrmask` skip
    /// `(n+7)/8` mask bytes after clearing their operands.
    n_stems: usize,
    /// Whether the path has any painted (non-move) element.
    open: bool,
    /// Whether a moveto has been seen (a curve/line before the first
    /// moveto is a syntax error per spec §4.3).
    have_moveto: bool,
    flex: Flex,
    /// `seac` args when `endchar` carries accent/base charcodes.
    seac_parts: Option<(f64, f64, f64, f64, f64)>,
    /// Number of flex curve points queued by subr 2 (0..7).
    flex_vectors: usize,
    /// Accumulated flex curve endpoints (up to 7 absolute points).
    flex_pts: [(f64, f64); 7],
    /// Results a handled `callothersubr` pushed onto the stack — the
    /// following `pop`s consume them without touching `ps_stack`.
    known_results: usize,
    /// Results an *unhandled* `callothersubr` left for `pop`.
    unknown_results: usize,
    /// The arguments of that unhandled call, re-exposed one per `pop`
    /// in their original order (FreeType semantics).
    pending_args: Vec<f64>,
}

// type-1 charstring limits (bounded; tied to Limits at the call site
// via the caller's caps — we hard-code conservative defaults here).
const MAX_OPS: u64 = 200_000;
const MAX_STACK: usize = 48;
const MAX_CALL_DEPTH: u32 = 10;
const MAX_PATH_ELEMS: usize = 100_000;
const MAX_FLEX_VECTORS: usize = 7;

impl Interp {
    fn push(&mut self, v: f64) -> Result<(), Error> {
        if self.stack.len() >= MAX_STACK {
            return Err(Error::LimitExceeded {
                limit: "type1 operand stack",
                max: MAX_STACK as u64,
            });
        }
        self.stack.push(v);
        Ok(())
    }
    /// Pop the top value (used only by `div`, `callsubr`,
    /// `callothersubr`, `pop` per the spec's "different" reading).
    fn pop(&mut self) -> Result<f64, Error> {
        self.stack
            .pop()
            .ok_or_else(|| Error::Structure("type1 stack underflow".into()))
    }
    /// Drain the whole operand stack, returning its contents
    /// bottom-up (the order operands were pushed in — `arg1 arg2 …`).
    fn popall(&mut self) -> Vec<f64> {
        std::mem::take(&mut self.stack)
    }
    /// Pop a single operand off the bottom of the stack (the only
    /// operand). Used by single-arg moveto ops.
    fn popall_one(&mut self) -> Result<f64, Error> {
        if self.stack.is_empty() {
            return Err(Error::Structure("type1 stack underflow".into()));
        }
        let v = self.stack[0];
        self.stack.clear();
        Ok(v)
    }
    fn clear(&mut self) {
        self.stack.clear();
    }
    /// Advance the current point — `hsbw`/`sbw`/`rmoveto`/`moveto`
    /// offset from `(pos + lsb)`; drawing ops accumulate.
    fn move_to(&mut self, b: &mut dyn ttf_parser::OutlineBuilder) {
        b.move_to(self.x as f32, self.y as f32);
        self.have_moveto = true;
    }
    fn need_moveto(&self) -> Result<(), Error> {
        if !self.have_moveto {
            return Err(Error::Structure("type1 draw op before first moveto".into()));
        }
        Ok(())
    }

    fn new() -> Self {
        Interp {
            stack: Vec::with_capacity(MAX_STACK),
            call_depth: 0,
            ops: 0,
            x: 0.0,
            y: 0.0,
            lsb: (0.0, 0.0),
            width: 0.0,
            pos: (0.0, 0.0),
            n_stems: 0,
            open: false,
            have_moveto: false,
            flex: Flex::None,
            seac_parts: None,
            flex_vectors: 0,
            flex_pts: [(0.0, 0.0); MAX_FLEX_VECTORS],
            known_results: 0,
            unknown_results: 0,
            pending_args: Vec::new(),
        }
    }

    fn run(
        &mut self,
        code: &[u8],
        prog: &Type1Program,
        b: &mut dyn ttf_parser::OutlineBuilder,
    ) -> Result<bool, Error> {
        let mut i = 0;
        let mut elems = 0usize;
        while i < code.len() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return Err(Error::LimitExceeded {
                    limit: "type1 instructions",
                    max: MAX_OPS,
                });
            }
            let c = code[i];
            i += 1;
            match c {
                // ---- numbers ---------------------------------------
                32..=246 => {
                    self.push(c as f64 - 139.0)?;
                }
                247..=250 => {
                    if i >= code.len() {
                        return Err(Error::Structure("type1 number truncated".into()));
                    }
                    let c2 = code[i];
                    i += 1;
                    self.push((c as f64 - 247.0) * 256.0 + c2 as f64 + 108.0)?;
                }
                251..=254 => {
                    if i >= code.len() {
                        return Err(Error::Structure("type1 number truncated".into()));
                    }
                    let c2 = code[i];
                    i += 1;
                    self.push(-((c as f64 - 251.0) * 256.0) - c2 as f64 - 108.0)?;
                }
                255 => {
                    if i + 4 > code.len() {
                        return Err(Error::Structure("type1 number truncated".into()));
                    }
                    let v = i32::from_be_bytes([code[i], code[i + 1], code[i + 2], code[i + 3]]);
                    i += 4;
                    // Type 1: a plain 32-bit *integer* (Type 1 spec
                    // §6.2). Only Type 2 charstrings read this form as
                    // 16.16 fixed — treating it that way here turned
                    // every operand above 1131 (e.g. hint-replacement
                    // Subr indices in cm-super fonts) into ~0.
                    self.push(v as f64)?;
                }
                28 => {
                    // `shortint` exists only in Type 2 charstrings; in a
                    // Type 1 program byte 28 is an undefined operator.
                    return Err(Error::Unsupported {
                        feature: "Type1 charstring op",
                        detail: "opcode 28 (Type 2 shortint) is not a Type 1 operator".into(),
                    });
                }
                12 => {
                    // escaped op
                    if i >= code.len() {
                        return Err(Error::Structure("type1 escape truncated".into()));
                    }
                    let e = code[i];
                    i += 1;
                    self.escaped(e, b)?;
                }
                // ---- stem hints (recorded only as a stem-pair count;
                //       rasterization uses AA, not hinting) ------------
                1 | 3 => {
                    // hstem / vstem — operands are stem-position pairs;
                    // each pair = one stem. pop all pending operands.
                    let pending = self.stack.len();
                    self.n_stems += pending / 2;
                    self.clear();
                }
                18 | 23 => {
                    // hstemhm / vstemhm — hint-mask variants; same operand
                    // shape, recorded for the hintmask byte count.
                    let pending = self.stack.len();
                    self.n_stems += pending / 2;
                    self.clear();
                }
                19 | 20 => {
                    // hintmask / cntrmask — pop all pending operands
                    // (which may themselves declare stems), then skip
                    // (nstems+7)/8 mask bytes.
                    let pending = self.stack.len();
                    self.n_stems += pending / 2;
                    self.clear();
                    let mask = (self.n_stems + 7) / 8;
                    if i + mask > code.len() {
                        return Err(Error::Structure("type1 hintmask truncated".into()));
                    }
                    i += mask;
                }
                // ---- moves ------------------------------------------
                4 => {
                    // vmoveto: dy
                    let dy = self.popall_one()?;
                    self.y += dy;
                    if !matches!(self.flex, Flex::Active) {
                        self.move_to(b);
                    }
                }
                21 => {
                    // rmoveto: dx dy
                    let a = self.popall();
                    self.x += a[0];
                    self.y += a[1];
                    if !matches!(self.flex, Flex::Active) {
                        self.move_to(b);
                    }
                }
                22 => {
                    // hmoveto: dx
                    let dx = self.popall_one()?;
                    self.x += dx;
                    if !matches!(self.flex, Flex::Active) {
                        self.move_to(b);
                    }
                }
                // ---- lines ------------------------------------------
                5 => {
                    // rlineto: dx dy (pairs)
                    let a = self.popall();
                    if a.len() % 2 != 0 {
                        return Err(Error::Structure("type1 rlineto odd operands".into()));
                    }
                    for p in a.chunks_exact(2) {
                        self.need_moveto()?;
                        self.x += p[0];
                        self.y += p[1];
                        b.line_to(self.x as f32, self.y as f32);
                        elems += 1;
                        self.open = true;
                    }
                }
                6 => {
                    // hlineto: dx dy dx dy ... starts horizontal
                    let a = self.popall();
                    let mut horiz = true;
                    for &v in &a {
                        self.need_moveto()?;
                        if horiz {
                            self.x += v;
                        } else {
                            self.y += v;
                        }
                        horiz = !horiz;
                        b.line_to(self.x as f32, self.y as f32);
                        elems += 1;
                        self.open = true;
                    }
                }
                7 => {
                    // vlineto: dy dx alternating, starts vertical
                    let a = self.popall();
                    let mut vert = true;
                    for &v in &a {
                        self.need_moveto()?;
                        if vert {
                            self.y += v;
                        } else {
                            self.x += v;
                        }
                        vert = !vert;
                        b.line_to(self.x as f32, self.y as f32);
                        elems += 1;
                        self.open = true;
                    }
                }
                // ---- curves ------------------------------------------
                8 => {
                    // rrcurveto: {dxa dya dxb dyb dxc dyc}+
                    let a = self.popall();
                    if a.len() % 6 != 0 {
                        return Err(Error::Structure("type1 rrcurveto bad operand count".into()));
                    }
                    for p in a.chunks_exact(6) {
                        self.need_moveto()?;
                        let x1 = self.x + p[0];
                        let y1 = self.y + p[1];
                        let x2 = x1 + p[2];
                        let y2 = y1 + p[3];
                        self.x = x2 + p[4];
                        self.y = y2 + p[5];
                        b.curve_to(
                            x1 as f32,
                            y1 as f32,
                            x2 as f32,
                            y2 as f32,
                            self.x as f32,
                            self.y as f32,
                        );
                        elems += 1;
                        self.open = true;
                    }
                }
                30 => {
                    // vhcurveto: dy1 dx2 dy2 dx3 (ends horizontal)
                    let a = self.popall();
                    self.need_moveto()?;
                    elems += self.vh_hv_curve(&a, true, b)?;
                }
                31 => {
                    // hvcurveto: dx1 dx2 dy2 dy3 (ends vertical)
                    let a = self.popall();
                    self.need_moveto()?;
                    elems += self.vh_hv_curve(&a, false, b)?;
                }
                // ---- subroutines, width, endchar ---------------------
                10 => {
                    // callsubr n — operand is the *top* of stack
                    let n = self.pop()? as i64;
                    if self.call_depth >= MAX_CALL_DEPTH {
                        return Err(Error::LimitExceeded {
                            limit: "type1 subr depth",
                            max: MAX_CALL_DEPTH as u64,
                        });
                    }
                    let idx = if n >= 0 {
                        n as usize
                    } else {
                        return Err(Error::Structure("type1 negative subr index".into()));
                    };
                    let subr = prog.subrs.get(idx).cloned().ok_or_else(|| {
                        Error::Structure(format!("type1 subr {idx} out of range"))
                    })?;
                    self.call_depth += 1;
                    self.run(&subr, prog, b)?;
                    self.call_depth -= 1;
                }
                11 => {
                    // return from subr — valid only inside a call
                    if self.call_depth == 0 {
                        return Err(Error::Structure("type1 'return' outside subroutine".into()));
                    }
                    return Ok(self.open);
                }
                13 => {
                    // hsbw: sbx wx — set width + current point at
                    // (pos.x + sbx, pos.y). Operands bottom-up.
                    let a = self.popall();
                    if a.len() < 2 {
                        return Err(Error::Structure("type1 hsbw underflow".into()));
                    }
                    self.lsb.0 += a[0];
                    self.width = a[1];
                    self.x = self.pos.0 + a[0];
                    self.y = self.pos.1;
                    self.have_moveto = false; // width set; path not started
                }
                9 => {
                    // closepath — close current contour.
                    if self.have_moveto {
                        b.close();
                    }
                    self.have_moveto = false;
                }
                14 => {
                    // endchar — close path; run seac composite if any.
                    b.close();
                    if let Some(parts) = self.seac_parts.take() {
                        self.composite_seac(parts, prog, b)?;
                    }
                    return Ok(self.open);
                }
                _ => {
                    // 0,2,15,16,17,24-29 and any other byte not a
                    // Type 1 operator → strict Unsupported (24-27 are
                    // Type2-only and must not be silently accepted).
                    return Err(Error::Unsupported {
                        feature: "Type1 charstring op",
                        detail: format!("opcode {c} is not a Type 1 operator"),
                    });
                }
            }
            if elems > MAX_PATH_ELEMS {
                return Err(Error::LimitExceeded {
                    limit: "type1 path elements",
                    max: MAX_PATH_ELEMS as u64,
                });
            }
        }
        Ok(self.open)
    }

    fn escaped(&mut self, e: u8, b: &mut dyn ttf_parser::OutlineBuilder) -> Result<(), Error> {
        match e {
            0 => {
                // dotsection — hint, no operands
            }
            1 => {
                // vstem3 — 3 vertical stem pairs (6 operands)
                let a = self.popall();
                if a.len() != 6 {
                    return Err(Error::Structure("type1 vstem3 operands".into()));
                }
                self.n_stems += 3;
            }
            2 => {
                // hstem3 — 3 horizontal stem pairs
                let a = self.popall();
                if a.len() != 6 {
                    return Err(Error::Structure("type1 hstem3 operands".into()));
                }
                self.n_stems += 3;
            }
            6 => {
                // seac: asb adx ady bchar achar — record for endchar
                let a = self.popall();
                if a.len() != 5 {
                    return Err(Error::Structure("type1 seac operands".into()));
                }
                self.seac_parts = Some((a[0], a[1], a[2], a[3], a[4]));
            }
            7 => {
                // sbw: sbx sby wx wy — set width + current point.
                let a = self.popall();
                if a.len() != 4 {
                    return Err(Error::Structure("type1 sbw operands".into()));
                }
                self.lsb = (a[0], a[1]);
                self.width = a[2];
                self.x = self.pos.0 + a[0];
                self.y = self.pos.1 + a[1];
                self.have_moveto = false;
            }
            12 => {
                // div: num1 num2 → num1/num2 (top-of-stack operands)
                let b_ = self.pop()?;
                let a = self.pop()?;
                if b_ == 0.0 {
                    return Err(Error::Structure("type1 div by zero".into()));
                }
                self.push(a / b_)?;
            }
            16 => {
                // callothersubr: operands are `arg1..argN N othersubr#`
                // (Type 1 spec §8.3) — othersubr# is the *top* of the
                // stack, N just below it, then the N args. The standard
                // Subrs make this concrete: Subrs[0] = `3 0 callothersubr`
                // (flex end, 3 args), Subrs[1] = `0 1 callothersubr`.
                let subr_no = self.pop()? as i64;
                let arg_cnt = self.pop()? as i64;
                if arg_cnt < 0 || self.stack.len() < arg_cnt as usize {
                    return Err(Error::Structure(format!(
                        "type1 callothersubr underflow: othersubr {subr_no} wants {arg_cnt} \
                         args, stack has {}",
                        self.stack.len()
                    )));
                }
                // pop the arg_cnt operands (bottom-up order preserved)
                let base = self.stack.len() - arg_cnt as usize;
                let args: Vec<f64> = self.stack[base..].to_vec();
                self.stack.truncate(base);
                self.known_results = 0;
                self.unknown_results = 0;
                self.pending_args.clear();
                match subr_no {
                    0 => {
                        // flex end — push current point for setcurrentpoint
                        if arg_cnt != 3 || self.flex_vectors != 7 {
                            return Err(Error::Structure("type1 unexpected flex end".into()));
                        }
                        self.push(self.x)?;
                        self.push(self.y)?;
                        self.known_results = 2;
                    }
                    1 => {
                        // flex begin
                        if arg_cnt != 0 {
                            return Err(Error::Structure("type1 unexpected flex begin".into()));
                        }
                        self.flex = Flex::Active;
                        self.flex_vectors = 0;
                        self.need_moveto()?;
                    }
                    2 => {
                        // flex add vector — accumulate the point
                        if arg_cnt != 0 {
                            return Err(Error::Structure("type1 unexpected flex vector".into()));
                        }
                        if !matches!(self.flex, Flex::Active) {
                            return Err(Error::Structure(
                                "type1 flex vector before flex begin".into(),
                            ));
                        }
                        let idx = self.flex_vectors;
                        self.flex_vectors += 1;
                        // idx 0 just records the anchor point; 1..6 are
                        // control points, idx 6 is the last endpoint.
                        if idx > 0 && idx < MAX_FLEX_VECTORS {
                            self.flex_pts[idx] = (self.x, self.y);
                        }
                    }
                    3 => {
                        // hint replacement: `subr# 1 3 callothersubr pop
                        // callsubr` — OtherSubrs[3] leaves subr# on the
                        // PostScript stack for the following `pop`, so
                        // hand the argument straight back.
                        if arg_cnt != 1 {
                            return Err(Error::Structure("type1 unexpected othersubr 3".into()));
                        }
                        self.push(args[0])?;
                        self.known_results = 1;
                    }
                    12 | 13 => {
                        // counter-control hints — clear stack
                        self.clear();
                    }
                    20 => {
                        if arg_cnt != 2 {
                            return Err(Error::Structure("type1 othersubr 20".into()));
                        }
                        self.push(args[0] + args[1])?;
                        self.known_results = 1;
                    }
                    21 => {
                        if arg_cnt != 2 {
                            return Err(Error::Structure("type1 othersubr 21".into()));
                        }
                        self.push(args[0] - args[1])?;
                        self.known_results = 1;
                    }
                    22 => {
                        if arg_cnt != 2 {
                            return Err(Error::Structure("type1 othersubr 22".into()));
                        }
                        self.push(args[0] * args[1])?;
                        self.known_results = 1;
                    }
                    23 => {
                        if arg_cnt != 2 || args[1] == 0.0 {
                            return Err(Error::Structure("type1 othersubr 23".into()));
                        }
                        self.push(args[0] / args[1])?;
                        self.known_results = 1;
                    }
                    27 => {
                        // if <val1> <= <val2> push res1 else res2
                        if arg_cnt != 4 {
                            return Err(Error::Structure("type1 othersubr 27".into()));
                        }
                        let r = if args[2] <= args[3] { args[0] } else { args[1] };
                        self.push(r)?;
                        self.known_results = 1;
                    }
                    _ => {
                        // Unhandled othersubr (unknown PostScript code):
                        // FreeType's rule is that the arguments stay
                        // available and each following `pop` re-exposes
                        // the next one in order — the net effect of a
                        // PS procedure that leaves its inputs untouched.
                        self.unknown_results = arg_cnt as usize;
                        self.pending_args = args.clone();
                    }
                }
            }
            17 => {
                // pop — pull one result left by a handled othersubr, or
                // re-expose the next argument of an unhandled one.
                if self.known_results > 0 {
                    self.known_results -= 1;
                    // value already pushed by the handler — nothing to do
                } else if self.unknown_results > 0 {
                    self.unknown_results -= 1;
                    let v = if self.pending_args.is_empty() {
                        0.0
                    } else {
                        self.pending_args.remove(0)
                    };
                    self.push(v)?;
                } else {
                    return Err(Error::Structure(
                        "type1 pop without pending othersubr result".into(),
                    ));
                }
            }
            33 => {
                // setcurrentpoint: x y — after a flex `callothersubr 0`
                // pushed the endpoint; emit the accumulated flex curves.
                let a = self.popall();
                if a.len() < 2 {
                    return Err(Error::Structure("type1 setcurrentpoint".into()));
                }
                self.x = a[0];
                self.y = a[1];
                if matches!(self.flex, Flex::Active) {
                    self.finish_flex(b);
                    self.flex = Flex::None;
                }
            }
            _ => {
                return Err(Error::Unsupported {
                    feature: "Type1 escaped op",
                    detail: format!("escape opcode 12/{e} is not implemented"),
                })
            }
        }
        Ok(())
    }

    /// Emit the two flex curves once `setcurrentpoint` completes a flex
    /// sequence. The 7 recorded points become two cubic curves: the
    /// first joins points 1→3, the second 3→6 (flex vectors per spec).
    fn finish_flex(&mut self, b: &mut dyn ttf_parser::OutlineBuilder) {
        if self.flex_vectors != 7 {
            self.flex_pts = [(0.0, 0.0); MAX_FLEX_VECTORS];
            self.flex_vectors = 0;
            return;
        }
        let p = &self.flex_pts;
        // First curve: current point → pts[1..3] → pts[3]
        b.curve_to(
            p[1].0 as f32,
            p[1].1 as f32,
            p[2].0 as f32,
            p[2].1 as f32,
            p[3].0 as f32,
            p[3].1 as f32,
        );
        // Second curve: pts[4..5] → pts[6]
        b.curve_to(
            p[4].0 as f32,
            p[4].1 as f32,
            p[5].0 as f32,
            p[5].1 as f32,
            p[6].0 as f32,
            p[6].1 as f32,
        );
        self.x = p[6].0;
        self.y = p[6].1;
        self.flex_pts = [(0.0, 0.0); MAX_FLEX_VECTORS];
        self.flex_vectors = 0;
        self.open = true;
    }

    /// `vhcurveto` (30: `dy1 dx2 dy2 dx3`) and `hvcurveto` (31: `dx1
    /// dx2 dy2 dy3`) — one Bézier whose first tangent is vertical
    /// resp. horizontal and whose *last* tangent is the other axis
    /// (Type 1 spec §6.4: `vhcurveto` ≡ `0 dy1 dx2 dy2 dx3 0
    /// rrcurveto`, `hvcurveto` ≡ `dx1 0 dx2 dy2 0 dy3 rrcurveto`).
    /// Type 1 takes exactly four operands; the alternating multi-curve
    /// forms belong to Type 2 and are refused here.
    fn vh_hv_curve(
        &mut self,
        args: &[f64],
        vert_first: bool,
        b: &mut dyn ttf_parser::OutlineBuilder,
    ) -> Result<usize, Error> {
        if args.len() != 4 {
            return Err(Error::Structure(format!(
                "type1 {} takes 4 operands, got {}",
                if vert_first { "vhcurveto" } else { "hvcurveto" },
                args.len()
            )));
        }
        let (x1, y1, x2, y2, x3, y3);
        if vert_first {
            y1 = self.y + args[0];
            x1 = self.x;
            x2 = x1 + args[1];
            y2 = y1 + args[2];
            x3 = x2 + args[3];
            y3 = y2;
        } else {
            // Ending this curve horizontally instead of vertically
            // distorted every round glyph of the arXiv CM fonts.
            x1 = self.x + args[0];
            y1 = self.y;
            x2 = x1 + args[1];
            y2 = y1 + args[2];
            x3 = x2;
            y3 = y2 + args[3];
        }
        b.curve_to(
            x1 as f32, y1 as f32, x2 as f32, y2 as f32, x3 as f32, y3 as f32,
        );
        self.x = x3;
        self.y = y3;
        self.open = true;
        Ok(1)
    }

    /// `seac` — composite glyph from a base + accent charstring.
    /// `asb adx ady bchar achar`: the accent is placed at
    /// `(adx - asb, ady)` relative to the base origin. `bchar`/`achar`
    /// are StandardEncoding charcodes → resolve to glyph names, then
    /// re-run those two charstrings non-nested (spec forbids seac
    /// inside seac).
    fn composite_seac(
        &mut self,
        parts: (f64, f64, f64, f64, f64),
        prog: &Type1Program,
        b: &mut dyn ttf_parser::OutlineBuilder,
    ) -> Result<(), Error> {
        let (asb, adx, ady, bch, ach) = parts;
        // StandardEncoding charcode → glyph name.
        let base_name = std_encoding_name(bch as i64)
            .ok_or_else(|| Error::Structure("type1 seac base charcode".into()))?;
        let acc_name = std_encoding_name(ach as i64)
            .ok_or_else(|| Error::Structure("type1 seac accent charcode".into()))?;

        // seac weirdness (fontTools): the accent origin is shifted by
        // `adx + sbx - asb` where `sbx` is *this* glyph's own
        // left-bearing (set by its hsbw/sbw), and `ady` vertically.
        let accent_dx = adx + self.lsb.0 - asb;
        let accent_dy = ady;

        // Run the base charstring into the builder (current offset).
        if let Some(code) = prog.charstrings.get(base_name) {
            if !code.is_empty() {
                let code = code.clone();
                let saved = (self.pos, self.x, self.y, self.have_moveto, self.call_depth);
                self.pos = (0.0, 0.0);
                self.x = 0.0;
                self.y = 0.0;
                self.have_moveto = false;
                self.call_depth = 0;
                self.lsb = (0.0, 0.0);
                self.run(&code, prog, b)?;
                let _ = saved;
            }
        }
        // Then the accent at its offset.
        if let Some(code) = prog.charstrings.get(acc_name) {
            if !code.is_empty() {
                let code = code.clone();
                self.pos = (accent_dx, accent_dy);
                self.x = accent_dx;
                self.y = accent_dy;
                self.have_moveto = false;
                self.call_depth = 0;
                self.lsb = (0.0, 0.0);
                self.run(&code, prog, b)?;
            }
        }
        Ok(())
    }
}

/// StandardEncoding charcode → glyph name (Adobe table). `None` for
/// unassigned slots. Used only by `seac` (which references the
/// *standard* encoding regardless of the font's `/Encoding`).
pub(crate) fn std_encoding_name(code: i64) -> Option<&'static [u8]> {
    static NAMES: [&[u8]; 256] = {
        let mut t: [&[u8]; 256] = [b".notdef"; 256];
        // The 46 assigned StandardEncoding names (code → name), from
        // the Type 1 spec appendix. Most of 0-31 and 40-59 are .notdef.
        let assigned: &[(usize, &[u8])] = &[
            (32, b"space"),
            (33, b"exclam"),
            (34, b"quotedbl"),
            (35, b"numbersign"),
            (36, b"dollar"),
            (37, b"percent"),
            (38, b"ampersand"),
            (39, b"quoteright"),
            (40, b"parenleft"),
            (41, b"parenright"),
            (42, b"asterisk"),
            (43, b"plus"),
            (44, b"comma"),
            (45, b"hyphen"),
            (46, b"period"),
            (47, b"slash"),
            (48, b"zero"),
            (49, b"one"),
            (50, b"two"),
            (51, b"three"),
            (52, b"four"),
            (53, b"five"),
            (54, b"six"),
            (55, b"seven"),
            (56, b"eight"),
            (57, b"nine"),
            (58, b"colon"),
            (59, b"semicolon"),
            (60, b"less"),
            (61, b"equal"),
            (62, b"greater"),
            (63, b"question"),
            (64, b"at"),
            (65, b"A"),
            (66, b"B"),
            (67, b"C"),
            (68, b"D"),
            (69, b"E"),
            (70, b"F"),
            (71, b"G"),
            (72, b"H"),
            (73, b"I"),
            (74, b"J"),
            (75, b"K"),
            (76, b"L"),
            (77, b"M"),
            (78, b"N"),
            (79, b"O"),
            (80, b"P"),
            (81, b"Q"),
            (82, b"R"),
            (83, b"S"),
            (84, b"T"),
            (85, b"U"),
            (86, b"V"),
            (87, b"W"),
            (88, b"X"),
            (89, b"Y"),
            (90, b"Z"),
            (91, b"bracketleft"),
            (92, b"backslash"),
            (93, b"bracketright"),
            (94, b"asciicircum"),
            (95, b"underscore"),
            (96, b"quoteleft"),
            (97, b"a"),
            (98, b"b"),
            (99, b"c"),
            (100, b"d"),
            (101, b"e"),
            (102, b"f"),
            (103, b"g"),
            (104, b"h"),
            (105, b"i"),
            (106, b"j"),
            (107, b"k"),
            (108, b"l"),
            (109, b"m"),
            (110, b"n"),
            (111, b"o"),
            (112, b"p"),
            (113, b"q"),
            (114, b"r"),
            (115, b"s"),
            (116, b"t"),
            (117, b"u"),
            (118, b"v"),
            (119, b"w"),
            (120, b"x"),
            (121, b"y"),
            (122, b"z"),
            (123, b"braceleft"),
            (124, b"bar"),
            (125, b"braceright"),
            (126, b"asciitilde"),
            (161, b"exclamdown"),
            (162, b"cent"),
            (163, b"sterling"),
            (164, b"fraction"),
            (165, b"yen"),
            (166, b"florin"),
            (167, b"section"),
            (168, b"currency"),
            (169, b"quotesingle"),
            (170, b"quotedblleft"),
            (171, b"guillemotleft"),
            (172, b"guilsinglleft"),
            (173, b"guilsinglright"),
            (174, b"fi"),
            (175, b"fl"),
            (177, b"endash"),
            (178, b"dagger"),
            (179, b"daggerdbl"),
            (180, b"periodcentered"),
            (182, b"paragraph"),
            (183, b"bullet"),
            (184, b"quotesinglbase"),
            (185, b"quotedblbase"),
            (186, b"quotedblright"),
            (187, b"guillemotright"),
            (188, b"ellipsis"),
            (189, b"perthousand"),
            (191, b"questiondown"),
            (193, b"grave"),
            (194, b"acute"),
            (195, b"circumflex"),
            (196, b"tilde"),
            (197, b"macron"),
            (198, b"breve"),
            (199, b"dotaccent"),
            (200, b"dieresis"),
            (202, b"ring"),
            (203, b"cedilla"),
            (205, b"hungarumlaut"),
            (206, b"ogonek"),
            (207, b"caron"),
            (208, b"emdash"),
            (225, b"AE"),
            (227, b"ordfeminine"),
            (232, b"Lslash"),
            (233, b"Oslash"),
            (234, b"OE"),
            (235, b"ordmasculine"),
            (241, b"ae"),
            (245, b"dotlessi"),
            (248, b"lslash"),
            (249, b"oslash"),
            (250, b"oe"),
            (251, b"germandbls"),
        ];
        let mut i = 0;
        while i < assigned.len() {
            let (c, n) = assigned[i];
            t[c] = n;
            i += 1;
        }
        t
    };
    if (0..256).contains(&code) {
        let n = NAMES[code as usize];
        if n != b".notdef" {
            Some(n)
        } else {
            None
        }
    } else {
        None
    }
}

// --------------------------------------------------------------------
// tests
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal recording `OutlineBuilder` — captures op + endpoint.
    struct Cap(Vec<(u8, f32, f32)>);
    impl ttf_parser::OutlineBuilder for Cap {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push((b'm', x, y));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push((b'l', x, y));
        }
        fn quad_to(&mut self, _x1: f32, _y1: f32, x: f32, y: f32) {
            self.0.push((b'q', x, y));
        }
        fn curve_to(&mut self, _x1: f32, _y1: f32, _x2: f32, _y2: f32, x: f32, y: f32) {
            self.0.push((b'c', x, y));
        }
        fn close(&mut self) {
            self.0.push((b'z', 0.0, 0.0));
        }
    }

    /// Number → charstring bytes (the Type-1 integer encoding).
    fn enc_num(v: i64) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![(v + 139) as u8]
        } else if (108..=1131).contains(&v) {
            let w = v - 108;
            vec![((w >> 8) + 247) as u8, (w & 0xFF) as u8]
        } else if (-1131..=-108).contains(&v) {
            let w = -v - 108;
            vec![((w >> 8) + 251) as u8, (w & 0xFF) as u8]
        } else {
            // 4-byte int (255)
            let mut out = vec![255];
            out.extend_from_slice(&(v as i32).to_be_bytes());
            out
        }
    }

    /// Build a complete `/FontFile` program wrapping one charstring.
    /// `subrs` is an optional `/Subrs N array dup i len RD …` blob.
    fn make_prog(cs_name: &[u8], charstring: &[u8], subrs: &[Vec<u8>]) -> Vec<u8> {
        let dict = b"%!PS-AdobeFont-1.0: T 1.0\n/FontName /T def\n\
            /FontType 1 def\n/FontMatrix [0.001 0 0 0.001 0 0] def\n\
            /Encoding 256 array 0 1 255 {1 index exch /.notdef put} for def\n\
            currentdict end\ncurrentfile eexec\n";
        // encrypt each charstring/subr body with the charstring cipher.
        let enc = |b: &[u8]| {
            let mut v = vec![0u8; 4]; // lenIV=4 padding
            v.extend_from_slice(b);
            encrypt(&mut v, CHARSTRING_KEY);
            v
        };
        let cs = enc(charstring);
        // The Private section is ASCII text with binary charstring
        // blobs spliced in after each `RD `; `chunks` records where
        // each blob goes (offset into `hdr`) so the text and the
        // binary can be assembled in one pass.
        let mut private_bytes: Vec<u8> = Vec::new();
        let mut hdr = String::from("dup /Private 8 dict dup begin /lenIV 4 def /BlueValues [] def");
        let mut chunks: Vec<(usize, Vec<u8>)> = Vec::new(); // (pos after hdr, bytes)
        if !subrs.is_empty() {
            hdr += &format!("/Subrs {} array ", subrs.len());
            for (i, s) in subrs.iter().enumerate() {
                let e = enc(s);
                let decl = format!("dup {} {} RD ", i, e.len());
                hdr += &decl;
                chunks.push((hdr.len(), e));
            }
        }
        hdr += &format!(
            "/CharStrings 2 dict dup begin /.notdef {} RD ",
            enc(b"\x0e").len() // .notdef = endchar
        );
        chunks.push((hdr.len(), enc(b"\x0e")));
        hdr += &format!("ND /{} {} RD ", String::from_utf8_lossy(cs_name), cs.len());
        chunks.push((hdr.len(), cs));
        hdr += "ND end end readonly put\n";
        // splice
        let mut off = 0usize;
        chunks.sort_by_key(|(p, _)| *p);
        for (pos, bytes) in chunks {
            private_bytes.extend_from_slice(&hdr.as_bytes()[off..pos]);
            private_bytes.extend_from_slice(&bytes);
            off = pos;
        }
        private_bytes.extend_from_slice(&hdr.as_bytes()[off..]);
        let mut prog = dict.to_vec();
        let mut e = private_bytes;
        encrypt(&mut e, EEXEC_KEY);
        prog.extend_from_slice(&e);
        prog
    }

    fn outline(name: &[u8], cs: &[u8], subrs: &[Vec<u8>]) -> (Vec<(u8, f32, f32)>, f64) {
        let prog = make_prog(name, cs, subrs);
        let p = parse_type1_program(&prog).expect("parse");
        let mut b = Cap(Vec::new());
        let (had, w) = p.charstring(name, &mut b).expect("interp");
        assert!(had, "expected an outline");
        (b.0, w)
    }

    #[test]
    fn parses_eexec_and_draws_square() {
        // hsbw 0 500; rmoveto 10 10; rlineto 100 0 0 100 -100 0; closepath; endchar
        let mut cs = enc_num(0);
        cs.extend(enc_num(500));
        cs.push(13); // hsbw
        cs.extend(enc_num(10));
        cs.extend(enc_num(10));
        cs.push(21); // rmoveto
        cs.extend(enc_num(100));
        cs.extend(enc_num(0));
        cs.extend(enc_num(0));
        cs.extend(enc_num(100));
        cs.extend(enc_num(-100));
        cs.extend(enc_num(0));
        cs.push(5); // rlineto
        cs.push(9); // closepath
        cs.push(14); // endchar
        let (ops, w) = outline(b"sq", &cs, &[]);
        assert_eq!(w, 500.0);
        assert_eq!(ops[0], (b'm', 10.0, 10.0));
        assert!(ops.iter().any(|(o, ..)| *o == b'l'), "no lineto: {ops:?}");
        assert!(ops.iter().any(|(o, ..)| *o == b'z'), "no close: {ops:?}");
    }

    /// The four standard Subrs every real Type 1 font starts with
    /// (flex end / flex begin / flex vector / no-op) plus the usual
    /// hint-replacement helper `1 3 callothersubr pop callsubr`.
    fn standard_subrs() -> Vec<Vec<u8>> {
        let mut s0 = enc_num(3);
        s0.extend(enc_num(0));
        s0.extend([12, 16, 12, 17, 12, 17, 12, 33, 11]); // callothersubr pop pop setcurrentpoint return
        let mut s1 = enc_num(0);
        s1.extend(enc_num(1));
        s1.extend([12, 16, 11]);
        let mut s2 = enc_num(0);
        s2.extend(enc_num(2));
        s2.extend([12, 16, 11]);
        let s3 = vec![11];
        let mut s4 = enc_num(1);
        s4.extend(enc_num(3));
        s4.extend([12, 16, 12, 17, 10, 11]); // callothersubr pop callsubr return
        vec![s0, s1, s2, s3, s4]
    }

    #[test]
    fn five_byte_numbers_are_integers_not_fixed() {
        // `0 2000 hsbw` — 2000 needs the `255 + int32` form, which Type 1
        // reads as an integer (Type 2 would read 16.16 fixed = 0.03).
        let mut cs = enc_num(0);
        cs.extend(enc_num(2000));
        assert_eq!(cs[1], 255, "test must exercise the 5-byte form");
        cs.push(13);
        cs.extend(enc_num(10));
        cs.extend(enc_num(10));
        cs.push(21);
        cs.extend(enc_num(50));
        cs.extend(enc_num(0));
        cs.push(5);
        cs.push(9);
        cs.push(14);
        let (_, w) = outline(b"w", &cs, &[]);
        assert_eq!(w, 2000.0);
    }

    #[test]
    fn hint_replacement_via_othersubr_3() {
        // Subr 5 = `1 61 hstem return`; glyph does `5 4 callsubr` —
        // OtherSubrs[3] must hand the subr# (5) back to the `pop` so
        // the following `callsubr` runs Subr 5, not Subr 0.
        let mut subrs = standard_subrs();
        let mut s5 = enc_num(1);
        s5.extend(enc_num(61));
        s5.extend([1, 11]);
        subrs.push(s5);
        let mut cs = enc_num(0);
        cs.extend(enc_num(500));
        cs.push(13); // hsbw
        cs.extend(enc_num(5));
        cs.extend(enc_num(4));
        cs.push(10); // callsubr 4 → hint replacement → callsubr 5
        cs.extend(enc_num(10));
        cs.extend(enc_num(10));
        cs.push(21);
        cs.extend(enc_num(50));
        cs.extend(enc_num(0));
        cs.push(5);
        cs.push(9);
        cs.push(14);
        let (ops, w) = outline(b"h", &cs, &subrs);
        assert_eq!(w, 500.0);
        assert_eq!(ops[0], (b'm', 10.0, 10.0), "{ops:?}");
        assert!(ops.iter().any(|(o, ..)| *o == b'l'), "{ops:?}");
    }

    #[test]
    fn flex_via_standard_subrs_emits_two_curves() {
        // `1 callsubr`, 7 × `dx dy rmoveto 2 callsubr`, `fd x y 0 callsubr`.
        let subrs = standard_subrs();
        let mut cs = enc_num(0);
        cs.extend(enc_num(500));
        cs.push(13);
        cs.extend(enc_num(0));
        cs.extend(enc_num(0));
        cs.push(21); // rmoveto 0 0
        cs.extend(enc_num(1));
        cs.push(10); // 1 callsubr → flex begin
                     // reference point then 6 control/end points, all via rmoveto
        for (dx, dy) in [
            (50, 0),
            (10, 5),
            (10, 5),
            (10, 0),
            (10, 0),
            (10, -5),
            (10, -5),
        ] {
            cs.extend(enc_num(dx));
            cs.extend(enc_num(dy));
            cs.push(21);
            cs.extend(enc_num(2));
            cs.push(10); // 2 callsubr
        }
        cs.extend(enc_num(50)); // flex height
        cs.extend(enc_num(110)); // end x
        cs.extend(enc_num(0)); // end y
        cs.extend(enc_num(0));
        cs.push(10); // 0 callsubr → flex end
        cs.push(9);
        cs.push(14);
        let (ops, _) = outline(b"f", &cs, &subrs);
        let curves = ops.iter().filter(|(o, ..)| *o == b'c').count();
        assert_eq!(curves, 2, "flex must emit two cubics: {ops:?}");
        let last_c = ops.iter().rev().find(|(o, ..)| *o == b'c').unwrap();
        assert!(
            (last_c.1 - 110.0).abs() < 1e-6 && last_c.2.abs() < 1e-6,
            "{ops:?}"
        );
    }

    #[test]
    fn callsubr_runs_nested() {
        // subr 0 = rmoveto 5 5; rlineto 50 0; return
        let mut s0 = enc_num(5);
        s0.extend(enc_num(5));
        s0.push(21);
        s0.extend(enc_num(50));
        s0.extend(enc_num(0));
        s0.push(5);
        s0.push(11); // return
                     // glyph = hsbw 0 0; callsubr 0; endchar
        let mut cs = enc_num(0);
        cs.extend(enc_num(0));
        cs.push(13);
        cs.extend(enc_num(0));
        cs.push(10); // callsubr 0
        cs.push(14);
        let (ops, _) = outline(b"g", &cs, &[s0]);
        assert!(ops.iter().any(|(o, ..)| *o == b'l'));
    }

    #[test]
    fn vhcurveto_draws_curve() {
        // hsbw; rmoveto; vhcurveto dy1 dx2 dy2 dx3; endchar
        let mut cs = enc_num(0);
        cs.extend(enc_num(0));
        cs.push(13);
        cs.extend(enc_num(0));
        cs.extend(enc_num(0));
        cs.push(21);
        cs.extend(enc_num(43));
        cs.extend(enc_num(-35));
        cs.extend(enc_num(35));
        cs.extend(enc_num(-43));
        cs.push(30); // vhcurveto
        cs.push(14);
        let (ops, _) = outline(b"c", &cs, &[]);
        assert!(ops.iter().any(|(o, ..)| *o == b'c'), "no curve: {ops:?}");
    }

    #[test]
    fn hvcurveto_ends_vertical_vhcurveto_ends_horizontal() {
        // hsbw 0 0; rmoveto 100 100; hvcurveto 10 20 30 40 → end (130,170)
        // (dx1=10, dx2=20, dy2=30, dy3=40); then vhcurveto 5 6 7 8 →
        // end (130+6+8, 170+5+7) = (144,182).
        let mut cs = enc_num(0);
        cs.extend(enc_num(0));
        cs.push(13);
        cs.extend(enc_num(100));
        cs.extend(enc_num(100));
        cs.push(21);
        for v in [10, 20, 30, 40] {
            cs.extend(enc_num(v));
        }
        cs.push(31); // hvcurveto
        for v in [5, 6, 7, 8] {
            cs.extend(enc_num(v));
        }
        cs.push(30); // vhcurveto
        cs.push(9);
        cs.push(14);
        let (ops, _) = outline(b"c", &cs, &[]);
        let curves: Vec<_> = ops.iter().filter(|(o, ..)| *o == b'c').collect();
        assert_eq!(curves.len(), 2, "{ops:?}");
        assert_eq!((curves[0].1, curves[0].2), (130.0, 170.0), "{ops:?}");
        assert_eq!((curves[1].1, curves[1].2), (144.0, 182.0), "{ops:?}");
        // Type 1 has no alternating 8-operand form.
        let mut bad = enc_num(0);
        bad.extend(enc_num(0));
        bad.push(13);
        bad.extend(enc_num(0));
        bad.extend(enc_num(0));
        bad.push(21);
        for v in [1, 2, 3, 4, 5, 6, 7, 8] {
            bad.extend(enc_num(v));
        }
        bad.push(31);
        bad.push(14);
        let prog = make_prog(b"bad", &bad, &[]);
        let p = parse_type1_program(&prog).unwrap();
        let mut b = Cap(Vec::new());
        assert!(matches!(
            p.charstring(b"bad", &mut b),
            Err(Error::Structure(_))
        ));
    }

    #[test]
    fn hintmask_consumes_mask_bytes() {
        // hstem a b; vstem c d; hintmask <mask>; rmoveto; …; endchar
        let mut cs = enc_num(0);
        cs.extend(enc_num(0));
        cs.push(13); // hsbw
        cs.extend(enc_num(0));
        cs.extend(enc_num(50));
        cs.push(1); // hstem  → n_stems=1
        cs.extend(enc_num(0));
        cs.extend(enc_num(50));
        cs.push(3); // vstem  → n_stems=2
        cs.extend(enc_num(10));
        cs.extend(enc_num(20));
        cs.push(19); // hintmask → +1 stem (10 20), n_stems=3, mask=(3+7)/8=1 byte
        cs.push(0xFF); // the mask byte
        cs.extend(enc_num(5));
        cs.extend(enc_num(5));
        cs.push(21); // rmoveto
        cs.extend(enc_num(10));
        cs.extend(enc_num(0));
        cs.push(5); // rlineto
        cs.push(14);
        let (ops, _) = outline(b"h", &cs, &[]);
        assert!(
            ops.iter().any(|(o, ..)| *o == b'l'),
            "hintmask desync: {ops:?}"
        );
    }

    #[test]
    fn seac_composite_draws_two_parts() {
        // base 'A' and accent 'acute' both exist → seac combines them
        let base = {
            let mut c = enc_num(0);
            c.extend(enc_num(600));
            c.push(13);
            c.extend(enc_num(0));
            c.extend(enc_num(0));
            c.push(21);
            c.extend(enc_num(50));
            c.extend(enc_num(0));
            c.push(5);
            c.push(14);
            c
        };
        let acc = {
            let mut c = enc_num(0);
            c.extend(enc_num(0));
            c.push(13);
            c.extend(enc_num(10));
            c.extend(enc_num(10));
            c.push(21);
            c.extend(enc_num(10));
            c.extend(enc_num(10));
            c.push(5);
            c.push(14);
            c
        };
        // composite glyph: hsbw; seac 0 0 0 A(65) acute(194); endchar
        let mut cs = enc_num(0);
        cs.extend(enc_num(600));
        cs.push(13);
        cs.extend(enc_num(0));
        cs.extend(enc_num(0));
        cs.extend(enc_num(0));
        cs.extend(enc_num(65)); // bchar = 'A'
        cs.extend(enc_num(194)); // achar = 'acute'
        cs.push(12);
        cs.push(6); // seac
        cs.push(14); // endchar
                     // Build a program with base+acc+composite charstrings.
        let dict = b"%!PS-AdobeFont-1.0: T 1.0\n/FontName /T def\n/FontType 1 def\n\
            /FontMatrix [0.001 0 0 0.001 0 0] def\ncurrentdict end\ncurrentfile eexec\n";
        let enc = |b: &[u8]| {
            let mut v = vec![0u8; 4];
            v.extend_from_slice(b);
            encrypt(&mut v, CHARSTRING_KEY);
            v
        };
        let cb = enc(&base);
        let ca = enc(&acc);
        let cc = enc(&cs);
        let mut hdr = String::from(
            "dup /Private 8 dict dup begin /lenIV 4 def /CharStrings 4 dict dup begin ",
        );
        let mut chunks: Vec<(usize, Vec<u8>)> = Vec::new();
        for (name, enc_bytes) in [
            (".notdef", enc(b"\x0e")),
            ("A", cb),
            ("acute", ca),
            ("Aacute", cc),
        ] {
            hdr += &format!("/{} {} RD ", name, enc_bytes.len());
            chunks.push((hdr.len(), enc_bytes));
        }
        hdr += "ND end end readonly put\n";
        let mut priv_bytes = Vec::new();
        let mut off = 0;
        chunks.sort_by_key(|(p, _)| *p);
        for (pos, bytes) in chunks {
            priv_bytes.extend_from_slice(&hdr.as_bytes()[off..pos]);
            priv_bytes.extend_from_slice(&bytes);
            off = pos;
        }
        priv_bytes.extend_from_slice(&hdr.as_bytes()[off..]);
        let mut prog = dict.to_vec();
        encrypt(&mut priv_bytes, EEXEC_KEY);
        prog.extend_from_slice(&priv_bytes);
        let p = parse_type1_program(&prog).expect("parse");
        let mut b = Cap(Vec::new());
        let (had, _w) = p.charstring(b"Aacute", &mut b).expect("seac interp");
        assert!(had, "seac produced no outline");
        // base + accent each emit ≥1 line → at least 2 line_to calls
        assert!(
            b.0.iter().filter(|(o, ..)| *o == b'l').count() >= 2,
            "seac should draw base+accent: {:?}",
            b.0
        );
    }

    #[test]
    fn bad_opcode_is_unsupported_not_panic() {
        // opcode 24 (rcurveline) is Type2-only → must error, not draw.
        let mut cs = enc_num(0);
        cs.extend(enc_num(0));
        cs.push(13);
        cs.push(24); // invalid Type1 op
        cs.push(14);
        let prog = make_prog(b"bad", &cs, &[]);
        let p = parse_type1_program(&prog).expect("parse");
        let mut b = Cap(Vec::new());
        match p.charstring(b"bad", &mut b) {
            Err(Error::Unsupported { .. }) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn stack_overflow_is_bounded() {
        // Push > MAX_STACK operands → LimitExceeded, not a panic/overflow.
        let mut cs = Vec::new();
        for _ in 0..(MAX_STACK + 4) {
            cs.extend(enc_num(0));
        }
        cs.push(14);
        let prog = make_prog(b"ov", &cs, &[]);
        let p = parse_type1_program(&prog).expect("parse");
        let mut b = Cap(Vec::new());
        match p.charstring(b"ov", &mut b) {
            Err(Error::LimitExceeded { .. }) => {}
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn missing_glyph_returns_notdef() {
        let cs = {
            let mut c = enc_num(0);
            c.extend(enc_num(0));
            c.push(13);
            c.push(14);
            c
        };
        let prog = make_prog(b"present", &cs, &[]);
        let p = parse_type1_program(&prog).expect("parse");
        let mut b = Cap(Vec::new());
        let (had, _) = p.charstring(b"absent", &mut b).expect("absent");
        assert!(!had, "absent glyph should be empty");
    }

    #[test]
    fn real_cmsy10_charstring() {
        let data =
            std::fs::read("tests/fixtures/eexec_cmsy10.bin").expect("fixture eexec_cmsy10.bin");
        let p = parse_type1_program(&data).expect("parse real CMBSY10");
        assert!(p.n_glyphs() > 0);
        let mut b = Cap(Vec::new());
        let (had, w) = p.charstring(b"periodcentered", &mut b).expect("interp");
        assert!(had, "periodcentered empty");
        assert_eq!(w, 319.0, "periodcentered width");
        assert!(!b.0.is_empty(), "no outline emitted");
    }
}
