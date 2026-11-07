//! Embedding the substitutes of non-embedded fonts before redaction
//! (`RedactOptions::embed_substitute_fonts`).
//!
//! A non-embedded font is drawn by each reader with a font of its own
//! choosing, so where its glyphs reach is not known and `redact`
//! refuses it. Embedding the face this crate substitutes makes the
//! document say exactly what is drawn: every reader must use an
//! embedded program, so the glyphs' extent is the embedded outlines'
//! and redaction can decide by ink as for any embedded font.
//!
//! Each non-embedded Latin Standard 14 Type 1 font (or a
//! metric-compatible alias such as Arial) is replaced, document-wide,
//! by a non-symbolic TrueType font with the bundled Liberation face as
//! `/FontFile2`:
//!
//! - `/Encoding` lists every code's glyph name in `/Differences` — the
//!   name this crate draws for it (the font's `/Encoding`, else
//!   StandardEncoding), or `.notdef` when the face lacks it — so a
//!   reader finds each glyph by name → Unicode → the face's (3,1)
//!   cmap, as §9.6.6.4 prescribes for a non-symbolic TrueType font;
//! - `/Widths` gives every code's advance as this crate lays it out
//!   (the font's `/Widths`, `/MissingWidth`, or the Adobe AFM widths),
//!   so positions do not move;
//! - `/ToUnicode` is kept.
//!
//! The glyph shapes become Liberation's everywhere the font is used —
//! the visible change this mode makes. Symbol, ZapfDingbats, other
//! non-embedded fonts and CID fonts are not embedded (and stay
//! refused by `redact`).

use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use std::io::Write;

/// The document with each embeddable non-embedded font replaced, and
/// how many were; `None` when there is none.
pub(crate) fn embed_substitutes(
    doc: &Document,
    limits: &Limits,
) -> Result<Option<(Vec<u8>, u64)>, Error> {
    let refs: Vec<_> = doc
        .xref_entries()
        .filter(|(_, e)| !matches!(e, crate::xref::XrefEntry::Free { .. }))
        .map(|(r, _)| r)
        .collect();
    let mut edit: Option<Edit> = None;
    let mut programs: std::collections::HashMap<&'static str, crate::object::IndirectRef> =
        std::collections::HashMap::new();
    let mut count = 0u64;
    for r in refs {
        let Ok(obj) = doc.resolve(r) else { continue };
        let Some(d) = obj.as_dict() else { continue };
        let name = |k: &[u8]| d.get(k).and_then(|v| v.as_name());
        if name(b"Type") != Some(b"Font")
            || !matches!(name(b"Subtype"), Some(b"Type1" | b"MMType1"))
        {
            continue;
        }
        let Some(font) = crate::font::load_type1_dict(doc, d, limits)? else {
            continue;
        };
        let Some((face, names, widths)) = font.substitute_for_embedding() else {
            continue;
        };
        let e = match &mut edit {
            Some(e) => e,
            None => edit.insert(Edit::new(doc, limits.clone())?),
        };
        let file = match programs.get(face.name) {
            Some(f) => *f,
            None => {
                let mut z =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(face.data).map_err(|err| {
                    Error::Structure(format!("compressing a font program: {err}"))
                })?;
                let data = z.finish().map_err(|err| {
                    Error::Structure(format!("compressing a font program: {err}"))
                })?;
                let mut sd = Dict::new();
                sd.insert(b"Length1".to_vec(), Object::Integer(face.data.len() as i64));
                sd.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
                sd.insert(b"Length".to_vec(), Object::Integer(data.len() as i64));
                let fr = e.alloc()?;
                e.overlay.insert(
                    (fr.number, fr.generation),
                    Object::Stream { dict: sd, data },
                );
                programs.insert(face.name, fr);
                fr
            }
        };
        let f = face.face();
        let upem = f.units_per_em() as f64;
        let k = 1000.0 / if upem > 0.0 { upem } else { 1000.0 };
        let bb = f.global_bounding_box();
        let base = Object::Name(face.name.as_bytes().to_vec());
        let mut fd = Dict::new();
        fd.insert(b"Type".to_vec(), Object::Name(b"FontDescriptor".to_vec()));
        fd.insert(b"FontName".to_vec(), base.clone());
        // Nonsymbolic (bit 6): glyphs are found by name (§9.6.6.4).
        fd.insert(b"Flags".to_vec(), Object::Integer(32));
        fd.insert(
            b"FontBBox".to_vec(),
            Object::Array(
                [bb.x_min, bb.y_min, bb.x_max, bb.y_max]
                    .iter()
                    .map(|v| Object::Integer((*v as f64 * k).round() as i64))
                    .collect(),
            ),
        );
        fd.insert(
            b"ItalicAngle".to_vec(),
            Object::Real(f.italic_angle() as f64),
        );
        fd.insert(
            b"Ascent".to_vec(),
            Object::Integer((f.ascender() as f64 * k).round() as i64),
        );
        fd.insert(
            b"Descent".to_vec(),
            Object::Integer((f.descender() as f64 * k).round() as i64),
        );
        fd.insert(
            b"CapHeight".to_vec(),
            Object::Integer((f.capital_height().unwrap_or(f.ascender()) as f64 * k).round() as i64),
        );
        fd.insert(b"StemV".to_vec(), Object::Integer(80));
        fd.insert(b"FontFile2".to_vec(), Object::Ref(file));
        let fdr = e.alloc()?;
        e.overlay
            .insert((fdr.number, fdr.generation), Object::Dict(fd));

        let mut diffs = vec![Object::Integer(0)];
        diffs.extend(
            names
                .iter()
                .map(|n| Object::Name(n.as_deref().unwrap_or(".notdef").as_bytes().to_vec())),
        );
        let mut enc = Dict::new();
        enc.insert(b"Type".to_vec(), Object::Name(b"Encoding".to_vec()));
        enc.insert(b"Differences".to_vec(), Object::Array(diffs));
        let mut nd = Dict::new();
        nd.insert(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        nd.insert(b"Subtype".to_vec(), Object::Name(b"TrueType".to_vec()));
        nd.insert(b"BaseFont".to_vec(), base);
        nd.insert(b"FirstChar".to_vec(), Object::Integer(0));
        nd.insert(b"LastChar".to_vec(), Object::Integer(255));
        nd.insert(
            b"Widths".to_vec(),
            Object::Array(widths.iter().map(|w| Object::Real(*w)).collect()),
        );
        nd.insert(b"Encoding".to_vec(), Object::Dict(enc));
        nd.insert(b"FontDescriptor".to_vec(), Object::Ref(fdr));
        if let Some(tu) = d.get(b"ToUnicode".as_slice()) {
            nd.insert(b"ToUnicode".to_vec(), tu.clone());
        }
        e.overlay.insert((r.number, r.generation), Object::Dict(nd));
        count += 1;
    }
    match edit {
        None => Ok(None),
        Some(e) => Ok(Some((e.rewrite()?, count))),
    }
}
