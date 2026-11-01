//! Cross-reference precedence across update sections (ISO 32000-1
//! §7.5.4, §7.5.6, §7.5.8; ISO 32000-2 §7.5.8.4 as clarified by PDF
//! Association issue #237): per object *number*, the newest section that
//! lists it decides — in use, compressed or free; within a hybrid
//! section the classic table is searched before its `/XRefStm` stream,
//! which comes before the `/Prev` section.
//!
//! Every version of an object carries a distinct value (the width in
//! `[0 0 W 100]`, or a string), and each test checks which body was
//! chosen — never only a page count.

use kasane_pdf::{Document, Error, IndirectRef, Object};

/// A PDF assembled section by section; offsets are byte positions.
struct Pdf {
    b: Vec<u8>,
}

/// `(number, generation, 'n' | 'f', offset)`.
type Row = (u32, u16, u8, u64);
/// `(number, type, field 2, field 3)` of an xref stream.
type SRow = (u32, u8, u64, u64);

impl Pdf {
    fn new() -> Self {
        Pdf {
            b: b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n".to_vec(),
        }
    }

    fn obj(&mut self, num: u32, gen: u16, body: &str) -> u64 {
        let at = self.b.len() as u64;
        self.b
            .extend_from_slice(format!("{num} {gen} obj\n{body}\nendobj\n").as_bytes());
        at
    }

    /// An object stream `num` holding `members`.
    fn objstm(&mut self, num: u32, members: &[(u32, &str)]) -> u64 {
        let mut head = String::new();
        let mut data = String::new();
        for (n, body) in members {
            head.push_str(&format!("{n} {} ", data.len()));
            data.push_str(body);
            data.push(' ');
        }
        let s = format!("{head}{data}");
        self.obj(
            num,
            0,
            &format!(
                "<< /Type /ObjStm /N {} /First {} /Length {} >>\nstream\n{s}\nendstream",
                members.len(),
                head.len(),
                s.len()
            ),
        )
    }

    fn classic(&mut self, rows: &[Row], trailer: &str) -> u64 {
        let at = self.b.len() as u64;
        let mut rows = rows.to_vec();
        rows.sort();
        self.b.extend_from_slice(b"xref\n");
        let mut i = 0;
        while i < rows.len() {
            let mut j = i;
            while j + 1 < rows.len() && rows[j + 1].0 == rows[j].0 + 1 {
                j += 1;
            }
            self.b
                .extend_from_slice(format!("{} {}\n", rows[i].0, j - i + 1).as_bytes());
            for (_, g, k, off) in &rows[i..=j] {
                self.b
                    .extend_from_slice(format!("{off:010} {g:05} {} \n", *k as char).as_bytes());
            }
            i = j + 1;
        }
        self.b
            .extend_from_slice(format!("trailer\n{trailer}\n").as_bytes());
        at
    }

    /// An uncompressed xref stream object `num` with field widths `w`.
    fn xstream(&mut self, num: u32, w: [usize; 3], rows: &[SRow], extra: &str) -> u64 {
        let mut rows = rows.to_vec();
        rows.sort();
        let mut index = Vec::new();
        let mut data = Vec::new();
        let field = |v: u64, n: usize, out: &mut Vec<u8>| {
            for k in (0..n).rev() {
                out.push((v >> (8 * k)) as u8);
            }
        };
        let mut i = 0;
        while i < rows.len() {
            let mut j = i;
            while j + 1 < rows.len() && rows[j + 1].0 == rows[j].0 + 1 {
                j += 1;
            }
            index.push(format!("{} {}", rows[i].0, j - i + 1));
            for (_, t, f2, f3) in &rows[i..=j] {
                field(*t as u64, w[0], &mut data);
                field(*f2, w[1], &mut data);
                field(*f3, w[2], &mut data);
            }
            i = j + 1;
        }
        let at = self.b.len() as u64;
        self.b.extend_from_slice(
            format!(
                "{num} 0 obj\n<< /Type /XRef /W [{} {} {}] /Index [{}] {extra} /Length {} >>\nstream\n",
                w[0],
                w[1],
                w[2],
                index.join(" "),
                data.len()
            )
            .as_bytes(),
        );
        self.b.extend_from_slice(&data);
        self.b.extend_from_slice(b"\nendstream\nendobj\n");
        at
    }

    fn end(&mut self, startxref: u64) {
        self.b
            .extend_from_slice(format!("startxref\n{startxref}\n%%EOF\n").as_bytes());
    }

    fn pos(&self) -> u64 {
        self.b.len() as u64
    }
}

const W: [usize; 3] = [1, 4, 2];

/// `b` with the first `from` replaced by `to` (same length: offsets kept).
fn patch(b: &[u8], from: &str, to: &str) -> Vec<u8> {
    assert_eq!(from.len(), to.len());
    let at = b
        .windows(from.len())
        .position(|w| w == from.as_bytes())
        .unwrap_or_else(|| panic!("{from} not found"));
    let mut out = b.to_vec();
    out[at..at + to.len()].copy_from_slice(to.as_bytes());
    out
}

fn r(number: u32, generation: u16) -> IndirectRef {
    IndirectRef { number, generation }
}

/// The width `W` of `[0 0 W 100]` at `n g R`, or `None` for null.
fn width(doc: &Document, n: u32, g: u16) -> Option<f64> {
    match doc.resolve(r(n, g)).unwrap() {
        Object::Array(a) => a.get(2).and_then(|x| x.as_number()),
        Object::Null => None,
        other => panic!("{n} {g} R: unexpected {other:?}"),
    }
}

/// Catalog 1, pages 2, page 3 whose `/MediaBox` is `5 gen R`.
fn base(p: &mut Pdf, gen: u16) -> Vec<Row> {
    let a = p.obj(1, 0, "<< /Type /Catalog /Pages 2 0 R >>");
    let b = p.obj(2, 0, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let c = p.obj(
        3,
        0,
        &format!("<< /Type /Page /Parent 2 0 R /MediaBox 5 {gen} R >>"),
    );
    vec![
        (0, 65535, b'f', 0),
        (1, 0, b'n', a),
        (2, 0, b'n', b),
        (3, 0, b'n', c),
    ]
}

fn as_srows(rows: &[Row]) -> Vec<SRow> {
    rows.iter()
        .map(|&(n, g, k, off)| match k {
            b'n' => (n, 1, off, g as u64),
            _ => (n, 0, 0, g as u64),
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Classic,
    Stream,
    Hybrid,
}

/// A base revision of kind `a` with `5 0 obj [0 0 100 100]`, then an
/// update of kind `b` redefining object 5 as width 200.
fn two_revisions(a: Kind, b: Kind, hidden_in_objstm: bool) -> Vec<u8> {
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    let x1 = match a {
        Kind::Classic => {
            rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
            p.classic(&rows, "<< /Size 9 /Root 1 0 R >>")
        }
        Kind::Stream => {
            rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
            let mut s = as_srows(&rows);
            s.push((6, 1, p.pos(), 0));
            p.xstream(6, W, &s, "/Size 9 /Root 1 0 R")
        }
        Kind::Hybrid => {
            // §7.5.8.4 pattern: section 0 lists 5 as free (hidden from
            // pre-1.5 readers); the hybrid section's /XRefStm supplies it.
            rows.push((5, 0, b'f', 0));
            let x0 = p.classic(&rows, "<< /Size 9 /Root 1 0 R >>");
            p.end(x0);
            let mut s: Vec<SRow> = if hidden_in_objstm {
                let os = p.objstm(8, &[(5, "[0 0 100 100]")]);
                vec![(5, 2, 8, 0), (8, 1, os, 0)]
            } else {
                vec![(5, 1, p.obj(5, 0, "[0 0 100 100]"), 0)]
            };
            s.push((6, 1, p.pos(), 0));
            let xs = p.xstream(6, W, &s, "/Size 9");
            p.classic(
                &[(0, 65535, b'f', 0)],
                &format!("<< /Size 9 /Root 1 0 R /Prev {x0} /XRefStm {xs} >>"),
            )
        }
    };
    p.end(x1);
    let new5 = p.obj(5, 0, "[0 0 200 100]");
    let x2 = match b {
        Kind::Classic => p.classic(
            &[(5, 0, b'n', new5)],
            &format!("<< /Size 9 /Root 1 0 R /Prev {x1} >>"),
        ),
        Kind::Stream => {
            let at = p.pos();
            p.xstream(
                10,
                W,
                &[(5, 1, new5, 0), (10, 1, at, 0)],
                &format!("/Size 11 /Root 1 0 R /Prev {x1}"),
            )
        }
        Kind::Hybrid => {
            let at = p.pos();
            let xs = p.xstream(10, W, &[(5, 1, new5, 0), (10, 1, at, 0)], "/Size 11");
            p.classic(
                &[(0, 65535, b'f', 0)],
                &format!("<< /Size 11 /Root 1 0 R /Prev {x1} /XRefStm {xs} >>"),
            )
        }
    };
    p.end(x2);
    p.b
}

// -------------------------------------------------------------------
// 1–7: update orders
// -------------------------------------------------------------------

#[test]
fn the_newer_section_wins_in_every_update_order() {
    for a in [Kind::Classic, Kind::Stream, Kind::Hybrid] {
        for b in [Kind::Classic, Kind::Stream, Kind::Hybrid] {
            for hidden in [false, true] {
                if hidden && !matches!(a, Kind::Hybrid) {
                    continue;
                }
                let doc = Document::from_bytes(&two_revisions(a, b, hidden)).unwrap();
                assert_eq!(
                    width(&doc, 5, 0),
                    Some(200.0),
                    "{a:?} -> {b:?} (objstm {hidden})"
                );
                assert_eq!(doc.pages()[0].media_box().urx, 200.0, "{a:?} -> {b:?}");
            }
        }
    }
}

#[test]
fn a_hidden_object_comes_from_the_xrefstm_not_the_older_free_entry() {
    // The hybrid base alone: §7.5.8.4 NOTE — the reader finds the object
    // in the cross-reference stream and ignores the free entry of the
    // previous section.
    for hidden in [false, true] {
        let mut p = Pdf::new();
        let mut rows = base(&mut p, 0);
        rows.push((5, 0, b'f', 0));
        let x0 = p.classic(&rows, "<< /Size 9 /Root 1 0 R >>");
        p.end(x0);
        let mut s: Vec<SRow> = if hidden {
            let os = p.objstm(8, &[(5, "[0 0 100 100]")]);
            vec![(5, 2, 8, 0), (8, 1, os, 0)]
        } else {
            vec![(5, 1, p.obj(5, 0, "[0 0 100 100]"), 0)]
        };
        s.push((6, 1, p.pos(), 0));
        let xs = p.xstream(6, W, &s, "/Size 9");
        let x1 = p.classic(
            &[(0, 65535, b'f', 0)],
            &format!("<< /Size 9 /Root 1 0 R /Prev {x0} /XRefStm {xs} >>"),
        );
        p.end(x1);
        let doc = Document::from_bytes(&p.b).unwrap();
        assert_eq!(width(&doc, 5, 0), Some(100.0), "objstm {hidden}");
    }
}

// -------------------------------------------------------------------
// 8–10: the same object number in several places
// -------------------------------------------------------------------

#[test]
fn an_older_xrefstm_never_overrides_a_newer_section() {
    // The older hybrid section's /XRefStm lists 5 at the old body; the
    // newer classic section lists 5 at the new body — and so does an
    // object the newer section does *not* list (7) keep its older entry.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    let old5 = p.obj(5, 0, "[0 0 100 100]");
    let only7 = p.obj(7, 0, "(seven)");
    rows.push((5, 0, b'f', 0));
    let x0 = p.classic(&rows, "<< /Size 9 /Root 1 0 R >>");
    p.end(x0);
    let at = p.pos();
    let xs = p.xstream(
        6,
        W,
        &[(5, 1, old5, 0), (6, 1, at, 0), (7, 1, only7, 0)],
        "/Size 9",
    );
    let x1 = p.classic(
        &[(0, 65535, b'f', 0)],
        &format!("<< /Size 9 /Root 1 0 R /Prev {x0} /XRefStm {xs} >>"),
    );
    p.end(x1);
    let new5 = p.obj(5, 0, "[0 0 200 100]");
    let x2 = p.classic(
        &[(5, 0, b'n', new5)],
        &format!("<< /Size 9 /Root 1 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 0), Some(200.0));
    assert_eq!(
        doc.resolve(r(7, 0)).unwrap(),
        Object::String(b"seven".to_vec())
    );
}

/// One hybrid section: the table lists 5 in use at the 100 body; the
/// section's /XRefStm lists 5 free, or in use at the 300 body.
fn same_section(stream_free: bool) -> Document {
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    let a = p.obj(5, 0, "[0 0 100 100]");
    let b = p.obj(5, 0, "[0 0 300 100]");
    let srow = if stream_free {
        (5, 0, 0, 1)
    } else {
        (5, 1, b, 0)
    };
    let at = p.pos();
    let xs = p.xstream(6, W, &[srow, (6, 1, at, 0)], "/Size 7");
    rows.push((5, 0, b'n', a));
    let x = p.classic(&rows, &format!("<< /Size 7 /Root 1 0 R /XRefStm {xs} >>"));
    p.end(x);
    Document::from_bytes(&p.b).unwrap()
}

#[test]
fn within_a_hybrid_section_the_table_precedes_its_stream() {
    // In use in the table: the table's body, whatever the stream says
    // (qpdf, Poppler and Quartz agree).
    assert_eq!(width(&same_section(true), 5, 0), Some(100.0));
    assert_eq!(width(&same_section(false), 5, 0), Some(100.0));
    // Free in the table of the same section: the entry is found, so the
    // stream is not consulted (§7.5.8.4: the stream is searched only when
    // the section has no entry) — 5 0 R is null, as with qpdf. (Poppler
    // and Quartz take the stream's body here; the corpus has no such
    // file.)
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    let b = p.obj(5, 0, "[0 0 300 100]");
    let at = p.pos();
    let xs = p.xstream(6, W, &[(5, 1, b, 0), (6, 1, at, 0)], "/Size 7");
    rows.push((5, 1, b'f', 0));
    let x = p.classic(&rows, &format!("<< /Size 7 /Root 1 0 R /XRefStm {xs} >>"));
    p.end(x);
    // The page's /MediaBox is then null: the document does not open, and
    // the object itself is not the stream's body.
    assert!(Document::from_bytes(&p.b).is_err());
}

// -------------------------------------------------------------------
// 11–13: generations, deletion, reuse
// -------------------------------------------------------------------

#[test]
fn a_reference_resolves_only_at_the_deciding_generation() {
    // The newer section lists 5 in use at generation 2: 5 2 R is the new
    // body, 5 0 R (the older generation's body) is undefined — null.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 2);
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let x1 = p.classic(&rows, "<< /Size 6 /Root 1 0 R >>");
    p.end(x1);
    let new5 = p.obj(5, 2, "[0 0 200 100]");
    let x2 = p.classic(
        &[(5, 2, b'n', new5)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 2), Some(200.0));
    assert_eq!(width(&doc, 5, 0), None);
}

#[test]
fn a_freed_object_is_not_resurrected() {
    // §7.5.4: freeing object 5 (generation 0) writes a free entry with the
    // next generation, 1. The base section's in-use entry for 5 0 must not
    // come back (v0.4.0 resolved it: entries were keyed by number and
    // generation, so the free entry 5/1 did not shadow 5/0).
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    let contents = p.obj(4, 0, "<< /Length 2 >>\nstream\nhi\nendstream");
    rows.push((4, 0, b'n', contents));
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let x1 = p.classic(&rows, "<< /Size 6 /Root 1 0 R >>");
    p.end(x1);
    let x2 = p.classic(
        &[(0, 65535, b'f', 0), (4, 1, b'f', 0)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(doc.resolve(r(4, 0)).unwrap(), Object::Null);
    let w = doc.take_warnings();
    assert!(
        w.iter().any(|w| matches!(w, kasane_pdf::Warning::UndefinedReference { reference } if reference.number == 4)),
        "{w:?}"
    );
    // Freed in a newer xref *stream* (type 0, generation 1): the same.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    rows.push((4, 0, b'n', contents_at(&mut p)));
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let x1 = p.classic(&rows, "<< /Size 7 /Root 1 0 R >>");
    p.end(x1);
    let at = p.pos();
    let x2 = p.xstream(
        6,
        W,
        &[(4, 0, 0, 1), (6, 1, at, 0)],
        &format!("/Size 7 /Root 1 0 R /Prev {x1}"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(doc.resolve(r(4, 0)).unwrap(), Object::Null);
}

fn contents_at(p: &mut Pdf) -> u64 {
    p.obj(4, 0, "<< /Length 2 >>\nstream\nhi\nendstream")
}

#[test]
fn a_number_reused_at_the_next_generation() {
    // Revision 1 defines 5 0; revision 2 frees it (generation 1);
    // revision 3 reuses the number at generation 1.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 1);
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let x1 = p.classic(&rows, "<< /Size 6 /Root 1 0 R >>");
    p.end(x1);
    let x2 = p.classic(
        &[(0, 65535, b'f', 0), (5, 1, b'f', 0)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let reused = p.obj(5, 1, "[0 0 400 100]");
    let x3 = p.classic(
        &[(5, 1, b'n', reused)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x2} >>"),
    );
    p.end(x3);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 1), Some(400.0));
    assert_eq!(width(&doc, 5, 0), None);
    assert_eq!(doc.pages()[0].media_box().urx, 400.0);
}

#[test]
fn an_xref_stream_entry_keeps_its_generation() {
    // Type 1, field 3 = generation 3 (§7.5.8.3 Table 18); v0.4.0 keyed
    // every stream entry at generation 0, so 5 3 R was undefined.
    let mut p = Pdf::new();
    let rows = base(&mut p, 3);
    let off5 = p.obj(5, 3, "[0 0 500 100]");
    let mut s = as_srows(&rows);
    s.push((5, 1, off5, 3));
    s.push((6, 1, p.pos(), 0));
    let x = p.xstream(6, W, &s, "/Size 7 /Root 1 0 R");
    p.end(x);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 3), Some(500.0));
    assert_eq!(width(&doc, 5, 0), None);
}

// -------------------------------------------------------------------
// 14–21: hostile structure
// -------------------------------------------------------------------

fn simple_classic(trailer_extra: &str) -> (Pdf, Vec<Row>) {
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let _ = trailer_extra;
    (p, rows)
}

#[test]
fn a_prev_cycle_is_refused() {
    // Section A's /Prev names section B, B's names A.
    let (mut p, rows) = simple_classic("");
    let x1 = p.classic(&rows, "<< /Size 6 /Root 1 0 R /Prev 0000000000 >>");
    let x2 = p.classic(
        &[(0, 65535, b'f', 0)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x1:010} >>"),
    );
    p.end(x2);
    let b = patch(&p.b, "/Prev 0000000000", &format!("/Prev {x2:010}"));
    let err = Document::from_bytes(&b).unwrap_err();
    assert!(matches!(err, Error::CycleDetected { .. }), "{err:?}");
}

#[test]
fn an_xrefstm_is_one_stream_never_a_chain() {
    // /XRefStm pointing at a classic table (here the section itself) is
    // refused; an /XRefStm stream's own /Prev and /XRefStm are not
    // followed (no second chain, no loop).
    let (mut p, rows) = simple_classic("");
    let x = p.pos();
    let x_self = p.classic(&rows, &format!("<< /Size 6 /Root 1 0 R /XRefStm {x} >>"));
    assert_eq!(x, x_self);
    p.end(x);
    let err = Document::from_bytes(&p.b).unwrap_err();
    assert!(
        matches!(err, Error::Structure(ref m) if m.contains("XRefStm")),
        "{err:?}"
    );

    let (mut p, rows) = simple_classic("");
    let xs_at = p.pos();
    // The stream claims /Prev and /XRefStm of itself.
    let xs = p.xstream(
        6,
        W,
        &[(6, 1, xs_at, 0)],
        &format!("/Size 7 /Prev {xs_at} /XRefStm {xs_at}"),
    );
    let x = p.classic(&rows, &format!("<< /Size 7 /Root 1 0 R /XRefStm {xs} >>"));
    p.end(x);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 0), Some(100.0));
}

#[test]
fn offsets_beyond_the_input_are_refused() {
    let (mut p, rows) = simple_classic("");
    let x = p.classic(&rows, "<< /Size 6 /Root 1 0 R /Prev 99999999 >>");
    p.end(x);
    assert!(matches!(
        Document::from_bytes(&p.b).unwrap_err(),
        Error::Parse(_)
    ));

    let (mut p, rows) = simple_classic("");
    let x = p.classic(&rows, "<< /Size 6 /Root 1 0 R /XRefStm 99999999 >>");
    p.end(x);
    assert!(matches!(
        Document::from_bytes(&p.b).unwrap_err(),
        Error::Parse(_)
    ));

    // An in-use entry far beyond the input (beyond 4 GiB too: no 32-bit
    // wrap onto a real position): the object does not resolve.
    let (mut p, mut rows) = simple_classic("");
    rows.retain(|r| r.0 != 5);
    rows.push((5, 0, b'n', 4_294_967_396)); // 2^32 + 100
    let x = p.classic(&rows, "<< /Size 6 /Root 1 0 R >>");
    p.end(x);
    let doc = Document::from_bytes(&p.b);
    match doc {
        Ok(d) => assert!(d.resolve(r(5, 0)).is_err()),
        Err(e) => assert!(matches!(e, Error::Parse(_) | Error::Structure(_)), "{e:?}"),
    }
}

#[test]
fn a_truncated_xref_stream_is_refused() {
    let mut p = Pdf::new();
    let rows = base(&mut p, 0);
    let mut s = as_srows(&rows);
    s.push((6, 1, p.pos(), 0));
    let x = p.xstream(6, W, &s, "/Size 7 /Root 1 0 R");
    p.end(x);
    // Claim more entries than the data holds.
    let t = patch(&p.b, "/Index [0 4 6 1]", "/Index [0 4 6 9]");
    let err = Document::from_bytes(&t).unwrap_err();
    assert!(matches!(err, Error::UnexpectedEof { .. }), "{err:?}");
}

#[test]
fn an_unknown_xref_stream_entry_type_is_refused() {
    let mut p = Pdf::new();
    let rows = base(&mut p, 0);
    let mut s = as_srows(&rows);
    s.push((5, 3, 0, 0));
    s.push((6, 1, p.pos(), 0));
    let x = p.xstream(6, W, &s, "/Size 7 /Root 1 0 R");
    p.end(x);
    let err = Document::from_bytes(&p.b).unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("entry type")),
        "{err:?}"
    );
}

#[test]
fn object_numbers_at_the_top_of_the_range() {
    // A subsection running past u32::MAX is refused; one ending at it is
    // read.
    let (mut p, rows) = simple_classic("");
    let x = p.pos();
    p.b.extend_from_slice(b"xref\n4294967295 2\n0000000000 00000 f \n0000000000 00000 f \ntrailer\n<< /Size 6 /Root 1 0 R >>\n");
    let _ = rows;
    p.end(x);
    assert!(matches!(
        Document::from_bytes(&p.b).unwrap_err(),
        Error::Parse(_)
    ));

    let (mut p, rows) = simple_classic("");
    let at = p.pos();
    let mut s = as_srows(&rows);
    s.push((6, 1, at, 0));
    s.push((u32::MAX, 0, 0, 0));
    let x = p.xstream(6, W, &s, "/Size 7 /Root 1 0 R");
    p.end(x);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 0), Some(100.0));
    assert_eq!(doc.resolve(r(u32::MAX, 0)).unwrap(), Object::Null);
}

#[test]
fn generations_outside_the_range_are_never_truncated() {
    // Classic in-use entry at generation 65541: refused, not read as
    // generation 5 (65541 mod 65536) — which would alias 5 5 R.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 5);
    let off = p.obj(5, 5, "[0 0 100 100]");
    let x = p.pos();
    rows.sort();
    p.b.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for (n, _, _, o) in rows.iter().skip(1) {
        let _ = n;
        p.b.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    p.b.extend_from_slice(format!("0000000000 65535 f \n{off:010} 65541 n \n").as_bytes());
    p.b.extend_from_slice(b"trailer\n<< /Size 6 /Root 1 0 R >>\n");
    p.end(x);
    let err = Document::from_bytes(&p.b).unwrap_err();
    assert!(
        matches!(err, Error::Parse(ref m) if m.contains("generation")),
        "{err:?}"
    );

    // A free entry above the range means "never reuse": accepted.
    let (mut p, mut rows) = simple_classic("");
    let x = p.pos();
    rows.sort();
    p.b.extend_from_slice(b"xref\n0 4\n");
    for (_, g, k, o) in &rows[..4] {
        p.b.extend_from_slice(format!("{o:010} {g:05} {} \n", *k as char).as_bytes());
    }
    let (_, g, k, o) = rows[4];
    p.b.extend_from_slice(format!("5 2\n{o:010} {g:05} {} \n", k as char).as_bytes());
    p.b.extend_from_slice(b"0000000000 65536 f \n");
    p.b.extend_from_slice(b"trailer\n<< /Size 7 /Root 1 0 R >>\n");
    p.end(x);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 0), Some(100.0));

    // Xref stream: a type 1 generation above 65535 and a type 2 object
    // stream number above u32::MAX are refused, not truncated.
    for (row, what) in [
        ((5u32, 1u8, 9u64, 65_541u64), "generation"),
        ((5, 2, 1 << 33, 0), "object stream"),
    ] {
        let mut p = Pdf::new();
        let rows = base(&mut p, 0);
        let mut s = as_srows(&rows);
        s.push(row);
        s.push((6, 1, p.pos(), 0));
        let x = p.xstream(6, [1, 8, 4], &s, "/Size 7 /Root 1 0 R");
        p.end(x);
        let err = Document::from_bytes(&p.b).unwrap_err();
        assert!(
            matches!(err, Error::Parse(ref m) if m.contains(what)),
            "{what}: {err:?}"
        );
    }
}

#[test]
fn bad_object_stream_references_fail_cleanly() {
    // Type 2 entries naming a missing object stream, a non-stream, and an
    // index past the stream's members: the object does not resolve, no
    // panic.
    for (objstm, index) in [(42u64, 0u64), (1, 0), (8, 7)] {
        let mut p = Pdf::new();
        let rows = base(&mut p, 0);
        let os = p.objstm(8, &[(9, "[0 0 1 1]")]);
        let mut s = as_srows(&rows);
        s.push((5, 2, objstm, index));
        s.push((8, 1, os, 0));
        s.push((6, 1, p.pos(), 0));
        let x = p.xstream(6, W, &s, "/Size 10 /Root 1 0 R");
        p.end(x);
        let doc = Document::from_bytes(&p.b);
        if let Ok(d) = doc {
            assert!(
                !matches!(d.resolve(r(5, 0)), Ok(Object::Array(_))),
                "objstm {objstm} index {index}"
            );
        }
    }
}

// -------------------------------------------------------------------
// 22–25: rewrite, incremental save, recovery, trailer
// -------------------------------------------------------------------

#[test]
fn rewrite_keeps_the_updated_object() {
    for b in [Kind::Classic, Kind::Stream, Kind::Hybrid] {
        let doc = Document::from_bytes(&two_revisions(Kind::Hybrid, b, true)).unwrap();
        let out = kasane_pdf::rewrite(&doc).unwrap();
        let doc2 = Document::from_bytes(&out).unwrap();
        assert_eq!(doc2.pages()[0].media_box().urx, 200.0, "{b:?}");
        assert!(
            !String::from_utf8_lossy(&out).contains("0 0 100 100"),
            "{b:?}: old body carried"
        );
    }
}

#[test]
fn incremental_save_after_a_hybrid_section_shows_the_edit() {
    // A text field held in an object stream that only the hybrid
    // section's /XRefStm lists; the edit replaces the field object under
    // the same number in a new classic section. v0.4.0 read the old
    // value back (the old /XRefStm overrode the new section).
    let mut p = Pdf::new();
    let a = p.obj(
        1,
        0,
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [7 0 R] >> >>",
    );
    let b = p.obj(2, 0, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let c = p.obj(
        3,
        0,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Annots [7 0 R] >>",
    );
    let rows = vec![
        (0, 65535, b'f', 0),
        (1, 0, b'n', a),
        (2, 0, b'n', b),
        (3, 0, b'n', c),
        (7, 0, b'f', 0),
    ];
    let x0 = p.classic(&rows, "<< /Size 9 /Root 1 0 R >>");
    p.end(x0);
    let os = p.objstm(
        8,
        &[(7, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (old) /Rect [0 0 50 20] /P 3 0 R >>")],
    );
    let at = p.pos();
    let xs = p.xstream(
        6,
        W,
        &[(6, 1, at, 0), (7, 2, 8, 0), (8, 1, os, 0)],
        "/Size 9",
    );
    let x1 = p.classic(
        &[(0, 65535, b'f', 0)],
        &format!("<< /Size 9 /Root 1 0 R /Prev {x0} /XRefStm {xs} >>"),
    );
    p.end(x1);
    let doc = Document::from_bytes(&p.b).unwrap();
    let mut ed = doc.edit().unwrap();
    ed.set_field_value("name", Object::String(b"new".to_vec()))
        .unwrap();
    let out = ed.save_incremental().unwrap();
    assert!(out.starts_with(&p.b));
    let doc2 = Document::from_bytes(&out).unwrap();
    let field = doc2.resolve(r(7, 0)).unwrap();
    let v = field
        .as_dict()
        .and_then(|d| d.get(b"V".as_slice()))
        .cloned();
    assert_eq!(v, Some(Object::String(b"new".to_vec())));
}

#[test]
fn recovery_still_takes_the_last_definition() {
    // A damaged two-revision file (no usable startxref): the recovery
    // scan keeps the last definition of each number, as before.
    let mut b = two_revisions(Kind::Classic, Kind::Classic, false);
    // The newest startxref points into the header.
    let at = b.windows(9).rposition(|w| w == b"startxref").unwrap() + 10;
    let end = at + b[at..].iter().position(|c| *c == b'\n').unwrap();
    b.splice(at..end, *b"5");
    assert!(Document::from_bytes(&b).is_err());
    let doc = Document::from_bytes_recovering(&b).unwrap();
    assert!(doc.recovery_mode());
    assert_eq!(width(&doc, 5, 0), Some(200.0));
}

#[test]
fn the_newest_trailer_decides_its_keys() {
    // Both trailers name an /Info; the newest one is used. A key only an
    // older trailer has is still available (lenient: §7.5.6 asks every
    // update's trailer to repeat it).
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    rows.push((8, 0, b'n', p.obj(8, 0, "<< /Title (old) >>")));
    let x1 = p.classic(
        &rows,
        "<< /Size 10 /Root 1 0 R /Info 8 0 R /ID [<01> <01>] >>",
    );
    p.end(x1);
    let info = p.obj(9, 0, "<< /Title (new) >>");
    let x2 = p.classic(
        &[(9, 0, b'n', info)],
        &format!("<< /Size 10 /Root 1 0 R /Info 9 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(
        doc.trailer().get(b"Info".as_slice()),
        Some(&Object::Ref(r(9, 0)))
    );
    assert!(doc.trailer().contains_key(b"ID".as_slice()));
}

#[test]
fn many_sections_naming_one_xrefstm_read_it_once() {
    // 2 000 classic sections, every one naming the same /XRefStm: the
    // stream supplies the hidden object once; the chain opens promptly
    // (no decode per section).
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    rows.push((5, 0, b'f', 0));
    let x0 = p.classic(&rows, "<< /Size 9 /Root 1 0 R >>");
    p.end(x0);
    let off5 = p.obj(5, 0, "[0 0 100 100]");
    let at = p.pos();
    let xs = p.xstream(6, W, &[(5, 1, off5, 0), (6, 1, at, 0)], "/Size 9");
    let mut prev = x0;
    for _ in 0..2000 {
        prev = p.classic(
            &[(0, 65535, b'f', 0)],
            &format!("<< /Size 9 /Root 1 0 R /Prev {prev} /XRefStm {xs} >>"),
        );
    }
    p.end(prev);
    let t = std::time::Instant::now();
    let doc = Document::from_bytes(&p.b).unwrap();
    assert_eq!(width(&doc, 5, 0), Some(100.0));
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
}

#[test]
fn the_audit_sees_bodies_an_old_xrefstm_listed() {
    // Object 5 is listed only by the old hybrid section's /XRefStm and
    // redefined by a newer classic section: its old body is reported as
    // superseded (the audit re-reads old sections' streams too).
    let doc = Document::from_bytes(&two_revisions(Kind::Hybrid, Kind::Classic, false)).unwrap();
    let audit = kasane_pdf::audit_residuals(&doc, &kasane_pdf::Limits::default()).unwrap();
    assert!(
        audit
            .findings
            .iter()
            .any(|f| f.kind == kasane_pdf::FindingKind::SupersededObject
                && f.object.map(|r| r.number) == Some(5)),
        "{:?}",
        audit
            .findings
            .iter()
            .map(|f| (f.kind, f.object))
            .collect::<Vec<_>>()
    );
    // A freed object's old body is superseded too, not live.
    let mut p = Pdf::new();
    let mut rows = base(&mut p, 0);
    rows.push((4, 0, b'n', contents_at(&mut p)));
    rows.push((5, 0, b'n', p.obj(5, 0, "[0 0 100 100]")));
    let x1 = p.classic(&rows, "<< /Size 6 /Root 1 0 R >>");
    p.end(x1);
    let x2 = p.classic(
        &[(4, 1, b'f', 0)],
        &format!("<< /Size 6 /Root 1 0 R /Prev {x1} >>"),
    );
    p.end(x2);
    let doc = Document::from_bytes(&p.b).unwrap();
    let audit = kasane_pdf::audit_residuals(&doc, &kasane_pdf::Limits::default()).unwrap();
    assert!(audit
        .findings
        .iter()
        .any(|f| f.kind == kasane_pdf::FindingKind::SupersededObject
            && f.object.map(|r| r.number) == Some(4)));
}
