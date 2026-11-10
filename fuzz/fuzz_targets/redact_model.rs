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
#![no_main]

#[path = "../../tests/model/mod.rs"]
mod model;

use kasane_pdf::{extract_text, redact, Document, RedactOptions, TextOptions};
use libfuzzer_sys::fuzz_target;
use model::{make, mask, Rng, RECT};

const FONT: &[u8] = include_bytes!("../../fonts/liberation/LiberationSans-Regular.ttf");

fuzz_target!(|data: &[u8]| {
    let (annotations, data) = match data.split_first() {
        Some((b, rest)) => (b & 1 == 1, rest),
        None => (false, data),
    };
    let (a, public) = make(Rng::from_bytes(data), 1, FONT);
    let (b, _) = make(Rng::from_bytes(data), 2, FONT);
    let opts = RedactOptions::new()
        .remove_annotations(annotations)
        .with_limits(kasane_pdf_fuzz::limits());
    let run = |pdf: &[u8]| {
        let doc = Document::from_bytes(pdf).expect("a generated document parses");
        redact(&doc, &[(0, vec![RECT])], &opts)
    };
    match (run(&a), run(&b)) {
        (Ok(x), Ok(y)) => {
            assert!(x.output == y.output, "the outputs differ: something of the secret reached the output");
            let doc = Document::from_bytes(&x.output).expect("the output parses");
            for (p, texts) in public.iter().enumerate() {
                let got: String = extract_text(&doc, &doc.pages()[p], &TextOptions::default())
                    .expect("the output's text extracts")
                    .chars
                    .iter()
                    .filter_map(|c| c.unicode.as_deref())
                    .collect();
                for want in texts {
                    assert!(got.contains(want.as_str()), "page {} lost public text {want}", p + 1);
                }
            }
        }
        (Err(x), Err(y)) => assert_eq!(mask(&x), mask(&y), "refused unlike"),
        (x, y) => panic!(
            "one redacted, one refused: {:?} / {:?}",
            x.map(|r| r.codes_removed).map_err(|e| e.to_string()),
            y.map(|r| r.codes_removed).map_err(|e| e.to_string())
        ),
    }
});
