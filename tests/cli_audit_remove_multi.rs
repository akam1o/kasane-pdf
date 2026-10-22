//! CLI `audit-residuals`, `experimental-remove-regions`,
//! `remove-document-metadata`, and combined metadata-strip
//! tests — multi-page and document-level edits.

mod common;
use common::{bin, minimal_pdf, tmpdir};
use std::fs;

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

/// A PDF whose `/EmbeddedFiles` name-tree root is a dangling ref —
/// the audit cannot evaluate that tree, so it must report an
/// `incomplete` scope naming the tree and the failure kind, never
/// claim "no attachments" and never let a good sibling tree drop.
fn broken_name_tree_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    // /EmbeddedFiles 99 0 R is dangling; /JavaScript 5 0 R is fine.
    push(
        1,
        b"<< /Type /Catalog /Pages 2 0 R \
          /Names << /EmbeddedFiles 99 0 R \
                   /JavaScript 5 0 R >> >>",
    );
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] \
          /Annots [4 0 R] >>",
    );
    push(
        4,
        b"<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] \
          /Contents (SECRET-BROKEN-TREE-DO-NOT-PRINT) >>",
    );
    push(5, b"<< /Names [(app.js) 6 0 R] >>");
    push(6, b"<< /Type /Action /S /JavaScript /JS (x) >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
    for n in 1..7u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        }
    }
    parts.push(b"trailer\n<< /Size 7 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_audit_incomplete_text_names_scope_and_reason() {
    let dir = tmpdir("audit-inc");
    let src = dir.join("in.pdf");
    let before = broken_name_tree_pdf();
    fs::write(&src, &before).unwrap();
    let out = bin().args(["audit-residuals"]).arg(&src).output().unwrap();
    // Completed audit → exit 0 (not a safety verdict, just ran).
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // The failing tree is named, with its resolution failure —
    // never flattened to a bare "incomplete" word.
    assert!(
        stdout.contains("name tree /EmbeddedFiles"),
        "scope missing: {stdout}"
    );
    assert!(
        stdout.contains("does not resolve"),
        "reason missing: {stdout}"
    );
    // The good sibling tree's finding still made it through.
    assert!(stdout.contains("JavaScript"), "sibling lost: {stdout}");
    // Input bytes untouched; no stray PDF written next to it.
    assert_eq!(fs::read(&src).unwrap(), before, "input changed");
    assert!(!dir.join("out.pdf").exists());
    // The annot's /Contents text never reaches stdout.
    assert!(
        !stdout.contains("SECRET-BROKEN-TREE-DO-NOT-PRINT"),
        "leaked body: {stdout}"
    );
    // "walk incomplete" wording is present (the reachability walk is
    // fine — the *tree* scope is incomplete; the two are distinct).
    assert!(
        stdout.contains("incomplete"),
        "no incomplete section: {stdout}"
    );
}

#[test]
fn cli_audit_incomplete_json_structure() {
    let dir = tmpdir("audit-inc-json");
    let src = dir.join("in.pdf");
    fs::write(&src, broken_name_tree_pdf()).unwrap();
    let out = bin()
        .args(["audit-residuals"])
        .arg(&src)
        .args(["--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.trim_start().starts_with('{'), "{stdout}");
    // Structural check: the incomplete array carries the failing
    // scope, reason and a limit flag — not just a bare keyword.
    assert!(
        stdout.contains("\"scope\": \"name tree /EmbeddedFiles\""),
        "scope missing: {stdout}"
    );
    assert!(
        stdout.contains("\"reason\": \"root does not resolve"),
        "reason missing: {stdout}"
    );
    // A missing ref is *not* a limit failure.
    assert!(
        stdout.contains("\"limit\": false"),
        "limit misclassified: {stdout}"
    );
    // The JavaScript sibling finding survives in JSON too.
    assert!(stdout.contains("\"kind\": \"JavaScript\""), "{stdout}");
    // Body text still never reaches the JSON output.
    assert!(!stdout.contains("SECRET-BROKEN-TREE-DO-NOT-PRINT"));
}

/// No name trees at all → no name-tree incomplete in CLI output —
/// absence is not incompleteness.
#[test]
fn cli_audit_no_name_tree_no_incomplete() {
    let dir = tmpdir("audit-no-tree");
    let src = dir.join("in.pdf");
    fs::write(&src, minimal_pdf()).unwrap();
    let out = bin().args(["audit-residuals"]).arg(&src).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // The catalog has no /Names key at all, so no name-tree scope
    // may appear under `incomplete`.
    assert!(
        !stdout.contains("name tree"),
        "spurious name-tree incomplete: {stdout}"
    );
    assert!(
        !stdout.contains("EmbeddedFiles"),
        "spurious EmbeddedFiles: {stdout}"
    );
    // Exit 0 means the audit ran — it is not a "safe" verdict.
}

// ------------------------------------------------------------------
// M24 — experimental-remove-regions
// ------------------------------------------------------------------

/// A 2-page fixture: page 1 has text at (20,140), page 2 shares the
/// same image at (100,100). Used to verify multi-page CLI removal.
fn two_page_text_image_pdf() -> Vec<u8> {
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
    push(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Resources << /Font << /F1 5 0 R >> \
          /XObject << /Im1 10 0 R >> >> \
          /Contents 9 0 R >>",
    );
    push(
        4,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
          /Resources << /Font << /F1 5 0 R >> \
          /XObject << /Im1 10 0 R >> >> \
          /Contents 12 0 R >>",
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
    font_dict.extend_from_slice(b" /FontDescriptor 6 0 R /Encoding /WinAnsiEncoding >>");
    push(5, &font_dict);
    push(
        6,
        b"<< /Type /FontDescriptor /FontName /KasaneTest /Flags 32 \
          /FontBBox [-100 -300 1200 1000] /ItalicAngle 0 /Ascent 800 \
          /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 7 0 R >>",
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
    push(7, &fs);
    let img_data = [0x10u8, 0x20, 0x30, 0x40];
    let mut f = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 \
          /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
        .to_vec();
    f.extend_from_slice(img_data.len().to_string().as_bytes());
    f.extend_from_slice(b" >>\nstream\n");
    f.extend_from_slice(&img_data);
    f.extend_from_slice(b"\nendstream");
    push(10, &f);
    let body1 = b"BT /F1 24 Tf 20 140 Td (ABC) Tj ET\n40 0 0 40 20 60 cm\n/Im1 Do\n";
    let mut sb = format!("<< /Length {} >>\nstream\n", body1.len()).into_bytes();
    sb.extend_from_slice(body1);
    sb.extend_from_slice(b"\nendstream");
    push(9, &sb);
    let body2 = b"BT /F1 24 Tf 20 140 Td (XYZ) Tj ET\n40 0 0 40 100 100 cm\n/Im1 Do\n";
    let mut sb2 = format!("<< /Length {} >>\nstream\n", body2.len()).into_bytes();
    sb2.extend_from_slice(body2);
    sb2.extend_from_slice(b"\nendstream");
    push(12, &sb2);
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 13\n0000000000 65535 f \n".to_vec());
    for n in 1..13u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 13 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_remove_regions_success() {
    let dir = tmpdir("m24-ok");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, two_page_text_image_pdf()).unwrap();
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args([
            "--page", "1", "--rect", "20", "140", "30", "160", "--page", "2", "--rect", "100",
            "100", "140", "140", "--output",
        ])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // Page numbers and counts only — no text/image bytes.
    assert!(stdout.contains("codes removed"), "{stdout}");
    assert!(stdout.contains("page"), "{stdout}");
    assert!(!stdout.contains("(ABC)"), "leaked text: {stdout}");
    assert!(!stdout.contains("(XYZ)"), "leaked text: {stdout}");
    // Output written and parses.
    assert!(dst.exists());
    let out_bytes = fs::read(&dst).unwrap();
    let _ = kasane_pdf::Document::from_bytes(&out_bytes).unwrap();
    // Input untouched.
    assert_eq!(fs::read(&src).unwrap(), two_page_text_image_pdf());
}

#[test]
fn cli_remove_regions_validation() {
    let dir = tmpdir("m24-val");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, two_page_text_image_pdf()).unwrap();
    // --rect before --page is refused.
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args(["--rect", "0", "0", "10", "10", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("--rect must follow a --page"), "{stderr}");
    // --page with no rect is refused.
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args(["--page", "1", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("has no --rect"), "{stderr}");
    // Failure leaves no output.
    assert!(!dst.exists());
}

#[test]
fn cli_remove_regions_failure_preserves_output() {
    let dir = tmpdir("m24-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, two_page_text_image_pdf()).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    // Page 2 is refused (annot fixture would be needed — use a page
    // that does not exist to force a Structure error).
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args([
            "--page", "1", "--rect", "20", "140", "30", "160", "--page", "9", "--rect", "0", "0",
            "10", "10", "--output",
        ])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    // Existing output is untouched.
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
}

/// M24.1 — `--page 0` is refused at argument validation; the
/// existing output and the input bytes are preserved and no temp
/// file remains.
#[test]
fn cli_remove_regions_page_zero_refused() {
    let dir = tmpdir("m24-1-rects");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, two_page_text_image_pdf()).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    // --page 0 is invalid (CLI pages are 1-based) — refused before
    // any output is written.
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args(["--page", "0", "--rect", "0", "0", "10", "10", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("invalid page number"), "{stderr}");
    // Existing output and input are preserved.
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    assert_eq!(fs::read(&src).unwrap(), two_page_text_image_pdf());
    // No temp file remains.
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
    // stdout/stderr carry no document content.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("(ABC)"), "leaked text: {stdout}");
    assert!(!stdout.contains("(XYZ)"), "leaked text: {stdout}");
    assert!(!stderr.contains("(ABC)"), "leaked text: {stderr}");
}

/// M24.2 — `max_diag_rects` exceeded via the real CLI: the merged
/// per-page rect count crosses the default limit (256) and the
/// call fails as a `LimitExceeded`, leaving the sentinel output
/// and the input untouched.
#[test]
fn cli_remove_regions_rect_count_over_limit() {
    let dir = tmpdir("m24-2-rects");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, two_page_text_image_pdf()).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    // Limits::default().max_diag_rects is 256 — pass 257 rects for
    // the same page. The rects may be identical; the cap counts
    // input entries, not distinct regions.
    let over = kasane_pdf::Limits::default().max_diag_rects + 1;
    let mut args: Vec<String> = vec!["--page".into(), "1".into()];
    for _ in 0..over {
        args.extend(
            ["--rect", "0", "0", "10", "10"]
                .iter()
                .map(|s| s.to_string()),
        );
    }
    args.push("--output".into());
    args.push(dst.to_string_lossy().into_owned());
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args(&args)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // The refusal is the rect-count limit, not an argument-parse
    // error or a per-page eligibility refusal.
    assert!(
        stderr.contains("limit exceeded") && stderr.contains("max_diag_rects"),
        "expected a max_diag_rects refusal, got: {stderr}"
    );
    // Not an argument error and not a page-level unsupported refusal.
    assert!(!stderr.contains("usage:"), "{stderr}");
    assert!(!stderr.contains("unsupported"), "{stderr}");
    // Existing output and input are preserved.
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    assert_eq!(fs::read(&src).unwrap(), two_page_text_image_pdf());
    // No temp file remains.
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
    // stdout/stderr carry no document content.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("(ABC)"), "leaked text: {stdout}");
    assert!(!stdout.contains("(XYZ)"), "leaked text: {stdout}");
    assert!(!stderr.contains("(ABC)"), "leaked text: {stderr}");
}

// ------------------------------------------------------------------
// M25 — remove-document-metadata
// ------------------------------------------------------------------

/// A 1-page fixture with trailer /Info and catalog /Metadata.
fn doc_with_metadata_pdf() -> Vec<u8> {
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(1, b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>");
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>",
    );
    push(4, b"<< /Length 0 >>\nstream\n\nendstream");
    push(5, b"<< /Title (Secret Title) /Author (Secret Author) >>");
    let xmp = b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>";
    let mut m = b"<< /Type /Metadata /Subtype /XML /Length ".to_vec();
    m.extend_from_slice(xmp.len().to_string().as_bytes());
    m.extend_from_slice(b" >>\nstream\n");
    m.extend_from_slice(xmp);
    m.extend_from_slice(b"\nendstream");
    push(6, &m);
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 7\n0000000000 65535 f \n".to_vec());
    for n in 1..7u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 7 /Root 1 0 R /Info 5 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}

#[test]
fn cli_remove_document_metadata_success() {
    let dir = tmpdir("m25-ok");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, doc_with_metadata_pdf()).unwrap();
    let out = bin()
        .args(["remove-document-metadata"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // Reports which entries were removed — no metadata values.
    assert!(stdout.contains("info entry removed: true"), "{stdout}");
    assert!(
        stdout.contains("catalog /Metadata entry removed: true"),
        "{stdout}"
    );
    assert!(!stdout.contains("Secret Title"), "leaked title: {stdout}");
    assert!(!stdout.contains("SECRET-XMP"), "leaked xmp: {stdout}");
    assert!(!stdout.contains("safe"), "{stdout}");
    assert!(!stdout.contains("sanitiz"), "{stdout}");
    // Output parses and the entries are gone.
    let out_doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert!(!out_doc.trailer().contains_key(b"Info".as_slice()));
    assert!(!out_doc.catalog().contains_key(b"Metadata".as_slice()));
    // Input unchanged.
    assert_eq!(fs::read(&src).unwrap(), doc_with_metadata_pdf());
}

#[test]
fn cli_remove_document_metadata_same_file() {
    let dir = tmpdir("m25-same");
    let src = dir.join("in.pdf");
    fs::write(&src, doc_with_metadata_pdf()).unwrap();
    let out = bin()
        .args(["remove-document-metadata"])
        .arg(&src)
        .args(["--output"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("same file"), "{stderr}");
    assert_eq!(fs::read(&src).unwrap(), doc_with_metadata_pdf());
}

#[test]
fn cli_remove_document_metadata_failure_preserves_output() {
    let dir = tmpdir("m25-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // A signed document — refused by the inherited rewrite checks.
    let mut parts: Vec<Vec<u8>> = vec![b"%PDF-1.4\n".to_vec()];
    let mut offs: Vec<(u32, usize)> = Vec::new();
    let mut push = |n: u32, body: &[u8]| {
        offs.push((n, parts.iter().map(|p| p.len()).sum()));
        parts.push(format!("{n} 0 obj\n").into_bytes());
        parts.push(body.to_vec());
        parts.push(b"\nendobj\n".to_vec());
    };
    push(
        1,
        b"<< /Type /Catalog /Pages 2 0 R /Perms << /DocMDP 5 0 R >> >>",
    );
    push(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    push(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>",
    );
    push(4, b"<< /Length 0 >>\nstream\n\nendstream");
    push(5, b"<< /Type /Sig >>");
    let xref: usize = parts.iter().map(|p| p.len()).sum();
    parts.push(b"xref\n0 6\n0000000000 65535 f \n".to_vec());
    for n in 1..6u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 6 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    fs::write(&src, parts.concat()).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    let out = bin()
        .args(["remove-document-metadata"])
        .arg(&src)
        .args(["--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("signed"), "{stderr}");
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    // No temp file remains.
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
}

// ------------------------------------------------------------------
// M26 — experimental-remove-regions --remove-document-metadata
// ------------------------------------------------------------------

/// Combined region edit + metadata entry removal via one CLI call.
#[test]
fn cli_remove_regions_with_metadata_strip() {
    let dir = tmpdir("m26-combined");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, doc_with_metadata_pdf()).unwrap();
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args([
            "--page",
            "1",
            "--rect",
            "0",
            "0",
            "10",
            "10",
            "--remove-document-metadata",
            "--output",
        ])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("info entry removed: true"), "{stdout}");
    assert!(
        stdout.contains("catalog /Metadata entry removed: true"),
        "{stdout}"
    );
    assert!(!stdout.contains("Secret Title"), "{stdout}");
    assert!(!stdout.contains("SECRET-XMP"), "{stdout}");
    // Re-parse: both entries gone.
    let out_doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert!(!out_doc.trailer().contains_key(b"Info".as_slice()));
    assert!(!out_doc.catalog().contains_key(b"Metadata".as_slice()));
}

/// Without the flag the metadata entries are kept — opt-in only.
#[test]
fn cli_remove_regions_without_flag_keeps_metadata() {
    let dir = tmpdir("m26-noflag");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, doc_with_metadata_pdf()).unwrap();
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args(["--page", "1", "--rect", "0", "0", "10", "10", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(out.status.success());
    let out_doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    assert!(out_doc.trailer().contains_key(b"Info".as_slice()));
    assert!(out_doc.catalog().contains_key(b"Metadata".as_slice()));
}

/// A failing combined call leaves the sentinel output and input
/// untouched and leaves no temp file.
#[test]
fn cli_remove_regions_metadata_failure_preserves_output() {
    let dir = tmpdir("m26-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, doc_with_metadata_pdf()).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    // Page 9 does not exist — the combined call fails before write.
    let out = bin()
        .args(["experimental-remove-regions"])
        .arg(&src)
        .args([
            "--page",
            "1",
            "--rect",
            "0",
            "0",
            "10",
            "10",
            "--page",
            "9",
            "--rect",
            "0",
            "0",
            "10",
            "10",
            "--remove-document-metadata",
            "--output",
        ])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    assert_eq!(fs::read(&src).unwrap(), doc_with_metadata_pdf());
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("Secret Title"), "{stdout}");
}

// ------------------------------------------------------------------
