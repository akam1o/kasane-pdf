//! Pre-redaction diagnosis — **not** a redaction engine and **not** a
//! safety guarantee.
//!
//! `diagnose_redaction` inspects one page's content streams and the
//! resources reachable from them, reporting what a future redaction
//! would have to handle: which characters, images, path paintings and
//! Form XObject invocations overlap the given rectangles, which
//! structures block exact processing, which occurrences cannot be
//! localised, which parts of the document were never examined, and
//! which objects are *shared* so touching them affects other pages or
//! other positions.
//!
//! A clean report means *only* "the scan completed and found no
//! obstacle in the examined scope". It does **not** mean a redaction
//! would succeed, and it never certifies the output is free of the
//! target information.
//!
//! Coordinates are PDF user space, unrotated and un-cropped — the same
//! convention as [`crate::text_extract`] and `inspect`: +x right,
//! +y up, CropBox offsets are not rebased, `/Rotate` is not applied,
//! and there is no DPI involved.

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{IndirectRef, Object};
use crate::page::{Page, Rect};
use std::collections::{BTreeMap, BTreeSet, HashMap};

// -------------------------------------------------------------------
// Public result types
// -------------------------------------------------------------------

/// The kind of a diagnosed occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    /// A character shown by `Tj`/`TJ`/`'`/`"`.
    Text,
    /// An `/Image` XObject `Do` call.
    Image,
    /// A path painted by a fill/stroke op (`re`, `m`/`l`/`c`, `S`, `f`…).
    Path,
    /// A `/Form` XObject `Do` call (the invocation itself — its inner
    /// occurrences are reported separately with a `form_path` prefix).
    Form,
    /// A shading `sh` or other painted entity we cannot enumerate —
    /// reported as a blocker instead of a target when it blocks.
    Other,
}

/// Where inside the logical content stream an occurrence was found.
///
/// `op_index` is the index into `Document::page_operations` — a
/// *logical* index across the concatenated `/Contents` array (the
/// engine already treats the array as one stream, §7.4.1). For a
/// Form-nested occurrence `op_index` is the index inside that form's
/// own stream; `form_path` says which form.
#[derive(Debug, Clone)]
pub struct Site {
    /// Indirect refs of the page's `/Contents` stream(s) — every
    /// element when it is an array, since the logical index does not
    /// identify which element an op came from. Empty for a direct
    /// stream (rare) or a non-stream context.
    pub contents: Vec<IndirectRef>,
    /// Index of the operation inside the parsed op list for the
    /// stream named by `form_path.last()` (or the page's logical
    /// content when `form_path` is empty).
    pub op_index: usize,
    /// `Do` invocation chain that led here — outermost first. Empty at
    /// page level. Each element is `(resource name, resolved ref)`.
    pub form_path: Vec<(Vec<u8>, Option<IndirectRef>)>,
}

/// A diagnosed occurrence with a determined page-space footprint.
#[derive(Debug, Clone)]
pub struct Target {
    /// What kind of paint this is.
    pub kind: TargetKind,
    /// Where in the content it was emitted.
    pub site: Site,
    /// Conservative axis-aligned bounding box in user space
    /// `(llx, lly, urx, ury)`. For images this is the unit square
    /// under the CTM (the *whole* image, not the pixels actually
    /// drawn); for text the glyph outline bbox or the estimated cell;
    /// for paths the path bounds inflated by stroke attributes.
    pub bounds: (f64, f64, f64, f64),
    /// Indices of the input rectangles this bounds intersects
    /// (empty when it does not overlap any).
    pub hits: Vec<usize>,
    /// Kind-specific detail, serialisable for the CLI.
    pub detail: TargetDetail,
}

/// Kind-specific payload of a [`Target`].
#[derive(Debug, Clone)]
pub enum TargetDetail {
    /// A shown character.
    Text {
        /// The raw character code (0–255 simple, 0–65535 composite).
        code: u32,
        /// Resolved Unicode, or `None` when unresolvable — the code is
        /// never lost even when Unicode is.
        unicode: Option<String>,
        /// `/Resources /Font` key this glyph was shown with.
        font: Vec<u8>,
        /// Rendering mode (0–7; 3 = invisible).
        render_mode: u8,
        /// Glyph origin in user space.
        origin: (f64, f64),
        /// The bounds source.
        bounds_kind: TextBounds,
    },
    /// An `/Image` XObject `Do` call.
    Image {
        /// The indirect ref of the image stream, when resolvable.
        xobj: Option<IndirectRef>,
        /// The resource name used for the call.
        name: Vec<u8>,
        /// `/Width` `/Height` in samples.
        size: (i64, i64),
        /// Attributes that block exact processing (`/Mask`, `/SMask`,
        /// `/ImageMask`, `/OC`, unsupported filter/colour space).
        blockers: Vec<String>,
    },
    /// A painted path.
    Path {
        /// The painting op (`S`, `f`, `B`, …).
        op: &'static str,
        /// Whether the path bounds include the stroke outline
        /// inflation (line width / cap / join / CTM scaling).
        stroked: bool,
    },
    /// A `/Form` XObject `Do` invocation.
    Form {
        /// The form stream's ref when resolvable.
        xobj: Option<IndirectRef>,
        /// Resource name used for the call.
        name: Vec<u8>,
        /// The form's `/BBox` mapped into page space.
        bbox: (f64, f64, f64, f64),
        /// The `/Matrix` concatenated for this call.
        matrix: [f64; 6],
    },
    /// Anything painted we cannot enumerate precisely.
    Other {
        /// Description of what was seen.
        what: String,
    },
}

/// Where a text target's bounds came from — the reported rectangle is
/// never silently a "safe" redaction region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBounds {
    /// The glyph's *outline* bbox — ink extent only. Stroke-mode text
    /// (render modes 1/2/5/6) may paint outside this, and spacing
    /// areas are not covered.
    Outline,
    /// Estimated character *cell* — origin + advance + font ascent,
    /// used when the glyph has no outline (space, .notdef) or the
    /// outline could not be computed. An estimate, not an ink extent.
    Cell,
}

/// An occurrence that could not be localised to page space.
#[derive(Debug, Clone)]
pub struct Uncertain {
    /// What was seen.
    pub kind: TargetKind,
    /// Where it was emitted.
    pub site: Site,
    /// Why the position is undetermined.
    pub reason: String,
}

/// A structure that *blocks* exact processing — not merely a missing
/// feature but something whose presence means any redaction would
/// operate on unknown semantics.
#[derive(Debug, Clone)]
pub struct Blocker {
    /// Where it was encountered.
    pub site: Option<Site>,
    /// The construct (`/ActualText`, `/OC`, `unsupported XObject`,
    /// `inline image`, `ExtGState`, `pattern`, `shading`,
    /// `singular CTM`, `UserUnit`, `/Annots`, …).
    pub what: String,
    /// Detail.
    pub detail: String,
}

/// A resource used at more than one site — touching it affects every
/// call site, not just the diagnosed one.
#[derive(Debug, Clone)]
pub struct SharedUse {
    /// The shared object.
    pub obj: IndirectRef,
    /// What kind it is (`Image`, `Form`, `Font`, `ExtGState`, …).
    pub kind: String,
    /// The sites that invoked it (`form_path` distinguishes positions).
    pub sites: Vec<Site>,
    /// Page indices (0-based) other than the diagnosed page where the
    /// same object was found — searched up to a bounded page count,
    /// see [`Diagnosis::shared_scope`].
    pub other_pages: Vec<usize>,
}

/// How far the shared-resource scan reached.
#[derive(Debug, Clone)]
pub struct SharedScope {
    /// Pages actually scanned for cross-page sharing (0-based indices).
    pub pages_scanned: usize,
    /// Total page count in the document.
    pub pages_total: usize,
    /// Whether the scan was truncated by `max_diag_share_pages` or
    /// `max_diag_share_refs` — `true` means *unscanned* pages may still
    /// share objects, so "not reported as shared" is not a guarantee.
    pub truncated: bool,
}

/// A page annotation overlapping (or not) the diagnosis rectangles.
#[derive(Debug, Clone)]
pub struct AnnotInfo {
    /// `/Subtype` name (`Link`, `Square`, `Widget`, …).
    pub subtype: String,
    /// `/Rect` in user space, or `None` when missing/malformed.
    pub rect: Option<(f64, f64, f64, f64)>,
    /// Indices of the input rectangles this `/Rect` intersects.
    /// The Rect is the annotation's outer frame, **not** the extent of
    /// all data it carries (appearance, popups, contents, actions) —
    /// overlapping or non-overlapping says nothing about those.
    pub hits: Vec<usize>,
    /// Whether the annotation dict carries extra payloads
    /// (`/Contents`, `/A`, `/AP`, `/AA`, `/Popup`) a redaction must
    /// also reason about.
    pub extra_keys: Vec<String>,
}

/// Why the scan did not complete.
#[derive(Debug, Clone)]
pub struct Incomplete {
    /// What stopped the walk.
    pub reason: String,
    /// The op index range that was *not* examined (from this index
    /// onward), when known.
    pub from_op: Option<usize>,
}

/// The full diagnosis result.
///
/// Nothing here is a boolean "safe to redact": `targets` lists what
/// *would* be affected, `blockers` what makes exact processing
/// impossible, `uncertain` what could not be localised, `incomplete`
/// what was never scanned, `unexamined` whole document areas never
/// evaluated, and `shared` objects that must not be edited because
/// other sites use them.
#[derive(Debug)]
pub struct Diagnosis {
    /// Validated input rectangles (echoed back).
    pub rects: Vec<Rect>,
    /// Occurrences localised to page space, in content order.
    /// `hits` lists which input rects each overlaps.
    pub targets: Vec<Target>,
    /// Occurrences whose page position could not be determined.
    pub uncertain: Vec<Uncertain>,
    /// Structures that block exact processing, in encounter order.
    pub blockers: Vec<Blocker>,
    /// Objects invoked from more than one site or page.
    pub shared: Vec<SharedUse>,
    /// How far the cross-page sharing scan reached.
    pub shared_scope: SharedScope,
    /// Page annotations with their `/Rect` overlap.
    pub annotations: Vec<AnnotInfo>,
    /// Document areas never evaluated by this page-space scan:
    /// `/Info`, `/XMP`, attachments, form values, structure tree /
    /// alternate text, JavaScript/actions, font-embedded data, and
    /// same-content data on other pages. Each entry is a fixed label
    /// — "not examined" is **not** "absent".
    pub unexamined: Vec<&'static str>,
    /// `Some` when the walk ended before the content stream was fully
    /// scanned — `targets`/`blockers` are then *partial* and must not
    /// be treated as "nothing found".
    pub incomplete: Option<Incomplete>,
}

// -------------------------------------------------------------------
// Rect validation
// -------------------------------------------------------------------

/// Validate one user-supplied rectangle: finite, and strictly
/// positive width/height with `urx > llx`, `ury > lly`. Corners are
/// **not** swapped — a reversed rect is a caller bug, not something
/// to silently fix.
fn check_rect(i: usize, llx: f64, lly: f64, urx: f64, ury: f64) -> Result<Rect, Error> {
    for (j, v) in [llx, lly, urx, ury].iter().enumerate() {
        if !v.is_finite() {
            return Err(Error::Structure(format!(
                "rect {i} component {j} is not finite ({v})"
            )));
        }
    }
    let r = Rect { llx, lly, urx, ury };
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return Err(Error::Structure(format!(
            "rect {i} must satisfy llx<urx and lly<ury (got {} {} {} {})",
            llx, lly, urx, ury
        )));
    }
    Ok(r)
}

/// Axis-aligned bbox intersection — the *conservative* overlap test
/// used for every target. Touching edges count as overlapping: a
/// zero-area contact still reports the hit because stroke outlines,
/// clips and estimates mean the real coverage may reach inside.
fn intersects(b: &(f64, f64, f64, f64), r: &Rect) -> bool {
    b.0 <= r.urx && b.2 >= r.llx && b.1 <= r.ury && b.3 >= r.lly
}

// -------------------------------------------------------------------
// Entry point
// -------------------------------------------------------------------

/// Diagnose `page_index` (0-based) of `doc` against `rects` (user
/// space, +x right +y up, unrotated, un-cropped). Never mutates the
/// document or re-saves anything.
///
/// Returns `Err` on invalid input (empty rect list, non-finite or
/// non-positive rects, out-of-range page) and on structural parse
/// failures — a diagnosis that cannot read the content cannot report
/// on it. When the walk is interrupted mid-stream the result carries
/// `incomplete` instead of `Err`, so callers can distinguish
/// "nothing found" from "could not finish".
pub fn diagnose_redaction(
    doc: &Document,
    page_index: usize,
    rects: &[(f64, f64, f64, f64)],
    limits: &Limits,
) -> Result<Diagnosis, Error> {
    // ---- input validation ----------------------------------------
    if rects.is_empty() {
        return Err(Error::Structure("at least one --rect is required".into()));
    }
    if rects.len() as u64 > limits.max_diag_rects {
        return Err(Error::LimitExceeded {
            limit: "max_diag_rects",
            max: limits.max_diag_rects,
        });
    }
    let mut validated = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        validated.push(check_rect(i, r.0, r.1, r.2, r.3)?);
    }
    let page = doc.pages().get(page_index).ok_or_else(|| {
        Error::Structure(format!(
            "page {} does not exist (document has {} pages)",
            page_index + 1,
            doc.page_count()
        ))
    })?;

    let mut diag = Diagnosis {
        rects: validated.clone(),
        targets: Vec::new(),
        uncertain: Vec::new(),
        blockers: Vec::new(),
        shared: Vec::new(),
        shared_scope: SharedScope {
            pages_scanned: 0,
            pages_total: doc.page_count(),
            truncated: false,
        },
        annotations: Vec::new(),
        unexamined: Vec::new(),
        incomplete: None,
    };

    // ---- page-level blockers -------------------------------------
    if let Some(uu) = page_dict_entry(doc, page, b"UserUnit")? {
        match uu.as_number() {
            Some(v) if (v - 1.0).abs() > f64::EPSILON => diag.blockers.push(Blocker {
                site: None,
                what: "/UserUnit".into(),
                detail: format!("UserUnit {v} — coordinates are not default units"),
            }),
            Some(_) => {}
            None => {
                return Err(Error::Structure("/UserUnit is not a number".into()));
            }
        }
    }
    if let Some(annots) = page_dict_entry(doc, page, b"Annots")? {
        collect_annots(doc, &annots, &validated, &mut diag)?;
    }

    // ---- content walk ---------------------------------------------
    let ops = doc.page_operations(page)?;
    let mut walker = DiagWalker::new(doc, page, limits, &validated);
    match walker.run(&ops) {
        Ok(()) => {}
        Err(e) => {
            // Convert a mid-stream failure into `incomplete` so the
            // caller sees the partial result *and* knows it is partial —
            // never "nothing found". Hard structural failures (page
            // parse itself) still return Err via `?` above.
            diag.incomplete = Some(Incomplete {
                reason: format!("{e}"),
                from_op: Some(walker.op_index),
            });
        }
    }
    diag.targets = walker.targets;
    diag.uncertain = walker.uncertain;
    diag.blockers.extend(walker.blockers);
    let site_map = walker.site_map;

    // ---- shared-resource analysis --------------------------------
    // Any object invoked from >=2 distinct sites on this page, or from
    // this page plus another, is "shared": editing it affects every
    // call site. The cross-page scan is bounded by limits — a
    // truncated scan is *not* a "not shared" guarantee.
    collect_shared(doc, page_index, &site_map, limits, &mut diag)?;

    // ---- unexamined areas ----------------------------------------
    diag.unexamined = vec![
        "/Info and /XMP metadata",
        "embedded files and attachments",
        "form field values",
        "structure tree and alternate text (/Alt, /ActualText outside walked streams)",
        "JavaScript and action dictionaries",
        "font-embedded data (ToUnicode, metadata in font streams)",
        "identical content duplicated on other pages or under other objects",
        "annotation appearances, popups and action payloads",
        "named destinations and link target text outside this page",
    ];
    Ok(diag)
}

// -------------------------------------------------------------------
// Page annotations
// -------------------------------------------------------------------

/// Collect `/Annots` entries with their subtype, Rect and rect hits.
/// A dangling or non-dict annot is a *blocker* — the diagnosis still
/// reports it, but nothing about that annotation is "understood".
fn collect_annots(
    doc: &Document,
    annots_obj: &Object,
    rects: &[Rect],
    diag: &mut Diagnosis,
) -> Result<(), Error> {
    let items = match annots_obj {
        Object::Array(a) => a.clone(),
        other => {
            return Err(Error::Structure(format!(
                "page /Annots is not an array ({other:?})"
            )))
        }
    };
    for (i, entry) in items.iter().enumerate() {
        let site = Site {
            contents: Vec::new(),
            op_index: usize::MAX,
            form_path: Vec::new(),
        };
        let aobj = match entry {
            Object::Ref(r) => match doc.resolve(*r) {
                Ok(o) => o,
                Err(e) => {
                    diag.blockers.push(Blocker {
                        site: Some(site.clone()),
                        what: "annotation".into(),
                        detail: format!("annot {i} ({r}) does not resolve: {e}"),
                    });
                    continue;
                }
            },
            o => o.clone(),
        };
        let adict = match aobj.as_dict() {
            Some(d) => d,
            None => {
                diag.blockers.push(Blocker {
                    site: Some(site.clone()),
                    what: "annotation".into(),
                    detail: format!("annot {i} is not a dictionary"),
                });
                continue;
            }
        };
        let subtype = adict
            .get(b"Subtype".as_slice())
            .and_then(|o| o.as_name())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_else(|| "unknown".into());
        let rect = adict
            .get(b"Rect".as_slice())
            .and_then(crate::page::Rect::from_object)
            .map(|r| (r.llx, r.lly, r.urx, r.ury));
        let hits = rect.map(|b| hits_for(&b, rects)).unwrap_or_default();
        let extra_keys: Vec<String> = [
            b"Contents".as_slice(),
            b"A".as_slice(),
            b"AP".as_slice(),
            b"AA".as_slice(),
            b"Popup".as_slice(),
            b"Dest".as_slice(),
        ]
        .iter()
        .filter(|k| adict.contains_key(**k))
        .map(|k| format!("/{}", String::from_utf8_lossy(k)))
        .collect();
        diag.annotations.push(AnnotInfo {
            subtype,
            rect,
            hits,
            extra_keys,
        });
    }
    Ok(())
}

/// Collect the indirect refs behind a page's `/Contents` — a single
/// stream → one ref; an array → every ref element (non-ref elements
/// are skipped, they carry no identity). Used by `Site::contents`.
fn contents_refs(page: &Page) -> Vec<IndirectRef> {
    let mut out = Vec::new();
    match page.contents() {
        Some(Object::Ref(r)) => out.push(*r),
        Some(Object::Array(a)) => {
            for e in a {
                if let Object::Ref(r) = e {
                    out.push(*r);
                }
            }
        }
        _ => {}
    }
    out
}

/// `page_dict_entry` variant kept local to avoid a circular import —
/// resolve one key through the page dict the way `render.rs` does.
fn page_dict_entry(doc: &Document, page: &Page, key: &[u8]) -> Result<Option<Object>, Error> {
    let r = match page.object_ref() {
        Some(r) => r,
        None => return Ok(None),
    };
    let obj = doc.resolve(r)?;
    let dict = obj
        .as_dict()
        .ok_or_else(|| Error::Structure("page object is not a dictionary".into()))?;
    match dict.get(key) {
        None => Ok(None),
        Some(o) => Ok(Some(doc.deref(o)?)),
    }
}

// -------------------------------------------------------------------
// Sharing analysis
// -------------------------------------------------------------------

/// Merge `site_map` (object ref → `(kind, sites)` on this page) with a
/// bounded cross-page `/Resources` scan: a ref used here *and* on
/// another page, or twice here, is `shared`. The scan reads resource
/// dictionaries only — it does not parse other pages' content, so a
/// `Do`-level use elsewhere is not detected; that limit is part of
/// [`SharedScope`].
fn collect_shared(
    doc: &Document,
    page_index: usize,
    site_map: &BTreeMap<(u32, u16), (String, Vec<Site>)>,
    limits: &Limits,
    diag: &mut Diagnosis,
) -> Result<(), Error> {
    let want: BTreeSet<(u32, u16)> = site_map.keys().copied().collect();
    if want.is_empty() {
        diag.shared_scope.pages_scanned = 1;
        return Ok(());
    }

    // Scan other pages' /Resources for the same refs — cheap (dict
    // lookups only, no content parsing) and bounded.
    let mut pages_scanned = 0usize;
    let mut refs_seen = 0u64;
    let mut truncated = false;
    let mut other_hits: HashMap<(u32, u16), Vec<usize>> = HashMap::new();
    'pages: for (pi, p) in doc.pages().iter().enumerate() {
        if pi == page_index {
            continue;
        }
        if pages_scanned as u64 >= limits.max_diag_share_pages {
            truncated = true;
            break;
        }
        pages_scanned += 1;
        let Some(res_obj) = p.resources() else {
            continue;
        };
        let Ok(res) = doc.deref(res_obj) else {
            continue;
        };
        let Some(resd) = res.as_dict() else {
            continue;
        };
        for cat in [
            b"Font".as_slice(),
            b"XObject".as_slice(),
            b"ExtGState".as_slice(),
            b"ColorSpace".as_slice(),
            b"Pattern".as_slice(),
            b"Shading".as_slice(),
            b"Properties".as_slice(),
        ] {
            let Some(sub) = resd.get(cat) else { continue };
            let Ok(sub) = doc.deref(sub) else { continue };
            let Some(subd) = sub.as_dict() else {
                continue;
            };
            for v in subd.values() {
                refs_seen += 1;
                if refs_seen > limits.max_diag_share_refs {
                    truncated = true;
                    break 'pages;
                }
                if let Object::Ref(r) = v {
                    let key = (r.number, r.generation);
                    if want.contains(&key) {
                        other_hits.entry(key).or_default().push(pi);
                    }
                }
            }
        }
    }

    // Emit SharedUse for anything invoked ≥2 sites *or* listed in
    // another page's resources. A single-site, single-page use is not
    // shared. `other_pages` is a page index list, sorted and deduped.
    for (key, (kind, sites)) in site_map.iter() {
        let mut other_pages = other_hits.remove(key).unwrap_or_default();
        other_pages.sort_unstable();
        other_pages.dedup();
        if sites.len() >= 2 || !other_pages.is_empty() {
            diag.shared.push(SharedUse {
                obj: IndirectRef::new(key.0, key.1),
                kind: kind.clone(),
                sites: sites.clone(),
                other_pages,
            });
        }
    }
    diag.shared_scope.pages_scanned = pages_scanned + 1; // + this page
    diag.shared_scope.truncated = truncated;
    Ok(())
}

// -------------------------------------------------------------------
// Content walker
// -------------------------------------------------------------------

/// Minimal graphics state the diagnosis needs: CTM for geometry,
/// clip rect for conservative coverage, and the current text state
/// to position shown characters.
struct DiagGs {
    ctm: crate::matrix::Matrix,
    /// Active clip in user space, when known — path/image/text
    /// bounds are intersected with it for the overlap test but the
    /// reported bounds stay the *whole* object (a conservative
    /// candidate, never a "safe outside" verdict).
    clip: Option<(f64, f64, f64, f64)>,
}

struct DiagWalker<'a> {
    doc: &'a Document,
    limits: &'a Limits,
    rects: &'a [Rect],
    /// `/Contents` element refs for `Site::contents` — resolved once
    /// at construction (refs only, no stream decode).
    contents_refs: Vec<IndirectRef>,
    gs: DiagGs,
    gs_stack: Vec<DiagGs>,
    resources: Object,
    /// Current `Do` nesting for site reporting.
    form_path: Vec<(Vec<u8>, Option<IndirectRef>)>,
    xobject_depth: u32,
    xobject_invocations: u64,
    xobject_stack: Vec<IndirectRef>,
    op_index: usize,
    /// Path being built — bounds accumulation in user space plus
    /// whether a current point exists (for `l`/`c`/`v`/`y` validity).
    path_bounds: Option<(f64, f64, f64, f64)>,
    path_has_point: bool,
    pending_clip: Option<()>,
    /// Current line width in user space — path bounds are inflated
    /// by half this (scaled by the CTM) for the stroke extent.
    stroke_width: f64,
    /// Text state needed to position shown text.
    text_tm: crate::matrix::Matrix,
    text_tlm: crate::matrix::Matrix,
    text_state: TextState,
    in_text: bool,
    targets: Vec<Target>,
    uncertain: Vec<Uncertain>,
    blockers: Vec<Blocker>,
    /// object ref → (kind, sites) for sharing analysis.
    site_map: BTreeMap<(u32, u16), (String, Vec<Site>)>,
    /// Counting budget for reported items.
    reported: u64,
}

/// Text state we must track to position glyphs (subset of the
/// renderer's).
#[derive(Default, Clone)]
struct TextState {
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    font_size: f64,
    render_mode: i64,
    rise: f64,
    leading: f64,
    font: Option<crate::font::Font>,
    font_name: Vec<u8>,
}

impl<'a> DiagWalker<'a> {
    fn new(doc: &'a Document, page: &'a Page, limits: &'a Limits, rects: &'a [Rect]) -> Self {
        let resources = page
            .resources()
            .cloned()
            .unwrap_or(Object::Dict(Default::default()));
        DiagWalker {
            doc,
            limits,
            rects,
            contents_refs: contents_refs(page),
            gs: DiagGs {
                ctm: crate::matrix::Matrix::IDENTITY,
                clip: None,
            },
            gs_stack: Vec::new(),
            resources,
            form_path: Vec::new(),
            xobject_depth: 0,
            xobject_invocations: 0,
            xobject_stack: Vec::new(),
            op_index: 0,
            path_bounds: None,
            path_has_point: false,
            pending_clip: None,
            stroke_width: 1.0,
            text_tm: crate::matrix::Matrix::IDENTITY,
            text_tlm: crate::matrix::Matrix::IDENTITY,
            text_state: TextState {
                h_scale: 1.0,
                ..Default::default()
            },
            in_text: false,
            targets: Vec::new(),
            uncertain: Vec::new(),
            blockers: Vec::new(),
            site_map: BTreeMap::new(),
            reported: 0,
        }
    }

    fn site(&self) -> Site {
        Site {
            contents: self.contents_refs.clone(),
            op_index: self.op_index,
            form_path: self.form_path.clone(),
        }
    }

    fn bump_reported(&mut self) -> Result<(), Error> {
        self.reported += 1;
        if self.reported > self.limits.max_diag_items {
            return Err(Error::LimitExceeded {
                limit: "max_diag_items",
                max: self.limits.max_diag_items,
            });
        }
        Ok(())
    }

    fn run(&mut self, ops: &[crate::content::Operation]) -> Result<(), Error> {
        for (i, op) in ops.iter().enumerate() {
            self.op_index = i;
            self.exec(op)?;
        }
        Ok(())
    }

    fn exec(&mut self, op: &crate::content::Operation) -> Result<(), Error> {
        use crate::content::Operation as O;
        match op {
            O::SaveState => {
                if self.gs_stack.len() >= self.limits.max_gsave_depth {
                    return Err(Error::LimitExceeded {
                        limit: "max_gsave_depth",
                        max: self.limits.max_gsave_depth as u64,
                    });
                }
                self.gs_stack.push(DiagGs {
                    ctm: self.gs.ctm,
                    clip: self.gs.clip,
                });
            }
            O::RestoreState => {
                let popped = self
                    .gs_stack
                    .pop()
                    .ok_or_else(|| Error::Structure("Q without matching q".into()))?;
                self.gs = popped;
            }
            O::ConcatMatrix(m) => {
                if !m.is_finite() {
                    // A non-finite transform means later geometry
                    // cannot be trusted at all — stop the walk.
                    return Err(Error::Structure(
                        "non-finite cm operands — cannot continue diagnosis".into(),
                    ));
                }
                self.gs.ctm = self.gs.ctm.concat(*m);
            }
            O::SetLineWidth(w) => self.stroke_width = *w,
            O::SetLineCap(_)
            | O::SetLineJoin(_)
            | O::SetMiterLimit(_)
            | O::SetDash { .. }
            | O::SetFlatness(_)
            | O::SetRenderingIntent(_) => {}
            O::SetColorSpaceStroke(n) | O::SetColorSpaceFill(n) => {
                // A Pattern colour space means subsequent SC/sc paints
                // a *pattern* — its extent is whatever the pattern
                // paints, not the path bounds. Block.
                if is_pattern_name(self.doc, &self.resources, n)? {
                    self.blockers.push(Blocker {
                        site: Some(self.site()),
                        what: "pattern colour space".into(),
                        detail: format!(
                            "/{} — pattern fills paint outside the path bbox",
                            String::from_utf8_lossy(n)
                        ),
                    });
                }
            }
            O::SetColorStroke(_)
            | O::SetColorFill(_)
            | O::SetGrayStroke(_)
            | O::SetGrayFill(_)
            | O::SetRgbStroke(..)
            | O::SetRgbFill(..)
            | O::SetCmykStroke(..)
            | O::SetCmykFill(..) => {}
            O::MoveTo(x, y) => {
                self.path_point(*x, *y);
                self.path_has_point = true;
            }
            O::LineTo(x, y) => {
                if !self.path_has_point {
                    return Err(Error::Structure(
                        "l without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path_point(*x, *y);
            }
            O::CurveTo(x1, y1, x2, y2, x3, y3) => {
                if !self.path_has_point {
                    return Err(Error::Structure(
                        "c without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path_point(*x1, *y1);
                self.path_point(*x2, *y2);
                self.path_point(*x3, *y3);
            }
            O::CurveToV(x2, y2, x3, y3) => {
                if !self.path_has_point {
                    return Err(Error::Structure(
                        "v without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path_point(*x2, *y2);
                self.path_point(*x3, *y3);
            }
            O::CurveToY(x1, y1, x2, y2) => {
                if !self.path_has_point {
                    return Err(Error::Structure(
                        "y without a preceding m/re (no current point)".into(),
                    ));
                }
                self.path_point(*x1, *y1);
                self.path_point(*x2, *y2);
            }
            O::Rectangle(x, y, w, h) => {
                self.path_point(*x, *y);
                self.path_point(*x + *w, *y + *h);
                self.path_has_point = false; // re is a complete subpath
            }
            O::ClosePath => self.path_has_point = false,
            O::Clip | O::ClipEvenOdd => {
                // A clip path constrains later painting — record its
                // bounds as the clip rect (conservative: the *path*
                // bounds, exact intersection is not computed).
                self.pending_clip = Some(());
            }
            O::Stroke
            | O::CloseAndStroke
            | O::Fill
            | O::FillEvenOdd
            | O::FillAndStroke
            | O::FillAndStrokeEvenOdd
            | O::CloseFillAndStroke
            | O::CloseFillAndStrokeEvenOdd
            | O::EndPath => {
                self.paint_path_target(op)?;
            }
            O::BeginText => {
                self.in_text = true;
                self.text_tm = crate::matrix::Matrix::IDENTITY;
                self.text_tlm = crate::matrix::Matrix::IDENTITY;
            }
            O::EndText => self.in_text = false,
            O::SetTextFont { font, size } => {
                self.text_state.font = Some(crate::font::load_font(
                    self.doc,
                    &self.resources,
                    font,
                    self.limits,
                )?);
                self.text_state.font_name = font.clone();
                self.text_state.font_size = *size;
            }
            O::SetCharSpacing(v) => self.text_state.char_spacing = *v,
            O::SetWordSpacing(v) => self.text_state.word_spacing = *v,
            O::SetHorizontalScaling(v) => self.text_state.h_scale = *v / 100.0,
            O::SetTextLeading(v) => self.text_state.leading = *v,
            O::SetTextRenderingMode(m) => self.text_state.render_mode = *m,
            O::SetTextRise(v) => self.text_state.rise = *v,
            O::MoveTextPosition(x, y) => {
                let t = crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, *x, *y);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            O::MoveTextPositionAndSetLeading(x, y) => {
                self.text_state.leading = -*y;
                let t = crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, *x, *y);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            O::SetTextMatrix(m) => {
                self.text_tm = *m;
                self.text_tlm = *m;
            }
            O::NextLine => {
                let t =
                    crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.text_state.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
            }
            O::ShowText(bytes) => self.show_text(bytes)?,
            O::ShowTextAdjusted(items) => {
                for it in items {
                    match it {
                        crate::content::TJItem::Text(b) => self.show_text(b)?,
                        crate::content::TJItem::Kern(n) => {
                            let t = &self.text_state;
                            let tx = -*n / 1000.0 * t.font_size * t.h_scale;
                            let tm = crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
                            self.text_tm = tm.concat(self.text_tm);
                        }
                    }
                }
            }
            O::NextLineShowText(text) => {
                let t =
                    crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.text_state.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
                self.show_text(text)?;
            }
            O::SetSpacingNextLineShowText {
                word_spacing,
                char_spacing,
                text,
            } => {
                self.text_state.word_spacing = *word_spacing;
                self.text_state.char_spacing = *char_spacing;
                let t =
                    crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, -self.text_state.leading);
                self.text_tlm = t.concat(self.text_tlm);
                self.text_tm = self.text_tlm;
                self.show_text(text)?;
            }
            O::Do(n) => self.do_xobject(n)?,
            O::Shading(n) => {
                self.blockers.push(Blocker {
                    site: Some(self.site()),
                    what: "shading (sh)".into(),
                    detail: format!(
                        "/{} — shading fills an area we cannot bound",
                        String::from_utf8_lossy(n)
                    ),
                });
            }
            O::SetExtGState(n) => {
                self.blockers.push(Blocker {
                    site: Some(self.site()),
                    what: "ExtGState (gs)".into(),
                    detail: format!(
                        "/{} — transparency/blend state is not implemented",
                        String::from_utf8_lossy(n)
                    ),
                });
            }
            O::MarkedContentPoint(_) => {}
            O::DesignatedMarkedContentPoint { tag, properties }
            | O::BeginMarkedContentWithProps { tag, properties } => {
                self.marked_content(tag, properties)?;
            }
            // begin/end marked-content without properties carry no
            // payload we must evaluate.
            O::BeginMarkedContent(_)
            | O::EndMarkedContent
            | O::BeginCompatibility
            | O::EndCompatibility => {}
            O::Unknown { operator, .. } => {
                // An op we cannot interpret means *position tracking*
                // may drift — nothing after is "certain".
                self.uncertain.push(Uncertain {
                    kind: TargetKind::Other,
                    site: self.site(),
                    reason: format!(
                        "unknown operator '{}' — subsequent positions may be wrong",
                        String::from_utf8_lossy(operator)
                    ),
                });
            }
        }
        Ok(())
    }

    fn path_point(&mut self, x: f64, y: f64) {
        let (ux, uy) = self.gs.ctm.transform(x, y);
        match &mut self.path_bounds {
            None => self.path_bounds = Some((ux, uy, ux, uy)),
            Some(b) => {
                b.0 = b.0.min(ux);
                b.1 = b.1.min(uy);
                b.2 = b.2.max(ux);
                b.3 = b.3.max(uy);
            }
        }
    }

    fn paint_path_target(&mut self, op: &crate::content::Operation) -> Result<(), Error> {
        use crate::content::Operation as O;
        let is_paint = !matches!(op, O::EndPath);
        if is_paint {
            if let Some(mut b) = self.path_bounds {
                // Conservative: inflate by half the line width scaled
                // by the CTM — stroke extent depends on width/cap/join
                // and we must not under-report coverage.
                let inflate = (self.stroke_width / 2.0).abs() * ctm_scale(&self.gs.ctm).max(0.0);
                if inflate.is_finite() {
                    b.0 -= inflate;
                    b.1 -= inflate;
                    b.2 += inflate;
                    b.3 += inflate;
                }
                let hits = hits_for(&b, self.rects);
                self.bump_reported()?;
                self.targets.push(Target {
                    kind: TargetKind::Path,
                    site: self.site(),
                    bounds: b,
                    hits,
                    detail: TargetDetail::Path {
                        op: op.name(),
                        stroked: inflate > 0.0,
                    },
                });
            }
        }
        // Install a pending clip as the *path's* bounds (approximation).
        if self.pending_clip.take().is_some() {
            if let Some(b) = self.path_bounds {
                self.gs.clip = Some(match self.gs.clip {
                    None => b,
                    Some(c) => (c.0.max(b.0), c.1.max(b.1), c.2.min(b.2), c.3.min(b.3)),
                });
            }
        }
        self.path_bounds = None;
        self.path_has_point = false;
        Ok(())
    }

    fn show_text(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if !self.in_text {
            return Err(Error::Structure("Tj outside a text object".into()));
        }
        let font = self
            .text_state
            .font
            .clone()
            .ok_or_else(|| Error::Structure("Tj before Tf — no font selected".into()))?;
        if !(self.text_state.font_size.is_finite() && self.text_state.font_size > 0.0) {
            return Err(Error::Structure("Tj before Tf — no font size".into()));
        }
        let mut pos = 0;
        while pos < bytes.len() {
            let (code, len) = font.read_code(bytes, pos)?;
            pos += len;
            self.show_glyph(&font, code)?;
        }
        Ok(())
    }

    /// Position one character code and record the target — mirrors the
    /// renderer's `show_glyph` math but reports instead of painting.
    fn show_glyph(&mut self, font: &crate::font::Font, code: u32) -> Result<(), Error> {
        // Copy the scalars out of `text_state` up front — we mutate
        // `self` (report, site_map) while the glyph positions itself.
        let (font_size, h_scale, rise, char_spacing, word_spacing) = {
            let t = &self.text_state;
            (
                t.font_size,
                t.h_scale,
                t.rise,
                t.char_spacing,
                t.word_spacing,
            )
        };
        // Glyph → page transform: Trm × CTM (§9.4.4).
        let trm = crate::matrix::Matrix::new(font_size * h_scale, 0.0, 0.0, font_size, 0.0, rise)
            .concat(self.text_tm)
            .concat(self.gs.ctm);
        if !trm.is_finite() {
            self.uncertain.push(Uncertain {
                kind: TargetKind::Text,
                site: self.site(),
                reason: "non-finite text rendering matrix".into(),
            });
            return Ok(());
        }
        let origin = trm.transform(0.0, 0.0);
        // Outline bounds, or a cell estimate when the glyph has no
        // outline (space, .notdef) — the estimate is *not* ink extent.
        let gid = font.glyph_id(code).ok();
        let units = font.units_per_em().unwrap_or(1000.0);
        let mut gp = crate::font::GlyphPath::new(units);
        let has_outline = gid
            .map(|g| font.outline(g, &mut gp).unwrap_or(false))
            .unwrap_or(false);
        let (bounds, bounds_kind) = if has_outline {
            if let Some(path) = gp.finish() {
                let b = path.bounds();
                let corners = [
                    trm.transform(b.left() as f64, b.top() as f64),
                    trm.transform(b.right() as f64, b.bottom() as f64),
                    trm.transform(b.left() as f64, b.bottom() as f64),
                    trm.transform(b.right() as f64, b.top() as f64),
                ];
                (
                    (
                        corners[0]
                            .0
                            .min(corners[1].0)
                            .min(corners[2].0)
                            .min(corners[3].0),
                        corners[0]
                            .1
                            .min(corners[1].1)
                            .min(corners[2].1)
                            .min(corners[3].1),
                        corners[0]
                            .0
                            .max(corners[1].0)
                            .max(corners[2].0)
                            .max(corners[3].0),
                        corners[0]
                            .1
                            .max(corners[1].1)
                            .max(corners[2].1)
                            .max(corners[3].1),
                    ),
                    TextBounds::Outline,
                )
            } else {
                (cell_estimate(&trm), TextBounds::Cell)
            }
        } else {
            (cell_estimate(&trm), TextBounds::Cell)
        };
        if !bounds.0.is_finite() || !bounds.1.is_finite() {
            self.uncertain.push(Uncertain {
                kind: TargetKind::Text,
                site: self.site(),
                reason: "non-finite glyph bounds".into(),
            });
        } else {
            let hits = hits_for(&bounds, self.rects);
            let (unicode, _via) = crate::text_extract::resolve_unicode(font, code);
            self.bump_reported()?;
            self.targets.push(Target {
                kind: TargetKind::Text,
                site: self.site(),
                bounds,
                hits,
                detail: TargetDetail::Text {
                    code,
                    unicode,
                    font: self.text_state.font_name.clone(),
                    render_mode: self.text_state.render_mode as u8,
                    origin,
                    bounds_kind,
                },
            });
        }
        // Advance the text matrix exactly like the renderer — the next
        // glyph's position depends on this.
        let w0 = font.width(code);
        let tw = if font.is_space_code(code) {
            word_spacing
        } else {
            0.0
        };
        let tx = ((w0 / 1000.0) * font_size + char_spacing + tw) * h_scale;
        let adv = crate::matrix::Matrix::new(1.0, 0.0, 0.0, 1.0, tx, 0.0);
        self.text_tm = adv.concat(self.text_tm);
        Ok(())
    }

    fn do_xobject(&mut self, name: &[u8]) -> Result<(), Error> {
        if self.xobject_invocations >= self.limits.max_xobject_invocations {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_invocations",
                max: self.limits.max_xobject_invocations,
            });
        }
        self.xobject_invocations += 1;
        let entry = self
            .doc
            .resolve_resource_raw(&self.resources, "XObject", name)?
            .ok_or_else(|| {
                Error::Structure(format!(
                    "/XObject /{} is not defined in the current resources",
                    String::from_utf8_lossy(name)
                ))
            })?;
        let entry_ref = entry.as_ref();
        let obj = self.doc.deref(&entry)?;
        let (dict, raw) = obj.as_stream().ok_or_else(|| {
            Error::Structure(format!(
                "/XObject /{} is not a stream",
                String::from_utf8_lossy(name)
            ))
        })?;
        let dict = dict.clone();
        let raw = raw.to_vec();
        match dict.get(b"Subtype".as_slice()).and_then(|o| o.as_name()) {
            Some(b"Form") => self.do_form(&dict, &raw, name, entry_ref),
            Some(b"Image") => self.do_image(&dict, name, entry_ref),
            Some(other) => {
                self.blockers.push(Blocker {
                    site: Some(self.site()),
                    what: "XObject subtype".into(),
                    detail: format!(
                        "/{} — only /Form and /Image XObjects are diagnosable",
                        String::from_utf8_lossy(other)
                    ),
                });
                Ok(())
            }
            None => Err(Error::Structure(format!(
                "/XObject /{} has no /Subtype",
                String::from_utf8_lossy(name)
            ))),
        }
    }

    /// Image `Do`: no decode — record placement bbox, blocking attrs,
    /// and register the stream ref for sharing analysis.
    fn do_image(
        &mut self,
        dict: &Dict,
        name: &[u8],
        entry_ref: Option<IndirectRef>,
    ) -> Result<(), Error> {
        let mut blockers = Vec::new();
        for key in [
            b"Mask".as_slice(),
            b"SMask".as_slice(),
            b"SMaskInData".as_slice(),
            b"OC".as_slice(),
            b"ImageMask".as_slice(),
            b"Interpolate".as_slice(),
        ] {
            if let Some(v) = dict.get(key) {
                if key == b"SMaskInData" && v.as_i64() == Some(0) {
                    continue;
                }
                if key == b"ImageMask" && v.as_bool() == Some(false) {
                    continue;
                }
                if key == b"Interpolate" && v.as_bool() == Some(false) {
                    continue;
                }
                blockers.push(format!("/{}", String::from_utf8_lossy(key)));
            }
        }
        // Filters and colour space decide whether the *content* is
        // examinable; a filter we do not decode still diagnoses fine —
        // we never decode anyway — but it limits *editing* options.
        let filters =
            crate::filter::normalize_filters(dict.get(b"Filter".as_slice())).unwrap_or_default();
        if !filters.is_empty() {
            blockers.push(format!(
                "filter {}",
                filters
                    .iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let width = dict
            .get(b"Width".as_slice())
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        let height = dict
            .get(b"Height".as_slice())
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        // Unit square under CTM → destination parallelogram's
        // axis-aligned bbox. The *whole* image extent, not the painted
        // pixels — a conservative candidate per spec.
        let corners = [
            self.gs.ctm.transform(0.0, 0.0),
            self.gs.ctm.transform(1.0, 0.0),
            self.gs.ctm.transform(0.0, 1.0),
            self.gs.ctm.transform(1.0, 1.0),
        ];
        let bounds = (
            corners[0]
                .0
                .min(corners[1].0)
                .min(corners[2].0)
                .min(corners[3].0),
            corners[0]
                .1
                .min(corners[1].1)
                .min(corners[2].1)
                .min(corners[3].1),
            corners[0]
                .0
                .max(corners[1].0)
                .max(corners[2].0)
                .max(corners[3].0),
            corners[0]
                .1
                .max(corners[1].1)
                .max(corners[2].1)
                .max(corners[3].1),
        );
        if !bounds.0.is_finite() {
            self.uncertain.push(Uncertain {
                kind: TargetKind::Image,
                site: self.site(),
                reason: "non-finite image placement".into(),
            });
        } else {
            let hits = hits_for(&bounds, self.rects);
            self.bump_reported()?;
            self.targets.push(Target {
                kind: TargetKind::Image,
                site: self.site(),
                bounds,
                hits,
                detail: TargetDetail::Image {
                    xobj: entry_ref,
                    name: name.to_vec(),
                    size: (width, height),
                    blockers,
                },
            });
        }
        if let Some(r) = entry_ref {
            let site = self.site();
            self.site_map
                .entry((r.number, r.generation))
                .or_insert_with(|| ("Image".into(), Vec::new()))
                .1
                .push(site);
        }
        Ok(())
    }

    /// Form `Do`: recurse with the same mechanics as the renderer —
    /// save gs, concat /Matrix, record BBox clip, run under the form's
    /// own /Resources. The invocation itself is a target (its BBox in
    /// page space); inner occurrences report with `form_path` set.
    fn do_form(
        &mut self,
        dict: &Dict,
        raw: &[u8],
        name: &[u8],
        entry_ref: Option<IndirectRef>,
    ) -> Result<(), Error> {
        if self.xobject_depth >= self.limits.max_xobject_depth {
            return Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                max: self.limits.max_xobject_depth as u64,
            });
        }
        // Blocking attributes that change what's painted.
        for key in [b"Group".as_slice(), b"OC".as_slice(), b"Ref".as_slice()] {
            if dict.contains_key(key) {
                self.blockers.push(Blocker {
                    site: Some(self.site()),
                    what: format!("form /{}", String::from_utf8_lossy(key)),
                    detail: match key {
                        b"Group" => "group attributes (e.g. /Transparency) are not implemented",
                        b"OC" => "optional-content membership needs visibility evaluation",
                        _ => "reference XObject imports external content",
                    }
                    .into(),
                });
            }
        }
        let bbox = dict
            .get(b"BBox".as_slice())
            .and_then(crate::page::Rect::from_object)
            .ok_or_else(|| Error::Structure("form XObject has no /BBox".into()))?;
        let form_matrix = match dict.get(b"Matrix".as_slice()) {
            None | Some(Object::Null) => crate::matrix::Matrix::IDENTITY,
            Some(o) => matrix_from_array(o)
                .ok_or_else(|| Error::Structure("/Matrix is not a 6-number array".into()))?,
        };
        if !form_matrix.is_finite() {
            return Err(Error::Structure("/Matrix has non-finite values".into()));
        }
        if let Some(r) = entry_ref {
            if self.xobject_stack.contains(&r) {
                return Err(Error::CycleDetected {
                    context: "form XObject recursion",
                });
            }
            self.xobject_stack.push(r);
        }
        let pop_ref = entry_ref.is_some();
        // The invocation is itself a target — its painted extent is
        // the BBox under form_matrix × CTM.
        let corners = [
            (bbox.llx, bbox.lly),
            (bbox.urx, bbox.lly),
            (bbox.llx, bbox.ury),
            (bbox.urx, bbox.ury),
        ]
        .map(|(x, y)| self.gs.ctm.concat(form_matrix).transform(x, y));
        let bounds = (
            corners[0]
                .0
                .min(corners[1].0)
                .min(corners[2].0)
                .min(corners[3].0),
            corners[0]
                .1
                .min(corners[1].1)
                .min(corners[2].1)
                .min(corners[3].1),
            corners[0]
                .0
                .max(corners[1].0)
                .max(corners[2].0)
                .max(corners[3].0),
            corners[0]
                .1
                .max(corners[1].1)
                .max(corners[2].1)
                .max(corners[3].1),
        );
        if bounds.0.is_finite() {
            let hits = hits_for(&bounds, self.rects);
            self.bump_reported()?;
            self.targets.push(Target {
                kind: TargetKind::Form,
                site: self.site(),
                bounds,
                hits,
                detail: TargetDetail::Form {
                    xobj: entry_ref,
                    name: name.to_vec(),
                    bbox: bounds,
                    matrix: [
                        form_matrix.a,
                        form_matrix.b,
                        form_matrix.c,
                        form_matrix.d,
                        form_matrix.e,
                        form_matrix.f,
                    ],
                },
            });
        } else {
            self.uncertain.push(Uncertain {
                kind: TargetKind::Form,
                site: self.site(),
                reason: "non-finite form placement".into(),
            });
        }
        if let Some(r) = entry_ref {
            let site = self.site();
            self.site_map
                .entry((r.number, r.generation))
                .or_insert_with(|| ("Form".into(), Vec::new()))
                .1
                .push(site);
        }
        // Recurse: save gs (resources swap + ctm concat), run inner
        // ops, restore — same lifecycle as the renderer.
        self.form_path.push((name.to_vec(), entry_ref));
        let saved_resources = match dict.get(b"Resources".as_slice()) {
            None | Some(Object::Null) => None,
            Some(r) => Some(std::mem::replace(&mut self.resources, r.clone())),
        };
        self.gs_stack.push(DiagGs {
            ctm: self.gs.ctm,
            clip: self.gs.clip,
        });
        self.gs.ctm = self.gs.ctm.concat(form_matrix);
        // The form's BBox is its clip in *form space* — record it as
        // the current clip rect mapped to page space.
        self.gs.clip = Some(match self.gs.clip {
            None => bounds,
            Some(c) => (
                c.0.max(bounds.0),
                c.1.max(bounds.1),
                c.2.min(bounds.2),
                c.3.min(bounds.3),
            ),
        });
        let run_result = (|| {
            let decoded = self.doc.decode_stream(dict, raw)?;
            let ops = crate::content::parse_content_stream(&[decoded], self.limits)?;
            self.xobject_depth += 1;
            let r = self.run(&ops);
            self.xobject_depth -= 1;
            r
        })();
        if let Some(r) = saved_resources {
            self.resources = r;
        }
        self.gs = self.gs_stack.pop().expect("pushed above");
        self.form_path.pop();
        if pop_ref {
            self.xobject_stack.pop();
        }
        run_result
    }

    /// DP/BDC properties: `/OC` (optional content) and `/Span
    /// <</ActualText …>>` both change what the content means — record
    /// them as blockers exactly as the renderer refuses them.
    fn marked_content(&mut self, tag: &[u8], properties: &Object) -> Result<(), Error> {
        let resolved = match properties {
            Object::Name(n) => {
                if tag != b"OC" && tag != b"Span" {
                    return Ok(());
                }
                match self
                    .doc
                    .resolve_resource(&self.resources, "Properties", n)?
                {
                    Some(o) => o,
                    None => {
                        return Err(Error::Structure(format!(
                            "marked-content /Properties entry /{} is undefined",
                            String::from_utf8_lossy(n)
                        )))
                    }
                }
            }
            obj @ Object::Dict(_) => obj.clone(),
            _ => return Ok(()),
        };
        let dict = resolved.as_dict().ok_or_else(|| {
            Error::Structure("marked-content properties is not a dictionary".into())
        })?;
        if tag == b"OC" {
            match dict.get(b"Type".as_slice()).and_then(|t| t.as_name()) {
                Some(b"OCG") | Some(b"OCMD") => self.blockers.push(Blocker {
                    site: Some(self.site()),
                    what: "optional content (/OC)".into(),
                    detail: "OCG/OCMD membership needs visibility evaluation".into(),
                }),
                _ => {}
            }
        }
        if tag == b"Span" && dict.contains_key(b"ActualText".as_slice()) {
            self.blockers.push(Blocker {
                site: Some(self.site()),
                what: "ActualText".into(),
                detail: "replacement text substitutes the shown characters".into(),
            });
        }
        Ok(())
    }
}

/// Rough scale factor of a matrix — used to inflate path bounds for
/// stroke extent. `sqrt(|det|)` overestimates anisotropy on shear but
/// is conservative (never under-reports).
fn ctm_scale(m: &crate::matrix::Matrix) -> f64 {
    let det = (m.a * m.d - m.b * m.c).abs();
    det.sqrt()
}

/// Estimated character cell — origin, one-em advance, one-em ascent
/// mapped through `trm`. Used when a glyph has no outline (space,
/// .notdef): an *estimate* labelled `TextBounds::Cell`, never an ink
/// extent, and *not* the same thing as the advance vector — the cell
/// exists so a space or missing glyph inside the rects still reports.
fn cell_estimate(trm: &crate::matrix::Matrix) -> (f64, f64, f64, f64) {
    let origin = trm.transform(0.0, 0.0);
    // Advance point: same displacement the matrix advance uses.
    let adv = trm.transform(1.0, 0.0);
    let top = trm.transform(0.0, 1.0);
    let xs = [origin.0, adv.0, top.0];
    let ys = [origin.1, adv.1, top.1];
    (
        xs.iter().cloned().fold(f64::INFINITY, f64::min),
        ys.iter().cloned().fold(f64::INFINITY, f64::min),
        xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    )
}

/// Which input rects `bounds` overlaps — `hits_for` is the single
/// conservative intersection used for every target kind.
fn hits_for(b: &(f64, f64, f64, f64), rects: &[Rect]) -> Vec<usize> {
    rects
        .iter()
        .enumerate()
        .filter(|(_, r)| intersects(b, r))
        .map(|(i, _)| i)
        .collect()
}

/// Whether `n` names a pattern-type colour space entry in the current
/// resources — pattern fills paint outside the path bounds.
fn is_pattern_name(doc: &Document, resources: &Object, n: &[u8]) -> Result<bool, Error> {
    if n == b"Pattern" {
        return Ok(true);
    }
    let resolved = doc.resolve_resource(resources, "ColorSpace", n)?;
    Ok(matches!(resolved, Some(o) if {
        match &o {
            Object::Name(nn) => nn == b"Pattern",
            Object::Array(a) => a.first().and_then(|x| x.as_name()) == Some(b"Pattern".as_slice()),
            _ => false,
        }
    }))
}

/// Parse a `/Matrix` array into a `Matrix` — shared with `render.rs`'s
/// logic; duplicated here because that helper is private.
fn matrix_from_array(o: &Object) -> Option<crate::matrix::Matrix> {
    let a = o.as_array()?;
    if a.len() != 6 {
        return None;
    }
    let mut v = [0.0f64; 6];
    for (i, item) in a.iter().enumerate() {
        v[i] = item.as_number()?;
    }
    Some(crate::matrix::Matrix::new(
        v[0], v[1], v[2], v[3], v[4], v[5],
    ))
}

use crate::object::Dict;

#[cfg(test)]
mod tests {
    //! Unit tests for the diagnosis helpers — the CLI path is covered
    //! by `tests/m8_cli.rs`; here we pin the geometry and the
    //! rect-validation rules that the caller-visible contract rests
    //! on, plus the conservative bbox intersection.
    use super::*;

    /// A minimal 1-page PDF exposing one text glyph (KasaneTest.ttf)
    /// — the same TTF fixture `text_basic.pdf` uses, so the outline
    /// bounds are deterministic.
    fn one_glyph_pdf() -> Vec<u8> {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        // Same recipe as make_fixtures.py's text_basic.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        macro_rules! push {
            ($n:expr, $body:expr) => {{
                offs.push(($n, parts.iter().map(|p| p.len()).sum()));
                parts.push(format!("{} 0 obj\n", $n).into_bytes());
                parts.push(($body as &[u8]).to_vec());
                parts.push(b"\nendobj\n".to_vec());
            }};
        }
        push!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push!(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>"
        );
        // Same widths table as the fixture: 224 entries (32..=255),
        // all 600 except space (250) — 'A' maps to 600 by index 33.
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            let s = w
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            format!("[{s}]").into_bytes()
        };
        let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        font_dict.extend_from_slice(&widths);
        font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push!(4, &font_dict);
        push!(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>"
        );
        // The font stream — flate-compressed like the fixture.
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs.extend_from_slice(&enc);
        fs.extend_from_slice(b"\nendstream");
        push!(6, &fs);
        let body = b"BT /F1 24 Tf 20 60 Td (A) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push!(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    #[test]
    fn rect_validation_rules() {
        // Zero-size and reversed rects are refused — a rect is never
        // silently normalised or shrunk.
        for (llx, lly, urx, ury) in [
            (10.0, 10.0, 10.0, 20.0),  // zero width
            (10.0, 10.0, 20.0, 10.0),  // zero height
            (50.0, 0.0, 10.0, 10.0),   // llx > urx
            (0.0, 50.0, 10.0, 10.0),   // lly > ury
            (0.0, 0.0, f64::NAN, 10.), // non-finite
            (0.0, 0.0, f64::INFINITY, 10.),
        ] {
            assert!(
                check_rect(0, llx, lly, urx, ury).is_err(),
                "rect [{llx},{lly},{urx},{ury}] must be refused"
            );
        }
        assert!(check_rect(0, 0.0, 0.0, 10.0, 10.0).is_ok());
    }

    #[test]
    fn empty_rects_is_an_error() {
        let doc = crate::document::Document::from_bytes(&one_glyph_pdf()).unwrap();
        let e = diagnose_redaction(&doc, 0, &[], &Limits::default()).unwrap_err();
        assert!(e.to_string().contains("--rect"), "{e}");
    }

    #[test]
    fn glyph_outline_bounds_are_exact() {
        // The same 'A' glyph text_basic.pdf extracts: origin (20,60),
        // advance 14.4, bounds [20,60,34.4,76.8] at size 24.
        let doc = crate::document::Document::from_bytes(&one_glyph_pdf()).unwrap();
        let d =
            diagnose_redaction(&doc, 0, &[(15.0, 55.0, 40.0, 80.0)], &Limits::default()).unwrap();
        assert_eq!(d.targets.len(), 1, "{:?}", d.targets);
        let t = &d.targets[0];
        assert_eq!(t.kind, TargetKind::Text);
        assert_eq!(t.hits, vec![0]);
        match &t.detail {
            TargetDetail::Text {
                code,
                origin,
                bounds_kind,
                ..
            } => {
                assert_eq!(*code, 0x41);
                assert!((origin.0 - 20.0).abs() < 0.5);
                assert!((origin.1 - 60.0).abs() < 0.5);
                assert_eq!(*bounds_kind, TextBounds::Outline);
            }
            _ => panic!("expected text detail"),
        }
        // Ink extent matches the extract path's bounds.
        assert!((t.bounds.2 - 34.4).abs() < 0.5, "{:?}", t.bounds);
        // A rect *outside* the glyph reports no hits — and the target
        // still lists so callers see the whole picture.
        let miss = diagnose_redaction(&doc, 0, &[(100.0, 100.0, 150.0, 150.0)], &Limits::default())
            .unwrap();
        assert_eq!(miss.targets.len(), 1);
        assert!(miss.targets[0].hits.is_empty());
    }

    #[test]
    fn edge_touching_counts_as_hit() {
        // A rect touching the glyph's ink boundary is a hit —
        // conservative: stroke outlines and estimates mean the real
        // coverage may reach inside.
        let doc = crate::document::Document::from_bytes(&one_glyph_pdf()).unwrap();
        let d = diagnose_redaction(
            &doc,
            0,
            &[(34.4, 60.0, 60.0, 80.0)], // left edge == glyph right edge
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(d.targets[0].hits, vec![0]);
    }

    #[test]
    fn incomplete_is_not_targets_empty() {
        // Unsupported font → the walk stops mid-stream. `targets`
        // stays empty *and* `incomplete` explains why — callers must
        // not read "no targets" as "nothing to redact".
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        let mut push = |n: u32, body: &[u8]| {
            offs.push((n, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} 0 obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        );
        push(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        push(
            5,
            b"<< /Length 34 >>\nstream\nBT /F1 12 Tf 10 10 Td (Hi) Tj ET\nendstream",
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
        for n in 1..6u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let doc = crate::document::Document::from_bytes(&parts.concat()).unwrap();
        let d =
            diagnose_redaction(&doc, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert!(d.incomplete.is_some(), "Type1 → incomplete expected");
        assert!(d.targets.is_empty());
    }

    #[test]
    fn shared_scope_reports_single_page() {
        let doc = crate::document::Document::from_bytes(&one_glyph_pdf()).unwrap();
        let d =
            diagnose_redaction(&doc, 0, &[(0.0, 0.0, 200.0, 200.0)], &Limits::default()).unwrap();
        assert_eq!(d.shared_scope.pages_total, 1);
        assert!(!d.shared_scope.truncated);
        // A single-page doc can't have cross-page sharing.
        assert!(d.shared.is_empty());
    }
}
