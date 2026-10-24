//! Substitute fonts for **non-embedded** simple fonts.
//!
//! A PDF may reference a font without embedding its program — legal
//! for the Standard 14 (§9.6.2.2), common in practice for a few more.
//! Rendering such text needs *some* outline source. This module
//! chooses one from a fixed set of bundled, licence-clean fonts
//! (`fonts/liberation/`, SIL OFL 1.1 — see its README for provenance):
//!
//! | family requested | substitute |
//! |---|---|
//! | Helvetica / Arial | Liberation Sans |
//! | Times / Times New Roman | Liberation Serif |
//! | Courier / Courier New | Liberation Mono |
//!
//! Liberation is metric-compatible with those families, so advances
//! match when the PDF omits `/Widths`. Shapes are *not* Adobe's; every
//! substitution is reported through [`FontSubstitution`] so callers
//! (and the compat runner) can tell a substituted render from a
//! faithful one.
//!
//! What is deliberately **not** done:
//! * no host-OS font lookup — output is reproducible everywhere;
//! * Symbol / ZapfDingbats draw with a bundled DejaVu Sans subset
//!   through their built-in encodings (`SubstKind::SymbolFallback`,
//!   an approximation that is always reported); previously refused as
//!   metric-compatible clone, and drawing those codes with Latin
//!   glyphs would be wrong, so they are refused (`Unsupported`);
//! * no guess for fonts whose glyph set we cannot know — a symbolic
//!   font with an unknown name, or a name that is not even ASCII
//!   (CJK fonts named in a legacy code page) yields `None` and the
//!   caller refuses as before.
//!
//! Selection is name-driven first (Standard 14 names and their
//! Arial/Times New Roman/Courier New aliases, style suffixes such as
//! `-BoldOblique`, `,BoldItalic`, `-BoldMT`), then descriptor-driven
//! for unknown Latin fonts (`/Flags` Serif/FixedPitch/Italic/ForceBold,
//! `/FontWeight`, `/ItalicAngle`), which is what other viewers do too.

use crate::error::Error;
use crate::object::Dict;
use ttf_parser::{Face, GlyphId};

/// Which bundled family was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstFamily {
    Sans,
    Serif,
    Mono,
}

/// How confident the choice is.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubstKind {
    /// One of the Standard 14 names (metric-compatible substitute).
    Standard14,
    /// A well-known alias of a Standard 14 face (Arial, Times New
    /// Roman, Courier New — metric-compatible as well).
    Alias,
    /// An unknown Latin font — family picked from the descriptor
    /// flags / name hints; metrics come from the PDF `/Widths`.
    Generic,
    /// A non-embedded CID-keyed CJK font drawn with the bundled
    /// fallback face (`cjk-fallback` feature) through CID → Unicode;
    /// glyph shapes differ from the intended font, metrics come from
    /// the PDF `/W`/`/DW`.
    CjkFallback,
    /// A non-embedded Symbol or ZapfDingbats font drawn with the
    /// bundled symbol fallback face (a DejaVu Sans subset) through the
    /// font's built-in encoding and glyph names → Unicode; glyph shapes
    /// differ from Adobe's, metrics come from the PDF `/Widths` or the
    /// Adobe AFM widths. An approximation, always reported.
    SymbolFallback,
}

/// One substitution the renderer performed on a page — surfaced in
/// `RenderedPage::font_substitutions` so a substituted render is
/// never mistaken for a faithful one.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSubstitution {
    /// The PDF `/BaseFont` name (subset tag stripped).
    pub base_font: String,
    /// The bundled face used, e.g. `LiberationSans-Bold`.
    pub substitute: &'static str,
    /// How the substitute was chosen (and therefore how far its
    /// shapes and metrics can be trusted).
    pub kind: SubstKind,
}

/// A chosen substitute: the static font bytes plus a parsed face.
pub(crate) struct SubstFace {
    pub(crate) name: &'static str,
    pub(crate) data: &'static [u8],
    pub(crate) kind: SubstKind,
    pub(crate) base_font: String,
    face: Face<'static>,
    /// The font's own built-in encoding when it is not
    /// StandardEncoding (Symbol, ZapfDingbats — Annex D.5/D.6).
    pub(crate) builtin_encoding: Option<&'static [Option<&'static str>; 256]>,
    /// AFM advance widths by glyph name, when the substitute's own
    /// metrics must not be used (Symbol, ZapfDingbats).
    pub(crate) afm_widths: Option<&'static [(&'static str, u16)]>,
}

impl SubstFace {
    pub(crate) fn face(&self) -> &Face<'static> {
        &self.face
    }

    /// Glyph for a PostScript glyph name via its Unicode value and the
    /// face's cmap. `None` when the name has no Unicode mapping or the
    /// substitute lacks the character.
    pub(crate) fn glyph_for_name(&self, name: &str) -> Option<GlyphId> {
        let u = glyph_name_unicode(name).or_else(|| crate::symbol_fonts::name_to_unicode(name))?;
        let c = char::from_u32(u)?;
        self.face.glyph_index(c)
    }

    /// Horizontal advance in PDF 1/1000-em units — used when the PDF
    /// omits `/Widths` (allowed for the Standard 14; Liberation's
    /// metrics match).
    pub(crate) fn advance_1000(&self, gid: GlyphId) -> Option<f64> {
        let adv = self.face.glyph_hor_advance(gid)? as f64;
        let upem = self.face.units_per_em() as f64;
        if upem <= 0.0 {
            return None;
        }
        Some(adv * 1000.0 / upem)
    }

    pub(crate) fn record(&self) -> FontSubstitution {
        FontSubstitution {
            base_font: self.base_font.clone(),
            substitute: self.name,
            kind: self.kind,
        }
    }
}

/// Glyph name → Unicode: the AGL table shared with the font loaders
/// plus the StandardEncoding / Symbol-ish names that table lacks and
/// `uniXXXX` / `uXXXX[XX]` forms.
pub(crate) fn glyph_name_unicode(name: &str) -> Option<u32> {
    if let Some(u) = crate::font::name_to_unicode(name) {
        return Some(u);
    }
    if let Some(&(_, u)) = EXTRA_NAMES.iter().find(|(n, _)| *n == name) {
        return Some(u);
    }
    // uniXXXX (exactly 4 hex) / uXXXX..uXXXXXX
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 {
            return u32::from_str_radix(hex, 16).ok();
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return u32::from_str_radix(hex, 16).ok();
        }
    }
    None
}

/// Names from StandardEncoding / MacRoman / common math that the
/// WinAnsi-derived AGL table in `font.rs` does not carry.
const EXTRA_NAMES: &[(&str, u32)] = &[
    ("fi", 0xFB01),
    ("fl", 0xFB02),
    ("ff", 0xFB00),
    ("ffi", 0xFB03),
    ("ffl", 0xFB04),
    ("dotlessi", 0x0131),
    ("fraction", 0x2044),
    ("breve", 0x02D8),
    ("dotaccent", 0x02D9),
    ("ring", 0x02DA),
    ("ogonek", 0x02DB),
    ("hungarumlaut", 0x02DD),
    ("caron", 0x02C7),
    ("macron", 0x00AF),
    ("Lslash", 0x0141),
    ("lslash", 0x0142),
    ("florin", 0x0192),
    ("quotesingle", 0x0027),
    ("grave", 0x0060),
    ("minus", 0x2212),
    ("mu", 0x00B5),
    ("Delta", 0x2206),
    ("Omega", 0x2126),
    ("pi", 0x03C0),
    ("product", 0x220F),
    ("summation", 0x2211),
    ("radical", 0x221A),
    ("infinity", 0x221E),
    ("integral", 0x222B),
    ("approxequal", 0x2248),
    ("notequal", 0x2260),
    ("lessequal", 0x2264),
    ("greaterequal", 0x2265),
    ("lozenge", 0x25CA),
    ("partialdiff", 0x2202),
    ("apple", 0xF8FF),
    ("nbspace", 0x00A0),
    ("sfthyphen", 0x00AD),
    ("periodcentered", 0x00B7),
    ("bulletoperator", 0x2219),
    ("Euro", 0x20AC),
    ("hyphen", 0x002D),
    ("space", 0x0020),
];

macro_rules! font_data {
    ($name:literal) => {
        (
            $name,
            include_bytes!(concat!("../fonts/liberation/", $name, ".ttf")) as &'static [u8],
        )
    };
}

/// One bundled face: `(name, TrueType bytes)`.
type FaceData = (&'static str, &'static [u8]);

/// The bundled faces, indexed `[family][bold][italic]`.
const FACES: [[[FaceData; 2]; 2]; 3] = [
    [
        [
            font_data!("LiberationSans-Regular"),
            font_data!("LiberationSans-Italic"),
        ],
        [
            font_data!("LiberationSans-Bold"),
            font_data!("LiberationSans-BoldItalic"),
        ],
    ],
    [
        [
            font_data!("LiberationSerif-Regular"),
            font_data!("LiberationSerif-Italic"),
        ],
        [
            font_data!("LiberationSerif-Bold"),
            font_data!("LiberationSerif-BoldItalic"),
        ],
    ],
    [
        [
            font_data!("LiberationMono-Regular"),
            font_data!("LiberationMono-Italic"),
        ],
        [
            font_data!("LiberationMono-Bold"),
            font_data!("LiberationMono-BoldItalic"),
        ],
    ],
];

/// Descriptor `/Flags` bits (Table 123).
const FLAG_FIXED_PITCH: i64 = 1;
const FLAG_SERIF: i64 = 1 << 1;
const FLAG_SYMBOLIC: i64 = 1 << 2;
const FLAG_NONSYMBOLIC: i64 = 1 << 5;
const FLAG_ITALIC: i64 = 1 << 6;
const FLAG_FORCE_BOLD: i64 = 1 << 18;

/// Choose a substitute for `base_font` (the raw `/BaseFont` name),
/// using `descriptor` (the `/FontDescriptor` dict, if any) for style
/// and family hints.
///
/// * `Ok(Some(face))` — a substitute was chosen.
/// * `Ok(None)` — no sound basis for a choice (non-ASCII name, or a
///   symbolic font we do not know); the caller refuses as before.
/// * `Err(Unsupported)` — Symbol / ZapfDingbats: known, but no
///   licence-clean substitute exists and approximating is wrong.
pub(crate) fn pick(
    base_font: &[u8],
    descriptor: Option<&Dict>,
) -> Result<Option<SubstFace>, Error> {
    let raw = match std::str::from_utf8(base_font) {
        Ok(s) if s.chars().all(|c| c.is_ascii_graphic() || c == ' ') && !s.is_empty() => s,
        _ => return Ok(None),
    };
    let stripped = strip_subset_tag(raw);
    let lower = stripped.to_ascii_lowercase();
    let (family_part, style_part) = split_family_style(&lower);
    let family_key = normalise_family(&family_part);
    let style_part = style_part.as_str();

    let flags = descriptor
        .and_then(|d| d.get(b"Flags".as_slice()))
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let weight = descriptor
        .and_then(|d| d.get(b"FontWeight".as_slice()))
        .and_then(|o| o.as_number())
        .unwrap_or(0.0);
    let italic_angle = descriptor
        .and_then(|d| d.get(b"ItalicAngle".as_slice()))
        .and_then(|o| o.as_number())
        .unwrap_or(0.0);

    let bold = style_has(
        style_part,
        &["bold", "black", "heavy", "semibold", "demibold"],
    ) || flags & FLAG_FORCE_BOLD != 0
        || weight >= 600.0;
    let italic = style_has(style_part, &["italic", "oblique"])
        || flags & FLAG_ITALIC != 0
        || italic_angle != 0.0;

    let (family, kind) = match family_key.as_str() {
        "helvetica" => (SubstFamily::Sans, SubstKind::Standard14),
        "times" | "timesroman" => (SubstFamily::Serif, SubstKind::Standard14),
        "courier" => (SubstFamily::Mono, SubstKind::Standard14),
        "arial" | "arialmt" => (SubstFamily::Sans, SubstKind::Alias),
        "timesnewroman" | "timesnewromanps" | "timesnewromanpsmt" => {
            (SubstFamily::Serif, SubstKind::Alias)
        }
        "couriernew" | "couriernewps" | "couriernewpsmt" => (SubstFamily::Mono, SubstKind::Alias),
        "symbol" => {
            return symbol_fallback(
                stripped,
                &crate::symbol_fonts::SYMBOL_ENCODING,
                crate::symbol_fonts::SYMBOL_WIDTHS,
            )
            .map(Some);
        }
        "zapfdingbats" | "dingbats" => {
            return symbol_fallback(
                stripped,
                &crate::symbol_fonts::ZAPF_DINGBATS_ENCODING,
                crate::symbol_fonts::ZAPF_DINGBATS_WIDTHS,
            )
            .map(Some);
        }
        _ => {
            // Unknown name. A symbolic font with no Nonsymbolic bit has
            // a glyph set we cannot know — refuse rather than draw
            // Latin letters for arbitrary codes.
            if flags & FLAG_SYMBOLIC != 0 && flags & FLAG_NONSYMBOLIC == 0 {
                return Ok(None);
            }
            let fam = if flags & FLAG_FIXED_PITCH != 0
                || name_has(&family_key, &["mono", "courier", "consol", "typewriter"])
            {
                SubstFamily::Mono
            } else if flags & FLAG_SERIF != 0
                || name_has(
                    &family_key,
                    &[
                        "serif",
                        "roman",
                        "times",
                        "georgia",
                        "garamond",
                        "book",
                        "minion",
                        "palatino",
                        "cambria",
                        "century",
                        "bodoni",
                        "baskerville",
                    ],
                )
            {
                SubstFamily::Serif
            } else {
                SubstFamily::Sans
            };
            (fam, SubstKind::Generic)
        }
    };

    let (name, data) = FACES[family as usize][bold as usize][italic as usize];
    let face = Face::parse(data, 0)
        .map_err(|e| Error::Structure(format!("bundled substitute font {name}: {e:?}")))?;
    Ok(Some(SubstFace {
        name,
        data,
        kind,
        base_font: stripped.to_string(),
        face,
        builtin_encoding: None,
        afm_widths: None,
    }))
}

/// The bundled symbol fallback face (`fonts/symbol-fallback/`): a
/// DejaVu Sans subset covering the Symbol and ZapfDingbats repertoires.
const SYMBOL_FALLBACK: FaceData = (
    "KasaneSymbolFallback",
    include_bytes!("../fonts/symbol-fallback/KasaneSymbolFallback.ttf") as &'static [u8],
);

/// Substitute for a non-embedded Symbol or ZapfDingbats font: the
/// bundled symbol face, the font's built-in encoding (Annex D.5/D.6)
/// for codes the PDF does not name, and the Adobe AFM widths when the
/// PDF supplies no `/Widths`. Reported as `SubstKind::SymbolFallback`.
fn symbol_fallback(
    stripped: &str,
    builtin: &'static [Option<&'static str>; 256],
    afm: &'static [(&'static str, u16)],
) -> Result<SubstFace, Error> {
    let (name, data) = SYMBOL_FALLBACK;
    let face = Face::parse(data, 0)
        .map_err(|e| Error::Structure(format!("bundled substitute font {name}: {e:?}")))?;
    Ok(SubstFace {
        name,
        data,
        kind: SubstKind::SymbolFallback,
        base_font: stripped.to_string(),
        face,
        builtin_encoding: Some(builtin),
        afm_widths: Some(afm),
    })
}

/// What the engine would do for a non-embedded simple font with this
/// `/BaseFont` and `/FontDescriptor` — the public, data-only view of
/// `pick` for tooling (the compat probe mirrors the renderer's
/// refusal rules with it). `Ok(Some(_))` = would substitute,
/// `Ok(None)` = would refuse (unknowable glyph set),
/// `Err(Unsupported)` = would refuse (Symbol / ZapfDingbats).
pub fn substitution_for(
    base_font: &[u8],
    descriptor: Option<&Dict>,
) -> Result<Option<FontSubstitution>, Error> {
    Ok(pick(base_font, descriptor)?.map(|s| s.record()))
}

/// `ABCDEF+Name` → `Name`.
fn strip_subset_tag(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest))
            if tag.len() == 6
                && tag.chars().all(|c| c.is_ascii_uppercase())
                && !rest.is_empty() =>
        {
            rest
        }
        _ => name,
    }
}

/// `helvetica-boldoblique` → (`helvetica`, `boldoblique`);
/// `arial,bold` → (`arial`, `bold`); `timesnewromanps-bolditalicmt` →
/// (`timesnewromanps`, `bolditalicmt`). Spaces are dropped first.
fn split_family_style(lower: &str) -> (String, String) {
    let compact: String = lower.chars().filter(|c| *c != ' ').collect();
    match compact.find([',', '-']) {
        Some(i) => (compact[..i].to_string(), compact[i + 1..].to_string()),
        None => {
            // Style glued onto the family: `arialbold`, `helveticabold`.
            for suffix in ["bolditalic", "boldoblique", "bold", "italic", "oblique"] {
                if let Some(fam) = compact.strip_suffix(suffix) {
                    if !fam.is_empty() {
                        return (fam.to_string(), suffix.to_string());
                    }
                }
            }
            (compact, String::new())
        }
    }
}

/// Drop decorative suffixes (`mt`, `psmt`, `ps`) so `arialmt` and
/// `arial` match the same key; keep known full keys intact.
fn normalise_family(fam: &str) -> String {
    let f = fam.to_string();
    for suffix in ["psmt", "mt"] {
        if let Some(core) = f.strip_suffix(suffix) {
            if matches!(
                core,
                "arial" | "timesnewroman" | "timesnewromanps" | "couriernew" | "couriernewps"
            ) {
                return core.trim_end_matches("ps").to_string();
            }
        }
    }
    f
}

fn style_has(style: &str, words: &[&str]) -> bool {
    words.iter().any(|w| style.contains(w))
}

fn name_has(name: &str, words: &[&str]) -> bool {
    words.iter().any(|w| name.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick_name(n: &str) -> SubstFace {
        pick(n.as_bytes(), None).unwrap().expect("substitute")
    }

    #[test]
    fn standard14_families_and_styles() {
        assert_eq!(pick_name("Helvetica").name, "LiberationSans-Regular");
        assert_eq!(pick_name("Helvetica-Bold").name, "LiberationSans-Bold");
        assert_eq!(pick_name("Helvetica-Oblique").name, "LiberationSans-Italic");
        assert_eq!(
            pick_name("Helvetica-BoldOblique").name,
            "LiberationSans-BoldItalic"
        );
        assert_eq!(pick_name("Times-Roman").name, "LiberationSerif-Regular");
        assert_eq!(
            pick_name("Times-BoldItalic").name,
            "LiberationSerif-BoldItalic"
        );
        assert_eq!(pick_name("Courier").name, "LiberationMono-Regular");
        assert_eq!(pick_name("Courier-Bold").name, "LiberationMono-Bold");
        assert_eq!(pick_name("Courier").kind, SubstKind::Standard14);
    }

    #[test]
    fn aliases_and_subset_tags() {
        assert_eq!(pick_name("Arial").name, "LiberationSans-Regular");
        assert_eq!(pick_name("Arial,Bold").name, "LiberationSans-Bold");
        assert_eq!(
            pick_name("Arial-BoldItalicMT").name,
            "LiberationSans-BoldItalic"
        );
        assert_eq!(pick_name("ArialMT").kind, SubstKind::Alias);
        assert_eq!(
            pick_name("TimesNewRomanPSMT").name,
            "LiberationSerif-Regular"
        );
        assert_eq!(
            pick_name("TimesNewRomanPS-BoldMT").name,
            "LiberationSerif-Bold"
        );
        assert_eq!(pick_name("CourierNewPSMT").name, "LiberationMono-Regular");
        let s = pick_name("ABCDEF+Helvetica-Bold");
        assert_eq!(s.name, "LiberationSans-Bold");
        assert_eq!(s.base_font, "Helvetica-Bold");
    }

    #[test]
    fn symbol_fonts_use_the_symbol_fallback() {
        // Symbol and ZapfDingbats draw with the bundled symbol face
        // through their built-in encodings, reported as an
        // approximation; their AFM widths stand in for /Widths.
        for n in ["Symbol", "ZapfDingbats", "Dingbats"] {
            let s = pick(n.as_bytes(), None).unwrap().expect(n);
            assert_eq!(s.kind, SubstKind::SymbolFallback, "{n}");
            assert_eq!(s.name, "KasaneSymbolFallback");
            let builtin = s.builtin_encoding.expect("built-in encoding");
            let afm = s.afm_widths.expect("AFM widths");
            if n == "Symbol" {
                assert_eq!(builtin[0x61], Some("alpha"));
                assert_eq!(builtin[0x22], Some("universal"));
                assert!(s.glyph_for_name("alpha").is_some(), "α");
                assert!(s.glyph_for_name("universal").is_some(), "∀");
                assert!(s.glyph_for_name("bracelefttp").is_some(), "⎧");
                assert_eq!(crate::symbol_fonts::afm_width(afm, "alpha"), Some(631.0));
            } else {
                assert_eq!(builtin[0x33], Some("a19"));
                assert!(s.glyph_for_name("a19").is_some(), "✓");
                assert!(s.glyph_for_name("a20").is_some(), "✔");
                assert!(crate::symbol_fonts::afm_width(afm, "a19").is_some());
            }
        }
    }

    #[test]
    fn unknown_fonts_use_descriptor_flags() {
        let mut d = Dict::new();
        d.insert(
            b"Flags".to_vec(),
            crate::object::Object::Integer(FLAG_SERIF | FLAG_NONSYMBOLIC),
        );
        d.insert(b"FontWeight".to_vec(), crate::object::Object::Integer(700));
        let s = pick(b"Garamond", Some(&d)).unwrap().unwrap();
        assert_eq!(s.name, "LiberationSerif-Bold");
        assert_eq!(s.kind, SubstKind::Generic);
        let mut d = Dict::new();
        d.insert(
            b"Flags".to_vec(),
            crate::object::Object::Integer(FLAG_FIXED_PITCH | FLAG_NONSYMBOLIC | FLAG_ITALIC),
        );
        assert_eq!(
            pick(b"Consolas", Some(&d)).unwrap().unwrap().name,
            "LiberationMono-Italic"
        );
        // no descriptor, unknown name → sans
        assert_eq!(pick_name("Verdana").name, "LiberationSans-Regular");
    }

    #[test]
    fn unknowable_fonts_yield_none() {
        // symbolic unknown
        let mut d = Dict::new();
        d.insert(
            b"Flags".to_vec(),
            crate::object::Object::Integer(FLAG_SYMBOLIC),
        );
        assert!(pick(b"Wingdings", Some(&d)).unwrap().is_none());
        // non-ASCII (legacy code-page CJK name)
        assert!(pick(b"\xcb\xce\xcc\xe5", None).unwrap().is_none());
        assert!(pick(b"", None).unwrap().is_none());
    }

    #[test]
    fn glyphs_and_metrics_come_from_the_face() {
        let s = pick_name("Helvetica");
        let a = s.glyph_for_name("A").expect("A");
        assert!(a.0 != 0);
        // Liberation Sans is metric-compatible with Helvetica: 'A' = 667.
        let w = s.advance_1000(a).unwrap();
        assert!((w - 667.0).abs() < 1.0, "A advance {w}");
        assert!(s.glyph_for_name("fi").is_some(), "StandardEncoding fi");
        assert!(s.glyph_for_name("uni00E9").is_some());
        assert!(s.glyph_for_name("nosuchglyph").is_none());
    }
}
