//! Object streams (`/Type /ObjStm`) — ISO 32000-1:2008, §7.5.7.
//!
//! An object stream is a stream containing a header (`N` pairs of
//! `<objnum> <offset>`) followed by `N` object bodies. Objects inside
//! it are reached via xref-stream entries of type 2.
//!
//! Structural rules enforced (per spec):
//! - `/N` and `/First` are required non-negative integers.
//! - `/First` must lie within the decoded data.
//! - Offsets in the header are relative to the first byte after the
//!   header (i.e. `offset` is added to `/First`).
//! - Offsets must be strictly increasing and within bounds.
//! - The object number in the header must match the xref key that
//!   requested it (a mismatch is a corrupted file, not a wrong object).
//! - Object streams may not be nested and may not contain other streams;
//!   we do not check for the latter here — the contained objects are
//!   ordinary non-stream objects by construction.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use crate::parser::Parser;
use std::collections::HashMap;

/// A parsed object stream: the decoded body split into per-object ranges.
#[derive(Debug)]
pub(crate) struct ObjectStream {
    /// Object number → parsed object.
    objects: HashMap<u32, Object>,
}

impl ObjectStream {
    /// Parse a decoded object-stream body.
    ///
    /// `n` is the `/N` value, `first` is `/First`. `expected` is the
    /// (objnum, index) we were asked for — we verify it is present.
    pub(crate) fn parse(
        decoded: &[u8],
        dict: &Dict,
        expected: (u32, u32),
        limits: &Limits,
    ) -> Result<ObjectStream, Error> {
        let n = dict
            .get(b"N".as_slice())
            .and_then(|o| o.as_i64())
            .filter(|i| *i >= 0 && *i <= limits.max_objstm_entries as i64)
            .ok_or_else(|| Error::Structure("object stream /N is missing or out of range".into()))?
            as usize;
        let first = dict
            .get(b"First".as_slice())
            .and_then(|o| o.as_i64())
            .filter(|i| *i >= 0 && *i as usize <= decoded.len())
            .ok_or_else(|| {
                Error::Structure("object stream /First is missing or out of range".into())
            })? as usize;

        // Parse the header: N pairs of (objnum, offset), read from the
        // first `first` bytes of the decoded stream.
        let mut parser = Parser::new(decoded, limits);
        parser.seek(0);
        let mut header: Vec<(u32, usize)> = Vec::with_capacity(n);
        for _ in 0..n {
            if parser.pos() > first {
                return Err(Error::Structure(
                    "object stream header overruns /First".into(),
                ));
            }
            let objnum = read_usize_word(&mut parser, "object number")?;
            let offset = read_usize_word(&mut parser, "object offset")?;
            if objnum > u32::MAX as usize {
                return Err(Error::Structure(
                    "object stream object number exceeds u32".into(),
                ));
            }
            header.push((objnum as u32, offset));
        }

        // Validate offsets: strictly increasing, within [first, len).
        let mut prev_end = first;
        let mut bounds = Vec::with_capacity(n);
        for &(_num, off) in &header {
            let start = first
                .checked_add(off)
                .ok_or_else(|| Error::Structure("object stream offset overflow".into()))?;
            if start < prev_end || start > decoded.len() {
                return Err(Error::Structure(format!(
                    "object stream offset {off} out of bounds"
                )));
            }
            bounds.push(start);
            prev_end = start;
        }
        bounds.push(decoded.len());

        // Parse each object.
        let mut objects = HashMap::with_capacity(n);
        for (i, &(num, _off)) in header.iter().enumerate() {
            let start = bounds[i];
            let end = bounds[i + 1];
            if start > end {
                return Err(Error::Structure(
                    "object stream object bounds are inverted".into(),
                ));
            }
            let mut p = Parser::new(&decoded[start..end], limits);
            let obj = p.parse_object(0)?.ok_or(Error::UnexpectedEof {
                offset: start,
                context: "object stream object",
            })?;
            objects.insert(num, obj);
        }

        // The entry we were asked for must exist and match.
        let (want_num, want_idx) = expected;
        if (want_idx as usize) >= header.len() || header[want_idx as usize].0 != want_num {
            return Err(Error::Structure(format!(
                "object stream index {} does not contain object {}",
                want_idx, want_num
            )));
        }

        Ok(ObjectStream { objects })
    }

    /// Look up an object by number.
    pub(crate) fn get(&self, num: u32) -> Option<&Object> {
        self.objects.get(&num)
    }
}

/// Read a single whitespace-delimited ASCII integer token from a parser.
fn read_usize_word(parser: &mut Parser, context: &'static str) -> Result<usize, Error> {
    match parser.read_word_token()? {
        Some(w) => std::str::from_utf8(&w)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| Error::Parse(format!("bad {context} in object stream header"))),
        None => Err(Error::UnexpectedEof {
            offset: parser.pos(),
            context,
        }),
    }
}
