//! Annotation-edit session: `/Square` additions and the incremental-save edits (metadata, annotations, form-field values).
//!
//! An [`Edit`] borrows an immutable [`Document`] and accumulates
//! additions as **synthetic objects** in a reserved indirect-number
//! space (`SYNTH_BASE ..= u32::MAX`). The original bytes are never
//! touched; the added /Square annotations and the modified `/Annots`
//! arrays they hang off live only in this session's overlay. When the
//! session is written out via [`Edit::rewrite`], the rewrite
//! collector resolves synthetic refs from the overlay and *real* refs
//! from the document — producing a normal single-revision file.
//!
//! Scope: `/Subtype /Square` annotations only — opaque RGB stroke
//! colour, positive line width, unfilled interior, a generated `/AP`
//! `/N` appearance stream, and printing+viewing flag set. Nothing else
//! (no fill, dashes, other subtypes, editing/deleting existing
//! annotations, page edits, redaction, flattening).

use crate::document::Document;
use crate::error::Error;
use crate::limits::Limits;
use crate::object::{Dict, IndirectRef, Object};
use crate::page::Rect;
use crate::rewrite;
use std::collections::{HashMap, HashSet};

/// Synthetic indirect-object numbers are allocated downward from
/// `u32::MAX` generation 0 — far above any realistic object count —
/// skipping any `(num, 0)` that already exists in the input xref and
/// any already allocated. A document is never *rejected* for having
/// high numbers: the allocator simply walks past them.
const SYNTH_TOP: u32 = u32::MAX - 1;

/// One pending rectangle annotation: the page it lands on plus the
/// validated user inputs.
struct PendingSquare {
    page_index: usize,
    rect: Rect,
    /// RGB stroke colour, each channel already validated to `0..=1`.
    rgb: [f64; 3],
    /// Line width in user units, already validated finite and `> 0`.
    width: f64,
}

/// An in-progress edit session over `doc`. Created by
/// [`Document::edit`]; consumed by [`rewrite`].
pub struct Edit<'a> {
    doc: &'a Document,
    /// `(obj, gen) → body` for every synthetic object and every real
    /// object whose body the edit replaces (the page dict and any
    /// copied `/Annots` array). Fed to [`rewrite::rewrite_overlay`].
    /// `pub(crate)` — `remove_text` inserts its `/Contents`
    /// stream and page-dict update through the same overlay.
    pub(crate) overlay: HashMap<(u32, u16), Object>,
    /// Next synthetic object number to hand out.
    next_synth: u32,
    /// Validated annotations to materialise at `commit` time.
    pending: Vec<PendingSquare>,
    /// Pages already edited (page object ref → its new `/Annots`
    /// array ref) so multiple additions to one page share one array.
    annots_by_page: HashMap<IndirectRef, IndirectRef>,
    /// Total bytes of generated appearance streams — bounded by
    /// `max_edit_annot_bytes` so an absurd loop can't grow memory
    /// without limit.
    appearance_bytes: u64,
    /// Cumulative count of synthetic objects created.
    synth_objects: u64,
    /// `(num, gen)` of every synthetic ref handed out — passed to the
    /// collector so an *input* reference landing on a synthetic number
    /// is still treated as the dangling reference it is, never
    /// silently resolved to a synthetic body.
    synth_numbers: HashSet<(u32, u16)>,
    /// Replacement `/Info` dictionary (`set_info`), written by the
    /// incremental save as a new object.
    info: Option<Dict>,
    /// What the session changed, for the signature policy.
    changes: Vec<crate::signature::ChangeKind>,
    limits: Limits,
}

impl<'a> Edit<'a> {
    /// `pub(crate)` — `remove_text` allocates one synthetic
    /// object (the new `/Contents` stream) through the same allocator.
    pub(crate) fn new(doc: &'a Document, limits: Limits) -> Result<Self, Error> {
        Ok(Edit {
            doc,
            overlay: HashMap::new(),
            next_synth: SYNTH_TOP,
            pending: Vec::new(),
            annots_by_page: HashMap::new(),
            appearance_bytes: 0,
            synth_objects: 0,
            synth_numbers: HashSet::new(),
            info: None,
            changes: Vec::new(),
            limits,
        })
    }

    /// Allocate the next free synthetic object number.
    ///
    /// Walks `next_synth` downward, skipping any number that is
    /// *reserved* — present in the input xref at any generation or
    /// type (`Document::number_reserved`), already allocated as a
    /// synthetic, or replaced by an overlay body — so a synthetic ref
    /// can never alias a real or reserved input number. A candidate
    /// region is exhausted (input filled `SYNTH_TOP`…`0`) only in a
    /// pathological file; hitting `max_edit_objects` or `0` is a typed
    /// `LimitExceeded`/`Unsupported` error, never a wraparound.
    /// `pub(crate)` — `remove_text` allocates the new `/Contents` stream ref
    /// through the same synthetic-number allocator.
    pub(crate) fn alloc(&mut self) -> Result<IndirectRef, Error> {
        self.synth_objects += 1;
        if self.synth_objects > self.limits.max_edit_objects {
            return Err(Error::LimitExceeded {
                limit: "max_edit_objects",
                max: self.limits.max_edit_objects,
            });
        }
        let mut n = self.next_synth;
        loop {
            if n == 0 {
                return Err(Error::Unsupported {
                    feature: "edit session",
                    detail: "synthetic object-number space exhausted".into(),
                });
            }
            let taken = self.doc.number_reserved(n)
                || self.synth_numbers.contains(&(n, 0))
                || self.overlay.contains_key(&(n, 0));
            if !taken {
                break;
            }
            n -= 1;
        }
        self.next_synth = n - 1;
        let r = IndirectRef::new(n, 0);
        self.synth_numbers.insert((r.number, r.generation));
        Ok(r)
    }

    /// Add a `/Subtype /Square` rectangle annotation to page
    /// `page_index` (0-based, matching [`Document::pages`] order).
    ///
    /// `rect` is in un-rotated PDF user space (+x right, +y up) — the
    /// same coordinates `extract_text` reports. `rgb` is the
    /// stroke colour, each channel `0..=1`; `width` is the stroke
    /// line width in user units, must be `> 0`.
    ///
    /// Validation happens now; nothing is committed until
    /// [`rewrite`]. On any error the session is unchanged (no partial
    /// state) — the caller can keep adding or discard it.
    pub fn add_square(
        &mut self,
        page_index: usize,
        rect: Rect,
        rgb: [f64; 3],
        width: f64,
    ) -> Result<(), Error> {
        // ---- validation: all-or-nothing, no overlay mutation yet ----
        let page = self.doc.pages().get(page_index).ok_or_else(|| {
            Error::Structure(format!(
                "page index {page_index} out of range ({} pages)",
                self.doc.page_count()
            ))
        })?;
        if self.pending.len() as u64 >= self.limits.max_edit_annots {
            return Err(Error::LimitExceeded {
                limit: "max_edit_annots",
                max: self.limits.max_edit_annots,
            });
        }
        for (i, v) in [rect.llx, rect.lly, rect.urx, rect.ury].iter().enumerate() {
            if !v.is_finite() {
                return Err(Error::Structure(format!(
                    "rect component {i} is not finite ({v})"
                )));
            }
        }
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return Err(Error::Structure(format!(
                "rect must have positive width and height (got {} x {})",
                rect.width(),
                rect.height()
            )));
        }
        for (i, c) in rgb.iter().enumerate() {
            if !c.is_finite() || !(0.0..=1.0).contains(c) {
                return Err(Error::Structure(format!(
                    "RGB channel {i} out of range 0..=1 ({c})"
                )));
            }
        }
        if !width.is_finite() || width <= 0.0 {
            return Err(Error::Structure(format!(
                "line width must be finite and > 0 (got {width})"
            )));
        }
        // A stroke of half the width extends past the Rect; if the
        // width dwarfs the rect the appearance would invert — refuse
        // rather than produce a meaningless box.
        let min_dim = rect.width().min(rect.height());
        if width >= min_dim {
            return Err(Error::Structure(format!(
                "line width {width} is not smaller than the rectangle \
                 ({} x {}) — appearance would invert",
                rect.width(),
                rect.height()
            )));
        }
        // /UserUnit ≠ 1 changes the meaning of user-space units; the editor
        // writes the rect verbatim, so a non-unit page would misplace
        // the box — refuse rather than treat it as points.
        if let Some(page_ref) = page.object_ref() {
            let page_obj = self.doc.resolve(page_ref)?;
            if let Some(d) = page_obj.as_dict() {
                if let Some(uu) = d.get(b"UserUnit".as_slice()) {
                    let uu = self.doc.deref(uu)?;
                    if uu.as_number() != Some(1.0) {
                        return Err(Error::Unsupported {
                            feature: "/UserUnit other than 1",
                            detail: "annotation rect is in default user units; \
                                     a scaled page would misplace it"
                                .into(),
                        });
                    }
                }
            }
        }

        self.changes.push(crate::signature::ChangeKind::Annotations);
        self.pending.push(PendingSquare {
            page_index,
            rect,
            rgb,
            width,
        });
        Ok(())
    }

    /// Materialise every pending annotation into the overlay and
    /// rewrite the document to a new single-revision PDF.
    ///
    /// All appearance streams and `/Annots` edits are committed here —
    /// a failure leaves the underlying `Document` untouched.
    pub fn rewrite(self) -> Result<Vec<u8>, Error> {
        self.rewrite_with_overrides(&rewrite::RewriteOverrides::default())
    }

    /// Append the session's changes to the original bytes as a new
    /// revision (§7.5.6; see `src/incremental.rs`). The original
    /// file is kept byte-for-byte, so earlier revisions and the bytes
    /// a signature covers survive; the signature policy
    /// ([`crate::signature`]) decides whether the changes may be
    /// appended at all — anything a signature does not permit is
    /// refused before a byte is written. Metadata set with
    /// [`Edit::set_info`] becomes a new `/Info` object.
    pub fn save_incremental(mut self) -> Result<Vec<u8>, Error> {
        let mut kinds = self.changes.clone();
        kinds.dedup();
        crate::signature::incremental_policy(self.doc, &kinds)?;
        self.commit()?;
        crate::incremental::write(
            self.doc,
            &self.overlay,
            &self.synth_numbers,
            self.info.as_ref(),
            &self.limits,
        )
    }

    /// Replace document-information entries: `entries` are merged
    /// over the current `/Info` (a `null` value removes a key). Written
    /// as a new `/Info` dictionary by [`Edit::save_incremental`]; a
    /// full [`Edit::rewrite`] does not carry it (use
    /// `remove_document_metadata` / the rewrite overrides there).
    pub fn set_info(&mut self, entries: Dict) -> Result<(), Error> {
        let mut info = match self.doc.trailer().get(b"Info".as_slice()) {
            Some(o) => self.doc.deref(o)?.as_dict().cloned().unwrap_or_default(),
            None => Dict::new(),
        };
        if let Some(cur) = &self.info {
            info = cur.clone();
        }
        for (k, v) in entries {
            if matches!(v, Object::Null) {
                info.remove(&k);
            } else {
                info.insert(k, v);
            }
        }
        self.info = Some(info);
        self.changes.push(crate::signature::ChangeKind::Metadata);
        Ok(())
    }

    /// The (overlay-owned) `/Annots` array of a page, created from the
    /// page's current array on first use so shared arrays are never
    /// mutated in place.
    fn annots_array_ref(&mut self, page_index: usize) -> Result<(IndirectRef, IndirectRef), Error> {
        let page = self
            .doc
            .pages()
            .get(page_index)
            .cloned()
            .ok_or_else(|| Error::Structure(format!("page index {page_index} out of range")))?;
        let page_ref = page.object_ref().ok_or_else(|| {
            Error::Structure("cannot edit a page that has no indirect reference".into())
        })?;
        if let Some(&r) = self.annots_by_page.get(&page_ref) {
            return Ok((page_ref, r));
        }
        let page_obj = self.doc.resolve(page_ref)?;
        let mut page_dict = page_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure(format!("page {page_ref} is not a dictionary")))?;
        let existing: Vec<Object> = match page_dict.get(b"Annots".as_slice()) {
            Some(o) => match self.doc.deref(o)? {
                Object::Array(a) => a,
                Object::Null => Vec::new(),
                other => {
                    return Err(Error::Structure(format!(
                        "page /Annots is not an array ({other:?})"
                    )))
                }
            },
            None => Vec::new(),
        };
        let arr_ref = self.alloc()?;
        self.overlay.insert(
            (arr_ref.number, arr_ref.generation),
            Object::Array(existing),
        );
        self.annots_by_page.insert(page_ref, arr_ref);
        page_dict.insert(b"Annots".to_vec(), Object::Ref(arr_ref));
        self.overlay.insert(
            (page_ref.number, page_ref.generation),
            Object::Dict(page_dict),
        );
        Ok((page_ref, arr_ref))
    }

    /// Remove annotation `index` (0-based, in `/Annots` order) from
    /// page `page_index`. The annotation object itself stays in the
    /// file (an incremental save cannot delete bytes); the page no
    /// longer references it.
    pub fn remove_annotation(&mut self, page_index: usize, index: usize) -> Result<(), Error> {
        let (_, arr_ref) = self.annots_array_ref(page_index)?;
        let arr = self
            .overlay
            .get_mut(&(arr_ref.number, arr_ref.generation))
            .and_then(|o| o.as_array_mut())
            .ok_or_else(|| Error::Structure("annot array missing from overlay".into()))?;
        if index >= arr.len() {
            return Err(Error::Structure(format!(
                "page {page_index} has {} annotations, index {index} is out of range",
                arr.len()
            )));
        }
        arr.remove(index);
        self.changes.push(crate::signature::ChangeKind::Annotations);
        Ok(())
    }

    /// Set (or, with a `null` value, remove) entries of annotation
    /// `index` on page `page_index` — e.g. `/Contents`, `/Rect`, `/C`,
    /// `/F`. An existing `/AP` is kept: a changed `/Rect` refits it
    /// (§12.5.5), a changed colour does not redraw it; remove `/AP`
    /// explicitly (set it to `null`) to have the appearance
    /// synthesised from the new entries.
    pub fn set_annotation_entries(
        &mut self,
        page_index: usize,
        index: usize,
        entries: Dict,
    ) -> Result<(), Error> {
        let (_, arr_ref) = self.annots_array_ref(page_index)?;
        let entry = self
            .overlay
            .get(&(arr_ref.number, arr_ref.generation))
            .and_then(|o| o.as_array())
            .and_then(|a| a.get(index).cloned())
            .ok_or_else(|| Error::Structure(format!("annotation index {index} out of range")))?;
        let apply = |d: &mut Dict| {
            for (k, v) in &entries {
                if matches!(v, Object::Null) {
                    d.remove(k);
                } else {
                    d.insert(k.clone(), v.clone());
                }
            }
        };
        match entry {
            Object::Ref(r) => {
                let key = (r.number, r.generation);
                let mut d = match self.overlay.get(&key) {
                    Some(Object::Dict(d)) => d.clone(),
                    Some(_) => {
                        return Err(Error::Structure(format!(
                            "annotation {r} is not a dictionary"
                        )))
                    }
                    None => self.doc.resolve(r)?.as_dict().cloned().ok_or_else(|| {
                        Error::Structure(format!("annotation {r} is not a dictionary"))
                    })?,
                };
                apply(&mut d);
                self.overlay.insert(key, Object::Dict(d));
            }
            Object::Dict(mut d) => {
                apply(&mut d);
                let arr = self
                    .overlay
                    .get_mut(&(arr_ref.number, arr_ref.generation))
                    .and_then(|o| o.as_array_mut())
                    .ok_or_else(|| Error::Structure("annot array missing from overlay".into()))?;
                arr[index] = Object::Dict(d);
            }
            other => {
                return Err(Error::Structure(format!(
                    "annotation entry {index} is not a dictionary ({other:?})"
                )))
            }
        }
        self.changes.push(crate::signature::ChangeKind::Annotations);
        Ok(())
    }

    /// Set the value of form field `name` (its fully qualified name,
    /// `parent.child`) to `value`: `/V` on the field; for a name value
    /// also `/AS` on the field's widgets whose `/AP /N` has that state
    /// (`/Off` otherwise); and `/NeedAppearances true` on `/AcroForm`
    /// so viewers (and this renderer) regenerate text appearances.
    pub fn set_field_value(&mut self, name: &str, value: Object) -> Result<(), Error> {
        let af_entry = self
            .doc
            .catalog()
            .get(b"AcroForm".as_slice())
            .cloned()
            .ok_or_else(|| Error::Structure("document has no /AcroForm".into()))?;
        let af = self.doc.deref(&af_entry)?;
        let mut af_dict = af
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure("/AcroForm is not a dictionary".into()))?;
        let fields = match af_dict.get(b"Fields".as_slice()) {
            Some(o) => self
                .doc
                .deref(o)?
                .as_array()
                .map(|a| a.to_vec())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let field_ref = find_field(self.doc, &fields, name, "", 0)?
            .ok_or_else(|| Error::Structure(format!("form field {name:?} not found")))?;
        let mut fd = self
            .doc
            .resolve(field_ref)?
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure(format!("field {field_ref} is not a dictionary")))?;
        fd.insert(b"V".to_vec(), value.clone());
        // Widgets: the field itself when it carries /AP, else its /Kids.
        if let Object::Name(state) = &value {
            let mut widgets: Vec<(IndirectRef, Dict)> = Vec::new();
            if fd.contains_key(b"AP".as_slice()) {
                widgets.push((field_ref, fd.clone()));
            } else if let Some(kids) = fd.get(b"Kids".as_slice()) {
                if let Some(ks) = self.doc.deref(kids)?.as_array() {
                    for k in ks {
                        if let Some(kr) = k.as_ref() {
                            if let Some(kd) = self.doc.resolve(kr)?.as_dict() {
                                if kd.contains_key(b"AP".as_slice()) {
                                    widgets.push((kr, kd.clone()));
                                }
                            }
                        }
                    }
                }
            }
            for (wr, mut wd) in widgets {
                let has_state = wd
                    .get(b"AP".as_slice())
                    .and_then(|o| self.doc.deref(o).ok())
                    .and_then(|ap| ap.as_dict().and_then(|d| d.get(b"N".as_slice()).cloned()))
                    .and_then(|n| self.doc.deref(&n).ok())
                    .and_then(|n| n.as_dict().map(|d| d.contains_key(state.as_slice())))
                    .unwrap_or(false);
                let as_name = if has_state {
                    state.clone()
                } else {
                    b"Off".to_vec()
                };
                wd.insert(b"AS".to_vec(), Object::Name(as_name.clone()));
                if wr == field_ref {
                    fd.insert(b"AS".to_vec(), Object::Name(as_name));
                } else {
                    self.overlay
                        .insert((wr.number, wr.generation), Object::Dict(wd));
                }
            }
        }
        self.overlay
            .insert((field_ref.number, field_ref.generation), Object::Dict(fd));
        // /NeedAppearances true on the AcroForm dictionary (indirect:
        // its own object; direct: through the catalog).
        af_dict.insert(b"NeedAppearances".to_vec(), Object::Bool(true));
        match af_entry.as_ref() {
            Some(r) => {
                self.overlay
                    .insert((r.number, r.generation), Object::Dict(af_dict));
            }
            None => {
                let root = self
                    .doc
                    .trailer()
                    .get(b"Root".as_slice())
                    .and_then(|o| o.as_ref())
                    .ok_or_else(|| Error::Structure("trailer /Root is not a reference".into()))?;
                let mut cat = self.doc.catalog().clone();
                cat.insert(b"AcroForm".to_vec(), Object::Dict(af_dict));
                self.overlay
                    .insert((root.number, root.generation), Object::Dict(cat));
            }
        }
        self.changes.push(crate::signature::ChangeKind::FormFill);
        Ok(())
    }

    /// `pub(crate)` — the combined region-removal + metadata-strip
    /// call needs the same overlay+commit pipeline but with the
    /// writer's internal trailer/catalog overrides applied in the
    /// same rewrite. Behaviour is identical to [`Edit::rewrite`]
    /// when `overrides` is default.
    pub(crate) fn rewrite_with_overrides(
        mut self,
        overrides: &rewrite::RewriteOverrides,
    ) -> Result<Vec<u8>, Error> {
        self.commit()?;
        rewrite::rewrite_overlay_with(
            self.doc,
            &self.overlay,
            &self.synth_numbers,
            &self.limits,
            overrides,
        )
    }

    /// Build the annotation dicts + appearance streams and splice them
    /// into the overlay's page dicts.
    fn commit(&mut self) -> Result<(), Error> {
        // Move pending out so `commit_page` can borrow `self` mutably.
        let pending = std::mem::take(&mut self.pending);
        let mut by_page: HashMap<usize, Vec<PendingSquare>> = HashMap::new();
        for p in pending {
            by_page.entry(p.page_index).or_default().push(p);
        }
        for (page_index, squares) in by_page {
            self.commit_page(page_index, &squares)?;
        }
        Ok(())
    }

    /// Commit all pending `/Square`s for one page: read the page dict,
    /// build the new `/Annots` array (preserving existing entries),
    /// emit each annot + its appearance stream into the overlay, then
    /// store the modified page dict in the overlay.
    fn commit_page(&mut self, page_index: usize, squares: &[PendingSquare]) -> Result<(), Error> {
        let (page_ref, annots_ref) = self.annots_array_ref(page_index)?;

        for sq in squares {
            let annot_ref = self.emit_square(sq, annots_ref, page_ref)?;
            let arr = self
                .overlay
                .get_mut(&(annots_ref.number, annots_ref.generation))
                .and_then(|o| o.as_array_mut())
                .ok_or_else(|| Error::Structure("annot array missing from overlay".into()))?;
            arr.push(Object::Ref(annot_ref));
        }

        // Point the page dict at the (possibly new) array and store it.
        let _ = page_ref;
        Ok(())
    }

    /// Emit one `/Square` annotation dict and its appearance stream
    /// into the overlay; returns the annotation's synthetic ref.
    /// `annots_ref`/`page_ref` are kept for `/P` (the page backlink).
    fn emit_square(
        &mut self,
        sq: &PendingSquare,
        _annots_ref: IndirectRef,
        page_ref: IndirectRef,
    ) -> Result<IndirectRef, Error> {
        let annot_ref = self.alloc()?;
        let ap_ref = self.alloc()?;

        // ---- appearance stream ----
        //
        // Per §12.5.5 the annotation Rect is the annotation's *outer*
        // bounding box and "the border shall be drawn completely inside
        // the annotation rectangle" (§"Border Styles"). The appearance
        // form's own coordinate system is mapped onto Rect by
        // AA = Matrix × A (A aligning the transformed BBox to Rect).
        //
        // To keep the stroke width exact and fully inside Rect we make
        // the *transformed* appearance box coincide with Rect:
        //   * BBox = [0 0 w h]  (form-space local coords)
        //   * Matrix = translate(Rect.llx, Rect.lly)  (BBox-ll → Rect-ll)
        // then A = identity, so nothing is scaled and the declared
        // width is preserved exactly.
        //
        // The stroke is drawn on the *inner* rectangle — the centreline
        // is inset by half the width on every side, so the stroke's
        // outer edge coincides with Rect and nothing spills outside.
        let half = sq.width / 2.0;
        let (w_rect, h_rect) = (sq.rect.width(), sq.rect.height());
        // inner stroked rect in form space: [half half w-half h-half]
        let (ix0, iy0, ix1, iy1) = (half, half, w_rect - half, h_rect - half);
        let [r, g, b] = sq.rgb;
        let body = format!(
            "q\n{r} {g} {b} RG\n{w} w\n{x0} {y0} {wrect} {hrect} re\nS\nQ\n",
            w = fmt_num(sq.width),
            x0 = fmt_num(ix0),
            y0 = fmt_num(iy0),
            wrect = fmt_num(ix1 - ix0),
            hrect = fmt_num(iy1 - iy0),
        );
        let body_bytes = body.into_bytes();
        self.appearance_bytes += body_bytes.len() as u64;
        if self.appearance_bytes > self.limits.max_edit_annot_bytes {
            return Err(Error::LimitExceeded {
                limit: "max_edit_annot_bytes",
                max: self.limits.max_edit_annot_bytes,
            });
        }

        let mut ap_dict: Dict = Dict::new();
        ap_dict.insert(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
        ap_dict.insert(b"Subtype".to_vec(), Object::Name(b"Form".to_vec()));
        // BBox covers the whole form space [0 0 w h]; after Matrix it
        // coincides exactly with the annotation Rect.
        ap_dict.insert(
            b"BBox".to_vec(),
            Object::Array(
                [0.0, 0.0, w_rect, h_rect]
                    .iter()
                    .map(|&v| Object::Real(v))
                    .collect(),
            ),
        );
        // Matrix maps form-space origin → Rect.llx/lly. Combined with
        // BBox's own transform (A = identity since the transformed BBox
        // already equals Rect) this yields AA = Matrix, an exact
        // translate — the stroke width is not rescaled.
        ap_dict.insert(
            b"Matrix".to_vec(),
            Object::Array(
                [
                    Object::Real(1.0),
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(1.0),
                    Object::Real(sq.rect.llx),
                    Object::Real(sq.rect.lly),
                ]
                .to_vec(),
            ),
        );
        let ap_stream = Object::Stream {
            dict: ap_dict,
            data: body_bytes,
        };
        self.overlay
            .insert((ap_ref.number, ap_ref.generation), ap_stream);

        // ---- annotation dict ----
        let mut annot: Dict = Dict::new();
        annot.insert(b"Type".to_vec(), Object::Name(b"Annot".to_vec()));
        annot.insert(b"Subtype".to_vec(), Object::Name(b"Square".to_vec()));
        annot.insert(
            b"Rect".to_vec(),
            Object::Array(
                [sq.rect.llx, sq.rect.lly, sq.rect.urx, sq.rect.ury]
                    .iter()
                    .map(|&v| Object::Real(v))
                    .collect(),
            ),
        );
        // /C = stroke colour (RGB). No /IC — interior is unfilled.
        annot.insert(
            b"C".to_vec(),
            Object::Array(sq.rgb.iter().map(|&v| Object::Real(v)).collect()),
        );
        // Border width via /BS (preferred over legacy /Border).
        let mut bs: Dict = Dict::new();
        bs.insert(b"W".to_vec(), Object::Real(sq.width));
        bs.insert(b"S".to_vec(), Object::Name(b"S".to_vec())); // solid
        annot.insert(b"BS".to_vec(), Object::Dict(bs));
        // Print (bit 3) + view (default) so it shows on screen and on paper.
        annot.insert(b"F".to_vec(), Object::Integer(4));
        annot.insert(b"P".to_vec(), Object::Ref(page_ref));
        // /AP /N points at the normal-appearance form.
        let mut ap: Dict = Dict::new();
        ap.insert(b"N".to_vec(), Object::Ref(ap_ref));
        annot.insert(b"AP".to_vec(), Object::Dict(ap));

        self.overlay.insert(
            (annot_ref.number, annot_ref.generation),
            Object::Dict(annot),
        );
        Ok(annot_ref)
    }
}

/// Format a user-space coordinate for the content stream.
/// Integer when possible; else trimmed fixed-point (never exponent,
/// never NaN — inputs are validated finite upstream).
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.4}");
        let t = s.trim_end_matches('0').trim_end_matches('.');
        t.to_string()
    }
}

impl<'a> std::fmt::Debug for Edit<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Edit")
            .field("pending", &self.pending.len())
            .field("overlay_objects", &self.overlay.len())
            .finish()
    }
}

/// Find a field by fully qualified name (`/T` chain joined by `.`),
/// returning its reference; direct (non-indirect) field dicts cannot
/// be updated in place and are reported as such.
fn find_field(
    doc: &Document,
    fields: &[Object],
    want: &str,
    prefix: &str,
    depth: usize,
) -> Result<Option<IndirectRef>, Error> {
    if depth > 32 {
        return Ok(None);
    }
    for f in fields {
        let fo = doc.deref(f)?;
        let Some(fd) = fo.as_dict() else { continue };
        let t = fd
            .get(b"T".as_slice())
            .and_then(|o| o.as_string())
            .map(crate::render::pdf_text_string)
            .unwrap_or_default();
        let full = match (prefix.is_empty(), t.is_empty()) {
            (true, _) => t.clone(),
            (false, true) => prefix.to_string(),
            (false, false) => format!("{prefix}.{t}"),
        };
        if full == want {
            return match f.as_ref() {
                Some(r) => Ok(Some(r)),
                None => Err(Error::Unsupported {
                    feature: "form field value",
                    detail: format!(
                        "field {want:?} is a direct object and cannot be updated in place"
                    ),
                }),
            };
        }
        if let Some(kids) = fd.get(b"Kids".as_slice()) {
            if let Some(ks) = doc.deref(kids)?.as_array() {
                if let Some(r) = find_field(doc, ks, want, &full, depth + 1)? {
                    return Ok(Some(r));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    //! Allocator behaviour that can't be observed after output
    //! renumbering — the synthetic *input* number space is what must
    //! not collide.

    use super::*;
    use crate::limits::Limits;

    /// These tests exercise `alloc` through the public `Edit` API
    /// against real fixtures that place objects at the synthetic
    /// boundary — the allocator itself is not a public interface.
    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    /// Build a minimal 1-page PDF plus extra objects declared at
    /// `(num, gen)` — gen may be non-zero to reproduce the
    /// generation-mismatch cases.
    fn base_pdf(extra: &[(u32, u16, &[u8])]) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, u16, usize)> = Vec::new();
        let mut push = |n: u32, g: u16, body: &[u8]| {
            offs.push((n, g, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} {g} obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, 0, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, 0, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            0,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >>",
        );
        for (n, g, b) in extra {
            push(*n, *g, b);
        }
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        // object 0 free + entries for each declared object via its own
        // subsection (sparse numbering is legal).
        parts.push(b"xref\n".to_vec());
        parts.push(b"0 4\n0000000000 65535 f \n".to_vec());
        for n in 1..4u32 {
            if let Some((_, _, o)) = offs.iter().find(|(on, _, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        for (n, g, _) in extra {
            if let Some((_, _, o)) = offs.iter().find(|(on, og, _)| *on == *n && *og == *g) {
                parts.push(format!("{n} 1\n{o:010} {g:05} n \n").into_bytes());
            }
        }
        parts.push(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", u32::MAX).into_bytes());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    }

    #[test]
    fn alloc_skips_existing_input_object() {
        // Real object exactly at SYNTH_TOP — allocator must skip it.
        let src = base_pdf(&[(u32::MAX - 1, 0, b"0")]);
        let d = doc(&src);
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        let r = e.alloc().unwrap();
        assert_ne!(r.number, u32::MAX - 1);
    }

    #[test]
    fn alloc_skips_run_of_existing() {
        let src = base_pdf(&[
            (u32::MAX - 1, 0, b"0"),
            (u32::MAX - 2, 0, b"0"),
            (u32::MAX - 3, 0, b"0"),
        ]);
        let d = doc(&src);
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        // First three candidates are taken; next must be MAX-4 or lower.
        let r = e.alloc().unwrap();
        assert!(r.number <= u32::MAX - 4, "got {}", r.number);
    }

    #[test]
    fn alloc_never_repeats() {
        let src = base_pdf(&[]);
        let d = doc(&src);
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        let r1 = e.alloc().unwrap();
        // Simulate the caller marking r1 used in the overlay.
        e.overlay.insert((r1.number, r1.generation), Object::Null);
        let r2 = e.alloc().unwrap();
        assert_ne!(r1, r2);
        assert_eq!(r2.number, r1.number - 1);
    }

    #[test]
    fn alloc_respects_object_limit() {
        let src = base_pdf(&[]);
        let d = doc(&src);
        let mut e = Edit::new(
            &d,
            Limits {
                max_edit_objects: 1,
                ..Default::default()
            },
        )
        .unwrap();
        e.alloc().unwrap();
        assert!(matches!(e.alloc(), Err(Error::LimitExceeded { .. })));
    }

    // --- generation mismatch / dangling-ref protection ---

    /// A: input has a *valid* object at (n, 1) and a *bad* ref to
    /// (n, 0). The bad ref is an input defect; it must fail, not be
    /// resolved to a synthetic. The allocator must not pick n either.
    #[test]
    fn alloc_skips_number_reserved_at_other_generation() {
        // (u32::MAX-1, gen 1) is a real object; (u32::MAX-1, 0) is a
        // dangling ref inside the page dict scenario.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, u16, usize)> = Vec::new();
        let mut push = |n: u32, g: u16, body: &[u8]| {
            offs.push((n, g, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} {g} obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, 0, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, 0, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        // Page /Annots references (u32::MAX-1, 0) — that entry does NOT
        // exist (only gen 1 does). A dangling input ref.
        push(
            3,
            0,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots {} 0 R >>",
                u32::MAX - 1
            )
            .as_bytes(),
        );
        push(
            u32::MAX - 1,
            1,
            b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (gen1) >>",
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
        for n in 1..4u32 {
            if let Some((_, _, o)) = offs.iter().find(|(on, _, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        if let Some((_, _, o)) = offs
            .iter()
            .find(|(on, og, _)| *on == u32::MAX - 1 && *og == 1)
        {
            parts.push(format!("{} 1\n{o:010} 00001 n \n", u32::MAX - 1).into_bytes());
        }
        parts.push(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", u32::MAX).into_bytes());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let src = parts.concat();
        let d = doc(&src);
        // The edit must fail — the dangling (n,0) ref is an input bug,
        // and a synthetic must not land on the reserved number n.
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [0.0; 3],
            1.0,
        )
        .unwrap();
        let r = e.rewrite();
        assert!(r.is_err(), "dangling (n,0) ref must fail, got {r:?}");
    }

    /// B: a *valid* (n, 1) object referenced correctly — annotate and
    /// the object must survive rewrite unchanged.
    #[test]
    fn valid_nonzero_generation_object_survives() {
        // (u32::MAX-1, 1) is a real annot referenced by the page's
        // /Annots — perfectly valid, just a nonzero generation.
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, u16, usize)> = Vec::new();
        let mut push = |n: u32, g: u16, body: &[u8]| {
            offs.push((n, g, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} {g} obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, 0, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, 0, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            0,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots [{} 1 R] >>",
                u32::MAX - 1
            )
            .as_bytes(),
        );
        push(
            u32::MAX - 1,
            1,
            b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (gen1) >>",
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
        for n in 1..4u32 {
            if let Some((_, _, o)) = offs.iter().find(|(on, _, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        if let Some((_, _, o)) = offs
            .iter()
            .find(|(on, og, _)| *on == u32::MAX - 1 && *og == 1)
        {
            parts.push(format!("{} 1\n{o:010} 00001 n \n", u32::MAX - 1).into_bytes());
        }
        parts.push(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", u32::MAX).into_bytes());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let src = parts.concat();
        let d = doc(&src);
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [0.0; 3],
            1.0,
        )
        .unwrap();
        let out = e.rewrite().unwrap();
        let d2 = doc(&out);
        // Reach the page dict through the catalog — after renumbering
        // the page's own object_ref() may not carry the new number,
        // so resolve via /Pages /Kids.
        let catalog = d2.catalog();
        let pages_ref = catalog.get(b"Pages".as_slice()).unwrap().as_ref().unwrap();
        let pages_obj = d2.resolve(pages_ref).unwrap();
        let kids = pages_obj
            .as_dict()
            .unwrap()
            .get(b"Kids".as_slice())
            .unwrap()
            .as_array()
            .unwrap();
        let page_ref = kids[0].as_ref().unwrap();
        let page_obj = d2.resolve(page_ref).unwrap();
        let annots_obj = page_obj
            .as_dict()
            .unwrap()
            .get(b"Annots".as_slice())
            .unwrap()
            .clone();
        // /Annots may be a direct array or an indirect ref.
        let annots: Vec<Object> = match &annots_obj {
            Object::Array(a) => a.clone(),
            Object::Ref(r) => d2.resolve(*r).unwrap().as_array().unwrap().to_vec(),
            other => panic!("unexpected /Annots: {other:?}"),
        };
        assert_eq!(annots.len(), 2, "original + new square");
        // Original (n,1) annot preserved verbatim.
        let first_obj = d2.resolve(annots[0].as_ref().unwrap()).unwrap();
        let first = first_obj.as_dict().unwrap().clone();
        assert_eq!(
            first.get(b"Subtype".as_slice()).unwrap().as_name().unwrap(),
            b"Text"
        );
        assert_eq!(
            first.get(b"Contents".as_slice()).unwrap(),
            &Object::String(b"gen1".to_vec())
        );
    }

    /// C: a dangling ref to a number *absent* from the xref that
    /// coincides with a synthetic candidate — must still fail, never
    /// resolve to the synthetic body.
    #[test]
    fn dangling_ref_to_absent_number_not_repaired() {
        // Page /Annots refs (u32::MAX-1, 0) which doesn't exist in the
        // input xref at all — a true dangling ref.
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
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Annots {} 0 R >>",
                u32::MAX - 1
            )
            .as_bytes(),
        );
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
        for n in 1..4u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            }
        }
        parts.push(format!("trailer\n<< /Size {} /Root 1 0 R >>\n", u32::MAX).into_bytes());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        let src = parts.concat();
        let d = doc(&src);
        let mut e = Edit::new(&d, Limits::default()).unwrap();
        e.add_square(
            0,
            Rect {
                llx: 0.0,
                lly: 0.0,
                urx: 9.0,
                ury: 9.0,
            },
            [0.0; 3],
            1.0,
        )
        .unwrap();
        let r = e.rewrite();
        // Must fail — resolving the dangling ref must not silently
        // succeed because a synthetic landed on that number.
        assert!(r.is_err(), "dangling ref must fail, got {r:?}");
    }
}
