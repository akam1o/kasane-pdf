//! What a document *uses*: the streams a reader reads to draw its pages,
//! each with the role it is read in and the pages reading it.
//!
//! The walk starts at each selected page — its content streams, its
//! thumbnail, its annotations' appearance streams — and follows only
//! what drawing reads: an XObject a `Do` invokes (an image with its
//! masks and colour space, a form with what it draws), the font a `Tf`
//! or an ExtGState `/Font` selects (its program and code data; a Type 3
//! font's procedures for the glyphs actually shown, with what they
//! draw), an ExtGState's soft mask group, transfer functions and
//! halftone, a pattern a colour operator selects, a shading `sh`
//! paints, the colour spaces in force (ICC profiles, lookup tables,
//! tint functions). Names resolve against the resources in force, as
//! [`crate::redact::drawn_xobjects`] resolves them: a form's own
//! `/Resources`, else those it inherits.
//!
//! It is the basis of the verification's use invariants
//! ([`crate::redact_verify`]), which compare what the input used with
//! what the output uses. An undecidable point — a name that resolves
//! to nothing, a stream that does not decode, an XObject of no known
//! kind — is handled by [`Bound`]: over the input, everything the page
//! reaches counts as possibly used (the comparison then expects more of
//! the output, never less); over the output, nothing beyond what was
//! decided counts (a copy there is then unexplained, never excused).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::content::{Operation, TJItem};
use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};

/// The role a stream is read in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Use {
    /// A page's `/Contents`.
    Contents,
    /// An annotation appearance stream (`/AP`, every state).
    Appearance,
    /// A form XObject a `Do` invokes.
    Form,
    /// An image XObject a `Do` invokes.
    Image,
    /// An image's `/SMask` or stencil `/Mask`.
    Mask,
    /// A page's `/Thumb`.
    Thumbnail,
    /// A Type 3 glyph procedure of a glyph shown.
    CharProc,
    /// A tiling pattern's cell.
    Pattern,
    /// The group of an ExtGState soft mask.
    SoftMaskGroup,
    /// A shading stream (mesh data).
    Shading,
    /// A function stream (sampled or PostScript calculator).
    Function,
    /// A colour space's data: an `/Indexed` lookup table.
    ColorSpaceData,
    /// An ICC profile.
    IccProfile,
    /// A font program (`/FontFile`, `/FontFile2`, `/FontFile3`).
    FontProgram,
    /// A font's code data: `/ToUnicode`, an encoding CMap and its
    /// `/UseCMap`, `/CIDSet`, `/CIDToGIDMap`.
    FontData,
    /// A halftone stream.
    Halftone,
    /// Reached from a page whose use could not be decided
    /// ([`Bound::Over`] only).
    Undecided,
}

impl Use {
    /// Whether a stream read in this role pictures or spells content —
    /// text, shapes, samples, code to text mappings of shown glyphs —
    /// rather than describing how to read content (a font program, a
    /// font's code data, an ICC profile). The verification's first use
    /// invariant applies to these.
    pub(crate) fn content_bearing(self) -> bool {
        !matches!(self, Use::IccProfile | Use::FontProgram | Use::FontData)
    }
}

/// How an undecidable point is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// Everything the page reaches counts as possibly used.
    Over,
    /// Only what was decided counts.
    Under,
}

/// The streams used, by reference: their roles and the pages using them.
#[derive(Default, Debug)]
pub(crate) struct Usage {
    pub(crate) used: HashMap<IndirectRef, (BTreeSet<Use>, BTreeSet<usize>)>,
    /// Per page: why its use could not (wholly) be decided.
    pub(crate) undecided: Vec<(usize, String)>,
}

impl Usage {
    /// The streams page `page` uses, with their roles.
    pub(crate) fn on_page(
        &self,
        page: usize,
    ) -> impl Iterator<Item = (IndirectRef, &BTreeSet<Use>)> {
        self.used
            .iter()
            .filter(move |(_, (_, pages))| pages.contains(&page))
            .map(|(r, (uses, _))| (*r, uses))
    }
}

/// The resources a content stream resolves names against, and an
/// identity for them (the frame is read once per resources and font).
#[derive(Clone)]
struct Frame {
    ops: Vec<Operation>,
    res: Object,
    res_id: ResId,
    depth: usize,
    /// The state in force where the stream is invoked (a form
    /// inherits the graphics and text state).
    state: State,
}

/// What painting would read, held until something is painted: a colour
/// selected but never painted with, or a soft mask in force over
/// nothing, is not used.
#[derive(Clone, Default)]
struct State {
    /// The Type 3 font in force.
    font: Option<Type3>,
    fill: Paint,
    stroke: Paint,
    /// ExtGState dictionaries in force (with the resources they were
    /// selected in) whose soft mask, transfer functions or halftone
    /// apply to what is painted next.
    gs: Vec<(Object, Object, ResId)>,
    /// The text rendering mode (`Tr`).
    render: i64,
}

/// A non-device colour in force: its colour space (a resource value,
/// with the resources it was selected in) and, for a pattern colour,
/// the pattern (a resource entry).
#[derive(Clone, Default)]
struct Paint {
    space: Option<(Object, Object)>,
    pattern: Option<Object>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ResId {
    /// The page's (inherited) resources.
    Page,
    /// The resources of the stream with this reference.
    Own(IndirectRef),
    /// None (an appearance without `/Resources`).
    Empty,
}

/// A Type 3 font in force: its dictionary's reference (or `None` when
/// direct) and the loaded font.
#[derive(Clone)]
struct Type3 {
    id: Option<IndirectRef>,
    font: std::rc::Rc<crate::font::Type3Font>,
}

struct Walk<'a> {
    doc: &'a Document,
    limits: &'a Limits,
    bound: Bound,
    u: Usage,
    page: usize,
    budget: u64,
    seen_frames: HashSet<(IndirectRef, ResId, Option<IndirectRef>)>,
    seen_fonts: HashSet<IndirectRef>,
    loaded: HashMap<IndirectRef, Option<Type3>>,
    undecided_here: bool,
}

/// What the pages `select` chooses use (see the module documentation).
pub(crate) fn usage(
    doc: &Document,
    select: &dyn Fn(usize) -> bool,
    bound: Bound,
    limits: &Limits,
) -> Result<Usage, Error> {
    let _fonts = doc.font_cache_scope();
    let mut w = Walk {
        doc,
        limits,
        bound,
        u: Usage::default(),
        page: 0,
        budget: 0,
        seen_frames: HashSet::new(),
        seen_fonts: HashSet::new(),
        loaded: HashMap::new(),
        undecided_here: false,
    };
    for (i, page) in doc.pages().iter().enumerate() {
        if !select(i) {
            continue;
        }
        w.page = i;
        w.seen_frames.clear();
        w.seen_fonts.clear();
        w.undecided_here = false;
        w.walk_page(page)?;
        if w.undecided_here && bound == Bound::Over {
            w.mark_reachable(page)?;
        }
    }
    Ok(w.u)
}

impl Walk<'_> {
    fn tick(&mut self, n: u64) -> Result<(), Error> {
        self.budget = self.budget.saturating_add(n);
        if self.budget > self.limits.max_diag_exec_ops {
            return Err(Error::LimitExceeded {
                limit: "max_diag_exec_ops",
                max: self.limits.max_diag_exec_ops,
            });
        }
        Ok(())
    }

    fn undecided(&mut self, why: String) {
        self.undecided_here = true;
        if self.u.undecided.len() < 64 {
            self.u.undecided.push((self.page, why));
        }
    }

    /// Record that `r` is used as `role` on the current page; whether
    /// this is its first use in that role here.
    fn mark(&mut self, r: IndirectRef, role: Use) -> bool {
        let e = self.u.used.entry(r).or_default();
        let new = e.0.insert(role);
        e.1.insert(self.page) || new
    }

    /// Mark `o` (a reference) if it is a stream; `false` when it is not.
    fn mark_stream(&mut self, o: &Object, role: Use) -> Result<bool, Error> {
        let Object::Ref(r) = o else {
            return Ok(false);
        };
        if matches!(self.doc.resolve(*r)?, Object::Stream { .. }) {
            self.mark(*r, role);
            return Ok(true);
        }
        Ok(false)
    }

    fn walk_page(&mut self, page: &crate::page::Page) -> Result<(), Error> {
        let doc = self.doc;
        let Some(pr) = page.object_ref() else {
            self.undecided("a page that is not an indirect object".into());
            return Ok(());
        };
        let pd = match doc.resolve(pr)? {
            Object::Dict(d) => d,
            _ => {
                self.undecided("a page that is not a dictionary".into());
                return Ok(());
            }
        };
        let res = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Dict::new()));
        // Contents.
        match pd.get(b"Contents".as_slice()) {
            Some(Object::Ref(r)) => match doc.resolve(*r)? {
                Object::Array(a) => {
                    for c in &a {
                        self.mark_stream(c, Use::Contents)?;
                    }
                }
                Object::Stream { .. } => {
                    self.mark(*r, Use::Contents);
                }
                _ => {}
            },
            Some(Object::Array(a)) => {
                for c in a {
                    self.mark_stream(c, Use::Contents)?;
                }
            }
            _ => {}
        }
        if let Some(t) = pd.get(b"Thumb".as_slice()) {
            self.mark_stream(t, Use::Thumbnail)?;
        }
        if let Some(g) = pd.get(b"Group".as_slice()) {
            let g = doc.deref(g)?;
            if let Some(cs) = g.as_dict().and_then(|d| d.get(b"CS".as_slice())) {
                self.color_space(cs, &res)?;
            }
        }
        let mut stack: Vec<Frame> = Vec::new();
        match doc.page_operations(page) {
            Ok(ops) => stack.push(Frame {
                ops,
                res: res.clone(),
                res_id: ResId::Page,
                depth: 0,
                state: State::default(),
            }),
            Err(e @ Error::LimitExceeded { .. }) => return Err(e),
            Err(e) => self.undecided(format!("its content is not read: {e}")),
        }
        // Annotation appearances: their own resources, else none.
        if let Some(annots) = pd.get(b"Annots".as_slice()) {
            let annots = doc.deref(annots)?;
            for a in annots.as_array().unwrap_or(&[]) {
                let a = doc.deref(a)?;
                let Some(ad) = a.as_dict() else { continue };
                if let Some(ap) = ad.get(b"AP".as_slice()) {
                    let ap = doc.deref(ap)?;
                    for k in [b"N".as_slice(), b"R", b"D"] {
                        let Some(v) = ap.as_dict().and_then(|d| d.get(k)) else {
                            continue;
                        };
                        let states: Vec<Object> = match v {
                            Object::Ref(r) => match doc.resolve(*r)? {
                                Object::Dict(s) => s.values().cloned().collect(),
                                _ => vec![v.clone()],
                            },
                            Object::Dict(s) => s.values().cloned().collect(),
                            _ => Vec::new(),
                        };
                        for s in states {
                            if let Object::Ref(r) = s {
                                let empty = Object::Dict(Dict::new());
                                if let Some(f) = self.enter_form(
                                    r,
                                    Use::Appearance,
                                    &empty,
                                    ResId::Empty,
                                    0,
                                    &State::default(),
                                )? {
                                    stack.push(f);
                                }
                            }
                        }
                    }
                }
                // Icons a reader may draw a widget's appearance from.
                if self.bound == Bound::Over {
                    if let Some(mk) = ad.get(b"MK".as_slice()) {
                        let mk = doc.deref(mk)?;
                        for k in [b"I".as_slice(), b"RI", b"IX"] {
                            if let Some(i) = mk.as_dict().and_then(|d| d.get(k)) {
                                self.mark_stream(i, Use::Appearance)?;
                            }
                        }
                    }
                }
            }
        }
        while let Some(f) = stack.pop() {
            self.run(f, &mut stack)?;
        }
        Ok(())
    }

    /// Enter form `r` (as `role`), invoked under `res`: the frame to
    /// run, once per resources and font.
    fn enter_form(
        &mut self,
        r: IndirectRef,
        role: Use,
        res: &Object,
        res_id: ResId,
        depth: usize,
        state: &State,
    ) -> Result<Option<Frame>, Error> {
        let doc = self.doc;
        let obj = doc.resolve(r)?;
        let Object::Stream { dict, data } = &obj else {
            return Ok(None);
        };
        self.mark(r, role);
        let (res, res_id) = match dict.get(b"Resources".as_slice()) {
            None | Some(Object::Null) => (res.clone(), res_id),
            Some(own) => (own.clone(), ResId::Own(r)),
        };
        if let Some(g) = dict.get(b"Group".as_slice()) {
            let g = doc.deref(g)?;
            if let Some(cs) = g.as_dict().and_then(|d| d.get(b"CS".as_slice())) {
                self.color_space(cs, &res)?;
            }
        }
        let font_id = state.font.as_ref().and_then(|f| f.id);
        if !self.seen_frames.insert((r, res_id, font_id)) {
            return Ok(None);
        }
        if depth + 1 > self.limits.max_xobject_depth as usize {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                max: self.limits.max_xobject_depth as u64,
            });
        }
        let decoded = match doc.decode_stream(dict, data) {
            Ok(d) => d,
            Err(e @ Error::LimitExceeded { .. }) => return Err(e),
            Err(e) => {
                self.undecided(format!("stream {r} does not decode: {e}"));
                return Ok(None);
            }
        };
        let mut warnings = Vec::new();
        let ops = match crate::content::parse_content_stream(
            &[decoded],
            self.limits,
            doc.recovery_mode(),
            &mut warnings,
        ) {
            Ok(o) => o,
            Err(e @ Error::LimitExceeded { .. }) => return Err(e),
            Err(e) => {
                self.undecided(format!("stream {r} does not parse: {e}"));
                return Ok(None);
            }
        };
        Ok(Some(Frame {
            ops,
            res,
            res_id,
            depth: depth + 1,
            state: state.clone(),
        }))
    }

    /// A resource entry as written, `None` (recorded as undecided) when
    /// the name resolves to nothing usable.
    fn resource(&mut self, res: &Object, category: &str, name: &[u8]) -> Option<Object> {
        match self.doc.resolve_resource_raw(res, category, name) {
            Ok(Some(o)) => Some(o),
            Ok(None) => {
                self.undecided(format!(
                    "/{category} /{} is not in the resources in force",
                    String::from_utf8_lossy(name)
                ));
                None
            }
            Err(e) => {
                self.undecided(format!(
                    "/{category} /{}: {e}",
                    String::from_utf8_lossy(name)
                ));
                None
            }
        }
    }

    fn run(&mut self, f: Frame, stack: &mut Vec<Frame>) -> Result<(), Error> {
        let doc = self.doc;
        self.tick(f.ops.len() as u64)?;
        let mut st = f.state.clone();
        let mut saved: Vec<State> = Vec::new();
        for op in &f.ops {
            match op {
                Operation::SaveState => saved.push(st.clone()),
                Operation::RestoreState => st = saved.pop().unwrap_or_default(),
                Operation::SetTextFont { font: name, .. } => {
                    st.font = match self.resource(&f.res, "Font", name) {
                        Some(o) => self.select_font(&o)?,
                        None => None,
                    };
                }
                Operation::SetTextRenderingMode(m) => st.render = *m,
                Operation::SetExtGState(name) => {
                    let Some(gs) = self.resource(&f.res, "ExtGState", name) else {
                        continue;
                    };
                    let g = doc.deref(&gs)?;
                    let font = g
                        .as_dict()
                        .and_then(|d| d.get(b"Font".as_slice()))
                        .map(|fo| doc.deref(fo))
                        .transpose()?;
                    if let Some(first) = font
                        .as_ref()
                        .and_then(|fo| fo.as_array())
                        .and_then(|a| a.first())
                    {
                        st.font = self.select_font(first)?;
                    }
                    // `/SMask /None` ends the soft masks in force.
                    let ends_mask = g
                        .as_dict()
                        .and_then(|d| d.get(b"SMask".as_slice()))
                        .and_then(|m| m.as_name())
                        == Some(b"None");
                    if ends_mask {
                        st.gs.clear();
                    }
                    st.gs.push((g, f.res.clone(), f.res_id));
                }
                Operation::SetColorSpaceFill(n) | Operation::SetColorSpaceStroke(n) => {
                    let space = if is_device_space(n) {
                        None
                    } else {
                        self.resource(&f.res, "ColorSpace", n)
                            .map(|cs| (cs, f.res.clone()))
                    };
                    let p = Paint {
                        space,
                        pattern: None,
                    };
                    if matches!(op, Operation::SetColorSpaceFill(_)) {
                        st.fill = p;
                    } else {
                        st.stroke = p;
                    }
                }
                Operation::SetColorFill(v) | Operation::SetColorStroke(v) => {
                    let pattern = match v.last() {
                        Some(Object::Name(n)) => self.resource(&f.res, "Pattern", n),
                        _ => None,
                    };
                    if matches!(op, Operation::SetColorFill(_)) {
                        st.fill.pattern = pattern;
                    } else {
                        st.stroke.pattern = pattern;
                    }
                }
                Operation::SetGrayFill(_)
                | Operation::SetRgbFill(..)
                | Operation::SetCmykFill(..) => st.fill = Paint::default(),
                Operation::SetGrayStroke(_)
                | Operation::SetRgbStroke(..)
                | Operation::SetCmykStroke(..) => st.stroke = Paint::default(),
                Operation::Fill | Operation::FillEvenOdd => {
                    self.paint(&mut st, true, false, &f, stack)?
                }
                Operation::Stroke | Operation::CloseAndStroke => {
                    self.paint(&mut st, false, true, &f, stack)?
                }
                Operation::FillAndStroke
                | Operation::FillAndStrokeEvenOdd
                | Operation::CloseFillAndStroke
                | Operation::CloseFillAndStrokeEvenOdd => {
                    self.paint(&mut st, true, true, &f, stack)?
                }
                Operation::Shading(name) => {
                    self.paint(&mut st, false, false, &f, stack)?;
                    if let Some(s) = self.resource(&f.res, "Shading", name) {
                        self.shading(&s, &f.res)?;
                    }
                }
                Operation::Do(name) => {
                    let Some(x) = self.resource(&f.res, "XObject", name) else {
                        continue;
                    };
                    let Object::Ref(r) = x else {
                        self.undecided("an XObject entry that is not a reference".into());
                        continue;
                    };
                    let Object::Stream { dict, .. } = doc.resolve(r)? else {
                        self.undecided(format!("XObject {r} is not a stream"));
                        continue;
                    };
                    match dict.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) {
                        Some(b"Image") => {
                            // A stencil mask paints with the fill colour.
                            let stencil =
                                dict.get(b"ImageMask".as_slice()).and_then(|b| b.as_bool())
                                    == Some(true);
                            self.paint(&mut st, stencil, false, &f, stack)?;
                            self.image(r, &dict, &f.res)?
                        }
                        Some(b"Form") => {
                            if let Some(nf) =
                                self.enter_form(r, Use::Form, &f.res, f.res_id, f.depth, &st)?
                            {
                                stack.push(nf);
                            }
                        }
                        _ => {
                            self.mark(r, Use::Form);
                            self.undecided(format!("XObject {r} is of no known kind"));
                        }
                    }
                }
                Operation::InlineImage { dict, .. } => {
                    let stencil = dict
                        .get(b"IM".as_slice())
                        .or_else(|| dict.get(b"ImageMask".as_slice()))
                        .and_then(|b| b.as_bool())
                        == Some(true);
                    self.paint(&mut st, stencil, false, &f, stack)?;
                    let cs = dict
                        .get(b"CS".as_slice())
                        .or_else(|| dict.get(b"ColorSpace".as_slice()));
                    match cs {
                        Some(Object::Name(n))
                            if !is_device_space(n) && !is_inline_abbreviation(n) =>
                        {
                            if let Some(cs) = self.resource(&f.res, "ColorSpace", n) {
                                self.color_space(&cs, &f.res)?;
                            }
                        }
                        Some(o @ Object::Array(_)) => self.color_space(o, &f.res)?,
                        _ => {}
                    }
                }
                Operation::ShowText(b) | Operation::NextLineShowText(b) => {
                    self.show_painted(&mut st, b, &f, stack)?
                }
                Operation::SetSpacingNextLineShowText { text, .. } => {
                    self.show_painted(&mut st, text, &f, stack)?
                }
                Operation::ShowTextAdjusted(items) => {
                    for i in items {
                        if let TJItem::Text(b) = i {
                            self.show_painted(&mut st, b, &f, stack)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Text shown: painted with the fill and/or stroke colour as the
    /// rendering mode says (modes 3 and 7 paint nothing), and a Type 3
    /// font's procedures run.
    fn show_painted(
        &mut self,
        st: &mut State,
        bytes: &[u8],
        f: &Frame,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let (fill, stroke) = match st.render {
            0 | 4 => (true, false),
            1 | 5 => (false, true),
            2 | 6 => (true, true),
            _ => (false, false),
        };
        if fill || stroke {
            self.paint(st, fill, stroke, f, stack)?;
        }
        let font = st.font.clone();
        self.show(&font, st, bytes, f, stack)
    }

    /// Something is painted: what the colours painted with and the
    /// graphics state in force read becomes used.
    fn paint(
        &mut self,
        st: &mut State,
        fill: bool,
        stroke: bool,
        f: &Frame,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        // Realized once: what they read is used from now on (marks do
        // not repeat), and a long run of `gs` stays linear.
        for (g, res, res_id) in std::mem::take(&mut st.gs) {
            self.ext_g_state(&g, &res, res_id, f.depth, stack)?;
        }
        for (on, p) in [(fill, st.fill.clone()), (stroke, st.stroke.clone())] {
            if !on {
                continue;
            }
            if let Some((cs, res)) = &p.space {
                self.color_space(cs, res)?;
            }
            if let Some(pat) = &p.pattern {
                self.pattern(pat, f, stack)?;
            }
        }
        Ok(())
    }

    /// The font object `o` (a resource entry) selected: its data marked;
    /// the Type 3 font in force after it, if it is one.
    fn select_font(&mut self, o: &Object) -> Result<Option<Type3>, Error> {
        let doc = self.doc;
        let id = o.as_ref();
        if let Some(r) = id {
            if let Some(t3) = self.loaded.get(&r) {
                let t3 = t3.clone();
                if self.seen_fonts.insert(r) {
                    let d = doc.resolve(r)?;
                    if let Some(d) = d.as_dict() {
                        self.font_data(d, 0)?;
                    }
                }
                return Ok(t3);
            }
        }
        let d = match doc.deref(o)? {
            Object::Dict(d) => d,
            _ => {
                self.undecided("a font entry that is not a dictionary".into());
                return Ok(None);
            }
        };
        if let Some(r) = id {
            self.seen_fonts.insert(r);
        }
        self.font_data(&d, 0)?;
        let t3 = if d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Type3") {
            match crate::font::load_font_dict(doc, &d, b"F", self.limits) {
                Ok(f) => f.as_type3().map(|t| Type3 {
                    id,
                    font: t.clone(),
                }),
                Err(e @ Error::LimitExceeded { .. }) => return Err(e),
                Err(e) => {
                    self.undecided(format!("a Type 3 font that does not load: {e}"));
                    None
                }
            }
        } else {
            None
        };
        if let Some(r) = id {
            self.loaded.insert(r, t3.clone());
        }
        Ok(t3)
    }

    /// A font dictionary's program and code data (not a Type 3 font's
    /// procedures: those are marked per glyph shown).
    fn font_data(&mut self, d: &Dict, depth: u32) -> Result<(), Error> {
        let doc = self.doc;
        if depth > 4 {
            return Ok(());
        }
        for k in [b"ToUnicode".as_slice(), b"CIDToGIDMap", b"CIDSet"] {
            if let Some(v) = d.get(k) {
                self.mark_stream(v, Use::FontData)?;
            }
        }
        if let Some(e) = d.get(b"Encoding".as_slice()) {
            if self.mark_stream(e, Use::FontData)? {
                if let Object::Stream { dict, .. } = doc.deref(e)? {
                    if let Some(u) = dict.get(b"UseCMap".as_slice()) {
                        self.mark_stream(u, Use::FontData)?;
                    }
                }
            }
        }
        if let Some(fd) = d.get(b"FontDescriptor".as_slice()) {
            if let Some(fd) = doc.deref(fd)?.as_dict() {
                for k in [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"] {
                    if let Some(v) = fd.get(k) {
                        self.mark_stream(v, Use::FontProgram)?;
                    }
                }
                if let Some(v) = fd.get(b"CIDSet".as_slice()) {
                    self.mark_stream(v, Use::FontData)?;
                }
            }
        }
        if let Some(df) = d.get(b"DescendantFonts".as_slice()) {
            for f in doc.deref(df)?.as_array().unwrap_or(&[]) {
                if let Some(fd) = doc.deref(f)?.as_dict() {
                    self.font_data(fd, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// Text shown in `font`: a Type 3 font's procedures for the glyphs
    /// shown, and what they draw.
    fn show(
        &mut self,
        font: &Option<Type3>,
        st: &State,
        bytes: &[u8],
        f: &Frame,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let Some(t3) = font else { return Ok(()) };
        // A Type 3 font without `/Resources` resolves names in the
        // resources in force where it is used.
        let (res, res_id) = match t3.font.resources() {
            Some(r) => (r.clone(), t3.id.map_or(f.res_id, ResId::Own)),
            None => (f.res.clone(), f.res_id),
        };
        let mut procs: BTreeSet<IndirectRef> = BTreeSet::new();
        for b in bytes {
            match t3.font.charproc_entry(*b) {
                Some(Object::Ref(r)) => {
                    procs.insert(*r);
                }
                Some(_) => self.undecided("a glyph procedure that is not a reference".into()),
                None => {}
            }
        }
        for r in procs {
            if let Some(nf) = self.enter_form(r, Use::CharProc, &res, res_id, f.depth, st)? {
                stack.push(nf);
            }
        }
        Ok(())
    }

    /// An ExtGState in force over something painted: its soft mask
    /// group (run), transfer functions, halftone.
    fn ext_g_state(
        &mut self,
        g: &Object,
        res: &Object,
        res_id: ResId,
        depth: usize,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let doc = self.doc;
        let Some(g) = g.as_dict() else {
            self.undecided("an ExtGState that is not a dictionary".into());
            return Ok(());
        };
        if let Some(sm) = g.get(b"SMask".as_slice()) {
            let sm = doc.deref(sm)?;
            if let Some(sd) = sm.as_dict() {
                if let Some(Object::Ref(gr)) = sd.get(b"G".as_slice()) {
                    let none = State::default();
                    if let Some(nf) =
                        self.enter_form(*gr, Use::SoftMaskGroup, res, res_id, depth, &none)?
                    {
                        stack.push(nf);
                    }
                }
                if let Some(tr) = sd.get(b"TR".as_slice()) {
                    self.function(tr, 0)?;
                }
            }
        }
        for k in [b"TR".as_slice(), b"TR2", b"BG", b"BG2", b"UCR", b"UCR2"] {
            if let Some(v) = g.get(k) {
                self.function(v, 0)?;
            }
        }
        if let Some(ht) = g.get(b"HT".as_slice()) {
            self.halftone(ht, 0)?;
        }
        Ok(())
    }

    fn halftone(&mut self, ht: &Object, depth: u32) -> Result<(), Error> {
        if depth > 4 {
            return Ok(());
        }
        let doc = self.doc;
        if self.mark_stream(ht, Use::Halftone)? {
            if let Object::Stream { dict, .. } = doc.deref(ht)? {
                if let Some(t) = dict.get(b"TransferFunction".as_slice()) {
                    self.function(t, 0)?;
                }
            }
            return Ok(());
        }
        let ht = doc.deref(ht)?;
        if let Some(d) = ht.as_dict() {
            for (k, v) in d {
                if k.as_slice() == b"TransferFunction" {
                    self.function(v, 0)?;
                } else if matches!(v, Object::Ref(_) | Object::Dict(_)) && k.as_slice() != b"Type" {
                    // A type 5 halftone's component halftones.
                    self.halftone(v, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    fn function(&mut self, o: &Object, depth: u32) -> Result<(), Error> {
        if depth > 16 {
            return Err(Error::LimitExceeded {
                limit: "function nesting (usage)",
                max: 16,
            });
        }
        let doc = self.doc;
        self.mark_stream(o, Use::Function)?;
        match doc.deref(o)? {
            Object::Array(a) => {
                for x in &a {
                    self.function(x, depth + 1)?;
                }
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                if let Some(fs) = d.get(b"Functions".as_slice()) {
                    self.function(fs, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn shading(&mut self, s: &Object, res: &Object) -> Result<(), Error> {
        let doc = self.doc;
        self.mark_stream(s, Use::Shading)?;
        let s = doc.deref(s)?;
        let d = match &s {
            Object::Dict(d) | Object::Stream { dict: d, .. } => d,
            _ => {
                self.undecided("a shading that is not a dictionary".into());
                return Ok(());
            }
        };
        if let Some(f) = d.get(b"Function".as_slice()) {
            self.function(f, 0)?;
        }
        if let Some(cs) = d.get(b"ColorSpace".as_slice()) {
            self.color_space(cs, res)?;
        }
        Ok(())
    }

    fn pattern(&mut self, p: &Object, f: &Frame, stack: &mut Vec<Frame>) -> Result<(), Error> {
        let doc = self.doc;
        let Object::Ref(r) = p else {
            self.undecided("a pattern entry that is not a reference".into());
            return Ok(());
        };
        match doc.resolve(*r)? {
            Object::Stream { .. } => {
                // A tiling pattern: its cell, with its own resources (else
                // those in force, as readers resolve them).
                if let Some(nf) = self.enter_form(
                    *r,
                    Use::Pattern,
                    &f.res,
                    f.res_id,
                    f.depth,
                    &State::default(),
                )? {
                    stack.push(nf);
                }
            }
            Object::Dict(d) => {
                if let Some(s) = d.get(b"Shading".as_slice()) {
                    self.shading(s, &f.res)?;
                }
                if let Some(gs) = d.get(b"ExtGState".as_slice()) {
                    let gs = doc.deref(gs)?;
                    self.ext_g_state(&gs, &f.res, f.res_id, f.depth, stack)?;
                }
            }
            _ => self.undecided(format!("pattern {r} is neither a stream nor a dictionary")),
        }
        Ok(())
    }

    fn image(&mut self, r: IndirectRef, dict: &Dict, res: &Object) -> Result<(), Error> {
        let doc = self.doc;
        self.mark(r, Use::Image);
        for k in [b"SMask".as_slice(), b"Mask"] {
            if let Some(m) = dict.get(k) {
                if self.mark_stream(m, Use::Mask)? {
                    if let Object::Stream { dict: md, .. } = doc.deref(m)? {
                        if let Some(cs) = md.get(b"ColorSpace".as_slice()) {
                            self.color_space(cs, res)?;
                        }
                    }
                }
            }
        }
        if let Some(cs) = dict.get(b"ColorSpace".as_slice()) {
            self.color_space(cs, res)?;
        }
        if let Some(alts) = dict.get(b"Alternates".as_slice()) {
            for a in doc.deref(alts)?.as_array().unwrap_or(&[]) {
                if let Some(i) = doc
                    .deref(a)?
                    .as_dict()
                    .and_then(|d| d.get(b"Image".as_slice()))
                {
                    self.mark_stream(i, Use::Image)?;
                }
            }
        }
        Ok(())
    }

    fn color_space(&mut self, cs: &Object, res: &Object) -> Result<(), Error> {
        self.color_space_at(cs, res, 0)
    }

    fn color_space_at(&mut self, cs: &Object, res: &Object, nest: u32) -> Result<(), Error> {
        if nest > 8 {
            return Err(Error::LimitExceeded {
                limit: "colour space nesting (usage)",
                max: 8,
            });
        }
        let doc = self.doc;
        let a = match doc.deref(cs)? {
            Object::Array(a) => a,
            Object::Name(n) => {
                // A named space in a nested position (an /Indexed base):
                // a resource, unless a family name.
                if !is_device_space(&n) && nest > 0 {
                    if let Some(c) = self.resource(res, "ColorSpace", &n) {
                        return self.color_space_at(&c, res, nest + 1);
                    }
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        let family = a.first().and_then(|f| f.as_name()).unwrap_or(b"");
        match family {
            b"ICCBased" => {
                if let Some(s) = a.get(1) {
                    if self.mark_stream(s, Use::IccProfile)? {
                        if let Object::Stream { dict, .. } = doc.deref(s)? {
                            if let Some(alt) = dict.get(b"Alternate".as_slice()) {
                                self.color_space_at(alt, res, nest + 1)?;
                            }
                        }
                    }
                }
            }
            b"Indexed" | b"I" => {
                if let Some(b) = a.get(1) {
                    self.color_space_at(b, res, nest + 1)?;
                }
                if let Some(l) = a.get(3) {
                    self.mark_stream(l, Use::ColorSpaceData)?;
                }
            }
            b"Separation" => {
                if let Some(alt) = a.get(2) {
                    self.color_space_at(alt, res, nest + 1)?;
                }
                if let Some(t) = a.get(3) {
                    self.function(t, 0)?;
                }
            }
            b"DeviceN" => {
                if let Some(alt) = a.get(2) {
                    self.color_space_at(alt, res, nest + 1)?;
                }
                if let Some(t) = a.get(3) {
                    self.function(t, 0)?;
                }
                if let Some(attrs) = a.get(4) {
                    let attrs = doc.deref(attrs)?;
                    if let Some(ad) = attrs.as_dict() {
                        if let Some(c) = ad.get(b"Colorants".as_slice()) {
                            let c = doc.deref(c)?;
                            for v in c
                                .as_dict()
                                .map(|d| d.values().cloned().collect::<Vec<_>>())
                                .unwrap_or_default()
                            {
                                self.color_space_at(&v, res, nest + 1)?;
                            }
                        }
                        if let Some(p) = ad.get(b"Process".as_slice()) {
                            let p = doc.deref(p)?;
                            if let Some(pcs) =
                                p.as_dict().and_then(|d| d.get(b"ColorSpace".as_slice()))
                            {
                                self.color_space_at(pcs, res, nest + 1)?;
                            }
                        }
                    }
                }
            }
            b"Pattern" => {
                if let Some(b) = a.get(1) {
                    self.color_space_at(b, res, nest + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// [`Bound::Over`] on an undecidable page: every stream the page
    /// reaches — its dictionary (but `/Parent`) and its resources, by
    /// any key — counts as possibly used.
    fn mark_reachable(&mut self, page: &crate::page::Page) -> Result<(), Error> {
        let doc = self.doc;
        let mut stack: Vec<Object> = Vec::new();
        if let Some(r) = page.object_ref() {
            if let Object::Dict(d) = doc.resolve(r)? {
                stack.extend(
                    d.iter()
                        .filter(|(k, _)| k.as_slice() != b"Parent")
                        .map(|(_, v)| v.clone()),
                );
            }
        }
        if let Some(res) = page.resources() {
            stack.push(res.clone());
        }
        let mut seen: HashSet<IndirectRef> = HashSet::new();
        let mut n: u64 = 0;
        while let Some(o) = stack.pop() {
            n += 1;
            if n > self.limits.max_audit_elements {
                return Err(Error::LimitExceeded {
                    limit: "max_audit_elements",
                    max: self.limits.max_audit_elements,
                });
            }
            match o {
                Object::Ref(r) => {
                    if !seen.insert(r) {
                        continue;
                    }
                    let t = doc.resolve(r)?;
                    if matches!(t, Object::Stream { .. }) {
                        self.mark(r, Use::Undecided);
                    }
                    // Back links lead to the whole document.
                    match t {
                        Object::Dict(d) | Object::Stream { dict: d, .. } => stack.extend(
                            d.into_iter()
                                .filter(|(k, _)| !matches!(k.as_slice(), b"Parent" | b"P"))
                                .map(|(_, v)| v),
                        ),
                        Object::Array(a) => stack.extend(a),
                        _ => {}
                    }
                }
                Object::Dict(d) => stack.extend(
                    d.into_iter()
                        .filter(|(k, _)| !matches!(k.as_slice(), b"Parent" | b"P"))
                        .map(|(_, v)| v),
                ),
                Object::Array(a) => stack.extend(a),
                _ => {}
            }
        }
        Ok(())
    }
}

fn is_device_space(n: &[u8]) -> bool {
    matches!(
        n,
        b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Pattern" | b"G" | b"RGB" | b"CMYK"
    )
}

/// Inline image abbreviations of families (§8.9.7 Table 91).
fn is_inline_abbreviation(n: &[u8]) -> bool {
    matches!(n, b"I" | b"Indexed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = std::collections::BTreeMap::new();
        for (n, b) in objs {
            offs.insert(*n, out.len());
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(b);
            out.extend_from_slice(b"\nendobj\n");
        }
        let size = offs.keys().max().unwrap() + 1;
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for i in 1..size {
            match offs.get(&i) {
                Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
            }
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes(),
        );
        out
    }

    fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    }

    /// Forms, images and masks drawn, a pattern cell, a soft mask group,
    /// an ICC profile and a shading function are used; a named but
    /// undrawn image, an unknown entry, and what is selected but never
    /// painted with are not.
    #[test]
    fn what_drawing_reads() {
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /XYZ 13 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
                  /Resources << /XObject << /Fm 5 0 R /Un 12 0 R >> /Pattern << /P 8 0 R /Q 16 0 R >> \
                  /ExtGState << /G << /SMask << /S /Luminosity /G 9 0 R >> >> \
                  /Un2 << /TR 15 0 R >> >> \
                  /ColorSpace << /C [/ICCBased 10 0 R] >> /Shading << /S 11 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream("", b"/Fm Do /Pattern cs /P scn 0 0 1 1 re f /G gs /C cs 1 sc 0 0 1 1 re f /S sh /Pattern cs /Q scn 0 g 0 0 1 1 re f /Un2 gs")),
            (5, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Im 6 0 R >> >>", b"/Im Do")),
            (6, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 7 0 R", b"\x00")),
            (7, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x00")),
            (8, stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << >>", b"0 0 1 1 re f")),
            (9, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Group << /S /Transparency >>", b"0 0 1 1 re f")),
            (10, stream("/N 1", b"icc")),
            (11, b"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 0] /Function 14 0 R >>".to_vec()),
            (12, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x01")),
            (13, stream("", b"private")),
            (14, stream("/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8", b"\x00\xff")),
            (15, stream("/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8", b"\x01\xfe")),
            (16, stream("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << >>", b"0 0 0.5 1 re f")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        let role = |n: u32| {
            u.used
                .get(&IndirectRef::new(n, 0))
                .map(|(r, _)| r.iter().copied().collect::<Vec<_>>())
        };
        assert_eq!(role(4), Some(vec![Use::Contents]));
        assert_eq!(role(5), Some(vec![Use::Form]));
        assert_eq!(role(6), Some(vec![Use::Image]));
        assert_eq!(role(7), Some(vec![Use::Mask]));
        assert_eq!(role(8), Some(vec![Use::Pattern]));
        assert_eq!(role(9), Some(vec![Use::SoftMaskGroup]));
        assert_eq!(role(10), Some(vec![Use::IccProfile]));
        assert_eq!(role(14), Some(vec![Use::Function]));
        assert_eq!(role(12), None, "named, never drawn");
        assert_eq!(role(13), None, "an unknown entry");
        // Selected, never painted with: the transfer function of an
        // ExtGState set last (the shading painted before it), a pattern
        // replaced by a grey before the fill.
        assert_eq!(role(15), None, "a graphics state over nothing");
        assert_eq!(role(16), None, "a pattern never painted with");
        assert!(u.undecided.is_empty(), "{:?}", u.undecided);
    }

    /// A name that resolves to nothing: undecided; over the input, the
    /// page's streams all count as possibly used.
    #[test]
    fn undecided_pages() {
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
                  /Resources << /XObject << /Un 5 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream("", b"/Missing Do")),
            (5, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x01")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let under = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert_eq!(under.undecided.len(), 1);
        assert!(!under.used.contains_key(&IndirectRef::new(5, 0)));
        let over = usage(&doc, &|_| true, Bound::Over, &Limits::default()).unwrap();
        assert!(over.used.contains_key(&IndirectRef::new(5, 0)));
    }
}
