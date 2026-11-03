//! V4 categories of the semantic snapshot, **report only**: multimedia
//! and signatures, and the action inventory the D2 report is checked
//! against (`plans/rewrite-policy.md` §3.5, §5.7).
//!
//! The document-level rules apply: values by canonical form with streams
//! by decoded digest (`cval`), positions instead of object numbers, an
//! unreadable object `#unresolved` (never null), limits `error`.

use super::*;
use std::collections::VecDeque;
use std::rc::Rc;

/// Action types of ISO 32000-2 Table 201 (and the annotation subtypes)
/// by class (`plans/rewrite-policy.md` §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Script,
    External,
    State,
    Navigation,
    Multimedia,
    Unknown,
}

pub fn classify(s: &str) -> ActionClass {
    match s {
        "JavaScript" => ActionClass::Script,
        "Launch" | "URI" | "GoToR" | "GoToE" | "SubmitForm" | "ImportData" => ActionClass::External,
        "ResetForm" | "Hide" | "SetOCGState" | "Trans" => ActionClass::State,
        "GoTo" | "GoToDp" | "Thread" | "Named" | "GoTo3DView" => ActionClass::Navigation,
        "Rendition" | "Movie" | "Sound" | "RichMediaExecute" => ActionClass::Multimedia,
        _ => ActionClass::Unknown,
    }
}

/// An owner's content: an action, or a multimedia annotation itself
/// (subtype, object).
pub(super) enum Root {
    Action(Object),
    Media(String, Option<IndirectRef>),
}

/// `/AA` keys of ISO 32000-2 Tables 197–200; any other key is `other`
/// (as in the library's report).
fn trigger(k: &[u8]) -> &'static str {
    const T: [&str; 20] = [
        "E", "X", "D", "U", "Fo", "Bl", "PO", "PC", "PV", "PI", "O", "C", "K", "F", "V", "WC",
        "WS", "DS", "WP", "DP",
    ];
    T.iter()
        .find(|t| t.as_bytes() == k)
        .copied()
        .unwrap_or("other")
}

/// Annotation subtypes that carry multimedia content.
const MULTIMEDIA_ANNOTS: [&str; 5] = ["RichMedia", "Screen", "Movie", "Sound", "3D"];

/// An XFA owner, its packet object and its number of scripts.
type XfaScripts = (String, Option<(u32, u16)>, u32);

/// The class of an action dictionary of kind `kind`: `/Named` is
/// navigation for the four page operations, state otherwise.
fn classify_action(kind: &str, d: &Dict) -> ActionClass {
    if kind == "Named" {
        return match d.get(b"N".as_slice()).and_then(name).as_deref() {
            Some("NextPage" | "PrevPage" | "FirstPage" | "LastPage") => ActionClass::Navigation,
            _ => ActionClass::State,
        };
    }
    classify(kind)
}

/// Depth of a `/Next` chain the inventory follows (the library's default
/// `max_rewrite_walk_depth`).
const INVENTORY_CHAIN_DEPTH: usize = 512;

/// XFA text as one byte per character: UTF-16 (byte order mark, or `<`
/// as the first code unit with a zero byte) narrowed, non-ASCII → 0xFF; a
/// UTF-8 byte order mark dropped.
fn narrow(b: &[u8]) -> Vec<u8> {
    // UTF-16: a byte order mark, or NUL bytes with `<` as the first
    // non-blank code unit (the library's rule).
    let first = |le: bool| {
        b.as_chunks::<2>()
            .0
            .iter()
            .map(|&[x, y]| if le { (x, y) } else { (y, x) })
            .find(|&(lo, hi)| !(hi == 0 && matches!(lo, b' ' | b'\t' | b'\r' | b'\n')))
    };
    let nul = b.contains(&0);
    let le = b.starts_with(&[0xFF, 0xFE]) || (nul && first(true) == Some((b'<', 0)));
    let be = !le && (b.starts_with(&[0xFE, 0xFF]) || (nul && first(false) == Some((b'<', 0))));
    if !(le || be) {
        return b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b).to_vec();
    }
    let body = if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        &b[2..]
    } else {
        b
    };
    body.as_chunks::<2>()
        .0
        .iter()
        .map(|&[a, b]| {
            let (lo, hi) = if le { (a, b) } else { (b, a) };
            if hi == 0 && lo < 0x80 {
                lo
            } else {
                0xFF
            }
        })
        .collect()
}

/// Numeric character references expanded (ASCII to the byte, others to
/// 0xFF).
fn expand_refs(t: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'&' && t.get(i + 1) == Some(&b'#') {
            let (radix, start) = if matches!(t.get(i + 2), Some(b'x' | b'X')) {
                (16, i + 3)
            } else {
                (10, i + 2)
            };
            let digits = t[start..]
                .iter()
                .take_while(|c| (**c as char).is_digit(radix))
                .count();
            if digits > 0 && t.get(start + digits) == Some(&b';') {
                let v = t[start..start + digits].iter().fold(0u32, |v, c| {
                    v.saturating_mul(radix)
                        .saturating_add((*c as char).to_digit(radix).unwrap_or(0))
                });
                out.push(if v < 0x80 { v as u8 } else { 0xFF });
                i = start + digits + 1;
                continue;
            }
        }
        out.push(t[i]);
        i += 1;
    }
    out
}

/// `script` elements: `<`, an optional `NCName:`, `script`, then white
/// space, `>` or `/`.
fn count_scripts(t: &[u8]) -> u32 {
    let start = |c: u8| c.is_ascii_alphabetic() || c == b'_' || c >= 0x80;
    let more = |c: u8| start(c) || c.is_ascii_digit() || c == b'.' || c == b'-';
    let mut n = 0;
    for (i, _) in t.iter().enumerate().filter(|(_, c)| **c == b'<') {
        let name = |from: usize| from + t[from..].iter().take_while(|c| more(**c)).count();
        let mut j = name(i + 1);
        let mut local = &t[i + 1..j];
        if t.get(j) == Some(&b':') && local.first().is_some_and(|c| start(*c)) {
            let k = name(j + 1);
            local = &t[j + 1..k];
            j = k;
        }
        if local == b"script"
            && matches!(t.get(j), Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/'))
        {
            n += 1;
        }
    }
    n
}

/// One action reached from an owner, in the order the chains are walked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InventoryItem {
    pub class: ActionClass,
    /// The `/S` name (`-` when missing).
    pub kind: String,
    /// Positional owner: `catalog/OpenAction`, `catalog/AA/<trigger>`,
    /// `page<p>/AA/<trigger>`, `annot<p>.<k>/A`, `annot<p>.<k>/AA/<trigger>`,
    /// `field<path>/AA/<trigger>`, `outline<path>/A`, `names/JavaScript#<i>`.
    pub owner: String,
    /// 0 for the owner's action, n for the n-th `/Next` step (depth first).
    pub position: u32,
    /// The first item (index) with the same action object, when shared.
    pub shared_with: Option<usize>,
    /// Length of a script (string, or decoded stream); never its text.
    pub script_bytes: Option<u64>,
    /// The action's (or annotation's, or XFA packet's) input object.
    #[serde(default)]
    pub input: Option<(u32, u16)>,
    /// A script stream that does not decode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub script_undecodable: bool,
}

impl<'a> Ctx<'a> {
    /// Every action owner of the document, in a fixed order, with the
    /// action object it holds (`plans/rewrite-policy.md` §3.1).
    pub(super) fn action_roots(&mut self) -> Res<Vec<(String, Object)>> {
        Ok(self
            .roots()?
            .into_iter()
            .filter_map(|(owner, r)| match r {
                Root::Action(o) => Some((owner, o)),
                Root::Media(..) => None,
            })
            .collect())
    }

    /// [`action_roots`] with the multimedia annotations themselves, each
    /// before the annotation's own actions — the inventory's owners.
    fn roots(&mut self) -> Res<Vec<(String, Root)>> {
        let mut out = Vec::new();
        let cat = self.doc.catalog().clone();
        if let Some(oa) = self.entry_pdf20(&cat, b"OpenAction")? {
            match self.get(&oa)? {
                Object::Dict(_) => out.push(("catalog/OpenAction".to_string(), Root::Action(oa))),
                // An array is a destination (its first element a page),
                // unless its first element is an action dictionary
                // (non-standard, seen): then each member is one.
                Object::Array(a) if !a.is_empty() && self.all_actions(&a)? => {
                    for (i, x) in a.into_iter().enumerate() {
                        out.push((format!("catalog/OpenAction#{i}"), Root::Action(x)));
                    }
                }
                // Unreadable: listed (`#unresolved`), never dropped.
                Object::Name(n) if n == b"#unresolved" => {
                    out.push(("catalog/OpenAction".to_string(), Root::Action(oa)))
                }
                // A destination (array, or a named one as name or
                // string) or an undefined reference (null): no action.
                Object::Array(_) | Object::Name(_) | Object::String(_) | Object::Null => {}
                // Malformed (a number, a stream…): listed as such.
                _ => out.push(("catalog/OpenAction".to_string(), Root::Action(oa))),
            }
        }
        self.aa_roots("catalog", &cat, &mut out)?;
        for i in 0..self.doc.pages().len() {
            let Some(pd) = self.page_dict(i)? else {
                continue;
            };
            self.aa_roots(&format!("page{i}"), &pd, &mut out)?;
            if let Some(a) = pd.get(b"Annots".as_slice()) {
                if let Object::Array(items) = self.get(a)? {
                    for (k, it) in items.iter().enumerate() {
                        let Some(ad) = self.dict(it)? else { continue };
                        let st = ad
                            .get(b"Subtype".as_slice())
                            .and_then(name)
                            .unwrap_or_default();
                        if MULTIMEDIA_ANNOTS.contains(&st.as_str()) {
                            out.push((format!("annot{i}.{k}"), Root::Media(st, it.as_ref())));
                        }
                        if let Some(x) = self.entry_pdf20(&ad, b"A")? {
                            out.push((format!("annot{i}.{k}/A"), Root::Action(x)));
                        }
                        self.aa_roots(&format!("annot{i}.{k}"), &ad, &mut out)?;
                    }
                }
            }
        }
        if let Some(af) = cat.get(b"AcroForm".as_slice()) {
            if let Some(af) = self.dict(af)? {
                if let Some(f) = af.get(b"Fields".as_slice()) {
                    if let Object::Array(fields) = self.get(f)? {
                        let mut seen = HashSet::new();
                        for (i, x) in fields.iter().enumerate() {
                            self.field_roots(x, &i.to_string(), 0, &mut seen, &mut out)?;
                        }
                    }
                }
            }
        }
        if let Some(o) = cat.get(b"Outlines".as_slice()) {
            if let Some(od) = self.dict(o)? {
                let mut seen = HashSet::new();
                self.outline_roots(&od, "", 0, &mut seen, &mut out)?;
            }
        }
        let names = match cat.get(b"Names".as_slice()) {
            Some(n) => self.dict(n)?,
            None => None,
        };
        if let Some(n) = names {
            if let Some(t) = self.entry_pdf20(&n, b"JavaScript")? {
                for (i, (_, v)) in self.tree(&t, false)?.into_iter().enumerate() {
                    out.push((format!("names/JavaScript#{i}"), Root::Action(v)));
                }
            }
        }
        Ok(out)
    }

    /// The first member is a dictionary with `/S` (an action): the
    /// array is a list of actions, not a destination.
    fn all_actions(&mut self, a: &[Object]) -> Res<bool> {
        Ok(match a.first() {
            Some(x) => matches!(self.get(x)?, Object::Dict(d) if d.contains_key(b"S".as_slice())),
            None => false,
        })
    }

    fn aa_roots(&mut self, head: &str, d: &Dict, out: &mut Vec<(String, Root)>) -> Res<()> {
        let Some(aa) = self.entry_pdf20(d, b"AA")? else {
            return Ok(());
        };
        if let Object::Dict(aa) = self.get(&aa)? {
            for (k, v) in &aa {
                out.push((format!("{head}/AA/{}", trigger(k)), Root::Action(v.clone())));
            }
        }
        Ok(())
    }

    /// Field nodes (by index path) that are not annotations on a page:
    /// their `/AA` (a page's widget is listed as an annotation).
    fn field_roots(
        &mut self,
        o: &Object,
        at: &str,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<(String, Root)>,
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
        let on_page = o.as_ref().is_some_and(|r| self.annots.contains_key(&r));
        if !on_page {
            self.aa_roots(&format!("field{at}"), &d, out)?;
        }
        if let Some(k) = d.get(b"Kids".as_slice()) {
            if let Object::Array(kids) = self.get(k)? {
                for (i, k) in kids.iter().enumerate() {
                    self.field_roots(k, &format!("{at}.{i}"), depth + 1, seen, out)?;
                }
            }
        }
        Ok(())
    }

    fn outline_roots(
        &mut self,
        parent: &Dict,
        at: &str,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
        out: &mut Vec<(String, Root)>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("outline tree deeper than the limit");
        }
        let mut cur = parent.get(b"First".as_slice()).cloned();
        let mut i = 0usize;
        while let Some(o) = cur {
            if let Some(r) = o.as_ref() {
                if !seen.insert(r) {
                    return err("cycle in the outline tree");
                }
            }
            let Some(d) = self.dict(&o)? else { break };
            let p = if at.is_empty() {
                i.to_string()
            } else {
                format!("{at}.{i}")
            };
            if let Some(a) = self.entry_pdf20(&d, b"A")? {
                out.push((format!("outline{p}/A"), Root::Action(a)));
            }
            self.outline_roots(&d, &p, depth + 1, seen, out)?;
            cur = d.get(b"Next".as_slice()).cloned();
            i += 1;
        }
        Ok(())
    }

    /// Every action reachable from the owners, `/Next` chains followed
    /// depth first (single or array), with sharing and script lengths.
    pub fn inventory(&mut self) -> Res<Vec<InventoryItem>> {
        let roots = self.roots()?;
        let mut items = Vec::new();
        let mut first: HashMap<IndirectRef, usize> = HashMap::new();
        for (owner, root) in roots {
            let o = match root {
                Root::Action(o) => o,
                Root::Media(subtype, r) => {
                    items.push(InventoryItem {
                        class: ActionClass::Multimedia,
                        kind: subtype,
                        owner,
                        position: 0,
                        shared_with: None,
                        script_bytes: None,
                        input: r.map(|r| (r.number, r.generation)),
                        script_undecodable: false,
                    });
                    continue;
                }
            };
            let mut position = 0u32;
            // (object, depth, chain path for cycles)
            let mut stack: Vec<(Object, usize, Rc<Vec<IndirectRef>>)> = vec![(o, 0, Rc::default())];
            while let Some((o, depth, path)) = stack.pop() {
                if depth > INVENTORY_CHAIN_DEPTH {
                    return err("action chain deeper than the limit");
                }
                let r = o.as_ref();
                if let Some(r) = r {
                    if path.contains(&r) {
                        continue; // a cycle: the chain is listed up to it
                    }
                }
                let d = match self.get(&o)? {
                    Object::Dict(d) => d,
                    // An undefined reference is null (§7.3.10): no action.
                    Object::Null => continue,
                    // An unreadable or malformed action is listed, never
                    // dropped: it is what a policy must refuse.
                    other => {
                        let kind = match other {
                            Object::Name(n) if n == b"#unresolved" => "#unresolved",
                            _ => "#malformed",
                        };
                        items.push(InventoryItem {
                            class: ActionClass::Unknown,
                            kind: kind.into(),
                            owner: owner.clone(),
                            position,
                            shared_with: None,
                            script_bytes: None,
                            input: r.map(|r| (r.number, r.generation)),
                            script_undecodable: false,
                        });
                        position += 1;
                        continue;
                    }
                };
                // `/S` directly or through a reference.
                let kind = match d.get(b"S".as_slice()).map(|s| self.get(s)).transpose()? {
                    Some(Object::Name(n)) => String::from_utf8_lossy(&n).into_owned(),
                    _ => "-".into(),
                };
                let shared_with = r.and_then(|r| first.get(&r).copied());
                let mut script_undecodable = false;
                let script_bytes = if kind == "JavaScript" {
                    match d.get(b"JS".as_slice()).map(|x| self.get(x)).transpose()? {
                        Some(Object::String(s)) => Some(s.len() as u64),
                        Some(Object::Stream { dict, data }) => {
                            match self.decode("script", &dict, &data)? {
                                doclevel::Decoded::Bytes(b) => Some(b.len() as u64),
                                doclevel::Decoded::Undecodable(_) => {
                                    script_undecodable = true;
                                    None
                                }
                            }
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                if let (Some(r), None) = (r, shared_with) {
                    first.insert(r, items.len());
                }
                items.push(InventoryItem {
                    class: classify_action(&kind, &d),
                    kind,
                    owner: owner.clone(),
                    position,
                    shared_with,
                    script_bytes,
                    input: r.map(|r| (r.number, r.generation)),
                    script_undecodable,
                });
                position += 1;
                if shared_with.is_some() {
                    continue; // its chain was listed with its first owner
                }
                if let Some(n) = self.entry_pdf20(&d, b"Next")? {
                    let next = match self.get(&n)? {
                        Object::Array(a) => a,
                        _ => vec![n],
                    };
                    if stack.len() + next.len() > MAX_NODES {
                        return err(format!("action chains past {MAX_NODES} entries"));
                    }
                    let path = match r {
                        Some(r) => {
                            let mut p = (*path).clone();
                            p.push(r);
                            Rc::new(p)
                        }
                        None => path,
                    };
                    for x in next.into_iter().rev() {
                        stack.push((x, depth + 1, Rc::clone(&path)));
                    }
                }
            }
        }
        for (owner, input, count) in self.xfa_scripts()? {
            for n in 0..count {
                items.push(InventoryItem {
                    class: ActionClass::Script,
                    kind: "#xfa-script".into(),
                    owner: owner.clone(),
                    position: n,
                    shared_with: None,
                    script_bytes: None,
                    input,
                    script_undecodable: false,
                });
            }
        }
        Ok(items)
    }

    /// XFA `script` elements by packet (`xfa#<i>` among the packet
    /// streams, `xfa` for a single stream) and those visible only across
    /// packet boundaries (`xfa/across`): `<`, an optional `NCName:`, the
    /// local name `script`, then white space, `>` or `/`, on the decoded
    /// text (UTF-16 narrowed, numeric character references expanded).
    fn xfa_scripts(&mut self) -> Res<Vec<XfaScripts>> {
        let cat = self.doc.catalog().clone();
        let Some(af) = cat.get(b"AcroForm".as_slice()) else {
            return Ok(vec![]);
        };
        let Some(af) = self.dict(af)? else {
            return Ok(vec![]);
        };
        let Some(x) = self.entry_pdf20(&af, b"XFA")? else {
            return Ok(vec![]);
        };
        let text = |c: &mut Self, o: &Object| -> Res<Option<Vec<u8>>> {
            let Object::Stream { dict, data } = c.get(o)? else {
                return Ok(None);
            };
            match c.decode("XFA packet", &dict, &data)? {
                doclevel::Decoded::Bytes(b) => Ok(Some(expand_refs(&narrow(&b)))),
                doclevel::Decoded::Undecodable(_) => err("an XFA packet does not decode"),
            }
        };
        let mut out = Vec::new();
        match self.get(&x)? {
            Object::Array(a) => {
                let (mut whole, mut sum, mut packet) = (Vec::new(), 0u32, 0u32);
                for it in &a {
                    if let Some(t) = text(self, it)? {
                        let n = count_scripts(&t);
                        sum += n;
                        out.push((
                            format!("xfa#{packet}"),
                            it.as_ref().map(|r| (r.number, r.generation)),
                            n,
                        ));
                        whole.extend_from_slice(&t);
                        packet += 1;
                    }
                }
                out.push((
                    "xfa/across".to_string(),
                    None,
                    count_scripts(&whole).saturating_sub(sum),
                ));
            }
            Object::Stream { .. } => {
                let n = text(self, &x)?.map(|t| count_scripts(&t)).unwrap_or(0);
                out.push((
                    "xfa".to_string(),
                    x.as_ref().map(|r| (r.number, r.generation)),
                    n,
                ));
            }
            _ => {}
        }
        Ok(out)
    }

    // ---------------- multimedia ----------------

    /// Multimedia annotations (RichMedia, Screen, Movie, Sound, 3D) by
    /// page and position, the catalog's `/RichMedia`, and multimedia
    /// actions (Rendition, Movie, Sound, RichMediaExecute) by owner and
    /// chain position — each as a whole, streams (media, assets,
    /// embedded files) by decoded digest.
    pub(super) fn multimedia(&mut self) -> Res<Option<Vec<String>>> {
        let mut out = Vec::new();
        // Sharing: each referenced value of an annotation or action (its
        // content, settings, rendition, media) named by the first owner
        // and key that used it.
        let mut first: HashMap<IndirectRef, String> = HashMap::new();
        let cat = self.doc.catalog().clone();
        if let Some(rm) = self.entry_pdf20(&cat, b"RichMedia")? {
            out.push(format!(
                "catalog_richmedia\tvalue={}",
                self.cval(&rm, 1, &mut Vec::new())?
            ));
        }
        for i in 0..self.doc.pages().len() {
            let Some(pd) = self.page_dict(i)? else {
                continue;
            };
            let Some(a) = pd.get(b"Annots".as_slice()) else {
                continue;
            };
            let Object::Array(items) = self.get(a)? else {
                continue;
            };
            for (k, it) in items.iter().enumerate() {
                let ad = match self.get(it)? {
                    Object::Dict(d) | Object::Stream { dict: d, .. } => d,
                    // Possibly a multimedia annotation: not "absent".
                    Object::Name(n) if n == b"#unresolved" => {
                        out.push(format!("annot\tat={i}.{k}\tunresolved"));
                        continue;
                    }
                    _ => continue,
                };
                let st = ad
                    .get(b"Subtype".as_slice())
                    .and_then(name)
                    .unwrap_or_default();
                if MULTIMEDIA_ANNOTS.contains(&st.as_str()) {
                    let mut rest = ad.clone();
                    // Appearance and popup are not the media.
                    for key in ["AP", "Popup"] {
                        rest.remove(key.as_bytes());
                    }
                    let v = self.cval_dict(&rest, 1, &mut Vec::new())?;
                    let shared = sharing(&rest, &format!("annot{i}.{k}"), &mut first);
                    out.push(format!(
                        "annot\tat={i}.{k}\tsubtype=/{st}\tshared={shared}\tvalue={v}"
                    ));
                }
            }
        }
        for it in self.multimedia_actions(&mut first)? {
            out.push(it);
        }
        Ok((!out.is_empty()).then_some(out))
    }

    fn multimedia_actions(&mut self, first: &mut HashMap<IndirectRef, String>) -> Res<Vec<String>> {
        let roots = self.action_roots()?;
        let mut out = Vec::new();
        for (owner, o) in roots {
            let mut position = 0u32;
            let mut stack: Vec<(Object, usize, Rc<Vec<IndirectRef>>)> = vec![(o, 0, Rc::default())];
            while let Some((o, depth, path)) = stack.pop() {
                if depth > MAX_ACTION_DEPTH {
                    return err("action chain deeper than the limit");
                }
                let r = o.as_ref();
                if r.is_some_and(|r| path.contains(&r)) {
                    continue;
                }
                let d = match self.get(&o)? {
                    Object::Dict(d) => d,
                    // Possibly a multimedia action: not "absent".
                    Object::Name(n) if n == b"#unresolved" => {
                        out.push(format!(
                            "action\towner={owner}\tposition={position}\tunresolved"
                        ));
                        position += 1;
                        continue;
                    }
                    Object::Null => continue,
                    // Malformed: listed by the inventory, so it counts.
                    _ => {
                        position += 1;
                        continue;
                    }
                };
                let kind = d.get(b"S".as_slice()).and_then(name).unwrap_or_default();
                if classify(&kind) == ActionClass::Multimedia {
                    let mut rest = d.clone();
                    rest.remove(b"Next".as_slice());
                    let v = self.cval_dict(&rest, 1, &mut Vec::new())?;
                    let label = format!("{owner}@{position}");
                    let mut shared = sharing(&rest, &label, first);
                    if let Some(r) = r {
                        if let Some(f) = first.get(&r) {
                            shared = format!("action={f};{shared}");
                        } else {
                            first.insert(r, label.clone());
                        }
                    }
                    out.push(format!(
                        "action\towner={owner}\tposition={position}\tshared={shared}\tvalue={v}"
                    ));
                }
                position += 1;
                if let Some(n) = self.entry_pdf20(&d, b"Next")? {
                    let next = match self.get(&n)? {
                        Object::Array(a) => a,
                        _ => vec![n],
                    };
                    if stack.len() + next.len() > MAX_NODES {
                        return err(format!("action chains past {MAX_NODES} entries"));
                    }
                    let path = match r {
                        Some(r) => {
                            let mut p = (*path).clone();
                            p.push(r);
                            Rc::new(p)
                        }
                        None => path,
                    };
                    for x in next.into_iter().rev() {
                        stack.push((x, depth + 1, Rc::clone(&path)));
                    }
                }
            }
        }
        Ok(out)
    }

    // ---------------- signatures ----------------

    /// Signature structure (`plans/rewrite-policy.md` §5.3, §5.7):
    /// - every *signature-like* dictionary in the reachable graph (a
    ///   `/ByteRange`; `/Type /Sig` or `/DocTimeStamp`; `/Contents` with
    ///   any `/Filter` or `/SubFilter`; a `/Reference` with
    ///   `/TransformMethod`; `/Cert` with `/Contents`), by the first path
    ///   a breadth-first walk from the trailer reaches it on and the
    ///   number of references to it — never `/Contents`, only its length;
    /// - the catalog's `/Perms` entries, `/AcroForm /SigFlags`, signature
    ///   fields and fields with `/Lock` or `/SV`, `/DSS` (counts),
    ///   `/Legal`, and XFA packets that name the XML-DSig namespace.
    ///
    /// The walk has its own node budget; one that does not finish is an
    /// error, never a partial pass.
    pub(super) fn signatures(&mut self) -> Res<Option<Vec<String>>> {
        let mut w = Ctx::new(self.doc);
        w.doclevel = true;
        let walk = w.signature_walk()?;
        let mut out = Vec::new();
        for (node, r, d) in &walk.sigs {
            let n = r.and_then(|r| walk.refs.get(&r).copied()).unwrap_or(1);
            let at = walk.path(*node)?;
            out.push(format!("sig\tat={at}\trefs={n}\t{}", w.sig_summary(d)?));
        }
        let unresolved = walk.unresolved;
        let path_of = |o: &Object| -> Res<String> {
            match o.as_ref().and_then(|r| walk.first.get(&r)) {
                Some(n) => walk.path(*n),
                None => Ok("direct".into()),
            }
        };
        let cat = self.doc.catalog().clone();
        if let Some(p) = self.entry_pdf20(&cat, b"Perms")? {
            match self.get(&p)? {
                Object::Dict(pd) => {
                    // Located through the walk's own nodes, so a direct
                    // value (an inline /UR3 dictionary) has its path too.
                    let root = self.doc.trailer().get(b"Root".as_slice()).cloned();
                    let root = root.and_then(|r| walk.node_of(None, "/Root", &r));
                    let perms = root.and_then(|n| walk.node_of(Some(n), "/Perms", &p));
                    let at = |n: Option<usize>| -> Res<String> {
                        n.map_or(Ok("unreached".into()), |n| walk.path(n))
                    };
                    out.push(format!("perms\tat={}\tkeys={}", at(perms)?, pd.len()));
                    for (k, v) in &pd {
                        let key = String::from_utf8_lossy(k);
                        let t = perms.and_then(|n| walk.node_of(Some(n), &format!("/{key}"), v));
                        out.push(format!("perms_entry\tkey=/{key}\ttarget={}", at(t)?));
                    }
                }
                other => out.push(format!("perms\tmalformed={}", self.canon0(&other)?)),
            }
        }
        if let Some(af) = cat.get(b"AcroForm".as_slice()).cloned() {
            if let Some(afd) = self.dict(&af)? {
                if let Some(sf) = self.entry_pdf20(&afd, b"SigFlags")? {
                    out.push(format!("sigflags\tvalue={}", self.canon0(&sf)?));
                }
                if let Some(f) = afd.get(b"Fields".as_slice()) {
                    if let Object::Array(fields) = self.get(f)? {
                        let mut seen = HashSet::new();
                        for (i, x) in fields.iter().enumerate() {
                            self.sig_fields(
                                x,
                                &i.to_string(),
                                None,
                                0,
                                &mut seen,
                                &path_of,
                                &mut out,
                            )?;
                        }
                    }
                }
                if let Some(x) = self.entry_pdf20(&afd, b"XFA")? {
                    self.xfa_dsig(&x, &mut out)?;
                }
            }
        }
        if let Some(dss) = self.entry_pdf20(&cat, b"DSS")? {
            match self.get(&dss)? {
                Object::Dict(d) => {
                    let mut parts = Vec::new();
                    for (k, v) in &d {
                        let n = match self.get(v)? {
                            Object::Array(a) => a.len().to_string(),
                            Object::Dict(x) => x.len().to_string(),
                            _ => "1".into(),
                        };
                        parts.push(format!("{}={n}", String::from_utf8_lossy(k)));
                    }
                    out.push(format!("dss\t{}", parts.join(" ")));
                }
                other => out.push(format!("dss\tmalformed={}", self.canon0(&other)?)),
            }
        }
        if let Some(l) = self.entry_pdf20(&cat, b"Legal")? {
            out.push(format!("legal\t{}", self.summary(&l)?));
        }
        if unresolved > 0 {
            out.push(format!("walk\tunresolved={unresolved}"));
        }
        Ok((!out.is_empty()).then_some(out))
    }

    /// Breadth-first walk from the trailer's `/Root` and `/Info`
    /// ([`Walk`]). Each value met is a node with its parent and step, so
    /// memory stays linear in the graph (a path is built only for a line
    /// that prints it); the queue counts against the node budget.
    fn signature_walk(&mut self) -> Res<Walk> {
        let trailer = self.doc.trailer().clone();
        let mut queue: VecDeque<(Object, Option<usize>, Box<str>)> = VecDeque::new();
        for k in ["Root", "Info"] {
            if let Some(v) = trailer.get(k.as_bytes()) {
                queue.push_back((v.clone(), None, format!("/{k}").into()));
            }
        }
        let mut w = Walk::default();
        while let Some((o, parent, step)) = queue.pop_front() {
            if queue.len() + w.nodes.len() > MAX_NODES {
                return err(format!("signature walk stopped after {MAX_NODES} values"));
            }
            let r = o.as_ref();
            if let Some(r) = r {
                *w.refs.entry(r).or_default() += 1;
                if w.first.contains_key(&r) {
                    continue;
                }
            }
            let node = w.nodes.len();
            w.nodes.push((parent, step));
            if let Some(r) = r {
                w.first.insert(r, node);
            }
            let v = self.get(&o)?;
            let (d, stream) = match v {
                Object::Name(n) if n == b"#unresolved" => {
                    w.unresolved += 1;
                    continue;
                }
                Object::Dict(d) => (d, false),
                Object::Stream { dict, .. } => (dict, true),
                Object::Array(a) => {
                    for (i, x) in a.into_iter().enumerate() {
                        if matches!(x, Object::Ref(_) | Object::Array(_) | Object::Dict(_)) {
                            queue.push_back((x, Some(node), format!("[{i}]").into()));
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            if self.signature_like(&d)? {
                w.sigs.push((node, r, d.clone()));
            }
            for (k, x) in d {
                // A stream's length is storage, not content: a rewrite
                // writes it directly (an unreadable `/Length` object
                // disappears, correctly).
                if stream && k.as_slice() == b"Length" {
                    continue;
                }
                if matches!(k.as_slice(), b"Parent" | b"P") {
                    // Back-pointers lead to objects reached by their own path.
                    if let Some(r) = x.as_ref() {
                        *w.refs.entry(r).or_default() += 1;
                        continue;
                    }
                }
                if matches!(x, Object::Ref(_) | Object::Array(_) | Object::Dict(_)) {
                    let step = format!("/{}", String::from_utf8_lossy(&k));
                    queue.push_back((x, Some(node), step.into()));
                }
            }
        }
        Ok(w)
    }

    /// A value by its keys and a digest of its canonical form: a change
    /// is seen, the content (a seed value's certificates, an
    /// attestation's text, locked field names) is not kept in the line.
    fn summary(&mut self, o: &Object) -> Res<String> {
        let c = self.cval(o, 1, &mut Vec::new())?;
        let keys = match self.get(o)? {
            Object::Dict(d) => d
                .keys()
                .map(|k| String::from_utf8_lossy(k).into_owned())
                .collect::<Vec<_>>()
                .join(","),
            _ => "-".into(),
        };
        Ok(format!("keys=[{keys}]:{}", super::sha(c.as_bytes())))
    }

    fn signature_like(&mut self, d: &Dict) -> Res<bool> {
        let has = |k: &str| d.contains_key(k.as_bytes());
        if has("ByteRange")
            || (has("Contents") && (has("Filter") || has("SubFilter")))
            || (has("Cert") && has("Contents"))
        {
            return Ok(true);
        }
        if let Some(t) = d.get(b"Type".as_slice()).and_then(name) {
            if t == "Sig" || t == "DocTimeStamp" {
                return Ok(true);
            }
        }
        if let Some(r) = d.get(b"Reference".as_slice()) {
            for x in self.references(r)? {
                if let Some(e) = self.dict(&x)? {
                    if e.contains_key(b"TransformMethod".as_slice()) {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    /// The signature references of a `/Reference` value: an array's
    /// elements, or a single (malformed but read) dictionary.
    fn references(&mut self, r: &Object) -> Res<Vec<Object>> {
        Ok(match self.get(r)? {
            Object::Array(a) => a,
            Object::Dict(_) => vec![r.clone()],
            _ => vec![],
        })
    }

    /// What a signature-like dictionary declares — kinds, names, sizes;
    /// never `/Contents`, `/Cert`, names or reasons themselves. The
    /// transform parameters are kept whole (canonical form), and they can
    /// name fields. Lines stay in memory; reports carry field *names* of
    /// differences only.
    fn sig_summary(&mut self, d: &Dict) -> Res<String> {
        let nm = |k: &str| {
            d.get(k.as_bytes())
                .and_then(name)
                .unwrap_or_else(|| "-".into())
        };
        let br = match d
            .get(b"ByteRange".as_slice())
            .map(|x| self.get(x))
            .transpose()?
        {
            None => "-".to_string(),
            Some(Object::Array(a)) => format!("{}", a.len()),
            Some(_) => "malformed".into(),
        };
        let contents = match d
            .get(b"Contents".as_slice())
            .map(|x| self.get(x))
            .transpose()?
        {
            None => "-".to_string(),
            Some(Object::String(s)) => s.len().to_string(),
            Some(_) => "malformed".into(),
        };
        let mut reference = Vec::new();
        if let Some(r) = d.get(b"Reference".as_slice()) {
            if !matches!(self.get(r)?, Object::Array(_)) {
                reference.push("not-an-array".into());
            }
            {
                for x in &self.references(r)? {
                    match self.dict(x)? {
                        Some(e) => {
                            let tm = e
                                .get(b"TransformMethod".as_slice())
                                .and_then(name)
                                .unwrap_or_else(|| "-".into());
                            let tp = match e.get(b"TransformParams".as_slice()) {
                                Some(p) => self.cval(p, 1, &mut Vec::new())?,
                                None => "-".into(),
                            };
                            reference.push(format!(
                                "{tm}:{tp}:data={}",
                                e.contains_key(b"Data".as_slice())
                            ));
                        }
                        None => reference.push("malformed".into()),
                    }
                }
            }
        }
        let present: Vec<&str> = [
            "M",
            "Name",
            "Reason",
            "Location",
            "ContactInfo",
            "Prop_Build",
            "Cert",
            "Changes",
        ]
        .into_iter()
        .filter(|k| d.contains_key(k.as_bytes()))
        .collect();
        Ok(format!(
            "type={}\tfilter={}\tsubfilter={}\tbyterange={br}\tcontents_bytes={contents}\treference=[{}]\tpresent={}",
            nm("Type"),
            nm("Filter"),
            nm("SubFilter"),
            reference.join(" "),
            present.join(",")
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn sig_fields(
        &mut self,
        o: &Object,
        at: &str,
        inherited_ft: Option<String>,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
        path_of: &dyn Fn(&Object) -> Res<String>,
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
        let ft = d.get(b"FT".as_slice()).and_then(name).or(inherited_ft);
        let lock = self.entry_pdf20(&d, b"Lock")?;
        let sv = self.entry_pdf20(&d, b"SV")?;
        if ft.as_deref() == Some("Sig") || lock.is_some() || sv.is_some() {
            let v = match self.entry_pdf20(&d, b"V")? {
                None => "-".to_string(),
                Some(v) => match self.get(&v)? {
                    Object::Dict(_) => format!("dict@{}", path_of(&v)?),
                    other => format!("other:{}", self.canon0(&other)?),
                },
            };
            let lock = match lock {
                Some(l) => self.summary(&l)?,
                None => "-".into(),
            };
            let sv = match sv {
                Some(s) => self.summary(&s)?,
                None => "-".into(),
            };
            out.push(format!(
                "field\tpath={at}\tft={}\tv={v}\tlock={lock}\tsv={sv}",
                ft.clone().unwrap_or_else(|| "-".into())
            ));
        }
        if let Some(k) = d.get(b"Kids".as_slice()) {
            if let Object::Array(kids) = self.get(k)? {
                for (i, k) in kids.iter().enumerate() {
                    self.sig_fields(
                        k,
                        &format!("{at}.{i}"),
                        ft.clone(),
                        depth + 1,
                        seen,
                        path_of,
                        out,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// XFA packets whose decoded content names the XML-DSig namespace
    /// (UTF-16 read through its low bytes). Informational: the rewriter's
    /// own check (`xfa_xml_signature`) is the refusal.
    fn xfa_dsig(&mut self, x: &Object, out: &mut Vec<String>) -> Res<()> {
        let streams: Vec<Object> = match self.get(x)? {
            Object::Array(a) => a.into_iter().skip(1).step_by(2).collect(),
            s @ Object::Stream { .. } => vec![s],
            _ => vec![],
        };
        for (i, s) in streams.iter().enumerate() {
            let Object::Stream { dict, data } = self.get(s)? else {
                continue;
            };
            let present = match self.decode("XFA packet", &dict, &data)? {
                doclevel::Decoded::Bytes(b) => {
                    let narrow: Vec<u8> = b.iter().copied().filter(|c| *c != 0).collect();
                    narrow
                        .windows(7)
                        .any(|w| w.eq_ignore_ascii_case(b"xmldsig"))
                }
                doclevel::Decoded::Undecodable(_) => return err("an XFA packet does not decode"),
            };
            if present {
                out.push(format!("xfa_xmldsig\tpacket={i}"));
            }
        }
        Ok(())
    }
}

/// The referenced values of `d` met before, as `key=first-owner/key`
/// (sorted by key); the others are recorded as first met under `owner`.
fn sharing(d: &Dict, owner: &str, first: &mut HashMap<IndirectRef, String>) -> String {
    let mut parts = Vec::new();
    for (k, v) in d {
        // Back-pointers are shared by design (every annotation of a page).
        if matches!(k.as_slice(), b"P" | b"Parent") {
            continue;
        }
        let Some(r) = v.as_ref() else { continue };
        let key = String::from_utf8_lossy(k);
        match first.get(&r) {
            Some(f) => parts.push(format!("{key}={f}")),
            None => {
                first.insert(r, format!("{owner}/{key}"));
            }
        }
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(",")
    }
}

/// The signature walk: every value met (node = parent and step), the
/// first node of each indirect object, references counted, unreadable
/// objects, signature-like dictionaries by node.
#[derive(Default)]
struct Walk {
    nodes: Vec<(Option<usize>, Box<str>)>,
    first: HashMap<IndirectRef, usize>,
    refs: HashMap<IndirectRef, usize>,
    unresolved: usize,
    sigs: Vec<(usize, Option<IndirectRef>, Dict)>,
}

impl Walk {
    /// The node of `o` met as `step` under `parent`: an indirect object's
    /// first node, or the direct value's own node.
    fn node_of(&self, parent: Option<usize>, step: &str, o: &Object) -> Option<usize> {
        match o.as_ref() {
            Some(r) => self.first.get(&r).copied(),
            None => self
                .nodes
                .iter()
                .position(|(p, s)| *p == parent && &**s == step),
        }
    }

    /// The path of `node` from the trailer; an error past the line limit.
    fn path(&self, node: usize) -> Res<String> {
        let mut steps = Vec::new();
        let mut len = 0usize;
        let mut cur = Some(node);
        while let Some(n) = cur {
            let (parent, step) = &self.nodes[n];
            len += step.len();
            if len > MAX_LINE_BYTES {
                return err("a signature path longer than the limit");
            }
            steps.push(&**step);
            cur = *parent;
        }
        steps.reverse();
        Ok(steps.concat())
    }
}
