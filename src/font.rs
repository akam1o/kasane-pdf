//! Font handling for text rendering (ISO 32000-1 §9).
//!
//! **Embedded TrueType simple fonts**:
//! `/Subtype /TrueType` + `/FontDescriptor /FontFile2` + a
//! `WinAnsiEncoding` (or an encoding dictionary with a
//! `WinAnsiEncoding`/`MacRomanEncoding` `BaseEncoding` plus an
//! optional `Differences` array).
//!
//! **Composite Type 0 fonts** — `/Subtype /Type0` with
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
    /// Decoded `/FontFile2` bytes — kept alive for `face`. Borrowed
    /// (static) when a bundled substitute stands in for a non-embedded
    /// font.
    program: std::borrow::Cow<'static, [u8]>,
    /// The substitution in effect for a non-embedded font.
    subst: Option<crate::fontsubst::FontSubstitution>,
    /// Encoding table: char code (0..=255) → glyph name from the PDF
    /// `/Encoding` (base table + `/Differences`). `None` means no
    /// name applies and the code goes straight to the font's cmap
    /// (§9.6.6.4 — the symbolic-font path).
    encoding: [Option<Vec<u8>>; 256],
    /// `/FontDescriptor /Flags` Symbolic bit set (and Nonsymbolic
    /// clear): codes map through the `(3,0)`/`(1,0)` cmaps directly.
    symbolic: bool,
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
    /// A parsed eexec (`/FontFile`) Type 1 program — `Some` when the
    /// descriptor carries a real Adobe Type 1 program rather than a
    /// CFF `/FontFile3`. Holds decrypted CharStrings + Subrs; glyph
    /// outlines are produced by the Type-1 charstring interpreter.
    t1_program: Option<crate::type1::Type1Program>,
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
    /// Bundled substitute for a non-embedded font (see
    /// [`crate::fontsubst`]). `Some` only when both `program` and
    /// `t1_program` are `None` and a sound substitute exists.
    subst: Option<crate::fontsubst::SubstFace>,
    /// Whether the PDF supplied `/Widths`. Without it a substituted
    /// Standard 14 font takes advances from the (metric-compatible)
    /// substitute's `hmtx`.
    has_widths: bool,
}

/// The resolved font behind a `Tf` selection — a simple TrueType
/// font, a CFF Type1 font, or a composite Type 0 / CIDFontType2
/// font. They share outline extraction and the §9.4 text-state
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
    load_font_dict(doc, dict, name, limits)
}

/// Load a font from its dictionary — the resource lookup already
/// done (`Tf`) or not applicable (`/ExtGState /Font [ref size]`,
/// §8.4.5, which names the font object directly). `name` labels
/// errors only.
pub(crate) fn load_font_dict(
    doc: &Document,
    dict: &Dict,
    name: &[u8],
    limits: &Limits,
) -> Result<Font, Error> {
    // A simple font whose program is an OpenType wrapper (`/FontFile3
    // /Subtype /OpenType`, §9.9 Table 126) is loaded through the
    // TrueType path whatever its declared subtype: `ttf-parser` reads
    // both `glyf` and `CFF ` outlines from the sfnt container, and the
    // glyph lookup (cmap, post, CFF charset names) is the same.
    let opentype_ff3 = dict
        .get(b"FontDescriptor".as_slice())
        .and_then(|o| doc.deref(o).ok())
        .and_then(|o| o.as_dict().cloned())
        .and_then(|fd| {
            fd.get(b"FontFile3".as_slice())
                .and_then(|o| doc.deref(o).ok())
        })
        .and_then(|ff| {
            ff.as_stream().map(|(d, raw)| {
                d.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) == Some(b"OpenType")
                    || doc
                        .decode_stream(d, raw)
                        .map(|b| is_sfnt(&b))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
        Some(b"TrueType") => Ok(Font::Simple(load_simple_font_dict(
            doc, dict, name, limits,
        )?)),
        Some(b"Type1") | Some(b"MMType1") if opentype_ff3 => Ok(Font::Simple(
            load_simple_font_dict(doc, dict, name, limits)?,
        )),
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

/// [`load_simple_font`] on an already resolved dictionary; `name`
/// labels errors only.
pub(crate) fn load_simple_font_dict(
    doc: &Document,
    dict: &Dict,
    name: &[u8],
    limits: &Limits,
) -> Result<Rc<SimpleFont>, Error> {
    match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
        // /Type1 lands here only with an OpenType /FontFile3 (see
        // `load_font`).
        Some(b"TrueType") | Some(b"Type1") | Some(b"MMType1") => {}
        Some(other) => {
            return Err(Error::Unsupported {
                feature: "font subtype",
                detail: format!(
                    "/{} — not a simple TrueType font",
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
        // `/Widths 12 0 R` is common (pdfTeX, many generators) — the
        // array itself may be indirect, not just its entries.
        let w = doc.deref(w)?;
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

    // ---- FontDescriptor + FontFile2 -------------------------------
    let fd_obj = dict
        .get(b"FontDescriptor".as_slice())
        .ok_or_else(|| Error::Structure("font has no /FontDescriptor".into()))?;
    let fd = doc.deref(fd_obj)?;
    let fd_dict = fd
        .as_dict()
        .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;
    // Table 123: bit 3 = Symbolic, bit 6 = Nonsymbolic. A font that
    // sets Symbolic (and not Nonsymbolic) addresses its glyphs by code
    // through the (3,0)/(1,0) cmaps; everything else goes through
    // glyph names (§9.6.6.4).
    let flags = fd_dict
        .get(b"Flags".as_slice())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let symbolic = flags & 4 != 0 && flags & 32 == 0;

    // ---- Encoding ------------------------------------------------
    let encoding = resolve_truetype_encoding(doc, dict, symbolic)?;
    let (program, subst): (
        std::borrow::Cow<'static, [u8]>,
        Option<crate::fontsubst::FontSubstitution>,
    ) = match fd_dict
        .get(b"FontFile2".as_slice())
        .or_else(|| fd_dict.get(b"FontFile3".as_slice()))
    {
        Some(ff2_obj) => {
            let ff2 = doc.deref(ff2_obj)?;
            let (ff2_dict, ff2_raw) = ff2
                .as_stream()
                .ok_or_else(|| Error::Structure("/FontFile2 is not a stream".into()))?;
            let program = doc.decode_stream(ff2_dict, ff2_raw)?;
            if fd_dict.get(b"FontFile2".as_slice()).is_none() && !is_sfnt(&program) {
                return Err(Error::Structure(
                    "TrueType font with a non-OpenType /FontFile3 — the subtype and program disagree".into(),
                ));
            }
            if program.len() as u64 > limits.max_stream_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_stream_bytes (embedded font)",
                    max: limits.max_stream_bytes,
                });
            }
            (std::borrow::Cow::Owned(program), None)
        }
        None => {
            // Non-embedded: a bundled substitute, never a host font.
            // The Symbolic refusal above already excluded fonts whose
            // glyph set is unknowable; `pick` still returns `None` for
            // names it cannot read (legacy code-page CJK names).
            let base = dict
                .get(b"BaseFont".as_slice())
                .and_then(|o| o.as_name())
                .unwrap_or(b"");
            match crate::fontsubst::pick(base, Some(fd_dict))? {
                Some(s) => (std::borrow::Cow::Borrowed(s.data), Some(s.record())),
                None => {
                    return Err(Error::Unsupported {
                        feature: "non-embedded TrueType font",
                        detail: "/FontFile2 absent and no sound substitute for this font \
                                 name; OS substitution is not performed"
                            .into(),
                    })
                }
            }
        }
    };

    // ---- ToUnicode (optional) ------------------------------------
    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;

    Ok(Rc::new(SimpleFont {
        program,
        subst,
        encoding,
        symbolic,
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
    let has_widths = dict.contains_key(b"Widths".as_slice());
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
    let mut fd_for_subst: Option<Dict> = None;

    // ---- Font program (optional) --------------------------------
    // Resolve the program:
    //   * `/FontFile3` (CFF) → decode + validate → `program`.
    //   * `/FontFile` (Adobe Type 1 / eexec) → decode, eexec-decrypt +
    //     charstring parse → `t1_program`.
    //   * no font-file / no descriptor → both `None` → metrics-only
    //     font (extraction works; `outline` refuses so render is
    //     honestly Unsupported).
    let mut program: Option<Vec<u8>> = None;
    let mut t1_program: Option<crate::type1::Type1Program> = None;
    if let Some(fd_obj) = dict.get(b"FontDescriptor".as_slice()) {
        let fd = doc.deref(fd_obj)?;
        let fd_dict = fd
            .as_dict()
            .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;
        fd_for_subst = Some(fd_dict.clone());
        if let Some(ff_obj) = fd_dict.get(b"FontFile".as_slice()) {
            // eexec Type 1 — the real PFB/PFA program.
            let ff = doc.deref(ff_obj)?;
            let (ff_dict, ff_raw) = ff
                .as_stream()
                .ok_or_else(|| Error::Structure("/FontFile is not a stream".into()))?;
            let bytes = doc.decode_stream(ff_dict, ff_raw)?;
            if bytes.len() as u64 > limits.max_stream_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_stream_bytes (embedded font)",
                    max: limits.max_stream_bytes,
                });
            }
            t1_program = Some(crate::type1::parse_type1_program(&bytes)?);
        } else if let Some(ff3_obj) = fd_dict.get(b"FontFile3".as_slice()) {
            let ff3 = doc.deref(ff3_obj)?;
            let (ff3_dict, ff3_raw) = ff3
                .as_stream()
                .ok_or_else(|| Error::Structure("/FontFile3 is not a stream".into()))?;
            // `/Subtype /CIDFontType0C` inside /FontFile3 → not simple.
            if let Some(st) = ff3_dict
                .get(b"Subtype".as_slice())
                .and_then(|o| o.as_name())
            {
                if st == b"CIDFontType0C" {
                    return Err(Error::Unsupported {
                        feature: "CIDFontType0 CFF",
                        detail: "/FontFile3 CIDFontType0C is a CID font, not simple Type1".into(),
                    });
                }
            }
            let prog = doc.decode_stream(ff3_dict, ff3_raw)?;
            if prog.len() as u64 > limits.max_stream_bytes {
                return Err(Error::LimitExceeded {
                    limit: "max_stream_bytes (embedded font)",
                    max: limits.max_stream_bytes,
                });
            }
            CffTable::parse(&prog)
                .ok_or_else(|| Error::Structure("/FontFile3 is not a valid CFF program".into()))?;
            program = Some(prog);
        }
        // no font-file key → metrics-only (both stay None).
    }

    // ---- Non-embedded: bundled substitute ------------------------
    // Standard 14 names (and their Arial/Times New Roman/Courier New
    // aliases) get a metric-compatible Liberation face; unknown Latin
    // fonts a flag-driven generic one; Symbol/ZapfDingbats are refused
    // by `pick` itself; names we cannot read stay metrics-only (render
    // refuses, extraction still works).
    let subst = if program.is_none() && t1_program.is_none() {
        let base = dict
            .get(b"BaseFont".as_slice())
            .and_then(|o| o.as_name())
            .unwrap_or(b"");
        crate::fontsubst::pick(base, fd_for_subst.as_ref())?
    } else {
        None
    };

    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;

    Ok(Rc::new(Type1Font {
        program,
        t1_program,
        encoding,
        widths,
        tounicode,
        builtin_cff_encoding: builtin,
        cff: RefCell::new(None),
        subst,
        has_widths,
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

    // Seed with the named base table (a bare name or /BaseEncoding).
    // No base → the program's built-in encoding stays in effect for
    // codes /Differences does not name (Table 114).
    let base = match &enc {
        Object::Name(n) => Some(n.as_slice()),
        Object::Dict(d) => d.get(b"BaseEncoding".as_slice()).and_then(|o| o.as_name()),
        _ => None,
    };
    if let Some(b) = base {
        let tbl: &[Option<&str>; 256] = match b {
            b"WinAnsiEncoding" => &WINANSI,
            b"MacRomanEncoding" => &crate::encodings::MAC_ROMAN,
            b"StandardEncoding" => &crate::encodings::STANDARD,
            b"MacExpertEncoding" => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: "MacExpertEncoding is not supported".into(),
                })
            }
            other => {
                return Err(Error::Unsupported {
                    feature: "font encoding",
                    detail: format!(
                        "/{} is not a predefined encoding",
                        String::from_utf8_lossy(other)
                    ),
                })
            }
        };
        for (i, n) in tbl.iter().enumerate() {
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

/// Build the TrueType char-code → glyph-name table (§9.6.6.4,
/// Table 114).
///
/// * Non-symbolic: the base is the named encoding, the dict's
///   `/BaseEncoding`, or `StandardEncoding` when neither is given;
///   `/Differences` overlays it.
/// * Symbolic: no implicit base — codes go straight to the `(3,0)` /
///   `(1,0)` cmap unless `/Encoding` names a table or `/Differences`
///   names a code (those names are honoured first, then the cmap).
///
/// `MacExpertEncoding` and unknown encoding names are refused.
fn resolve_truetype_encoding(
    doc: &Document,
    font_dict: &Dict,
    symbolic: bool,
) -> Result<[Option<Vec<u8>>; 256], Error> {
    let mut table: [Option<Vec<u8>>; 256] = [const { None }; 256];
    let enc = match font_dict.get(b"Encoding".as_slice()) {
        None | Some(Object::Null) => None,
        Some(o) => Some(doc.deref(o)?),
    };
    let (base, diffs): (Option<Vec<u8>>, Option<Object>) = match &enc {
        None => (None, None),
        Some(Object::Name(n)) => (Some(n.clone()), None),
        Some(Object::Dict(d)) => (
            d.get(b"BaseEncoding".as_slice())
                .and_then(|o| o.as_name())
                .map(|n| n.to_vec()),
            d.get(b"Differences".as_slice()).cloned(),
        ),
        Some(_) => {
            return Err(Error::Structure(
                "/Encoding is neither a name nor a dictionary".into(),
            ))
        }
    };
    let base_table: Option<&[Option<&str>; 256]> = match base.as_deref() {
        Some(b"WinAnsiEncoding") => Some(&WINANSI),
        Some(b"MacRomanEncoding") => Some(&crate::encodings::MAC_ROMAN),
        Some(b"StandardEncoding") => Some(&crate::encodings::STANDARD),
        Some(b"MacExpertEncoding") => {
            return Err(Error::Unsupported {
                feature: "font encoding",
                detail: "MacExpertEncoding is not supported".into(),
            })
        }
        Some(other) => {
            return Err(Error::Unsupported {
                feature: "font encoding",
                detail: format!(
                    "/{} is not a predefined encoding",
                    String::from_utf8_lossy(other)
                ),
            })
        }
        None if symbolic => None,
        None => Some(&crate::encodings::STANDARD),
    };
    if let Some(t) = base_table {
        for (i, n) in t.iter().enumerate() {
            table[i] = n.map(|s| s.as_bytes().to_vec());
        }
    }
    if let Some(diff_obj) = diffs {
        let arr = doc.deref(&diff_obj)?;
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
    Ok(table)
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

    /// Char code (0..=255) → glyph name per the PDF encoding, if one
    /// applies (symbolic fonts without `/Differences` have none).
    pub(crate) fn glyph_name(&self, code: u8) -> Option<&str> {
        self.encoding[code as usize]
            .as_deref()
            .and_then(|n| std::str::from_utf8(n).ok())
    }

    /// The substitution in effect, if any — for render reporting.
    pub(crate) fn substitution(&self) -> Option<crate::fontsubst::FontSubstitution> {
        self.subst.clone()
    }

    /// Char code → glyph advance in 1/1000-em units (PDF `/Widths`).
    pub(crate) fn width(&self, code: u8) -> f64 {
        self.widths[code as usize]
    }

    /// Char code → glyph id (`.notdef` = glyph 0 when nothing maps).
    /// §9.6.6.4 in order:
    ///
    /// 1. An encoding name (non-symbolic base table or `/Differences`):
    ///    name → Unicode → `(3,1)` cmap; name → Mac OS Roman code →
    ///    `(1,0)` cmap; `post` table name; `gNN`/`glyphNN` index.
    /// 2. The code itself: `(3,0)` cmap at `code`, `0xF000`, `0xF100`,
    ///    `0xF200` + code; then `(1,0)` cmap at `code`.
    /// 3. A font with **no** cmap: the code is the glyph index.
    /// 4. A symbolic font that only has a `(3,1)` cmap and no names:
    ///    StandardEncoding name → Unicode → `(3,1)` (the producer
    ///    mislabelled a non-symbolic font — the only inference made,
    ///    and it still cannot pick a glyph the font does not name).
    ///
    /// Anything else is `.notdef`; no glyph is ever chosen by position
    /// in a table the font did not define.
    pub(crate) fn glyph_id(&self, code: u8) -> Result<Option<GlyphId>, Error> {
        let face = self.face()?;
        let tables = face.tables();
        let n_glyphs = face.number_of_glyphs();
        let cmap31 = find_cmap(tables, 3, 1);
        let cmap30 = find_cmap(tables, 3, 0);
        let cmap10 = find_cmap(tables, 1, 0);
        let has_cmap = tables
            .cmap
            .map(|c| !c.subtables.is_empty())
            .unwrap_or(false);

        let by_name = |name: &str| -> Option<GlyphId> {
            if let Some(u) = name_to_unicode(name) {
                if let Some(g) = cmap31.as_ref().and_then(|s| s.glyph_index(u)) {
                    return Some(g);
                }
                // Symbol fonts park Unicode-named glyphs in the F0xx range.
                if let Some(g) = cmap30
                    .as_ref()
                    .and_then(|s| s.glyph_index(0xF000 + (u & 0xFF)).filter(|_| u < 0x100))
                {
                    return Some(g);
                }
            }
            if let Some(mc) = mac_roman_code(name) {
                if let Some(g) = cmap10.as_ref().and_then(|s| s.glyph_index(mc as u32)) {
                    return Some(g);
                }
            }
            if let Some(g) = face.glyph_index_by_name(name) {
                return Some(g);
            }
            if let Some(gi) = crate::encodings::glyph_index_name(name) {
                if gi < n_glyphs {
                    return Some(GlyphId(gi));
                }
            }
            None
        };

        if let Some(name) = self.glyph_name(code) {
            if let Some(g) = by_name(name) {
                return Ok(Some(g));
            }
        }
        if let Some(s) = &cmap30 {
            for c in [
                code as u32,
                0xF000 + code as u32,
                0xF100 + code as u32,
                0xF200 + code as u32,
            ] {
                if let Some(g) = s.glyph_index(c) {
                    return Ok(Some(g));
                }
            }
        }
        if let Some(s) = &cmap10 {
            if let Some(g) = s.glyph_index(code as u32) {
                return Ok(Some(g));
            }
        }
        if !has_cmap {
            return Ok(Some(if (code as u16) < n_glyphs {
                GlyphId(code as u16)
            } else {
                GlyphId(0)
            }));
        }
        if self.symbolic && self.encoding[code as usize].is_none() {
            if let Some(name) = crate::encodings::STANDARD[code as usize] {
                if let Some(g) = by_name(name) {
                    return Ok(Some(g));
                }
            }
        }
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
        Ok(outline_glyph_lsb_adjusted(&face, glyph, builder))
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
pub(crate) fn name_to_unicode(name: &str) -> Option<u32> {
    GLYPH_NAME_TO_UNICODE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, u)| *u)
        .or_else(|| crate::encodings::uni_name_to_unicode(name))
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

    /// Char code → glyph advance in 1/1000 text-space units, so the
    /// common `w / 1000 × Tfs` formula applies as for every other font.
    /// Type 3 `/Widths` are in *glyph space* (§9.6.4, Table 112) and
    /// pass through `/FontMatrix` first: with the default
    /// `[0.001 0 0 0.001 0 0]` that is the familiar ÷1000, but a
    /// `[1 0 0 -1 0 0]` font (common from `dvipdf` bitmap output)
    /// advances by the raw width. `/Widths` is authoritative — the
    /// CharProc's `d0`/`d1` `wx` should agree but the font dict wins on
    /// conflict (§9.6.4.1).
    pub(crate) fn width(&self, code: u8) -> f64 {
        let w = self.widths[code as usize];
        // Horizontal component of (w, 0) under FontMatrix.
        self.font_matrix.a * w * 1000.0
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
                detail: "no embedded font program and no sound substitute (unknown or \
                         symbolic font) — glyph outlines unavailable"
                    .into(),
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
    /// The glyph name for `code`: the PDF `/Encoding` entry, else the
    /// embedded Type 1 program's own encoding (Table 114 — the
    /// built-in encoding is the base when no `/BaseEncoding` applies).
    fn glyph_name_bytes(&self, code: u8) -> Option<&[u8]> {
        if let Some(n) = self.encoding[code as usize].as_deref() {
            return Some(n);
        }
        self.t1_program
            .as_ref()
            .and_then(|t1| t1.builtin_glyph_name(code))
    }

    /// Char code → glyph advance in 1/1000-em units (PDF `/Widths`).
    /// A substituted Standard 14 font without `/Widths` takes the
    /// advance from the metric-compatible substitute.
    pub(crate) fn width(&self, code: u8) -> f64 {
        if !self.has_widths {
            if let Some(s) = &self.subst {
                if let Some(gid) = self.subst_glyph(code) {
                    if let Some(w) = s.advance_1000(gid) {
                        return w;
                    }
                }
            }
        }
        self.widths[code as usize]
    }

    /// The glyph name for `code` when a substitute is in use: the PDF
    /// `/Encoding` name, else StandardEncoding (the built-in encoding
    /// of every non-symbolic Standard 14 Type 1 font).
    fn subst_glyph(&self, code: u8) -> Option<GlyphId> {
        let s = self.subst.as_ref()?;
        let name: Vec<u8> = match self.glyph_name_bytes(code) {
            Some(n) => n.to_vec(),
            None => crate::type1::std_encoding_name(code as i64)?.to_vec(),
        };
        s.glyph_for_name(std::str::from_utf8(&name).ok()?)
    }

    /// The substitution in effect, if any — for render reporting.
    pub(crate) fn substitution(&self) -> Option<crate::fontsubst::FontSubstitution> {
        self.subst.as_ref().map(|s| s.record())
    }

    /// Char code → glyph id. Order (§9.6.5.1):
    ///
    /// **eexec `/FontFile`:** the encoding name maps to a CharString —
    /// if one exists we return `GlyphId(code)` so `outline` can recover
    /// the code; `.notdef` otherwise.
    /// **CFF `/FontFile3`:** `/Encoding` name → `glyph_index_by_name`,
    /// else the program's built-in `glyph_index`, else `.notdef`.
    /// **Substitute:** encoding name (or StandardEncoding) → Unicode →
    /// the substitute's cmap; `.notdef` when the substitute lacks it.
    /// **No program** → `.notdef`; `outline` refuses, keeping render
    /// honest for metrics-only fonts.
    pub(crate) fn glyph_id(&self, code: u8) -> Result<Option<GlyphId>, Error> {
        if self.subst.is_some() {
            return Ok(Some(self.subst_glyph(code).unwrap_or(GlyphId(0))));
        }
        // eexec program — name→CharString.
        if let Some(t1) = &self.t1_program {
            if let Some(name) = self.glyph_name_bytes(code) {
                if t1.has_glyph(name) {
                    return Ok(Some(GlyphId(code as u16)));
                }
            }
            // no /Encoding name or glyph missing — .notdef
            return Ok(Some(GlyphId(0)));
        }
        // CFF program.
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
    /// eexec `glyph` carries the char *code* (see `glyph_id`).
    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        if let Some(s) = &self.subst {
            if glyph.0 == 0 {
                return Ok(false); // .notdef — draw nothing, still advance
            }
            return Ok(outline_glyph_lsb_adjusted(s.face(), glyph, builder));
        }
        if let Some(t1) = &self.t1_program {
            let code = glyph.0 as u8;
            let name = match self.glyph_name_bytes(code) {
                Some(n) => n.to_vec(),
                None => return Ok(false), // .notdef
            };
            // Interpret the CharString → outline moves in glyph space.
            return t1.charstring(&name, builder).map(|(had, _)| had);
        }
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

    /// CFF fonts are nominally 1000 units/em (FontMatrix 0.001). For
    /// an eexec program the `/FontMatrix` scale maps design space →
    /// text space, so the effective upem is `1/fm.a` (1/0.001 = 1000
    /// for the standard matrix) — `GlyphPath` then produces
    /// text-space coordinates directly.
    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        if let Some(s) = &self.subst {
            return Ok(f64::from(s.face().units_per_em()));
        }
        if let Some(t1) = &self.t1_program {
            let a = t1.font_matrix().a;
            if a != 0.0 && a.is_finite() {
                return Ok(1.0 / a);
            }
        }
        Ok(1000.0)
    }
}

// --------------------------------------------------------------------
// Composite font (Type 0 / CIDFontType2 / CIDFontType0)
// --------------------------------------------------------------------

/// The glyph program behind a CIDFont.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CidProgram {
    /// `/FontFile2` — a TrueType (`glyf`) program; CID → GID through
    /// `/CIDToGIDMap`.
    TrueType,
    /// `/FontFile3 /Subtype /OpenType` — an OpenType wrapper; CID → GID
    /// through the embedded CFF charset when the CFF is CID-keyed,
    /// `/CIDToGIDMap` for a `glyf`-based one.
    OpenType,
    /// `/FontFile3 /Subtype /CIDFontType0C` (or `/Type1C`) — a bare CFF
    /// program; CID → GID through its charset when CID-keyed, else
    /// CID = GID (§9.7.4.2).
    Cff,
}

/// A Type 0 composite font (§9.7): character codes → CID through the
/// `/Encoding` CMap (Identity, or an embedded CMap stream), CID → GID
/// through the descendant's program, widths from `/DW`/`/W`
/// (vertical: `/DW2`/`/W2`), Unicode from `/ToUnicode`. Codes, CIDs
/// and GIDs are three different numbers and are never conflated.
pub(crate) struct CompositeFont {
    /// Decoded font program of the descendant CIDFont (borrowed for
    /// the bundled fallback face).
    program: std::borrow::Cow<'static, [u8]>,
    kind: CidProgram,
    /// Non-embedded font drawn with the bundled fallback: CID →
    /// Unicode map (glyphs are found through the fallback's cmap) and
    /// the substitution record.
    fallback: Option<(
        &'static std::collections::HashMap<u32, char>,
        crate::fontsubst::FontSubstitution,
    )>,
    /// `/CIDToGIDMap` (CIDFontType2 only): `None` = Identity, `Some`
    /// = big-endian 2-byte-per-CID lookup.
    cid_to_gid: Option<Vec<u8>>,
    /// Embedded `/Encoding` CMap; `None` = Identity-H/V.
    cmap: Option<crate::cmap::CMap>,
    /// Vertical writing mode (`Identity-V` or CMap `/WMode 1`).
    vertical: bool,
    /// Default glyph width (1/1000 em), from `/DW`. Default 1000.
    dw: f64,
    /// Width overrides from `/W` — sorted `(first_cid, last_cid, width)`.
    w: Vec<(u32, u32, f64)>,
    /// `/DW2` `[vy w1y]`, default `[880 −1000]` (§9.7.4.3).
    dw2: (f64, f64),
    /// `/W2` overrides — sorted `(first_cid, last_cid, w1y, vx, vy)`.
    w2: Vec<(u32, u32, f64, f64, f64)>,
    /// `/ToUnicode` CMap — `None` if absent.
    tounicode: Option<crate::tounicode::ToUnicode>,
    /// `Face` over `program` (TrueType / OpenType) — lazy, same
    /// borrow pattern as `SimpleFont`.
    face: RefCell<Option<Face<'static>>>,
    /// Bare CFF table over `program` (`CidProgram::Cff`) — lazy.
    cff: RefCell<Option<CffTable<'static>>>,
    /// CID → GID for a CID-keyed CFF charset, built once from the
    /// table's GID → CID mapping. `None` until built; an empty map
    /// means the CFF is not CID-keyed (CID = GID).
    cid_gid: RefCell<Option<std::collections::HashMap<u16, u16>>>,
}

/// Load a Type 0 font dictionary (§9.7.6): `/Encoding` is
/// `/Identity-H`, `/Identity-V` or an embedded CMap stream;
/// `/DescendantFonts` holds exactly one CIDFontType2 (TrueType,
/// `/FontFile2` or `/FontFile3 /OpenType`) or CIDFontType0 (CFF,
/// `/FontFile3`). Other predefined CMaps and non-embedded programs
/// are refused.
fn load_composite_font(
    doc: &Document,
    dict: &Dict,
    limits: &Limits,
) -> Result<Rc<CompositeFont>, Error> {
    let (cmap, mut vertical) = match dict.get(b"Encoding".as_slice()) {
        Some(Object::Name(n)) if n.as_slice() == b"Identity-H" => (None, false),
        Some(Object::Name(n)) if n.as_slice() == b"Identity-V" => (None, true),
        Some(Object::Name(other)) => match crate::cmap::CMap::predefined(other, limits)? {
            Some(cm) => {
                let v = cm.vertical;
                (Some(cm), v)
            }
            None => {
                return Err(Error::Unsupported {
                    feature: "Type 0 font encoding",
                    detail: format!(
                        "/{} — predefined CMaps other than Identity-H/V are not bundled \
                         (build with the `cjk-fallback` feature for the Adobe-Japan1 set)",
                        String::from_utf8_lossy(other)
                    ),
                })
            }
        },
        Some(o @ Object::Ref(_)) | Some(o @ Object::Stream { .. }) => {
            let s = doc.deref(o)?;
            let Some((sd, raw)) = s.as_stream() else {
                return Err(Error::Structure(
                    "/Encoding must be the name Identity-H/V or a CMap stream".into(),
                ));
            };
            let bytes = doc.decode_stream(sd, raw)?;
            let use_cmap = sd.get(b"UseCMap".as_slice()).and_then(|o| o.as_name());
            let cm = crate::cmap::CMap::parse(&bytes, use_cmap, limits)?;
            let wmode = sd.get(b"WMode".as_slice()).and_then(|o| o.as_i64()) == Some(1);
            let v = cm.vertical || wmode;
            (Some(cm), v)
        }
        Some(_) => {
            return Err(Error::Structure(
                "/Encoding must be the name Identity-H/V or a CMap stream".into(),
            ))
        }
        None => return Err(Error::Structure("/Type0 font has no /Encoding".into())),
    };

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
    let is_type0 = match cid_dict
        .get(b"Subtype".as_slice())
        .and_then(|o| o.as_name())
    {
        Some(b"CIDFontType2") => false,
        Some(b"CIDFontType0") => true,
        Some(other) => {
            return Err(Error::Unsupported {
                feature: "CIDFont subtype",
                detail: format!(
                    "/{} — only /CIDFontType0 and /CIDFontType2 exist",
                    String::from_utf8_lossy(other)
                ),
            })
        }
        None => {
            return Err(Error::Structure(
                "/DescendantFonts[0] has no /Subtype".into(),
            ))
        }
    };
    let mut ordering: Vec<u8> = Vec::new();
    match cid_dict.get(b"CIDSystemInfo".as_slice()) {
        Some(o) => {
            let info = doc.deref(o)?;
            let d = info
                .as_dict()
                .ok_or_else(|| Error::Structure("/CIDSystemInfo is not a dictionary".into()))?;
            for key in [&b"Registry"[..], &b"Ordering"[..]] {
                match d.get(key).map(|o| doc.deref(o)).transpose()? {
                    Some(Object::String(s)) => {
                        if key == b"Ordering" {
                            ordering = s.clone();
                        }
                    }
                    None => {}
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

    let fd_obj = cid_dict
        .get(b"FontDescriptor".as_slice())
        .ok_or_else(|| Error::Structure("CIDFont has no /FontDescriptor".into()))?;
    let fd = doc.deref(fd_obj)?;
    let fd_dict = fd
        .as_dict()
        .ok_or_else(|| Error::Structure("/FontDescriptor is not a dictionary".into()))?;

    // ---- the program ---------------------------------------------
    let mut fallback: Option<(
        &'static std::collections::HashMap<u32, char>,
        crate::fontsubst::FontSubstitution,
    )> = None;
    let (kind, program): (CidProgram, std::borrow::Cow<'static, [u8]>) =
        if let Some(ff3) = fd_dict.get(b"FontFile3".as_slice()) {
            let ff3 = doc.deref(ff3)?;
            let (ff3_dict, ff3_raw) = ff3
                .as_stream()
                .ok_or_else(|| Error::Structure("/FontFile3 is not a stream".into()))?;
            let program = doc.decode_stream(ff3_dict, ff3_raw)?;
            let kind = match ff3_dict
                .get(b"Subtype".as_slice())
                .and_then(|o| o.as_name())
            {
                // The bytes decide: an sfnt wrapper labelled /CIDFontType0C
                // is still OpenType (common in arXiv/LaTeX output).
                _ if is_sfnt(&program) => CidProgram::OpenType,
                Some(b"OpenType") => CidProgram::OpenType,
                Some(b"CIDFontType0C") | Some(b"Type1C") | None => CidProgram::Cff,
                Some(other) => {
                    return Err(Error::Unsupported {
                        feature: "CIDFont /FontFile3 subtype",
                        detail: format!("/{}", String::from_utf8_lossy(other)),
                    })
                }
            };
            (kind, std::borrow::Cow::Owned(program))
        } else if let Some(ff2) = fd_dict.get(b"FontFile2".as_slice()) {
            let ff2 = doc.deref(ff2)?;
            let (ff2_dict, ff2_raw) = ff2
                .as_stream()
                .ok_or_else(|| Error::Structure("/FontFile2 is not a stream".into()))?;
            (
                CidProgram::TrueType,
                std::borrow::Cow::Owned(doc.decode_stream(ff2_dict, ff2_raw)?),
            )
        } else {
            // Non-embedded: the bundled CJK fallback for a known ordering
            // (feature `cjk-fallback`), drawn through CID → Unicode →
            // the fallback face's cmap; never a host font.
            match (
                crate::cjk::fallback_font(&ordering),
                crate::cjk::cid_to_unicode(&ordering),
            ) {
                (Some((name, data)), Some(map)) => {
                    let base = dict
                        .get(b"BaseFont".as_slice())
                        .and_then(|o| o.as_name())
                        .map(|n| {
                            let n = String::from_utf8_lossy(n).into_owned();
                            // Strip a subset tag (`ABCDEF+Name`).
                            match n.split_once('+') {
                                Some((tag, rest))
                                    if tag.len() == 6
                                        && tag.bytes().all(|b| b.is_ascii_uppercase()) =>
                                {
                                    rest.to_string()
                                }
                                _ => n,
                            }
                        })
                        .unwrap_or_default();
                    fallback = Some((
                        map,
                        crate::fontsubst::FontSubstitution {
                            base_font: base,
                            substitute: name,
                            kind: crate::fontsubst::SubstKind::CjkFallback,
                        },
                    ));
                    (CidProgram::OpenType, std::borrow::Cow::Borrowed(data))
                }
                _ => {
                    return Err(Error::Unsupported {
                        feature: "non-embedded CIDFont",
                        detail: format!(
                        "/FontFile2 or /FontFile3 is required; OS substitution is not performed \
                         (ordering {:?}; the `cjk-fallback` feature bundles a fallback for Japan1)",
                        String::from_utf8_lossy(&ordering)
                    ),
                    });
                }
            }
        };
    if program.len() as u64 > limits.max_stream_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_stream_bytes (embedded CID font)",
            max: limits.max_stream_bytes,
        });
    }
    // Validate the program up front so a broken font is a structure
    // error at `Tf`, not a surprise mid-string.
    match kind {
        CidProgram::Cff => {
            if is_type0 && CffTable::parse(&program).is_none() {
                return Err(Error::Structure(
                    "/FontFile3 is not a valid CFF program".into(),
                ));
            }
            if !is_type0 {
                return Err(Error::Structure(
                    "CIDFontType2 with a bare CFF /FontFile3 — the subtype and program disagree"
                        .into(),
                ));
            }
        }
        CidProgram::TrueType | CidProgram::OpenType => {
            if Face::parse(&program, 0).is_err() {
                return Err(Error::Structure(
                    "CIDFont program is not a valid TrueType/OpenType font".into(),
                ));
            }
        }
    }

    let cid_to_gid = match cid_dict.get(b"CIDToGIDMap".as_slice()) {
        None | Some(Object::Null) => None, // default: Identity
        Some(Object::Name(n)) if n.as_slice() == b"Identity" => None,
        Some(_) if is_type0 => None, // only meaningful for CIDFontType2
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

    let dw = match cid_dict.get(b"DW".as_slice()) {
        None | Some(Object::Null) => 1000.0,
        Some(o) => {
            let w = doc
                .deref(o)?
                .as_number()
                .ok_or_else(|| Error::Structure("/DW is not a number".into()))?;
            if !(w.is_finite() && w >= 0.0) {
                return Err(Error::Structure("/DW is not a non-negative number".into()));
            }
            w
        }
    };
    let w = parse_w_array(doc, cid_dict.get(b"W".as_slice()))?;

    // ---- vertical metrics (§9.7.4.3) ------------------------------
    let dw2 = match cid_dict.get(b"DW2".as_slice()) {
        None | Some(Object::Null) => (880.0, -1000.0),
        Some(o) => {
            let a = doc.deref(o)?;
            let arr = a
                .as_array()
                .ok_or_else(|| Error::Structure("/DW2 is not an array".into()))?;
            let v: Option<Vec<f64>> = arr.iter().map(|x| x.as_number()).collect();
            match v.as_deref() {
                Some([vy, w1]) if vy.is_finite() && w1.is_finite() => (*vy, *w1),
                _ => return Err(Error::Structure("/DW2 must be [vy w1y]".into())),
            }
        }
    };
    let mut w2: Vec<(u32, u32, f64, f64, f64)> = Vec::new();
    if let Some(w2_obj) = cid_dict.get(b"W2".as_slice()) {
        let arr = doc.deref(w2_obj)?;
        let arr = arr
            .as_array()
            .ok_or_else(|| Error::Structure("/W2 is not an array".into()))?;
        let num = |o: &Object| -> Result<f64, Error> {
            let v = doc
                .deref(o)?
                .as_number()
                .ok_or_else(|| Error::Structure("/W2 entries must be numbers".into()))?;
            if !v.is_finite() {
                return Err(Error::Structure("/W2 entries must be finite".into()));
            }
            Ok(v)
        };
        let mut i = 0usize;
        while i < arr.len() {
            let c0 = arr[i]
                .as_i64()
                .filter(|c| (0..=0xFFFF).contains(c))
                .ok_or_else(|| Error::Structure(format!("/W2[{i}] must be a CID")))?
                as u32;
            i += 1;
            match arr.get(i).map(|o| doc.deref(o)).transpose()? {
                Some(Object::Array(ws)) => {
                    if ws.len() % 3 != 0 {
                        return Err(Error::Structure("/W2 sub-array must hold triples".into()));
                    }
                    for (k, tri) in ws.as_chunks::<3>().0.iter().enumerate() {
                        let cid = c0
                            .checked_add(k as u32)
                            .filter(|c| *c <= 0xFFFF)
                            .ok_or_else(|| Error::Structure("/W2 CID overflow".into()))?;
                        w2.push((cid, cid, num(&tri[0])?, num(&tri[1])?, num(&tri[2])?));
                    }
                    i += 1;
                }
                Some(_) => {
                    let c1 = arr[i]
                        .as_i64()
                        .filter(|c| (c0 as i64..=0xFFFF).contains(c))
                        .ok_or_else(|| Error::Structure("/W2 range bound is invalid".into()))?
                        as u32;
                    if i + 3 >= arr.len() {
                        return Err(Error::Structure("/W2 range needs w1y vx vy".into()));
                    }
                    w2.push((
                        c0,
                        c1,
                        num(&arr[i + 1])?,
                        num(&arr[i + 2])?,
                        num(&arr[i + 3])?,
                    ));
                    i += 4;
                }
                None => return Err(Error::Structure("/W2 array ends mid-group".into())),
            }
        }
        w2.sort_unstable_by_key(|e| e.0);
    }
    if !vertical && cid_dict.contains_key(b"W2".as_slice()) {
        // Vertical metrics present on a horizontal font are inert.
        vertical = false;
    }

    let tounicode = crate::tounicode::load_tounicode(doc, dict, limits)?;
    if let Some(t) = &tounicode {
        let two_byte = cmap.as_ref().map(|c| c.is_two_byte()).unwrap_or(true);
        if two_byte && t.code_len() != 2 {
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
        kind,
        fallback: fallback.take(),
        cid_to_gid,
        cmap,
        vertical,
        dw,
        w,
        dw2,
        w2,
        tounicode,
        face: RefCell::new(None),
        cff: RefCell::new(None),
        cid_gid: RefCell::new(None),
    }))
}

/// `/W` (§9.7.4.3): `c [w …]` and `cfirst clast w` groups → sorted
/// `(first, last, width)` ranges.
fn parse_w_array(doc: &Document, w_obj: Option<&Object>) -> Result<Vec<(u32, u32, f64)>, Error> {
    let mut w: Vec<(u32, u32, f64)> = Vec::new();
    let Some(w_obj) = w_obj else { return Ok(w) };
    let arr = doc.deref(w_obj)?;
    let arr = arr
        .as_array()
        .ok_or_else(|| Error::Structure("/W is not an array".into()))?;
    let mut i = 0usize;
    while i < arr.len() {
        let c0 = doc
            .deref(&arr[i])?
            .as_i64()
            .ok_or_else(|| Error::Structure(format!("/W[{i}] must be an integer CID")))?;
        if !(0..=0xFFFF).contains(&c0) {
            return Err(Error::Structure(format!("/W[{i}] CID {c0} out of range")));
        }
        i += 1;
        if i >= arr.len() {
            return Err(Error::Structure("/W array ends mid-group".into()));
        }
        match doc.deref(&arr[i])? {
            Object::Array(ws) => {
                let base = c0 as u32;
                for (k, e) in ws.iter().enumerate() {
                    let wv = doc.deref(e)?.as_number().ok_or_else(|| {
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
            other => {
                let clast = other
                    .as_i64()
                    .ok_or_else(|| Error::Structure("/W range bound must be an integer".into()))?;
                let wv = arr
                    .get(i + 1)
                    .map(|o| doc.deref(o))
                    .transpose()?
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
    Ok(w)
}

impl CompositeFont {
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

    fn cff(&self) -> Result<std::cell::Ref<'_, CffTable<'static>>, Error> {
        {
            let mut slot = self.cff.borrow_mut();
            if slot.is_none() {
                let t = CffTable::parse(&self.program).ok_or_else(|| {
                    Error::Structure("/FontFile3 is not a valid CFF program".into())
                })?;
                // SAFETY: as for `face` — `program` is owned and immutable.
                let t: CffTable<'static> = unsafe { std::mem::transmute(t) };
                *slot = Some(t);
            }
        }
        Ok(std::cell::Ref::map(self.cff.borrow(), |o| {
            o.as_ref().unwrap()
        }))
    }

    /// CID → GID through a CFF charset (§9.7.4.2): for a CID-keyed
    /// CFF the charset maps GID → CID and is inverted once here; for a
    /// non-CID CFF the CID is the GID. Built from at most 65 536
    /// glyphs, so bounded regardless of the charset's claims.
    fn cff_gid(&self, table: &CffTable<'_>, cid: u32) -> u16 {
        let n = table.number_of_glyphs();
        if self.cid_gid.borrow().is_none() {
            let mut map = std::collections::HashMap::new();
            if table.glyph_cid(GlyphId(0)).is_some() {
                for g in 0..n {
                    if let Some(c) = table.glyph_cid(GlyphId(g)) {
                        // The first glyph claiming a CID wins — a
                        // duplicate CID in a charset is a font bug.
                        map.entry(c).or_insert(g);
                    }
                }
            }
            *self.cid_gid.borrow_mut() = Some(map);
        }
        let map = self.cid_gid.borrow();
        let map = map.as_ref().expect("built above");
        if map.is_empty() {
            // Not CID-keyed: CID = GID.
            return if cid < n as u32 { cid as u16 } else { 0 };
        }
        if cid > 0xFFFF {
            return 0;
        }
        map.get(&(cid as u16)).copied().unwrap_or(0)
    }

    /// Character code → CID (§9.7.6.3).
    fn cid(&self, code: u32, code_len: usize) -> u32 {
        match &self.cmap {
            None => code,
            Some(cm) => cm.cid(code, code_len),
        }
    }

    /// Read one code at `bytes[pos]`: 2 bytes for Identity, else as
    /// the CMap's codespace dictates.
    pub(crate) fn read_code(&self, bytes: &[u8], pos: usize) -> Result<(u32, usize), Error> {
        match &self.cmap {
            None => {
                if pos + 1 >= bytes.len() {
                    return Err(Error::Structure(
                        "string ends mid-code — Identity CMaps use 2-byte codes".into(),
                    ));
                }
                Ok((((bytes[pos] as u32) << 8) | bytes[pos + 1] as u32, 2))
            }
            Some(cm) => Ok(cm.next_code(bytes, pos)),
        }
    }

    /// Code (with its byte length packed by `Font::read_code`) → GID.
    /// `code` here is the plain code; `len` the byte length it was
    /// read with (needed to disambiguate CMap ranges).
    pub(crate) fn glyph_id_len(&self, code: u32, len: usize) -> Result<GlyphId, Error> {
        let cid = self.cid(code, len);
        if let Some((map, _)) = &self.fallback {
            let face = self.face()?;
            return Ok(map
                .get(&cid)
                .and_then(|ch| face.glyph_index(*ch))
                .unwrap_or(GlyphId(0)));
        }
        match self.kind {
            CidProgram::Cff => {
                let t = self.cff()?;
                Ok(GlyphId(self.cff_gid(&t, cid)))
            }
            CidProgram::OpenType => {
                let face = self.face()?;
                if let Some(t) = face.tables().cff.as_ref() {
                    if t.glyph_cid(GlyphId(0)).is_some() {
                        return Ok(GlyphId(self.cff_gid(t, cid)));
                    }
                }
                let gid = self.truetype_gid(cid);
                let n = face.number_of_glyphs();
                Ok(if gid < n as u32 {
                    GlyphId(gid as u16)
                } else {
                    GlyphId(0)
                })
            }
            CidProgram::TrueType => {
                let gid = self.truetype_gid(cid);
                let face = self.face()?;
                let n = face.number_of_glyphs();
                Ok(if gid < n as u32 {
                    GlyphId(gid as u16)
                } else {
                    GlyphId(0)
                })
            }
        }
    }

    fn truetype_gid(&self, cid: u32) -> u32 {
        match &self.cid_to_gid {
            None => cid,
            Some(map) => {
                let off = (cid as usize) * 2;
                if off + 2 > map.len() {
                    0 // CID beyond map → .notdef
                } else {
                    ((map[off] as u32) << 8) | map[off + 1] as u32
                }
            }
        }
    }

    /// Horizontal advance `w0` (1/1000 em) for `code`.
    pub(crate) fn width_len(&self, code: u32, len: usize) -> f64 {
        let cid = self.cid(code, len);
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

    /// Vertical writing mode.
    pub(crate) fn vertical(&self) -> bool {
        self.vertical
    }

    /// Vertical metrics `(w1y, vx, vy)` in 1/1000 em (§9.7.4.3): from
    /// `/W2`, else `vx = w0 / 2`, `(vy, w1y)` from `/DW2`.
    pub(crate) fn vertical_metrics_len(&self, code: u32, len: usize) -> (f64, f64, f64) {
        let cid = self.cid(code, len);
        for (a, b, w1y, vx, vy) in &self.w2 {
            if cid >= *a && cid <= *b {
                return (*w1y, *vx, *vy);
            }
        }
        (self.dw2.1, self.width_len(code, len) / 2.0, self.dw2.0)
    }

    pub(crate) fn outline(
        &self,
        glyph: GlyphId,
        builder: &mut dyn OutlineBuilder,
    ) -> Result<bool, Error> {
        match self.kind {
            CidProgram::Cff => {
                let t = self.cff()?;
                Ok(t.outline(glyph, builder).is_ok())
            }
            CidProgram::OpenType | CidProgram::TrueType => {
                let face = self.face()?;
                Ok(outline_glyph_lsb_adjusted(&face, glyph, builder))
            }
        }
    }

    pub(crate) fn unicode(&self, code: u32) -> Option<String> {
        self.tounicode.as_ref()?.decode(code)
    }

    pub(crate) fn has_tounicode(&self) -> bool {
        self.tounicode.is_some()
    }

    /// Word spacing applies to the single-byte code 32 only (§9.3.3):
    /// never under Identity, only for a CMap with a 1-byte codespace.
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        match &self.cmap {
            Some(cm) => code == 32 && !cm.is_two_byte() && cm.next_code(&[32], 0) == (32, 1),
            None => false,
        }
    }

    pub(crate) fn units_per_em(&self) -> Result<f64, Error> {
        match self.kind {
            CidProgram::Cff => {
                let t = self.cff()?;
                let m = t.matrix();
                let sx = m.sx as f64;
                Ok(if sx.is_finite() && sx > 0.0 {
                    1.0 / sx
                } else {
                    1000.0
                })
            }
            _ => Ok(self.face()?.units_per_em() as f64),
        }
    }
}

impl Font {
    /// Decode one code at `bytes` big-endian. Returns `(code,
    /// consumed_bytes)` or `Err` for a truncated tail (odd trailing
    /// byte on a 2-byte font). Codes never span string boundaries.
    /// Read one character code at `bytes[pos]` → `(code, byte length)`.
    /// Composite codes may be 1–4 bytes (their CMap decides); the
    /// length is packed into the code's top bits (`CODE_LEN_SHIFT`) so
    /// every later lookup can tell `<41>` from `<0041>`.
    pub(crate) fn read_code(&self, bytes: &[u8], pos: usize) -> Result<(u32, usize), Error> {
        match self {
            Font::Simple(_) | Font::Type1(_) | Font::Type3(_) => Ok((bytes[pos] as u32, 1)),
            Font::Composite(f) => {
                let (code, len) = f.read_code(bytes, pos)?;
                Ok((pack_code(code, len), len))
            }
        }
    }

    /// Vertical writing mode (composite fonts with Identity-V / WMode 1).
    pub(crate) fn vertical(&self) -> bool {
        match self {
            Font::Composite(f) => f.vertical(),
            _ => false,
        }
    }

    /// Vertical metrics `(w1y, vx, vy)` in 1/1000 em for a composite
    /// font code; `(−1000, w0/2, 880)` for anything else.
    pub(crate) fn vertical_metrics(&self, code: u32) -> (f64, f64, f64) {
        match self {
            Font::Composite(f) => {
                let (c, l) = unpack_code(code);
                f.vertical_metrics_len(c, l)
            }
            _ => (-1000.0, self.width(code) / 2.0, 880.0),
        }
    }

    /// `true` when `Tw` applies to this code — only single-byte
    /// code 32 on a simple font (§9.3.3).
    pub(crate) fn is_space_code(&self, code: u32) -> bool {
        match self {
            Font::Simple(f) => f.is_space_code(code),
            Font::Composite(f) => f.is_space_code(unpack_code(code).0),
            Font::Type1(f) => f.is_space_code(code),
            Font::Type3(f) => f.is_space_code(code),
        }
    }

    /// Code → glyph advance (1/1000 em) from the font's PDF width
    /// table — `/Widths` for simple, `/DW`/`/W` for composite.
    pub(crate) fn width(&self, code: u32) -> f64 {
        match self {
            Font::Simple(f) => f.width(code as u8),
            Font::Composite(f) => {
                let (c, l) = unpack_code(code);
                f.width_len(c, l)
            }
            Font::Type1(f) => f.width(code as u8),
            Font::Type3(f) => f.width(code as u8),
        }
    }

    /// Code → glyph id — `.notdef` (0) when unresolvable. Never
    /// `None`: a code always produces a glyph slot.
    pub(crate) fn glyph_id(&self, code: u32) -> Result<GlyphId, Error> {
        match self {
            Font::Simple(f) => Ok(f.glyph_id(code as u8)?.unwrap_or(GlyphId(0))),
            Font::Composite(f) => {
                let (c, l) = unpack_code(code);
                f.glyph_id_len(c, l)
            }
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
            Font::Composite(f) => f.unicode(unpack_code(code).0),
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

    /// The bundled-substitute record when this font is non-embedded
    /// and drawn with a substitute; `None` for embedded fonts.
    pub(crate) fn substitution(&self) -> Option<crate::fontsubst::FontSubstitution> {
        match self {
            Font::Simple(f) => f.substitution(),
            Font::Type1(f) => f.substitution(),
            Font::Composite(f) => f.fallback.as_ref().map(|(_, s)| s.clone()),
            Font::Type3(_) => None,
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
/// `true` when `b` starts with an sfnt signature (`OTTO`, TrueType
/// `0x00010000`, Apple `true`, or a `ttcf` collection).
fn is_sfnt(b: &[u8]) -> bool {
    b.len() >= 4 && matches!(&b[..4], b"OTTO" | b"\x00\x01\x00\x00" | b"true" | b"ttcf")
}

/// Composite character codes carry their byte length so CMap lookups
/// can tell `<41>` from `<0041>`: 2-byte codes (the Identity case and
/// by far the most common) are stored untagged; 1- and 3-byte codes
/// put their length in the top byte; a 4-byte code is stored as-is
/// (`unpack_code` reads a value above 0xFFFFFF as 4 bytes — a 4-byte
/// code whose top byte is 0–3 is ambiguous and reported as 2/1/3
/// bytes; such codespaces do not occur in practice). `CharInfo.code`
/// always receives the *unpacked* code.
pub(crate) fn pack_code(code: u32, len: usize) -> u32 {
    match len {
        1 | 3 if code < 1 << 24 => code | ((len as u32) << 24),
        _ => code,
    }
}

/// The plain character code for reporting: composite codes are
/// unpacked, simple-font codes are returned unchanged.
pub(crate) fn unpack_code_for_font(font: &Font, code: u32) -> u32 {
    match font {
        Font::Composite(_) => unpack_code(code).0,
        _ => code,
    }
}

pub(crate) fn unpack_code(packed: u32) -> (u32, usize) {
    match packed >> 24 {
        0 => (packed, 2),
        t @ (1 | 3) => (packed & 0x00FF_FFFF, t as usize),
        _ => (packed, 4),
    }
}

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

/// Outline a TrueType (`glyf`) glyph with its origin placed the way
/// FreeType — and therefore Poppler, Acrobat and most viewers — place
/// it: when the `hmtx` left side bearing differs from the `glyf`
/// header `xMin`, the outline is translated by `lsb − xMin` so the ink
/// starts at the advertised bearing. Well-formed fonts have
/// `lsb == xMin` and pass through untouched; faces without a `glyf`
/// table (CFF-flavoured OpenType) are never shifted. The corpus file
/// `cidfont_cmap_overflow` (lsb 0, xMin 80) rendered 80 units to the
/// right of the reference before this adjustment.
fn outline_glyph_lsb_adjusted(
    face: &Face<'_>,
    glyph: GlyphId,
    builder: &mut dyn OutlineBuilder,
) -> bool {
    let dx = tt_lsb_shift(face, glyph);
    if dx == 0.0 {
        return face.outline_glyph(glyph, builder).is_some();
    }
    let mut shifted = ShiftX { inner: builder, dx };
    face.outline_glyph(glyph, &mut shifted).is_some()
}

/// `lsb − xMin` for a `glyf` glyph, computed the way FreeType's
/// TrueType loader derives its first phantom point
/// (`tt_loader_set_pp`): from the glyph *header* xMin (not the
/// outlined extent) and the `hmtx` bearing. For a composite glyph the
/// component flagged `USE_MY_METRICS` supplies the phantom point
/// instead (FreeType "restores" the composite's own points only when
/// no component claims them), so an accented letter built on a
/// well-formed base is never shifted even if the composite's own
/// header box disagrees with its bearing. Returns 0 for CFF faces,
/// empty glyphs, or anything unparseable.
fn tt_lsb_shift(face: &Face<'_>, glyph: GlyphId) -> f32 {
    // `FaceTables` does not expose `loca`; rebuild it from the raw
    // table with the `head`/`maxp` parameters the face already parsed.
    let raw = face.raw_face();
    let (Some(loca_raw), Some(glyf)) = (
        raw.table(ttf_parser::Tag::from_bytes(b"loca")),
        raw.table(ttf_parser::Tag::from_bytes(b"glyf")),
    ) else {
        return 0.0;
    };
    let Some(n_glyphs) = std::num::NonZeroU16::new(face.number_of_glyphs()) else {
        return 0.0;
    };
    let Some(loca) = ttf_parser::loca::Table::parse(
        n_glyphs,
        face.tables().head.index_to_location_format,
        loca_raw,
    ) else {
        return 0.0;
    };
    fn shift(
        face: &Face<'_>,
        loca: &ttf_parser::loca::Table<'_>,
        glyf: &[u8],
        gid: GlyphId,
        depth: usize,
    ) -> Option<f32> {
        if depth > 4 {
            return None;
        }
        let data = glyf.get(loca.glyph_range(gid)?)?;
        if data.len() < 10 {
            return None;
        }
        let be16 = |i: usize| i16::from_be_bytes([data[i], data[i + 1]]);
        let n_contours = be16(0);
        let x_min = be16(2);
        if n_contours < 0 {
            // Composite: walk the component records looking for
            // USE_MY_METRICS (0x0200). Record layout: flags, glyphIndex,
            // 2 args (bytes or words), optional scale/xy-scale/2×2.
            let mut p = 10usize;
            while p + 4 <= data.len() {
                let flags = u16::from_be_bytes([data[p], data[p + 1]]);
                let gi = u16::from_be_bytes([data[p + 2], data[p + 3]]);
                p += 4;
                p += if flags & 0x0001 != 0 { 4 } else { 2 };
                if flags & 0x0008 != 0 {
                    p += 2;
                } else if flags & 0x0040 != 0 {
                    p += 4;
                } else if flags & 0x0080 != 0 {
                    p += 8;
                }
                if flags & 0x0200 != 0 {
                    return shift(face, loca, glyf, GlyphId(gi), depth + 1);
                }
                if flags & 0x0020 == 0 {
                    break;
                }
            }
        }
        let lsb = face.glyph_hor_side_bearing(gid)?;
        Some(f32::from(lsb) - f32::from(x_min))
    }
    shift(face, &loca, glyf, glyph, 0).unwrap_or(0.0)
}

/// `OutlineBuilder` adaptor that offsets every x coordinate by `dx`
/// (font units) before forwarding — see `outline_glyph_lsb_adjusted`.
struct ShiftX<'a> {
    inner: &'a mut dyn OutlineBuilder,
    dx: f32,
}

impl OutlineBuilder for ShiftX<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.inner.move_to(x + self.dx, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.inner.line_to(x + self.dx, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.inner.quad_to(x1 + self.dx, y1, x + self.dx, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.inner
            .curve_to(x1 + self.dx, y1, x2 + self.dx, y2, x + self.dx, y);
    }
    fn close(&mut self) {
        self.inner.close();
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
