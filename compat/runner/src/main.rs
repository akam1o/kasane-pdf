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
mod colour;
mod configs;
mod fontaudit;
mod gsaudit;
mod policy;
mod probe;
mod profile;
mod roundtrip;
mod semantic;

use analysis::{priorities_table, PrioritiesConfig};
use cause::classify;
use kasane_pdf::{
    extract_text, render_page, rewrite, Document, Limits, RenderOptions, TextOptions,
};
use probe::probe_document;
use roundtrip::{RoundTrip, RtClass, RtOptions, RtTally};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Serde default for reports written before the field existed.
    fn not_run() -> Self {
        StageResult::skip("not run")
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

impl Default for StageResult {
    fn default() -> Self {
        StageResult::skip("not attempted")
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
    /// Per-stage results. `parse` is the *strict* parse; `recover` is
    /// the xref-rebuild attempt made only when `parse` failed (`skip`
    /// otherwise). Render/text/rewrite run on whichever document was
    /// obtained — `recovered` says which — but the verdict and the
    /// compatibility rates keep the strict result.
    parse: StageResult,
    #[serde(default)]
    recover: StageResult,
    #[serde(default)]
    recovered: bool,
    render: StageResult,
    /// Render on a recovery-mode document (`Document::from_bytes_recovering`):
    /// the `render` stage itself when the strict parse failed and the
    /// xref was rebuilt, or a second attempt made after the strict render
    /// failed with an error. Skipped when the strict render passed.
    #[serde(default = "StageResult::not_run")]
    render_recovered: StageResult,
    /// The counted render used a reported approximation (a substituted
    /// font, an approximated colour conversion).
    #[serde(default)]
    render_approximate: bool,
    /// Recovery-mode reinterpretations recorded while rendering
    /// (`Warning::RecoveryApplied`), plus the xref rebuild's notes.
    #[serde(default)]
    recovery_notes: usize,
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
    /// Producer family and `%PDF` version from the manifest ("" when
    /// unknown) — the grouping axes of the report.
    #[serde(default)]
    producer: String,
    #[serde(default)]
    pdf_version: String,
    /// Probe coverage — how much of the file the blocker set actually
    /// describes. All `default` for older reports.
    /// Total page count of the document (0 when parse failed).
    #[serde(default)]
    pub total_pages: usize,
    /// Pages the probe walked (first `--max-pages`).
    #[serde(default)]
    pub probe_pages: usize,
    /// Pages the probe could not walk (structure errors) — their
    /// blockers are unknown.
    #[serde(default)]
    pub probe_walk_errors: usize,
    /// Form XObjects seen; the probe does not look inside them, so
    /// each is an unprobed region.
    #[serde(default)]
    pub probe_forms: usize,
    /// Extra metrics collected regardless of verdict.
    metrics: Metrics,
    /// Document features relevant to round-trip coverage
    /// (`roundtrip::features`): multipage, acroform, annots, dests,
    /// signed, incremental, transparency, embedded-fonts, …
    #[serde(default)]
    features: Vec<String>,
    /// Rewrite → reparse → compare pipeline (`roundtrip::run`); `None`
    /// when the strict parse failed (and for reports saved before it).
    #[serde(default)]
    roundtrip: Option<RoundTrip>,
    /// Incremental save of a one-entry `/Info` edit on the strictly
    /// parsed document: original bytes kept as a prefix, the result
    /// reparses strictly with the edit visible and the same page count.
    #[serde(default = "StageResult::not_run")]
    incremental: StageResult,
    /// Measured content profile (`profile.rs`) for per-category tables.
    #[serde(default)]
    profile: profile::Profile,
    /// The policy that refused the default rewrite (`configs::refusal`);
    /// absent when it was not refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rewrite_refusal: Option<configs::Refusal>,
    /// Non-default rewrite configurations (`--rewrite-options`), each in
    /// its own namespace; the default stays in `rewrite` / `roundtrip`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rewrite_options: BTreeMap<String, configs::OptionRun>,
    /// `rewrite_with` with the default options against `rewrite` (bytes
    /// or error), and its report against the runner's inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_check: Option<PolicyCheck>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PolicyCheck {
    /// `rewrite_with(default)` gave `rewrite`'s bytes (or the same error).
    identical: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<policy::ReportSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inventory: Option<policy::InventoryCheck>,
    /// The report's version change agrees with the runner's own T1
    /// declaration (raised exactly when T1 is declared, to the same
    /// versions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_agrees: Option<bool>,
}

/// The document parsed again the way the run parsed it (strict, or
/// recovering): each rewrite under comparison starts from a fresh one,
/// so none inherits the reference budget another operation consumed.
fn reopen(data: &[u8], recovered: bool) -> Option<Document> {
    if recovered {
        Document::from_bytes_recovering(data).ok()
    } else {
        Document::from_bytes(data).ok()
    }
}

/// `rewrite_with(default)` against `rewrite`, each on a fresh parse of
/// `data`.
fn policy_check(data: &[u8], recovered: bool) -> Option<PolicyCheck> {
    let none = |identical| PolicyCheck {
        identical,
        report: None,
        inventory: None,
        version_agrees: None,
    };
    let (a, b) = (reopen(data, recovered)?, reopen(data, recovered)?);
    Some(
        match (
            kasane_pdf::rewrite(&a),
            kasane_pdf::rewrite_with(&b, &kasane_pdf::RewriteOptions::default()),
        ) {
            (Ok(bytes), Ok(out)) => {
                let input = reopen(data, recovered)?;
                let inventory = Document::from_bytes(out.bytes())
                    .ok()
                    .map(|o| policy::check(out.report(), &input, &o));
                PolicyCheck {
                    identical: out.bytes() == bytes.as_slice(),
                    report: Some(policy::summary(out.report())),
                    inventory,
                    version_agrees: Some(policy::version_agrees(out.report(), &input, data)),
                }
            }
            (Err(x), Err(y)) => none(x.to_string() == y.to_string() && x.kind() == y.kind()),
            _ => none(false),
        },
    )
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
    /// Content-aware quality of the worst page (see [`RenderQuality`]).
    #[serde(default)]
    render_quality: Option<RenderQuality>,
    /// Every compared page, in page order.
    #[serde(default)]
    render_pages: Vec<PageQuality>,
    /// Non-embedded fonts drawn with a bundled substitute on any
    /// rendered page (`base → substitute`), deduplicated. Non-empty
    /// means the render executed but glyph shapes are approximations.
    #[serde(default)]
    render_font_substitutions: Vec<String>,
    /// Colour spaces converted through an approximation on any
    /// rendered page (an `/ICCBased` profile the engine cannot
    /// evaluate, so its `/Alternate` was used), deduplicated.
    /// Non-empty means hues may differ from a CMS-backed viewer.
    #[serde(default)]
    render_color_approximations: Vec<String>,
    /// Images decoded below full resolution because they exceed the
    /// image limits (`RenderedPage::image_approximations`), deduplicated.
    #[serde(default)]
    render_image_approximations: Vec<String>,
    /// Non-fatal findings the engine recorded on the document while
    /// the stages ran (`Document::take_warnings`), deduplicated: the
    /// input deviated from the specification in a way the engine
    /// processed exactly. A stage that passed with a warning is
    /// *accepted with warnings*, counted separately from a clean pass.
    #[serde(default)]
    warnings: Vec<String>,
    /// Reading-order-sensitive agreement with `pdftotext` on page 1:
    /// longest common subsequence over the longer of the two strings,
    /// whitespace removed (no other normalisation).
    text_match_ratio: Option<f64>,
    /// Order-independent agreement on page 1: the two texts as
    /// character multisets after NFKC (ligatures expanded, compatibility
    /// and half/full-width forms unified, combining marks composed) with
    /// white space, controls and default-ignorable characters removed.
    /// Precision = shared / ours, recall = shared / pdftotext's.
    #[serde(default)]
    text_char_precision: Option<f64>,
    #[serde(default)]
    text_char_recall: Option<f64>,
    #[serde(default)]
    text_char_f1: Option<f64>,
    /// Content characters (after the normalisation above) on page 1:
    /// ours and pdftotext's. Both 0 is a page without text, where every
    /// score is trivially 1 — the analysis leaves those out of the
    /// text-agreement denominators.
    #[serde(default)]
    text_chars_ours: Option<usize>,
    #[serde(default)]
    text_chars_ref: Option<usize>,
    /// `text_char_f1` with the page's annotation and form-field
    /// appearance text added to ours (report only; `text_char_f1` and
    /// its gate are unchanged). `pdftotext` draws annotation appearances,
    /// so the appearance is the comparable source; see
    /// [`AnnotTextAgreement`].
    #[serde(default)]
    text_char_f1_with_annots: Option<f64>,
    #[serde(default)]
    text_with_annots: Option<AnnotTextAgreement>,
    rewrite_qpdf_ok: Option<bool>,
    rewrite_reparsed: Option<bool>,
}

/// Drain the document's warnings into the per-file list (deduplicated
/// by text, bounded so a pathological file cannot bloat the report).
/// Page-1 text agreement with annotations (report only). Ours is the
/// page text plus the text drawn by each visible annotation's normal
/// appearance — once per annotation, as `pdftotext` draws it: Popup,
/// Hidden and NoView annotations draw nothing, and each widget of a
/// field draws its own appearance. `/Contents` and field values are
/// counted by source but never added: a field's value is normally also
/// drawn by its appearance, and a note's `/Contents` is not drawn, so
/// adding either would credit text the reference does not contain or
/// count it twice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AnnotTextAgreement {
    precision: f64,
    recall: f64,
    /// Content characters of ours with the appearance text.
    ours_chars: usize,
    /// Annotations reported on page 1, and those shown.
    annotations: usize,
    visible: usize,
    /// Content characters by source (same normalisation as the F1).
    appearance_chars: usize,
    contents_chars: usize,
    field_value_chars: usize,
    /// The extraction with annotations failed (a malformed appearance
    /// stream); no F1 is reported. The default extraction is unaffected.
    #[serde(default)]
    error: Option<String>,
}

fn collect_warnings(doc: &Document, into: &mut Vec<String>) {
    for w in doc.take_warnings() {
        let s = w.to_string();
        if !into.contains(&s) && into.len() < 64 {
            into.push(s);
        }
    }
}

/// Content-aware fidelity metrics that complement the whole-page
/// MAE/SSIM/diff-pixel trio. The whole-page numbers stay as the
/// comparable baseline; these exist because a mostly-blank page scores
/// SSIM ≈ 1 even when every glyph is in the wrong place (the Type3
/// origin bug scored 0.997 on `simpletype3font`), and because a 1-px
/// anti-aliasing difference between rasterisers is not a defect.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RenderQuality {
    /// SSIM averaged only over 8×8 windows that contain ink (luma <
    /// 200) in *either* image — blank windows cannot inflate it.
    content_ssim: f64,
    /// Number of ink-bearing windows the content SSIM was averaged over.
    ink_windows: usize,
    /// Different-pixel ratio where a pixel only counts as different
    /// when no pixel in the other image's 3×3 neighbourhood is within
    /// the luma threshold — tolerant of ±1 px AA / hinting placement.
    diff_px_tol1: f64,
    /// Ink coverage (fraction of pixels with luma < 200), ours and the
    /// reference. A ratio far from 1 means content is missing or
    /// extra, whatever SSIM says.
    ink_ours: f64,
    ink_ref: f64,
    /// Integer (dx, dy) shift of our image, within ±3 px, that
    /// minimises the different-pixel ratio, and that ratio. A non-zero
    /// shift with a large drop points at a systematic glyph/position
    /// offset (metrics, lsb, matrix) rather than shape differences.
    best_shift: (i32, i32),
    diff_px_at_best_shift: f64,
    /// Reference dimensions differed from ours before resampling
    /// (page-size / DPI rounding) — the comparison is approximate.
    size_mismatch: bool,
    /// Pixel sizes `[ours_w, ours_h, ref_w, ref_h]` before resampling,
    /// so the quality gate can tell 1-px rounding from a different page
    /// size (e.g. `/UserUnit`, which Poppler ignores). Empty for older
    /// reports.
    #[serde(default)]
    sizes: Vec<usize>,
    /// Content SSIM with our image moved by `best_shift` first, so a
    /// whole-page ±1–3 px placement difference (rounding, image origin)
    /// does not count as structure lost. `None` in older reports.
    #[serde(default)]
    content_ssim_at_shift: Option<f64>,
    /// Local placement check, after `best_shift`, in tiles of
    /// `TILE_PX` pixels: the largest share, over tiles holding at least
    /// `TILE_MIN_INK` of our ink pixels, of our ink (luma < 200) with no
    /// reference mark (luma < 245) within `NEAR_PX` pixels — content we
    /// drew where the reference has nothing (misplaced, rotated or extra
    /// content). Faint marks count on the other side so a lighter
    /// anti-aliased stroke is not "nothing". `None` in older reports.
    #[serde(default)]
    displaced_ink_tile: Option<f64>,
    /// Our ink pixels with no reference mark within `NEAR_PX`, whole page.
    #[serde(default)]
    displaced_ink_px: Option<usize>,
    /// The reverse of `displaced_ink_tile`: reference ink with nothing
    /// of ours near it (content we did not draw, or drew elsewhere).
    #[serde(default)]
    missing_ink_tile: Option<f64>,
    #[serde(default)]
    missing_ink_px: Option<usize>,
    /// Colour agreement (`colour.rs`): CIEDE2000 over the pixels both
    /// renders draw in a flat colour, after `best_shift`, against an RGB
    /// Poppler render. `None`: fewer than `colour::MIN_COMPARED`
    /// comparable pixels, no RGB reference, or an older report.
    #[serde(default)]
    colour: Option<colour::ColourStats>,
    /// Wall time of the colour comparison, the RGB reference render
    /// included (ms).
    #[serde(default)]
    colour_ms: Option<u64>,
}

/// One compared page — whole-page metrics plus the content-aware set.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageQuality {
    page: usize,
    mae: f64,
    ssim: f64,
    diff_px_ratio: f64,
    quality: RenderQuality,
}

// ------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------

fn sha256_file(p: &Path) -> String {
    let d = fs::read(p).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&d);
    // Formatted byte by byte so the code is independent of the digest
    // type's `LowerHex` support, which changed between sha2 0.10 and 0.11.
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
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
        SignaturePolicy { .. } => "signed document".into(),
        Encrypted => "encrypted".into(),
        LimitExceeded { .. } => "limit".into(),
        Io(_) => "io".into(),
        _ => String::new(),
    };
    // A signature-policy refusal is a documented profile limit, like
    // an unsupported feature — never a failure of the engine.
    let class = match e {
        Unsupported { .. } | SignaturePolicy { .. } | Encrypted => Class::Unsupported,
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
    quality: RenderQuality,
    diff_path: Option<String>,
    ours_path: Option<String>,
    ref_path: Option<String>,
}

/// Luma below which a pixel counts as "ink" for coverage/windows.
const INK_LUMA: u8 = 200;
/// Luma delta above which two pixels count as different.
const DIFF_LUMA: i32 = 8;

/// Fraction of pixels darker than `INK_LUMA`.
fn ink_coverage(a: &[u8]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    a.iter().filter(|&&v| v < INK_LUMA).count() as f64 / a.len() as f64
}

/// SSIM averaged over the 8×8 windows (stride 4) that contain ink in
/// either image. Returns (ssim, windows); ssim = 1.0 when nothing is
/// inked anywhere (two blank pages agree).
fn content_ssim_gray(a: &[u8], b: &[u8], w: usize, h: usize) -> (f64, usize) {
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let mut sum = 0.0;
    let mut n = 0usize;
    let mut y = 0usize;
    while y + 8 <= h {
        let mut x = 0usize;
        while x + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            let mut ink = false;
            for yy in y..y + 8 {
                let row = yy * w;
                for xx in x..x + 8 {
                    let pa = a[row + xx];
                    let pb = b[row + xx];
                    ink |= pa < INK_LUMA || pb < INK_LUMA;
                    let (pa, pb) = (pa as f64, pb as f64);
                    sa += pa;
                    sb += pb;
                    saa += pa * pa;
                    sbb += pb * pb;
                    sab += pa * pb;
                }
            }
            if ink {
                let ua = sa / 64.0;
                let ub = sb / 64.0;
                let va = saa / 64.0 - ua * ua;
                let vb = sbb / 64.0 - ub * ub;
                let cov = sab / 64.0 - ua * ub;
                let num = (2.0 * ua * ub + C1) * (2.0 * cov + C2);
                let den = (ua * ua + ub * ub + C1) * (va + vb + C2);
                sum += num / den;
                n += 1;
            }
            x += 4;
        }
        y += 4;
    }
    if n == 0 {
        (1.0, 0)
    } else {
        (sum / n as f64, n)
    }
}

/// Different-pixel ratio with a 3×3 neighbourhood tolerance, symmetric
/// (ours→ref and ref→ours averaged): a pixel is different only when no
/// neighbour in the other image is within `DIFF_LUMA`.
fn diff_px_tolerant(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    fn one_way(a: &[u8], b: &[u8], w: usize, h: usize) -> usize {
        let mut c = 0usize;
        for y in 0..h {
            for x in 0..w {
                let v = a[y * w + x] as i32;
                if (v - b[y * w + x] as i32).abs() <= DIFF_LUMA {
                    continue;
                }
                let mut matched = false;
                'nb: for dy in -1i32..=1 {
                    let yy = y as i32 + dy;
                    if yy < 0 || yy >= h as i32 {
                        continue;
                    }
                    for dx in -1i32..=1 {
                        let xx = x as i32 + dx;
                        if xx < 0 || xx >= w as i32 {
                            continue;
                        }
                        if (v - b[yy as usize * w + xx as usize] as i32).abs() <= DIFF_LUMA {
                            matched = true;
                            break 'nb;
                        }
                    }
                }
                if !matched {
                    c += 1;
                }
            }
        }
        c
    }
    let n = (w * h).max(1) as f64;
    (one_way(a, b, w, h) + one_way(b, a, w, h)) as f64 / (2.0 * n)
}

/// Different-pixel ratio of `a` shifted by (dx, dy) against `b`, over
/// the overlapping region.
fn diff_px_shifted(a: &[u8], b: &[u8], w: usize, h: usize, dx: i32, dy: i32) -> f64 {
    let mut diffs = 0usize;
    let mut n = 0usize;
    let y0 = (-dy).max(0) as usize;
    let y1 = (h as i32 - dy.max(0)) as usize;
    let x0 = (-dx).max(0) as usize;
    let x1 = (w as i32 - dx.max(0)) as usize;
    for y in y0..y1 {
        let ra = y * w;
        let rb = (y as i32 + dy) as usize * w;
        for x in x0..x1 {
            let va = a[ra + x] as i32;
            let vb = b[rb + (x as i32 + dx) as usize] as i32;
            if (va - vb).abs() > DIFF_LUMA {
                diffs += 1;
            }
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        diffs as f64 / n as f64
    }
}

/// `a` moved by (dx, dy) — pixel (x, y) to (x + dx, y + dy), the pairing
/// `diff_px_shifted` uses — with white where nothing moved in.
fn shifted(a: &[u8], w: usize, h: usize, dx: i32, dy: i32) -> Vec<u8> {
    let mut out = vec![255u8; w * h];
    for y in 0..h as i32 {
        let ty = y + dy;
        if ty < 0 || ty >= h as i32 {
            continue;
        }
        for x in 0..w as i32 {
            let tx = x + dx;
            if tx >= 0 && tx < w as i32 {
                out[ty as usize * w + tx as usize] = a[y as usize * w + x as usize];
            }
        }
    }
    out
}

/// Tile edge for the local placement check (24 pt at 96 dpi).
const TILE_PX: usize = 32;
/// Tiles with fewer tested ink pixels than this are not judged.
const TILE_MIN_INK: usize = 20;
/// Distance within which a mark in the other image counts as "near".
const NEAR_PX: usize = 2;
/// Luma below which a pixel is a mark (faint anti-aliasing included).
const MARK_LUMA: u8 = 245;

/// `(worst tile share, pixels)` of `a`'s ink with no mark of `b` within
/// `NEAR_PX` (see `RenderQuality::displaced_ink_tile`).
fn unmatched_ink(a: &[u8], b: &[u8], w: usize, h: usize) -> (f64, usize) {
    // Distance-limited dilation of b's marks: a row pass then a column
    // pass of a (2·NEAR_PX + 1) box.
    let mark: Vec<bool> = b.iter().map(|&v| v < MARK_LUMA).collect();
    let mut rows = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if mark[y * w + x] {
                for xx in x.saturating_sub(NEAR_PX)..(x + NEAR_PX + 1).min(w) {
                    rows[y * w + xx] = true;
                }
            }
        }
    }
    let mut near = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if rows[y * w + x] {
                for yy in y.saturating_sub(NEAR_PX)..(y + NEAR_PX + 1).min(h) {
                    near[yy * w + x] = true;
                }
            }
        }
    }
    let (tw, th) = (w.div_ceil(TILE_PX), h.div_ceil(TILE_PX));
    let mut ink = vec![0usize; tw * th];
    let mut lone = vec![0usize; tw * th];
    let mut total = 0usize;
    for y in 0..h {
        for x in 0..w {
            if a[y * w + x] < INK_LUMA {
                let t = (y / TILE_PX) * tw + x / TILE_PX;
                ink[t] += 1;
                if !near[y * w + x] {
                    lone[t] += 1;
                    total += 1;
                }
            }
        }
    }
    let worst = ink
        .iter()
        .zip(&lone)
        .filter(|(i, _)| **i >= TILE_MIN_INK)
        .map(|(i, l)| *l as f64 / *i as f64)
        .fold(0.0, f64::max);
    (worst, total)
}

/// The integer shift within ±`max` that minimises the different-pixel
/// ratio, with that ratio. Ties prefer the smaller shift.
fn best_shift(a: &[u8], b: &[u8], w: usize, h: usize, max: i32) -> ((i32, i32), f64) {
    let mut best: ((i32, i32), f64) = ((0, 0), diff_px_shifted(a, b, w, h, 0, 0));
    for dy in -max..=max {
        for dx in -max..=max {
            if (dx, dy) == (0, 0) {
                continue;
            }
            let r = diff_px_shifted(a, b, w, h, dx, dy);
            let cur_mag = best.0 .0.abs() + best.0 .1.abs();
            let mag = dx.abs() + dy.abs();
            if r < best.1 - 1e-9 || (r <= best.1 + 1e-9 && mag < cur_mag) {
                best = ((dx, dy), r);
            }
        }
    }
    best
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
    let out = roundtrip::run_bounded(
        Command::new("pdftoppm")
            .args(["-f", &page.to_string(), "-l", &page.to_string()])
            // `-cropbox`: our page size is the /CropBox (the visible region),
            // so the reference must be too or the pixmaps cannot be aligned.
            .args(["-r", &dpi.to_string(), "-gray", "-singlefile", "-cropbox"])
            .arg(pdf)
            .arg(&prefix),
    )?;
    if !out.status.success() {
        return None;
    }
    let ref_pgm = prefix.with_extension("pgm");
    let ref_bytes = fs::read(&ref_pgm).ok()?;
    let (rw, rh, ref_pix) = parse_pgm(&ref_bytes)?;

    // kasane pixmap → luminance buffer at its own resolution.
    let kw = kasane.width() as usize;
    let kh = kasane.height() as usize;
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

    // Content-aware metrics (see `RenderQuality`).
    let (content_ssim, ink_windows) = content_ssim_gray(&kpix, &ref_sized, kw, kh);
    let diff_px_tol1 = diff_px_tolerant(&kpix, &ref_sized, kw, kh);
    let (shift, diff_px_at_best_shift) = best_shift(&kpix, &ref_sized, kw, kh, 3);
    let moved = shifted(&kpix, kw, kh, shift.0, shift.1);
    let (content_ssim_at_shift, _) = content_ssim_gray(&moved, &ref_sized, kw, kh);
    let (displaced_ink_tile, displaced_ink_px) = unmatched_ink(&moved, &ref_sized, kw, kh);
    let (missing_ink_tile, missing_ink_px) = unmatched_ink(&ref_sized, &moved, kw, kh);
    drop(moved);
    let t_colour = Instant::now();
    let colour = colour_against_poppler(kasane, pdf, page, dpi, &prefix, (rw, rh), shift);
    let colour_ms = t_colour.elapsed().as_millis() as u64;
    let quality = RenderQuality {
        content_ssim,
        ink_windows,
        diff_px_tol1,
        ink_ours: ink_coverage(&kpix),
        ink_ref: ink_coverage(&ref_sized),
        best_shift: shift,
        diff_px_at_best_shift,
        size_mismatch: rw != kw || rh != kh,
        sizes: vec![kw, kh, rw, rh],
        content_ssim_at_shift: Some(content_ssim_at_shift),
        displaced_ink_tile: Some(displaced_ink_tile),
        displaced_ink_px: Some(displaced_ink_px),
        missing_ink_tile: Some(missing_ink_tile),
        missing_ink_px: Some(missing_ink_px),
        colour,
        colour_ms: Some(colour_ms),
    };

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
        quality,
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
    let d = p.pixmap().data();
    let mut out = Vec::with_capacity((p.width() * p.height()) as usize);
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for px in d.chunks_exact(4) {
        // Premultiplied, composited on white like `pixmap_rgb` (pages
        // are opaque; this only matters if one ever is not).
        let [r, g, b] = on_white([px[0], px[1], px[2], px[3]]).map(u32::from);
        out.push(((r * 299 + g * 587 + b * 114) / 1000) as u8);
    }
    out
}

/// Our page as opaque RGB: premultiplied RGBA composited on white.
fn pixmap_rgb(p: &kasane_pdf::RenderedPage) -> Vec<u8> {
    let d = p.pixmap().data();
    let mut out = Vec::with_capacity((p.width() * p.height()) as usize * 3);
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for px in d.chunks_exact(4) {
        out.extend_from_slice(&on_white([px[0], px[1], px[2], px[3]]));
    }
    out
}

/// A premultiplied RGBA pixel composited on white: c + (255 − α).
fn on_white(px: [u8; 4]) -> [u8; 3] {
    let white = 255 - px[3];
    [
        px[0].saturating_add(white),
        px[1].saturating_add(white),
        px[2].saturating_add(white),
    ]
}

/// The colour statistics of one page: an RGB Poppler render of the same
/// page, size and crop as the grey one (`gray_size`), resampled like it
/// when the sizes differ, compared with our render moved by `shift`.
/// The RGB reference is read and deleted; it is never kept.
fn colour_against_poppler(
    kasane: &kasane_pdf::RenderedPage,
    pdf: &Path,
    page: usize,
    dpi: u32,
    prefix: &Path,
    gray_size: (usize, usize),
    shift: (i32, i32),
) -> Option<colour::ColourStats> {
    let rgb_prefix =
        prefix.with_file_name(format!("{}-rgb", prefix.file_name()?.to_string_lossy()));
    let out = roundtrip::run_bounded(
        Command::new("pdftoppm")
            .args(["-f", &page.to_string(), "-l", &page.to_string()])
            .args(["-r", &dpi.to_string(), "-singlefile", "-cropbox"])
            .arg(pdf)
            .arg(&rgb_prefix),
    )?;
    let ppm = rgb_prefix.with_extension("ppm");
    let bytes = fs::read(&ppm).ok();
    let _ = fs::remove_file(&ppm);
    if !out.status.success() {
        return None;
    }
    let (rw, rh, rgb) = parse_ppm(&bytes?)?;
    // The RGB render must describe the same raster as the grey one.
    if (rw, rh) != gray_size {
        return None;
    }
    let kw = kasane.width() as usize;
    let kh = kasane.height() as usize;
    let reference = if rw == kw && rh == kh {
        rgb
    } else {
        resample_rgb(&rgb, rw, rh, kw, kh)
    };
    let ours = shifted_rgb(kasane, shift.0, shift.1);
    colour::compare(&ours, &reference, kw, kh)
}

/// Our page as opaque RGB (`pixmap_rgb`) moved by (dx, dy) like
/// `shifted`, built straight from the pixmap (no intermediate copy).
fn shifted_rgb(p: &kasane_pdf::RenderedPage, dx: i32, dy: i32) -> Vec<u8> {
    let (w, h) = (p.width() as usize, p.height() as usize);
    let d = p.pixmap().data();
    let mut out = vec![255u8; w * h * 3];
    for y in 0..h as i32 {
        let ty = y + dy;
        if ty < 0 || ty >= h as i32 {
            continue;
        }
        for x in 0..w as i32 {
            let tx = x + dx;
            if tx >= 0 && tx < w as i32 {
                let s = (y as usize * w + x as usize) * 4;
                let t = (ty as usize * w + tx as usize) * 3;
                out[t..t + 3].copy_from_slice(&on_white([d[s], d[s + 1], d[s + 2], d[s + 3]]));
            }
        }
    }
    out
}

fn resample_rgb(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 3];
    for y in 0..dh {
        for x in 0..dw {
            let s = (y * sh / dh * sw + x * sw / dw) * 3;
            out[(y * dw + x) * 3..(y * dw + x) * 3 + 3].copy_from_slice(&src[s..s + 3]);
        }
    }
    out
}

/// Parse a binary PPM (P6, maxval 255) — returns (w, h, rgb).
fn parse_ppm(b: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut i = 0;
    if next_tok(b, &mut i)? != b"P6" {
        return None;
    }
    let w = atoi(next_tok(b, &mut i)?);
    let h = atoi(next_tok(b, &mut i)?);
    if atoi(next_tok(b, &mut i)?) != 255 {
        return None;
    }
    i += 1;
    let n = w.checked_mul(h)?.checked_mul(3)?;
    if b.len() < i.checked_add(n)? {
        return None;
    }
    Some((w, h, b[i..i + n].to_vec()))
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
/// Page-text agreement with pdftotext (see the `Metrics` fields).
struct TextAgreement {
    order: f64,
    precision: f64,
    recall: f64,
    f1: f64,
    ours_chars: usize,
    ref_chars: usize,
}

/// NFKC, then drop white space, controls and default-ignorables.
fn content_chars(s: &str) -> Vec<char> {
    use unicode_normalization::UnicodeNormalization;
    s.nfkc()
        .filter(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(*c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
        })
        .collect()
}

/// Multiset precision / recall / F1 of `ours` against `reference`.
fn multiset(ours: &[char], reference: &[char]) -> (f64, f64, f64) {
    if ours.is_empty() && reference.is_empty() {
        return (1.0, 1.0, 1.0);
    }
    let mut count = std::collections::HashMap::<char, i64>::new();
    for c in reference {
        *count.entry(*c).or_default() += 1;
    }
    let mut shared = 0usize;
    for c in ours {
        if let Some(n) = count.get_mut(c) {
            if *n > 0 {
                *n -= 1;
                shared += 1;
            }
        }
    }
    let p = if ours.is_empty() {
        0.0
    } else {
        shared as f64 / ours.len() as f64
    };
    let r = if reference.is_empty() {
        0.0
    } else {
        shared as f64 / reference.len() as f64
    };
    let f = if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    };
    (p, r, f)
}

fn pdftotext_page(pdf: &Path, page: usize) -> Option<String> {
    let out = Command::new("pdftotext")
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .arg(pdf)
        .arg("-")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn compare_text(ours: &str, theirs: &str) -> TextAgreement {
    let a = normalize(ours);
    let b = normalize(theirs);
    let (oc, rc) = (content_chars(ours), content_chars(theirs));
    let (precision, recall, f1) = multiset(&oc, &rc);
    TextAgreement {
        order: text_match(&a, &b),
        precision,
        recall,
        f1,
        ours_chars: oc.len(),
        ref_chars: rc.len(),
    }
}

/// Page 1 with its annotations against `pdftotext` (see
/// [`AnnotTextAgreement`]). The extraction's warnings are discarded:
/// the text stage's warnings stay those of the default extraction.
fn compare_text_with_annots(
    doc: &Document,
    page: &kasane_pdf::Page,
    page_text: &str,
    theirs: &str,
) -> (Option<f64>, AnnotTextAgreement) {
    let x = extract_text(doc, page, &TextOptions::default().with_annotations(true));
    let _ = doc.take_warnings();
    let x = match x {
        Ok(x) => x,
        Err(e) => {
            let a = AnnotTextAgreement {
                error: Some(e.to_string()),
                ..Default::default()
            };
            return (None, a);
        }
    };
    let mut ours = page_text.to_string();
    let mut a = AnnotTextAgreement {
        annotations: x.annotations.len(),
        ..Default::default()
    };
    for an in &x.annotations {
        let drawn: String = an
            .appearance
            .iter()
            .filter_map(|c| c.unicode.as_deref())
            .collect();
        if !an.hidden {
            a.visible += 1;
            ours.push_str(&drawn);
            a.appearance_chars += content_chars(&drawn).len();
        }
        a.contents_chars += an.contents.as_deref().map_or(0, |s| content_chars(s).len());
        if let Some(f) = &an.field {
            a.field_value_chars += f
                .value
                .iter()
                .map(|v| content_chars(v).len())
                .sum::<usize>();
        }
    }
    let oc = content_chars(&ours);
    let (p, r, f1) = multiset(&oc, &content_chars(theirs));
    a.precision = p;
    a.recall = r;
    a.ours_chars = oc.len();
    (Some(f1), a)
}

/// Save a one-entry `/Info` edit incrementally and check the result
/// (see `FileResult::incremental`). Refusals by policy (signatures,
/// encryption, recovered documents) are `Unsupported`.
fn incremental_probe(doc: &Document, data: &[u8]) -> StageResult {
    let t = Instant::now();
    let fail = |c: Class, d: String| {
        StageResult::caused(
            c,
            d,
            String::new(),
            "incremental".into(),
            t.elapsed().as_millis(),
        )
    };
    let mut ed = match doc.edit() {
        Ok(e) => e,
        Err(e) => {
            let (c, d, f, cid) = classify_err("incremental", &e);
            return StageResult::caused(c, d, f, cid, t.elapsed().as_millis());
        }
    };
    let mut info = kasane_pdf::Dict::new();
    info.insert(
        b"Title".to_vec(),
        kasane_pdf::Object::String(b"kasane-compat incremental probe".to_vec()),
    );
    let out = match ed.set_info(info).and_then(|_| ed.save_incremental()) {
        Ok(o) => o,
        Err(e) => {
            let (c, d, f, cid) = classify_err("incremental", &e);
            return StageResult::caused(c, d, f, cid, t.elapsed().as_millis());
        }
    };
    if out.len() < data.len() || out[..data.len()] != *data {
        return fail(
            Class::Error,
            "original bytes not preserved as a prefix".into(),
        );
    }
    let re = match Document::from_bytes(&out) {
        Ok(d) => d,
        Err(e) => return fail(Class::Error, format!("reparse: {e}")),
    };
    if re.page_count() != doc.page_count() {
        return fail(Class::Error, "page count changed".into());
    }
    let title = re
        .trailer()
        .get(b"Info".as_slice())
        .and_then(|i| re.deref(i).ok())
        .and_then(|i| match i.as_dict()?.get(b"Title".as_slice())? {
            kasane_pdf::Object::String(s) => Some(s.clone()),
            _ => None,
        });
    if title.as_deref() != Some(b"kasane-compat incremental probe".as_slice()) {
        return fail(Class::Error, "edit not visible after reparse".into());
    }
    StageResult::pass(t.elapsed().as_millis())
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
        render_quality: None,
        render_pages: Vec::new(),
        render_font_substitutions: Vec::new(),
        render_color_approximations: Vec::new(),
        render_image_approximations: Vec::new(),
        warnings: Vec::new(),
        text_match_ratio: None,
        text_char_precision: None,
        text_char_recall: None,
        text_char_f1: None,
        text_chars_ours: None,
        text_chars_ref: None,
        text_char_f1_with_annots: None,
        text_with_annots: None,
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
    let (doc, parse, recover, recovered) = match Document::from_bytes(&data) {
        Ok(d) => (
            d,
            StageResult::pass(t.elapsed().as_millis()),
            StageResult::skip("strict parse passed"),
            false,
        ),
        Err(e) => {
            let (c, d, f, cid) = classify_err("parse", &e);
            let mut parse = StageResult::new(c, d, f, t.elapsed().as_millis());
            parse.cause = cid;
            // Strict failed: try the xref rebuild. Its outcome is a
            // separate stage so a recovered file never counts as a
            // strict parse pass.
            let t2 = Instant::now();
            match Document::from_bytes_recovering(&data) {
                Ok(d) => {
                    let mut rec = StageResult::pass(t2.elapsed().as_millis());
                    if let Some(rep) = d.recovery() {
                        rec.detail = format!(
                            "{} object(s) scanned, {} from object streams, /Root via {}; {} warning(s){}",
                            rep.objects_found,
                            rep.objects_from_streams,
                            rep.trailer_source,
                            rep.warnings.len(),
                            if rep.warnings.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", rep.warnings.join(" | "))
                            }
                        );
                    }
                    (d, parse, rec, true)
                }
                Err(e2) => {
                    let (c2, d2, f2, cid2) = classify_err("recover", &e2);
                    let mut rec = StageResult::new(c2, d2, f2, t2.elapsed().as_millis());
                    rec.cause = cid2;
                    return finish(FileResult {
                        id: id.into(),
                        sha256: sha,
                        bytes,
                        category: category.into(),
                        verdict: c,
                        pages_seen: 0,
                        parse,
                        recover: rec,
                        recovered: false,
                        render: StageResult::skip("parse failed"),
                        render_recovered: StageResult::skip("recovery parse failed"),
                        render_approximate: false,
                        recovery_notes: 0,
                        text: StageResult::skip("parse failed"),
                        rewrite: StageResult::skip("parse failed"),
                        causes: Vec::new(),
                        cause_pages: BTreeMap::new(),
                        expect: String::new(),
                        producer: String::new(),
                        pdf_version: String::new(),
                        total_pages: 0,
                        probe_pages: 0,
                        probe_walk_errors: 0,
                        probe_forms: 0,
                        metrics: {
                            metrics.total_ms = start.elapsed().as_millis();
                            metrics.max_rss_bytes = max_rss_bytes();
                            metrics
                        },
                        features: Vec::new(),
                        roundtrip: None,
                        incremental: StageResult::not_run(),
                        profile: profile::Profile::default(),
                        rewrite_refusal: None,
                        rewrite_options: BTreeMap::new(),
                        policy_check: None,
                    });
                }
            }
        }
    };
    // Notes the xref rebuild made (counted with the render-time rules).
    let recover_notes = doc.recovery().map(|r| r.warnings.len()).unwrap_or(0);
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
        let opts = RenderOptions::default()
            .with_dpi(args.dpi)
            .with_limits(Limits::default());
        match render_page(&doc, page, &opts) {
            Ok(r) => {
                rendered += 1;
                for s in r.font_substitutions() {
                    let tag = format!("{} → {}", s.base_font, s.substitute);
                    if !metrics.render_font_substitutions.contains(&tag) {
                        metrics.render_font_substitutions.push(tag);
                    }
                }
                for a in r.color_approximations() {
                    if !metrics.render_color_approximations.contains(a) {
                        metrics.render_color_approximations.push(a.clone());
                    }
                }
                for a in r.image_approximations() {
                    if !metrics.render_image_approximations.contains(a) {
                        metrics.render_image_approximations.push(a.clone());
                    }
                }
                collect_warnings(&doc, &mut metrics.warnings);
                // Visual fidelity vs poppler on every rendered page.
                if args.with_poppler {
                    let diff_dir = args.out.join("diff");
                    fs::create_dir_all(&diff_dir).ok();
                    let tag = format!("{id}-p{}", i + 1);
                    if let Some(f) =
                        compare_render(&r, path, i + 1, args.dpi as u32, &diff_dir, &tag)
                    {
                        metrics.render_pages.push(PageQuality {
                            page: i + 1,
                            mae: f.mae,
                            ssim: f.ssim,
                            diff_px_ratio: f.diff_px_ratio,
                            quality: f.quality.clone(),
                        });
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
    let render_approximate = !metrics.render_font_substitutions.is_empty()
        || !metrics.render_color_approximations.is_empty()
        || !metrics.render_image_approximations.is_empty();
    // Recovered render: the render above already ran on a recovery-mode
    // document when the strict parse failed; otherwise, a strict render
    // *error* (not an unsupported feature, not a limit) is retried on a
    // recovery-mode document so the recovery rules can be measured —
    // counted apart from the strict render, never in its place.
    let render_recovered = if recovered {
        render.clone()
    } else if render.class == Class::Error {
        let t3 = Instant::now();
        match Document::from_bytes_recovering(&data) {
            Ok(rdoc) => {
                let mut res = StageResult::pass(0);
                for (i, page) in rdoc.pages().iter().take(args.max_pages).enumerate() {
                    let opts = RenderOptions::default()
                        .with_dpi(args.dpi)
                        .with_limits(Limits::default());
                    match render_page(&rdoc, page, &opts) {
                        Ok(_) => {}
                        Err(e) => {
                            let (c, d, f, cid) = classify_err("render", &e);
                            res = StageResult::caused(
                                c,
                                format!("page {}: {d}", i + 1),
                                f,
                                cid,
                                t3.elapsed().as_millis(),
                            );
                            break;
                        }
                    }
                }
                collect_warnings(&rdoc, &mut metrics.warnings);
                if res.class == Class::Pass {
                    res.ms = t3.elapsed().as_millis();
                }
                res
            }
            Err(e) => {
                let (c, d, f, cid) = classify_err("recover", &e);
                StageResult::caused(c, d, f, cid, t3.elapsed().as_millis())
            }
        }
    } else {
        StageResult::skip("strict render passed or was unsupported")
    };
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
        metrics.render_quality = Some(f.quality);
        let _ = best_pgno;
    }

    // ---- Text extract (first N pages) ------------------------------
    let t = Instant::now();
    let mut text = StageResult::pass(0);
    let mut our_text = String::new();
    let mut page1_text = String::new();
    let mut extracted = 0usize;
    for (i, page) in pages.iter().take(args.max_pages).enumerate() {
        match extract_text(&doc, page, &TextOptions::default()) {
            Ok(t2) => {
                collect_warnings(&doc, &mut metrics.warnings);
                for ch in &t2.chars {
                    if let Some(u) = &ch.unicode {
                        our_text.push_str(u);
                        if i == 0 {
                            page1_text.push_str(u);
                        }
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
        // Page 1 against page 1: the whole document's text against
        // pdftotext's first page (as before) understated multi-page
        // files.
        if args.with_poppler {
            if let Some(theirs) = pdftotext_page(path, 1) {
                let t = compare_text(&page1_text, &theirs);
                metrics.text_match_ratio = Some(t.order);
                metrics.text_char_precision = Some(t.precision);
                metrics.text_char_recall = Some(t.recall);
                metrics.text_char_f1 = Some(t.f1);
                metrics.text_chars_ours = Some(t.ours_chars);
                metrics.text_chars_ref = Some(t.ref_chars);
                let (f1, a) = compare_text_with_annots(&doc, &pages[0], &page1_text, &theirs);
                metrics.text_char_f1_with_annots = f1;
                metrics.text_with_annots = Some(a);
            }
        }
    }

    // ---- Rewrite → reparse → round-trip comparison ----------------
    let features = roundtrip::features(&doc, &data);
    let t = Instant::now();
    let mut rewrite_res = StageResult::pass(0);
    let mut rewrite_refusal = None;
    let check;
    let rt = match rewrite(&doc) {
        Ok(out) => {
            check = policy_check(&data, recovered);
            let rewrite_ms = t.elapsed().as_millis();
            let rt_opts = rt_options(args);
            let decls = configs::RewriteConfig::Default.declarations(&doc, &data);
            let rt = roundtrip::run(&doc, &data, &out, rewrite_ms, &rt_opts, id, &decls);
            metrics.rewrite_qpdf_ok = rt
                .qpdf
                .as_ref()
                .map(|q| q.exit_code == 0 && q.warnings == 0 && q.errors == 0);
            metrics.rewrite_reparsed = Some(rt.reparse.class == RtClass::Pass);
            if rt.reparse.class != RtClass::Pass {
                rewrite_res = StageResult::caused(
                    Class::Error,
                    format!("reparse: {}", rt.reparse.notes.join("; ")),
                    String::new(),
                    "rewrite.reparse".into(),
                    t.elapsed().as_millis(),
                );
            }
            rt
        }
        Err(e) => {
            let (c, d, f, cid) = classify_err("rewrite", &e);
            rewrite_res = StageResult::caused(c, d, f, cid, t.elapsed().as_millis());
            rewrite_refusal = configs::refusal(&e);
            check = policy_check(&data, recovered);
            roundtrip::refused(&e, t.elapsed().as_millis())
        }
    };
    let rewrite_options = run_rewrite_options(&data, recovered, id, args);
    if rewrite_res.class == Class::Pass {
        rewrite_res.ms = rt.rewrite.ms;
    }
    // Like render / text / rewrite, the probe counts only on a strictly
    // parsed document (the writer refuses recovered ones that needed a
    // rebuilt table; one that only needed a recovery rule would save
    // the invalid input unchanged).
    let incremental = if recovered {
        StageResult::skip("strict parse failed; recovered document")
    } else {
        incremental_probe(&doc, &data)
    };
    let mut prof = profile::structural(&doc, args.max_pages);
    prof.text_layer_page1 = (extracted > 0).then_some(!page1_text.trim().is_empty());
    prof.scripts = profile::scripts(&our_text);

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
        recover,
        recovered,
        render,
        render_recovered,
        render_approximate,
        recovery_notes: metrics
            .warnings
            .iter()
            .filter(|w| w.starts_with("recovery:"))
            .count()
            + recover_notes,
        text,
        rewrite: rewrite_res,
        causes,
        cause_pages,
        expect: String::new(),
        producer: String::new(),
        pdf_version: String::new(),
        total_pages: pages.len(),
        probe_pages: probe.pages,
        probe_walk_errors: probe.walk_errors,
        probe_forms: probe.forms,
        metrics,
        features,
        roundtrip: Some(rt),
        incremental,
        profile: prof,
        rewrite_refusal,
        rewrite_options,
        policy_check: check,
    }
}

fn rt_options(args: &Args) -> RtOptions {
    RtOptions {
        max_pages: (args.rt_pages != usize::MAX).then_some(args.rt_pages),
        dpi: args.rt_dpi,
        poppler: args.with_poppler,
        poppler_pages: (args.poppler_pages != usize::MAX).then_some(args.poppler_pages),
        budget_ms: args.rt_budget_secs as u128 * 1000,
    }
}

/// The non-default configurations of `--rewrite-options` on a strictly
/// parsed document, each recorded under its own name. `main` refuses a
/// configuration the library cannot run before any file is processed.
fn run_rewrite_options(
    data: &[u8],
    recovered: bool,
    id: &str,
    args: &Args,
) -> BTreeMap<String, configs::OptionRun> {
    let mut out = BTreeMap::new();
    for c in &args.rewrite_options {
        if *c == configs::RewriteConfig::Default {
            continue;
        }
        // A fresh parse per configuration (and for its checks).
        let (Some(fresh), Some(input)) = (reopen(data, recovered), reopen(data, recovered)) else {
            continue;
        };
        let doc = &fresh;
        let t = Instant::now();
        let run = match c.rewrite(doc) {
            Err(why) => configs::OptionRun {
                rewrite: "error".into(),
                refusal: None,
                declared: Vec::new(),
                unavailable: Some(why.to_string()),
                roundtrip: None,
                report: None,
                inventory: None,
            },
            Ok(Ok((bytes, report))) => {
                let decls = c.declarations(doc, data);
                let tag = format!("{id}-{}", c.name().replace(',', "+"));
                let rt = roundtrip::run(
                    doc,
                    data,
                    &bytes,
                    t.elapsed().as_millis(),
                    &rt_options(args),
                    &tag,
                    &decls,
                );
                let inventory = report.as_ref().and_then(|r| {
                    Document::from_bytes(&bytes)
                        .ok()
                        .map(|o| policy::check(r, &input, &o))
                });
                configs::OptionRun {
                    rewrite: "pass".into(),
                    refusal: None,
                    declared: decls.iter().map(|d| d.rule_name().to_string()).collect(),
                    unavailable: None,
                    roundtrip: Some(rt),
                    report: report.as_ref().map(policy::summary),
                    inventory,
                }
            }
            Ok(Err(e)) => {
                let (class, ..) = classify_err("rewrite", &e);
                configs::OptionRun {
                    rewrite: class.label().to_lowercase(),
                    refusal: configs::refusal(&e),
                    declared: Vec::new(),
                    unavailable: None,
                    roundtrip: Some(roundtrip::refused(&e, t.elapsed().as_millis())),
                    report: None,
                    inventory: None,
                }
            }
        };
        out.insert(c.name().to_string(), run);
    }
    out
}

/// `kasane-compat inventory FILE`: the action inventory of FILE, one JSON
/// object per line (class, kind, owner, position, shared_with,
/// script_bytes) — content-free, for `scripts/pdf-inventory.py --compare`.
fn print_inventory(p: &Path) -> i32 {
    let doc = match fs::read(p)
        .map_err(|e| e.to_string())
        .and_then(|b| Document::from_bytes(&b).map_err(|e| e.to_string()))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("inventory: {e}");
            return 1;
        }
    };
    match semantic::inventory(&doc) {
        Ok(items) => {
            for it in items {
                println!("{}", serde_json::to_string(&it).unwrap());
            }
            0
        }
        Err(e) => {
            eprintln!("inventory: {e}");
            1
        }
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
    /// `--semantic-probe`: mutate real files through the editing API and
    /// check the semantic snapshot notices (`semantic::probe`).
    semantic_probe: bool,
    /// `--colour-probe`: inject colour errors into real renders and
    /// record the colour statistics (`run_colour_probe`).
    colour_probe: bool,
    /// Run the font usage audit instead of a compat run.
    font_audit: bool,
    /// Round-trip knobs (`--mode smoke|full` sets them together).
    rt_pages: usize,
    rt_dpi: f64,
    poppler_pages: usize,
    rt_budget_secs: u64,
    mode: String,
    /// `--rewrite-options <set>` (repeatable): extra configurations
    /// measured next to the default (`configs.rs`).
    rewrite_options: Vec<configs::RewriteConfig>,
    /// `inventory FILE`: print the action inventory as JSON lines.
    inventory: Option<PathBuf>,
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
        semantic_probe: false,
        colour_probe: false,
        font_audit: false,
        rt_pages: usize::MAX,
        rt_dpi: 48.0,
        poppler_pages: usize::MAX,
        rt_budget_secs: 120,
        mode: "full".into(),
        rewrite_options: Vec::new(),
        inventory: None,
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
            "--semantic-probe" => a.semantic_probe = true,
            "--colour-probe" => a.colour_probe = true,
            "--rt-pages" => {
                a.rt_pages = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.rt_pages)
            }
            "--rt-dpi" => a.rt_dpi = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.rt_dpi),
            "--poppler-pages" => {
                a.poppler_pages = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(a.poppler_pages)
            }
            "--rt-budget-secs" => {
                a.rt_budget_secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(a.rt_budget_secs)
            }
            // Presets. `smoke`: PR-sized (2 pages, low dpi, short
            // budget); `full`: every page of every file, Poppler on
            // every page.
            "--mode" => {
                a.mode = it.next().unwrap_or_else(|| "full".into());
                match a.mode.as_str() {
                    "smoke" => {
                        a.max_pages = 2;
                        a.rt_pages = 2;
                        a.rt_dpi = 24.0;
                        a.poppler_pages = 1;
                        a.rt_budget_secs = 10;
                        a.timeout_secs = 10;
                    }
                    _ => {
                        a.max_pages = usize::MAX;
                        a.rt_pages = usize::MAX;
                        a.rt_dpi = 48.0;
                        a.poppler_pages = usize::MAX;
                        a.rt_budget_secs = 120;
                    }
                }
            }
            "--font-audit" => a.font_audit = true,
            "--rewrite-options" => {
                let set = it.next().unwrap_or_default();
                match configs::RewriteConfig::parse(&set) {
                    Some(c) if !a.rewrite_options.contains(&c) => a.rewrite_options.push(c),
                    Some(_) => {}
                    None => {
                        eprintln!("unknown --rewrite-options set {set:?} (default, carry, remove-usage-rights, carry,remove-usage-rights)");
                        std::process::exit(2);
                    }
                }
            }
            "inventory" => a.inventory = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    a
}

fn main() {
    let args = parse_args();
    for c in &args.rewrite_options {
        if let Some(why) = c.unavailable() {
            eprintln!("--rewrite-options {}: not available: {why}", c.name());
            std::process::exit(2);
        }
    }
    if let Some(p) = &args.inventory {
        std::process::exit(print_inventory(p));
    }
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
    if args.colour_probe {
        run_colour_probe(&files, &args);
        return;
    }
    if args.semantic_probe {
        semantic::probe(&files);
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
        r.producer = if entry.producer.is_empty() {
            "unknown".into()
        } else {
            entry.producer.clone()
        };
        r.pdf_version = entry.version.clone();
        eprintln!("{}", r.verdict.label());
        results.push(r);
    }

    // ---- analysis ----------------------------------------------------

    // ---- analysis ----------------------------------------------------
    let cfg = PrioritiesConfig::default();
    let feature_stats = analysis::analyze(&results, &cfg);
    let combos = analysis::combo_analysis(&results, &cfg, 4);
    let stage_summary = analysis::stage_summary(&results);
    let categories = analysis::category_summary(&results);
    let split = analysis::split_summary(&results);
    let tallies = analysis::tallies(&results);
    let roundtrip = roundtrip_tallies(&results);

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
            tallies,
            combos,
            roundtrip,
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

/// Round-trip tallies: all strictly parsed files, by expect tag, by
/// category and by document feature. Rates are PASS over *attempted*;
/// not-applicable and refused rewrites never count as passes.
fn roundtrip_tallies(files: &[FileResult]) -> Vec<RtTally> {
    let eligible: Vec<&FileResult> = files
        .iter()
        .filter(|f| !f.recovered && f.parse.class == Class::Pass && f.roundtrip.is_some())
        .collect();
    let of = |name: &str, pred: &dyn Fn(&FileResult) -> bool| -> RtTally {
        RtTally::of(
            name,
            eligible
                .iter()
                .filter(|f| pred(f))
                .filter_map(|f| f.roundtrip.as_ref()),
        )
    };
    let mut out = vec![of("all", &|_| true)];
    for tag in ["pass", "reject", "any"] {
        out.push(of(&format!("expect:{tag}"), &|f| f.expect == tag));
    }
    let mut cats: Vec<String> = eligible.iter().map(|f| f.category.clone()).collect();
    cats.sort();
    cats.dedup();
    for c in cats {
        out.push(of(&format!("category:{c}"), &|f| f.category == c));
    }
    let mut feats: Vec<String> = eligible.iter().flat_map(|f| f.features.clone()).collect();
    feats.sort();
    feats.dedup();
    for ft in feats {
        out.push(of(&format!("feature:{ft}"), &|f| f.features.contains(&ft)));
    }
    out
}

fn roundtrip_section(s: &mut String, r: &RunReport) {
    let Some(a) = &r.analysis else { return };
    if a.roundtrip.is_empty() {
        return;
    }
    s.push_str("## Round trip (rewrite → reparse → compare)\n\n");
    s.push_str(
        "Does the rewrite preserve the document? Each strictly parsed file is rewritten, \
         reparsed, and compared with the original: the **object graph** reachable from \
         `/Root` and `/Info` (renumbering-independent — every key, value, stream byte and \
         reference target), the **render** (kasane on both files, every page within the \
         per-file budget at the round-trip dpi; a match proves only that the rewrite changed \
         nothing *this* renderer shows), the **text** (characters, Unicode, positions, \
         ActualText per page) and the **annotations / form fields / named destinations**. \
         Poppler renders every compared page of both files as an independent witness. Each \
         cell is PASS/FAIL over *attempted*; U = the \
         engine refused the rewrite (profile limit, not damage), N/A = nothing of that kind \
         in the file. *full* = rewrite passed and every attempted comparison passed; its \
         denominator is *files*.\n\n\
         | group | files | rewrite P/F/U | reparse P/F | structure P/F | render P/F/N/A | text P/F/N/A | annots P/F/N/A | qpdf clean/warn/err | **full** | size ×mean/max |\n\
         |---|---:|---|---|---|---|---|---|---|---:|---|\n",
    );
    let c = |x: &roundtrip::RtCounts, na: bool| {
        if na {
            format!("{}/{}/{}", x.pass, x.fail, x.not_applicable)
        } else {
            format!("{}/{}", x.pass, x.fail)
        }
    };
    for t in &a.roundtrip {
        if t.files == 0 {
            continue;
        }
        s.push_str(&format!(
            "| {} | {} | {}/{}/{} | {} | {} | {} | {} | {} | {}/{}/{} | **{}** | {:.2}/{:.2} |\n",
            t.group,
            t.files,
            t.rewrite.pass,
            t.rewrite.fail,
            t.rewrite.unsupported,
            c(&t.reparse, false),
            c(&t.structure, false),
            c(&t.render, true),
            c(&t.text, true),
            c(&t.annotations, true),
            t.qpdf_clean,
            t.qpdf_warnings,
            t.qpdf_errors,
            t.full,
            t.size_ratio_mean,
            t.size_ratio_max
        ));
    }
    s.push('\n');
    s.push_str(
        "External validity and page coverage (same groups): *valid* = qpdf accepts the \
         rewritten file with no warning; *no new* = no qpdf warning/error and no Poppler \
         syntax diagnostic beyond what the original already produced; *pages* = pages our \
         renderer compared / files whose budget left pages uncompared (those count as \
         PARTIAL, never as a pass); *Poppler* = files Poppler compared on every page / files \
         with any divergent page.\n\n\
         | group | valid P/F/N/A | no new P/F/N/A | render partial | pages compared / files with skipped pages | Poppler files / divergent |\n\
         |---|---|---|---:|---|---|\n",
    );
    for t in &a.roundtrip {
        if t.files == 0 {
            continue;
        }
        s.push_str(&format!(
            "| {} | {}/{}/{} | {}/{}/{} | {} | {} / {} | {} / {} |\n",
            t.group,
            t.external_valid.pass,
            t.external_valid.fail,
            t.external_valid.not_applicable,
            t.external_no_new.pass,
            t.external_no_new.fail,
            t.external_no_new.not_applicable,
            t.render.partial,
            t.pages_compared,
            t.pages_skipped_files,
            t.poppler_files,
            t.poppler_divergent
        ));
    }
    s.push('\n');
    // Failures, with their first note.
    let mut rows = 0;
    for f in &r.files {
        let Some(rt) = &f.roundtrip else { continue };
        if f.recovered || f.parse.class != Class::Pass {
            continue;
        }
        let failing: Vec<(&str, &roundtrip::RtCheck)> = [
            ("rewrite", &rt.rewrite),
            ("reparse", &rt.reparse),
            ("structure", &rt.structure),
            ("render", &rt.render),
            ("text", &rt.text),
            ("annotations", &rt.annotations),
            ("external-valid", &rt.external_valid),
            ("external-no-new", &rt.external_no_new),
        ]
        .into_iter()
        .filter(|(_, c)| matches!(c.class, RtClass::Fail | RtClass::Partial))
        .collect();
        if failing.is_empty() {
            continue;
        }
        if rows == 0 {
            s.push_str("Round-trip failures (first note each):\n\n| file | check | result | note |\n|---|---|---|---|\n");
        }
        rows += 1;
        if rows > 60 {
            s.push_str("| … | | | |\n");
            break;
        }
        for (name, chk) in failing {
            s.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                f.id,
                name,
                chk.class.label(),
                chk.notes
                    .first()
                    .map(|n| n.replace('|', "\\|"))
                    .unwrap_or_default()
            ));
        }
    }
    if rows > 0 {
        s.push('\n');
    }
    // Poppler witness summary.
    let w: Vec<f64> = r
        .files
        .iter()
        .filter_map(|f| f.roundtrip.as_ref().and_then(|rt| rt.poppler_ssim))
        .collect();
    if !w.is_empty() {
        let low = w.iter().filter(|v| **v < 0.99).count();
        s.push_str(&format!(
            "Poppler witness (page 1 of original vs rewritten, SSIM): {} files, mean {:.4}, {} below 0.99.\n\n",
            w.len(),
            w.iter().sum::<f64>() / w.len() as f64,
            low
        ));
    }
    let ms: Vec<u128> = r
        .files
        .iter()
        .filter_map(|f| f.roundtrip.as_ref().map(|rt| rt.total_ms))
        .collect();
    if !ms.is_empty() {
        let mut sorted = ms.clone();
        sorted.sort_unstable();
        s.push_str(&format!(
            "Round-trip time per file: median {} ms, p90 {} ms, max {} ms.\n\n",
            sorted[sorted.len() / 2],
            sorted[sorted.len() * 9 / 10],
            sorted[sorted.len() - 1]
        ));
    }
}

/// One grouped table: `key` names the group; counts are strict (see
/// `analysis::rated_class`); `full` = every stage passed strictly.
fn group_table(
    s: &mut String,
    title: &str,
    files: &[FileResult],
    key: impl Fn(&FileResult) -> String,
    max_rows: usize,
) {
    let mut groups: BTreeMap<String, Vec<&FileResult>> = BTreeMap::new();
    for f in files {
        groups.entry(key(f)).or_default().push(f);
    }
    let mut rows: Vec<(String, Vec<&FileResult>)> = groups.into_iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    s.push_str(&format!(
        "## {title}\n\n| {} | files | parse | render | text | rewrite | full PASS | render rate |\n|---|---:|---:|---:|---:|---:|---:|---:|\n",
        title.trim_start_matches("By ")
    ));
    let mut other: Vec<&FileResult> = Vec::new();
    for (i, (name, fs)) in rows.iter().enumerate() {
        if i >= max_rows {
            other.extend(fs.iter().copied());
            continue;
        }
        push_group_row(s, name, fs);
    }
    if !other.is_empty() {
        push_group_row(s, "(other)", &other);
    }
    s.push('\n');
}

fn push_group_row(s: &mut String, name: &str, fs: &[&FileResult]) {
    let strict = |f: &FileResult, st: &StageResult| analysis::rated_class(f, st) == Class::Pass;
    let parse = fs.iter().filter(|f| f.parse.class == Class::Pass).count();
    let render = fs.iter().filter(|f| strict(f, &f.render)).count();
    let text = fs.iter().filter(|f| strict(f, &f.text)).count();
    let rewrite = fs.iter().filter(|f| strict(f, &f.rewrite)).count();
    let full = fs
        .iter()
        .filter(|f| {
            f.parse.class == Class::Pass
                && strict(f, &f.render)
                && strict(f, &f.text)
                && strict(f, &f.rewrite)
        })
        .count();
    s.push_str(&format!(
        "| {} | {} | {} | {} | {} | {} | {} | {:.0}% |\n",
        name,
        fs.len(),
        parse,
        render,
        text,
        rewrite,
        full,
        render as f64 / fs.len().max(1) as f64 * 100.0
    ));
}

/// Wall time and memory: distribution, the slowest files normalised
/// by page and by size, and the largest peak RSS — the inputs for
/// deciding which limits to tighten.
fn performance_section(s: &mut String, files: &[FileResult]) {
    if files.is_empty() {
        return;
    }
    let mut totals: Vec<u128> = files.iter().map(|f| f.metrics.total_ms).collect();
    totals.sort_unstable();
    let pct = |p: f64| totals[((totals.len() - 1) as f64 * p).round() as usize];
    let stage_sum =
        |get: fn(&FileResult) -> &StageResult| -> u128 { files.iter().map(|f| get(f).ms).sum() };
    s.push_str("## Performance\n\n");
    s.push_str(&format!(
        "Per-file wall time: median {} ms, p90 {} ms, p99 {} ms, max {} ms. \
         Stage totals across the corpus: parse {} ms, render {} ms, text {} ms, rewrite {} ms \
         (render includes the Poppler reference and image comparison when enabled).\n\n",
        pct(0.5),
        pct(0.9),
        pct(0.99),
        totals.last().copied().unwrap_or(0),
        stage_sum(|f| &f.parse),
        stage_sum(|f| &f.render),
        stage_sum(|f| &f.text),
        stage_sum(|f| &f.rewrite)
    ));
    let mut by_total: Vec<&FileResult> = files.iter().collect();
    by_total.sort_by_key(|f| std::cmp::Reverse(f.metrics.total_ms));
    s.push_str("Slowest files:\n\n| file | pages | KB | parse | render | text | rewrite | total ms | ms/page | ms/MB | peak RSS MB |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for f in by_total.iter().take(12) {
        let pages = f.total_pages.max(1) as f64;
        let mb = (f.bytes as f64 / 1_048_576.0).max(0.001);
        s.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.0} |\n",
            f.id,
            f.total_pages,
            f.bytes / 1024,
            f.parse.ms,
            f.render.ms,
            f.text.ms,
            f.rewrite.ms,
            f.metrics.total_ms,
            f.metrics.total_ms as f64 / pages,
            f.metrics.total_ms as f64 / mb,
            f.metrics.max_rss_bytes as f64 / 1_048_576.0
        ));
    }
    s.push('\n');
    let mut by_rss: Vec<&FileResult> = files.iter().collect();
    by_rss.sort_by_key(|f| std::cmp::Reverse(f.metrics.max_rss_bytes));
    s.push_str(
        "Largest peak RSS (process-wide high-water mark sampled after the file — \
         cumulative over the run, so an upper bound per file):\n\n",
    );
    for f in by_rss.iter().take(5) {
        s.push_str(&format!(
            "- `{}` — {:.0} MB ({} KB input, {} pages)\n",
            f.id,
            f.metrics.max_rss_bytes as f64 / 1_048_576.0,
            f.bytes / 1024,
            f.total_pages
        ));
    }
    s.push('\n');
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
    /// Producer family (normalised `/Producer`, from `fetch-corpus.py`).
    producer: String,
    /// `%PDF-M.m` header version.
    version: String,
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
        } else if let Some(v) = l.strip_prefix("producer:") {
            cur.producer = v.trim().trim_matches(|c| c == '"' || c == '\'').into();
        } else if let Some(v) = l.strip_prefix("version:") {
            cur.version = v.trim().trim_matches(|c| c == '"' || c == '\'').into();
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
    /// Headline tallies with explicit denominators (see
    /// `analysis::Tally`); `default` for reports saved before it.
    #[serde(default)]
    tallies: Vec<analysis::Tally>,
    /// Feature-*set* priorities: what implementing 1–4 features
    /// together would unblock (see `analysis::combo_analysis`).
    #[serde(default)]
    combos: Vec<analysis::ComboStat>,
    /// Round-trip tallies per group (`roundtrip::RtTally`).
    #[serde(default)]
    roundtrip: Vec<RtTally>,
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

    // headline counts — every number in one row shares that row's
    // denominator (`files`); see `analysis::Tally`.
    s.push_str("## Headline\n\n");
    s.push_str(
        "Strict counts: a stage counts only when it passed on a strictly parsed document; \
         *full PASS* = all four stages. Each row is its own population — never compare \
         numbers across rows.\n\n\
         | files | n | parse | render | text | rewrite | full PASS | recovered | crash/timeout |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for t in analysis::tallies(&r.files) {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | **{}** | {} | {} |\n",
            t.group,
            t.files,
            t.parse,
            t.render,
            t.text,
            t.rewrite,
            t.full_pass,
            t.recovered,
            t.crash_or_timeout
        ));
        debug_assert!(t.consistent(), "{t:?}");
    }
    s.push('\n');
    s.push_str(
        "Render outcomes: *strict* is the render column above (strictly parsed, rendered \
         with every construct the specification allows — the number comparable across \
         runs); *approximate* is the part of it that used a reported substitution or colour \
         approximation; *recovered* rendered only on a recovery-mode document \
         (`Document::from_bytes_recovering`: rebuilt xref and/or the recovery rules, each \
         recorded as a warning) — never counted as strict; *unsupported* and *rejected* \
         did not render either way. Each row is its own population.\n\n\
         | files | n | strict | of which approximate | recovered | unsupported | rejected | warnings | recovery notes |\n\
         |---|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for t in analysis::tallies(&r.files) {
        s.push_str(&format!(
            "| {} | {} | **{}** | {} | {} | {} | {} | {} | {} |\n",
            t.group,
            t.files,
            t.render,
            t.render_approximate,
            t.render_recovered,
            t.render_unsupported,
            t.render_rejected,
            t.warnings,
            t.recovery_notes
        ));
    }
    s.push('\n');
    let warned: Vec<&FileResult> = r
        .files
        .iter()
        .filter(|f| !f.metrics.warnings.is_empty())
        .collect();
    let warned_pass = warned.iter().filter(|f| f.verdict == Class::Pass).count();
    s.push_str(&format!(
        "Accepted with warnings: {} file(s) recorded a non-fatal deviation the engine \
         processed exactly (`Document::take_warnings`) — {} of them are full PASS, so a \
         *clean* full PASS is {} on all files. Per-file warnings are listed under \
         *Warnings* below.\n\n",
        warned.len(),
        warned_pass,
        r.files.iter().filter(|f| f.verdict == Class::Pass).count() - warned_pass
    ));
    roundtrip_section(&mut s, r);

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
            let idx = match analysis::rated_class(f, get(f)) {
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

    // Recovery parse: files whose strict parse failed but whose xref
    // table could be rebuilt by scanning. Their downstream stages ran
    // on the rebuilt document; they stay out of every pass count above.
    let recovered: Vec<&FileResult> = r.files.iter().filter(|f| f.recovered).collect();
    let attempted = r
        .files
        .iter()
        .filter(|f| f.parse.class != Class::Pass)
        .count();
    if attempted > 0 {
        s.push_str(&format!(
            "## Recovery parse\n\n{} file(s) failed the strict parse; the xref rebuild \
             (`Document::from_bytes_recovering`) succeeded on **{}**. Their render/text/rewrite \
             results below are *not* counted in the stage or compatibility rates — a recovered \
             document is a best-effort reading of a damaged file.\n\n",
            attempted,
            recovered.len()
        ));
        if !recovered.is_empty() {
            s.push_str("| file | expect | strict error | recovery | render | text | rewrite |\n|---|---|---|---|---|---|---|\n");
            for f in &recovered {
                s.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} | {} |\n",
                    f.id,
                    f.expect,
                    f.parse.detail.replace('|', "\\|"),
                    f.recover.detail.replace('|', "\\|"),
                    f.render.class.label(),
                    f.text.class.label(),
                    f.rewrite.class.label()
                ));
            }
            s.push('\n');
        }
        let failed: Vec<&FileResult> = r
            .files
            .iter()
            .filter(|f| f.parse.class != Class::Pass && !f.recovered)
            .collect();
        if !failed.is_empty() {
            s.push_str("Not recoverable:\n\n");
            for f in &failed {
                s.push_str(&format!(
                    "- `{}` ({}) — {}\n",
                    f.id,
                    f.expect,
                    f.recover.detail.replace('|', "\\|")
                ));
            }
            s.push('\n');
        }
    }

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
            // Content bar — the whole-page trio stays the comparable
            // baseline; this second bar cannot be inflated by blank
            // area and catches missing/extra content: ink coverage
            // within ×0.7–1.4 of the reference AND either content-SSIM
            // ≥ 0.85 over ink windows or a ±1 px-tolerant diff ≤ 0.5%
            // (a page whose only ink is a few thin strokes one pixel
            // off scores a poor content-SSIM yet is not misdrawn).
            // Files with no ink anywhere pass it.
            let content_ok = |q: &RenderQuality| {
                let ratio = if q.ink_ref > 0.0 {
                    q.ink_ours / q.ink_ref
                } else if q.ink_ours > 0.0 {
                    f64::INFINITY
                } else {
                    1.0
                };
                (0.7..=1.4).contains(&ratio) && (q.content_ssim >= 0.85 || q.diff_px_tol1 <= 0.005)
            };
            let with_q: Vec<&&FileResult> = measured
                .iter()
                .filter(|f| f.metrics.render_quality.is_some())
                .collect();
            let content_pass = with_q
                .iter()
                .filter(|f| content_ok(f.metrics.render_quality.as_ref().unwrap()))
                .count();
            let mean_cssim = mean(&|f| f.metrics.render_quality.as_ref().map(|q| q.content_ssim));
            let min_cssim = with_q
                .iter()
                .filter_map(|f| f.metrics.render_quality.as_ref().map(|q| q.content_ssim))
                .fold(f64::INFINITY, f64::min);
            let mean_tol1 = mean(&|f| f.metrics.render_quality.as_ref().map(|q| q.diff_px_tol1));
            let pages_compared: usize = measured.iter().map(|f| f.metrics.render_pages.len()).sum();
            s.push_str("## Render Visual Fidelity\n\n");
            s.push_str(&format!(
                "*measured on {} file(s) / {pages_compared} page(s) with a Poppler \
                 reference — {fid_pass} meet the comparability bar \
                 (SSIM ≥ 0.98, diff-pixels ≤ 2%, MAE ≤ 0.05) and {content_pass} of \
                 {} meet the content bar (ink coverage within ×0.7–1.4 of the \
                 reference, and content-SSIM ≥ 0.85 over ink windows or a ±1 px \
                 tolerant diff ≤ 0.5%). \
                 Per-page render execution: {pages_ok}/{pages_total}. Execution \
                 success and fidelity are counted separately — a page that renders \
                 but diverges is a render PASS and a fidelity failure.*\n\n",
                n,
                with_q.len()
            ));
            // Substituted fonts: execution PASS, but the glyph shapes
            // are the bundled Liberation faces, not the named fonts —
            // say so next to the fidelity numbers, never hide it.
            let subst_files: Vec<&&FileResult> = measured
                .iter()
                .filter(|f| !f.metrics.render_font_substitutions.is_empty())
                .collect();
            if !subst_files.is_empty() {
                s.push_str(&format!(
                    "*{} of the measured file(s) drew text with a **bundled substitute** \
                     for a non-embedded font (metric-compatible Liberation faces; shapes \
                     are approximations of the named fonts, so their SSIM is bounded by \
                     the substitute, not by the engine):*\n\n",
                    subst_files.len()
                ));
                for f in &subst_files {
                    s.push_str(&format!(
                        "- `{}` — {}\n",
                        f.id,
                        f.metrics.render_font_substitutions.join("; ")
                    ));
                }
                s.push('\n');
            }
            // Approximated colour spaces: the render executed, but an
            // ICC profile was replaced by its /Alternate — hue/
            // gradation differences against a CMS-backed viewer are
            // expected and are not a compositing defect.
            let approx_files: Vec<&&FileResult> = measured
                .iter()
                .filter(|f| !f.metrics.render_color_approximations.is_empty())
                .collect();
            if !approx_files.is_empty() {
                s.push_str(&format!(
                    "*{} of the measured file(s) converted colours through an \
                     **approximated colour space** (an ICC profile the engine does not \
                     evaluate — LUT/CMYK/Lab-PCS — so its `/Alternate` was used; hues may \
                     differ from poppler's lcms output):*\n\n",
                    approx_files.len()
                ));
                for f in &approx_files {
                    s.push_str(&format!(
                        "- `{}` — {}\n",
                        f.id,
                        f.metrics.render_color_approximations.join("; ")
                    ));
                }
                s.push('\n');
            }
            s.push_str("| metric | mean | worst |\n|---|---:|---:|\n");
            s.push_str(&format!(
                "| SSIM (higher = closer) | {:.4} | {:.4} (min) |\n",
                mean_ssim, min_ssim
            ));
            s.push_str(&format!(
                "| content-SSIM (ink windows only) | {:.4} | {:.4} (min) |\n",
                mean_cssim,
                if min_cssim.is_finite() {
                    min_cssim
                } else {
                    1.0
                }
            ));
            s.push_str(&format!(
                "| MAE (lower = closer) | {:.4} | {:.4} (max) |\n",
                mean_mae, max_mae
            ));
            s.push_str(&format!("| diff-pixel ratio | {:.4} | — |\n", mean_dpx));
            s.push_str(&format!(
                "| diff-pixel ratio, ±1 px tolerant | {:.4} | — |\n\n",
                mean_tol1
            ));
            // list the files that diverged beyond either bar
            let divergent: Vec<_> = measured
                .iter()
                .filter(|f| {
                    let comparability = f.metrics.render_ssim.unwrap_or(0.0) >= 0.98
                        && f.metrics.render_diff_px_ratio.unwrap_or(1.0) <= 0.02
                        && f.metrics.render_mae.unwrap_or(1.0) <= 0.05;
                    let content = f
                        .metrics
                        .render_quality
                        .as_ref()
                        .map(content_ok)
                        .unwrap_or(true);
                    !(comparability && content)
                })
                .collect();
            if !divergent.is_empty() {
                s.push_str(&format!(
                    "**{} file(s) diverge from the reference** (worst page shown; \
                     ours/ref/diff images saved):\n\n",
                    divergent.len()
                ));
                s.push_str(
                    "| file | page | SSIM | content-SSIM | MAE | diff-px | ±1px | ink ours/ref | best shift | bar failed | diff image |\n\
                     |---|---:|---:|---:|---:|---:|---:|---|---|---|---|\n",
                );
                for f in divergent.iter().take(30) {
                    let q = f.metrics.render_quality.as_ref();
                    let worst_page = f
                        .metrics
                        .render_pages
                        .iter()
                        .min_by(|a, b| a.ssim.partial_cmp(&b.ssim).unwrap())
                        .map(|p| p.page)
                        .unwrap_or(1);
                    let comparability = f.metrics.render_ssim.unwrap_or(0.0) >= 0.98
                        && f.metrics.render_diff_px_ratio.unwrap_or(1.0) <= 0.02
                        && f.metrics.render_mae.unwrap_or(1.0) <= 0.05;
                    let content = q.map(content_ok).unwrap_or(true);
                    let failed = match (comparability, content) {
                        (false, false) => "both",
                        (false, true) => "comparability",
                        (true, false) => "content",
                        (true, true) => "—",
                    };
                    s.push_str(&format!(
                        "| `{}` | {} | {:.3} | {} | {:.3} | {:.3} | {} | {} | {} | {} | {} |\n",
                        f.id,
                        worst_page,
                        f.metrics.render_ssim.unwrap_or(0.0),
                        q.map(|q| format!("{:.3}", q.content_ssim))
                            .unwrap_or_else(|| "—".into()),
                        f.metrics.render_mae.unwrap_or(0.0),
                        f.metrics.render_diff_px_ratio.unwrap_or(0.0),
                        q.map(|q| format!("{:.3}", q.diff_px_tol1))
                            .unwrap_or_else(|| "—".into()),
                        q.map(|q| format!("{:.3}/{:.3}", q.ink_ours, q.ink_ref))
                            .unwrap_or_else(|| "—".into()),
                        q.map(|q| {
                            format!(
                                "({:+},{:+}) → {:.3}",
                                q.best_shift.0, q.best_shift.1, q.diff_px_at_best_shift
                            )
                        })
                        .unwrap_or_else(|| "—".into()),
                        failed,
                        f.metrics
                            .render_diff_png
                            .as_deref()
                            .map(|p| format!("`{}`", p))
                            .unwrap_or_default()
                    ));
                }
                s.push('\n');
                s.push_str(
                    "*How to read: a low SSIM with a high content-SSIM and a small ±1px \
                     ratio is rasteriser/anti-aliasing noise; a low content-SSIM with ink \
                     coverage ≈ reference is misplaced content; ink coverage far from the \
                     reference is missing or extra content; a non-zero best shift that \
                     sharply lowers diff-px is a systematic position offset.*\n\n",
                );
            }
        }

        // Compatibility vs Robustness — the two independent rates.
        if let Some(sp) = &a.split {
            s.push_str("## Compatibility vs Robustness\n\n");
            s.push_str(&format!(
                "**Compatibility** — {} well-formed files (`expect: pass`):\n\n",
                sp.compat_total
            ));
            let full = r
                .files
                .iter()
                .filter(|f| f.expect == "pass")
                .filter(|f| {
                    f.parse.class == Class::Pass
                        && analysis::rated_class(f, &f.render) == Class::Pass
                        && analysis::rated_class(f, &f.text) == Class::Pass
                        && analysis::rated_class(f, &f.rewrite) == Class::Pass
                })
                .count();
            s.push_str("| stage | pass | rate |\n|---|---:|---:|\n");
            for (name, n) in [
                ("parse", sp.compat_parse_pass),
                ("render", sp.compat_render_pass),
                ("text", sp.compat_text_pass),
                ("rewrite", sp.compat_rewrite_pass),
                ("full PASS (all four)", full),
            ] {
                s.push_str(&format!(
                    "| {} | {} | {:.0}% |\n",
                    name,
                    n,
                    n as f64 / sp.compat_total.max(1) as f64 * 100.0
                ));
            }
            s.push_str(&format!(
                "\nUnexpected rejections (strict parse or render refused an \
                 `expect: pass` file): {}.\n",
                sp.compat_total - sp.compat_render_pass
            ));
            s.push_str(&format!(
                "\n**Robustness** — {} adversarial files (`expect: reject`):\n\
                 safely handled (no crash / timeout) {} ({:.0}%), refused by the \
                 strict parse or render {}, unexpected acceptances (parsed and \
                 rendered strictly) {} — **{} crash/hang failure(s)**. \
                 Legacy \"cleanly refused\" (verdict error/unsupported): {}.\n\n",
                sp.robust_total,
                sp.robust_safe,
                sp.robust_safe as f64 / sp.robust_total.max(1) as f64 * 100.0,
                sp.robust_strict_refused,
                sp.robust_unexpected_accept.len(),
                sp.robust_failures.len(),
                sp.robust_refused
            ));
            for id in &sp.robust_unexpected_accept {
                s.push_str(&format!("- `{id}` — tagged `reject`, accepted strictly\n"));
            }
            if !sp.robust_unexpected_accept.is_empty() {
                s.push('\n');
            }
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

        // Grouping axes beyond category: producer, PDF version, page
        // count, file size, encryption — each with the same strict
        // counts (a recovered file's downstream stages count as skip).
        group_table(&mut s, "By producer", &r.files, |f| f.producer.clone(), 30);
        group_table(
            &mut s,
            "By PDF version",
            &r.files,
            |f| {
                if f.pdf_version.is_empty() {
                    "unknown".into()
                } else {
                    f.pdf_version.clone()
                }
            },
            12,
        );
        group_table(
            &mut s,
            "By page count",
            &r.files,
            |f| {
                match f.total_pages {
                    0 => "unknown (parse failed)",
                    1 => "1",
                    2..=5 => "2–5",
                    6..=20 => "6–20",
                    21..=100 => "21–100",
                    _ => "> 100",
                }
                .into()
            },
            8,
        );
        group_table(
            &mut s,
            "By file size",
            &r.files,
            |f| {
                match f.bytes {
                    0..=10_239 => "< 10 KB",
                    10_240..=102_399 => "10–100 KB",
                    102_400..=1_048_575 => "100 KB – 1 MB",
                    1_048_576..=8_388_607 => "1–8 MB",
                    _ => "> 8 MB",
                }
                .into()
            },
            6,
        );
        group_table(
            &mut s,
            "By encryption",
            &r.files,
            |f| {
                if f.parse.detail == "encrypted" || f.parse.feature == "encrypted" {
                    "encrypted".into()
                } else {
                    "not encrypted".into()
                }
            },
            3,
        );
        performance_section(&mut s, &r.files);

        // development priorities
        s.push_str("## Development priorities\n\n");
        s.push_str(&priorities_table(&a.features));
        s.push('\n');
        if !a.combos.is_empty() {
            s.push_str(&analysis::combos_table(&a.combos, 12));
        }
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
    let warned: Vec<&FileResult> = r
        .files
        .iter()
        .filter(|f| !f.metrics.warnings.is_empty())
        .collect();
    if !warned.is_empty() {
        s.push_str("## Warnings\n\nFiles the engine accepted while recording a non-fatal deviation (the result is exact for the bytes present; the input is not conforming):\n\n| file | verdict | warnings |\n|---|---|---|\n");
        for f in &warned {
            s.push_str(&format!(
                "| `{}` | {} | {} |\n",
                f.id,
                f.verdict.label(),
                f.metrics.warnings.join("; ")
            ));
        }
        s.push('\n');
    }
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
/// Colour-metric calibration on real pages: for page 1 of every file
/// that parses and renders strictly, the colour statistics of our render
/// against Poppler's as is (`none`), and with one injected colour error
/// in a colourful 48-px patch of our render — channels rotated (a hue
/// change), red and blue swapped, the CMYK black dropped (each colour
/// through naive CMYK and back without K), the patch desaturated to the
/// grey of the same luma (a chroma change). Writes
/// `colour-probe.json` (per page: id, kind, statistics) into `--out`;
/// the analysis is done outside the runner.
fn run_colour_probe(files: &[PathBuf], args: &Args) {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for path in files {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        let Ok(data) = fs::read(path) else { continue };
        let Ok(doc) = Document::from_bytes(&data) else {
            continue;
        };
        let Some(page) = doc.pages().first() else {
            continue;
        };
        let opts = RenderOptions::new().with_dpi(args.dpi);
        let Ok(r) = render_page(&doc, page, &opts) else {
            continue;
        };
        let prefix = args.out.join(format!("probe-{id}"));
        let out = roundtrip::run_bounded(
            Command::new("pdftoppm")
                .args([
                    "-f",
                    "1",
                    "-l",
                    "1",
                    "-r",
                    &args.dpi.to_string(),
                    "-singlefile",
                    "-cropbox",
                ])
                .arg(path)
                .arg(&prefix),
        );
        let ppm = prefix.with_extension("ppm");
        let bytes = fs::read(&ppm).ok();
        let _ = fs::remove_file(&ppm);
        let (Some(out), Some(bytes)) = (out, bytes) else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let Some((rw, rh, reference)) = parse_ppm(&bytes) else {
            continue;
        };
        let (kw, kh) = (r.width() as usize, r.height() as usize);
        if (rw, rh) != (kw, kh) {
            continue;
        }
        let ours = pixmap_rgb(&r);
        let mut push = |kind: &str, img: &[u8]| {
            rows.push(serde_json::json!({
                "id": id, "kind": kind,
                "stats": colour::compare(img, &reference, kw, kh),
            }));
        };
        push("none", &ours);
        // A colourful patch: >= 30 % of its pixels with a channel range
        // >= 40 in *both* renders (so the colour is shared, not a shape).
        let colourful = |img: &[u8], i: usize| {
            let (a, b, c) = (img[i], img[i + 1], img[i + 2]);
            a.max(b).max(c) - a.min(b).min(c) >= 40
        };
        let mut found = None;
        'scan: for y in (0..kh.saturating_sub(48)).step_by(16) {
            for x in (0..kw.saturating_sub(48)).step_by(16) {
                let mut n = 0;
                for yy in y..y + 48 {
                    for xx in x..x + 48 {
                        let i = (yy * kw + xx) * 3;
                        n += (colourful(&ours, i) && colourful(&reference, i)) as usize;
                    }
                }
                if n * 10 >= 48 * 48 * 3 {
                    found = Some((x, y));
                    break 'scan;
                }
            }
        }
        let Some((x0, y0)) = found else { continue };
        let inject = |f: &dyn Fn([u8; 3]) -> [u8; 3]| {
            let mut img = ours.clone();
            for yy in y0..y0 + 48 {
                for xx in x0..x0 + 48 {
                    let i = (yy * kw + xx) * 3;
                    let p = f([img[i], img[i + 1], img[i + 2]]);
                    img[i..i + 3].copy_from_slice(&p);
                }
            }
            img
        };
        push("rotate", &inject(&|p| [p[1], p[2], p[0]]));
        push("swap_rb", &inject(&|p| [p[2], p[1], p[0]]));
        push(
            "cmyk_no_k",
            &inject(&|p| {
                // Naive RGB -> CMYK with black generation, back without K.
                let (c, m, y) = (255 - p[0] as i32, 255 - p[1] as i32, 255 - p[2] as i32);
                let k = c.min(m).min(y);
                [
                    (255 - (c - k)) as u8,
                    (255 - (m - k)) as u8,
                    (255 - (y - k)) as u8,
                ]
            }),
        );
        push(
            "desaturate",
            &inject(&|p| {
                let l = ((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000) as u8;
                [l, l, l]
            }),
        );
    }
    let path = args.out.join("colour-probe.json");
    fs::write(&path, serde_json::to_vec(&rows).unwrap_or_default()).ok();
    println!(
        "colour probe: {} rows written to {}",
        rows.len(),
        path.display()
    );
}

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

#[cfg(test)]
mod text_metric_tests {
    use super::{content_chars, multiset};

    #[test]
    fn normalisation_and_multiset() {
        // Ligature, full-width, combining mark, line breaks, soft hyphen.
        let ours = content_chars("ﬁne Ａ e\u{301}\nx\u{AD}y");
        let reference = content_chars("fine A é xy");
        assert_eq!(ours, reference);
        // Same characters, different order: multiset F1 is 1.
        let (p, r, f) = multiset(&content_chars("ab cd"), &content_chars("cd ab"));
        assert_eq!((p, r, f), (1.0, 1.0, 1.0));
        // Ours half of the reference: precision 1, recall 0.5.
        let (p, r, _) = multiset(&content_chars("ab"), &content_chars("abcd"));
        assert_eq!((p, r), (1.0, 0.5));
        // Vertical-form punctuation and CJK compatibility forms.
        assert_eq!(
            content_chars("\u{FE11}\u{F900}"),
            content_chars("\u{3001}\u{8C48}")
        );
        assert_eq!(multiset(&[], &[]), (1.0, 1.0, 1.0));
    }

    /// Page text "Body"; a field with value "Alice" shown by two widgets
    /// whose appearances draw "Alice"; a hidden widget drawing "Secret";
    /// a note with `/Contents` "Note" and no appearance, and its Popup.
    fn annotated_page() -> kasane_pdf::Document {
        let ap = |s: &str| {
            let c = format!("BT /Helv 10 Tf 2 2 Td ({s}) Tj ET");
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 50 20] /Resources << /Font << /Helv 9 0 R >> >> /Length {} >>\nstream\n{c}\nendstream",
                c.len()
            )
        };
        let content = "BT /Helv 10 Tf 10 100 Td (Body) Tj ET";
        let objs: Vec<(u32, String)> = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 8 0 R] >> >>".into()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
            (
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
                 /Resources << /Font << /Helv 9 0 R >> >> /Annots [6 0 R 7 0 R 8 0 R 10 0 R 11 0 R] >>"
                    .into(),
            ),
            (4, format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())),
            (5, "<< /T (name) /FT /Tx /V (Alice) /Kids [6 0 R 7 0 R] >>".into()),
            (6, "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /Rect [10 10 60 30] /P 3 0 R /AP << /N 12 0 R >> >>".into()),
            (7, "<< /Type /Annot /Subtype /Widget /Parent 5 0 R /Rect [10 40 60 60] /P 3 0 R /AP << /N 13 0 R >> >>".into()),
            (8, "<< /Type /Annot /Subtype /Widget /T (hidden) /FT /Tx /V (Secret) /F 2 /Rect [10 70 60 90] /P 3 0 R /AP << /N 14 0 R >> >>".into()),
            (9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()),
            (10, "<< /Type /Annot /Subtype /Text /Rect [100 100 120 120] /Contents (Note) /Popup 11 0 R >>".into()),
            (11, "<< /Type /Annot /Subtype /Popup /Rect [120 100 180 140] /Parent 10 0 R >>".into()),
            (12, ap("Alice")),
            (13, ap("Alice")),
            (14, ap("Secret")),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (n, b) in &objs {
            offs.push(out.len());
            out.extend_from_slice(format!("{n} 0 obj\n{b}\nendobj\n").as_bytes());
        }
        let x = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        kasane_pdf::Document::from_bytes(&out).unwrap()
    }

    #[test]
    fn annotation_text_counts_each_drawn_appearance_once() {
        use super::compare_text_with_annots;
        let doc = annotated_page();
        let page = &doc.pages()[0];
        // The reference draws the page and both visible widgets.
        let (f1, a) = compare_text_with_annots(&doc, page, "Body", "Body\nAlice\nAlice\n");
        assert_eq!(f1, Some(1.0), "{a:?}");
        assert_eq!(a.ours_chars, 14);
        // Appearances of the two visible widgets only; the field value
        // (once, on the first widget) and /Contents are counted by source
        // and not added; the hidden widget and the Popup add nothing.
        assert_eq!(a.appearance_chars, 10);
        assert_eq!(a.field_value_chars, 5 + 6);
        assert_eq!(a.contents_chars, 4);
        assert_eq!(a.visible, a.annotations - 1, "{a:?}");
        assert!(a.error.is_none());
        // Against a reference without annotation text, adding them costs
        // precision instead of inflating the score.
        let (f1, a) = compare_text_with_annots(&doc, page, "Body", "Body");
        assert!(f1.unwrap() < 0.5 && a.recall == 1.0, "{f1:?} {a:?}");
    }
}

#[cfg(test)]
mod placement_tests {
    use super::{shifted, unmatched_ink};

    /// A 64 × 64 white image with a black `s`-px square at (x, y).
    fn img(x: usize, y: usize, s: usize) -> Vec<u8> {
        let mut v = vec![255u8; 64 * 64];
        for yy in y..y + s {
            for xx in x..x + s {
                v[yy * 64 + xx] = 0;
            }
        }
        v
    }

    #[test]
    fn displaced_and_missing_ink() {
        let r = img(4, 4, 10);
        // Same place: nothing unmatched.
        assert_eq!(unmatched_ink(&r, &r, 64, 64), (0.0, 0));
        // Within NEAR_PX: still matched (rasteriser placement noise).
        assert_eq!(unmatched_ink(&img(6, 4, 10), &r, 64, 64).1, 0);
        // Moved by 40 px: every ink pixel of ours is displaced.
        let (t, n) = unmatched_ink(&img(44, 44, 10), &r, 64, 64);
        assert_eq!((t, n), (1.0, 100));
        // A faint (luma 240) reference stroke counts as a mark.
        let mut faint = r.clone();
        faint
            .iter_mut()
            .for_each(|v| *v = if *v == 0 { 240 } else { *v });
        assert_eq!(unmatched_ink(&r, &faint, 64, 64).1, 0);
        // Too little ink in a tile: not judged, but counted.
        let (t, n) = unmatched_ink(&img(50, 50, 3), &r, 64, 64);
        assert_eq!((t, n), (0.0, 9));
    }

    #[test]
    fn transparent_pixels_composite_on_white() {
        use super::on_white;
        assert_eq!(on_white([0, 0, 0, 0]), [255, 255, 255]);
        assert_eq!(on_white([200, 0, 0, 255]), [200, 0, 0]);
        // Half-transparent pure red, premultiplied: (128, 0, 0, 128).
        assert_eq!(on_white([128, 0, 0, 128]), [255, 127, 127]);
    }

    #[test]
    fn shift_moves_pixels_the_way_best_shift_pairs_them() {
        let a = img(4, 4, 2);
        let m = shifted(&a, 64, 64, 3, -2);
        assert_eq!(m[2 * 64 + 7], 0);
        assert_eq!(m[4 * 64 + 4], 255);
    }

    #[test]
    fn the_rgb_shift_moves_pixels_like_the_luma_shift() {
        use super::{pixmap_luma, shifted_rgb};
        // A red square on a 20 × 20 pt page, rendered at 72 dpi.
        let content = b"1 0 0 rg 4 4 6 6 re f";
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R >>".into(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                std::str::from_utf8(content).unwrap()
            ),
        ];
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for o in offs {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        let doc = kasane_pdf::Document::from_bytes(&pdf).unwrap();
        let r = kasane_pdf::render_page(
            &doc,
            &doc.pages()[0],
            &kasane_pdf::RenderOptions::new().with_dpi(72.0),
        )
        .unwrap();
        let (w, h) = (r.width() as usize, r.height() as usize);
        for (dx, dy) in [(0, 0), (3, -2), (-1, 4)] {
            let luma = shifted(&pixmap_luma(&r), w, h, dx, dy);
            let rgb = shifted_rgb(&r, dx, dy);
            for i in 0..w * h {
                let [cr, cg, cb] = [rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]].map(u32::from);
                assert_eq!(
                    luma[i],
                    ((cr * 299 + cg * 587 + cb * 114) / 1000) as u8,
                    "({dx}, {dy}) px {i}"
                );
            }
        }
    }
}
