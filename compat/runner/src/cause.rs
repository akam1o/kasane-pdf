//! Structured failure-cause classification.
//!
//! Every stage failure is mapped to a stable `Cause` id like
//! `render.extgstate.unsupported` or `parse.encryption.unsupported`.
//! The id is **stable**: derived from `Error::kind()` plus a feature
//! tag, never from the free-text message (so message rewording does
//! not break the analysis).
//!
//! Shape:  `<stage>.<category>[.<specific>].<class>`
//!   stage    — parse | render | text | rewrite
//!   category — font | image | colorspace | extgstate | shading |
//!              pattern | annotation | oc | xobject | stream | filter |
//!              encryption | named-dest | userunit | ctm | version |
//!              toUnicode | actualtext | structure | io | limit |
//!              unknown
//!   class    — unsupported | error | mismatch
//!
//! The probe (`probe.rs`) emits *more specific* ids than the engine's
//! own error strings (e.g. `render.font.type1` vs the engine's
//! generic `"font subtype"`), so the blocker map is finer-grained.

use kasane_pdf::Error;

/// A classified cause. `id` is the machine-stable key; `feature` is
/// the human-readable feature for reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cause {
    pub id: String,
    pub feature: String,
}

impl Cause {
    fn new(id: impl Into<String>, feature: impl Into<String>) -> Self {
        Cause {
            id: id.into(),
            feature: feature.into(),
        }
    }
    /// Fallback for errors that don't fit the taxonomy. Currently the
    /// match is exhaustive, but kept so a future `Error` variant that
    /// slips through still produces a stable id rather than panicking.
    #[allow(dead_code)]
    pub fn unknown(stage: &str, detail: &str) -> Self {
        Cause::new(
            format!("{stage}.unknown.error"),
            format!("unclassified: {detail}"),
        )
    }
}

/// Map an engine `Error` to a `Cause`. `stage` scopes the id.
/// `feature_hint` is the probe's finer tag when the error is generic.
pub fn classify(stage: &str, e: &Error) -> Cause {
    match e {
        Error::Unsupported { feature, detail } => Cause::new(
            format!("{stage}.{}.unsupported", cause_category(feature)),
            format!("{feature}: {detail}"),
        ),
        Error::Encrypted => Cause::new(
            format!("{stage}.encryption.unsupported"),
            "encrypted".to_string(),
        ),
        Error::NotPdf => Cause::new(format!("{stage}.notpdf.error"), String::from("not a PDF")),
        Error::Parse(m) => Cause::new(format!("{stage}.parse.error"), m.clone()),
        // Truncated / malformed xref tables land here. Keep it under a
        // stable id so a reworded `context` string can't re-bucket it.
        Error::UnexpectedEof { .. } => Cause::new(format!("{stage}.eof.error"), e.to_string()),
        Error::Structure(m) => Cause::new(format!("{stage}.structure.error"), m.clone()),
        Error::MissingReference(_) => {
            Cause::new(format!("{stage}.missingref.error"), e.to_string())
        }
        Error::CycleDetected { .. } => Cause::new(format!("{stage}.cycle.error"), e.to_string()),
        Error::LimitExceeded { .. } => Cause::new(format!("{stage}.limit.error"), e.to_string()),
        Error::Io(_) => Cause::new(format!("{stage}.io.error"), e.to_string()),
    }
}

/// Turn an engine `feature` tag into a short, stable category slug.
/// Keys on the exact strings emitted by the engine (stable contract),
/// falling back to a sanitised slug so unknown features still bucket.
pub fn cause_category(feature: &str) -> &'static str {
    // Match on the stable feature text. These strings are part of the
    // engine's error contract; add new arms when the engine grows new
    // feature tags.
    match feature {
        // fonts
        "font subtype" => "font",
        "symbolic font" => "font",
        "non-embedded TrueType font" => "font.nonembedded",
        "non-embedded Type1 font" => "font.nonembedded",
        "non-embedded CIDFont" => "font.nonembedded",
        "font without /Encoding" => "font.encoding",
        "font encoding" => "font.encoding",
        "font encoding dictionary without /BaseEncoding" => "font.encoding",
        "Type 0 font encoding" => "font.encoding",
        "Type 0 font /Encoding CMap" => "font.encoding",
        "Type 0 font with multiple descendant fonts" => "font",
        "CIDFontType0 (CFF/Type 1) descendant" => "font.cff",
        "CIDFont subtype" => "font",
        "vertical writing (/Encoding /Identity-V)" => "font.vertical",
        // graphics state / colour
        "extended graphics state (gs)" => "extgstate",
        "non-device colour space" => "colorspace",
        "pattern colour (SCN/scn name operand)" => "pattern",
        "shading (sh)" => "shading",
        // xobjects / images
        "XObject subtype" => "xobject",
        "group XObject (/Group in a form dictionary)" => "xobject.group",
        "optional content on a form XObject (/OC)" => "xobject.oc",
        "reference XObject (/Ref in a form dictionary)" => "xobject.ref",
        "image colour space" => "image.colorspace",
        "image stream filter" => "image.filter",
        "image attribute" => "image",
        "image BitsPerComponent" => "image.bpc",
        "image mask (/ImageMask true)" => "image.mask",
        "image transform" => "image.transform",
        "image with non-default /Decode" => "image.decode",
        "inline image (BI/ID/EI)" => "image.inline",
        "DCTDecode /ColorTransform" => "image.dct",
        "DCTDecode /DecodeParms" => "image.dct",
        "DCTDecode Adobe APP14 transform" => "image.dct",
        "DCTDecode Adobe APP14 transform=2 (YCCK)" => "image.dct",
        "DCTDecode coding process" => "image.dct",
        "DCTDecode image" => "image.dct",
        "DCTDecode raw RGB on subsampled JPEG" => "image.dct",
        "DCTDecode /ColorTransform 0 on subsampled JPEG" => "image.dct",
        // marked content / OC / annotations
        "optional content (/OC or /OCMD marked content)" => "oc",
        "replacement text (/Span <</ActualText …>>)" => "actualtext",
        "page annotations (/Annots)" => "annotation",
        // text shaping / unicode
        "text rendering mode" => "text.mode",
        "ToUnicode source code width" => "tounicode",
        "ToUnicode mixed code lengths" => "tounicode",
        "ToUnicode codespace" => "tounicode",
        "ToUnicode usecmap/usefont" => "tounicode",
        "ToUnicode /UseCMap" => "tounicode",
        // stream / structure
        "stream filter" => "filter",
        "xref stream field width" => "filter",
        "FlateDecode predictor" => "filter",
        "FlateDecode BitsPerComponent" => "filter",
        "FlateDecode Colors" => "filter",
        "PNG predictor BitsPerComponent" => "filter",
        "TIFF predictor BitsPerComponent" => "filter",
        // document-level
        "/UserUnit other than 1" => "userunit",
        "singular CTM (determinant 0)" => "ctm",
        "named destination" => "named-dest",
        "signed document" => "signature",
        "signed signature field" => "signature",
        "XFA form" => "xfa",
        "OpenAction JavaScript" => "javascript",
        "JavaScript name tree" => "javascript",
        "catalog entry outside profile" => "profile",
        "PDF version" => "version",
        "edit session" => "edit",
        // removal-family blockers — group under "region" so they don't
        // inflate the feature ranking (they're the tool's own rules,
        // not a missing engine capability).
        _ if feature.starts_with("text removal") => "region",
        _ if feature.starts_with("image removal") => "region",
        _ if feature.starts_with("region removal") => "region",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported(feature: &'static str) -> Error {
        Error::Unsupported {
            feature,
            detail: "test".into(),
        }
    }

    #[test]
    fn unsupported_feature_maps_to_stable_id() {
        let c = classify("render", &unsupported("extended graphics state (gs)"));
        assert_eq!(c.id, "render.extgstate.unsupported");
        assert!(c.feature.contains("extended graphics state"));
    }

    #[test]
    fn stage_scopes_the_id() {
        let e = unsupported("shading (sh)");
        assert_eq!(classify("render", &e).id, "render.shading.unsupported");
        assert_eq!(classify("text", &e).id, "text.shading.unsupported");
        assert_eq!(classify("rewrite", &e).id, "rewrite.shading.unsupported");
    }

    #[test]
    fn font_subtype_detail_does_not_change_the_id() {
        // The id must be stable across different detail text — the
        // category comes from the feature tag, not the message.
        let e1 = Error::Unsupported {
            feature: "font subtype",
            detail: "/Type1 — only /TrueType...".into(),
        };
        let e2 = Error::Unsupported {
            feature: "font subtype",
            detail: "/Type3 — only /TrueType...".into(),
        };
        assert_eq!(classify("render", &e1).id, classify("render", &e2).id);
        assert_eq!(classify("render", &e1).id, "render.font.unsupported");
    }

    #[test]
    fn encrypted_is_not_an_error() {
        let c = classify("parse", &Error::Encrypted);
        assert_eq!(c.id, "parse.encryption.unsupported");
    }

    #[test]
    fn known_error_kinds_bucket_sensibly() {
        assert_eq!(classify("parse", &Error::NotPdf).id, "parse.notpdf.error");
        assert_eq!(
            classify("parse", &Error::Parse("bad".into())).id,
            "parse.parse.error"
        );
        assert_eq!(
            classify("render", &Error::Structure("bad".into())).id,
            "render.structure.error"
        );
        assert_eq!(
            classify(
                "parse",
                &Error::UnexpectedEof {
                    offset: 10,
                    context: "xref stream data"
                }
            )
            .id,
            "parse.eof.error"
        );
    }

    #[test]
    fn unknown_feature_still_buckets() {
        let c = classify("render", &unsupported("some brand new feature"));
        assert_eq!(c.id, "render.unknown.unsupported");
    }

    #[test]
    fn removal_family_goes_to_region() {
        let c = classify("rewrite", &unsupported("region removal on a page with sh"));
        assert_eq!(c.id, "rewrite.region.unsupported");
        let c2 = classify("rewrite", &unsupported("text removal with Tz = 0"));
        assert_eq!(c2.id, "rewrite.region.unsupported");
    }

    #[test]
    fn cause_category_covers_document_level() {
        assert_eq!(cause_category("named destination"), "named-dest");
        assert_eq!(cause_category("/UserUnit other than 1"), "userunit");
        assert_eq!(cause_category("PDF version"), "version");
        assert_eq!(cause_category("page annotations (/Annots)"), "annotation");
    }
}
