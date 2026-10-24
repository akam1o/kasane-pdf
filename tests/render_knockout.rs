//! Knockout transparency groups (§11.4.8): every object composites
//! against the group's initial backdrop and replaces, within its
//! shape, what earlier objects of the group painted.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

/// Page 100 × 100 with form XObject /F (object 5) drawn by `/F Do`;
/// the form carries ExtGState /H (ca = ca_fill) in its own resources.
fn page_with_group(group_extra: &str, ca_fill: f64, content: &[u8]) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
               /Resources << /XObject << /F 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"/F Do")),
        (
            5,
            stream_obj(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                     /Group << /S /Transparency /CS /DeviceRGB {group_extra} >> \
                     /Resources << /ExtGState << /H << /ca {ca_fill} /CA {ca_fill} >> >> >>"
                ),
                content,
            ),
        ),
    ])
}

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let i = (y as usize * r.stride()) + x as usize * 4;
    let p = &r.pixels()[i..i + 4];
    (p[0], p[1], p[2])
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: u8) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol as i32
        && (a.1 as i32 - b.1 as i32).abs() <= tol as i32
        && (a.2 as i32 - b.2 as i32).abs() <= tol as i32
}

/// Two half-transparent squares, red then blue, overlapping in the
/// middle. Red covers x 10..60, blue x 40..90, both y 10..90 (PDF
/// space, y up; pixel row = 100 − y).
const TWO_SQUARES: &[u8] = b"/H gs 1 0 0 rg 10 10 50 80 re f 0 0 1 rg 40 10 50 80 re f";

#[test]
fn knockout_replaces_earlier_objects_within_the_shape() {
    // Isolated knockout: in the overlap the blue square alone, at 50 %
    // over white — the red underneath is knocked out.
    let r = render(&page_with_group("/I true /K true", 0.5, TWO_SQUARES)).unwrap();
    assert!(
        near(px(&r, 50, 50), (128, 128, 255), 3),
        "{:?}",
        px(&r, 50, 50)
    );
    // Red alone where only red was painted; blue alone likewise.
    assert!(
        near(px(&r, 20, 50), (255, 128, 128), 3),
        "{:?}",
        px(&r, 20, 50)
    );
    assert!(
        near(px(&r, 80, 50), (128, 128, 255), 3),
        "{:?}",
        px(&r, 80, 50)
    );
    assert_eq!(px(&r, 5, 5), (255, 255, 255));
    assert!(r.font_substitutions().is_empty());
}

#[test]
fn without_knockout_the_objects_accumulate() {
    // The same group without /K: blue at 50 % over red at 50 % over
    // white — the reference the knockout result must differ from.
    let r = render(&page_with_group("/I true", 0.5, TWO_SQUARES)).unwrap();
    assert!(
        near(px(&r, 50, 50), (128, 64, 191), 3),
        "{:?}",
        px(&r, 50, 50)
    );
}

#[test]
fn non_isolated_knockout_matches_isolated_over_an_opaque_page() {
    // Over an opaque white page the initial backdrop is the page in
    // both cases, so the results agree after backdrop removal.
    let iso = render(&page_with_group("/I true /K true", 0.5, TWO_SQUARES)).unwrap();
    let non = render(&page_with_group("/K true", 0.5, TWO_SQUARES)).unwrap();
    for (x, y) in [(50, 50), (20, 50), (80, 50), (5, 5), (45, 15)] {
        assert!(
            near(px(&iso, x, y), px(&non, x, y), 4),
            "({x},{y}) {:?} vs {:?}",
            px(&iso, x, y),
            px(&non, x, y)
        );
    }
}

#[test]
fn knockout_with_group_alpha_and_opaque_objects() {
    // Opaque objects inside, group ca applied by the caller through a
    // page-level ExtGState: knockout changes nothing for opaque
    // objects (the last one wins in the overlap either way).
    let r = render(&page_with_group("/I true /K true", 1.0, TWO_SQUARES)).unwrap();
    assert_eq!(px(&r, 50, 50), (0, 0, 255));
    assert_eq!(px(&r, 20, 50), (255, 0, 0));
}

#[test]
fn knockout_object_count_is_bounded() {
    let mut content = b"/H gs 1 0 0 rg ".to_vec();
    for i in 0..10 {
        content.extend_from_slice(format!("{} 10 5 5 re f ", 10 + i * 8).as_bytes());
    }
    let pdf = page_with_group("/I true /K true", 0.5, &content);
    let doc = Document::from_bytes(&pdf).unwrap();
    let opts = RenderOptions::default().with_limits(Limits::default().with_max_knockout_objects(4));
    let e = render_page(&doc, &doc.pages()[0], &opts).unwrap_err();
    assert!(
        matches!(
            e,
            Error::LimitExceeded {
                limit: "max_knockout_objects",
                ..
            }
        ),
        "{e:?}"
    );
    render_page(&doc, &doc.pages()[0], &RenderOptions::default()).unwrap();
}
