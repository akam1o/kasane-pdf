//! Declared transformations (`plans/rewrite-policy.md` §6).
//!
//! The snapshot comparison finds every difference and is not changed by
//! anything here. This module only *classifies* a category's
//! differences against the transformations a run declares:
//!
//! - no difference: the class the comparison gave (`pass`), unless a
//!   declaration expected a change in this category — then `fail`
//!   (a declared transformation that did not happen);
//! - exactly the differences the declarations predict: `transformed`,
//!   with the rule names — never counted as `pass`;
//! - anything else, or an invalid declaration: `fail`.
//!
//! A section the snapshot could not take (`error`, `unsupported`) keeps
//! its class: a rule never turns an incomplete comparison into a result.
//!
//! Each rule computes, from the *original* snapshot and the declaration,
//! the exact lines that must disappear and appear; the comparison's
//! actual multiset difference must equal them.

use super::*;

/// A transformation a run declares. Produced by the runner from its own
/// reading of the input (T1) or from the rewrite's options (T2); the
/// library's report, once it exists, must agree with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// T1: the effective version raised because a carried feature needs
    /// a later one (the writer's floor: optional content needs 1.5).
    VersionRaised {
        from: (u32, u32),
        to: (u32, u32),
        reason: VersionReason,
    },
    /// T2: `/Perms` usage-rights signatures removed (D1, `Remove`).
    UsageRightsRemoved {
        /// `UR`, `UR3`.
        keys: Vec<String>,
        /// `/Perms` dropped because nothing else was left in it.
        perms_removed: bool,
        /// `/AcroForm /SigFlags` before and after (bit 1 cleared).
        sig_flags: Option<(i64, i64)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionReason {
    /// `/OCProperties` (PDF 1.5) in a document declaring less.
    OptionalContent,
}

impl Declaration {
    pub fn rule_name(&self) -> &'static str {
        match self {
            Declaration::VersionRaised { .. } => "T1 version raised",
            Declaration::UsageRightsRemoved { .. } => "T2 usage rights removed",
        }
    }
}

/// Lines a rule expects to disappear and to appear in one category.
#[derive(Debug, Default)]
struct Expected {
    removed: Vec<String>,
    added: Vec<String>,
}

/// The expectation of `decl` for `category`, `Ok(None)` when the rule
/// says nothing about this category, `Err` when the declaration does not
/// fit the original (an invalid declaration).
fn expect(
    decl: &Declaration,
    category: &str,
    original: &[String],
) -> Result<Option<Expected>, String> {
    match (decl, category) {
        (Declaration::VersionRaised { from, to, reason }, "version") => {
            let VersionReason::OptionalContent = reason;
            if *to != (1, 5) || *from >= (1, 5) {
                return Err("the optional-content floor raises a version below 1.5 to 1.5".into());
            }
            let old = format!("effective\tversion={}.{}", from.0, from.1);
            if !original.contains(&old) {
                return Err("the declared input version is not the document's".into());
            }
            Ok(Some(Expected {
                removed: vec![old],
                added: vec![format!("effective\tversion={}.{}", to.0, to.1)],
            }))
        }
        (
            Declaration::UsageRightsRemoved {
                keys,
                perms_removed,
                sig_flags,
            },
            "signatures",
        ) => {
            if keys.is_empty() || keys.iter().any(|k| k != "UR" && k != "UR3") {
                return Err("only /UR and /UR3 can be removed".into());
            }
            let mut e = Expected::default();
            let entries: Vec<&String> = original
                .iter()
                .filter(|l| l.starts_with("perms_entry\t"))
                .collect();
            for k in keys {
                let head = format!("perms_entry\tkey=/{k}\ttarget=");
                let Some(line) = entries.iter().find(|l| l.starts_with(&head)) else {
                    return Err(format!("/Perms has no /{k}"));
                };
                let target = &line[head.len()..];
                e.removed.push((*line).clone());
                // The usage-rights signature dictionary itself, reached
                // only from /Perms (a shared one cannot be removed).
                let sig_head = format!("sig\tat={target}\t");
                let sigs: Vec<&String> = original
                    .iter()
                    .filter(|l| l.starts_with(&sig_head))
                    .collect();
                match sigs.as_slice() {
                    [s] if s.contains("\trefs=1\t") => e.removed.push((*s).clone()),
                    [_] => return Err(format!("the /{k} signature is reachable from elsewhere")),
                    _ => {
                        return Err(format!(
                            "the /{k} target is not a signature-like dictionary"
                        ))
                    }
                }
            }
            let Some(perms) = original.iter().find(|l| l.starts_with("perms\t")) else {
                return Err("no /Perms".into());
            };
            let n: usize = perms
                .rsplit_once("\tkeys=")
                .and_then(|(_, n)| n.parse().ok())
                .ok_or("malformed /Perms line")?;
            if *perms_removed {
                if n != keys.len() {
                    return Err("/Perms holds more than the removed entries".into());
                }
                e.removed.push(perms.clone());
            } else {
                if n == keys.len() {
                    return Err("/Perms is left empty but declared kept".into());
                }
                let (head, _) = perms.rsplit_once("\tkeys=").unwrap();
                e.removed.push(perms.clone());
                e.added.push(format!("{head}\tkeys={}", n - keys.len()));
            }
            if let Some((before, after)) = sig_flags {
                if before & 1 == 0 || *after != (before & !1) {
                    return Err("/SigFlags may change only by clearing a set bit 1".into());
                }
                let old = format!("sigflags\tvalue={before}");
                if !original.contains(&old) {
                    return Err("the declared /SigFlags is not the document's".into());
                }
                e.removed.push(old);
                e.added.push(format!("sigflags\tvalue={after}"));
            }
            Ok(Some(e))
        }
        (
            Declaration::UsageRightsRemoved {
                sig_flags: Some((before, after)),
                ..
            },
            "acroform",
        ) => {
            let old = format!("\tsig_flags={before}\t");
            let Some(line) = original
                .iter()
                .find(|l| l.starts_with("form\t") && l.contains(&old))
            else {
                return Err("the declared /SigFlags is not the form's".into());
            };
            Ok(Some(Expected {
                removed: vec![line.clone()],
                added: vec![line.replacen(&old, &format!("\tsig_flags={after}\t"), 1)],
            }))
        }
        _ => Ok(None),
    }
}

/// Multiset difference `a - b`, sorted.
fn minus(a: &[String], b: &[String]) -> Vec<String> {
    let mut count: HashMap<&str, i64> = HashMap::new();
    for l in b {
        *count.entry(l).or_default() += 1;
    }
    let mut out = Vec::new();
    for l in a {
        let c = count.entry(l).or_default();
        if *c > 0 {
            *c -= 1;
        } else {
            out.push(l.clone());
        }
    }
    out.sort();
    out
}

/// Classify `check` (the comparison of `a` and `b` in its category)
/// against `decls`. Lines starting with `~` (declared form) are not
/// differences and are left out.
pub fn classify(check: &mut SemCheck, a: &Section, b: &Section, decls: &[Declaration]) {
    if decls.is_empty() {
        return;
    }
    let lines = |s: &Section| -> Option<Vec<String>> {
        match s {
            Section::Data(v) => Some(v.iter().filter(|l| !l.starts_with('~')).cloned().collect()),
            Section::Absent => Some(Vec::new()),
            _ => None,
        }
    };
    let (Some(la), Some(lb)) = (lines(a), lines(b)) else {
        return; // error / unsupported stay as they are
    };
    let mut expected = Expected::default();
    let mut rules = Vec::new();
    for d in decls {
        match expect(d, &check.category, &la) {
            Ok(Some(e)) => {
                expected.removed.extend(e.removed);
                expected.added.extend(e.added);
                rules.push(d.rule_name().to_string());
            }
            Ok(None) => {}
            Err(m) => {
                check.class = SemClass::Fail;
                check
                    .notes
                    .push(format!("invalid declaration ({}): {m}", d.rule_name()));
                return;
            }
        }
    }
    if rules.is_empty() {
        return; // no rule speaks for this category: the comparison stands
    }
    expected.removed.sort();
    expected.added.sort();
    let removed = minus(&la, &lb);
    let added = minus(&lb, &la);
    if removed == expected.removed && added == expected.added {
        if removed.is_empty() && added.is_empty() {
            // Only possible when a rule expects nothing here.
            return;
        }
        check.class = SemClass::Transformed;
        check.rules = rules;
        return;
    }
    check.class = SemClass::Fail;
    let unexplained_removed = minus(&removed, &expected.removed).len();
    let unexplained_added = minus(&added, &expected.added).len();
    let missing = minus(&expected.removed, &removed).len() + minus(&expected.added, &added).len();
    check.notes.push(format!(
        "declared ({}): {unexplained_removed} line(s) removed and {unexplained_added} added beyond the declaration, {missing} declared change(s) missing",
        rules.join(", ")
    ));
}

/// Whether the graph comparison's notes (`roundtrip::graph_diff`) are
/// exactly what a T2 declaration allows: `/Root/Perms` (or its removed
/// entries) dropped, and `/Root/AcroForm/SigFlags` changed when bit 1
/// was cleared — nothing else. For the D1 round trip.
pub fn structure_allows(notes: &[String], decls: &[Declaration]) -> bool {
    let Some(Declaration::UsageRightsRemoved {
        keys,
        perms_removed,
        sig_flags,
    }) = decls
        .iter()
        .find(|d| matches!(d, Declaration::UsageRightsRemoved { .. }))
    else {
        return notes.is_empty();
    };
    let mut allowed: Vec<String> = if *perms_removed {
        vec!["/Root/Perms".into()]
    } else {
        keys.iter().map(|k| format!("/Root/Perms/{k}")).collect()
    };
    if sig_flags.is_some() {
        allowed.push("/Root/AcroForm/SigFlags".into());
    }
    let path_of = |n: &String| n.split(':').next().unwrap_or("").to_string();
    let seen: Vec<String> = notes.iter().map(path_of).collect();
    !notes.is_empty()
        && seen.iter().all(|p| allowed.contains(p))
        && allowed.iter().all(|p| seen.contains(p))
}

/// The declarations the runner derives for a default rewrite from its
/// own reading of the input: T1 when the document carries
/// `/OCProperties` and declares less than 1.5 (header, or the catalog
/// `/Version` when later).
pub fn derive_default(doc: &Document, bytes: &[u8]) -> Vec<Declaration> {
    let cat = doc.catalog();
    let has_oc = match cat.get(b"OCProperties".as_slice()) {
        None | Some(Object::Null) => false,
        Some(o @ Object::Ref(_)) => !matches!(doc.deref(o), Ok(Object::Null)),
        Some(_) => true,
    };
    if !has_oc {
        return vec![];
    }
    let Some(v) = super::pdf20::effective_version(doc, bytes) else {
        return vec![];
    };
    if v < (1, 5) {
        vec![Declaration::VersionRaised {
            from: v,
            to: (1, 5),
            reason: VersionReason::OptionalContent,
        }]
    } else {
        vec![]
    }
}
