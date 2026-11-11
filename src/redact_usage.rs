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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

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

    /// Whether the role's bytes are a content stream (operators), where
    /// whitespace means nothing; elsewhere every byte may be data.
    pub(crate) fn content_language(self) -> bool {
        matches!(
            self,
            Use::Contents
                | Use::Appearance
                | Use::Form
                | Use::CharProc
                | Use::Pattern
                | Use::SoftMaskGroup
        )
    }
}

/// How an undecidable point is counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// Everything the page reaches counts as possibly used.
    Over,
    /// Everything a reader may draw counts — every appearance state, a
    /// Hidden annotation, optional content — but an undecidable point
    /// adds nothing (the non-interference comparison, both sides).
    Complete,
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
    res: Rc<Object>,
    res_id: ResId,
    depth: usize,
    /// The state in force where the stream is invoked (a form
    /// inherits the graphics and text state).
    state: State,
}

/// What painting would read, held until something is painted: a colour
/// selected but never painted with, or a soft mask in force over
/// nothing, is not used.
///
/// Each parameter is a slot holding its latest value (a later `gs`
/// replaces an earlier one's), and every value keeps the resources it
/// was selected in: a form invoked under the state reads it from there,
/// not from its own resources. [`State::key`] is an exact, comparable
/// form of the state (no hash decides identity).
#[derive(Clone, Default)]
struct State {
    /// The Type 3 font in force.
    font: Option<Type3>,
    fill: Paint,
    stroke: Paint,
    /// The ExtGState that set the soft mask in force, until something
    /// is painted under it; `/SMask /None` ends it.
    smask: Option<Scoped>,
    /// Transfer function, black generation, undercolour removal and
    /// halftone in force and not yet painted under: the slot's name and
    /// value (`/TR2` replaces `/TR`, and so on, §8.4.5 Table 57).
    params: BTreeMap<&'static [u8], (Rc<Object>, Canon)>,
    /// The text rendering mode (`Tr`).
    render: i64,
}

/// A resource value with the resources it was selected in, and the
/// canonical form of what identifies it in a [`StateKey`].
#[derive(Clone)]
struct Scoped {
    /// Shared: a state is cloned for every frame it is inherited by.
    value: Rc<Object>,
    res: Rc<Object>,
    res_id: ResId,
    canon: Canon,
}

impl Scoped {
    fn key(&self) -> (Canon, ResId) {
        (self.canon.clone(), self.res_id)
    }
}

/// Canonical bytes ([`canon`]), interned per walk: equal bytes are one
/// allocation, so two are equal exactly when they are the same
/// allocation (compared and hashed by address, in constant time; the
/// intern table compares the bytes themselves).
#[derive(Clone)]
struct Canon(Rc<[u8]>);

impl PartialEq for Canon {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}

impl Eq for Canon {}

impl std::hash::Hash for Canon {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        (Rc::as_ptr(&self.0) as *const u8 as usize).hash(h)
    }
}

/// A colour in force: its colour space (a resource value) and, for a
/// pattern colour, the pattern (a resource entry); for a device colour,
/// the `Default*` space that would replace it (`/DefaultGray`,
/// `/DefaultRGB`, `/DefaultCMYK`, §8.6.5.6) — each with the resources
/// it was selected in.
#[derive(Clone, Default)]
struct Paint {
    space: Option<Scoped>,
    pattern: Option<Scoped>,
    device: Option<(&'static [u8], Rc<Object>, ResId)>,
}

/// The exact identity of a [`State`] for the form memo: canonical bytes
/// of every value in force, with the identity of the resources each was
/// selected in.
#[derive(PartialEq, Eq, Hash)]
struct StateKey {
    font: Option<FontKey>,
    paints: [PaintKey; 2],
    smask: Option<(Canon, ResId)>,
    params: Vec<(&'static [u8], Canon)>,
    render: i64,
}

type PaintKey = (
    Option<(Canon, ResId)>,
    Option<(Canon, ResId)>,
    Option<(&'static [u8], ResId)>,
);

/// A Type 3 font's identity: its reference, or (a direct dictionary)
/// its canonical bytes — which include its own `/Resources`.
#[derive(Clone, PartialEq, Eq, Hash)]
enum FontKey {
    Ref(IndirectRef),
    Direct(Canon),
}

impl State {
    fn key(&self) -> StateKey {
        let paint = |p: &Paint| -> PaintKey {
            (
                p.space.as_ref().map(Scoped::key),
                p.pattern.as_ref().map(Scoped::key),
                p.device.as_ref().map(|(n, _, id)| (*n, *id)),
            )
        };
        StateKey {
            font: self.font.as_ref().map(|t| t.key.clone()),
            paints: [paint(&self.fill), paint(&self.stroke)],
            smask: self.smask.as_ref().map(Scoped::key),
            params: self
                .params
                .iter()
                .map(|(k, (_, c))| (*k, c.clone()))
                .collect(),
            render: self.render,
        }
    }
}

/// Whether annotation `ad`'s flags let a reader show it: `/F` an integer
/// without Hidden (2) or NoView (32). A `/F` of another type (a real,
/// which §12.5.3 does not allow and readers read differently) counts as
/// not shown.
fn flags_shown(doc: &Document, ad: &Dict) -> Result<bool, Error> {
    let flags = match ad.get(b"F".as_slice()).map(|f| doc.deref(f)).transpose()? {
        None => 0,
        Some(Object::Integer(i)) => i,
        Some(_) => return Ok(false),
    };
    Ok(flags & (2 | 32) == 0)
}

/// Which optional content groups the document's default configuration
/// shows (`/OCProperties /D`, §8.11.4.3): `/BaseState` (`/ON` when
/// absent) with its `/ON` and `/OFF` exceptions.
struct OcDefault {
    base_on: bool,
    /// The configuration's `/Intent` is viewing (absent: `/View`).
    view_intent: bool,
    on: HashSet<IndirectRef>,
    off: HashSet<IndirectRef>,
}

impl OcDefault {
    fn of(doc: &Document) -> Result<OcDefault, Error> {
        let mut oc = OcDefault {
            base_on: true,
            view_intent: true,
            on: HashSet::new(),
            off: HashSet::new(),
        };
        let Some(p) = doc.catalog().get(b"OCProperties".as_slice()) else {
            return Ok(oc);
        };
        let p = doc.deref(p)?;
        let Some(d) = p.as_dict().and_then(|p| p.get(b"D".as_slice())) else {
            return Ok(oc);
        };
        let d = doc.deref(d)?;
        let Some(d) = d.as_dict() else { return Ok(oc) };
        if let Some(b) = d.get(b"BaseState".as_slice()) {
            oc.base_on = doc.deref(b)?.as_name() == Some(b"ON");
        }
        if let Some(i) = d.get(b"Intent".as_slice()) {
            oc.view_intent = match doc.deref(i)? {
                Object::Name(n) => n.as_slice() == b"View",
                Object::Array(a) => a.len() == 1 && a[0].as_name() == Some(b"View"),
                _ => false,
            };
        }
        for (k, set) in [(b"ON".as_slice(), &mut oc.on), (b"OFF", &mut oc.off)] {
            if let Some(a) = d.get(k) {
                for x in doc.deref(a)?.as_array().unwrap_or(&[]) {
                    if let Object::Ref(r) = x {
                        set.insert(*r);
                    }
                }
            }
        }
        // Groups an automatic state (`/AS`, §8.11.4.4) may switch by
        // their usage: readers differ (MuPDF applies it, Poppler does
        // not), so they count as off.
        if let Some(a) = d.get(b"AS".as_slice()) {
            for e in doc.deref(a)?.as_array().unwrap_or(&[]) {
                let e = doc.deref(e)?;
                let Some(g) = e.as_dict().and_then(|e| e.get(b"OCGs".as_slice())) else {
                    continue;
                };
                for x in doc.deref(g)?.as_array().unwrap_or(&[]) {
                    if let Object::Ref(r) = x {
                        oc.off.insert(*r);
                        oc.on.remove(r);
                    }
                }
            }
        }
        Ok(oc)
    }

    /// Whether content under `oc` (an `/OC` value, a `BDC /OC`
    /// properties entry) is shown by default: only a group, by
    /// reference, that the default configuration turns on and no
    /// automatic state names. A membership dictionary or
    /// anything else counts as hidden.
    fn shows(&self, doc: &Document, oc: &Object) -> Result<bool, Error> {
        let Object::Ref(r) = oc else { return Ok(false) };
        let g = doc.resolve(*r)?;
        let is_group = g
            .as_dict()
            .is_some_and(|d| d.get(b"Type".as_slice()).and_then(|t| t.as_name()) == Some(b"OCG"));
        // `/OFF` applies after `/ON` (as mutool, Poppler and pdf.js
        // read a group in both); a group whose usage turns it off for
        // viewing, printing or export, or a configuration for an intent
        // other than viewing, counts as off (readers differ on them).
        let usage_off = g
            .as_dict()
            .and_then(|d| d.get(b"Usage".as_slice()))
            .map(|u| usage_turns_off(doc, u))
            .transpose()?
            .unwrap_or(false);
        Ok(is_group
            && self.view_intent
            && !usage_off
            && (self.base_on || self.on.contains(r))
            && !self.off.contains(r))
    }
}

/// Whether a group's `/Usage` (§8.11.4.4, Table 102) sets a view, print
/// or export state OFF, or is not a dictionary.
fn usage_turns_off(doc: &Document, u: &Object) -> Result<bool, Error> {
    let u = doc.deref(u)?;
    let Some(u) = u.as_dict() else {
        return Ok(true);
    };
    for (k, state) in [
        (b"View".as_slice(), b"ViewState".as_slice()),
        (b"Print", b"PrintState"),
        (b"Export", b"ExportState"),
    ] {
        if let Some(v) = u.get(k) {
            let v = doc.deref(v)?;
            let st = v
                .as_dict()
                .and_then(|d| d.get(state))
                .map(|s| doc.deref(s))
                .transpose()?;
            if st.as_ref().and_then(|s| s.as_name()) == Some(b"OFF") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The value of field attribute `key` of widget `ad`, its own or
/// inherited through `/Parent` (§12.7.4.1).
fn field_attr(doc: &Document, ad: &Dict, key: &[u8]) -> Result<Option<Object>, Error> {
    let mut cur = ad.clone();
    for _ in 0..32 {
        if let Some(v) = cur.get(key) {
            return Ok(Some(doc.deref(v)?));
        }
        match cur
            .get(b"Parent".as_slice())
            .map(|p| doc.deref(p))
            .transpose()?
        {
            Some(Object::Dict(p)) => cur = p,
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Which appearances of widget `ad` a reader presents as it is used
/// (§12.7.5.2): a button field's that is not read only and has an area —
/// a checkbox's or radio button's normal states (an off state and one
/// on state), a push button's down and rollover appearances. `None`: the
/// annotation shows its selected normal appearance only.
fn interactive_appearances(doc: &Document, ad: &Dict) -> Result<Option<Interactive>, Error> {
    if ad.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) != Some(b"Widget") {
        return Ok(None);
    }
    if field_attr(doc, ad, b"FT")?
        .as_ref()
        .and_then(|f| f.as_name())
        != Some(b"Btn")
    {
        return Ok(None);
    }
    let ff = match field_attr(doc, ad, b"Ff")? {
        None => 0,
        Some(Object::Integer(i)) => i,
        Some(_) => return Ok(None),
    };
    let flags = match ad.get(b"F".as_slice()).map(|f| doc.deref(f)).transpose()? {
        None => 0,
        Some(Object::Integer(i)) => i,
        Some(_) => return Ok(None),
    };
    // Read only (field bit 1, annotation flag 64), or locked (128).
    if ff & 1 != 0 || flags & (64 | 128) != 0 {
        return Ok(None);
    }
    let area = match ad
        .get(b"Rect".as_slice())
        .map(|r| doc.deref(r))
        .transpose()?
    {
        Some(Object::Array(a)) if a.len() == 4 => {
            let n: Vec<f64> = a.iter().filter_map(|x| x.as_number()).collect();
            n.len() == 4 && (n[2] - n[0]).abs() > 0.0 && (n[3] - n[1]).abs() > 0.0
        }
        _ => false,
    };
    if !area {
        return Ok(None);
    }
    if ff & (1 << 16) != 0 {
        // A push button presents its down appearance only with the push
        // highlighting mode (`/H /P`; the default inverts the normal one).
        let push = ad.get(b"H".as_slice()).map(|h| doc.deref(h)).transpose()?;
        let push = push.as_ref().and_then(|h| h.as_name()) == Some(b"P");
        return Ok(Some(Interactive::PushButton { down: push }));
    }
    Ok(Some(Interactive::States))
}

#[derive(Clone, Copy, PartialEq)]
enum Interactive {
    /// A checkbox or radio button: its normal states.
    States,
    /// A push button: its normal and rollover appearances, and its down
    /// one when it highlights by pushing.
    PushButton { down: bool },
}

/// `o` written canonically (references as references, dictionary keys
/// sorted, every item length-prefixed): equal bytes, equal values.
fn canon(o: &Object) -> Vec<u8> {
    fn put(o: &Object, out: &mut Vec<u8>) {
        let bytes = |tag: u8, b: &[u8], out: &mut Vec<u8>| {
            out.push(tag);
            out.extend_from_slice(&(b.len() as u64).to_be_bytes());
            out.extend_from_slice(b);
        };
        match o {
            Object::Ref(r) => {
                out.push(b'R');
                out.extend_from_slice(&r.number.to_be_bytes());
                out.extend_from_slice(&r.generation.to_be_bytes());
            }
            Object::Dict(d) | Object::Stream { dict: d, .. } => {
                if let Object::Stream { data, .. } = o {
                    bytes(b'S', data, out);
                }
                let mut keys: Vec<&Vec<u8>> = d.keys().collect();
                keys.sort();
                out.push(b'<');
                out.extend_from_slice(&(keys.len() as u64).to_be_bytes());
                for k in keys {
                    bytes(b'k', k, out);
                    put(&d[k.as_slice()], out);
                }
            }
            Object::Array(a) => {
                out.push(b'[');
                out.extend_from_slice(&(a.len() as u64).to_be_bytes());
                for v in a {
                    put(v, out);
                }
            }
            Object::Name(n) => bytes(b'n', n, out),
            Object::String(t) => bytes(b's', t, out),
            Object::Integer(i) => {
                out.push(b'i');
                out.extend_from_slice(&i.to_be_bytes());
            }
            Object::Real(f) => {
                out.push(b'f');
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            other => bytes(b'.', format!("{other:?}").as_bytes(), out),
        }
    }
    let mut out = Vec::new();
    put(o, &mut out);
    out
}

/// A device colour painted with, selected under `res`: its `Default*`
/// substitute, if any.
fn device(default: &'static [u8], res: &Rc<Object>, res_id: ResId) -> Paint {
    Paint {
        space: None,
        pattern: None,
        device: Some((default, res.clone(), res_id)),
    }
}

/// Whether `op` draws (or invokes something that may).
fn draws(op: &Operation) -> bool {
    matches!(
        op,
        Operation::Do(_)
            | Operation::InlineImage { .. }
            | Operation::Shading(_)
            | Operation::ShowText(_)
            | Operation::NextLineShowText(_)
            | Operation::SetSpacingNextLineShowText { .. }
            | Operation::ShowTextAdjusted(_)
            | Operation::Fill
            | Operation::FillEvenOdd
            | Operation::Stroke
            | Operation::CloseAndStroke
            | Operation::FillAndStroke
            | Operation::FillAndStrokeEvenOdd
            | Operation::CloseFillAndStroke
            | Operation::CloseFillAndStrokeEvenOdd
    )
}

/// The `Default*` resource a device colour space name is replaced by.
fn default_of(n: &[u8]) -> Option<&'static [u8]> {
    match n {
        b"DeviceGray" | b"G" => Some(b"DefaultGray"),
        b"DeviceRGB" | b"RGB" => Some(b"DefaultRGB"),
        b"DeviceCMYK" | b"CMYK" => Some(b"DefaultCMYK"),
        _ => None,
    }
}

/// The canonical forms a selected resource entry contributes.
#[derive(Clone)]
enum Selected {
    Value(Canon),
    Gs {
        /// `Some(None)`: `/SMask /None`, ending a mask.
        smask: Option<Option<Canon>>,
        params: Vec<(&'static [u8], Rc<Object>, Canon)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ResId {
    /// The page's (inherited) resources.
    Page,
    /// The resources of the stream (or Type 3 font) with this reference.
    Own(IndirectRef),
    /// None (an appearance without `/Resources`).
    Empty,
    /// Resources of a direct Type 3 font, numbered by the font's bytes.
    Anon(u64),
}

/// A Type 3 font in force: its dictionary's reference (or `None` when
/// direct) and the loaded font.
#[derive(Clone)]
struct Type3 {
    key: FontKey,
    font: std::rc::Rc<crate::font::Type3Font>,
}

struct Walk<'a> {
    doc: &'a Document,
    limits: &'a Limits,
    bound: Bound,
    u: Usage,
    page: usize,
    budget: u64,
    seen_frames: HashSet<(IndirectRef, ResId, StateKey)>,
    /// Direct Type 3 fonts: their resources' identity, and the fonts
    /// loaded.
    anon_ids: HashMap<Canon, u64>,
    loaded_direct: HashMap<Canon, Option<Type3>>,
    /// Per page: direct fonts selected, by resources and name.
    direct_fonts: HashMap<(ResId, Vec<u8>), Option<Type3>>,
    /// Canonical bytes, interned for the whole walk.
    interned: HashSet<Rc<[u8]>>,
    /// Per page: what a resource entry selected contributes to a state
    /// key, by resources, category and name (a colour space or pattern:
    /// its canonical value; an ExtGState: its soft mask's and each
    /// parameter slot's).
    selected: HashMap<(ResId, &'static str, Vec<u8>), Selected>,
    oc: OcDefault,
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
    let pages: Vec<usize> = (0..doc.pages().len()).filter(|i| select(*i)).collect();
    usage_of(doc, &pages, bound, limits)
}

/// [`usage`] of the pages `pages` (indices, in order), without visiting
/// the others.
pub(crate) fn usage_of(
    doc: &Document,
    pages: &[usize],
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
        anon_ids: HashMap::new(),
        loaded_direct: HashMap::new(),
        direct_fonts: HashMap::new(),
        interned: HashSet::new(),
        selected: HashMap::new(),
        oc: OcDefault::of(doc)?,
        seen_fonts: HashSet::new(),
        loaded: HashMap::new(),
        undecided_here: false,
    };
    let all = doc.pages();
    for &i in pages {
        let Some(page) = all.get(i) else { continue };
        w.page = i;
        // Each page within the bound on its own: a long document of
        // ordinary pages is not refused for its length.
        w.budget = 0;
        w.seen_frames.clear();
        w.selected.clear();
        w.direct_fonts.clear();
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
        let page_res = Rc::new(res.clone());
        match doc.page_operations(page) {
            Ok(ops) => stack.push(Frame {
                ops,
                res: page_res.clone(),
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
                // Under: only what a reader draws — the normal appearance
                // in the state `/AS` selects, of an annotation neither
                // Hidden nor NoView nor under optional content.
                let under = self.bound == Bound::Under;
                if under {
                    let oc_shown = match ad.get(b"OC".as_slice()) {
                        Some(o) => self.oc.shows(doc, o)?,
                        None => true,
                    };
                    if !flags_shown(doc, ad)? || !oc_shown {
                        continue;
                    }
                }
                // A widget shows every state and its down and rollover
                // appearances as it is used; other annotations show the
                // normal appearance in the state `/AS` selects.
                let interactive = if under {
                    interactive_appearances(doc, ad)?
                } else {
                    None
                };
                let selected = match ad.get(b"AS".as_slice()).map(|a| doc.deref(a)).transpose()? {
                    Some(Object::Name(n)) => Some(n),
                    _ => None,
                };
                if let Some(ap) = ad.get(b"AP".as_slice()) {
                    let ap = doc.deref(ap)?;
                    let keys: &[&[u8]] = match (under, interactive) {
                        (false, _) | (true, Some(Interactive::PushButton { down: true })) => {
                            &[b"N", b"R", b"D"]
                        }
                        (true, Some(Interactive::PushButton { down: false })) => &[b"N", b"R"],
                        _ => &[b"N"],
                    };
                    for k in keys {
                        let Some(v) = ap.as_dict().and_then(|d| d.get(*k)) else {
                            continue;
                        };
                        // A checkbox's or radio button's normal states:
                        // all of them when they are an off state and one
                        // on state (what clicking it shows).
                        let toggles = |s: &crate::object::Dict| {
                            interactive == Some(Interactive::States)
                                && *k == b"N"
                                && s.len() == 2
                                && s.contains_key(b"Off".as_slice())
                        };
                        let pick = |s: crate::object::Dict| -> Vec<Object> {
                            let push = matches!(interactive, Some(Interactive::PushButton { .. }));
                            if under && !toggles(&s) && !push {
                                selected
                                    .as_ref()
                                    .and_then(|n| s.get(n.as_slice()).cloned())
                                    .into_iter()
                                    .collect()
                            } else {
                                s.values().cloned().collect()
                            }
                        };
                        let states: Vec<Object> = match v {
                            Object::Ref(r) => match doc.resolve(*r)? {
                                Object::Dict(s) => pick(s),
                                _ => vec![v.clone()],
                            },
                            Object::Dict(s) => pick(s.clone()),
                            _ => Vec::new(),
                        };
                        for s in states {
                            if let Object::Ref(r) = s {
                                // An appearance without `/Resources`:
                                // some readers resolve its names in the
                                // page's (mutool, Poppler), others in
                                // none (pdf.js). Over the output to
                                // excuse a copy, none; otherwise the
                                // page's, so what they draw is counted.
                                let (res, res_id) = if self.bound == Bound::Under {
                                    (Rc::new(Object::Dict(Dict::new())), ResId::Empty)
                                } else {
                                    (page_res.clone(), ResId::Page)
                                };
                                if let Some(f) = self.enter_form(
                                    r,
                                    Use::Appearance,
                                    &res,
                                    res_id,
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
                if self.bound != Bound::Under {
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
        res: &Rc<Object>,
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
            Some(own) => (Rc::new(own.clone()), ResId::Own(r)),
        };
        if let Some(g) = dict.get(b"Group".as_slice()) {
            let g = doc.deref(g)?;
            if let Some(cs) = g.as_dict().and_then(|d| d.get(b"CS".as_slice())) {
                self.color_space(cs, &res)?;
            }
        }
        // Once per resources, font and painting state inherited: a form
        // painting with an inherited colour reads that colour's data.
        if !self.seen_frames.insert((r, res_id, state.key())) {
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
        // Under: marked content under optional content (`BDC /OC`) is
        // not counted as drawn (a reader may hide it).
        let mut marked: Vec<bool> = Vec::new();
        let mut hidden = 0usize;
        for op in &f.ops {
            match op {
                Operation::BeginMarkedContent(_) => marked.push(false),
                Operation::BeginMarkedContentWithProps { tag, properties } => {
                    let oc = self.bound == Bound::Under
                        && tag.as_slice() == b"OC"
                        && !self.marked_visible(properties, &f.res)?;
                    hidden += usize::from(oc);
                    marked.push(oc);
                }
                Operation::EndMarkedContent if marked.pop() == Some(true) => hidden -= 1,
                _ => {}
            }
            if hidden > 0 && draws(op) {
                continue;
            }
            match op {
                Operation::SaveState => saved.push(st.clone()),
                Operation::RestoreState => st = saved.pop().unwrap_or_default(),
                Operation::SetTextFont { font: name, .. } => {
                    // A direct font: selected once per page and name (its
                    // dictionary is not read and canonicalised per `Tf`).
                    let k = (f.res_id, name.to_vec());
                    st.font = match self.direct_fonts.get(&k) {
                        Some(t3) => t3.clone(),
                        None => match self.resource(&f.res, "Font", name) {
                            Some(o) => {
                                let t3 = self.select_font(&o)?;
                                if !matches!(o, Object::Ref(_)) {
                                    self.direct_fonts.insert(k, t3.clone());
                                }
                                t3
                            }
                            None => None,
                        },
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
                    if let Some(d) = g.as_dict() {
                        let k = (f.res_id, "ExtGState", name.to_vec());
                        let sel = match self.selected.get(&k) {
                            Some(s @ Selected::Gs { .. }) => s.clone(),
                            _ => {
                                // A soft mask (`/None` ends one), and each
                                // slot's value; the `2` form wins within
                                // one dictionary.
                                let smask = match d
                                    .get(b"SMask".as_slice())
                                    .map(|m| doc.deref(m))
                                    .transpose()?
                                {
                                    Some(Object::Name(n)) if n.as_slice() == b"None" => Some(None),
                                    Some(v) => Some(Some(self.intern(&v))),
                                    None => None,
                                };
                                let mut params = Vec::new();
                                for (slot, keys) in [
                                    (b"TR".as_slice(), [b"TR2".as_slice(), b"TR"]),
                                    (b"BG", [b"BG2", b"BG"]),
                                    (b"UCR", [b"UCR2", b"UCR"]),
                                    (b"HT", [b"HT", b"HT"]),
                                ] {
                                    if let Some(v) = keys.iter().find_map(|k| d.get(*k)) {
                                        params.push((slot, Rc::new(v.clone()), self.intern(v)));
                                    }
                                }
                                let s = Selected::Gs { smask, params };
                                self.selected.insert(k, s.clone());
                                s
                            }
                        };
                        if let Selected::Gs { smask, params } = sel {
                            match smask {
                                Some(None) => st.smask = None,
                                Some(Some(c)) => {
                                    st.smask = Some(Scoped {
                                        value: Rc::new(g.clone()),
                                        res: f.res.clone(),
                                        res_id: f.res_id,
                                        canon: c,
                                    })
                                }
                                None => {}
                            }
                            for (slot, v, c) in params {
                                st.params.insert(slot, (v, c));
                            }
                        }
                    }
                }
                Operation::SetColorSpaceFill(n) | Operation::SetColorSpaceStroke(n) => {
                    let space = if is_device_space(n) {
                        None
                    } else {
                        self.resource(&f.res, "ColorSpace", n).map(|cs| Scoped {
                            canon: self.selected_value(f.res_id, "ColorSpace", n, &cs),
                            value: Rc::new(cs),
                            res: f.res.clone(),
                            res_id: f.res_id,
                        })
                    };
                    let p = Paint {
                        space,
                        pattern: None,
                        device: default_of(n).map(|d| (d, f.res.clone(), f.res_id)),
                    };
                    if matches!(op, Operation::SetColorSpaceFill(_)) {
                        st.fill = p;
                    } else {
                        st.stroke = p;
                    }
                }
                Operation::SetColorFill(v) | Operation::SetColorStroke(v) => {
                    let pattern = match v.last() {
                        Some(Object::Name(n)) => {
                            self.resource(&f.res, "Pattern", n).map(|p| Scoped {
                                canon: self.selected_value(f.res_id, "Pattern", n, &p),
                                value: Rc::new(p),
                                res: f.res.clone(),
                                res_id: f.res_id,
                            })
                        }
                        _ => None,
                    };
                    if matches!(op, Operation::SetColorFill(_)) {
                        st.fill.pattern = pattern;
                    } else {
                        st.stroke.pattern = pattern;
                    }
                }
                Operation::SetGrayFill(_) => st.fill = device(b"DefaultGray", &f.res, f.res_id),
                Operation::SetRgbFill(..) => st.fill = device(b"DefaultRGB", &f.res, f.res_id),
                Operation::SetCmykFill(..) => st.fill = device(b"DefaultCMYK", &f.res, f.res_id),
                Operation::SetGrayStroke(_) => st.stroke = device(b"DefaultGray", &f.res, f.res_id),
                Operation::SetRgbStroke(..) => st.stroke = device(b"DefaultRGB", &f.res, f.res_id),
                Operation::SetCmykStroke(..) => {
                    st.stroke = device(b"DefaultCMYK", &f.res, f.res_id)
                }
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
                    // Under: an XObject under optional content may be hidden.
                    if self.bound == Bound::Under {
                        if let Some(oc) = dict.get(b"OC".as_slice()) {
                            if !self.oc.shows(doc, oc)? {
                                continue;
                            }
                        }
                    }
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
                    if let Some(d) = cs.and_then(|c| c.as_name()).and_then(default_of) {
                        self.default_space(d, &f.res)?;
                    }
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

    /// `o`'s canonical bytes, interned.
    fn intern(&mut self, o: &Object) -> Canon {
        let b = canon(o);
        if let Some(x) = self.interned.get(b.as_slice()) {
            return Canon(x.clone());
        }
        let x: Rc<[u8]> = Rc::from(b);
        self.interned.insert(x.clone());
        Canon(x)
    }

    /// The canonical value of resource `name` in `category` (a colour
    /// space or a pattern) of resources `res_id`, once per page.
    fn selected_value(
        &mut self,
        res_id: ResId,
        category: &'static str,
        name: &[u8],
        value: &Object,
    ) -> Canon {
        let k = (res_id, category, name.to_vec());
        if let Some(Selected::Value(c)) = self.selected.get(&k) {
            return c.clone();
        }
        let c = self.intern(value);
        self.selected.insert(k, Selected::Value(c.clone()));
        c
    }

    /// Whether a `BDC /OC` sequence (its properties operand, a name in
    /// `/Properties` or a direct dictionary) is shown by default.
    fn marked_visible(&mut self, properties: &Object, res: &Object) -> Result<bool, Error> {
        let Object::Name(n) = properties else {
            return Ok(false);
        };
        match self.doc.resolve_resource_raw(res, "Properties", n) {
            Ok(Some(o)) => self.oc.shows(self.doc, &o),
            Ok(None) => Ok(false),
            Err(e @ Error::LimitExceeded { .. }) => Err(e),
            Err(_) => Ok(false),
        }
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
        if let Some(m) = st.smask.take() {
            self.soft_mask(&m.value, &m.res, m.res_id, f.depth, stack)?;
        }
        for (slot, (v, _)) in std::mem::take(&mut st.params) {
            if slot == b"HT" {
                self.halftone(&v, 0)?;
            } else {
                self.function(&v, 0)?;
            }
        }
        for (on, p) in [(fill, st.fill.clone()), (stroke, st.stroke.clone())] {
            if !on {
                continue;
            }
            if let Some(cs) = &p.space {
                self.color_space(&cs.value, &cs.res)?;
            }
            if let Some((d, res, _)) = &p.device {
                self.default_space(d, res)?;
            }
            if let Some(pat) = &p.pattern {
                self.pattern(pat, f.depth, stack)?;
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
        let is_type3 = d.get(b"Subtype".as_slice()).and_then(|s| s.as_name()) == Some(b"Type3");
        // A direct Type 3 font: by its bytes, loaded once.
        let key = match id {
            Some(r) => FontKey::Ref(r),
            None if is_type3 => {
                let c = self.intern(&Object::Dict(d.clone()));
                if let Some(t3) = self.loaded_direct.get(&c) {
                    return Ok(t3.clone());
                }
                FontKey::Direct(c)
            }
            None => return Ok(None),
        };
        let t3 = if is_type3 {
            match crate::font::load_font_dict(doc, &d, b"F", self.limits) {
                Ok(f) => f.as_type3().map(|t| Type3 {
                    key: key.clone(),
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
        match &key {
            FontKey::Ref(r) => {
                self.loaded.insert(*r, t3.clone());
            }
            FontKey::Direct(c) => {
                self.loaded_direct.insert(c.clone(), t3.clone());
            }
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
            Some(r) => (
                Rc::new(r.clone()),
                match &t3.key {
                    FontKey::Ref(id) => ResId::Own(*id),
                    // The same bytes, the same resources.
                    FontKey::Direct(c) => {
                        let next = self.anon_ids.len() as u64;
                        ResId::Anon(*self.anon_ids.entry(c.clone()).or_insert(next))
                    }
                },
            ),
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

    /// The soft mask group of an ExtGState in force over something
    /// painted (run) and its transfer function.
    fn soft_mask(
        &mut self,
        g: &Object,
        res: &Rc<Object>,
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
        Ok(())
    }

    /// The transfer functions and halftone of an ExtGState in force over
    /// something painted.
    fn gs_params(&mut self, g: &Object) -> Result<(), Error> {
        let Some(g) = g.as_dict() else {
            self.undecided("an ExtGState that is not a dictionary".into());
            return Ok(());
        };
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

    /// A device colour painted: the `Default*` space replacing it, if the
    /// resources in force name one.
    fn default_space(&mut self, name: &[u8], res: &Object) -> Result<(), Error> {
        match self.doc.resolve_resource_raw(res, "ColorSpace", name) {
            Ok(Some(cs)) => self.color_space_at(&cs, res, 1),
            Ok(None) => Ok(()),
            Err(e @ Error::LimitExceeded { .. }) => Err(e),
            Err(e) => {
                self.undecided(format!(
                    "/ColorSpace /{}: {e}",
                    String::from_utf8_lossy(name)
                ));
                Ok(())
            }
        }
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

    /// A pattern painted with, resolved (and its cell's names resolved,
    /// when it has no resources) in the resources it was selected in.
    fn pattern(&mut self, p: &Scoped, depth: usize, stack: &mut Vec<Frame>) -> Result<(), Error> {
        let doc = self.doc;
        let (res, res_id) = (&p.res, p.res_id);
        let Object::Ref(r) = &*p.value else {
            self.undecided("a pattern entry that is not a reference".into());
            return Ok(());
        };
        match doc.resolve(*r)? {
            Object::Stream { .. } => {
                // A tiling pattern: its cell, with its own resources (else
                // those in force, as readers resolve them).
                if let Some(nf) =
                    self.enter_form(*r, Use::Pattern, res, res_id, depth, &State::default())?
                {
                    stack.push(nf);
                }
            }
            Object::Dict(d) => {
                if let Some(s) = d.get(b"Shading".as_slice()) {
                    self.shading(s, res)?;
                }
                if let Some(gs) = d.get(b"ExtGState".as_slice()) {
                    let gs = doc.deref(gs)?;
                    self.soft_mask(&gs, res, res_id, depth, stack)?;
                    self.gs_params(&gs)?;
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
            if let Some(d) = cs.as_name().and_then(default_of) {
                self.default_space(d, res)?;
            }
        }
        // A JBIG2 image's shared symbol dictionary is read with it.
        if let Some(p) = dict.get(b"DecodeParms".as_slice()) {
            let p = doc.deref(p)?;
            let parms: Vec<Object> = match p {
                Object::Array(a) => a,
                o => vec![o],
            };
            for x in parms {
                if let Some(g) = doc
                    .deref(&x)?
                    .as_dict()
                    .and_then(|d| d.get(b"JBIG2Globals".as_slice()))
                {
                    self.mark_stream(g, Use::Image)?;
                }
            }
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

    /// A soft mask selected in two resource scopes is walked in each
    /// (its group's names resolve differently), though the form painted
    /// under it is shared. (Review.)
    #[test]
    fn a_soft_mask_keeps_the_resources_it_was_selected_in() {
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /A 5 0 R /B 6 0 R >> >> >>".to_vec()),
            (4, stream("", b"/A Do /B Do")),
            (5, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Common 7 0 R /Im 12 0 R >> /ExtGState << /G 10 0 R >> >>", b"/G gs /Common Do")),
            (6, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Common 7 0 R /Im 13 0 R >> /ExtGState << /G 10 0 R >> >>", b"/G gs /Common Do")),
            (7, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>", b"0 0 1 1 re f")),
            (10, b"<< /SMask << /S /Luminosity /G 11 0 R >> >>".to_vec()),
            (11, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Group << /S /Transparency /CS /DeviceGray >>", b"/Im Do")),
            (12, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x3a")),
            (13, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x7c")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        for bound in [Bound::Under, Bound::Over] {
            let u = usage(&doc, &|_| true, bound, &Limits::default()).unwrap();
            for n in [12, 13] {
                assert!(
                    u.used.contains_key(&IndirectRef::new(n, 0)),
                    "{bound:?}: image {n}"
                );
            }
        }
    }

    /// A device colour selected in the page's resources and painted in a
    /// form without them uses the page's `/DefaultGray`. (Review.)
    #[test]
    fn a_default_space_keeps_the_resources_it_was_selected_in() {
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /Fm 7 0 R >> /ColorSpace << /DefaultGray [/Separation /Ink /DeviceRGB 8 0 R] >> >> >>".to_vec()),
            (4, stream("", b"1 g /Fm Do")),
            (7, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>", b"0 0 1 1 re f")),
            (8, stream("/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [2] /BitsPerSample 8", b"\xff\x00\x00\xff\x00\x00")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert!(u.used.contains_key(&IndirectRef::new(8, 0)));
    }

    /// Forms invoked twice per level under graphics states with the same
    /// effective parameters, and long runs of `gs` and `Do`: the walk
    /// stays within its bound and linear (the state is its latest values,
    /// not a history). (Review.)
    #[test]
    fn graphics_state_runs_stay_linear() {
        let depth = 40u32;
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /Fm 10 0 R >> >> >>".to_vec()),
            (4, stream("", b"/Fm Do")),
        ];
        for i in 0..depth {
            objs.push((
                10 + i,
                stream(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /N {} 0 R >> \
                         /ExtGState << /A << /TR /Identity >> /B << /TR /Identity /SA true >> >> >>",
                        11 + i
                    ),
                    b"q /A gs /N Do Q q /B gs /N Do Q",
                ),
            ));
        }
        objs.push((
            10 + depth,
            stream(
                "/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>",
                b"",
            ),
        ));
        let doc = Document::from_bytes(&pdf(&objs)).unwrap();
        let u = usage(&doc, &|_| true, Bound::Over, &Limits::default()).unwrap();
        assert_eq!(u.used.len() as u32, depth + 2);
        // 20,000 `gs` + `Do` pairs with a transfer function by reference.
        let mut c = Vec::new();
        for _ in 0..20_000 {
            c.extend_from_slice(b"/G gs /X Do ");
        }
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /X 5 0 R >> /ExtGState << /G << /TR 6 0 R /HT /Default >> >> >> >>".to_vec()),
            (4, stream("", &c)),
            (5, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>", b"0 0 1 1 re f")),
            (6, stream("/FunctionType 0 /Domain [0 1] /Range [0 1] /Size [2] /BitsPerSample 8", b"\x00\xff")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let t = std::time::Instant::now();
        let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert!(u.used.contains_key(&IndirectRef::new(6, 0)));
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
    }

    /// A direct Type 3 font with resources, shown many times: its glyph
    /// procedures are walked once (the font is known by its bytes), and a
    /// large direct value in the graphics state costs once, not per
    /// `Do`. (Review.)
    #[test]
    fn repeated_direct_values_are_walked_once() {
        let font = "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 700] /FontMatrix [0.01 0 0 0.01 0 0] \
             /CharProcs << /a 5 0 R >> /Encoding << /Type /Encoding /Differences [97 /a] >> \
             /FirstChar 97 /LastChar 97 /Widths [500] /Resources << /XObject << >> >> >>";
        let mut glyph = b"500 0 d0".to_vec();
        for _ in 0..100 {
            glyph.extend_from_slice(b" 0 0 1 1 re f");
        }
        let mut c = Vec::new();
        for _ in 0..3000 {
            c.extend_from_slice(b"BT /T 10 Tf 0 0 Td (a) Tj ET ");
        }
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Font << /T {font} >> >> >>").into_bytes()),
            (4, stream("", &c)),
            (5, stream("", &glyph)),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert!(u.used.contains_key(&IndirectRef::new(5, 0)));
        // A 1 MiB direct transfer function, then 2,048 `Do`.
        let big = format!(
            "<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 /Private ({}) >>",
            "a".repeat(1 << 20)
        );
        let mut c = b"/G gs ".to_vec();
        for _ in 0..2048 {
            c.extend_from_slice(b"/X Do ");
        }
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /XObject << /X 6 0 R >> /ExtGState << /G << /TR {big} >> >> >> >>").into_bytes()),
            (4, stream("", &c)),
            (6, stream("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >>", b"")),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let t = std::time::Instant::now();
        usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    }

    /// A group whose usage turns it off for viewing (mutool and pdf.js
    /// hide it, Poppler does not) counts as off over the output.
    #[test]
    fn optional_content_usage_off() {
        let bytes = pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [20 0 R] /D << >> >> >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << /L 20 0 R >> /XObject << /Im 9 0 R >> >> >>".to_vec()),
            (4, stream("", b"/OC /L BDC q 20 0 0 20 10 10 cm /Im Do Q EMC")),
            (8, stream("", b"\x01\x02\x03\xf1\xf2\xf3")),
            (9, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceRGB 1 8 0 R]", b"\x00")),
            (20, b"<< /Type /OCG /Name (layer) /Usage << /View << /ViewState /OFF >> >> >>".to_vec()),
        ]);
        let doc = Document::from_bytes(&bytes).unwrap();
        let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
        assert!(!u.used.contains_key(&IndirectRef::new(8, 0)));
    }

    /// Optional content: a group on in the default configuration is
    /// drawn (over the output, too); one off, or named by an automatic
    /// state (`/AS`, which readers apply differently), is not. (Review.)
    #[test]
    fn optional_content_by_the_default_configuration() {
        let make = |d: &str| {
            pdf(&[
                (1, format!("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [20 0 R] /D << {d} >> >> >>").into_bytes()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << /L 20 0 R >> /XObject << /Im 9 0 R >> >> >>".to_vec()),
                (4, stream("", b"/OC /L BDC q 20 0 0 20 10 10 cm /Im Do Q EMC")),
                (8, stream("", b"\x01\x02\x03\xf1\xf2\xf3")),
                (9, stream("/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceRGB 1 8 0 R]", b"\x00")),
                (20, b"<< /Type /OCG /Name (layer) >>".to_vec()),
            ])
        };
        for (d, drawn) in [
            ("", true),
            ("/BaseState /ON", true),
            ("/OFF [20 0 R]", false),
            ("/BaseState /OFF", false),
            ("/BaseState /OFF /ON [20 0 R]", true),
            (
                "/BaseState /ON /AS [<< /Event /View /OCGs [20 0 R] /Category [/View] >>]",
                false,
            ),
            // `/OFF` after `/ON`; an intent other than viewing.
            ("/BaseState /OFF /ON [20 0 R] /OFF [20 0 R]", false),
            ("/Intent /Design", false),
            ("/Intent [/View]", true),
        ] {
            let doc = Document::from_bytes(&make(d)).unwrap();
            let u = usage(&doc, &|_| true, Bound::Under, &Limits::default()).unwrap();
            assert_eq!(u.used.contains_key(&IndirectRef::new(8, 0)), drawn, "{d}");
            // The other bounds count it whatever the configuration.
            for b in [Bound::Over, Bound::Complete] {
                let u = usage(&doc, &|_| true, b, &Limits::default()).unwrap();
                assert!(u.used.contains_key(&IndirectRef::new(8, 0)), "{d} {b:?}");
            }
        }
    }
}
