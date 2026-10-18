//! `kasane-pdf` command-line interface (M0–M2).
//!
//! Loads a PDF file and prints basic document information: page count,
//! per-page sizes, and the trailer `/Info` dictionary keys. Two
//! subcommands inspect a single page: `contents` prints the decoded
//! content stream(s) and `operations` prints the parsed typed
//! operations — kept behind subcommands so normal inspection doesn't
//! dump file data. Exit code 0 on success, 1 on any error (the error
//! is printed with its category).

use kasane_pdf::{Document, Error, Object};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("kasane-pdf");

    match args.get(1).map(String::as_str) {
        Some(cmd @ ("contents" | "operations")) => {
            let (path, page_no) = match (args.get(2), args.get(3)) {
                (Some(p), Some(n)) => (p, n),
                _ => {
                    eprintln!("usage: {prog} {cmd} <file.pdf> <page-number>");
                    return ExitCode::from(2);
                }
            };
            let page_no: usize = match page_no.parse() {
                Ok(n) if n >= 1 => n,
                _ => {
                    eprintln!("error: <page-number> must be a positive integer");
                    return ExitCode::from(2);
                }
            };
            let result = if cmd == "contents" {
                run_contents(path, page_no)
            } else {
                run_operations(path, page_no)
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some(path) if !path.starts_with('-') => match run(path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error [{}]: {e}", e.kind());
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: {prog} <file.pdf>");
            eprintln!("       {prog} contents <file.pdf> <page-number>");
            eprintln!("       {prog} operations <file.pdf> <page-number>");
            ExitCode::from(2)
        }
    }
}

fn run(path: &str) -> Result<(), Error> {
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;

    println!("file: {path}");
    println!("pages: {}", doc.page_count());
    for page in doc.pages() {
        let mb = page.media_box();
        let cb = page.crop_box();
        let mut line = format!(
            "  page {}: media box {} ({} x {} pts)",
            page.index() + 1,
            mb,
            mb.width(),
            mb.height()
        );
        if cb != mb {
            line.push_str(&format!(", crop box {cb}"));
        }
        if page.rotate() != 0 {
            line.push_str(&format!(", rotate {}", page.rotate()));
        }
        if let Some(r) = page.object_ref() {
            line.push_str(&format!("  ({r})"));
        }
        println!("{line}");
    }

    if let Some(info) = doc.trailer().get(b"Info".as_slice()) {
        match doc.deref(info) {
            Ok(Object::Dict(d)) => {
                println!(
                    "info keys: {}",
                    d.keys()
                        .map(|k| String::from_utf8_lossy(k))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            _ => println!("info: (non-dictionary)"),
        }
    }
    Ok(())
}

fn run_contents(path: &str, page_no: usize) -> Result<(), Error> {
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    let page = doc
        .pages()
        .get(page_no - 1)
        .ok_or_else(|| Error::Structure(format!("page {page_no} does not exist")))?;
    let contents = doc.page_contents(page)?;
    println!("file: {path}");
    println!("page: {page_no} ({} content stream(s))", contents.len());
    for (i, body) in contents.iter().enumerate() {
        println!("--- stream {} ({} bytes) ---", i + 1, body.len());
        // Print only the beginning to avoid dumping large binary data.
        let preview = if body.len() > 512 { &body[..512] } else { body };
        println!("{}", String::from_utf8_lossy(preview));
        if body.len() > 512 {
            println!("... ({} more bytes)", body.len() - 512);
        }
    }
    Ok(())
}

/// Maximum operations printed by the `operations` subcommand; longer
/// pages are truncated with a summary line so output stays bounded.
const MAX_PRINTED_OPERATIONS: usize = 10_000;

fn run_operations(path: &str, page_no: usize) -> Result<(), Error> {
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    let page = doc
        .pages()
        .get(page_no - 1)
        .ok_or_else(|| Error::Structure(format!("page {page_no} does not exist")))?;
    let ops = doc.page_operations(page)?;
    println!("file: {path}");
    println!("page: {page_no} ({} operation(s))", ops.len());
    for (i, op) in ops.iter().take(MAX_PRINTED_OPERATIONS).enumerate() {
        println!("{i:5}  {op}");
    }
    if ops.len() > MAX_PRINTED_OPERATIONS {
        println!(
            "... ({} more operation(s))",
            ops.len() - MAX_PRINTED_OPERATIONS
        );
    }
    Ok(())
}
