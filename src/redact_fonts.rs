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
//! metric-compatible alias such as Arial), and each non-symbolic simple
//! TrueType font naming one (ArialMT …), is replaced, document-wide,
//! by a non-symbolic TrueType font with its bundled substitute face as
//! `/FontFile2`:
//!
//! - `/Encoding` lists every code's glyph name in `/Differences` — the
//!   name this crate draws for it (the font's `/Encoding`, else
//!   StandardEncoding), or the space glyph (`uni0020`, blank) when the
//!   face lacks it — never `.notdef`, which readers draw differently — so a
//!   reader finds each glyph by name → Unicode → the face's (3,1)
//!   cmap, as §9.6.6.4 prescribes for a non-symbolic TrueType font;
//! - `/Widths` gives every code's advance as this crate lays it out
//!   (the font's `/Widths`, `/MissingWidth`, or the Adobe AFM widths),
//!   so positions do not move;
//! - `/ToUnicode` is kept.
//!
//! The glyph shapes become the substitute's everywhere the font is used —
//! the visible change this mode makes. Symbol, ZapfDingbats, symbolic
//! TrueType fonts, other non-embedded fonts and CID fonts are not
//! embedded (and stay refused by `redact`).
//!
//! The rewritten document is read back before it is redacted: every
//! font dictionary written here, whatever its name, must hold exactly
//! the substitute's program, encoding and advances, and map each code
//! as readers do (`check_embedded`). Anything else refuses.

use crate::document::Document;
use crate::edit::Edit;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, Object};
use std::io::Write;

/// The document with each embeddable non-embedded font replaced, and
/// how many were; `None` when there is none. `scripts_go`: the
/// redaction removes every JavaScript action afterwards, so a catalog
/// running scripts is not refused in this intermediate document.
pub(crate) fn embed_substitutes(
    doc: &Document,
    limits: &Limits,
    scripts_go: bool,
) -> Result<Option<(Vec<u8>, u64)>, Error> {
    embed_substitutes_with(doc, limits, scripts_go, &mut |_, _| Ok(()))
}

/// A font dictionary this module wrote, and what it must hold in the
/// rewritten document.
struct Written {
    /// The font dictionary's object, in the input document.
    font: crate::object::IndirectRef,
    /// The substitute: its name and the bytes embedded as `/FontFile2`.
    name: &'static str,
    data: &'static [u8],
    /// Each code's glyph name (`None`: the blank `uni0020`).
    names: Vec<Option<String>>,
    /// Each code's advance.
    widths: Vec<f64>,
}

/// A hook on the edit before it is written.
type BeforeRewrite<'h> = dyn FnMut(&mut Edit, &[Written]) -> Result<(), Error> + 'h;

/// [`embed_substitutes`], with `before_rewrite` run on the edit just
/// before it is written (tests alter the output there).
fn embed_substitutes_with(
    doc: &Document,
    limits: &Limits,
    scripts_go: bool,
    before_rewrite: &mut BeforeRewrite,
) -> Result<Option<(Vec<u8>, u64)>, Error> {
    let mut written: Vec<Written> = Vec::new();
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
            || !matches!(name(b"Subtype"), Some(b"Type1" | b"MMType1" | b"TrueType"))
        {
            continue;
        }
        let owned;
        let type1;
        let (face, names, widths) = if name(b"Subtype") == Some(b"TrueType") {
            let Some(s) = crate::font::truetype_substitute_for_embedding(doc, d, limits)? else {
                continue;
            };
            owned = s;
            (&owned.0, owned.1.clone(), owned.2.clone())
        } else {
            let Some(font) = crate::font::load_type1_dict(doc, d, limits)? else {
                continue;
            };
            type1 = font;
            let Some(s) = type1.substitute_for_embedding() else {
                continue;
            };
            s
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
                // A code the substitute cannot name: its blank space
                // glyph, never `.notdef` — readers draw `.notdef`
                // differently (some fall back to the code's own glyph),
                // a blank glyph alike.
                .map(|n| Object::Name(n.as_deref().unwrap_or("uni0020").as_bytes().to_vec())),
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
        written.push(Written {
            font: r,
            name: face.name,
            data: face.data,
            names,
            widths,
        });
        count += 1;
    }
    let Some(mut e) = edit else {
        return Ok(None);
    };
    before_rewrite(&mut e, &written)?;
    let (bytes, map) = e.rewrite_with_overrides_mapped(&crate::rewrite::RewriteOverrides {
        javascript_removed: scripts_go,
        ..Default::default()
    })?;
    check_embedded(&bytes, limits, &written, &map)?;
    Ok(Some((bytes, count)))
}

/// Every font dictionary written here, whatever its name, is checked
/// in the rewritten document as this crate reads it: a non-embedded-free
/// TrueType font whose `/FontFile2` is exactly the substitute's bytes,
/// with the `/Encoding` and `/Widths` written, each code mapping to the
/// glyph its `uniXXXX` name designates through the face's (3,1) cmap —
/// what readers do for a non-symbolic TrueType font (§9.6.6.4) — so that
/// the redaction's ink decision, made on this document, is about the
/// glyphs readers draw. A font the rewrite did not write (unreachable)
/// is not in the output. Anything else — a font missing, unreadable, of
/// another kind or program, or mapping a code elsewhere — refuses.
fn check_embedded(
    bytes: &[u8],
    limits: &Limits,
    written: &[Written],
    map: &crate::rewrite::ObjectMap,
) -> Result<(), Error> {
    let doc = Document::from_bytes_with_limits(bytes, limits.clone())?;
    let mismatch = |detail: String| Error::Unsupported {
        feature: "embedding a substitute font",
        detail,
    };
    for w in written {
        let Some(&number) = map.get(&(w.font.number, w.font.generation)) else {
            continue;
        };
        let r = crate::object::IndirectRef {
            number,
            generation: 0,
        };
        let bad = |what: &str| mismatch(format!("object {r}: {what}"));
        let obj = doc
            .resolve(r)
            .map_err(|_| bad("unreadable after embedding"))?;
        let d = obj.as_dict().ok_or_else(|| bad("not a font dictionary"))?;
        let name = |k: &[u8]| d.get(k).and_then(|v| v.as_name());
        if name(b"Type") != Some(b"Font")
            || name(b"Subtype") != Some(b"TrueType")
            || name(b"BaseFont") != Some(w.name.as_bytes())
        {
            return Err(bad("not the TrueType font written"));
        }
        let diffs = d
            .get(b"Encoding".as_slice())
            .map(|e| doc.deref(e))
            .transpose()
            .map_err(|_| bad("unreadable encoding"))?
            .and_then(|e| e.as_dict().cloned())
            .and_then(|e| e.get(b"Differences".as_slice()).cloned())
            .and_then(|a| a.as_array().map(|a| a.to_vec()))
            .ok_or_else(|| bad("no /Differences"))?;
        let want_names: Vec<&[u8]> = w
            .names
            .iter()
            .map(|n| n.as_deref().unwrap_or("uni0020").as_bytes())
            .collect();
        if diffs.len() != 257
            || diffs[0].as_i64() != Some(0)
            || diffs[1..]
                .iter()
                .zip(&want_names)
                .any(|(a, b)| a.as_name() != Some(*b))
        {
            return Err(bad("another encoding than the one written"));
        }
        // The program: exactly the substitute's bytes.
        let fd = d
            .get(b"FontDescriptor".as_slice())
            .map(|o| doc.deref(o))
            .transpose()
            .map_err(|_| bad("unreadable font descriptor"))?
            .and_then(|o| o.as_dict().cloned())
            .ok_or_else(|| bad("no font descriptor"))?;
        let program = match fd
            .get(b"FontFile2".as_slice())
            .map(|o| doc.deref(o))
            .transpose()
        {
            Ok(Some(Object::Stream { dict, data })) => doc
                .decode_stream(&dict, &data)
                .map_err(|_| bad("unreadable font program"))?,
            _ => return Err(bad("no font program")),
        };
        if program != w.data {
            return Err(bad("another font program than the substitute"));
        }
        let font = crate::font::load_font_dict(&doc, d, b"", limits)
            .map_err(|_| bad("does not load after embedding"))?;
        let crate::font::Font::Simple(f) = &font else {
            return Err(bad("did not load as a TrueType font"));
        };
        if f.substitution().is_some() {
            return Err(bad("loads as non-embedded"));
        }
        for (code, (name, want_w)) in want_names.iter().zip(&w.widths).enumerate() {
            let want = std::str::from_utf8(name)
                .ok()
                .and_then(|n| n.strip_prefix("uni"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .and_then(|u| f.unicode_glyph(u))
                .unwrap_or(ttf_parser::GlyphId(0));
            let got = f
                .glyph_id(code as u8)
                .map_err(|_| bad("a code that does not resolve"))?;
            if got != Some(want) {
                return Err(bad("a code resolves to another glyph than its name"));
            }
            if (f.width(code as u8) - want_w).abs() > 1e-3 {
                return Err(bad("an advance other than the one written"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page drawing `fonts` (resource name, font dictionary), each
    /// as object 10, 11 …; `orphan`: one more font no page uses.
    fn pdf(fonts: &[(&str, &str)], orphan: Option<&str>) -> Vec<u8> {
        let mut objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        ];
        let res: String = fonts
            .iter()
            .enumerate()
            .map(|(i, (n, _))| format!("/{n} {} 0 R ", 10 + i))
            .collect();
        objs.push((
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
                 /Resources << /Font << {res}>> >> /Contents 4 0 R >>"
            ),
        ));
        let content: String = fonts
            .iter()
            .enumerate()
            .map(|(i, (n, _))| format!("BT /{n} 12 Tf 10 {} Td (ABC xyz) Tj ET ", 20 + 20 * i))
            .collect();
        objs.push((
            4,
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ));
        for (i, (_, d)) in fonts.iter().enumerate() {
            objs.push((10 + i as u32, d.to_string()));
        }
        if let Some(d) = orphan {
            objs.push((30, d.to_string()));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        for (n, o) in &objs {
            offs.push((*n, out.len()));
            out.extend_from_slice(format!("{n} 0 obj\n{o}\nendobj\n").as_bytes());
        }
        let size = objs.iter().map(|(n, _)| *n).max().unwrap() + 1;
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for n in 1..size {
            match offs.iter().find(|(m, _)| *m == n) {
                Some((_, o)) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
        );
        out
    }

    const TYPE1: &str =
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>";
    const TRUETYPE: &str = "<< /Type /Font /Subtype /TrueType /BaseFont /ArialMT \
        /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 32 /Widths [278] \
        /FontDescriptor << /Type /FontDescriptor /FontName /ArialMT /Flags 32 \
        /FontWeight 700 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
        /CapHeight 700 /StemV 80 >> >>";

    fn doc() -> Document {
        Document::from_bytes(&pdf(&[("C", TYPE1), ("A", TRUETYPE)], None)).unwrap()
    }

    /// Embed, altering the edit first; the error's text when refused.
    fn tampered(alter: &mut dyn FnMut(&mut Edit, &[Written])) -> Result<u64, String> {
        let d = doc();
        embed_substitutes_with(&d, &Limits::default(), false, &mut |e, w| {
            alter(e, w);
            Ok(())
        })
        .map(|r| r.unwrap().1)
        .map_err(|e| e.to_string())
    }

    fn font_dict<'e>(e: &'e mut Edit, w: &Written) -> &'e mut Dict {
        match e.overlay.get_mut(&(w.font.number, w.font.generation)) {
            Some(Object::Dict(d)) => d,
            _ => panic!("the written font is in the overlay"),
        }
    }

    fn program_ref(e: &mut Edit, w: &Written) -> crate::object::IndirectRef {
        let fd = match font_dict(e, w).get(b"FontDescriptor".as_slice()) {
            Some(Object::Ref(r)) => *r,
            _ => panic!("descriptor"),
        };
        match e.overlay.get(&(fd.number, fd.generation)) {
            Some(Object::Dict(d)) => match d.get(b"FontFile2".as_slice()) {
                Some(Object::Ref(r)) => *r,
                _ => panic!("program"),
            },
            _ => panic!("descriptor dict"),
        }
    }

    #[test]
    fn both_paths_embed_and_check() {
        assert_eq!(tampered(&mut |_, _| {}), Ok(2));
        // The TrueType font is drawn bold (its descriptor): the embedded
        // face is the drawn one, never one picked otherwise.
        let mut names = Vec::new();
        tampered(&mut |_, w| names.extend(w.iter().map(|w| w.name))).unwrap();
        names.sort();
        assert_eq!(names, ["KasaneCourier-Regular", "LiberationSans-Bold"]);
    }

    #[test]
    fn a_font_no_page_uses_is_not_refused() {
        let d = Document::from_bytes(&pdf(&[("C", TYPE1)], Some(TYPE1))).unwrap();
        assert!(embed_substitutes(&d, &Limits::default(), false)
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_renamed_font_is_checked_all_the_same() {
        // Any name — the check is of what was written, not of names.
        for base in [&b"Renamed"[..], b"Remapped"] {
            let e = tampered(&mut |e, w| {
                let d = font_dict(e, &w[0]);
                if base == b"Renamed" {
                    d.insert(b"BaseFont".to_vec(), Object::Name(base.to_vec()));
                } else {
                    // Named right, mapping a code elsewhere.
                    let Some(Object::Dict(enc)) = d.get_mut(b"Encoding".as_slice()) else {
                        panic!("encoding")
                    };
                    let Some(Object::Array(a)) = enc.get_mut(b"Differences".as_slice()) else {
                        panic!("differences")
                    };
                    a[66] = Object::Name(b"uni0058".to_vec());
                }
            })
            .unwrap_err();
            assert!(e.contains("embedding a substitute font"), "{e}");
        }
    }

    #[test]
    fn another_program_than_the_substitute_is_refused() {
        let e = tampered(&mut |e, w| {
            let r = program_ref(e, &w[0]);
            let other = w.iter().find(|x| x.data != w[0].data).unwrap().data;
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(other).unwrap();
            let data = z.finish().unwrap();
            let mut sd = Dict::new();
            sd.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
            sd.insert(b"Length".to_vec(), Object::Integer(data.len() as i64));
            e.overlay
                .insert((r.number, r.generation), Object::Stream { dict: sd, data });
        })
        .unwrap_err();
        assert!(e.contains("another font program"), "{e}");
        // A program that does not decode.
        let e = tampered(&mut |e, w| {
            let r = program_ref(e, &w[0]);
            let mut sd = Dict::new();
            sd.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
            sd.insert(b"Length".to_vec(), Object::Integer(4));
            e.overlay.insert(
                (r.number, r.generation),
                Object::Stream {
                    dict: sd,
                    data: b"\x00\x01\x02\x03".to_vec(),
                },
            );
        })
        .unwrap_err();
        assert!(e.contains("embedding a substitute font"), "{e}");
    }

    #[test]
    fn a_font_unreadable_after_embedding_is_refused() {
        for what in 0..3 {
            let e = tampered(&mut |e, w| {
                let r = (w[0].font.number, w[0].font.generation);
                match what {
                    0 => {
                        e.overlay.insert(r, Object::Integer(7));
                    }
                    1 => {
                        font_dict(e, &w[0]).remove(b"FontDescriptor".as_slice());
                    }
                    _ => {
                        font_dict(e, &w[0])
                            .insert(b"Subtype".to_vec(), Object::Name(b"Type1".to_vec()));
                    }
                }
            })
            .unwrap_err();
            assert!(e.contains("embedding a substitute font"), "{what}: {e}");
        }
    }

    #[test]
    fn another_advance_is_refused() {
        let e = tampered(&mut |e, w| {
            let Some(Object::Array(a)) = font_dict(e, &w[0]).get_mut(b"Widths".as_slice()) else {
                panic!("widths")
            };
            a[65] = Object::Real(500.0);
        })
        .unwrap_err();
        assert!(e.contains("advance"), "{e}");
    }

    #[test]
    fn the_refusal_names_no_document_string() {
        let e = tampered(&mut |e, w| {
            font_dict(e, &w[0]).insert(b"BaseFont".to_vec(), Object::Name(b"QZXWVKQZ".to_vec()));
        })
        .unwrap_err();
        assert!(!e.contains("QZXWVKQZ"), "{e}");
    }
}
