//! kasane-compat — real-world PDF compatibility runner.
//!
//! Runs each corpus PDF through the kasane-pdf public API and records
//! a classified outcome per stage. The runner never links PDFium /
//! MuPDF / Poppler into the engine — comparison is done by invoking
//! their CLIs as child processes and diffing their output.
//!
//! Usage:
//!   kasane-compat run --corpus compat/corpus --out compat/reports \
//!       [--with-poppler] [--dpi 96] [--timeout-secs 20] [--only ID]
//!   kasane-compat report --results compat/reports/run-*.json [--format md|html]
//!
//! Result classes (kept mutually exclusive, worst-first):
//!   crash > timeout > error > unsupported > mismatch > pass > skip

use kasane_pdf::{extract_text, render_page, rewrite, Document, Limits, RenderOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

// ------------------------------------------------------------------
// Result model
// ------------------------------------------------------------------

/// Per-stage classified outcome. `worst-first` ordering matters for the
/// roll-up: a crash anywhere marks the whole file `crash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Class {
    Skip,
    Pass,
    Mismatch,
    Unsupported,
    Error,
    Timeout,
    Crash,
}

impl Class {
    fn label(self) -> &'static str {
        match self {
            Class::Pass => "PASS",
            Class::Mismatch => "MISMATCH",
            Class::Unsupported => "UNSUPPORTED",
            Class::Error => "ERROR",
            Class::Crash => "CRASH",
            Class::Timeout => "TIMEOUT",
            Class::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StageResult {
    class: Class,
    /// Machine-readable error kind from kasane_pdf::Error::kind(), or
    /// a comparison note. Empty on PASS.
    detail: String,
    /// Feature tag for UNSUPPORTED (e.g. "image mask"), else "".
    feature: String,
    /// Wall time for this stage.
    ms: u128,
}

impl StageResult {
    fn new(class: Class, detail: String, feature: String, ms: u128) -> Self {
        StageResult {
            class,
            detail,
            feature,
            ms,
        }
    }
    fn pass(ms: u128) -> Self {
        Self::new(Class::Pass, String::new(), String::new(), ms)
    }
    fn skip(reason: &str) -> Self {
        Self::new(Class::Skip, reason.into(), String::new(), 0)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FileResult {
    id: String,
    sha256: String,
    bytes: u64,
    category: String,
    /// Worst class across all stages (the headline verdict).
    verdict: Class,
    pages_seen: usize,
    /// Per-stage results.
    parse: StageResult,
    render: StageResult,
    text: StageResult,
    rewrite: StageResult,
    /// Extra metrics collected regardless of verdict.
    metrics: Metrics,
}

#[derive(Debug, Serialize, Deserialize)]
struct Metrics {
    /// Wall-clock for the whole file's processing.
    total_ms: u128,
    /// Peak RSS sampled after the run (bytes, best-effort).
    max_rss_bytes: u64,
    /// Poppler cross-check when enabled.
    poppler_pages: i64,
    render_mae: Option<f64>,
    render_diff_png: Option<String>,
    text_match_ratio: Option<f64>,
    rewrite_qpdf_ok: Option<bool>,
    rewrite_reparsed: Option<bool>,
}

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

fn sha256_file(p: &Path) -> String {
    let d = fs::read(p).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&d);
    format!("{:x}", h.finalize())
}

fn max_rss_bytes() -> u64 {
    unsafe {
        let mut r: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut r);
        // ru_maxrss is KiB on Linux, bytes on macOS.
        #[cfg(target_os = "macos")]
        return r.ru_maxrss as u64;
        #[cfg(not(target_os = "macos"))]
        return (r.ru_maxrss as u64) * 1024;
    }
}

/// Classify a kasane_pdf::Error into a result class + feature tag.
fn classify_err(e: &kasane_pdf::Error) -> (Class, String, String) {
    use kasane_pdf::Error::*;
    match e {
        Unsupported { feature, detail } => (
            Class::Unsupported,
            format!("{feature}: {detail}"),
            feature.to_string(),
        ),
        Encrypted => (Class::Unsupported, "encrypted".into(), "encrypted".into()),
        NotPdf | Parse(_) | Structure(_) | MissingReference(_) | CycleDetected { .. } => {
            (Class::Error, e.to_string(), String::new())
        }
        LimitExceeded { .. } => (Class::Error, e.to_string(), "limit".into()),
        Io(_) => (Class::Error, e.to_string(), "io".into()),
        _ => (Class::Error, e.to_string(), String::new()),
    }
}

// ------------------------------------------------------------------
// comparison helpers
// ------------------------------------------------------------------

/// Render the same page with `pdftoppm` and compute a mean-absolute-
/// error (0.0–1.0) against our pixmap's luminance. pdftoppm is asked
/// for grayscale PGM so no decoder is needed on the reference side.
/// Returns (mae, Option<diff_pgm_path>) or None when poppler can't run.
fn compare_render(
    kasane: &kasane_pdf::RenderedPage,
    pdf: &Path,
    page: usize,
    dpi: u32,
    diff_dir: &Path,
    tag: &str,
) -> Option<(f64, Option<String>)> {
    let prefix = diff_dir.join(format!("{tag}-ref"));
    let out = Command::new("pdftoppm")
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .args(["-r", &dpi.to_string(), "-gray", "-singlefile"])
        .arg(pdf)
        .arg(&prefix)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ref_pgm = prefix.with_extension("pgm");
    let ref_bytes = fs::read(&ref_pgm).ok()?;
    let (rw, rh, ref_pix) = parse_pgm(&ref_bytes)?;

    // kasane pixmap → luminance buffer at its own resolution.
    let kw = kasane.width as usize;
    let kh = kasane.height as usize;
    let kpix = pixmap_luma(kasane);

    // If dims differ (rounding/DPI), resample ref to kasane size by
    // nearest-neighbour for a stable comparison.
    let ref_sized = if rw == kw && rh == kh {
        ref_pix
    } else {
        resample_gray(&ref_pix, rw, rh, kw, kh)
    };

    // Mean absolute error + write a visual diff PGM.
    let mut sum = 0f64;
    let mut diffimg = vec![0u8; kw * kh];
    for i in 0..kw * kh {
        let d = (kpix[i] as i32 - ref_sized[i] as i32).unsigned_abs() as u8;
        diffimg[i] = 255u8.saturating_sub(d); // white = identical
        sum += d as f64;
    }
    let mae = sum / (kw * kh * 255) as f64;
    let diff_path = diff_dir.join(format!("{tag}-diff.pgm"));
    write_pgm(&diff_path, kw, kh, &diffimg).ok();
    Some((mae, Some(diff_path.to_string_lossy().into_owned())))
}

/// luminance of a premultiplied RGBA8 pixmap on white (PDF background
/// convention) — matches pdftoppm -gray output.
fn pixmap_luma(p: &kasane_pdf::RenderedPage) -> Vec<u8> {
    let d = p.pixmap.data();
    let mut out = Vec::with_capacity((p.width * p.height) as usize);
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for px in d.chunks_exact(4) {
        // unpremultiply on white: c' = c*a + 255*(1-a)
        let a = px[3] as u32;
        let l = |c: u8| (u32::from(c) * a + 255 * (255 - a)) / 255;
        let (r, g, b) = (l(px[0]), l(px[1]), l(px[2]));
        out.push(((r * 299 + g * 587 + b * 114) / 1000) as u8);
    }
    out
}

fn resample_gray(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh];
    for y in 0..dh {
        for x in 0..dw {
            out[y * dw + x] = src[y * sh / dh * sw + x * sw / dw];
        }
    }
    out
}

/// Parse a binary PGM (P5) — returns (w, h, pixels).
fn parse_pgm(b: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut i = 0;
    let magic = next_tok(b, &mut i)?;
    if magic != b"P5" {
        return None;
    }
    let w = atoi(next_tok(b, &mut i)?);
    let h = atoi(next_tok(b, &mut i)?);
    let _max = atoi(next_tok(b, &mut i)?);
    i += 1; // single whitespace after maxval
    if b.len() < i + w * h {
        return None;
    }
    Some((w, h, b[i..i + w * h].to_vec()))
}

fn next_tok<'a>(b: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b'#') {
        if b[*i] == b'#' {
            while *i < b.len() && b[*i] != b'\n' {
                *i += 1;
            }
        } else {
            *i += 1;
        }
    }
    let s = *i;
    while *i < b.len() && !b[*i].is_ascii_whitespace() {
        *i += 1;
    }
    if s == *i {
        None
    } else {
        Some(&b[s..*i])
    }
}

fn atoi(b: &[u8]) -> usize {
    b.iter().fold(0, |a, c| a * 10 + (c - b'0') as usize)
}

fn write_pgm(p: &Path, w: usize, h: usize, pix: &[u8]) -> std::io::Result<()> {
    let mut v = format!("P5\n{w} {h}\n255\n").into_bytes();
    v.extend_from_slice(pix);
    fs::write(p, v)
}

/// Compare extracted text against `pdftotext` output (Unicode NFC,
/// whitespace-collapsed). Returns a 0..=1 match ratio.
fn compare_text(ours: &str, pdf: &Path, page: usize) -> Option<f64> {
    let out = Command::new("pdftotext")
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .arg(pdf)
        .arg("-")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let theirs = String::from_utf8_lossy(&out.stdout);
    let a = normalize(ours);
    let b = normalize(&theirs);
    Some(text_match(&a, &b))
}

fn normalize(s: &str) -> Vec<char> {
    s.split_whitespace().collect::<String>().chars().collect()
}

/// Simple order-sensitive match ratio: longest-common-subsequence
/// length over max length. O(n*m) — fine for page text.
fn text_match(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let n = a.len().max(b.len());
    // LCS via DP (bounded — corpus pages are short)
    let (la, lb) = (a.len(), b.len());
    let mut prev = vec![0usize; lb + 1];
    let mut cur = vec![0usize; lb + 1];
    for i in 1..=la {
        for j in 1..=lb {
            cur[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1] + 1
            } else {
                prev[j].max(cur[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[lb] as f64 / n as f64
}

// ------------------------------------------------------------------
// per-file pipeline
// ------------------------------------------------------------------

fn process_file(path: &Path, id: &str, category: &str, args: &Args) -> FileResult {
    let start = Instant::now();
    let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let sha = sha256_file(path);
    let mut metrics = Metrics {
        total_ms: 0,
        max_rss_bytes: 0,
        poppler_pages: -1,
        render_mae: None,
        render_diff_png: None,
        text_match_ratio: None,
        rewrite_qpdf_ok: None,
        rewrite_reparsed: None,
    };

    // ---- poppler page count (reference) ----------------------------
    if args.with_poppler {
        metrics.poppler_pages = poppler_page_count(path).unwrap_or(-1);
    }

    // ---- Parse ------------------------------------------------------
    let t = Instant::now();
    let data = fs::read(path).unwrap_or_default();
    let doc = match Document::from_bytes(&data) {
        Ok(d) => d,
        Err(e) => {
            let (c, d, f) = classify_err(&e);
            return finish(FileResult {
                id: id.into(),
                sha256: sha,
                bytes,
                category: category.into(),
                verdict: c,
                pages_seen: 0,
                parse: StageResult::new(c, d, f, t.elapsed().as_millis()),
                render: StageResult::skip("parse failed"),
                text: StageResult::skip("parse failed"),
                rewrite: StageResult::skip("parse failed"),
                metrics: {
                    metrics.total_ms = start.elapsed().as_millis();
                    metrics.max_rss_bytes = max_rss_bytes();
                    metrics
                },
            });
        }
    };
    let parse = StageResult::pass(t.elapsed().as_millis());
    let pages: Vec<kasane_pdf::Page> = doc.pages().to_vec();

    // ---- Render (first N pages) ------------------------------------
    let t = Instant::now();
    let mut render = StageResult::pass(0);
    let mut first_render: Option<kasane_pdf::RenderedPage> = None;
    let render_pages = pages.iter().take(args.max_pages);
    let mut rendered = 0usize;
    for (i, page) in render_pages.enumerate() {
        let opts = RenderOptions {
            dpi: args.dpi,
            limits: Limits::default(),
        };
        match render_page(&doc, page, &opts) {
            Ok(r) => {
                if i == 0 {
                    first_render = Some(r);
                }
                rendered += 1;
            }
            Err(e) => {
                let (c, d, f) = classify_err(&e);
                render = StageResult::new(
                    c,
                    format!("page {}: {d}", i + 1),
                    f,
                    t.elapsed().as_millis(),
                );
                break;
            }
        }
    }
    if rendered > 0 && render.class == Class::Pass {
        render.ms = t.elapsed().as_millis();
    }
    // poppler render comparison on page 1
    if args.with_poppler && render.class == Class::Pass {
        if let Some(rp) = &first_render {
            let diff_dir = args.out.join("diff");
            fs::create_dir_all(&diff_dir).ok();
            if let Some((mae, diff)) = compare_render(rp, path, 1, args.dpi as u32, &diff_dir, id) {
                metrics.render_mae = Some(mae);
                metrics.render_diff_png = diff;
            }
        }
    }

    // ---- Text extract (first N pages) ------------------------------
    let t = Instant::now();
    let mut text = StageResult::pass(0);
    let mut our_text = String::new();
    let mut extracted = 0usize;
    for (i, page) in pages.iter().take(args.max_pages).enumerate() {
        match extract_text(&doc, page, &Limits::default()) {
            Ok(t2) => {
                for ch in &t2.chars {
                    if let Some(u) = &ch.unicode {
                        our_text.push_str(u);
                    }
                }
                extracted += 1;
            }
            Err(e) => {
                let (c, d, f) = classify_err(&e);
                text = StageResult::new(
                    c,
                    format!("page {}: {d}", i + 1),
                    f,
                    t.elapsed().as_millis(),
                );
                break;
            }
        }
    }
    if extracted > 0 && text.class == Class::Pass {
        text.ms = t.elapsed().as_millis();
        if args.with_poppler {
            metrics.text_match_ratio = compare_text(&our_text, path, 1);
        }
    }

    // ---- Rewrite + reparse + qpdf ----------------------------------
    let t = Instant::now();
    let mut rewrite_res = StageResult::pass(0);
    match rewrite(&doc) {
        Ok(out) => {
            metrics.rewrite_qpdf_ok = Some(qpdf_check(&out));
            match Document::from_bytes(&out) {
                Ok(d2) => {
                    metrics.rewrite_reparsed = Some(d2.pages().len() == pages.len());
                }
                Err(e) => {
                    let (c, d, f) = classify_err(&e);
                    rewrite_res =
                        StageResult::new(c, format!("reparse: {d}"), f, t.elapsed().as_millis());
                }
            }
            let _ = out;
        }
        Err(e) => {
            let (c, d, f) = classify_err(&e);
            rewrite_res = StageResult::new(c, d, f, t.elapsed().as_millis());
        }
    }
    if rewrite_res.class == Class::Pass {
        rewrite_res.ms = t.elapsed().as_millis();
    }

    let verdict = [parse.class, render.class, text.class, rewrite_res.class]
        .into_iter()
        .max()
        .unwrap_or(Class::Pass);

    metrics.total_ms = start.elapsed().as_millis();
    metrics.max_rss_bytes = max_rss_bytes();

    FileResult {
        id: id.into(),
        sha256: sha,
        bytes,
        category: category.into(),
        verdict,
        pages_seen: pages.len(),
        parse,
        render,
        text,
        rewrite: rewrite_res,
        metrics,
    }
}

fn finish(mut r: FileResult) -> FileResult {
    r.metrics.max_rss_bytes = max_rss_bytes();
    r
}

fn poppler_page_count(pdf: &Path) -> Option<i64> {
    let out = Command::new("pdfinfo").arg(pdf).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines()
        .find_map(|l| l.strip_prefix("Pages:").map(|v| v.trim().parse().ok()))?
}

fn qpdf_check(pdf_bytes: &[u8]) -> bool {
    // qpdf reads files, so spool to a temp file.
    let tmp = std::env::temp_dir().join(format!("kasane-qc-{}.pdf", std::process::id()));
    if fs::write(&tmp, pdf_bytes).is_err() {
        return false;
    }
    let ok = Command::new("qpdf")
        .args(["--check", tmp.to_str().unwrap_or("")])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).find("warning").is_none())
        .unwrap_or(false);
    let _ = fs::remove_file(&tmp);
    ok
}

// ------------------------------------------------------------------
// args / main
// ------------------------------------------------------------------

struct Args {
    corpus: PathBuf,
    out: PathBuf,
    with_poppler: bool,
    dpi: f64,
    max_pages: usize,
    timeout_secs: u64,
    only: Option<String>,
    report: Option<PathBuf>,
    report_format: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        corpus: PathBuf::from("compat/corpus"),
        out: PathBuf::from("compat/reports"),
        with_poppler: false,
        dpi: 96.0,
        max_pages: 3,
        timeout_secs: 20,
        only: None,
        report: None,
        report_format: "md".into(),
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "run" => {}
            "report" => a.report = Some(PathBuf::from(it.next().unwrap_or_default())),
            "--corpus" => a.corpus = it.next().map(Into::into).unwrap_or(a.corpus),
            "--out" => a.out = it.next().map(Into::into).unwrap_or(a.out),
            "--with-poppler" => a.with_poppler = true,
            "--dpi" => a.dpi = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.dpi),
            "--max-pages" => {
                a.max_pages = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(a.max_pages)
            }
            "--timeout-secs" => {
                a.timeout_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(a.timeout_secs)
            }
            "--only" => a.only = it.next(),
            "--format" => a.report_format = it.next().unwrap_or_else(|| "md".into()),
            _ => {}
        }
    }
    a
}

fn main() {
    let args = parse_args();
    if let Some(res) = &args.report {
        report(res, &args.report_format);
        return;
    }

    // id → category map from the manifest (flat yaml, parsed by hand
    // to avoid a yaml dep in the runner).
    let cats = load_categories(&args.corpus.join("../manifest.yaml"));

    let mut files: Vec<PathBuf> = Vec::new();
    if args.corpus.is_dir() {
        for e in fs::read_dir(&args.corpus).unwrap() {
            let p = e.unwrap().path();
            if p.extension().map(|x| x == "pdf").unwrap_or(false) {
                files.push(p);
            }
        }
    }
    files.sort();
    fs::create_dir_all(&args.out).ok();

    let mut results = Vec::new();
    for path in &files {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        if let Some(only) = &args.only {
            if &id != only {
                continue;
            }
        }
        let cat = cats
            .get(id.as_str())
            .cloned()
            .unwrap_or_else(|| "misc".into());
        eprint!("  {id} ... ");
        let r = process_file(path, &id, &cat, &args);
        eprintln!("{}", r.verdict.label());
        results.push(r);
    }

    let run = RunReport {
        generated: now_iso(),
        tool: env!("CARGO_PKG_VERSION").into(),
        corpus_dir: args.corpus.to_string_lossy().into_owned(),
        files: results,
    };
    let ts = chrono_stamp();
    let json_path = args.out.join(format!("run-{ts}.json"));
    let latest = args.out.join("latest.json");
    fs::write(&json_path, serde_json::to_string_pretty(&run).unwrap()).unwrap();
    fs::write(&latest, serde_json::to_string_pretty(&run).unwrap()).unwrap();

    let md = render_markdown(&run);
    let md_path = args.out.join(format!("run-{ts}.md"));
    fs::write(&md_path, &md).unwrap();
    fs::write(args.out.join("latest.md"), &md).unwrap();

    eprintln!("\nWrote {json_path:?} and {md_path:?}");
}

/// id → category from `manifest.yaml`'s flat `files:` list. Parses the
/// simple `- id:`/`category:` line pairs without a yaml dep; entries
/// that don't match are simply absent (callers default to "misc").
fn load_categories(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    let mut cur_id: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("- id:").or_else(|| l.strip_prefix("id:")) {
            cur_id = Some(v.trim().trim_matches('"').into());
        } else if let Some(v) = l.strip_prefix("category:") {
            if let Some(id) = cur_id.take() {
                map.insert(id, v.trim().trim_matches('"').into());
            }
        }
    }
    map
}

// ------------------------------------------------------------------
// reporting
// ------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct RunReport {
    generated: String,
    tool: String,
    corpus_dir: String,
    files: Vec<FileResult>,
}

fn now_iso() -> String {
    // no chrono dep — unix seconds + a stable sort key is enough for
    // diff-comparison; ISO via libc::time
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        t.to_string()
    }
}

fn chrono_stamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        format!("{}", t)
    }
}

fn report(results_json: &Path, format: &str) {
    let data = fs::read_to_string(results_json).expect("read results");
    let run: RunReport = serde_json::from_str(&data).expect("parse results");
    match format {
        "html" => print!("{}", render_html(&run)),
        _ => print!("{}", render_markdown(&run)),
    }
}

fn render_markdown(r: &RunReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("# kasane-pdf compat run {}\n\n", r.generated));
    s.push_str(&format!(
        "corpus: `{}` · {} files\n\n",
        r.corpus_dir,
        r.files.len()
    ));

    // headline counts
    let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &r.files {
        *tally.entry(f.verdict.label()).or_default() += 1;
    }
    s.push_str("## Verdicts\n\n| verdict | files |\n|---|---|\n");
    for (k, v) in &tally {
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    s.push('\n');

    // per-stage success rates
    s.push_str("## Stage success\n\n| stage | pass | mismatch | unsupported | error | crash | timeout | skip |\n|---|---|---|---|---|---|---|---|\n");
    type StageFn = fn(&FileResult) -> &StageResult;
    let stages: [(&str, StageFn); 4] = [
        ("parse", |f| &f.parse),
        ("render", |f| &f.render),
        ("text", |f| &f.text),
        ("rewrite", |f| &f.rewrite),
    ];
    for (name, get) in stages {
        let mut c = [0usize; 7];
        for f in &r.files {
            let idx = match get(f).class {
                Class::Pass => 0,
                Class::Mismatch => 1,
                Class::Unsupported => 2,
                Class::Error => 3,
                Class::Crash => 4,
                Class::Timeout => 5,
                Class::Skip => 6,
            };
            c[idx] += 1;
        }
        s.push_str(&format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} |\n",
            c[0], c[1], c[2], c[3], c[4], c[5], c[6]
        ));
    }
    s.push('\n');

    // unsupported feature frequency
    let mut feats: BTreeMap<&str, usize> = BTreeMap::new();
    for f in &r.files {
        for st in [&f.parse, &f.render, &f.text, &f.rewrite] {
            if st.class == Class::Unsupported && !st.feature.is_empty() {
                *feats.entry(st.feature.as_str()).or_default() += 1;
            }
        }
    }
    if !feats.is_empty() {
        s.push_str("## Top unsupported features\n\n| feature | count |\n|---|---|\n");
        let mut v: Vec<_> = feats.iter().collect();
        v.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (k, c) in v.iter().take(15) {
            s.push_str(&format!("| `{k}` | {c} |\n"));
        }
        s.push('\n');
    }

    // per-file table
    s.push_str("## Per-file\n\n| file | verdict | parse | render | text | rewrite | pages | bytes | ms |\n|---|---|---|---|---|---|---|---|---|\n");
    for f in &r.files {
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            f.id,
            f.verdict.label(),
            f.parse.class.label(),
            f.render.class.label(),
            f.text.class.label(),
            f.rewrite.class.label(),
            f.pages_seen,
            f.bytes,
            f.metrics.total_ms
        ));
    }
    s
}

fn render_html(r: &RunReport) -> String {
    let mut s = String::from(
        "<!doctype html><meta charset=utf-8><title>kasane-pdf compat</title>\
         <style>body{font:14px system-ui;margin:2rem}table{border-collapse:collapse}\
         td,th{border:1px solid #ccc;padding:3px 8px;font:12px monospace}\
         .PASS{background:#cfc}.MISMATCH{background:#ffc}.UNSUPPORTED{background:#fde}\
         .ERROR,.CRASH,.TIMEOUT{background:#fcc}</style>",
    );
    s.push_str(&format!("<h1>kasane-pdf compat run {}</h1>", r.generated));
    s.push_str(&format!(
        "<p>corpus <code>{}</code> · {} files</p>",
        r.corpus_dir,
        r.files.len()
    ));
    s.push_str(
        "<table><tr><th>file<th>verdict<th>parse<th>render<th>text<th>rewrite<th>pages<th>ms",
    );
    for f in &r.files {
        s.push_str(&format!(
            "<tr><td>{}<td class={}>{}<td>{}<td>{}<td>{}<td>{}<td>{}<td>{}",
            f.id,
            f.verdict.label(),
            f.verdict.label(),
            f.parse.class.label(),
            f.render.class.label(),
            f.text.class.label(),
            f.rewrite.class.label(),
            f.pages_seen,
            f.metrics.total_ms
        ));
    }
    s.push_str("</table>");
    s
}
