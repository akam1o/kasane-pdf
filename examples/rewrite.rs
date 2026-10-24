//! Rewrite a document as a fresh single-revision file — the reachable
//! object graph re-serialised with dense numbering, stream bytes kept
//! verbatim — and show what the rewrite refuses.
//!
//! ```console
//! cargo run --example rewrite -- input.pdf out.pdf
//! ```

use kasane_pdf::{rewrite, Document, Error};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: rewrite <input.pdf> <out.pdf>");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1])?;
    let doc = Document::from_bytes(&data)?;

    match rewrite(&doc) {
        Ok(out) => {
            // Not an incremental update: the output shares no bytes with
            // the input (use `Edit::save_incremental` to keep them) and
            // carries every reachable object verbatim — it is not a
            // sanitizer.
            std::fs::write(&args[2], &out)?;
            println!(
                "{} bytes -> {} bytes, {} page(s)",
                data.len(),
                out.len(),
                doc.page_count()
            );
            for w in doc.take_warnings() {
                println!("warning: {w}");
            }
        }
        // A signed document, PDF 2.0, XFA, … are refused with a typed
        // error before anything is written; the input is never touched.
        Err(Error::Unsupported { feature, detail }) => {
            println!("refused: {feature}: {detail}");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
