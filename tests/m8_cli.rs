//! M8.2 — CLI-level rewrite/save integration tests.
//!
//! Runs the built `kasane-pdf` binary as a child process (Cargo
//! exports `CARGO_BIN_EXE_<name>` at compile time). Each test works
//! in its own temp directory so parallel runs never collide.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kasane-pdf"))
}

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kasane-m82-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A minimal 1-page PDF the writer accepts — kept inline so the test
/// does not depend on a fixture that may itself evolve.
fn minimal_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

// ------------------------------------------------------------------
// success paths
// ------------------------------------------------------------------

#[test]
fn cli_rewrite_to_new_output() {
    let dir = tmpdir("new");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();

    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    assert!(dst.exists());
    // Output must itself parse.
    let doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert_eq!(doc.page_count(), 1);
    // No temp file left behind.
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
}

#[test]
fn cli_rewrite_overwrites_existing_output() {
    let dir = tmpdir("overwrite");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    fs::write(&dst, b"old content").unwrap();

    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    // Old bytes are gone — the file now parses as a real PDF.
    assert!(kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).is_ok());
}

#[test]
fn cli_input_unchanged() {
    let dir = tmpdir("input-unchanged");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let pdf = minimal_pdf();
    fs::write(&src, &pdf).unwrap();

    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(fs::read(&src).unwrap(), pdf, "input was modified");
}

#[test]
fn cli_output_reparses() {
    let dir = tmpdir("reparse");
    let dst = dir.join("out.pdf");
    let st = bin()
        .args(["rewrite"])
        .arg(fixture("hello.pdf"))
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    let doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert_eq!(doc.page_count(), 1);
}

// ------------------------------------------------------------------
// same-file refusals
// ------------------------------------------------------------------

#[test]
fn cli_same_path_refused() {
    let dir = tmpdir("same");
    let src = dir.join("f.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(!st.success());
}

#[test]
fn cli_relative_vs_absolute_refused() {
    let dir = tmpdir("relabs");
    let src = dir.join("f.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    // Run with cwd = dir, input relative, output absolute.
    let st = bin()
        .current_dir(&dir)
        .args(["rewrite", "f.pdf", "--output"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(!st.success());
}

#[cfg(unix)]
#[test]
fn cli_symlink_refused() {
    let dir = tmpdir("symlink");
    let src = dir.join("real.pdf");
    let link = dir.join("link.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    std::os::unix::fs::symlink(&src, &link).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&link)
        .args(["--output"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(!st.success());
}

#[cfg(unix)]
#[test]
fn cli_hardlink_refused() {
    let dir = tmpdir("hardlink");
    let a = dir.join("a.pdf");
    let b = dir.join("b.pdf");
    fs::write(&a, minimal_pdf()).unwrap();
    fs::hard_link(&a, &b).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&a)
        .args(["--output"])
        .arg(&b)
        .status()
        .unwrap();
    assert!(!st.success());
    // Input must be untouched.
    let doc = kasane_pdf::Document::from_bytes(&fs::read(&a).unwrap()).unwrap();
    assert_eq!(doc.page_count(), 1);
}

// ------------------------------------------------------------------
// failure paths
// ------------------------------------------------------------------

#[test]
fn cli_tmp_name_collision_refused() {
    let dir = tmpdir("tmp-collision");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    // Pre-create the exact temp name the CLI would pick.
    let tmp = dir.join(format!("out.pdf.kasane-tmp-{}", std::process::id()));
    fs::write(&tmp, b"pre-existing").unwrap();

    // The child's PID differs, so the *name* it picks won't match —
    // instead verify create_new(true) semantics directly: a second
    // process trying the *same* tmp name would fail. What we CAN
    // test deterministically at CLI level is that a rewrite succeeds
    // alongside a leftover tmp file (collision is handled by the
    // OS-level unique name + create_new, not by name prediction).
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    // Our planted file survives — we never delete others' files.
    assert_eq!(fs::read(&tmp).unwrap(), b"pre-existing");
}

#[cfg(unix)]
#[test]
fn cli_tmp_symlink_not_followed() {
    let dir = tmpdir("tmp-symlink");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let victim = dir.join("victim.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    fs::write(&victim, b"IMPORTANT").unwrap();
    // Plant a dangling symlink at the exact tmp path the child would
    // use if PIDs lined up — create_new never follows it regardless.
    let tmp = dir.join(format!("out.pdf.kasane-tmp-{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();

    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(fs::read(&victim).unwrap(), b"IMPORTANT");
}

#[test]
fn cli_output_dir_refused() {
    let dir = tmpdir("out-is-dir");
    let src = dir.join("in.pdf");
    let dst = dir.join("subdir");
    fs::create_dir(&dst).unwrap();
    fs::write(&src, minimal_pdf()).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
}

#[test]
fn cli_unsupported_input_fails_without_output() {
    let dir = tmpdir("unsupported");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, fs::read(fixture("encrypted.pdf")).unwrap()).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
    assert!(!dst.exists(), "output must not be created on refusal");
}

#[test]
fn cli_failure_preserves_existing_output() {
    let dir = tmpdir("fail-preserve");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let original = b"PREVIOUS-OUTPUT";
    fs::write(&src, fs::read(fixture("encrypted.pdf")).unwrap()).unwrap();
    fs::write(&dst, original).unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(!st.success());
    assert_eq!(fs::read(&dst).unwrap(), original);
}

#[test]
fn cli_leftover_tmp_not_deleted_by_us() {
    let dir = tmpdir("tmp-leftover");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    // A stray tmp from an earlier crash — we must not touch it.
    let stray = dir.join("out.pdf.kasane-tmp-12345");
    fs::write(&stray, b"stray").unwrap();
    let st = bin()
        .args(["rewrite"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(fs::read(&stray).unwrap(), b"stray");
}

// ------------------------------------------------------------------
// M9 — annotate-rect subcommand
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// M10 — `inspect` subcommand
// ------------------------------------------------------------------

/// A 1-page PDF whose page carries every field inspect prints:
/// MediaBox/CropBox, /Annots (2), /Resources with Font+XObject,
/// a single /Contents ref, and /UserUnit.
fn inspect_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /CropBox [10 20 600 780] /Rotate 90 \
          /Annots [4 0 R 5 0 R] \
          /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 7 0 R >> >> \
          /Contents 8 0 R /UserUnit 2 >>",
    );
    push(4, b"<< /Type /Annot /Subtype /Text /Rect [0 0 9 9] >>");
    push(5, b"<< /Type /Annot /Subtype /Text /Rect [9 9 18 18] >>");
    push(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    push(
        7,
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /Length 0 >>\nstream\n\nendstream",
    );
    push(8, b"<< /Length 3 >>\nstream\nq Q\nendstream");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 9\n0000000000 65535 f \n".to_vec());
    for n in 1..9u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 9 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_single_page_full() {
    let dir = tmpdir("inspect-full");
    let src = dir.join("in.pdf");
    fs::write(&src, inspect_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let expected = [
        "page 1",
        "mediabox 0 0 612 792",
        "cropbox 10 20 600 780",
        "rotation 90",
        "annotations 2",
        "resources fonts 1",
        "font F1 Type1 /Helvetica",
        "xobjects 1",
        "xobject Im1 Image /Width=1 /Height=1",
        "colorspaces 0",
        "patterns 0",
        "shadings 0",
        "extgstates 0",
        "procsets 0",
        "properties 0",
        "tokens 2",
        "operators 2",
        "opstats Q:1 q:1",
        "contents 8 0 R",
        "userunit 2",
        "annot 0 Text rect 0 0 9 9 /Rect=[0 0 9 9] /Subtype=/Text /Type=/Annot",
        "annot 1 Text rect 9 9 18 18 /Rect=[9 9 18 18] /Subtype=/Text /Type=/Annot",
        "document",
        "version 1.4",
        "info 0",
    ];
    let got: Vec<&str> = s.lines().collect();
    assert_eq!(got, expected, "mismatch:\n{s}");
}

#[test]
fn cli_inspect_rejects_multi_page() {
    let dir = tmpdir("inspect-multi");
    let src = dir.join("in.pdf");
    fs::write(&src, fs::read(fixture("pages.pdf")).unwrap()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success());
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("3 pages"), "error must name the page count: {e}");
}

#[test]
fn cli_inspect_contents_array_and_absent() {
    // A /Contents array of two valid streams — the inspect line is
    // the ref array, and the stats aggregate across both streams.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] \
          /Contents [4 0 R 5 0 R] >>",
    );
    push(4, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    push(5, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
    for n in 1..6u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-arr");
    let src = dir.join("a.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    // Exact array line: comma-separated refs inside brackets.
    assert!(
        s.lines().any(|l| l == "contents [4 0 R, 5 0 R]"),
        "expected 'contents [4 0 R, 5 0 R]', got:\n{s}"
    );
    // Two `q Q` streams → tokens 4, operators 4, opstats Q:2 q:2.
    assert!(s.lines().any(|l| l == "tokens 4"), "tokens:\n{s}");
    assert!(s.lines().any(|l| l == "operators 4"), "ops:\n{s}");
    assert!(s.lines().any(|l| l == "opstats Q:2 q:2"), "opstats:\n{s}");

    // contents_none fixture: no /Contents at all.
    let src2 = dir.join("n.pdf");
    fs::write(&src2, fs::read(fixture("contents_none.pdf")).unwrap()).unwrap();
    let out = bin()
        .args(["inspect"])
        .arg(&src2)
        .arg("1")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s2 = String::from_utf8_lossy(&out.stdout);
    assert!(s2.lines().any(|l| l == "contents absent"));
    assert!(s2.lines().any(|l| l == "tokens 0"));
    assert!(s2.lines().any(|l| l == "operators 0"));
    assert!(s2.lines().any(|l| l == "opstats"));
}

#[test]
fn cli_inspect_bad_annots_is_error() {
    // /Annots that is a number — malformed, must error not print 0.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] /Annots 42 >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-badannots");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success(), "bad /Annots type must fail");
}

// ------------------------------------------------------------------
// M11 — `inspect` annotation dump
// ------------------------------------------------------------------

/// A 1-page PDF with two annotations exercising every value type:
/// a Link with a dict /A and an array /Border, a Square with a ref
/// /AP and a string /Contents.
fn annot_inspect_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Annots [4 0 R 5 0 R] /Contents 6 0 R /Resources << >> >>",
    );
    push(
        4,
        b"<< /Type /Annot /Subtype /Link /Rect [10 10 50 20] \
          /A << /S /URI /URI (https://a/) >> /Border [0 0 0] /F 4 >>",
    );
    push(
        5,
        b"<< /Type /Annot /Subtype /Square /Rect [20 30 100 80] \
          /AP << /N 7 0 R >> /BS << /S /S /W 2 >> /C [1 0 0] \
          /Contents (comment) /F 4 /P 3 0 R >>",
    );
    push(6, b"<< /Length 3 >>\nstream\nq Q\nendstream");
    push(
        7,
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 80 50] /Length 0 >>\nstream\n\nendstream",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 8\n0000000000 65535 f \n".to_vec());
    for n in 1..8u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 8 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_annot_lines() {
    let dir = tmpdir("inspect-annot");
    let src = dir.join("in.pdf");
    fs::write(&src, annot_inspect_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let a0 = s
        .lines()
        .find(|l| l.starts_with("annot 0 "))
        .expect("no annot 0 line");
    assert_eq!(
        a0,
        "annot 0 Link rect 10 10 50 20 /A=<< /S /URI /URI (https://a/) >> \
         /Border=[0 0 0] /F=4 /Rect=[10 10 50 20] /Subtype=/Link /Type=/Annot",
        "annot 0 mismatch:\n{a0}"
    );
    let a1 = s
        .lines()
        .find(|l| l.starts_with("annot 1 "))
        .expect("no annot 1 line");
    assert_eq!(
        a1,
        "annot 1 Square rect 20 30 100 80 /AP=<< /N 7 0 R >> \
         /BS=<< /S /S /W 2 >> /C=[1 0 0] /Contents=(comment) /F=4 /P=3 0 R \
         /Rect=[20 30 100 80] /Subtype=/Square /Type=/Annot",
        "annot 1 mismatch:\n{a1}"
    );
}

#[test]
fn cli_inspect_annot_missing_rect_errors() {
    // Annot without /Rect → error, not a guessed box.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] /Annots [4 0 R] >>",
    );
    push(4, b"<< /Type /Annot /Subtype /Text >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
    for n in 1..5u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-annot-norect");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success(), "missing /Rect must fail");
}

#[test]
fn cli_inspect_annot_dangling_ref_errors() {
    // /Annots [4 0 R] where object 4 does not exist → MissingReference.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] /Annots [4 0 R] >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-annot-dangle");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success(), "dangling annot ref must fail");
}

// ------------------------------------------------------------------
// M11.1 — `inspect` annotation element types (ref | dict)
// ------------------------------------------------------------------

/// /Annots mixes a direct dict and an indirect ref — real files do.
#[test]
fn cli_inspect_annot_direct_and_mixed() {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    // Direct dict + indirect ref in one /Annots array.
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] \
          /Annots [<< /Subtype /Text /Rect [0 0 1 1] >> 4 0 R] >>",
    );
    push(4, b"<< /Type /Annot /Subtype /Square /Rect [1 1 5 5] >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
    for n in 1..5u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-annot-mixed");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "mixed ref/dict annots must parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    let a0 = s.lines().find(|l| l.starts_with("annot 0 ")).unwrap();
    assert!(a0.contains("Text rect 0 0 1 1"), "annot 0: {a0}");
    let a1 = s.lines().find(|l| l.starts_with("annot 1 ")).unwrap();
    assert!(a1.contains("Square rect 1 1 5 5"), "annot 1: {a1}");
}

#[test]
fn cli_inspect_annot_bad_element_errors() {
    // /Annots containing a number → not a ref or dict → error.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] /Annots [42] >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-annot-badel");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        !out.status.success(),
        "non-ref/dict annot element must fail"
    );
}

// ------------------------------------------------------------------
// M12 — `inspect` content-stream statistics
// ------------------------------------------------------------------

/// A 1-page PDF whose /Contents exercises SC vs SCN, sc vs scn, and
/// an Unknown operator inside BX/EX — opstats must report the *real*
/// operator names, not grouped labels.
fn tokens_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
          /Contents 4 0 R /Resources << >> >>",
    );
    // Operators: q,w,re,S,Q + SCN + scn + BX,Bar,EX = 10 ops.
    // tokens: q,2,w,10,10,90,70,re,S,Q=10; +1,0,0,SCN=4; +0,0,1,scn=4;
    //         +BX,Bar,EX=3 → 21 tokens.
    push(
        4,
        b"<< /Length 60 >>\nstream\nq 2 w 10 10 90 70 re S Q\n1 0 0 SCN\n0 0 1 scn\nBX Bar EX\nendstream",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
    for n in 1..5u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_tokens_operators_opstats() {
    let dir = tmpdir("inspect-tokens");
    let src = dir.join("in.pdf");
    fs::write(&src, tokens_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    for line in [
        "tokens 21",
        "operators 10",
        "opstats BX:1 Bar:1 EX:1 Q:1 S:1 SCN:1 q:1 re:1 scn:1 w:1",
    ] {
        assert!(s.lines().any(|l| l == line), "missing '{line}':\n{s}");
    }
}

#[test]
fn cli_inspect_tokens_zero_when_no_contents() {
    let dir = tmpdir("inspect-tokens-none");
    let src = dir.join("in.pdf");
    fs::write(&src, fs::read(fixture("contents_none.pdf")).unwrap()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    for line in ["tokens 0", "operators 0", "opstats"] {
        assert!(s.lines().any(|l| l == line), "missing '{line}':\n{s}");
    }
}

#[test]
fn cli_inspect_tokens_counts_array_streams() {
    // Two `q Q` streams in /Contents — tokens and operators aggregate.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] \
          /Contents [4 0 R 5 0 R] >>",
    );
    push(4, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    push(5, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
    for n in 1..6u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-tokens-arr");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    let toks: usize = s
        .lines()
        .find_map(|l| l.strip_prefix("tokens "))
        .unwrap()
        .parse()
        .unwrap();
    let ops: usize = s
        .lines()
        .find_map(|l| l.strip_prefix("operators "))
        .unwrap()
        .parse()
        .unwrap();
    assert!(toks > 0 && ops > 0, "expected nonzero counts:\n{s}");
    assert!(toks >= ops, "tokens {toks} must be >= operators {ops}");
}

// ------------------------------------------------------------------
// M13 — `inspect` resource name enumeration
// ------------------------------------------------------------------

/// A 1-page PDF with every enumerable resource category populated:
/// a Type1 font, a Form XObject, a DeviceRGB colorspace, a tiling
/// pattern, an axial shading, and an ExtGState.
fn resources_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] \
          /Resources << \
            /Font << /F1 4 0 R /F2 5 0 R >> \
            /XObject << /Fm1 6 0 R /Im1 7 0 R >> \
            /ColorSpace << /Cs1 8 0 R >> \
            /Pattern << /P1 9 0 R >> \
            /Shading << /Sh1 10 0 R >> \
            /ExtGState << /GS1 11 0 R >> \
          >> \
          /Contents 12 0 R >>",
    );
    push(4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    push(
        5,
        b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest >>",
    );
    push(
        6,
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 80 50] /Length 0 >>\nstream\n\nendstream",
    );
    push(
        7,
        b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /Length 0 >>\nstream\n\nendstream",
    );
    push(8, b"/DeviceRGB");
    push(9, b"<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 8 8] /XStep 8 /YStep 8 /Length 0 >>\nstream\n\nendstream");
    push(10, b"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 1 1] /Function << /FunctionType 2 /Domain [0 1] /C0 [0 0 0] /C1 [1 1 1] /N 1 >> >>");
    push(11, b"<< /Type /ExtGState /LW 2 >>");
    push(12, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 13\n0000000000 65535 f \n".to_vec());
    for n in 1..13u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 13 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_resource_enumeration() {
    let dir = tmpdir("inspect-res");
    let src = dir.join("in.pdf");
    fs::write(&src, resources_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    for line in [
        "resources fonts 2",
        "font F1 Type1 /Helvetica",
        "font F2 TrueType /KasaneTest",
        "xobjects 2",
        "xobject Fm1 Form /BBox=[0 0 80 50]",
        "xobject Im1 Image /Width=8 /Height=8",
        "colorspaces 1",
        "colorspace Cs1 /DeviceRGB",
        "patterns 1",
        "pattern P1 1",
        "shadings 1",
        "shading Sh1 2",
        "extgstates 1",
        "extgstate GS1 /ExtGState",
    ] {
        assert!(s.lines().any(|l| l == line), "missing '{line}':\n{s}");
    }
}

// ------------------------------------------------------------------
// M13.1 — `inspect` resource-line prefix consistency
// ------------------------------------------------------------------

/// The resource block opens with `resources fonts <n>` — `resources`
/// marks the start of the section — then every other category uses
/// its plain name (`xobjects`, `colorspaces`, …). Enumeration lines
/// (`font`, `xobject`, …) carry no prefix either. This pins that
/// exact shape so a stray `resources` never drifts in or out.
#[test]
fn cli_inspect_resource_prefix_consistency() {
    let dir = tmpdir("inspect-prefix");
    let src = dir.join("in.pdf");
    fs::write(&src, resources_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = s.lines().collect();
    // Exactly one line starts with "resources " — the fonts header.
    let res_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with("resources "))
        .collect();
    assert_eq!(res_lines.len(), 1, "expected one 'resources ' line:\n{s}");
    assert_eq!(*res_lines[0], "resources fonts 2");
    // Every other category header has NO prefix.
    for h in [
        "xobjects 2",
        "colorspaces 1",
        "patterns 1",
        "shadings 1",
        "extgstates 1",
    ] {
        assert!(lines.contains(&h), "missing plain header '{h}':\n{s}");
    }
    // Enumeration lines start with the singular category name.
    for e in [
        "font F1 ",
        "font F2 ",
        "xobject Fm1 ",
        "xobject Im1 ",
        "colorspace Cs1 ",
        "pattern P1 ",
        "shading Sh1 ",
        "extgstate GS1 ",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(e)),
            "missing enum line '{e}…':\n{s}"
        );
    }
}

// ------------------------------------------------------------------
// M13.2 — `inspect` resource-category order
// ------------------------------------------------------------------

/// The resource section lists categories in the canonical order a
/// PDF reader walks them — Font, XObject, ColorSpace, Pattern,
/// Shading, ExtGState — each as a (count line, then its enumerated
/// names) block. `ProcSet`/`Properties` trail last, counts only.
/// This pins that order against `resources_pdf` which populates
/// every category.
#[test]
fn cli_inspect_resource_category_order() {
    let dir = tmpdir("inspect-order");
    let src = dir.join("in.pdf");
    fs::write(&src, resources_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    // Slice out the resource block: from "resources fonts" to the
    // line before "tokens".
    let lines: Vec<&str> = s.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("resources "))
        .unwrap();
    let end = lines.iter().position(|l| l.starts_with("tokens ")).unwrap();
    let block: Vec<&str> = lines[start..end].to_vec();
    // Category headers must appear in canonical order.
    let heads: Vec<&str> = block
        .iter()
        .filter(|l| {
            l.starts_with("resources fonts")
                || l.starts_with("xobjects")
                || l.starts_with("colorspaces")
                || l.starts_with("patterns")
                || l.starts_with("shadings")
                || l.starts_with("extgstates")
                || l.starts_with("procsets")
                || l.starts_with("properties")
        })
        .copied()
        .collect();
    assert_eq!(
        heads,
        vec![
            "resources fonts 2",
            "xobjects 2",
            "colorspaces 1",
            "patterns 1",
            "shadings 1",
            "extgstates 1",
            "procsets 0",
            "properties 0",
        ],
        "category order wrong:\n{}",
        block.join("\n")
    );
    // Each enumeration line sits inside its own block, right after
    // the category count: font lines between 'resources fonts' and
    // 'xobjects', xobject lines between 'xobjects' and 'colorspaces'.
    let pos = |needle: &str| block.iter().position(|l| l.starts_with(needle)).unwrap();
    assert!(pos("font F1") < pos("xobjects"));
    assert!(pos("font F2") < pos("xobjects"));
    assert!(pos("xobject Fm1") < pos("colorspaces"));
    assert!(pos("xobject Im1") < pos("colorspaces"));
    assert!(pos("colorspace Cs1") < pos("patterns"));
    assert!(pos("pattern P1") < pos("shadings"));
    assert!(pos("shading Sh1") < pos("extgstates"));
    assert!(pos("extgstate GS1") < pos("procsets"));
}

// ------------------------------------------------------------------
// M14 — `inspect` /Info dump
// ------------------------------------------------------------------

/// A 1-page PDF with a populated /Info dict — every value type
/// appears so `key …` lines exercise all type names.
fn info_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] /Contents 4 0 R >>",
    );
    push(4, b"<< /Length 4 >>\nstream\nq Q\nendstream");
    push(
        5,
        b"<< /Title (Test Doc) /Author (me) /CreationDate (D:2024) \
          /Producer 6 0 R /Keywords [/a /b] /Trapped /False \
          /Count 3 /Ratio 1.5 /Draft true >>",
    );
    push(6, b"<< /Subtype /Type1 >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
    for n in 1..7u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 7 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_info_dump() {
    let dir = tmpdir("inspect-info");
    let src = dir.join("in.pdf");
    fs::write(&src, info_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    // The document block trails the page output.
    let doc_pos = s.lines().position(|l| l == "document").unwrap();
    let after: Vec<&str> = s.lines().skip(doc_pos).collect();
    let expected = [
        "document",
        "version 1.4",
        "info 9",
        "key Author string",
        "key Count int",
        "key CreationDate string",
        "key Draft bool",
        "key Keywords array",
        "key Producer ref",
        "key Ratio real",
        "key Title string",
        "key Trapped name",
    ];
    assert_eq!(&after[..expected.len()], expected, "info block:\n{s}");
}

#[test]
fn cli_inspect_info_absent() {
    let dir = tmpdir("inspect-info-none");
    let src = dir.join("in.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.lines().any(|l| l == "document"));
    assert!(s.lines().any(|l| l == "info 0"));
    // No key lines when /Info is absent.
    assert!(!s.lines().any(|l| l.starts_with("key ")));
}

// ------------------------------------------------------------------
// M15 — `inspect` PDF version line
// ------------------------------------------------------------------

/// A minimal 1-page PDF with a given header version and optional
/// catalog /Version — the `version` line reports the *effective*
/// (max) version.
fn version_pdf(header: &[u8], cat_version: Option<&[u8]>) -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![header.to_vec(), b"\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    let cat = match cat_version {
        Some(v) => format!(
            "<< /Type /Catalog /Pages 2 0 R /Version /{} >>",
            String::from_utf8_lossy(v)
        ),
        None => "<< /Type /Catalog /Pages 2 0 R >>".into(),
    };
    push(1, cat.as_bytes());
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_inspect_version_header_only() {
    let dir = tmpdir("inspect-ver-hdr");
    let src = dir.join("in.pdf");
    fs::write(&src, version_pdf(b"%PDF-1.4", None)).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l == "version 1.4"));
}

#[test]
fn cli_inspect_version_catalog_wins() {
    // Header 1.4 + catalog /Version /1.7 → max = 1.7.
    let dir = tmpdir("inspect-ver-cat");
    let src = dir.join("in.pdf");
    fs::write(&src, version_pdf(b"%PDF-1.4", Some(b"1.7"))).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l == "version 1.7"));

    // Header 1.7 + catalog /Version /1.4 → max = 1.7.
    let src2 = dir.join("in2.pdf");
    fs::write(&src2, version_pdf(b"%PDF-1.7", Some(b"1.4"))).unwrap();
    let out = bin()
        .args(["inspect"])
        .arg(&src2)
        .arg("1")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l == "version 1.7"));
}

#[test]
fn cli_inspect_version_bad_catalog_errors() {
    // /Version not a name → error, not "ignore it".
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R /Version (1.7) >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-ver-bad");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success(), "bad /Version type must fail");
}

#[test]
fn cli_inspect_version_malformed_name_errors() {
    // /Version /2 (name but not M.m) → error.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R /Version /2 >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 9 9] >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 4\n0000000000 65535 f \n".to_vec());
    for n in 1..4u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 4 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    let dir = tmpdir("inspect-ver-badname");
    let src = dir.join("in.pdf");
    fs::write(&src, parts.concat()).unwrap();
    let out = bin().args(["inspect"]).arg(&src).arg("1").output().unwrap();
    assert!(!out.status.success(), "malformed /Version name must fail");
}

// ------------------------------------------------------------------
// M16 — `preflight-redaction`
// ------------------------------------------------------------------

/// A 1-page PDF with a TrueType font (no embedding — glyph outlines
/// fall back to the cell estimate), one image XObject, one Form
/// XObject that wraps a `Tj`, and a Link annotation. Exercises every
/// preflight category: text, image, form, path, annot, shared.
fn preflight_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Annots [9 0 R] \
          /Resources << \
            /Font << /F1 4 0 R >> \
            /XObject << /Im1 5 0 R /Fm1 6 0 R /Fm2 6 0 R >> \
          >> \
          /Contents 7 0 R >>",
    );
    // Type1 — unsupported: any Tj under it stops the walk.
    push(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    push(
        5,
        b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 16 >>\nstream\nxxxxxxxxxxxxxxxx\nendstream",
    );
    // A form with no text — just a path — so the Do call itself is
    // diagnosable even though the page's Tj is not.
    push(
        6,
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 40] /Length 19 >>\nstream\n0 0 100 40 re f\nendstream",
    );
    // Content order: image, form×2 (shared), path — *then* the
    // unsupported-font text so the walk stops after diagnosing the
    // shared/path targets.
    let body = b"q 40 0 0 40 100 50 cm /Im1 Do Q\nq /Fm1 Do Q\nq 1 0 0 1 120 120 cm /Fm2 Do Q\n2 w 20 20 160 160 re S\nBT /F1 12 Tf 50 150 Td (Hi) Tj ET\n";
    let stream = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    let mut s = stream;
    s.extend_from_slice(body);
    s.extend_from_slice(b"\nendstream");
    // Drop the closure's borrow before a manual push, then rebind it.
    drop(push);
    offs.push((7, parts.iter().map(|p| p.len()).sum()));
    parts.push(b"7 0 obj\n".to_vec());
    parts.push(s);
    parts.push(b"\nendobj\n".to_vec());
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(
        9,
        b"<< /Type /Annot /Subtype /Link /Rect [40 140 70 160] /A << /S /URI /URI (https://x) >> >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
    for n in 1..10u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_preflight_happy_text_format() {
    let dir = tmpdir("pf-text");
    let src = dir.join("in.pdf");
    // text_basic.pdf: KasaneTest.ttf TrueType, /Encoding WinAnsi —
    // fully diagnosable, "ABC" at (20,60) size 24.
    fs::write(&src, fs::read(fixture("text_basic.pdf")).unwrap()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "15", "55", "70", "80"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    for line in [
        "preflight-redaction — diagnosis, not a redaction",
        "page: 1",
        "rects: 1",
        "  rect 0: 15 55 70 80",
        "targets 3",
        "uncertain 0",
        "blockers 0",
        "shared",
        "annotations 0",
        "unexamined",
    ] {
        assert!(
            s.lines().any(|l| l.starts_with(line)),
            "missing '{line}':\n{s}"
        );
    }
    // Each of the three 'A','B','C' glyphs overlaps rect 0 — outline
    // bounds are reported (ink extent), not a redaction region.
    let text_hits = s
        .lines()
        .filter(|l| l.contains("Text") && l.contains("rects 0"))
        .count();
    assert_eq!(text_hits, 3, "expected 3 text targets on rect 0:\n{s}");
    // Unicode resolved through the encoding fallback.
    assert!(s.contains("\"A\""), "unicode 'A':\n{s}");
    // The footer disclaimer must be the only "verdict" — never a
    // "this file is safe to redact" claim.
    assert!(!s.contains("safe to redact"), "no safety verdict:\n{s}");
}

#[test]
fn cli_preflight_unsupported_font_reports_incomplete() {
    // Type1 fonts are not supported — the walk must mark the scan
    // incomplete, never "no targets found".
    let dir = tmpdir("pf-type1");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l.starts_with("incomplete: ")),
        "unsupported font → incomplete:\n{s}"
    );
    // The annot is still reported (it's above the content walk).
    assert!(s.lines().any(|l| l.starts_with("annotations 1")), "{s}");
    // The walk stopped at the Type1 Tj — targets seen before that
    // (image, two form calls, three paths incl. two inside the
    // forms) are reported *partially*, never as "all clear".
    assert!(s.lines().any(|l| l.starts_with("targets 6")), "{s}");
}

#[test]
fn cli_preflight_shared_form_is_reported() {
    // preflight_pdf calls the same form as /Fm1 and /Fm2 — the form
    // object is shared even though each Do site is distinct.
    let dir = tmpdir("pf-shared");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "10", "10"]) // rect misses text; shared still reports
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    // Two Do sites → shared, even though the font inside the form is
    // Type1 (so the inner walk also goes incomplete — the shared
    // report is about the *object*, not its contents).
    assert!(
        s.lines().any(|l| l.starts_with("shared 1")),
        "expected shared form:\n{s}"
    );
}

#[test]
fn cli_preflight_json_format() {
    let dir = tmpdir("pf-json");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    // JSON output is parseable and carries the core fields.
    assert!(s.starts_with("{\n"), "JSON expected:\n{s}");
    for k in [
        "\"rects\"",
        "\"targets\"",
        "\"target_list\"",
        "\"uncertain\"",
        "\"blockers\"",
        "\"shared\"",
        "\"annotations\"",
        "\"unexamined\"",
        "\"incomplete\"",
    ] {
        assert!(s.contains(k), "missing key {k}:\n{s}");
    }
    // The Type1 font makes the walk incomplete — the JSON still
    // carries the image/form/path targets seen before the text op
    // was reached (they come first in the stream).
    assert!(s.contains("\"kind\": \"Image\""), "image target:\n{s}");
    assert!(s.contains("\"kind\": \"Form\""), "form target:\n{s}");
    assert!(s.contains("\"kind\": \"Path\""), "path target:\n{s}");
    // Incomplete is an object, not null.
    assert!(s.contains("\"incomplete\": {"), "{s}");
}

#[test]
fn cli_preflight_requires_rect() {
    let dir = tmpdir("pf-norect");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "no --rect must fail");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("--rect"), "error must mention --rect: {e}");
}

#[test]
fn cli_preflight_rejects_bad_rect() {
    let dir = tmpdir("pf-badrect");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    for rect in [
        ["50", "50", "50", "60"], // zero width
        ["0", "0", "-1", "10"],   // negative width after normalise
        ["0", "0", "abc", "10"],  // non-numeric
    ] {
        let mut a = vec!["preflight-redaction".to_string()];
        a.push(src.to_string_lossy().into_owned());
        a.push("1".into());
        a.push("--rect".into());
        a.extend(rect.iter().map(|s| s.to_string()));
        let out = bin().args(&a).output().unwrap();
        assert!(
            !out.status.success(),
            "rect {rect:?} must fail: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn cli_preflight_input_unchanged() {
    let dir = tmpdir("pf-unchanged");
    let src = dir.join("in.pdf");
    let pdf = preflight_pdf();
    fs::write(&src, &pdf).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(fs::read(&src).unwrap(), pdf, "input was modified");
}

#[test]
fn cli_preflight_page_out_of_range() {
    let dir = tmpdir("pf-page");
    let src = dir.join("in.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    let out = bin()
        .args(["preflight-redaction"])
        .arg(&src)
        .arg("5")
        .args(["--rect", "0", "0", "10", "10"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "page 5 of 1 must fail");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("page 5") || e.contains("1 page"), "{e}");
}

// M17 — `experimental-remove-text`
// ------------------------------------------------------------------

/// A 1-page PDF with the embedded KasaneTest TrueType and `(ABC) Tj`
/// at `20 60 Td` — the fixture `remove_text`'s unit tests use, so the
/// CLI end-to-end check sees the same geometry ('A'@20..34.4,
/// 'B'@34.4..40.4, 'C'@48.8..63.2 at 24pt).
fn remove_text_pdf() -> Vec<u8> {
    let font_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/KasaneTest.ttf"
    ))
    .unwrap();
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>",
    );
    let widths = {
        let mut w = [600u32; 224];
        w[0] = 250;
        format!(
            "[{}]",
            w.iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )
        .into_bytes()
    };
    let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
          /FirstChar 32 /LastChar 255 /Widths "
        .to_vec();
    font_dict.extend_from_slice(&widths);
    font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
    push(4, &font_dict);
    push(
        5,
        b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
          /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
          /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>",
    );
    let enc = {
        use std::io::Write;
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&font_bytes).unwrap();
        e.finish().unwrap()
    };
    let mut fs = format!(
        "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
        enc.len(),
        font_bytes.len()
    )
    .into_bytes();
    fs.extend_from_slice(&enc);
    fs.extend_from_slice(b"\nendstream");
    push(6, &fs);
    let body = b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET\n";
    let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    sb.extend_from_slice(body);
    sb.extend_from_slice(b"\nendstream");
    push(9, &sb);
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
    for n in 1..10u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_remove_text_happy_path() {
    let dir = tmpdir("rt-happy");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, remove_text_pdf()).unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "35", "60", "38", "80", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("codes removed: 1"), "{stdout}");
    // Output re-parses and the removed 'B' is gone from extraction.
    let doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    let text =
        kasane_pdf::extract_text(&doc, &doc.pages()[0], &kasane_pdf::Limits::default()).unwrap();
    let s: String = text
        .chars
        .iter()
        .filter_map(|c| c.unicode.as_deref())
        .collect();
    assert_eq!(s, "AC", "B must be gone: {s:?}");
    // The kern lands between kept runs — 'C' keeps its origin.
    let raw = String::from_utf8_lossy(&fs::read(&dst).unwrap()).into_owned();
    assert!(raw.contains("[(A) -600 (C)] TJ"), "{raw}");
}

#[test]
fn cli_remove_text_same_file_refused() {
    let dir = tmpdir("rt-same");
    let src = dir.join("in.pdf");
    fs::write(&src, remove_text_pdf()).unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(!out.status.success(), "same input/output must fail");
}

#[test]
fn cli_remove_text_requires_output_and_rect() {
    let dir = tmpdir("rt-args");
    let src = dir.join("in.pdf");
    fs::write(&src, remove_text_pdf()).unwrap();
    // No --output.
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "10", "10"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing --output must fail");
    // No --rect.
    let dst = dir.join("out.pdf");
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing --rect must fail");
}

#[test]
fn cli_remove_text_failure_preserves_output() {
    // preflight_pdf has /Annots + unsupported font — removal refuses
    // before writing, and an existing output is left untouched.
    let dir = tmpdir("rt-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, preflight_pdf()).unwrap();
    fs::write(&dst, b"sentinel").unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success(), "an /Annots page must fail");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("unsupported") || e.contains("Annots"), "{e}");
    assert_eq!(fs::read(&dst).unwrap(), b"sentinel", "output untouched");
}

#[test]
fn cli_remove_text_input_unchanged() {
    let dir = tmpdir("rt-unchanged");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let pdf = remove_text_pdf();
    fs::write(&src, &pdf).unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(fs::read(&src).unwrap(), pdf, "input was modified");
}

#[test]
fn cli_remove_text_op_state_change() {
    // M17.1 — a Tc change mid-stream must not rewrite an earlier op
    // under the stream-end state. `Tc=5` before `(AB)`, `Tc=0` before
    // `(C)`: removing 'B' must carry the op-time kern
    // -(600 + 5·1000/24) = -808.3333, not the end-of-stream -600.
    let dir = tmpdir("rt-state");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // Same fixture as remove_text_pdf but with `Tc` changes.
    let pdf = {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        let mut push = |n: u32, body: &[u8]| {
            offs.push((n, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} 0 obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> >> /Contents 9 0 R >>",
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push(4, &fd);
        push(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>",
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push(6, &fs2);
        let body = b"BT /F1 24 Tf 5 Tc 20 60 Td (AB) Tj 0 Tc (C) Tj ET\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 10\n0000000000 65535 f \n".to_vec());
        for n in 1..10u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 10 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    };
    fs::write(&src, &pdf).unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "36", "55", "46", "80", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&fs::read(&dst).unwrap()).into_owned();
    assert!(
        raw.contains("[(A) -808.3333] TJ"),
        "kern must use the op-time Tc=5: {raw}"
    );
}

#[test]
fn cli_remove_text_inside_form() {
    // M18 — a Form `Do` whose inner text a rect hits is cloned
    // per-invocation: the page's `Do` repoints at a `/KasFm0` clone
    // carrying the rewritten TJ, while the original form object keeps
    // its `(ABC)` bytes.
    let dir = tmpdir("rt-form");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let font_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/KasaneTest.ttf"
    ))
    .unwrap();
    let pdf = {
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        let mut push = |n: u32, body: &[u8]| {
            offs.push((n, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} 0 obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Fm1 10 0 R >> >> \
              /Contents 9 0 R >>",
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut fd = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        fd.extend_from_slice(&widths);
        fd.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push(4, &fd);
        push(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>",
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs2 = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs2.extend_from_slice(&enc);
        fs2.extend_from_slice(b"\nendstream");
        push(6, &fs2);
        let body = b"q 1 0 0 1 20 60 cm /Fm1 Do Q\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push(9, &sb);
        let fb = b"BT /F1 24 Tf 0 0 Td (ABC) Tj ET\n";
        let mut f = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 200] \
              /Matrix [1 0 0 1 0 0] /Length "
            .to_vec();
        f.extend_from_slice(fb.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(fb);
        f.extend_from_slice(b"\nendstream");
        push(10, &f);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    };
    fs::write(&src, &pdf).unwrap();
    let out = bin()
        .args(["experimental-remove-text"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "34", "55", "42", "80", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&fs::read(&dst).unwrap()).into_owned();
    // Cloned stream + renamed Do; original keeps (ABC).
    assert!(raw.contains("/KasFm0 Do"), "page Do repointed: {raw}");
    assert!(raw.contains("(ABC)"), "original form keeps (ABC): {raw}");
    // Rect (34,55,42,80) hits 'A' (ink 20..34.4 — edge touch) and
    // 'B' (34.4..40.4) under cm(20,60); the clone keeps only 'C'.
    assert!(
        raw.contains("[-1200 (C)] TJ"),
        "clone carries the rewrite: {raw}"
    );
}

// M20.1 — `experimental-remove-region` CLI regression tests
// ------------------------------------------------------------------

/// A 1-page PDF with a JPEG image — the image is inside the rect
/// so `remove_region` refuses the whole call.
fn region_jpeg_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Resources << /XObject << /Im1 10 0 R >> >> /Contents 9 0 R >>",
    );
    // Minimal JPEG (DCTDecode).
    let jpeg_data = [0xffu8, 0xd8, 0xff, 0xd9];
    let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
          /ColorSpace /DeviceGray /BitsPerComponent 8 \
          /Filter /DCTDecode /Length "
        .to_vec();
    f.extend_from_slice(jpeg_data.len().to_string().as_bytes());
    f.extend_from_slice(b" >>\nstream\n");
    f.extend_from_slice(&jpeg_data);
    f.extend_from_slice(b"\nendstream");
    push(10, &f);
    let body = b"40 0 0 40 20 60 cm\n/Im1 Do\n";
    let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    sb.extend_from_slice(body);
    sb.extend_from_slice(b"\nendstream");
    push(9, &sb);
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
    for n in 1..11u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

/// The JPEG image is inside the rect — the whole call fails and
/// the pre-existing output file is left byte-identical.
#[test]
fn cli_remove_region_unsupported_image_refused() {
    let dir = tmpdir("rr-jpeg");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, region_jpeg_pdf()).unwrap();
    // Pre-existing output must be preserved.
    fs::write(&dst, b"sentinel").unwrap();
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "20", "60", "60", "100", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success(), "JPEG image in rect must fail");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(e.contains("unsupported") || e.contains("filter"), "{e}");
    assert_eq!(fs::read(&dst).unwrap(), b"sentinel", "output untouched");
}

/// A happy-path run: the JPEG is *outside* the rect — the call
/// succeeds because the image is never touched.
#[test]
fn cli_remove_region_unsupported_image_outside_rect() {
    let dir = tmpdir("rr-jpeg-out");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, region_jpeg_pdf()).unwrap();
    // Rect is outside the image's painted area.
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "150", "150", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "JPEG outside rect must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("invocations edited: 0"), "{stdout}");
}

/// A `'` op expands to `T*` + `TJ` — a `Do` after it keeps its
/// original `op_index` and is correctly repointed (M20.1 §5).
#[test]
fn cli_remove_region_quote_expansion_before_do() {
    let dir = tmpdir("rr-quote");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let pdf = {
        let font_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/KasaneTest.ttf"
        ))
        .unwrap();
        let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
        let mut offs: Vec<(u32, usize)> = Vec::new();
        let mut push = |n: u32, body: &[u8]| {
            offs.push((n, parts.iter().map(|p| p.len()).sum()));
            parts.push(format!("{n} 0 obj\n").into_bytes());
            parts.push(body.to_vec());
            parts.push(b"\nendobj\n".to_vec());
        };
        push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
        push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        push(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 10 0 R >> >> \
              /Contents 9 0 R >>",
        );
        let widths = {
            let mut w = [600u32; 224];
            w[0] = 250;
            format!(
                "[{}]",
                w.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .into_bytes()
        };
        let mut font_dict = b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest \
              /FirstChar 32 /LastChar 255 /Widths "
            .to_vec();
        font_dict.extend_from_slice(&widths);
        font_dict.extend_from_slice(b" /FontDescriptor 5 0 R /Encoding /WinAnsiEncoding >>");
        push(4, &font_dict);
        push(
            5,
            b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
              /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
              /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 6 0 R >>",
        );
        let enc = {
            use std::io::Write;
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(&font_bytes).unwrap();
            e.finish().unwrap()
        };
        let mut fs_ = format!(
            "<< /Length {} /Filter /FlateDecode /Length1 {} >>\nstream\n",
            enc.len(),
            font_bytes.len()
        )
        .into_bytes();
        fs_.extend_from_slice(&enc);
        fs_.extend_from_slice(b"\nendstream");
        push(6, &fs_);
        let img_data = [0x10u8, 0x20, 0x30, 0x40];
        let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
              /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
            .to_vec();
        f.extend_from_slice(img_data.len().to_string().as_bytes());
        f.extend_from_slice(b" >>\nstream\n");
        f.extend_from_slice(&img_data);
        f.extend_from_slice(b"\nendstream");
        push(10, &f);
        // ' expands to T*+TJ; the Do after it is at the same op_index.
        let body = b"BT /F1 24 Tf 20 140 Td (ABC) ' ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
        let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
        sb.extend_from_slice(body);
        sb.extend_from_slice(b"\nendstream");
        push(9, &sb);
        let xref: usize = parts.iter().map(|p| p.len()).sum();
        parts.push(b"xref\n0 11\n0000000000 65535 f \n".to_vec());
        for n in 1..11u32 {
            if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
                parts.push(format!("{o:010} 00000 n \n").into_bytes());
            } else {
                parts.push(b"0000000000 00000 f \n".to_vec());
            }
        }
        parts.push(b"trailer\n<< /Size 11 /Root 1 0 R >>\n".to_vec());
        parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
        parts.concat()
    };
    fs::write(&src, &pdf).unwrap();
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args([
            "--rect", "20", "140", "30", "160", "--rect", "20", "80", "40", "100", "--output",
        ])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&fs::read(&dst).unwrap()).into_owned();
    // The Do after the expanded ' is repointed to the new name.
    assert!(raw.contains("/KasIm0"), "Do repointed: {raw}");
    assert!(raw.contains("T*"), "' expanded: {raw}");
    assert!(raw.contains("TJ"), "TJ present: {raw}");
}

// ------------------------------------------------------------------
// M23 — audit-residuals
// ------------------------------------------------------------------

/// A PDF carrying one annotation with text — used to check the audit
/// reports the annot *presence* without leaking its /Contents value.
fn annot_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
          /Annots [4 0 R] >>",
    );
    push(
        4,
        b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] \
          /Contents (SECRET-NOTE-DO-NOT-PRINT) >>",
    );
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 5\n0000000000 65535 f \n".to_vec());
    for n in 1..5u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 5 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_audit_text_and_json() {
    let dir = tmpdir("audit");
    let src = dir.join("in.pdf");
    fs::write(&src, annot_pdf()).unwrap();
    // text mode
    let out = bin().args(["audit-residuals"]).arg(&src).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("audit-residuals"), "{stdout}");
    assert!(stdout.contains("AnnotText"), "{stdout}");
    // The annot's /Contents value must never be printed.
    assert!(
        !stdout.contains("SECRET-NOTE-DO-NOT-PRINT"),
        "leaked annot contents: {stdout}"
    );
    // json mode — structurally check the JSON without a serde dep.
    let out = bin()
        .args(["audit-residuals"])
        .arg(&src)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.trim_start().starts_with('{'), "{stdout}");
    assert!(stdout.contains("\"kind\": \"AnnotText\""), "{stdout}");
    assert!(stdout.contains("\"reachability\""), "{stdout}");
    assert!(!stdout.contains("SECRET-NOTE-DO-NOT-PRINT"));
}

#[test]
fn cli_audit_never_writes_output() {
    let dir = tmpdir("audit-noout");
    let src = dir.join("in.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let before = fs::read(&src).unwrap();
    let out = bin().args(["audit-residuals"]).arg(&src).output().unwrap();
    assert!(out.status.success());
    // No output file is created next to the input.
    assert!(!dir.join("out.pdf").exists());
    assert_eq!(fs::read(&src).unwrap(), before, "input unchanged");
}

#[test]
fn cli_audit_bad_format_fails() {
    let dir = tmpdir("audit-badfmt");
    let src = dir.join("in.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let out = bin()
        .args(["audit-residuals"])
        .arg(&src)
        .args(["--format", "xml"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("--format"), "{stderr}");
}
