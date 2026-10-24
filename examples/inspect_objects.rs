//! Walk a document's structure through the typed object model: the
//! PDF version, each page's media box and annotation count, the first
//! content-stream operators, and the trailer `/Info` dictionary.
//!
//! ```console
//! cargo run --example inspect_objects -- input.pdf
//! ```

use kasane_pdf::{Document, Object};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: inspect_objects <input.pdf>")?;
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?; // strict; `from_bytes_recovering` for damaged files
    println!(
        "version {:?}, {} pages",
        doc.catalog_version()?,
        doc.page_count()
    );
    for page in doc.pages() {
        let dict = doc.resolve(page.object_ref().ok_or("direct page")?)?;
        let annots = match dict.as_dict().and_then(|d| d.get(b"Annots".as_slice())) {
            Some(a) => doc.deref(a)?.as_array().map(|a| a.len()).unwrap_or(0),
            None => 0,
        };
        let mb = page.media_box();
        println!(
            "page {}: {} x {} pt, {} annotation(s)",
            page.index() + 1,
            mb.urx - mb.llx,
            mb.ury - mb.lly,
            annots
        );
        for op in doc.page_operations(page)?.iter().take(3) {
            println!("  {op:?}");
        }
    }
    if let Some(info) = doc.trailer().get(b"Info".as_slice()) {
        if let Object::Dict(d) = doc.deref(info)? {
            println!("info keys: {}", d.len());
        }
    }
    Ok(())
}
