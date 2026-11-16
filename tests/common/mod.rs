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
    let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    // Indexed by object number (first definition wins): a linear search
    // per xref entry made 100k-object tests take a minute in debug.
    let mut offsets: Vec<Option<usize>> = vec![None; size as usize];
    for (n, body) in objects {
        offsets[*n as usize].get_or_insert(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets[n as usize] {
            Some(off) => out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes()),
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

/// A 40 × 20 baseline JPEG (Pillow, quality 95, no subsampling): the
/// left half red, the right half blue.
pub const JPEG_40X20: &[&str] = &[
    "ffd8ffe000104a46494600010100000100010000ffdb0043000201010101010201010102020202020403020202020504",
    "040304060506060605060606070908060709070606080b08090a0a0a0a0a06080b0c0b0a0c090a0a0affdb0043010202",
    "02020202050303050a0706070a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
    "0a0a0a0a0a0a0a0a0a0a0a0a0a0affc00011080014002803011100021101031101ffc4001f0000010501010101010100",
    "000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221",
    "31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
    "434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899",
    "9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
    "f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100",
    "020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015",
    "6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869",
    "6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
    "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f8",
    "bebf94cff7f02803c87f6aaff980ff00dbd7fed1aff553f665ff00cd57ff00723ffbb87f9aff00b433fe699ffb9dff00",
    "dd43c86bfd543fcd70a00faf2bfe55cffa500a00f21fdaabfe603ff6f5ff00b46bfd54fd997ff355ff00dc8ffeee1fe6",
    "bfed0cff009a67fee77ff750f21aff00550ff35c2803ebcaff009573fe9402803c87f6aaff00980ffdbd7fed1aff0055",
    "3f665ffcd57ff723ff00bb87f9affb433fe699ff00b9dffdd43c86bfd543fcd70a00ffd9",
];

pub fn unhex(parts: &[&str]) -> Vec<u8> {
    let h: String = parts.concat();
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

/// A JBIG2 stream (embedded organisation) of an 8 × 2 page: each row
/// four white pixels, then four black (MMR generic region).
pub fn jbig2_8x2() -> Vec<u8> {
    let be = |v: u32| v.to_be_bytes();
    let seg = |num: u32, kind: u8, data: &[u8]| {
        let mut s = be(num).to_vec();
        s.extend_from_slice(&[kind, 0, 1]);
        s.extend_from_slice(&be(data.len() as u32));
        s.extend_from_slice(data);
        s
    };
    let mut info = [be(8), be(2), be(0), be(0)].concat();
    info.extend_from_slice(&[0, 0, 0]);
    let mut region = [be(8), be(2), be(0), be(0)].concat();
    region.extend_from_slice(&[0, 1, 0x36, 0xF0, 0x01, 0x00, 0x10]);
    [seg(0, 48, &info), seg(1, 38, &region)].concat()
}
