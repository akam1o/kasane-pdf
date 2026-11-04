//! The content interpreter and rasteriser on structured content (v0.5.1
//! R4, target B): a fixed, valid page whose resources hold nested Form
//! XObjects (one shared, one cyclic), a transparency group, soft masks
//! (alpha and luminosity), DeviceRGB / DeviceGray / 1-bit and soft-masked
//! images, axial and radial shadings, a tiling pattern, a Type 3 font, a
//! standard font and an annotation appearance; the page content is a
//! sequence of operators built from the input — the right operand count
//! or one too few or too many, numbers from a set of extremes
//! (zero, huge, tiny, negative, overflowing, not-a-number-like tokens),
//! defined and undefined resource names, inline images, unbalanced
//! `q`/`Q`, `BT`/`ET`, marked content.
//!
//! Oracles, all true by the specification:
//! - no panic, stack overflow, timeout or runaway allocation (libFuzzer);
//! - a rendered page is the size the page box and DPI give, within
//!   `max_render_total_pixels`;
//! - rendering is deterministic (the same input twice: the same pixels);
//! - when the content renders, wrapping it in `q … Q` and prepending an
//!   identity `cm` change nothing;
//! - drawing a shared form twice equals drawing two equal copies.
#![no_main]

use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};
use kasane_pdf_fuzz::limits;
use libfuzzer_sys::fuzz_target;

struct Input<'a>(std::slice::Iter<'a, u8>);
impl Input<'_> {
    fn byte(&mut self) -> Option<u8> {
        self.0.next().copied()
    }
}

const NUMS: [&str; 16] = [
    "0", "1", "-1", "0.5", "10", "-20", "255", "1000000", "-1000000", "3.4e38", "1e400",
    "0.000001", "99999999999999999999", "-0", ".", "5",
];
const NAMES: [&str; 14] = [
    "/F1", "/F3", "/Fx", "/Fy", "/Grp", "/Im1", "/Im2", "/ImM", "/GS1", "/GSm", "/GSl", "/Sh1",
    "/P1", "/Nope",
];

/// (operator, operand kinds: n = number, a = name, s = string, A = array)
const OPS: [(&str, &str); 52] = [
    ("q", ""), ("Q", ""), ("cm", "nnnnnn"), ("w", "n"), ("J", "n"), ("j", "n"), ("M", "n"),
    ("d", "An"), ("ri", "a"), ("i", "n"), ("gs", "a"), ("m", "nn"), ("l", "nn"),
    ("c", "nnnnnn"), ("v", "nnnn"), ("y", "nnnn"), ("h", ""), ("re", "nnnn"), ("S", ""),
    ("s", ""), ("f", ""), ("f*", ""), ("B", ""), ("B*", ""), ("b", ""), ("n", ""), ("W", ""),
    ("W*", ""), ("cs", "a"), ("CS", "a"), ("sc", "nnn"), ("scn", "a"), ("g", "n"), ("G", "n"),
    ("rg", "nnn"), ("RG", "nnn"), ("k", "nnnn"), ("sh", "a"), ("Do", "a"), ("BT", ""),
    ("ET", ""), ("Tf", "an"), ("Td", "nn"), ("Tm", "nnnnnn"), ("Tr", "n"), ("Tj", "s"),
    ("TJ", "A"), ("'", "s"), ("Tc", "n"), ("BDC", "aa"), ("EMC", ""), ("BMC", "a"),
];

/// Well-formed constructs the operator-level generator rarely forms on
/// its own: text in each font, pattern fills, masked and blended
/// drawing, shadings under a clip, form invocations.
const SNIPPETS: [&str; 14] = [
    "BT /F3 12 Tf 2 2 Td (ab) Tj ET",
    "BT /F1 8 Tf 1 20 Td 2 Tr (Hi) Tj [(a) -120 (b)] TJ ET",
    "BT /F3 6 Tf 7 Tr 1 1 Td (ba) Tj ET 0 0 1 rg 0 0 60 60 re f",
    "/Pattern cs /P1 scn 0 0 40 40 re f",
    "/Pattern cs /P2 scn 5 5 30 30 re f",
    "q /GSm gs 0 0 1 rg 0 0 40 40 re f Q",
    "q /GSl gs 30 0 0 30 0 0 cm /Im1 Do Q",
    "q /GS1 gs 1 0 0 rg 10 10 20 20 re f /Grp Do Q",
    "q 0 0 20 20 re W n /Sh2 sh Q",
    "/Sh1 sh",
    "/Fx Do /Fy Do",
    "/Cyc Do",
    "q 20 0 0 20 5 5 cm /Im2 Do /ImM Do Q",
    "q 1 0 0 1 0 0 cm 0 1 0 RG 3 w 0 0 m 60 60 l S Q",
];

fn operand(inp: &mut Input, kind: char) -> String {
    let b = inp.byte().unwrap_or(0) as usize;
    match kind {
        'n' => NUMS[b % NUMS.len()].into(),
        'a' => {
            // Resource names for the operator's resource kind mostly, any
            // name sometimes; pattern names after `cs`.
            if b % 5 == 0 { "/Pattern".into() } else { NAMES[b % NAMES.len()].into() }
        }
        's' => ["(ab)", "()", "<41>", "(\\101)", "(\\(x)"][b % 5].into(),
        _ => ["[]", "[3 1]", "[(a) -250 (b)]", "[1e400]"][b % 4].into(),
    }
}

fn content(inp: &mut Input) -> Vec<u8> {
    let n = 1 + inp.byte().unwrap_or(0) as usize % 48;
    let mut c = Vec::new();
    for _ in 0..n {
        let Some(b) = inp.byte() else { break };
        if b == 254 || b == 253 || b == 252 {
            let k = inp.byte().unwrap_or(0) as usize;
            c.extend_from_slice(SNIPPETS[k % SNIPPETS.len()].as_bytes());
            c.push(b'\n');
            continue;
        }
        if b == 255 {
            // An inline image, well-formed or truncated.
            let k = inp.byte().unwrap_or(0);
            c.extend_from_slice(match k % 3 {
                0 => b"BI /W 2 /H 2 /BPC 8 /CS /G ID \x00\x80\x40\xff EI ",
                1 => b"BI /W 2 /H 2 /BPC 1 /IM true ID \xc0\x40 EI ",
                _ => b"BI /W 4 /H 4 /BPC 8 /CS /RGB ID \x01\x02 EI ",
            });
            continue;
        }
        let (op, args) = OPS[b as usize % OPS.len()];
        let mut kinds: Vec<char> = args.chars().collect();
        // One operand too few or too many now and then.
        match inp.byte().unwrap_or(0) % 16 {
            0 => {
                kinds.pop();
            }
            1 => kinds.push('n'),
            _ => {}
        }
        for k in kinds {
            c.extend_from_slice(operand(inp, k).as_bytes());
            c.push(b' ');
        }
        c.extend_from_slice(op.as_bytes());
        c.push(b'\n');
    }
    c
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// The page with `content`; `twin` draws the shared form through a
/// second, equal copy instead of a second use of the same object.
fn page(content: &[u8], twin: bool) -> Vec<u8> {
    let fy = if twin { 21 } else { 20 };
    let res = format!(
        "<< /Font << /F1 30 0 R /F3 31 0 R >> /XObject << /Fx 20 0 R /Fy {fy} 0 R /Grp 22 0 R /Cyc 23 0 R \
         /Im1 40 0 R /Im2 41 0 R /ImM 42 0 R >> /ExtGState << /GS1 << /CA 0.5 /ca 0.5 /BM /Multiply >> \
         /GSm << /SMask << /S /Alpha /G 24 0 R >> >> /GSl << /SMask << /S /Luminosity /G 24 0 R /BC [0] >> >> >> \
         /Shading << /Sh1 50 0 R /Sh2 51 0 R >> /Pattern << /P1 52 0 R /P2 53 0 R >> /ColorSpace << /CS0 /DeviceRGB >> >>"
    );
    let form = "/Type /XObject /Subtype /Form /BBox [0 0 40 40] /Resources << /XObject << /In 25 0 R >> >>";
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 60 60] /Resources {res} /Contents 4 0 R /Annots [60 0 R] >>")
                .into_bytes(),
        ),
        (4, stream("", content)),
        (20, stream(form, b"1 0 0 rg 0 0 20 20 re f /In Do")),
        (21, stream(form, b"1 0 0 rg 0 0 20 20 re f /In Do")),
        (22, stream("/Type /XObject /Subtype /Form /BBox [0 0 40 40] /Group << /S /Transparency /I true /K true >>", b"0 1 0 rg 5 5 30 30 re f")),
        (23, stream("/Type /XObject /Subtype /Form /BBox [0 0 40 40] /Resources << /XObject << /Cyc 23 0 R >> >>", b"/Cyc Do")),
        (24, stream("/Type /XObject /Subtype /Form /BBox [0 0 60 60] /Group << /S /Transparency /CS /DeviceGray >>", b"0.7 g 0 0 30 60 re f")),
        (25, stream("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Matrix [0.5 0 0 0.5 2 2]", b"0 0 1 rg 0 0 10 10 re f")),
        (30, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (31, b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 10 10] /FontMatrix [0.1 0 0 0.1 0 0] /CharProcs << /a 32 0 R >> /Encoding << /Differences [97 /a 98 /a] >> /FirstChar 97 /LastChar 98 /Widths [10 10] >>".to_vec()),
        (32, stream("", b"10 0 d0 0 0 8 8 re f")),
        (40, stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0])),
        (41, stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 43 0 R", &[0, 85, 170, 255])),
        (42, stream("/Type /XObject /Subtype /Image /Width 8 /Height 2 /ImageMask true /BitsPerComponent 1", &[0xa5, 0x5a])),
        (43, stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[255, 128, 64, 0])),
        (50, b"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 60 60] /Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> /Extend [true true] >>".to_vec()),
        (51, b"<< /ShadingType 3 /ColorSpace /DeviceGray /Coords [30 30 0 30 30 30] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 2 >> >>".to_vec()),
        (52, stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>", b"1 0 1 rg 0 0 5 5 re f")),
        (53, b"<< /PatternType 2 /Shading 50 0 R >>".to_vec()),
        (60, b"<< /Type /Annot /Subtype /Square /Rect [40 40 58 58] /AP << /N 61 0 R >> >>".to_vec()),
        (61, stream("/Type /XObject /Subtype /Form /BBox [0 0 18 18] /Resources << /ExtGState << /G << /ca 0.4 >> >> >>", b"/G gs 0 0 1 rg 0 0 18 18 re f")),
    ];
    let mut b = b"%PDF-1.7\n".to_vec();
    let size = 62;
    let mut offs = vec![0usize; size];
    for (n, body) in &objs {
        offs[*n as usize] = b.len();
        b.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        b.extend_from_slice(body);
        b.extend_from_slice(b"\nendobj\n");
    }
    let x = b.len();
    b.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for off in &offs[1..] {
        if *off > 0 {
            b.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        } else {
            b.extend_from_slice(b"0000000000 65535 f \n");
        }
    }
    b.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
    b
}

fn limits_for(page_limits: &Limits) -> RenderOptions {
    RenderOptions::new().with_dpi(36.0).with_limits(page_limits.clone())
}

/// Pixels of the page, or the error kind.
fn render(b: &[u8], l: &Limits) -> Result<(u32, u32, Vec<u8>), Error> {
    let d = Document::open(b, &kasane_pdf::OpenOptions::new().with_limits(l.clone()))?;
    let p = &d.pages()[0];
    let r = render_page(&d, p, &limits_for(l))?;
    let _ = kasane_pdf::extract_text(&d, p, &kasane_pdf::TextOptions::default().with_limits(l.clone()));
    Ok((r.width(), r.height(), r.pixels().to_vec()))
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data.iter());
    let body = content(&mut inp);
    let l = limits();
    let a = render(&page(&body, false), &l);
    let b = render(&page(&body, false), &l);
    match (&a, &b) {
        (Ok(x), Ok(y)) => assert!(x == y, "rendering is not deterministic"),
        (Err(x), Err(y)) => assert_eq!(x.to_string(), y.to_string(), "errors differ"),
        _ => panic!("one of two identical renders failed"),
    }
    let Ok((w, h, px)) = a else { return };
    // 60 pt at 36 dpi.
    assert_eq!((w, h), (30, 30), "page size");
    assert_eq!(px.len(), (w * h * 4) as usize);
    assert!((w as u64) * (h as u64) <= l.max_render_total_pixels);
    // Equivalences (each only when the variant itself renders: a deeper
    // `q` stack or one more operator may meet a limit the original did
    // not).
    for variant in [[&b"q\n"[..], &body, b"\nQ"].concat(), [&b"1 0 0 1 0 0 cm\n"[..], &body].concat()] {
        if let Ok(v) = render(&page(&variant, false), &l) {
            assert!(v.2 == px, "an equivalent content renders differently");
        }
    }
    if let Ok(t) = render(&page(&body, true), &l) {
        assert!(t.2 == px, "a shared form and its copy render differently");
    }
});
