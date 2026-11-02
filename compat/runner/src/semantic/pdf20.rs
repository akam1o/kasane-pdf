//! PDF 2.0 categories of the semantic snapshot — **report only**: the
//! declared and effective version, page-level metadata, associated files
//! (`/AF`) and document parts (DPart). They are taken with their own
//! node budget after the other categories, are reported next to them
//! (`SemanticRoundTrip::report_only`) and never enter the overall class,
//! so adding them changes no existing result.
//!
//! The rules of the document-level categories apply (`doclevel.rs`):
//! streams are compared by the length and digest of their decoded bytes
//! (a change of compression is not a difference, an unsupported filter
//! is `unsupported`), a value cut at the depth limit, an over-long line
//! or the decoded-byte budget make the category `error`, an unreadable
//! object is `#unresolved` (not null), and objects are named by position
//! (page, annotation, tree path, first owner of a shared object), never
//! by number.
//!
//! Responsibilities next to the earlier categories: the catalog's
//! `/Metadata` stays in `metadata`, `page_metadata` takes the pages'
//! own. `embedded_files` (unchanged) lists the catalog `/AF` with a few
//! fields of each file specification; `associated_files` covers every
//! `/AF` owner with the whole file specification and the sharing of
//! file specifications and embedded-file streams.

use super::*;

/// The report-only categories, in snapshot order.
pub const REPORT_ONLY: [&str; 4] = ["version", "page_metadata", "associated_files", "dparts"];

/// Nesting of form XObjects followed for `/AF` (resources of resources):
/// the general tree limit — resources can nest deeper than what content
/// draws (the engine's `max_xobject_depth` bounds drawing, 32).
const MAX_XOBJECT_DEPTH: usize = MAX_DEPTH;

/// Lines that start with this record the *declared form* (how a value is
/// written), compared for the notes only — never a difference.
const DECLARED: char = '~';

/// The report-only sections of `doc`, whose file bytes are `bytes`.
pub fn snapshot_report(doc: &Document, bytes: &[u8]) -> Vec<Section> {
    let mut ctx = Ctx::new(doc);
    REPORT_ONLY
        .iter()
        .map(|c| {
            let r = match *c {
                "version" => ctx.strict_with(|cx| cx.version(bytes)),
                "page_metadata" => ctx.strict(Ctx::page_metadata),
                "associated_files" => ctx.strict(Ctx::associated_files),
                "dparts" => ctx.strict(Ctx::dparts),
                _ => unreachable!(),
            };
            section(r)
        })
        .collect()
}

fn section(r: Res<Option<Vec<String>>>) -> Section {
    match r {
        Ok(Some(mut v)) if !v.is_empty() => {
            v.sort();
            Section::Data(v)
        }
        Ok(_) => Section::Absent,
        Err(Snag::Unsupported(m)) => Section::Unsupported(m),
        Err(Snag::Error(m)) => Section::Error(m),
    }
}

/// Compare the report-only sections of an original and its rewrite.
pub fn compare_report(a: &Document, a_bytes: &[u8], b: &Document, b_bytes: &[u8]) -> Vec<SemCheck> {
    let (sa, sb) = (snapshot_report(a, a_bytes), snapshot_report(b, b_bytes));
    REPORT_ONLY
        .iter()
        .zip(sa.iter().zip(sb.iter()))
        .map(|(c, (x, y))| compare_report_section(c, x, y))
        .collect()
}

/// `compare_section` with the declared-form lines set aside: they never
/// make a difference, but a change is named in the notes.
pub(super) fn compare_report_section(category: &str, a: &Section, b: &Section) -> SemCheck {
    let split = |s: &Section| -> (Section, Vec<String>) {
        match s {
            Section::Data(v) => {
                let (decl, rest): (Vec<String>, Vec<String>) =
                    v.iter().cloned().partition(|l| l.starts_with(DECLARED));
                let rest = if rest.is_empty() {
                    Section::Absent
                } else {
                    Section::Data(rest)
                };
                (rest, decl)
            }
            other => (other.clone(), vec![]),
        }
    };
    let ((ra, da), (rb, db)) = (split(a), split(b));
    let mut c = compare_section(category, &ra, &rb);
    if da != db {
        let fields: Vec<String> = da
            .iter()
            .chain(db.iter())
            .filter(|l| !(da.contains(l) && db.contains(l)))
            .filter_map(|l| l[1..].split('\t').next().map(str::to_string))
            .fold(Vec::new(), |mut v, f| {
                if !v.contains(&f) {
                    v.push(f);
                }
                v
            });
        c.notes.push(format!(
            "declared form changed (not a difference): {}",
            fields.join(", ")
        ));
    }
    c
}

/// `M.m` → (M, m), up to three digits each — looser than the engine's
/// header check (`1.0`–`1.9`, `2.0`) on purpose: the snapshot records
/// what is declared; a version the engine does not accept is never
/// rewritten, so never compared.
fn parse_version(s: &[u8]) -> Option<(u32, u32)> {
    let s = std::str::from_utf8(s).ok()?;
    let (m, n) = s.split_once('.')?;
    let ok = |t: &str| !t.is_empty() && t.len() <= 3 && t.bytes().all(|c| c.is_ascii_digit());
    if !ok(m) || !ok(n) {
        return None;
    }
    Some((m.parse().ok()?, n.parse().ok()?))
}

impl<'a> Ctx<'a> {
    /// `strict` for a category that needs more than the context.
    fn strict_with(
        &mut self,
        f: impl FnOnce(&mut Self) -> Res<Option<Vec<String>>>,
    ) -> Res<Option<Vec<String>>> {
        self.truncated = false;
        self.doclevel = true;
        let r = f(self);
        self.doclevel = false;
        let r = r?;
        if self.truncated {
            return err("a value is nested deeper than the limit");
        }
        if let Some(v) = &r {
            if v.iter().any(|l| l.len() > MAX_LINE_BYTES) {
                return err("a snapshot line is longer than the limit");
            }
        }
        Ok(r)
    }

    // ---------------- version ----------------

    /// The effective version (§7.2.2, ISO 32000-2 §7.2.2 and §7.7.2: the
    /// catalog `/Version` applies when it is later than the header's),
    /// compared; the header token and the catalog entry as declared
    /// (present, absent, indirect, malformed), reported only. Read from
    /// the file bytes and the catalog by the snapshot itself.
    pub(super) fn version(&mut self, bytes: &[u8]) -> Res<Option<Vec<String>>> {
        let head = &bytes[..bytes.len().min(1024)];
        let header = head.windows(5).position(|w| w == b"%PDF-").map(|i| {
            let t = &head[i + 5..];
            let end = t
                .iter()
                .position(|c| !(c.is_ascii_digit() || *c == b'.'))
                .unwrap_or(t.len());
            &t[..end]
        });
        let hv = header.and_then(parse_version);
        let mut out = Vec::new();
        out.push(format!(
            "{DECLARED}header\tdeclared={}",
            match (header, hv) {
                (None, _) => "absent".to_string(),
                (Some(_), Some((m, n))) => format!("{m}.{n}"),
                (Some(t), None) => format!("malformed:{}", hex(t)),
            }
        ));
        let cat = self.doc.catalog().clone();
        let (cv, form) = match cat.get(b"Version".as_slice()) {
            None => (None, "absent".to_string()),
            Some(o) => {
                let indirect = o.as_ref().is_some();
                match self.get(o)? {
                    Object::Null => (None, "null".to_string()),
                    Object::Name(n) => match parse_version(&n) {
                        Some((m, k)) => (
                            Some((m, k)),
                            format!("{}{m}.{k}", if indirect { "indirect:" } else { "" }),
                        ),
                        None => (None, format!("malformed:{}", hex(&n))),
                    },
                    other => (None, format!("malformed:{}", self.canon0(&other)?)),
                }
            }
        };
        out.push(format!("{DECLARED}catalog\tdeclared={form}"));
        let effective = match (hv, cv) {
            (Some(h), Some(c)) => Some(h.max(c)),
            (Some(h), None) => Some(h),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        };
        out.push(match effective {
            Some((m, n)) => format!("effective\tversion={m}.{n}"),
            None => "effective\tversion=unknown".to_string(),
        });
        Ok(Some(out))
    }

    // ---------------- page metadata ----------------

    /// Each page's own `/Metadata` stream (not inheritable, §14.3.2) by
    /// page position and decoded content; a stream shared by pages is
    /// named by the first page that has it.
    pub(super) fn page_metadata(&mut self) -> Res<Option<Vec<String>>> {
        let mut out = Vec::new();
        let mut first: HashMap<IndirectRef, usize> = HashMap::new();
        for i in 0..self.doc.pages().len() {
            let Some(pd) = self.page_dict(i)? else {
                continue;
            };
            let Some(m) = self.entry_pdf20(&pd, b"Metadata")? else {
                continue;
            };
            let shared = match m.as_ref() {
                Some(r) => match first.get(&r) {
                    Some(p) => format!("p{p}"),
                    None => {
                        first.insert(r, i);
                        "-".into()
                    }
                },
                None => "direct".into(),
            };
            let line = match self.get(&m)? {
                Object::Stream { dict, data } => {
                    let (state, d, c) = self.stream_parts("page XMP", &dict, &data)?;
                    format!("page\tp={i}\tstate={state}\tshared={shared}\tdict={d}\tcontent={c}")
                }
                other => format!(
                    "page\tp={i}\tstate=not-a-stream\tshared={shared}\tvalue={}",
                    self.canon0(&other)?
                ),
            };
            out.push(line);
        }
        Ok((!out.is_empty()).then_some(out))
    }

    /// Page `i`'s dictionary, `None` when the page has no object.
    fn page_dict(&mut self, i: usize) -> Res<Option<Dict>> {
        let Some(r) = self.doc.pages().get(i).and_then(|p| p.object_ref()) else {
            return Ok(None);
        };
        self.dict(&Object::Ref(r))
    }

    /// `d[key]` unless absent or null (an undefined object is null,
    /// §7.3.10; an unreadable one is not).
    fn entry_pdf20(&mut self, d: &Dict, key: &[u8]) -> Res<Option<Object>> {
        match d.get(key) {
            None => Ok(None),
            Some(v) => {
                let null = match v {
                    Object::Null => true,
                    Object::Ref(_) => matches!(self.get(v)?, Object::Null),
                    _ => false,
                };
                Ok((!null).then(|| v.clone()))
            }
        }
    }

    /// State, other dictionary entries and decoded content of a stream.
    fn stream_parts(
        &mut self,
        what: &str,
        dict: &Dict,
        data: &[u8],
    ) -> Res<(String, String, String)> {
        let c = self.stream_digest(what, dict, data)?;
        let state = if c.starts_with("undecodable:") {
            "undecodable"
        } else if c.starts_with("0:") {
            "empty"
        } else {
            "present"
        };
        let mut rest = dict.clone();
        for k in ["Length", "Filter", "DecodeParms", "DL"] {
            rest.remove(k.as_bytes());
        }
        let d = self.cval_dict(&rest, 1, &mut Vec::new())?;
        Ok((state.into(), d, c))
    }

    fn stream_digest(&mut self, what: &str, dict: &Dict, data: &[u8]) -> Res<String> {
        self.digest(what, dict, data)
    }

    /// A value with streams compared by decoded content (the
    /// document-level `canon` hashes stream bytes as stored).
    fn cval(&mut self, o: &Object, depth: usize, path: &mut Vec<IndirectRef>) -> Res<String> {
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
            if path.contains(&r) {
                return Ok("<cycle>".into());
            }
            path.push(r);
            let v = self.get(o)?;
            let s = self.cval(&v, depth + 1, path);
            path.pop();
            return s;
        }
        Ok(match o {
            Object::Array(a) => {
                let mut v = Vec::new();
                for x in a {
                    v.push(self.cval(x, depth + 1, path)?);
                }
                format!("[{}]", v.join(" "))
            }
            Object::Dict(d) => self.cval_dict(d, depth, path)?,
            Object::Stream { dict, data } => {
                let c = self.stream_digest("stream", dict, data)?;
                let mut rest = dict.clone();
                for k in ["Length", "Filter", "DecodeParms", "DL"] {
                    rest.remove(k.as_bytes());
                }
                format!("stream{}content={c}", self.cval_dict(&rest, depth, path)?)
            }
            other => self.canon(other, depth, path)?,
        })
    }

    fn cval_dict(&mut self, d: &Dict, depth: usize, path: &mut Vec<IndirectRef>) -> Res<String> {
        let mut v = Vec::new();
        for (k, x) in d {
            if k.as_slice() == b"Parent" {
                continue;
            }
            v.push(format!(
                "/{} {}",
                String::from_utf8_lossy(k),
                self.cval(x, depth + 1, path)?
            ));
        }
        Ok(format!("<<{}>>", v.join(" ")))
    }

    // ---------------- associated files ----------------

    /// Every `/AF` array (ISO 32000-2 §14.13) by owner — the catalog,
    /// each page, annotation (page and position), structure element
    /// (tree path), XObject and marked-content property list in a page's
    /// resources (resource-name path; form XObjects followed), annotation
    /// appearance stream (`/AP` entries, states included; their resources
    /// walked like a page's), and document part (tree path) — and each file specification in it, in
    /// order, as a whole: names, description, `/AFRelationship`, every
    /// other key, `/EF` streams by decoded content with their `/Params`.
    /// A file specification or embedded-file stream reached a second time
    /// is named by its first owner and position, so sharing is compared
    /// and object numbers are not.
    pub(super) fn associated_files(&mut self) -> Res<Option<Vec<String>>> {
        let mut out = Vec::new();
        let mut seen: HashMap<IndirectRef, String> = HashMap::new();
        let cat = self.doc.catalog().clone();
        self.af_owner("catalog", &cat, &mut seen, &mut out)?;
        for i in 0..self.doc.pages().len() {
            let Some(pd) = self.page_dict(i)? else {
                continue;
            };
            self.af_owner(&format!("page:p{i}"), &pd, &mut seen, &mut out)?;
            if let Some(a) = pd.get(b"Annots".as_slice()) {
                if let Object::Array(items) = self.get(a)? {
                    for (k, it) in items.iter().enumerate() {
                        if let Some(ad) = self.dict(it)? {
                            let at = format!("annot:{i}.{k}");
                            self.af_owner(&at, &ad, &mut seen, &mut out)?;
                            self.appearance_afs(&at, &ad, &mut seen, &mut out)?;
                        }
                    }
                }
            }
            if let Some(res) = self.page_resources(&pd)? {
                let mut stack = Vec::new();
                self.resource_afs(
                    &format!("res:p{i}"),
                    &res,
                    0,
                    &mut stack,
                    &mut seen,
                    &mut out,
                )?;
            }
        }
        if let Some(root) = cat.get(b"StructTreeRoot".as_slice()).cloned() {
            if let Some(root) = self.dict(&root)? {
                if let Some(k) = root.get(b"K".as_slice()).cloned() {
                    let mut on_path = HashSet::new();
                    self.struct_afs(&k, "", 0, &mut on_path, &mut seen, &mut out)?;
                }
            }
        }
        for (path, node) in self.dpart_nodes()? {
            if let Some(d) = self.dict(&node)? {
                self.af_owner(&format!("dpart:{path}"), &d, &mut seen, &mut out)?;
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }

    fn af_owner(
        &mut self,
        owner: &str,
        d: &Dict,
        seen: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        let Some(af) = self.entry_pdf20(d, b"AF")? else {
            return Ok(());
        };
        let items = match self.get(&af)? {
            Object::Array(a) => a,
            other => {
                out.push(format!(
                    "owner\towner={owner}\tmalformed={}",
                    self.canon0(&other)?
                ));
                return Ok(());
            }
        };
        out.push(format!("owner\towner={owner}\tcount={}", items.len()));
        for (k, fs) in items.iter().enumerate() {
            let at = format!("{owner}#{k}");
            let id = match fs.as_ref() {
                Some(r) => match seen.get(&r) {
                    Some(first) => {
                        out.push(format!("af\tat={at}\tsame_as={first}"));
                        continue;
                    }
                    None => {
                        seen.insert(r, at.clone());
                        "indirect"
                    }
                },
                None => "direct",
            };
            let body = match self.get(fs)? {
                Object::Dict(fd) => self.filespec_full(&fd, &at, seen)?,
                Object::String(s) => format!("string={}", text(&s)),
                other => format!("malformed={}", self.canon0(&other)?),
            };
            out.push(format!("af\tat={at}\tobj={id}\t{body}"));
        }
        Ok(())
    }

    /// A file specification dictionary as a whole; its `/EF` streams by
    /// decoded content, each named by first owner when shared.
    fn filespec_full(
        &mut self,
        fd: &Dict,
        at: &str,
        seen: &mut HashMap<IndirectRef, String>,
    ) -> Res<String> {
        let mut parts = Vec::new();
        for (k, v) in fd {
            let key = String::from_utf8_lossy(k).into_owned();
            if k.as_slice() == b"EF" {
                let ef = match self.get(v)? {
                    Object::Dict(e) => e,
                    other => {
                        parts.push(format!("/EF malformed:{}", self.canon0(&other)?));
                        continue;
                    }
                };
                let mut streams = Vec::new();
                for (ek, ev) in &ef {
                    let ename = String::from_utf8_lossy(ek).into_owned();
                    let label = format!("{at}/EF/{ename}");
                    let shared = match ev.as_ref() {
                        Some(r) => match seen.get(&r) {
                            Some(first) => Some(first.clone()),
                            None => {
                                seen.insert(r, label.clone());
                                None
                            }
                        },
                        None => None,
                    };
                    let s = match shared {
                        Some(first) => format!("/{ename} same_as={first}"),
                        None => match self.get(ev)? {
                            Object::Stream { dict, data } => {
                                let (state, d, c) =
                                    self.stream_parts("embedded file", &dict, &data)?;
                                format!("/{ename} state={state} dict={d} content={c}")
                            }
                            Object::Null => format!("/{ename} null"),
                            other => format!("/{ename} not-a-stream:{}", self.canon0(&other)?),
                        },
                    };
                    streams.push(s);
                }
                parts.push(format!("/EF {{{}}}", streams.join(" ")));
            } else {
                parts.push(format!("/{key} {}", self.cval(v, 1, &mut Vec::new())?));
            }
        }
        Ok(format!("fs=<<{}>>", parts.join(" ")))
    }

    /// `/AF` of an annotation's appearance streams (`/AP` `/N` `/R` `/D`,
    /// a stream or a dictionary of appearance states) — each a form
    /// XObject, an `/AF` owner like any other — and of their resources.
    fn appearance_afs(
        &mut self,
        at: &str,
        ad: &Dict,
        seen: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        let Some(ap) = ad.get(b"AP".as_slice()) else {
            return Ok(());
        };
        let Some(ap) = self.dict(ap)? else {
            return Ok(());
        };
        for (kind, v) in &ap {
            let streams: Vec<(String, Object)> = match self.get(v)? {
                Object::Stream { .. } => vec![(String::new(), v.clone())],
                Object::Dict(states) => states
                    .iter()
                    .map(|(s, x)| (format!(":{}", String::from_utf8_lossy(s)), x.clone()))
                    .collect(),
                _ => vec![],
            };
            for (state, x) in streams {
                let owner = format!("{at}/ap:{}{state}", String::from_utf8_lossy(kind));
                // As in `resource_afs`: only a stream is an owner, and it
                // claims its first-owner slot once it is known to be one.
                let Object::Stream { dict, .. } = self.get(&x)? else {
                    continue;
                };
                if let Some(r) = x.as_ref() {
                    if let Some(first) = seen.get(&r) {
                        if dict.contains_key(b"AF".as_slice()) {
                            out.push(format!("owner\towner={owner}\tsame_as={first}"));
                        }
                        continue;
                    }
                    seen.insert(r, owner.clone());
                }
                self.af_owner(&owner, &dict, seen, out)?;
                if let Some(res) = dict.get(b"Resources".as_slice()) {
                    if let Some(res) = self.dict(res)? {
                        let mut stack: Vec<IndirectRef> = x.as_ref().into_iter().collect();
                        self.resource_afs(&owner, &res, 1, &mut stack, seen, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The page's resource dictionary, inherited through `/Parent`
    /// (§7.7.3.4) when the page has none.
    fn page_resources(&mut self, pd: &Dict) -> Res<Option<Dict>> {
        let mut d = pd.clone();
        for _ in 0..MAX_DEPTH {
            if let Some(r) = d.get(b"Resources".as_slice()).cloned() {
                return self.dict(&r);
            }
            match d.get(b"Parent".as_slice()).cloned() {
                Some(p) => match self.dict(&p)? {
                    Some(pp) => d = pp,
                    None => return Ok(None),
                },
                None => return Ok(None),
            }
        }
        err("page tree deeper than the limit")
    }

    /// `/AF` of the XObjects (forms followed into their own resources)
    /// and marked-content property lists of a resource dictionary. An
    /// XObject reached again (shared by pages or forms) is followed once:
    /// later owners with an `/AF` name the first.
    fn resource_afs(
        &mut self,
        at: &str,
        res: &Dict,
        depth: usize,
        stack: &mut Vec<IndirectRef>,
        seen: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_XOBJECT_DEPTH {
            return err("form XObjects nested deeper than the limit");
        }
        if let Some(props) = res.get(b"Properties".as_slice()) {
            if let Some(props) = self.dict(props)? {
                for (k, v) in &props {
                    if let Some(pl) = self.dict(v)? {
                        let owner = format!("{at}/props:{}", String::from_utf8_lossy(k));
                        self.af_owner(&owner, &pl, seen, out)?;
                    }
                }
            }
        }
        let Some(xo) = res.get(b"XObject".as_slice()) else {
            return Ok(());
        };
        let Some(xo) = self.dict(xo)? else {
            return Ok(());
        };
        for (k, v) in &xo {
            let owner = format!("{at}/xobject:{}", String::from_utf8_lossy(k));
            let r = v.as_ref();
            if let Some(r) = r {
                if stack.contains(&r) {
                    continue; // a form drawing itself: the rest of the chain is listed
                }
            }
            let Object::Stream { dict, .. } = self.get(v)? else {
                continue;
            };
            if let Some(r) = r {
                if let Some(first) = seen.get(&r) {
                    if dict.contains_key(b"AF".as_slice()) {
                        out.push(format!("owner	owner={owner}	same_as={first}"));
                    }
                    continue;
                }
                seen.insert(r, owner.clone());
            }
            self.af_owner(&owner, &dict, seen, out)?;
            let is_form = dict.get(b"Subtype".as_slice()).and_then(name).as_deref() == Some("Form");
            if is_form {
                if let Some(sub) = dict.get(b"Resources".as_slice()) {
                    if let Some(sub) = self.dict(sub)? {
                        if let Some(r) = r {
                            stack.push(r);
                        }
                        let res = self.resource_afs(&owner, &sub, depth + 1, stack, seen, out);
                        if r.is_some() {
                            stack.pop();
                        }
                        res?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `/AF` of the structure elements, by child-index path.
    fn struct_afs(
        &mut self,
        k: &Object,
        at: &str,
        depth: usize,
        on_path: &mut HashSet<IndirectRef>,
        seen: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("structure tree deeper than the limit");
        }
        let items = match self.get(k)? {
            Object::Array(a) => a,
            _ => vec![k.clone()],
        };
        for (i, it) in items.iter().enumerate() {
            let p = if at.is_empty() {
                i.to_string()
            } else {
                format!("{at}.{i}")
            };
            let r = it.as_ref();
            if let Some(r) = r {
                if !on_path.insert(r) {
                    return err("cycle in the structure tree");
                }
            }
            let res = (|| -> Res<()> {
                let Some(d) = self.dict(it)? else {
                    return Ok(());
                };
                // A structure element has /S; a marked-content reference
                // (/MCR) carries no /AF. An object reference (/OBJR)
                // carries none either, but the object it names may: an
                // annotation is an owner already (by page position), an
                // XObject is one here unless a page's resources listed
                // it — then it is named by that first owner.
                if !d.contains_key(b"S".as_slice()) {
                    if let Some(obj) = d.get(b"Obj".as_slice()).cloned() {
                        self.objr_afs(&format!("struct:{p}/objr"), &obj, seen, out)?;
                    }
                    return Ok(());
                }
                self.af_owner(&format!("struct:{p}"), &d, seen, out)?;
                if let Some(kids) = d.get(b"K".as_slice()).cloned() {
                    self.struct_afs(&kids, &p, depth + 1, on_path, seen, out)?;
                }
                Ok(())
            })();
            if let Some(r) = r {
                on_path.remove(&r);
            }
            res?;
        }
        Ok(())
    }

    /// The object of a structure element's `/OBJR`: an XObject not
    /// reached before is an owner (its resources walked like a page's);
    /// one reached before is named by its first owner when it has an
    /// `/AF`; an annotation on a page is already listed by position.
    fn objr_afs(
        &mut self,
        owner: &str,
        obj: &Object,
        seen: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        let r = obj.as_ref();
        if r.is_some_and(|r| self.annots.contains_key(&r)) {
            return Ok(());
        }
        let Object::Stream { dict, .. } = self.get(obj)? else {
            return Ok(());
        };
        if let Some(r) = r {
            if let Some(first) = seen.get(&r) {
                if dict.contains_key(b"AF".as_slice()) {
                    out.push(format!("owner\towner={owner}\tsame_as={first}"));
                }
                return Ok(());
            }
            seen.insert(r, owner.to_string());
        }
        self.af_owner(owner, &dict, seen, out)?;
        let is_form = dict.get(b"Subtype".as_slice()).and_then(name).as_deref() == Some("Form");
        if is_form {
            if let Some(res) = dict.get(b"Resources".as_slice()) {
                if let Some(res) = self.dict(res)? {
                    let mut stack: Vec<IndirectRef> = r.into_iter().collect();
                    self.resource_afs(owner, &res, 1, &mut stack, seen, out)?;
                }
            }
        }
        Ok(())
    }

    // ---------------- document parts ----------------

    /// The DPart tree (ISO 32000-2 §14.12): the root's entries, each node
    /// by tree path — its `/Start` and `/End` pages, `/DPM`, number of
    /// children, whether `/Parent` points at its parent, every other key
    /// — and each page's `/DPart` as the path of the node it names. A
    /// node reached twice is named by its first path (sharing), a node
    /// on its own ancestry by the path it returns to (a cycle). `/AF` of
    /// a node is in `associated_files`.
    pub(super) fn dparts(&mut self) -> Res<Option<Vec<String>>> {
        let cat = self.doc.catalog().clone();
        let Some(root) = self.entry_pdf20(&cat, b"DPartRoot")? else {
            let mut out = Vec::new();
            self.page_dpart_lines(&HashMap::new(), &mut out)?;
            return Ok((!out.is_empty()).then_some(out));
        };
        let root_ref = root.as_ref();
        let Some(rd) = self.dict(&root)? else {
            return Ok(Some(vec![format!(
                "root\tmalformed={}",
                self.canon0(&root)?
            )]));
        };
        let mut out = Vec::new();
        let mut rest = rd.clone();
        for k in ["DPartRootNode", "Type"] {
            rest.remove(k.as_bytes());
        }
        out.push(format!(
            "root\ttype={}\tentries={}",
            rd.get(b"Type".as_slice())
                .and_then(name)
                .unwrap_or_default(),
            self.cval_dict(&rest, 1, &mut Vec::new())?
        ));
        let mut paths: HashMap<IndirectRef, String> = HashMap::new();
        if let Some(node) = self.entry_pdf20(&rd, b"DPartRootNode")? {
            let mut ancestry = Vec::new();
            self.dpart_node(&node, "0", root_ref, 0, &mut ancestry, &mut paths, &mut out)?;
        } else {
            out.push("root\tnode=none".into());
        }
        self.page_dpart_lines(&paths, &mut out)?;
        Ok(Some(out))
    }

    #[allow(clippy::too_many_arguments)]
    fn dpart_node(
        &mut self,
        node: &Object,
        path: &str,
        parent: Option<IndirectRef>,
        depth: usize,
        ancestry: &mut Vec<(IndirectRef, String)>,
        paths: &mut HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        if depth > MAX_DEPTH {
            return err("DPart tree deeper than the limit");
        }
        let r = node.as_ref();
        if let Some(r) = r {
            if let Some((_, p)) = ancestry.iter().find(|(a, _)| *a == r) {
                out.push(format!("node\tpath={path}\tcycle_to={p}"));
                return Ok(());
            }
            if let Some(first) = paths.get(&r) {
                out.push(format!("node\tpath={path}\tsame_as={first}"));
                return Ok(());
            }
            paths.insert(r, path.to_string());
        }
        let d = match self.get(node)? {
            Object::Dict(d) => d,
            other => {
                out.push(format!(
                    "node\tpath={path}\tmalformed={}",
                    self.canon0(&other)?
                ));
                return Ok(());
            }
        };
        let page = |cx: &mut Self, key: &[u8]| -> Res<String> {
            Ok(match cx.entry_pdf20(&d, key)? {
                None => "-".into(),
                Some(o) => match o.as_ref().and_then(|r| cx.pages.get(&r)) {
                    Some(i) => format!("p{i}"),
                    None => match cx.get(&o)? {
                        Object::Name(n) if n == b"#unresolved" => "unresolved".into(),
                        _ => "not-a-page".into(),
                    },
                },
            })
        };
        let start = page(self, b"Start")?;
        let end = page(self, b"End")?;
        let parent_ok = match (d.get(b"Parent".as_slice()), parent) {
            (None, _) => "missing",
            (Some(p), Some(want)) if p.as_ref() == Some(want) => "ok",
            (Some(_), _) => "other",
        };
        let dpm = match self.entry_pdf20(&d, b"DPM")? {
            Some(m) => self.cval(&m, 1, &mut Vec::new())?,
            None => "-".into(),
        };
        let af = match self.entry_pdf20(&d, b"AF")? {
            Some(a) => match self.get(&a)? {
                Object::Array(a) => a.len().to_string(),
                _ => "malformed".into(),
            },
            None => "-".into(),
        };
        let mut rest = d.clone();
        for k in ["Type", "Parent", "DParts", "Start", "End", "DPM", "AF"] {
            rest.remove(k.as_bytes());
        }
        let rest = self.cval_dict(&rest, 1, &mut Vec::new())?;
        // /DParts: an array of arrays of DPart dictionaries (§14.12.3,
        // Table 410); a bare node in the outer array is kept as such.
        let kids: Vec<(String, Object)> = match self.entry_pdf20(&d, b"DParts")? {
            None => vec![],
            Some(k) => match self.get(&k)? {
                Object::Array(groups) => {
                    let mut v = Vec::new();
                    for (gi, g) in groups.iter().enumerate() {
                        match self.get(g)? {
                            Object::Array(nodes) => {
                                for (ni, n) in nodes.iter().enumerate() {
                                    v.push((format!("{path}.{gi}.{ni}"), n.clone()));
                                }
                            }
                            _ => v.push((format!("{path}.{gi}"), g.clone())),
                        }
                    }
                    v
                }
                other => {
                    out.push(format!(
                        "node\tpath={path}\tdparts_malformed={}",
                        self.canon0(&other)?
                    ));
                    vec![]
                }
            },
        };
        out.push(format!(
            "node\tpath={path}\ttype={}\tstart={start}\tend={end}\tparent={parent_ok}\tdpm={dpm}\taf={af}\tkids={}\trest={rest}",
            d.get(b"Type".as_slice()).and_then(name).unwrap_or_default(),
            kids.len()
        ));
        if let Some(r) = r {
            ancestry.push((r, path.to_string()));
        }
        let mut res = Ok(());
        for (p, k) in &kids {
            res = self.dpart_node(k, p, r, depth + 1, ancestry, paths, out);
            if res.is_err() {
                break;
            }
        }
        if r.is_some() {
            ancestry.pop();
        }
        res
    }

    /// Each page's `/DPart` (ISO 32000-2 Table 31) as the path of the
    /// node it names.
    fn page_dpart_lines(
        &mut self,
        paths: &HashMap<IndirectRef, String>,
        out: &mut Vec<String>,
    ) -> Res<()> {
        for i in 0..self.doc.pages().len() {
            let Some(pd) = self.page_dict(i)? else {
                continue;
            };
            let Some(dp) = self.entry_pdf20(&pd, b"DPart")? else {
                continue;
            };
            let target = match dp.as_ref().and_then(|r| paths.get(&r)) {
                Some(p) => p.clone(),
                None => match self.get(&dp)? {
                    Object::Name(n) if n == b"#unresolved" => "unresolved".into(),
                    Object::Dict(_) => "not-in-tree".into(),
                    _ => "malformed".into(),
                },
            };
            out.push(format!("page\tp={i}\tdpart={target}"));
        }
        Ok(())
    }

    /// The DPart nodes in tree order with their paths (for `/AF`).
    fn dpart_nodes(&mut self) -> Res<Vec<(String, Object)>> {
        let cat = self.doc.catalog().clone();
        let Some(root) = self.entry_pdf20(&cat, b"DPartRoot")? else {
            return Ok(vec![]);
        };
        let Some(rd) = self.dict(&root)? else {
            return Ok(vec![]);
        };
        let Some(node) = self.entry_pdf20(&rd, b"DPartRootNode")? else {
            return Ok(vec![]);
        };
        let mut v = Vec::new();
        let mut stack = vec![("0".to_string(), node, 0usize)];
        let mut seen = HashSet::new();
        while let Some((p, n, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return err("DPart tree deeper than the limit");
            }
            if let Some(r) = n.as_ref() {
                if !seen.insert(r) {
                    continue;
                }
            }
            let Some(d) = self.dict(&n)? else {
                continue;
            };
            v.push((p.clone(), n.clone()));
            if let Some(k) = self.entry_pdf20(&d, b"DParts")? {
                if let Object::Array(groups) = self.get(&k)? {
                    let mut kids = Vec::new();
                    for (gi, g) in groups.iter().enumerate() {
                        match self.get(g)? {
                            Object::Array(nodes) => {
                                for (ni, x) in nodes.iter().enumerate() {
                                    kids.push((format!("{p}.{gi}.{ni}"), x.clone(), depth + 1));
                                }
                            }
                            _ => kids.push((format!("{p}.{gi}"), g.clone(), depth + 1)),
                        }
                    }
                    stack.extend(kids.into_iter().rev());
                }
            }
        }
        Ok(v)
    }
}
