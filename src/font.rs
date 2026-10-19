//! Font handling for text rendering (ISO 32000-1 §9).
//!
//! M5 covers **embedded TrueType simple fonts** only:
//! `/Subtype /TrueType` + `/FontDescriptor /FontFile2` + a
//! `WinAnsiEncoding` (or an encoding dictionary with a
//! `WinAnsiEncoding`/`MacRomanEncoding` `BaseEncoding` plus an
//! optional `Differences` array). Everything else — Type 0 / CID
//! fonts, Type 1, Type 3, CFF (`FontFile3`), non-embedded fonts,
//! and any encoding setup we do not implement — is refused with
//! [`Error::Unsupported`] or [`Error::Structure`], never silently
//! rendered with a substitute.
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
            if let Some(s) = tu.decode(code) {
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
