//! Round-trip verification: does `rewrite` preserve what the document
//! *means*? The pipeline per file is
//!
//! ```text
//! strict parse → rewrite → reparse → object-graph comparison →
//! render comparison → text comparison → annotation/form comparison →
//! external check (qpdf) → verdict
//! ```
//!
//! Object numbers, xref offsets and the file bytes are allowed to
//! change; everything reachable from the catalog and `/Info` must be
//! the same *graph* — the comparison walks both documents in
//! lock-step, resolving references on each side and keeping an
//! old → new reference map, so renumbering is transparent while a
//! dropped key, a changed value, a stream with different bytes or a
//! reference that now points somewhere else is reported with its path.
//!
//! Rendering is compared kasane-vs-kasane (the same renderer on both
//! files — this detects damage *added* by the rewrite even where our
//! rendering of the original is itself wrong) and, when Poppler is
//! available, Poppler-vs-Poppler on the first page as an independent
//! witness. Two renderers agreeing on both files does not prove either
//! is right; a kasane-vs-kasane match only proves the rewrite changed
//! nothing *we* can see.
//!
//! Every comparison has three outcomes besides PASS/FAIL: *not
//! attempted* (an earlier stage failed), *unsupported* (the engine
//! refused the operation — a profile limit, not damage) and *not
//! applicable* (the document has nothing of that kind, e.g. no
//! annotations). Rates in the report count PASS over *attempted*
//! only; not-applicable never counts as a pass.

use kasane_pdf::{
    extract_text, named_destinations, render_page, Document, Error, IndirectRef, Limits, Object,
    RenderOptions, TextOptions,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Outcome of one round-trip check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RtClass {
    Pass,
    Fail,
    /// Every page compared agreed, but the per-file budget left pages
    /// uncompared — neither a pass nor a failure.
    Partial,
    /// The engine refused (profile limit) — not a round-trip failure.
    Unsupported,
    /// The document has nothing of this kind to preserve.
    NotApplicable,
    /// An earlier stage failed, so this one did not run.
    NotAttempted,
}

impl RtClass {
    pub fn label(self) -> &'static str {
        match self {
            RtClass::Pass => "PASS",
            RtClass::Fail => "FAIL",
            RtClass::Partial => "PARTIAL",
            RtClass::Unsupported => "UNSUPPORTED",
            RtClass::NotApplicable => "N/A",
            RtClass::NotAttempted => "-",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtCheck {
    pub class: RtClass,
    /// First differences / the refusal reason (bounded).
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub ms: u128,
}

impl RtCheck {
    fn pass(ms: u128) -> Self {
        RtCheck {
            class: RtClass::Pass,
            notes: Vec::new(),
            ms,
        }
    }
    fn fail(notes: Vec<String>, ms: u128) -> Self {
        RtCheck {
            class: RtClass::Fail,
            notes,
            ms,
        }
    }
    fn na(note: &str) -> Self {
        RtCheck {
            class: RtClass::NotApplicable,
            notes: vec![note.into()],
            ms: 0,
        }
    }
    fn skip() -> Self {
        RtCheck {
            class: RtClass::NotAttempted,
            notes: Vec::new(),
            ms: 0,
        }
    }
}

impl Default for RtCheck {
    fn default() -> Self {
        RtCheck::skip()
    }
}

/// Per-page render comparison (kasane vs kasane).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtRenderPage {
    pub page: usize,
    pub size_equal: bool,
    pub mae: f64,
    pub ssim: f64,
    pub diff_px_ratio: f64,
}

/// Page coverage of the render comparison.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RtPageStats {
    pub total_pages: usize,
    pub compared_pages: usize,
    /// Not compared because the per-file budget ran out.
    pub skipped_pages: usize,
    /// Our renderer refuses the original page: nothing to compare.
    pub unsupported_pages: usize,
    pub failed_pages: usize,
    pub worst_ssim: Option<f64>,
    pub mean_ssim: Option<f64>,
    pub first_divergent_page: Option<usize>,
}

/// Poppler-vs-Poppler comparison of original and rewritten pages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RtPoppler {
    pub pages_compared: usize,
    pub size_mismatch_pages: usize,
    pub mean_ssim: Option<f64>,
    pub worst_ssim: Option<f64>,
    pub mean_mae: Option<f64>,
    pub worst_diff_px: Option<f64>,
    pub first_divergent_page: Option<usize>,
    /// `Syntax Error`/`Syntax Warning` lines pdftoppm printed for the
    /// original and for the rewritten file.
    pub diagnostics_original: usize,
    pub diagnostics_rewritten: usize,
}

/// Knobs of a run (smoke vs full).
#[derive(Debug, Clone, Copy)]
pub struct RtOptions {
    /// Pages to compare with our renderer (`None` = all).
    pub max_pages: Option<usize>,
    pub dpi: f64,
    /// Run Poppler on both files.
    pub poppler: bool,
    /// Pages Poppler compares (`None` = all).
    pub poppler_pages: Option<usize>,
    /// Wall-clock budget for the render comparisons of one file.
    pub budget_ms: u128,
}

impl Default for RtOptions {
    fn default() -> Self {
        RtOptions {
            max_pages: None,
            dpi: 48.0,
            poppler: false,
            poppler_pages: None,
            budget_ms: 120_000,
        }
    }
}

/// `qpdf --check` on the rewritten file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QpdfCheck {
    pub exit_code: i32,
    pub warnings: usize,
    pub errors: usize,
    /// First few diagnostic lines.
    #[serde(default)]
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoundTrip {
    pub rewrite: RtCheck,
    pub reparse: RtCheck,
    pub structure: RtCheck,
    pub render: RtCheck,
    #[serde(default)]
    pub render_pages: Vec<RtRenderPage>,
    /// Poppler-vs-Poppler SSIM on page 1 (independent witness).
    #[serde(default)]
    pub poppler_ssim: Option<f64>,
    /// Poppler-vs-Poppler over the compared pages.
    #[serde(default)]
    pub poppler: Option<RtPoppler>,
    /// Per-page coverage of the kasane render comparison.
    #[serde(default)]
    pub pages: RtPageStats,
    /// External parser (qpdf) on the *original*, for the "newly
    /// introduced" judgement.
    #[serde(default)]
    pub qpdf_original: Option<QpdfCheck>,
    /// The rewritten file is valid for qpdf (exit 0, no warnings).
    #[serde(default)]
    pub external_valid: RtCheck,
    /// The rewritten file has no qpdf warning/error the original did
    /// not already have, and no Poppler syntax error the original did
    /// not already produce.
    #[serde(default)]
    pub external_no_new: RtCheck,
    pub text: RtCheck,
    /// Normalised (white-space-collapsed) text match ratio per file,
    /// reported next to the exact comparison.
    #[serde(default)]
    pub text_normalized_ratio: Option<f64>,
    pub annotations: RtCheck,
    #[serde(default)]
    pub qpdf: Option<QpdfCheck>,
    /// rewritten bytes / original bytes.
    #[serde(default)]
    pub size_ratio: Option<f64>,
    #[serde(default)]
    pub rewritten_bytes: u64,
    /// Every attempted check passed (unsupported/N/A excluded, but a
    /// refused rewrite is *not* a full round trip).
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub total_ms: u128,
}

/// Document features relevant to round-trip coverage (Task G): which
/// files exercise which preservation path.
pub fn features(doc: &Document, bytes: &[u8]) -> Vec<String> {
    let mut f = Vec::new();
    let cat = doc.catalog();
    if doc.pages().len() > 1 {
        f.push("multipage".into());
    }
    if cat.contains_key(b"AcroForm".as_slice()) {
        f.push("acroform".into());
    }
    if cat.contains_key(b"Names".as_slice()) || cat.contains_key(b"Dests".as_slice()) {
        f.push("dests".into());
    }
    if cat.contains_key(b"Outlines".as_slice()) {
        f.push("outlines".into());
    }
    if cat.contains_key(b"OCProperties".as_slice()) {
        f.push("optional-content".into());
    }
    if cat.contains_key(b"Metadata".as_slice()) || doc.trailer().contains_key(b"Info".as_slice()) {
        f.push("metadata".into());
    }
    if contains(bytes, b"/ByteRange") {
        f.push("signed".into());
    }
    if count(bytes, b"startxref") > 1 || contains(bytes, b"/Prev") {
        f.push("incremental".into());
    }
    if contains(bytes, b"/EmbeddedFiles") {
        f.push("embedded-files".into());
    }
    let mut annots = false;
    let mut images = false;
    let mut transparency = false;
    let mut emb_fonts = false;
    let mut nonemb_fonts = false;
    for page in doc.pages() {
        let Some(pref) = page.object_ref() else {
            continue;
        };
        let Ok(pd) = doc.resolve(pref) else { continue };
        let Some(pd) = pd.as_dict() else { continue };
        if pd.contains_key(b"Annots".as_slice()) {
            annots = true;
        }
        if pd.contains_key(b"Group".as_slice()) {
            transparency = true;
        }
        let Some(res) = page.resources().and_then(|r| doc.deref(r).ok()) else {
            continue;
        };
        let Some(rd) = res.as_dict() else { continue };
        if let Some(Ok(xo)) = rd.get(b"XObject".as_slice()).map(|o| doc.deref(o)) {
            if let Some(xd) = xo.as_dict() {
                for v in xd.values() {
                    if let Ok(x) = doc.deref(v) {
                        if let Some((d, _)) = x.as_stream() {
                            match d.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
                                Some(b"Image") => images = true,
                                Some(b"Form") if d.contains_key(b"Group".as_slice()) => {
                                    transparency = true
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        if let Some(Ok(gs)) = rd.get(b"ExtGState".as_slice()).map(|o| doc.deref(o)) {
            if let Some(gd) = gs.as_dict() {
                for v in gd.values() {
                    if let Ok(Object::Dict(g)) = doc.deref(v) {
                        if g.contains_key(b"SMask".as_slice())
                            || g.contains_key(b"ca".as_slice())
                            || g.contains_key(b"CA".as_slice())
                            || g.contains_key(b"BM".as_slice())
                        {
                            transparency = true;
                        }
                    }
                }
            }
        }
        if let Some(Ok(fonts)) = rd.get(b"Font".as_slice()).map(|o| doc.deref(o)) {
            if let Some(fd) = fonts.as_dict() {
                for v in fd.values() {
                    if let Ok(Object::Dict(f)) = doc.deref(v) {
                        if font_embedded(doc, &f) {
                            emb_fonts = true;
                        } else {
                            nonemb_fonts = true;
                        }
                    }
                }
            }
        }
    }
    for (flag, name) in [
        (annots, "annots"),
        (images, "images"),
        (transparency, "transparency"),
        (emb_fonts, "embedded-fonts"),
        (nonemb_fonts, "nonembedded-fonts"),
    ] {
        if flag {
            f.push(name.into());
        }
    }
    f
}

fn font_embedded(doc: &Document, f: &kasane_pdf::Dict) -> bool {
    let desc = |d: &kasane_pdf::Dict| -> bool {
        d.get(b"FontDescriptor".as_slice())
            .and_then(|o| doc.deref(o).ok())
            .and_then(|o| o.as_dict().cloned())
            .map(|fd| {
                fd.contains_key(b"FontFile".as_slice())
                    || fd.contains_key(b"FontFile2".as_slice())
                    || fd.contains_key(b"FontFile3".as_slice())
            })
            .unwrap_or(false)
    };
    if f.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) == Some(b"Type0") {
        if let Some(Ok(Object::Array(df))) =
            f.get(b"DescendantFonts".as_slice()).map(|o| doc.deref(o))
        {
            return df
                .first()
                .and_then(|o| doc.deref(o).ok())
                .and_then(|o| o.as_dict().cloned())
                .map(|d| desc(&d))
                .unwrap_or(false);
        }
        return false;
    }
    if f.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) == Some(b"Type3") {
        return true; // glyph programs are in the file
    }
    desc(f)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| w == &needle).count()
}

// ------------------------------------------------------------------
// external processes
// ------------------------------------------------------------------

/// Seconds an external tool (pdftoppm, qpdf) may take on one file —
/// a rewritten decompression bomb must not stall the whole run.
pub const EXTERNAL_TIMEOUT_SECS: u64 = 20;

/// Run `cmd`, killing it after [`EXTERNAL_TIMEOUT_SECS`]; `None` on
/// spawn failure or timeout.
pub fn run_bounded(cmd: &mut Command) -> Option<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    // Drain the pipes on helper threads so a chatty child cannot block.
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(EXTERNAL_TIMEOUT_SECS);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Some(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}

// ------------------------------------------------------------------
// driver
// ------------------------------------------------------------------

/// Limits on the comparison work per file.
const MAX_DIFFS: usize = 40;
const MAX_GRAPH_NODES: usize = 400_000;
const MAX_GRAPH_DEPTH: usize = 96;

/// The round trip of a document whose rewrite was refused: nothing
/// else runs; a refusal is *unsupported* (a profile limit), any other
/// error a failure.
pub fn refused(e: &Error, ms: u128) -> RoundTrip {
    let class = match e {
        Error::Unsupported { .. } | Error::SignaturePolicy { .. } | Error::Encrypted => {
            RtClass::Unsupported
        }
        _ => RtClass::Fail,
    };
    RoundTrip {
        rewrite: RtCheck {
            class,
            notes: vec![format!("{e}")],
            ms,
        },
        total_ms: ms,
        ..Default::default()
    }
}

/// Run the pipeline after a successful `rewrite` of the strictly
/// parsed `doc` (original bytes `orig`, rewritten bytes `out`).
/// `with_poppler` adds the Poppler-vs-Poppler witness; renders are
/// compared in memory, nothing is written.
pub fn run(
    doc: &Document,
    orig: &[u8],
    out: &[u8],
    rewrite_ms: u128,
    opts: &RtOptions,
    tag: &str,
) -> RoundTrip {
    let start = Instant::now();
    let mut rt = RoundTrip {
        rewrite: RtCheck::pass(rewrite_ms),
        ..Default::default()
    };
    rt.rewritten_bytes = out.len() as u64;
    rt.size_ratio = Some(out.len() as f64 / orig.len().max(1) as f64);

    let t = Instant::now();
    let doc2 = match Document::from_bytes(out) {
        Ok(d) => {
            rt.reparse = RtCheck::pass(t.elapsed().as_millis());
            d
        }
        Err(e) => {
            rt.reparse = RtCheck::fail(vec![format!("{e}")], t.elapsed().as_millis());
            rt.qpdf = qpdf(out, tag);
            rt.qpdf_original = qpdf(orig, &format!("{tag}-orig"));
            external_checks(&mut rt);
            rt.total_ms = start.elapsed().as_millis();
            return rt;
        }
    };

    let t = Instant::now();
    let diffs = graph_diff(doc, &doc2);
    // The render and text comparisons must open the rewrite the way the
    // original was opened: a document that renders only under the
    // recovery rules is compared against its rewrite under the same
    // rules (the structural checks above stay strict — the rewrite must
    // always parse strictly).
    let doc2 = if doc.recovery_mode() {
        Document::from_bytes_recovering(out).unwrap_or(doc2)
    } else {
        doc2
    };
    rt.structure = if diffs.is_empty() {
        RtCheck::pass(t.elapsed().as_millis())
    } else {
        RtCheck::fail(diffs, t.elapsed().as_millis())
    };

    let t = Instant::now();
    let (render, pages, stats) = compare_render(doc, &doc2, opts);
    rt.render = RtCheck {
        ms: t.elapsed().as_millis(),
        ..render
    };
    rt.render_pages = pages;
    rt.pages = stats;
    if opts.poppler {
        rt.poppler = poppler_witness(orig, out, tag, opts);
        rt.poppler_ssim = rt.poppler.as_ref().and_then(|p| p.mean_ssim);
    }

    let t = Instant::now();
    let (text, norm) = compare_text(doc, &doc2);
    rt.text = RtCheck {
        ms: t.elapsed().as_millis(),
        ..text
    };
    rt.text_normalized_ratio = norm;

    let t = Instant::now();
    rt.annotations = RtCheck {
        ms: t.elapsed().as_millis(),
        ..compare_annotations(doc, &doc2)
    };

    rt.qpdf = qpdf(out, tag);
    rt.qpdf_original = qpdf(orig, &format!("{tag}-orig"));
    external_checks(&mut rt);
    rt.full = [
        &rt.rewrite,
        &rt.reparse,
        &rt.structure,
        &rt.render,
        &rt.text,
        &rt.annotations,
    ]
    .iter()
    .all(|c| matches!(c.class, RtClass::Pass | RtClass::NotApplicable))
        && rt.rewrite.class == RtClass::Pass
        && rt.pages.skipped_pages == 0
        && rt.external_no_new.class != RtClass::Fail
        && rt
            .poppler
            .as_ref()
            .map(|p| p.first_divergent_page.is_none() && p.size_mismatch_pages == 0)
            .unwrap_or(true);
    rt.total_ms = start.elapsed().as_millis();
    rt
}

// ------------------------------------------------------------------
// object-graph comparison
// ------------------------------------------------------------------

struct GraphWalk<'a> {
    a: &'a Document,
    b: &'a Document,
    /// old ref → new ref, fixed the first time a pair is seen.
    map: HashMap<IndirectRef, IndirectRef>,
    /// Refs whose bodies have been (or are being) compared.
    done: HashSet<IndirectRef>,
    /// Reference pairs still to compare, with the path they were
    /// reached by — the walk is iterative over references so the
    /// recursion depth is bounded by *direct* nesting only.
    queue: Vec<(IndirectRef, IndirectRef, String)>,
    diffs: Vec<String>,
    nodes: usize,
}

pub fn graph_diff(a: &Document, b: &Document) -> Vec<String> {
    let mut w = GraphWalk {
        a,
        b,
        map: HashMap::new(),
        done: HashSet::new(),
        queue: Vec::new(),
        diffs: Vec::new(),
        nodes: 0,
    };
    w.cmp(
        &Object::Dict(a.catalog().clone()),
        &Object::Dict(b.catalog().clone()),
        "/Root",
        0,
    );
    match (
        a.trailer().get(b"Info".as_slice()),
        b.trailer().get(b"Info".as_slice()),
    ) {
        (Some(ia), Some(ib)) => w.cmp(ia, ib, "/Info", 0),
        (None, None) => {}
        (Some(_), None) => w.note("/Info: dropped by the rewrite".into()),
        (None, Some(_)) => w.note("/Info: added by the rewrite".into()),
    }
    while let Some((ra, rb, path)) = w.queue.pop() {
        if w.diffs.len() >= MAX_DIFFS {
            break;
        }
        let va = w.a.resolve(ra);
        let vb = w.b.resolve(rb);
        match (va, vb) {
            (Ok(va), Ok(vb)) => w.cmp(&va, &vb, &path, 0),
            (Err(_), Err(_)) => {} // dangling on both sides
            (Err(e), Ok(_)) => w.note(format!(
                "{path}: {ra} unresolvable before ({e}), present after"
            )),
            (Ok(_), Err(e)) => w.note(format!("{path}: {rb} unresolvable after the rewrite ({e})")),
        }
    }
    if a.pages().len() != b.pages().len() {
        w.note(format!(
            "page count {} → {}",
            a.pages().len(),
            b.pages().len()
        ));
    }
    w.diffs
}

impl GraphWalk<'_> {
    fn note(&mut self, s: String) {
        if self.diffs.len() < MAX_DIFFS {
            // Paths through deep resource chains get long; keep the
            // tail, which names the differing key.
            let s = if s.chars().count() > 240 {
                let tail: String = s
                    .chars()
                    .rev()
                    .take(236)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("…{tail}")
            } else {
                s
            };
            self.diffs.push(s);
        }
    }

    fn cmp(&mut self, oa: &Object, ob: &Object, path: &str, depth: usize) {
        if self.diffs.len() >= MAX_DIFFS {
            return;
        }
        self.nodes += 1;
        if self.nodes > MAX_GRAPH_NODES {
            if self.nodes == MAX_GRAPH_NODES + 1 {
                self.note(format!(
                    "{path}: comparison stopped after {MAX_GRAPH_NODES} nodes"
                ));
            }
            return;
        }
        if depth > MAX_GRAPH_DEPTH {
            self.note(format!("{path}: nesting deeper than {MAX_GRAPH_DEPTH}"));
            return;
        }
        match (oa, ob) {
            (Object::Ref(ra), Object::Ref(rb)) => {
                match self.map.get(ra) {
                    Some(prev) if prev != rb => {
                        self.note(format!(
                            "{path}: {ra} was renumbered to {prev} elsewhere but to {rb} here"
                        ));
                        return;
                    }
                    Some(_) => return, // already compared (or in progress)
                    None => {
                        self.map.insert(*ra, *rb);
                    }
                }
                if !self.done.insert(*ra) {
                    return;
                }
                self.queue.push((*ra, *rb, path.to_string()));
            }
            (Object::Ref(ra), direct) => {
                // The rewrite never inlines; a ref that became direct is
                // still compared by value.
                match self.a.resolve(*ra) {
                    Ok(va) => self.cmp(&va, direct, path, depth + 1),
                    Err(e) => self.note(format!("{path}: {ra} unresolvable before ({e})")),
                }
            }
            (direct, Object::Ref(rb)) => match self.b.resolve(*rb) {
                Ok(vb) => self.cmp(direct, &vb, path, depth + 1),
                Err(e) => self.note(format!("{path}: {rb} unresolvable after ({e})")),
            },
            (Object::Dict(da), Object::Dict(db)) => self.cmp_dict(da, db, path, depth),
            (Object::Stream { dict: da, data: xa }, Object::Stream { dict: db, data: xb }) => {
                self.cmp_dict(da, db, path, depth);
                if xa != xb {
                    self.note(format!(
                        "{path}: stream bytes differ ({} → {} bytes)",
                        xa.len(),
                        xb.len()
                    ));
                }
            }
            (Object::Array(aa), Object::Array(ab)) => {
                if aa.len() != ab.len() {
                    self.note(format!("{path}: array length {} → {}", aa.len(), ab.len()));
                    return;
                }
                for (i, (x, y)) in aa.iter().zip(ab).enumerate() {
                    self.cmp(x, y, &format!("{path}[{i}]"), depth + 1);
                }
            }
            (Object::Integer(x), Object::Integer(y)) if x == y => {}
            (Object::Integer(_) | Object::Real(_), Object::Integer(_) | Object::Real(_)) => {
                let x = oa.as_number().unwrap_or(f64::NAN);
                let y = ob.as_number().unwrap_or(f64::NAN);
                if (x - y).abs() > 1e-6 * x.abs().max(1.0) || x.is_nan() != y.is_nan() {
                    self.note(format!("{path}: number {x} → {y}"));
                }
            }
            (Object::Null, Object::Null) => {}
            (Object::Bool(x), Object::Bool(y)) if x == y => {}
            (Object::Name(x), Object::Name(y)) if x == y => {}
            (Object::String(x), Object::String(y)) if x == y => {}
            _ => self.note(format!("{path}: {} → {}", describe(oa), describe(ob))),
        }
    }

    fn cmp_dict(&mut self, da: &kasane_pdf::Dict, db: &kasane_pdf::Dict, path: &str, depth: usize) {
        for (k, va) in da {
            let key = String::from_utf8_lossy(k);
            // /Length is bookkeeping the writer recomputes.
            if k.as_slice() == b"Length" {
                continue;
            }
            match db.get(k) {
                None => self.note(format!("{path}/{key}: dropped by the rewrite")),
                Some(vb) => self.cmp(va, vb, &format!("{path}/{key}"), depth + 1),
            }
        }
        for k in db.keys() {
            if !da.contains_key(k) && k.as_slice() != b"Length" {
                self.note(format!(
                    "{path}/{}: added by the rewrite",
                    String::from_utf8_lossy(k)
                ));
            }
        }
    }
}

fn describe(o: &Object) -> String {
    match o {
        Object::Null => "null".into(),
        Object::Bool(b) => format!("{b}"),
        Object::Integer(i) => format!("{i}"),
        Object::Real(r) => format!("{r}"),
        Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
        Object::String(s) => format!("({} bytes)", s.len()),
        Object::Array(a) => format!("array[{}]", a.len()),
        Object::Dict(d) => format!("dict[{}]", d.len()),
        Object::Stream { data, .. } => format!("stream[{} bytes]", data.len()),
        Object::Ref(r) => format!("{r}"),
    }
}

// ------------------------------------------------------------------
// render comparison (kasane vs kasane)
// ------------------------------------------------------------------

fn compare_render(
    a: &Document,
    b: &Document,
    opts: &RtOptions,
) -> (RtCheck, Vec<RtRenderPage>, RtPageStats) {
    let ropts = RenderOptions::default().with_dpi(opts.dpi);
    let total = a.pages().len().min(b.pages().len());
    let n = opts.max_pages.map(|m| m.min(total)).unwrap_or(total);
    let mut pages = Vec::new();
    let mut notes = Vec::new();
    let mut stats = RtPageStats {
        total_pages: a.pages().len(),
        ..Default::default()
    };
    let start = Instant::now();
    let mut ssims = Vec::new();
    for i in 0..n {
        if start.elapsed().as_millis() > opts.budget_ms {
            stats.skipped_pages = n - i;
            notes.push(format!(
                "pages {}..{} not compared: {} ms budget exhausted",
                i + 1,
                n,
                opts.budget_ms
            ));
            break;
        }
        let ra = match render_page(a, &a.pages()[i], &ropts) {
            Ok(r) => r,
            Err(_) => {
                stats.unsupported_pages += 1;
                continue; // our rendering of the original fails: nothing to compare
            }
        };
        stats.compared_pages += 1;
        let rb = match render_page(b, &b.pages()[i], &ropts) {
            Ok(r) => r,
            Err(e) => {
                notes.push(format!(
                    "page {}: rewritten file does not render: {e}",
                    i + 1
                ));
                stats.failed_pages += 1;
                stats.first_divergent_page.get_or_insert(i + 1);
                pages.push(RtRenderPage {
                    page: i + 1,
                    size_equal: false,
                    mae: 1.0,
                    ssim: 0.0,
                    diff_px_ratio: 1.0,
                });
                ssims.push(0.0);
                continue;
            }
        };
        let size_equal = ra.pixmap().width() == rb.pixmap().width()
            && ra.pixmap().height() == rb.pixmap().height();
        if !size_equal {
            notes.push(format!(
                "page {}: size {}x{} → {}x{}",
                i + 1,
                ra.pixmap().width(),
                ra.pixmap().height(),
                rb.pixmap().width(),
                rb.pixmap().height()
            ));
            stats.failed_pages += 1;
            stats.first_divergent_page.get_or_insert(i + 1);
            pages.push(RtRenderPage {
                page: i + 1,
                size_equal,
                mae: 1.0,
                ssim: 0.0,
                diff_px_ratio: 1.0,
            });
            ssims.push(0.0);
            continue;
        }
        let la = crate::pixmap_luma(&ra);
        let lb = crate::pixmap_luma(&rb);
        let (w, h) = (ra.pixmap().width() as usize, ra.pixmap().height() as usize);
        let mae = la
            .iter()
            .zip(&lb)
            .map(|(x, y)| (*x as f64 - *y as f64).abs())
            .sum::<f64>()
            / (la.len().max(1) as f64 * 255.0);
        let ssim = crate::ssim_gray(&la, &lb, w, h);
        let diff = crate::diff_px_tolerant(&la, &lb, w, h);
        ssims.push(ssim);
        if diff > 0.0005 || ssim < 0.995 {
            notes.push(format!(
                "page {}: diff-px {diff:.4}, SSIM {ssim:.4}, MAE {mae:.4}",
                i + 1
            ));
            stats.failed_pages += 1;
            stats.first_divergent_page.get_or_insert(i + 1);
        }
        pages.push(RtRenderPage {
            page: i + 1,
            size_equal,
            mae,
            ssim,
            diff_px_ratio: diff,
        });
    }
    if !ssims.is_empty() {
        stats.worst_ssim = ssims.iter().cloned().reduce(f64::min);
        stats.mean_ssim = Some(ssims.iter().sum::<f64>() / ssims.len() as f64);
    }
    let partial = |notes: Vec<String>| RtCheck {
        class: RtClass::Partial,
        notes,
        ms: 0,
    };
    if stats.compared_pages == 0 {
        let check = if stats.skipped_pages > 0 {
            partial(notes)
        } else {
            RtCheck::na("the original does not render (nothing to compare)")
        };
        return (check, pages, stats);
    }
    let check = if stats.failed_pages > 0 {
        RtCheck::fail(notes, 0)
    } else if stats.skipped_pages > 0 {
        partial(notes)
    } else {
        RtCheck::pass(0)
    };
    (check, pages, stats)
}

/// `(width, height, luma)` of one page rendered by pdftoppm.
type GrayPage = (usize, usize, Vec<u8>);

/// Poppler renders the compared pages of both files; per-page SSIM /
/// MAE / diff-px between the two is the independent witness, and the
/// `Syntax Error`/`Syntax Warning` lines it prints are counted for
/// each file (`None` when Poppler is unavailable).
fn poppler_witness(orig: &[u8], out: &[u8], tag: &str, opts: &RtOptions) -> Option<RtPoppler> {
    let dir = std::env::temp_dir().join(format!("kasane-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let pa = dir.join(format!("{tag}-a.pdf"));
    let pb = dir.join(format!("{tag}-b.pdf"));
    std::fs::write(&pa, orig).ok()?;
    std::fs::write(&pb, out).ok()?;
    let dpi = opts.dpi.clamp(12.0, 48.0) as u32;
    let render = |p: &Path, stem: &str| -> Option<(Vec<GrayPage>, usize)> {
        let prefix = dir.join(stem);
        let mut cmd = Command::new("pdftoppm");
        if let Some(n) = opts.poppler_pages {
            cmd.args(["-f", "1", "-l", &n.to_string()]);
        }
        cmd.args(["-r", &dpi.to_string(), "-gray", "-cropbox"])
            .arg(p)
            .arg(&prefix);
        let st = run_bounded(&mut cmd)?;
        let diag = String::from_utf8_lossy(&st.stderr)
            .lines()
            .filter(|l| l.contains("Syntax Error") || l.contains("Syntax Warning"))
            .count();
        let needle = format!("{stem}-");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|f| {
                f.extension().map(|x| x == "pgm").unwrap_or(false)
                    && f.file_name()
                        .map(|n| n.to_string_lossy().starts_with(&needle))
                        .unwrap_or(false)
            })
            .collect();
        files.sort();
        let mut pages = Vec::new();
        for f in files {
            if let Some(p) = std::fs::read(&f).ok().and_then(|b| crate::parse_pgm(&b)) {
                pages.push(p);
            }
            let _ = std::fs::remove_file(&f);
        }
        Some((pages, diag))
    };
    let ra = render(&pa, &format!("{tag}-a"));
    let rb = render(&pb, &format!("{tag}-b"));
    let _ = std::fs::remove_file(&pa);
    let _ = std::fs::remove_file(&pb);
    let (pages_a, diag_a) = ra?;
    let (pages_b, diag_b) = rb?;
    let mut res = RtPoppler {
        diagnostics_original: diag_a,
        diagnostics_rewritten: diag_b,
        ..Default::default()
    };
    if pages_a.len() != pages_b.len() {
        res.size_mismatch_pages = pages_a.len().abs_diff(pages_b.len());
        res.first_divergent_page = Some(pages_a.len().min(pages_b.len()) + 1);
    }
    let mut ssims = Vec::new();
    let mut maes = Vec::new();
    for (i, ((wa, ha, la), (wb, hb, lb))) in pages_a.iter().zip(&pages_b).enumerate() {
        res.pages_compared += 1;
        if wa != wb || ha != hb {
            res.size_mismatch_pages += 1;
            res.first_divergent_page.get_or_insert(i + 1);
            ssims.push(0.0);
            continue;
        }
        let ssim = crate::ssim_gray(la, lb, *wa, *ha);
        let mae = la
            .iter()
            .zip(lb)
            .map(|(x, y)| (*x as f64 - *y as f64).abs())
            .sum::<f64>()
            / (la.len().max(1) as f64 * 255.0);
        let diff = crate::diff_px_tolerant(la, lb, *wa, *ha);
        ssims.push(ssim);
        maes.push(mae);
        res.worst_diff_px = Some(res.worst_diff_px.unwrap_or(0.0).max(diff));
        if ssim < 0.99 || diff > 0.001 {
            res.first_divergent_page.get_or_insert(i + 1);
        }
    }
    if !ssims.is_empty() {
        res.worst_ssim = ssims.iter().cloned().reduce(f64::min);
        res.mean_ssim = Some(ssims.iter().sum::<f64>() / ssims.len() as f64);
    }
    if !maes.is_empty() {
        res.mean_mae = Some(maes.iter().sum::<f64>() / maes.len() as f64);
    }
    Some(res)
}

/// Derive the two external judgements from the qpdf results on both
/// files and the Poppler diagnostics: *valid* = the rewritten file is
/// clean for qpdf; *no new* = it has no warning/error the original did
/// not have (counts compared), nor more Poppler syntax diagnostics.
fn external_checks(rt: &mut RoundTrip) {
    let Some(q) = &rt.qpdf else {
        rt.external_valid = RtCheck::na("qpdf unavailable");
        rt.external_no_new = RtCheck::na("qpdf unavailable");
        return;
    };
    rt.external_valid = if q.exit_code == 0 && q.warnings == 0 && q.errors == 0 {
        RtCheck::pass(0)
    } else {
        RtCheck::fail(
            std::iter::once(format!(
                "qpdf exit {}, {} warning(s), {} error(s)",
                q.exit_code, q.warnings, q.errors
            ))
            .chain(q.lines.iter().cloned())
            .collect(),
            0,
        )
    };
    let mut notes = Vec::new();
    let mut new_errors = false;
    if let Some(o) = &rt.qpdf_original {
        if q.errors > o.errors {
            notes.push(format!("qpdf errors {} → {}", o.errors, q.errors));
            new_errors = true;
        }
        if q.warnings > o.warnings {
            notes.push(format!("qpdf warnings {} → {}", o.warnings, q.warnings));
            new_errors = true;
        }
        if !new_errors && (o.errors > 0 || o.warnings > 0) {
            notes.push(format!(
                "inherited from the input: {} warning(s), {} error(s)",
                o.warnings, o.errors
            ));
        }
    }
    if let Some(p) = &rt.poppler {
        if p.diagnostics_rewritten > p.diagnostics_original {
            notes.push(format!(
                "Poppler syntax diagnostics {} → {}",
                p.diagnostics_original, p.diagnostics_rewritten
            ));
            new_errors = true;
        }
    }
    rt.external_no_new = if new_errors {
        RtCheck::fail(notes, 0)
    } else {
        RtCheck {
            class: RtClass::Pass,
            notes,
            ms: 0,
        }
    };
}

// ------------------------------------------------------------------
// text comparison
// ------------------------------------------------------------------

fn compare_text(a: &Document, b: &Document) -> (RtCheck, Option<f64>) {
    let limits = Limits::default();
    let n = a.pages().len().min(b.pages().len());
    let mut notes = Vec::new();
    let mut attempted = 0;
    let mut norm_match = 0usize;
    let mut norm_total = 0usize;
    for i in 0..n {
        let ta = match extract_text(
            a,
            &a.pages()[i],
            &TextOptions::default().with_limits(limits.clone()),
        ) {
            Ok(t) => t,
            Err(_) => continue, // extraction fails on the original: not comparable
        };
        attempted += 1;
        let tb = match extract_text(
            b,
            &b.pages()[i],
            &TextOptions::default().with_limits(limits.clone()),
        ) {
            Ok(t) => t,
            Err(e) => {
                notes.push(format!(
                    "page {}: extraction fails after the rewrite: {e}",
                    i + 1
                ));
                norm_total += 1;
                continue;
            }
        };
        let sa = joined(&ta.chars);
        let sb = joined(&tb.chars);
        norm_total += 1;
        if crate::normalize(&sa) == crate::normalize(&sb) {
            norm_match += 1;
        }
        if ta.chars.len() != tb.chars.len() {
            notes.push(format!(
                "page {}: {} → {} characters",
                i + 1,
                ta.chars.len(),
                tb.chars.len()
            ));
            continue;
        }
        if sa != sb {
            notes.push(format!("page {}: text differs", i + 1));
            continue;
        }
        let mut moved = 0usize;
        let mut via = 0usize;
        for (ca, cb) in ta.chars.iter().zip(&tb.chars) {
            if (ca.origin.0 - cb.origin.0).abs() > 1e-3
                || (ca.origin.1 - cb.origin.1).abs() > 1e-3
                || (ca.advance.0 - cb.advance.0).abs() > 1e-3
                || (ca.advance.1 - cb.advance.1).abs() > 1e-3
            {
                moved += 1;
            }
            if ca.resolved_via != cb.resolved_via || ca.code != cb.code {
                via += 1;
            }
        }
        if moved > 0 {
            notes.push(format!("page {}: {moved} characters moved", i + 1));
        }
        if via > 0 {
            notes.push(format!(
                "page {}: {via} characters resolve differently (code or ActualText)",
                i + 1
            ));
        }
    }
    let norm = (norm_total > 0).then(|| norm_match as f64 / norm_total as f64);
    if attempted == 0 {
        return (
            RtCheck::na("no page's text extracts from the original"),
            None,
        );
    }
    let check = if notes.is_empty() {
        RtCheck::pass(0)
    } else {
        RtCheck::fail(notes, 0)
    };
    (check, norm)
}

fn joined(chars: &[kasane_pdf::CharInfo]) -> String {
    chars
        .iter()
        .map(|c| c.unicode.as_deref().unwrap_or("\u{FFFD}"))
        .collect()
}

// ------------------------------------------------------------------
// annotations / forms
// ------------------------------------------------------------------

/// A canonical, renumbering-independent description of one annotation.
fn annot_summary(doc: &Document, o: &Object) -> String {
    let Ok(a) = doc.deref(o) else {
        return "<unresolvable>".into();
    };
    let Some(d) = a.as_dict() else {
        return describe(&a);
    };
    let name = |k: &[u8]| {
        d.get(k)
            .and_then(|o| doc.deref(o).ok())
            .map(|o| canon(doc, &o, 0))
            .unwrap_or_default()
    };
    let ap = match d.get(b"AP".as_slice()).and_then(|o| doc.deref(o).ok()) {
        Some(Object::Dict(ap)) => match ap.get(b"N".as_slice()).and_then(|o| doc.deref(o).ok()) {
            Some(Object::Stream { data, .. }) => format!("N:stream[{}]", data.len()),
            Some(Object::Dict(states)) => {
                let mut ks: Vec<String> = states
                    .keys()
                    .map(|k| String::from_utf8_lossy(k).into_owned())
                    .collect();
                ks.sort();
                format!("N:states[{}]", ks.join(","))
            }
            _ => "N:none".into(),
        },
        _ => "AP:none".into(),
    };
    format!(
        "Subtype={} Rect={} F={} Contents={} AS={} FT={} T={} V={} {ap}",
        name(b"Subtype"),
        name(b"Rect"),
        name(b"F"),
        name(b"Contents"),
        name(b"AS"),
        name(b"FT"),
        name(b"T"),
        name(b"V"),
    )
}

/// Canonical text of a direct-ish object (refs resolved, depth-bounded).
fn canon(doc: &Document, o: &Object, depth: usize) -> String {
    if depth > 8 {
        return "…".into();
    }
    match o {
        Object::Ref(r) => match doc.resolve(*r) {
            Ok(v) => canon(doc, &v, depth + 1),
            Err(_) => "<dangling>".into(),
        },
        Object::Array(a) => format!(
            "[{}]",
            a.iter()
                .map(|x| canon(doc, x, depth + 1))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Object::Dict(d) => format!(
            "<<{}>>",
            d.iter()
                .filter(|(k, _)| k.as_slice() != b"Parent" && k.as_slice() != b"P")
                .map(|(k, v)| format!(
                    "/{} {}",
                    String::from_utf8_lossy(k),
                    canon(doc, v, depth + 1)
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        Object::Real(r) => format!("{:.4}", r),
        Object::String(s) => format!("({})", String::from_utf8_lossy(s)),
        other => describe(other),
    }
}

/// Field full names (T chain) with type and value, depth-first.
fn fields(doc: &Document, out: &mut Vec<String>, arr: &[Object], prefix: &str, depth: usize) {
    if depth > 32 || out.len() > 4096 {
        return;
    }
    for f in arr {
        let Ok(fd) = doc.deref(f) else { continue };
        let Some(d) = fd.as_dict() else { continue };
        let t = d
            .get(b"T".as_slice())
            .and_then(|o| o.as_string())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default();
        let full = if prefix.is_empty() {
            t
        } else if t.is_empty() {
            prefix.to_string()
        } else {
            format!("{prefix}.{t}")
        };
        let get = |k: &[u8]| d.get(k).map(|o| canon(doc, o, 0)).unwrap_or_default();
        out.push(format!(
            "{full} FT={} V={} Ff={} DA={} Opt={}",
            get(b"FT"),
            get(b"V"),
            get(b"Ff"),
            get(b"DA"),
            get(b"Opt")
        ));
        if let Some(Ok(Object::Array(kids))) = d.get(b"Kids".as_slice()).map(|o| doc.deref(o)) {
            fields(doc, out, &kids, &full, depth + 1);
        }
    }
}

fn compare_annotations(a: &Document, b: &Document) -> RtCheck {
    let mut notes = Vec::new();
    let mut any = false;
    let n = a.pages().len().min(b.pages().len());
    for i in 0..n {
        let list = |doc: &Document, pg: &kasane_pdf::Page| -> Vec<String> {
            let Some(pref) = pg.object_ref() else {
                return Vec::new();
            };
            let Ok(pd) = doc.resolve(pref) else {
                return Vec::new();
            };
            let Some(pd) = pd.as_dict() else {
                return Vec::new();
            };
            let Some(an) = pd.get(b"Annots".as_slice()).and_then(|o| doc.deref(o).ok()) else {
                return Vec::new();
            };
            an.as_array()
                .map(|arr| arr.iter().map(|o| annot_summary(doc, o)).collect())
                .unwrap_or_default()
        };
        let la = list(a, &a.pages()[i]);
        let lb = list(b, &b.pages()[i]);
        if !la.is_empty() || !lb.is_empty() {
            any = true;
        }
        if la.len() != lb.len() {
            notes.push(format!(
                "page {}: {} → {} annotations",
                i + 1,
                la.len(),
                lb.len()
            ));
            continue;
        }
        for (k, (x, y)) in la.iter().zip(&lb).enumerate() {
            if x != y && notes.len() < MAX_DIFFS {
                notes.push(format!("page {} annotation {k}: {x} → {y}", i + 1));
            }
        }
    }
    let form = |doc: &Document| -> Option<Vec<String>> {
        let af = doc.deref(doc.catalog().get(b"AcroForm".as_slice())?).ok()?;
        let d = af.as_dict()?;
        let mut out = Vec::new();
        if let Some(Ok(Object::Array(arr))) = d.get(b"Fields".as_slice()).map(|o| doc.deref(o)) {
            fields(doc, &mut out, &arr, "", 0);
        }
        out.push(format!(
            "NeedAppearances={} DA={} Q={}",
            d.get(b"NeedAppearances".as_slice())
                .map(|o| canon(doc, o, 0))
                .unwrap_or_default(),
            d.get(b"DA".as_slice())
                .map(|o| canon(doc, o, 0))
                .unwrap_or_default(),
            d.get(b"Q".as_slice())
                .map(|o| canon(doc, o, 0))
                .unwrap_or_default(),
        ));
        Some(out)
    };
    match (form(a), form(b)) {
        (Some(fa), Some(fb)) => {
            any = true;
            if fa.len() != fb.len() {
                notes.push(format!(
                    "AcroForm: {} → {} fields",
                    fa.len() - 1,
                    fb.len() - 1
                ));
            } else {
                for (x, y) in fa.iter().zip(&fb) {
                    if x != y && notes.len() < MAX_DIFFS {
                        notes.push(format!("AcroForm field: {x} → {y}"));
                    }
                }
            }
        }
        (Some(_), None) => {
            any = true;
            notes.push("AcroForm dropped by the rewrite".into());
        }
        (None, Some(_)) => {
            any = true;
            notes.push("AcroForm added by the rewrite".into());
        }
        (None, None) => {}
    }
    // Named destinations resolve to the same pages.
    let limits = Limits::default();
    if let (Ok(da), Ok(db)) = (
        named_destinations(a, &limits),
        named_destinations(b, &limits),
    ) {
        if !da.is_empty() || !db.is_empty() {
            any = true;
        }
        let m = |v: Vec<kasane_pdf::NamedDest>| -> BTreeMap<Vec<u8>, Option<usize>> {
            v.into_iter().map(|d| (d.name, d.page)).collect()
        };
        let (ma, mb) = (m(da), m(db));
        if ma != mb {
            notes.push(format!(
                "named destinations differ ({} → {} names)",
                ma.len(),
                mb.len()
            ));
        }
    }
    if !any {
        return RtCheck::na("no annotations, form fields or named destinations");
    }
    if notes.is_empty() {
        RtCheck::pass(0)
    } else {
        RtCheck::fail(notes, 0)
    }
}

// ------------------------------------------------------------------
// external check
// ------------------------------------------------------------------

/// `qpdf --check` on the rewritten bytes. Exit 0 = clean, 3 =
/// warnings only, 2 = errors; the output is scanned so warnings and
/// errors are counted separately rather than collapsed into the code.
fn qpdf(out: &[u8], tag: &str) -> Option<QpdfCheck> {
    let tmp = std::env::temp_dir().join(format!("kasane-rt-{}-{tag}.pdf", std::process::id()));
    std::fs::write(&tmp, out).ok()?;
    let o = run_bounded(Command::new("qpdf").args(["--check", tmp.to_str().unwrap_or("")]));
    let _ = std::fs::remove_file(&tmp);
    let o = o?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let exit_code = o.status.code().unwrap_or(-1);
    let mut warnings = 0;
    let mut errors = 0;
    let mut lines = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        let low = t.to_ascii_lowercase();
        // qpdf's all-clear message mentions "errors" too.
        if t.is_empty()
            || low.starts_with("checking ")
            || low.starts_with("pdf version")
            || low.starts_with("file is not encrypted")
            || low.starts_with("file is not linearized")
            || low.starts_with("file is linearized")
            || low.contains("no syntax or stream encoding errors")
            || low.contains("errors that qpdf cannot detect")
        {
            continue;
        }
        if low.starts_with("warning") {
            warnings += 1;
        } else if exit_code == 2 || low.contains("error") {
            errors += 1;
        } else {
            continue;
        }
        if lines.len() < 5 {
            lines.push(t.to_string());
        }
    }
    if exit_code == 2 && errors == 0 {
        errors = 1;
    }
    Some(QpdfCheck {
        exit_code,
        warnings,
        errors,
        lines,
    })
}

// ------------------------------------------------------------------
// tallies
// ------------------------------------------------------------------

/// Counts for one check over a group of files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RtCounts {
    pub attempted: usize,
    pub pass: usize,
    pub fail: usize,
    pub unsupported: usize,
    pub not_applicable: usize,
    #[serde(default)]
    pub partial: usize,
}

impl RtCounts {
    fn add(&mut self, c: &RtCheck) {
        match c.class {
            RtClass::Pass => {
                self.attempted += 1;
                self.pass += 1;
            }
            RtClass::Fail => {
                self.attempted += 1;
                self.fail += 1;
            }
            RtClass::Partial => {
                self.attempted += 1;
                self.partial += 1;
            }
            RtClass::Unsupported => self.unsupported += 1,
            RtClass::NotApplicable => self.not_applicable += 1,
            RtClass::NotAttempted => {}
        }
    }
}

/// Round-trip tallies for a group; `files` is the denominator of
/// `full` (strictly parsed files in the group), every other row's
/// denominator is its own `attempted`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RtTally {
    pub group: String,
    pub files: usize,
    pub rewrite: RtCounts,
    pub reparse: RtCounts,
    pub structure: RtCounts,
    pub render: RtCounts,
    pub text: RtCounts,
    pub annotations: RtCounts,
    #[serde(default)]
    pub external_valid: RtCounts,
    #[serde(default)]
    pub external_no_new: RtCounts,
    /// Files whose render comparison left pages uncompared (budget).
    #[serde(default)]
    pub pages_skipped_files: usize,
    #[serde(default)]
    pub pages_compared: usize,
    #[serde(default)]
    pub poppler_files: usize,
    #[serde(default)]
    pub poppler_divergent: usize,
    pub qpdf_clean: usize,
    pub qpdf_warnings: usize,
    pub qpdf_errors: usize,
    pub full: usize,
    pub size_ratio_mean: f64,
    pub size_ratio_max: f64,
}

impl RtTally {
    pub fn of<'a>(group: &str, items: impl Iterator<Item = &'a RoundTrip>) -> RtTally {
        let mut t = RtTally {
            group: group.into(),
            ..Default::default()
        };
        let mut ratios = Vec::new();
        for rt in items {
            t.files += 1;
            t.rewrite.add(&rt.rewrite);
            t.reparse.add(&rt.reparse);
            t.structure.add(&rt.structure);
            t.render.add(&rt.render);
            t.text.add(&rt.text);
            t.annotations.add(&rt.annotations);
            t.external_valid.add(&rt.external_valid);
            t.external_no_new.add(&rt.external_no_new);
            if rt.pages.skipped_pages > 0 {
                t.pages_skipped_files += 1;
            }
            t.pages_compared += rt.pages.compared_pages;
            if let Some(p) = &rt.poppler {
                t.poppler_files += 1;
                if p.first_divergent_page.is_some() {
                    t.poppler_divergent += 1;
                }
            }
            if let Some(q) = &rt.qpdf {
                if q.errors > 0 || q.exit_code == 2 {
                    t.qpdf_errors += 1;
                } else if q.warnings > 0 || q.exit_code == 3 {
                    t.qpdf_warnings += 1;
                } else if q.exit_code == 0 {
                    t.qpdf_clean += 1;
                }
            }
            if rt.full {
                t.full += 1;
            }
            if let Some(r) = rt.size_ratio {
                ratios.push(r);
            }
        }
        if !ratios.is_empty() {
            t.size_ratio_mean = ratios.iter().sum::<f64>() / ratios.len() as f64;
            t.size_ratio_max = ratios.iter().cloned().fold(0.0, f64::max);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_separate_na_from_pass() {
        let mut c = RtCounts::default();
        c.add(&RtCheck::pass(0));
        c.add(&RtCheck::na("x"));
        c.add(&RtCheck::fail(vec![], 0));
        c.add(&RtCheck::skip());
        assert_eq!(
            (c.attempted, c.pass, c.fail, c.not_applicable),
            (2, 1, 1, 1)
        );
    }

    #[test]
    fn graph_diff_sees_through_renumbering_and_catches_changes() {
        let a = Document::from_bytes(&mini(
            b"<< /Type /Catalog /Pages 2 0 R /Lang (en) >>",
            b"(x)",
        ))
        .unwrap();
        let b = Document::from_bytes(&mini(
            b"<< /Type /Catalog /Pages 2 0 R /Lang (en) >>",
            b"(x)",
        ))
        .unwrap();
        assert!(graph_diff(&a, &b).is_empty());
        let c = Document::from_bytes(&mini(b"<< /Type /Catalog /Pages 2 0 R >>", b"(y)")).unwrap();
        let d = graph_diff(&a, &c);
        assert!(
            d.iter()
                .any(|s| s.contains("/Root/Lang") && s.contains("dropped")),
            "{d:?}"
        );
        assert!(d.iter().any(|s| s.contains("stream bytes differ")), "{d:?}");
    }

    fn mini(catalog: &[u8], content: &[u8]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offs = Vec::new();
        let objs: Vec<(u32, Vec<u8>)> = vec![
            (1, catalog.to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>".to_vec(),
            ),
            (4, {
                let mut s = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
                s.extend_from_slice(content);
                s.extend_from_slice(b"\nendstream");
                s
            }),
        ];
        for (n, b) in &objs {
            offs.push(out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(b);
            out.extend_from_slice(b"\nendobj\n");
        }
        let x = out.len();
        out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
        );
        out
    }
}
