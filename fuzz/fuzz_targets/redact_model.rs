//! `redact` on the generated documents of `tests/redact_model.rs`, the
//! input's bytes steering every structural choice (pages, elements,
//! forms and their nesting, annotations, a structure tree, Type 3 fonts,
//! references to secret objects from places that draw nothing, copies,
//! dangling and wrong-generation references, cycles, object streams,
//! incremental updates).
//! Oracle (a false success, not only a crash): the two variants — they
//! differ only in what lies inside the rect — redact to byte-identical
//! outputs or are refused for the same reason, and a success keeps
//! every public text drawn outside the rect.
//! When the first byte has bits 2 and 3 set (a quarter of the inputs),
//! the variants are checked again opened in recovery mode — as
//! generated (bit 4 clear) or with `startxref` broken so that the table
//! is rebuilt by scanning (bit 4 set) — under the same oracle
//! (docs/redaction-safety-model.md § Recovery mode).
#![no_main]

#[path = "../../tests/model/mod.rs"]
mod model;

use kasane_pdf::{extract_text, redact, Document, RedactOptions, TextOptions};
use libfuzzer_sys::fuzz_target;
use model::{make, mask, Rng, RECT};

const FONT: &[u8] = include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf");

/// How the variants are opened.
#[derive(Clone, Copy, Debug)]
enum Open {
    Strict,
    Recovering,
    Rebuilt,
}

fn break_startxref(mut pdf: Vec<u8>) -> Vec<u8> {
    let i = pdf
        .windows(10)
        .rposition(|w| w == b"startxref\n")
        .expect("startxref")
        + 10;
    pdf.truncate(i);
    pdf.extend_from_slice(b"999999999\n%%EOF\n");
    pdf
}

fn check(a: &[u8], b: &[u8], public: &[Vec<String>], opts: &RedactOptions, how: Open) {
    let (a, b) = match how {
        Open::Rebuilt => (break_startxref(a.to_vec()), break_startxref(b.to_vec())),
        _ => (a.to_vec(), b.to_vec()),
    };
    let run = |pdf: &[u8]| {
        let doc = match how {
            Open::Strict => Document::from_bytes(pdf).expect("a generated document parses"),
            Open::Recovering | Open::Rebuilt => {
                let doc = Document::from_bytes_recovering(pdf)
                    .expect("a generated document parses in recovery mode");
                assert!(
                    !matches!(how, Open::Rebuilt) || doc.recovery().is_some(),
                    "the table was not rebuilt"
                );
                doc
            }
        };
        redact(&doc, &[(0, vec![RECT])], opts)
    };
    match (run(&a), run(&b)) {
        (Ok(x), Ok(y)) => {
            assert!(
                x.output == y.output,
                "{how:?}: the outputs differ: something of the secret reached the output"
            );
            let doc = Document::from_bytes(&x.output).expect("the output parses");
            for (p, texts) in public.iter().enumerate() {
                let got: String = extract_text(&doc, &doc.pages()[p], &TextOptions::default())
                    .expect("the output's text extracts")
                    .chars
                    .iter()
                    .filter_map(|c| c.unicode.as_deref())
                    .collect();
                for want in texts {
                    assert!(
                        got.contains(want.as_str()),
                        "{how:?}: page {} lost public text {want}",
                        p + 1
                    );
                }
            }
        }
        (Err(x), Err(y)) => assert_eq!(mask(&x), mask(&y), "{how:?}: refused unlike"),
        (x, y) => panic!(
            "{how:?}: one redacted, one refused: {:?} / {:?}",
            x.map(|r| r.codes_removed).map_err(|e| e.to_string()),
            y.map(|r| r.codes_removed).map_err(|e| e.to_string())
        ),
    }
}

fuzz_target!(|data: &[u8]| {
    let (first, data) = match data.split_first() {
        Some((b, rest)) => (*b, rest),
        None => (0, data),
    };
    let (annotations, javascript) = (first & 1 == 1, first & 3 == 3);
    let (a, public) = make(Rng::from_bytes(data), 1, FONT);
    let (b, _) = make(Rng::from_bytes(data), 2, FONT);
    let opts = RedactOptions::new()
        .remove_annotations(annotations)
        .remove_javascript(javascript)
        .with_limits(kasane_pdf_fuzz::limits());
    check(&a, &b, &public, &opts, Open::Strict);
    if first & 12 == 12 {
        let how = if first & 16 == 16 {
            Open::Rebuilt
        } else {
            Open::Recovering
        };
        check(&a, &b, &public, &opts, how);
    }
});
