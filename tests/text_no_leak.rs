//! Text extraction frees everything it allocates: repeated calls on one
//! document do not grow the live heap (a placeholder pixmap used to be
//! leaked on every call).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

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

#[test]
fn repeated_extraction_does_not_grow_the_heap() {
    let data = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/text_basic.pdf"
    ))
    .unwrap();
    let doc = kasane_pdf::Document::from_bytes(&data).unwrap();
    let page = doc.pages()[0].clone();
    let opts = kasane_pdf::TextOptions::default();
    // Warm up caches (fonts, decoded streams) first.
    for _ in 0..5 {
        kasane_pdf::extract_text(&doc, &page, &opts).unwrap();
    }
    let before = LIVE.load(Ordering::Relaxed);
    for _ in 0..2000 {
        kasane_pdf::extract_text(&doc, &page, &opts).unwrap();
    }
    let grown = LIVE.load(Ordering::Relaxed) - before;
    assert!(
        grown < 4096,
        "live heap grew by {grown} bytes over 2000 extractions"
    );
}
