//! Failure-cause aggregation and development-priority scoring.
//!
//! For each unsupported feature the analysis records three counts —
//! they are *not* the same thing:
//!
//! - **affected** — PDFs the probe found the feature on at all.
//! - **sole-blocker** — PDFs where that feature is the *only* known
//!   blocker (implementing it alone would unblock the file, assuming
//!   no hidden blockers downstream).
//! - **potentially-unblocked** — PDFs that could newly pass a stage
//!   *if* the feature is implemented; this is an **estimate**, because
//!   the probe only sees first-N pages and can't see inside Form
//!   XObjects — it is never presented as a guarantee.
//!
//! Priority = `unblocked / effort` where effort comes from a config
//! table (person-day estimates). Effort is a *guess*, marked as such
//! — never a measured value.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{Class, FileResult};

/// One row of the priority table.
#[derive(Debug, Serialize, Deserialize)]
pub struct FeatureStat {
    pub feature: String,
    /// Cause ids grouped under this feature.
    pub causes: Vec<String>,
    pub affected: usize,
    pub sole_blocker: usize,
    pub potentially_unblocked: usize,
    /// affected pages (sum of cause_pages across files).
    pub affected_pages: usize,
    /// effort estimate (person-days); None = unestimated.
    pub effort: Option<u32>,
    /// priority score = potentially_unblocked / effort. None when
    /// effort is unknown or zero.
    pub score: Option<f64>,
    /// files carrying this feature (ids), for drill-down.
    pub files: Vec<String>,
}

/// Map a cause id to a feature bucket. Buckets are the implementable
/// units of work — several fine-grained causes roll up to one feature
/// (e.g. all `*.font.*` font-family blockers → "embedded-fonts").
pub fn feature_bucket(cause: &str) -> &'static str {
    // Buckets chosen to match natural implementation units.
    if cause.contains("smask") || cause.contains("image.smask") || cause.contains("image.mask") {
        "Image masks / SMask"
    } else if cause.contains("blendmode")
        || cause.contains("transfer")
        || cause.contains("extgstate")
    {
        "ExtGState (gs)"
    } else if cause.contains("shading") {
        "Shading (sh)"
    } else if cause.contains("pattern") {
        "Pattern colours"
    } else if cause.contains("font.type1") {
        "Type1 fonts"
    } else if cause.contains("font.cff") || cause.contains("cid-subtype") {
        "CFF/CIDFontType0 fonts"
    } else if cause.contains("font.type3") {
        "Type3 fonts"
    } else if cause.contains("font.vertical") || cause.contains("type0-enc") {
        "Vertical/CJK encodings"
    } else if cause.contains("font") {
        "Fonts (other)"
    } else if cause.contains("annotation") {
        "Annotations (/Annots)"
    } else if cause.contains(".oc.") || cause.contains("xobject.oc") {
        "Optional Content (OC/OCMD)"
    } else if cause.contains("colorspace") {
        "Non-device colour spaces"
    } else if cause.contains("image.dct") || cause.contains("image.") {
        "Image filters/decode"
    } else if cause.contains("filter.jpx") {
        "JPXDecode (JPEG2000)"
    } else if cause.contains("filter.jbig2") {
        "JBIG2Decode"
    } else if cause.contains("filter") {
        "Other stream filters"
    } else if cause.contains("named-dest") {
        "Named destinations"
    } else if cause.contains("rewrite.profile") {
        "Rewrite profile (catalog entries)"
    } else if cause.contains("structure") {
        // Engine refusals on malformed-or-unexpected structure: each is
        // a bug or a strictness decision to review, not a feature.
        "Engine errors (Structure)"
    } else if cause.contains(".limit.") {
        "Resource limits"
    } else if cause.contains("userunit") {
        "/UserUnit ≠ 1"
    } else if cause.contains("actualtext") {
        "/ActualText"
    } else if cause.contains("encryption") {
        "Encryption"
    } else if cause.contains("tounicode") {
        "ToUnicode CMaps"
    } else if cause.contains("xobject") {
        "XObject subtypes"
    } else {
        "Other"
    }
}

/// Effort estimates (person-days) per feature bucket, from
/// `priorities.toml`-equivalent config baked in here. These are
/// guesses; a `None` means "unestimated" — the report marks them.
#[derive(Debug, Clone)]
pub struct PrioritiesConfig {
    /// feature bucket → person-day estimate.
    pub effort: BTreeMap<String, u32>,
    /// feature bucket → prerequisite buckets. A feature only pays off
    /// once its prerequisites exist, so the combination analysis
    /// closes every candidate set over this table and charges the
    /// prerequisites' effort too.
    pub deps: BTreeMap<String, Vec<String>>,
}

impl Default for PrioritiesConfig {
    fn default() -> Self {
        let mut e = BTreeMap::new();
        // Rough person-day estimates — order-of-magnitude, flagged as
        // guesses in the report. Adjust in the config, not the code.
        //
        // ExtGState: the basic set + alpha + separable blend modes are
        // now IMPLEMENTED (render.rs `apply_extgstate`). What remains is
        // the transparency-group / soft-mask layer — genuinely harder
        // (nested group compositing), so the residual estimate is
        // higher than the original 8pd, not lower.
        e.insert("ExtGState (gs)".into(), 18);
        e.insert("Shading (sh)".into(), 15);
        e.insert("Pattern colours".into(), 12);
        e.insert("Type1 fonts".into(), 12);
        e.insert("CFF/CIDFontType0 fonts".into(), 15);
        e.insert("Type3 fonts".into(), 10);
        e.insert("Vertical/CJK encodings".into(), 8);
        e.insert("Fonts (other)".into(), 10);
        e.insert("Annotations (/Annots)".into(), 10);
        e.insert("Optional Content (OC/OCMD)".into(), 12);
        e.insert("Non-device colour spaces".into(), 12);
        e.insert("Image masks / SMask".into(), 10);
        e.insert("Image filters/decode".into(), 8);
        e.insert("JPXDecode (JPEG2000)".into(), 20);
        e.insert("JBIG2Decode".into(), 20);
        e.insert("Other stream filters".into(), 6);
        e.insert("Named destinations".into(), 3);
        e.insert("/UserUnit ≠ 1".into(), 2);
        e.insert("/ActualText".into(), 6);
        e.insert("Encryption".into(), 25);
        e.insert("ToUnicode CMaps".into(), 8);
        e.insert("XObject subtypes".into(), 8);
        e.insert("Other".into(), 8);
        let mut d: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Shading patterns (PatternType 2) are drawn by the `sh`
        // machinery; tiling patterns alone don't cover the corpus hits.
        d.insert("Pattern colours".into(), vec!["Shading (sh)".into()]);
        // The ExtGState residue is soft masks, which composite through a
        // transparency *group* XObject (`render.xobject.group`).
        d.insert("ExtGState (gs)".into(), vec!["XObject subtypes".into()]);
        PrioritiesConfig { effort: e, deps: d }
    }
}

// ------------------------------------------------------------------
// Feature-set (combination) analysis
// ------------------------------------------------------------------

/// Combinations the task asked about explicitly. They are evaluated
/// even when the data would never generate them, and marked as
/// hypotheses in the output.
pub const HYPOTHESES: &[&[&str]] = &[
    &["Fonts (other)", "Annotations (/Annots)"],
    &["Optional Content (OC/OCMD)", "Annotations (/Annots)"],
    &["Pattern colours", "Type3 fonts"],
    &["Vertical/CJK encodings", "CFF/CIDFontType0 fonts"],
    &["Non-device colour spaces", "ExtGState (gs)"],
    &["ToUnicode CMaps", "Fonts (other)"],
];

/// One candidate set of features to implement together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComboStat {
    /// The features asked about (sorted).
    pub features: Vec<String>,
    /// `features` plus prerequisites from `PrioritiesConfig::deps` —
    /// the work that actually has to happen.
    pub closure: Vec<String>,
    /// Files carrying at least one feature of the closure.
    pub affected: usize,
    /// Currently-failing, well-formed files whose *entire known*
    /// blocker set lies inside the closure. "Known" = parse succeeded
    /// and the probe ran; unprobed regions may hide more — an
    /// estimate, never a guarantee.
    pub potentially_unblocked: usize,
    /// Subset of `potentially_unblocked` with full probe coverage
    /// (every page walked, no walk errors, no Form XObjects whose
    /// interior the probe cannot see). Still not a promise — the probe
    /// mirrors the renderer's rules rather than running them — but
    /// there is no known unscanned region.
    pub fully_unblocked: usize,
    /// Gain over the best single feature inside the set.
    pub synergy: usize,
    /// Σ effort over the closure (person-days, estimate); `None` when
    /// any member is unestimated.
    pub effort: Option<u32>,
    /// `potentially_unblocked / effort` — expected newly-PASS files per
    /// person-day.
    pub score: Option<f64>,
    /// "high" / "medium" / "low" — see [`combo_confidence`].
    pub confidence: String,
    pub reason: String,
    /// From [`HYPOTHESES`] rather than generated from the data.
    pub hypothesis: bool,
    /// `(file id, full blocker set)` for every potentially-unblocked
    /// file, so the claim can be checked by hand.
    pub files: Vec<(String, Vec<String>)>,
    /// Of those, the ids with full probe coverage.
    pub fully_probed_files: Vec<String>,
}

/// A file's blocker set as the combination analysis sees it.
#[derive(Debug, Clone)]
pub struct FileBlockers {
    pub id: String,
    /// Feature buckets from the probe *and* from every failing stage's
    /// cause id (a rewrite-only blocker such as named destinations
    /// still stops a full PASS).
    pub features: BTreeSet<String>,
    /// Parse succeeded, so the probe could run. A parse failure means
    /// the blocker set is simply unknown — never "empty".
    pub known: bool,
    /// Every page probed, no walk errors, no Form XObjects.
    pub fully_probed: bool,
    pub currently_pass: bool,
}

pub fn file_blockers(f: &FileResult) -> FileBlockers {
    let mut features: BTreeSet<String> = f
        .causes
        .iter()
        .map(|c| feature_bucket(c).to_string())
        .collect();
    for st in [&f.render, &f.text, &f.rewrite] {
        match st.class {
            Class::Pass | Class::Skip => {}
            _ => {
                let b = if st.cause.is_empty() {
                    "Other"
                } else {
                    feature_bucket(&st.cause)
                };
                features.insert(b.to_string());
            }
        }
    }
    let known = f.parse.class == Class::Pass;
    let fully_probed = known
        && f.probe_walk_errors == 0
        && f.probe_forms == 0
        && f.total_pages > 0
        && f.probe_pages >= f.total_pages;
    FileBlockers {
        id: f.id.clone(),
        features,
        known,
        fully_probed,
        currently_pass: f.verdict == Class::Pass,
    }
}

/// Close `set` over the dependency table.
fn dep_closure(set: &BTreeSet<String>, cfg: &PrioritiesConfig) -> BTreeSet<String> {
    let mut out = set.clone();
    let mut stack: Vec<String> = set.iter().cloned().collect();
    while let Some(f) = stack.pop() {
        if let Some(ds) = cfg.deps.get(&f) {
            for d in ds {
                if out.insert(d.clone()) {
                    stack.push(d.clone());
                }
            }
        }
    }
    out
}

/// Confidence that implementing the set really yields the counted
/// files: driven by how many candidates have *no* unprobed region.
pub fn combo_confidence(potentially: usize, fully: usize, effort: Option<u32>) -> &'static str {
    if potentially == 0 || effort.is_none() {
        return "low";
    }
    let ratio = fully as f64 / potentially as f64;
    if ratio >= 0.5 && potentially >= 2 {
        "high"
    } else if fully >= 1 || potentially >= 3 {
        "medium"
    } else {
        "low"
    }
}

fn evaluate_combo(
    features: BTreeSet<String>,
    hypothesis: bool,
    blockers: &[FileBlockers],
    single: &BTreeMap<String, usize>,
    cfg: &PrioritiesConfig,
) -> ComboStat {
    let closure = dep_closure(&features, cfg);
    let affected = blockers
        .iter()
        .filter(|b| b.known && !b.features.is_disjoint(&closure))
        .count();
    let unblocked: Vec<&FileBlockers> = blockers
        .iter()
        .filter(|b| {
            b.known && !b.currently_pass && !b.features.is_empty() && b.features.is_subset(&closure)
        })
        .collect();
    let fully: Vec<&FileBlockers> = unblocked
        .iter()
        .copied()
        .filter(|b| b.fully_probed)
        .collect();
    let effort = closure
        .iter()
        .map(|f| cfg.effort.get(f).copied())
        .try_fold(0u32, |acc, e| e.map(|e| acc + e));
    let score = effort
        .filter(|e| *e > 0)
        .map(|e| unblocked.len() as f64 / e as f64);
    let (best_single_name, best_single) = features
        .iter()
        .map(|f| (f.as_str(), single.get(f).copied().unwrap_or(0)))
        .max_by_key(|(_, n)| *n)
        .unwrap_or(("", 0));
    let synergy = unblocked.len().saturating_sub(best_single);
    let confidence = combo_confidence(unblocked.len(), fully.len(), effort);
    let mut reason = if unblocked.is_empty() {
        "no currently-failing file has only these blockers".to_string()
    } else {
        format!(
            "{} file(s) have no other known blocker, {} with full probe coverage",
            unblocked.len(),
            fully.len()
        )
    };
    if features.len() > 1 && synergy > 0 {
        reason.push_str(&format!(
            "; +{synergy} over the best single feature ({best_single_name})"
        ));
    } else if features.len() > 1 {
        reason.push_str("; no gain over the best single feature");
    }
    let extra: Vec<&str> = closure
        .iter()
        .filter(|f| !features.contains(*f))
        .map(|s| s.as_str())
        .collect();
    if !extra.is_empty() {
        reason.push_str(&format!(
            "; effort includes prerequisite(s) {}",
            extra.join(", ")
        ));
    }
    if hypothesis {
        reason.push_str("; hypothesis from the task list");
    }
    ComboStat {
        features: features.into_iter().collect(),
        closure: closure.into_iter().collect(),
        affected,
        potentially_unblocked: unblocked.len(),
        fully_unblocked: fully.len(),
        synergy,
        effort,
        score,
        confidence: confidence.into(),
        reason,
        hypothesis,
        files: unblocked
            .iter()
            .map(|b| (b.id.clone(), b.features.iter().cloned().collect()))
            .collect(),
        fully_probed_files: fully.iter().map(|b| b.id.clone()).collect(),
    }
}

/// Evaluate feature *sets* of size 1..=`max_k`. Candidates are drawn
/// from the data — every failing file's own blocker set and the
/// pairwise unions of those sets (a set that is not a superset of some
/// file's blockers cannot unblock anything a smaller set wouldn't) —
/// plus the fixed [`HYPOTHESES`]. Files with `expect: reject` are
/// excluded (refusing them is the goal), and files whose parse failed
/// contribute nothing: their blockers are unknown, not absent.
///
/// Data-derived sets that unblock no more than one of their strict
/// subsets are dropped as dominated; hypotheses are always kept.
pub fn combo_analysis(
    files: &[FileResult],
    cfg: &PrioritiesConfig,
    max_k: usize,
) -> Vec<ComboStat> {
    let blockers: Vec<FileBlockers> = files
        .iter()
        .filter(|f| f.expect != "reject")
        .map(file_blockers)
        .collect();
    let failing_sets: Vec<&BTreeSet<String>> = blockers
        .iter()
        .filter(|b| b.known && !b.currently_pass && !b.features.is_empty())
        .map(|b| &b.features)
        .collect();

    let mut cands: BTreeSet<BTreeSet<String>> = BTreeSet::new();
    for s in &failing_sets {
        for f in s.iter() {
            cands.insert(BTreeSet::from([f.clone()]));
        }
        if s.len() <= max_k {
            cands.insert((*s).clone());
        }
    }
    for (i, a) in failing_sets.iter().enumerate() {
        for b in failing_sets.iter().skip(i + 1) {
            let u: BTreeSet<String> = a.union(b).cloned().collect();
            if u.len() <= max_k {
                cands.insert(u);
            }
        }
    }
    let hyps: Vec<BTreeSet<String>> = HYPOTHESES
        .iter()
        .map(|h| h.iter().map(|s| s.to_string()).collect())
        .collect();

    // Single-feature baseline for the synergy figure.
    let single: BTreeMap<String, usize> = cands
        .iter()
        .filter(|c| c.len() == 1)
        .chain(hyps.iter().flat_map(|_| std::iter::empty()))
        .map(|c| {
            let st = evaluate_combo(c.clone(), false, &blockers, &BTreeMap::new(), cfg);
            (c.iter().next().unwrap().clone(), st.potentially_unblocked)
        })
        .collect();
    // Hypothesis members that never appear in the data still need a
    // baseline entry (0) for the synergy arithmetic.
    let single: BTreeMap<String, usize> = {
        let mut s = single;
        for h in &hyps {
            for f in h {
                s.entry(f.clone()).or_insert_with(|| {
                    evaluate_combo(
                        BTreeSet::from([f.clone()]),
                        false,
                        &blockers,
                        &BTreeMap::new(),
                        cfg,
                    )
                    .potentially_unblocked
                });
            }
        }
        s
    };

    let mut out: Vec<ComboStat> = Vec::new();
    for c in &cands {
        let is_hyp = hyps.contains(c);
        out.push(evaluate_combo(c.clone(), is_hyp, &blockers, &single, cfg));
    }
    for h in &hyps {
        if !cands.contains(h) {
            out.push(evaluate_combo(h.clone(), true, &blockers, &single, cfg));
        }
    }

    // Drop dominated data-derived sets.
    let keep: Vec<bool> = out
        .iter()
        .map(|c| {
            if c.hypothesis || c.features.len() == 1 {
                return true;
            }
            let fs: BTreeSet<&String> = c.features.iter().collect();
            !out.iter().any(|o| {
                let os: BTreeSet<&String> = o.features.iter().collect();
                os.len() < fs.len()
                    && os.is_subset(&fs)
                    && o.potentially_unblocked >= c.potentially_unblocked
            })
        })
        .collect();
    let mut out: Vec<ComboStat> = out
        .into_iter()
        .zip(keep)
        .filter_map(|(c, k)| k.then_some(c))
        .collect();

    out.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.potentially_unblocked.cmp(&a.potentially_unblocked))
            .then(b.fully_unblocked.cmp(&a.fully_unblocked))
            .then(a.features.len().cmp(&b.features.len()))
            .then(a.features.cmp(&b.features))
    });
    out
}

/// Markdown: the ranked feature-set table plus a drill-down of which
/// files each set would unblock and their full blocker sets.
pub fn combos_table(stats: &[ComboStat], top: usize) -> String {
    let mut s = String::from(
        "## Feature-set priorities (1–4 features together)\n\n\
         | Rank | Feature set | Expected newly PASS (fully probed) | Effort (pd) | Confidence | Reason |\n\
         |---:|---|---:|---:|---|---|\n",
    );
    for (i, c) in stats.iter().take(top).enumerate() {
        let effort = c
            .effort
            .map(|e| e.to_string())
            .unwrap_or_else(|| "?".into());
        s.push_str(&format!(
            "| {} | {}{} | {} ({}) | {} | {} | {} |\n",
            i + 1,
            c.features.join(" + "),
            if c.hypothesis { " *(hypothesis)*" } else { "" },
            c.potentially_unblocked,
            c.fully_unblocked,
            effort,
            c.confidence,
            c.reason
        ));
    }
    s.push_str(
        "\n*Expected newly PASS counts currently-failing well-formed files whose every \
         **known** blocker is in the set (prerequisites included). Files whose parse \
         failed are excluded — their blockers are unknown, not absent. The bracketed \
         figure has full probe coverage (all pages walked, no Form XObjects); even that is \
         an estimate, since the probe mirrors the renderer's rules rather than running \
         them. Effort is a person-day guess from `PrioritiesConfig`.*\n\n",
    );
    s.push_str("### Candidate files per feature set\n\n");
    for c in stats.iter().take(top) {
        if c.files.is_empty() {
            continue;
        }
        s.push_str(&format!("- **{}**\n", c.features.join(" + ")));
        for (id, feats) in c.files.iter().take(12) {
            let cov = if c.fully_probed_files.contains(id) {
                "fully probed"
            } else {
                "has unprobed regions"
            };
            s.push_str(&format!("  - `{id}` — {{{}}} — {cov}\n", feats.join(", ")));
        }
        if c.files.len() > 12 {
            s.push_str(&format!("  - … {} more\n", c.files.len() - 12));
        }
    }
    s.push('\n');
    s
}

/// Aggregate per-file causes into per-feature stats.
pub fn analyze(files: &[FileResult], cfg: &PrioritiesConfig) -> Vec<FeatureStat> {
    // feature → set of file indices affected
    let mut affected: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut sole: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut pages: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cause_map: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();

    for (i, f) in files.iter().enumerate() {
        let mut file_features: BTreeSet<&str> = BTreeSet::new();
        for c in &f.causes {
            let b = feature_bucket(c);
            file_features.insert(b);
            cause_map.entry(b).or_default().insert(c.clone());
            *pages.entry(b).or_default() += f.cause_pages.get(c).copied().unwrap_or(0);
        }
        for b in &file_features {
            affected.entry(b).or_default().insert(i);
        }
        // sole-blocker: this file has exactly one feature bucket
        if file_features.len() == 1 {
            let b = *file_features.iter().next().unwrap();
            sole.entry(b).or_default().insert(i);
        }
    }

    let mut out = Vec::new();
    for (b, set) in affected {
        let mut causes: Vec<String> = cause_map[b].iter().cloned().collect();
        causes.sort();
        let effort = cfg.effort.get(b).copied();
        let sole_n = sole.get(b).map(|s| s.len()).unwrap_or(0);
        // potentially-unblocked ~ sole-blockers (implementing the only
        // known blocker unblocks the file) — an estimate.
        let unblocked = sole_n;
        let score = effort
            .filter(|e| *e > 0)
            .map(|e| unblocked as f64 / e as f64);
        out.push(FeatureStat {
            feature: b.to_string(),
            causes,
            affected: set.len(),
            sole_blocker: sole_n,
            potentially_unblocked: unblocked,
            affected_pages: pages.get(b).copied().unwrap_or(0),
            effort,
            score,
            files: set.iter().map(|i| files[*i].id.clone()).collect(),
        });
    }
    // Sort by the priority score (unblocked / effort) first — that is
    // the effort-adjusted rank the task asks for. Ties break toward
    // higher absolute unblock count, then broader affected reach.
    // Features with no effort estimate sort last (score = None → 0).
    out.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.potentially_unblocked.cmp(&a.potentially_unblocked))
            .then(b.affected.cmp(&a.affected))
    });
    out
}

/// Per-stage denominators: attempted vs skipped vs passed, both as
/// all-files and as attempted-only. This makes the "success rate"
/// denominator explicit — `skip` files (earlier stage failed) are
/// counted separately, never silently in either numerator.
#[derive(Debug, Serialize, Deserialize)]
pub struct StageSummary {
    pub total: usize,
    pub parse: StageCounts,
    pub render: StageCounts,
    pub text: StageCounts,
    pub rewrite: StageCounts,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StageCounts {
    /// files where the stage actually ran (not skipped).
    pub attempted: usize,
    /// files that were skipped (earlier stage failed / couldn't run).
    pub skipped: usize,
    /// breakdown of attempted files.
    pub pass: usize,
    pub mismatch: usize,
    pub unsupported: usize,
    pub error: usize,
    pub crash: usize,
    pub timeout: usize,
    /// success rate over the *attempted* set (0..1).
    pub pass_rate_attempted: f64,
    /// success rate over the *whole corpus* (0..1).
    pub pass_rate_all: f64,
}

/// Compatibility-vs-Robustness split.
///
/// Each file's `expect` tag (from the manifest) decides which rate it
/// feeds. `pass` files are *well-formed* documents the engine should
/// handle → the Compatibility rate (per-stage pass / count).
/// `reject` files are adversarial/malformed/encrypted — the Robustness
/// score counts a file OK when the engine *refuses it safely* (any
/// non-crash non-timeout result; a clean `Unsupported`/`Error` is a
/// pass, but actually rendering a hostile file to completion is also
/// acceptable behaviour — only CRASH/TIMEOUT fail). `any` files are
/// excluded from both rates.
#[derive(Debug, Serialize, Deserialize)]
pub struct SplitSummary {
    /// well-formed files (expect=pass): stage pass-rates.
    pub compat_total: usize,
    pub compat_parse_pass: usize,
    pub compat_render_pass: usize,
    pub compat_text_pass: usize,
    pub compat_rewrite_pass: usize,
    /// adversarial files (expect=reject).
    pub robust_total: usize,
    /// safely handled (no crash/hang) — the robustness criterion.
    pub robust_safe: usize,
    /// rejected with a classified error (not just crashed on).
    pub robust_refused: usize,
    /// crash/timeout on adversarial input — robustness failures.
    pub robust_failures: Vec<String>,
    /// expect=any files, excluded from both rates.
    pub other_total: usize,
}

pub fn split_summary(files: &[FileResult]) -> SplitSummary {
    let mut s = SplitSummary {
        compat_total: 0,
        compat_parse_pass: 0,
        compat_render_pass: 0,
        compat_text_pass: 0,
        compat_rewrite_pass: 0,
        robust_total: 0,
        robust_safe: 0,
        robust_refused: 0,
        robust_failures: Vec::new(),
        other_total: 0,
    };
    for f in files {
        match f.expect.as_str() {
            "pass" => {
                s.compat_total += 1;
                // Downstream passes count only on a strictly parsed
                // document — a recovered file's stages are reported
                // separately.
                if f.parse.class == Class::Pass {
                    s.compat_parse_pass += 1;
                    if f.render.class == Class::Pass {
                        s.compat_render_pass += 1;
                    }
                    if f.text.class == Class::Pass {
                        s.compat_text_pass += 1;
                    }
                    if f.rewrite.class == Class::Pass {
                        s.compat_rewrite_pass += 1;
                    }
                }
            }
            "reject" => {
                s.robust_total += 1;
                match f.verdict {
                    Class::Crash | Class::Timeout => {
                        s.robust_failures.push(f.id.clone());
                    }
                    _ => {
                        s.robust_safe += 1;
                        if f.verdict == Class::Error || f.verdict == Class::Unsupported {
                            s.robust_refused += 1;
                        }
                    }
                }
            }
            _ => s.other_total += 1,
        }
    }
    s
}

/// Per-category pass rates (verdict + per-stage).
#[derive(Debug, Serialize, Deserialize)]
pub struct CategorySummary {
    pub category: String,
    pub files: usize,
    pub parse_pass: usize,
    pub render_pass: usize,
    pub text_pass: usize,
    pub rewrite_pass: usize,
    pub verdict_pass: usize,
}

/// One group of files with every count over the *same* denominator.
///
/// This is the single source of truth for every headline number: a
/// file counts for a stage only when that stage passed *strictly*
/// (`rated_class`), and `full_pass` requires all four, so within one
/// group `full_pass ≤ min(parse, render, text, rewrite)` by
/// construction (`tally_invariants` checks it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tally {
    /// Which files: `"all"`, `"expect:pass"`, `"expect:reject"`,
    /// `"expect:any"`, or a category / producer name.
    pub group: String,
    /// The denominator of every count below.
    pub files: usize,
    pub parse: usize,
    pub render: usize,
    pub text: usize,
    pub rewrite: usize,
    /// All four stages passed strictly.
    pub full_pass: usize,
    /// Strict parse failed but the xref rebuild succeeded (their
    /// downstream stages are *not* in the counts above).
    pub recovered: usize,
    /// Crash or timeout in any stage.
    pub crash_or_timeout: usize,
}

impl Tally {
    pub fn of(group: &str, files: &[&FileResult]) -> Tally {
        let strict = |f: &FileResult, st: &crate::StageResult| rated_class(f, st) == Class::Pass;
        let mut t = Tally {
            group: group.into(),
            files: files.len(),
            parse: 0,
            render: 0,
            text: 0,
            rewrite: 0,
            full_pass: 0,
            recovered: 0,
            crash_or_timeout: 0,
        };
        for f in files {
            let p = f.parse.class == Class::Pass;
            let r = strict(f, &f.render);
            let x = strict(f, &f.text);
            let w = strict(f, &f.rewrite);
            t.parse += p as usize;
            t.render += r as usize;
            t.text += x as usize;
            t.rewrite += w as usize;
            t.full_pass += (p && r && x && w) as usize;
            t.recovered += f.recovered as usize;
            let bad = [&f.parse, &f.recover, &f.render, &f.text, &f.rewrite]
                .iter()
                .any(|s| matches!(s.class, Class::Crash | Class::Timeout));
            t.crash_or_timeout += bad as usize;
        }
        t
    }

    /// `true` when the counts are mutually consistent.
    pub fn consistent(&self) -> bool {
        self.full_pass <= self.parse.min(self.render).min(self.text).min(self.rewrite)
            && self.parse <= self.files
            && self.render <= self.parse
            && self.text <= self.parse
            && self.rewrite <= self.parse
            && self.recovered + self.parse <= self.files
    }
}

/// The headline tallies: all files, then by `expect` tag.
pub fn tallies(files: &[FileResult]) -> Vec<Tally> {
    let all: Vec<&FileResult> = files.iter().collect();
    let mut out = vec![Tally::of("all", &all)];
    for tag in ["pass", "reject", "any"] {
        let g: Vec<&FileResult> = files.iter().filter(|f| f.expect == tag).collect();
        out.push(Tally::of(&format!("expect:{tag}"), &g));
    }
    out
}

/// The class a stage contributes to the rates: a recovered file's
/// downstream stages ran on a rebuilt document and are reported in the
/// recovery section only, so here they count as skipped.
pub fn rated_class(f: &FileResult, stage: &crate::StageResult) -> Class {
    if f.recovered && !std::ptr::eq(stage, &f.parse) {
        Class::Skip
    } else {
        stage.class
    }
}

fn counts(files: &[FileResult], get: fn(&FileResult) -> &crate::StageResult) -> StageCounts {
    let mut c = StageCounts {
        attempted: 0,
        skipped: 0,
        pass: 0,
        mismatch: 0,
        unsupported: 0,
        error: 0,
        crash: 0,
        timeout: 0,
        pass_rate_attempted: 0.0,
        pass_rate_all: 0.0,
    };
    let total = files.len();
    for f in files {
        let class = rated_class(f, get(f));
        match class {
            Class::Skip => c.skipped += 1,
            _ => {
                c.attempted += 1;
                match class {
                    Class::Pass => c.pass += 1,
                    Class::Mismatch => c.mismatch += 1,
                    Class::Unsupported => c.unsupported += 1,
                    Class::Error => c.error += 1,
                    Class::Crash => c.crash += 1,
                    Class::Timeout => c.timeout += 1,
                    _ => {}
                }
            }
        }
    }
    c.pass_rate_attempted = if c.attempted > 0 {
        c.pass as f64 / c.attempted as f64
    } else {
        0.0
    };
    c.pass_rate_all = if total > 0 {
        c.pass as f64 / total as f64
    } else {
        0.0
    };
    c
}

pub fn stage_summary(files: &[FileResult]) -> StageSummary {
    StageSummary {
        total: files.len(),
        parse: counts(files, |f| &f.parse),
        render: counts(files, |f| &f.render),
        text: counts(files, |f| &f.text),
        rewrite: counts(files, |f| &f.rewrite),
    }
}

pub fn category_summary(files: &[FileResult]) -> Vec<CategorySummary> {
    let mut by_cat: BTreeMap<&str, Vec<&FileResult>> = BTreeMap::new();
    for f in files {
        by_cat.entry(f.category.as_str()).or_default().push(f);
    }
    let mut out = Vec::new();
    for (cat, fs) in by_cat {
        out.push(CategorySummary {
            category: cat.to_string(),
            files: fs.len(),
            parse_pass: fs.iter().filter(|f| f.parse.class == Class::Pass).count(),
            render_pass: fs
                .iter()
                .filter(|f| rated_class(f, &f.render) == Class::Pass)
                .count(),
            text_pass: fs
                .iter()
                .filter(|f| rated_class(f, &f.text) == Class::Pass)
                .count(),
            rewrite_pass: fs
                .iter()
                .filter(|f| rated_class(f, &f.rewrite) == Class::Pass)
                .count(),
            verdict_pass: fs.iter().filter(|f| f.verdict == Class::Pass).count(),
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.files));
    out
}

/// Render the priority table (Markdown) — affected / sole-blocker /
/// potentially-unblocked / effort / score.
pub fn priorities_table(stats: &[FeatureStat]) -> String {
    let mut s = String::from(
        "| feature | affected | sole-blocker | potentially unblocked* | effort (pd) | score |\n\
         |---|---:|---:|---:|---:|---:|\n",
    );
    for f in stats {
        let effort = f
            .effort
            .map(|e| e.to_string())
            .unwrap_or_else(|| "?".into());
        let score = f
            .score
            .map(|s| format!("{:.2}", s))
            .unwrap_or_else(|| "?".into());
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            f.feature, f.affected, f.sole_blocker, f.potentially_unblocked, effort, score
        ));
    }
    s.push_str(
        "\n\\* *potentially unblocked is an estimate — it counts files where this\n\
         is the only blocker the probe detected; hidden blockers inside Form\n\
         XObjects or later pages may still fail. It is not a guarantee.*\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Class, FileResult, Metrics, StageResult};

    fn file(id: &str, causes: &[&str]) -> FileResult {
        let pass = StageResult::pass(0);
        FileResult {
            id: id.into(),
            sha256: String::new(),
            bytes: 0,
            category: "test".into(),
            verdict: Class::Unsupported,
            pages_seen: 1,
            parse: pass,
            recover: Default::default(),
            recovered: false,
            render: StageResult::pass(0),
            text: StageResult::pass(0),
            rewrite: StageResult::pass(0),
            causes: causes.iter().map(|s| s.to_string()).collect(),
            cause_pages: causes.iter().map(|s| (s.to_string(), 1)).collect(),
            expect: "pass".into(),
            producer: String::new(),
            pdf_version: String::new(),
            total_pages: 1,
            probe_pages: 1,
            probe_walk_errors: 0,
            probe_forms: 0,
            metrics: Metrics {
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
                text_match_ratio: None,
                rewrite_qpdf_ok: None,
                rewrite_reparsed: None,
            },
            features: Vec::new(),
            roundtrip: None,
        }
    }

    #[test]
    fn bucket_groups_fine_causes() {
        assert_eq!(
            feature_bucket("render.extgstate.unsupported"),
            "ExtGState (gs)"
        );
        assert_eq!(
            feature_bucket("render.font.type1.unsupported"),
            "Type1 fonts"
        );
        assert_eq!(
            feature_bucket("render.font.cff.unsupported"),
            "CFF/CIDFontType0 fonts"
        );
        assert_eq!(feature_bucket("render.shading.unsupported"), "Shading (sh)");
        assert_eq!(feature_bucket("parse.encryption.unsupported"), "Encryption");
        assert_eq!(feature_bucket("something.else.error"), "Other");
    }

    #[test]
    fn sole_blocker_only_when_single_feature() {
        let files = vec![
            file("a", &["render.extgstate.unsupported"]),
            file(
                "b",
                &["render.extgstate.unsupported", "render.shading.unsupported"],
            ),
        ];
        let stats = analyze(&files, &PrioritiesConfig::default());
        let gs = stats
            .iter()
            .find(|s| s.feature == "ExtGState (gs)")
            .unwrap();
        assert_eq!(gs.affected, 2, "both files carry gs");
        assert_eq!(gs.sole_blocker, 1, "only file a is unblocked by gs alone");
        assert_eq!(gs.potentially_unblocked, 1);
    }

    #[test]
    fn multi_feature_files_count_in_every_bucket() {
        let files = vec![file(
            "b",
            &["render.extgstate.unsupported", "render.shading.unsupported"],
        )];
        let stats = analyze(&files, &PrioritiesConfig::default());
        let gs = stats
            .iter()
            .find(|s| s.feature == "ExtGState (gs)")
            .unwrap();
        let sh = stats.iter().find(|s| s.feature == "Shading (sh)").unwrap();
        assert_eq!(gs.affected, 1);
        assert_eq!(sh.affected, 1);
        assert_eq!(gs.sole_blocker, 0, "shared blockers can't be sole");
        assert_eq!(sh.sole_blocker, 0);
    }

    #[test]
    fn score_is_unblocked_per_effort() {
        let files = vec![file("a", &["render.userunit.unsupported"])];
        let stats = analyze(&files, &PrioritiesConfig::default());
        let uu = stats.iter().find(|s| s.feature == "/UserUnit ≠ 1").unwrap();
        // effort = 2 pd, unblocked = 1 → score 0.5
        assert_eq!(uu.effort, Some(2));
        assert_eq!(uu.score, Some(0.5));
    }

    #[test]
    fn tally_invariants() {
        // full_pass never exceeds any stage; a recovered file's
        // downstream stages are excluded; skips are not passes.
        let mut ok = file("ok", &[]);
        ok.expect = "pass".into();
        let mut recovered = file("rec", &[]);
        recovered.expect = "pass".into();
        recovered.parse = StageResult::new(Class::Error, "x".into(), String::new(), 0);
        recovered.recover = StageResult::pass(0);
        recovered.recovered = true;
        let mut render_fail = file("rf", &[]);
        render_fail.expect = "pass".into();
        render_fail.render = StageResult::new(Class::Unsupported, "y".into(), String::new(), 0);
        let mut hostile = file("h", &[]);
        hostile.expect = "reject".into();
        hostile.verdict = Class::Crash;
        hostile.render = StageResult::new(Class::Crash, "boom".into(), String::new(), 0);
        let files = vec![ok, recovered, render_fail, hostile];
        let t = tallies(&files);
        let all = &t[0];
        assert_eq!(all.group, "all");
        assert_eq!(all.files, 4);
        assert_eq!(all.parse, 3);
        assert_eq!(
            all.render, 1,
            "recovered and render_fail and hostile do not render strictly"
        );
        assert_eq!(all.full_pass, 1);
        assert_eq!(all.recovered, 1);
        assert_eq!(all.crash_or_timeout, 1);
        for g in &t {
            assert!(g.consistent(), "{g:?}");
        }
        let pass = t.iter().find(|g| g.group == "expect:pass").unwrap();
        assert_eq!(
            (pass.files, pass.parse, pass.render, pass.full_pass),
            (3, 2, 1, 1)
        );
        let reject = t.iter().find(|g| g.group == "expect:reject").unwrap();
        assert_eq!((reject.files, reject.crash_or_timeout), (1, 1));
    }

    #[test]
    fn split_summary_separates_compat_and_robust() {
        let mut compat_ok = file("ok", &[]);
        compat_ok.expect = "pass".into();
        let mut hostile_safe = file("hostile", &[]);
        hostile_safe.expect = "reject".into();
        hostile_safe.verdict = Class::Error; // refused safely
        let mut hostile_crash = file("boom", &[]);
        hostile_crash.expect = "reject".into();
        hostile_crash.verdict = Class::Crash;
        let mut other = file("edge", &[]);
        other.expect = "any".into();

        let s = split_summary(&[compat_ok, hostile_safe, hostile_crash, other]);
        assert_eq!(s.compat_total, 1);
        assert_eq!(s.compat_parse_pass, 1);
        assert_eq!(s.robust_total, 2);
        assert_eq!(s.robust_safe, 1);
        assert_eq!(s.robust_failures, vec!["boom"]);
        assert_eq!(s.other_total, 1);
    }

    // ---- combination analysis ------------------------------------

    fn find<'a>(stats: &'a [ComboStat], feats: &[&str]) -> &'a ComboStat {
        let want: Vec<String> = feats.iter().map(|s| s.to_string()).collect();
        stats
            .iter()
            .find(|c| c.features == want)
            .unwrap_or_else(|| panic!("no combo {want:?}"))
    }

    #[test]
    fn combo_counts_files_whose_whole_set_is_covered() {
        let files = vec![
            file("a", &["render.annotation.unsupported"]),
            file(
                "b",
                &[
                    "render.annotation.unsupported",
                    "render.font.nonembedded.unsupported",
                ],
            ),
            file("c", &["render.font.nonembedded.unsupported"]),
        ];
        let stats = combo_analysis(&files, &PrioritiesConfig::default(), 4);
        let ann = find(&stats, &["Annotations (/Annots)"]);
        assert_eq!(ann.affected, 2);
        assert_eq!(ann.potentially_unblocked, 1, "only a is annotation-only");
        let both = find(&stats, &["Annotations (/Annots)", "Fonts (other)"]);
        assert_eq!(both.potentially_unblocked, 3);
        assert_eq!(both.fully_unblocked, 3, "test files are fully probed");
        assert_eq!(both.synergy, 2, "3 vs best single (1)");
        assert_eq!(both.effort, Some(20));
        assert!(both.hypothesis, "listed in HYPOTHESES");
        assert_eq!(both.confidence, "high");
    }

    #[test]
    fn combo_ignores_parse_failures_and_rejects() {
        let mut bad = file("bad", &[]);
        bad.parse = StageResult::new(Class::Error, "eof".into(), String::new(), 0);
        bad.verdict = Class::Error;
        let mut hostile = file("hostile", &["render.annotation.unsupported"]);
        hostile.expect = "reject".into();
        let files = vec![file("a", &["render.annotation.unsupported"]), bad, hostile];
        let stats = combo_analysis(&files, &PrioritiesConfig::default(), 4);
        let ann = find(&stats, &["Annotations (/Annots)"]);
        assert_eq!(
            ann.potentially_unblocked, 1,
            "parse-failed + reject excluded"
        );
        assert!(!stats
            .iter()
            .any(|c| c.files.iter().any(|(id, _)| id == "bad")));
    }

    #[test]
    fn combo_stage_causes_count_as_blockers() {
        // Probe saw only annotations, but rewrite failed on named
        // destinations → the file is NOT annotation-only.
        let mut f = file("a", &["render.annotation.unsupported"]);
        f.rewrite = StageResult::caused(
            Class::Unsupported,
            "named dest".into(),
            "named-dest".into(),
            "rewrite.named-dest.unsupported".into(),
            0,
        );
        let stats = combo_analysis(&[f], &PrioritiesConfig::default(), 4);
        let ann = find(&stats, &["Annotations (/Annots)"]);
        assert_eq!(ann.potentially_unblocked, 0);
        let both = find(&stats, &["Annotations (/Annots)", "Named destinations"]);
        assert_eq!(both.potentially_unblocked, 1);
    }

    #[test]
    fn combo_full_coverage_needs_all_pages_and_no_forms() {
        let mut partial = file("p", &["render.annotation.unsupported"]);
        partial.total_pages = 10; // only 1 probed
        let mut formy = file("f", &["render.annotation.unsupported"]);
        formy.probe_forms = 2;
        let files = vec![
            file("a", &["render.annotation.unsupported"]),
            partial,
            formy,
        ];
        let stats = combo_analysis(&files, &PrioritiesConfig::default(), 4);
        let ann = find(&stats, &["Annotations (/Annots)"]);
        assert_eq!(ann.potentially_unblocked, 3);
        assert_eq!(ann.fully_unblocked, 1);
        assert_eq!(ann.fully_probed_files, vec!["a"]);
    }

    #[test]
    fn combo_deps_add_effort_and_members() {
        let files = vec![file("a", &["render.pattern.unsupported"])];
        let cfg = PrioritiesConfig::default();
        let stats = combo_analysis(&files, &cfg, 4);
        let pat = find(&stats, &["Pattern colours"]);
        assert!(pat.closure.contains(&"Shading (sh)".to_string()));
        assert_eq!(pat.effort, Some(12 + 15));
        assert!(pat.reason.contains("prerequisite"));
    }

    #[test]
    fn combo_dominated_sets_are_dropped() {
        // b adds nothing beyond a's single feature → the pair is dropped.
        let files = vec![
            file("a", &["render.userunit.unsupported"]),
            file(
                "b",
                &["render.userunit.unsupported", "render.shading.unsupported"],
            ),
            file("c", &["render.shading.unsupported"]),
        ];
        let stats = combo_analysis(&files, &PrioritiesConfig::default(), 4);
        // the pair unblocks 3 > any single (1) → kept
        assert!(stats
            .iter()
            .any(|c| c.features == ["/UserUnit ≠ 1", "Shading (sh)"]));
        // a hypothesis with zero unblocked is still listed, as low confidence
        let hyp = find(
            &stats,
            &["CFF/CIDFontType0 fonts", "Vertical/CJK encodings"],
        );
        assert_eq!(hyp.potentially_unblocked, 0);
        assert_eq!(hyp.confidence, "low");
        // Pattern's closure pulls in Shading, so the Pattern+Type3
        // hypothesis legitimately covers the shading-only file `c`.
        let pat = find(&stats, &["Pattern colours", "Type3 fonts"]);
        assert_eq!(pat.potentially_unblocked, 1);
        assert!(pat.closure.contains(&"Shading (sh)".to_string()));
    }

    #[test]
    fn affected_pages_accumulate() {
        let mut f = file("a", &["render.extgstate.unsupported"]);
        f.cause_pages
            .insert("render.extgstate.unsupported".into(), 3);
        let stats = analyze(&[f], &PrioritiesConfig::default());
        let gs = stats
            .iter()
            .find(|s| s.feature == "ExtGState (gs)")
            .unwrap();
        assert_eq!(gs.affected_pages, 3);
    }
}
