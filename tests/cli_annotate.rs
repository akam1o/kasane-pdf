//! CLI `annotate-rect` subcommand tests — /Square annotation
//! creation, arg validation, and failure-preserves-output.

mod common;
use common::{bin, fixture, minimal_pdf, tmpdir};
use std::fs;
use std::path::Path;

fn annot_args<'a>(page: &'a str, out: &'a Path) -> Vec<String> {
    vec![
        "annotate-rect".into(),
        "PLACEHOLDER".into(),
        page.to_string(),
        "--rect".into(),
        "20".into(),
        "30".into(),
        "120".into(),
        "80".into(),
        "--color".into(),
        "1".into(),
        "0".into(),
        "0".into(),
        "--width".into(),
        "2".into(),
        "--output".into(),
        out.to_string_lossy().into_owned(),
    ]
}

#[test]
fn cli_annotate_rect_creates_annot() {
    let dir = tmpdir("annot");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let mut a = annot_args("1", &dst);
    a[1] = src.to_string_lossy().into_owned();
    let st = bin().args(&a).status().unwrap();
    assert!(st.success());
    let doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert_eq!(doc.page_count(), 1);
    let bytes = fs::read(&dst).unwrap();
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("/Square"));
}

#[test]
fn cli_annotate_rect_same_file_refused() {
    let dir = tmpdir("annot-same");
    let src = dir.join("in.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let mut a = annot_args("1", &src);
    a[1] = src.to_string_lossy().into_owned();
    let st = bin().args(&a).status().unwrap();
    assert!(!st.success());
}

#[test]
fn cli_annotate_rect_invalid_args_diagnosed() {
    let dir = tmpdir("annot-badarg");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    // Missing --width.
    let st = bin()
        .args(["annotate-rect"])
        .arg(&src)
        .arg("1")
        .args([
            "--rect", "0", "0", "9", "9", "--color", "0", "0", "0", "--output",
        ])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
    // Out-of-range colour.
    let st = bin()
        .args(["annotate-rect"])
        .arg(&src)
        .arg("1")
        .args([
            "--rect", "0", "0", "9", "9", "--color", "2", "0", "0", "--width", "1", "--output",
        ])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
    // Bad page number.
    let st = bin()
        .args(["annotate-rect"])
        .arg(&src)
        .arg("0")
        .args([
            "--rect", "0", "0", "9", "9", "--color", "0", "0", "0", "--width", "1", "--output",
        ])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
}

#[test]
fn cli_annotate_rect_failure_preserves_output() {
    let dir = tmpdir("annot-fail-preserve");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, fs::read(fixture("encrypted.pdf")).unwrap()).unwrap();
    fs::write(&dst, b"PREVIOUS").unwrap();
    let mut a = annot_args("1", &dst);
    a[1] = src.to_string_lossy().into_owned();
    let st = bin().args(&a).status().unwrap();
    assert!(!st.success());
    assert_eq!(fs::read(&dst).unwrap(), b"PREVIOUS");
}
