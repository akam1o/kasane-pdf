//! Render a page whose Japanese CID font is not embedded. With the
//! `cjk-fallback` feature the Adobe-Japan1 predefined CMaps and the
//! bundled Noto Sans JP face stand in for it; the substitution is
//! reported on the result so the render is never mistaken for a
//! faithful one. Without the feature the same page is refused.
//!
//! ```console
//! cargo run --example cjk_fallback --features cjk-fallback -- japanese.pdf out.png
//! ```

use kasane_pdf::{
    cjk_fallback_enabled, pixmap_to_png, render_page, Document, Error, RenderOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: cjk_fallback <input.pdf> <out.png>");
        std::process::exit(2);
    }
    println!(
        "cjk-fallback compiled in: {} (predefined CMaps bundled: {})",
        cjk_fallback_enabled(),
        kasane_pdf::predefined_cmap_names().len()
    );
    let doc = Document::from_bytes(&std::fs::read(&args[1])?)?;
    match render_page(&doc, &doc.pages()[0], &RenderOptions::default()) {
        Ok(page) => {
            std::fs::write(&args[2], pixmap_to_png(&page.pixmap)?)?;
            for s in &page.font_substitutions {
                // `SubstKind::CjkFallback`: shapes from Noto Sans JP,
                // advances from the PDF's own /W and /DW.
                println!(
                    "substituted: {} -> {} ({:?})",
                    s.base_font, s.substitute, s.kind
                );
            }
            if page.font_substitutions.is_empty() {
                println!("every font was embedded; no fallback used");
            }
        }
        Err(Error::Unsupported { feature, detail }) => {
            println!("refused: {feature}: {detail}");
            if !cjk_fallback_enabled() {
                println!(
                    "rebuild with `--features cjk-fallback` to render non-embedded Japan1 fonts"
                );
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
