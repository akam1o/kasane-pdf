//! PDF basic object types (ISO 32000-1:2008, §7.3).
//!
//! [`Object`] is the in-memory representation of a parsed object. Stream
//! objects are represented by [`Object::Stream`], which carries the
//! **raw, still-encoded** byte range of the stream data within the input
//! buffer. Decoding filters (`/FlateDecode`, …) is intentionally out of
//! scope for the object layer; the raw range is recorded so that future layers do not
//! have to re-discover it.

use std::collections::BTreeMap;

/// A reference to an indirect object, written `n g R` in the file
/// (ISO 32000-1:2008, §7.3.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IndirectRef {
    /// Object number (`n`, > 0 for real objects; 0 is used by free xref
    /// entries and never resolves).
    pub number: u32,
    /// Generation number (`g`).
    pub generation: u16,
}

impl IndirectRef {
    pub(crate) fn new(number: u32, generation: u16) -> Self {
        IndirectRef { number, generation }
    }
}

impl std::fmt::Display for IndirectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

/// A PDF name (e.g. `/Page`), stored with `#xx` escapes already decoded.
///
/// Names are byte strings (§7.3.5); comparison is on the decoded bytes.
pub type Name = Vec<u8>;

/// A dictionary maps decoded name keys to objects (§7.3.7).
pub type Dict = BTreeMap<Name, Object>;

/// A parsed PDF object.
///
/// Integer and real numbers are kept distinct per §7.3.3; use
/// [`Object::as_number`] when either is acceptable.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// Integer number.
    Integer(i64),
    /// Real number.
    Real(f64),
    /// Name object; value excludes the leading `/` and has `#xx` escapes
    /// already decoded.
    Name(Name),
    /// String object (from a literal `(…)` or hexadecimal `<…>` string),
    /// stored as raw bytes. Text encoding is not interpreted.
    String(Vec<u8>),
    /// Array object.
    Array(Vec<Object>),
    /// Dictionary object.
    Dict(Dict),
    /// A stream object. `dict` is the stream dictionary; `data` is the
    /// raw, still-filtered stream contents (between `stream`+EOL and
    /// `endstream`). Filters are **not** applied.
    Stream {
        /// The stream's attribute dictionary (`/Length`, `/Filter`, …).
        dict: Dict,
        /// Raw encoded stream bytes.
        data: Vec<u8>,
    },
    /// Indirect reference `n g R`.
    Ref(IndirectRef),
}

impl Object {
    /// Returns the name value if this is a `Name` object.
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    /// Returns the byte contents if this is a `String` object.
    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Object::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the boolean if this is a `Bool` object.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the integer if this is an `Integer` object.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the numeric value if this is an `Integer` or `Real` object.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    /// Returns the elements if this is an `Array` object.
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Mutable access to the elements of an `Array` object.
    pub(crate) fn as_array_mut(&mut self) -> Option<&mut Vec<Object>> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns the dictionary if this is a `Dict` object.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            _ => None,
        }
    }

    /// Returns the dictionary if this is a `Dict` or a `Stream` object
    /// (both carry a dictionary).
    pub fn as_dict_like(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) | Object::Stream { dict: d, .. } => Some(d),
            _ => None,
        }
    }

    /// Returns the reference if this is an indirect reference object.
    pub fn as_ref(&self) -> Option<IndirectRef> {
        match self {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
    }

    /// Returns the stream dictionary and raw data if this is a `Stream`.
    pub fn as_stream(&self) -> Option<(&Dict, &[u8])> {
        match self {
            Object::Stream { dict, data } => Some((dict, data)),
            _ => None,
        }
    }
}
