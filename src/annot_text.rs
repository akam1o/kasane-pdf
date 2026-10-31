//! Text carried by a page's annotations and form fields, reported next
//! to — never mixed into — the page content's text (`TextOptions::
//! with_annotations`).
//!
//! Every annotation of `/Annots` yields one [`AnnotationText`], in
//! `/Annots` order, the same annotation object once even when listed
//! twice. Its sources stay apart, each read once:
//!
//! * `contents` — `/Contents` (§12.5.2), the annotation's text or its
//!   alternate description;
//! * `rich_text` — `/RC` (§12.7.3.4) exactly as stored (XHTML), not
//!   interpreted;
//! * `field` — for a widget, its form field (§12.7.3): fully qualified
//!   name, type and the *value* `/V` and default `/DV` (choice fields
//!   through `/Opt` to their display text). A field with several
//!   widgets on the page reports its value on the first only;
//! * `appearance` — the characters the normal appearance draws, placed
//!   on the page, exactly what rendering shows (a synthesised field
//!   appearance included). Hidden annotations draw nothing.
//!
//! [`AnnotationText::text`] picks one string by priority, so the same
//! words are not counted twice: a field's value (first widget only),
//! else for other annotations their `/Contents`, else the appearance.

use crate::document::Document;
use crate::error::Error;
use crate::object::{Dict, IndirectRef, Object};
use crate::page::Rect;
use crate::text_extract::CharInfo;

/// `/Parent` levels followed for field names and inherited attributes.
const MAX_PARENT_DEPTH: usize = 32;
/// `/Opt` entries read for a choice field.
const MAX_OPTIONS: usize = 1024;

/// The text of one annotation (see the module documentation).
///
/// Fields may be added in minor releases (`#[non_exhaustive]`).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct AnnotationText {
    /// Position of the annotation in the page's `/Annots` array.
    pub index: usize,
    /// `/Subtype` (`Widget`, `FreeText`, `Text`, …); empty if absent.
    pub subtype: String,
    /// `/Rect`, normalised (lower-left first), in user space.
    pub rect: Rect,
    /// Not shown: the Hidden or NoView flag, or optional content that
    /// is OFF. Its `appearance` is empty and [`AnnotationText::text`]
    /// is `None`; the dictionary values are still reported.
    pub hidden: bool,
    /// `/Contents` as text.
    pub contents: Option<String>,
    /// `/RC` (rich text, XHTML) exactly as stored.
    pub rich_text: Option<String>,
    /// The form field of a widget annotation.
    pub field: Option<FieldText>,
    /// Characters drawn by the normal appearance, in page space.
    pub appearance: Vec<CharInfo>,
}

/// The form field behind a widget annotation (§12.7.3).
///
/// Fields may be added in minor releases (`#[non_exhaustive]`).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FieldText {
    /// Fully qualified name: the `/T` of the field and its ancestors,
    /// joined with periods (§12.7.3.2).
    pub name: String,
    /// `/FT` (`Tx`, `Ch`, `Btn`, `Sig`), inherited; empty if absent.
    pub field_type: String,
    /// The value `/V` as text: one string for a text field, the
    /// selected options' display text for a choice field. Empty for
    /// buttons and signatures, when there is no value, and on every
    /// widget of the field but the first on the page.
    pub value: Vec<String>,
    /// The default value `/DV`, read like `value`.
    pub default_value: Vec<String>,
    /// Whether this is the field's first widget on the page (the one
    /// that reports `value`).
    pub first_widget: bool,
}

impl AnnotationText {
    /// The annotation's text, by priority, or `None`: nothing for a
    /// hidden annotation; a widget's field value (first widget only;
    /// further widgets of the field `None`), else its appearance; any
    /// other annotation's `/Contents`, else its appearance.
    pub fn text(&self) -> Option<String> {
        if self.hidden {
            return None;
        }
        let drawn = || {
            let t: String = self
                .appearance
                .iter()
                .filter_map(|c| c.unicode.as_deref())
                .collect();
            (!t.trim().is_empty()).then_some(t)
        };
        match &self.field {
            Some(f) if !f.first_widget => None,
            Some(f) if !f.value.is_empty() => Some(f.value.join("\n")),
            Some(_) => drawn(),
            None => self
                .contents
                .clone()
                .filter(|c| !c.trim().is_empty())
                .or_else(drawn),
        }
    }
}

/// Decode a PDF text string (§7.9.2.2): UTF-16BE or UTF-8 with their
/// byte-order marks (language escapes removed), else PDFDocEncoding
/// (Annex D); undefined PDFDocEncoding codes become U+FFFD.
pub(crate) fn text_string(b: &[u8]) -> String {
    let strip_escapes = |s: String| -> String {
        // ESC lang ESC sequences mark a language, not text.
        let mut out = String::with_capacity(s.len());
        let mut inside = false;
        for c in s.chars() {
            if c == '\u{1B}' {
                inside = !inside;
            } else if !inside {
                out.push(c);
            }
        }
        out
    };
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let units: Vec<u16> = b[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return strip_escapes(String::from_utf16_lossy(&units));
    }
    if b.len() >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF {
        return strip_escapes(String::from_utf8_lossy(&b[3..]).into_owned());
    }
    b.iter()
        .map(|&c| {
            if let Some((u, _)) = crate::crypt::PDFDOC_SPECIALS.iter().find(|(_, x)| *x == c) {
                *u
            } else if matches!(c, 0x7F | 0x9F | 0xAD) || (0x80..=0x9E).contains(&c) {
                '\u{FFFD}'
            } else {
                c as char
            }
        })
        .collect()
}

/// A text value that may be a string or a text stream (§12.7.4.3: a
/// text field's `/V`, `/RC`).
fn text_value(doc: &Document, o: &Object, budget: &mut Budget) -> Result<Option<String>, Error> {
    let t = match doc.deref(o)? {
        Object::String(s) => text_string(&s),
        Object::Stream { dict, data } => text_string(&doc.decode_stream(&dict, &data)?),
        _ => return Ok(None),
    };
    budget.charge(t.len())?;
    Ok(Some(t))
}

/// The UTF-8 bytes of annotation text a page may report, shared with
/// the page content's budget (`max_extracted_unicode_bytes`).
struct Budget {
    used: u64,
    max: u64,
}

impl Budget {
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.used = self.used.saturating_add(n as u64);
        if self.used > self.max {
            return Err(Error::LimitExceeded {
                limit: "max_extracted_unicode_bytes",
                max: self.max,
            });
        }
        Ok(())
    }
}

/// The field dictionary of a widget (itself when it carries `/T`, else
/// its `/Parent`), its reference when indirect, and its fully
/// qualified name.
fn field_of(
    doc: &Document,
    widget: &Dict,
    widget_ref: Option<IndirectRef>,
    budget: &mut Budget,
) -> Result<(Option<IndirectRef>, String), Error> {
    let mut parts: Vec<String> = Vec::new();
    let mut field_ref = widget_ref;
    let mut cur = widget.clone();
    let mut found_field = false;
    let mut seen: Vec<IndirectRef> = widget_ref.into_iter().collect();
    for depth in 0..=MAX_PARENT_DEPTH {
        if depth == MAX_PARENT_DEPTH {
            return Err(Error::LimitExceeded {
                limit: "form field /Parent depth",
                max: MAX_PARENT_DEPTH as u64,
            });
        }
        if let Some(t) = cur.get(b"T".as_slice()) {
            if let Some(t) = text_value(doc, t, budget)? {
                parts.push(t);
            }
            found_field = true;
        }
        let Some(p) = cur.get(b"Parent".as_slice()) else {
            break;
        };
        if let Some(r) = p.as_ref() {
            if seen.contains(&r) {
                return Err(Error::CycleDetected {
                    context: "form field /Parent chain",
                });
            }
            seen.push(r);
            if !found_field {
                field_ref = Some(r);
            }
        }
        match doc.deref(p)? {
            Object::Dict(d) => cur = d,
            // An undefined parent (§7.3.10) ends the chain.
            _ => break,
        }
    }
    parts.reverse();
    Ok((field_ref, parts.join(".")))
}

/// `key` on the widget or up its `/Parent` chain (bounded).
fn inherited(doc: &Document, widget: &Dict, key: &[u8]) -> Result<Option<Object>, Error> {
    let mut cur = widget.clone();
    for _ in 0..MAX_PARENT_DEPTH {
        if let Some(v) = cur.get(key) {
            return Ok(Some(doc.deref(v)?));
        }
        match cur
            .get(b"Parent".as_slice())
            .map(|p| doc.deref(p))
            .transpose()?
        {
            Some(Object::Dict(d)) => cur = d,
            _ => return Ok(None),
        }
    }
    Err(Error::LimitExceeded {
        limit: "form field /Parent depth",
        max: MAX_PARENT_DEPTH as u64,
    })
}

/// A value (`/V` or `/DV`) as display text: strings and text streams;
/// for a choice field each selected export value through `/Opt`.
fn values(
    doc: &Document,
    widget: &Dict,
    v: &Object,
    choice: bool,
    budget: &mut Budget,
) -> Result<Vec<String>, Error> {
    let raw: Vec<String> = match v {
        Object::Array(a) => {
            let mut out = Vec::new();
            for o in a.iter().take(MAX_OPTIONS) {
                if let Some(t) = text_value(doc, o, budget)? {
                    out.push(t);
                }
            }
            out
        }
        o => text_value(doc, o, budget)?.into_iter().collect(),
    };
    if !choice {
        return Ok(raw);
    }
    // §12.7.4.4: /Opt entries are display strings or [export display].
    let opts: Vec<(String, String)> = match inherited(doc, widget, b"Opt")? {
        Some(Object::Array(a)) => a
            .iter()
            .take(MAX_OPTIONS)
            .filter_map(|o| match doc.deref(o).ok()? {
                Object::String(s) => {
                    let t = text_string(&s);
                    Some((t.clone(), t))
                }
                Object::Array(pair) => {
                    let e = pair.first().and_then(|o| o.as_string()).map(text_string)?;
                    let d = pair
                        .get(1)
                        .and_then(|o| o.as_string())
                        .map(text_string)
                        .unwrap_or_else(|| e.clone());
                    Some((e, d))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(raw
        .into_iter()
        .map(|v| {
            opts.iter()
                .find(|(e, _)| *e == v)
                .map(|(_, d)| d.clone())
                .unwrap_or(v)
        })
        .collect())
}

/// The annotations of `page` (module documentation). `appearance`:
/// per `/Annots` index the characters its appearance drew; `hidden`:
/// the indices not drawn because hidden. `used` is the page content's
/// share of the Unicode budget.
pub(crate) fn collect(
    doc: &Document,
    page: &crate::page::Page,
    mut appearance: Vec<(usize, Vec<CharInfo>)>,
    hidden: &[usize],
    used: u64,
    max: u64,
) -> Result<Vec<AnnotationText>, Error> {
    let mut budget = Budget { used, max };
    let annots = match crate::render::page_annots(doc, page)? {
        Some(a) => a,
        None => return Ok(Vec::new()),
    };
    let mut out: Vec<AnnotationText> = Vec::new();
    let mut seen_annots: Vec<IndirectRef> = Vec::new();
    let mut seen_fields: Vec<IndirectRef> = Vec::new();
    for (index, entry) in annots.iter().enumerate() {
        let entry_ref = entry.as_ref();
        if let Some(r) = entry_ref {
            if seen_annots.contains(&r) {
                continue;
            }
            seen_annots.push(r);
        }
        let Object::Dict(ad) = doc.deref(entry)? else {
            // Null or not a dictionary: the renderer's rules decided
            // (refused, or skipped in recovery); nothing to report.
            continue;
        };
        let subtype = ad
            .get(b"Subtype".as_slice())
            .and_then(|o| o.as_name())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        if subtype == "Popup" {
            // A popup shows its parent's /Contents: reported there.
            continue;
        }
        let rect = ad
            .get(b"Rect".as_slice())
            .map(|o| doc.deref(o))
            .transpose()?
            .and_then(|o| Rect::from_object(&o))
            .map(|r| Rect {
                llx: r.llx.min(r.urx),
                lly: r.lly.min(r.ury),
                urx: r.llx.max(r.urx),
                ury: r.lly.max(r.ury),
            })
            .unwrap_or(Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 0.0,
                ury: 0.0,
            });
        let contents = match ad.get(b"Contents".as_slice()) {
            Some(o) => text_value(doc, o, &mut budget)?,
            None => None,
        };
        let rich_text = match ad.get(b"RC".as_slice()) {
            Some(o) => text_value(doc, o, &mut budget)?,
            None => None,
        };
        let field = if subtype == "Widget" {
            let (field_ref, name) = field_of(doc, &ad, entry_ref, &mut budget)?;
            let first_widget = match field_ref {
                Some(r) if seen_fields.contains(&r) => false,
                Some(r) => {
                    seen_fields.push(r);
                    true
                }
                None => true,
            };
            let field_type = match inherited(doc, &ad, b"FT")? {
                Some(Object::Name(n)) => String::from_utf8_lossy(&n).into_owned(),
                _ => String::new(),
            };
            let textual = field_type == "Tx" || field_type == "Ch";
            let read = |key: &[u8], budget: &mut Budget| -> Result<Vec<String>, Error> {
                if !textual || !first_widget {
                    return Ok(Vec::new());
                }
                match inherited(doc, &ad, key)? {
                    Some(v) => values(doc, &ad, &v, field_type == "Ch", budget),
                    None => Ok(Vec::new()),
                }
            };
            let value = read(b"V", &mut budget)?;
            let default_value = read(b"DV", &mut budget)?;
            Some(FieldText {
                name,
                field_type,
                value,
                default_value,
                first_widget,
            })
        } else {
            None
        };
        let chars = appearance
            .iter_mut()
            .find(|(i, _)| *i == index)
            .map(|(_, c)| std::mem::take(c))
            .unwrap_or_default();
        out.push(AnnotationText {
            index,
            subtype,
            rect,
            hidden: hidden.contains(&index),
            contents,
            rich_text,
            field,
            appearance: chars,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::text_string;

    #[test]
    fn text_strings_decode_per_section_7_9_2_2() {
        assert_eq!(text_string(b"Caf\xe9"), "Café");
        assert_eq!(text_string(b"\x84 \x92 \xa0"), "\u{2014} \u{2122} \u{20AC}");
        assert_eq!(text_string(b"\xad\x7f"), "\u{FFFD}\u{FFFD}");
        assert_eq!(text_string(b"\xfe\xff\x00A\x30\x42"), "Aあ");
        // A language escape inside UTF-16 is not text.
        assert_eq!(text_string(b"\xfe\xff\x00\x1bja\x00\x1b\x30\x42"), "あ");
        assert_eq!(text_string(b"\xef\xbb\xbfh\xc3\xa9"), "hé");
        // An odd trailing byte of UTF-16 is dropped, not a panic.
        assert_eq!(text_string(b"\xfe\xff\x00A\x00"), "A");
    }
}
