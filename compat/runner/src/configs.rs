//! Rewrite configurations measured side by side (`plans/rewrite-policy.md`
//! §7, "Runner design for the four figures").
//!
//! The default rewrite is the enforced run and keeps its fields of the
//! file record (`rewrite`, `roundtrip`). Every other configuration is
//! recorded in its own namespace (`rewrite_options[<name>]`), so it can
//! never change the default's figures or the baseline's format.
//!
//! Each non-default configuration calls `rewrite_with` with its option
//! set; a library without one would make the runner refuse the run
//! rather than imitate it ([`RewriteConfig::unavailable`]).

use crate::roundtrip::RoundTrip;
use crate::semantic::transform::{self, Declaration};
use kasane_pdf::{
    Document, Error, ExecutableContentPolicy, Object, RewriteOptions, RewriteReport,
    UsageRightsKey, UsageRightsOutcome, UsageRightsPolicy,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RewriteConfig {
    /// `rewrite` (= `rewrite_with` with the default options).
    Default,
    /// D2: `ExecutableContentPolicy::Carry`.
    Carry,
    /// D1: `UsageRightsPolicy::Remove`.
    RemoveUsageRights,
    /// D1 + D2.
    CarryRemoveUsageRights,
}

impl RewriteConfig {
    #[cfg(test)]
    pub const ALL: [RewriteConfig; 4] = [
        RewriteConfig::Default,
        RewriteConfig::Carry,
        RewriteConfig::RemoveUsageRights,
        RewriteConfig::CarryRemoveUsageRights,
    ];

    /// The name used on the command line and as the record's key.
    pub fn name(self) -> &'static str {
        match self {
            RewriteConfig::Default => "default",
            RewriteConfig::Carry => "carry",
            RewriteConfig::RemoveUsageRights => "remove-usage-rights",
            RewriteConfig::CarryRemoveUsageRights => "carry,remove-usage-rights",
        }
    }

    /// Parse an option set (`--rewrite-options carry,remove-usage-rights`;
    /// the order of the options does not matter).
    pub fn parse(s: &str) -> Option<RewriteConfig> {
        let mut opts: Vec<&str> = s
            .split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .collect();
        opts.sort_unstable();
        opts.dedup();
        match opts.as_slice() {
            [] | ["default"] => Some(RewriteConfig::Default),
            ["carry"] => Some(RewriteConfig::Carry),
            ["remove-usage-rights"] => Some(RewriteConfig::RemoveUsageRights),
            ["carry", "remove-usage-rights"] => Some(RewriteConfig::CarryRemoveUsageRights),
            _ => None,
        }
    }

    /// Why this library build cannot run the configuration, `None` when
    /// it can.
    pub fn unavailable(self) -> Option<&'static str> {
        None
    }

    /// The `rewrite_with` options of a non-default configuration.
    fn options(self) -> RewriteOptions {
        let carry = matches!(
            self,
            RewriteConfig::Carry | RewriteConfig::CarryRemoveUsageRights
        );
        let mut o = RewriteOptions::new();
        if carry {
            o = o.with_executable_content(ExecutableContentPolicy::Carry);
        }
        if self.removes_usage_rights() {
            o = o.with_usage_rights(UsageRightsPolicy::Remove);
        }
        o
    }

    /// Rewrite `doc` under this configuration, with the library's report
    /// when it has one; `Err(why)` when the library cannot (never a
    /// substitute rewrite). The default is `rewrite` itself (the enforced
    /// run; `rewrite_with`'s default is checked against it separately).
    #[allow(clippy::type_complexity)]
    pub fn rewrite(
        self,
        doc: &Document,
    ) -> Result<Result<(Vec<u8>, Option<RewriteReport>), Error>, &'static str> {
        match self {
            RewriteConfig::Default => Ok(kasane_pdf::rewrite(doc).map(|b| (b, None))),
            other => match other.unavailable() {
                Some(why) => Err(why),
                None => Ok(kasane_pdf::rewrite_with(doc, &other.options()).map(|o| {
                    let (b, r) = o.into_parts();
                    (b, Some(r))
                })),
            },
        }
    }

    pub fn removes_usage_rights(self) -> bool {
        matches!(
            self,
            RewriteConfig::RemoveUsageRights | RewriteConfig::CarryRemoveUsageRights
        )
    }

    /// The transformations a successful rewrite under this configuration
    /// makes, from the runner's own reading of the input: T1 for every
    /// configuration, T2 under `remove-usage-rights`.
    pub fn declarations(self, doc: &Document, bytes: &[u8]) -> Vec<Declaration> {
        let mut d = transform::derive_default(doc, bytes);
        if self.removes_usage_rights() {
            d.extend(derive_usage_rights(doc));
        }
        d
    }
}

/// T2 as the runner reads the input (`plans/rewrite-policy.md` §5.3):
/// the `/UR` and `/UR3` entries of a direct-or-indirect catalog
/// `/Perms`, `/Perms` dropped when nothing else is in it, `/SigFlags`
/// bit 1 cleared when set. `None` when there is nothing to remove —
/// whether the removal is *allowed* is the library's decision, and the
/// declaration checks what it did.
pub fn derive_usage_rights(doc: &Document) -> Option<Declaration> {
    let cat = doc.catalog();
    let perms = cat
        .get(b"Perms".as_slice())
        .and_then(|p| doc.deref(p).ok())?;
    let Object::Dict(perms) = perms else {
        return None;
    };
    let keys: Vec<String> = ["UR", "UR3"]
        .into_iter()
        .filter(|k| perms.contains_key(k.as_bytes()))
        .map(String::from)
        .collect();
    if keys.is_empty() {
        return None;
    }
    let sig_flags = cat
        .get(b"AcroForm".as_slice())
        .and_then(|a| doc.deref(a).ok())
        .and_then(|a| match a {
            Object::Dict(a) => a
                .get(b"SigFlags".as_slice())
                .and_then(|f| doc.deref(f).ok()),
            _ => None,
        })
        .and_then(|f| match f {
            Object::Integer(f) if f & 1 == 1 => Some((f, f & !1)),
            _ => None,
        });
    Some(Declaration::UsageRightsRemoved {
        perms_removed: perms.len() == keys.len(),
        keys,
        sig_flags,
    })
}

/// Why a rewrite was refused, by the policy that refused it, as the
/// library classifies it (`Error::refusal_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// A signature (`SignaturePolicy`, or a signed signature field).
    Signature,
    /// Executable content: catalog `/AA`, a JavaScript `/OpenAction`,
    /// `/Names /JavaScript`.
    Script,
    /// Catalog `/RichMedia`.
    Multimedia,
    Encrypted,
    /// An external action (`RefuseScriptsAndExternal`).
    External,
    /// An unknown action or an unclassified structure (`Carry`,
    /// `RefuseScriptsAndExternal`).
    UnknownAction,
    /// Any other unsupported feature.
    Unsupported,
}

pub fn refusal(e: &Error) -> Option<Refusal> {
    use kasane_pdf::RefusalReason as R;
    match e.refusal_reason() {
        Some(R::Signature) => Some(Refusal::Signature),
        Some(R::Encrypted) => Some(Refusal::Encrypted),
        Some(R::DocumentLevelScript | R::Script) => Some(Refusal::Script),
        Some(R::ExternalAction) => Some(Refusal::External),
        Some(R::UnknownAction | R::UnclassifiedStructure) => Some(Refusal::UnknownAction),
        Some(R::Multimedia) => Some(Refusal::Multimedia),
        Some(_) => Some(Refusal::Unsupported),
        None => matches!(e, Error::Unsupported { .. }).then_some(Refusal::Unsupported),
    }
}

/// One non-default configuration's result for a file.
#[derive(Debug, Serialize, Deserialize)]
pub struct OptionRun {
    /// `pass`, `unsupported` (refused), `error`.
    pub rewrite: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
    /// The declared transformations' rule names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<String>,
    /// Why the configuration could not run (`rewrite` is then `error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// The round trip under this configuration (same record as the
    /// default's).
    #[serde(default)]
    pub roundtrip: Option<RoundTrip>,
    /// The library's report, per class (`rewrite_with` configurations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<crate::policy::ReportSummary>,
    /// The report against the runner's own inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<crate::policy::InventoryCheck>,
    /// D1 configurations: what the library did with usage-rights
    /// signatures, against the runner's own reading of the input and the
    /// output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_rights: Option<UsageRightsCheck>,
}

/// A D1 configuration's usage-rights result, checked against the
/// runner's T2 declaration and its `signatures` snapshots (never taken
/// as the truth). Content-free: keys, counts, paths' kinds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRightsCheck {
    /// `removed`, `none_present`, or `refused` (`rule` says why).
    pub outcome: String,
    /// The library's `SignatureRule` when it refused with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// Removed entries (`UR`, `UR3`), in the report's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perms_removed: Option<bool>,
    /// `/SigFlags` before and after, as the library reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_flags: Option<(i64, i64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects_removed: Option<usize>,
    /// The runner's `signatures` snapshot of the input: signature-like
    /// dictionaries, those under `/Perms /UR` or `/UR3`, field locks.
    pub input_sigs: usize,
    pub input_usage_rights_sigs: usize,
    pub input_locks: usize,
    /// ... and of the output (a successful rewrite only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sigs: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_locks: Option<usize>,
    /// The report agrees with the runner's declaration and snapshots
    /// (successful rewrites), or the snapshot shows what the refusal
    /// names (an `OtherSignaturePresent` refusal); `None` when there is
    /// nothing to compare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agree: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatches: Vec<String>,
}

/// The runner's own counts from a `signatures` snapshot: `(sigs, under
/// /Perms /UR or /UR3, locks)`. A snapshot that could not be taken is
/// `None`.
fn signature_counts(doc: &Document, bytes: &[u8]) -> Option<(usize, usize, usize)> {
    let lines = crate::semantic::signature_lines(doc, bytes).ok()?;
    let sigs: Vec<&String> = lines.iter().filter(|l| l.starts_with("sig\t")).collect();
    let ur = sigs
        .iter()
        .filter(|l| {
            let at = l.split('\t').nth(1).unwrap_or("");
            // The target itself (indirect, first reached from /Perms) or
            // a direct dictionary in /Perms.
            matches!(at, "at=/Root/Perms/UR" | "at=/Root/Perms/UR3")
        })
        .count();
    let locks = lines.iter().filter(|l| l.starts_with("lock\t")).count();
    Some((sigs.len(), ur, locks))
}

/// The usage-rights record of a D1 run: `result` is the rewrite's
/// (`Ok((output bytes, report))` or the refusal), `input` the input as
/// parsed, `data` its bytes.
pub fn usage_rights_check(
    input: &Document,
    data: &[u8],
    result: Result<(&[u8], &RewriteReport), &Error>,
    decls: &[Declaration],
) -> UsageRightsCheck {
    let mut c = UsageRightsCheck::default();
    let mut miss = Vec::new();
    match signature_counts(input, data) {
        Some((s, u, l)) => (c.input_sigs, c.input_usage_rights_sigs, c.input_locks) = (s, u, l),
        None => miss.push("the input's signatures snapshot could not be taken".to_string()),
    }
    match result {
        Err(e) => {
            c.outcome = "refused".into();
            if let Error::SignaturePolicy { rule, .. } = e {
                c.rule = Some(format!("{rule:?}"));
                if matches!(rule, kasane_pdf::SignatureRule::OtherSignaturePresent) {
                    // Another signature, or a lock, is what the runner sees too.
                    let explained = c.input_locks > 0 || c.input_sigs > c.input_usage_rights_sigs;
                    if !explained {
                        miss.push(format!(
                            "refused for another signature; the runner sees {} sig(s), {} under \
                             /Perms /UR, /UR3, {} lock(s)",
                            c.input_sigs, c.input_usage_rights_sigs, c.input_locks
                        ));
                    }
                    c.agree = Some(explained);
                }
            }
        }
        Ok((out, report)) => {
            match Document::from_bytes(out)
                .ok()
                .and_then(|o| signature_counts(&o, out))
            {
                Some((s, _, l)) => (c.output_sigs, c.output_locks) = (Some(s), Some(l)),
                None => miss.push("the output's signatures snapshot could not be taken".into()),
            }
            if c.output_sigs.unwrap_or(0) > 0 || c.output_locks.unwrap_or(0) > 0 {
                miss.push("the output has a signature-like dictionary or a lock".into());
            }
            let declared = decls.iter().find_map(|d| match d {
                Declaration::UsageRightsRemoved {
                    keys,
                    perms_removed,
                    sig_flags,
                } => Some((keys, *perms_removed, *sig_flags)),
                _ => None,
            });
            match report.usage_rights() {
                UsageRightsOutcome::Removed(r) => {
                    c.outcome = "removed".into();
                    c.removed = r
                        .entries
                        .iter()
                        .map(|e| match e.key {
                            UsageRightsKey::Ur => "UR".to_string(),
                            UsageRightsKey::Ur3 => "UR3".to_string(),
                            _ => "other".to_string(),
                        })
                        .collect();
                    c.perms_removed = Some(r.perms_removed);
                    c.sig_flags = r.sig_flags.map(|f| (f.before, f.after));
                    c.objects_removed = Some(r.output_check.objects_removed);
                    match declared {
                        None => miss.push("removed, but the runner declares no T2".into()),
                        Some((keys, perms, flags)) => {
                            if *keys != c.removed {
                                miss.push(format!("keys: runner {keys:?}"));
                            }
                            if perms != r.perms_removed {
                                miss.push(format!("perms_removed: runner {perms}"));
                            }
                            let lib_flags = c.sig_flags.filter(|(b, a)| b != a);
                            if flags != lib_flags {
                                miss.push(format!(
                                    "sig_flags: runner {flags:?}, library {lib_flags:?}"
                                ));
                            }
                        }
                    }
                    if c.input_sigs != r.entries.len()
                        || c.input_usage_rights_sigs != r.entries.len()
                        || c.input_locks > 0
                    {
                        miss.push(format!(
                            "the runner sees {} sig(s), {} under /Perms /UR, /UR3, {} lock(s) for \
                             {} removed",
                            c.input_sigs,
                            c.input_usage_rights_sigs,
                            c.input_locks,
                            r.entries.len()
                        ));
                    }
                }
                UsageRightsOutcome::NonePresent => {
                    c.outcome = "none_present".into();
                    if declared.is_some() {
                        miss.push("nothing removed, but the runner declares T2".into());
                    }
                    if c.input_sigs > 0 || c.input_locks > 0 {
                        miss.push(format!(
                            "nothing removed, but the runner sees {} sig(s), {} lock(s)",
                            c.input_sigs, c.input_locks
                        ));
                    }
                }
                other => miss.push(format!("outcome {other:?} under Remove")),
            }
            c.agree = Some(miss.is_empty());
        }
    }
    c.mismatches = miss;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(catalog_extra: &str, extra: &str) -> Document {
        let objs = [
            format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"),
            "<< /Type /Pages /Kids [] /Count 0 >>".to_string(),
            extra.to_string(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
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
        Document::from_bytes(&out).unwrap()
    }

    #[test]
    fn option_sets_parse_and_every_configuration_runs() {
        for c in RewriteConfig::ALL {
            assert_eq!(RewriteConfig::parse(c.name()), Some(c));
        }
        assert_eq!(
            RewriteConfig::parse("remove-usage-rights, carry"),
            Some(RewriteConfig::CarryRemoveUsageRights)
        );
        assert_eq!(RewriteConfig::parse("sanitize"), None);
        assert_eq!(RewriteConfig::parse("carry,carry,x"), None);
        let d = doc("", "<< >>");
        assert!(RewriteConfig::Default
            .rewrite(&d)
            .unwrap()
            .unwrap()
            .1
            .is_none());
        let (_, report) = RewriteConfig::Carry.rewrite(&d).unwrap().unwrap();
        assert!(report.is_some(), "carry runs rewrite_with");
        for c in &RewriteConfig::ALL[2..] {
            assert!(c.unavailable().is_none());
            let (_, r) = c.rewrite(&d).unwrap().unwrap();
            let r = r.unwrap();
            assert_eq!(r.usage_rights_policy(), UsageRightsPolicy::Remove);
            assert_eq!(r.usage_rights(), &UsageRightsOutcome::NonePresent);
            let carry = r.executable_content_policy() == ExecutableContentPolicy::Carry;
            assert_eq!(carry, *c == RewriteConfig::CarryRemoveUsageRights);
        }
    }

    #[test]
    fn usage_rights_declaration_from_the_input() {
        let ur3 = "<< /Type /Sig /ByteRange [0 1 2 3] >>";
        let d = doc(
            "/Perms << /UR3 3 0 R >> /AcroForm << /Fields [] /SigFlags 2 >>",
            ur3,
        );
        assert_eq!(
            derive_usage_rights(&d),
            Some(Declaration::UsageRightsRemoved {
                keys: vec!["UR3".into()],
                perms_removed: true,
                sig_flags: None
            })
        );
        let d = doc(
            "/Perms << /UR3 3 0 R /DocMDP 3 0 R >> /AcroForm << /SigFlags 3 >>",
            ur3,
        );
        assert_eq!(
            derive_usage_rights(&d),
            Some(Declaration::UsageRightsRemoved {
                keys: vec!["UR3".into()],
                perms_removed: false,
                sig_flags: Some((3, 2))
            })
        );
        assert_eq!(
            derive_usage_rights(&doc("/Perms << /DocMDP 3 0 R >>", ur3)),
            None
        );
        assert_eq!(derive_usage_rights(&doc("", ur3)), None);
        // Only the D1 configurations declare it.
        let d = doc("/Perms << /UR 3 0 R >>", ur3);
        assert!(RewriteConfig::Carry
            .declarations(&d, b"%PDF-1.7")
            .is_empty());
        assert_eq!(
            RewriteConfig::RemoveUsageRights
                .declarations(&d, b"%PDF-1.7")
                .len(),
            1
        );
    }

    #[test]
    fn refusals_by_policy() {
        let u = |feature: &'static str, detail: &str| Error::Unsupported {
            feature,
            detail: detail.into(),
        };
        assert_eq!(
            refusal(&u("JavaScript name tree", "")),
            Some(Refusal::Script)
        );
        assert_eq!(
            refusal(&u("OpenAction JavaScript", "")),
            Some(Refusal::Script)
        );
        assert_eq!(
            refusal(&u(
                "catalog entry outside profile",
                "/AA is outside the rewrite profile"
            )),
            Some(Refusal::Script)
        );
        assert_eq!(
            refusal(&u(
                "catalog entry outside profile",
                "/RichMedia is outside the rewrite profile"
            )),
            Some(Refusal::Multimedia)
        );
        assert_eq!(
            refusal(&u("signed signature field", "")),
            Some(Refusal::Signature)
        );
        // An XFA packet that cannot be checked for an XML signature is a
        // signature refusal (the library's classification).
        assert_eq!(refusal(&u("XFA packet", "")), Some(Refusal::Signature));
        assert_eq!(
            refusal(&u("writing an encrypted document", "")),
            Some(Refusal::Encrypted)
        );
        assert_eq!(refusal(&u("PDF version", "")), Some(Refusal::Unsupported));
        assert_eq!(refusal(&Error::Encrypted), Some(Refusal::Encrypted));
        assert_eq!(refusal(&Error::Parse("x".into())), None);
    }

    const UR3: &str = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> \
        /Reference [<< /TransformMethod /UR3 /TransformParams << /Form [/FillIn] >> >>] >>";

    /// The cross-check of a D1 run: a removal the runner reads the same
    /// way agrees; a refusal for another signature is explained by the
    /// runner's own snapshot.
    #[test]
    fn usage_rights_checks_against_the_runner() {
        let bytes = |cat: &str, extra: &str| {
            let objs = [
                format!("<< /Type /Catalog /Pages 2 0 R {cat} >>"),
                "<< /Type /Pages /Kids [] /Count 0 >>".to_string(),
                extra.to_string(),
            ];
            let mut out = b"%PDF-1.7\n".to_vec();
            let mut offs = Vec::new();
            for (i, o) in objs.iter().enumerate() {
                offs.push(out.len());
                out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
            }
            let x = out.len();
            out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
            for o in offs {
                out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
            }
            out.extend_from_slice(
                format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
            );
            out
        };
        let run = |b: &[u8]| {
            let d = Document::from_bytes(b).unwrap();
            let decls = RewriteConfig::RemoveUsageRights.declarations(&d, b);
            let r = RewriteConfig::RemoveUsageRights.rewrite(&d).unwrap();
            let input = Document::from_bytes(b).unwrap();
            match &r {
                Ok((out, rep)) => {
                    usage_rights_check(&input, b, Ok((out, rep.as_ref().unwrap())), &decls)
                }
                Err(e) => usage_rights_check(&input, b, Err(e), &decls),
            }
        };
        let b = bytes(
            "/Perms << /UR3 3 0 R >> /AcroForm << /Fields [] /SigFlags 3 >>",
            UR3,
        );
        let c = run(&b);
        assert_eq!(
            (c.outcome.as_str(), c.agree),
            ("removed", Some(true)),
            "{c:?}"
        );
        assert_eq!(c.removed, vec!["UR3".to_string()]);
        assert_eq!((c.sig_flags, c.perms_removed), (Some((3, 2)), Some(true)));
        assert_eq!(
            (c.input_sigs, c.input_usage_rights_sigs, c.input_locks),
            (1, 1, 0)
        );
        assert_eq!((c.output_sigs, c.output_locks), (Some(0), Some(0)));
        // Another signature: refused, and the runner sees it too.
        let b = bytes("/Perms << /UR3 3 0 R >> /X << /Type /DocTimeStamp >>", UR3);
        let c = run(&b);
        assert_eq!(c.outcome, "refused");
        assert_eq!(c.rule.as_deref(), Some("OtherSignaturePresent"));
        assert_eq!(
            (c.input_sigs, c.input_usage_rights_sigs, c.agree),
            (2, 1, Some(true))
        );
        // A lock.
        let b = bytes(
            "/Perms << /UR3 3 0 R >> /AcroForm << /Fields [<< /FT /Tx /T (a) /Lock << /Action /All >> >>] >>",
            UR3,
        );
        let c = run(&b);
        assert_eq!(
            (c.rule.as_deref(), c.input_locks, c.agree),
            (Some("OtherSignaturePresent"), 1, Some(true))
        );
        // Nothing to remove.
        let c = run(&bytes("", "<< >>"));
        assert_eq!((c.outcome.as_str(), c.agree), ("none_present", Some(true)));
        // A declaration that disagrees with the report is recorded.
        let b = bytes("/Perms << /UR3 3 0 R >>", UR3);
        let d = Document::from_bytes(&b).unwrap();
        let (out, rep) = RewriteConfig::RemoveUsageRights
            .rewrite(&d)
            .unwrap()
            .unwrap();
        let wrong = [Declaration::UsageRightsRemoved {
            keys: vec!["UR".into()],
            perms_removed: false,
            sig_flags: Some((1, 0)),
        }];
        let c = usage_rights_check(&d, &b, Ok((&out, rep.as_ref().unwrap())), &wrong);
        assert_eq!(c.agree, Some(false));
        assert_eq!(c.mismatches.len(), 3, "{:?}", c.mismatches);
    }
}
