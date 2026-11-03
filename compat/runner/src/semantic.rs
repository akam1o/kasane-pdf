//! Semantic round trip: does a rewrite keep what the document *means*?
//!
//! `graph_diff` (roundtrip.rs) compares the object graph after
//! renumbering, so it sees bytes and keys. This module reduces each
//! document to a normalised **snapshot** per category — outlines, named
//! destinations, page labels, structure tree, embedded files, optional
//! content, AcroForm, and the document-level categories of
//! `doclevel.rs` (XFA, actions, JavaScript, metadata, viewer
//! preferences) — that is independent of object numbers, dictionary
//! order, name / number tree layout, string representation (literal vs
//! hex, PDFDocEncoding vs UTF-16, NFC) and of the storage form of a
//! destination (direct array, name, `/Dests` entry or name-tree entry).
//! Two snapshots are compared as multisets of lines.
//!
//! Each category is classified separately:
//! `pass` (same meaning), `fail` (a difference), `unsupported` (the
//! snapshot needs something not implemented), `not_applicable` (absent
//! on both sides) and `error` (the snapshot itself could not be taken:
//! a cycle, a limit, a malformed structure). Only `pass` is success.
//!
//! The snapshot is read by this module from the object graph — it does
//! not use the engine's own readers (`kasane_pdf::named_destinations`
//! …), so a reader bug cannot hide a writer bug symmetrically.
//!
//! Failure notes name the category, the counts and the *fields* that
//! differ — never field values: the run report can be uploaded as a CI
//! artifact, and values are document content.

use kasane_pdf::{Dict, Document, IndirectRef, Object};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

pub use pdf20::compare_report_declared;
#[cfg(test)]
pub use v4::ActionClass;
pub use v4::InventoryItem;

/// The action inventory of `doc` (`v4.rs`): every action reachable from
/// its owners, by class, owner and chain position — what the D2 report
/// is checked against. Content-free.
pub fn inventory(doc: &Document) -> Result<Vec<InventoryItem>, String> {
    let mut ctx = Ctx::new(doc);
    ctx.doclevel = true;
    ctx.inventory().map_err(|e| match e {
        Snag::Unsupported(m) | Snag::Error(m) => m,
    })
}

/// The document-level categories come last: they share the node budget,
/// and the earlier categories' results must not depend on them.
pub const CATEGORIES: [&str; 12] = [
    "outlines",
    "named_dests",
    "page_labels",
    "struct_tree",
    "embedded_files",
    "optional_content",
    "acroform",
    "xfa",
    "doc_actions",
    "javascript",
    "metadata",
    "viewer_prefs",
];

/// Object resolutions one snapshot may perform (all categories).
const MAX_NODES: usize = 200_000;
/// Nesting depth of any tree walked (outlines, struct tree, name trees).
const MAX_DEPTH: usize = 64;
/// Depth of a generic value (action, `/Usage`, field value).
const MAX_VALUE_DEPTH: usize = 12;
/// Decoded size of an embedded file hashed.
const MAX_EMBEDDED_BYTES: usize = 64 << 20;
/// Decoded size of one XFA packet, XMP or script stream hashed.
const MAX_DECODED_BYTES: usize = 64 << 20;
/// Decoded bytes one snapshot may hash in the document-level categories.
const MAX_DECODED_TOTAL: usize = 256 << 20;
/// Nesting of an action chain (`/Next`).
const MAX_ACTION_DEPTH: usize = 32;
/// Length of one snapshot line of the document-level categories.
const MAX_LINE_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SemClass {
    Pass,
    /// Every difference is a transformation the run declared
    /// (`transform.rs`); reported, never counted as `pass`.
    Transformed,
    Fail,
    Unsupported,
    #[default]
    NotApplicable,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemCheck {
    pub category: String,
    pub class: SemClass,
    /// Snapshot lines in the original (its coverage of the category).
    pub items: usize,
    #[serde(default)]
    pub notes: Vec<String>,
    /// The declared rules that explain a `transformed` category.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticRoundTrip {
    /// Overall: fail > error > unsupported > pass > not_applicable.
    pub class: SemClass,
    pub checks: Vec<SemCheck>,
    pub ms: u128,
    /// The report-only categories (`pdf20.rs`): reported per category,
    /// never part of `class`. Empty in older reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub report_only: Vec<SemCheck>,
}

/// One category of a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    Absent,
    Data(Vec<String>),
    Unsupported(String),
    Error(String),
}

enum Snag {
    Unsupported(String),
    Error(String),
}

type Res<T> = Result<T, Snag>;

fn err<T>(m: impl Into<String>) -> Res<T> {
    Err(Snag::Error(m.into()))
}

/// The snapshot of `doc`, one section per entry of `CATEGORIES`.
pub fn snapshot(doc: &Document) -> Vec<Section> {
    let mut ctx = Ctx::new(doc);
    // `outlines` is taken after `struct_tree`: an outline action's
    // structure destination (`/SD`) names its element by the tree path
    // the structure category records (as `doc_actions` does), instead
    // of walking into the tree. Sections keep the CATEGORIES order.
    let mut order: Vec<usize> = (0..CATEGORIES.len()).collect();
    order.retain(|&i| CATEGORIES[i] != "outlines");
    let at = order
        .iter()
        .position(|&i| CATEGORIES[i] == "struct_tree")
        .unwrap()
        + 1;
    order.insert(at, 0);
    let mut sections: Vec<Option<Section>> = vec![None; CATEGORIES.len()];
    for i in order {
        let c = &CATEGORIES[i];
        sections[i] = Some({
            let r = match *c {
                "outlines" => ctx.outlines(),
                "named_dests" => ctx.named_dests_lines(),
                "page_labels" => ctx.page_labels(),
                "struct_tree" => ctx.struct_tree(),
                "embedded_files" => ctx.embedded_files(),
                "optional_content" => ctx.optional_content(),
                "acroform" => ctx.acroform(),
                "xfa" => ctx.strict(Ctx::xfa),
                "doc_actions" => ctx.strict(Ctx::doc_actions),
                "javascript" => ctx.strict(Ctx::javascript),
                "metadata" => ctx.strict(Ctx::metadata),
                "viewer_prefs" => ctx.strict(Ctx::viewer_prefs),
                _ => unreachable!(),
            };
            match r {
                Ok(None) => Section::Absent,
                Ok(Some(mut v)) if !v.is_empty() => {
                    v.sort();
                    Section::Data(v)
                }
                Ok(Some(_)) => Section::Absent,
                Err(Snag::Unsupported(m)) => Section::Unsupported(m),
                Err(Snag::Error(m)) => Section::Error(m),
            }
        });
    }
    sections.into_iter().map(|s| s.unwrap()).collect()
}

/// Compare the snapshots of an original and its rewrite.
pub fn compare(a: &Document, b: &Document) -> SemanticRoundTrip {
    compare_declared(a, b, &[])
}

/// [`compare`] with the run's declared transformations (`transform.rs`);
/// with none, exactly the plain comparison.
pub fn compare_declared(
    a: &Document,
    b: &Document,
    decls: &[transform::Declaration],
) -> SemanticRoundTrip {
    let t = Instant::now();
    let (sa, sb) = (snapshot(a), snapshot(b));
    let checks: Vec<SemCheck> = CATEGORIES
        .iter()
        .zip(sa.iter().zip(sb.iter()))
        .map(|(c, (x, y))| {
            let mut check = compare_section(c, x, y);
            transform::classify(&mut check, x, y, decls);
            check
        })
        .collect();
    let has = |k: SemClass| checks.iter().any(|c| c.class == k);
    let class = [
        SemClass::Fail,
        SemClass::Error,
        SemClass::Unsupported,
        SemClass::Transformed,
        SemClass::Pass,
    ]
    .into_iter()
    .find(|k| has(*k))
    .unwrap_or(SemClass::NotApplicable);
    SemanticRoundTrip {
        class,
        checks,
        ms: t.elapsed().as_millis(),
        report_only: Vec::new(),
    }
}

fn compare_section(category: &str, a: &Section, b: &Section) -> SemCheck {
    let items = match a {
        Section::Data(v) => v.len(),
        _ => 0,
    };
    let (class, notes) = match (a, b) {
        (Section::Absent, Section::Absent) => (SemClass::NotApplicable, vec![]),
        (Section::Error(m), _) => (SemClass::Error, vec![format!("original: {m}")]),
        (_, Section::Error(m)) => (SemClass::Error, vec![format!("rewrite: {m}")]),
        (Section::Unsupported(m), _) => (SemClass::Unsupported, vec![format!("original: {m}")]),
        (_, Section::Unsupported(m)) => (SemClass::Unsupported, vec![format!("rewrite: {m}")]),
        (Section::Data(x), Section::Data(y)) if x == y => (SemClass::Pass, vec![]),
        (Section::Data(x), Section::Data(y)) => (SemClass::Fail, diff_notes(x, y)),
        (Section::Data(x), Section::Absent) => (
            SemClass::Fail,
            vec![format!("{} item(s) lost: the rewrite has none", x.len())],
        ),
        (Section::Absent, Section::Data(y)) => (
            SemClass::Fail,
            vec![format!("{} item(s) added by the rewrite", y.len())],
        ),
    };
    SemCheck {
        category: category.into(),
        class,
        items,
        notes,
        rules: Vec::new(),
    }
}

/// Counts and the names of the fields that differ — never values.
fn diff_notes(x: &[String], y: &[String]) -> Vec<String> {
    let mut count: HashMap<&str, i64> = HashMap::new();
    for l in x {
        *count.entry(l).or_default() += 1;
    }
    for l in y {
        *count.entry(l).or_default() -= 1;
    }
    let only_a: Vec<&str> = x
        .iter()
        .map(|s| s.as_str())
        .filter(|l| count[l] > 0)
        .collect();
    let only_b: Vec<&str> = y
        .iter()
        .map(|s| s.as_str())
        .filter(|l| count[l] < 0)
        .collect();
    let mut notes = vec![format!(
        "{} line(s) only in the original, {} only in the rewrite",
        only_a.len(),
        only_b.len()
    )];
    let fields = |l: &str| -> Vec<(String, String)> {
        l.split('\t')
            .map(|f| match f.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (f.to_string(), String::new()),
            })
            .collect()
    };
    // Pair lines of the same kind and position key, report differing
    // field names.
    let mut differing: Vec<String> = Vec::new();
    for la in only_a.iter().take(20) {
        let fa = fields(la);
        let key = &fa[..fa.len().min(2)];
        if let Some(lb) = only_b.iter().find(|lb| fields(lb).starts_with(key)) {
            for ((ka, va), (kb, vb)) in fa.iter().zip(fields(lb)) {
                if (va != &vb || ka != &kb) && !differing.contains(ka) {
                    differing.push(ka.clone());
                }
            }
        } else {
            let kind = format!("{} missing", fa[0].0);
            if !differing.contains(&kind) {
                differing.push(kind);
            }
        }
    }
    if !differing.is_empty() {
        notes.push(format!("differing: {}", differing.join(", ")));
    }
    notes
}

// ------------------------------------------------------------------
// Value normalisation
// ------------------------------------------------------------------

/// A PDF text string as Unicode (§7.9.2.2: UTF-16BE or UTF-8 with a
/// BOM, else PDFDocEncoding), NFC-normalised. Tabs and newlines are
/// escaped so they cannot break the line format.
pub fn text(b: &[u8]) -> String {
    use unicode_normalization::UnicodeNormalization;
    decode_text(b)
        .nfc()
        .flat_map(|c| match c {
            '\t' => vec!['\\', 't'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            '\\' => vec!['\\', '\\'],
            c => vec![c],
        })
        .collect()
}

/// `text` without the NFC step: the same Unicode in another encoding
/// compares equal, canonically equivalent sequences do not.
fn text_exact(b: &[u8]) -> String {
    decode_text(b)
        .chars()
        .flat_map(|c| match c {
            '\t' => vec!['\\', 't'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            '\\' => vec!['\\', '\\'],
            c => vec![c],
        })
        .collect()
}

/// A PDF text string or text stream as Unicode, exactly (no
/// normalisation): UTF-16 or UTF-8 with a BOM, else PDFDocEncoding.
fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&u)
    } else if b.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&u)
    } else if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(&b[3..]).into_owned()
    } else {
        b.iter().map(|&c| pdfdoc(c)).collect()
    }
}

fn pdfdoc(c: u8) -> char {
    const HI: [u32; 31] = [
        0x2022, 0x2020, 0x2021, 0x2026, 0x2014, 0x2013, 0x0192, 0x2044, 0x2039, 0x203A, 0x2212,
        0x2030, 0x201E, 0x201C, 0x201D, 0x2018, 0x2019, 0x201A, 0x2122, 0xFB01, 0xFB02, 0x0141,
        0x0152, 0x0160, 0x0178, 0x017D, 0x0131, 0x0142, 0x0153, 0x0161, 0x017E,
    ];
    const LO: [u32; 8] = [
        0x02D8, 0x02C7, 0x02C6, 0x02D9, 0x02DD, 0x02DB, 0x02DA, 0x02DC,
    ];
    let u = match c {
        0x18..=0x1F => LO[(c - 0x18) as usize],
        0x80..=0x9E => HI[(c - 0x80) as usize],
        0xA0 => 0x20AC,
        _ => c as u32,
    };
    char::from_u32(u).unwrap_or('\u{FFFD}')
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn num(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else if v.is_finite() {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        "nan".into()
    }
}

fn sha(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().take(12).map(|x| format!("{x:02x}")).collect()
}

fn name(o: &Object) -> Option<String> {
    o.as_name().map(|n| String::from_utf8_lossy(n).into_owned())
}

// ------------------------------------------------------------------
// The walker
// ------------------------------------------------------------------

struct Ctx<'a> {
    doc: &'a Document,
    pages: HashMap<IndirectRef, usize>,
    /// Annotation reference → (page index, position in `/Annots`).
    annots: HashMap<IndirectRef, (usize, usize)>,
    nodes: usize,
    /// Named destinations: (`/Dests` dictionary, `/Names` tree).
    named: Option<Res<Named>>,
    /// `canon` cut a value at `MAX_VALUE_DEPTH` (the document-level
    /// categories turn that into `error`).
    truncated: bool,
    /// Decoded bytes hashed by the document-level categories.
    decoded: usize,
    /// Structure elements by path, from `struct_tree` (which runs
    /// first): the document-level categories name an element a value
    /// points at (a structure destination `/SD`) by its path instead of
    /// expanding its subtree.
    struct_paths: HashMap<IndirectRef, String>,
    /// A document-level category is being taken.
    doclevel: bool,
    /// Action objects met in the current category, by first-visit
    /// position: an action reached again is named by that position, so
    /// sharing is compared and a shared chain is not expanded twice.
    action_ids: HashMap<IndirectRef, usize>,
}

impl<'a> Ctx<'a> {
    fn new(doc: &'a Document) -> Self {
        let mut pages = HashMap::new();
        for (i, p) in doc.pages().iter().enumerate() {
            if let Some(r) = p.object_ref() {
                pages.insert(r, i);
            }
        }
        let mut ctx = Ctx {
            doc,
            pages,
            annots: HashMap::new(),
            nodes: 0,
            named: None,
            truncated: false,
            decoded: 0,
            struct_paths: HashMap::new(),
            doclevel: false,
            action_ids: HashMap::new(),
        };
        for (i, p) in doc.pages().iter().enumerate() {
            let Some(r) = p.object_ref() else { continue };
            let Ok(pd) = doc.resolve(r) else { continue };
            let Some(a) = pd
                .as_dict()
                .and_then(|d| d.get(b"Annots".as_slice()))
                .cloned()
            else {
                continue;
            };
            if let Ok(Object::Array(items)) = doc.deref(&a) {
                for (k, it) in items.iter().enumerate() {
                    if let Some(ar) = it.as_ref() {
                        ctx.annots.entry(ar).or_insert((i, k));
                    }
                }
            }
        }
        ctx
    }

    /// Resolve one level, counting against the node budget.
    fn get(&mut self, o: &Object) -> Res<Object> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return err(format!("snapshot stopped after {MAX_NODES} objects"));
        }
        match o {
            // An unresolvable reference is not a null: a rewrite that
            // loses an object body must not compare equal.
            Object::Ref(_) => Ok(self
                .doc
                .deref(o)
                .unwrap_or_else(|_| Object::Name(b"#unresolved".to_vec()))),
            o => Ok(o.clone()),
        }
    }

    fn dict(&mut self, o: &Object) -> Res<Option<Dict>> {
        Ok(match self.get(o)? {
            Object::Dict(d) => Some(d),
            Object::Stream { dict, .. } => Some(dict),
            _ => None,
        })
    }

    fn page_of(&self, o: Option<&Object>) -> String {
        match o.and_then(|o| o.as_ref()).and_then(|r| self.pages.get(&r)) {
            Some(i) => format!("p{i}"),
            None => match o {
                None => "-".into(),
                Some(Object::Integer(n)) => format!("remote{n}"),
                Some(_) => "?".into(),
            },
        }
    }

    /// A generic value: dictionaries with sorted keys, references
    /// followed (pages and annotations by position; a reference already
    /// on the path is a cycle), streams by content hash.
    fn canon(&mut self, o: &Object, depth: usize, path: &mut Vec<IndirectRef>) -> Res<String> {
        if depth > MAX_VALUE_DEPTH {
            self.truncated = true;
            return Ok("…".into());
        }
        if let Some(r) = o.as_ref() {
            if let Some(i) = self.pages.get(&r) {
                return Ok(format!("page#{i}"));
            }
            if let Some((p, k)) = self.annots.get(&r) {
                return Ok(format!("annot#{p}.{k}"));
            }
            if self.doclevel {
                if let Some(p) = self.struct_paths.get(&r) {
                    return Ok(format!("elem#{p}"));
                }
            }
            if path.contains(&r) {
                return Ok("<cycle>".into());
            }
            path.push(r);
            let v = self.get(o)?;
            let s = self.canon(&v, depth + 1, path);
            path.pop();
            return s;
        }
        Ok(match o {
            Object::Null => "null".into(),
            Object::Bool(b) => b.to_string(),
            Object::Integer(i) => i.to_string(),
            Object::Real(f) => num(*f),
            Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
            Object::String(s) => format!("({})", text(s)),
            Object::Array(a) => {
                let mut v = Vec::new();
                for x in a {
                    v.push(self.canon(x, depth + 1, path)?);
                }
                format!("[{}]", v.join(" "))
            }
            Object::Dict(d) => self.canon_dict(d, depth, path)?,
            Object::Stream { dict, data } => {
                format!("stream{}{}", self.canon_dict(dict, depth, path)?, sha(data))
            }
            Object::Ref(_) => unreachable!(),
        })
    }

    fn canon_dict(&mut self, d: &Dict, depth: usize, path: &mut Vec<IndirectRef>) -> Res<String> {
        let mut v = Vec::new();
        for (k, x) in d {
            // Back-pointers and bookkeeping carry no meaning of their own.
            // In the document-level categories only `/Parent` and a
            // stream's `/Length` are: inside an action value `/P`,
            // `/Next` and `/Prev` can be content (a GoToE target's page,
            // a Launch action's parameters).
            let skip = if self.doclevel {
                matches!(k.as_slice(), b"Parent" | b"Length")
            } else {
                matches!(
                    k.as_slice(),
                    b"Parent" | b"P" | b"Length" | b"Next" | b"Prev"
                )
            };
            if skip {
                continue;
            }
            v.push(format!(
                "/{} {}",
                String::from_utf8_lossy(k),
                self.canon(x, depth + 1, path)?
            ));
        }
        Ok(format!("<<{}>>", v.join(" ")))
    }

    fn canon0(&mut self, o: &Object) -> Res<String> {
        self.canon(o, 0, &mut Vec::new())
    }

    /// Entries of a name tree (`numbers` false) or number tree.
    fn tree(&mut self, root: &Object, numbers: bool) -> Res<Vec<(Object, Object)>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(root.clone(), 0usize)];
        let key = if numbers {
            b"Nums".as_slice()
        } else {
            b"Names".as_slice()
        };
        while let Some((node, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return err("tree deeper than the limit");
            }
            if let Some(r) = node.as_ref() {
                if !seen.insert(r) {
                    return err("cycle in a name or number tree");
                }
            }
            let Some(d) = self.dict(&node)? else {
                return err("tree node is not a dictionary");
            };
            if let Some(kv) = d.get(key) {
                let Object::Array(kv) = self.get(kv)? else {
                    return err("tree leaf array is not an array");
                };
                for pair in kv.chunks(2) {
                    if let [k, v] = pair {
                        out.push((self.get(k)?, v.clone()));
                    }
                }
            }
            if let Some(kids) = d.get(b"Kids".as_slice()) {
                let Object::Array(kids) = self.get(kids)? else {
                    return err("tree /Kids is not an array");
                };
                for k in kids.iter().rev() {
                    stack.push((k.clone(), depth + 1));
                }
            }
        }
        Ok(out)
    }

    // ---------------- destinations ----------------

    fn named(&mut self) -> Res<Named> {
        if let Some(r) = &self.named {
            return match r {
                Ok(m) => Ok(m.clone()),
                Err(Snag::Error(e)) => err(e.clone()),
                Err(Snag::Unsupported(e)) => Err(Snag::Unsupported(e.clone())),
            };
        }
        let r = self.collect_named();
        let out = match &r {
            Ok(m) => Ok(m.clone()),
            Err(Snag::Error(e)) => err(e.clone()),
            Err(Snag::Unsupported(e)) => Err(Snag::Unsupported(e.clone())),
        };
        self.named = Some(r);
        out
    }

    fn collect_named(&mut self) -> Res<Named> {
        let mut dict: HashMap<Vec<u8>, Object> = HashMap::new();
        let mut m: HashMap<Vec<u8>, Object> = HashMap::new();
        let cat = self.doc.catalog().clone();
        if let Some(d) = cat.get(b"Dests".as_slice()) {
            if let Some(d) = self.dict(d)? {
                for (k, v) in d {
                    dict.insert(k, v);
                }
            }
        }
        if let Some(n) = cat.get(b"Names".as_slice()) {
            if let Some(n) = self.dict(n)? {
                if let Some(t) = n.get(b"Dests".as_slice()) {
                    for (k, v) in self.tree(t, false)? {
                        if let Some(k) = k.as_string() {
                            m.insert(k.to_vec(), v);
                        }
                    }
                }
            }
        }
        Ok((dict, m))
    }

    /// An explicit destination, or a name / string looked up first.
    fn dest(&mut self, o: &Object, hops: usize) -> Res<String> {
        let v = self.get(o)?;
        match &v {
            Object::Name(n) | Object::String(n) => {
                if hops > 4 {
                    return Ok("named→loop".into());
                }
                // §12.3.2.4: a name looks up `/Dests`, a string the name
                // tree; the other store is the fallback, so moving a
                // destination between the two is not a change, while a
                // key bound in both resolves to the one its type selects.
                let (dict, tree) = self.named()?;
                let (first, second) = if matches!(v, Object::Name(_)) {
                    (&dict, &tree)
                } else {
                    (&tree, &dict)
                };
                match first.get(n).or_else(|| second.get(n)).cloned() {
                    Some(t) => self.dest(&t, hops + 1),
                    None => Ok("named→unresolved".into()),
                }
            }
            Object::Dict(d) => match d.get(b"D".as_slice()) {
                Some(dd) => self.dest(dd, hops),
                None => Ok("dest-dict-without-D".into()),
            },
            Object::Array(a) if !a.is_empty() => {
                let page = self.page_of(a.first());
                let mut parts = vec![page];
                for x in &a[1..] {
                    parts.push(match self.get(x)? {
                        Object::Integer(i) => num(i as f64),
                        Object::Real(f) => num(f),
                        Object::Null => "null".into(),
                        other => self.canon0(&other)?,
                    });
                }
                Ok(parts.join(" "))
            }
            Object::Null => Ok("none".into()),
            _ => Ok("malformed".into()),
        }
    }

    /// An action (and its `/Next` chain): GoTo actions by their
    /// destination, others by their dictionary.
    fn outlines(&mut self) -> Res<Option<Vec<String>>> {
        self.action_ids.clear();
        let Some(root) = self.doc.catalog().get(b"Outlines".as_slice()).cloned() else {
            return Ok(None);
        };
        let Some(root) = self.dict(&root)? else {
            return Ok(None);
        };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.outline_children(&root, "", 0, &mut seen, &mut out)?;
        Ok(Some(out))
    }

    fn outline_children(
        &mut self,
        parent: &Dict,
        path: &str,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("outline deeper than the limit");
        }
        let mut cur = parent.get(b"First".as_slice()).cloned();
        let mut k = 0usize;
        while let Some(item) = cur {
            if matches!(item, Object::Null) {
                break;
            }
            if let Some(r) = item.as_ref() {
                if !seen.insert(r) {
                    return err("cycle in the outline");
                }
            }
            let Some(d) = self.dict(&item)? else {
                return err("outline item is not a dictionary");
            };
            k += 1;
            let p = if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}.{k}")
            };
            let title = match d
                .get(b"Title".as_slice())
                .map(|t| self.get(t))
                .transpose()?
            {
                Some(Object::String(s)) => text(&s),
                _ => "?".into(),
            };
            let has_kids = d.contains_key(b"First".as_slice());
            let count = d
                .get(b"Count".as_slice())
                .and_then(|c| c.as_i64())
                .unwrap_or(0);
            let state = match (has_kids, count > 0) {
                (false, _) => "leaf",
                (true, true) => "open",
                (true, false) => "closed",
            };
            let target = match (d.get(b"Dest".as_slice()), d.get(b"A".as_slice())) {
                (Some(x), _) => format!("dest {}", self.dest(x, 0)?),
                (None, Some(a)) => format!("action {}", self.strict_chain(a)?),
                (None, None) => "none".into(),
            };
            let style = format!(
                "{} {}",
                match d.get(b"C".as_slice()) {
                    Some(c) => self.canon0(c)?,
                    None => "-".into(),
                },
                d.get(b"F".as_slice()).and_then(|f| f.as_i64()).unwrap_or(0)
            );
            out.push(format!(
                "outline\tpath={p}\ttitle={title}\tstate={state}\ttarget={target}\tstyle={style}"
            ));
            self.outline_children(&d, &p, depth + 1, seen, out)?;
            cur = d.get(b"Next".as_slice()).cloned();
        }
        Ok(())
    }

    // ---------------- named destinations ----------------

    fn named_dests_lines(&mut self) -> Res<Option<Vec<String>>> {
        let cat = self.doc.catalog();
        let has_tree = cat
            .get(b"Names".as_slice())
            .and_then(|n| self.doc.deref(n).ok())
            .and_then(|n| n.as_dict().map(|d| d.contains_key(b"Dests".as_slice())))
            .unwrap_or(false);
        if !cat.contains_key(b"Dests".as_slice()) && !has_tree {
            return Ok(None);
        }
        let (dict, tree) = self.named()?;
        let mut out = Vec::new();
        // Both stores, untagged: a key bound in each is two lines, so a
        // rewrite that drops or merges one binding is a difference.
        for m in [dict, tree] {
            for (k, v) in m {
                let target = self.dest(&v, 1)?;
                out.push(format!("dest\tname={}\ttarget={target}", hex(&k)));
            }
        }
        Ok(Some(out))
    }

    // ---------------- page labels ----------------

    fn page_labels(&mut self) -> Res<Option<Vec<String>>> {
        let Some(root) = self.doc.catalog().get(b"PageLabels".as_slice()).cloned() else {
            return Ok(None);
        };
        let mut ranges: Vec<(i64, Dict)> = Vec::new();
        for (k, v) in self.tree(&root, true)? {
            let Some(start) = k.as_i64() else {
                return err("page-label key is not an integer");
            };
            let d = self.dict(&v)?.unwrap_or_default();
            ranges.push((start, d));
        }
        ranges.sort_by_key(|r| r.0);
        let mut out = Vec::new();
        for i in 0..self.doc.pages().len() as i64 {
            let Some((start, d)) = ranges.iter().rev().find(|r| r.0 <= i) else {
                out.push(format!("label\tpage={i}\tlabel=<none>"));
                continue;
            };
            let style = d.get(b"S".as_slice()).and_then(name).unwrap_or_default();
            let prefix = match d.get(b"P".as_slice()).map(|p| self.get(p)).transpose()? {
                Some(Object::String(s)) => text(&s),
                _ => String::new(),
            };
            let st = d
                .get(b"St".as_slice())
                .and_then(|s| s.as_i64())
                .unwrap_or(1);
            let n = st + (i - start);
            let body = match style.as_str() {
                "D" => n.to_string(),
                "R" => roman(n).to_uppercase(),
                "r" => roman(n),
                "A" => letters(n).to_uppercase(),
                "a" => letters(n),
                "" => String::new(),
                other => format!("<style {other}>"),
            };
            out.push(format!(
                "label\tpage={i}\tlabel={prefix}{body}\tstyle={style}\tprefix={prefix}\tstart={st}"
            ));
        }
        Ok(Some(out))
    }

    // ---------------- structure tree ----------------

    fn struct_tree(&mut self) -> Res<Option<Vec<String>>> {
        let Some(root) = self
            .doc
            .catalog()
            .get(b"StructTreeRoot".as_slice())
            .cloned()
        else {
            return Ok(None);
        };
        let Some(root) = self.dict(&root)? else {
            return Ok(None);
        };
        let mut role_map: HashMap<String, String> = HashMap::new();
        if let Some(rm) = root.get(b"RoleMap".as_slice()) {
            if let Some(rm) = self.dict(rm)? {
                for (k, v) in rm {
                    if let Some(v) = name(&v) {
                        role_map.insert(String::from_utf8_lossy(&k).into_owned(), v);
                    }
                }
            }
        }
        let mut out = Vec::new();
        let mut paths: HashMap<IndirectRef, String> = HashMap::new();
        let mut seen = HashSet::new();
        if let Some(k) = root.get(b"K".as_slice()).cloned() {
            self.struct_kids(&k, "", None, 0, &role_map, &mut seen, &mut paths, &mut out)?;
        }
        // ParentTree: page /StructParents and annotation /StructParent
        // → the element each marked-content id / object belongs to.
        let ptree: BTreeMap<i64, Object> = match root.get(b"ParentTree".as_slice()).cloned() {
            Some(t) => self
                .tree(&t, true)?
                .into_iter()
                .filter_map(|(k, v)| k.as_i64().map(|k| (k, v)))
                .collect(),
            None => BTreeMap::new(),
        };
        let elem = |ctx: &mut Ctx, o: &Object| -> String {
            match o.as_ref().and_then(|r| paths.get(&r)) {
                Some(p) => p.clone(),
                None => match o {
                    Object::Null => "null".into(),
                    _ => {
                        let _ = ctx;
                        "orphan".into()
                    }
                },
            }
        };
        let pages: Vec<(usize, IndirectRef)> = self
            .doc
            .pages()
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.object_ref().map(|r| (i, r)))
            .collect();
        for (i, r) in pages {
            let Some(pd) = self.dict(&Object::Ref(r))? else {
                continue;
            };
            if let Some(sp) = pd.get(b"StructParents".as_slice()).and_then(|v| v.as_i64()) {
                match ptree.get(&sp) {
                    Some(v) => match self.get(v)? {
                        Object::Array(a) => {
                            for (mcid, e) in a.iter().enumerate() {
                                let p = elem(self, e);
                                out.push(format!("ptree\tpage={i}\tmcid={mcid}\telem={p}"));
                            }
                        }
                        other => out.push(format!("ptree\tpage={i}\tentry={}", elem(self, &other))),
                    },
                    None => out.push(format!("ptree\tpage={i}\tmissing")),
                }
            }
        }
        let annots: Vec<(IndirectRef, (usize, usize))> =
            self.annots.iter().map(|(r, v)| (*r, *v)).collect();
        for (r, (p, k)) in annots {
            let Some(ad) = self.dict(&Object::Ref(r))? else {
                continue;
            };
            if let Some(sp) = ad.get(b"StructParent".as_slice()).and_then(|v| v.as_i64()) {
                let e = match ptree.get(&sp) {
                    Some(v) => elem(self, v),
                    None => "missing".into(),
                };
                out.push(format!("ptree\tannot={p}.{k}\telem={e}"));
            }
        }
        self.struct_paths = paths;
        Ok(Some(out))
    }

    #[allow(clippy::too_many_arguments)]
    fn struct_kids(
        &mut self,
        k: &Object,
        path: &str,
        page: Option<usize>,
        depth: usize,
        roles: &HashMap<String, String>,
        seen: &mut HashSet<IndirectRef>,
        paths: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("structure tree deeper than the limit");
        }
        let items = match self.get(k)? {
            Object::Array(a) => a,
            _ => vec![k.clone()],
        };
        for (n, it) in items.iter().enumerate() {
            let at = if path.is_empty() {
                n.to_string()
            } else {
                format!("{path}.{n}")
            };
            if let Some(r) = it.as_ref() {
                // `seen` holds the ancestors: only a back-edge is a cycle.
                if seen.contains(&r) {
                    return err("cycle in the structure tree");
                }
                // An element reachable from two parents is recorded at
                // its first path and referenced from the second.
                if let Some(first) = paths.get(&r) {
                    out.push(format!("shared\tpath={at}\tof={first}"));
                    continue;
                }
            }
            let v = self.get(it)?;
            match &v {
                Object::Integer(mcid) => {
                    out.push(format!("mcid\tpath={at}\tpage={}\tmcid={mcid}", pg(page)));
                }
                Object::Dict(d) => {
                    let ty = d.get(b"Type".as_slice()).and_then(name).unwrap_or_default();
                    let pg_here = match d.get(b"Pg".as_slice()) {
                        Some(p) => p.as_ref().and_then(|r| self.pages.get(&r).copied()),
                        None => page,
                    };
                    if ty == "MCR" {
                        let mcid = d
                            .get(b"MCID".as_slice())
                            .and_then(|m| m.as_i64())
                            .unwrap_or(-1);
                        let stm = match d.get(b"Stm".as_slice()) {
                            Some(s) => format!("\tstm={}", self.canon0(s)?),
                            None => String::new(),
                        };
                        out.push(format!(
                            "mcid\tpath={at}\tpage={}\tmcid={mcid}{stm}",
                            pg(pg_here)
                        ));
                    } else if ty == "OBJR" {
                        let obj = match d.get(b"Obj".as_slice()).and_then(|o| o.as_ref()) {
                            Some(r) => match self.annots.get(&r) {
                                Some((p, k)) => format!("annot#{p}.{k}"),
                                None => match self.dict(&Object::Ref(r))? {
                                    Some(od) => format!(
                                        "object {}",
                                        od.get(b"Subtype".as_slice())
                                            .and_then(name)
                                            .unwrap_or_default()
                                    ),
                                    None => "object ?".into(),
                                },
                            },
                            None => "none".into(),
                        };
                        out.push(format!("objr\tpath={at}\tpage={}\tobj={obj}", pg(pg_here)));
                    } else {
                        if let Some(r) = it.as_ref() {
                            paths.insert(r, at.clone());
                        }
                        let s = d.get(b"S".as_slice()).and_then(name).unwrap_or_default();
                        let mut role = s.clone();
                        for _ in 0..16 {
                            match roles.get(&role) {
                                Some(next) if *next != role => role = next.clone(),
                                _ => break,
                            }
                        }
                        let mut extra = String::new();
                        for key in [&b"Alt"[..], b"ActualText", b"Lang", b"E", b"T"] {
                            if let Some(Object::String(sv)) =
                                d.get(key).map(|x| self.get(x)).transpose()?
                            {
                                extra.push_str(&format!(
                                    "\t{}={}",
                                    String::from_utf8_lossy(key).to_lowercase(),
                                    text(&sv)
                                ));
                            }
                        }
                        out.push(format!(
                            "elem\tpath={at}\ts={s}\trole={role}\tpage={}{extra}",
                            pg(pg_here)
                        ));
                        if let Some(kk) = d.get(b"K".as_slice()) {
                            let r = it.as_ref();
                            if let Some(r) = r {
                                seen.insert(r);
                            }
                            let res = self.struct_kids(
                                kk,
                                &at,
                                pg_here,
                                depth + 1,
                                roles,
                                seen,
                                paths,
                                out,
                            );
                            if let Some(r) = r {
                                seen.remove(&r);
                            }
                            res?;
                        }
                    }
                }
                Object::Null => {}
                _ => out.push(format!("kid\tpath={at}\tmalformed")),
            }
        }
        Ok(())
    }

    // ---------------- embedded files ----------------

    fn embedded_files(&mut self) -> Res<Option<Vec<String>>> {
        let mut out = Vec::new();
        let mut present = false;
        let cat = self.doc.catalog().clone();
        if let Some(n) = cat.get(b"Names".as_slice()) {
            if let Some(n) = self.dict(n)? {
                if let Some(t) = n.get(b"EmbeddedFiles".as_slice()) {
                    present = true;
                    for (k, v) in self.tree(t, false)? {
                        let key = k.as_string().map(text).unwrap_or_else(|| "?".into());
                        let fs = self.filespec(&v)?;
                        out.push(format!("ef\tsource=names:{key}\t{fs}"));
                    }
                }
            }
        }
        if let Some(af) = cat.get(b"AF".as_slice()) {
            present = true;
            if let Object::Array(a) = self.get(af)? {
                for (i, x) in a.iter().enumerate() {
                    let fs = self.filespec(x)?;
                    out.push(format!("ef\tsource=catalog-af:{i}\t{fs}"));
                }
            }
        }
        let annots: Vec<(IndirectRef, (usize, usize))> =
            self.annots.iter().map(|(r, v)| (*r, *v)).collect();
        for (r, (p, k)) in annots {
            let Some(ad) = self.dict(&Object::Ref(r))? else {
                continue;
            };
            if ad.get(b"Subtype".as_slice()).and_then(name).as_deref() == Some("FileAttachment") {
                present = true;
                let fs = match ad.get(b"FS".as_slice()) {
                    Some(f) => self.filespec(f)?,
                    None => "fs=none".into(),
                };
                out.push(format!("ef\tsource=annot:{p}.{k}\t{fs}"));
            }
        }
        Ok(present.then_some(out))
    }

    fn filespec(&mut self, o: &Object) -> Res<String> {
        let v = self.get(o)?;
        let d = match v {
            Object::String(s) => return Ok(format!("f={}\tef=none", text(&s))),
            Object::Dict(d) => d,
            _ => return Ok("fs=malformed".into()),
        };
        let mut s = String::new();
        for key in [&b"F"[..], b"UF", b"Desc"] {
            let val = match d.get(key).map(|x| self.get(x)).transpose()? {
                Some(Object::String(b)) => text(&b),
                Some(_) => "?".into(),
                None => "-".into(),
            };
            s.push_str(&format!(
                "{}={val}\t",
                String::from_utf8_lossy(key).to_lowercase()
            ));
        }
        let rel = d
            .get(b"AFRelationship".as_slice())
            .and_then(name)
            .unwrap_or_default();
        s.push_str(&format!("rel={rel}\t"));
        let ef = match d.get(b"EF".as_slice()) {
            Some(e) => self.dict(e)?.unwrap_or_default(),
            None => Dict::new(),
        };
        let stream = ef
            .get(b"UF".as_slice())
            .or_else(|| ef.get(b"F".as_slice()))
            .cloned();
        match stream {
            None => s.push_str("ef=none"),
            Some(st) => match self.get(&st)? {
                Object::Stream { dict, data } => {
                    let mime = dict
                        .get(b"Subtype".as_slice())
                        .and_then(name)
                        .unwrap_or_default();
                    let params = match dict.get(b"Params".as_slice()) {
                        Some(p) => self.dict(p)?.unwrap_or_default(),
                        None => Dict::new(),
                    };
                    let size = params
                        .get(b"Size".as_slice())
                        .and_then(|x| x.as_i64())
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "-".into());
                    let modd = match params.get(b"ModDate".as_slice()) {
                        Some(Object::String(b)) => text(b),
                        _ => "-".into(),
                    };
                    let content = decoded_digest(&dict, &data)?;
                    s.push_str(&format!(
                        "mime={mime}\tsize={size}\tmod={modd}\tcontent={content}"
                    ));
                }
                _ => s.push_str("ef=not-a-stream"),
            },
        }
        Ok(s)
    }

    // ---------------- optional content ----------------

    fn optional_content(&mut self) -> Res<Option<Vec<String>>> {
        let Some(ocp) = self.doc.catalog().get(b"OCProperties".as_slice()).cloned() else {
            return Ok(None);
        };
        let Some(ocp) = self.dict(&ocp)? else {
            return err("/OCProperties is not a dictionary");
        };
        let ocgs: Vec<Object> = match ocp
            .get(b"OCGs".as_slice())
            .map(|o| self.get(o))
            .transpose()?
        {
            Some(Object::Array(a)) => a,
            _ => return err("/OCProperties has no /OCGs array"),
        };
        let mut idx: HashMap<IndirectRef, usize> = HashMap::new();
        let mut out = Vec::new();
        for (i, g) in ocgs.iter().enumerate() {
            if let Some(r) = g.as_ref() {
                idx.entry(r).or_insert(i);
            }
            let d = self.dict(g)?.unwrap_or_default();
            let nm = match d.get(b"Name".as_slice()).map(|x| self.get(x)).transpose()? {
                Some(Object::String(b)) => text(&b),
                _ => "?".into(),
            };
            let intent = match d.get(b"Intent".as_slice()) {
                Some(x) => self.canon0(x)?,
                None => "/View".into(),
            };
            let usage = match d.get(b"Usage".as_slice()) {
                Some(x) => self.canon0(x)?,
                None => "-".into(),
            };
            out.push(format!(
                "ocg\tindex={i}\tname={nm}\tintent={intent}\tusage={usage}"
            ));
        }
        let configs: Vec<(String, Object)> = {
            let mut v = Vec::new();
            if let Some(d) = ocp.get(b"D".as_slice()) {
                v.push(("D".to_string(), d.clone()));
            }
            if let Some(Object::Array(a)) = ocp
                .get(b"Configs".as_slice())
                .map(|o| self.get(o))
                .transpose()?
            {
                for (i, c) in a.into_iter().enumerate() {
                    v.push((format!("config{i}"), c));
                }
            }
            v
        };
        for (label, c) in configs {
            let d = self.dict(&c)?.unwrap_or_default();
            let mut parts = vec![format!("config\tid={label}")];
            let base = d
                .get(b"BaseState".as_slice())
                .and_then(name)
                .unwrap_or_else(|| "ON".into());
            parts.push(format!("base={base}"));
            for key in [&b"ON"[..], b"OFF", b"Locked"] {
                let mut list: Vec<String> = match d.get(key).map(|x| self.get(x)).transpose()? {
                    Some(Object::Array(a)) => a.iter().map(|g| ocg_ref(&idx, g)).collect(),
                    _ => vec![],
                };
                list.sort();
                parts.push(format!(
                    "{}=[{}]",
                    String::from_utf8_lossy(key).to_lowercase(),
                    list.join(" ")
                ));
            }
            let order = match d.get(b"Order".as_slice()) {
                Some(o) => self.oc_order(o, &idx, 0)?,
                None => "-".into(),
            };
            parts.push(format!("order={order}"));
            let mut rb: Vec<String> = Vec::new();
            if let Some(Object::Array(groups)) = d
                .get(b"RBGroups".as_slice())
                .map(|x| self.get(x))
                .transpose()?
            {
                for g in groups {
                    if let Object::Array(m) = self.get(&g)? {
                        let mut v: Vec<String> = m.iter().map(|x| ocg_ref(&idx, x)).collect();
                        v.sort();
                        rb.push(format!("[{}]", v.join(" ")));
                    }
                }
            }
            rb.sort();
            parts.push(format!("rb={}", rb.join(" ")));
            let list_mode = d
                .get(b"ListMode".as_slice())
                .and_then(name)
                .unwrap_or_else(|| "AllPages".into());
            parts.push(format!("list={list_mode}"));
            for key in [&b"Intent"[..], b"AS", b"Name", b"Creator"] {
                let v = match d.get(key) {
                    Some(x) => self.oc_value(x, &idx)?,
                    None => "-".into(),
                };
                parts.push(format!(
                    "{}={v}",
                    String::from_utf8_lossy(key).to_lowercase()
                ));
            }
            out.push(parts.join("\t"));
        }
        // Where content refers to optional content: page /Properties,
        // annotation /OC, image and form XObject /OC.
        let pages: Vec<(usize, IndirectRef)> = self
            .doc
            .pages()
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.object_ref().map(|r| (i, r)))
            .collect();
        for (i, page) in pages {
            let res = self.doc.pages()[i].resources().cloned();
            if let Some(res) = res {
                if let Some(rd) = self.dict(&res)? {
                    if let Some(props) = rd.get(b"Properties".as_slice()) {
                        for (k, v) in self.dict(props)?.unwrap_or_default() {
                            let t = self.oc_value(&v, &idx)?;
                            out.push(format!(
                                "use\tpage={i}\tprops={}\toc={t}",
                                String::from_utf8_lossy(&k)
                            ));
                        }
                    }
                    if let Some(xo) = rd.get(b"XObject".as_slice()) {
                        for (k, v) in self.dict(xo)?.unwrap_or_default() {
                            if let Some(xd) = self.dict(&v)? {
                                if let Some(oc) = xd.get(b"OC".as_slice()) {
                                    let t = self.oc_value(oc, &idx)?;
                                    out.push(format!(
                                        "use\tpage={i}\txobject={}\toc={t}",
                                        String::from_utf8_lossy(&k)
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            let _ = page;
        }
        let annots: Vec<(IndirectRef, (usize, usize))> =
            self.annots.iter().map(|(r, v)| (*r, *v)).collect();
        for (r, (p, k)) in annots {
            let Some(ad) = self.dict(&Object::Ref(r))? else {
                continue;
            };
            if let Some(oc) = ad.get(b"OC".as_slice()) {
                let t = self.oc_value(oc, &idx)?;
                out.push(format!("use\tannot={p}.{k}\toc={t}"));
            }
        }
        Ok(Some(out))
    }

    /// An OCG by its index in `/OCGs`, an OCMD by its members and
    /// policy / visibility expression, anything else generically.
    fn oc_value(&mut self, o: &Object, idx: &HashMap<IndirectRef, usize>) -> Res<String> {
        if let Some(r) = o.as_ref() {
            if let Some(i) = idx.get(&r) {
                return Ok(format!("ocg#{i}"));
            }
        }
        let v = self.get(o)?;
        match &v {
            Object::Dict(d)
                if d.get(b"Type".as_slice()).and_then(name).as_deref() == Some("OCMD") =>
            {
                let mut members: Vec<String> = match d.get(b"OCGs".as_slice()) {
                    Some(g) => match self.get(g)? {
                        Object::Array(a) => a.iter().map(|x| ocg_ref(idx, x)).collect(),
                        _ => vec![ocg_ref(idx, g)],
                    },
                    None => vec![],
                };
                members.sort();
                let p = d
                    .get(b"P".as_slice())
                    .and_then(name)
                    .unwrap_or_else(|| "AnyOn".into());
                let ve = match d.get(b"VE".as_slice()) {
                    Some(x) => self.oc_ve(x, idx, 0)?,
                    None => "-".into(),
                };
                Ok(format!("ocmd(p={p} ocgs=[{}] ve={ve})", members.join(" ")))
            }
            Object::Dict(d)
                if d.get(b"Type".as_slice()).and_then(name).as_deref() == Some("OCG") =>
            {
                Ok("ocg-not-in-OCGs".into())
            }
            Object::Array(a) => {
                let mut v = Vec::new();
                for x in a {
                    v.push(self.oc_value(x, idx)?);
                }
                Ok(format!("[{}]", v.join(" ")))
            }
            Object::Dict(d) => {
                let mut v = Vec::new();
                for (k, x) in d {
                    v.push(format!(
                        "/{} {}",
                        String::from_utf8_lossy(k),
                        self.oc_value(x, idx)?
                    ));
                }
                Ok(format!("<<{}>>", v.join(" ")))
            }
            other => self.canon0(other),
        }
    }

    fn oc_ve(
        &mut self,
        o: &Object,
        idx: &HashMap<IndirectRef, usize>,
        depth: usize,
    ) -> Res<String> {
        if depth > MAX_DEPTH {
            return err("visibility expression deeper than the limit");
        }
        if let Some(r) = o.as_ref() {
            if let Some(i) = idx.get(&r) {
                return Ok(format!("ocg#{i}"));
            }
        }
        match self.get(o)? {
            Object::Array(a) => {
                let mut v = Vec::new();
                for x in &a {
                    v.push(self.oc_ve(x, idx, depth + 1)?);
                }
                Ok(format!("[{}]", v.join(" ")))
            }
            Object::Name(n) => Ok(format!("/{}", String::from_utf8_lossy(&n))),
            Object::Dict(_) => self.oc_value(o, idx),
            _ => Ok("ocg?".into()),
        }
    }

    fn oc_order(
        &mut self,
        o: &Object,
        idx: &HashMap<IndirectRef, usize>,
        depth: usize,
    ) -> Res<String> {
        if depth > MAX_DEPTH {
            return err("/Order deeper than the limit");
        }
        if let Some(r) = o.as_ref() {
            if let Some(i) = idx.get(&r) {
                return Ok(format!("ocg#{i}"));
            }
        }
        match self.get(o)? {
            Object::Array(a) => {
                let mut v = Vec::new();
                for x in &a {
                    v.push(self.oc_order(x, idx, depth + 1)?);
                }
                Ok(format!("[{}]", v.join(" ")))
            }
            Object::String(s) => Ok(format!("({})", text(&s))),
            _ => Ok("ocg?".into()),
        }
    }

    // ---------------- AcroForm ----------------

    fn acroform(&mut self) -> Res<Option<Vec<String>>> {
        let Some(af) = self.doc.catalog().get(b"AcroForm".as_slice()).cloned() else {
            return Ok(None);
        };
        let Some(af) = self.dict(&af)? else {
            return err("/AcroForm is not a dictionary");
        };
        // `/XFA` is its own category (`xfa`); this one is the AcroForm
        // half of a form, compared whether or not XFA is present.
        let mut out = Vec::new();
        // Scalars through references too: an indirect `/SigFlags 5 0 R`
        // is its value, not 0.
        let need = match af
            .get(b"NeedAppearances".as_slice())
            .map(|x| self.get(x))
            .transpose()?
        {
            Some(v) => v.as_bool().unwrap_or(false),
            None => false,
        };
        let sig = match af
            .get(b"SigFlags".as_slice())
            .map(|x| self.get(x))
            .transpose()?
        {
            Some(v) => v.as_i64().unwrap_or(0),
            None => 0,
        };
        let da = match af.get(b"DA".as_slice()).map(|x| self.get(x)).transpose()? {
            Some(Object::String(b)) => text(&b),
            _ => "-".into(),
        };
        let q = match af.get(b"Q".as_slice()).map(|x| self.get(x)).transpose()? {
            Some(v) => v.as_i64().unwrap_or(0),
            None => 0,
        };
        out.push(format!(
            "form\tneed_appearances={need}\tsig_flags={sig}\tda={da}\tq={q}"
        ));
        let mut names: HashMap<IndirectRef, String> = HashMap::new();
        let mut seen = HashSet::new();
        if let Some(fields) = af.get(b"Fields".as_slice()).cloned() {
            if let Object::Array(a) = self.get(&fields)? {
                for f in a {
                    self.field(
                        &f,
                        "",
                        &Inherit::default(),
                        0,
                        &mut seen,
                        &mut names,
                        &mut out,
                    )?;
                }
            }
        }
        if let Some(co) = af.get(b"CO".as_slice()) {
            if let Object::Array(a) = self.get(co)? {
                let order: Vec<String> = a
                    .iter()
                    .map(|x| {
                        x.as_ref()
                            .and_then(|r| names.get(&r))
                            .cloned()
                            .unwrap_or_else(|| "?".into())
                    })
                    .collect();
                out.push(format!("calc_order\tfields={}", order.join(",")));
            }
        }
        Ok(Some(out))
    }

    #[allow(clippy::too_many_arguments)]
    fn field(
        &mut self,
        o: &Object,
        parent_name: &str,
        inh: &Inherit,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
        names: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("field tree deeper than the limit");
        }
        if let Some(r) = o.as_ref() {
            if !seen.insert(r) {
                return err("cycle in the field tree");
            }
        }
        let Some(d) = self.dict(o)? else {
            return Ok(());
        };
        let partial = match d.get(b"T".as_slice()).map(|x| self.get(x)).transpose()? {
            Some(Object::String(b)) => Some(text(&b)),
            _ => None,
        };
        let fq = match (&partial, parent_name.is_empty()) {
            (Some(t), true) => t.clone(),
            (Some(t), false) => format!("{parent_name}.{t}"),
            (None, _) => parent_name.to_string(),
        };
        if let Some(r) = o.as_ref() {
            names.insert(r, fq.clone());
        }
        let mut h = inh.clone();
        for (key, slot) in [
            (&b"FT"[..], &mut h.ft),
            (b"Ff", &mut h.ff),
            (b"V", &mut h.v),
            (b"DV", &mut h.dv),
            (b"DA", &mut h.da),
            (b"Q", &mut h.q),
            (b"Opt", &mut h.opt),
            (b"MaxLen", &mut h.max_len),
        ] {
            if let Some(x) = d.get(key) {
                *slot = Some(x.clone());
            }
        }
        let kids: Vec<Object> = match d.get(b"Kids".as_slice()).map(|x| self.get(x)).transpose()? {
            Some(Object::Array(a)) => a,
            _ => vec![],
        };
        let is_widget = d.get(b"Subtype".as_slice()).and_then(name).as_deref() == Some("Widget");
        // A terminal field: no kid with a /T of its own.
        let mut field_kids = Vec::new();
        let mut widget_kids = Vec::new();
        for k in &kids {
            let kd = self.dict(k)?.unwrap_or_default();
            // A kid is a field when it has a name of its own, or when it
            // is not a widget and has kids (an unnamed intermediate
            // node); widgets have no field kids.
            let kid_widget =
                kd.get(b"Subtype".as_slice()).and_then(name).as_deref() == Some("Widget");
            if kd.contains_key(b"T".as_slice())
                || (!kid_widget && kd.contains_key(b"Kids".as_slice()))
            {
                field_kids.push(k.clone());
            } else {
                widget_kids.push(k.clone());
            }
        }
        if field_kids.is_empty() && (partial.is_some() || is_widget || !widget_kids.is_empty()) {
            let line = format!(
                "field\tname={fq}\tft={}\tff={}\tv={}\tdv={}\topt={}\tmaxlen={}",
                self.opt(&h.ft)?,
                self.opt(&h.ff)?,
                self.opt(&h.v)?,
                self.opt(&h.dv)?,
                self.opt(&h.opt)?,
                self.opt(&h.max_len)?
            );
            out.push(line);
            if is_widget {
                let w = self.widget(o, &d, &h)?;
                out.push(format!("widget\tfield={fq}\t{w}"));
            }
            for k in &widget_kids {
                if let Some(r) = k.as_ref() {
                    if !seen.insert(r) {
                        return err("cycle in the field tree");
                    }
                }
                let kd = self.dict(k)?.unwrap_or_default();
                let w = self.widget(k, &kd, &h)?;
                out.push(format!("widget\tfield={fq}\t{w}"));
            }
        } else {
            for k in &field_kids {
                self.field(k, &fq, &h, depth + 1, seen, names, out)?;
            }
            for k in &widget_kids {
                self.field(k, &fq, &h, depth + 1, seen, names, out)?;
            }
        }
        Ok(())
    }

    fn opt(&mut self, o: &Option<Object>) -> Res<String> {
        match o {
            None => Ok("-".into()),
            Some(x) => self.canon0(x),
        }
    }

    fn widget(&mut self, o: &Object, d: &Dict, h: &Inherit) -> Res<String> {
        let place = match o.as_ref().and_then(|r| self.annots.get(&r)) {
            Some((p, k)) => format!("{p}.{k}"),
            None => "not-on-a-page".into(),
        };
        let rect = match d.get(b"Rect".as_slice()) {
            Some(r) => match self.get(r)? {
                Object::Array(a) => {
                    let v: Vec<f64> = a.iter().filter_map(|x| x.as_number()).collect();
                    if v.len() == 4 {
                        let (x0, x1) = (v[0].min(v[2]), v[0].max(v[2]));
                        let (y0, y1) = (v[1].min(v[3]), v[1].max(v[3]));
                        format!("{} {} {} {}", num(x0), num(y0), num(x1), num(y1))
                    } else {
                        "malformed".into()
                    }
                }
                _ => "malformed".into(),
            },
            None => "-".into(),
        };
        let as_ = d
            .get(b"AS".as_slice())
            .and_then(name)
            .unwrap_or_else(|| "-".into());
        let mk = match d.get(b"MK".as_slice()) {
            Some(x) => self.canon0(x)?,
            None => "-".into(),
        };
        let states = match d.get(b"AP".as_slice()) {
            Some(ap) => match self.dict(ap)? {
                Some(ap) => match ap.get(b"N".as_slice()).map(|n| self.get(n)).transpose()? {
                    Some(Object::Dict(states)) => {
                        let mut v: Vec<String> = states
                            .keys()
                            .map(|k| String::from_utf8_lossy(k).into_owned())
                            .collect();
                        v.sort();
                        format!("states:{}", v.join(","))
                    }
                    Some(Object::Stream { .. }) => "stream".into(),
                    _ => "none".into(),
                },
                None => "none".into(),
            },
            None => "none".into(),
        };
        let f = d.get(b"F".as_slice()).and_then(|v| v.as_i64()).unwrap_or(0);
        let da = self.opt(&d.get(b"DA".as_slice()).cloned().or_else(|| h.da.clone()))?;
        Ok(format!(
            "page={place}\trect={rect}\tas={as_}\tmk={mk}\tap={states}\tf={f}\tda={da}"
        ))
    }
}

type Named = (HashMap<Vec<u8>, Object>, HashMap<Vec<u8>, Object>);

#[derive(Clone, Default)]
struct Inherit {
    ft: Option<Object>,
    ff: Option<Object>,
    v: Option<Object>,
    dv: Option<Object>,
    da: Option<Object>,
    q: Option<Object>,
    opt: Option<Object>,
    max_len: Option<Object>,
}

fn pg(p: Option<usize>) -> String {
    p.map(|p| p.to_string()).unwrap_or_else(|| "-".into())
}

fn ocg_ref(idx: &HashMap<IndirectRef, usize>, o: &Object) -> String {
    match o.as_ref().and_then(|r| idx.get(&r)) {
        Some(i) => format!("ocg#{i}"),
        None => "ocg?".into(),
    }
}

fn roman(n: i64) -> String {
    if !(1..=4999).contains(&n) {
        return n.to_string();
    }
    let mut n = n;
    let mut s = String::new();
    for (v, r) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

/// a..z, then aa..zz, … (§12.4.2 Table 161).
fn letters(n: i64) -> String {
    if !(1..=26 * 1000).contains(&n) {
        return n.to_string();
    }
    let c = (b'a' + ((n - 1) % 26) as u8) as char;
    std::iter::repeat_n(c, ((n - 1) / 26 + 1) as usize).collect()
}

/// Length and digest of an embedded file's decoded bytes: FlateDecode
/// (without predictor) is decoded; unfiltered data is taken as is;
/// other filters are `unsupported` rather than compared encoded.
fn decoded_digest(dict: &Dict, data: &[u8]) -> Res<String> {
    let filters: Vec<String> = match dict.get(b"Filter".as_slice()) {
        None => vec![],
        Some(Object::Name(n)) => vec![String::from_utf8_lossy(n).into_owned()],
        Some(Object::Array(a)) => a.iter().filter_map(name).collect(),
        Some(_) => {
            return Err(Snag::Unsupported(
                "embedded file /Filter is not a name".into(),
            ))
        }
    };
    let has_parms = dict.contains_key(b"DecodeParms".as_slice());
    let bytes = match (filters.as_slice(), has_parms) {
        ([], _) => data.to_vec(),
        ([f], false) if f == "FlateDecode" => {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, MAX_EMBEDDED_BYTES)
                .map_err(|_| {
                    Snag::Error("embedded file: FlateDecode data does not decode".into())
                })?
        }
        _ => {
            return Err(Snag::Unsupported(format!(
                "embedded file filter {} (only FlateDecode without parameters is decoded)",
                filters.join("+")
            )))
        }
    };
    Ok(format!("{}:{}", bytes.len(), sha(&bytes)))
}

/// Detection check on real files: for every strictly parsed file, apply
/// one meaning-changing edit through the editing API, save
/// incrementally, and require the snapshot comparison to fail in the
/// expected category; and require an identity incremental save (an
/// unrelated `/Info` edit) to pass. Prints aggregates only.
pub fn probe(files: &[std::path::PathBuf]) {
    let mut tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut bump = |k: &str, hit: bool| {
        let e = tally.entry(k.to_string()).or_default();
        e.0 += 1;
        e.1 += hit as usize;
    };
    for p in files {
        let Ok(data) = std::fs::read(p) else { continue };
        let Ok(doc) = Document::from_bytes(&data) else {
            continue;
        };
        let save = |f: &dyn Fn(&mut kasane_pdf::Edit) -> Result<(), kasane_pdf::Error>| -> Option<Document> {
            let mut ed = doc.edit().ok()?;
            f(&mut ed).ok()?;
            let out = ed.save_incremental().ok()?;
            Document::from_bytes(&out).ok()
        };
        // Control: an `/Info` edit keeps every other category's meaning
        // — and is itself a metadata change.
        if let Some(d2) = save(&|ed| {
            let mut info = Dict::new();
            info.insert(
                b"Title".to_vec(),
                Object::String(b"semantic probe".to_vec()),
            );
            ed.set_info(info)
        }) {
            let r = compare(&doc, &d2);
            bump(
                "control (/Info edit): no category but metadata fails",
                r.checks
                    .iter()
                    .all(|c| c.category == "metadata" || c.class != SemClass::Fail),
            );
            bump(
                "metadata: /Info replaced",
                r.checks
                    .iter()
                    .any(|c| c.category == "metadata" && c.class == SemClass::Fail),
            );
        }
        let base = snapshot(&doc);
        let lines = |cat: &str| -> Vec<String> {
            let i = CATEGORIES.iter().position(|c| *c == cat).unwrap();
            match &base[i] {
                Section::Data(v) => v.clone(),
                _ => vec![],
            }
        };
        probe_doclevel::probe_file(&data, &doc, &mut bump);
        // AcroForm: change the value of the first text field.
        let text_field = lines("acroform").iter().find_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0] == "field" && f.contains(&"ft=/Tx"))
                .then(|| f[1].trim_start_matches("name=").to_string())
        });
        if let Some(name) = text_field {
            if let Some(d2) =
                save(&|ed| ed.set_field_value(&name, Object::String(b"semantic probe".to_vec())))
            {
                let r = compare(&doc, &d2);
                bump(
                    "acroform: field value changed",
                    r.checks
                        .iter()
                        .any(|c| c.category == "acroform" && c.class == SemClass::Fail),
                );
            }
        }
        // Structure tree: remove an annotation an OBJR points at.
        let objr = lines("struct_tree").iter().find_map(|l| {
            l.split('\t')
                .find_map(|f| f.strip_prefix("obj=annot#"))
                .and_then(|pk| {
                    let (p, k) = pk.split_once('.')?;
                    Some((p.parse::<usize>().ok()?, k.parse::<usize>().ok()?))
                })
        });
        if let Some((pg, k)) = objr {
            if let Some(d2) = save(&|ed| ed.remove_annotation(pg, k)) {
                let r = compare(&doc, &d2);
                bump(
                    "struct_tree: annotation behind an OBJR removed",
                    r.checks
                        .iter()
                        .any(|c| c.category == "struct_tree" && c.class == SemClass::Fail),
                );
            }
        }
        // Embedded files: remove a file-attachment annotation.
        let att = lines("embedded_files").iter().find_map(|l| {
            l.split('\t')
                .find_map(|f| f.strip_prefix("source=annot:"))
                .and_then(|pk| {
                    let (p, k) = pk.split_once('.')?;
                    Some((p.parse::<usize>().ok()?, k.parse::<usize>().ok()?))
                })
        });
        if let Some((pg, k)) = att {
            if let Some(d2) = save(&|ed| ed.remove_annotation(pg, k)) {
                let r = compare(&doc, &d2);
                bump(
                    "embedded_files: attachment annotation removed",
                    r.checks
                        .iter()
                        .any(|c| c.category == "embedded_files" && c.class == SemClass::Fail),
                );
            }
        }
    }
    println!("| probe | files | detected |\n|---|---:|---:|");
    for (k, (n, hit)) in tally {
        println!("| {k} | {n} | {hit} |");
    }
}

mod doclevel;
mod pdf20;
mod probe_doclevel;
pub mod transform;
mod v4;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_doclevel;
#[cfg(test)]
mod tests_pdf20;
#[cfg(test)]
mod tests_v4;
