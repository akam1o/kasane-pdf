//! Transparency groups whose blending colour space is DeviceCMYK
//! (ISO 32000-1 §11.3.4, §11.4.7, Table 147).
//!
//! Colours painted into such a group are converted to DeviceCMYK and
//! blended there; a separable blend mode works on the complements of
//! the components: c = 1 − B(1 − cb, 1 − cs). The group result is then
//! converted for display by §10.3.5 (R = 1 − min(1, C + K), …). The
//! expected colours are those formulas evaluated here, independently of
//! the renderer. Blending the same colours after converting them to RGB
//! gives other results for every mode but Normal — and for Normal too
//! once C + K exceeds 1, which the conversion clips.
//!
//! What the renderer does not blend in DeviceCMYK (the non-separable
//! modes, knockout groups, a nested group in another colour space, a
//! pattern selected before the group) is painted as a whole exactly as
//! without the group colour space, and reported.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Limits, RenderOptions};

const SEPARABLE: [&str; 12] = [
    "Normal",
    "Multiply",
    "Screen",
    "Overlay",
    "Darken",
    "Lighten",
    "ColorDodge",
    "ColorBurn",
    "HardLight",
    "SoftLight",
    "Difference",
    "Exclusion",
];
const NON_SEPARABLE: [&str; 4] = ["Hue", "Saturation", "Color", "Luminosity"];

/// §11.3.5, Table 136, on additive values.
fn blend(mode: &str, b: f64, s: f64) -> f64 {
    let screen = |b: f64, s: f64| b + s - b * s;
    let hard = |b: f64, s: f64| {
        if s <= 0.5 {
            b * 2.0 * s
        } else {
            screen(b, 2.0 * s - 1.0)
        }
    };
    match mode {
        "Normal" => s,
        "Multiply" => b * s,
        "Screen" => screen(b, s),
        "Overlay" => hard(s, b),
        "Darken" => b.min(s),
        "Lighten" => b.max(s),
        "ColorDodge" => {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        "ColorBurn" => {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }
        "HardLight" => hard(b, s),
        "SoftLight" => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        "Difference" => (b - s).abs(),
        "Exclusion" => b + s - 2.0 * b * s,
        _ => unreachable!(),
    }
}

/// §11.3.4: a separable mode in DeviceCMYK, through the complements.
fn blend_cmyk(mode: &str, cb: [f64; 4], cs: [f64; 4]) -> [f64; 4] {
    [0, 1, 2, 3].map(|i| 1.0 - blend(mode, 1.0 - cb[i], 1.0 - cs[i]))
}

/// §10.3.5.
fn cmyk_to_rgb(c: [f64; 4]) -> [f64; 3] {
    [0, 1, 2].map(|i| 1.0 - (c[i] + c[3]).min(1.0))
}

// C + K and Y + K exceed 1 in the source: the conversion clips them.
const CB: [f64; 4] = [0.1, 0.7, 0.2, 0.3];
const CS: [f64; 4] = [0.75, 0.2, 0.8, 0.4];

fn k(v: [f64; 4]) -> String {
    format!("{} {} {} {} k", v[0], v[1], v[2], v[3])
}

/// A 30 × 10 page whose group is `page_group`: the backdrop `CB` covers
/// x 0..30; `CS` is painted in `mode` by a path at x 1, a 1 × 1
/// DeviceCMYK image at x 11 and a group form (no /CS: the page's) at
/// x 21.
fn page(mode: &str, page_group: &str) -> Vec<u8> {
    page_with(mode, page_group, "", "/S /Transparency /I true")
}

/// `page` with `extra` added to the blend-mode ExtGState and `inner` as
/// the group form's group attributes.
fn page_with(mode: &str, page_group: &str, extra: &str, inner: &str) -> Vec<u8> {
    let content = format!(
        "{} 0 0 30 10 re f /GS gs {} 1 2 6 6 re f q 6 0 0 6 11 2 cm /Im Do Q \
         q 6 0 0 6 21 2 cm /Fm Do Q",
        k(CB),
        k(CS)
    );
    let px = CS.map(|v| (v * 255.0).round() as u8);
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 30 10] /Contents 4 0 R {page_group} \
                 /Resources << /ExtGState << /GS 5 0 R >> /XObject << /Im 6 0 R /Fm 7 0 R >> >> >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content.as_bytes())),
        (5, format!("<< /Type /ExtGState /BM /{mode} {extra} >>").into_bytes()),
        (
            6,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceCMYK",
                &px,
            ),
        ),
        (
            7,
            stream_obj(
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Group << {inner} >>"),
                format!("{} 0 0 1 1 re f", k(CS)).as_bytes(),
            ),
        ),
    ])
}

const CMYK_PAGE: &str = "/Group << /S /Transparency /CS /DeviceCMYK >>";
const RGB_PAGE: &str = "/Group << /S /Transparency /CS /DeviceRGB >>";

fn render(pdf: Vec<u8>, opts: &RenderOptions) -> kasane_pdf::RenderedPage {
    let doc = Document::from_bytes(&pdf).unwrap();
    render_page(&doc, &doc.pages()[0], opts).unwrap()
}

fn rgb(r: &kasane_pdf::RenderedPage, x: u32, y: u32) -> [f64; 3] {
    let p = r.pixmap().pixel(x, y).unwrap();
    [p.red(), p.green(), p.blue()].map(|v| v as f64 / 255.0)
}

fn close(got: [f64; 3], want: [f64; 3], what: &str) {
    close_within(got, want, 2.5, what)
}

fn close_within(got: [f64; 3], want: [f64; 3], levels: f64, what: &str) {
    for i in 0..3 {
        assert!(
            (got[i] - want[i]).abs() <= levels / 255.0,
            "{what}: got {got:?}, want {want:?}"
        );
    }
}

/// The backdrop is stored in 8 bits; ColorDodge and ColorBurn divide by
/// (1 − s) or s, which magnifies that rounding (here by up to 4).
fn levels(mode: &str) -> f64 {
    if matches!(mode, "ColorDodge" | "ColorBurn") {
        6.0
    } else {
        2.5
    }
}

fn opts() -> RenderOptions {
    RenderOptions::new().with_dpi(72.0)
}

#[test]
fn separable_modes_blend_the_cmyk_complements() {
    for mode in SEPARABLE {
        let r = render(page(mode, CMYK_PAGE), &opts());
        let want = cmyk_to_rgb(blend_cmyk(mode, CB, CS));
        for (x, path) in [(4, "path"), (14, "image"), (24, "group form")] {
            close_within(rgb(&r, x, 5), want, levels(mode), &format!("{mode} {path}"));
        }
        close(rgb(&r, 9, 5), cmyk_to_rgb(CB), &format!("{mode} backdrop"));
        assert!(
            r.color_approximations().is_empty(),
            "{mode}: {:?}",
            r.color_approximations()
        );
    }
}

#[test]
fn the_same_page_in_rgb_blends_the_rgb_values() {
    // Without the DeviceCMYK page group the colours are converted first
    // (the renderer's behaviour without /CS, unchanged).
    for mode in ["Screen", "Lighten", "Difference"] {
        let r = render(page(mode, RGB_PAGE), &opts());
        let (b, s) = (cmyk_to_rgb(CB), cmyk_to_rgb(CS));
        let want = [0, 1, 2].map(|i| blend(mode, b[i], s[i]));
        close(rgb(&r, 4, 5), want, mode);
        let cmyk = cmyk_to_rgb(blend_cmyk(mode, CB, CS));
        assert!(
            (0..3).any(|i| (want[i] - cmyk[i]).abs() > 0.05),
            "{mode} does not tell the spaces apart"
        );
    }
}

#[test]
fn normal_with_opacity_differs_where_the_conversion_clips() {
    // ca 0.5 of CS over CB: in CMYK the components are averaged, then
    // converted; in RGB the clipped conversions are averaged.
    let r = render(
        page_with("Normal", CMYK_PAGE, "/ca 0.5", "/S /Transparency /I true"),
        &opts(),
    );
    let avg = [0, 1, 2, 3].map(|i| (CB[i] + CS[i]) / 2.0);
    close(rgb(&r, 4, 5), cmyk_to_rgb(avg), "path, ca 0.5");
    let rgb_avg = [0, 1, 2].map(|i| (cmyk_to_rgb(CB)[i] + cmyk_to_rgb(CS)[i]) / 2.0);
    assert!((0..3).any(|i| (rgb_avg[i] - cmyk_to_rgb(avg)[i]).abs() > 0.05));
}

#[test]
fn rgb_colours_in_a_cmyk_group_under_normal_are_unchanged() {
    // §10.3.4 with BG/UCR the identity, then §10.3.5, is the identity
    // on RGB: an opaque RGB or grey fill renders as without the group,
    // to the 8-bit rounding of the two passes (one level).
    let content = "0.9 0.2 0.4 rg 0 0 10 10 re f 0.35 g 10 0 10 10 re f";
    let mk = |g: &str| {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Contents 4 0 R {g} >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", content.as_bytes())),
        ])
    };
    let a = render(mk(CMYK_PAGE), &opts());
    let b = render(mk(""), &opts());
    for (x, y) in a.pixmap().data().iter().zip(b.pixmap().data()) {
        assert!(x.abs_diff(*y) <= 1, "{x} vs {y}");
    }
    assert!(a.color_approximations().is_empty());
}

/// An RGB page drawing one isolated group form whose /CS is DeviceCMYK:
/// `CB` then `CS` in `mode` inside, transparent elsewhere.
fn form_group(mode: &str, group: &str) -> Vec<u8> {
    form_group_with(mode, group, "")
}

/// `form_group` whose group also draws `inner` (an isolated group form
/// painting `CS` at x 2..8, y 2..4) when given: one more layer inside.
fn form_group_with(mode: &str, group: &str, inner: &str) -> Vec<u8> {
    let nested = if inner.is_empty() { "" } else { " /In Do" };
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 30 10] /Contents 4 0 R \
              /Resources << /XObject << /Fm 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"0.2 0.5 0.9 rg 20 0 10 10 re f /Fm Do")),
        (
            5,
            stream_obj(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 30 10] /Group {group} \
                     /Resources << /ExtGState << /GS 6 0 R >> /XObject << /In 7 0 R >> >>"
                ),
                format!(
                    "{} 0 0 10 10 re f /GS gs {} 2 2 6 6 re f{nested}",
                    k(CB),
                    k(CS)
                )
                .as_bytes(),
            ),
        ),
        (
            6,
            format!("<< /Type /ExtGState /BM /{mode} >>").into_bytes(),
        ),
        (
            7,
            stream_obj(
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 30 10] /Group {inner}"),
                format!("{} 2 2 6 2 re f", k(CS)).as_bytes(),
            ),
        ),
    ])
}

#[test]
fn an_isolated_cmyk_group_form_on_an_rgb_page() {
    for mode in SEPARABLE {
        let r = render(
            form_group(mode, "<< /S /Transparency /I true /CS /DeviceCMYK >>"),
            &opts(),
        );
        close_within(
            rgb(&r, 5, 5),
            cmyk_to_rgb(blend_cmyk(mode, CB, CS)),
            levels(mode),
            mode,
        );
        close(rgb(&r, 1, 5), cmyk_to_rgb(CB), mode);
        // Where the group painted nothing it is transparent: the page
        // (white, and the RGB rectangle) shows through.
        close(rgb(&r, 15, 5), [1.0; 3], mode);
        close(rgb(&r, 25, 5), [0.2, 0.5, 0.9], mode);
        assert!(r.color_approximations().is_empty());
    }
}

#[test]
fn what_is_not_blended_in_cmyk_is_painted_as_without_the_group_space() {
    // The whole page (or group) as if it had no /CS, and reported.
    let note = |r: &kasane_pdf::RenderedPage| {
        r.color_approximations()
            .iter()
            .any(|a| a.starts_with("DeviceCMYK blending colour space: blended in RGB"))
    };
    for mode in NON_SEPARABLE {
        let a = render(page(mode, CMYK_PAGE), &opts());
        let b = render(page(mode, ""), &opts());
        assert_eq!(a.pixmap().data(), b.pixmap().data(), "{mode}");
        assert!(note(&a), "{mode}: {:?}", a.color_approximations());
        let a = render(
            form_group(mode, "<< /S /Transparency /I true /CS /DeviceCMYK >>"),
            &opts(),
        );
        let b = render(form_group(mode, "<< /S /Transparency /I true >>"), &opts());
        assert_eq!(a.pixmap().data(), b.pixmap().data(), "{mode} form");
        assert!(note(&a));
    }
    // A knockout group in DeviceCMYK.
    let a = render(
        form_group(
            "Screen",
            "<< /S /Transparency /I true /K true /CS /DeviceCMYK >>",
        ),
        &opts(),
    );
    let b = render(
        form_group("Screen", "<< /S /Transparency /I true /K true >>"),
        &opts(),
    );
    assert_eq!(a.pixmap().data(), b.pixmap().data(), "knockout");
    assert!(note(&a));
    // An isolated group in another space inside a DeviceCMYK page.
    let a = render(
        page_with(
            "Screen",
            CMYK_PAGE,
            "",
            "/S /Transparency /I true /CS /DeviceRGB",
        ),
        &opts(),
    );
    let b = render(page("Screen", ""), &opts());
    assert_eq!(a.pixmap().data(), b.pixmap().data(), "nested RGB group");
    assert!(note(&a));
    // Buffers over the budget: the page and the group form's layer fit
    // (RGB), the two page buffers and that layer do not.
    let limits = Limits::default().with_max_render_buffer_bytes(2 * 30 * 10 * 4 + 100);
    let small = RenderOptions::new().with_dpi(72.0).with_limits(limits);
    let a = render(page("Screen", CMYK_PAGE), &small);
    let b = render(page("Screen", ""), &small);
    assert_eq!(a.pixmap().data(), b.pixmap().data(), "budget");
    assert!(note(&a));
}

#[test]
fn a_pattern_selected_before_the_group_is_not_mixed_in() {
    // The fill colour is a shading pattern selected outside the CMYK
    // group and used inside: its colours were converted for RGB, so the
    // group is painted as without its space.
    let mk = |cs: &str| {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Contents 4 0 R \
                  /Resources << /Pattern << /P 5 0 R >> /XObject << /Fm 6 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"/Pattern cs /P scn /Fm Do")),
            (
                5,
                b"<< /PatternType 2 /Shading << /ShadingType 2 /ColorSpace /DeviceCMYK \
                  /Coords [0 0 20 0] /Function << /FunctionType 2 /Domain [0 1] \
                  /C0 [0 1 0 0] /C1 [1 0 0 0.5] /N 1 >> >> >>"
                    .to_vec(),
            ),
            (
                6,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 20 10] \
                         /Group << /S /Transparency /I true {cs} >>"
                    ),
                    b"0 0 20 10 re f",
                ),
            ),
        ])
    };
    let a = render(mk("/CS /DeviceCMYK"), &opts());
    let b = render(mk(""), &opts());
    assert_eq!(a.pixmap().data(), b.pixmap().data());
    assert!(!a.color_approximations().is_empty());
}

#[test]
fn a_shading_and_an_uncoloured_pattern_in_a_cmyk_group() {
    // Selected inside the group: converted for its passes. A constant
    // DeviceCMYK shading and an uncoloured tiling pattern in
    // [/Pattern /DeviceCMYK] both blend `CS` over `CB` with Screen.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 10] /Contents 4 0 R {CMYK_PAGE} \
                 /Resources << /ExtGState << /GS 5 0 R >> /Pattern << /T 7 0 R >> \
                 /Shading << /Sh 6 0 R >> /ColorSpace << /PC [/Pattern /DeviceCMYK] >> >> >>"
            )
            .into_bytes(),
        ),
        (
            4,
            stream_obj(
                "",
                format!(
                    "{} 0 0 20 10 re f /GS gs q 1 1 8 8 re W n /Sh sh Q \
                     /PC cs {} {} {} {} /T scn 11 1 8 8 re f",
                    k(CB),
                    CS[0],
                    CS[1],
                    CS[2],
                    CS[3]
                )
                .as_bytes(),
            ),
        ),
        (5, b"<< /Type /ExtGState /BM /Screen >>".to_vec()),
        (
            6,
            format!(
                "<< /ShadingType 2 /ColorSpace /DeviceCMYK /Coords [0 0 20 0] \
                 /Function << /FunctionType 2 /Domain [0 1] /C0 [{0} {1} {2} {3}] /C1 [{0} {1} {2} {3}] /N 1 >> >>",
                CS[0], CS[1], CS[2], CS[3]
            )
            .into_bytes(),
        ),
        (
            7,
            stream_obj(
                "/PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 2 2] /XStep 2 /YStep 2 /Resources << >>",
                b"0 0 2 2 re f",
            ),
        ),
    ]);
    let r = render(pdf, &opts());
    let want = cmyk_to_rgb(blend_cmyk("Screen", CB, CS));
    close(rgb(&r, 5, 5), want, "shading");
    close(rgb(&r, 15, 5), want, "uncoloured pattern");
    assert!(
        r.color_approximations().is_empty(),
        "{:?}",
        r.color_approximations()
    );
}

#[test]
fn colours_set_before_the_group_and_fresh_states_inside_it() {
    // An RGB page sets a DeviceCMYK fill before drawing an isolated
    // DeviceCMYK group that paints without setting one: the inherited
    // colour is painted in the group's passes. An annotation appearance
    // on a DeviceCMYK page starts from a fresh state (black), as does a
    // coloured tiling cell; each paints it at opacity 0.5 over white
    // (grey 0.5 — black painted as C = M = Y = K = 1 would stay black).
    let mk = |cs: &str, page_group: &str| {
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 30 10] /Contents 4 0 R {page_group} \
                     /Annots [6 0 R] /Resources << /XObject << /Fm 5 0 R >> /Pattern << /P 8 0 R >> >> >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", b"0 0 0 0.5 k /Fm Do /Pattern cs /P scn 20 0 5 10 re f")),
            (
                5,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                         /Group << /S /Transparency /I true {cs} >>"
                    ),
                    b"0 0 10 10 re f",
                ),
            ),
            (
                6,
                b"<< /Type /Annot /Subtype /Square /Rect [10 0 20 10] /AP << /N 7 0 R >> >>".to_vec(),
            ),
            (
                7,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                     /Resources << /ExtGState << /H << /ca 0.5 >> >> >>",
                    b"/H gs 0 0 10 10 re f",
                ),
            ),
            (
                8,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 5 5] /XStep 5 /YStep 5 \
                     /Resources << /ExtGState << /H << /ca 0.5 >> >> >>",
                    b"/H gs 0 0 5 5 re f",
                ),
            ),
        ])
    };
    for page_group in ["", CMYK_PAGE] {
        let r = render(mk("/CS /DeviceCMYK", page_group), &opts());
        close(rgb(&r, 5, 5), [0.5; 3], "inherited colour");
        close(rgb(&r, 15, 5), [0.5; 3], "annotation, fresh state");
        close(rgb(&r, 22, 5), [0.5; 3], "tiling cell, fresh state");
        assert!(
            r.color_approximations().is_empty(),
            "{:?}",
            r.color_approximations()
        );
    }
}

#[test]
fn the_page_group_is_converted_before_it_meets_the_paper() {
    // §11.4.7: the page group composites against transparency; its
    // result is converted and only then put on white. Black ink with
    // cyan, (1, 0, 0, 1), at opacity 0.5: converted it is black at 0.5,
    // grey on white — flattened on white first, the cyan would clip to
    // (0, .5, .5) instead.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R {CMYK_PAGE} \
                 /Resources << /ExtGState << /H << /ca 0.5 >> >> >> >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", b"/H gs 1 0 0 1 k 0 0 10 10 re f")),
    ]);
    let r = render(pdf, &opts());
    close(rgb(&r, 5, 5), [0.5; 3], "half-opaque black ink");
}

#[test]
fn a_knockout_page_group_and_an_unreadable_cs() {
    let note = |r: &kasane_pdf::RenderedPage| {
        r.color_approximations()
            .iter()
            .any(|a| a.contains("knockout page group"))
    };
    let ko = "/Group << /S /Transparency /K true /CS /DeviceCMYK >>";
    let a = render(page("Screen", ko), &opts());
    let b = render(
        page("Screen", "/Group << /S /Transparency /K true >>"),
        &opts(),
    );
    assert_eq!(a.pixmap().data(), b.pixmap().data());
    assert!(note(&a), "{:?}", a.color_approximations());
    // A non-isolated group's /CS is not read (Table 147), even when it
    // names no colour space; an isolated group's unreadable /CS blends
    // in RGB, as before.
    for inner in [
        "/S /Transparency /CS /NoSuchSpace",
        "/S /Transparency /I true /CS /NoSuchSpace",
    ] {
        let a = render(page_with("Screen", "", "", inner), &opts());
        let b = render(page("Screen", ""), &opts());
        assert_eq!(a.pixmap().data(), b.pixmap().data(), "{inner}");
    }
}

#[test]
fn every_pass_counts_against_the_work_budgets() {
    // An isolated DeviceCMYK group drawing one inner form: one painting
    // invokes 2 forms, the group's two passes 3. The work budgets count
    // what is really done, both passes included: under
    // max_xobject_invocations 2 the CMYK group is refused (a work limit
    // ends the render — it is not painted again in RGB), where the same
    // page without the group colour space renders; under 3 it renders.
    // `mode` set before the inner form, or (`late`) after it.
    let mk_at = |cs: &str, mode: &str, late: bool| {
        let body = if late {
            format!("{} 0 0 10 10 re f /In Do /GS gs 0 0 1 1 re f", k(CB))
        } else {
            format!("{} 0 0 10 10 re f /GS gs /In Do", k(CB))
        };
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R \
                  /Resources << /XObject << /G 5 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"/G Do")),
            (
                5,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                         /Group << /S /Transparency /I true {cs} >> \
                         /Resources << /XObject << /In 6 0 R >> /ExtGState << /GS 7 0 R >> >>"
                    ),
                    body.as_bytes(),
                ),
            ),
            (
                6,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                    format!("{} 0 0 10 10 re f", k(CS)).as_bytes(),
                ),
            ),
            (7, format!("<< /BM /{mode} >>").into_bytes()),
        ])
    };
    let mk = |cs: &str, mode: &str| mk_at(cs, mode, false);
    let under = |n: u64| {
        RenderOptions::new()
            .with_dpi(72.0)
            .with_limits(Limits::default().with_max_xobject_invocations(n))
    };
    let try_render = |pdf: Vec<u8>, o: &RenderOptions| {
        let doc = Document::from_bytes(&pdf).unwrap();
        render_page(&doc, &doc.pages()[0], o)
    };
    assert!(try_render(mk("", "Screen"), &under(2)).is_ok());
    let err = try_render(mk("/CS /DeviceCMYK", "Screen"), &under(2)).unwrap_err();
    assert!(
        matches!(
            err,
            kasane_pdf::Error::LimitExceeded {
                limit: "max_xobject_invocations",
                ..
            }
        ),
        "{err:?}"
    );
    let r = try_render(mk("/CS /DeviceCMYK", "Screen"), &under(3)).unwrap();
    close(
        rgb(&r, 5, 5),
        cmyk_to_rgb(blend_cmyk("Screen", CB, CS)),
        "Screen",
    );
    assert!(
        r.color_approximations().is_empty(),
        "{:?}",
        r.color_approximations()
    );
    // Abandoned at Hue *after* the inner form: the attempt's invocation
    // of it counts, then the RGB painting's — 1 + 1 + 1. Abandoned
    // before it: 1 + 1.
    assert!(try_render(mk_at("", "Hue", true), &under(2)).is_ok());
    assert!(try_render(mk_at("/CS /DeviceCMYK", "Hue", true), &under(2)).is_err());
    let r = try_render(mk_at("/CS /DeviceCMYK", "Hue", true), &under(3)).unwrap();
    assert!(!r.color_approximations().is_empty());
    assert!(try_render(mk_at("/CS /DeviceCMYK", "Hue", false), &under(2)).is_ok());
}

/// Expensive dashed strokes (about 1.2 million outline edges at 72
/// dpi, three tenths of the render's 4 million) then a fill.
const DASHED: &str = "0 0 0 1 K 0.1 w [0.001 0.001] 0 d \
    0 0 m 100 0 l S 0 1 m 100 1 l S 0 2 m 100 2 l S \
    0 3 m 100 3 l S 0 4 m 100 4 l S 0 5 m 100 5 l S";

/// `depth` isolated DeviceCMYK groups, each drawing the next; the
/// innermost strokes `DASHED`, then sets `/BM /Hue` (not blended in
/// CMYK) and fills. The page draws the outermost `copies` times.
fn nested_groups(depth: u32, copies: usize, cs: &str) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 10] /Contents 4 0 R \
              /Resources << /XObject << /X 10 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", "/X Do ".repeat(copies).as_bytes())),
        (5, b"<< /BM /Hue >>".to_vec()),
    ];
    for i in 0..depth {
        let group = format!(
            "/Type /XObject /Subtype /Form /BBox [0 0 100 10] /Group << /S /Transparency /I true {cs} >>"
        );
        objs.push(if i + 1 == depth {
            (
                10 + i,
                stream_obj(
                    &format!("{group} /Resources << /ExtGState << /H 5 0 R >> >>"),
                    format!("{DASHED} /H gs 0 0 0 1 k 0 0 10 10 re f").as_bytes(),
                ),
            )
        } else {
            (
                10 + i,
                stream_obj(
                    &format!("{group} /Resources << /XObject << /X {} 0 R >> >>", 11 + i),
                    b"/X Do",
                ),
            )
        });
    }
    build_pdf(&objs)
}

#[test]
fn a_region_painted_in_rgb_starts_no_attempt_inside_it() {
    // Four nested DeviceCMYK groups whose innermost one is not blended
    // in CMYK, after expensive drawing: the outermost group's attempt is
    // abandoned, and the whole is painted once in RGB — the inner groups
    // start no attempts of their own. Two paintings of the dashed
    // strokes fit the render's dashed-stroke work; five (an attempt per
    // level and the RGB painting) would not.
    let r = render(nested_groups(4, 1, "/CS /DeviceCMYK"), &opts());
    assert!(
        r.color_approximations()
            .iter()
            .any(|a| a.contains("non-separable")),
        "{:?}",
        r.color_approximations()
    );
    let b = render(nested_groups(4, 1, ""), &opts());
    assert_eq!(r.pixmap().data(), b.pixmap().data());
}

#[test]
fn abandoned_attempts_count_against_the_work_budgets() {
    // Two sibling groups like the above, one level each: each is
    // attempted (the dashed strokes painted, then abandoned) and painted
    // in RGB — four paintings of the strokes, past the render's
    // dashed-stroke work: refused. Without the group colour space the
    // two paintings fit. The work of an abandoned attempt is not given
    // back.
    let doc = Document::from_bytes(&nested_groups(1, 2, "/CS /DeviceCMYK")).unwrap();
    let err = render_page(&doc, &doc.pages()[0], &opts()).err().unwrap();
    assert!(err.to_string().contains("dashed stroke"), "{err}");
    let doc = Document::from_bytes(&nested_groups(1, 2, "")).unwrap();
    assert!(render_page(&doc, &doc.pages()[0], &opts()).is_ok());
}

#[test]
fn every_buffer_budget_gives_the_cmyk_result_or_the_reported_rgb_one() {
    // Whatever max_render_buffer_bytes allows, the page is the CMYK
    // result with no note, the RGB one with a note, or refused exactly
    // as the RGB page is — never a mixture of the two.
    let full = render(page("Screen", CMYK_PAGE), &opts());
    let one = 30 * 10 * 4;
    for budget in (one..=5 * one).step_by(one as usize / 4) {
        let o = RenderOptions::new()
            .with_dpi(72.0)
            .with_limits(Limits::default().with_max_render_buffer_bytes(budget));
        let doc = Document::from_bytes(&page("Screen", CMYK_PAGE)).unwrap();
        let rgb_doc = Document::from_bytes(&page("Screen", "")).unwrap();
        match (
            render_page(&doc, &doc.pages()[0], &o),
            render_page(&rgb_doc, &rgb_doc.pages()[0], &o),
        ) {
            (Ok(a), Ok(b)) => {
                if a.color_approximations().is_empty() {
                    assert_eq!(a.pixmap().data(), full.pixmap().data(), "{budget}");
                } else {
                    assert_eq!(a.pixmap().data(), b.pixmap().data(), "{budget}");
                }
            }
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{budget}"),
            (a, b) => panic!("{budget}: {:?} vs {:?}", a.is_ok(), b.is_ok()),
        }
    }
}

// tests/render_jpx.rs's 4 × 4 RGBA codestream (synthetic).
const RGBA_JP2: &[u8] = b"\x00\x00\x00\x0c\x6a\x50\x20\x20\x0d\x0a\x87\x0a\x00\x00\x00\x14\x66\x74\x79\x70\x6a\x70\x32\x20\x00\x00\x00\x00\x6a\x70\x32\x20\x00\x00\x00\x2d\x6a\x70\x32\x68\x00\x00\x00\x16\x69\x68\x64\x72\x00\x00\x00\x04\x00\x00\x00\x04\x00\x04\x07\x07\x00\x00\x00\x00\x00\x0f\x63\x6f\x6c\x72\x01\x00\x00\x00\x00\x00\x10\x00\x00\x00\xb1\x6a\x70\x32\x63\xff\x4f\xff\x51\x00\x32\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x07\x01\x01\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x36\x00\x01\xff\x93\xcf\xb4\x20\x11\x50\x49\x8a\x65\x20\x00\x05\x80\xdf\x80\x20\x14\x00\x5c\xaf\xdf\x80\x90\x1c\x46\xe8\x10\xa9\xcc\x16\x20\xd9\xa0\xf1\x3a\xdf\x5b\x4b\xab\x69\x75\xff\xd9";

#[test]
fn a_premultiplied_jpx_image_in_a_cmyk_group() {
    // /SMaskInData 2: the samples are divided by their alpha in the
    // image's space, before they are painted for either pass: red at
    // opacity 0.5 on white, on a DeviceCMYK page as on an RGB one.
    for g in [CMYK_PAGE, ""] {
        let r = render(jpx_page(g, RED_PM_JP2), &opts());
        close(rgb(&r, 5, 5), [1.0, 0.5, 0.5], g);
    }
    let mk = |g: &str| jpx_page(g, RGBA_JP2);
    let a = render(mk(CMYK_PAGE), &opts());
    let b = render(mk(""), &opts());
    for (x, y) in a.pixmap().data().iter().zip(b.pixmap().data()) {
        assert!(x.abs_diff(*y) <= 1, "{x} vs {y}");
    }
}

fn jpx_page(g: &str, jp2: &[u8]) -> Vec<u8> {
    jpx_page_with(g, jp2, "")
}

fn jpx_page_with(g: &str, jp2: &[u8], extra: &str) -> Vec<u8> {
    let side = if jp2.len() == RED_PM_JP2.len() { 8 } else { 4 };
    {
        let content = b"q 10 0 0 10 0 0 cm /Im Do Q";
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {side} /Height {side} /Filter /JPXDecode \
             /SMaskInData 2 {extra} /Length {} >>\nstream\n",
            jp2.len()
        )
        .into_bytes();
        img.extend_from_slice(jp2);
        img.extend_from_slice(b"\nendstream");
        build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R {g} \
                     /Resources << /XObject << /Im 5 0 R >> >> >>"
                )
                .into_bytes(),
            ),
            (4, stream_obj("", content)),
            (5, img),
        ])
    }
}

// An 8 × 8 red codestream with an alpha channel of 128 everywhere, the
// colour premultiplied by it (/SMaskInData 2): samples (128, 0, 0).
const RED_PM_JP2: &[u8] = b"\x00\x00\x00\x0c\x6a\x50\x20\x20\x0d\x0a\x87\x0a\x00\x00\x00\x14\x66\x74\x79\x70\x6a\x70\x32\x20\x00\x00\x00\x00\x6a\x70\x32\x20\x00\x00\x00\x2d\x6a\x70\x32\x68\x00\x00\x00\x16\x69\x68\x64\x72\x00\x00\x00\x08\x00\x00\x00\x08\x00\x04\x07\x07\x00\x00\x00\x00\x00\x0f\x63\x6f\x6c\x72\x01\x00\x00\x00\x00\x00\x10\x00\x00\x00\x9c\x6a\x70\x32\x63\xff\x4f\xff\x51\x00\x32\x00\x00\x00\x00\x00\x08\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x08\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x07\x01\x01\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x21\x00\x01\xff\x93\xcf\xb4\x18\x11\x50\x54\xaf\xff\x7f\x80\xdf\x80\x28\x14\x00\x5c\xa3\x6f\x80\xff\xd9";

#[test]
fn every_buffer_budget_for_a_cmyk_group_form() {
    // As for the page: an isolated DeviceCMYK group form on an RGB page
    // is the CMYK result with no note or the RGB one with a note, at
    // every max_render_buffer_bytes — also with an isolated group inside
    // it (one more layer within the passes, which may not fit beside
    // them). Both outcomes occur in each sweep.
    let cmyk = "<< /S /Transparency /I true /CS /DeviceCMYK >>";
    let plain = "<< /S /Transparency /I true >>";
    for inner in ["", plain] {
        let full = render(form_group_with("Screen", cmyk, inner), &opts());
        let one = 30 * 10 * 4;
        let mut seen = (false, false);
        for budget in (one..=5 * one).step_by(one as usize / 4) {
            let o = RenderOptions::new()
                .with_dpi(72.0)
                .with_limits(Limits::default().with_max_render_buffer_bytes(budget));
            let a = Document::from_bytes(&form_group_with("Screen", cmyk, inner)).unwrap();
            let b = Document::from_bytes(&form_group_with("Screen", plain, inner)).unwrap();
            match (
                render_page(&a, &a.pages()[0], &o),
                render_page(&b, &b.pages()[0], &o),
            ) {
                (Ok(a), Ok(b)) => {
                    if a.color_approximations().is_empty() {
                        seen.0 = true;
                        assert_eq!(a.pixmap().data(), full.pixmap().data(), "{budget}");
                    } else {
                        seen.1 = true;
                        assert_eq!(a.pixmap().data(), b.pixmap().data(), "{budget}");
                    }
                }
                (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "{budget}"),
                (a, b) => panic!("{inner} {budget}: {:?} vs {:?}", a.is_ok(), b.is_ok()),
            }
        }
        assert_eq!(seen, (true, true), "{inner}");
    }
}

#[test]
fn an_abandoned_pass_leaves_no_interpreter_state_behind() {
    // The CMYK group's pass is abandoned (Hue) inside a hidden optional
    // content span: the span the pass opened is closed with it, and the
    // red rectangle drawn after the group is visible.
    let mk = |cs: &str| {
        build_pdf(&[
            (
                1,
                b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [8 0 R] \
                  /D << /OFF [8 0 R] >> >> >>"
                    .to_vec(),
            ),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R \
                  /Resources << /XObject << /G 5 0 R >> >> >>"
                    .to_vec(),
            ),
            (4, stream_obj("", b"/G Do 1 0 0 rg 0 0 10 10 re f")),
            (
                5,
                stream_obj(
                    &format!(
                        "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                         /Group << /S /Transparency /I true {cs} >> \
                         /Resources << /Properties << /Layer 8 0 R >> /ExtGState << /H 6 0 R >> >>"
                    ),
                    b"/OC /Layer BDC /H gs 0 0 0 1 k 0 0 10 10 re f EMC",
                ),
            ),
            (6, b"<< /BM /Hue >>".to_vec()),
            (8, b"<< /Type /OCG /Name (L) >>".to_vec()),
        ])
    };
    let a = render(mk("/CS /DeviceCMYK"), &opts());
    let b = render(mk(""), &opts());
    assert_eq!(a.pixmap().data(), b.pixmap().data());
    close(rgb(&a, 5, 5), [1.0, 0.0, 0.0], "after the group");
    assert!(!a.color_approximations().is_empty());
}

#[test]
fn the_colour_key_of_a_premultiplied_jpx_image_is_on_its_stored_samples() {
    // §8.9.6.4: /Mask [128 128 0 0 0 0] keys the stored samples
    // (128, 0, 0) — every pixel is masked out — before /SMaskInData 2
    // divides them by their alpha.
    for g in [CMYK_PAGE, ""] {
        let r = render(
            jpx_page_with(g, RED_PM_JP2, "/Mask [128 128 0 0 0 0]"),
            &opts(),
        );
        close(rgb(&r, 5, 5), [1.0; 3], g);
    }
}

fn page_of(
    page_group: &str,
    resources: &str,
    content: &[u8],
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    page_sized(10, page_group, resources, content, extra)
}

fn page_sized(
    side: u32,
    page_group: &str,
    resources: &str,
    content: &[u8],
    extra: Vec<(u32, Vec<u8>)>,
) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {side} {side}] /Contents 4 0 R {page_group} \
                 /Resources << {resources} >> >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    objs.extend(extra);
    build_pdf(&objs)
}

fn with_limits(pdf: &[u8], limits: Limits) -> Result<kasane_pdf::RenderedPage, kasane_pdf::Error> {
    let doc = Document::from_bytes_with_limits(pdf, limits.clone()).unwrap();
    render_page(
        &doc,
        &doc.pages()[0],
        &RenderOptions::new().with_dpi(72.0).with_limits(limits),
    )
}

#[test]
fn the_page_passes_count_against_the_work_budgets() {
    // A DeviceCMYK page drawing one form: its two passes invoke it twice.
    // Under max_xobject_invocations 1 that is refused (the RGB page
    // renders), under 2 it renders in CMYK.
    let mk = |g: &str| {
        page_of(
            g,
            "/XObject << /F 5 0 R >>",
            b"/F Do",
            vec![(
                5,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                    b"0 0 0 0.5 k 0 0 10 10 re f",
                ),
            )],
        )
    };
    let l = |n| Limits::default().with_max_xobject_invocations(n);
    assert!(with_limits(&mk(""), l(1)).is_ok());
    let err = with_limits(&mk(CMYK_PAGE), l(1)).unwrap_err();
    assert!(
        matches!(
            err,
            kasane_pdf::Error::LimitExceeded {
                limit: "max_xobject_invocations",
                ..
            }
        ),
        "{err:?}"
    );
    let r = with_limits(&mk(CMYK_PAGE), l(2)).unwrap();
    assert!(r.color_approximations().is_empty());
    // Abandoned in the first pass (Hue after the form) and painted in
    // RGB: two.
    let mk_hue = |g: &str| {
        page_of(
            g,
            "/XObject << /F 5 0 R >> /ExtGState << /H << /BM /Hue >> >>",
            b"/F Do /H gs 0 0 1 1 re f",
            vec![(
                5,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                    b"0 0 0 0.5 k 0 0 10 10 re f",
                ),
            )],
        )
    };
    assert!(with_limits(&mk_hue(""), l(1)).is_ok());
    assert!(with_limits(&mk_hue(CMYK_PAGE), l(1)).is_err());
    let r = with_limits(&mk_hue(CMYK_PAGE), l(2)).unwrap();
    assert!(!r.color_approximations().is_empty());
}

#[test]
fn a_tiling_texture_refused_for_the_passes_buffer_goes_to_rgb() {
    // 10 000 one-point cells on a 100-point page: the periodic texture
    // fits beside the page
    // and one layer (RGB), not beside the passes' two. Painting the cells
    // one by one instead would meet max_pattern_tiles: the page is
    // painted in RGB (reported), exactly as without the group space.
    let mk = |g: &str| {
        page_sized(
            100,
            g,
            "/Pattern << /P 5 0 R >>",
            b"/Pattern cs /P scn 0 0 100 100 re f",
            vec![(
                5,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << >>",
                    b"0 0 0 1 k 0 0 1 1 re f",
                ),
            )],
        )
    };
    let page = 100 * 100 * 4;
    let mut found = false;
    for b in (page..5 * page).step_by(page as usize / 8) {
        let l = Limits::default()
            .with_max_pattern_tiles(16)
            .with_max_render_buffer_bytes(b);
        let (a, z) = (
            with_limits(&mk(CMYK_PAGE), l.clone()),
            with_limits(&mk(""), l),
        );
        match (a, z) {
            (Ok(a), Ok(z)) => {
                if !a.color_approximations().is_empty() {
                    found = true;
                    assert_eq!(a.pixmap().data(), z.pixmap().data(), "{b}");
                }
            }
            (Err(a), Err(z)) => assert_eq!(a.to_string(), z.to_string(), "{b}"),
            (a, z) => panic!("{b}: CMYK {:?} vs RGB {:?}", a.err(), z.err()),
        }
    }
    assert!(found, "no budget sends the page to RGB");
}

#[test]
fn a_work_limit_reading_a_group_space_is_not_an_unreadable_space() {
    // The page group's /CS is an ICCBased stream larger than the
    // decoded-bytes budget: refused for that limit, not blended in RGB as
    // if the /CS could not be read.
    let icc = vec![b'0'; 4000];
    let pdf = page_of(
        "/Group << /S /Transparency /CS [/ICCBased 5 0 R] >>",
        "",
        b"0 0 0 1 k 0 0 10 10 re f",
        vec![(5, stream_obj("/N 4 /Alternate /DeviceCMYK", &icc))],
    );
    let err = with_limits(&pdf, Limits::default().with_max_total_decoded_bytes(2000)).unwrap_err();
    assert!(
        matches!(
            err,
            kasane_pdf::Error::LimitExceeded {
                limit: "max_total_decoded_bytes",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn a_tiling_in_a_soft_mask_inside_the_passes() {
    // A DeviceCMYK page whose luminosity mask's group fills with a
    // 10 000-cell tiling: the mask is painted in RGB, but the passes'
    // layers are alive. At every buffer budget the page is the CMYK
    // result, the RGB one with a note, or refused as the RGB page is.
    let mk = |g: &str| {
        page_sized(
            100,
            g,
            "/ExtGState << /M << /SMask << /S /Luminosity /G 5 0 R >> >> >>",
            b"/M gs 0 0 0 1 k 0 0 100 100 re f",
            vec![
                (
                    5,
                    stream_obj(
                        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                         /Group << /S /Transparency /CS /DeviceGray >> /Resources << /Pattern << /P 6 0 R >> >>",
                        b"/Pattern cs /P scn 0 0 100 100 re f",
                    ),
                ),
                (
                    6,
                    stream_obj(
                        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 1 1] /XStep 1 /YStep 1 /Resources << >>",
                        b"1 g 0 0 1 1 re f",
                    ),
                ),
            ],
        )
    };
    let page = 100 * 100 * 4;
    let full = with_limits(&mk(CMYK_PAGE), Limits::default().with_max_pattern_tiles(16)).unwrap();
    let mut seen = (false, false);
    for b in (page..7 * page).step_by(page as usize / 8) {
        let l = Limits::default()
            .with_max_pattern_tiles(16)
            .with_max_render_buffer_bytes(b);
        match (
            with_limits(&mk(CMYK_PAGE), l.clone()),
            with_limits(&mk(""), l),
        ) {
            (Ok(a), Ok(z)) => {
                if a.color_approximations().is_empty() {
                    seen.0 = true;
                    assert_eq!(a.pixmap().data(), full.pixmap().data(), "{b}");
                } else {
                    seen.1 = true;
                    assert_eq!(a.pixmap().data(), z.pixmap().data(), "{b}");
                }
            }
            (Err(a), Err(z)) => assert_eq!(a.to_string(), z.to_string(), "{b}"),
            (a, z) => panic!("{b}: CMYK {:?} vs RGB {:?}", a.err(), z.err()),
        }
    }
    assert_eq!(seen, (true, true));
}

/// A PDF of plain objects `objs` (number, body) and object `inner`
/// stored in an object stream (padded with `pad` bytes, so that decoding
/// it costs decoded bytes), with a cross-reference stream; the object
/// stream and the cross-reference stream take the next two numbers.
fn with_object_stream(objs: &[(u32, Vec<u8>)], inner: (u32, &[u8]), pad: usize) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = std::collections::BTreeMap::new();
    for (n, body) in objs {
        offs.insert(*n, out.len());
        out.extend(format!("{n} 0 obj\n").as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let top = objs.iter().map(|o| o.0).chain([inner.0]).max().unwrap();
    let (os, xn) = (top + 1, top + 2);
    let header = format!("{} 0 ", inner.0).into_bytes();
    let mut data = header.clone();
    data.extend(inner.1);
    data.extend(vec![b' '; pad + 1]);
    offs.insert(os, out.len());
    out.extend(
        format!(
            "{os} 0 obj\n<< /Type /ObjStm /N 1 /First {} /Length {} >>\nstream\n",
            header.len(),
            data.len()
        )
        .as_bytes(),
    );
    out.extend(&data);
    out.extend(b"\nendstream\nendobj\n");
    let x_off = out.len();
    offs.insert(xn, x_off);
    let row = |t: u8, v: u32, g: u16| {
        [
            t,
            (v >> 24) as u8,
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
            (g >> 8) as u8,
            g as u8,
        ]
    };
    let mut xs = Vec::new();
    for n in 0..=xn {
        xs.extend(if n == 0 {
            row(0, 0, 0xffff)
        } else if n == inner.0 {
            row(2, os, 0)
        } else if let Some(o) = offs.get(&n) {
            row(1, *o as u32, 0)
        } else {
            row(0, 0, 0)
        });
    }
    out.extend(
        format!(
            "{xn} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Root 1 0 R /Length {} >>\nstream\n",
            xn + 1,
            xs.len()
        )
        .as_bytes(),
    );
    out.extend(&xs);
    out.extend(format!("\nendstream\nendobj\nstartxref\n{x_off}\n%%EOF\n").as_bytes());
    out
}

fn stream_body(dict: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("<< {dict} /Length {} >>\nstream\n", data.len()).as_bytes(),
        data,
        b"\nendstream",
    ]
    .concat()
}

#[test]
fn a_work_limit_in_the_blend_scan_is_not_a_blend() {
    // Overlapping cells (opacity 0.4 from outside) whose resources name
    // an /ExtGState (Multiply, not used by the cell) kept in an object
    // stream: the scan for blend modes
    // decodes that stream — in each pass, as caches are rolled back.
    // Whatever max_total_decoded_bytes allows, the page is the
    // full-budget result or refused: a scan cut short by the limit
    // never decides how the cells are painted.
    let mk = |g: &str| {
        let page = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R {g} \
             /Resources << /Pattern << /P 5 0 R >> /ExtGState << /A << /ca 0.4 >> >> >> >>"
        );
        let content = b"/A gs /Pattern cs /P scn 0 0 40 40 re f";
        let cell = b"0 0 0 1 k 0 0 1.5 1.5 re f";
        with_object_stream(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (3, page.into_bytes()),
                (4, stream_body("", content)),
                (
                    5,
                    stream_body(
                        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 2 2] /XStep 1 /YStep 1 \
                         /Resources << /ExtGState << /H 6 0 R >> >>",
                        cell,
                    ),
                ),
            ],
            (6, b"<< /BM /Multiply >>"),
            4000,
        )
    };
    for g in [CMYK_PAGE, ""] {
        let full = with_limits(&mk(g), Limits::default()).unwrap();
        let (mut ok, mut refused) = (false, false);
        for n in (500..20_000).step_by(250) {
            let l = Limits::default().with_max_total_decoded_bytes(n);
            let Ok(doc) = Document::from_bytes_with_limits(&mk(g), l.clone()) else {
                continue;
            };
            match render_page(
                &doc,
                &doc.pages()[0],
                &RenderOptions::new().with_dpi(72.0).with_limits(l),
            ) {
                Ok(r) => {
                    ok = true;
                    assert_eq!(r.pixmap().data(), full.pixmap().data(), "{g} {n}");
                }
                Err(_) => refused = true,
            }
        }
        assert!(ok && refused, "{g}: {ok} {refused}");
    }
}

#[test]
fn a_type3_width_read_in_the_passes() {
    // A Type 3 font whose /Widths entry is an indirect number kept in
    // an object stream: each pass decodes the stream again (the caches are
    // rolled back). Whatever max_total_decoded_bytes allows, the page is
    // the full-budget result or refused — a width read cut short by the
    // limit (a zero advance) never moves the second glyph.
    // `g`: the page group; "form": an RGB page drawing the text in an
    // isolated DeviceCMYK group form.
    let mk = |g: &str| {
        let form = g == "form";
        let g = if form { "" } else { g };
        let text: &[u8] = b"0 0 0 0 k 0 0 30 20 re f 0 .4 0 .4 k BT /F 20 Tf 2 3 Td (AA) Tj ET";
        with_object_stream(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (
                    3,
                    format!(
                        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 30 20] /Contents 4 0 R {g} \
                         /Resources << /Font << /F 5 0 R >> /XObject << /X 8 0 R >> >> >>"
                    )
                    .into_bytes(),
                ),
                (4, stream_body("", if form { b"/X Do" } else { text })),
                (
                    8,
                    stream_body(
                        "/Type /XObject /Subtype /Form /BBox [0 0 30 20] \
                         /Group << /S /Transparency /I true /CS /DeviceCMYK >> /Resources << /Font << /F 5 0 R >> >>",
                        text,
                    ),
                ),
                (
                    5,
                    b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 300 500] /FontMatrix [.001 0 0 .001 0 0] \
                      /CharProcs << /A 7 0 R >> /Encoding << /Differences [65 /A] >> /FirstChar 65 /LastChar 65 \
                      /Widths [6 0 R] /Resources << >> >>"
                        .to_vec(),
                ),
                (7, stream_body("", b"400 0 0 0 300 500 d1 0 0 300 500 re f")),
            ],
            (6, b"400"),
            6000,
        )
    };
    for g in [CMYK_PAGE, "form", ""] {
        let full = with_limits(&mk(g), Limits::default()).unwrap();
        let (mut ok, mut refused) = (false, false);
        for n in (500..20_000).step_by(250) {
            let l = Limits::default().with_max_total_decoded_bytes(n);
            let Ok(doc) = Document::from_bytes_with_limits(&mk(g), l.clone()) else {
                continue;
            };
            match render_page(
                &doc,
                &doc.pages()[0],
                &RenderOptions::new().with_dpi(72.0).with_limits(l),
            ) {
                Ok(r) => {
                    ok = true;
                    assert_eq!(r.pixmap().data(), full.pixmap().data(), "{g} {n}");
                }
                Err(_) => refused = true,
            }
        }
        assert!(ok && refused, "{g}: {ok} {refused}");
    }
}

#[test]
fn a_tile_limit_in_the_passes_is_not_a_reason_for_rgb() {
    // A DeviceCMYK page under a luminosity mask whose group blends in
    // DeviceCMYK too (Lighten), filling with a tiling of 16 cells, under
    // max_pattern_tiles 1: the limit is reached in the passes and the
    // render is refused. Painted again in RGB instead, the mask (not
    // blended in CMYK there) would be black, the tiling skipped, and the
    // page rendered white.
    let pdf = page_of(
        CMYK_PAGE,
        "/ExtGState << /M << /SMask << /S /Luminosity /G 5 0 R >> >> >> /Pattern << /P 6 0 R >>",
        b"/M gs /Pattern cs /P scn 0 0 10 10 re f",
        vec![
            (
                5,
                stream_obj(
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                     /Group << /S /Transparency /I true /CS /DeviceCMYK >> \
                     /Resources << /ExtGState << /L << /BM /Lighten >> >> >>",
                    b"1 1 1 0 k 0 0 10 10 re f /L gs 0 0 0 1 k 0 0 10 10 re f",
                ),
            ),
            (
                6,
                stream_obj(
                    "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 3 3] /XStep 3 /YStep 3 /Resources << >>",
                    b"0 g 0 0 3 3 re f",
                ),
            ),
        ],
    );
    let err = with_limits(&pdf, Limits::default().with_max_pattern_tiles(1)).unwrap_err();
    assert!(
        matches!(
            err,
            kasane_pdf::Error::LimitExceeded {
                limit: "max_pattern_tiles",
                ..
            }
        ),
        "{err:?}"
    );
    assert!(with_limits(&pdf, Limits::default()).is_ok());
}
