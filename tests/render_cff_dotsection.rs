//! CFF charstrings carrying `dotsection` (12 0), the Type 1 hint the
//! Type 2 format keeps only as deprecated and to be ignored (Adobe TN
//! 5177, appendix B). Programs converted from Type 1 fonts still have it
//! in glyphs with dots and accents; such a glyph used to be drawn empty.
//! The programs here are built by `cff` below: each glyph a 400 × 700
//! rectangle, with `dotsection` in the charstring itself, after a
//! `hintmask` whose two mask bytes are 12 and 0 (not an operator), in a
//! local subroutine and in a global one — each compared with the same
//! program without `dotsection`.

mod common;

use kasane_pdf::{render_page, Document, RenderOptions};

fn num(out: &mut Vec<u8>, n: i32) {
    if (-107..=107).contains(&n) {
        out.push((n + 139) as u8);
    } else {
        out.push(28);
        out.extend_from_slice(&(n as i16).to_be_bytes());
    }
}

fn op(out: &mut Vec<u8>, nums: &[i32], code: &[u8]) {
    for &n in nums {
        num(out, n);
    }
    out.extend_from_slice(code);
}

const DOTSECTION: &[u8] = &[12, 0];

type Bodies = Vec<Vec<u8>>;

/// The rectangle's sides (after a move to its corner).
fn sides(out: &mut Vec<u8>) {
    op(out, &[400, 0], &[5]);
    op(out, &[0, 700], &[5]);
    op(out, &[-400, 0], &[5]);
}

/// Charstrings for `a b c d` (and `.notdef`), local and global
/// subroutines; `dot` places `dotsection` in each.
fn glyphs(dot: bool) -> (Bodies, Bodies, Bodies) {
    let d = |out: &mut Vec<u8>| {
        if dot {
            out.extend_from_slice(DOTSECTION);
        }
    };
    let notdef = vec![14];
    let mut a = Vec::new();
    op(&mut a, &[100, 0], &[21]);
    d(&mut a);
    sides(&mut a);
    a.push(14);
    // Ten stems (five hstemhm, five implicit vstems): two mask bytes,
    // 0x0C 0x00 — data, not a dotsection.
    let mut b = Vec::new();
    op(&mut b, &[0, 10, 20, 10, 40, 10, 60, 10, 80, 10], &[18]);
    op(
        &mut b,
        &[0, 10, 20, 10, 40, 10, 60, 10, 80, 10],
        &[19, 0x0C, 0x00],
    );
    d(&mut b);
    op(&mut b, &[100, 0], &[21]);
    sides(&mut b);
    b.push(14);
    let mut c = Vec::new();
    op(&mut c, &[100, 0], &[21]);
    op(&mut c, &[-107], &[10]);
    c.push(14);
    let mut e = Vec::new();
    op(&mut e, &[100, 0], &[21]);
    op(&mut e, &[-107], &[29]);
    e.push(14);
    let mut sub = Vec::new();
    d(&mut sub);
    sides(&mut sub);
    sub.push(11);
    (vec![notdef, a, b, c, e], vec![sub.clone()], vec![sub])
}

fn index(out: &mut Vec<u8>, items: &[Vec<u8>]) {
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
    if items.is_empty() {
        return;
    }
    out.push(4);
    let mut off = 1u32;
    out.extend_from_slice(&off.to_be_bytes());
    for it in items {
        off += it.len() as u32;
        out.extend_from_slice(&off.to_be_bytes());
    }
    for it in items {
        out.extend_from_slice(it);
    }
}

fn int5(out: &mut Vec<u8>, n: usize) {
    out.push(29);
    out.extend_from_slice(&(n as i32).to_be_bytes());
}

/// A bare CFF program of the glyphs above, named `a b c d` (standard
/// strings 66–69) or, `cid`, CID-keyed with CIDs 1–4.
fn cff(dot: bool, cid: bool) -> Vec<u8> {
    let (charstrings, local, global) = glyphs(dot);
    let strings: Vec<Vec<u8>> = if cid {
        vec![b"Adobe".to_vec(), b"Identity".to_vec()]
    } else {
        vec![]
    };
    let mut charset = vec![0u8];
    for g in 1..charstrings.len() {
        let id = if cid { g } else { 65 + g };
        charset.extend_from_slice(&(id as u16).to_be_bytes());
    }
    // Private DICT: Subrs right after it (a 6-byte dict).
    let mut private = Vec::new();
    int5(&mut private, 6);
    private.push(19);

    // Offsets from the fixed-size Top DICT (5-byte integers).
    let top_len = top_dict(cid, [0; 5], 0).len();
    let mut head = vec![1, 0, 4, 4];
    index(&mut head, &[b"T".to_vec()]);
    let after_top = head.len() + 2 + 1 + 8 + top_len;
    let mut tail = Vec::new();
    index(&mut tail, &strings);
    index(&mut tail, &global);
    let charset_at = after_top + tail.len();
    tail.extend_from_slice(&charset);
    let cs_at = after_top + tail.len();
    index(&mut tail, &charstrings);
    let private_at = after_top + tail.len();
    tail.extend_from_slice(&private);
    index(&mut tail, &local);
    let (mut fdarray_at, mut fdselect_at) = (0, 0);
    if cid {
        let mut fd = Vec::new();
        int5(&mut fd, private.len());
        int5(&mut fd, private_at);
        fd.push(18);
        fdarray_at = after_top + tail.len();
        index(&mut tail, &[fd]);
        fdselect_at = after_top + tail.len();
        tail.push(0);
        tail.extend(std::iter::repeat_n(0u8, charstrings.len()));
    }
    let top = top_dict(
        cid,
        [charset_at, cs_at, private_at, fdarray_at, fdselect_at],
        private.len(),
    );
    assert_eq!(top.len(), top_len);
    index(&mut head, &[top]);
    head.extend_from_slice(&tail);
    head
}

/// The Top DICT: charset, CharStrings and Private, or (`cid`) ROS
/// (Adobe, Identity, 0), charset, CharStrings, FDArray and FDSelect.
fn top_dict(cid: bool, at: [usize; 5], private_len: usize) -> Vec<u8> {
    let [charset, cs, private, fdarray, fdselect] = at;
    let mut top = Vec::new();
    if cid {
        top.extend_from_slice(&[0xf8, 0x1b, 0xf8, 0x1c, 139, 12, 30]);
    }
    int5(&mut top, charset);
    top.push(15);
    int5(&mut top, cs);
    top.push(17);
    if cid {
        int5(&mut top, fdarray);
        top.extend_from_slice(&[12, 36]);
        int5(&mut top, fdselect);
        top.extend_from_slice(&[12, 37]);
    } else {
        int5(&mut top, private_len);
        int5(&mut top, private);
        top.push(18);
    }
    top
}

/// `abcd` at 100 pt, each glyph advancing 600, from (20, 50).
fn pdf(program: &[u8], cid: bool) -> Vec<u8> {
    let (content, font, subtype): (&[u8], String, &str) = if cid {
        (
            b"BT /F0 100 Tf 20 50 Td <0001000200030004> Tj ET",
            "<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H \
             /DescendantFonts [8 0 R] >>"
                .into(),
            "/CIDFontType0C",
        )
    } else {
        (
            b"BT /F0 100 Tf 20 50 Td (abcd) Tj ET",
            "<< /Type /Font /Subtype /Type1 /BaseFont /T /FirstChar 97 /LastChar 100 \
             /Widths [600 600 600 600] /Encoding << /Differences [97 /a /b /c /d] >> \
             /FontDescriptor 6 0 R >>"
                .into(),
            "/Type1C",
        )
    };
    common::build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 280 200] \
              /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, common::stream_obj("", content)),
        (5, font.into_bytes()),
        (
            6,
            b"<< /Type /FontDescriptor /FontName /T /Flags 32 /FontBBox [0 0 600 700] \
              /ItalicAngle 0 /Ascent 700 /Descent 0 /CapHeight 700 /StemV 80 /FontFile3 7 0 R >>"
                .to_vec(),
        ),
        (
            7,
            common::stream_obj(&format!("/Subtype {subtype}"), program),
        ),
        (
            8,
            b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /T \
              /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
              /DW 600 /FontDescriptor 6 0 R >>"
                .to_vec(),
        ),
    ])
}

fn draw(data: &[u8]) -> (usize, Vec<u8>) {
    let doc = Document::from_bytes(data).unwrap();
    let r = render_page(&doc, &doc.pages()[0], &RenderOptions::new()).unwrap();
    assert_eq!(r.stride(), r.width() as usize * 4);
    (r.width() as usize, r.pixels().to_vec())
}

/// Dark pixels in the cell of glyph `k` (60 pt wide from x 20), page y
/// 50..120.
fn ink((w, px): &(usize, Vec<u8>), k: usize) -> usize {
    let s = *w as f64 / 280.0;
    let h = px.len() / 4 / w;
    let (x0, x1) = (
        ((20.0 + 60.0 * k as f64) * s) as usize,
        ((80.0 + 60.0 * k as f64) * s) as usize,
    );
    let (y0, y1) = (
        ((200.0 - 120.0) * s) as usize,
        ((200.0 - 50.0) * s) as usize,
    );
    (y0..y1.min(h))
        .flat_map(|y| (x0..x1.min(*w)).map(move |x| (x, y)))
        .filter(|&(x, y)| px[(y * w + x) * 4] < 128)
        .count()
}

#[test]
fn glyphs_with_dotsection_are_drawn_as_without_it() {
    for cid in [false, true] {
        let with = draw(&pdf(&cff(true, cid), cid));
        let without = draw(&pdf(&cff(false, cid), cid));
        // Every glyph is its rectangle: 40 × 70 pt (2800 px at 72 dpi).
        for k in 0..4 {
            assert!(
                ink(&without, k) >= 2700,
                "cid {cid}: control glyph {k}: {}",
                ink(&without, k)
            );
            assert_eq!(ink(&with, k), ink(&without, k), "cid {cid}: glyph {k}");
        }
        assert!(with == without, "cid {cid}: the pages differ");
    }
}
