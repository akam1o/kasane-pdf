//! Page-tree results: [`Page`] and [`Rect`].
//!
//! Pages are produced by walking the page tree (§7.7.3.3) with inherited
//! attributes already applied: a [`Page`]'s `media_box`, `crop_box`,
//! `rotate` and `resources` reflect the node's own attributes merged over
//! its ancestors' inheritable attributes (Table 30 lists `Resources`,
//! `MediaBox`, `CropBox` and `Rotate` as inheritable).

use crate::object::{IndirectRef, Object};
use std::fmt;

/// A PDF rectangle: `[llx lly urx ury]` in default user-space units
/// (1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Lower-left x.
    pub llx: f64,
    /// Lower-left y.
    pub lly: f64,
    /// Upper-right x.
    pub urx: f64,
    /// Upper-right y.
    pub ury: f64,
}

impl Rect {
    /// Width (`urx - llx`); may be negative for malformed rectangles.
    pub fn width(&self) -> f64 {
        self.urx - self.llx
    }

    /// Height (`ury - lly`); may be negative for malformed rectangles.
    pub fn height(&self) -> f64 {
        self.ury - self.lly
    }

    /// Build a rectangle from a 4-element array of numbers. Returns
    /// `None` for any other shape.
    pub(crate) fn from_object(obj: &Object) -> Option<Rect> {
        let arr = obj.as_array()?;
        if arr.len() != 4 {
            return None;
        }
        Some(Rect {
            llx: arr[0].as_number()?,
            lly: arr[1].as_number()?,
            urx: arr[2].as_number()?,
            ury: arr[3].as_number()?,
        })
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{} {} {} {}]",
            fmt_num(self.llx),
            fmt_num(self.lly),
            fmt_num(self.urx),
            fmt_num(self.ury)
        )
    }
}

/// Format a coordinate as an integer when it is one, else with decimals.
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// One page of the document, in page-tree order.
///
/// `resources` and `contents` are stored as the (single-level resolved)
/// raw objects from the file. Their meaning — fonts, images, drawing
/// commands — is out of scope here.
#[derive(Debug, Clone)]
pub struct Page {
    index: usize,
    object_ref: Option<IndirectRef>,
    media_box: Rect,
    crop_box: Rect,
    rotate: i64,
    resources: Option<Object>,
    contents: Option<Object>,
}

impl Page {
    pub(crate) fn new(
        index: usize,
        object_ref: Option<IndirectRef>,
        media_box: Rect,
        crop_box: Rect,
        rotate: i64,
        resources: Option<Object>,
        contents: Option<Object>,
    ) -> Self {
        Page {
            index,
            object_ref,
            media_box,
            crop_box,
            rotate,
            resources,
            contents,
        }
    }

    /// Zero-based position in page order.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The page object's own indirect reference, if it was indirect
    /// (direct-dict pages in the Kids array have none).
    pub fn object_ref(&self) -> Option<IndirectRef> {
        self.object_ref
    }

    /// Effective media box (inherited attributes applied). Required by
    /// the spec; loading fails if no definition is reachable.
    pub fn media_box(&self) -> Rect {
        self.media_box
    }

    /// Effective crop box; defaults to the media box when undefined.
    pub fn crop_box(&self) -> Rect {
        self.crop_box
    }

    /// Effective rotation in degrees (multiple of 90, default 0).
    pub fn rotate(&self) -> i64 {
        self.rotate
    }

    /// Effective resources object (inherited if not set on the page).
    /// May be a dictionary or a reference-resolved value.
    pub fn resources(&self) -> Option<&Object> {
        self.resources.as_ref()
    }

    /// The page's `/Contents` object, unresolved beyond one level.
    /// Usually an indirect reference or an array of them.
    pub fn contents(&self) -> Option<&Object> {
        self.contents.as_ref()
    }
}
