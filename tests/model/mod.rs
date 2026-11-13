//! The document generator of `tests/redact_model.rs` (also built into
//! the `redact_model` fuzz target): see that file.

#![allow(dead_code)]

use std::collections::BTreeMap;

pub const RECT: (f64, f64, f64, f64) = (60.0, 60.0, 140.0, 140.0);

pub fn stream_obj(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

/// The generator's choices: bytes of a fuzzer's input first (so its
/// mutations steer the structure), then splitmix64 from a seed.
pub struct Rng<'a> {
    bytes: &'a [u8],
    state: u64,
}

impl<'a> Rng<'a> {
    pub fn seeded(seed: u64) -> Rng<'static> {
        Rng {
            bytes: &[],
            state: seed,
        }
    }
    pub fn from_bytes(bytes: &'a [u8]) -> Rng<'a> {
        Rng {
            bytes,
            state: bytes.len() as u64,
        }
    }
    fn next(&mut self) -> u64 {
        if let Some((b, rest)) = self.bytes.split_first() {
            self.bytes = rest;
            return u64::from(*b);
        }
        // splitmix64
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// A secret of `len` capital letters for variant `v` and element `k`:
/// different in every position between the variants.
fn secret(v: u32, k: u64, len: usize) -> String {
    (0..len)
        .map(|i| {
            let base = (k as usize * 7 + i * 3) % 13;
            (b'A' + (base as u8) + if v == 1 { 0 } else { 13 }) as char
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Plain,
    ObjectStreams,
    Incremental,
}

struct Doc {
    objs: BTreeMap<u32, Vec<u8>>,
    next: u32,
    /// Objects holding secret data (aliasing targets).
    secret_objs: Vec<u32>,
    /// Every object (aliasing targets for noise).
    any_objs: Vec<u32>,
    /// Public texts each page draws outside the rect.
    public: Vec<Vec<String>>,
    /// Objects added by the last revision (incremental layout).
    late: Vec<u32>,
}

impl Doc {
    fn add(&mut self, body: Vec<u8>) -> u32 {
        let n = self.next;
        self.next += 1;
        self.objs.insert(n, body);
        self.any_objs.push(n);
        n
    }
}

const FONT: u32 = 20;

fn font_objs(d: &mut Doc, ff: &[u8]) {
    let widths = vec!["600"; 95].join(" ");
    d.objs.insert(
        FONT,
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 \
             /LastChar 126 /Widths [{widths}] /Encoding /WinAnsiEncoding /FontDescriptor 21 0 R >>"
        )
        .into_bytes(),
    );
    d.objs.insert(
        21,
        b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 \
          /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 \
          /CapHeight 700 /StemV 80 /FontFile2 22 0 R >>"
            .to_vec(),
    );
    d.objs
        .insert(22, stream_obj(&format!("/Length1 {}", ff.len()), ff));
}

fn image(w: u32, h: u32, samples: &[u8]) -> Vec<u8> {
    stream_obj(
        &format!(
            "/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray \
             /BitsPerComponent 8"
        ),
        samples,
    )
}

/// What a content stream being built draws and names.
#[derive(Default)]
struct Content {
    ops: String,
    xobjects: Vec<(String, u32)>,
    fonts: Vec<(String, u32)>,
    shadings: Vec<(String, u32)>,
    patterns: Vec<(String, u32)>,
    /// Optional content groups named by `BDC /OC`.
    properties: Vec<(String, u32)>,
    mcid: i64,
    /// (MCID, alternate text) of the marked sequences.
    tagged: Vec<(i64, String)>,
}

impl Content {
    fn resources(&self) -> String {
        let mut s = format!("/Font << /F1 {FONT} 0 R");
        for (n, r) in &self.fonts {
            s.push_str(&format!(" /{n} {r} 0 R"));
        }
        s.push_str(" >>");
        if !self.xobjects.is_empty() {
            s.push_str(" /XObject <<");
            for (n, r) in &self.xobjects {
                s.push_str(&format!(" /{n} {r} 0 R"));
            }
            s.push_str(" >>");
        }
        for (key, list) in [
            ("Shading", &self.shadings),
            ("Pattern", &self.patterns),
            ("Properties", &self.properties),
        ] {
            if !list.is_empty() {
                s.push_str(&format!(" /{key} <<"));
                for (n, r) in list {
                    s.push_str(&format!(" /{n} {r} 0 R"));
                }
                s.push_str(" >>");
            }
        }
        s
    }
}

struct Gen<'a, 'r> {
    rng: Rng<'r>,
    v: u32,
    d: &'a mut Doc,
    k: u64,
    tagged: bool,
    /// Kept forms (public content only), to share between pages.
    kept_forms: Vec<u32>,
    /// Optional content groups (all off in the default configuration).
    ocgs: Vec<u32>,
}

impl Gen<'_, '_> {
    fn key(&mut self) -> u64 {
        self.k += 1;
        self.k
    }

    fn mark(&mut self, c: &mut Content, alt: String, body: String) {
        if self.tagged {
            let m = c.mcid;
            c.mcid += 1;
            c.ops
                .push_str(&format!("/P << /MCID {m} >> BDC {body} EMC "));
            c.tagged.push((m, alt));
        } else {
            c.ops.push_str(&body);
            c.ops.push(' ');
        }
    }

    /// One element drawn into `c` at `depth` (forms nest); `inside`:
    /// whether secrets may go there (page 1, not under a form placed
    /// outside).
    fn element(&mut self, c: &mut Content, page: usize, depth: u32, public: &mut Vec<String>) {
        let k = self.key();
        let edited = page == 0;
        let choice = self.rng.below(if edited { 13 } else { 4 });
        match choice {
            // Public text outside the rect.
            0 | 1 => {
                let t = format!("PUB{k:03}X");
                let y = if self.rng.chance(1, 2) {
                    20 + self.rng.below(25)
                } else {
                    150 + self.rng.below(35)
                };
                let x = 10 + self.rng.below(100);
                self.mark(
                    c,
                    format!("public {k}"),
                    format!("BT /F1 8 Tf {x} {y} Td ({t}) Tj ET"),
                );
                public.push(t);
            }
            // A kept form, maybe shared with another page.
            2 => {
                let f = if !self.kept_forms.is_empty() && self.rng.chance(1, 2) {
                    let i = self.rng.below(self.kept_forms.len() as u64) as usize;
                    self.kept_forms[i]
                } else {
                    let t = format!("FRM{k:03}X");
                    let inherit = self.rng.chance(1, 3);
                    let res = if inherit {
                        String::new()
                    } else {
                        format!("/Resources << /Font << /F1 {FONT} 0 R >> >>")
                    };
                    let body = format!("BT /F1 8 Tf 0 0 Td ({t}) Tj ET");
                    let f = self.d.add(stream_obj(
                        &format!("/Type /XObject /Subtype /Form /BBox [0 0 60 10] {res}"),
                        body.as_bytes(),
                    ));
                    self.kept_forms.push(f);
                    f
                };
                let name = format!("Kf{f}");
                if !c.xobjects.iter().any(|(n, _)| *n == name) {
                    c.xobjects.push((name.clone(), f));
                }
                let x = 10 + self.rng.below(100);
                self.mark(
                    c,
                    format!("kept form {k}"),
                    format!("q 1 0 0 1 {x} 25 cm /{name} Do Q"),
                );
                // Its text: extracted from the page.
                let body = self.d.objs[&f].clone();
                let s = String::from_utf8_lossy(&body).into_owned();
                if let Some(i) = s.find("(FRM") {
                    public.push(s[i + 1..i + 8].to_string());
                }
            }
            // A kept image outside.
            3 => {
                let samples: Vec<u8> = (0..16).map(|i| (i * 13 + k as u32) as u8).collect();
                let im = self.d.add(image(4, 4, &samples));
                let name = format!("Ki{im}");
                c.xobjects.push((name.clone(), im));
                self.mark(
                    c,
                    format!("kept image {k}"),
                    format!("q 10 0 0 10 160 10 cm /{name} Do Q"),
                );
            }
            // Secret text inside the rect (visible or invisible).
            4 | 5 => {
                let t = secret(self.v, k, 6);
                let mode = if self.rng.chance(1, 4) { 3 } else { 0 };
                let x = 65 + self.rng.below(30);
                let y = 70 + self.rng.below(60);
                self.mark(
                    c,
                    format!("ALT{}", secret(self.v, k, 5)),
                    format!("BT /F1 8 Tf {mode} Tr {x} {y} Td ({t}) Tj ET"),
                );
            }
            // A secret image inside the rect.
            6 => {
                let samples: Vec<u8> = (0..16)
                    .map(|i| (i * 7 + self.v * 101 + k as u32) as u8)
                    .collect();
                let im = self.d.add(image(4, 4, &samples));
                self.d.secret_objs.push(im);
                let name = format!("Si{im}");
                c.xobjects.push((name.clone(), im));
                self.mark(
                    c,
                    format!("ALT{}", secret(self.v, k, 5)),
                    format!("q 20 0 0 20 80 80 cm /{name} Do Q"),
                );
            }
            // An image across the rect's left edge: secret only in the
            // columns well inside.
            7 => {
                // Drawn at x 40..80: columns 0..3 outside, 4 on the edge.
                let samples: Vec<u8> = (0..64)
                    .map(|i| {
                        let col = i % 8;
                        if col >= 6 {
                            (i as u32 * 5 + self.v * 77) as u8
                        } else {
                            (i as u32 * 3) as u8
                        }
                    })
                    .collect();
                let im = self.d.add(image(8, 8, &samples));
                self.d.secret_objs.push(im);
                let name = format!("Xi{im}");
                c.xobjects.push((name.clone(), im));
                self.mark(
                    c,
                    format!("edge image {k}"),
                    format!("q 40 0 0 40 40 80 cm /{name} Do Q"),
                );
            }
            // A filled shape crossing the rect: what it draws outside is
            // public, its path inside the rect (a vertex or a curve's
            // control points) secret — it enters and leaves the rect at
            // the same points in both variants. Cut (0.10).
            8 => {
                let (sx, sy) = (
                    70 + (self.v * 37 + k as u32) % 60,
                    70 + (self.v * 53 + k as u32) % 60,
                );
                let rule = if self.rng.chance(1, 2) { "f" } else { "f*" };
                let shape = if self.rng.chance(1, 2) {
                    format!("30 90 m 60 90 l {sx} {sy} l 140 90 l 170 90 l 170 50 l 30 50 l h")
                } else {
                    format!("30 90 m 60 90 l 60 90 {sx} {sy} 140 90 c 170 90 l 170 50 l 30 50 l h")
                };
                self.mark(c, format!("shape {k}"), format!("0.3 g {shape} {rule} 0 g"));
            }
            // An axial shading in a band across the rect, varying only
            // along y: every colour it shows inside is shown outside.
            9 => {
                let sh = self.d.add(
                    b"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 0 200] /Extend [true true] \
                      /Function << /FunctionType 2 /Domain [0 1] /C0 [0.9 0.8 0.2] /C1 [0.2 0.8 0.9] /N 1 >> >>"
                        .to_vec(),
                );
                let name = format!("Sh{sh}");
                c.shadings.push((name.clone(), sh));
                self.mark(
                    c,
                    format!("shading {k}"),
                    format!("q 20 70 160 20 re W n /{name} sh Q"),
                );
            }
            // A rectangle filled with a tiling pattern across the rect,
            // a whole period showing outside.
            10 => {
                let pt = self.d.add(stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 \
                     /Resources << >>",
                    b"0.4 0.6 0.2 rg 0 0 4 4 re f",
                ));
                let name = format!("Pt{pt}");
                c.patterns.push((name.clone(), pt));
                self.mark(
                    c,
                    format!("pattern {k}"),
                    format!("/Pattern cs /{name} scn 20 100 160 30 re f 0 g"),
                );
            }
            // Secret text inside the rect under an optional content
            // group no viewer shows by default (hidden content).
            11 => {
                let ocg = self.d.add(b"<< /Type /OCG /Name (hidden) >>".to_vec());
                self.ocgs.push(ocg);
                let name = format!("Oc{ocg}");
                c.properties.push((name.clone(), ocg));
                let t = secret(self.v, k, 6);
                c.ops.push_str(&format!(
                    "/OC /{name} BDC BT /F1 8 Tf 75 95 Td ({t}) Tj ET EMC "
                ));
            }
            // A secret form inside the rect, maybe nesting another.
            _ => {
                let mut inner = Content::default();
                let t = secret(self.v, k, 5);
                inner
                    .ops
                    .push_str(&format!("BT /F1 6 Tf 0 0 Td ({t}) Tj ET "));
                if depth < 2 && self.rng.chance(1, 2) {
                    let t2 = secret(self.v, k + 1000, 4);
                    let nested = self.d.add(stream_obj(
                        "/Type /XObject /Subtype /Form /BBox [0 0 30 10]",
                        format!("BT /F1 6 Tf 0 0 Td ({t2}) Tj ET").as_bytes(),
                    ));
                    self.d.secret_objs.push(nested);
                    inner.xobjects.push((format!("N{nested}"), nested));
                    inner
                        .ops
                        .push_str(&format!("q 1 0 0 1 0 12 cm /N{nested} Do Q"));
                }
                let inherit = self.rng.chance(1, 3) && inner.xobjects.is_empty();
                let res = if inherit {
                    String::new()
                } else {
                    format!("/Resources << {} >>", inner.resources())
                };
                let f = self.d.add(stream_obj(
                    &format!("/Type /XObject /Subtype /Form /BBox [0 0 40 30] {res}"),
                    inner.ops.as_bytes(),
                ));
                self.d.secret_objs.push(f);
                let name = format!("Sf{f}");
                c.xobjects.push((name.clone(), f));
                self.mark(
                    c,
                    format!("ALT{}", secret(self.v, k, 5)),
                    format!("q 1 0 0 1 75 85 cm /{name} Do Q"),
                );
            }
        }
    }
}

/// A Type 3 font whose glyph `a` (secret drawing) is shown only inside
/// the rect and glyph `b` (public) maybe outside.
fn type3(g: &mut Gen<'_, '_>, c: &mut Content, public_b: bool) {
    let v = g.v;
    let pa = g.d.add(stream_obj(
        "",
        format!("500 0 d0 0 0 {} 600 re f", 100 + v * 50).as_bytes(),
    ));
    g.d.secret_objs.push(pa);
    let pb = g.d.add(stream_obj("", b"500 0 d0 0 0 400 300 re f"));
    let cp =
        g.d.add(format!("<< /a {pa} 0 R /b {pb} 0 R >>").into_bytes());
    let f = g.d.add(
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 700] /FontMatrix [0.01 0 0 0.01 0 0] \
             /CharProcs {cp} 0 R /Encoding << /Type /Encoding /Differences [97 /a /b] >> \
             /FirstChar 97 /LastChar 98 /Widths [500 500] /Resources << >> >>"
        )
        .into_bytes(),
    );
    c.fonts.push((format!("T{f}"), f));
    c.ops
        .push_str(&format!("BT /T{f} 1 Tf 90 100 Td (a) Tj ET "));
    if public_b {
        c.ops
            .push_str(&format!("BT /T{f} 1 Tf 20 20 Td (b) Tj ET "));
    }
}

/// The document for choices `rng` in variant `v` (1 or 2), with the
/// public texts each page draws; `font` the TrueType program used.
pub fn make(mut rng: Rng<'_>, v: u32, font: &[u8]) -> (Vec<u8>, Vec<Vec<String>>) {
    let mut d = Doc {
        objs: BTreeMap::new(),
        next: 100,
        secret_objs: Vec::new(),
        any_objs: Vec::new(),
        public: Vec::new(),
        late: Vec::new(),
    };
    font_objs(&mut d, font);
    let seed = rng.state;
    let pages = 1 + rng.below(3) as usize;
    let tagged = rng.chance(1, 3);
    let layout = match rng.below(4) {
        0 => Layout::ObjectStreams,
        1 => Layout::Incremental,
        _ => Layout::Plain,
    };
    let use_type3 = rng.chance(1, 4);
    let mut g = Gen {
        rng,
        v,
        d: &mut d,
        k: 0,
        tagged,
        kept_forms: Vec::new(),
        ocgs: Vec::new(),
    };
    let mut page_objs = Vec::new();
    let mut struct_kids: Vec<(usize, i64, String)> = Vec::new();
    let mut page_res: Vec<u32> = Vec::new();
    // One resource dictionary shared by every page, or one each.
    let shared_res = g.rng.chance(1, 3);
    let mut all_x: Vec<(String, u32)> = Vec::new();
    let mut all_f: Vec<(String, u32)> = Vec::new();
    let mut all_s: Vec<(String, u32)> = Vec::new();
    let mut all_p: Vec<(String, u32)> = Vec::new();
    let mut all_o: Vec<(String, u32)> = Vec::new();
    let mut contents: Vec<Content> = Vec::new();
    for p in 0..pages {
        let mut c = Content::default();
        let mut public = Vec::new();
        for _ in 0..(2 + g.rng.below(4)) {
            g.element(&mut c, p, 0, &mut public);
        }
        if use_type3 && p == 0 {
            let pb = g.rng.chance(1, 2);
            type3(&mut g, &mut c, pb);
        }
        // Always a secret on page 1 and something public on every page.
        if p == 0 {
            let t = secret(v, 999, 6);
            c.ops
                .push_str(&format!("BT /F1 8 Tf 70 120 Td ({t}) Tj ET "));
        }
        let t = format!("PUBP{p}END");
        c.ops.push_str(&format!("BT /F1 8 Tf 10 10 Td ({t}) Tj ET"));
        public.push(t);
        for (m, alt) in &c.tagged {
            struct_kids.push((p, *m, alt.clone()));
        }
        all_x.extend(c.xobjects.iter().cloned());
        all_f.extend(c.fonts.iter().cloned());
        all_s.extend(c.shadings.iter().cloned());
        all_p.extend(c.patterns.iter().cloned());
        all_o.extend(c.properties.iter().cloned());
        g.d.public.push(public);
        contents.push(c);
    }
    // Annotations on page 1: one inside the rect (secret appearance and
    // text), one outside (public).
    let mut annots = Vec::new();
    if g.rng.chance(1, 2) {
        let ap = g.d.add(stream_obj(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 30 10] /Resources << /Font << /F1 {FONT} 0 R >> >>"),
            format!("BT /F1 6 Tf 0 0 Td ({}) Tj ET", secret(v, 900, 5)).as_bytes(),
        ));
        g.d.secret_objs.push(ap);
        // Sometimes hidden (`/F 2`: drawn by no viewer, still removed).
        let hidden = if g.rng.chance(1, 3) { "/F 2" } else { "" };
        // Sometimes with a script (`remove_javascript` edits it).
        let js = if g.rng.chance(1, 2) {
            "/A << /S /JavaScript /JS (app.alert(1)) >>"
        } else {
            ""
        };
        let a = g.d.add(
            format!(
                "<< /Type /Annot /Subtype /Stamp /Rect [80 80 110 90] /AP << /N {ap} 0 R >> \
                 /Contents ({}) {js} {hidden} >>",
                secret(v, 901, 6)
            )
            .into_bytes(),
        );
        g.d.secret_objs.push(a);
        annots.push(a);
    }
    if g.rng.chance(1, 2) {
        let a = g.d.add(
            b"<< /Type /Annot /Subtype /Link /Rect [10 180 40 190] /A << /S /URI /URI (https://example.com/) >> >>"
                .to_vec(),
        );
        annots.push(a);
    }
    // Form fields: a widget inside the rect showing a secret value, a
    // widget outside showing a public one (0.10).
    let mut form_fields: Vec<u32> = Vec::new();
    if g.rng.chance(1, 3) {
        let ap = g.d.add(stream_obj(
            &format!("/Type /XObject /Subtype /Form /BBox [0 0 40 10] /Resources << /Font << /F1 {FONT} 0 R >> >>"),
            format!("BT /F1 6 Tf 0 0 Td ({}) Tj ET", secret(v, 950, 5)).as_bytes(),
        ));
        g.d.secret_objs.push(ap);
        let w = g.d.add(
            format!(
                "<< /Type /Annot /Subtype /Widget /FT /Tx /T (s{}) /V ({}) /Rect [80 100 120 110] \
                 /AP << /N {ap} 0 R >> >>",
                g.k,
                secret(v, 951, 6)
            )
            .into_bytes(),
        );
        g.d.secret_objs.push(w);
        annots.push(w);
        form_fields.push(w);
        if g.rng.chance(1, 2) {
            let k = g.d.add(
                b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (kept) /V (PUBLICFIELD) /Rect [10 160 50 170] >>"
                    .to_vec(),
            );
            annots.push(k);
            form_fields.push(k);
        }
    }
    for (p, c) in contents.iter().enumerate() {
        let cs = g.d.add(stream_obj("", c.ops.as_bytes()));
        if p == 0 {
            g.d.secret_objs.push(cs);
        }
        let res = if shared_res {
            if page_res.is_empty() {
                let all = Content {
                    xobjects: all_x.clone(),
                    fonts: all_f.clone(),
                    shadings: all_s.clone(),
                    patterns: all_p.clone(),
                    properties: all_o.clone(),
                    ..Content::default()
                };
                let r = g.d.add(format!("<< {} >>", all.resources()).into_bytes());
                page_res.push(r);
            }
            page_res[0]
        } else {
            let r = g.d.add(format!("<< {} >>", c.resources()).into_bytes());
            page_res.push(r);
            r
        };
        let annots = if p == 0 && !annots.is_empty() {
            format!(
                "/Annots [{}]",
                annots
                    .iter()
                    .map(|a| format!("{a} 0 R"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        } else {
            String::new()
        };
        let sp = if tagged {
            format!("/StructParents {p}")
        } else {
            String::new()
        };
        page_objs.push((cs, res, annots, sp));
    }
    // Hostile references to secret objects from places that draw nothing.
    let mut cat_extra = String::new();
    if !form_fields.is_empty() {
        cat_extra.push_str(&format!(
            " /AcroForm << /Fields [{}] >>",
            form_fields
                .iter()
                .map(|f| format!("{f} 0 R"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !g.ocgs.is_empty() {
        let list = g
            .ocgs
            .iter()
            .map(|o| format!("{o} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        cat_extra.push_str(&format!(
            " /OCProperties << /OCGs [{list}] /D << /OFF [{list}] >> >>"
        ));
    }
    let mut page_extra: Vec<String> = vec![String::new(); pages];
    // A thumbnail of page 1, picturing the secret.
    if g.rng.chance(1, 4) {
        let samples: Vec<u8> = (0..16).map(|i| (i * 11 + v * 59) as u8).collect();
        let th = g.d.add(stream_obj(
            "/Width 4 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 8",
            &samples,
        ));
        g.d.secret_objs.push(th);
        page_extra[0].push_str(&format!(" /Thumb {th} 0 R"));
    }
    let targets = g.d.secret_objs.clone();
    for i in 0..g.rng.below(4) {
        let pick =
            |g: &mut Gen<'_, '_>, list: &[u32]| list[g.rng.below(list.len() as u64) as usize];
        let target = if !targets.is_empty() && g.rng.chance(3, 4) {
            pick(&mut g, &targets)
        } else {
            let all = g.d.any_objs.clone();
            pick(&mut g, &all)
        };
        let late = layout == Layout::Incremental && g.rng.chance(1, 2);
        let alias = |g: &mut Gen<'_, '_>, body: Vec<u8>| {
            let n = g.d.add(body);
            if late {
                g.d.late.push(n);
            }
            n
        };
        match g.rng.below(7) {
            0 => cat_extra.push_str(&format!(" /XPriv{i} {target} 0 R")),
            1 => {
                let n = alias(
                    &mut g,
                    format!("<< /App << /Private {target} 0 R >> >>").into_bytes(),
                );
                cat_extra.push_str(&format!(" /PieceInfo {n} 0 R"));
            }
            2 => {
                let p = g.rng.below(pages as u64) as usize;
                page_extra[p].push_str(&format!(
                    " /PieceInfo << /App << /Private {target} 0 R >> >>"
                ));
            }
            3 => {
                // An identical copy under another number.
                let body = g.d.objs[&target].clone();
                let n = alias(&mut g, body);
                cat_extra.push_str(&format!(" /XCopy{i} {n} 0 R"));
            }
            4 => {
                // Dangling, wrong generation, a cycle.
                let n = alias(&mut g, b"[]".to_vec());
                g.d.objs
                    .insert(n, format!("[{n} 0 R 9999 0 R {target} 1 R]").into_bytes());
                cat_extra.push_str(&format!(" /XNoise{i} {n} 0 R"));
            }
            5 => {
                // An unused name on the last page's resources.
                if !shared_res && pages > 1 {
                    let r = page_res[pages - 1];
                    let body = String::from_utf8(g.d.objs[&r].clone()).unwrap();
                    let body = body.replacen(
                        "<<",
                        &format!("<< /XObject << /Alias{i} {target} 0 R >>"),
                        1,
                    );
                    // A second /XObject key would be ambiguous: only
                    // when the page names none.
                    if !body.contains("/XObject << /K") && body.matches("/XObject").count() == 1 {
                        g.d.objs.insert(r, body.into_bytes());
                    }
                }
            }
            _ => {
                if String::from_utf8_lossy(&g.d.objs[&target]).contains("stream") {
                    let n = alias(
                        &mut g,
                        format!("<< /Names [(f{i}) << /Type /Filespec /F (f{i}) /EF << /F {target} 0 R >> >>] >>")
                            .into_bytes(),
                    );
                    cat_extra.push_str(&format!(" /Names << /EmbeddedFiles {n} 0 R >>"));
                }
            }
        }
    }
    // The structure tree.
    let mut struct_root = String::new();
    if tagged && !struct_kids.is_empty() {
        let root = g.d.next + 1000;
        let mut kids = Vec::new();
        let mut nums: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for (p, m, alt) in &struct_kids {
            let page_ref = 10 + *p as u32 * 2;
            let e = g.d.add(
                format!("<< /Type /StructElem /S /P /P {root} 0 R /Pg {page_ref} 0 R /K {m} /Alt ({alt}) >>")
                    .into_bytes(),
            );
            if alt.starts_with("ALT") && *p == 0 {
                g.d.secret_objs.push(e);
            }
            kids.push(format!("{e} 0 R"));
            nums.entry(*p).or_default().push(format!("{e} 0 R"));
        }
        let nums: Vec<String> = nums
            .iter()
            .map(|(p, es)| format!("{p} [{}]", es.join(" ")))
            .collect();
        g.d.objs.insert(
            root,
            format!(
                "<< /Type /StructTreeRoot /K [{}] /ParentTree << /Nums [{}] >> >>",
                kids.join(" "),
                nums.join(" ")
            )
            .into_bytes(),
        );
        struct_root = format!(" /StructTreeRoot {root} 0 R /MarkInfo << /Marked true >>");
    }
    // Catalog, page tree, pages (numbers 1, 2, 10 + 2p).
    let kids: Vec<String> = (0..pages).map(|p| format!("{} 0 R", 10 + p * 2)).collect();
    d.objs.insert(
        1,
        format!("<< /Type /Catalog /Pages 2 0 R{struct_root}{cat_extra} >>").into_bytes(),
    );
    d.objs.insert(
        2,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        )
        .into_bytes(),
    );
    for (p, (cs, res, annots, sp)) in page_objs.iter().enumerate() {
        d.objs.insert(
            10 + p as u32 * 2,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources {res} 0 R \
                 /Contents {cs} 0 R {annots} {sp}{} >>",
                page_extra[p]
            )
            .into_bytes(),
        );
    }
    let public = d.public.clone();
    (write(&d, layout, seed), public)
}

/// Serialise: a classic xref; object and cross-reference streams; or
/// two revisions (the late objects and the catalog in the second).
fn write(d: &Doc, layout: Layout, seed: u64) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let size = d.objs.keys().max().unwrap() + 3;
    let mut offsets: BTreeMap<u32, usize> = BTreeMap::new();
    let put = |out: &mut Vec<u8>, offsets: &mut BTreeMap<u32, usize>, n: u32, body: &[u8]| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    match layout {
        Layout::ObjectStreams => {
            // Non-stream objects (some) into one object stream.
            let mut members: Vec<u32> = Vec::new();
            for (n, body) in &d.objs {
                let is_stream = body.windows(6).any(|w| w == b"stream");
                if !is_stream && !(n * 7).wrapping_add(seed as u32).is_multiple_of(3) {
                    members.push(*n);
                } else {
                    put(&mut out, &mut offsets, *n, body);
                }
            }
            let stm = size - 2;
            let xr = size - 1;
            let mut head = String::new();
            let mut body = Vec::new();
            for n in &members {
                head.push_str(&format!("{n} {} ", body.len()));
                body.extend_from_slice(&d.objs[n]);
                body.push(b'\n');
            }
            let mut data = head.clone().into_bytes();
            data.extend_from_slice(&body);
            put(
                &mut out,
                &mut offsets,
                stm,
                &stream_obj(
                    &format!("/Type /ObjStm /N {} /First {}", members.len(), head.len()),
                    &data,
                ),
            );
            let xoff = out.len();
            let mut rows = Vec::new();
            for n in 0..size {
                let mut row = [0u8; 7];
                if n == xr {
                    row[0] = 1;
                    row[1..5].copy_from_slice(&(xoff as u32).to_be_bytes());
                } else if let Some(o) = offsets.get(&n) {
                    row[0] = 1;
                    row[1..5].copy_from_slice(&(*o as u32).to_be_bytes());
                } else if let Some(i) = members.iter().position(|m| *m == n) {
                    row[0] = 2;
                    row[1..5].copy_from_slice(&stm.to_be_bytes());
                    row[5..7].copy_from_slice(&(i as u16).to_be_bytes());
                } else {
                    row[5..7].copy_from_slice(&0xffffu16.to_be_bytes());
                }
                rows.extend_from_slice(&row);
            }
            out.extend_from_slice(format!("{xr} 0 obj\n").as_bytes());
            out.extend_from_slice(&stream_obj(
                &format!("/Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R"),
                &rows,
            ));
            out.extend_from_slice(format!("\nendobj\nstartxref\n{xoff}\n%%EOF\n").as_bytes());
        }
        Layout::Plain | Layout::Incremental => {
            let late: Vec<u32> = if layout == Layout::Incremental {
                d.late.iter().copied().chain([1]).collect()
            } else {
                Vec::new()
            };
            for (n, body) in &d.objs {
                if late.contains(n) && *n != 1 {
                    continue;
                }
                // The first revision's catalog: without the late aliases.
                put(&mut out, &mut offsets, *n, body);
            }
            let x = out.len();
            out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
            for n in 1..size {
                match offsets.get(&n) {
                    Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                    None => out.extend_from_slice(b"0000000000 65535 f \n"),
                }
            }
            out.extend_from_slice(
                format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n")
                    .as_bytes(),
            );
            if layout == Layout::Incremental {
                let mut offs2 = BTreeMap::new();
                for n in &late {
                    put(&mut out, &mut offs2, *n, &d.objs[n]);
                }
                let x2 = out.len();
                out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
                for (n, o) in &offs2 {
                    out.extend_from_slice(format!("{n} 1\n{o:010} 00000 n \n").as_bytes());
                }
                out.extend_from_slice(
                    format!(
                        "trailer\n<< /Size {size} /Root 1 0 R /Prev {x} >>\nstartxref\n{x2}\n%%EOF\n"
                    )
                    .as_bytes(),
                );
            }
        }
    }
    out
}

/// The error text with what names a particular glyph masked.
pub fn mask(e: &kasane_pdf::Error) -> String {
    let s = e.to_string();
    match s.find("glyph 0x") {
        Some(i) => {
            let j = s[i + 8..].find(' ').map_or(s.len(), |k| i + 8 + k);
            format!("{}glyph{}", &s[..i], &s[j..])
        }
        None => s,
    }
}
