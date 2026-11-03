//! The library's `RewriteReport` (`rewrite_with`, D2) checked against the
//! runner's own inventory of the input (`semantic::inventory`), item by
//! item, and its output objects against the rewritten file
//! (`plans/rewrite-policy.md` §3.5, §13). The report is never taken as
//! the truth: a disagreement is recorded, with field names and positions
//! only.

use kasane_pdf::{ActionKind, Document, IndirectRef, Object, RewriteReport, ScriptSize};
use serde::{Deserialize, Serialize};

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Counts of a report, per class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSummary {
    pub script: usize,
    pub external: usize,
    pub state: usize,
    pub navigation: usize,
    pub multimedia: usize,
    pub unknown: usize,
    /// Structures the library could not classify.
    pub unclassified: usize,
}

pub fn summary(r: &RewriteReport) -> ReportSummary {
    let s = r.executable_summary();
    ReportSummary {
        script: s.script,
        external: s.external,
        state: s.state,
        navigation: s.navigation,
        multimedia: s.multimedia,
        unknown: s.unknown,
        unclassified: r.unclassified().len(),
    }
}

/// The comparison of a report with the runner's inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryCheck {
    pub library_items: usize,
    pub runner_items: usize,
    /// Every item equal in class, kind, owner, position, sharing, script
    /// size and input object, and every output object holding the same
    /// kind of action.
    pub agree: bool,
    /// The first disagreements: `item <i>: <field>,…` or `items <n> vs
    /// <m>` — positions and field names only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatches: Vec<String>,
    pub output_objects_checked: usize,
    pub output_mismatches: usize,
    /// Script streams the library decoded and the runner's own decoder
    /// (a restricted set of filters) could not: their sizes are not
    /// verified, not counted as a disagreement.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unverified_scripts: usize,
    /// The runner's inventory could not be taken (its own limits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_error: Option<String>,
}

/// Kinds the library names (anything else it reports as `#other`).
const KNOWN: [&str; 25] = [
    "JavaScript",
    "Launch",
    "URI",
    "GoToR",
    "GoToE",
    "SubmitForm",
    "ImportData",
    "ResetForm",
    "Hide",
    "SetOCGState",
    "Trans",
    "Named",
    "GoTo",
    "GoToDp",
    "Thread",
    "GoTo3DView",
    "Rendition",
    "Movie",
    "Sound",
    "RichMediaExecute",
    "RichMedia",
    "Screen",
    "3D",
    "#xfa-script",
    "#malformed",
];

pub fn check(report: &RewriteReport, input: &Document, output: &Document) -> InventoryCheck {
    let lib = report.executable_content();
    let mut c = InventoryCheck {
        library_items: lib.len(),
        ..Default::default()
    };
    let inv = match crate::semantic::inventory(input) {
        Ok(v) => v,
        Err(e) => {
            c.runner_error = Some(e);
            return c;
        }
    };
    c.runner_items = inv.len();
    if lib.len() != inv.len() {
        c.mismatches
            .push(format!("items {} vs {}", lib.len(), inv.len()));
    }
    let label = |owner: String, pos: u32| format!("{owner}@{pos}");
    for (i, (l, r)) in lib.iter().zip(inv.iter()).enumerate() {
        let mut fields = Vec::new();
        if l.class.as_str()
            != serde_json::to_value(r.class)
                .unwrap()
                .as_str()
                .unwrap_or("")
        {
            fields.push("class");
        }
        let rk = if r.kind == "-" || r.kind == "#unresolved" || KNOWN.contains(&r.kind.as_str()) {
            r.kind.as_str()
        } else {
            "#other"
        };
        if l.kind.as_str() != rk {
            fields.push("kind");
        }
        if l.owner.to_string() != r.owner {
            fields.push("owner");
        }
        if l.chain_position != r.position {
            fields.push("position");
        }
        let ls = l
            .shared_with
            .and_then(|k| lib.get(k))
            .map(|f| label(f.owner.to_string(), f.chain_position));
        let rs = r
            .shared_with
            .and_then(|k| inv.get(k))
            .map(|f| label(f.owner.clone(), f.position));
        if ls != rs {
            fields.push("shared_with");
        }
        let script_ok = match (l.script, r.script_bytes, r.script_undecodable) {
            (Some(ScriptSize::Bytes(a)), Some(b), false) => a == b,
            (Some(ScriptSize::Bytes(_)), None, true) => {
                c.unverified_scripts += 1;
                true
            }
            (Some(ScriptSize::Undecodable), None, true) => true,
            (None, None, false) => true,
            _ => false,
        };
        if !script_ok {
            fields.push("script");
        }
        if l.input_object.map(|r| (r.number, r.generation)) != r.input {
            fields.push("input");
        }
        if !fields.is_empty() && c.mismatches.len() < 5 {
            c.mismatches.push(format!("item {i}: {}", fields.join(",")));
        }
    }
    for it in lib {
        let (Some(i), Some(o)) = (it.input_object, it.output_object) else {
            continue;
        };
        c.output_objects_checked += 1;
        if !same_kind(it.kind, input, i, output, o) {
            c.output_mismatches += 1;
        }
    }
    c.agree = c.mismatches.is_empty() && c.output_mismatches == 0;
    c
}

/// The output object holds the same kind of thing as the input object:
/// an action with the same `/S`, an annotation with the same
/// `/Subtype`, a stream for an XFA packet.
fn same_kind(
    kind: ActionKind,
    a: &Document,
    ai: IndirectRef,
    b: &Document,
    bi: IndirectRef,
) -> bool {
    let (Ok(x), Ok(y)) = (a.resolve(ai), b.resolve(bi)) else {
        return false;
    };
    let key: &[u8] = match kind {
        ActionKind::XfaScript => {
            return matches!(
                (&x, &y),
                (Object::Stream { data: d1, .. }, Object::Stream { data: d2, .. }) if d1.len() == d2.len()
            )
        }
        ActionKind::MultimediaAnnotation(_) => b"Subtype",
        _ => b"S",
    };
    // The value through its reference (an indirect /S renumbered).
    let value = |doc: &Document, d: &kasane_pdf::Dict| d.get(key).and_then(|v| doc.deref(v).ok());
    match (&x, &y) {
        (Object::Dict(d1), Object::Dict(d2)) => value(a, d1) == value(b, d2),
        _ => std::mem::discriminant(&x) == std::mem::discriminant(&y),
    }
}

/// The report's version change against the runner's T1 declaration
/// (`semantic::transform::derive_default`, read from the input itself).
pub fn version_agrees(report: &RewriteReport, input: &Document, bytes: &[u8]) -> bool {
    use crate::semantic::transform::Declaration;
    let v = report.version();
    let to32 = |(a, b): (u8, u8)| (a as u32, b as u32);
    match crate::semantic::transform::derive_default(input, bytes).as_slice() {
        [Declaration::VersionRaised { from, to, .. }] => {
            v.reason == Some(kasane_pdf::VersionReason::OptionalContent)
                && to32(v.declared) == *from
                && to32(v.written) == *to
        }
        [] => v.reason.is_none() && v.declared == v.written,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasane_pdf::{rewrite_with, ExecutableContentPolicy, RewriteOptions};

    fn pdf(header: &str, cat: &str, page: &str) -> Vec<u8> {
        let objs = [
            format!("<< /Type /Catalog /Pages 2 0 R {cat} >>"),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] {page} >>"),
            "<< /S /JavaScript /JS (app.alert(1)) /Next [<< /S /URI /URI (u) >> 4 0 R] >>"
                .to_string(),
            "<< /Type /Annot /Subtype /Screen /Rect [0 0 1 1] /A 4 0 R >>".to_string(),
        ];
        let mut out = format!("%PDF-{header}\n").into_bytes();
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
        out
    }

    fn carry(b: &[u8]) -> (Document, kasane_pdf::RewriteOutput) {
        let d = Document::from_bytes(b).unwrap();
        let opts = RewriteOptions::new().with_executable_content(ExecutableContentPolicy::Carry);
        let out = rewrite_with(&d, &opts).unwrap();
        (d, out)
    }

    #[test]
    fn the_report_agrees_with_the_runner_item_by_item() {
        let b = pdf(
            "1.7",
            "/AA << /WC 4 0 R >> /OpenAction 4 0 R",
            "/AA << /O 4 0 R >> /Annots [5 0 R]",
        );
        let (d, out) = carry(&b);
        let o = Document::from_bytes(out.bytes()).unwrap();
        let c = check(out.report(), &d, &o);
        assert!(c.agree, "{c:?}");
        // OpenAction: JS, URI (the cycle back to 4 ends it); then each
        // other owner shares the first JS action; the Screen annotation.
        assert_eq!(c.library_items, 6, "{c:?}");
        assert_eq!(c.output_objects_checked, 5, "{c:?}");
        assert!(version_agrees(out.report(), &d, &b));
    }

    #[test]
    fn a_disagreement_is_seen() {
        // The report of one document against the inventory of another.
        let (_, out) = carry(&pdf("1.7", "/AA << /WC 4 0 R >>", "/AA << /O 4 0 R >>"));
        let other =
            Document::from_bytes(&pdf("1.7", "", "/AA << /O 4 0 R >> /Annots [5 0 R]")).unwrap();
        let c = check(
            out.report(),
            &other,
            &Document::from_bytes(out.bytes()).unwrap(),
        );
        assert!(!c.agree);
        assert!(!c.mismatches.is_empty(), "{c:?}");
        // T1: a 1.4 document with optional content is raised; a report
        // that says nothing changed does not agree.
        let oc = pdf("1.4", "/OCProperties << /OCGs [] /D << >> >>", "");
        let (d, out) = carry(&oc);
        assert!(version_agrees(out.report(), &d, &oc));
        let (_, plain) = carry(&pdf("1.4", "", ""));
        assert!(!version_agrees(plain.report(), &d, &oc));
    }
}

#[cfg(test)]
mod xfa_tests {
    #[test]
    fn utf16_with_leading_white_space_is_read_as_the_library_reads_it() {
        let wide: Vec<u8> = "  \n<template><script/></template>"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let objs = format!("<< /Length {} >>\nstream\n", wide.len());
        let mut s = objs.into_bytes();
        s.extend_from_slice(&wide);
        s.extend_from_slice(b"\nendstream");
        let bodies: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] /XFA [(t) 4 0 R] >> >>"
                .to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>".to_vec(),
            s,
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in bodies.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
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
        let d = kasane_pdf::Document::from_bytes(&out).unwrap();
        let opts = kasane_pdf::RewriteOptions::new()
            .with_executable_content(kasane_pdf::ExecutableContentPolicy::Carry);
        let o = kasane_pdf::rewrite_with(&d, &opts).unwrap();
        assert_eq!(o.report().executable_summary().script, 1);
        let c = super::check(
            o.report(),
            &d,
            &kasane_pdf::Document::from_bytes(o.bytes()).unwrap(),
        );
        assert!(c.agree, "{c:?}");
    }
}
