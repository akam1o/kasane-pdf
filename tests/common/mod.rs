//! Shared helpers for the CLI integration tests.
//!
//! Each `tests/cli_*.rs` file does `mod common;` to reach these.
//! They run the built `kasane-pdf` binary as a child process (Cargo
//! exports `CARGO_BIN_EXE_<name>` at compile time). Every test works
//! in its own temp directory so parallel runs never collide.

// Each `tests/cli_*.rs` binary pulls in only the helpers it names;
// the rest would warn as unused. `dead_code` is allowed module-wide
// because the set is deliberately shared, not per-file.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The compiled `kasane-pdf` binary under test.
pub fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_kasane-pdf"))
}

/// A fresh per-test temp directory under the system temp dir.
pub fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kasane-cli-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Absolute path to a hand-crafted fixture under `tests/fixtures/`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Serialise numbered object bodies into a classic-xref PDF (object
/// `1` must be the catalog). Mirrors `make_fixtures.py::build_pdf`
/// so render tests can build small deterministic files inline.
pub fn build_pdf(objects: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets: Vec<(u32, usize)> = Vec::new();
    for (n, body) in objects {
        offsets.push((*n, out.len()));
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.iter().find(|(on, _)| *on == n) {
            Some((_, off)) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\n").as_bytes());
    out.extend_from_slice(format!("startxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

/// A stream object body: `dict_entries` (without `<< >>`) plus
/// `/Length`, then the raw data.
pub fn stream_obj(dict_entries: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict_entries} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A minimal 1-page PDF the writer accepts — kept inline so the test
/// does not depend on a fixture that may itself evolve.
pub fn minimal_pdf() -> Vec<u8> {
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

/// A page carrying the blockers a preflight/diagnosis or removal
/// must report: `/Annots`, a `/Type1` (unsupported) font, an image,
/// and a Form invoked twice under two names. Used by the
/// `preflight-redaction` tests and by removal tests that need a
/// guaranteed-refusal page.
pub fn blocker_page_pdf() -> Vec<u8> {
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
    // Type1 backed by an eexec `/FontFile` — a program format we do
    // not parse, so `Tf` fails to load and the walk goes incomplete.
    // (A bare non-embedded Type1 now loads metrics-only for text
    // extraction — that would *not* stop the walk, only mark glyphs
    // uncertain.)
    push(
        4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding \
          /FontDescriptor 10 0 R >>",
    );
    push(
        10,
        b"<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 \
          /FontBBox [0 0 500 700] /Ascent 700 /Descent -200 /CapHeight 660 \
          /StemV 80 /FontFile 11 0 R >>",
    );
    push(11, b"<< /Length 8 >>\nstream\neexec!!!\nendstream");
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
    parts.push(b"xref\n0 12\n0000000000 65535 f \n".to_vec());
    for n in 1..12u32 {
        if let Some((_, o)) = offs.iter().find(|(on, _)| *on == n) {
            parts.push(format!("{o:010} 00000 n \n").into_bytes());
        } else {
            parts.push(b"0000000000 00000 f \n".to_vec());
        }
    }
    parts.push(b"trailer\n<< /Size 12 /Root 1 0 R >>\n".to_vec());
    parts.push(format!("startxref\n{xref}\n%%EOF\n").into_bytes());
    parts.concat()
}
