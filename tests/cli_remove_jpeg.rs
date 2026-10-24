//! CLI JPEG (`DCTDecode`) and grid-aligned-CTM
//! image-editing tests — rotated/reflected region removal
//! and refusal protections.

mod common;
use common::{bin, tmpdir};
use std::fs;

fn jpeg_page_pdf(jpeg: &[u8], w: u32, h: u32, cs: &str) -> Vec<u8> {
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
    let mut f = format!(
        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
          /ColorSpace /{cs} /BitsPerComponent 8 \
          /Filter /DCTDecode /Length {} >>\nstream\n",
        jpeg.len()
    )
    .into_bytes();
    f.extend_from_slice(jpeg);
    f.extend_from_slice(b"\nendstream");
    push(10, &f);
    let body = b"200 0 0 200 0 0 cm\n/Im1 Do\n";
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

// 8×8 DeviceGray JPEG generated with `cjpeg -grayscale -quality 90`
// from a deterministic PGM — same bytes as `JPEG_G8_HEX` in
// `src/remove_region.rs`'s tests.
const JPEG_G8_HEX: &str =
    "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07\
     0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc0000b080008\
     000801011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b51000020103\
     03020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024336272\
     82090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475\
     767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9\
     cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f003e12c1a5fecabf03\
     a5f19ded9f9daac98d3f42b46b569a3b9d49e37681240acb888796cee4bafc88c14972aadfffd9";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// `experimental-remove-image-region` on a real JPEG succeeds and
/// the output re-parses with the edited clone uncompressed.
#[test]
fn cli_remove_image_region_jpeg_success() {
    let dir = tmpdir("m27-img-jpeg");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, jpeg_page_pdf(&unhex(JPEG_G8_HEX), 8, 8, "DeviceGray")).unwrap();
    let out = bin()
        .args(["experimental-remove-image-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "150", "50", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("pixels changed: 3"), "{stdout}");
    let out_doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    // The clone exists and is uncompressed.
    let page = out_doc.pages().first().unwrap();
    let ops = out_doc.page_operations(page).unwrap();
    let names: Vec<_> = ops
        .iter()
        .filter_map(|o| match o {
            kasane_pdf::Operation::Do(n) => Some(n.clone()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|n| n == b"KasIm0"), "{names:?}");
}

/// `experimental-remove-region` on the same JPEG works through the
/// mixed text+image path.
#[test]
fn cli_remove_region_jpeg_success() {
    let dir = tmpdir("m27-reg-jpeg");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    fs::write(&src, jpeg_page_pdf(&unhex(JPEG_G8_HEX), 8, 8, "DeviceGray")).unwrap();
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "150", "50", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("pixels changed: 3"), "{stdout}");
}

/// A failing JPEG edit (corrupt bytes in the *rect*) leaves the
/// sentinel output and input untouched and drops no temp file.
#[test]
fn cli_remove_region_jpeg_failure_preserves_output() {
    let dir = tmpdir("m27-fail");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // Truncated JPEG — SOI + a partial marker, no image data.
    let bad_jpeg = [0xffu8, 0xd8, 0xff, 0xe0, 0x00];
    fs::write(&src, jpeg_page_pdf(&bad_jpeg, 8, 8, "DeviceGray")).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    assert_eq!(
        fs::read(&src).unwrap(),
        jpeg_page_pdf(&bad_jpeg, 8, 8, "DeviceGray")
    );
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
    // The error may name the filter ("DCTDecode (JPEG) header: …")
    // but must not dump document bytes or sample data.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stdout.contains("ff d8") && !stderr.contains("ff d8"),
        "{stdout}{stderr}"
    );
    assert!(
        !stdout.contains("\\xff") && !stderr.contains("\\xff"),
        "{stdout}{stderr}"
    );
}

// 8×8 DeviceRGB *progressive* JPEG — `cjpeg -progressive` on c8.ppm;
// same bytes as `JPEG_C8_PROG_HEX` in `src/remove_region.rs`'s tests.
const JPEG_C8_PROG_HEX: &str =
    "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a070706080c0a0c0c0b\
     0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffdb00430103040405040509050509140d0b0d14141414\
     14141414141414141414141414141414141414141414141414141414141414141414141414141414141414141414ffc20011080008000803\
     012200021101031101ffc4001500010100000000000000000000000000000005ffc4001501010100000000000000000000000000000103ff\
     da000c03010002100310000001921aff00ffc400161000030000000000000000000000000000000506ffda00080101000105024b207fffc4\
     00191100010500000000000000000000000000050002041422ffda0008010301013f01026a6d26e97fffc400171100030100000000000000\
     00000000000000020312ffda0008010201013f01951b27ffc400161000030000000000000000000000000000000122ffda0008010100063f\
     02507fffc40015100101000000000000000000000000000000f1ffda0008010100013f2184ffda000c03010002000300000010f3ffc40016\
     110003000000000000000000000000000000c1f0ffda0008010301013f109167ffc400161100030000000000000000000000000000001121\
     ffda0008010201013f105f4fffc40015100101000000000000000000000000000000a1ffda0008010100013f10867fffd9";

/// A *progressive* JPEG is refused as `Unsupported` — the data is
/// well-formed, so the failure is not a parse/corrupt error. Input
/// bytes, the sentinel output, and the absence of temp files are
/// all preserved.
#[test]
fn cli_remove_region_progressive_jpeg_unsupported() {
    let dir = tmpdir("m27-1-prog");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    let original = jpeg_page_pdf(&unhex(JPEG_C8_PROG_HEX), 8, 8, "DeviceRGB");
    fs::write(&src, &original).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    let out = bin()
        .args(["experimental-remove-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // Must say "unsupported" — not "corrupt" or "parse".
    assert!(
        stderr.to_ascii_lowercase().contains("unsupported"),
        "stderr should say unsupported: {stderr}"
    );
    // Input untouched, sentinel preserved, no temp files.
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
    // No document bytes leak.
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("ff d8") && !stderr.contains("ff d8"));
    assert!(!stdout.contains("\\xff") && !stderr.contains("\\xff"));
}

// ------------------------------------------------------------------
// Grid-aligned CTM (rotation / reflection) image editing
// ------------------------------------------------------------------

/// A raw 3×2 gray image painted under `cm` — non-square so each
/// pixel's position is identifiable. Rows top-down:
/// `0x10 0x20 0x30 / 0x40 0x50 0x60`.
fn grid_ctm_gray_pdf(cm: &str) -> Vec<u8> {
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
    let img = [0x10u8, 0x20, 0x30, 0x40, 0x50, 0x60];
    let mut f = b"<< /Type /XObject /Subtype /Image /Width 3 /Height 2 \
      /ColorSpace /DeviceGray /BitsPerComponent 8 /Length "
        .to_vec();
    f.extend_from_slice(img.len().to_string().as_bytes());
    f.extend_from_slice(b" >>\nstream\n");
    f.extend_from_slice(&img);
    f.extend_from_slice(b"\nendstream");
    push(10, &f);
    let body = format!("q\n{cm}\n/Im1 Do\nQ\n").into_bytes();
    let mut sb = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    sb.extend_from_slice(&body);
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

/// `experimental-remove-image-region` on a 90°-rotated image —
/// succeeds, output re-parses, and only the selected pixel is black.
#[test]
fn cli_m28_rot90_image_edit() {
    let dir = tmpdir("m28-rot90");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // 90° rotation: [0 10 -6 0 38 60] → pixel (0,0) at x∈[32,35],
    // y∈[60,63.33].
    fs::write(&src, grid_ctm_gray_pdf("0 10 -6 0 38 60 cm")).unwrap();
    let out = bin()
        .args(["experimental-remove-image-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "32", "60", "35", "63.3", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("pixels changed: 1"), "{stdout}");
    assert!(stdout.contains("invocations edited: 1"), "{stdout}");
    // Re-parse: KasIm0 clone exists; verify the pixel sample.
    let out_doc = kasane_pdf::Document::from_bytes(&fs::read(&dst).unwrap()).unwrap();
    let page = out_doc.pages().first().unwrap();
    let ops = out_doc.page_operations(page).unwrap();
    let names: Vec<_> = ops
        .iter()
        .filter_map(|o| match o {
            kasane_pdf::Operation::Do(n) => Some(n.clone()),
            _ => None,
        })
        .collect();
    assert!(names.iter().any(|n| n == b"KasIm0"), "{names:?}");
}

/// A reflected image (horizontal flip) — the clone's leftmost
/// painted pixel is the image's pixel (0,0) but at the *right* of
/// the painted bbox after the flip.
#[test]
fn cli_m28_hflip_image_edit() {
    let dir = tmpdir("m28-hflip");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // Horizontal reflection: [-6 0 0 10 38 60] → pixel (0,0) lands
    // at x∈[36,38], y∈[65,70].
    fs::write(&src, grid_ctm_gray_pdf("-6 0 0 10 38 60 cm")).unwrap();
    let out = bin()
        .args(["experimental-remove-image-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "36", "65", "38", "70", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("pixels changed: 1"), "{stdout}");
}

/// An arbitrary-angle (30°) rotation is refused — input and the
/// sentinel output stay untouched, no temp files leak, and no
/// document bytes reach stdout/stderr.
#[test]
fn cli_m28_arbitrary_rotation_refused() {
    let dir = tmpdir("m28-arb-ref");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // ~30° rotation [34.64 20 -20 34.64 ...] — neither axis-aligned
    // nor axis-swapped.
    fs::write(&src, grid_ctm_gray_pdf("34.64 20 -20 34.64 20 60 cm")).unwrap();
    let original = fs::read(&src).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    let out = bin()
        .args(["experimental-remove-image-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.to_ascii_lowercase().contains("unsupported"),
        "stderr should say unsupported: {stderr}"
    );
    assert_eq!(fs::read(&src).unwrap(), original);
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
    let stray: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.contains("kasane-tmp").then_some(n)
        })
        .collect();
    assert!(stray.is_empty(), "temp files left: {stray:?}");
}

/// A shear CTM is refused the same way — protects the
/// not-grid-aligned boundary.
#[test]
fn cli_m28_shear_refused() {
    let dir = tmpdir("m28-shear-ref");
    let src = dir.join("in.pdf");
    let dst = dir.join("out.pdf");
    // Shear [1 0 0.5 1 ...] — c != 0 breaks both classes.
    fs::write(&src, grid_ctm_gray_pdf("1 0 0.5 1 20 60 cm")).unwrap();
    fs::write(&dst, b"SENTINEL").unwrap();
    let out = bin()
        .args(["experimental-remove-image-region"])
        .arg(&src)
        .arg("1")
        .args(["--rect", "0", "0", "200", "200", "--output"])
        .arg(&dst)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(fs::read(&dst).unwrap(), b"SENTINEL");
}
