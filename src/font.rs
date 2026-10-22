//! Font handling for text rendering (ISO 32000-1 §9).
//!
//! M5 covers **embedded TrueType simple fonts**:
//! `/Subtype /TrueType` + `/FontDescriptor /FontFile2` + a
//! `WinAnsiEncoding` (or an encoding dictionary with a
//! `WinAnsiEncoding`/`MacRomanEncoding` `BaseEncoding` plus an
//! optional `Differences` array).
//!
//! M7 adds **composite Type 0 fonts** — `/Subtype /Type0` with
//! `/Encoding /Identity-H`, exactly one `/DescendantFonts` entry of
//! `/CIDFontType2`, and a `/FontFile2` TrueType program. String bytes
//! are read as 2-byte big-endian codes → CID (= code) →
//! `/CIDToGIDMap` → GID. Widths come from `/DW`/`/W` (never the
//! font's `hmtx`). `Identity-V`, CIDFontType0/CFF, other encodings,
//! non-embedded programs and `/FontFile3` are refused, never silently
//! substituted.
//!
//! The pipeline is deliberately split into three steps (§9.4.3,
//! §9.6.6):
//!
//! 1. *char code* — a byte of the string operand.
//! 2. *glyph name* — via the font's `/Encoding` (WinAnsi table or
//!    `Differences`-overridden).
//! 3. *glyph id* — glyph name → Unicode (AGL) → the font's `(3,1)`
//!    cmap; if no `(3,1)`, name → Mac-Roman code → `(1,0)` cmap;
//!    last resort, the font's `post` table. Missing glyphs map to
//!    `.notdef` (glyph 0).
//!
//! Widths come from the `/Widths` array (PDF user space units,
//! 1000-unit em) — *not* from the font's `hmtx`. They may differ.
//!
//! The module does **no** text shaping, ligature substitution,
//! kerning, or hinting — each character is placed independently at
//! the position the PDF operators dictate.

use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use crate::Document;
use std::cell::RefCell;
use std::rc::Rc;
use ttf_parser::cff::Table as CffTable;
use ttf_parser::{Face, GlyphId, OutlineBuilder};

// --------------------------------------------------------------------
// Public surface used by the renderer
// --------------------------------------------------------------------

/// A resolved, embedded TrueType font ready for glyph lookup and
/// outline extraction. Holds the decoded font program plus the
/// PDF-side encoding/widths data.
pub(crate) struct SimpleFont {
    /// Decoded `/FontFile2` bytes — kept alive for `face`.
    program: Vec<u8>,
    /// Encoding table: char code (0..=255) → glyph name.
    /// `None` means the code is not encodable (renders as .notdef).
    encoding: [Option<&'static str>; 256],
    /// Glyph advances in PDF 1/1000-em units, indexed by char code.
    widths: [f64; 256],
    /// `/ToUnicode` CMap — `None` if the font lacks one.
    tounicode: Option<crate::tounicode::ToUnicode>,
    /// `Face` built over `program`. `Face<'a>` borrows, so we keep
    /// it behind `Rc<RefCell>` built lazily per page render.
    face: RefCell<Option<Face<'static>>>,
}

/// A resolved `/Type1` (or `/MMType1`) font backed by an embedded
/// CFF program (`/FontDescriptor /FontFile3`). Outlines and glyph-id
/// lookup come from `ttf_parser`'s raw-CFF `Table`; encoding and
/// widths come from the PDF font dict. Single-byte codes.
///
/// `/FontFile` (Adobe Type1 / eexec) is **not** handled — that is a
/// different charstring format (Type 1, not Type 2) and is refused
/// at load time with `Error::Unsupported`.
pub(crate) struct Type1Font {
    /// Decoded `/FontFile3` CFF bytes — kept alive for `cff`.
    /// `None` for a non-embedded (Standard-14-style) Type1 font: such
    /// fonts carry no program, so [`Type1Font::outline`] refuses — the
    /// renderer fails, but text extraction still gets widths + Unicode.
    program: Option<Vec<u8>>,
    /// Encoding table: char code (0..=255) → glyph-name bytes. For
    /// CFF, names resolve via the font's charset, so we keep raw
    /// bytes (e.g. `/G36`) rather than an AGL `&'static str`.
    encoding: EncodingTable,
    /// Glyph advances in PDF 1/1000-em units, indexed by char code.
    widths: [f64; 256],
    /// `/ToUnicode` CMap — `None` if the font lacks one.
    tounicode: Option<crate::tounicode::ToUnicode>,
    /// When `true`, codes with no explicit `/Encoding` name resolve
    /// through the CFF program's own encoding + charset (the built-in
    /// Type1 behaviour). Always true for our CFF-backed Type1 fonts.
    builtin_cff_encoding: bool,
    /// Lazily-built CFF table over `program` (same borrow pattern as
    /// `SimpleFont::face`).
    cff: RefCell<Option<CffTable<'static>>>,
}

/// The resolved font behind a `Tf` selection — a simple TrueType
/// font (M5), a CFF Type1 font, or a composite Type 0 / CIDFontType2
/// font (M7). They share outline extraction and the §9.4 text-state
/// machinery; they differ in how string bytes become codes, how codes
/// become glyph ids, and where widths come from.
/// A `/Type3` font — each glyph is a `/CharProcs` content stream the
/// renderer executes (like a miniature Form XObject) instead of an
/// outline program. The font dict supplies `/Encoding` (code→char
/// name), `/FontMatrix` (glyph→text space), `/Widths`, and the
/// `/Resources` the char programs resolve their operands against.
pub(crate) struct Type3Font {
    /// `code → char name` per `/Encoding` (WinAnsi name or a
    /// `/Differences` dict). `None` = code is not encodable.
    encoding: EncodingTable,
    /// `char name → (stream dict, raw bytes)` per `/CharProcs`.
    /// Resolved lazily — the map keys are name bytes; lookups deref
    /// the stream object on demand.
    charprocs: Dict,
    /// `/FontMatrix` — glyph space → text space (default 0.001 scale).
    font_matrix: crate::matrix::Matrix,
    /// Glyph advances in PDF 1/1000-em units, indexed by char code.
    widths: [f64; 256],
    /// `/ToUnicode` CMap — `None` if the font lacks one.
    tounicode: Option<crate::tounicode::ToUnicode>,
    /// `/Resources` the CharProcs resolve against (`None` → inherit
    /// the page's). Stored unresolved; the renderer derefs per use.
    resources: Option<Object>,
}

#[derive(Clone)]
pub(crate) enum Font {
    Simple(Rc<SimpleFont>),
    Composite(Rc<CompositeFont>),
    Type1(Rc<Type1Font>),
    Type3(Rc<Type3Font>),
}

/// Resolve `name` in `/Resources /Font` — dispatch on `/Subtype`:
/// `/TrueType` → [`SimpleFont`], `/Type0` → [`CompositeFont`]
/// (CIDFontType2 + Identity-H only).
pub(crate) fn load_font(
    doc: &Document,
    resources: &Object,
    name: &[u8],
    limits: &Limits,
) -> Result<Font, Error> {
    // Peek at the subtype to pick the loader; each loader re-resolves.
    let obj = doc
        .resolve_resource(resources, "Font", name)?
        .ok_or_else(|| {
            Error::Structure(format!(
                "/Font /{} is not defined in the current resources",
                String::from_utf8_lossy(name)
            ))
        })?;
    let dict = obj.as_dict().ok_or_else(|| {
        Error::Structure(format!(
            "/Font /{} is not a dictionary",
            String::from_utf8_lossy(name)
        ))
    })?;
    match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
        Some(b"TrueType") => Ok(Font::Simple(load_simple_font(
            doc, resources, name, limits,
        )?)),
        Some(b"Type1") | Some(b"MMType1") => Ok(Font::Type1(load_type1_font(doc, dict, limits)?)),
        Some(b"Type3") => Ok(Font::Type3(load_type3_font(doc, dict, limits)?)),
        Some(b"Type0") => Ok(Font::Composite(load_composite_font(doc, dict, limits)?)),
        Some(other) => Err(Error::Unsupported {
            feature: "font subtype",
            detail: format!(
                "/{} — only /TrueType, /Type1 (CFF /FontFile3), /MMType1, /Type3, and /Type0 are supported",
                String::from_utf8_lossy(other)
            ),
        }),
        None => Err(Error::Structure(format!(
            "/Font /{} has no /Subtype",
            String::from_utf8_lossy(name)
        ))),
    }
}

/// Resolve `name` in `/Resources /Font`, load it as a simple
/// TrueType font, and return a ready-to-use [`SimpleFont`].
///
/// Errors:
/// - missing name → `Error::Structure`
/// - not a `/Font` dict or unsupported subtype → `Unsupported`
/// - not embedded / no `/FontFile2` → `Unsupported`
/// - bad widths/descriptor → `Structure`
pub(crate) fn load_simple_font(
    doc: &Document,
    resources: &Object,
    name: &[u8],
    limits: &Limits,
) -> Result<Rc<SimpleFont>, Error> {
    let obj = doc
        .resolve_resource(resources, "Font", name)?
        .ok_or_else(|| {
            Error::Structure(format!(
                "/Font /{} is not defined in the current resources",
                String::from_utf8_lossy(name)
            ))
        })?;
    let dict = obj.as_dict().ok_or_else(|| {
        Error::Structure(format!(
            "/Font /{} is not a dictionary",
            String::from_utf8_lossy(name)
        ))
    })?;
    match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
        Some(b"TrueType") => {}
        Some(other) => {
            return Err(Error::Unsupported {
                feature: "font subtype",
                detail: format!(
                    "/{} — only /TrueType is supported in M5",
                    String::from_utf8_lossy(other)
                ),
            })
        }
        None => {
            return Err(Error::Structure(format!(
                "/Font /{} has no /Subtype",
                String::from_utf8_lossy(name)
            )))
        }
    }

    // ---- FirstChar / LastChar / Widths --------------------------
    let first_char = dict
        .get(b"FirstChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let last_char = dict
        .get(b"LastChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(255);
    let widths_obj = dict.get(b"Widths".as_slice());
    let mut widths = [0.0f64; 256];
    if let Some(w) = widths_obj {
        let arr = w
            .as_array()
            .ok_or_else(|| Error::Structure("/Widths is not an array".into()))?;
        let expect = (last_char - first_char + 1) as usize;
        if arr.len() < expect {
            return Err(Error::Structure(format!(
                "/Widths has {} entries, expected at least {}",
                arr.len(),
                expect
            )));
        }
        for (i, e) in arr.iter().enumerate().take(expect) {
            let w = e
                .as_number()
                .ok_or_else(|| Error::Structure("/Widths entries must be numbers".into()))?;
            if !(w.is_finite() && w >= 0.0) {
                return Err(Error::Structure(format!(
                    "/Widths[{i}] is not a finite non-negative number"
                )));
            }
            widths[first_char as usize + i] = w;
        }
    } else {
        return Err(Error::Structure(
            "TrueType font has no /Widths — cannot lay out text".into(),
        ));
    }

    // ---- Encoding ------------------------------------------------
    let encoding = resolve_font_encoding(doc, dict, limits)?;

    // ---- FontDescriptor + FontFile2 -------------------------------
    let fd_obj = dict
        .get(b"FontDescriptor".as_slice())
        .ok_or_else(|| Error::Structure("font has no /FontDescriptor".into()))?;
    let fd = doc.deref(fd_obj)?;
    let fd_dict = fd
        .as_dict()
        .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;
    // Nonsymbolic check: the Symbolic flag (Table 123 bit 3) must
    // not be set — M5 only handles non-symbolic fonts.
    if let Some(f) = fd_dict.get(b"Flags".as_slice()).and_then(|o| o.as_i64()) {
        if f & 4 != 0 {
            return Err(Error::Unsupported {
                feature: "symbolic font",
                detail: "the Symbolic flag is set; M5 handles non-symbolic fonts only".into(),
            });
        }
    }
    let ff2_obj = fd_dict
        .get(b"FontFile2".as_slice())
        .ok_or_else(|| Error::Unsupported {
            feature: "non-embedded TrueType font",
            detail: "/FontFile2 is required; OS substitution is not performed".into(),
        })?;
    let ff2 = doc.deref(ff2_obj)?;
    let (ff2_dict, ff2_raw) = ff2
        .as_stream()
        .ok_or_else(|| Error::Structure("/FontFile2 is not a stream".into()))?;
    let program = doc.decode_stream(ff2_dict, ff2_raw)?;
    if program.len() as u64 > limits.max_stream_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_stream_bytes (embedded font)",
            max: limits.max_stream_bytes,
        });
    }

    // ---- ToUnicode (optional) ------------------------------------
    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;

    Ok(Rc::new(SimpleFont {
        program,
        encoding,
        widths,
        tounicode,
        face: RefCell::new(None),
    }))
}

/// Load a `/Type1` (or `/MMType1`) font dictionary backed by an
/// embedded CFF program (`/FontDescriptor /FontFile3`).
///
/// Accepted: `/Subtype /Type1` or `/MMType1`, `/FontFile3` (raw CFF —
/// parsed by `ttf_parser::tables::cff::Table`), `/Encoding` absent,
/// `/WinAnsiEncoding`, or a dict whose `/Differences` name the glyphs.
///
/// Refused: `/FontFile` (Adobe Type1 / eexec — Type 1 charstrings, a
/// different bytecode), `/FontFile2` (that is TrueType), non-embedded
/// fonts (no outlines → would render blank). A `/BaseEncoding` other
/// than `/WinAnsiEncoding` is refused like the TrueType path.
fn load_type1_font(doc: &Document, dict: &Dict, limits: &Limits) -> Result<Rc<Type1Font>, Error> {
    // ---- FirstChar / LastChar / Widths --------------------------
    let first_char = dict
        .get(b"FirstChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let last_char = dict
        .get(b"LastChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(255);
    let mut widths = [0.0f64; 256];
    if let Some(w_obj) = dict.get(b"Widths".as_slice()) {
        let w = doc.deref(w_obj)?;
        let arr = w
            .as_array()
            .ok_or_else(|| Error::Structure("/Widths is not an array".into()))?;
        let expect = (last_char - first_char + 1).max(0) as usize;
        for (i, e) in arr.iter().enumerate().take(expect.min(arr.len())) {
            // Entries may themselves be indirect.
            let e = doc.deref(e).unwrap_or_else(|_| e.clone());
            if let Some(v) = e.as_number() {
                if v.is_finite() && v >= 0.0 {
                    widths[first_char as usize + i] = v;
                }
            }
        }
    }

    // ---- Encoding → byte-name table ------------------------------
    let (encoding, builtin) = resolve_type1_encoding(doc, dict)?;

    // ---- FontDescriptor + FontFile3 (CFF) -------------------------
    // ---- Font program (optional) --------------------------------
    // Resolve the program bytes:
    //   * `/FontFile3` (CFF) → decode + validate.
    //   * `/FontFile` (Adobe Type 1 / eexec) → present but unparseable
    //     → Unsupported (don't degrade to metrics-only: the file *has*
    //     outlines, we just can't read them — calling it "supported for
    //     text" while the program is present is misleading).
    //   * no font-file / no descriptor → `None` → metrics-only font.
    //     Text extraction still gets widths/encoding/Unicode; outline
    //     calls refuse so render honestly reports Unsupported.
    let program: Option<Vec<u8>> = match dict.get(b"FontDescriptor".as_slice()) {
        None => None,
        Some(fd_obj) => {
            let fd = doc.deref(fd_obj)?;
            let fd_dict = fd
                .as_dict()
                .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;
            if fd_dict.get(b"FontFile".as_slice()).is_some() {
                return Err(Error::Unsupported {
                    feature: "Type1 /FontFile (Adobe Type 1 program)",
                    detail: "only CFF /FontFile3 Type1 programs are supported".into(),
                });
            }
            match fd_dict.get(b"FontFile3".as_slice()) {
                None => None,
                Some(ff3_obj) => {
                    let ff3 = doc.deref(ff3_obj)?;
                    let (ff3_dict, ff3_raw) = ff3
                        .as_stream()
                        .ok_or_else(|| Error::Structure("/FontFile3 is not a stream".into()))?;
                    // `/Subtype /CIDFontType0C` inside /FontFile3.
                    if let Some(st) = ff3_dict
                        .get(b"Subtype".as_slice())
                        .and_then(|o| o.as_name())
                    {
                        if st == b"CIDFontType0C" {
                            return Err(Error::Unsupported {
                                feature: "CIDFontType0 CFF",
                                detail: "/FontFile3 CIDFontType0C is a CID font, not simple Type1"
                                    .into(),
                            });
                        }
                    }
                    let program = doc.decode_stream(ff3_dict, ff3_raw)?;
                    if program.len() as u64 > limits.max_stream_bytes {
                        return Err(Error::LimitExceeded {
                            limit: "max_stream_bytes (embedded font)",
                            max: limits.max_stream_bytes,
                        });
                    }
                    CffTable::parse(&program).ok_or_else(|| {
                        Error::Structure("/FontFile3 is not a valid CFF program".into())
                    })?;
                    Some(program)
                }
            }
        }
    };

    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;

    Ok(Rc::new(Type1Font {
        program,
        encoding,
        widths,
        tounicode,
        builtin_cff_encoding: builtin,
        cff: RefCell::new(None),
    }))
}

/// A 256-entry encoding table mapping a char code to glyph-name bytes.
/// `None` = no explicit name → the font's built-in encoding applies.
type EncodingTable = [Option<Vec<u8>>; 256];

/// Build the Type1 encoding table.
///
/// Returns `(table, use_builtin)`. `table[c]` is the glyph-name bytes
/// for code `c` when an explicit encoding applies; `use_builtin`
/// tells [`Type1Font::glyph_id`] to fall back to the CFF program's own
/// encoding/charset when a code has no explicit name (the common case
/// for embedded Type1 without `/Encoding`).
fn resolve_type1_encoding(
    doc: &Document,
    font_dict: &Dict,
) -> Result<(EncodingTable, bool), Error> {
    let enc_obj = match font_dict.get(b"Encoding".as_slice()) {
        None | Some(Object::Null) => {
            // No explicit encoding → the font's built-in (CFF) encoding
            // and charset apply. Signal the caller to use them.
            return Ok(([const { None }; 256], true));
        }
        Some(o) => o.clone(),
    };
    let enc = doc.deref(&enc_obj)?;
    let mut table: EncodingTable = [const { None }; 256];

    // Seed with WinAnsi base when present (named or /BaseEncoding).
    let has_winansi = match &enc {
        Object::Name(n) => n.as_slice() == b"WinAnsiEncoding",
        Object::Dict(d) => {
            matches!(
                d.get(b"BaseEncoding".as_slice()).and_then(|o| o.as_name()),
                Some(b"WinAnsiEncoding")
            )
        }
        _ => false,
    };
    if has_winansi {
        for (i, n) in WINANSI.iter().enumerate() {
            table[i] = n.map(|s| s.as_bytes().to_vec());
        }
    }

    // Apply /Differences over whatever base is in effect.
    if let Object::Dict(d) = &enc {
        if let Some(diff_obj) = d.get(b"Differences".as_slice()) {
            let arr = doc.deref(diff_obj)?;
            let arr = arr
                .as_array()
                .ok_or_else(|| Error::Structure("/Differences is not an array".into()))?;
            let mut idx: Option<usize> = None;
            for e in arr {
                match e {
                    Object::Integer(i) => idx = Some((*i).clamp(0, 255) as usize),
                    Object::Real(r) => idx = Some((*r as i64).clamp(0, 255) as usize),
                    Object::Name(n) => {
                        let i = idx.ok_or_else(|| {
                            Error::Structure("/Differences entry has a name before any code".into())
                        })?;
                        if i > 255 {
                            return Err(Error::Structure("/Differences code exceeds 255".into()));
                        }
                        table[i] = Some(n.clone());
                        idx = Some(i + 1);
                    }
                    _ => {
                        return Err(Error::Structure(
                            "/Differences entries must be integers or names".into(),
                        ))
                    }
                }
            }
        }
    }
    // When a name (WinAnsi) or dict encoding was given, codes with no
    // explicit name still fall back to the CFF built-in encoding — so
    // `builtin` stays true in all cases for Type1/CFF.
    Ok((table, true))
}

/// Build the char-code → glyph-name table the font will use.
///
/// - `/Encoding /WinAnsiEncoding` → Annex D table.
/// - `/Encoding << /BaseEncoding /WinAnsiEncoding …>>` → that table.
/// - `/Encoding << /BaseEncoding /MacRomanEncoding …>>` → we do not
///   ship a MacRoman table; this combination is refused.
/// - `/Encoding << /Differences […] >>` → applied over the base.
/// - Missing `/Encoding` → `Error::Unsupported` (we do not guess).
fn resolve_font_encoding(
    doc: &Document,
    font_dict: &Dict,
    _limits: &Limits,
) -> Result<[Option<&'static str>; 256], Error> {
    let enc_obj = match font_dict.get(b"Encoding".as_slice()) {
        None | Some(Object::Null) => {
            return Err(Error::Unsupported {
                feature: "font without /Encoding",
                detail:
                    "the implicit (built-in) encoding is not used; M5 requires /WinAnsiEncoding"
                        .into(),
            })
        }
        Some(o) => o.clone(),
    };
    let enc = doc.deref(&enc_obj)?;
    let mut table: [Option<&'static str>; 256] = [None; 256];

    // Seed with the base table — WinAnsi or a dict's BaseEncoding.
    let base_name: &'static str = match &enc {
        Object::Name(n) => match n.as_slice() {
            b"WinAnsiEncoding" => "winansi",
            b"MacRomanEncoding" => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: "MacRomanEncoding is not supported in M5".into(),
                })
            }
            b"MacExpertEncoding" => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: "MacExpertEncoding is not supported in M5".into(),
                })
            }
            other => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: format!(
                        "/{} — only WinAnsiEncoding is supported",
                        String::from_utf8_lossy(other)
                    ),
                })
            }
        },
        Object::Dict(d) => match d.get(b"BaseEncoding".as_slice()).and_then(|o| o.as_name()) {
            Some(b"WinAnsiEncoding") => "winansi",
            Some(b"MacRomanEncoding") => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: "MacRomanEncoding is not supported in M5".into(),
                })
            }
            Some(b"MacExpertEncoding") => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: "MacExpertEncoding is not supported in M5".into(),
                })
            }
            Some(other) => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: format!(
                        "/BaseEncoding /{} — only WinAnsiEncoding is supported",
                        String::from_utf8_lossy(other)
                    ),
                })
            }
            None => "implicit", // no BaseEncoding — font's built-in
        },
        _ => {
            return Err(Error::Structure(
                "/Encoding is neither a name nor a dictionary".into(),
            ))
        }
    };
    if base_name == "winansi" {
        for (i, n) in WINANSI.iter().enumerate() {
            table[i] = *n;
        }
    } else {
        // No base table we can use — a bare Differences dict. The
        // spec says the implicit base is the font's built-in encoding;
        // we do not have a MacRoman table, so refuse.
        return Err(Error::Unsupported {
            feature: "font encoding dictionary without /BaseEncoding",
            detail: "the font's built-in encoding is not consulted in M5".into(),
        });
    }

    // Apply Differences, if any.
    if let Object::Dict(d) = &enc {
        if let Some(diff_obj) = d.get(b"Differences".as_slice()) {
            let arr = doc.deref(diff_obj)?;
            let arr = arr
                .as_array()
                .ok_or_else(|| Error::Structure("/Differences is not an array".into()))?;
            let mut idx: Option<usize> = None;
            for e in arr {
                match e {
                    Object::Integer(i) => idx = Some((*i).clamp(0, 255) as usize),
                    Object::Real(r) => idx = Some((*r as i64).clamp(0, 255) as usize),
                    Object::Name(n) => {
                        let i = idx.ok_or_else(|| {
                            Error::Structure("/Differences entry has a name before any code".into())
                        })?;
                        if i > 255 {
                            return Err(Error::Structure("/Differences code exceeds 255".into()));
                        }
                        // Leak a 'static name into the table — the
                        // names are drawn from a fixed static pool.
                        table[i] = Some(static_name(n));
                        idx = Some(i + 1);
                    }
                    _ => {
                        return Err(Error::Structure(
                            "/Differences entries must be integers or names".into(),
                        ))
                    }
                }
            }
        }
    }
    Ok(table)
}

/// Look up a glyph name in the static AGL-derived pool. Only names
/// appearing in WinAnsiEncoding are present; any other name yields
/// `None` (renders as `.notdef`).
fn static_name(n: &[u8]) -> &'static str {
    let s = std::str::from_utf8(n).unwrap_or("");
    GLYPH_NAME_TO_UNICODE
        .iter()
        .find(|(name, _)| name.as_bytes() == s.as_bytes())
        .map(|(name, _)| *name)
        .unwrap_or(".notdef")
}

impl SimpleFont {
    /// The font program parsed as a `ttf_parser::Face`. Lazily built
    /// and cached — the `Face` borrows `program` so we transmute the
    /// lifetime to `'static` inside the `RefCell` (safe: `program` is
    /// owned by `self` and never moves).
    fn face(&self) -> Result<std::cell::Ref<'_, Face<'static>>, Error> {
        {
            let mut slot = self.face.borrow_mut();
            if slot.is_none() {
                let face = Face::parse(&self.program, 0)
                    .map_err(|e| Error::Structure(format!("TrueType font program: {e:?}")))?;
                // SAFETY: `face` borrows `self.program`; `self` owns
                // both fields and `program` never moves or mutates.
                let face: Face<'static> = unsafe { std::mem::transmute(face) };
                *slot = Some(face);
            }
        }
        Ok(std::cell::Ref::map(self.face.borrow(), |o| {
            o.as_ref().unwrap()
        }))
    }

    /// Char code (0..=255) → glyph name per the PDF encoding.
    pub(crate) fn glyph_name(&self, code: u8) -> Option<&'static str> {
        self.encoding[code as usize]
    }

    /// Char code → glyph advance in 1/1000-em units (PDF `/Widths`).
    pub(crate) fn width(&self, code: u8) -> f64 {
        self.widths[code as usize]
    }

    /// Char code → glyph id, or `None` if the font has no glyph for it
    /// (renders as `.notdef` — glyph 0). Follows §9.6.6.4:
    /// code → name → Unicode (AGL) → `(3,1)` cmap; falls back to the
    /// `(1,0)` Mac-Roman subtable when no `(3,1)` exists, then to
    /// `post` table name lookup.
    pub(crate) fn glyph_id(&self, code: u8) -> Result<Option<GlyphId>, Error> {
        let name = match self.glyph_name(code) {
            Some(n) => n,
            None => return Ok(Some(GlyphId(0))), // undefined slot → .notdef
        };
        let face = self.face()?;
        let tables = face.tables();

        // Preferred path: name → Unicode → (3,1) cmap.
        let unicode = name_to_unicode(name);
        if let Some(u) = unicode {
            if let Some(sub) = find_cmap(tables, 3, 1) {
                if let Some(g) = sub.glyph_index(u) {
                    return Ok(Some(g));
                }
            }
        }
        // Fallback: name → Mac-Roman code → (1,0) cmap.
        if let Some(sub) = find_cmap(tables, 1, 0) {
            if let Some(mac_code) = mac_roman_code(name) {
                if let Some(g) = sub.glyph_index(mac_code as u32) {
                    return Ok(Some(g));
                }
            }
        }
        // Last resort: post table name lookup.
        if let Some(g) = face.glyph_index_by_name(name) {
            return Ok(Some(g));
        }
        // Nothing found — .notdef.
        Ok(Some(GlyphId(0)))
    }

    /// Extract the outline of `glyph` into `builder` in font units.
    /// Returns `false` for an empty outline (e.g. space) — the caller
    /// still advances the text position.
    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        let face = self.face()?;
        Ok(face.outline_glyph(glyph, builder).is_some())
    }

    /// Resolve `code` to a Unicode string, in the priority order of
    /// §9.10.2:
    /// 1. `/ToUnicode` CMap — if present and the code maps.
    /// 2. WinAnsi glyph name → AGL → Unicode — the encoding fallback
    ///    for our non-symbolic TrueType fonts.
    ///
    /// Returns `None` when *neither* path resolves — the caller
    /// distinguishes this from a mapped empty string or U+FFFD.
    pub(crate) fn unicode(&self, code: u8) -> Option<String> {
        if let Some(tu) = &self.tounicode {
            if let Some(s) = tu.decode(code as u32) {
                return Some(s);
            }
            // ToUnicode present but no entry → no fallback per spec
            // ("If these methods fail, there is no way to determine").
            return None;
        }
        // Fallback: encoding glyph name → AGL → Unicode.
        let name = self.glyph_name(code)?;
        let u = name_to_unicode(name)?;
        char::from_u32(u).map(|c| c.to_string())
    }

    /// `true` when the font carries a `/ToUnicode` CMap — used by
    /// the extractor to report how a code was resolved.
    pub(crate) fn has_tounicode(&self) -> bool {
        self.tounicode.is_some()
    }

    /// `true` when `code` is the font's word-space code for `Tw`
    /// purposes — the single-byte code 32 (§9.3.3). Composite fonts
    /// have no single-byte codes, so `Tw` never applies there.
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        code == 0x20
    }

    /// Units per em of the font program (normally 1000 or 2048).
    /// PDF widths are in 1/1000-em regardless; the glyph-space scale
    /// factor `1000 / units_per_em` converts font units to the PDF
    /// glyph coordinate system.
    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        Ok(self.face()?.units_per_em() as f64)
    }
}

// --------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------

/// Find a cmap subtable by (platform, encoding).
fn find_cmap<'a>(
    tables: &'a ttf_parser::FaceTables<'a>,
    platform: u16,
    encoding: u16,
) -> Option<ttf_parser::cmap::Subtable<'a>> {
    tables
        .cmap?
        .subtables
        .into_iter()
        .find(|s| s.platform_id as u16 == platform && s.encoding_id == encoding)
}

/// glyph name → Unicode scalar via the AGL-derived table.
fn name_to_unicode(name: &str) -> Option<u32> {
    GLYPH_NAME_TO_UNICODE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, u)| *u)
}

/// glyph name → Mac OS Roman code — the fallback for fonts without a
/// (3,1) cmap. Only implemented for the small set that overlaps
/// WinAnsi; `None` for anything else.
fn mac_roman_code(name: &str) -> Option<u8> {
    MAC_ROMAN.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

// --------------------------------------------------------------------
// Type 3 font (Subtype /Type3 — CharProcs content streams)
// --------------------------------------------------------------------

/// Load a `/Type3` font dictionary. A Type3 glyph is *not* an outline
/// program — it is a `/CharProcs` content stream the renderer runs
/// like a small Form XObject. We therefore only gather the dict data
/// the renderer needs; glyph *execution* lives in `render.rs`.
///
/// Required by spec: `/CharProcs`, `/Encoding` (or a usable built-in),
/// `/FirstChar`/`/LastChar`/`/Widths`. `/FontMatrix` defaults to
/// `[0.001 0 0 0.001 0 0]`.
fn load_type3_font(doc: &Document, dict: &Dict, limits: &Limits) -> Result<Rc<Type3Font>, Error> {
    // ---- CharProcs — the glyph program table --------------------
    let cp_obj = dict
        .get(b"CharProcs".as_slice())
        .ok_or_else(|| Error::Structure("Type3 font has no /CharProcs".into()))?;
    let cp = doc.deref(cp_obj)?;
    let charprocs = cp
        .as_dict()
        .ok_or_else(|| Error::Structure("/CharProcs is not a dictionary".into()))?
        .clone();

    // ---- Encoding — code → char name ----------------------------
    // Reuse the Type1 name-table builder: it already yields
    // code→name-bytes for WinAnsi / /Differences — exactly the Type3
    // code→char-name mapping. The builtin flag is CFF-specific and
    // unused for Type3.
    let (encoding, _builtin) = resolve_type1_encoding(doc, dict)?;

    // ---- FontMatrix (default 0.001 scale, §9.6.4) ---------------
    let font_matrix = match dict.get(b"FontMatrix".as_slice()) {
        None | Some(Object::Null) => crate::matrix::Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0),
        Some(o) => {
            let arr = o
                .as_array()
                .ok_or_else(|| Error::Structure("/FontMatrix is not an array".into()))?;
            if arr.len() != 6 {
                return Err(Error::Structure("/FontMatrix must have 6 numbers".into()));
            }
            let mut n = [0.0f64; 6];
            for (i, e) in arr.iter().enumerate() {
                n[i] = e.as_number().ok_or_else(|| {
                    Error::Structure("/FontMatrix entries must be numbers".into())
                })?;
            }
            crate::matrix::Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5])
        }
    };
    if !font_matrix.is_finite() {
        return Err(Error::Structure("/FontMatrix has non-finite values".into()));
    }

    // ---- FirstChar / LastChar / Widths --------------------------
    let first_char = dict
        .get(b"FirstChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let last_char = dict
        .get(b"LastChar".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(255);
    let mut widths = [0.0f64; 256];
    if let Some(w_obj) = dict.get(b"Widths".as_slice()) {
        let w = doc.deref(w_obj)?;
        let arr = w
            .as_array()
            .ok_or_else(|| Error::Structure("/Widths is not an array".into()))?;
        let expect = (last_char - first_char + 1).max(0) as usize;
        for (i, e) in arr.iter().enumerate().take(expect.min(arr.len())) {
            let e = doc.deref(e).unwrap_or_else(|_| e.clone());
            if let Some(v) = e.as_number() {
                if v.is_finite() && v >= 0.0 {
                    widths[first_char as usize + i] = v;
                }
            }
        }
    }

    // ---- /Resources the CharProcs resolve against ---------------
    let resources = dict.get(b"Resources".as_slice()).cloned();

    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;

    Ok(Rc::new(Type3Font {
        encoding,
        charprocs,
        font_matrix,
        widths,
        tounicode,
        resources,
    }))
}

impl Type3Font {
    /// `/FontMatrix` — glyph space → text space.
    pub(crate) fn font_matrix(&self) -> crate::matrix::Matrix {
        self.font_matrix
    }

    /// The font's `/Resources` for CharProc operand resolution —
    /// `None` means inherit the caller's.
    pub(crate) fn resources(&self) -> Option<&Object> {
        self.resources.as_ref()
    }

    /// Char code → the CharProc stream `(dict, raw)` for its glyph.
    /// `None` = code not encodable or no matching CharProc.
    pub(crate) fn charproc<'a>(
        &'a self,
        doc: &'a Document,
        code: u8,
    ) -> Result<Option<(Dict, Vec<u8>)>, Error> {
        let name = match &self.encoding[code as usize] {
            Some(n) => n.clone(),
            None => return Ok(None),
        };
        let entry = match self.charprocs.get(name.as_slice()) {
            Some(e) => e.clone(),
            None => return Ok(None),
        };
        let obj = doc.deref(&entry)?;
        let (d, raw) = obj
            .as_stream()
            .ok_or_else(|| Error::Structure("CharProc is not a stream".into()))?;
        Ok(Some((d.clone(), raw.to_vec())))
    }

    /// Char code → glyph advance in 1/1000-em units (PDF `/Widths`).
    /// `/Widths` is authoritative — the CharProc's `d0`/`d1` `wx`
    /// should agree but the font dict wins on conflict (§9.6.4.1).
    pub(crate) fn width(&self, code: u8) -> f64 {
        self.widths[code as usize]
    }

    /// Code → Unicode via `/ToUnicode` (else `.notdef` — no AGL for
    /// Type3 char names, which are arbitrary).
    pub(crate) fn unicode(&self, code: u8) -> Option<String> {
        self.tounicode.as_ref()?.decode(code as u32)
    }

    /// `true` when the font carries a `/ToUnicode` CMap.
    pub(crate) fn has_tounicode(&self) -> bool {
        self.tounicode.is_some()
    }

    /// `true` when `code` is the space char (§9.3.3, single-byte 32).
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        code == 0x20
    }
}

// --------------------------------------------------------------------
// Type 1 / CFF font (Subtype /Type1 or /MMType1 + /FontFile3)
// --------------------------------------------------------------------

impl Type1Font {
    /// The CFF program parsed as a `cff::Table`. Lazily built and
    /// cached — the table borrows `program`, so we transmute the
    /// lifetime to `'static` (safe: `self` owns `program`, never moved).
    /// `Err` when the font is non-embedded (`program: None`).
    fn cff(&self) -> Result<std::cell::Ref<'_, CffTable<'static>>, Error> {
        match &self.program {
            None => Err(Error::Unsupported {
                feature: "non-embedded Type1 font",
                detail: "no embedded font program — glyph outlines unavailable".into(),
            }),
            Some(program) => {
                {
                    let mut slot = self.cff.borrow_mut();
                    if slot.is_none() {
                        let t = CffTable::parse(program)
                            .ok_or_else(|| Error::Structure("invalid CFF program".into()))?;
                        // SAFETY: `t` borrows `self.program`; `self` owns
                        // both and `program` never moves or mutates.
                        let t: CffTable<'static> = unsafe { std::mem::transmute(t) };
                        *slot = Some(t);
                    }
                }
                Ok(std::cell::Ref::map(self.cff.borrow(), |o| {
                    o.as_ref().unwrap()
                }))
            }
        }
    }

    /// Char code (0..=255) → glyph-name bytes per the PDF encoding.
    /// `None` = rely on the CFF built-in encoding / .notdef.
    fn glyph_name_bytes(&self, code: u8) -> Option<&[u8]> {
        self.encoding[code as usize].as_deref()
    }

    /// Char code → glyph advance in 1/1000-em units (PDF `/Widths`).
    pub(crate) fn width(&self, code: u8) -> f64 {
        self.widths[code as usize]
    }

    /// Char code → glyph id. Order (§9.6.5.1):
    /// 1. `/Encoding` name → CFF `glyph_index_by_name` (charset lookup).
    /// 2. Fall back to the CFF program's own encoding via `glyph_index`.
    /// 3. `.notdef` (0). A non-embedded font has no program → `.notdef`;
    ///    `outline` is what actually refuses, keeping render honest.
    pub(crate) fn glyph_id(&self, code: u8) -> Result<Option<GlyphId>, Error> {
        let cff = match self.cff() {
            Ok(c) => c,
            Err(_) => return Ok(Some(GlyphId(0))), // no program → .notdef
        };
        if let Some(name) = self.glyph_name_bytes(code) {
            if let Ok(s) = std::str::from_utf8(name) {
                if let Some(g) = cff.glyph_index_by_name(s) {
                    return Ok(Some(g));
                }
            }
        }
        // Built-in CFF encoding (StandardEncoding/custom charset).
        if self.builtin_cff_encoding {
            if let Some(g) = cff.glyph_index(code) {
                return Ok(Some(g));
            }
        }
        Ok(Some(GlyphId(0)))
    }

    /// Extract `glyph`'s outline into `builder` in font units.
    /// `false` = empty. Non-embedded fonts have no program → propagate
    /// the `cff()` Unsupported error so rendering refuses honestly.
    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        let cff = self.cff()?; // Unsupported for non-embedded fonts
        Ok(cff.outline(glyph, builder).is_ok())
    }

    /// Code → Unicode via `/ToUnicode`, else glyph name → AGL →
    /// Unicode (only meaningful for named glyphs, not subset `/Gxx`).
    pub(crate) fn unicode(&self, code: u8) -> Option<String> {
        if let Some(tu) = &self.tounicode {
            return tu.decode(code as u32);
        }
        let name = self.glyph_name_bytes(code)?;
        let s = std::str::from_utf8(name).ok()?;
        let u = name_to_unicode(s)?;
        char::from_u32(u).map(|c| c.to_string())
    }

    /// `true` when the font carries a `/ToUnicode` CMap.
    pub(crate) fn has_tounicode(&self) -> bool {
        self.tounicode.is_some()
    }

    /// `true` when `code` is the space char (§9.3.3, single-byte 32).
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        code == 0x20
    }

    /// CFF fonts are nominally 1000 units/em (FontMatrix 0.001). The
    /// `/FontMatrix` scale is already baked into the charstring
    /// coordinates for CFF, so we report 1000 — matching the PDF glyph
    /// space that `Tm`'s `Tfs` then scales.
    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        Ok(1000.0)
    }
}

// --------------------------------------------------------------------
// Composite font (Type 0 / CIDFontType2 / Identity-H) — M7
// --------------------------------------------------------------------

/// A Type 0 composite font: 2-byte big-endian codes → CID →
/// CIDToGIDMap → GID. Only `Identity-H` encoding, `CIDFontType2`
/// descendants, and `/FontFile2` TrueType programs are supported.
/// Vertical writing (`Identity-V`, `WMode`, `DW2`/`W2`) is refused.
pub(crate) struct CompositeFont {
    /// Decoded `/FontFile2` of the descendant CIDFont.
    program: Vec<u8>,
    /// CID → GID map: `None` = Identity, `Some(v)` = big-endian
    /// 2-byte-per-CID lookup (`v[2c]<<8|v[2c+1]`).
    cid_to_gid: Option<Vec<u8>>,
    /// Default glyph width (1/1000 em), from `/DW`. Default 1000.
    dw: f64,
    /// Width overrides from `/W` — sorted, non-expanded ranges.
    /// Each entry is `(first_cid, last_cid, width)`.
    w: Vec<(u32, u32, f64)>,
    /// `/ToUnicode` CMap — `None` if absent.
    tounicode: Option<crate::tounicode::ToUnicode>,
    /// `Face` over `program` — same lazy-borrow pattern as
    /// `SimpleFont`.
    face: RefCell<Option<Face<'static>>>,
}

/// Load a Type 0 font dictionary: `/Encoding` must be `/Identity-H`,
/// `/DescendantFonts` must hold exactly one CIDFontType2 with a
/// `/FontFile2` TrueType program. `Identity-V`, CIDFontType0 (CFF),
/// non-embedded programs and arbitrary CMap names are refused.
fn load_composite_font(
    doc: &Document,
    dict: &Dict,
    limits: &Limits,
) -> Result<Rc<CompositeFont>, Error> {
    // ---- /Encoding /Identity-H -----------------------------------
    match dict.get(b"Encoding".as_slice()) {
        Some(Object::Name(n)) if n.as_slice() == b"Identity-H" => {}
        Some(Object::Name(n)) if n.as_slice() == b"Identity-V" => {
            return Err(Error::Unsupported {
                feature: "vertical writing (/Encoding /Identity-V)",
                detail: "only horizontal Identity-H is implemented".into(),
            })
        }
        Some(Object::Name(other)) => {
            return Err(Error::Unsupported {
                feature: "Type 0 font encoding",
                detail: format!(
                    "/{} — only Identity-H is supported",
                    String::from_utf8_lossy(other)
                ),
            })
        }
        Some(Object::Dict(_)) | Some(Object::Stream { .. }) => {
            // A CMap stream/name — arbitrary encodings are out of
            // scope.
            return Err(Error::Unsupported {
                feature: "Type 0 font /Encoding CMap",
                detail: "only the predefined Identity-H CMap is supported".into(),
            });
        }
        Some(_) => {
            return Err(Error::Structure(
                "/Encoding must be the name Identity-H (or a CMap — unsupported)".into(),
            ))
        }
        None => return Err(Error::Structure("/Type0 font has no /Encoding".into())),
    }

    // ---- /DescendantFonts — exactly one CIDFont -------------------
    let df_obj = dict
        .get(b"DescendantFonts".as_slice())
        .ok_or_else(|| Error::Structure("/Type0 font has no /DescendantFonts".into()))?;
    let df = doc.deref(df_obj)?;
    let df_arr = df
        .as_array()
        .ok_or_else(|| Error::Structure("/DescendantFonts is not an array".into()))?;
    if df_arr.len() != 1 {
        return Err(Error::Unsupported {
            feature: "Type 0 font with multiple descendant fonts",
            detail: format!("/DescendantFonts has {} entries", df_arr.len()),
        });
    }
    let cid_obj = doc.deref(&df_arr[0])?;
    let cid_dict = cid_obj
        .as_dict()
        .ok_or_else(|| Error::Structure("/DescendantFonts[0] is not a dictionary".into()))?;
    match cid_dict
        .get(b"Subtype".as_slice())
        .and_then(|o| o.as_name())
    {
        Some(b"CIDFontType2") => {}
        Some(b"CIDFontType0") => {
            return Err(Error::Unsupported {
                feature: "CIDFontType0 (CFF/Type 1) descendant",
                detail: "only CIDFontType2 (TrueType) is implemented".into(),
            })
        }
        Some(other) => {
            return Err(Error::Unsupported {
                feature: "CIDFont subtype",
                detail: format!("/{other:?} — only /CIDFontType2 is supported"),
            })
        }
        None => {
            return Err(Error::Structure(
                "/DescendantFonts[0] has no /Subtype".into(),
            ))
        }
    }
    // CIDSystemInfo is required but its values are free-form for
    // Identity-H; validate presence and shape only.
    match cid_dict.get(b"CIDSystemInfo".as_slice()) {
        Some(o) => {
            let info = doc.deref(o)?;
            let d = info
                .as_dict()
                .ok_or_else(|| Error::Structure("/CIDSystemInfo is not a dictionary".into()))?;
            for key in [&b"Registry"[..], &b"Ordering"[..]] {
                match d.get(key) {
                    Some(Object::String(_)) | None => {}
                    _ => {
                        return Err(Error::Structure(format!(
                            "/CIDSystemInfo /{} must be a string",
                            String::from_utf8_lossy(key)
                        )))
                    }
                }
            }
        }
        None => return Err(Error::Structure("CIDFont has no /CIDSystemInfo".into())),
    }

    // ---- /FontDescriptor → /FontFile2 -----------------------------
    let fd_obj = cid_dict
        .get(b"FontDescriptor".as_slice())
        .ok_or_else(|| Error::Structure("CIDFont has no /FontDescriptor".into()))?;
    let fd = doc.deref(fd_obj)?;
    let fd_dict = fd
        .as_dict()
        .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;
    // CFF/Type1 programs are refused explicitly.
    if fd_dict.get(b"FontFile3".as_slice()).is_some() {
        return Err(Error::Unsupported {
            feature: "CIDFontType0 /FontFile3 (CFF)",
            detail: "only /FontFile2 TrueType programs are supported".into(),
        });
    }
    let ff2_obj = fd_dict
        .get(b"FontFile2".as_slice())
        .ok_or_else(|| Error::Unsupported {
            feature: "non-embedded CIDFont",
            detail: "/FontFile2 is required; OS substitution is not performed".into(),
        })?;
    let ff2 = doc.deref(ff2_obj)?;
    let (ff2_dict, ff2_raw) = ff2
        .as_stream()
        .ok_or_else(|| Error::Structure("/FontFile2 is not a stream".into()))?;
    let program = doc.decode_stream(ff2_dict, ff2_raw)?;
    if program.len() as u64 > limits.max_stream_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_stream_bytes (embedded CID font)",
            max: limits.max_stream_bytes,
        });
    }

    // ---- /CIDToGIDMap ---------------------------------------------
    let cid_to_gid = match cid_dict.get(b"CIDToGIDMap".as_slice()) {
        None | Some(Object::Null) => None, // default: Identity
        Some(Object::Name(n)) if n.as_slice() == b"Identity" => None,
        Some(o) => {
            let s = doc.deref(o)?;
            let (sd, raw) = s
                .as_stream()
                .ok_or_else(|| Error::Structure("/CIDToGIDMap is not a stream".into()))?;
            let bytes = doc.decode_stream(sd, raw)?;
            if bytes.len() as u64 > limits.max_stream_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_stream_bytes (CIDToGIDMap)",
                    max: limits.max_stream_bytes,
                });
            }
            if bytes.len() % 2 != 0 {
                return Err(Error::Structure(
                    "/CIDToGIDMap stream has odd length — GIDs are 2-byte".into(),
                ));
            }
            Some(bytes)
        }
    };

    // ---- /DW ------------------------------------------------------
    let dw = match cid_dict.get(b"DW".as_slice()) {
        None | Some(Object::Null) => 1000.0,
        Some(o) => {
            let w = o
                .as_number()
                .ok_or_else(|| Error::Structure("/DW is not a number".into()))?;
            if !(w.is_finite() && w >= 0.0) {
                return Err(Error::Structure("/DW is not a non-negative number".into()));
            }
            w
        }
    };

    // ---- /W — two group forms; stored unexpanded ------------------
    let mut w: Vec<(u32, u32, f64)> = Vec::new();
    if let Some(w_obj) = cid_dict.get(b"W".as_slice()) {
        let arr = doc.deref(w_obj)?;
        let arr = arr
            .as_array()
            .ok_or_else(|| Error::Structure("/W is not an array".into()))?;
        let mut i = 0usize;
        while i < arr.len() {
            let c0 = arr[i]
                .as_i64()
                .ok_or_else(|| Error::Structure(format!("/W[{i}] must be an integer CID")))?;
            if !(0..=0xFFFF).contains(&c0) {
                return Err(Error::Structure(format!("/W[{i}] CID {c0} out of range")));
            }
            i += 1;
            if i >= arr.len() {
                return Err(Error::Structure("/W array ends mid-group".into()));
            }
            match &arr[i] {
                Object::Array(ws) => {
                    // c [w1 w2 … wn] — consecutive CIDs each with own width.
                    let base = c0 as u32;
                    for (k, e) in ws.iter().enumerate() {
                        let wv = e.as_number().ok_or_else(|| {
                            Error::Structure("/W sub-array entries must be numbers".into())
                        })?;
                        if !(wv.is_finite() && wv >= 0.0) {
                            return Err(Error::Structure(
                                "/W width is not a non-negative number".into(),
                            ));
                        }
                        let cid = base
                            .checked_add(k as u32)
                            .ok_or_else(|| Error::Structure("/W CID overflow".into()))?;
                        if cid > 0xFFFF {
                            return Err(Error::Structure("/W CID exceeds 0xFFFF".into()));
                        }
                        w.push((cid, cid, wv));
                    }
                    i += 1;
                }
                _ => {
                    // cfirst clast w — one width for a range.
                    let clast = arr[i].as_i64().ok_or_else(|| {
                        Error::Structure("/W range bound must be an integer".into())
                    })?;
                    let wv = arr
                        .get(i + 1)
                        .and_then(|o| o.as_number())
                        .ok_or_else(|| Error::Structure("/W range needs a width".into()))?;
                    if !(wv.is_finite() && wv >= 0.0) {
                        return Err(Error::Structure(
                            "/W width is not a non-negative number".into(),
                        ));
                    }
                    if clast < c0 || clast > 0xFFFF {
                        return Err(Error::Structure(format!(
                            "/W range {c0}..{clast} is invalid"
                        )));
                    }
                    w.push((c0 as u32, clast as u32, wv));
                    i += 2;
                }
            }
        }
        w.sort_unstable_by_key(|(a, _, _)| *a);
    }

    // ---- /ToUnicode -----------------------------------------------
    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;
    if let Some(t) = &tounicode {
        if t.code_len() != 2 {
            return Err(Error::Unsupported {
                feature: "ToUnicode codespace",
                detail: format!(
                    "Type 0 ToUnicode must use 2-byte codes; got {}-byte",
                    t.code_len()
                ),
            });
        }
    }

    Ok(Rc::new(CompositeFont {
        program,
        cid_to_gid,
        dw,
        w,
        tounicode,
        face: RefCell::new(None),
    }))
}

impl CompositeFont {
    /// Lazily parsed `Face` over `program` — same pattern as
    /// `SimpleFont::face`.
    fn face(&self) -> Result<std::cell::Ref<'_, Face<'static>>, Error> {
        {
            let mut slot = self.face.borrow_mut();
            if slot.is_none() {
                let face = Face::parse(&self.program, 0)
                    .map_err(|e| Error::Structure(format!("TrueType font program: {e:?}")))?;
                let face: Face<'static> = unsafe { std::mem::transmute(face) };
                *slot = Some(face);
            }
        }
        Ok(std::cell::Ref::map(self.face.borrow(), |o| {
            o.as_ref().unwrap()
        }))
    }

    /// CID for `code` — Identity-H maps code→CID identically.
    fn cid(&self, code: u32) -> u32 {
        code
    }

    /// GID for `code`: code → CID → CIDToGIDMap. Identity is the
    /// default and also explicit `/Identity`. A missing stream entry
    /// (or a GID past the font's glyph count) yields `.notdef` (gid 0)
    /// per the .notdef convention, not an error.
    pub(crate) fn glyph_id(&self, code: u32) -> Result<GlyphId, Error> {
        let cid = self.cid(code);
        let gid = match &self.cid_to_gid {
            None => cid,
            Some(map) => {
                let off = (cid as usize) * 2;
                if off + 2 > map.len() {
                    0 // CID beyond map → .notdef
                } else {
                    ((map[off] as u32) << 8) | map[off + 1] as u32
                }
            }
        };
        // Validate against the font's glyph count — a map entry naming
        // a nonexistent glyph is .notdef, not an out-of-bounds read.
        let face = self.face()?;
        let n = face.number_of_glyphs();
        Ok(if gid < n as u32 {
            GlyphId(gid as u16)
        } else {
            GlyphId(0)
        })
    }

    /// Width of `code` in 1/1000-em units, per `/W` (first match) or
    /// `/DW` (default 1000).
    pub(crate) fn width(&self, code: u32) -> f64 {
        let cid = self.cid(code);
        // Linear scan — W groups are sorted; binary search by first.
        let mut lo = 0usize;
        let mut hi = self.w.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let (a, b, _) = self.w[mid];
            if cid < a {
                hi = mid;
            } else if cid > b {
                lo = mid + 1;
            } else {
                return self.w[mid].2;
            }
        }
        self.dw
    }

    /// Extract the glyph outline into `builder` (font units).
    /// `false` = empty outline (space) — caller still advances.
    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        let face = self.face()?;
        Ok(face.outline_glyph(glyph, builder).is_some())
    }

    /// Resolve `code` via `/ToUnicode` only — composite fonts have no
    /// AGL fallback (their codes are CIDs, not character names).
    pub(crate) fn unicode(&self, code: u32) -> Option<String> {
        self.tounicode.as_ref()?.decode(code)
    }

    /// `true` when the font carries a `/ToUnicode` CMap.
    pub(crate) fn has_tounicode(&self) -> bool {
        self.tounicode.is_some()
    }

    /// Identity-H has no single-byte codes, so `Tw` never applies —
    /// §9.3.3 limits `Tw` to "occurrences of the single-byte
    /// character code 32", and a composite font's codes are all
    /// 2-byte.
    pub(crate) fn is_space_code(&self, _code: u32) -> bool {
        false
    }

    /// Units per em of the font program.
    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        Ok(self.face()?.units_per_em() as f64)
    }
}

impl Font {
    /// Decode one code at `bytes` big-endian. Returns `(code,
    /// consumed_bytes)` or `Err` for a truncated tail (odd trailing
    /// byte on a 2-byte font). Codes never span string boundaries.
    pub(crate) fn read_code(&self, bytes: &[u8], pos: usize) -> Result<(u32, usize), Error> {
        match self {
            Font::Simple(_) | Font::Type1(_) | Font::Type3(_) => Ok((bytes[pos] as u32, 1)),
            Font::Composite(_) => {
                if pos + 1 >= bytes.len() {
                    return Err(Error::Structure(
                        "string ends mid-code — composite fonts require 2-byte codes".into(),
                    ));
                }
                Ok((((bytes[pos] as u32) << 8) | bytes[pos + 1] as u32, 2))
            }
        }
    }

    /// `true` when `Tw` applies to this code — only single-byte
    /// code 32 on a simple font (§9.3.3).
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        match self {
            Font::Simple(f) => f.is_space_code(code),
            Font::Composite(f) => f.is_space_code(code),
            Font::Type1(f) => f.is_space_code(code),
            Font::Type3(f) => f.is_space_code(code),
        }
    }

    /// Code → glyph advance (1/1000 em) from the font's PDF width
    /// table — `/Widths` for simple, `/DW`/`/W` for composite.
    pub(crate) fn width(&self, code: u32) -> f64 {
        match self {
            Font::Simple(f) => f.width(code as u8),
            Font::Composite(f) => f.width(code),
            Font::Type1(f) => f.width(code as u8),
            Font::Type3(f) => f.width(code as u8),
        }
    }

    /// Code → glyph id — `.notdef` (0) when unresolvable. Never
    /// `None`: a code always produces a glyph slot.
    pub(crate) fn glyph_id(&self, code: u32) -> Result<GlyphId, Error> {
        match self {
            Font::Simple(f) => Ok(f.glyph_id(code as u8)?.unwrap_or(GlyphId(0))),
            Font::Composite(f) => f.glyph_id(code),
            Font::Type1(f) => Ok(f.glyph_id(code as u8)?.unwrap_or(GlyphId(0))),
            // Type3 glyphs are content streams, not outline glyphs —
            // the char code itself is the identity for bookkeeping.
            Font::Type3(_) => Ok(GlyphId(code as u16)),
        }
    }

    /// Extract `glyph`'s outline (font units). `false` = empty.
    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        match self {
            Font::Simple(f) => f.outline(glyph, builder),
            Font::Composite(f) => f.outline(glyph, builder),
            Font::Type1(f) => f.outline(glyph, builder),
            // Type3 has no outline program — its glyph is a CharProc
            // stream executed by the renderer. `show_glyph` handles it
            // before ever calling outline; this is a defensive default.
            Font::Type3(_) => Ok(false),
        }
    }

    /// Code → Unicode string, or `None` when unresolvable.
    /// ToUnicode first for both kinds; simple fonts fall back to
    /// the encoding-name→AGL path, composites do not.
    pub(crate) fn unicode(&self, code: u32) -> Option<String> {
        match self {
            Font::Simple(f) => f.unicode(code as u8),
            Font::Composite(f) => f.unicode(code),
            Font::Type1(f) => f.unicode(code as u8),
            Font::Type3(f) => f.unicode(code as u8),
        }
    }

    /// `true` when the font has a `/ToUnicode` CMap.
    pub(crate) fn has_tounicode(&self) -> bool {
        match self {
            Font::Simple(f) => f.has_tounicode(),
            Font::Composite(f) => f.has_tounicode(),
            Font::Type1(f) => f.has_tounicode(),
            Font::Type3(f) => f.has_tounicode(),
        }
    }

    /// Units per em.
    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        match self {
            Font::Simple(f) => f.units_per_em(),
            Font::Composite(f) => f.units_per_em(),
            Font::Type1(f) => f.units_per_em(),
            // Type3 glyph space is scaled by /FontMatrix (default
            // 0.001) into text space; the nominal em is 1000 units,
            // matching the 1/1000-em /Widths convention.
            Font::Type3(_) => Ok(1000.0),
        }
    }

    /// The inner [`Type3Font`] when this is a `/Type3` font — used by
    /// the renderer's CharProc executor, which is the only consumer
    /// that needs the raw struct rather than the uniform interface.
    pub(crate) fn as_type3(&self) -> Option<&Rc<Type3Font>> {
        match self {
            Font::Type3(f) => Some(f),
            _ => None,
        }
    }
}

/// Builder that converts ttf-parser outline segments into a
/// `tiny_skia::PathBuilder`. Coordinates arrive in *font units* and
/// are scaled by `1 / units_per_em` so the path ends up in the
/// PDF glyph coordinate system (em square), where `Tm`'s `Tfs`
/// scaling then applies the font size.
pub(crate) struct GlyphPath {
    pub(crate) builder: tiny_skia::PathBuilder,
    scale: f32,
}

impl GlyphPath {
    pub(crate) fn new(units_per_em: f64) -> Self {
        GlyphPath {
            builder: tiny_skia::PathBuilder::new(),
            scale: (1.0 / units_per_em) as f32,
        }
    }
    pub(crate) fn finish(self) -> Option<tiny_skia::Path> {
        self.builder.finish()
    }
}

impl OutlineBuilder for GlyphPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x * self.scale, y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x * self.scale, y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.builder.quad_to(
            x1 * self.scale,
            y1 * self.scale,
            x * self.scale,
            y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            x1 * self.scale,
            y1 * self.scale,
            x2 * self.scale,
            y2 * self.scale,
            x * self.scale,
            y * self.scale,
        );
    }
    fn close(&mut self) {
        self.builder.close();
    }
}

// --------------------------------------------------------------------
// tables — generated, see tests/fixtures/README.md for provenance
// --------------------------------------------------------------------

/// WinAnsiEncoding (Annex D) — char code → glyph name.
/// `None` = undefined slot (rendered as `.notdef`).
const WINANSI: [Option<&'static str>; 256] = [
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    Some("space"),
    Some("exclam"),
    Some("quotedbl"),
    Some("numbersign"),
    Some("dollar"),
    Some("percent"),
    Some("ampersand"),
    Some("quotesingle"),
    Some("parenleft"),
    Some("parenright"),
    Some("asterisk"),
    Some("plus"),
    Some("comma"),
    Some("hyphen"),
    Some("period"),
    Some("slash"),
    Some("zero"),
    Some("one"),
    Some("two"),
    Some("three"),
    Some("four"),
    Some("five"),
    Some("six"),
    Some("seven"),
    Some("eight"),
    Some("nine"),
    Some("colon"),
    Some("semicolon"),
    Some("less"),
    Some("equal"),
    Some("greater"),
    Some("question"),
    Some("at"),
    Some("A"),
    Some("B"),
    Some("C"),
    Some("D"),
    Some("E"),
    Some("F"),
    Some("G"),
    Some("H"),
    Some("I"),
    Some("J"),
    Some("K"),
    Some("L"),
    Some("M"),
    Some("N"),
    Some("O"),
    Some("P"),
    Some("Q"),
    Some("R"),
    Some("S"),
    Some("T"),
    Some("U"),
    Some("V"),
    Some("W"),
    Some("X"),
    Some("Y"),
    Some("Z"),
    Some("bracketleft"),
    Some("backslash"),
    Some("bracketright"),
    Some("asciicircum"),
    Some("underscore"),
    Some("grave"),
    Some("a"),
    Some("b"),
    Some("c"),
    Some("d"),
    Some("e"),
    Some("f"),
    Some("g"),
    Some("h"),
    Some("i"),
    Some("j"),
    Some("k"),
    Some("l"),
    Some("m"),
    Some("n"),
    Some("o"),
    Some("p"),
    Some("q"),
    Some("r"),
    Some("s"),
    Some("t"),
    Some("u"),
    Some("v"),
    Some("w"),
    Some("x"),
    Some("y"),
    Some("z"),
    Some("braceleft"),
    Some("bar"),
    Some("braceright"),
    Some("asciitilde"),
    None, // 0x7F
    Some("Euro"),
    None,
    Some("quotesinglbase"),
    Some("florin"),
    Some("quotedblbase"),
    Some("ellipsis"),
    Some("dagger"),
    Some("daggerdbl"),
    Some("circumflex"),
    Some("perthousand"),
    Some("Scaron"),
    Some("guilsinglleft"),
    Some("OE"),
    None,
    Some("Zcaron"),
    None,
    None,
    Some("quoteleft"),
    Some("quoteright"),
    Some("quotedblleft"),
    Some("quotedblright"),
    Some("bullet"),
    Some("endash"),
    Some("emdash"),
    Some("tilde"),
    Some("trademark"),
    Some("scaron"),
    Some("guilsinglright"),
    Some("oe"),
    None,
    Some("zcaron"),
    Some("Ydieresis"),
    Some("space"), // NBSP
    Some("exclamdown"),
    Some("cent"),
    Some("sterling"),
    Some("currency"),
    Some("yen"),
    Some("brokenbar"),
    Some("section"),
    Some("dieresis"),
    Some("copyright"),
    Some("ordfeminine"),
    Some("guillemotleft"),
    Some("logicalnot"),
    Some("hyphen"), // soft hyphen → hyphen
    Some("registered"),
    Some("macron"),
    Some("degree"),
    Some("plusminus"),
    Some("twosuperior"),
    Some("threesuperior"),
    Some("acute"),
    Some("mu"),
    Some("paragraph"),
    Some("periodcentered"),
    Some("cedilla"),
    Some("onesuperior"),
    Some("ordmasculine"),
    Some("guillemotright"),
    Some("onequarter"),
    Some("onehalf"),
    Some("threequarters"),
    Some("questiondown"),
    Some("Agrave"),
    Some("Aacute"),
    Some("Acircumflex"),
    Some("Atilde"),
    Some("Adieresis"),
    Some("Aring"),
    Some("AE"),
    Some("Ccedilla"),
    Some("Egrave"),
    Some("Eacute"),
    Some("Ecircumflex"),
    Some("Edieresis"),
    Some("Igrave"),
    Some("Iacute"),
    Some("Icircumflex"),
    Some("Idieresis"),
    Some("Eth"),
    Some("Ntilde"),
    Some("Ograve"),
    Some("Oacute"),
    Some("Ocircumflex"),
    Some("Otilde"),
    Some("Odieresis"),
    Some("multiply"),
    Some("Oslash"),
    Some("Ugrave"),
    Some("Uacute"),
    Some("Ucircumflex"),
    Some("Udieresis"),
    Some("Yacute"),
    Some("Thorn"),
    Some("germandbls"),
    Some("agrave"),
    Some("aacute"),
    Some("acircumflex"),
    Some("atilde"),
    Some("adieresis"),
    Some("aring"),
    Some("ae"),
    Some("ccedilla"),
    Some("egrave"),
    Some("eacute"),
    Some("ecircumflex"),
    Some("edieresis"),
    Some("igrave"),
    Some("iacute"),
    Some("icircumflex"),
    Some("idieresis"),
    Some("eth"),
    Some("ntilde"),
    Some("ograve"),
    Some("oacute"),
    Some("ocircumflex"),
    Some("otilde"),
    Some("odieresis"),
    Some("divide"),
    Some("oslash"),
    Some("ugrave"),
    Some("uacute"),
    Some("ucircumflex"),
    Some("udieresis"),
    Some("yacute"),
    Some("thorn"),
    Some("ydieresis"),
];

/// Subset of Mac OS Roman used as a fallback when no (3,1) cmap is
/// present — only the codes we can safely map. `name` is the AGL
/// name, `code` the Mac-Roman byte value.
static MAC_ROMAN: &[(&str, u8)] = &[
    ("A", 65),
    ("B", 66),
    ("C", 67),
    ("D", 68),
    ("E", 69),
    ("F", 70),
    ("G", 71),
    ("H", 72),
    ("I", 73),
    ("J", 74),
    ("K", 75),
    ("L", 76),
    ("M", 77),
    ("N", 78),
    ("O", 79),
    ("P", 80),
    ("Q", 81),
    ("R", 82),
    ("S", 83),
    ("T", 84),
    ("U", 85),
    ("V", 86),
    ("W", 87),
    ("X", 88),
    ("Y", 89),
    ("Z", 90),
    ("a", 97),
    ("b", 98),
    ("c", 99),
    ("d", 100),
    ("e", 101),
    ("f", 102),
    ("g", 103),
    ("h", 104),
    ("i", 105),
    ("j", 106),
    ("k", 107),
    ("l", 108),
    ("m", 109),
    ("n", 110),
    ("o", 111),
    ("p", 112),
    ("q", 113),
    ("r", 114),
    ("s", 115),
    ("t", 116),
    ("u", 117),
    ("v", 118),
    ("w", 119),
    ("x", 120),
    ("y", 121),
    ("z", 122),
    ("space", 32),
    ("zero", 48),
    ("one", 49),
    ("two", 50),
    ("three", 51),
    ("four", 52),
    ("five", 53),
    ("six", 54),
    ("seven", 55),
    ("eight", 56),
    ("nine", 57),
    ("hyphen", 45),
    ("period", 46),
    ("comma", 44),
    ("colon", 58),
    ("semicolon", 59),
    ("exclam", 33),
    ("question", 63),
    ("parenleft", 40),
    ("parenright", 41),
    ("bracketleft", 91),
    ("bracketright", 93),
    ("slash", 47),
    ("backslash", 92),
    ("quotesingle", 39),
    ("quotedbl", 34),
    ("grave", 96),
    ("underscore", 95),
    ("asciicircum", 94),
    ("asciitilde", 126),
    ("at", 64),
    ("numbersign", 35),
    ("dollar", 36),
    ("percent", 37),
    ("ampersand", 38),
    ("asterisk", 42),
    ("plus", 43),
    ("equal", 61),
    ("less", 60),
    ("greater", 62),
    ("bar", 124),
    ("braceleft", 123),
    ("braceright", 125),
    // Accented chars (Mac Roman ≈ different codes than WinAnsi)
    ("Adieresis", 128),
    ("Aring", 129),
    ("Ccedilla", 130),
    ("Eacute", 131),
    ("Ntilde", 132),
    ("Odieresis", 133),
    ("Udieresis", 134),
    ("aacute", 135),
    ("agrave", 136),
    ("acircumflex", 137),
    ("adieresis", 138),
    ("atilde", 139),
    ("aring", 140),
    ("ccedilla", 141),
    ("eacute", 142),
    ("egrave", 143),
    ("ecircumflex", 144),
    ("edieresis", 145),
    ("iacute", 146),
    ("igrave", 147),
    ("icircumflex", 148),
    ("idieresis", 149),
    ("ntilde", 150),
    ("oacute", 151),
    ("ograve", 152),
    ("ocircumflex", 153),
    ("odieresis", 154),
    ("otilde", 155),
    ("uacute", 156),
    ("ugrave", 157),
    ("ucircumflex", 158),
    ("udieresis", 159),
    ("dagger", 160),
    ("degree", 161),
    ("cent", 162),
    ("sterling", 163),
    ("section", 164),
    ("bullet", 165),
    ("paragraph", 166),
    ("germandbls", 167),
    ("registered", 168),
    ("copyright", 169),
    ("trademark", 170),
    ("acute", 171),
    ("dieresis", 172),
    ("notequal", 173),
    ("AE", 174),
    ("Oslash", 175),
    ("infinity", 176),
    ("plusminus", 177),
    ("lessequal", 178),
    ("greaterequal", 179),
    ("yen", 180),
    ("mu", 181),
    ("partialdiff", 182),
    ("summation", 183),
    ("product", 184),
    ("pi", 185),
    ("integral", 186),
    ("ordfeminine", 187),
    ("ordmasculine", 188),
    ("Omega", 189),
    ("ae", 190),
    ("oslash", 191),
    ("questiondown", 192),
    ("exclamdown", 193),
    ("logicalnot", 194),
    ("radical", 195),
    ("florin", 196),
    ("approxequal", 197),
    ("Delta", 198),
    ("guillemotleft", 199),
    ("guillemotright", 200),
    ("ellipsis", 201),
    ("space", 202),
    ("Agrave", 203),
    ("Atilde", 204),
    ("Otilde", 205),
    ("OE", 206),
    ("oe", 207),
    ("endash", 208),
    ("emdash", 209),
    ("quotedblleft", 210),
    ("quotedblright", 211),
    ("quoteleft", 212),
    ("quoteright", 213),
    ("divide", 214),
    ("lozenge", 215),
    ("ydieresis", 216),
    ("Ydieresis", 217),
    ("fraction", 218),
    ("currency", 219),
    ("guilsinglleft", 220),
    ("guilsinglright", 221),
    ("fi", 222),
    ("fl", 223),
    ("daggerdbl", 224),
    ("periodcentered", 225),
    ("quotesinglbase", 226),
    ("quotedblbase", 227),
    ("perthousand", 228),
    ("Acircumflex", 229),
    ("Ecircumflex", 230),
    ("Aacute", 231),
    ("Edieresis", 232),
    ("Egrave", 233),
    ("Iacute", 234),
    ("Icircumflex", 235),
    ("Idieresis", 236),
    ("Igrave", 237),
    ("Oacute", 238),
    ("Ocircumflex", 239),
    ("apple", 240),
    ("Ograve", 241),
    ("Uacute", 242),
    ("Ucircumflex", 243),
    ("Ugrave", 244),
    ("dotlessi", 245),
    ("circumflex", 246),
    ("tilde", 247),
    ("macron", 248),
    ("breve", 249),
    ("dotaccent", 250),
    ("ring", 251),
    ("cedilla", 252),
    ("hungarumlaut", 253),
    ("ogonek", 254),
    ("caron", 255),
];

/// Glyph name → Unicode scalar, subset of the Adobe Glyph List that
/// WinAnsiEncoding can produce. Generated from fontTools' `agl`
/// module (Adobe Glyph List For New Fonts + legacy names).
static GLYPH_NAME_TO_UNICODE: &[(&str, u32)] = &[
    ("A", 65),
    ("AE", 198),
    ("Aacute", 193),
    ("Acircumflex", 194),
    ("Adieresis", 196),
    ("Agrave", 192),
    ("Aring", 197),
    ("Atilde", 195),
    ("B", 66),
    ("C", 67),
    ("Ccedilla", 199),
    ("D", 68),
    ("E", 69),
    ("Eacute", 201),
    ("Ecircumflex", 202),
    ("Edieresis", 203),
    ("Egrave", 200),
    ("Eth", 208),
    ("Euro", 8364),
    ("F", 70),
    ("G", 71),
    ("H", 72),
    ("I", 73),
    ("Iacute", 205),
    ("Icircumflex", 206),
    ("Idieresis", 207),
    ("Igrave", 204),
    ("J", 74),
    ("K", 75),
    ("L", 76),
    ("M", 77),
    ("N", 78),
    ("Ntilde", 209),
    ("O", 79),
    ("OE", 338),
    ("Oacute", 211),
    ("Ocircumflex", 212),
    ("Odieresis", 214),
    ("Ograve", 210),
    ("Oslash", 216),
    ("Otilde", 213),
    ("P", 80),
    ("Q", 81),
    ("R", 82),
    ("S", 83),
    ("Scaron", 352),
    ("T", 84),
    ("Thorn", 222),
    ("U", 85),
    ("Uacute", 218),
    ("Ucircumflex", 219),
    ("Udieresis", 220),
    ("Ugrave", 217),
    ("V", 86),
    ("W", 87),
    ("X", 88),
    ("Y", 89),
    ("Yacute", 221),
    ("Ydieresis", 376),
    ("Z", 90),
    ("Zcaron", 381),
    ("a", 97),
    ("aacute", 225),
    ("acircumflex", 226),
    ("acute", 180),
    ("adieresis", 228),
    ("ae", 230),
    ("agrave", 224),
    ("ampersand", 38),
    ("aring", 229),
    ("asciicircum", 94),
    ("asciitilde", 126),
    ("asterisk", 42),
    ("at", 64),
    ("atilde", 227),
    ("b", 98),
    ("backslash", 92),
    ("bar", 124),
    ("braceleft", 123),
    ("braceright", 125),
    ("bracketleft", 91),
    ("bracketright", 93),
    ("brokenbar", 166),
    ("bullet", 8226),
    ("c", 99),
    ("ccedilla", 231),
    ("cedilla", 184),
    ("cent", 162),
    ("circumflex", 710),
    ("colon", 58),
    ("comma", 44),
    ("copyright", 169),
    ("currency", 164),
    ("d", 100),
    ("dagger", 8224),
    ("daggerdbl", 8225),
    ("degree", 176),
    ("dieresis", 168),
    ("divide", 247),
    ("dollar", 36),
    ("e", 101),
    ("eacute", 233),
    ("ecircumflex", 234),
    ("edieresis", 235),
    ("egrave", 232),
    ("eight", 56),
    ("ellipsis", 8230),
    ("emdash", 8212),
    ("endash", 8211),
    ("equal", 61),
    ("eth", 240),
    ("exclam", 33),
    ("exclamdown", 161),
    ("f", 102),
    ("five", 53),
    ("florin", 402),
    ("four", 52),
    ("g", 103),
    ("germandbls", 223),
    ("grave", 96),
    ("greater", 62),
    ("guillemotleft", 171),
    ("guillemotright", 187),
    ("guilsinglleft", 8249),
    ("guilsinglright", 8250),
    ("h", 104),
    ("hyphen", 45),
    ("i", 105),
    ("iacute", 237),
    ("icircumflex", 238),
    ("idieresis", 239),
    ("igrave", 236),
    ("j", 106),
    ("k", 107),
    ("l", 108),
    ("less", 60),
    ("logicalnot", 172),
    ("m", 109),
    ("macron", 175),
    ("mu", 181),
    ("multiply", 215),
    ("n", 110),
    ("nine", 57),
    ("ntilde", 241),
    ("numbersign", 35),
    ("o", 111),
    ("oacute", 243),
    ("ocircumflex", 244),
    ("odieresis", 246),
    ("oe", 339),
    ("ograve", 242),
    ("one", 49),
    ("onehalf", 189),
    ("onequarter", 188),
    ("onesuperior", 185),
    ("ordfeminine", 170),
    ("ordmasculine", 186),
    ("oslash", 248),
    ("otilde", 245),
    ("p", 112),
    ("paragraph", 182),
    ("parenleft", 40),
    ("parenright", 41),
    ("percent", 37),
    ("period", 46),
    ("periodcentered", 183),
    ("perthousand", 8240),
    ("plus", 43),
    ("plusminus", 177),
    ("q", 113),
    ("question", 63),
    ("questiondown", 191),
    ("quotedbl", 34),
    ("quotedblbase", 8222),
    ("quotedblleft", 8220),
    ("quotedblright", 8221),
    ("quoteleft", 8216),
    ("quoteright", 8217),
    ("quotesinglbase", 8218),
    ("quotesingle", 39),
    ("r", 114),
    ("registered", 174),
    ("s", 115),
    ("scaron", 353),
    ("section", 167),
    ("semicolon", 59),
    ("seven", 55),
    ("six", 54),
    ("slash", 47),
    ("space", 32),
    ("sterling", 163),
    ("t", 116),
    ("thorn", 254),
    ("three", 51),
    ("threequarters", 190),
    ("threesuperior", 179),
    ("tilde", 732),
    ("trademark", 8482),
    ("two", 50),
    ("twosuperior", 178),
    ("u", 117),
    ("uacute", 250),
    ("ucircumflex", 251),
    ("udieresis", 252),
    ("ugrave", 249),
    ("underscore", 95),
    ("v", 118),
    ("w", 119),
    ("x", 120),
    ("y", 121),
    ("yacute", 253),
    ("ydieresis", 255),
    ("yen", 165),
    ("z", 122),
    ("zcaron", 382),
    ("zero", 48),
];
