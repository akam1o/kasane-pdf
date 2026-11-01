//! Detection check of the document-level categories on real files: one
//! object of the file is replaced by an appended revision (an
//! incremental update written here, independent of the engine's
//! writers), and the snapshot comparison must fail in the expected
//! category — or pass, for the controls that store the same content
//! differently (another compression level). Aggregates only.

use super::*;

/// Minimal serialiser for replacement bodies.
fn ser(o: &Object, out: &mut Vec<u8>) {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(f) => out.extend_from_slice(format!("{f:.6}").as_bytes()),
        Object::Name(n) => {
            out.push(b'/');
            for &c in n {
                if c.is_ascii_alphanumeric() || b"-_.+:*".contains(&c) {
                    out.push(c);
                } else {
                    out.extend_from_slice(format!("#{c:02X}").as_bytes());
                }
            }
        }
        Object::String(s) => out.extend_from_slice(format!("<{}>", hex(s)).as_bytes()),
        Object::Array(a) => {
            out.push(b'[');
            for x in a {
                ser(x, out);
                out.push(b' ');
            }
            out.push(b']');
        }
        Object::Dict(d) => ser_dict(d, out),
        Object::Stream { .. } => unreachable!("streams are top-level bodies"),
        Object::Ref(r) => {
            out.extend_from_slice(format!("{} {} R", r.number, r.generation).as_bytes())
        }
    }
}

fn ser_dict(d: &Dict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        ser(&Object::Name(k.clone()), out);
        out.push(b' ');
        ser(v, out);
        out.push(b' ');
    }
    out.extend_from_slice(b">>");
}

/// `data` plus a revision replacing `obj` with `body` (classic xref
/// section, `/Prev` to the last one).
fn append(data: &[u8], doc: &Document, obj: IndirectRef, body: &Object) -> Option<Vec<u8>> {
    let tail = &data[data.len().saturating_sub(2048)..];
    let p = tail.windows(9).rposition(|w| w == b"startxref")?;
    let prev: u64 = std::str::from_utf8(&tail[p + 9..])
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let mut out = data.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let at = out.len();
    out.extend_from_slice(format!("{} {} obj\n", obj.number, obj.generation).as_bytes());
    match body {
        Object::Stream { dict, data } => {
            let mut d = dict.clone();
            d.insert(b"Length".to_vec(), Object::Integer(data.len() as i64));
            ser_dict(&d, &mut out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
        other => ser(other, &mut out),
    }
    out.extend_from_slice(b"\nendobj\n");
    let x = out.len();
    out.extend_from_slice(
        format!(
            "xref\n{} 1\n{at:010} {:05} n \ntrailer\n",
            obj.number, obj.generation
        )
        .as_bytes(),
    );
    let mut t = Dict::new();
    for k in ["Root", "Info", "ID", "Size"] {
        if let Some(v) = doc.trailer().get(k.as_bytes()) {
            t.insert(k.as_bytes().to_vec(), v.clone());
        }
    }
    t.insert(b"Prev".to_vec(), Object::Integer(prev as i64));
    ser_dict(&t, &mut out);
    out.extend_from_slice(format!("\nstartxref\n{x}\n%%EOF\n").as_bytes());
    Some(out)
}

fn flate(data: &[u8], level: u8) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, level)
}

/// The decoded bytes of a stream the probe can rewrite (FlateDecode
/// without parameters, or unfiltered), and whether it was compressed.
fn plain_bytes(dict: &Dict, data: &[u8]) -> Option<(Vec<u8>, bool)> {
    if dict.contains_key(b"DecodeParms".as_slice()) {
        return None;
    }
    let flate = match dict.get(b"Filter".as_slice()) {
        None => false,
        Some(Object::Name(n)) if n == b"FlateDecode" => true,
        Some(Object::Array(a))
            if a.len() == 1 && a[0].as_name() == Some(b"FlateDecode".as_slice()) =>
        {
            true
        }
        _ => return None,
    };
    if flate {
        Some((
            miniz_oxide::inflate::decompress_to_vec_zlib(data).ok()?,
            true,
        ))
    } else {
        Some((data.to_vec(), false))
    }
}

/// A stream body with `bytes`, compressed at `level` (or stored plain).
fn restream(dict: &Dict, bytes: &[u8], level: Option<u8>) -> Object {
    let mut d = dict.clone();
    d.remove(b"DecodeParms".as_slice());
    match level {
        Some(l) => {
            d.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
            Object::Stream {
                dict: d,
                data: flate(bytes, l),
            }
        }
        None => {
            d.remove(b"Filter".as_slice());
            Object::Stream {
                dict: d,
                data: bytes.to_vec(),
            }
        }
    }
}

/// A stream object `r` of `doc`: its body if the probe can rewrite it.
fn stream_at(doc: &Document, o: &Object) -> Option<(IndirectRef, Dict, Vec<u8>, bool)> {
    let r = o.as_ref()?;
    match doc.resolve(r).ok()? {
        Object::Stream { dict, data } => {
            let (b, z) = plain_bytes(&dict, &data)?;
            Some((r, dict, b, z))
        }
        _ => None,
    }
}

/// Run the probes on one file; `bump(name, detected)`.
pub(super) fn probe_file(data: &[u8], doc: &Document, bump: &mut dyn FnMut(&str, bool)) {
    if doc.encryption().is_some() {
        return;
    }
    // Precondition: the appended revision is what the engine now reads
    // for `obj` (it is not, in v0.4.0, after a hybrid-reference section
    // whose `/XRefStm` lists the object — counted apart, not as a miss).
    let class =
        |d2: &[u8], obj: IndirectRef, body: &Object, cat: &str| -> Option<Result<SemClass, ()>> {
            let d2 = Document::from_bytes(d2).ok()?;
            let visible = match (d2.resolve(obj).ok()?, body) {
                (Object::Stream { data: a, .. }, Object::Stream { data: b, .. }) => a == *b,
                (a, b) => a == *b,
            };
            if !visible {
                return Some(Err(()));
            }
            compare(doc, &d2)
                .checks
                .into_iter()
                .find(|c| c.category == cat)
                .map(|c| Ok(c.class))
        };
    let mut check =
        |name: &str, obj: IndirectRef, body: &Object, cat: &str, want: SemClass| match append(
            data, doc, obj, body,
        )
        .and_then(|d2| class(&d2, obj, body, cat))
        {
            Some(Ok(c)) => bump(name, c == want),
            Some(Err(())) => bump(
                "(precondition) appended revision not read by the engine",
                false,
            ),
            None => {}
        };
    let cat = doc.catalog().clone();
    let deref = |o: &Object| doc.deref(o).ok();
    // XFA: the second packet (or the stream) changed by one byte; the
    // same bytes recompressed (control).
    let xfa = cat
        .get(b"AcroForm".as_slice())
        .and_then(deref)
        .and_then(|a| a.as_dict().and_then(|d| d.get(b"XFA".as_slice()).cloned()));
    if let Some(x) = xfa {
        let target = match deref(&x) {
            Some(Object::Array(a)) if a.len() >= 4 => Some(a[3].clone()),
            Some(Object::Array(a)) if a.len() >= 2 => Some(a[1].clone()),
            _ => Some(x.clone()),
        };
        if let Some((r, d, mut b, z)) = target.as_ref().and_then(|t| stream_at(doc, t)) {
            let lvl = if z { Some(1) } else { Some(6) };
            check(
                "xfa control: packet recompressed",
                r,
                &restream(&d, &b, lvl),
                "xfa",
                SemClass::Pass,
            );
            b.push(b' ');
            check(
                "xfa: packet content changed",
                r,
                &restream(&d, &b, z.then_some(6)),
                "xfa",
                SemClass::Fail,
            );
        }
    }
    // JavaScript: the first script of the name tree changed.
    let js_tree = cat.get(b"Names".as_slice()).and_then(deref).and_then(|n| {
        n.as_dict()
            .and_then(|d| d.get(b"JavaScript".as_slice()).cloned())
    });
    if let Some(t) = js_tree {
        let mut node = deref(&t);
        let mut action = None;
        for _ in 0..16 {
            let Some(Object::Dict(d)) = node.clone() else {
                break;
            };
            if let Some(Object::Array(a)) = d.get(b"Names".as_slice()).and_then(deref) {
                action = a.get(1).cloned();
                break;
            }
            node = match d.get(b"Kids".as_slice()).and_then(deref) {
                Some(Object::Array(k)) if !k.is_empty() => deref(&k[0]),
                _ => None,
            };
        }
        if let Some(a) = action {
            let js =
                deref(&a).and_then(|x| x.as_dict().and_then(|d| d.get(b"JS".as_slice()).cloned()));
            match (a.as_ref(), js) {
                (_, Some(js @ Object::Ref(_))) => {
                    if let Some((r, d, mut b, z)) = stream_at(doc, &js) {
                        check(
                            "javascript control: script recompressed",
                            r,
                            &restream(&d, &b, Some(if z { 1 } else { 6 })),
                            "javascript",
                            SemClass::Pass,
                        );
                        b.extend_from_slice(b"\n;");
                        check(
                            "javascript: script changed",
                            r,
                            &restream(&d, &b, z.then_some(6)),
                            "javascript",
                            SemClass::Fail,
                        );
                    }
                }
                (Some(ar), Some(Object::String(s))) => {
                    if let Some(Object::Dict(mut d)) = deref(&a) {
                        let mut s2 = s.clone();
                        s2.extend_from_slice(b"\n;");
                        d.insert(b"JS".to_vec(), Object::String(s2));
                        check(
                            "javascript: script changed",
                            ar,
                            &Object::Dict(d),
                            "javascript",
                            SemClass::Fail,
                        );
                    }
                }
                _ => {}
            }
        }
    }
    // XMP: content changed; recompressed (control).
    if let Some((r, d, mut b, z)) = cat
        .get(b"Metadata".as_slice())
        .and_then(|m| stream_at(doc, m))
    {
        check(
            "metadata control: XMP recompressed",
            r,
            &restream(&d, &b, if z { None } else { Some(6) }),
            "metadata",
            SemClass::Pass,
        );
        b.extend_from_slice(b"<!-- probe -->");
        check(
            "metadata: XMP changed",
            r,
            &restream(&d, &b, z.then_some(6)),
            "metadata",
            SemClass::Fail,
        );
    }
    // Catalog: a viewer preference added; the open action dropped.
    if let Some(root) = doc
        .trailer()
        .get(b"Root".as_slice())
        .and_then(|o| o.as_ref())
    {
        let mut c = cat.clone();
        let mut vp = match c.get(b"ViewerPreferences".as_slice()).and_then(deref) {
            Some(Object::Dict(d)) => d,
            _ => Dict::new(),
        };
        let was = vp
            .get(b"DisplayDocTitle".as_slice())
            .and_then(deref)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        vp.insert(b"DisplayDocTitle".to_vec(), Object::Bool(!was));
        c.insert(b"ViewerPreferences".to_vec(), Object::Dict(vp));
        check(
            "viewer_prefs: a preference changed",
            root,
            &Object::Dict(c),
            "viewer_prefs",
            SemClass::Fail,
        );
        if cat.contains_key(b"OpenAction".as_slice()) {
            let mut c = cat.clone();
            c.remove(b"OpenAction".as_slice());
            check(
                "doc_actions: open action removed",
                root,
                &Object::Dict(c),
                "doc_actions",
                SemClass::Fail,
            );
        }
        // Control: the catalog re-stored with the same entries — no
        // category may fail.
        if let Some(d2) = append(data, doc, root, &Object::Dict(cat.clone())) {
            if let Ok(d2) = Document::from_bytes(&d2) {
                let r = compare(doc, &d2);
                bump(
                    "control: catalog re-stored unchanged, no category fails",
                    r.checks.iter().all(|c| c.class != SemClass::Fail),
                );
            }
        }
    }
}
