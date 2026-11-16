//! `kasane-pdf text --annotations` (T1, v0.11): the page's characters
//! exactly as without the flag, then each annotation's shown text and
//! note (`docs/annotation-text.md`). Without the flag the output is
//! unchanged.

mod common;

use common::{bin, build_pdf, stream_obj, tmpdir};
use std::fs;

/// A JSON value (RFC 8259), parsed strictly enough to reject what is
/// not JSON: the output must be read by any JSON parser.
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn parse(s: &str) -> Result<Json, String> {
        let b = s.as_bytes();
        let mut i = 0;
        let v = Self::value(b, &mut i)?;
        Self::ws(b, &mut i);
        if i != b.len() {
            return Err(format!("trailing data at {i}"));
        }
        Ok(v)
    }
    fn ws(b: &[u8], i: &mut usize) {
        while *i < b.len() && matches!(b[*i], b' ' | b'\n' | b'\r' | b'\t') {
            *i += 1;
        }
    }
    fn value(b: &[u8], i: &mut usize) -> Result<Json, String> {
        Self::ws(b, i);
        let lit = |i: &mut usize, w: &str, v: Json| {
            if b[*i..].starts_with(w.as_bytes()) {
                *i += w.len();
                Ok(v)
            } else {
                Err(format!("bad literal at {i}"))
            }
        };
        match b.get(*i) {
            Some(b'n') => lit(i, "null", Json::Null),
            Some(b't') => lit(i, "true", Json::Bool(true)),
            Some(b'f') => lit(i, "false", Json::Bool(false)),
            Some(b'"') => Self::string(b, i).map(Json::Str),
            Some(b'[') => {
                *i += 1;
                let mut out = Vec::new();
                Self::ws(b, i);
                if b.get(*i) == Some(&b']') {
                    *i += 1;
                    return Ok(Json::Arr(out));
                }
                loop {
                    out.push(Self::value(b, i)?);
                    Self::ws(b, i);
                    match b.get(*i) {
                        Some(b',') => *i += 1,
                        Some(b']') => {
                            *i += 1;
                            return Ok(Json::Arr(out));
                        }
                        _ => return Err(format!("bad array at {i}")),
                    }
                }
            }
            Some(b'{') => {
                *i += 1;
                let mut out = Vec::new();
                Self::ws(b, i);
                if b.get(*i) == Some(&b'}') {
                    *i += 1;
                    return Ok(Json::Obj(out));
                }
                loop {
                    Self::ws(b, i);
                    let k = Self::string(b, i)?;
                    Self::ws(b, i);
                    if b.get(*i) != Some(&b':') {
                        return Err(format!("bad object at {i}"));
                    }
                    *i += 1;
                    out.push((k, Self::value(b, i)?));
                    Self::ws(b, i);
                    match b.get(*i) {
                        Some(b',') => *i += 1,
                        Some(b'}') => {
                            *i += 1;
                            return Ok(Json::Obj(out));
                        }
                        _ => return Err(format!("bad object at {i}")),
                    }
                }
            }
            _ => {
                let start = *i;
                while *i < b.len()
                    && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    *i += 1;
                }
                std::str::from_utf8(&b[start..*i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(Json::Num)
                    .ok_or_else(|| format!("bad value at {start}"))
            }
        }
    }
    fn string(b: &[u8], i: &mut usize) -> Result<String, String> {
        if b.get(*i) != Some(&b'"') {
            return Err(format!("expected a string at {i}"));
        }
        *i += 1;
        let mut units: Vec<u16> = Vec::new();
        let mut out = String::new();
        let flush = |units: &mut Vec<u16>, out: &mut String| -> Result<(), String> {
            if !units.is_empty() {
                out.push_str(
                    &String::from_utf16(units).map_err(|_| "unpaired surrogate".to_string())?,
                );
                units.clear();
            }
            Ok(())
        };
        loop {
            match b.get(*i) {
                None => return Err("unterminated string".into()),
                Some(b'"') => {
                    *i += 1;
                    flush(&mut units, &mut out)?;
                    return Ok(out);
                }
                Some(b'\\') => {
                    let e = *b.get(*i + 1).ok_or("bad escape")?;
                    *i += 2;
                    if e == b'u' {
                        let h = std::str::from_utf8(b.get(*i..*i + 4).ok_or("bad escape")?)
                            .map_err(|_| "bad escape")?;
                        units.push(u16::from_str_radix(h, 16).map_err(|_| "bad escape")?);
                        *i += 4;
                        continue;
                    }
                    flush(&mut units, &mut out)?;
                    out.push(match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        _ => return Err(format!("bad escape at {i}")),
                    });
                }
                Some(&c) if c < 0x20 => {
                    return Err(format!("control character in a string at {i}"))
                }
                Some(_) => {
                    flush(&mut units, &mut out)?;
                    let rest = std::str::from_utf8(&b[*i..]).map_err(|_| "invalid UTF-8")?;
                    let ch = rest.chars().next().unwrap();
                    out.push(ch);
                    *i += ch.len_utf8();
                }
            }
        }
    }
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(kv) => kv
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or(&Json::Null),
            _ => &Json::Null,
        }
    }
    fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

fn sample() -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 4 0 R \
              /Resources << /Font << /F 5 0 R >> >> /Annots [10 0 R 12 0 R 13 0 R 14 0 R] >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /F 12 Tf 10 280 Td (Body) Tj ET")),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (
            10,
            b"<< /Type /Annot /Subtype /FreeText /Rect [10 100 110 120] /DA (/F 10 Tf 0 g) \
              /Contents (CONTENTSX) /T (AUTHORX) /NM (NMX) /AP << /N 11 0 R >> >>"
                .to_vec(),
        ),
        (
            11,
            stream_obj(
                "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /H 5 0 R >> >>",
                b"BT /H 10 Tf 2 5 Td (DRAWNX) Tj ET",
            ),
        ),
        (
            12,
            b"<< /Type /Annot /Subtype /Text /Rect [10 150 30 170] /Contents <FEFF65E5672C8A9E> \
              /T (AUTHORX) /Subj (SUBJX) >>"
                .to_vec(),
        ),
        (
            13,
            b"<< /Type /Annot /Subtype /Link /Rect [10 200 110 220] /Contents (ALTX) \
              /A << /S /URI /URI (http://example.invalid/) >> >>"
                .to_vec(),
        ),
        (
            14,
            b"<< /Type /Annot /Subtype /Text /F 2 /Rect [10 250 30 270] /Contents (HIDDENX) >>".to_vec(),
        ),
    ])
}

fn text(args: &[&str]) -> (bool, String, String) {
    let dir = tmpdir("text-annots");
    let f = dir.join("in.pdf");
    fs::write(&f, sample()).unwrap();
    let mut a = vec![
        "text".to_string(),
        f.to_string_lossy().into_owned(),
        "1".to_string(),
    ];
    a.extend(args.iter().map(|s| s.to_string()));
    let out = bin().args(&a).output().unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn the_default_output_is_unchanged() {
    for fmt in ["json", "lines"] {
        let (ok, out, err) = text(&["--format", fmt]);
        assert!(ok, "{err}");
        for w in [
            "DRAWNX",
            "CONTENTSX",
            "日本語",
            "ALTX",
            "HIDDENX",
            "AUTHORX",
            "annotation",
        ] {
            assert!(!out.contains(w), "{fmt}: {w} in the default output");
        }
    }
    // JSON stays an array of the page's characters.
    let (_, out, _) = text(&[]);
    assert!(out.trim_start().starts_with('['), "{out}");
}

#[test]
fn json_with_annotations() {
    let (ok, out, err) = text(&["--annotations"]);
    assert!(ok, "{err}");
    let v = Json::parse(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    // The page's characters, as without the flag. That output keeps
    // its historical form (the `via` value unquoted, so it is not JSON);
    // with the flag the same entries are written as JSON.
    let (_, plain, _) = text(&[]);
    assert!(Json::parse(&plain).is_err(), "the default output changed");
    let quoted: String = plain
        .lines()
        .map(|l| match (l.find("\"via\": "), l.find(", \"origin\"")) {
            (Some(a), Some(b)) => format!("{}\"via\": \"{}\"{}", &l[..a], &l[a + 7..b], &l[b..]),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(v.get("chars"), &Json::parse(&quoted).unwrap());
    let Json::Arr(a) = v.get("annotations") else {
        panic!("no annotations array: {out}")
    };
    assert_eq!(a.len(), 4);
    let row = |i: usize, key: &str| a[i].get(key).clone();
    let st = |x: &str| Json::Str(x.to_string());
    assert_eq!(
        (row(0, "subtype"), row(0, "shown"), row(0, "note")),
        (st("FreeText"), st("DRAWNX"), Json::Null)
    );
    assert_eq!(row(0, "contents"), st("CONTENTSX"));
    assert_eq!(
        (row(1, "shown"), row(1, "note")),
        (Json::Null, st("日本語"))
    );
    assert_eq!(
        (row(2, "subtype"), row(2, "shown"), row(2, "note")),
        (st("Link"), Json::Null, Json::Null)
    );
    assert_eq!(
        (row(3, "hidden"), row(3, "shown"), row(3, "note")),
        (Json::Bool(true), Json::Null, Json::Null)
    );
    for (i, r) in a.iter().enumerate() {
        assert_eq!(r.get("recovered"), &Json::Bool(false));
        assert_eq!(r.get("index"), &Json::Num(i as f64));
        assert!(r.get("rect") != &Json::Null);
    }
    assert_eq!(row(0, "subtype").str(), Some("FreeText"));
    // Identifiers and metadata are never reported.
    for w in ["AUTHORX", "NMX", "SUBJX"] {
        assert!(!out.contains(w), "{w} in the output");
    }
}

#[test]
fn lines_with_annotations() {
    let (ok, out, err) = text(&["--format", "lines", "--annotations"]);
    assert!(ok, "{err}");
    let (_, plain, _) = text(&["--format", "lines"]);
    assert!(
        out.starts_with(&plain),
        "the page's lines come first, unchanged"
    );
    let rest: Vec<&str> = out[plain.len()..].lines().collect();
    assert_eq!(
        rest,
        vec![
            "annotation 0 FreeText shown \"DRAWNX\"",
            "annotation 1 Text note \"日本語\"",
        ]
    );
}

#[test]
fn the_flag_in_any_position_and_bad_options() {
    let (ok, a, _) = text(&["--annotations", "--format", "json"]);
    assert!(ok);
    let (_, b, _) = text(&["--format", "json", "--annotations"]);
    assert_eq!(a, b);
    let (ok, _, err) = text(&["--annotation"]);
    assert!(!ok && err.contains("unknown option"), "{err}");
}
