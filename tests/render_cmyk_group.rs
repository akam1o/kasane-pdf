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
fn the_passes_cost_the_budgets_one_painting() {
    // An isolated DeviceCMYK group drawing one inner form, under a
    // max_xobject_invocations that one painting of the page needs
    // exactly: blended in CMYK, not refused — and with a non-separable
    // mode inside, painted in RGB under the same limit, not refused.
    let mk = |mode: &str| {
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
                    "/Type /XObject /Subtype /Form /BBox [0 0 10 10] \
                     /Group << /S /Transparency /I true /CS /DeviceCMYK >> \
                     /Resources << /XObject << /In 6 0 R >> /ExtGState << /GS 7 0 R >> >>",
                    format!("{} 0 0 10 10 re f /GS gs /In Do", k(CB)).as_bytes(),
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
    let tight = RenderOptions::new()
        .with_dpi(72.0)
        .with_limits(Limits::default().with_max_xobject_invocations(2));
    let r = render(mk("Screen"), &tight);
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
    let r = render(mk("Hue"), &tight);
    assert!(!r.color_approximations().is_empty());
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
    let side = if jp2.len() == RED_PM_JP2.len() { 8 } else { 4 };
    {
        let content = b"q 10 0 0 10 0 0 cm /Im Do Q";
        let mut img = format!(
            "<< /Type /XObject /Subtype /Image /Width {side} /Height {side} /Filter /JPXDecode \
             /SMaskInData 2 /Length {} >>\nstream\n",
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
