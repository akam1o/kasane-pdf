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

mod analysis;
mod cause;
mod fontaudit;
mod gsaudit;
mod probe;

use analysis::{priorities_table, PrioritiesConfig};
use cause::classify;
use kasane_pdf::{extract_text, render_page, rewrite, Document, Limits, RenderOptions};
use probe::probe_document;
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
    /// Structured cause id (e.g. "render.extgstate.unsupported") or "".
    /// `default` so reports saved before this field still deserialize.
    #[serde(default)]
    cause: String,
    /// Wall time for this stage.
    ms: u128,
}

impl StageResult {
    fn new(class: Class, detail: String, feature: String, ms: u128) -> Self {
        StageResult {
            class,
            detail,
            feature,
            cause: String::new(),
            ms,
        }
    }
    /// Like `new` but records the structured cause id too.
    fn caused(class: Class, detail: String, feature: String, cause: String, ms: u128) -> Self {
        StageResult {
            class,
            detail,
            feature,
            cause,
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
    /// Every cause id the probe found on this file (union over pages);
    /// empty when parse failed before any page could be probed.
    /// `default` for reports saved before the probe existed.
    #[serde(default)]
    causes: Vec<String>,
    /// cause id → # pages it was seen on (probe).
    #[serde(default)]
    cause_pages: BTreeMap<String, usize>,
    /// Expected outcome from the manifest (`pass`/`reject`/`any`),
    /// used to split the Compatibility vs Robustness rates.
    #[serde(default)]
    expect: String,
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
    /// SSIM vs the Poppler reference (1.0 = identical). `default` for
    /// reports that predate SSIM.
    #[serde(default)]
    render_ssim: Option<f64>,
    /// Fraction of pixels differing from the reference (0.0–1.0).
    #[serde(default)]
    render_diff_px_ratio: Option<f64>,
    /// Per-page render success: pages that rendered without error.
    #[serde(default)]
    render_pages_ok: u32,
    /// Pages attempted for render (before the first hard failure).
    #[serde(default)]
    render_pages_total: u32,
    render_diff_png: Option<String>,
    /// Path to our own page-1 render (PGM) saved for inspection.
    #[serde(default)]
    render_ours_png: Option<String>,
    /// Path to the reference (Poppler) page-1 render (PGM).
    #[serde(default)]
    render_ref_png: Option<String>,
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

/// Classify a kasane_pdf::Error into (class, detail, feature, cause-id).
fn classify_err(stage: &str, e: &kasane_pdf::Error) -> (Class, String, String, String) {
    let c = classify(stage, e);
    use kasane_pdf::Error::*;
    let feature = match e {
        Unsupported { feature, .. } => feature.to_string(),
        Encrypted => "encrypted".into(),
        LimitExceeded { .. } => "limit".into(),
        Io(_) => "io".into(),
        _ => String::new(),
    };
    let class = match e {
        Unsupported { .. } | Encrypted => Class::Unsupported,
        _ => Class::Error,
    };
    (class, c.feature, feature, c.id)
}

// ------------------------------------------------------------------
// comparison helpers
// ------------------------------------------------------------------

/// Per-page visual-fidelity result against the Poppler reference.
struct Fidelity {
    mae: f64,
    ssim: f64,
    /// fraction of pixels whose luminance differs by >8 (≈3%) — the
    /// "different pixel" ratio the task asks for.
    diff_px_ratio: f64,
    diff_path: Option<String>,
    ours_path: Option<String>,
    ref_path: Option<String>,
}

/// Render the same page with `pdftoppm` and compare against our pixmap's
/// luminance: MAE, SSIM, and the different-pixel ratio. We also save all
/// three images (ours / reference / diff) so a flagged page can be
/// eyeballed without re-running. Returns `None` when poppler can't run.
fn compare_render(
    kasane: &kasane_pdf::RenderedPage,
    pdf: &Path,
    page: usize,
    dpi: u32,
    diff_dir: &Path,
    tag: &str,
) -> Option<Fidelity> {
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

    // MAE + different-pixel ratio + visual diff image.
    let mut sum = 0f64;
    let mut diffs = 0usize;
    let mut diffimg = vec![0u8; kw * kh];
    for i in 0..kw * kh {
        let d = (kpix[i] as i32 - ref_sized[i] as i32).unsigned_abs() as u8;
        diffimg[i] = 255u8.saturating_sub(d); // white = identical
        sum += d as f64;
        if d > 8 {
            diffs += 1;
        }
    }
    let mae = sum / (kw * kh * 255) as f64;
    let diff_px_ratio = diffs as f64 / (kw * kh) as f64;
    let ssim = ssim_gray(&kpix, &ref_sized, kw, kh);

    // Save ours, reference (resized), and diff images for inspection.
    let ours_path = diff_dir.join(format!("{tag}-ours.pgm"));
    let refr_path = diff_dir.join(format!("{tag}-ref.pgm"));
    let diff_path = diff_dir.join(format!("{tag}-diff.pgm"));
    write_pgm(&ours_path, kw, kh, &kpix).ok();
    write_pgm(&refr_path, kw, kh, &ref_sized).ok();
    write_pgm(&diff_path, kw, kh, &diffimg).ok();
    Some(Fidelity {
        mae,
        ssim,
        diff_px_ratio,
        diff_path: Some(diff_path.to_string_lossy().into_owned()),
        ours_path: Some(ours_path.to_string_lossy().into_owned()),
        ref_path: Some(refr_path.to_string_lossy().into_owned()),
    })
}

/// Structural Similarity Index (Wang et al.) over two equal-size
/// grayscale buffers — the standard luminance/contrast/structure
/// metric. Uses 8×8 windows, stride 4, C1/C2 for the 0–255 range.
fn ssim_gray(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let mut sum = 0.0;
    let mut n = 0usize;
    let mut y = 0usize;
    while y + 8 <= h {
        let mut x = 0usize;
        while x + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for yy in y..y + 8 {
                for xx in x..x + 8 {
                    let pa = a[yy * w + xx] as f64;
                    let pb = b[yy * w + xx] as f64;
                    sa += pa;
                    sb += pb;
                    saa += pa * pa;
                    sbb += pb * pb;
                    sab += pa * pb;
                }
            }
            let cnt = 64.0;
            let ua = sa / cnt;
            let ub = sb / cnt;
            let va = saa / cnt - ua * ua;
            let vb = sbb / cnt - ub * ub;
            let cov = sab / cnt - ua * ub;
            let num = (2.0 * ua * ub + C1) * (2.0 * cov + C2);
            let den = (ua * ua + ub * ub + C1) * (va + vb + C2);
            sum += num / den;
            n += 1;
            x += 4;
        }
        y += 4;
    }
    if n == 0 {
        return 1.0;
    }
    sum / n as f64
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
        render_ssim: None,
        render_diff_px_ratio: None,
        render_pages_ok: 0,
        render_pages_total: 0,
        render_diff_png: None,
        render_ours_png: None,
        render_ref_png: None,
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
            let (c, d, f, cid) = classify_err("parse", &e);
            let mut parse = StageResult::new(c, d, f, t.elapsed().as_millis());
            parse.cause = cid;
            return finish(FileResult {
                id: id.into(),
                sha256: sha,
                bytes,
                category: category.into(),
                verdict: c,
                pages_seen: 0,
                parse,
                render: StageResult::skip("parse failed"),
                text: StageResult::skip("parse failed"),
                rewrite: StageResult::skip("parse failed"),
                causes: Vec::new(),
                cause_pages: BTreeMap::new(),
                expect: String::new(),
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

    // ---- Probe: enumerate *all* blockers (read-only) ----------------
    let probe = probe_document(&doc, args.max_pages);
    let mut causes: Vec<String> = probe.causes.iter().cloned().collect();
    causes.sort();
    let cause_pages: BTreeMap<String, usize> = probe.pages_per_cause.clone();

    // ---- Render (first N pages) ------------------------------------
    let t = Instant::now();
    let mut render = StageResult::pass(0);
    let render_pages = pages.iter().take(args.max_pages);
    let mut rendered = 0usize;
    let mut attempted = 0usize;
    let mut worst: Option<(f64, f64)> = None; // (min ssim, max mae)
    let mut best_fid: Option<Fidelity> = None;
    let mut best_pgno = 0usize;
    for (i, page) in render_pages.enumerate() {
        attempted += 1;
        let opts = RenderOptions {
            dpi: args.dpi,
            limits: Limits::default(),
        };
        match render_page(&doc, page, &opts) {
            Ok(r) => {
                rendered += 1;
                // Visual fidelity vs poppler on every rendered page.
                if args.with_poppler {
                    let diff_dir = args.out.join("diff");
                    fs::create_dir_all(&diff_dir).ok();
                    let tag = format!("{id}-p{}", i + 1);
                    if let Some(f) =
                        compare_render(&r, path, i + 1, args.dpi as u32, &diff_dir, &tag)
                    {
                        let worse = match worst {
                            Some((s, _)) => f.ssim < s,
                            None => true,
                        };
                        if worse {
                            worst = Some((f.ssim, f.mae));
                            best_fid = Some(f);
                            best_pgno = i + 1;
                        }
                    }
                }
            }
            Err(e) => {
                let (c, d, f, cid) = classify_err("render", &e);
                render = StageResult::caused(
                    c,
                    format!("page {}: {d}", i + 1),
                    f,
                    cid,
                    t.elapsed().as_millis(),
                );
                break;
            }
        }
    }
    metrics.render_pages_ok = rendered as u32;
    metrics.render_pages_total = attempted as u32;
    if rendered > 0 && render.class == Class::Pass {
        render.ms = t.elapsed().as_millis();
    }
    // Record the worst-fidelity page's metrics + images. The detailed
    // ours/ref/diff paths let us eyeball exactly the page that diverged.
    if let Some(f) = best_fid {
        metrics.render_mae = Some(f.mae);
        metrics.render_ssim = Some(f.ssim);
        metrics.render_diff_px_ratio = Some(f.diff_px_ratio);
        metrics.render_diff_png = f.diff_path;
        metrics.render_ours_png = f.ours_path;
        metrics.render_ref_png = f.ref_path;
        let _ = best_pgno;
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
                let (c, d, f, cid) = classify_err("text", &e);
                text = StageResult::caused(
                    c,
                    format!("page {}: {d}", i + 1),
                    f,
                    cid,
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
                    let (c, d, f, cid) = classify_err("rewrite", &e);
                    rewrite_res = StageResult::caused(
                        c,
                        format!("reparse: {d}"),
                        f,
                        cid,
                        t.elapsed().as_millis(),
                    );
                }
            }
            let _ = out;
        }
        Err(e) => {
            let (c, d, f, cid) = classify_err("rewrite", &e);
            rewrite_res = StageResult::caused(c, d, f, cid, t.elapsed().as_millis());
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
        causes,
        cause_pages,
        expect: String::new(),
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
    /// Previous run JSON to diff against (`report --compare old.json`).
    compare: Option<PathBuf>,
    /// Run the ExtGState usage audit instead of a compat run.
    gs_audit: bool,
    /// Run the font usage audit instead of a compat run.
    font_audit: bool,
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
        compare: None,
        gs_audit: false,
        font_audit: false,
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
            "--compare" => a.compare = it.next().map(PathBuf::from),
            "--gs-audit" => a.gs_audit = true,
            "--font-audit" => a.font_audit = true,
            _ => {}
        }
    }
    a
}

fn main() {
    let args = parse_args();
    if let Some(res) = &args.report {
        report(res, &args.report_format, args.compare.as_deref());
        return;
    }

    // id → (category, expect) map from the manifest (flat yaml, parsed
    // by hand to avoid a yaml dep in the runner).
    let manifest = load_manifest(&args.corpus.join("../manifest.yaml"));

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

    // ---- ExtGState usage audit (diagnostic, not a compat run) ------
    if args.gs_audit {
        run_gs_audit(&files, &args);
        return;
    }
    if args.font_audit {
        run_font_audit(&files, &args);
        return;
    }

    let mut results = Vec::new();
    for path in &files {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        if let Some(only) = &args.only {
            if &id != only {
                continue;
            }
        }
        let entry = manifest.get(id.as_str()).cloned().unwrap_or_default();
        let cat = if entry.category.is_empty() {
            "misc".to_string()
        } else {
            entry.category.clone()
        };
        eprint!("  {id} ... ");
        let mut r = process_file(path, &id, &cat, &args);
        r.expect = entry.expect.clone();
        eprintln!("{}", r.verdict.label());
        results.push(r);
    }

    // ---- analysis ----------------------------------------------------

    // ---- analysis ----------------------------------------------------
    let cfg = PrioritiesConfig::default();
    let feature_stats = analysis::analyze(&results, &cfg);
    let stage_summary = analysis::stage_summary(&results);
    let categories = analysis::category_summary(&results);
    let split = analysis::split_summary(&results);

    // Record the corpus location relative to the cwd when possible so a
    // report written under an absolute --corpus doesn't leak a local
    // home-dir / machine path into a shareable artifact.
    let corpus_dir = args
        .corpus
        .strip_prefix(std::env::current_dir().unwrap_or_default())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| args.corpus.clone());
    let corpus_dir = if corpus_dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        corpus_dir
    };
    let run = RunReport {
        generated: now_iso(),
        tool: env!("CARGO_PKG_VERSION").into(),
        corpus_dir: corpus_dir.to_string_lossy().into_owned(),
        files: results,
        analysis: Some(Analysis {
            stage_summary,
            categories,
            features: feature_stats,
            split: Some(split),
            effort_is_estimate: true,
        }),
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

/// Manifest metadata for one corpus file.
#[derive(Debug, Default, Clone)]
struct ManifestEntry {
    category: String,
    /// Expected outcome: `pass` (well-formed, engine should handle),
    /// `reject` (adversarial/malformed/encrypted — must refuse
    /// safely), or `any` (valid but uses out-of-scope features —
    /// counted in neither the compatibility nor robustness rate).
    expect: String,
}

/// id → (category, expect) from `manifest.yaml`'s flat `files:` list.
/// Parses `- id:` / `category:` / `expect:` line pairs without a yaml
/// dep; entries that don't match are simply absent (callers default).
fn load_manifest(path: &Path) -> BTreeMap<String, ManifestEntry> {
    let mut map = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return map;
    };
    let mut cur_id: Option<String> = None;
    let mut cur = ManifestEntry::default();
    for line in text.lines() {
        let l = line.trim();
        if let Some(v) = l.strip_prefix("- id:").or_else(|| l.strip_prefix("id:")) {
            if let Some(id) = cur_id.take() {
                map.insert(id, cur.clone());
                cur = ManifestEntry::default();
            }
            cur_id = Some(v.trim().trim_matches('"').into());
        } else if let Some(v) = l.strip_prefix("category:") {
            cur.category = v.trim().trim_matches('"').into();
        } else if let Some(v) = l.strip_prefix("expect:") {
            cur.expect = v.trim().trim_matches('"').into();
        }
    }
    if let Some(id) = cur_id.take() {
        map.insert(id, cur);
    }
    map
}

// ------------------------------------------------------------------
// reporting
// ------------------------------------------------------------------

/// Aggregated analysis attached to a run report.
#[derive(Debug, Serialize, Deserialize)]
struct Analysis {
    stage_summary: analysis::StageSummary,
    categories: Vec<analysis::CategorySummary>,
    features: Vec<analysis::FeatureStat>,
    /// Compatibility-vs-Robustness split (expect-tag driven).
    #[serde(default)]
    split: Option<analysis::SplitSummary>,
    /// effort values are person-day *estimates*, flagged so readers
    /// don't treat them as measured.
    effort_is_estimate: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct RunReport {
    generated: String,
    tool: String,
    corpus_dir: String,
    files: Vec<FileResult>,
    /// Present on reports generated by this version; older saved
    /// reports may not have it — `report` tolerates its absence.
    #[serde(default)]
    analysis: Option<Analysis>,
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

fn report(results_json: &Path, format: &str, compare: Option<&Path>) {
    let data = fs::read_to_string(results_json).expect("read results");
    let run: RunReport = serde_json::from_str(&data).expect("parse results");
    let mut out = match format {
        "html" => render_html(&run),
        _ => render_markdown(&run),
    };
    if let Some(old_path) = compare {
        let old: RunReport =
            serde_json::from_str(&fs::read_to_string(old_path).expect("read compare report"))
                .expect("parse compare report");
        out.push_str("\n\n---\n\n");
        out.push_str(&regression_markdown(&run, &old));
    }
    print!("{out}");
}

/// Diff `new` against `old`: newly-passing / newly-failing files,
/// changed verdicts, and cause-set changes. Same corpus assumed —
/// flagged when file sets differ.
fn regression_markdown(new: &RunReport, old: &RunReport) -> String {
    use std::collections::BTreeMap;
    let oldmap: BTreeMap<&str, &FileResult> =
        old.files.iter().map(|f| (f.id.as_str(), f)).collect();
    let newmap: BTreeMap<&str, &FileResult> =
        new.files.iter().map(|f| (f.id.as_str(), f)).collect();

    let mut s = String::from("# Regression analysis\n\n");
    if oldmap.len() != newmap.len() {
        s.push_str(&format!(
            "*file sets differ: old {} files, new {} files — the diff below only\n\
             covers ids present in both reports.*\n\n",
            oldmap.len(),
            newmap.len()
        ));
    }

    let mut gained = Vec::new();
    let mut lost = Vec::new();
    let mut verdict_changed = Vec::new();
    let mut cause_changed = Vec::new();
    let mut mae_improved = Vec::new();
    let mut mae_regressed = Vec::new();
    for (id, nf) in &newmap {
        let Some(of) = oldmap.get(id) else { continue };
        if nf.verdict != of.verdict {
            verdict_changed.push((*id, of.verdict, nf.verdict));
            if nf.verdict == Class::Pass {
                gained.push(*id);
            } else if of.verdict == Class::Pass {
                lost.push(*id);
            }
        }
        let oc: std::collections::BTreeSet<_> = of.causes.iter().collect();
        let nc: std::collections::BTreeSet<_> = nf.causes.iter().collect();
        if oc != nc {
            cause_changed.push(*id);
        }
        if let (Some(o), Some(n)) = (of.metrics.render_mae, nf.metrics.render_mae) {
            let d = n - o;
            if d < -0.005 {
                mae_improved.push((*id, o, n));
            } else if d > 0.005 {
                mae_regressed.push((*id, o, n));
            }
        }
    }

    s.push_str(&format!(
        "| metric | count |\n|---|---|\n| newly PASS | {} |\n| newly FAIL | {} |\n| verdict changed | {} |\n| cause set changed | {} |\n| render MAE improved | {} |\n| render MAE regressed | {} |\n\n",
        gained.len(),
        lost.len(),
        verdict_changed.len(),
        cause_changed.len(),
        mae_improved.len(),
        mae_regressed.len()
    ));
    if !verdict_changed.is_empty() {
        s.push_str("## Verdict changes\n\n| file | old | new |\n|---|---|---|\n");
        for (id, o, n) in &verdict_changed {
            s.push_str(&format!("| `{id}` | {} | {} |\n", o.label(), n.label()));
        }
        s.push('\n');
    }
    if !cause_changed.is_empty() {
        s.push_str(&format!(
            "## Cause-set changes ({})\n\n",
            cause_changed.len()
        ));
        for id in cause_changed.iter().take(30) {
            s.push_str(&format!("- `{id}`\n"));
        }
        s.push('\n');
    }
    s
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

    // ---- Analysis: stage denominators + categories + priorities ----
    if let Some(a) = &r.analysis {
        let s2 = &a.stage_summary;
        s.push_str("## Stage denominators\n\n");
        s.push_str(
            "| stage | attempted | skipped | pass | unsupported | error | pass-rate (attempted) | pass-rate (all) |\n\
             |---|---:|---:|---:|---:|---:|---:|---:|\n",
        );
        for (name, c) in [
            ("parse", &s2.parse),
            ("render", &s2.render),
            ("text", &s2.text),
            ("rewrite", &s2.rewrite),
        ] {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {:.0}% | {:.0}% |\n",
                name,
                c.attempted,
                c.skipped,
                c.pass,
                c.unsupported,
                c.error,
                c.pass_rate_attempted * 100.0,
                c.pass_rate_all * 100.0
            ));
        }
        s.push('\n');

        // ---- Render Visual Fidelity (separate from execution) -------
        // Execution success = the stage pass count above; fidelity =
        // how close the image is to the Poppler reference. A page that
        // renders but diverges badly is flagged here, not as a failure.
        let measured: Vec<&FileResult> = r
            .files
            .iter()
            .filter(|f| f.metrics.render_ssim.is_some())
            .collect();
        if !measured.is_empty() {
            let n = measured.len();
            let mean = |g: &dyn Fn(&FileResult) -> Option<f64>| -> f64 {
                let (s, c) = measured
                    .iter()
                    .filter_map(|f| g(f))
                    .fold((0.0, 0usize), |(s, c), v| (s + v, c + 1));
                if c == 0 {
                    0.0
                } else {
                    s / c as f64
                }
            };
            let mean_ssim = mean(&|f| f.metrics.render_ssim);
            let mean_mae = mean(&|f| f.metrics.render_mae);
            let mean_dpx = mean(&|f| f.metrics.render_diff_px_ratio);
            let min_ssim = measured
                .iter()
                .filter_map(|f| f.metrics.render_ssim)
                .fold(f64::INFINITY, f64::min);
            let max_mae = measured
                .iter()
                .filter_map(|f| f.metrics.render_mae)
                .fold(0.0f64, f64::max);
            // fidelity PASS = SSIM≥0.98 AND diff-pixel ratio≤2% AND
            // MAE≤0.05 — generous to AA differences between engines.
            let fid_pass = measured
                .iter()
                .filter(|f| {
                    f.metrics.render_ssim.unwrap_or(0.0) >= 0.98
                        && f.metrics.render_diff_px_ratio.unwrap_or(1.0) <= 0.02
                        && f.metrics.render_mae.unwrap_or(1.0) <= 0.05
                })
                .count();
            let pages_ok: u64 = measured
                .iter()
                .map(|f| f.metrics.render_pages_ok as u64)
                .sum();
            let pages_total: u64 = measured
                .iter()
                .map(|f| f.metrics.render_pages_total as u64)
                .sum();
            s.push_str("## Render Visual Fidelity\n\n");
            s.push_str(&format!(
                "*measured on {} file(s) with a Poppler reference — \
                 {fid_pass} meet the fidelity bar \
                 (SSIM ≥ 0.98, diff-pixels ≤ 2%, MAE ≤ 0.05). \
                 Per-page render success: {pages_ok}/{pages_total}.*\n\n",
                n
            ));
            s.push_str("| metric | mean | worst |\n|---|---:|---:|\n");
            s.push_str(&format!(
                "| SSIM (higher = closer) | {:.4} | {:.4} (min) |\n",
                mean_ssim, min_ssim
            ));
            s.push_str(&format!(
                "| MAE (lower = closer) | {:.4} | {:.4} (max) |\n",
                mean_mae, max_mae
            ));
            s.push_str(&format!("| diff-pixel ratio | {:.4} | — |\n\n", mean_dpx));
            // list the files that diverged beyond the bar
            let divergent: Vec<_> = measured
                .iter()
                .filter(|f| {
                    !(f.metrics.render_ssim.unwrap_or(0.0) >= 0.98
                        && f.metrics.render_diff_px_ratio.unwrap_or(1.0) <= 0.02
                        && f.metrics.render_mae.unwrap_or(1.0) <= 0.05)
                })
                .collect();
            if !divergent.is_empty() {
                s.push_str(&format!(
                    "**{} file(s) diverge from the reference** (image saved):\n\n",
                    divergent.len()
                ));
                s.push_str(
                    "| file | SSIM | MAE | diff-px | diff image |\n|---|---:|---:|---:|---|\n",
                );
                for f in divergent.iter().take(25) {
                    s.push_str(&format!(
                        "| `{}` | {:.3} | {:.3} | {:.3} | {} |\n",
                        f.id,
                        f.metrics.render_ssim.unwrap_or(0.0),
                        f.metrics.render_mae.unwrap_or(0.0),
                        f.metrics.render_diff_px_ratio.unwrap_or(0.0),
                        f.metrics
                            .render_diff_png
                            .as_deref()
                            .map(|p| format!("`{}`", p))
                            .unwrap_or_default()
                    ));
                }
                s.push('\n');
            }
        }

        // Compatibility vs Robustness — the two independent rates.
        if let Some(sp) = &a.split {
            s.push_str("## Compatibility vs Robustness\n\n");
            s.push_str(&format!(
                "**Compatibility** — {} well-formed files (`expect: pass`):\n\n",
                sp.compat_total
            ));
            s.push_str("| stage | pass | rate |\n|---|---:|---:|\n");
            for (name, n) in [
                ("parse", sp.compat_parse_pass),
                ("render", sp.compat_render_pass),
                ("text", sp.compat_text_pass),
                ("rewrite", sp.compat_rewrite_pass),
            ] {
                s.push_str(&format!(
                    "| {} | {} | {:.0}% |\n",
                    name,
                    n,
                    n as f64 / sp.compat_total.max(1) as f64 * 100.0
                ));
            }
            s.push_str(&format!(
                "\n**Robustness** — {} adversarial files (`expect: reject`):\n\
                 safely handled {} ({:.0}%), cleanly refused {} — \
                 **{} crash/hang failure(s)**.\n\n",
                sp.robust_total,
                sp.robust_safe,
                sp.robust_safe as f64 / sp.robust_total.max(1) as f64 * 100.0,
                sp.robust_refused,
                sp.robust_failures.len()
            ));
            if !sp.robust_failures.is_empty() {
                for id in &sp.robust_failures {
                    s.push_str(&format!("- `{id}` — crashed or hung on hostile input\n"));
                }
                s.push('\n');
            }
            s.push_str(&format!(
                "*{} files tagged `expect: any` are excluded from both rates\n\
                 (valid documents exercising out-of-scope features).*\n\n",
                sp.other_total
            ));
        }

        // categories
        s.push_str("## By category\n\n");
        s.push_str(
            "| category | files | parse | render | text | rewrite | verdict-pass |\n\
             |---|---:|---:|---:|---:|---:|---:|\n",
        );
        for c in &a.categories {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                c.category,
                c.files,
                c.parse_pass,
                c.render_pass,
                c.text_pass,
                c.rewrite_pass,
                c.verdict_pass
            ));
        }
        s.push('\n');

        // development priorities
        s.push_str("## Development priorities\n\n");
        s.push_str(&priorities_table(&a.features));
        s.push('\n');
        if a.effort_is_estimate {
            s.push_str(
                "*effort values are person-day **estimates** set in\n\
                 `analysis.rs` (`PrioritiesConfig`), not measured — adjust\n\
                 them as real implementation cost becomes known.*\n\n",
            );
        }

        // multi-blocker files
        let multi: Vec<_> = r
            .files
            .iter()
            .filter(|f| {
                let mut b = std::collections::BTreeSet::new();
                for c in &f.causes {
                    b.insert(analysis::feature_bucket(c));
                }
                b.len() > 1
            })
            .collect();
        if !multi.is_empty() {
            s.push_str(&format!(
                "## Multi-blocker files\n\n{} file(s) carry **more than one** unsupported feature — a single\nimplementation will not unblock them:\n\n",
                multi.len()
            ));
            for f in multi.iter().take(20) {
                let mut feats: Vec<&str> = f
                    .causes
                    .iter()
                    .map(|c| analysis::feature_bucket(c))
                    .collect();
                feats.sort();
                feats.dedup();
                s.push_str(&format!("- `{}` — {}\n", f.id, feats.join(", ")));
            }
            s.push('\n');
        }
    }

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
        s.push_str(
            "## Top unsupported features (first-error hits)\n\n| feature | count |\n|---|---|\n",
        );
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

    // ---- analysis block (mirrors the markdown sections) -----------
    if let Some(a) = &r.analysis {
        let s2 = &a.stage_summary;
        s.push_str("<h2>Stage denominators</h2><table><tr><th>stage<th>attempted<th>skipped<th>pass<th>unsupported<th>error<th>pass-rate (attempted)<th>pass-rate (all)");
        for (name, c) in [
            ("parse", &s2.parse),
            ("render", &s2.render),
            ("text", &s2.text),
            ("rewrite", &s2.rewrite),
        ] {
            s.push_str(&format!(
                "<tr><td>{name}<td>{}<td>{}<td>{}<td>{}<td>{}<td>{:.0}%<td>{:.0}%",
                c.attempted,
                c.skipped,
                c.pass,
                c.unsupported,
                c.error,
                c.pass_rate_attempted * 100.0,
                c.pass_rate_all * 100.0
            ));
        }
        s.push_str("</table>");

        s.push_str("<h2>By category</h2><table><tr><th>category<th>files<th>parse<th>render<th>text<th>rewrite<th>verdict-pass");
        for c in &a.categories {
            s.push_str(&format!(
                "<tr><td>{}<td>{}<td>{}<td>{}<td>{}<td>{}<td>{}",
                c.category,
                c.files,
                c.parse_pass,
                c.render_pass,
                c.text_pass,
                c.rewrite_pass,
                c.verdict_pass
            ));
        }
        s.push_str("</table>");

        s.push_str("<h2>Development priorities</h2><table><tr><th>feature<th>affected<th>sole-blocker<th>potentially unblocked<th>effort (pd)<th>score");
        for f in &a.features {
            let effort = f
                .effort
                .map(|e| e.to_string())
                .unwrap_or_else(|| "?".into());
            let score = f
                .score
                .map(|s| format!("{s:.2}"))
                .unwrap_or_else(|| "?".into());
            s.push_str(&format!(
                "<tr><td>{}<td>{}<td>{}<td>{}<td>{}<td>{}",
                f.feature, f.affected, f.sole_blocker, f.potentially_unblocked, effort, score
            ));
        }
        s.push_str("</table><p><i>potentially-unblocked is an estimate; effort values are person-day guesses in <code>PrioritiesConfig</code>, not measured.</i></p>");
    }

    s.push_str(
        "<h2>Per-file</h2><table><tr><th>file<th>verdict<th>parse<th>render<th>text<th>rewrite<th>pages<th>ms",
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

// ------------------------------------------------------------------
// ExtGState usage audit
// ------------------------------------------------------------------

/// Walk every corpus PDF, audit its `/ExtGState` resources, and print
/// a per-key usage table. Which keys appear, on how many files/pages,
/// and whether the gs was actually `gs`-invoked (vs merely defined).
fn run_gs_audit(files: &[PathBuf], args: &Args) {
    // key → (files_using, pages_using, sample_values)
    let mut key_files: BTreeMap<String, usize> = BTreeMap::new();
    let mut key_values: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut files_with_gs = 0usize;
    let mut files_invoked = 0usize;
    let mut total_entries = 0usize;
    let mut referenced_entries = 0usize;

    for path in files {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        if let Some(only) = &args.only {
            if &id != only {
                continue;
            }
        }
        let data = fs::read(path).unwrap_or_default();
        let doc = match Document::from_bytes(&data) {
            Ok(d) => d,
            Err(_) => continue, // parse-fail files carry no gs info
        };
        let a = gsaudit::audit(&doc);
        if a.entries.is_empty() {
            continue;
        }
        files_with_gs += 1;
        if !a.referenced.is_empty() {
            files_invoked += 1;
        }
        total_entries += a.entries.len();
        referenced_entries += a.referenced.len();

        // per-file key set (a file counts once per key regardless of
        // how many gs dicts use it)
        let mut file_keys: std::collections::BTreeSet<String> = Default::default();
        for e in &a.entries {
            for (k, v) in &e.keys {
                file_keys.insert(k.clone());
                *key_values
                    .entry(k.clone())
                    .or_default()
                    .entry(v.clone())
                    .or_default() += 1;
            }
        }
        for k in file_keys {
            *key_files.entry(k).or_default() += 1;
        }
    }

    println!("# ExtGState audit — {} corpus files", files.len());
    println!("files with any /ExtGState defined : {files_with_gs}");
    println!("files where a gs is invoked       : {files_invoked}");
    println!("total /ExtGState dicts            : {total_entries}");
    println!("referenced gs names               : {referenced_entries}");
    println!();
    println!("| key | files | common values |");
    println!("|---|---:|---|");
    let mut rows: Vec<_> = key_files.iter().collect();
    rows.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (k, c) in rows {
        let mut vals: Vec<_> = key_values[k].iter().collect();
        vals.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        let sample: Vec<String> = vals
            .iter()
            .take(4)
            .map(|(v, n)| format!("{v}×{n}"))
            .collect();
        println!("| `{k}` | {c} | {} |", sample.join(", "));
    }
}

// ------------------------------------------------------------------
// Font usage audit
// ------------------------------------------------------------------

fn run_font_audit(files: &[PathBuf], args: &Args) {
    // subtype → files; also bucket by embedded/encoding/tounicode
    let mut sub_files: BTreeMap<String, usize> = BTreeMap::new();
    let mut detail: BTreeMap<String, usize> = BTreeMap::new();
    let mut files_with_fonts = 0usize;

    for path in files {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        if let Some(only) = &args.only {
            if &id != only {
                continue;
            }
        }
        let data = fs::read(path).unwrap_or_default();
        let doc = match Document::from_bytes(&data) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let a = fontaudit::audit(&doc);
        if a.fonts.is_empty() {
            continue;
        }
        files_with_fonts += 1;
        let mut subs_in_file: std::collections::BTreeSet<String> = Default::default();
        for (n, f) in &a.fonts {
            subs_in_file.insert(f.subtype.clone());
            let key = format!(
                "{}|emb={}|enc={}|tu={}|w={}",
                f.subtype, f.embedded, f.encoding, f.to_unicode, f.widths
            );
            if f.used {
                *detail.entry(key).or_default() += 1;
            }
            let _ = n;
        }
        for s in subs_in_file {
            *sub_files.entry(s).or_default() += 1;
        }
        if args.only.is_some() {
            // detail dump for single file
            println!("== {id} ==");
            for (n, f) in &a.fonts {
                println!(
                    "  {n}: sub={} base={} emb={} desc={} enc={} tu={} w={} used={} cp={}",
                    f.subtype,
                    f.base_font,
                    f.embedded,
                    f.descriptor,
                    f.encoding,
                    f.to_unicode,
                    f.widths,
                    f.used,
                    f.charprocs
                );
            }
        }
    }

    println!("# Font audit — {} files with fonts", files_with_fonts);
    println!("| subtype | files |");
    println!("|---|---:|");
    let mut rows: Vec<_> = sub_files.iter().collect();
    rows.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (s, c) in rows {
        println!("| `{s}` | {c} |");
    }
    println!();
    println!("## Used-font shapes (only fonts actually Tf-invoked)");
    println!("| shape | files |");
    println!("|---|---:|");
    let mut rows: Vec<_> = detail.iter().collect();
    rows.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (s, c) in rows {
        println!("| `{s}` | {c} |");
    }
}
