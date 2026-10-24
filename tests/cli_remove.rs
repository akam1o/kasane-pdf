//! CLI `experimental-remove-text` / `experimental-remove-region`
//! tests — scoped text removal, image-region editing, atomicity,
//! and refusal protections.

mod common;
use common::{bin, blocker_page_pdf, tmpdir};
use std::fs;

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
        .args(["--rect", "36.2", "60", "39.2", "80", "--output"])
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
    // blocker_page_pdf has /Annots + unsupported font — removal refuses
    // before writing, and an existing output is left untouched.
    let dir = tmpdir("rt-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, blocker_page_pdf()).unwrap();
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
    // A Tc change mid-stream must not rewrite an earlier op
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
    // A Form `Do` whose inner text a rect hits is cloned
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

// `experimental-remove-region` CLI regression tests
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

/// The JPEG image is inside the rect but its bytes are truncated
/// junk — the whole call fails and the pre-existing output file is
/// left byte-identical. (DCTDecode itself is in scope.)
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
    assert!(!out.status.success(), "corrupt JPEG in rect must fail");
    let e = String::from_utf8_lossy(&out.stderr);
    assert!(
        e.contains("parse") || e.contains("JPEG") || e.contains("DCT"),
        "{e}"
    );
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
/// original `op_index` and is correctly repointed.
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
