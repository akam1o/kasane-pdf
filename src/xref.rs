//! Classic cross-reference table and trailer parsing
//! (ISO 32000-1:2008, §7.5.4–7.5.5).
//!
//! Supported: plain `xref` sections (with multiple subsections), the
//! `trailer` dictionary, and `/Prev` chains of classic sections — which
//! is how incremental updates appear in otherwise-M0-format files.
//!
//! Not supported (reported as [`Error::Unsupported`]):
//! * xref streams (`startxref` pointing at `n g obj` with `/Type /XRef`)
//! * hybrid-reference files (trailer `/XRefStm`)
//! * object streams (xref-stream files; classic files referencing
//!   `/ObjStm` objects are rejected at resolution time)
//!
//! Entries are stored as a map `(obj, gen) -> byte offset`. Both `n` and
//! `f` entry types are recorded; only `n` entries resolve.

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
    /// Retained for future diagnostics and trailer-key provenance.
    #[allow(dead_code)]
    pub(crate) latest_trailer: Dict,
    /// Byte offsets of all xref sections read, newest first.
    /// Retained for diagnostics (e.g. explaining /Prev chain failures).
    #[allow(dead_code)]
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
fn read_one_section(
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
                Err(Error::Unsupported {
                    feature: "xref stream",
                    detail: format!("object {} at byte {offset}", ind.object_ref),
                })
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
                    on_entry((obj_num, line.generation), entry);
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
