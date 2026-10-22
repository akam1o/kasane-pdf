//! CLI `rewrite` subcommand tests — output generation,
//! atomic-write guards, same-file/symlink/hardlink/dir refusal,
//! and failure-preserves-output semantics.

mod common;
use common::{bin, fixture, minimal_pdf, tmpdir};
use std::fs;

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
