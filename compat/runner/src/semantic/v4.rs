//! V4 categories of the semantic snapshot, **report only**: multimedia
//! and signatures, and the action inventory the D2 report is checked
//! against (`plans/rewrite-policy.md` §3.5, §5.7).
//!
//! The document-level rules apply: values by canonical form with streams
//! by decoded digest (`cval`), positions instead of object numbers, an
//! unreadable object `#unresolved` (never null), limits `error`.

use super::*;
use std::collections::VecDeque;

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

/// Annotation subtypes that carry multimedia content.
const MULTIMEDIA_ANNOTS: [&str; 5] = ["RichMedia", "Screen", "Movie", "Sound", "3D"];

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
}

impl<'a> Ctx<'a> {
    /// Every action owner of the document, in a fixed order, with the
    /// action object it holds (`plans/rewrite-policy.md` §3.1).
    pub(super) fn action_roots(&mut self) -> Res<Vec<(String, Object)>> {
        let mut out = Vec::new();
        let cat = self.doc.catalog().clone();
        if let Some(oa) = self.entry_pdf20(&cat, b"OpenAction")? {
            // An array is a destination (its first element a page), not
            // an action.
            if let Object::Dict(_) = self.get(&oa)? {
                out.push(("catalog/OpenAction".to_string(), oa));
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
                        if let Some(x) = self.entry_pdf20(&ad, b"A")? {
                            out.push((format!("annot{i}.{k}/A"), x));
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
                    out.push((format!("names/JavaScript#{i}"), v));
                }
            }
        }
        Ok(out)
    }

    fn aa_roots(&mut self, head: &str, d: &Dict, out: &mut Vec<(String, Object)>) -> Res<()> {
        let Some(aa) = self.entry_pdf20(d, b"AA")? else {
            return Ok(());
        };
        if let Object::Dict(aa) = self.get(&aa)? {
            for (k, v) in &aa {
                out.push((
                    format!("{head}/AA/{}", String::from_utf8_lossy(k)),
                    v.clone(),
                ));
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
        out: &mut Vec<(String, Object)>,
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
        out: &mut Vec<(String, Object)>,
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
                out.push((format!("outline{p}/A"), a));
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
        let roots = self.action_roots()?;
        let mut items = Vec::new();
        let mut first: HashMap<IndirectRef, usize> = HashMap::new();
        for (owner, o) in roots {
            let mut position = 0u32;
            // (object, depth, chain path for cycles)
            let mut stack: Vec<(Object, usize, Vec<IndirectRef>)> = vec![(o, 0, Vec::new())];
            while let Some((o, depth, mut path)) = stack.pop() {
                if depth > MAX_ACTION_DEPTH {
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
                        });
                        position += 1;
                        continue;
                    }
                };
                let kind = d
                    .get(b"S".as_slice())
                    .and_then(name)
                    .unwrap_or_else(|| "-".into());
                let shared_with = r.and_then(|r| first.get(&r).copied());
                let script_bytes = if kind == "JavaScript" {
                    match d.get(b"JS".as_slice()).map(|x| self.get(x)).transpose()? {
                        Some(Object::String(s)) => Some(s.len() as u64),
                        Some(Object::Stream { dict, data }) => {
                            let c = self.digest("script", &dict, &data)?;
                            c.split(':').next().and_then(|n| n.parse().ok())
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
                    class: classify(&kind),
                    kind,
                    owner: owner.clone(),
                    position,
                    shared_with,
                    script_bytes,
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
                    if let Some(r) = r {
                        path.push(r);
                    }
                    for x in next.into_iter().rev() {
                        stack.push((x, depth + 1, path.clone()));
                    }
                }
            }
        }
        Ok(items)
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
                    Object::Dict(d) => d,
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
            let mut stack: Vec<(Object, usize, Vec<IndirectRef>)> = vec![(o, 0, Vec::new())];
            while let Some((o, depth, mut path)) = stack.pop() {
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
                    if let Some(r) = r {
                        path.push(r);
                    }
                    for x in next.into_iter().rev() {
                        stack.push((x, depth + 1, path.clone()));
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
        let (paths, refs, unresolved, sigs) = w.signature_walk()?;
        let mut out = Vec::new();
        for (at, d) in &sigs {
            let r = paths.iter().find(|(_, p)| p == at).map(|(r, _)| *r);
            let n = r.and_then(|r| refs.get(&r).copied()).unwrap_or(1);
            out.push(format!("sig\tat={at}\trefs={n}\t{}", w.sig_summary(d)?));
        }
        let path_of = |o: &Object| -> String {
            match o.as_ref().and_then(|r| paths.iter().find(|(x, _)| *x == r)) {
                Some((_, p)) => p.clone(),
                None => "direct".into(),
            }
        };
        let cat = self.doc.catalog().clone();
        if let Some(p) = self.entry_pdf20(&cat, b"Perms")? {
            match self.get(&p)? {
                Object::Dict(pd) => {
                    out.push(format!("perms\tat={}\tkeys={}", path_of(&p), pd.len()));
                    for (k, v) in &pd {
                        out.push(format!(
                            "perms_entry\tkey=/{}\ttarget={}",
                            String::from_utf8_lossy(k),
                            path_of(v)
                        ));
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
            out.push(format!(
                "legal\tvalue={}",
                self.cval(&l, 1, &mut Vec::new())?
            ));
        }
        if unresolved > 0 {
            out.push(format!("walk\tunresolved={unresolved}"));
        }
        Ok((!out.is_empty()).then_some(out))
    }

    /// Breadth-first walk from the trailer's `/Root` and `/Info`: the
    /// first path of every indirect object, the number of references to
    /// it, the unreadable objects met, and the signature-like
    /// dictionaries (by first path, in walk order).
    #[allow(clippy::type_complexity)]
    fn signature_walk(
        &mut self,
    ) -> Res<(
        Vec<(IndirectRef, String)>,
        HashMap<IndirectRef, usize>,
        usize,
        Vec<(String, Dict)>,
    )> {
        let trailer = self.doc.trailer().clone();
        let mut queue: VecDeque<(Object, String)> = VecDeque::new();
        for k in ["Root", "Info"] {
            if let Some(v) = trailer.get(k.as_bytes()) {
                queue.push_back((v.clone(), format!("/{k}")));
            }
        }
        let mut paths: Vec<(IndirectRef, String)> = Vec::new();
        let mut known: HashSet<IndirectRef> = HashSet::new();
        let mut refs: HashMap<IndirectRef, usize> = HashMap::new();
        let mut unresolved = 0usize;
        let mut sigs = Vec::new();
        while let Some((o, at)) = queue.pop_front() {
            if let Some(r) = o.as_ref() {
                *refs.entry(r).or_default() += 1;
                if !known.insert(r) {
                    continue;
                }
                paths.push((r, at.clone()));
            }
            let v = self.get(&o)?;
            let d = match &v {
                Object::Name(n) if n == b"#unresolved" => {
                    unresolved += 1;
                    continue;
                }
                Object::Dict(d) => Some(d.clone()),
                Object::Stream { dict, .. } => Some(dict.clone()),
                Object::Array(a) => {
                    for (i, x) in a.iter().enumerate() {
                        if matches!(x, Object::Ref(_) | Object::Array(_) | Object::Dict(_)) {
                            queue.push_back((x.clone(), format!("{at}[{i}]")));
                        }
                    }
                    None
                }
                _ => None,
            };
            let Some(d) = d else { continue };
            if self.signature_like(&d)? {
                sigs.push((at.clone(), d.clone()));
            }
            for (k, x) in &d {
                if k.as_slice() == b"Parent" || k.as_slice() == b"P" {
                    // Back-pointers lead to objects reached by their own path.
                    if matches!(x, Object::Ref(_)) {
                        if let Some(r) = x.as_ref() {
                            *refs.entry(r).or_default() += 1;
                        }
                        continue;
                    }
                }
                if matches!(x, Object::Ref(_) | Object::Array(_) | Object::Dict(_)) {
                    queue.push_back((x.clone(), format!("{at}/{}", String::from_utf8_lossy(k))));
                }
            }
        }
        Ok((paths, refs, unresolved, sigs))
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
            if let Object::Array(a) = self.get(r)? {
                for x in &a {
                    if let Some(e) = self.dict(x)? {
                        if e.contains_key(b"TransformMethod".as_slice()) {
                            return Ok(true);
                        }
                    }
                }
            }
        }
        Ok(false)
    }

    /// What a signature-like dictionary declares — kinds, names, sizes;
    /// never `/Contents`, `/Cert`, names or reasons themselves.
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
            if let Object::Array(a) = self.get(r)? {
                for x in &a {
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
        path_of: &dyn Fn(&Object) -> String,
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
                    Object::Dict(_) => format!("dict@{}", path_of(&v)),
                    other => format!("other:{}", self.canon0(&other)?),
                },
            };
            let lock = match lock {
                Some(l) => self.cval(&l, 1, &mut Vec::new())?,
                None => "-".into(),
            };
            let sv = match sv {
                Some(s) => self.cval(&s, 1, &mut Vec::new())?,
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
