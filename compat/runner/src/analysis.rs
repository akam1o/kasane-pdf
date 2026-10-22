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
        PrioritiesConfig { effort: e }
    }
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
                if f.parse.class == Class::Pass {
                    s.compat_parse_pass += 1;
                }
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
        match get(f).class {
            Class::Skip => c.skipped += 1,
            _ => {
                c.attempted += 1;
                match get(f).class {
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
            render_pass: fs.iter().filter(|f| f.render.class == Class::Pass).count(),
            text_pass: fs.iter().filter(|f| f.text.class == Class::Pass).count(),
            rewrite_pass: fs.iter().filter(|f| f.rewrite.class == Class::Pass).count(),
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
            render: StageResult::pass(0),
            text: StageResult::pass(0),
            rewrite: StageResult::pass(0),
            causes: causes.iter().map(|s| s.to_string()).collect(),
            cause_pages: causes.iter().map(|s| (s.to_string(), 1)).collect(),
            expect: "pass".into(),
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
                text_match_ratio: None,
                rewrite_qpdf_ok: None,
                rewrite_reparsed: None,
            },
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
