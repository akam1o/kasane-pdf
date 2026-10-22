//! Font usage audit — for each corpus PDF, enumerate the `/Font`
//! resources and which are actually selected by `Tf` on a page,
//! split by subtype, embedding, encoding, and ToUnicode. Drives the
//! Type1/Type3 implementation-priority decision.

use kasane_pdf::{Document, Object, Operation};
use std::collections::{BTreeMap, BTreeSet};

/// One font's salient properties for bucketing.
#[derive(Debug, Default)]
pub struct FontInfo {
    pub subtype: String,
    pub base_font: String,
    /// `/FontFile`/`/FontFile2`/`/FontFile3` present → embedded.
    pub embedded: bool,
    /// has a `/FontDescriptor` (needed for embedded/widths).
    pub descriptor: bool,
    /// `/Encoding` present (name or dict).
    pub encoding: String,
    /// `/ToUnicode` present.
    pub to_unicode: bool,
    /// `/Widths` present (simple fonts).
    pub widths: bool,
    /// actually selected by a `Tf` op on some page.
    pub used: bool,
    /// `/CharProcs` present (Type3).
    pub charprocs: bool,
}

/// Per-file audit.
#[derive(Debug, Default)]
pub struct FontAudit {
    /// resource name → info (dedup by name).
    pub fonts: BTreeMap<String, FontInfo>,
    /// font names actually `Tf`-invoked somewhere.
    pub used_names: BTreeSet<String>,
}

/// Audit one document.
pub fn audit(doc: &Document) -> FontAudit {
    let mut a = FontAudit::default();
    let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
    for page in doc.pages() {
        let resources = page.resources().cloned().unwrap_or(Object::Null);
        // Which font names does this page's content invoke?
        if let Ok(ops) = doc.page_operations(page) {
            collect_tf(&ops, &resources, doc, &mut a, &mut seen);
        }
        // Enumerate every /Font entry (referenced or not).
        if let Ok(Object::Dict(res)) = doc.deref(&resources) {
            if let Some(fcat) = res.get(b"Font".as_slice()) {
                if let Ok(Object::Dict(fd)) = doc.deref(fcat) {
                    for (name, entry) in fd {
                        if seen.insert(name.clone()) {
                            if let Ok(o) = doc.deref(&entry) {
                                if let Some(d) = o.as_dict() {
                                    a.fonts
                                        .entry(lossy(&name))
                                        .or_insert(font_info(doc, d));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for n in &a.used_names {
        if let Some(f) = a.fonts.get_mut(n) {
            f.used = true;
        }
    }
    a
}

fn collect_tf(
    ops: &[Operation],
    resources: &Object,
    doc: &Document,
    a: &mut FontAudit,
    seen: &mut BTreeSet<Vec<u8>>,
) {
    for op in ops {
        match op {
            Operation::SetTextFont { font, .. } => {
                a.used_names.insert(lossy(font));
                if seen.insert(font.clone()) {
                    if let Ok(Some(o)) = doc.resolve_resource(resources, "Font", font) {
                        if let Some(d) = o.as_dict() {
                            a.fonts.entry(lossy(font)).or_insert(font_info(doc, d));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn font_info(doc: &Document, d: &kasane_pdf::Dict) -> FontInfo {
    let mut f = FontInfo::default();
    f.subtype = d
        .get(b"Subtype".as_slice())
        .and_then(|o| o.as_name())
        .map(lossy)
        .unwrap_or_else(|| "?".into());
    f.base_font = d
        .get(b"BaseFont".as_slice())
        .and_then(|o| o.as_name())
        .map(lossy)
        .unwrap_or_default();
    f.to_unicode = d.contains_key(b"ToUnicode".as_slice());
    f.widths = d.contains_key(b"Widths".as_slice());
    f.charprocs = d.contains_key(b"CharProcs".as_slice());
    f.encoding = match d.get(b"Encoding".as_slice()) {
        Some(o) => match doc.deref(o) {
            Ok(Object::Name(n)) => format!("/{}", lossy(&n)),
            Ok(Object::Dict(_)) => "<dict>".into(),
            _ => "<?>".into(),
        },
        None => String::new(),
    };
    // descriptor → embedded font-file
    if let Some(desc) = d.get(b"FontDescriptor".as_slice()) {
        f.descriptor = true;
        if let Ok(dd) = doc.deref(desc) {
            if let Some(dsc) = dd.as_dict() {
                f.embedded = dsc.contains_key(b"FontFile".as_slice())
                    || dsc.contains_key(b"FontFile2".as_slice())
                    || dsc.contains_key(b"FontFile3".as_slice());
            }
        }
    }
    // Type0 descendant subtype matters for CFF vs TrueType.
    if f.subtype == "Type0" {
        if let Some(df) = d.get(b"DescendantFonts".as_slice()) {
            if let Ok(arr) = doc.deref(df) {
                if let Some(a0) = arr.as_array().and_then(|a| a.first()) {
                    if let Ok(dd) = doc.deref(a0) {
                        if let Some(ddf) = dd.as_dict() {
                            let st = ddf
                                .get(b"Subtype".as_slice())
                                .and_then(|o| o.as_name())
                                .map(lossy)
                                .unwrap_or_default();
                            f.subtype = format!("Type0/{st}");
                            f.embedded = ddf.contains_key(b"FontFile3".as_slice())
                                || ddf.contains_key(b"FontFile2".as_slice())
                                || ddf.contains_key(b"FontFile".as_slice());
                        }
                    }
                }
            }
        }
    }
    f
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}
