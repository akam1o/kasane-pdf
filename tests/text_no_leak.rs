//! Text extraction frees everything it allocates: repeated calls do not
//! grow the live heap — on success, on the error path, on a recovered
//! and on an encrypted document, with annotations, and when the document
//! is opened anew each time. (Every published version up to 0.3.1
//! leaked a 36-byte placeholder pixmap per `extract_text` call.)
//!
//! Each scenario is warmed up first (font and stream caches), then run
//! twice 500 times; the heap may not grow in the second half — a leak
//! grows linearly, a cache does not.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use kasane_pdf::{extract_text, Document, OpenOptions, TextOptions};

struct Counting;
static LIVE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// Live-heap growth over 500 calls after 520 warm-up calls.
fn growth(mut f: impl FnMut()) -> isize {
    for _ in 0..520 {
        f();
    }
    let before = LIVE.load(Ordering::Relaxed);
    for _ in 0..500 {
        f();
    }
    LIVE.load(Ordering::Relaxed) - before
}

#[test]
fn repeated_extraction_does_not_grow_the_heap() {
    // One test, so no other test thread allocates meanwhile.
    let mut bad = Vec::new();
    let mut check = |name: &str, g: isize| {
        if g > 0 {
            bad.push(format!("{name}: +{g} bytes over 500 calls"));
        }
    };

    let plain = fixture("text_basic.pdf");
    let doc = Document::from_bytes(&plain).unwrap();
    let page = doc.pages()[0].clone();
    check(
        "plain",
        growth(|| {
            extract_text(&doc, &page, &TextOptions::default()).unwrap();
        }),
    );
    check(
        "opened anew each call",
        growth(|| {
            let d = Document::from_bytes(&plain).unwrap();
            let p = d.pages()[0].clone();
            extract_text(&d, &p, &TextOptions::default()).unwrap();
        }),
    );

    let failing = fixture("text_missing_font_ref.pdf");
    let dfail = Document::from_bytes(&failing).unwrap();
    let pfail = dfail.pages()[0].clone();
    assert!(extract_text(&dfail, &pfail, &TextOptions::default()).is_err());
    check(
        "error path",
        growth(|| {
            let _ = extract_text(&dfail, &pfail, &TextOptions::default());
        }),
    );

    let damaged = fixture("xref_stream_bad_type.pdf");
    let drec = Document::from_bytes_recovering(&damaged).unwrap();
    let prec = drec.pages()[0].clone();
    check(
        "recovered document",
        growth(|| {
            let _ = extract_text(&drec, &prec, &TextOptions::default());
        }),
    );

    let encrypted = fixture("encrypted/enc_r6_aes256_empty.pdf");
    let denc = Document::open(&encrypted, &OpenOptions::new()).unwrap();
    let penc = denc.pages()[0].clone();
    check(
        "encrypted document",
        growth(|| {
            let _ = extract_text(&denc, &penc, &TextOptions::default());
        }),
    );

    let annotated = fixture("render_annot.pdf");
    let dann = Document::from_bytes(&annotated).unwrap();
    let pann = dann.pages()[0].clone();
    check(
        "with annotations",
        growth(|| {
            let _ = extract_text(&dann, &pann, &TextOptions::new().with_annotations(true));
        }),
    );

    assert!(bad.is_empty(), "\n{}", bad.join("\n"));
}
