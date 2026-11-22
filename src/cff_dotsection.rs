//! A CFF program without its `dotsection` operators.
//!
//! `dotsection` (escape operator 12 0) is a Type 1 hint that the Type 2
//! charstring format keeps only as deprecated, to be ignored (Adobe
//! TN 5177, appendix B). Programs converted from Type 1 fonts still carry
//! it — typically in glyphs with dots or accents — and the CFF
//! interpreter we use refuses a charstring that contains it, so the
//! glyph is drawn empty. [`without_dotsection`] rewrites the program with
//! the operator removed from every charstring and subroutine; nothing
//! else changes (the same dictionaries, strings, charset, encoding and
//! glyph order), so the outlines are those of a reader that ignores it.
//!
//! Charstrings are tokenised as the interpreter reads them: numbers,
//! operators, and the mask bytes after `hintmask` / `cntrmask`, whose
//! length follows from the stem hints declared before them — through
//! the subroutines a charstring calls, in the order it calls them. A
//! subroutine read with two different stem counts at a mask, a call the
//! tokeniser cannot resolve, a malformed structure or more work than
//! [`MAX_TOKENS`] leaves the program as it is (`None`).

use std::collections::HashMap;

/// Tokens read across the whole program before giving up (each
/// subroutine call is followed, so a program can cost more than its size).
const MAX_TOKENS: usize = 4_000_000;
/// The Type 2 limit on subroutine nesting.
const MAX_DEPTH: usize = 10;

/// `program` with every `dotsection` removed, or `None` when it has none
/// or cannot be rewritten exactly (see the module documentation).
pub(crate) fn without_dotsection(program: &[u8]) -> Option<Vec<u8>> {
    if !program.windows(2).any(|w| w == [12, 0]) {
        return None;
    }
    let cff = Cff::parse(program)?;
    let mut walk = Walk {
        cff: &cff,
        tokens: 0,
        cuts: HashMap::new(),
        stems_at_mask: HashMap::new(),
    };
    for (i, private) in (0..cff.charstrings.len()).map(|i| (i, cff.private_for(i))) {
        let private = private?;
        let mut st = State::default();
        walk.body(Body::Glyph(i), private, &mut st, 0)?;
    }
    if walk.cuts.values().all(|c| c.is_empty()) {
        return None;
    }
    cff.rebuild(&walk.cuts)
}

/// A charstring body: a glyph, a global subroutine, or a local
/// subroutine of a Private DICT (by its index in `Cff::privates`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Body {
    Glyph(usize),
    Global(usize),
    Local(usize, usize),
}

#[derive(Default)]
struct State {
    /// Numbers on the argument stack.
    args: usize,
    /// The last number pushed (a subroutine call's index).
    last: Option<i32>,
    /// Stem hints declared so far.
    stems: usize,
}

struct Walk<'a> {
    cff: &'a Cff<'a>,
    tokens: usize,
    /// Byte offsets of each `dotsection` (its escape byte) per body.
    cuts: HashMap<Body, Vec<usize>>,
    /// For a subroutine with a mask: the stem count it was read with.
    stems_at_mask: HashMap<(Body, usize), usize>,
}

/// Whether the body ended the glyph (`endchar`) or returned.
enum End {
    Return,
    EndChar,
}

impl Walk<'_> {
    fn body(&mut self, b: Body, private: usize, st: &mut State, depth: usize) -> Option<End> {
        if depth > MAX_DEPTH {
            return None;
        }
        let data = match b {
            Body::Glyph(i) => self.cff.charstrings.get(i)?,
            Body::Global(i) => self.cff.gsubrs.get(i)?,
            Body::Local(p, i) => self.cff.privates.get(p)?.subrs.get(i)?,
        };
        let mut cuts = Vec::new();
        let mut i = 0usize;
        let mut ended = None;
        while i < data.len() {
            self.tokens += 1;
            if self.tokens > MAX_TOKENS {
                return None;
            }
            let v = data[i];
            match v {
                28 => {
                    let n = i16::from_be_bytes([*data.get(i + 1)?, *data.get(i + 2)?]);
                    st.args += 1;
                    st.last = Some(n as i32);
                    i += 3;
                }
                255 => {
                    data.get(i + 4)?;
                    st.args += 1;
                    st.last = None; // 16.16 fixed: never a subroutine index
                    i += 5;
                }
                32..=246 => {
                    st.args += 1;
                    st.last = Some(v as i32 - 139);
                    i += 1;
                }
                247..=250 => {
                    st.args += 1;
                    st.last = Some((v as i32 - 247) * 256 + *data.get(i + 1)? as i32 + 108);
                    i += 2;
                }
                251..=254 => {
                    st.args += 1;
                    st.last = Some(-(v as i32 - 251) * 256 - *data.get(i + 1)? as i32 - 108);
                    i += 2;
                }
                12 => {
                    let op = *data.get(i + 1)?;
                    if op == 0 {
                        cuts.push(i);
                    }
                    st.args = 0;
                    i += 2;
                }
                1 | 3 | 18 | 23 => {
                    // hstem vstem hstemhm vstemhm (a width first, if odd)
                    st.stems += st.args / 2;
                    st.args = 0;
                    i += 1;
                }
                19 | 20 => {
                    // hintmask cntrmask: stems given before them are vstems.
                    st.stems += st.args / 2;
                    st.args = 0;
                    if matches!(b, Body::Global(_) | Body::Local(..)) {
                        match self.stems_at_mask.insert((b, i), st.stems) {
                            Some(n) if n != st.stems => return None,
                            _ => {}
                        }
                    }
                    i += 1 + st.stems.div_ceil(8);
                    if i > data.len() {
                        return None;
                    }
                }
                10 | 29 => {
                    // callsubr callgsubr: the index is the last number.
                    let n = st.last.take()?;
                    st.args = st.args.checked_sub(1)?;
                    let (subrs, target) = if v == 10 {
                        let s = &self.cff.privates.get(private)?.subrs;
                        (s.len(), Body::Local(private, 0))
                    } else {
                        (self.cff.gsubrs.len(), Body::Global(0))
                    };
                    let idx = usize::try_from(n + bias(subrs)).ok()?;
                    if idx >= subrs {
                        return None;
                    }
                    let target = match target {
                        Body::Local(p, _) => Body::Local(p, idx),
                        _ => Body::Global(idx),
                    };
                    i += 1;
                    if let End::EndChar = self.body(target, private, st, depth + 1)? {
                        ended = Some(End::EndChar);
                        break;
                    }
                }
                11 => {
                    // return
                    ended = Some(End::Return);
                    break;
                }
                14 => {
                    ended = Some(End::EndChar);
                    break;
                }
                0..=31 => {
                    // Any other operator clears the stack.
                    st.args = 0;
                    st.last = None;
                    i += 1;
                }
            }
        }
        match self.cuts.insert(b, cuts.clone()) {
            Some(old) if old != cuts => return None,
            _ => {}
        }
        match (b, ended) {
            (Body::Glyph(_), _) => Some(End::EndChar),
            (_, Some(e)) => Some(e),
            // A subroutine running off its end returns.
            (_, None) => Some(End::Return),
        }
    }
}

/// The subroutine index bias (Type 2 §4.7).
fn bias(n: usize) -> i32 {
    if n < 1240 {
        107
    } else if n < 33900 {
        1131
    } else {
        32768
    }
}

/// An INDEX: its items, and where it ends in the program.
fn index(d: &[u8], at: usize) -> Option<(Vec<&[u8]>, usize)> {
    let count = u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]) as usize;
    if count == 0 {
        return Some((Vec::new(), at + 2));
    }
    let size = *d.get(at + 2)? as usize;
    if !(1..=4).contains(&size) {
        return None;
    }
    let offs_at = at + 3;
    let off = |k: usize| -> Option<usize> {
        let s = d.get(offs_at + k * size..offs_at + (k + 1) * size)?;
        Some(s.iter().fold(0usize, |a, &b| (a << 8) | b as usize))
    };
    let base = offs_at + (count + 1) * size - 1;
    let mut items = Vec::with_capacity(count);
    let mut prev = off(0)?;
    for k in 1..=count {
        let next = off(k)?;
        if next < prev || prev == 0 {
            return None;
        }
        items.push(d.get(base + prev..base + next)?);
        prev = next;
    }
    Some((items, base + prev))
}

/// INDEX bytes for `items` (4-byte offsets: simple and always enough).
fn write_index(out: &mut Vec<u8>, items: &[Vec<u8>]) {
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
    if items.is_empty() {
        return;
    }
    out.push(4);
    let mut off = 1u32;
    out.extend_from_slice(&off.to_be_bytes());
    for it in items {
        off += it.len() as u32;
        out.extend_from_slice(&off.to_be_bytes());
    }
    for it in items {
        out.extend_from_slice(it);
    }
}

/// A DICT entry: its operator, operand bytes and operand values.
type Entry = (u16, Vec<u8>, Vec<i64>);
type Dict = Vec<Entry>;

/// A DICT as its operators in order, each with its operand bytes.
fn dict(d: &[u8]) -> Option<Dict> {
    let mut out = Vec::new();
    let (mut raw, mut nums) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < d.len() {
        let v = d[i];
        match v {
            0..=21 => {
                let op = if v == 12 {
                    i += 1;
                    1200 + *d.get(i)? as u16
                } else {
                    v as u16
                };
                out.push((op, std::mem::take(&mut raw), std::mem::take(&mut nums)));
                i += 1;
                continue;
            }
            28 => {
                let s = d.get(i..i + 3)?;
                nums.push(i16::from_be_bytes([s[1], s[2]]) as i64);
                raw.extend_from_slice(s);
                i += 3;
            }
            29 => {
                let s = d.get(i..i + 5)?;
                nums.push(i32::from_be_bytes([s[1], s[2], s[3], s[4]]) as i64);
                raw.extend_from_slice(s);
                i += 5;
            }
            30 => {
                let start = i;
                i += 1;
                loop {
                    let b = *d.get(i)?;
                    i += 1;
                    if b & 0x0f == 0x0f || b >> 4 == 0x0f {
                        break;
                    }
                }
                nums.push(0); // a real: never an offset
                raw.extend_from_slice(&d[start..i]);
            }
            32..=246 => {
                nums.push(v as i64 - 139);
                raw.push(v);
                i += 1;
            }
            247..=250 => {
                nums.push((v as i64 - 247) * 256 + *d.get(i + 1)? as i64 + 108);
                raw.extend_from_slice(d.get(i..i + 2)?);
                i += 2;
            }
            251..=254 => {
                nums.push(-(v as i64 - 251) * 256 - *d.get(i + 1)? as i64 - 108);
                raw.extend_from_slice(d.get(i..i + 2)?);
                i += 2;
            }
            _ => return None,
        }
    }
    if !raw.is_empty() {
        return None;
    }
    Some(out)
}

/// A DICT's bytes, with the operands of the operators in `set`
/// replaced (as 5-byte integers).
fn write_dict(ops: &[Entry], set: &HashMap<u16, Vec<i64>>) -> Vec<u8> {
    let mut out = Vec::new();
    for (op, raw, _) in ops {
        match set.get(op) {
            Some(vals) => {
                for &v in vals {
                    out.push(29);
                    out.extend_from_slice(&(v as i32).to_be_bytes());
                }
            }
            None => out.extend_from_slice(raw),
        }
        if *op >= 1200 {
            out.extend_from_slice(&[12, (*op - 1200) as u8]);
        } else {
            out.push(*op as u8);
        }
    }
    out
}

struct Private<'a> {
    dict: Dict,
    subrs: Vec<&'a [u8]>,
}

struct Cff<'a> {
    data: &'a [u8],
    header: &'a [u8],
    names: &'a [u8],
    top: Dict,
    strings: &'a [u8],
    gsubrs: Vec<&'a [u8]>,
    charstrings: Vec<&'a [u8]>,
    /// The Top DICT's Private DICT, or a CID font's FDArray ones.
    privates: Vec<Private<'a>>,
    /// CID-keyed: the FDArray font dicts and the FDSelect bytes.
    fd_dicts: Vec<Dict>,
    fd_select: Option<(&'a [u8], Vec<u8>)>,
}

const CHARSET: u16 = 15;
const ENCODING: u16 = 16;
const CHARSTRINGS: u16 = 17;
const PRIVATE: u16 = 18;
const SUBRS: u16 = 19;
const FDARRAY: u16 = 1236;
const FDSELECT: u16 = 1237;

fn arg(d: &[Entry], op: u16) -> Option<&[i64]> {
    d.iter().find(|(o, _, _)| *o == op).map(|(_, _, n)| &n[..])
}

impl<'a> Cff<'a> {
    fn parse(d: &'a [u8]) -> Option<Cff<'a>> {
        if *d.first()? != 1 {
            return None;
        }
        let hdr = *d.get(2)? as usize;
        let (_, names_end) = index(d, hdr)?;
        let (tops, tops_end) = index(d, names_end)?;
        if tops.len() != 1 {
            return None;
        }
        let top = dict(tops[0])?;
        let (_, strings_end) = index(d, tops_end)?;
        let (gsubrs, _) = index(d, strings_end)?;
        let cs_at = usize::try_from(*arg(&top, CHARSTRINGS)?.first()?).ok()?;
        let (charstrings, _) = index(d, cs_at)?;
        let read_private = |pd: &[i64]| -> Option<Private<'a>> {
            let (size, at) = (usize::try_from(pd[0]).ok()?, usize::try_from(pd[1]).ok()?);
            let dct = dict(d.get(at..at.checked_add(size)?)?)?;
            let subrs = match arg(&dct, SUBRS).and_then(|v| v.first().copied()) {
                Some(off) => index(d, at.checked_add(usize::try_from(off).ok()?)?)?.0,
                None => Vec::new(),
            };
            Some(Private { dict: dct, subrs })
        };
        let mut privates = Vec::new();
        let mut fd_dicts = Vec::new();
        let mut fd_select = None;
        if let Some(fda) = arg(&top, FDARRAY) {
            let (fds, _) = index(d, usize::try_from(*fda.first()?).ok()?)?;
            for fd in fds {
                let fdd = dict(fd)?;
                let pd = arg(&fdd, PRIVATE)?;
                if pd.len() != 2 {
                    return None;
                }
                privates.push(read_private(pd)?);
                fd_dicts.push(fdd);
            }
            let at = usize::try_from(*arg(&top, FDSELECT)?.first()?).ok()?;
            let n = charstrings.len();
            let bytes = match *d.get(at)? {
                0 => d.get(at..at + 1 + n)?,
                3 => {
                    let ranges = u16::from_be_bytes([*d.get(at + 1)?, *d.get(at + 2)?]) as usize;
                    d.get(at..at + 3 + 3 * ranges + 2)?
                }
                _ => return None,
            };
            let map = fd_map(bytes, n)?;
            fd_select = Some((bytes, map));
        } else {
            match arg(&top, PRIVATE) {
                Some(pd) if pd.len() == 2 => privates.push(read_private(pd)?),
                Some(_) => return None,
                None => privates.push(Private {
                    dict: Vec::new(),
                    subrs: Vec::new(),
                }),
            }
        }
        Some(Cff {
            data: d,
            header: d.get(..hdr)?,
            names: d.get(hdr..names_end)?,
            top,
            strings: d.get(tops_end..strings_end)?,
            gsubrs,
            charstrings,
            privates,
            fd_dicts,
            fd_select,
        })
    }

    /// The Private DICT (by index in `privates`) glyph `g` runs under.
    fn private_for(&self, g: usize) -> Option<usize> {
        match &self.fd_select {
            None => Some(0),
            Some((_, map)) => {
                let fd = *map.get(g)? as usize;
                (fd < self.privates.len()).then_some(fd)
            }
        }
    }

    /// The charset or encoding bytes at Top DICT operator `op`, unless
    /// predefined (offset 0, 1 or 2) or absent.
    fn table(&self, op: u16) -> Option<Option<&'a [u8]>> {
        let Some(at) = arg(&self.top, op).and_then(|v| v.first().copied()) else {
            return Some(None);
        };
        if at <= 2 {
            return Some(None);
        }
        let at = usize::try_from(at).ok()?;
        let d = self.data;
        let n = self.charstrings.len();
        let end = if op == CHARSET {
            match *d.get(at)? {
                0 => at + 1 + 2 * n.saturating_sub(1),
                f @ (1 | 2) => {
                    let mut i = at + 1;
                    let mut covered = 1usize;
                    while covered < n {
                        let left = if f == 1 {
                            *d.get(i + 2)? as usize
                        } else {
                            u16::from_be_bytes([*d.get(i + 2)?, *d.get(i + 3)?]) as usize
                        };
                        covered += left + 1;
                        i += if f == 1 { 3 } else { 4 };
                    }
                    i
                }
                _ => return None,
            }
        } else {
            let f = *d.get(at)?;
            let mut i = at + 1;
            match f & 0x7f {
                0 => i += 1 + *d.get(i)? as usize,
                1 => i += 1 + 2 * *d.get(i)? as usize,
                _ => return None,
            }
            if f & 0x80 != 0 {
                i += 1 + 3 * *d.get(i)? as usize;
            }
            i
        };
        Some(Some(d.get(at..end)?))
    }

    /// The program rewritten with `cuts` removed from their bodies.
    fn rebuild(&self, cuts: &HashMap<Body, Vec<usize>>) -> Option<Vec<u8>> {
        let cut = |b: Body, data: &[u8]| -> Vec<u8> {
            let mut out = data.to_vec();
            if let Some(at) = cuts.get(&b) {
                for &i in at.iter().rev() {
                    out.drain(i..i + 2);
                }
            }
            out
        };
        let gsubrs: Vec<Vec<u8>> = (0..self.gsubrs.len())
            .map(|i| cut(Body::Global(i), self.gsubrs[i]))
            .collect();
        let charstrings: Vec<Vec<u8>> = (0..self.charstrings.len())
            .map(|i| cut(Body::Glyph(i), self.charstrings[i]))
            .collect();
        let charset = self.table(CHARSET)?;
        let encoding = self.table(ENCODING)?;

        // Everything after the Global Subrs INDEX, at offsets known once
        // the Top DICT's size is: its offsets are always 5-byte integers.
        let mut set = HashMap::new();
        let placeholder = |n: usize| vec![0i64; n];
        if charset.is_some() {
            set.insert(CHARSET, placeholder(1));
        }
        if encoding.is_some() {
            set.insert(ENCODING, placeholder(1));
        }
        set.insert(CHARSTRINGS, placeholder(1));
        if self.fd_select.is_some() {
            set.insert(FDARRAY, placeholder(1));
            set.insert(FDSELECT, placeholder(1));
        } else if arg(&self.top, PRIVATE).is_some() {
            set.insert(PRIVATE, placeholder(2));
        }
        let top_len = write_dict(&self.top, &set).len();
        let mut head = Vec::new();
        head.extend_from_slice(self.header);
        head.extend_from_slice(self.names);
        let top_at = head.len();
        // The Top DICT INDEX: count 1, offset size 4, two offsets.
        let after_top = top_at + 2 + 1 + 8 + top_len;
        let mut tail = Vec::new();
        tail.extend_from_slice(self.strings);
        write_index(&mut tail, &gsubrs);
        let at = |tail: &Vec<u8>| (after_top + tail.len()) as i64;
        if let Some(c) = charset {
            set.insert(CHARSET, vec![at(&tail)]);
            tail.extend_from_slice(c);
        }
        if let Some(e) = encoding {
            set.insert(ENCODING, vec![at(&tail)]);
            tail.extend_from_slice(e);
        }
        set.insert(CHARSTRINGS, vec![at(&tail)]);
        write_index(&mut tail, &charstrings);
        // Each Private DICT, its Subrs right after it.
        let mut private_at = Vec::new();
        for (p, private) in self.privates.iter().enumerate() {
            let subrs: Vec<Vec<u8>> = (0..private.subrs.len())
                .map(|i| cut(Body::Local(p, i), private.subrs[i]))
                .collect();
            // A Subrs entry, even of an empty INDEX, keeps one after it.
            let has_subrs = arg(&private.dict, SUBRS).is_some();
            let mut pset = HashMap::new();
            if has_subrs {
                pset.insert(SUBRS, vec![0]);
            }
            let size = write_dict(&private.dict, &pset).len();
            if has_subrs {
                pset.insert(SUBRS, vec![size as i64]);
            }
            let bytes = write_dict(&private.dict, &pset);
            private_at.push((bytes.len() as i64, at(&tail)));
            tail.extend_from_slice(&bytes);
            if has_subrs {
                write_index(&mut tail, &subrs);
            }
        }
        if let Some((select, _)) = &self.fd_select {
            let fds: Vec<Vec<u8>> = self
                .fd_dicts
                .iter()
                .zip(&private_at)
                .map(|(fd, &(size, off))| {
                    write_dict(fd, &HashMap::from([(PRIVATE, vec![size, off])]))
                })
                .collect();
            set.insert(FDARRAY, vec![at(&tail)]);
            write_index(&mut tail, &fds);
            set.insert(FDSELECT, vec![at(&tail)]);
            tail.extend_from_slice(select);
        } else if arg(&self.top, PRIVATE).is_some() {
            let (size, off) = private_at[0];
            set.insert(PRIVATE, vec![size, off]);
        }
        let top = write_dict(&self.top, &set);
        if top.len() != top_len {
            return None;
        }
        let mut out = head;
        write_index(&mut out, &[top]);
        out.extend_from_slice(&tail);
        Some(out)
    }
}

/// FDSelect → the font dict of each glyph.
fn fd_map(b: &[u8], n: usize) -> Option<Vec<u8>> {
    match b[0] {
        0 => Some(b[1..].to_vec()),
        _ => {
            let ranges = u16::from_be_bytes([b[1], b[2]]) as usize;
            let mut map = vec![0u8; n];
            for r in 0..ranges {
                let first = u16::from_be_bytes([b[3 + 3 * r], b[4 + 3 * r]]) as usize;
                let fd = b[5 + 3 * r];
                let next = u16::from_be_bytes([*b.get(6 + 3 * r)?, *b.get(7 + 3 * r)?]) as usize;
                if first > next || next > n {
                    return None;
                }
                map[first..next].fill(fd);
            }
            Some(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(items: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        write_index(&mut out, items);
        out
    }

    fn int5(out: &mut Vec<u8>, n: usize) {
        out.push(29);
        out.extend_from_slice(&(n as i32).to_be_bytes());
    }

    /// A name-keyed program of `charstrings` (glyph names: standard
    /// strings from 1) with local subroutines `subrs`.
    fn program(charstrings: &[Vec<u8>], subrs: &[Vec<u8>]) -> Vec<u8> {
        let top_len = 6 + 6 + 11;
        let mut head = vec![1, 0, 4, 4];
        head.extend(idx(&[b"T".to_vec()]));
        let after_top = head.len() + 2 + 1 + 8 + top_len;
        let mut tail = idx(&[]);
        tail.extend(idx(&[]));
        let charset_at = after_top + tail.len();
        tail.push(0);
        for g in 1..charstrings.len() {
            tail.extend_from_slice(&(g as u16).to_be_bytes());
        }
        let cs_at = after_top + tail.len();
        tail.extend(idx(charstrings));
        let private_at = after_top + tail.len();
        let mut private = Vec::new();
        int5(&mut private, 6);
        private.push(19);
        tail.extend_from_slice(&private);
        tail.extend(idx(subrs));
        let mut top = Vec::new();
        int5(&mut top, charset_at);
        top.push(15);
        int5(&mut top, cs_at);
        top.push(17);
        int5(&mut top, private.len());
        int5(&mut top, private_at);
        top.push(18);
        assert_eq!(top.len(), top_len);
        head.extend(idx(&[top]));
        head.extend(tail);
        head
    }

    struct Count(usize);
    impl ttf_parser::OutlineBuilder for Count {
        fn move_to(&mut self, _: f32, _: f32) {
            self.0 += 1
        }
        fn line_to(&mut self, _: f32, _: f32) {
            self.0 += 1
        }
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }

    fn outline(p: &[u8], g: u16) -> Option<usize> {
        let t = ttf_parser::cff::Table::parse(p)?;
        let mut c = Count(0);
        t.outline(ttf_parser::GlyphId(g), &mut c).ok()?;
        Some(c.0)
    }

    // 100 0 rmoveto 400 0 rlineto 0 700 rlineto
    const PATH: &[u8] = &[239, 139, 21, 28, 1, 144, 139, 5, 139, 28, 2, 188, 5];

    fn glyph(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn dotsection_is_removed_from_glyphs_and_subroutines() {
        let call0 = [32u8, 10]; // -107 callsubr: local subroutine 0
        let p = program(
            &[
                vec![14],
                glyph(&[PATH, &[12, 0], &[14]]),
                glyph(&[&[12, 0], &call0, &[14]]),
            ],
            &[glyph(&[&[12, 0], PATH, &[11]])],
        );
        assert_eq!(outline(&p, 1), None, "the interpreter refuses dotsection");
        let q = without_dotsection(&p).expect("rewritten");
        assert_eq!(outline(&q, 1), Some(3));
        assert_eq!(outline(&q, 2), Some(3));
        assert!(!q.windows(3).any(|w| w == [12, 0, 14]));
        // Nothing to remove: nothing rewritten.
        assert_eq!(
            without_dotsection(&program(&[vec![14], glyph(&[PATH, &[14]])], &[])),
            None
        );
    }

    #[test]
    fn mask_bytes_and_operands_are_not_operators() {
        // Ten stems: the hintmask's two bytes are 12 and 0; an operand
        // 28 12 0 (3072) too. Only the real dotsection goes.
        let mut stems = Vec::new();
        for _ in 0..2 {
            for n in [0, 10, 20, 10, 40, 10, 60, 10, 80, 10] {
                stems.push((n + 139) as u8);
            }
            stems.push(if stems.len() == 10 { 18 } else { 19 });
        }
        let g = glyph(&[
            &stems,
            &[12, 0],
            &[28, 12, 0, 139, 21],
            &[12, 0],
            PATH,
            &[14],
        ]);
        let p = program(&[vec![14], g.clone()], &[]);
        let q = without_dotsection(&p).unwrap();
        let want = glyph(&[&stems, &[12, 0], &[28, 12, 0, 139, 21], PATH, &[14]]);
        let c = Cff::parse(&q).unwrap();
        assert_eq!(c.charstrings[1], &want[..]);
        assert!(outline(&q, 1).is_some());
    }

    #[test]
    fn a_subroutine_read_with_two_stem_counts_at_a_mask_is_left_alone() {
        // The subroutine's mask is one byte after 2 stems, two after 10.
        let sub = glyph(&[&[19, 0xff], &[12, 0], &[11]]);
        let call0 = [32u8, 10];
        let two = glyph(&[&[139, 149, 139, 149, 1], &call0, &[14]]);
        let mut ten = Vec::new();
        for n in [
            0, 10, 20, 10, 40, 10, 60, 10, 80, 10, 0, 10, 20, 10, 40, 10, 60, 10, 80, 10,
        ] {
            ten.push((n + 139) as u8);
        }
        ten.push(1);
        let ten = glyph(&[&ten, &call0, &[14]]);
        assert_eq!(
            without_dotsection(&program(&[vec![14], two, ten], &[sub])),
            None
        );
    }

    #[test]
    fn malformed_or_runaway_programs_are_left_alone() {
        let p = program(&[vec![14], glyph(&[PATH, &[12, 0], &[14]])], &[]);
        for cut in [4, 20, p.len() / 2, p.len() - 3] {
            assert_eq!(without_dotsection(&p[..cut]), None, "cut at {cut}");
        }
        // A subroutine calling itself: bounded by the nesting limit.
        let call0 = [32u8, 10];
        let selfcall = glyph(&[&[12, 0], &call0, &[11]]);
        assert_eq!(
            without_dotsection(&program(&[vec![14], glyph(&[&call0, &[14]])], &[selfcall])),
            None
        );
        // Calls fanning out ten deep (2^10 paths per glyph, many glyphs):
        // the token budget, not the fan-out, bounds the work.
        let mut subrs = Vec::new();
        for k in 0..10u8 {
            let next = (k as i32 + 1 - 107 + 139) as u8;
            subrs.push(if k == 9 {
                glyph(&[&[12, 0], &[11]])
            } else {
                glyph(&[&[next, 10, next, 10], &[11]])
            });
        }
        let glyphs: Vec<Vec<u8>> = std::iter::once(vec![14])
            .chain((0..4000).map(|_| glyph(&[&[32, 10], &[14]])))
            .collect();
        let t = std::time::Instant::now();
        assert_eq!(without_dotsection(&program(&glyphs, &subrs)), None);
        assert!(t.elapsed().as_secs() < 5);
    }
}
