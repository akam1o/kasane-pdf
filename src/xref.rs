//! Cross-reference table and trailer parsing (ISO 32000-1:2008,
//! §7.5.4–7.5.5 for classic sections, §7.5.8 for xref streams and
//! hybrid-reference files).
//!
//! Supported: plain `xref` sections (with multiple subsections), the
//! `trailer` dictionary, `/Prev` chains of classic sections, xref streams,
//! and hybrid-reference files (`/XRefStm` in a classic trailer).
//!
//! Entries are stored as a map `(obj, gen) -> entry`. Newest section wins;
//! older `/Prev` sections only fill gaps. In hybrid files the classic
//! table (and its `/Prev` chain) is primary; `/XRefStm` supplies entries
//! only for objects not covered by a newer classic section.
//!
//! Object streams: xref-stream entries of type 2 are recorded as
//! `Compressed` and resolved via the document layer.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::parser::Parser;
use std::collections::{BTreeMap, HashSet};

/// One entry of the combined cross-reference map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum XrefEntry {
    /// In-use (`n`) object at a byte offset in the file.
    InUse {
        /// Byte offset of the `n g obj` definition.
        offset: u64,
    },
    /// Free (`f`) entry. `next_free` is the object-number link field.
    #[allow(dead_code)]
    Free {
        /// The "next free object" field of the xref line.
        next_free: u32,
    },
    /// Compressed object inside an object stream (type 2).
    Compressed {
        /// Object number of the containing `/ObjStm`.
        objstm: u32,
        /// Index of the object within the object stream's `/N` list.
        index: u32,
    },
}

/// The merged result of reading a `startxref` chain.
#[derive(Debug)]
pub(crate) struct XrefTable {
    /// `(obj, gen) -> entry` for every subsection line seen. Entries from
    /// the newest section (at `startxref`) win; `/Prev` sections fill gaps.
    entries: BTreeMap<(u32, u16), XrefEntry>,
    /// Merged trailer: newest trailer first, older trailers fill missing
    /// keys (e.g. `/Root` and `/Size` surviving across increments).
    pub(crate) trailer: Dict,
    /// The trailer closest to `startxref` (the authoritative one).
    /// Used by the M23 audit for trailer-key provenance — the merged
    /// `trailer` can carry keys (e.g. a stale `/Info`) that the
    /// newest trailer no longer has.
    pub(crate) latest_trailer: Dict,
    /// Byte offsets of all xref sections read, newest first.
    /// Retained for diagnostics (e.g. explaining /Prev chain failures)
    /// and for the M23 audit's revision history.
    pub(crate) section_offsets: Vec<u64>,
}

impl XrefTable {
    /// Byte offset where object `r` is defined, if it is in-use.
    pub(crate) fn offset_of(&self, r: IndirectRef) -> Option<u64> {
        match self.entries.get(&(r.number, r.generation)) {
            Some(XrefEntry::InUse { offset }) => Some(*offset),
            _ => None,
        }
    }

    /// Object stream location for a compressed object, if `r` is stored
    /// inside one.
    pub(crate) fn compressed_location(&self, r: IndirectRef) -> Option<(u32, u32)> {
        match self.entries.get(&(r.number, r.generation)) {
            Some(XrefEntry::Compressed { objstm, index }) => Some((*objstm, *index)),
            _ => None,
        }
    }

    /// Whether the input xref has *any* entry for object number `num`,
    /// regardless of generation or entry type (in-use, free or
    /// compressed). The edit session uses this to *reserve* a number:
    /// per §7.5.2 a `(number, generation)` pair identifies an object,
    /// so a number known to the file at one generation must never be
    /// reissued at another as a synthetic — a dangling input ref to
    /// that number at generation 0 could otherwise silently resolve
    /// to a synthetic object instead of failing.
    ///
    /// `entries` is keyed `(num, gen)` in a `BTreeMap`, so
    /// `range((num,0)..=(num,u16::MAX))` finds every entry for `num`
    /// in one probe without scanning the table.
    pub(crate) fn number_used(&self, num: u32) -> bool {
        self.entries
            .range((num, 0)..=(num, u16::MAX))
            .next()
            .is_some()
    }

    /// One entry's classification, if present in the merged view.
    pub(crate) fn entry_of(&self, r: IndirectRef) -> Option<XrefEntry> {
        self.entries.get(&(r.number, r.generation)).copied()
    }

    /// Iterate every merged xref entry (newest-wins view), in
    /// `(number, generation)` order. Used by the M23 residual audit
    /// to enumerate live objects without scanning every object number
    /// up to `/Size`.
    pub(crate) fn iter_entries(&self) -> impl Iterator<Item = (IndirectRef, XrefEntry)> + '_ {
        self.entries
            .iter()
            .map(|(&(n, g), &e)| (IndirectRef::new(n, g), e))
    }
}

/// Read the whole xref chain starting at `startxref`.
///
/// Detection of xref streams happens here: if the offset does not point
/// at the `xref` keyword we attempt an indirect-object parse and check
/// for `/Type /XRef`; anything else at the offset is a hard error.
pub(crate) fn read_xref_chain(
    input: &[u8],
    limits: &Limits,
    startxref: u64,
) -> Result<XrefTable, Error> {
    let mut entries: BTreeMap<(u32, u16), XrefEntry> = BTreeMap::new();
    let mut trailer = Dict::new();
    let mut latest_trailer: Option<Dict> = None;
    let mut section_offsets = Vec::new();
    let mut visited_offsets: HashSet<u64> = HashSet::new();

    let mut current = Some(startxref);
    while let Some(offset) = current {
        if !visited_offsets.insert(offset) {
            return Err(Error::CycleDetected {
                context: "/Prev xref chain",
            });
        }
        if section_offsets.len() as u64 >= limits.max_page_tree_nodes {
            return Err(Error::LimitExceeded {
                limit: "xref section count",
                max: limits.max_page_tree_nodes,
            });
        }
        if offset as usize > input.len() {
            return Err(Error::Parse(format!(
                "xref offset {offset} is beyond end of input ({} bytes)",
                input.len()
            )));
        }
        section_offsets.push(offset);

        let trailer_dict = read_one_section(input, limits, offset, &mut |key, entry| {
            // Newest section wins: /Prev sections only fill gaps.
            entries.entry(key).or_insert(entry);
        })?;

        if latest_trailer.is_none() {
            latest_trailer = Some(trailer_dict.clone());
        }
        for (k, v) in &trailer_dict {
            trailer.entry(k.clone()).or_insert_with(|| v.clone());
        }

        // A classic trailer may point at an additional xref stream with
        // compressed entries (§7.5.8.4 hybrid-reference files). Within the
        // same update section, the xref stream *replaces* entries seen in
        // the classic table: the table is a fallback for pre-1.5 readers,
        // while the stream carries the authoritative view (including
        // type-2 compressed objects a table cannot express).
        // Per the qpdf implementation notes (consistent with §7.5.8.4):
        //   "read the xref table first; then replace any object that it
        //    has already seen with any defined in the xref stream".
        // `/Prev` in the *xref stream* is ignored (the classic trailer's
        // /Prev drives the chain; a stream's /Prev would only matter for
        // a non-hybrid chain, which `read_one_section` already handles).
        if let Some(xrefstm) = trailer_dict.get(b"XRefStm".as_slice()) {
            let pos = match xrefstm {
                Object::Integer(i) if *i >= 0 => *i as u64,
                _ => {
                    return Err(Error::Structure(
                        "trailer /XRefStm is not a non-negative integer".into(),
                    ))
                }
            };
            if pos as usize > input.len() {
                return Err(Error::Parse(format!(
                    "/XRefStm offset {pos} is beyond end of input"
                )));
            }
            read_one_section(input, limits, pos, &mut |key, entry| {
                // XRefStm overrides classic entries from *this* section.
                entries.insert(key, entry);
            })?;
        }

        current = next_prev(&trailer_dict)?;
    }

    Ok(XrefTable {
        entries,
        trailer,
        latest_trailer: latest_trailer.unwrap_or_default(),
        section_offsets,
    })
}

/// Read one xref section at `offset`. Returns its trailer dictionary.
/// `pub(crate)` so the M23 audit can enumerate per-section entries —
/// the merged view discards superseded bodies that an audit must
/// still report.
pub(crate) fn read_one_section(
    input: &[u8],
    limits: &Limits,
    offset: u64,
    on_entry: &mut dyn FnMut((u32, u16), XrefEntry),
) -> Result<Dict, Error> {
    let mut parser = Parser::new(input, limits);
    parser.seek(offset as usize);

    // Distinguish "xref" keyword from an xref-stream object.
    match parser.read_word_token()? {
        Some(w) if w == b"xref" => {
            return read_classic_section(&mut parser, limits, offset, on_entry);
        }
        Some(_) | None => {
            // Not the classic keyword: maybe an xref-stream object.
            parser.seek(offset as usize);
        }
    }

    match parser.parse_indirect_object()? {
        Some(ind) => {
            let is_xref_stream = ind
                .object
                .as_dict_like()
                .and_then(|d| d.get(b"Type".as_slice()))
                .and_then(|o| o.as_name())
                .map(|n| n == b"XRef")
                .unwrap_or(false);
            if is_xref_stream {
                read_xref_stream_section(&ind, limits, on_entry)
            } else {
                Err(Error::Parse(format!(
                    "expected 'xref' at byte {offset}, found object {}",
                    ind.object_ref
                )))
            }
        }
        None => Err(Error::Parse(format!(
            "expected 'xref' keyword at byte {offset}"
        ))),
    }
}

/// Parse an xref stream's `/W`/`/Index` fields and emit entries.
///
/// `ind` is the already-parsed `n g obj` wrapper around the stream.
fn read_xref_stream_section(
    ind: &crate::parser::IndirectObject,
    limits: &Limits,
    on_entry: &mut dyn FnMut((u32, u16), XrefEntry),
) -> Result<Dict, Error> {
    let (dict, raw) = ind
        .object
        .as_stream()
        .ok_or_else(|| Error::Parse(format!("object {} is not a stream", ind.object_ref)))?;
    // The entries live in the *decoded* stream (§7.5.8.2 — xref
    // streams are almost always FlateDecode + PNG predictor).
    let decoded = crate::filter::decode_stream(dict, raw, limits)?;
    let data: &[u8] = &decoded;

    // /W: three integer widths, each 0..=8 (enough for 8-byte fields).
    let w_obj = dict
        .get(b"W".as_slice())
        .ok_or_else(|| Error::Structure("xref stream has no /W".into()))?;
    let w_arr = w_obj
        .as_array()
        .ok_or_else(|| Error::Structure("xref stream /W is not an array".into()))?;
    if w_arr.len() != 3 {
        return Err(Error::Structure(
            "xref stream /W must have 3 elements".into(),
        ));
    }
    let mut w = [0usize; 3];
    for (i, o) in w_arr.iter().enumerate() {
        let v = o
            .as_i64()
            .ok_or_else(|| Error::Structure("xref stream /W entries must be integers".into()))?;
        if !(0..=8).contains(&v) {
            return Err(Error::Unsupported {
                feature: "xref stream field width",
                detail: format!("/W[{i}] = {v}"),
            });
        }
        w[i] = v as usize;
    }
    let entry_len = w[0] + w[1] + w[2];
    if entry_len == 0 {
        return Err(Error::Structure("xref stream entry size is zero".into()));
    }

    // /Index: array of [first count] pairs; default [0 /Size].
    let index_pairs: Vec<(u32, u32)> = match dict.get(b"Index".as_slice()) {
        Some(Object::Array(a)) => {
            if a.len() % 2 != 0 {
                return Err(Error::Structure(
                    "xref stream /Index must have an even number of entries".into(),
                ));
            }
            let mut v = Vec::with_capacity(a.len() / 2);
            for pair in a.chunks_exact(2) {
                let first = pair[0]
                    .as_i64()
                    .filter(|i| *i >= 0 && *i <= u32::MAX as i64)
                    .ok_or_else(|| Error::Structure("bad /Index first object".into()))?
                    as u32;
                let count = pair[1]
                    .as_i64()
                    .filter(|i| *i >= 0 && *i <= u32::MAX as i64)
                    .ok_or_else(|| Error::Structure("bad /Index count".into()))?
                    as u32;
                v.push((first, count));
            }
            v
        }
        Some(_) => {
            return Err(Error::Structure(
                "xref stream /Index is not an array".into(),
            ))
        }
        None => {
            let size = dict
                .get(b"Size".as_slice())
                .and_then(|o| o.as_i64())
                .filter(|i| *i >= 0 && *i <= u32::MAX as i64)
                .ok_or_else(|| {
                    Error::Structure("xref stream missing /Size for default /Index".into())
                })? as u32;
            vec![(0, size)]
        }
    };

    let total_entries: u64 = index_pairs.iter().map(|&(_, c)| c as u64).sum();
    if total_entries > limits.max_array_elements as u64 {
        return Err(Error::LimitExceeded {
            limit: "xref stream entry count",
            max: limits.max_array_elements as u64,
        });
    }
    let needed = total_entries as usize * entry_len;
    if data.len() < needed {
        return Err(Error::UnexpectedEof {
            offset: data.len(),
            context: "xref stream data",
        });
    }

    let mut cursor = 0usize;
    for &(first, count) in &index_pairs {
        for i in 0..count {
            let obj_num = first
                .checked_add(i)
                .ok_or_else(|| Error::Structure("xref stream object number overflow".into()))?;
            let entry = parse_xref_stream_entry(&data[cursor..cursor + entry_len], w)?;
            cursor += entry_len;
            // Compressed objects are always generation 0 in the key;
            // their /ObjStm index is stored in the entry itself.
            on_entry((obj_num, 0), entry);
        }
    }

    // The trailer is the xref stream's own dictionary.
    Ok(dict.clone())
}

/// Decode one xref stream entry of `entry_len` bytes with field widths `w`.
fn parse_xref_stream_entry(buf: &[u8], w: [usize; 3]) -> Result<XrefEntry, Error> {
    let f1 = read_field(&buf[0..w[0]]);
    let f2 = read_field(&buf[w[0]..w[0] + w[1]]);
    let f3 = read_field(&buf[w[0] + w[1]..]);

    // Field widths of 0 imply the default value (§7.5.8.2 Table 17).
    let entry_type = if w[0] == 0 { 1 } else { f1 };
    match entry_type {
        0 => Ok(XrefEntry::Free {
            next_free: f2 as u32,
        }),
        1 => Ok(XrefEntry::InUse { offset: f2 }),
        2 => Ok(XrefEntry::Compressed {
            objstm: f2 as u32,
            index: f3 as u32,
        }),
        other => Err(Error::Parse(format!("bad xref stream entry type {other}"))),
    }
}

/// Read an unsigned big-endian field of up to 8 bytes (width 0 -> 0).
fn read_field(bytes: &[u8]) -> u64 {
    let mut v: u64 = 0;
    for &b in bytes {
        v = (v << 8) | b as u64;
    }
    v
}

/// Read `xref` subsections + `trailer` at the current position
/// (positioned just past the `xref` keyword). Returns the trailer dict.
fn read_classic_section(
    parser: &mut Parser,
    limits: &Limits,
    offset: u64,
    on_entry: &mut dyn FnMut((u32, u16), XrefEntry),
) -> Result<Dict, Error> {
    let mut total_entries: u64 = 0;
    loop {
        let before = parser.pos();
        match parser.read_word_token()? {
            None => {
                return Err(Error::UnexpectedEof {
                    offset: before,
                    context: "xref subsection or trailer",
                })
            }
            Some(w) if w == b"trailer" => break,
            Some(first_word) => {
                let first = parse_u32_word(&first_word).ok_or_else(|| {
                    Error::Parse(format!(
                        "bad xref subsection header '{}' near byte {before}",
                        String::from_utf8_lossy(&first_word)
                    ))
                })?;
                let count = match parser.read_word_token()? {
                    Some(w) => parse_u32_word(&w).ok_or_else(|| {
                        Error::Parse(format!(
                            "bad xref subsection count '{}' near byte {before}",
                            String::from_utf8_lossy(&w)
                        ))
                    })?,
                    None => {
                        return Err(Error::UnexpectedEof {
                            offset: before,
                            context: "xref subsection count",
                        })
                    }
                };
                total_entries = total_entries.saturating_add(count as u64);
                if total_entries > limits.max_array_elements as u64 {
                    return Err(Error::LimitExceeded {
                        limit: "xref entry count",
                        max: limits.max_array_elements as u64,
                    });
                }
                for i in 0..count {
                    let line = parser.read_xref_entry_line()?.ok_or(Error::UnexpectedEof {
                        offset: parser.pos(),
                        context: "xref entry line",
                    })?;
                    let obj_num = first.checked_add(i).ok_or_else(|| {
                        Error::Parse(format!(
                            "xref subsection overflows object number near byte {offset}"
                        ))
                    })?;
                    let entry = match line.kind {
                        b'n' => XrefEntry::InUse {
                            offset: line.offset,
                        },
                        b'f' => XrefEntry::Free {
                            next_free: line.offset as u32,
                        },
                        other => {
                            return Err(Error::Parse(format!(
                                "bad xref entry type '{}' near byte {}",
                                other as char,
                                parser.pos()
                            )))
                        }
                    };
                    on_entry((obj_num, line.generation as u16), entry);
                }
            }
        }
    }
    parser.parse_dict_at_pos()
}

/// Extract `/Prev` from a trailer dict, if present.
fn next_prev(trailer: &Dict) -> Result<Option<u64>, Error> {
    match trailer.get(b"Prev".as_slice()) {
        None => Ok(None),
        Some(Object::Integer(i)) if *i >= 0 => Ok(Some(*i as u64)),
        Some(_) => Err(Error::Structure(
            "trailer /Prev is not a non-negative integer".into(),
        )),
    }
}

/// Parse a subsection-header style unsigned-integer word.
fn parse_u32_word(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse::<u32>().ok()
}

// --------------------------------------------------------------------
// Recovery: rebuild the table by scanning for object definitions
// --------------------------------------------------------------------

/// What [`reconstruct`] did to a damaged file — surfaced through
/// [`crate::Document::recovery`] so a caller can tell a recovered
/// document from a well-formed one and see every assumption made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// The error the strict parse stopped at.
    pub strict_error: String,
    /// `n g obj` definitions found by the scan (after de-duplication:
    /// a later definition of the same object replaces an earlier one,
    /// as in an incremental update).
    pub objects_found: usize,
    /// Objects registered from inside `/Type /ObjStm` streams that no
    /// direct definition shadowed.
    pub objects_from_streams: usize,
    /// Where `/Root` came from: `"trailer dictionary"`, `"xref stream
    /// dictionary"` or `"/Type /Catalog scan"`.
    pub trailer_source: String,
    /// Everything that deviated from a clean read, in scan order.
    pub warnings: Vec<String>,
}

/// Rebuild an [`XrefTable`] from the object definitions actually
/// present in `input`, for a file whose `startxref` chain is unusable.
///
/// The scan is a single pass over the bytes for the `obj` keyword with
/// a preceding `<number> <generation>`; it is bounded by
/// `max_recovery_objects` and never resolves references, so it cannot
/// loop. Nothing is inferred beyond what the bytes state: an object is
/// registered at the offset where *it* says `n g obj`; a `/Root` is
/// taken from a `trailer` dictionary or an xref-stream dictionary, or
/// — recorded as a warning — from the single object declaring
/// `/Type /Catalog`; objects inside object streams are registered only
/// where no direct definition exists. An `/Encrypt` entry still refuses
/// the file.
pub(crate) fn reconstruct(
    input: &[u8],
    limits: &Limits,
    strict_error: &Error,
) -> Result<(XrefTable, RecoveryReport), Error> {
    let mut warnings: Vec<String> = Vec::new();
    let mut warn = |w: String| {
        if warnings.len() < 64 {
            warnings.push(w);
        }
    };

    // ---- 1. every `n g obj` ------------------------------------------
    let mut found: BTreeMap<u32, (u16, u64)> = BTreeMap::new();
    let mut order: Vec<(u32, u64)> = Vec::new();
    let mut i = 0usize;
    let mut replaced = 0usize;
    while i + 3 <= input.len() {
        if &input[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        let after_ok = i + 3 == input.len()
            || crate::lexer::is_white(input[i + 3])
            || matches!(input[i + 3], b'<' | b'[' | b'(' | b'/' | b'%');
        if !after_ok {
            i += 3;
            continue;
        }
        if let Some((num, gen, start)) = header_before(input, i) {
            if order.len() >= limits.max_recovery_objects {
                return Err(Error::LimitExceeded {
                    limit: "max_recovery_objects",
                    max: limits.max_recovery_objects as u64,
                });
            }
            if found.insert(num, (gen, start as u64)).is_some() {
                replaced += 1;
            }
            order.push((num, start as u64));
        }
        i += 3;
    }
    if found.is_empty() {
        return Err(Error::Parse(
            "recovery: no `n g obj` definitions found in the file".into(),
        ));
    }
    if replaced > 0 {
        warn(format!(
            "{replaced} object number(s) defined more than once; the last definition in the file is used"
        ));
    }

    // ---- 2. trailer: `trailer` dicts, then /Type /XRef dicts ---------
    let mut trailer = Dict::new();
    let mut trailer_source = String::new();
    let mut pos = 0usize;
    let mut trailers = 0usize;
    while let Some(k) = find_from(input, b"trailer", pos) {
        pos = k + 7;
        let mut p = Parser::new(input, limits);
        p.seek(pos);
        if let Ok(Some(Object::Dict(d))) = p.parse_object(0) {
            trailers += 1;
            // Later trailers win (incremental updates append).
            for (key, v) in d {
                trailer.insert(key, v);
            }
        }
        if trailers > 1024 {
            break;
        }
    }
    if trailer.contains_key(b"Root".as_slice()) {
        trailer_source = "trailer dictionary".into();
    }

    // Object bodies are parsed lazily below — only where a keyword
    // suggests they matter (xref-stream dicts, object streams, and the
    // catalog when no trailer names it). `region` bounds each body to
    // the next definition so a damaged object cannot make the scan
    // quadratic.
    let mut starts: Vec<u64> = order.iter().map(|(_, s)| *s).collect();
    starts.sort_unstable();
    starts.dedup();
    let region = |start: u64| -> &[u8] {
        let idx = starts.partition_point(|s| *s <= start);
        let end = starts.get(idx).copied().unwrap_or(input.len() as u64) as usize;
        &input[start as usize..end.min(input.len())]
    };
    let parse_at = |start: u64| -> Option<crate::parser::IndirectObject> {
        let mut p = Parser::new(input, limits);
        p.seek(start as usize);
        p.parse_indirect_object().ok().flatten()
    };

    if trailer_source.is_empty() {
        // Newest xref-stream dictionary in file order.
        for (_, start) in order.iter().rev() {
            if !contains(region(*start), b"/XRef") {
                continue;
            }
            if let Some(ind) = parse_at(*start) {
                if let Some(d) = ind.object.as_dict_like() {
                    if d.get(b"Type".as_slice()).and_then(|o| o.as_name()) == Some(b"XRef") {
                        for (key, v) in d {
                            trailer.entry(key.clone()).or_insert_with(|| v.clone());
                        }
                        if trailer.contains_key(b"Root".as_slice()) {
                            trailer_source = "xref stream dictionary".into();
                            break;
                        }
                    }
                }
            }
        }
    }
    if trailer.contains_key(b"Encrypt".as_slice()) {
        return Err(Error::Encrypted);
    }

    // ---- 3. object streams: members fill gaps -------------------------
    let mut entries: BTreeMap<(u32, u16), XrefEntry> = BTreeMap::new();
    for (num, (gen, start)) in &found {
        entries.insert((*num, *gen), XrefEntry::InUse { offset: *start });
    }
    let mut from_streams = 0usize;
    let mut objstms = 0usize;
    // `(objstm offset, ref)` of catalog objects found inside streams.
    let mut compressed_catalogs: Vec<(u64, IndirectRef)> = Vec::new();
    for (num, (_gen, start)) in &found {
        if !contains(region(*start), b"/ObjStm") {
            continue;
        }
        let Some(ind) = parse_at(*start) else {
            continue;
        };
        let Some((dict, raw)) = ind.object.as_stream() else {
            continue;
        };
        if dict.get(b"Type".as_slice()).and_then(|o| o.as_name()) != Some(b"ObjStm") {
            continue;
        }
        objstms += 1;
        if objstms > limits.max_objstm_entries {
            warn("object-stream count limit reached; remaining streams not expanded".into());
            break;
        }
        let decoded = match crate::filter::decode_stream(dict, raw, limits) {
            Ok(d) => d,
            Err(e) => {
                warn(format!(
                    "object stream {num} could not be decoded ({e}); its members are unavailable"
                ));
                continue;
            }
        };
        match crate::objstm::members(&decoded, dict, limits) {
            Ok(members) => {
                for (idx, (m, obj)) in members.into_iter().enumerate() {
                    if found.contains_key(&m) {
                        continue;
                    }
                    let key = (m, 0u16);
                    if let std::collections::btree_map::Entry::Vacant(v) = entries.entry(key) {
                        v.insert(XrefEntry::Compressed {
                            objstm: *num,
                            index: idx as u32,
                        });
                        from_streams += 1;
                        if obj
                            .as_dict()
                            .and_then(|d| d.get(b"Type".as_slice()))
                            .and_then(|o| o.as_name())
                            == Some(b"Catalog")
                        {
                            compressed_catalogs.push((*start, IndirectRef::new(m, 0)));
                        }
                    }
                }
            }
            Err(e) => warn(format!(
                "object stream {num} has an unreadable header ({e})"
            )),
        }
    }

    // ---- 4. /Root fallback: the object that declares /Type /Catalog ---
    if !trailer.contains_key(b"Root".as_slice()) {
        // `(file position, ref)` — direct catalogs at their own offset,
        // compressed ones at their object stream's.
        let mut positioned: Vec<(u64, IndirectRef)> = compressed_catalogs;
        for (num, (gen, start)) in &found {
            if !contains(region(*start), b"/Catalog") {
                continue;
            }
            if let Some(ind) = parse_at(*start) {
                if ind
                    .object
                    .as_dict()
                    .and_then(|d| d.get(b"Type".as_slice()))
                    .and_then(|o| o.as_name())
                    == Some(b"Catalog")
                {
                    positioned.push((*start, IndirectRef::new(*num, *gen)));
                }
            }
        }
        positioned.sort_by_key(|(pos, _)| *pos);
        let mut catalogs: Vec<IndirectRef> = positioned.iter().map(|(_, r)| *r).collect();
        match catalogs.len() {
            0 => {
                return Err(Error::Structure(
                    "recovery: no trailer /Root and no object with /Type /Catalog".into(),
                ))
            }
            1 => {
                warn(format!(
                    "no trailer /Root; catalog taken from the only /Type /Catalog object ({})",
                    catalogs[0]
                ));
            }
            n => {
                // Several catalogs (incremental saves): the one defined
                // last in the file is the newest, as with duplicate
                // object numbers — stated, not guessed.
                let last = catalogs[n - 1];
                warn(format!(
                    "no trailer /Root; {n} /Type /Catalog objects, using the last defined ({last})"
                ));
                catalogs = vec![last];
            }
        }
        trailer.insert(b"Root".to_vec(), Object::Ref(catalogs[0]));
        trailer_source = "/Type /Catalog scan".into();
    }
    if !trailer.contains_key(b"Size".as_slice()) {
        let max = found.keys().next_back().copied().unwrap_or(0) as i64 + 1;
        trailer.insert(b"Size".to_vec(), Object::Integer(max));
    }

    let report = RecoveryReport {
        strict_error: strict_error.to_string(),
        objects_found: found.len(),
        objects_from_streams: from_streams,
        trailer_source,
        warnings,
    };
    Ok((
        XrefTable {
            entries,
            latest_trailer: trailer.clone(),
            trailer,
            section_offsets: Vec::new(),
        },
        report,
    ))
}

/// Given `obj` at `kw`, read back `<gen>` and `<num>` tokens; returns
/// `(num, gen, offset_of_num)` when the bytes form `num gen obj` with
/// a non-regular byte (or the file start) before `num`.
fn header_before(input: &[u8], kw: usize) -> Option<(u32, u16, usize)> {
    let mut j = kw;
    // whitespace between gen and obj (at least one byte)
    let mut ws = 0;
    while j > 0 && crate::lexer::is_white(input[j - 1]) {
        j -= 1;
        ws += 1;
    }
    if ws == 0 {
        return None;
    }
    let gen_end = j;
    while j > 0 && input[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j == gen_end || gen_end - j > 5 {
        return None;
    }
    let gen: u32 = std::str::from_utf8(&input[j..gen_end]).ok()?.parse().ok()?;
    if gen > u16::MAX as u32 {
        return None;
    }
    let mut ws = 0;
    while j > 0 && crate::lexer::is_white(input[j - 1]) {
        j -= 1;
        ws += 1;
    }
    if ws == 0 {
        return None;
    }
    let num_end = j;
    while j > 0 && input[j - 1].is_ascii_digit() {
        j -= 1;
    }
    if j == num_end || num_end - j > 10 {
        return None;
    }
    if j > 0 && !(crate::lexer::is_white(input[j - 1]) || is_delim(input[j - 1])) {
        return None;
    }
    let num: u64 = std::str::from_utf8(&input[j..num_end]).ok()?.parse().ok()?;
    if num > u32::MAX as u64 {
        return None;
    }
    Some((num as u32, gen as u16, j))
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    find_from(hay, needle, 0).is_some()
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn header_before_accepts_only_well_formed_headers() {
        let s = b"xx 12 0 obj<<>>";
        assert_eq!(header_before(s, 8), Some((12, 0, 3)));
        assert_eq!(header_before(b"12 0obj", 4), None, "no space before obj");
        assert_eq!(header_before(b"120 obj", 4), None, "missing generation");
        assert_eq!(header_before(b"a12 0 obj", 6), None, "glued to a word");
        assert_eq!(header_before(b"12 99999 obj", 9), None, "generation > u16");
    }
}
