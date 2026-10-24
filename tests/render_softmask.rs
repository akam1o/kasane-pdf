//! ExtGState soft masks (ISO 32000-1 §11.6.5), transparency-group
//! compositing (§11.4.7, §11.6.6) and the non-separable blend modes
//! (§11.3.5.3). 100×100 pt page at 72 dpi: pixmap (x, y) = user
//! (x, 100 − y).

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, RenderOptions, RenderedPage};

/// A page whose `/ExtGState` entries are `gs`, `/XObject` entries
/// `xobjects` (name → object number) and extra objects `objs`.
fn page(content: &[u8], gs: &str, xobjects: &[(&str, u32)], objs: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let xo: String = xobjects
        .iter()
        .map(|(n, o)| format!("/{n} {o} 0 R "))
        .collect();
    let mut all = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /ExtGState << {gs} >> /XObject << {xo} >> >> \
                 /Contents 4 0 R >>"
            )
            .into_bytes(),
        ),
        (4, stream_obj("", content)),
    ];
    all.extend(objs);
    build_pdf(&all)
}

/// A transparency-group form XObject covering `bbox` with `content`.
fn group(bbox: &str, extra: &str, content: &[u8]) -> Vec<u8> {
    stream_obj(
        &format!(
            "/Type /XObject /Subtype /Form /BBox [{bbox}] \
             /Group << /S /Transparency /CS /DeviceRGB {extra} >>"
        ),
        content,
    )
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol
        && (a.1 as i32 - b.1 as i32).abs() <= tol
        && (a.2 as i32 - b.2 as i32).abs() <= tol
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const BLACK: (u8, u8, u8) = (0, 0, 0);
const RED: (u8, u8, u8) = (255, 0, 0);

/// Luminosity mask: white where x < 50, black elsewhere, as a group
/// covering the whole page.
fn half_mask() -> Vec<u8> {
    group(
        "0 0 100 100",
        "",
        b"1 g 0 0 50 100 re f 0 g 50 0 50 100 re f",
    )
}

#[test]
fn luminosity_mask_gates_the_fill() {
    // A full-page black fill under the mask: left half painted, right
    // half not.
    let r = render(&page(
        b"/G gs 0 g 0 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[],
        vec![(10, half_mask())],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 50), BLACK);
    assert_eq!(px(&r, 75, 50), WHITE);
    // A grey mask value is a partial alpha: 50 % grey → 50 % black.
    let grey = group("0 0 100 100", "", b"0.5 g 0 0 100 100 re f");
    let r = render(&page(
        b"/G gs 0 g 0 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[],
        vec![(10, grey)],
    ))
    .unwrap();
    assert!(
        near(px(&r, 50, 50), (128, 128, 128), 4),
        "{:?}",
        px(&r, 50, 50)
    );
}

#[test]
fn backdrop_colour_fills_outside_the_mask_bbox_and_tr_inverts() {
    // The mask group only covers the left half; outside its /BBox the
    // backdrop /BC decides. Default black → nothing painted on the
    // right; /BC [1 1 1] → everything painted on the right.
    let small = group("0 0 50 100", "", b"1 g 0 0 50 100 re f");
    let r = render(&page(
        b"/G gs 0 g 0 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[],
        vec![(10, small.clone())],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 50), BLACK);
    assert_eq!(
        px(&r, 75, 50),
        WHITE,
        "outside the bbox: black backdrop = masked out"
    );
    let r = render(&page(
        b"/G gs 0 g 0 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R /BC [1 1 1] >> >>",
        &[],
        vec![(10, small.clone())],
    ))
    .unwrap();
    assert_eq!(px(&r, 75, 50), BLACK, "white backdrop = fully visible");
    // /TR inverts the mask: the lit half becomes hidden.
    let r = render(&page(
        b"/G gs 0 g 0 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R \
         /TR << /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >> >> >>",
        &[],
        vec![(10, half_mask())],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 50), WHITE);
    assert_eq!(px(&r, 75, 50), BLACK);
}

#[test]
fn alpha_mask_uses_the_group_alpha() {
    // /S /Alpha: the group's painted area (left half, any colour) is
    // the mask; its colour does not matter.
    let g = group("0 0 100 100", "", b"0 g 0 0 50 100 re f");
    let r = render(&page(
        b"/G gs 1 0 0 rg 0 0 100 100 re f",
        "/G << /SMask << /S /Alpha /G 10 0 R >> >>",
        &[],
        vec![(10, g)],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 50), RED);
    assert_eq!(px(&r, 75, 50), WHITE);
}

#[test]
fn mask_uses_the_ctm_at_gs_time_and_combines_with_clips() {
    // The mask is placed by the CTM in force when `gs` runs: shifting
    // the CTM by 25 afterwards does not move it, shifting it before
    // does.
    let r = render(&page(
        b"/G gs 1 0 0 1 25 0 cm 0 g -25 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[],
        vec![(10, half_mask())],
    ))
    .unwrap();
    assert_eq!(px(&r, 25, 50), BLACK);
    assert_eq!(px(&r, 75, 50), WHITE);
    let r = render(&page(
        b"1 0 0 1 25 0 cm /G gs 0 g -25 0 100 100 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[],
        vec![(10, half_mask())],
    ))
    .unwrap();
    assert_eq!(px(&r, 60, 50), BLACK, "mask moved right by 25");
    assert_eq!(px(&r, 90, 50), WHITE);
    // A clip set after the mask intersects with it; /SMask /None drops
    // the mask but keeps that clip; Q restores everything.
    let r = render(&page(
        b"q /G gs 0 0 100 50 re W n 0 g 0 0 100 100 re f /N gs 0 0 1 rg 0 0 100 100 re f Q \
          1 0 0 rg 0 50 100 50 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >> /N << /SMask /None >>",
        &[],
        vec![(10, half_mask())],
    ))
    .unwrap();
    assert_eq!(
        px(&r, 25, 75),
        (0, 0, 255),
        "blue after /None covers the black (clip kept)"
    );
    assert_eq!(
        px(&r, 75, 75),
        (0, 0, 255),
        "right half visible once the mask is gone"
    );
    assert_eq!(px(&r, 50, 25), RED, "after Q the clip is gone too");
}

#[test]
fn mask_applies_once_to_a_group_and_q_restores() {
    // Two overlapping opaque squares inside a transparency group under
    // a 50 % mask: the overlap is as light as the rest (the mask is
    // applied to the group result, not per element).
    let grey = group("0 0 100 100", "", b"0.5 g 0 0 100 100 re f");
    let content = group("0 0 100 100", "", b"0 g 10 10 50 50 re f 30 30 50 50 re f");
    let r = render(&page(
        b"q /G gs /F Do Q 0 g 90 90 10 10 re f",
        "/G << /SMask << /S /Luminosity /G 10 0 R >> >>",
        &[("F", 11)],
        vec![(10, grey), (11, content)],
    ))
    .unwrap();
    let single = px(&r, 20, 80);
    let overlap = px(&r, 40, 60);
    assert!(near(single, (128, 128, 128), 4), "{single:?}");
    assert!(
        near(overlap, single, 4),
        "overlap {overlap:?} vs {single:?}"
    );
    assert_eq!(px(&r, 95, 5), BLACK, "after Q the mask is gone");
}

#[test]
fn isolated_and_non_isolated_groups_with_a_blend_mode_inside() {
    // Page: left half red. A group paints a Multiply black square over
    // the middle. Isolated: Multiply sees a transparent backdrop →
    // the square is plain black everywhere. Non-isolated: Multiply
    // sees the page → still black (black × anything). Use a 50 % grey
    // square instead so the difference shows: isolated → grey over
    // red; non-isolated → red × grey = dark red on the left.
    let iso = group("0 0 100 100", "/I true", b"/M gs 0.5 g 25 25 50 50 re f");
    let non = group("0 0 100 100", "/I false", b"/M gs 0.5 g 25 25 50 50 re f");
    // The group XObject's own resources carry the blend-mode state.
    let with_res = |g: Vec<u8>| -> Vec<u8> {
        let needle = b"/Group <<";
        let pos = g.windows(needle.len()).position(|w| w == needle).unwrap();
        let mut out = g[..pos].to_vec();
        out.extend_from_slice(b"/Resources << /ExtGState << /M << /BM /Multiply >> >> >> ");
        out.extend_from_slice(&g[pos..]);
        out
    };
    let r = render(&page(
        b"1 0 0 rg 0 0 50 100 re f /H gs /F Do",
        "/H << /ca 1 >>",
        &[("F", 10)],
        vec![(10, with_res(iso))],
    ))
    .unwrap();
    assert!(
        near(px(&r, 40, 50), (128, 128, 128), 4),
        "isolated: {:?}",
        px(&r, 40, 50)
    );
    assert!(near(px(&r, 60, 50), (128, 128, 128), 4));
    // Non-isolated with group alpha 0.5 so it goes through the layer:
    // result = page over (page × grey) at 50 %: red side → red blended
    // with dark red = (191, 0, 0); white side → white with grey = 191.
    let r = render(&page(
        b"1 0 0 rg 0 0 50 100 re f /H gs /F Do",
        "/H << /ca 0.5 >>",
        &[("F", 10)],
        vec![(10, with_res(non))],
    ))
    .unwrap();
    assert!(
        near(px(&r, 40, 50), (191, 0, 0), 6),
        "non-isolated red side: {:?}",
        px(&r, 40, 50)
    );
    assert!(
        near(px(&r, 60, 50), (191, 191, 191), 6),
        "non-isolated white side: {:?}",
        px(&r, 60, 50)
    );
    assert_eq!(
        px(&r, 10, 50),
        RED,
        "outside the square the page is untouched"
    );
}

#[test]
fn non_separable_blend_modes_on_images_and_groups() {
    // A pure red 1×1 image blended with /Luminosity over a 50 % grey
    // page: Luminosity keeps the backdrop's hue (none) and takes the
    // source's luminosity (0.3) → grey 77.
    let im = stream_obj(
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB \
         /BitsPerComponent 8",
        b"\xff\x00\x00",
    );
    let r = render(&page(
        b"0.5 g 0 0 100 100 re f /L gs q 50 0 0 50 25 25 cm /Im Do Q",
        "/L << /BM /Luminosity >>",
        &[("Im", 10)],
        vec![(10, im.clone())],
    ))
    .unwrap();
    assert!(
        near(px(&r, 50, 50), (77, 77, 77), 4),
        "{:?}",
        px(&r, 50, 50)
    );
    assert!(near(px(&r, 10, 10), (128, 128, 128), 2));
    // /Hue of red over grey: grey has no saturation → stays grey.
    let r = render(&page(
        b"0.5 g 0 0 100 100 re f /L gs q 50 0 0 50 25 25 cm /Im Do Q",
        "/L << /BM /Hue >>",
        &[("Im", 10)],
        vec![(10, im.clone())],
    ))
    .unwrap();
    assert!(
        near(px(&r, 50, 50), (128, 128, 128), 4),
        "{:?}",
        px(&r, 50, 50)
    );
    // /Color of red over 50 % grey: red's hue and saturation with the
    // backdrop's luminosity 0.5 → a red whose luminosity is 0.5.
    let r = render(&page(
        b"0.5 g 0 0 100 100 re f /L gs q 50 0 0 50 25 25 cm /Im Do Q",
        "/L << /BM /Color >>",
        &[("Im", 10)],
        vec![(10, im)],
    ))
    .unwrap();
    let c = px(&r, 50, 50);
    let lum = 0.3 * c.0 as f64 + 0.59 * c.1 as f64 + 0.11 * c.2 as f64;
    assert!(
        (lum - 128.0).abs() < 6.0 && c.0 > c.1 && c.1 == c.2,
        "{c:?}"
    );
    // The same mode on a transparency group goes through the layer path.
    let g = group("0 0 100 100", "/I true", b"1 0 0 rg 25 25 50 50 re f");
    let r = render(&page(
        b"0.5 g 0 0 100 100 re f /L gs /F Do",
        "/L << /BM /Luminosity >>",
        &[("F", 10)],
        vec![(10, g)],
    ))
    .unwrap();
    assert!(
        near(px(&r, 50, 50), (77, 77, 77), 4),
        "{:?}",
        px(&r, 50, 50)
    );
}

#[test]
fn malformed_soft_masks_and_knockout_are_refused() {
    let ok = half_mask();
    for (gs, what) in [
        ("/G << /SMask << /S /Bogus /G 10 0 R >> >>", "bad /S"),
        ("/G << /SMask << /S /Luminosity >> >>", "no /G"),
        (
            "/G << /SMask << /S /Luminosity /G 4 0 R >> >>",
            "/G not a group",
        ),
        ("/G << /SMask 5 >>", "not a dict"),
        (
            "/G << /SMask << /S /Luminosity /G 10 0 R /BC [1 1] >> >>",
            "/BC component count",
        ),
        (
            "/G << /SMask << /S /Luminosity /G 10 0 R /TR /Bogus >> >>",
            "/TR name",
        ),
    ] {
        let err = render(&page(
            b"/G gs 0 g 0 0 100 100 re f",
            gs,
            &[],
            vec![(10, ok.clone())],
        ))
        .unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{what}: {err:?}");
    }
    // Knockout groups are rendered (tests/render_knockout.rs).
}
