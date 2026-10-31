//! Measured content profile of a document, for the per-category
//! quality tables (`scripts/quality-by-category.py`). Every field is
//! read from the file or from our own extraction — nothing is inferred
//! from file names or sources; what cannot be measured stays empty and
//! is reported as *unknown*.

use kasane_pdf::{Dict, Document, Object};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Font kinds reachable from the page resources (and their forms):
    /// `Type1-eexec`, `Type1-CFF`, `Type1-OpenType`, `TrueType`,
    /// `Type3`, `CIDFontType0`, `CIDFontType2`, each suffixed
    /// `-nonembedded` when the program is absent.
    pub font_kinds: Vec<String>,
    /// The image compression filters of image XObjects (the
    /// image-specific filter when one is present, else the last filter;
    /// `none` for unfiltered).
    pub image_filters: Vec<String>,
    /// A `/Shading` resource or a shading pattern is present.
    pub shading: bool,
    /// The document is encrypted (`Document::encryption`).
    pub encrypted: bool,
    /// Whether page 1 has extractable text at all (`None`: not run).
    pub text_layer_page1: Option<bool>,
    /// Scripts of the Unicode text we extracted (≥ 5 characters each):
    /// `Latin`, `Greek`, `Cyrillic`, `Arabic`, `Hebrew`, `Han`, `Kana`,
    /// `Hangul`, `Thai`, `Devanagari`. Empty: no resolved text.
    pub scripts: Vec<String>,
}

fn program_kind(doc: &Document, fd: Option<&Object>) -> Option<&'static str> {
    let fd = fd.and_then(|o| doc.deref(o).ok())?;
    let fd = fd.as_dict()?;
    if fd.contains_key(b"FontFile".as_slice()) {
        return Some("eexec");
    }
    if fd.contains_key(b"FontFile2".as_slice()) {
        return Some("TrueType");
    }
    let ff3 = doc.deref(fd.get(b"FontFile3".as_slice())?).ok()?;
    let sub = ff3.as_stream()?.0.get(b"Subtype".as_slice())?.as_name()?;
    Some(if sub == b"OpenType" {
        "OpenType"
    } else {
        "CFF"
    })
}

fn font_kind(doc: &Document, f: &Dict) -> String {
    let sub = f
        .get(b"Subtype".as_slice())
        .and_then(|o| o.as_name())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_else(|| "unknown".into());
    match sub.as_str() {
        "Type3" => "Type3".into(),
        "Type0" => {
            let desc = f
                .get(b"DescendantFonts".as_slice())
                .and_then(|o| doc.deref(o).ok())
                .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
                .and_then(|o| doc.deref(&o).ok());
            let Some(Object::Dict(d)) = desc else {
                return "Type0-unknown".into();
            };
            let ds = d
                .get(b"Subtype".as_slice())
                .and_then(|o| o.as_name())
                .map(|n| String::from_utf8_lossy(n).into_owned())
                .unwrap_or_else(|| "unknown".into());
            match program_kind(doc, d.get(b"FontDescriptor".as_slice())) {
                Some(_) => ds,
                None => format!("{ds}-nonembedded"),
            }
        }
        "TrueType" => match program_kind(doc, f.get(b"FontDescriptor".as_slice())) {
            Some(_) => "TrueType".into(),
            None => "TrueType-nonembedded".into(),
        },
        "Type1" | "MMType1" => match program_kind(doc, f.get(b"FontDescriptor".as_slice())) {
            Some("eexec") => "Type1-eexec".into(),
            Some("OpenType") => "Type1-OpenType".into(),
            Some(_) => "Type1-CFF".into(),
            None => "Type1-nonembedded".into(),
        },
        other => format!("other-{other}"),
    }
}

const IMAGE_FILTERS: [&str; 4] = ["DCTDecode", "JPXDecode", "JBIG2Decode", "CCITTFaxDecode"];

fn image_filter(d: &Dict) -> String {
    let names: Vec<String> = match d.get(b"Filter".as_slice()) {
        Some(Object::Name(n)) => vec![String::from_utf8_lossy(n).into_owned()],
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect(),
        _ => Vec::new(),
    };
    names
        .iter()
        .find(|n| IMAGE_FILTERS.contains(&n.as_str()))
        .or(names.last())
        .cloned()
        .unwrap_or_else(|| "none".into())
}

struct Walk<'a> {
    doc: &'a Document,
    seen: BTreeSet<String>,
    budget: usize,
    fonts: BTreeSet<String>,
    images: BTreeSet<String>,
    shading: bool,
}

impl Walk<'_> {
    fn resources(&mut self, res: &Object, depth: u32) {
        if depth > 6 || self.budget == 0 {
            return;
        }
        self.budget -= 1;
        let Ok(res) = self.doc.deref(res) else { return };
        let Some(rd) = res.as_dict() else { return };
        let doc = self.doc;
        let entries = |key: &[u8]| -> Vec<Object> {
            rd.get(key)
                .and_then(|o| doc.deref(o).ok())
                .and_then(|o| o.as_dict().map(|d| d.values().cloned().collect()))
                .unwrap_or_default()
        };
        if !entries(b"Shading").is_empty() {
            self.shading = true;
        }
        for p in entries(b"Pattern") {
            if let Ok(o) = doc.deref(&p) {
                let d = o
                    .as_stream()
                    .map(|s| s.0.clone())
                    .or_else(|| o.as_dict().cloned());
                if let Some(d) = d {
                    if d.get(b"PatternType".as_slice()).and_then(|o| o.as_i64()) == Some(2) {
                        self.shading = true;
                    } else if let Some(r) = d.get(b"Resources".as_slice()) {
                        self.resources(r, depth + 1);
                    }
                }
            }
        }
        for f in entries(b"Font") {
            let key = format!("f{f:?}");
            if self.seen.insert(key) {
                if let Ok(Object::Dict(fd)) = doc.deref(&f) {
                    self.fonts.insert(font_kind(doc, &fd));
                }
            }
        }
        for x in entries(b"XObject") {
            let key = format!("x{x:?}");
            if !self.seen.insert(key) {
                continue;
            }
            let Ok(o) = doc.deref(&x) else { continue };
            let Some((d, _)) = o.as_stream() else {
                continue;
            };
            match d.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
                Some(b"Image") => {
                    if d.get(b"ImageMask".as_slice()).and_then(|o| o.as_bool()) != Some(true) {
                        self.images.insert(image_filter(d));
                    }
                }
                Some(b"Form") => {
                    if let Some(r) = d.get(b"Resources".as_slice()) {
                        self.resources(r, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The structural part of the profile (fonts, images, shading,
/// encryption), bounded to the first `max_pages` pages.
pub fn structural(doc: &Document, max_pages: usize) -> Profile {
    let mut w = Walk {
        doc,
        seen: BTreeSet::new(),
        budget: 4000,
        fonts: BTreeSet::new(),
        images: BTreeSet::new(),
        shading: false,
    };
    for page in doc.pages().iter().take(max_pages) {
        if let Some(r) = page.resources() {
            w.resources(r, 0);
        }
    }
    Profile {
        font_kinds: w.fonts.into_iter().collect(),
        image_filters: w.images.into_iter().collect(),
        shading: w.shading,
        encrypted: doc.encryption().is_some(),
        text_layer_page1: None,
        scripts: Vec::new(),
    }
}

/// Scripts with at least five characters in `text`.
pub fn scripts(text: &str) -> Vec<String> {
    let mut n = std::collections::BTreeMap::<&str, usize>::new();
    for c in text.chars() {
        let u = c as u32;
        let s = match u {
            0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x1E00..=0x1EFF => "Latin",
            0x370..=0x3FF => "Greek",
            0x400..=0x4FF => "Cyrillic",
            0x590..=0x5FF => "Hebrew",
            0x600..=0x6FF | 0x750..=0x77F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => "Arabic",
            0x900..=0x97F => "Devanagari",
            0xE00..=0xE7F => "Thai",
            0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => "Kana",
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F => "Han",
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => "Hangul",
            _ => continue,
        };
        *n.entry(s).or_default() += 1;
    }
    n.into_iter()
        .filter(|(_, k)| *k >= 5)
        .map(|(s, _)| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn scripts_need_five_characters() {
        assert_eq!(
            super::scripts("Hello 日本語のテキスト"),
            vec!["Kana", "Latin"]
        );
        assert!(super::scripts("ab").is_empty());
    }
}
