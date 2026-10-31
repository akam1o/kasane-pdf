//! Unicode for character codes a font's `/ToUnicode` does not cover —
//! text extraction only; glyph selection never uses any of this.
//!
//! Two sources, both exact or silent:
//!
//! * **Glyph names** (§9.10.2 method 2): a name is mapped by the
//!   algorithm of Adobe's "Unicode and Glyph Names" — drop everything
//!   from the first period, split at underscores, map every component
//!   through the Adobe Glyph List or as `uniXXXX…` / `uXXXX[XX]` — and
//!   the whole name maps only when every component does. Names with no
//!   meaning (`g12`, `glyph7`, `a65`) map to nothing.
//! * **The font's own Unicode `cmap`**, read backwards (glyph →
//!   code points). A glyph reached from several code points is
//!   ambiguous and maps to nothing, unless the code points are one of a
//!   few known equivalents (space / no-break space, hyphen / soft
//!   hyphen, ohm / omega, …), which map to the first of them. The
//!   reverse table is bounded ([`MAX_REVERSE_CMAP`]); a larger `cmap`
//!   is not read backwards at all.
//!
//! Private-use code points, controls and noncharacters are never
//! returned: they say nothing about the text.

use std::collections::HashMap;

/// Code points read from a font's `cmap` to build its reverse table;
/// above this the table is not built and the source is unused.
pub(crate) const MAX_REVERSE_CMAP: usize = 131_072;

/// Whether `c` can stand for text (not private use, a control, a
/// surrogate or a noncharacter).
pub(crate) fn textual(c: u32) -> bool {
    !(c < 0x20
        || (0x7F..=0x9F).contains(&c)
        || (0xD800..=0xDFFF).contains(&c)
        || (0xE000..=0xF8FF).contains(&c)
        || (0xF0000..=0x10FFFF).contains(&c)
        || (0xFDD0..=0xFDEF).contains(&c)
        || (c & 0xFFFE) == 0xFFFE
        || c > 0x10FFFF)
}

/// Upper-case hex digits only (the glyph-name convention).
fn upper_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// One underscore-separated component of a glyph name.
fn component(c: &str) -> Option<String> {
    if let Ok(i) = crate::agl::AGL.binary_search_by(|(n, _)| n.as_bytes().cmp(c.as_bytes())) {
        return Some(crate::agl::AGL[i].1.to_string());
    }
    if let Some(h) = c.strip_prefix("uni") {
        if h.len() >= 4 && h.len() % 4 == 0 && upper_hex(h) {
            let mut s = String::new();
            for k in (0..h.len()).step_by(4) {
                let v = u32::from_str_radix(&h[k..k + 4], 16).ok()?;
                if !textual(v) {
                    return None;
                }
                s.push(char::from_u32(v)?);
            }
            return Some(s);
        }
    }
    if let Some(h) = c.strip_prefix('u') {
        if (4..=6).contains(&h.len()) && upper_hex(h) {
            let v = u32::from_str_radix(h, 16).ok()?;
            if textual(v) {
                return char::from_u32(v).map(String::from);
            }
        }
    }
    None
}

/// The Unicode string a glyph name stands for, by the Adobe glyph-name
/// algorithm (module docs), or `None`.
pub(crate) fn agl_unicode(name: &str) -> Option<String> {
    let base = name.split('.').next().unwrap_or("");
    if base.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in base.split('_') {
        out.push_str(&component(c)?);
    }
    (!out.is_empty()).then_some(out)
}

/// Code points that fonts commonly give one shared glyph and that read
/// as the same text; the first of each group is the answer.
const EQUIVALENT: &[&[u32]] = &[
    &[0x0020, 0x00A0],
    &[0x002D, 0x00AD, 0x2010, 0x2011],
    &[0x03A9, 0x2126],
    &[0x03BC, 0x00B5],
    &[0x0394, 0x2206],
    &[0x002F, 0x2215],
    &[0x00B7, 0x2219],
];

/// One answer for a glyph reached from `cps` (deduplicated, textual),
/// or `None` when they disagree.
fn decide(cps: &[u32]) -> Option<char> {
    match cps {
        [] => None,
        [c] => char::from_u32(*c),
        _ => EQUIVALENT
            .iter()
            .find(|g| cps.iter().all(|c| g.contains(c)))
            .and_then(|g| char::from_u32(g[0])),
    }
}

/// Glyph → Unicode from a font's Unicode `cmap` subtables (platform 0,
/// and platform 3 encodings 1 and 10), read backwards.
pub(crate) struct ReverseCmap(HashMap<u16, Option<char>>);

impl ReverseCmap {
    /// `None` when the font has no Unicode subtable or more than
    /// [`MAX_REVERSE_CMAP`] mapped code points.
    pub(crate) fn build(face: &ttf_parser::Face<'_>) -> Option<ReverseCmap> {
        let cmap = face.tables().cmap?;
        let mut by_gid: HashMap<u16, Vec<u32>> = HashMap::new();
        let mut seen = 0usize;
        let mut any = false;
        for st in cmap.subtables {
            if !st.is_unicode() {
                continue;
            }
            any = true;
            let mut cps = Vec::new();
            let mut over = false;
            st.codepoints(|cp| {
                if cps.len() < MAX_REVERSE_CMAP {
                    cps.push(cp);
                } else {
                    over = true;
                }
            });
            seen += cps.len();
            if over || seen > MAX_REVERSE_CMAP {
                return None;
            }
            for cp in cps {
                if !textual(cp) {
                    continue;
                }
                if let Some(g) = st.glyph_index(cp) {
                    if g.0 != 0 {
                        let v = by_gid.entry(g.0).or_default();
                        if !v.contains(&cp) {
                            v.push(cp);
                        }
                    }
                }
            }
        }
        if !any {
            return None;
        }
        Some(ReverseCmap(
            by_gid
                .into_iter()
                .map(|(g, mut cps)| {
                    cps.sort_unstable();
                    (g, decide(&cps))
                })
                .collect(),
        ))
    }

    /// The single character glyph `gid` stands for, if unambiguous.
    pub(crate) fn get(&self, gid: u16) -> Option<char> {
        self.0.get(&gid).copied().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names_follow_the_adobe_algorithm() {
        for (name, want) in [
            ("A", Some("A")),
            ("fi", Some("\u{FB01}")),
            ("ffi", Some("\u{FB03}")),
            ("minus", Some("\u{2212}")),
            ("Omega", Some("\u{2126}")),
            ("a.sc", Some("a")),
            ("one.oldstyle", Some("1")),
            ("f_f_i", Some("ffi")),
            ("uni0041", Some("A")),
            ("uni00410042", Some("AB")),
            ("u1F600", Some("\u{1F600}")),
            ("uni004", None),
            ("uni0041004", None),
            ("uni00e9", None),  // lower-case hex is not a uni name
            ("uniD800", None),  // a surrogate
            ("uniE000", None),  // private use
            ("u110000", None),  // beyond Unicode
            ("dotlessj", None), // AGL maps it to private use
            (".notdef", None),
            ("g12", None),
            ("glyph7", None),
            ("f_xyz", None), // one component means nothing
            ("", None),
        ] {
            assert_eq!(agl_unicode(name).as_deref(), want, "{name}");
        }
    }

    #[test]
    fn the_table_agrees_with_the_older_one() {
        // Every name the WinAnsi-subset table knows means the same in
        // the full list.
        for name in crate::encodings::STANDARD
            .iter()
            .chain(crate::encodings::MAC_ROMAN.iter())
            .flatten()
        {
            if let (Some(old), Some(new)) = (crate::font::name_to_unicode(name), agl_unicode(name))
            {
                assert_eq!(char::from_u32(old).map(String::from), Some(new), "{name}");
            }
        }
        // Sorted, so the binary search is sound.
        assert!(crate::agl::AGL
            .windows(2)
            .all(|w| w[0].0.as_bytes() < w[1].0.as_bytes()));
    }

    #[test]
    fn ambiguous_glyphs_decide_only_for_equivalents() {
        assert_eq!(decide(&[0x41]), Some('A'));
        assert_eq!(decide(&[0x20, 0xA0]), Some(' '));
        assert_eq!(decide(&[0x2D, 0xAD]), Some('-'));
        assert_eq!(decide(&[0x41, 0x391]), None); // Latin A vs Greek Alpha
        assert_eq!(decide(&[]), None);
        for c in [
            0x0, 0x1F, 0x85, 0xE000, 0xF8FF, 0xFFFE, 0x10FFFF, 0xFDD0, 0xD800,
        ] {
            assert!(!textual(c), "{c:X}");
        }
        for c in [0x20, 0x41, 0x3042, 0x1F600, 0xFFFD] {
            assert!(textual(c), "{c:X}");
        }
    }
}
