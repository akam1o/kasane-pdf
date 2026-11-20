//! Non-embedded Courier and Courier New drawn with Kasane Courier
//! (`fonts/kasane-courier/`): which face, which glyphs, where. Every
//! input is synthetic.
//!
//! A glyph's extracted `bounds` are its outline's: matching Kasane
//! Courier's glyph box (and not Liberation Mono's) shows the face drawn;
//! the origins and advances show the layout, which comes from the Adobe
//! AFM widths (600) or, for a name they lack, Liberation Mono's advance.

use kasane_pdf::{extract_text, render_page, CharInfo, SubstKind, TextOptions};
use kasane_pdf::{Document, RenderOptions};

const KC: [(&str, &[u8]); 4] = [
    (
        "KasaneCourier-Regular",
        include_bytes!("../fonts/kasane-courier/KasaneCourier-Regular.ttf"),
    ),
    (
        "KasaneCourier-Bold",
        include_bytes!("../fonts/kasane-courier/KasaneCourier-Bold.ttf"),
    ),
    (
        "KasaneCourier-Italic",
        include_bytes!("../fonts/kasane-courier/KasaneCourier-Italic.ttf"),
    ),
    (
        "KasaneCourier-BoldItalic",
        include_bytes!("../fonts/kasane-courier/KasaneCourier-BoldItalic.ttf"),
    ),
];
const LM_REGULAR: &[u8] = include_bytes!("../fonts/liberation/LiberationMono-Regular.ttf");

/// A one-page PDF: `fonts` are objects 5, 6 … named F1, F2 …
fn pdf(fonts: &[String], content: &str) -> Vec<u8> {
    let res: String = (0..fonts.len())
        .map(|i| format!("/F{} {} 0 R ", i + 1, 5 + i))
        .collect();
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /Font << {res}>> >> /Contents 4 0 R >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    objs.extend(fonts.iter().cloned());
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn type1(base: &str, enc: &str) -> String {
    format!("<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding {enc} >>")
}

fn chars(pdf: &[u8]) -> Vec<CharInfo> {
    let doc = Document::from_bytes(pdf).unwrap();
    extract_text(&doc, &doc.pages()[0], &TextOptions::default())
        .unwrap()
        .chars
}

fn substitutes(pdf: &[u8]) -> Vec<(String, &'static str, SubstKind)> {
    let doc = Document::from_bytes(pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let mut v: Vec<_> = r
        .font_substitutions()
        .iter()
        .map(|s| (s.base_font.clone(), s.substitute, s.kind))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Dark pixels inside `b` (user space, 72 dpi, page height 300).
fn ink_in(pdf: &[u8], b: (f64, f64, f64, f64)) -> usize {
    let doc = Document::from_bytes(pdf).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
    let p = r.pixmap();
    let mut n = 0;
    let (x0, x1) = (
        b.0.floor().max(0.0) as u32,
        (b.2.ceil() as u32).min(p.width()),
    );
    let (y0, y1) = (
        (300.0 - b.3).floor().max(0.0) as u32,
        ((300.0 - b.1).ceil() as u32).min(p.height()),
    );
    for y in y0..y1 {
        for x in x0..x1 {
            if p.pixel(x, y).unwrap().red() < 160 {
                n += 1;
            }
        }
    }
    n
}

/// The glyph box of `c` in `face` at `size`, placed at `origin`.
fn face_box(face: &[u8], c: char, size: f64, origin: (f64, f64)) -> Option<(f64, f64, f64, f64)> {
    let f = ttf_parser::Face::parse(face, 0).unwrap();
    let r = f.glyph_bounding_box(f.glyph_index(c)?)?;
    let k = size / f.units_per_em() as f64;
    Some((
        origin.0 + r.x_min as f64 * k,
        origin.1 + r.y_min as f64 * k,
        origin.0 + r.x_max as f64 * k,
        origin.1 + r.y_max as f64 * k,
    ))
}

fn near(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64), tol: f64) -> bool {
    (a.0 - b.0).abs() <= tol
        && (a.1 - b.1).abs() <= tol
        && (a.2 - b.2).abs() <= tol
        && (a.3 - b.3).abs() <= tol
}

/// Each drawn character's bounds are Kasane Courier's glyph box.
fn assert_drawn_by(face: &[u8], cs: &[CharInfo], size: f64, what: &str) {
    let mut n = 0;
    for c in cs {
        let Some(u) = c.unicode.as_deref().and_then(|s| s.chars().next()) else {
            continue;
        };
        let Some(want) = face_box(face, u, size, c.origin) else {
            assert!(
                c.bounds.is_none(),
                "{what}: {u:?} has ink where the face has none"
            );
            continue;
        };
        let got = c
            .bounds
            .unwrap_or_else(|| panic!("{what}: {u:?} not drawn"));
        assert!(
            near(got, want, 0.02 * size),
            "{what}: {u:?} {got:?} != {want:?}"
        );
        n += 1;
    }
    assert!(n > 0, "{what}: nothing drawn");
}

// 1–4: the four Courier styles, each with its own face.
#[test]
fn the_four_courier_styles() {
    for (i, base) in [
        "Courier",
        "Courier-Bold",
        "Courier-Oblique",
        "Courier-BoldOblique",
    ]
    .iter()
    .enumerate()
    {
        let p = pdf(
            &[type1(base, "/WinAnsiEncoding")],
            "BT /F1 12 Tf 20 200 Td (Hxga Qy&) Tj ET",
        );
        assert_eq!(
            substitutes(&p),
            [(base.to_string(), KC[i].0, SubstKind::Standard14)],
            "{base}"
        );
        let cs = chars(&p);
        assert_drawn_by(KC[i].1, &cs, 12.0, base);
        let x = cs
            .iter()
            .find(|c| c.unicode.as_deref() == Some("x"))
            .unwrap();
        assert!(ink_in(&p, x.bounds.unwrap()) > 3, "{base}: x not inked");
    }
    // The shape is Kasane Courier's, not Liberation Mono's: their x
    // heights differ by more than the tolerance used above.
    let k = face_box(KC[0].1, 'x', 12.0, (0.0, 0.0)).unwrap();
    let l = face_box(LM_REGULAR, 'x', 12.0, (0.0, 0.0)).unwrap();
    assert!((k.3 - l.3).abs() > 0.5, "{k:?} {l:?}");
}

// 5–7: ASCII, digits, symbols: every printable character drawn, 600 wide.
#[test]
fn ascii_digits_symbols() {
    let all: String = (0x21u8..0x7f).map(|c| c as char).collect();
    let esc = all
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)");
    let p = pdf(
        &[type1("Courier", "/WinAnsiEncoding")],
        &format!("BT /F1 10 Tf 10 200 Td ({esc}) Tj ET"),
    );
    let cs = chars(&p);
    assert_eq!(cs.len(), 94);
    for c in &cs {
        assert!(c.bounds.is_some(), "{:?}", c.unicode);
        assert!(
            (c.advance.0 - 6.0).abs() < 1e-9,
            "{:?}: {:?}",
            c.unicode,
            c.advance
        );
    }
    assert_drawn_by(KC[0].1, &cs, 10.0, "ascii");
}

// 8–9: /Differences naming accented Latin, Greek, Cyrillic (from Liberation
// Mono), uniXXXX, a name no face has, and StandardEncoding / MacRoman.
#[test]
fn encodings_differences_and_missing_glyphs() {
    let diffs = "<< /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences \
        [65 /Aacute /Scaron /Zcaron /lslash /Omega /Delta /uni0410 /uni0416 /uni0496 /uni03B1 /uni2603 /notaglyph] >>";
    let p = pdf(
        &[type1("Courier", diffs)],
        "BT /F1 12 Tf 20 200 Td (ABCDEFGHIJKL) Tj ET",
    );
    let cs = chars(&p);
    assert_eq!(cs.len(), 12);
    // Ω (the AGL's U+2126), ∆, А, Ж, Җ, α: characters only Liberation
    // Mono had, still drawn.
    for (i, u) in [
        (4, '\u{2126}'),
        (5, '\u{2206}'),
        (6, 'А'),
        (7, 'Ж'),
        (8, 'Җ'),
        (9, 'α'),
    ] {
        assert_eq!(
            cs[i].unicode.as_deref().and_then(|s| s.chars().next()),
            Some(u),
            "{i}"
        );
        let b = cs[i].bounds.unwrap_or_else(|| panic!("{u} not drawn"));
        assert!(ink_in(&p, b) > 3, "{u} not inked");
    }
    assert_drawn_by(KC[0].1, &cs, 12.0, "differences");
    // A character neither face has and a name no table knows: no ink,
    // and (no /Widths, no AFM width, no glyph) no advance — as with
    // Liberation Mono.
    for c in &cs[10..] {
        assert!(c.bounds.is_none(), "{:?}", c.unicode);
        assert_eq!(c.advance.0, 0.0, "{:?}", c.unicode);
    }
    for enc in ["/MacRomanEncoding", "/StandardEncoding"] {
        let p = pdf(
            &[type1("Courier", enc)],
            "BT /F1 12 Tf 20 200 Td (\\200\\241\\256\\302 AZ) Tj ET",
        );
        assert_drawn_by(KC[0].1, &chars(&p), 12.0, enc);
    }
}

// 10: widths: the AFM's 600 for its names; a name the AFM lacks (Cyrillic,
// Greek) Liberation Mono's advance, as before; /Widths when the PDF gives
// them.
#[test]
fn advances() {
    let diffs = "<< /Type /Encoding /Differences [65 /uni0416 /uni03B1 /A] >>";
    let p = pdf(
        &[type1("Courier", diffs)],
        "BT /F1 10 Tf 0 200 Td (ABC) Tj ET",
    );
    let cs = chars(&p);
    let lm = 1229.0 / 2048.0 * 10.0;
    assert!((cs[0].advance.0 - lm).abs() < 1e-9, "{:?}", cs[0].advance);
    assert!((cs[1].advance.0 - lm).abs() < 1e-9, "{:?}", cs[1].advance);
    assert!((cs[2].advance.0 - 6.0).abs() < 1e-9, "{:?}", cs[2].advance);
    assert!((cs[2].origin.0 - 2.0 * lm).abs() < 1e-9);
    let w = "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding \
             /FirstChar 65 /LastChar 66 /Widths [500 700] >>";
    let cs = chars(&pdf(&[w.to_string()], "BT /F1 10 Tf 0 200 Td (ABAB) Tj ET"));
    let xs: Vec<f64> = cs.iter().map(|c| c.origin.0).collect();
    assert_eq!(xs, [0.0, 5.0, 12.0, 17.0]);
}

// 11–13: Tc, Tw, Tz move origins by the specification's formula.
#[test]
fn character_word_spacing_and_horizontal_scaling() {
    let f = [type1("Courier", "/WinAnsiEncoding")];
    let xs = |ops: &str| -> Vec<f64> {
        chars(&pdf(
            &f,
            &format!("BT /F1 10 Tf {ops} 0 200 Td (a b) Tj ET"),
        ))
        .iter()
        .map(|c| c.origin.0)
        .collect()
    };
    let close = |a: &[f64], b: &[f64]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    };
    assert!(close(&xs(""), &[0.0, 6.0, 12.0]));
    assert!(close(&xs("2 Tc"), &[0.0, 8.0, 16.0]));
    assert!(close(&xs("3 Tw"), &[0.0, 6.0, 15.0]));
    assert!(close(&xs("50 Tz"), &[0.0, 3.0, 6.0]));
}

// 14–17: rotation, scaling, sizes, lines: the glyph boxes follow.
#[test]
fn transforms_sizes_and_lines() {
    let f = [type1("Courier", "/WinAnsiEncoding")];
    // Rotated 90°: the box of x is the face's, turned.
    let cs = chars(&pdf(&f, "BT /F1 20 Tf 0 1 -1 0 200 100 Tm (x) Tj ET"));
    let b = cs[0].bounds.unwrap();
    let up = face_box(KC[0].1, 'x', 20.0, (0.0, 0.0)).unwrap();
    assert!(
        near(
            b,
            (200.0 - up.3, 100.0 + up.0, 200.0 - up.1, 100.0 + up.2),
            0.4
        ),
        "{b:?} {up:?}"
    );
    // Scaled by cm, and at several sizes: the box scales.
    for (pre, size) in [("q 2 0 0 2 0 0 cm", 20.0), ("q", 7.0), ("q", 33.0)] {
        let tf = if pre == "q" { size } else { 10.0 };
        let cs = chars(&pdf(
            &f,
            &format!("{pre} BT /F1 {tf} Tf 10 10 Td (Hx) Tj ET Q"),
        ));
        let h = cs[0].bounds.unwrap();
        let want = face_box(KC[0].1, 'H', size, (0.0, 0.0)).unwrap();
        assert!(
            ((h.3 - h.1) - (want.3 - want.1)).abs() < 0.03 * size,
            "{pre} {size}: {h:?}"
        );
        assert!(
            (cs[1].origin.0 - cs[0].origin.0 - 0.6 * size).abs() < 1e-6,
            "{pre} {size}"
        );
    }
    // Lines: T* moves down by the leading, each line drawn.
    let cs = chars(&pdf(
        &f,
        "BT /F1 10 Tf 14 TL 10 200 Td (ab) Tj T* (cd) Tj T* (ef) Tj ET",
    ));
    let ys: Vec<f64> = cs.iter().step_by(2).map(|c| c.origin.1).collect();
    assert_eq!(ys, [200.0, 186.0, 172.0]);
    assert_drawn_by(KC[0].1, &cs, 10.0, "lines");
}

// 18–19: fonts mixed on a page: only Courier changes face; Helvetica,
// Times, a fixed-pitch font of another name and an embedded font keep
// theirs.
#[test]
fn mixed_fonts_and_no_effect_elsewhere() {
    let mono = "<< /Type /Font /Subtype /TrueType /BaseFont /SomeTypewriter /Encoding /WinAnsiEncoding \
        /FirstChar 32 /LastChar 126 /Widths [95 600 95 600] /FontDescriptor << /Type /FontDescriptor \
        /FontName /SomeTypewriter /Flags 33 /FontBBox [0 -200 600 800] /ItalicAngle 0 /Ascent 800 \
        /Descent -200 /CapHeight 600 /StemV 80 >> >>"
        .replace("[95 600 95 600]", &format!("[{}]", vec!["600"; 95].join(" ")));
    let p = pdf(
        &[
            type1("Courier-Bold", "/WinAnsiEncoding"),
            type1("Helvetica", "/WinAnsiEncoding"),
            type1("Times-Roman", "/WinAnsiEncoding"),
            mono,
            type1("CourierNewPS-ItalicMT", "/WinAnsiEncoding"),
        ],
        "BT /F1 12 Tf 10 250 Td (Abc) Tj /F2 12 Tf (Abc) Tj /F3 12 Tf (Abc) Tj /F4 12 Tf (Abc) Tj \
         /F5 12 Tf (Abc) Tj ET",
    );
    assert_eq!(
        substitutes(&p),
        [
            (
                "Courier-Bold".into(),
                "KasaneCourier-Bold",
                SubstKind::Standard14
            ),
            (
                "CourierNewPS-ItalicMT".into(),
                "KasaneCourier-Italic",
                SubstKind::Alias
            ),
            (
                "Helvetica".into(),
                "LiberationSans-Regular",
                SubstKind::Standard14
            ),
            (
                "SomeTypewriter".into(),
                "LiberationMono-Regular",
                SubstKind::Generic
            ),
            (
                "Times-Roman".into(),
                "LiberationSerif-Regular",
                SubstKind::Standard14
            ),
        ]
    );
    let cs = chars(&p);
    assert_drawn_by(KC[1].1, &cs[0..3], 12.0, "Courier-Bold");
    assert_drawn_by(KC[2].1, &cs[12..15], 12.0, "Courier New Italic");
    assert_drawn_by(LM_REGULAR, &cs[9..12], 12.0, "fixed pitch, another name");
}

// 20: names that are not Courier, or not readable, take no Courier face;
// a non-ASCII name is still refused.
#[test]
fn other_and_unreadable_names() {
    for base in ["Courierish", "CourierPrime", "Cour", "Courier10Pitch"] {
        let p = pdf(
            &[type1(base, "/WinAnsiEncoding")],
            "BT /F1 12 Tf 20 200 Td (Ab) Tj ET",
        );
        let s = substitutes(&p);
        assert!(
            s.iter().all(|(_, f, _)| !f.starts_with("KasaneCourier")),
            "{base}: {s:?}"
        );
    }
    let p = pdf(
        &[type1("Courier#E9", "/WinAnsiEncoding")],
        "BT /F1 12 Tf 20 200 Td (Ab) Tj ET",
    );
    let doc = Document::from_bytes(&p).unwrap();
    assert!(render_page(&doc, &doc.pages()[0], &RenderOptions::default()).is_err());
}
