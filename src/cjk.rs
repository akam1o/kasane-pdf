//! CJK fallback assets, behind the `cjk-fallback` Cargo feature:
//! the Adobe-Japan1 predefined CMaps (§9.7.5.2, Table 118), the
//! Adobe-Japan1-UCS2 CID → Unicode mapping, and a bundled Japanese
//! fallback face (Noto Sans JP Regular, SIL OFL 1.1) used to draw
//! *non-embedded* CID-keyed fonts of the Adobe-Japan1 ordering.
//!
//! Nothing here touches host fonts: with the feature off every lookup
//! returns `None` and the engine refuses those fonts exactly as
//! before; with it on, the assets are compiled into the binary from
//! `assets/cjk/` (fetched by `scripts/fetch-cjk-assets.sh`, pinned
//! upstream sources, licences alongside). A fallback draw is always
//! reported through `RenderedPage::font_substitutions` with
//! `SubstKind::CjkFallback`, so a substituted render is never mistaken
//! for a faithful one.

/// Whether the fallback assets are compiled in.
pub fn cjk_fallback_enabled() -> bool {
    cfg!(feature = "cjk-fallback")
}

/// Whether `name` is a predefined CMap the engine can load (the
/// Identity CMaps always are; the Adobe-Japan1 set with the feature).
pub fn predefined_cmap_known(name: &[u8]) -> bool {
    name == b"Identity-H" || name == b"Identity-V" || predefined_cmap(name).is_some()
}

#[cfg(feature = "cjk-fallback")]
macro_rules! cmaps {
    ($($name:literal),* $(,)?) => {
        pub(crate) fn predefined_cmap(name: &[u8]) -> Option<&'static [u8]> {
            match name {
                $(n if n == $name.as_bytes() => Some(include_bytes!(concat!("../assets/cjk/cmap/", $name))),)*
                _ => None,
            }
        }
        const PREDEFINED_CMAPS: &[&str] = &[$($name),*];
    };
}

#[cfg(feature = "cjk-fallback")]
cmaps!(
    "H",
    "V",
    "90ms-RKSJ-H",
    "90ms-RKSJ-V",
    "90msp-RKSJ-H",
    "90msp-RKSJ-V",
    "90pv-RKSJ-H",
    "90pv-RKSJ-V",
    "83pv-RKSJ-H",
    "Add-RKSJ-H",
    "Add-RKSJ-V",
    "Ext-RKSJ-H",
    "Ext-RKSJ-V",
    "EUC-H",
    "EUC-V",
    "UniJIS-UCS2-H",
    "UniJIS-UCS2-V",
    "UniJIS-UCS2-HW-H",
    "UniJIS-UCS2-HW-V",
    "UniJIS-UTF16-H",
    "UniJIS-UTF16-V",
    "UniJIS2004-UTF16-H",
    "UniJIS2004-UTF16-V",
    "UniJISX0213-UTF32-H",
    "UniJISX0213-UTF32-V",
    "UniJISX02132004-UTF32-H",
    "UniJISX02132004-UTF32-V",
);

#[cfg(not(feature = "cjk-fallback"))]
pub(crate) fn predefined_cmap(_name: &[u8]) -> Option<&'static [u8]> {
    None
}

#[cfg(not(feature = "cjk-fallback"))]
const PREDEFINED_CMAPS: &[&str] = &[];

/// The predefined CMap names bundled by the `cjk-fallback` feature
/// (empty without it; the Identity CMaps are always available).
pub fn predefined_cmap_names() -> &'static [&'static str] {
    PREDEFINED_CMAPS
}

/// The bundled fallback face for a CID ordering: `(name, program)`.
/// Only `Japan1` is covered.
pub(crate) fn fallback_font(ordering: &[u8]) -> Option<(&'static str, &'static [u8])> {
    #[cfg(feature = "cjk-fallback")]
    {
        if ordering == b"Japan1" {
            return Some((
                "NotoSansJP-Regular",
                include_bytes!("../assets/cjk/NotoSansJP-Regular.otf"),
            ));
        }
    }
    let _ = ordering;
    None
}

/// CID → Unicode for an ordering, parsed once per process.
pub(crate) fn cid_to_unicode(
    ordering: &[u8],
) -> Option<&'static std::collections::HashMap<u32, char>> {
    #[cfg(feature = "cjk-fallback")]
    {
        use std::collections::HashMap;
        use std::sync::OnceLock;
        static JAPAN1: OnceLock<HashMap<u32, char>> = OnceLock::new();
        if ordering == b"Japan1" {
            return Some(JAPAN1.get_or_init(|| {
                let data: &[u8] = include_bytes!("../assets/cjk/cmap/Adobe-Japan1-UCS2");
                let limits = crate::limits::Limits::default();
                let mut map = HashMap::new();
                if let Ok(tu) = crate::tounicode::ToUnicode::parse(data, &limits) {
                    for (cid, units) in tu.entries() {
                        if let Some(ch) = char::decode_utf16(units.iter().copied())
                            .next()
                            .and_then(|r| r.ok())
                        {
                            map.insert(cid, ch);
                        }
                    }
                }
                map
            }));
        }
    }
    let _ = ordering;
    None
}
