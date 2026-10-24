//! Set the document title and add a square annotation, then save the
//! result as an incremental update — the original bytes are kept
//! verbatim and a new revision is appended.
//!
//! ```console
//! cargo run --example incremental_save -- input.pdf out.pdf
//! ```

use kasane_pdf::{signatures, Dict, Document, Object, Rect};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: incremental_save <input.pdf> <out.pdf>");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1])?;
    let doc = Document::from_bytes(&data)?;

    // Signed documents are protected: a metadata change is refused
    // whenever a signature exists, and other edits only when the
    // signer permitted them. Nothing here verifies a signature
    // cryptographically.
    for sig in signatures(&doc)? {
        println!(
            "signature field {:?}: range intact = {}, DocMDP P = {:?}",
            sig.field, sig.range_intact, sig.docmdp_p
        );
    }

    let mut edit = doc.edit()?;
    let mut info = Dict::new();
    info.insert(
        b"Title".to_vec(),
        Object::String(b"Annotated copy".to_vec()),
    );
    edit.set_info(info)?;
    edit.add_square(
        0,
        Rect {
            llx: 72.0,
            lly: 72.0,
            urx: 200.0,
            ury: 144.0,
        },
        [1.0, 0.0, 0.0],
        2.0,
    )?;

    let out = edit.save_incremental()?;
    assert!(out.starts_with(&data), "the original bytes are preserved");
    println!(
        "wrote {} ({} bytes appended)",
        args[2],
        out.len() - data.len()
    );
    std::fs::write(&args[2], out)?;
    Ok(())
}
