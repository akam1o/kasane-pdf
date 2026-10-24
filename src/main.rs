//! `kasane-pdf` command-line interface.
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
        Some("preflight-redaction") => {
            // preflight-redaction <file.pdf> <page> --rect llx lly urx ury
            //   [--rect …] [--format text|json]
            // Diagnose what a *future* redaction would face — never
            // modifies the file, and a clean report is NOT a "safe to
            // redact" verdict.
            match run_preflight(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("experimental-remove-text") => {
            // experimental-remove-text <file.pdf> <page>
            //   --rect llx lly urx ury [--rect …] --output <out.pdf>
            // Experimental scoped text removal — NOT a redaction tool:
            // removes the shown codes whose filled ink bounds hit a
            // rect and preserves the survivors' positions. See the
            // `remove_text` module docs for the full refusal list.
            match run_remove_text(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("experimental-remove-image-region") => {
            // experimental-remove-image-region <file.pdf> <page>
            //   --rect llx lly urx ury [--rect …] --output <out.pdf>
            // Experimental image-region removal — NOT a redaction tool:
            // replaces image samples whose painted positions hit a rect
            // with opaque black in a *new* image object; the original
            // stream is never mutated. See `src/remove_image.rs` for the
            // full refusal list.
            match run_remove_image_region(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("experimental-remove-region") => {
            // experimental-remove-region <file.pdf> <page>
            //   --rect llx lly urx ury [--rect …] --output <out.pdf>
            // Experimental region removal — NOT a redaction tool:
            // removes text codes whose bounds hit a rect *and* blackens
            // image samples whose painted positions hit the same rect,
            // in one pass. See `src/remove_region.rs` for the full
            // refusal list.
            match run_remove_region(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("experimental-remove-regions") => {
            // experimental-remove-regions <file.pdf>
            //   --page <n> --rect llx lly urx ury [--rect …]
            //   [--page <m> --rect …] --output <out.pdf>
            // Same experimental edit across multiple pages in one
            // session — NOT a redaction tool.
            match run_remove_regions(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("remove-document-metadata") => {
            // remove-document-metadata <file.pdf> --output <out.pdf>
            // Removes the trailer /Info and catalog /Metadata
            // *entries* — not a metadata scrubber or redaction tool.
            match run_remove_document_metadata(&args[2..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error [{}]: {e}", e.kind());
                    ExitCode::FAILURE
                }
            }
        }
        Some("audit-residuals") => {
            // audit-residuals <file.pdf> [--format text|json]
            // Read-only inventory of residual information — never
            // modifies the file, and a clean report is NOT a
            // "safe to distribute" verdict.
            match run_audit(&args[2..]) {
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
            eprintln!(
                "       {prog} preflight-redaction <file.pdf> <page-number> --rect llx lly urx ury [--rect …] [--format text|json]"
            );
            eprintln!(
                "       {prog} experimental-remove-text <file.pdf> <page-number> --rect llx lly urx ury [--rect …] --output <out.pdf>"
            );
            eprintln!("       {prog} audit-residuals <file.pdf> [--format text|json]");
            eprintln!("       {prog} remove-document-metadata <file.pdf> --output <out.pdf>");
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
/// `extract_text` reports. Page is 1-based like `contents`.
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
/// contents <ref> | contents [<ref>, <ref>, ...] | contents absent
/// userunit <v> | userunit absent
/// annot <i> <subtype> rect <llx> <lly> <urx> <ury> [/<k>=<v> ...]
/// ```
///
/// MediaBox/CropBox/Rotate are the *inherited* values the page tree
/// already computed. `/Annots` must be an array (or a ref to one);
/// `/Contents` a ref or an array of refs; `/UserUnit` a number —
/// anything else is a structural error, not silently coerced.
/// Each `annot` line dumps the annotation dict: every key in
/// byte-order, values formatted recursively (refs verbatim `n g R`,
/// streams as `<stream>`, strings `(text)` with non-ASCII escaped
/// `\xNN`).
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
    let annots_obj = match page_dict.get(b"Annots".as_slice()) {
        None => None,
        Some(o) => Some(doc.deref(o)?),
    };
    let annots: Vec<Object> = match annots_obj {
        None => Vec::new(),
        Some(Object::Array(a)) => a,
        Some(other) => {
            return Err(Error::Structure(format!(
                "page /Annots is not an array ({other:?})"
            )))
        }
    };
    println!("annotations {}", annots.len());

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
    // Enumerate named resources after each count line — `font F1
    // Type1 /Helvetica`, `xobject Im1 Form /BBox=[…]`, etc. Only the
    // top-level dict entries are listed; we do not recurse into
    // nested resource dicts.
    println!("resources fonts {}", c[0]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"Font".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(fonts) = res {
                for (name, fref) in fonts.iter() {
                    let fobj = doc.deref(fref)?;
                    let fdict = fobj.as_dict().ok_or_else(|| {
                        Error::Structure(format!(
                            "font resource /{} is not a dictionary",
                            String::from_utf8_lossy(name)
                        ))
                    })?;
                    let subtype = fdict
                        .get(b"Subtype".as_slice())
                        .and_then(|o| o.as_name())
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_else(|| "unknown".into());
                    let base = fdict
                        .get(b"BaseFont".as_slice())
                        .and_then(|o| o.as_name())
                        .map(|n| format!(" /{}", String::from_utf8_lossy(n)))
                        .unwrap_or_default();
                    println!("font {} {}{}", String::from_utf8_lossy(name), subtype, base);
                }
            }
        }
    }
    println!("xobjects {}", c[1]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"XObject".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(xos) = res {
                for (name, xref) in xos.iter() {
                    let xobj = doc.deref(xref)?;
                    let (dict, _) = xobj.as_stream().ok_or_else(|| {
                        Error::Structure(format!(
                            "XObject resource /{} is not a stream",
                            String::from_utf8_lossy(name)
                        ))
                    })?;
                    let subtype = dict
                        .get(b"Subtype".as_slice())
                        .and_then(|o| o.as_name())
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_else(|| "unknown".into());
                    let extra = match subtype.as_str() {
                        "Form" => dict
                            .get(b"BBox".as_slice())
                            .map(|o| format!(" /BBox={}", fmt_value(o)))
                            .unwrap_or_default(),
                        "Image" => {
                            let w = dict
                                .get(b"Width".as_slice())
                                .map(|o| format!(" /Width={}", fmt_value(o)))
                                .unwrap_or_default();
                            let h = dict
                                .get(b"Height".as_slice())
                                .map(|o| format!(" /Height={}", fmt_value(o)))
                                .unwrap_or_default();
                            format!("{w}{h}")
                        }
                        _ => String::new(),
                    };
                    println!(
                        "xobject {} {}{}",
                        String::from_utf8_lossy(name),
                        subtype,
                        extra
                    );
                }
            }
        }
    }
    println!("colorspaces {}", c[2]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"ColorSpace".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(css) = res {
                for (name, cref) in css.iter() {
                    let cobj = doc.deref(cref)?;
                    let csname = match &cobj {
                        Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
                        Object::Array(a) => a
                            .first()
                            .and_then(|o| o.as_name())
                            .map(|n| format!("/{}", String::from_utf8_lossy(n)))
                            .unwrap_or_else(|| "unknown".into()),
                        Object::Dict(d2) => d2
                            .get(b"Subtype".as_slice())
                            .or_else(|| d2.get(b"Base".as_slice()))
                            .and_then(|o| o.as_name())
                            .map(|n| format!("/{}", String::from_utf8_lossy(n)))
                            .unwrap_or_else(|| "unknown".into()),
                        _ => "unknown".into(),
                    };
                    println!("colorspace {} {}", String::from_utf8_lossy(name), csname);
                }
            }
        }
    }
    println!("patterns {}", c[3]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"Pattern".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(pats) = res {
                for (name, pref) in pats.iter() {
                    let pobj = doc.deref(pref)?;
                    let ptype = match &pobj {
                        Object::Dict(d2) | Object::Stream { dict: d2, .. } => d2
                            .get(b"PatternType".as_slice())
                            .and_then(|o| o.as_number())
                            .map(fmt_num)
                            .unwrap_or_else(|| "unknown".into()),
                        _ => "unknown".into(),
                    };
                    println!("pattern {} {}", String::from_utf8_lossy(name), ptype);
                }
            }
        }
    }
    println!("shadings {}", c[4]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"Shading".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(shs) = res {
                for (name, sref) in shs.iter() {
                    let sobj = doc.deref(sref)?;
                    let stype = match &sobj {
                        Object::Dict(d2) | Object::Stream { dict: d2, .. } => d2
                            .get(b"ShadingType".as_slice())
                            .and_then(|o| o.as_number())
                            .map(fmt_num)
                            .unwrap_or_else(|| "unknown".into()),
                        _ => "unknown".into(),
                    };
                    println!("shading {} {}", String::from_utf8_lossy(name), stype);
                }
            }
        }
    }
    println!("extgstates {}", c[5]);
    if let Some(d) = &res_dict {
        if let Some(v) = d.get(b"ExtGState".as_slice()) {
            let res = doc.deref(v)?;
            if let Object::Dict(egs) = res {
                for (name, eref) in egs.iter() {
                    let eobj = doc.deref(eref)?;
                    let etype = eobj
                        .as_dict()
                        .and_then(|d2| d2.get(b"Type".as_slice()))
                        .and_then(|o| o.as_name())
                        .map(|n| format!("/{}", String::from_utf8_lossy(n)))
                        .unwrap_or_else(|| "unknown".into());
                    println!("extgstate {} {}", String::from_utf8_lossy(name), etype);
                }
            }
        }
    }
    println!("procsets {}", c[6]);
    println!("properties {}", c[7]);

    // Content-stream statistics: token count (operands + operators),
    // operator count, and per-operator tallies in name order.
    // `contents absent` → all zeros.
    match page.contents() {
        None => {
            println!("tokens 0");
            println!("operators 0");
            println!("opstats");
        }
        Some(_) => {
            let (ops, op_names, tokens) = doc.page_operations_named(page)?;
            let mut stats: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for name in &op_names {
                *stats
                    .entry(String::from_utf8_lossy(name).into_owned())
                    .or_insert(0) += 1;
            }
            println!("tokens {tokens}");
            println!("operators {}", ops.len());
            let stats_str: Vec<String> = stats.iter().map(|(n, c)| format!("{n}:{c}")).collect();
            println!("opstats {}", stats_str.join(" "));
        }
    }

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
            println!("contents [{}]", parts.join(", "));
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

    // One line per annotation, in /Annots order. Each element may be
    // an indirect ref to a dict (the usual form) or a direct dict —
    // real files do embed annotations inline. Any other type is a
    // structural error.
    for (i, entry) in annots.iter().enumerate() {
        let adict_owned;
        let adict = match entry {
            Object::Ref(r) => {
                adict_owned = doc.resolve(*r)?;
                adict_owned.as_dict().ok_or_else(|| {
                    Error::Structure(format!("annotation {i} is not a dictionary"))
                })?
            }
            Object::Dict(d) => d,
            other => {
                return Err(Error::Structure(format!(
                    "annotation {i} is not an indirect reference or a dictionary ({other:?})"
                )))
            }
        };
        let subtype = adict
            .get(b"Subtype".as_slice())
            .and_then(|o| o.as_name())
            .ok_or_else(|| {
                Error::Structure(format!("annotation {i} /Subtype is missing or not a name"))
            })?;
        let subtype = String::from_utf8_lossy(subtype).into_owned();
        let rect = match adict.get(b"Rect".as_slice()) {
            Some(Object::Array(a)) if a.len() == 4 => {
                let v: Vec<f64> = a.iter().filter_map(|o| o.as_number()).collect();
                if v.len() == 4 {
                    [v[0], v[1], v[2], v[3]]
                } else {
                    return Err(Error::Structure(format!(
                        "annotation {i} /Rect is not 4 numbers"
                    )));
                }
            }
            _ => {
                return Err(Error::Structure(format!(
                    "annotation {i} /Rect is missing or not a 4-number array"
                )))
            }
        };
        let mut line = format!(
            "annot {i} {subtype} rect {} {} {} {}",
            fmt_num(rect[0]),
            fmt_num(rect[1]),
            fmt_num(rect[2]),
            fmt_num(rect[3])
        );
        // Every remaining key in BTreeMap (lexicographic byte) order —
        // /Rect and /Subtype appear again here per the spec example.
        for (k, v) in adict.iter() {
            line.push_str(&format!(
                " /{}={}",
                String::from_utf8_lossy(k),
                fmt_value(v)
            ));
        }
        println!("{line}");
    }

    // ---- document info (/Info) ----
    // Trailing section: PDF version, then the trailer's /Info dict.
    // `version` reports the effective declared version — the larger
    // of the `%PDF-M.m` header and the catalog `/Version` name. A
    // malformed header (None) or malformed /Version is a typed error,
    // not a guess.
    println!("document");
    let version = {
        let hdr = doc.header_version().ok_or_else(|| Error::Unsupported {
            feature: "PDF version",
            detail: "header %PDF- token is malformed — cannot determine version".to_string(),
        })?;
        let cat = doc.catalog_version()?;
        match cat {
            Some(c) if c > hdr => c,
            _ => hdr,
        }
    };
    println!("version {}.{}", version.0, version.1);
    let info_obj = match doc.trailer().get(b"Info".as_slice()) {
        None => None,
        Some(o) => Some(doc.deref(o)?),
    };
    let info_dict = match info_obj {
        None => None,
        Some(Object::Dict(d)) => Some(d),
        Some(other) => {
            return Err(Error::Structure(format!(
                "trailer /Info is not a dictionary ({other:?})"
            )))
        }
    };
    let info_count = info_dict.as_ref().map_or(0, |d| d.len());
    println!("info {info_count}");
    if let Some(d) = info_dict {
        for (k, v) in d.iter() {
            println!("key {} {}", String::from_utf8_lossy(k), type_name(v));
        }
    }
    Ok(())
}

/// The PDF type name `inspect` reports for an `/Info` value — never
/// the value itself. `ref` covers any indirect reference without
/// chasing it.
fn type_name(o: &Object) -> &'static str {
    match o {
        Object::Null => "null",
        Object::Bool(_) => "bool",
        Object::Integer(_) => "int",
        Object::Real(_) => "real",
        Object::Name(_) => "name",
        Object::String(_) => "string",
        Object::Array(_) => "array",
        Object::Dict(_) => "dict",
        Object::Stream { .. } => "stream",
        Object::Ref(_) => "ref",
    }
}

/// Render one `Object` for the inspect annot dump. Refs are shown
/// verbatim (`n g R`); streams as `<stream n g>` placeholders —
/// their dicts are not expanded. Strings are `(text)` with non-ASCII
/// or non-printing bytes escaped as `\xNN`.
fn fmt_value(o: &Object) -> String {
    match o {
        Object::Null => "null".into(),
        Object::Bool(b) => b.to_string(),
        Object::Integer(i) => i.to_string(),
        Object::Real(r) => fmt_num(*r),
        Object::Name(n) => format!("/{}", escape_bytes(n)),
        Object::String(s) => format!("({})", escape_bytes(s)),
        Object::Array(items) => {
            let inner: Vec<String> = items.iter().map(fmt_value).collect();
            format!("[{}]", inner.join(" "))
        }
        Object::Dict(d) => {
            let mut parts = Vec::with_capacity(d.len());
            for (k, v) in d.iter() {
                parts.push(format!("/{} {}", escape_bytes(k), fmt_value(v)));
            }
            format!("<< {} >>", parts.join(" "))
        }
        Object::Stream { .. } => "<stream>".into(),
        Object::Ref(r) => r.to_string(),
    }
}

/// Render a byte string for display inside `(…)`: printable ASCII
/// passes through, `(` `)` and `\` are escaped as `\xNN` so the
/// surrounding parens stay balanced, everything else is `\xNN`.
fn escape_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for &c in b {
        if (0x20..=0x7e).contains(&c) && c != b'(' && c != b')' && c != b'\\' {
            out.push(c as char);
        } else {
            out.push_str(&format!("\\x{c:02X}"));
        }
    }
    out
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
    report_warnings(&doc);
    Ok(())
}

/// Print the document's non-fatal findings (`Document::take_warnings`)
/// on stderr, one `warning: …` line each, so an accepted-with-warnings
/// result is never mistaken for a clean one.
fn report_warnings(doc: &Document) {
    for w in doc.take_warnings() {
        eprintln!("warning: {w}");
    }
    let dropped = doc.warnings_dropped();
    if dropped > 0 {
        eprintln!("warning: {dropped} further warning(s) not retained");
    }
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
    report_warnings(&doc);
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

/// `preflight-redaction` subcommand:
/// `preflight-redaction <file.pdf> <page> --rect llx lly urx ury
///   [--rect …] [--format text|json]`.
///
/// Diagnose what a *future* redaction would face — never modifies the
/// file. A clean report is **not** a "safe to redact" verdict; the
/// output names targets, blockers, uncertain positions, unexamined
/// areas, and shared resources.
fn run_preflight(args: &[String]) -> Result<(), Error> {
    // args[0]=input args[1]=page then --rect ×N + --format.
    if args.len() < 2 {
        return Err(Error::Structure(preflight_usage()));
    }
    let input = &args[0];
    let page: usize = args[1]
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| Error::Structure(format!("invalid page number '{}'", args[1])))?;
    let mut rects: Vec<[f64; 4]> = Vec::new();
    let mut format = "text";
    let mut i = 2;
    while i < args.len() {
        let need = |n: usize| -> Result<(), Error> {
            if i + n < args.len() {
                Ok(())
            } else {
                Err(Error::Structure(format!(
                    "flag '{}' expects {n} value(s)\n{}",
                    args[i],
                    preflight_usage()
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
                rects.push([
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ]);
                i += 5;
            }
            "--format" => {
                need(1)?;
                format = args[i + 1].as_str();
                if format != "text" && format != "json" {
                    return Err(Error::Structure(format!(
                        "unknown --format '{format}' (text|json)"
                    )));
                }
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    preflight_usage()
                )));
            }
        }
    }
    if rects.is_empty() {
        return Err(Error::Structure(format!(
            "at least one --rect is required\n{}",
            preflight_usage()
        )));
    }
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let diag = kasane_pdf::diagnose_redaction(
        &doc,
        page - 1,
        &rects
            .iter()
            .map(|r| (r[0], r[1], r[2], r[3]))
            .collect::<Vec<_>>(),
        &Limits::default(),
    )?;
    match format {
        "json" => print_preflight_json(&diag),
        _ => print_preflight_text(&diag, page),
    }
    Ok(())
}

fn preflight_usage() -> String {
    "usage: kasane-pdf preflight-redaction <file.pdf> <page> \
     --rect <llx> <lly> <urx> <ury> [--rect …] [--format text|json]\n\
     rect corners are PDF user space (+x right, +y up; ll = lower-left)\n\
     — the diagnosis never modifies the file and a clean report is not \
     a 'safe to redact' verdict."
        .into()
}

/// `experimental-remove-text` subcommand:
/// `experimental-remove-text <file.pdf> <page>
///   --rect llx lly urx ury [--rect …] --output <out.pdf>`.
///
/// Experimental scoped text removal — **not** a redaction tool: it
/// deletes the shown character codes whose filled ink bounds hit a
/// rect and preserves the survivors' positions. A page carrying
/// Form XObjects, annotations, unsupported fonts, or any blocker/
/// uncertain diagnosis entry is refused outright — nothing is
/// emitted on failure. See `src/remove_text.rs` for the full list.
fn run_remove_text(args: &[String]) -> Result<(), Error> {
    if args.len() < 2 {
        return Err(Error::Structure(remove_text_usage()));
    }
    let input = &args[0];
    let page: usize = args[1]
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| Error::Structure(format!("invalid page number '{}'", args[1])))?;
    let mut rects: Vec<[f64; 4]> = Vec::new();
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
                    remove_text_usage()
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
                rects.push([
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ]);
                i += 5;
            }
            "--output" => {
                need(1)?;
                output = Some(&args[i + 1]);
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    remove_text_usage()
                )));
            }
        }
    }
    if rects.is_empty() {
        return Err(Error::Structure(format!(
            "at least one --rect is required\n{}",
            remove_text_usage()
        )));
    }
    let output = output.ok_or_else(|| Error::Structure(remove_text_usage()))?;
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let r = kasane_pdf::remove_text(
        &doc,
        page - 1,
        &rects
            .iter()
            .map(|r| (r[0], r[1], r[2], r[3]))
            .collect::<Vec<_>>(),
        &Limits::default(),
    )?;
    write_atomic(
        &r.output,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!(
        "experimental-remove-text {input} page {page} -> {output} ({} bytes)",
        r.output.len()
    );
    println!("codes removed: {}", r.codes_removed);
    for s in &r.scope {
        println!("  {s}");
    }
    Ok(())
}

fn remove_text_usage() -> String {
    "usage: kasane-pdf experimental-remove-text <file.pdf> <page> \
     --rect <llx> <lly> <urx> <ury> [--rect …] --output <out.pdf>\n\
     Experimental scoped text removal — NOT a redaction or sanitize \
     tool: deletes shown codes whose filled ink bounds hit a rect \
     (Tr 0 text at page level only) and preserves the survivors' \
     positions. A page with Form XObjects, annotations, unsupported \
     fonts, or any blocker/uncertain diagnosis entry is refused."
        .into()
}

fn remove_image_region_usage() -> String {
    "usage: kasane-pdf experimental-remove-image-region <file.pdf> <page> \
     --rect <llx> <lly> <urx> <ury> [--rect …] --output <out.pdf>\n\
     Experimental image-region removal — NOT a redaction or sanitize \
     tool: replaces image samples whose painted positions hit a rect \
     with opaque black in a *new* image object (8-bit DeviceGray/\
     DeviceRGB; no filter, FlateDecode, or single DCTDecode JPEG). \
     The original stream is never mutated; other copies of the same \
     image data are preserved. A page with Form XObjects, annotations, \
     masks, non-default /Decode, CMYK/unsupported JPEG, or any \
     blocker/uncertain diagnosis entry is refused."
        .into()
}

/// `experimental-remove-image-region` subcommand:
/// `experimental-remove-image-region <file.pdf> <page>
///   --rect llx lly urx ury [--rect …] --output <out.pdf>`.
///
/// Experimental image-region removal — **not** a redaction tool: it
/// replaces the image samples whose painted positions hit a rect with
/// opaque black in a *new* image object. The original stream is never
/// mutated; only the `Do` call sites that hit a rect are repointed to
/// the clone. See `src/remove_image.rs` for the full refusal list.
fn run_remove_image_region(args: &[String]) -> Result<(), Error> {
    if args.len() < 2 {
        return Err(Error::Structure(remove_image_region_usage()));
    }
    let input = &args[0];
    let page: usize = args[1]
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| Error::Structure(format!("invalid page number '{}'", args[1])))?;
    let mut rects: Vec<[f64; 4]> = Vec::new();
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
                    remove_image_region_usage()
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
                rects.push([
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ]);
                i += 5;
            }
            "--output" => {
                need(1)?;
                output = Some(&args[i + 1]);
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    remove_image_region_usage()
                )));
            }
        }
    }
    if rects.is_empty() {
        return Err(Error::Structure(format!(
            "at least one --rect is required\n{}",
            remove_image_region_usage()
        )));
    }
    let output = output.ok_or_else(|| Error::Structure(remove_image_region_usage()))?;
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let r = kasane_pdf::remove_image_region(
        &doc,
        page - 1,
        &rects
            .iter()
            .map(|r| (r[0], r[1], r[2], r[3]))
            .collect::<Vec<_>>(),
        &Limits::default(),
    )?;
    write_atomic(
        &r.output,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!(
        "experimental-remove-image-region {input} page {page} -> {output} ({} bytes)",
        r.output.len()
    );
    println!("invocations edited: {}", r.invocations_edited);
    println!("pixels changed: {}", r.pixels_changed);
    for s in &r.scope {
        println!("  {s}");
    }
    Ok(())
}

/// Usage string shared by `experimental-remove-region`.
fn remove_region_usage() -> &'static str {
    "usage: kasane-pdf experimental-remove-region <file.pdf> <page> \
     --rect <llx> <lly> <urx> <ury> [--rect …] --output <out.pdf>\n\
     Experimental region removal — NOT a redaction or sanitize tool: \
     removes text codes whose bounds hit a rect and blackens image \
     samples whose painted positions hit the same rect, in one pass. \
     The input is never modified; the original text and image data \
     are preserved elsewhere in the file. A page with Form XObjects, \
     annotations, masks, non-default /Decode, non-device colour, \
     CMYK/unsupported JPEG, or any blocker/uncertain diagnosis \
     entry is refused."
}

/// `experimental-remove-region` subcommand:
/// `experimental-remove-region <file.pdf> <page>
///   --rect llx lly urx ury [--rect …] --output <out.pdf>`.
///
/// Experimental region removal — **not** a redaction tool: removes
/// text codes whose bounds hit a rect and blackens image samples
/// whose painted positions hit the same rect, in one pass. The
/// input is never modified. See `src/remove_region.rs` for the
/// full refusal list.
fn run_remove_region(args: &[String]) -> Result<(), Error> {
    if args.len() < 2 {
        return Err(Error::Structure(remove_region_usage().to_string()));
    }
    let input = &args[0];
    let page: usize = args[1]
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| Error::Structure(format!("invalid page number '{}'", args[1])))?;
    let mut rects: Vec<[f64; 4]> = Vec::new();
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
                    remove_region_usage()
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
                rects.push([
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ]);
                i += 5;
            }
            "--output" => {
                need(1)?;
                output = Some(&args[i + 1]);
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    remove_region_usage()
                )));
            }
        }
    }
    if rects.is_empty() {
        return Err(Error::Structure(format!(
            "at least one --rect is required\n{}",
            remove_region_usage()
        )));
    }
    let output = output.ok_or_else(|| Error::Structure(remove_region_usage().to_string()))?;
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let r = kasane_pdf::remove_region(
        &doc,
        page - 1,
        &rects
            .iter()
            .map(|r| (r[0], r[1], r[2], r[3]))
            .collect::<Vec<_>>(),
        &Limits::default(),
    )?;
    write_atomic(
        &r.output,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!(
        "experimental-remove-region {input} page {page} -> {output} ({} bytes)",
        r.output.len()
    );
    println!("codes removed: {}", r.codes_removed);
    println!("invocations edited: {}", r.invocations_edited);
    println!("pixels changed: {}", r.pixels_changed);
    for s in &r.scope {
        println!("  {s}");
    }
    Ok(())
}

/// Usage string shared by `experimental-remove-regions`.
fn remove_regions_usage() -> &'static str {
    "usage: kasane-pdf experimental-remove-regions <file.pdf> \
     --page <n> --rect <llx> <lly> <urx> <ury> [--rect …] \
     [--page <m> --rect …] [--remove-document-metadata] --output <out.pdf>\n\
     Experimental multi-page region removal — NOT a redaction or \
     sanitize tool: the same per-page edit as experimental-remove-region \
     applied to several pages in one session and one rewrite. --page is \
     1-based; each --rect belongs to the nearest preceding --page; a \
     page may be repeated to add rects. --remove-document-metadata also \
     removes the trailer /Info and catalog /Metadata *entries* in the \
     same rewrite (not a metadata scrub). The input is never modified; \
     a failure produces no output. Images: 8-bit DeviceGray/RGB, no \
     filter / FlateDecode / single DCTDecode (baseline JPEG, Gray/RGB \
     only); CMYK, masks, non-default /Decode, and filter chains are \
     refused."
}

/// `experimental-remove-regions` subcommand — multi-page variant.
fn run_remove_regions(args: &[String]) -> Result<(), Error> {
    if args.is_empty() {
        return Err(Error::Structure(remove_regions_usage().to_string()));
    }
    let input = &args[0];
    // (1-based page, rects) — each --rect attaches to the nearest
    // preceding --page; the same page may repeat to add rects.
    let mut specs: Vec<(usize, Vec<[f64; 4]>)> = Vec::new();
    let mut output: Option<&str> = None;
    let mut strip_metadata = false;
    let mut current_page: Option<usize> = None;
    let mut i = 1;
    while i < args.len() {
        let need = |n: usize| -> Result<(), Error> {
            if i + n < args.len() {
                Ok(())
            } else {
                Err(Error::Structure(format!(
                    "flag '{}' expects {n} value(s)\n{}",
                    args[i],
                    remove_regions_usage()
                )))
            }
        };
        let num = |s: &str| -> Result<f64, Error> {
            s.parse::<f64>()
                .map_err(|_| Error::Structure(format!("expected a number, got '{s}'")))
        };
        match args[i].as_str() {
            "--page" => {
                need(1)?;
                let p: usize = args[i + 1]
                    .parse()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| {
                        Error::Structure(format!("invalid page number '{}'", args[i + 1]))
                    })?;
                current_page = Some(p);
                specs.push((p, Vec::new()));
                i += 2;
            }
            "--rect" => {
                need(4)?;
                if current_page.is_none() {
                    return Err(Error::Structure(format!(
                        "--rect must follow a --page\n{}",
                        remove_regions_usage()
                    )));
                }
                let r = [
                    num(&args[i + 1])?,
                    num(&args[i + 2])?,
                    num(&args[i + 3])?,
                    num(&args[i + 4])?,
                ];
                specs.last_mut().unwrap().1.push(r);
                i += 5;
            }
            "--output" => {
                need(1)?;
                output = Some(&args[i + 1]);
                i += 2;
            }
            "--remove-document-metadata" => {
                strip_metadata = true;
                i += 1;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    remove_regions_usage()
                )));
            }
        }
    }
    if specs.is_empty() {
        return Err(Error::Structure(format!(
            "at least one --page is required\n{}",
            remove_regions_usage()
        )));
    }
    // A page with no rects is an error — not silently skipped.
    for (p, rects) in &specs {
        if rects.is_empty() {
            return Err(Error::Structure(format!(
                "page {p} has no --rect\n{}",
                remove_regions_usage()
            )));
        }
    }
    let output = output.ok_or_else(|| Error::Structure(remove_regions_usage().to_string()))?;
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let api_specs: Vec<kasane_pdf::PageSpec> = specs
        .iter()
        .map(|(p, rs)| {
            (
                *p - 1,
                rs.iter().map(|r| (r[0], r[1], r[2], r[3])).collect(),
            )
        })
        .collect();
    if strip_metadata {
        let r =
            kasane_pdf::remove_regions_and_document_metadata(&doc, &api_specs, &Limits::default())?;
        write_atomic(
            &r.output,
            &format!("{output}.kasane-tmp-{}", std::process::id()),
            output,
        )?;
        println!(
            "experimental-remove-regions {input} -> {output} ({} bytes)",
            r.output.len()
        );
        println!("codes removed: {}", r.codes_removed);
        println!("invocations edited: {}", r.invocations_edited);
        println!("pixels changed: {}", r.pixels_changed);
        println!("info entry removed: {}", r.info_removed);
        println!("catalog /Metadata entry removed: {}", r.metadata_removed);
        for s in &r.scope {
            println!("  {s}");
        }
    } else {
        let r = kasane_pdf::remove_regions(&doc, &api_specs, &Limits::default())?;
        write_atomic(
            &r.output,
            &format!("{output}.kasane-tmp-{}", std::process::id()),
            output,
        )?;
        println!(
            "experimental-remove-regions {input} -> {output} ({} bytes)",
            r.output.len()
        );
        println!("codes removed: {}", r.codes_removed);
        println!("invocations edited: {}", r.invocations_edited);
        println!("pixels changed: {}", r.pixels_changed);
        for s in &r.scope {
            println!("  {s}");
        }
    }
    Ok(())
}

/// Usage string shared by `remove-document-metadata`.
fn remove_document_metadata_usage() -> &'static str {
    "usage: kasane-pdf remove-document-metadata <file.pdf> --output <out.pdf>\n\
     Removes the trailer /Info and catalog /Metadata *entries* and writes a\n\
     new PDF. This is NOT a metadata scrubber or redaction tool — it removes\n\
     two specific references only; the underlying data may remain reachable\n\
     through other keys, and copies may exist elsewhere in the file."
}

/// `remove-document-metadata` subcommand.
fn run_remove_document_metadata(args: &[String]) -> Result<(), Error> {
    if args.is_empty() {
        return Err(Error::Structure(
            remove_document_metadata_usage().to_string(),
        ));
    }
    let input = &args[0];
    let mut output: Option<&str> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--output" => {
                if i + 1 >= args.len() {
                    return Err(Error::Structure(format!(
                        "flag '--output' expects a value\n{}",
                        remove_document_metadata_usage()
                    )));
                }
                output = Some(&args[i + 1]);
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!(
                    "unknown option '{other}'\n{}",
                    remove_document_metadata_usage()
                )));
            }
        }
    }
    let output =
        output.ok_or_else(|| Error::Structure(remove_document_metadata_usage().to_string()))?;
    refuse_same_file(input, output)?;
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let r = kasane_pdf::remove_document_metadata(&doc, &Limits::default())?;
    write_atomic(
        &r.output,
        &format!("{output}.kasane-tmp-{}", std::process::id()),
        output,
    )?;
    println!(
        "remove-document-metadata {input} -> {output} ({} bytes)",
        r.output.len()
    );
    println!("info entry removed: {}", r.info_removed);
    println!("catalog /Metadata entry removed: {}", r.metadata_removed);
    for s in &r.scope {
        println!("  {s}");
    }
    Ok(())
}

// Output caps — a single preflight run prints at most this many
// entries per category; every truncation states the omitted count.
const MAX_PREFLIGHT_LINES: usize = 1_000;

fn clip_preflight<T>(v: &[T]) -> (&[T], usize) {
    let shown = v.len().min(MAX_PREFLIGHT_LINES);
    (&v[..shown], v.len() - shown)
}

fn print_preflight_text(d: &kasane_pdf::Diagnosis, page: usize) {
    println!("preflight-redaction — diagnosis, not a redaction");
    println!("page: {page}");
    println!("rects: {}", d.rects.len());
    for (i, r) in d.rects.iter().enumerate() {
        println!(
            "  rect {i}: {} {} {} {}",
            fmt_num(r.llx),
            fmt_num(r.lly),
            fmt_num(r.urx),
            fmt_num(r.ury)
        );
    }
    if let Some(inc) = &d.incomplete {
        println!("incomplete: {}", inc.reason);
        if let Some(op) = inc.from_op {
            println!("  (scan stopped before operation {op})");
        }
    }
    let (targets, tn) = clip_preflight(&d.targets);
    println!("targets {}", d.targets.len());
    for t in targets {
        println!("  {}", describe_target(t));
    }
    if tn > 0 {
        println!("  ... ({tn} more — output capped at {MAX_PREFLIGHT_LINES})");
    }
    let (uncertain, un) = clip_preflight(&d.uncertain);
    println!("uncertain {}", d.uncertain.len());
    for u in uncertain {
        println!("  {:?} at {}: {}", u.kind, site_str(&u.site), u.reason);
    }
    if un > 0 {
        println!("  ... ({un} more)");
    }
    let (blockers, bn) = clip_preflight(&d.blockers);
    println!("blockers {}", d.blockers.len());
    for b in blockers {
        let site = b.site.as_ref().map(site_str).unwrap_or_default();
        println!("  {} {}{}", b.what, b.detail, site);
    }
    if bn > 0 {
        println!("  ... ({bn} more)");
    }
    let (shared, sn) = clip_preflight(&d.shared);
    println!(
        "shared {} (scanned {}/{} pages{})",
        d.shared.len(),
        d.shared_scope.pages_scanned,
        d.shared_scope.pages_total,
        if d.shared_scope.truncated {
            " — truncated"
        } else {
            ""
        }
    );
    for s in shared {
        let other = if s.other_pages.is_empty() {
            String::new()
        } else {
            format!(
                ", also in resources of page(s) {}",
                s.other_pages
                    .iter()
                    .map(|p| (p + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        println!(
            "  {} {} used at {} site(s){}",
            s.obj,
            s.kind,
            s.sites.len(),
            other
        );
    }
    if sn > 0 {
        println!("  ... ({sn} more)");
    }
    let (annots, an) = clip_preflight(&d.annotations);
    println!("annotations {}", d.annotations.len());
    for a in annots {
        let rect = a
            .rect
            .map(|r| {
                format!(
                    "[{} {} {} {}]",
                    fmt_num(r.0),
                    fmt_num(r.1),
                    fmt_num(r.2),
                    fmt_num(r.3)
                )
            })
            .unwrap_or_else(|| "<none>".into());
        let hits = if a.hits.is_empty() {
            String::new()
        } else {
            format!(
                " — overlaps rect(s) {}",
                a.hits
                    .iter()
                    .map(|h| h.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        let extra = if a.extra_keys.is_empty() {
            String::new()
        } else {
            format!(" carries {}", a.extra_keys.join(","))
        };
        println!("  /{} rect {}{}{}", a.subtype, rect, hits, extra);
    }
    if an > 0 {
        println!("  ... ({an} more)");
    }
    println!("unexamined {}", d.unexamined.len());
    for u in &d.unexamined {
        println!("  - {u}");
    }
    println!(
        "note: a clean report means 'no obstacle found in the examined scope' — \
         it is NOT a redaction-safety verdict."
    );
}

fn site_str(s: &kasane_pdf::redact_diag::Site) -> String {
    let mut out = String::new();
    if !s.contents.is_empty() {
        out.push_str(&format!(
            " contents[{}]",
            s.contents
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    out.push_str(&format!(" op#{}", s.op_index));
    for f in &s.form_path {
        out.push_str(&format!(
            " via /{}{}@op#{}",
            String::from_utf8_lossy(&f.name),
            f.xobj.map(|r| format!(" ({r})")).unwrap_or_default(),
            f.op_index
        ));
    }
    if let Some(ci) = s.code_index {
        out.push_str(&format!(" code#{ci}"));
    }
    out
}

fn describe_target(t: &kasane_pdf::Target) -> String {
    use kasane_pdf::redact_diag::TargetDetail as D;
    let hits = if t.hits.is_empty() {
        String::new()
    } else {
        format!(
            " rects {}",
            t.hits
                .iter()
                .map(|h| h.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let head = format!(
        "{:?} bounds [{} {} {} {}]{} @{}",
        t.kind,
        fmt_num(t.bounds.0),
        fmt_num(t.bounds.1),
        fmt_num(t.bounds.2),
        fmt_num(t.bounds.3),
        hits,
        site_str(&t.site)
    );
    match &t.detail {
        D::Text {
            code,
            unicode,
            font,
            render_mode,
            origin,
            bounds_kind,
        } => format!(
            "{head} text code=0x{code:04X} unicode={} font=/{} mode={render_mode} origin=({} {}) via={:?}",
            unicode
                .as_deref()
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "<unresolved>".into()),
            escape_bytes(font),
            fmt_num(origin.0),
            fmt_num(origin.1),
            bounds_kind
        ),
        D::Image {
            xobj,
            name,
            size,
            blockers,
            ..
        } => format!(
            "{head} image /{} {}x{}{}{}",
            escape_bytes(name),
            size.0,
            size.1,
            xobj.map(|r| format!(" {r}")).unwrap_or_default(),
            if blockers.is_empty() {
                String::new()
            } else {
                format!(" blockers:{}", blockers.join(","))
            }
        ),
        D::Path { op, stroked } => format!(
            "{head} path {}{}",
            op,
            if *stroked { " (stroke-inflated)" } else { "" }
        ),
        D::Form {
            xobj,
            name,
            bbox,
            matrix,
        } => format!(
            "{head} form /{}{} bbox [{} {} {} {}] matrix [{}]",
            escape_bytes(name),
            xobj.map(|r| format!(" {r}")).unwrap_or_default(),
            fmt_num(bbox.0),
            fmt_num(bbox.1),
            fmt_num(bbox.2),
            fmt_num(bbox.3),
            matrix.iter().map(|v| fmt_num(*v)).collect::<Vec<_>>().join(" ")
        ),
        D::Other { what } => format!("{head} {what}"),
    }
}

fn print_preflight_json(d: &kasane_pdf::Diagnosis) {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"rects\": [{}],\n",
        d.rects
            .iter()
            .map(|r| format!(
                "[{}, {}, {}, {}]",
                fmt_num(r.llx),
                fmt_num(r.lly),
                fmt_num(r.urx),
                fmt_num(r.ury)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let (targets, tn) = clip_preflight(&d.targets);
    out.push_str(&format!("  \"targets\": {},\n", d.targets.len()));
    out.push_str("  \"target_list\": [\n");
    for (i, t) in targets.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("    ");
        out.push_str(&target_json(t));
    }
    out.push_str("\n  ]");
    if tn > 0 {
        out.push_str(&format!(",\n  \"targets_omitted\": {tn}"));
    }
    out.push_str(",\n  \"uncertain\": ");
    out.push_str(&d.uncertain.len().to_string());
    out.push_str(",\n  \"blockers\": ");
    out.push_str(&d.blockers.len().to_string());
    out.push_str(&format!(
        ",\n  \"shared\": {},\n  \"shared_scope\": {{\"pages_scanned\": {}, \"pages_total\": {}, \"truncated\": {}}}",
        d.shared.len(),
        d.shared_scope.pages_scanned,
        d.shared_scope.pages_total,
        d.shared_scope.truncated
    ));
    out.push_str(&format!(
        ",\n  \"annotations\": {},\n  \"incomplete\": {}",
        d.annotations.len(),
        match &d.incomplete {
            Some(i) => format!("{{\"reason\": {:?}}}", i.reason),
            None => "null".into(),
        }
    ));
    out.push_str(&format!(
        ",\n  \"unexamined\": {}\n}}\n",
        d.unexamined.len()
    ));
    print!("{out}");
}

fn target_json(t: &kasane_pdf::Target) -> String {
    use kasane_pdf::redact_diag::TargetDetail as D;
    let mut s = format!(
        "{{\"kind\": \"{:?}\", \"bounds\": [{}, {}, {}, {}], \"hits\": [{}]",
        t.kind,
        fmt_num(t.bounds.0),
        fmt_num(t.bounds.1),
        fmt_num(t.bounds.2),
        fmt_num(t.bounds.3),
        t.hits
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    s.push_str(&format!(", \"site\": {{\"op\": {}", t.site.op_index));
    if !t.site.contents.is_empty() {
        s.push_str(&format!(
            ", \"contents\": [{}]",
            t.site
                .contents
                .iter()
                .map(|r| format!("\"{r}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !t.site.form_path.is_empty() {
        s.push_str(&format!(
            ", \"form_path\": [{}]",
            t.site
                .form_path
                .iter()
                .map(|f| format!(
                    "{{\"name\": \"{}\", \"ref\": {}, \"op_index\": {}}}",
                    escape_bytes(&f.name),
                    f.xobj
                        .map(|r| format!("\"{r}\""))
                        .unwrap_or_else(|| "null".into()),
                    f.op_index
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(ci) = t.site.code_index {
        s.push_str(&format!(", \"code_index\": {ci}"));
    }
    s.push('}');
    match &t.detail {
        D::Text {
            code,
            unicode,
            font,
            render_mode,
            origin,
            bounds_kind,
        } => s.push_str(&format!(
            ", \"text\": {{\"code\": {}, \"unicode\": {}, \"font\": \"{}\", \"mode\": {}, \"origin\": [{}, {}], \"bounds_kind\": \"{:?}\"}}",
            code,
            unicode
                .as_ref()
                .map(|u| format!("{u:?}"))
                .unwrap_or_else(|| "null".into()),
            escape_bytes(font),
            render_mode,
            fmt_num(origin.0),
            fmt_num(origin.1),
            bounds_kind
        )),
        D::Image {
            xobj,
            name,
            size,
            blockers,
            ..
        } => s.push_str(&format!(
            ", \"image\": {{\"name\": \"{}\", \"ref\": {}, \"size\": [{}, {}], \"blockers\": [{}]}}",
            escape_bytes(name),
            xobj.map(|r| format!("\"{r}\""))
                .unwrap_or_else(|| "null".into()),
            size.0,
            size.1,
            blockers
                .iter()
                .map(|b| format!("{b:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        D::Path { op, stroked } => {
            s.push_str(&format!(
                ", \"path\": {{\"op\": \"{}\", \"stroked\": {}}}",
                op, stroked
            ))
        }
        D::Form {
            xobj,
            name,
            bbox,
            matrix,
        } => s.push_str(&format!(
            ", \"form\": {{\"name\": \"{}\", \"ref\": {}, \"bbox\": [{}, {}, {}, {}], \"matrix\": [{}]}}",
            escape_bytes(name),
            xobj.map(|r| format!("\"{r}\""))
                .unwrap_or_else(|| "null".into()),
            fmt_num(bbox.0),
            fmt_num(bbox.1),
            fmt_num(bbox.2),
            fmt_num(bbox.3),
            matrix
                .iter()
                .map(|v| fmt_num(*v))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        D::Other { what } => s.push_str(&format!(", \"other\": {what:?}")),
    }
    s.push('}');
    s
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

// ---------------------------------------------------------------------
// Audit-residuals
// ---------------------------------------------------------------------

/// Output cap — a single audit prints at most this many findings;
/// every truncation states the omitted count.
const MAX_AUDIT_LINES: usize = 2_000;

fn run_audit(args: &[String]) -> Result<(), Error> {
    if args.is_empty() {
        return Err(Error::Structure(
            "usage: kasane-pdf audit-residuals <file.pdf> [--format text|json]".into(),
        ));
    }
    let input = &args[0];
    let mut format = "text";
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                if i + 1 >= args.len() {
                    return Err(Error::Structure("--format expects text|json".into()));
                }
                format = args[i + 1].as_str();
                if format != "text" && format != "json" {
                    return Err(Error::Structure(format!(
                        "unknown --format '{format}' (text|json)"
                    )));
                }
                i += 2;
            }
            other => {
                return Err(Error::Structure(format!("unknown option '{other}'")));
            }
        }
    }
    let data = std::fs::read(input)?;
    let doc = Document::from_bytes(&data)?;
    let audit = kasane_pdf::audit_residuals(&doc, &Limits::default())?;
    match format {
        "json" => print_audit_json(&audit),
        _ => print_audit_text(&audit),
    }
    Ok(())
}

/// Escape a byte string for JSON — printable ASCII passes through,
/// everything else becomes `\u00XX` (the byte value as a codepoint).
fn json_escape_bytes(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for &c in b {
        match c {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(c as char),
            _ => out.push_str(&format!("\\u{c:04x}")),
        }
    }
    out
}

fn path_steps_json(via: &[kasane_pdf::PathStep]) -> String {
    let parts: Vec<String> = via
        .iter()
        .map(|s| match s {
            kasane_pdf::PathStep::TrailerKey(k) => {
                format!("trailer/{}", json_escape_bytes(k))
            }
            kasane_pdf::PathStep::Key(k) => format!("/{}", json_escape_bytes(k)),
            kasane_pdf::PathStep::Index(i) => format!("[{i}]"),
            kasane_pdf::PathStep::NameTreeEntry(n) => {
                format!("name-tree:{}", json_escape_bytes(n))
            }
            kasane_pdf::PathStep::ResourceName { category, name } => format!(
                "/{}/{}",
                json_escape_bytes(category),
                json_escape_bytes(name)
            ),
        })
        .collect();
    format!(
        "[{}]",
        parts
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn print_audit_text(a: &kasane_pdf::ResidualAudit) {
    println!("audit-residuals — residual-information inventory (read-only)");
    println!("objects-examined: {}", a.objects_examined);
    println!("edges-examined: {}", a.edges_examined);
    let r = &a.reachability;
    println!(
        "reachability: {} live / {} reachable / {} unreachable-live / {} unevaluated-live / {} edges / {} missing-refs{}",
        r.live_objects,
        r.reachable_live,
        r.unreachable_live,
        r.unevaluated_live,
        r.edges_followed,
        r.missing_references,
        if r.walk_complete { "" } else { " (walk incomplete)" }
    );
    let h = &a.history;
    println!(
        "history: {} xref section(s){}, {} free, {} compressed, {} in old region",
        h.xref_sections,
        h.incremental_history
            .map(|n| format!(", {n} prior revision(s)"))
            .unwrap_or_default(),
        h.free_entries,
        h.compressed_entries,
        h.objects_in_old_region,
    );
    if !h.physical_slack_examined {
        println!("history: physical slack NOT examined (xref-only audit)");
    }
    let (findings, omitted) = {
        let shown = a.findings.len().min(MAX_AUDIT_LINES);
        (&a.findings[..shown], a.findings.len() - shown)
    };
    println!("findings {}", a.findings.len());
    for f in findings {
        let obj = f
            .object
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".into());
        let bytes = f
            .bytes
            .map(|(enc, dec)| {
                format!(
                    " bytes={}{}",
                    enc,
                    dec.map(|d| format!("(declared {d})")).unwrap_or_default()
                )
            })
            .unwrap_or_default();
        println!(
            "  {:?} {} reach={:?}{} {}",
            f.kind, obj, f.reach, bytes, f.note
        );
    }
    if omitted > 0 {
        println!("  ... ({omitted} more — output capped at {MAX_AUDIT_LINES})");
    }
    if !a.incomplete.is_empty() {
        println!("incomplete {}", a.incomplete.len());
        for i in &a.incomplete {
            println!(
                "  {} — {}{}",
                i.scope,
                i.reason,
                if i.limit { " (limit)" } else { "" }
            );
        }
    }
    if !a.unexamined.is_empty() {
        println!("unexamined {}", a.unexamined.len());
        for u in &a.unexamined {
            println!("  - {u}");
        }
    }
    println!(
        "note: a listed item means 'this construct exists', not \
         'this is sensitive'; a clean report is NOT a distribution-safety \
         verdict."
    );
}

fn print_audit_json(a: &kasane_pdf::ResidualAudit) {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"objects_examined\": {},\n",
        a.objects_examined
    ));
    out.push_str(&format!("  \"edges_examined\": {},\n", a.edges_examined));
    let r = &a.reachability;
    out.push_str(&format!(
        "  \"reachability\": {{\"live_objects\": {}, \"reachable\": {}, \"reachable_live\": {}, \"unreachable_live\": {}, \"unevaluated_live\": {}, \"edges_followed\": {}, \"missing_references\": {}, \"walk_complete\": {}}},\n",
        r.live_objects, r.reachable, r.reachable_live, r.unreachable_live,
        r.unevaluated_live, r.edges_followed, r.missing_references,
        r.walk_complete
    ));
    let h = &a.history;
    out.push_str(&format!(
        "  \"history\": {{\"xref_sections\": {}, \"incremental_history\": {}, \"free_entries\": {}, \"compressed_entries\": {}, \"objects_in_old_region\": {}, \"physical_slack_examined\": {}}},\n",
        h.xref_sections,
        h.incremental_history.map(|n| n.to_string()).unwrap_or_else(|| "null".into()),
        h.free_entries, h.compressed_entries, h.objects_in_old_region,
        h.physical_slack_examined
    ));
    let shown = a.findings.len().min(MAX_AUDIT_LINES);
    out.push_str(&format!(
        "  \"findings\": {},\n  \"finding_list\": [\n",
        a.findings.len()
    ));
    for (i, f) in a.findings.iter().take(shown).enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("    {");
        out.push_str(&format!("\"kind\": \"{:?}\"", f.kind));
        if let Some(o) = f.object {
            out.push_str(&format!(", \"object\": \"{o}\""));
        }
        out.push_str(&format!(", \"reach\": \"{:?}\"", f.reach));
        if let Some((enc, dec)) = f.bytes {
            out.push_str(&format!(
                ", \"bytes\": {}{}",
                enc,
                dec.map(|d| format!(", \"declared_length\": {d}"))
                    .unwrap_or_default()
            ));
        }
        out.push_str(&format!(
            ", \"via\": {}, \"note\": \"{}\"",
            path_steps_json(&f.via),
            json_escape_bytes(f.note.as_bytes())
        ));
        out.push('}');
    }
    out.push_str("\n  ]");
    if shown < a.findings.len() {
        out.push_str(&format!(
            ",\n  \"findings_omitted\": {}",
            a.findings.len() - shown
        ));
    }
    if !a.incomplete.is_empty() {
        out.push_str(",\n  \"incomplete\": [");
        for (i, inc) in a.incomplete.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "{{\"scope\": \"{}\", \"reason\": \"{}\", \"limit\": {}}}",
                json_escape_bytes(inc.scope.as_bytes()),
                json_escape_bytes(inc.reason.as_bytes()),
                inc.limit
            ));
        }
        out.push(']');
    }
    if !a.unexamined.is_empty() {
        out.push_str(",\n  \"unexamined\": [");
        for (i, u) in a.unexamined.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("\"{}\"", json_escape_bytes(u.as_bytes())));
        }
        out.push(']');
    }
    out.push_str("\n}\n");
    print!("{out}");
}

#[cfg(test)]
mod tests {
    //! Unit tests for the atomic-write helper — the CLI-level
    //! behaviour is covered by `tests/cli_rewrite.rs`; here we pin down
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
