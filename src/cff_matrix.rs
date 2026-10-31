//! The glyph-space → text-space matrix of a CFF font program
//! (Adobe Technical Note #5176): the Top DICT `FontMatrix` (default
//! `[0.001 0 0 0.001 0 0]`) and, in a CID-keyed font, the `FontMatrix`
//! of the Font DICT that `FDSelect` assigns to each glyph.
//!
//! ttf-parser exposes only the Top DICT matrix and ignores Font DICT
//! matrices, so this module reads the few DICT entries it needs itself.
//! Every read is bounds-checked; a structure it cannot read yields an
//! error rather than a guessed matrix.

use crate::error::Error;
use crate::matrix::Matrix;

/// The default `FontMatrix` of a CFF Top DICT (TN 5176 Table 9).
pub(crate) const DEFAULT: Matrix = Matrix {
    a: 0.001,
    b: 0.0,
    c: 0.0,
    d: 0.001,
    e: 0.0,
    f: 0.0,
};

/// Matrices of one CFF program.
#[derive(Debug, Clone)]
pub(crate) struct CffMatrices {
    /// The Top DICT `FontMatrix`, when the DICT states one.
    top: Option<Matrix>,
    /// Per Font DICT (CID-keyed fonts): its own `FontMatrix`, if any.
    fds: Vec<Option<Matrix>>,
    fd_select: FdSelect,
}

#[derive(Debug, Clone)]
enum FdSelect {
    None,
    /// Format 0: one FD index per glyph.
    Format0(Vec<u8>),
    /// Format 3: `(first glyph, fd)` ranges and the sentinel.
    Format3(Vec<(u16, u8)>, u16),
}

fn bad(msg: &str) -> Error {
    Error::Structure(format!("CFF font program: {msg}"))
}

fn u16_at(d: &[u8], i: usize) -> Result<u16, Error> {
    d.get(i..i + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| bad("truncated"))
}

fn offset_at(d: &[u8], i: usize, size: u8) -> Result<usize, Error> {
    let b = d
        .get(i..i + size as usize)
        .ok_or_else(|| bad("truncated"))?;
    Ok(b.iter().fold(0usize, |acc, &x| (acc << 8) | x as usize))
}

/// An INDEX at `at`: the byte ranges of its objects and the end offset.
fn index(d: &[u8], at: usize) -> Result<(Vec<(usize, usize)>, usize), Error> {
    let count = u16_at(d, at)? as usize;
    if count == 0 {
        return Ok((Vec::new(), at + 2));
    }
    let off_size = *d.get(at + 2).ok_or_else(|| bad("truncated INDEX"))?;
    if !(1..=4).contains(&off_size) {
        return Err(bad("INDEX offSize outside 1..4"));
    }
    let offsets_at = at + 3;
    let data_start = offsets_at + (count + 1) * off_size as usize - 1;
    let mut ranges = Vec::with_capacity(count);
    let mut prev = offset_at(d, offsets_at, off_size)?;
    for k in 1..=count {
        let next = offset_at(d, offsets_at + k * off_size as usize, off_size)?;
        if next < prev || prev == 0 {
            return Err(bad("INDEX offsets out of order"));
        }
        ranges.push((data_start + prev, data_start + next));
        prev = next;
    }
    let end = data_start + prev;
    if end > d.len() {
        return Err(bad("INDEX data beyond the program"));
    }
    Ok((ranges, end))
}

/// A real number operand (TN 5176 Table 5: nibbles).
fn real(d: &[u8], i: &mut usize) -> Result<f64, Error> {
    let mut s = String::new();
    loop {
        let b = *d.get(*i).ok_or_else(|| bad("truncated real"))?;
        *i += 1;
        for nib in [b >> 4, b & 0x0f] {
            match nib {
                0..=9 => s.push((b'0' + nib) as char),
                0xa => s.push('.'),
                0xb => s.push('E'),
                0xc => s.push_str("E-"),
                0xe => s.push('-'),
                0xf => return s.parse().map_err(|_| bad("malformed real operand")),
                _ => return Err(bad("reserved nibble in real operand")),
            }
            if s.len() > 64 {
                return Err(bad("real operand too long"));
            }
        }
    }
}

/// The `(operator, operands)` entries of a DICT.
fn dict(d: &[u8]) -> Result<Vec<(u16, Vec<f64>)>, Error> {
    let mut out = Vec::new();
    let mut ops: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let b0 = d[i];
        i += 1;
        let num = |i: usize, n: usize| d.get(i..i + n).ok_or_else(|| bad("truncated operand"));
        match b0 {
            0..=11 | 13..=21 => {
                out.push((b0 as u16, std::mem::take(&mut ops)));
            }
            12 => {
                let b1 = *d.get(i).ok_or_else(|| bad("truncated operator"))?;
                i += 1;
                out.push((1200 + b1 as u16, std::mem::take(&mut ops)));
            }
            28 => {
                let b = num(i, 2)?;
                ops.push(i16::from_be_bytes([b[0], b[1]]) as f64);
                i += 2;
            }
            29 => {
                let b = num(i, 4)?;
                ops.push(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64);
                i += 4;
            }
            30 => ops.push(real(d, &mut i)?),
            32..=246 => ops.push(b0 as f64 - 139.0),
            247..=250 => {
                let b1 = *num(i, 1)?.first().unwrap_or(&0) as f64;
                ops.push((b0 as f64 - 247.0) * 256.0 + b1 + 108.0);
                i += 1;
            }
            251..=254 => {
                let b1 = *num(i, 1)?.first().unwrap_or(&0) as f64;
                ops.push(-(b0 as f64 - 251.0) * 256.0 - b1 - 108.0);
                i += 1;
            }
            _ => return Err(bad("reserved byte in DICT")),
        }
        if ops.len() > 48 {
            return Err(bad("too many DICT operands"));
        }
    }
    Ok(out)
}

const FONT_MATRIX: u16 = 1207;
const FD_ARRAY: u16 = 1236;
const FD_SELECT: u16 = 1237;

/// A `FontMatrix` operand list: six finite numbers forming an
/// invertible matrix, else a structure error (no default is guessed
/// for a matrix the program does state).
pub(crate) fn checked_matrix(v: &[f64]) -> Result<Matrix, Error> {
    if v.len() != 6 || v.iter().any(|x| !x.is_finite()) {
        return Err(bad("FontMatrix must be six finite numbers"));
    }
    let m = Matrix::new(v[0], v[1], v[2], v[3], v[4], v[5]);
    if (m.a * m.d - m.b * m.c).abs() < 1e-12 {
        return Err(bad("FontMatrix is not invertible"));
    }
    Ok(m)
}

fn offset_operand(v: &[f64], what: &str) -> Result<usize, Error> {
    match v {
        [o] if *o >= 0.0 && o.fract() == 0.0 => Ok(*o as usize),
        _ => Err(bad(&format!("{what} offset is not a non-negative integer"))),
    }
}

impl CffMatrices {
    /// Read the matrices of the CFF program `d` (the first font of its
    /// Name INDEX, as every CFF embedded in a PDF has exactly one).
    pub(crate) fn parse(d: &[u8]) -> Result<CffMatrices, Error> {
        let hdr = *d.get(2).ok_or_else(|| bad("truncated header"))? as usize;
        let (_, names_end) = index(d, hdr)?;
        let (tops, _) = index(d, names_end)?;
        let &(s, e) = tops.first().ok_or_else(|| bad("empty Top DICT INDEX"))?;
        let mut top = None;
        let (mut fd_array, mut fd_select_at) = (None, None);
        for (op, v) in dict(&d[s..e])? {
            match op {
                FONT_MATRIX => top = Some(checked_matrix(&v)?),
                FD_ARRAY => fd_array = Some(offset_operand(&v, "FDArray")?),
                FD_SELECT => fd_select_at = Some(offset_operand(&v, "FDSelect")?),
                _ => {}
            }
        }
        let mut fds = Vec::new();
        let mut fd_select = FdSelect::None;
        if let (Some(fa), Some(fs)) = (fd_array, fd_select_at) {
            let (ranges, _) = index(d, fa)?;
            if ranges.len() > 256 {
                return Err(bad("more than 256 Font DICTs"));
            }
            for (s, e) in ranges {
                let mut m = None;
                for (op, v) in dict(&d[s..e])? {
                    if op == FONT_MATRIX {
                        m = Some(checked_matrix(&v)?);
                    }
                }
                fds.push(m);
            }
            fd_select = match d.get(fs) {
                Some(0) => FdSelect::Format0(d[fs + 1..].to_vec()),
                Some(3) => {
                    let n = u16_at(d, fs + 1)? as usize;
                    let mut r = Vec::with_capacity(n);
                    for k in 0..n {
                        let at = fs + 3 + k * 3;
                        r.push((
                            u16_at(d, at)?,
                            *d.get(at + 2).ok_or_else(|| bad("truncated FDSelect"))?,
                        ));
                    }
                    FdSelect::Format3(r, u16_at(d, fs + 3 + n * 3)?)
                }
                _ => return Err(bad("FDSelect format other than 0 or 3")),
            };
        }
        Ok(CffMatrices {
            top,
            fds,
            fd_select,
        })
    }

    fn fd_of(&self, gid: u16) -> Option<usize> {
        match &self.fd_select {
            FdSelect::None => None,
            FdSelect::Format0(v) => v.get(gid as usize).map(|&f| f as usize),
            FdSelect::Format3(r, sentinel) => {
                if gid >= *sentinel {
                    return None;
                }
                r.iter()
                    .rev()
                    .find(|(first, _)| *first <= gid)
                    .map(|&(_, f)| f as usize)
            }
        }
    }

    /// Glyph space → text space for glyph `gid`. A Font DICT matrix is
    /// combined with an explicit Top DICT matrix the way FreeType
    /// (`cff_face_init`) does — product of the two, rescaled by the
    /// smaller of their units-per-em — so the common `0.001` × `0.001`
    /// pair stays `0.001`; alone it is used as stated.
    pub(crate) fn glyph_matrix(&self, gid: u16) -> Matrix {
        let top = self.top.unwrap_or(DEFAULT);
        let Some(Some(sub)) = self.fd_of(gid).and_then(|f| self.fds.get(f)) else {
            return top;
        };
        if self.top.is_none() {
            return *sub;
        }
        let upem = |m: &Matrix| 1.0 / m.a.abs().max(m.b.abs()).max(m.c.abs()).max(m.d.abs());
        let (ut, us) = (upem(&top), upem(sub));
        let s = if ut > 1.0 && us > 1.0 {
            ut.min(us)
        } else {
            1.0
        };
        let p = sub.concat(top);
        Matrix::new(p.a * s, p.b * s, p.c * s, p.d * s, p.e * s, p.f * s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_operands_and_reals() {
        // 12 7 with [0.0005 0 0 0.0005 0 0]: real 0.0005 = 1e a0 00 5f
        let real5 = [0x1e, 0x0a, 0x00, 0x05, 0xff];
        let mut d = Vec::new();
        for v in [&real5[..], &[139], &[139], &real5[..], &[139], &[139]] {
            d.extend_from_slice(v);
        }
        d.extend_from_slice(&[12, 7]);
        let e = dict(&d).unwrap();
        assert_eq!(e[0].0, FONT_MATRIX);
        assert_eq!(e[0].1, vec![0.0005, 0.0, 0.0, 0.0005, 0.0, 0.0]);
        assert!(checked_matrix(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).is_err());
        assert!(checked_matrix(&[f64::NAN, 0.0, 0.0, 1.0, 0.0, 0.0]).is_err());
        assert!(checked_matrix(&[0.001, 0.0, 0.0]).is_err());
    }

    /// A minimal CID-keyed CFF: Top DICT `FontMatrix` [1 0 0 1 0 0],
    /// two Font DICTs (0.001 and 0.0005), FDSelect format 3 assigning
    /// glyphs 0–4 to the first and 5–9 to the second.
    #[test]
    fn cid_keyed_program_is_read_per_glyph() {
        let int5 = |v: i32| [&[29u8][..], &v.to_be_bytes()].concat();
        let real = |hex: &[u8]| [&[30u8][..], hex].concat();
        let index = |items: &[Vec<u8>]| {
            let mut out = (items.len() as u16).to_be_bytes().to_vec();
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
            out
        };
        let matrix = |a: &[u8]| {
            let mut d = Vec::new();
            for v in [a, &[139][..], &[139][..], a, &[139][..], &[139][..]] {
                d.extend_from_slice(v);
            }
            d.extend_from_slice(&[12, 7]);
            d
        };
        let fd_array = index(&[
            matrix(&real(&[0x0a, 0x00, 0x1f])),
            matrix(&real(&[0x0a, 0x00, 0x05, 0xff])),
        ]);
        let fd_select = [3u8, 0, 2, 0, 0, 0, 0, 5, 1, 0, 10].to_vec();
        let names = index(&[b"A".to_vec()]);
        // Offsets are fixed-width, so the Top DICT size is known first.
        let top_dict = |fa: i32, fs: i32| {
            let mut d = matrix(&[140]);
            d.extend(int5(fa));
            d.extend_from_slice(&[12, 36]);
            d.extend(int5(fs));
            d.extend_from_slice(&[12, 37]);
            d
        };
        let top_len = index(&[top_dict(0, 0)]).len();
        let fs = 4 + names.len() + top_len;
        let fa = fs + fd_select.len();
        let mut cff = vec![1, 0, 4, 4];
        cff.extend(names);
        cff.extend(index(&[top_dict(fa as i32, fs as i32)]));
        cff.extend(fd_select);
        cff.extend(fd_array);
        let m = CffMatrices::parse(&cff).unwrap();
        assert!((m.glyph_matrix(0).a - 0.001).abs() < 1e-12);
        assert!((m.glyph_matrix(7).a - 0.0005).abs() < 1e-12);
        // Outside every range: the Top DICT matrix alone.
        assert!((m.glyph_matrix(12).a - 1.0).abs() < 1e-12);
        // Truncations are errors, never a default.
        for n in [3, 10, cff.len() - 3] {
            assert!(CffMatrices::parse(&cff[..n]).is_err(), "cut at {n}");
        }
    }

    #[test]
    fn font_dict_matrix_combination() {
        let m = |s| Some(Matrix::new(s, 0.0, 0.0, s, 0.0, 0.0));
        let base = |top, sub| CffMatrices {
            top,
            fds: vec![sub],
            fd_select: FdSelect::Format0(vec![0]),
        };
        let a = |c: CffMatrices| (c.glyph_matrix(0).a * 1e6).round() / 1e6;
        assert_eq!(a(base(None, None)), 0.001);
        assert_eq!(a(base(m(0.001), m(0.001))), 0.001);
        assert_eq!(a(base(m(1.0), m(0.001))), 0.001);
        assert_eq!(a(base(None, m(0.0005))), 0.0005);
        assert_eq!(a(base(m(0.001), m(0.0005))), 0.0005);
    }
}
