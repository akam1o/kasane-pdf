//! Render one page of a PDF to a PNG file.
//!
//! ```console
//! cargo run --example render_page -- input.pdf 1 out.png [dpi]
//! ```

use kasane_pdf::{render_page, Document, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: render_page <input.pdf> <page-number> <out.png> [dpi]");
        std::process::exit(2);
    }
    let dpi: f64 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(96.0);

    let data = std::fs::read(&args[1])?;
    let doc = Document::from_bytes(&data)?;
    let page_no: usize = args[2].parse()?;
    let page = doc
        .pages()
        .get(page_no.wrapping_sub(1))
        .ok_or("page number out of range")?;

    let opts = RenderOptions::default().with_dpi(dpi);
    let rendered = render_page(&doc, page, &opts)?;
    std::fs::write(&args[3], rendered.to_png()?)?;

    println!(
        "{}x{} px at {} dpi",
        rendered.width(),
        rendered.height(),
        dpi
    );
    // A render that used a bundled substitute for a non-embedded font
    // is reported, so it is never mistaken for a faithful one.
    for s in rendered.font_substitutions() {
        println!("substituted font: {} -> {}", s.base_font, s.substitute);
    }
    Ok(())
}
