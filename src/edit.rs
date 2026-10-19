//! Minimal annotation-edit session — M9 "add a rectangle annotation".
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
//! Scope (M9): `/Subtype /Square` annotations only — opaque RGB stroke
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
use std::collections::HashMap;

/// Synthetic indirect-object numbers start here and count *up*. Real
/// objects occupy low numbers; a document whose trailer `/Size`
/// exceeds `SYNTH_BASE` is refused rather than risk a collision.
const SYNTH_BASE: u32 = u32::MAX - 1_000_000;

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
    overlay: HashMap<(u32, u16), Object>,
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
    limits: Limits,
}

impl<'a> Edit<'a> {
    pub(crate) fn new(doc: &'a Document, limits: Limits) -> Result<Self, Error> {
        // Refuse a document whose own objects could collide with the
        // synthetic space. Trailer /Size is the declared object count.
        let declared = doc
            .trailer()
            .get(b"Size".as_slice())
            .and_then(|o| o.as_number())
            .unwrap_or(0.0);
        if declared as u64 >= SYNTH_BASE as u64 {
            return Err(Error::Unsupported {
                feature: "edit session",
                detail: format!(
                    "document declares /Size {declared} — synthetic object space would collide"
                ),
            });
        }
        Ok(Edit {
            doc,
            overlay: HashMap::new(),
            next_synth: SYNTH_BASE,
            pending: Vec::new(),
            annots_by_page: HashMap::new(),
            appearance_bytes: 0,
            synth_objects: 0,
            limits,
        })
    }

    /// Allocate a synthetic indirect-object number.
    fn alloc(&mut self) -> Result<IndirectRef, Error> {
        self.synth_objects += 1;
        if self.synth_objects > self.limits.max_edit_objects {
            return Err(Error::LimitExceeded {
                limit: "max_edit_objects",
                max: self.limits.max_edit_objects,
            });
        }
        let n = self.next_synth;
        self.next_synth += 1;
        Ok(IndirectRef::new(n, 0))
    }

    /// Add a `/Subtype /Square` rectangle annotation to page
    /// `page_index` (0-based, matching [`Document::pages`] order).
    ///
    /// `rect` is in un-rotated PDF user space (+x right, +y up) — the
    /// same coordinates `M6.1` extraction reports. `rgb` is the
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
        // /UserUnit ≠ 1 changes the meaning of user-space units; M9
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
    pub fn rewrite(mut self) -> Result<Vec<u8>, Error> {
        self.commit()?;
        rewrite::rewrite_overlay(self.doc, &self.overlay, &self.limits)
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
        let page = self.doc.pages()[page_index].clone();
        let page_ref = page.object_ref().ok_or_else(|| {
            Error::Structure("cannot annotate a page that has no indirect reference".into())
        })?;
        let page_obj = self.doc.resolve(page_ref)?;
        let mut page_dict = page_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| Error::Structure(format!("page {page_ref} is not a dictionary")))?;

        // Get or create the /Annots array object for this page.
        let annots_ref = match self.annots_by_page.get(&page_ref) {
            Some(&r) => r,
            None => {
                // Read existing /Annots (may be direct array, indirect
                // ref, or absent). We always materialise a fresh array
                // object so shared arrays are never mutated in place.
                let existing: Vec<Object> = match page_dict.get(b"Annots".as_slice()) {
                    Some(o) => {
                        let resolved = self.doc.deref(o)?;
                        match resolved {
                            Object::Array(a) => a,
                            other => {
                                return Err(Error::Structure(format!(
                                    "page /Annots is not an array ({other:?})"
                                )))
                            }
                        }
                    }
                    None => Vec::new(),
                };
                let arr_ref = self.alloc()?;
                self.overlay.insert(
                    (arr_ref.number, arr_ref.generation),
                    Object::Array(existing),
                );
                self.annots_by_page.insert(page_ref, arr_ref);
                arr_ref
            }
        };

        // Append each new annotation to the shared array.
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
        page_dict.insert(b"Annots".to_vec(), Object::Ref(annots_ref));
        self.overlay.insert(
            (page_ref.number, page_ref.generation),
            Object::Dict(page_dict),
        );
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
        // Stroke is centred on the path edge; pad the BBox by half the
        // width on every side so the stroke is never clipped.
        let half = sq.width / 2.0;
        let bbox = [
            sq.rect.llx - half,
            sq.rect.lly - half,
            sq.rect.urx + half,
            sq.rect.ury + half,
        ];
        let [r, g, b] = sq.rgb;
        let body = format!(
            "q\n{r} {g} {b} RG\n{w} w\n{x0} {y0} {wrect} {hrect} re\nS\nQ\n",
            w = fmt_num(sq.width),
            x0 = fmt_num(sq.rect.llx),
            y0 = fmt_num(sq.rect.lly),
            wrect = fmt_num(sq.rect.width()),
            hrect = fmt_num(sq.rect.height()),
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
        ap_dict.insert(
            b"BBox".to_vec(),
            Object::Array(bbox.iter().map(|&v| Object::Real(v)).collect()),
        );
        // Identity matrix is the default — omit /Matrix.
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
