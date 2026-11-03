//! Rewrite configurations measured side by side (`plans/rewrite-policy.md`
//! §7, "Runner design for the four figures").
//!
//! The default rewrite is the enforced run and keeps its fields of the
//! file record (`rewrite`, `roundtrip`). Every other configuration is
//! recorded in its own namespace (`rewrite_options[<name>]`), so it can
//! never change the default's figures or the baseline's format.
//!
//! Only `default` exists in the library today (`rewrite`); the D2 and D1
//! options do not, and the runner refuses to start a run that asks for
//! them rather than imitating them. When `rewrite_with` lands, each
//! configuration's [`RewriteConfig::rewrite`] arm calls it with that
//! option set.

use crate::roundtrip::RoundTrip;
use crate::semantic::transform::{self, Declaration};
use kasane_pdf::{Document, Error, ExecutableContentPolicy, Object, RewriteOptions, RewriteReport};
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
        match self {
            RewriteConfig::Default => None,
            RewriteConfig::Carry => None,
            RewriteConfig::RemoveUsageRights | RewriteConfig::CarryRemoveUsageRights => {
                Some("the library has no UsageRightsPolicy::Remove yet (D1 is not implemented)")
            }
        }
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
            RewriteConfig::Carry => {
                let opts =
                    RewriteOptions::new().with_executable_content(ExecutableContentPolicy::Carry);
                Ok(kasane_pdf::rewrite_with(doc, &opts).map(|o| {
                    let (b, r) = o.into_parts();
                    (b, Some(r))
                }))
            }
            other => Err(other.unavailable().unwrap_or("not available")),
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
    fn option_sets_parse_and_only_default_runs() {
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
            assert!(c.unavailable().is_some());
            assert!(c.rewrite(&d).is_err(), "{c:?} must not run a substitute");
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
}
