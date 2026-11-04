//! `max_xobject_depth` (v0.5.1 R2): the default (48) and its boundary
//! on every path that nests the content interpreter — Form XObjects,
//! transparency groups, soft-mask groups, annotation appearances and
//! Type 3 glyph procedures — cycles, the state after a refusal, and the
//! stack the default needs (in a child process: an overflow must never
//! take the test runner down).

mod common;

use common::build_pdf;
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};

fn stream(dict: &str, data: &str) -> Vec<u8> {
    format!(
        "<< {dict} /Length {} >>\nstream\n{data}\nendstream",
        data.len()
    )
    .into_bytes()
}

const LEAF: &str = "0 0 1 rg 0 0 10 10 re f";

/// A page whose content reaches `n` nested levels of `kind`.
fn nested(kind: &str, n: u32) -> Vec<u8> {
    let mut o: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
    ];
    let (mut res, mut content, mut annots) = (
        "/XObject << /X 100 0 R >>".to_string(),
        "/X Do".to_string(),
        String::new(),
    );
    if kind == "type3" {
        for i in 0..n {
            let id = 100 + 2 * i;
            let last = i + 1 == n;
            let glyph = if last {
                format!("10 0 d0 {LEAF}")
            } else {
                "10 0 d0 BT /F 1 Tf (a) Tj ET".to_string()
            };
            let r = if last {
                String::new()
            } else {
                format!("/Resources << /Font << /F {} 0 R >> >>", id + 2)
            };
            o.push((id, format!("<< /Type /Font /Subtype /Type3 /FontBBox [0 0 10 10] /FontMatrix [0.1 0 0 0.1 0 0] /CharProcs << /a {} 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [10] {r} >>", id + 1).into_bytes()));
            o.push((id + 1, stream("", &glyph)));
        }
        res = "/Font << /F 100 0 R >>".into();
        content = "BT /F 10 Tf 1 1 Td (a) Tj ET".into();
    } else {
        for i in 0..n {
            let id = 100 + i;
            let last = i + 1 == n;
            let next = format!("/XObject << /X {} 0 R >>", id + 1);
            let s = match (kind, last) {
                ("form" | "annot", true) => stream("/Type /XObject /Subtype /Form /BBox [0 0 20 20]", LEAF),
                ("form" | "annot", false) => stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Resources << {next} >>"), "/X Do"),
                ("group", true) => stream("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Group << /S /Transparency /I true >>", LEAF),
                ("group", false) => stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Group << /S /Transparency /I true >> /Resources << {next} >>"), "/X Do"),
                ("smask", true) => stream("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Group << /S /Transparency >>", LEAF),
                ("smask", false) => stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Group << /S /Transparency >> /Resources << /ExtGState << /G << /SMask << /S /Alpha /G {} 0 R >> >> >> >>", id + 1), &format!("/G gs {LEAF}")),
                _ => unreachable!(),
            };
            o.push((id, s));
        }
        if kind == "annot" {
            res.clear();
            content.clear();
            o.push((
                90,
                b"<< /Type /Annot /Subtype /Square /Rect [0 0 20 20] /AP << /N 100 0 R >> >>"
                    .to_vec(),
            ));
            annots = "/Annots [90 0 R]".into();
        }
    }
    o.push((3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 50] /Resources << {res} >> /Contents 4 0 R {annots} >>").into_bytes()));
    o.push((4, stream("", &content)));
    build_pdf(&o)
}

fn render(b: &[u8], limits: Option<Limits>) -> Result<(), Error> {
    let d = Document::from_bytes(b).unwrap();
    let mut opts = RenderOptions::new().with_dpi(18.0);
    if let Some(l) = limits {
        opts = opts.with_limits(l);
    }
    render_page(&d, &d.pages()[0], &opts).map(|_| ())
}

const KINDS: [&str; 5] = ["form", "group", "smask", "annot", "type3"];

#[test]
fn the_default_is_48() {
    assert_eq!(Limits::default().max_xobject_depth, 48);
}

/// 48 levels render; 49 are refused with a typed error, on every path.
#[test]
fn boundary_on_every_nesting_path() {
    for kind in KINDS {
        for n in [1, 47, 48] {
            render(&nested(kind, n), None).unwrap_or_else(|e| panic!("{kind} {n}: {e:?}"));
        }
        match render(&nested(kind, 49), None) {
            Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                max: 48,
            }) => {}
            other => panic!("{kind} 49: {other:?}"),
        }
        // A caller's own limit still applies both ways.
        assert!(render(
            &nested(kind, 49),
            Some(Limits::default().with_max_xobject_depth(49))
        )
        .is_ok());
        assert!(matches!(
            render(
                &nested(kind, 3),
                Some(Limits::default().with_max_xobject_depth(2))
            ),
            Err(Error::LimitExceeded {
                limit: "max_xobject_depth",
                ..
            })
        ));
    }
}

/// A form that invokes itself, directly or through another, stops with
/// `CycleDetected` long before the depth limit.
#[test]
fn cycles_stop() {
    for (a, b) in [("/X 10 0 R", "/X 10 0 R"), ("/X 11 0 R", "/X 10 0 R")] {
        let p = build_pdf(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 50] /Resources << /XObject << /X 10 0 R >> >> /Contents 4 0 R >>".to_vec()),
            (4, stream("", "/X Do")),
            (10, stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Resources << /XObject << {a} >> >>"), "/X Do")),
            (11, stream(&format!("/Type /XObject /Subtype /Form /BBox [0 0 20 20] /Resources << /XObject << {b} >> >>"), "/X Do")),
        ]);
        assert!(
            matches!(render(&p, None), Err(Error::CycleDetected { .. })),
            "{a}"
        );
    }
}

/// A refusal leaves nothing behind: the same document renders a valid
/// page afterwards exactly as a fresh one does.
#[test]
fn state_after_a_refusal() {
    let deep = nested("smask", 60);
    let ok = nested("smask", 10);
    let d = Document::from_bytes(&deep).unwrap();
    let opts = RenderOptions::new().with_dpi(18.0);
    assert!(render_page(&d, &d.pages()[0], &opts).is_err());
    assert!(
        render_page(&d, &d.pages()[0], &opts).is_err(),
        "refused again, same way"
    );
    let fresh = Document::from_bytes(&ok).unwrap();
    let a = render_page(&fresh, &fresh.pages()[0], &opts).unwrap();
    let again = Document::from_bytes(&ok).unwrap();
    assert!(render(&deep, None).is_err());
    let b = render_page(&again, &again.pages()[0], &opts).unwrap();
    assert_eq!(a.pixels(), b.pixels());
}

/// Eight tiling patterns, each cell painting the next through an
/// inline stencil image (`BI /IM true … EI` under a pattern colour).
fn inline_pattern_chain() -> Vec<u8> {
    let paint = "/Pattern cs /P scn q 20 0 0 20 0 0 cm BI /W 1 /H 1 /IM true /BPC 1 ID \x00 EI Q";
    let mut o: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 50] /Resources << /Pattern << /P 100 0 R >> >> /Contents 4 0 R >>".to_vec()),
        (4, stream("", paint)),
    ];
    for i in 0..8 {
        let (res, body) = if i == 7 {
            (String::new(), LEAF.to_string())
        } else {
            (
                format!("/Pattern << /P {} 0 R >>", 101 + i),
                paint.to_string(),
            )
        };
        o.push((100 + i, stream(&format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << {res} >>"), &body)));
    }
    build_pdf(&o)
}

/// The stack the default needs, in a child process (this test binary,
/// filtered to this test, the thread's stack in KiB in the environment):
/// each kind at the default depth and the inline-image pattern chain,
/// in a thread of 2 MiB (std's default, and a test thread's) in debug
/// builds and 512 KiB in release builds. The same child with a 64 KiB
/// stack must die of the overflow — showing the check can fail, and
/// that an overflow takes the child down, never the runner.
#[test]
fn default_depth_fits_the_default_thread_stack() {
    const CHILD: &str = "KASANE_R2_STACK_CHILD_KIB";
    if let Some(kib) = std::env::var_os(CHILD) {
        let stack = kib.to_str().unwrap().parse::<usize>().unwrap() << 10;
        let mut docs: Vec<(&str, Vec<u8>)> = KINDS.iter().map(|k| (*k, nested(k, 48))).collect();
        docs.push(("inline pattern chain", inline_pattern_chain()));
        for (kind, b) in docs {
            std::thread::Builder::new()
                .stack_size(stack)
                .spawn(move || render(&b, None).unwrap())
                .unwrap()
                .join()
                .unwrap_or_else(|_| panic!("{kind}"));
        }
        return;
    }
    let child = |kib: usize| {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "default_depth_fits_the_default_thread_stack",
                "--test-threads",
                "1",
            ])
            .env(CHILD, kib.to_string())
            .output()
            .unwrap()
    };
    let fits = child(if cfg!(debug_assertions) { 2048 } else { 512 });
    assert!(
        fits.status.success(),
        "child: {:?}\n{}",
        fits.status,
        String::from_utf8_lossy(&fits.stderr)
    );
    let small = child(64);
    assert!(
        !small.status.success(),
        "48 levels in a 64 KiB stack should overflow"
    );
    // An overflow kills the child with a signal (Rust's handler prints
    // "has overflowed its stack" where it has one); an ordinary failure
    // (a panic: exit status 101) would not prove the check can fail.
    let stderr = String::from_utf8_lossy(&small.stderr);
    assert!(
        small.status.code().is_none() || stderr.contains("overflow"),
        "the 64 KiB child failed for another reason: {:?}\n{stderr}",
        small.status
    );
}
