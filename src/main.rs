//! `kasane-pdf` command-line interface (M0–M2).
//!
//! Loads a PDF file and prints basic document information: page count,
//! per-page sizes, and the trailer `/Info` dictionary keys. Two
//! subcommands inspect a single page: `contents` prints the decoded
//! content stream(s) and `operations` prints the parsed typed
//! operations — kept behind subcommands so normal inspection doesn't
//! dump file data. Exit code 0 on success, 1 on any error (the error
//! is printed with its category).

use kasane_pdf::{pixmap_to_png, render_page, Document, Error, Limits, Object, RenderOptions};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("kasane-pdf");

    match args.get(1).map(String::as_str) {
        Some(cmd @ ("contents" | "operations" | "render" | "text")) => {
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
            let result = match cmd {
                "contents" => run_contents(path, page_no),
                "operations" => run_operations(path, page_no),
                "render" => run_render(path, page_no, &args[4..]),
                "text" => run_text(path, page_no, &args[4..]),
                _ => unreachable!(),
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
            eprintln!(
                "       {prog} render <file.pdf> <page-number> --output <file.png> [--dpi <dpi>]"
            );
            eprintln!("       {prog} text <file.pdf> <page-number> [--format json|lines]");
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

/// `render` subcommand: `render <file.pdf> <page> --output <png> [--dpi <d>]`.
/// The output PNG is written only on success — a failed render never
/// leaves a partial file. An existing file is overwritten.
fn run_render(path: &str, page_no: usize, args: &[String]) -> Result<(), Error> {
    let mut output: Option<String> = None;
    let mut dpi: f64 = 72.0;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                output = args.get(i + 1).cloned();
                i += 2;
            }
            "--dpi" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| Error::Structure("--dpi requires a value".into()))?;
                dpi = v
                    .parse::<f64>()
                    .map_err(|_| Error::Structure(format!("invalid --dpi value '{v}'")))?;
                i += 2;
            }
            other => {
                eprintln!("error: unknown render option '{other}'");
                return Err(Error::Structure(format!("unknown option '{other}'")));
            }
        }
    }
    let output =
        output.ok_or_else(|| Error::Structure("render requires --output <file.png>".into()))?;

    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    let page = doc
        .pages()
        .get(page_no - 1)
        .ok_or_else(|| Error::Structure(format!("page {page_no} does not exist")))?;

    let opts = RenderOptions {
        dpi,
        limits: Limits::default(),
    };
    let rendered = render_page(&doc, page, &opts)?;
    let png = pixmap_to_png(&rendered.pixmap)?;
    std::fs::write(&output, png)?;
    println!(
        "rendered page {} -> {} ({}x{} px, {} pts, dpi {})",
        page_no, output, rendered.width, rendered.height, rendered.page_size_pts.0, rendered.dpi
    );
    Ok(())
}

/// `text` subcommand: `text <file.pdf> <page> [--format json|lines]`.
/// Prints each extracted character in content order with its
/// resolved Unicode value and page-space geometry.
fn run_text(path: &str, page_no: usize, args: &[String]) -> Result<(), Error> {
    let mut format = "json";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                format = args
                    .get(i + 1)
                    .map(|s| s.as_str())
                    .ok_or_else(|| Error::Structure("--format requires a value".into()))?;
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!("unknown option '{other}'")));
            }
        }
    }
    if format != "json" && format != "lines" {
        return Err(Error::Structure(format!(
            "unknown --format '{format}' (json|lines)"
        )));
    }

    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    let page = doc
        .pages()
        .get(page_no - 1)
        .ok_or_else(|| Error::Structure(format!("page {page_no} does not exist")))?;

    let text = kasane_pdf::extract_text(&doc, page, &Limits::default())?;
    match format {
        "json" => print_json(&text),
        "lines" => print_lines(&text),
        _ => unreachable!(),
    }
    Ok(())
}

/// CLI output caps — the `text` command prints at most this many
/// entries and caps each unicode string; truncation is always stated.
const MAX_TEXT_ENTRIES: usize = 50_000;
const MAX_TEXT_UNICODE_CHARS: usize = 512;

fn clip_unicode(s: &str) -> (String, bool) {
    if s.chars().count() <= MAX_TEXT_UNICODE_CHARS {
        return (s.to_string(), false);
    }
    (s.chars().take(MAX_TEXT_UNICODE_CHARS).collect(), true)
}

fn print_json(t: &kasane_pdf::ExtractedText) {
    let shown = t.chars.len().min(MAX_TEXT_ENTRIES);
    let mut out = String::from("[\n");
    for (i, c) in t.chars.iter().take(shown).enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  {");
        out.push_str(&format!("\"code\": {}, ", c.code));
        match &c.unicode {
            Some(s) => {
                let (u, clipped) = clip_unicode(s);
                out.push_str(&format!("\"unicode\": {:?}, ", u));
                if clipped {
                    out.push_str("\"truncated\": true, ");
                }
            }
            None => out.push_str("\"unicode\": null, "),
        }
        out.push_str(&format!("\"via\": {:?}, ", c.resolved_via));
        out.push_str(&format!(
            "\"origin\": [{:.2}, {:.2}], \"advance\": [{:.2}, {:.2}], ",
            c.origin.0, c.origin.1, c.advance.0, c.advance.1
        ));
        match c.bounds {
            Some(b) => out.push_str(&format!(
                "\"bounds\": [{:.2}, {:.2}, {:.2}, {:.2}], ",
                b.0, b.1, b.2, b.3
            )),
            None => out.push_str("\"bounds\": null, "),
        }
        out.push_str(&format!(
            "\"mode\": {}, \"size\": {:.2}, \"rise\": {:.2}, \"tj\": {:.2}",
            c.render_mode, c.font_size, c.rise, c.tj_adjust
        ));
        out.push('}');
    }
    if shown < t.chars.len() {
        if shown > 0 {
            out.push_str(",\n");
        }
        out.push_str(&format!(
            "  {{\"_omitted\": {}, \"_note\": \"output capped at {MAX_TEXT_ENTRIES} entries\"}}",
            t.chars.len() - shown
        ));
    }
    out.push_str("\n]\n");
    print!("{out}");
}

fn print_lines(t: &kasane_pdf::ExtractedText) {
    let shown = t.chars.len().min(MAX_TEXT_ENTRIES);
    for c in t.chars.iter().take(shown) {
        let u = c.unicode.as_deref().unwrap_or("<unresolved>");
        println!(
            "0x{:04X} {:?} at ({:.1},{:.1}) adv({:.1},{:.1}) mode={} size={:.1}",
            c.code, u, c.origin.0, c.origin.1, c.advance.0, c.advance.1, c.render_mode, c.font_size
        );
    }
    if shown < t.chars.len() {
        println!(
            "... ({} more character(s) omitted — output capped at {MAX_TEXT_ENTRIES})",
            t.chars.len() - shown
        );
    }
}
