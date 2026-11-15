//! Model-based non-interference tests for `redact`.
//!
//! A seeded generator builds documents whose meaning the test knows:
//! what each page draws, which of it lies inside the rect on page 1, and
//! which objects hold *secret* data — data drawn only inside the rect.
//! Every generated document comes in two variants that differ only in
//! that secret data (text, image samples, glyph procedures, appearance
//! drawings, alternate text, annotation text); structure, object
//! numbers and every random choice are the same. Around it the
//! generator adds what made past versions leak: shared resources and
//! forms, nested forms inheriting resources, annotations, a structure
//! tree, Type 3 fonts, references to secret objects from places that
//! draw nothing (private catalog and page entries, `/PieceInfo`, an
//! unused resource name on another page, an attachment), identical
//! copies under other numbers, dangling and wrong-generation references,
//! reference cycles, object and cross-reference streams, incremental
//! updates.
//!
//! Oracle: the two variants redact to byte-identical outputs or are
//! refused for the same reason — then nothing of the secret reaches the
//! output by any route. A success must also keep every public text
//! drawn outside the rect. Secrets have equal lengths in both variants,
//! so the position after removed text (which redaction keeps, by
//! contract) is the same.
//!
//! The same documents are also opened in recovery mode
//! (`Document::from_bytes_recovering`) — as they are, and with
//! `startxref` broken so that the cross-reference table is rebuilt by
//! scanning (incremental updates, object and cross-reference streams
//! included) — under the same oracle (docs/redaction-safety-model.md
//! § Recovery mode).
//!
//! `KASANE_MODEL_SEEDS` sets the number of seeds (default 160; the
//! recovery-mode runs take a quarter of them).

mod model;

use kasane_pdf::{extract_text, redact, Document, RedactOptions, TextOptions};
use model::{make, mask, Rng, RECT};

fn font() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/fonts/liberation/LiberationSans-Regular.ttf"
    ))
    .unwrap()
}

#[derive(Default, Debug)]
struct Tally {
    ok: u32,
    refused: u32,
}

/// How a generated document is opened.
#[derive(Clone, Copy, Debug)]
enum Open {
    Strict,
    /// `from_bytes_recovering` on the document as generated.
    Recovering,
    /// `from_bytes_recovering` with `startxref` past the end: the table
    /// is rebuilt by scanning.
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

fn check(seed: u64, annotations: bool, t: &mut Tally, font: &[u8]) {
    check_opened(seed, annotations, Open::Strict, t, font)
}

fn check_opened(seed: u64, annotations: bool, how: Open, t: &mut Tally, font: &[u8]) {
    // With annotations removed, every other seed removes scripts too.
    let opts = RedactOptions::new()
        .remove_annotations(annotations)
        .remove_javascript(annotations && seed % 2 == 1);
    let (a, public) = make(Rng::seeded(seed), 1, font);
    let (b, _) = make(Rng::seeded(seed), 2, font);
    assert!(a != b, "seed {seed}: the variants do not differ");
    let (a, b) = match how {
        Open::Rebuilt => (break_startxref(a), break_startxref(b)),
        _ => (a, b),
    };
    let run = |pdf: &[u8]| {
        let doc = match how {
            Open::Strict => Document::from_bytes(pdf),
            Open::Recovering | Open::Rebuilt => Document::from_bytes_recovering(pdf),
        }
        .unwrap_or_else(|e| panic!("seed {seed} ({how:?}): {e}"));
        if let Open::Rebuilt = how {
            assert!(
                doc.recovery().is_some(),
                "seed {seed}: the table was not rebuilt"
            );
        }
        redact(&doc, &[(0, vec![RECT])], &opts)
    };
    match (run(&a), run(&b)) {
        (Ok(x), Ok(y)) => {
            assert!(
                x.output == y.output,
                "seed {seed} (annotations {annotations}, {how:?}): the outputs differ — something \
                 of the secret reached the output"
            );
            let doc = Document::from_bytes(&x.output).unwrap();
            for (p, texts) in public.iter().enumerate() {
                let page = &doc.pages()[p];
                let got: String = extract_text(&doc, page, &TextOptions::default())
                    .unwrap()
                    .chars
                    .iter()
                    .filter_map(|c| c.unicode.as_deref())
                    .collect();
                for want in texts {
                    assert!(
                        got.contains(want.as_str()),
                        "seed {seed} ({how:?}): page {} lost public text {want}",
                        p + 1
                    );
                }
            }
            t.ok += 1;
        }
        (Err(x), Err(y)) => {
            assert_eq!(mask(&x), mask(&y), "seed {seed} ({how:?}): refused unlike");
            t.refused += 1;
            if std::env::var("KASANE_MODEL_VERBOSE").is_ok() {
                let m = mask(&x);
                eprintln!("refused: {}", &m[..m.len().min(110)]);
            }
        }
        (x, y) => panic!(
            "seed {seed} (annotations {annotations}, {how:?}): one redacted, one refused: {:?} / {:?}",
            x.map(|r| r.codes_removed).map_err(|e| e.to_string()),
            y.map(|r| r.codes_removed).map_err(|e| e.to_string())
        ),
    }
}

#[test]
fn generated_documents_are_non_interfering() {
    let seeds: u64 = std::env::var("KASANE_MODEL_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(160);
    let mut t = Tally::default();
    let font = font();
    for seed in 0..seeds {
        check(seed, false, &mut t, &font);
        check(seed, true, &mut t, &font);
    }
    eprintln!("model: {t:?}");
    // The generator must exercise successes, not only refusals.
    assert!(t.ok * 5 >= t.ok + t.refused, "too few successes: {t:?}");
}

#[test]
fn generated_documents_are_non_interfering_in_recovery_mode() {
    let seeds: u64 = std::env::var("KASANE_MODEL_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(160)
        / 4;
    let font = font();
    for how in [Open::Recovering, Open::Rebuilt] {
        let mut t = Tally::default();
        for seed in 0..seeds.max(1) {
            check_opened(seed, false, how, &mut t, &font);
            check_opened(seed, true, how, &mut t, &font);
        }
        eprintln!("model ({how:?}): {t:?}");
        assert!(
            t.ok * 5 >= t.ok + t.refused,
            "{how:?}: too few successes: {t:?}"
        );
    }
}
