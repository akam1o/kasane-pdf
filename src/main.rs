//! `kasane-pdf` command-line interface (M0–M2).
//!
//! Loads a PDF file and prints basic document information: page count,
//! per-page sizes, and the trailer `/Info` dictionary keys. Two
//! subcommands inspect a single page: `contents` prints the decoded
//! content stream(s) and `operations` prints the parsed typed
//! operations — kept behind subcommands so normal inspection doesn't
//! dump file data. Exit code 0 on success, 1 on any error (the error
//! is printed with its category).

use kasane_pdf::{
    pixmap_to_png, render_page, Dict, Document, Error, Limits, Object, RenderOptions,
};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("kasane-pdf");

    match args.get(1).map(String::as_str) {
        Some("rewrite") => {
            let (input, output) = match (args.get(2), args.get(4)) {
                (Some(i), Some(o)) if args.get(3).map(|s| s.as_str()) == Some("--output") => (i, o),
                _ => {
                    eprintln!("usage: {prog} rewrite <file.pdf> --output <out.pdf>");
                    return ExitCode::from(2);
                }
            };
            match run_rewrite(input, output) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("annotate-rect") => {
            // annotate-rect <file.pdf> <page> --rect x0 y0 x1 y1
            //   --color r g b --width w --output out.pdf
            // (rect = lower-left x,y + upper-right x,y in PDF user
            //  space; color = RGB 0..1; width = stroke width, >0)
            let result = run_annotate_rect(&args[2..]);
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("inspect") => {
            // inspect <file.pdf> <page> — describe one page's structure.
            let (path, page_no) = match (args.get(2), args.get(3)) {
                (Some(p), Some(n)) => (p, n),
                _ => {
                    eprintln!("usage: {prog} inspect <file.pdf> <page-number>");
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
            match run_inspect(path, page_no) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
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
            eprintln!("       {prog} rewrite <file.pdf> --output <out.pdf>");
            eprintln!("       {prog} inspect <file.pdf> <page-number>");
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

/// `rewrite` subcommand: `rewrite <file.pdf> --output <out.pdf>`.
///
/// The output is written only on success — a failed rewrite never
/// leaves a partial file, and a pre-existing output is never touched
/// until the new bytes are complete. Input and output may not be the
/// same file (symlinks resolved before comparison).
fn run_rewrite(input: &str, output: &str) -> Result<(), Error> {
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let bytes = kasane_pdf::rewrite(&doc)?;
    write_atomic(
        &bytes,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!("rewrote {input} -> {output} ({} bytes)", bytes.len());
    Ok(())
}

/// `annotate-rect` subcommand:
/// `annotate-rect <in.pdf> <page> --rect x0 y0 x1 y1 --color r g b
///  --width w --output <out.pdf>`.
///
/// `rect` is lower-left x,y then upper-right x,y in un-rotated PDF
/// user space (+y up, CropBox/Rotate not applied) — the same space
/// M6.1 extraction reports. Page is 1-based like `contents`.
fn run_annotate_rect(args: &[String]) -> Result<(), Error> {
    // args[0]=input args[1]=page  then flag/value pairs.
    if args.len() < 2 {
        return Err(Error::Structure(annotate_usage()));
    }
    let input = &args[0];
    let page: usize = args[1]
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| Error::Structure(format!("invalid page number '{}'", args[1])))?;
    // Parse --rect x0 y0 x1 y1, --color r g b, --width w, --output f.
    let mut rect: Option<[f64; 4]> = None;
    let mut color: Option<[f64; 3]> = None;
    let mut width: Option<f64> = None;
    let mut output: Option<&str> = None;
    let mut i = 2;
    while i < args.len() {
        let need = |n: usize| -> Result<(), Error> {
            if i + n < args.len() {
                Ok(())
            } else {
                Err(Error::Structure(format!(
                    "flag '{}' expects {n} value(s)\n{}",
                    args[i],
                    annotate_usage()
                )))
            }
        };
        let num = |s: &str| -> Result<f64, Error> {
            s.parse::<f64>()
                .map_err(|_| Error::Structure(format!("expected a number, got '{s}'")))
        };
        match args[i].as_str() {
            "--rect" => {
                need(4)?;
                rect = Some([
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ]);
                i += 5;
            }
            "--color" => {
                need(3)?;
                color = Some([num(&args[i + 1])?, num(&args[i + 2])?, num(&args[i + 3])?]);
                i += 4;
            }
            "--width" => {
                need(1)?;
                width = Some(num(&args[i + 1])?);
                i += 2;
            }
            "--output" => {
                need(1)?;
                output = Some(&args[i + 1]);
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown flag '{other}'\n{}",
                    annotate_usage()
                )))
            }
        }
    }
    let (rect, color, width, output) = match (rect, color, width, output) {
        (Some(r), Some(c), Some(w), Some(o)) => (r, c, w, o),
        _ => return Err(Error::Structure(annotate_usage())),
    };

    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let mut e = doc.edit()?;
    e.add_square(
        page - 1,
        kasane_pdf::Rect {
            llx: rect[0],
            lly: rect[1],
            urx: rect[2],
            ury: rect[3],
        },
        color,
        width,
    )?;
    let bytes = e.rewrite()?;
    write_atomic(
        &bytes,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!(
        "annotated {input} page {page} -> {output} ({} bytes)",
        bytes.len()
    );
    Ok(())
}

fn annotate_usage() -> String {
    "usage: kasane-pdf annotate-rect <in.pdf> <page> --rect x0 y0 x1 y1 \
     --color r g b --width w --output <out.pdf>\n  rect: lower-left x y \
     then upper-right x y in PDF user space (1/72in, +y up)\n  color: R \
     G B each 0..1;  width: stroke width > 0"
        .into()
}

/// `inspect` subcommand: `inspect <file.pdf> <page-number>`.
///
/// Describe a single page's structure — a diagnostic aid for bug
/// reports. The file must contain exactly one page; `page-number`
/// selects it (1-based). Prints a fixed line order:
///
/// ```text
/// page <n>
/// mediabox <llx> <lly> <urx> <ury>
/// cropbox  <llx> <lly> <urx> <ury>
/// rotation <deg>
/// annotations <count>
/// fonts <count>
/// xobjects <count>
/// colorspaces <count>
/// patterns <count>
/// shadings <count>
/// extgstates <count>
/// procsets <count>
/// properties <count>
/// contents <ref> | contents [<ref> ...] | contents absent
/// userunit <v> | userunit absent
/// ```
///
/// MediaBox/CropBox/Rotate are the *inherited* values the page tree
/// already computed. `/Annots` must be an array (or a ref to one);
/// `/Contents` a ref or an array of refs; `/UserUnit` a number —
/// anything else is a structural error, not silently coerced.
fn run_inspect(path: &str, page_no: usize) -> Result<(), Error> {
    let data = std::fs::read(path)?;
    let doc = Document::from_bytes(&data)?;
    if doc.page_count() != 1 {
        return Err(Error::Structure(format!(
            "inspect expects a 1-page PDF, found {} pages",
            doc.page_count()
        )));
    }
    let page = doc
        .pages()
        .get(page_no - 1)
        .ok_or_else(|| Error::Structure(format!("page {page_no} does not exist")))?;

    let mb = page.media_box();
    let cb = page.crop_box();
    println!("page {}", page_no);
    println!(
        "mediabox {} {} {} {}",
        fmt_num(mb.llx),
        fmt_num(mb.lly),
        fmt_num(mb.urx),
        fmt_num(mb.ury)
    );
    println!(
        "cropbox {} {} {} {}",
        fmt_num(cb.llx),
        fmt_num(cb.lly),
        fmt_num(cb.urx),
        fmt_num(cb.ury)
    );
    println!("rotation {}", page.rotate());

    // /Annots: must be an array or an indirect ref to one.
    let page_obj = match page.object_ref() {
        Some(r) => doc.resolve(r)?,
        None => {
            return Err(Error::Structure(
                "page has no indirect reference — cannot read its dictionary".into(),
            ))
        }
    };
    let page_dict = page_obj
        .as_dict()
        .ok_or_else(|| Error::Structure("page object is not a dictionary".into()))?;
    let annots = match page_dict.get(b"Annots".as_slice()) {
        None => 0,
        Some(o) => {
            let resolved = doc.deref(o)?;
            match resolved {
                Object::Array(a) => a.len(),
                other => {
                    return Err(Error::Structure(format!(
                        "page /Annots is not an array ({other:?})"
                    )))
                }
            }
        }
    };
    println!("annotations {annots}");

    // Resource categories — count direct entries of each sub-dict.
    // /Resources may be absent (all zero) but if present must
    // resolve to a dictionary.
    let res_counts = |dict: Option<&Dict>| -> Result<[usize; 8], Error> {
        let d = match dict {
            None => return Ok([0; 8]),
            Some(d) => d,
        };
        let mut out = [0usize; 8];
        for (i, key) in [
            b"Font".as_slice(),
            b"XObject".as_slice(),
            b"ColorSpace".as_slice(),
            b"Pattern".as_slice(),
            b"Shading".as_slice(),
            b"ExtGState".as_slice(),
            b"ProcSet".as_slice(),
            b"Properties".as_slice(),
        ]
        .iter()
        .enumerate()
        {
            if let Some(v) = d.get(*key) {
                let res = doc.deref(v)?;
                out[i] = match res {
                    Object::Dict(dd) => dd.len(),
                    Object::Array(a) => a.len(),
                    _ => 0,
                };
            }
        }
        Ok(out)
    };
    let res_dict = match page.resources() {
        None => None,
        Some(o) => {
            let resolved = doc.deref(o)?;
            match resolved {
                Object::Dict(d) => Some(d.clone()),
                other => {
                    return Err(Error::Structure(format!(
                        "page /Resources is not a dictionary ({other:?})"
                    )))
                }
            }
        }
    };
    let c = res_counts(res_dict.as_ref())?;
    println!("fonts {}", c[0]);
    println!("xobjects {}", c[1]);
    println!("colorspaces {}", c[2]);
    println!("patterns {}", c[3]);
    println!("shadings {}", c[4]);
    println!("extgstates {}", c[5]);
    println!("procsets {}", c[6]);
    println!("properties {}", c[7]);

    // /Contents: ref, array of refs, or absent. `page.contents()`
    // is already single-level resolved — a `Ref` or an array of `Ref`s.
    match page.contents() {
        None => println!("contents absent"),
        Some(Object::Ref(r)) => println!("contents {r}"),
        Some(Object::Array(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for it in items {
                match it {
                    Object::Ref(r) => parts.push(r.to_string()),
                    other => {
                        return Err(Error::Structure(format!(
                            "page /Contents element is not an indirect reference ({other:?})"
                        )))
                    }
                }
            }
            println!("contents [{}]", parts.join(" "));
        }
        Some(other) => {
            return Err(Error::Structure(format!(
                "page /Contents is not a reference or an array ({other:?})"
            )))
        }
    }

    // /UserUnit: absent or a number.
    match page_dict.get(b"UserUnit".as_slice()) {
        None => println!("userunit absent"),
        Some(o) => {
            let resolved = doc.deref(o)?;
            match resolved.as_number() {
                Some(v) => println!("userunit {}", fmt_num(v)),
                None => {
                    return Err(Error::Structure(format!(
                        "page /UserUnit is not a number ({resolved:?})"
                    )))
                }
            }
        }
    }
    Ok(())
}

/// Format a number like the PDF spec writes it: integers without a
/// decimal point, everything else with full precision.
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Refuse when `input` and `output` resolve to the same file —
/// canonicalize + Unix (dev, ino) so relative paths, `.`/`..`,
/// symlinks and hardlinks all compare equal.
fn refuse_same_file(input: &str, output: &str) -> Result<(), Error> {
    let in_path = std::fs::canonicalize(input)
        .map_err(|e| Error::Structure(format!("cannot canonicalize input '{input}': {e}")))?;
    let out_path = std::fs::canonicalize(output).unwrap_or_else(|_| {
        std::path::Path::new(output)
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .map(|p| p.join(std::path::Path::new(output).file_name().unwrap_or_default()))
            .unwrap_or_else(|| std::path::PathBuf::from(output))
    });
    if in_path == out_path || same_file(&in_path, &out_path) {
        return Err(Error::Structure(
            "rewrite input and output are the same file".into(),
        ));
    }
    Ok(())
}

/// Write `bytes` to `tmp` (a name that must not already exist) and
/// then `rename` it over `output`. `create_new(true)` makes the
/// open fail on any existing name — regular file *or* symlink — so a
/// leftover tmp or a planted link is never followed or clobbered.
/// `sync_data` flushes before the rename; on any failure only `tmp`
/// (the file we created) is removed — `output` is never touched.
///
/// This is *atomic replace* plus best-effort durability: the rename
/// is atomic, but a power loss between rename and a later fsync of
/// the parent directory can still lose the update. That bound is
/// documented, not hidden.
fn write_atomic(bytes: &[u8], tmp: &str, output: &str) -> Result<(), Error> {
    use std::io::Write;
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Error::Structure(format!(
                "temporary output name '{tmp}' already exists — refusing to proceed"
            )));
        }
        Err(e) => return Err(e.into()),
    };
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_data()) {
        drop(f);
        let _ = std::fs::remove_file(tmp);
        return Err(e.into());
    }
    drop(f);
    if let Err(e) = std::fs::rename(tmp, output) {
        let _ = std::fs::remove_file(tmp);
        return Err(e.into());
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

/// Unix-only file identity — (dev, ino) identifies the inode, so two
/// hardlink names resolve to the same pair. Returns false on any
/// metadata failure (the path may not exist yet for the output).
#[cfg(unix)]
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => (ma.dev(), ma.ino()) == (mb.dev(), mb.ino()),
        _ => false,
    }
}
#[cfg(not(unix))]
fn same_file(_a: &std::path::Path, _b: &std::path::Path) -> bool {
    false
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

#[cfg(test)]
mod tests {
    //! Unit tests for the atomic-write helper — the CLI-level
    //! behaviour is covered by `tests/m8_cli.rs`; here we pin down
    //! `create_new` semantics that a PID-named child can't hit
    //! deterministically.

    use super::write_atomic;
    use std::fs;

    fn tmp(name: &str) -> (std::path::PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("kasane-atomic-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("out.kasane-tmp");
        let out = dir.join("out");
        (
            dir,
            tmp.to_string_lossy().into_owned(),
            out.to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn atomic_write_refuses_existing_tmp() {
        let (_dir, tmp, out) = tmp("exists");
        fs::write(&tmp, b"pre-existing").unwrap();
        // Even valid bytes must fail — we never overwrite a leftover.
        let r = write_atomic(b"payload", &tmp, &out);
        assert!(r.is_err());
        assert_eq!(fs::read(&tmp).unwrap(), b"pre-existing");
        assert!(!std::path::Path::new(&out).exists());
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_tmp_symlink() {
        let (_dir, tmp, out) = tmp("symlink");
        let victim = std::path::Path::new(&tmp).with_file_name("victim");
        fs::write(&victim, b"IMPORTANT").unwrap();
        std::os::unix::fs::symlink(&victim, &tmp).unwrap();
        let r = write_atomic(b"payload", &tmp, &out);
        assert!(r.is_err());
        // The symlink target was never written to.
        assert_eq!(fs::read(&victim).unwrap(), b"IMPORTANT");
        assert!(!std::path::Path::new(&out).exists());
    }

    #[test]
    fn atomic_write_happy_path() {
        let (_dir, tmp, out) = tmp("ok");
        write_atomic(b"payload", &tmp, &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"payload");
        assert!(!std::path::Path::new(&tmp).exists(), "tmp not cleaned up");
    }

    #[test]
    fn atomic_write_rename_failure_cleans_tmp() {
        // Output path is a directory → rename fails; tmp is removed,
        // the directory is untouched.
        let (dir, tmp, _out) = tmp("rename-fail");
        let dirpath = dir.join("outdir");
        fs::create_dir(&dirpath).unwrap();
        let r = write_atomic(b"payload", &tmp, dirpath.to_str().unwrap());
        assert!(r.is_err());
        assert!(!std::path::Path::new(&tmp).exists());
        assert!(dirpath.is_dir());
    }
}
