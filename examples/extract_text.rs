//! Print the text of every page, one page per paragraph.
//!
//! ```console
//! cargo run --example extract_text -- input.pdf
//! ```

use kasane_pdf::{extract_text, Document, Limits, TextOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: extract_text <input.pdf>")?;
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    let limits = Limits::default();

    for page in doc.pages() {
        let text = extract_text(
            &doc,
            page,
            &TextOptions::default().with_limits(limits.clone()),
        )?;
        // Each `CharInfo` carries the character code, its Unicode
        // mapping (`None` when the font gives none — never a guess),
        // and its position in user space.
        let line: String = text
            .chars
            .iter()
            .filter_map(|c| c.unicode.as_deref())
            .collect();
        println!("--- page {} ---\n{}", page.index() + 1, line);
    }
    Ok(())
}
