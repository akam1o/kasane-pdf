//! Open a damaged file: strict parse first, then the explicit recovery
//! parse, reporting what the recovery assumed. Nothing is recovered
//! silently — a strictly parsed document has no report.
//!
//! ```console
//! cargo run --example recovery -- damaged.pdf
//! ```

use kasane_pdf::{Document, Error};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: recovery <input.pdf>")?;
    let data = std::fs::read(path)?;

    let doc = match Document::from_bytes(&data) {
        Ok(doc) => {
            println!("strict parse ok: {} page(s)", doc.page_count());
            doc
        }
        // Encrypted files and limit hits are refused in both modes;
        // only a structurally damaged file is worth the recovery scan.
        Err(e @ (Error::Encrypted | Error::LimitExceeded { .. } | Error::NotPdf)) => {
            return Err(e.into());
        }
        Err(e) => {
            println!("strict parse failed [{}]: {e}", e.kind());
            let doc = Document::from_bytes_recovering(&data)?;
            let r = doc
                .recovery()
                .expect("the strict parse failed, so the xref was rebuilt");
            println!(
                "recovered: {} object(s) found by scanning ({} inside object streams), \
                 trailer from {}, {} page(s)",
                r.objects_found,
                r.objects_from_streams,
                r.trailer_source,
                doc.page_count()
            );
            for w in &r.warnings {
                println!("  recovery note: {w}");
            }
            doc
        }
    };

    // Non-fatal findings accumulate on either kind of document.
    for w in doc.take_warnings() {
        println!("warning: {w}");
    }
    Ok(())
}
