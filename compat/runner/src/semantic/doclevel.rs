//! Document-level categories of the semantic snapshot: XFA, actions,
//! JavaScript, metadata and viewer preferences — what a rewrite that
//! carries forms, scripts or metadata must keep.
//!
//! Content (XFA packets, XMP, scripts) is compared by the length and
//! digest of its *decoded* bytes, so a change of compression is not a
//! difference while any change of content is; the content itself never
//! reaches a line's notes. Stricter than the earlier categories: a value
//! cut at the value-depth limit, an action chain past its depth limit, a
//! line past its size limit or the decoded-byte budget make the category
//! `error`, never a silently shortened line.

use super::*;

/// Decoded content of a stream compared by content.
pub(super) enum Decoded {
    Bytes(Vec<u8>),
    /// Filtered data that does not decode: its raw digest, so a verbatim
    /// copy still compares equal and any other change does not.
    Undecodable(String),
}

impl<'a> Ctx<'a> {
    /// Run a document-level category with the strict rules above.
    pub(super) fn strict(
        &mut self,
        f: fn(&mut Self) -> Res<Option<Vec<String>>>,
    ) -> Res<Option<Vec<String>>> {
        self.truncated = false;
        self.doclevel = true;
        self.action_ids.clear();
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

    /// `d[key]`, unless absent or null — a null value and a reference to
    /// an undefined object both mean the entry is absent (§7.3.7,
    /// §7.3.10). The value is returned unresolved (a reference keeps its
    /// identity for cycle detection); an unreadable object is not null.
    fn entry(&mut self, d: &Dict, key: &[u8]) -> Res<Option<Object>> {
        match d.get(key) {
            None => Ok(None),
            Some(v) => Ok((!self.is_null(v)?).then(|| v.clone())),
        }
    }

    fn is_null(&mut self, v: &Object) -> Res<bool> {
        Ok(match v {
            Object::Null => true,
            Object::Ref(_) => matches!(self.get(v)?, Object::Null),
            _ => false,
        })
    }

    /// The entries of `d` that are not null, in key order.
    fn entries(&mut self, d: &Dict) -> Res<Vec<(Vec<u8>, Object)>> {
        let mut v = Vec::new();
        for (k, x) in d {
            if !self.is_null(x)? {
                v.push((k.clone(), x.clone()));
            }
        }
        Ok(v)
    }

    /// The filter chain is decoded by the snapshot's own decoders:
    /// FlateDecode without a predictor, ASCII85Decode and ASCIIHexDecode
    /// (abbreviations included, as the engine accepts them) — every XFA,
    /// XMP and script stream in the corpus; another filter or a predictor
    /// is `unsupported` rather than compared encoded.
    pub(super) fn decode(&mut self, what: &str, dict: &Dict, data: &[u8]) -> Res<Decoded> {
        let unsupported = |m: String| Err(Snag::Unsupported(format!("{what}: {m}")));
        let filters: Vec<Vec<u8>> = match dict
            .get(b"Filter".as_slice())
            .map(|f| self.get(f))
            .transpose()?
        {
            None | Some(Object::Null) => vec![],
            Some(Object::Name(n)) => vec![n],
            Some(Object::Array(a)) => {
                let mut v = Vec::new();
                for x in &a {
                    match self.get(x)? {
                        Object::Name(n) => v.push(n),
                        _ => return unsupported("/Filter entry is not a name".into()),
                    }
                }
                v
            }
            Some(_) => return unsupported("/Filter is not a name".into()),
        };
        let parms: Vec<Object> = match dict
            .get(b"DecodeParms".as_slice())
            .map(|p| self.get(p))
            .transpose()?
        {
            Some(Object::Array(a)) => {
                let mut v = Vec::new();
                for x in &a {
                    v.push(self.get(x)?);
                }
                v
            }
            Some(p) => vec![p],
            None => vec![],
        };
        let mut buf = data.to_vec();
        for (i, f) in filters.iter().enumerate() {
            let predictor = match parms.get(i) {
                None | Some(Object::Null) => 1,
                Some(Object::Dict(d)) => match d
                    .get(b"Predictor".as_slice())
                    .map(|p| self.get(p))
                    .transpose()?
                {
                    None => 1,
                    Some(Object::Integer(p)) => p,
                    Some(_) => -1,
                },
                Some(_) => -1,
            };
            let out = match f.as_slice() {
                b"FlateDecode" | b"Fl" if predictor == 1 => {
                    use miniz_oxide::inflate::{decompress_to_vec_zlib_with_limit, TINFLStatus};
                    match decompress_to_vec_zlib_with_limit(&buf, MAX_DECODED_BYTES) {
                        Ok(b) => Some(b),
                        Err(e) if e.status == TINFLStatus::HasMoreOutput => {
                            return err(format!("{what}: decoded stream larger than {MAX_DECODED_BYTES} bytes"))
                        }
                        Err(_) => None,
                    }
                }
                b"ASCII85Decode" | b"A85" => ascii85(&buf),
                b"ASCIIHexDecode" | b"AHx" => asciihex(&buf),
                _ => {
                    return unsupported(format!(
                        "filter {}{} (FlateDecode without a predictor, ASCII85Decode and ASCIIHexDecode are decoded)",
                        String::from_utf8_lossy(f),
                        if predictor != 1 { " with a predictor" } else { "" }
                    ))
                }
            };
            match out {
                Some(b) => buf = b,
                None => {
                    return Ok(Decoded::Undecodable(format!(
                        "{}:{}",
                        data.len(),
                        sha(data)
                    )))
                }
            }
        }
        self.decoded = self.decoded.saturating_add(buf.len());
        if self.decoded > MAX_DECODED_TOTAL {
            return err(format!(
                "snapshot stopped after {MAX_DECODED_TOTAL} decoded bytes"
            ));
        }
        Ok(Decoded::Bytes(buf))
    }

    /// `length:digest` of a stream's decoded bytes.
    pub(super) fn digest(&mut self, what: &str, dict: &Dict, data: &[u8]) -> Res<String> {
        Ok(match self.decode(what, dict, data)? {
            Decoded::Bytes(b) => format!("{}:{}", b.len(), sha(&b)),
            Decoded::Undecodable(raw) => format!("undecodable:{raw}"),
        })
    }

    /// A script (`/JS`): a text string or a text stream (§12.6.4.17)
    /// — the same script either way, so both are decoded to Unicode
    /// (exactly, no normalisation) before the digest.
    fn script(&mut self, o: &Object) -> Res<String> {
        let digest = |b: &[u8]| {
            let s = decode_text(b);
            format!("{}:{}", s.chars().count(), sha(s.as_bytes()))
        };
        Ok(match self.get(o)? {
            Object::String(s) => digest(&s),
            Object::Stream { dict, data } => match self.decode("script", &dict, &data)? {
                Decoded::Bytes(b) => digest(&b),
                Decoded::Undecodable(raw) => format!("undecodable:{raw}"),
            },
            Object::Null => "null".into(),
            other => format!("malformed:{}", self.canon0(&other)?),
        })
    }

    /// An action and its `/Next` chain: the action type, every other key
    /// (a script by its digest, a GoTo destination resolved to page and
    /// view), then the `/Next` actions in order — a single action and a
    /// one-element array are the same (§12.6.2). A reference back onto
    /// the chain is named by the position it returns to; nesting past
    /// `MAX_ACTION_DEPTH` is `error`.
    pub(super) fn chain(
        &mut self,
        o: &Object,
        depth: usize,
        path: &mut Vec<IndirectRef>,
    ) -> Res<String> {
        if depth > MAX_ACTION_DEPTH {
            return err("action chain deeper than the limit");
        }
        let r = o.as_ref();
        let mut id = String::new();
        if let Some(r) = r {
            if let Some(k) = path.iter().position(|p| *p == r) {
                return Ok(format!("<cycle@{k}>"));
            }
            // Sharing: an action object met before in this category is
            // named by its first-visit position, not expanded again.
            if let Some(n) = self.action_ids.get(&r) {
                return Ok(format!("{{=#{n}}}"));
            }
            let n = self.action_ids.len();
            self.action_ids.insert(r, n);
            id = format!("#{n} ");
            path.push(r);
        }
        let s = self
            .chain_body(o, depth, path)
            .map(|s| match s.strip_prefix('{') {
                Some(rest) if !id.is_empty() => format!("{{{id}{rest}"),
                _ => s,
            });
        if r.is_some() {
            path.pop();
        }
        let s = s?;
        if s.len() > MAX_LINE_BYTES {
            return err("action chain longer than the limit");
        }
        Ok(s)
    }

    fn chain_body(&mut self, o: &Object, depth: usize, path: &mut Vec<IndirectRef>) -> Res<String> {
        let d = match self.get(o)? {
            Object::Dict(d) => d,
            Object::Null => return Ok("null".into()),
            other => return Ok(format!("malformed:{}", self.canon0(&other)?)),
        };
        let s = d.get(b"S".as_slice()).and_then(name).unwrap_or_default();
        let mut parts = vec![format!("S=/{s}")];
        for (k, v) in &self.entries(&d)? {
            match k.as_slice() {
                b"S" | b"Next" => {}
                b"JS" => parts.push(format!("JS={}", self.script(v)?)),
                b"D" if s == "GoTo" => parts.push(format!("D={}", self.dest(v, 0)?)),
                _ => parts.push(format!(
                    "/{}={}",
                    String::from_utf8_lossy(k),
                    self.canon(v, 1, &mut Vec::new())?
                )),
            }
        }
        if let Some(n) = self.entry(&d, b"Next")? {
            let items = match self.get(&n)? {
                Object::Array(a) => a,
                _ => vec![n],
            };
            let mut next = Vec::new();
            for it in &items {
                next.push(self.chain(it, depth + 1, path)?);
            }
            parts.push(format!("next=[{}]", next.join(" | ")));
        }
        Ok(format!("{{{}}}", parts.join(" ")))
    }

    /// Lines for an additional-actions dictionary, one per trigger.
    fn aa_lines(&mut self, head: &str, aa: &Object, out: &mut Vec<String>) -> Res<()> {
        match self.get(aa)? {
            Object::Dict(d) => {
                for (k, v) in &self.entries(&d)? {
                    let a = self.chain(v, 0, &mut Vec::new())?;
                    out.push(format!(
                        "{head}\ttrigger=/{}\taction={a}",
                        String::from_utf8_lossy(k)
                    ));
                }
            }
            other => out.push(format!("{head}\tmalformed={}", self.canon0(&other)?)),
        }
        Ok(())
    }

    /// `chain` with the document-level rules, from a category that does
    /// not use them otherwise (outlines): a value cut at the depth limit
    /// is an error, not a shortened line.
    pub(super) fn strict_chain(&mut self, o: &Object) -> Res<String> {
        let (doclevel, truncated) = (self.doclevel, self.truncated);
        self.doclevel = true;
        self.truncated = false;
        let r = self.chain(o, 0, &mut Vec::new());
        let cut = self.truncated;
        self.doclevel = doclevel;
        self.truncated = truncated;
        let s = r?;
        if cut {
            return err("an action value is nested deeper than the limit");
        }
        Ok(s)
    }

    // ---------------- XFA ----------------

    /// `/AcroForm /XFA` — one stream, or an array of packet name and
    /// stream pairs (§12.7.8) — and the catalog's `/NeedsRendering`.
    /// The two forms are different storage with no defined equivalence
    /// here, so they never compare equal; a packet is identified by its
    /// position and name (duplicates and order kept) and compared by its
    /// decoded content.
    pub(super) fn xfa(&mut self) -> Res<Option<Vec<String>>> {
        let cat = self.doc.catalog().clone();
        let af = match cat.get(b"AcroForm".as_slice()) {
            Some(a) => self.dict(a)?,
            None => None,
        };
        let x = match &af {
            Some(d) => self.entry(d, b"XFA")?,
            None => None,
        };
        let nr = self.entry(&cat, b"NeedsRendering")?;
        if x.is_none() && nr.is_none() {
            return Ok(None);
        }
        let mut out = Vec::new();
        if let Some(x) = x {
            match self.get(&x)? {
                Object::Stream { dict, data } => {
                    let c = self.digest("XFA stream", &dict, &data)?;
                    out.push(format!("xfa\tform=stream\tcontent={c}"));
                }
                Object::Array(a) => {
                    if a.len() % 2 != 0 {
                        return err("XFA array has an odd number of elements");
                    }
                    out.push(format!("xfa\tform=array\tpackets={}", a.len() / 2));
                    for (i, pair) in a.chunks(2).enumerate() {
                        let n = match self.get(&pair[0])? {
                            Object::String(s) => text_exact(&s),
                            _ => return err("XFA packet name is not a string"),
                        };
                        let c = match self.get(&pair[1])? {
                            Object::Stream { dict, data } => {
                                self.digest("XFA packet", &dict, &data)?
                            }
                            _ => return err("XFA packet is not a stream"),
                        };
                        out.push(format!("packet\ti={i}\tname={n}\tcontent={c}"));
                    }
                }
                _ => return err("XFA is neither a stream nor an array"),
            }
        }
        if let Some(nr) = nr {
            out.push(format!("needs_rendering\tvalue={}", self.canon0(&nr)?));
        }
        Ok(Some(out))
    }

    // ---------------- actions ----------------

    /// Document actions: catalog `/OpenAction` (a destination or an
    /// action) and `/AA`; page `/AA`; every annotation's `/A`, `/AA` and
    /// `/Dest` by page and position; `/AA` of field-tree nodes that are
    /// not annotations on a page, by fully qualified name.
    pub(super) fn doc_actions(&mut self) -> Res<Option<Vec<String>>> {
        let cat = self.doc.catalog().clone();
        let mut out = Vec::new();
        if let Some(oa) = self.entry(&cat, b"OpenAction")? {
            let line = match self.get(&oa)? {
                Object::Dict(_) => format!(
                    "open_action\taction={}",
                    self.chain(&oa, 0, &mut Vec::new())?
                ),
                Object::Array(_) | Object::Name(_) | Object::String(_) => {
                    format!("open_action\tdest={}", self.dest(&oa, 0)?)
                }
                other => format!("open_action\tmalformed={}", self.canon0(&other)?),
            };
            out.push(line);
        }
        if let Some(aa) = self.entry(&cat, b"AA")? {
            self.aa_lines("catalog_aa", &aa, &mut out)?;
        }
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
            if let Some(aa) = self.entry(&pd, b"AA")? {
                self.aa_lines(&format!("page_aa\tpage=p{i}"), &aa, &mut out)?;
            }
            let annots = match pd
                .get(b"Annots".as_slice())
                .map(|a| self.get(a))
                .transpose()?
            {
                Some(Object::Array(a)) => a,
                _ => continue,
            };
            for (k, a) in annots.iter().enumerate() {
                let Some(ad) = self.dict(a)? else {
                    continue;
                };
                let at = format!("{i}.{k}");
                if let Some(x) = self.entry(&ad, b"A")? {
                    let c = self.chain(&x, 0, &mut Vec::new())?;
                    out.push(format!("annot_action\tat={at}\tkey=/A\taction={c}"));
                }
                if let Some(x) = self.entry(&ad, b"Dest")? {
                    let d = self.dest(&x, 0)?;
                    out.push(format!("annot_action\tat={at}\tkey=/Dest\tdest={d}"));
                }
                if let Some(x) = self.entry(&ad, b"AA")? {
                    self.aa_lines(&format!("annot_aa\tat={at}"), &x, &mut out)?;
                }
            }
        }
        let fields = match cat
            .get(b"AcroForm".as_slice())
            .map(|a| self.dict(a))
            .transpose()?
        {
            Some(Some(af)) => af.get(b"Fields".as_slice()).cloned(),
            _ => None,
        };
        if let Some(f) = fields {
            if let Object::Array(a) = self.get(&f)? {
                let mut seen = HashSet::new();
                for x in &a {
                    self.field_actions(x, "", 0, &mut seen, &mut out)?;
                }
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }

    fn field_actions(
        &mut self,
        o: &Object,
        parent: &str,
        depth: usize,
        seen: &mut HashSet<IndirectRef>,
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
        let fq = match d.get(b"T".as_slice()).map(|x| self.get(x)).transpose()? {
            Some(Object::String(t)) if parent.is_empty() => text(&t),
            Some(Object::String(t)) => format!("{parent}.{}", text(&t)),
            _ => parent.to_string(),
        };
        // Annotations on a page were recorded by position above.
        let on_page = o.as_ref().is_some_and(|r| self.annots.contains_key(&r));
        if !on_page {
            if let Some(aa) = self.entry(&d, b"AA")? {
                self.aa_lines(&format!("field_aa\tfield={fq}"), &aa, out)?;
            }
        }
        if let Some(k) = d.get(b"Kids".as_slice()) {
            if let Object::Array(kids) = self.get(k)? {
                for k in &kids {
                    self.field_actions(k, &fq, depth + 1, seen, out)?;
                }
            }
        }
        Ok(())
    }

    // ---------------- JavaScript ----------------

    /// The `/Names /JavaScript` name tree flattened: each entry by its
    /// key (the exact bytes — name-tree keys are byte strings, §7.9.6)
    /// and its action chain. Tree layout and storage of the script
    /// (string or stream, compressed or not) are not differences.
    pub(super) fn javascript(&mut self) -> Res<Option<Vec<String>>> {
        let names = match self.doc.catalog().get(b"Names".as_slice()).cloned() {
            Some(n) => self.dict(&n)?,
            None => None,
        };
        let t = match &names {
            Some(n) => self.entry(n, b"JavaScript")?,
            None => None,
        };
        let Some(t) = t else {
            return Ok(None);
        };
        let entries = self.tree(&t, false)?;
        let mut out = vec![format!("tree\tentries={}", entries.len())];
        for (k, v) in entries {
            let key = match &k {
                Object::String(s) => hex(s),
                other => format!("malformed:{}", self.canon0(other)?),
            };
            let a = self.chain(&v, 0, &mut Vec::new())?;
            out.push(format!("script\tname={key}\taction={a}"));
        }
        Ok(Some(out))
    }

    // ---------------- metadata ----------------

    /// The trailer's `/Info` (every entry; text strings decoded, so a
    /// re-encoding is not a difference) and the catalog's `/Metadata`
    /// stream by decoded content. Absent, empty, not a stream and
    /// undecodable are distinct states.
    pub(super) fn metadata(&mut self) -> Res<Option<Vec<String>>> {
        let mut out = Vec::new();
        let trailer = self.doc.trailer().clone();
        if let Some(i) = self.entry(&trailer, b"Info")? {
            match self.get(&i)? {
                Object::Dict(d) => {
                    let d = self.entries(&d)?;
                    out.push(format!("info\tentries={}", d.len()));
                    for (k, v) in &d {
                        let v = self.canon(v, 1, &mut Vec::new())?;
                        out.push(format!(
                            "info_entry\tkey=/{}\tvalue={v}",
                            String::from_utf8_lossy(k)
                        ));
                    }
                }
                other => out.push(format!("info\tmalformed={}", self.canon0(&other)?)),
            }
        }
        let cat = self.doc.catalog().clone();
        if let Some(m) = self.entry(&cat, b"Metadata")? {
            match self.get(&m)? {
                Object::Stream { dict, data } => {
                    let c = self.digest("XMP", &dict, &data)?;
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
                    let rest = self.canon_dict(&rest, 1, &mut Vec::new())?;
                    out.push(format!("xmp\tstate={state}\tdict={rest}\tcontent={c}"));
                }
                other => out.push(format!(
                    "xmp\tstate=not-a-stream\tvalue={}",
                    self.canon0(&other)?
                )),
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }

    // ---------------- viewer preferences ----------------

    /// `/ViewerPreferences` (every key, extensions included),
    /// `/PageMode` and `/PageLayout`. An absent entry is not filled in
    /// with its default: adding or dropping one is a difference.
    pub(super) fn viewer_prefs(&mut self) -> Res<Option<Vec<String>>> {
        let cat = self.doc.catalog().clone();
        let mut out = Vec::new();
        if let Some(vp) = self.entry(&cat, b"ViewerPreferences")? {
            match self.get(&vp)? {
                Object::Dict(d) => {
                    let d = self.entries(&d)?;
                    out.push(format!("prefs\tentries={}", d.len()));
                    for (k, v) in &d {
                        let v = self.canon(v, 1, &mut Vec::new())?;
                        out.push(format!(
                            "pref\tkey=/{}\tvalue={v}",
                            String::from_utf8_lossy(k)
                        ));
                    }
                }
                other => out.push(format!("prefs\tmalformed={}", self.canon0(&other)?)),
            }
        }
        for (key, head) in [
            (&b"PageMode"[..], "page_mode"),
            (b"PageLayout", "page_layout"),
        ] {
            if let Some(v) = self.entry(&cat, key)? {
                out.push(format!("{head}\tvalue={}", self.canon0(&v)?));
            }
        }
        Ok((!out.is_empty()).then_some(out))
    }
}

/// ASCII85 (§7.4.3): white space ignored, `z` for four zeros, `~>` ends;
/// `None` for anything else.
pub(super) fn ascii85(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len() / 5 * 4 + 4);
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        i += 1;
        match c {
            b'~' if b.get(i) == Some(&b'>') => break,
            b'~' => return None,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
                    out.extend_from_slice(&u32::try_from(v).ok()?.to_be_bytes());
                    n = 0;
                }
            }
            c if c.is_ascii_whitespace() || c == 0 => {}
            _ => return None,
        }
    }
    if n == 1 {
        return None;
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |a, &d| a * 85 + d as u64);
        out.extend_from_slice(&u32::try_from(v).ok()?.to_be_bytes()[..n - 1]);
    }
    Some(out)
}

/// ASCIIHex (§7.4.2): white space ignored, `>` ends, an odd final digit
/// is followed by 0; `None` for anything else.
pub(super) fn asciihex(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len() / 2 + 1);
    let mut hi: Option<u8> = None;
    for &c in b {
        let d = match c {
            b'>' => break,
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            c if c.is_ascii_whitespace() || c == 0 => continue,
            _ => return None,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | d),
            None => hi = Some(d),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    Some(out)
}
