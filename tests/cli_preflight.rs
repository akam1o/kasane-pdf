//! CLI `preflight-redaction` subcommand tests — the read-only
//! diagnosis output, JSON format, and rect validation.

mod common;
use common::{bin, blocker_page_pdf, fixture, tmpdir};
use std::fs;

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
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    // blocker_page_pdf calls the same form as /Fm1 and /Fm2 — the form
    // object is shared even though each Do site is distinct.
    let dir = tmpdir("pf-shared");
    let src = dir.join("in.pdf");
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    let pdf = blocker_page_pdf();
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
    fs::write(&src, blocker_page_pdf()).unwrap();
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
