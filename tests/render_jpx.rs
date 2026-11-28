//! `/JPXDecode` images (ISO 32000-1 §7.4.9): 4×4 codestreams written
//! by OpenJPEG (`opj_compress -n 1`), left half red / right half blue
//! (RGB), left black / right white (gray), and an RGBA JP2 whose
//! alpha is opaque on the left and transparent on the right. Drawn
//! over a 100×100 pt page at 72 dpi: pixmap x < 50 is the left half.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

const RGB_J2K: &[u8] = b"\xff\x4f\xff\x51\x00\x2f\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x29\x00\x01\xff\x93\xcf\xb4\x20\x11\x50\x49\x8a\x65\x20\x00\x05\xdf\x80\x28\x1c\xf2\xdb\xc7\x7f\xdf\x80\x28\x13\xff\x48\xe4\x23\xff\xd9";
const GRAY_J2K: &[u8] = b"\xff\x4f\xff\x51\x00\x29\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x00\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x24\x00\x01\xff\x93\xdf\x80\x98\x11\x4f\x85\xfd\xdf\xd9\xeb\xd4\xe0\x1a\x91\xe0\xd7\x57\x09\xd6\xfa\xda\x7f\xff\xd9";
const RGB_JP2: &[u8] = b"\x00\x00\x00\x0c\x6a\x50\x20\x20\x0d\x0a\x87\x0a\x00\x00\x00\x14\x66\x74\x79\x70\x6a\x70\x32\x20\x00\x00\x00\x00\x6a\x70\x32\x20\x00\x00\x00\x2d\x6a\x70\x32\x68\x00\x00\x00\x16\x69\x68\x64\x72\x00\x00\x00\x04\x00\x00\x00\x04\x00\x03\x07\x07\x00\x00\x00\x00\x00\x0f\x63\x6f\x6c\x72\x01\x00\x00\x00\x00\x00\x10\x00\x00\x00\xa1\x6a\x70\x32\x63\xff\x4f\xff\x51\x00\x2f\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x03\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x29\x00\x01\xff\x93\xcf\xb4\x20\x11\x50\x49\x8a\x65\x20\x00\x05\xdf\x80\x28\x1c\xf2\xdb\xc7\x7f\xdf\x80\x28\x13\xff\x48\xe4\x23\xff\xd9";
const RGBA_JP2: &[u8] = b"\x00\x00\x00\x0c\x6a\x50\x20\x20\x0d\x0a\x87\x0a\x00\x00\x00\x14\x66\x74\x79\x70\x6a\x70\x32\x20\x00\x00\x00\x00\x6a\x70\x32\x20\x00\x00\x00\x2d\x6a\x70\x32\x68\x00\x00\x00\x16\x69\x68\x64\x72\x00\x00\x00\x04\x00\x00\x00\x04\x00\x04\x07\x07\x00\x00\x00\x00\x00\x0f\x63\x6f\x6c\x72\x01\x00\x00\x00\x00\x00\x10\x00\x00\x00\xb1\x6a\x70\x32\x63\xff\x4f\xff\x51\x00\x32\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x07\x01\x01\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x36\x00\x01\xff\x93\xcf\xb4\x20\x11\x50\x49\x8a\x65\x20\x00\x05\x80\xdf\x80\x20\x14\x00\x5c\xaf\xdf\x80\x90\x1c\x46\xe8\x10\xa9\xcc\x16\x20\xd9\xa0\xf1\x3a\xdf\x5b\x4b\xab\x69\x75\xff\xd9";

fn page(content: &[u8], image: Vec<u8>) -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", content)),
        (5, image),
    ])
}

fn image(dict: &str, data: &[u8]) -> Vec<u8> {
    stream_obj(
        &format!("/Type /XObject /Subtype /Image /Width 4 /Height 4 /Filter /JPXDecode {dict}"),
        data,
    )
}

const DRAW: &[u8] = b"q 100 0 0 100 0 0 cm /Im Do Q";

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol
        && (a.1 as i32 - b.1 as i32).abs() <= tol
        && (a.2 as i32 - b.2 as i32).abs() <= tol
}

#[test]
fn rgb_codestream_with_and_without_colour_space() {
    for dict in ["/ColorSpace /DeviceRGB /BitsPerComponent 8", ""] {
        let r = render(&page(DRAW, image(dict, RGB_J2K))).unwrap();
        assert!(
            near(px(&r, 25, 50), (255, 0, 0), 3),
            "{dict:?}: {:?}",
            px(&r, 25, 50)
        );
        assert!(
            near(px(&r, 75, 50), (0, 0, 255), 3),
            "{dict:?}: {:?}",
            px(&r, 75, 50)
        );
    }
    // The JP2 container wraps the same codestream; /BitsPerComponent
    // and /Decode are ignored for JPX (§7.4.9).
    let r = render(&page(
        DRAW,
        image("/BitsPerComponent 1 /Decode [1 0 1 0 1 0]", RGB_JP2),
    ))
    .unwrap();
    assert!(near(px(&r, 25, 50), (255, 0, 0), 3));
    assert!(near(px(&r, 75, 50), (0, 0, 255), 3));
}

#[test]
fn gray_codestream_and_byte_filter_chain() {
    let r = render(&page(DRAW, image("", GRAY_J2K))).unwrap();
    assert!(near(px(&r, 25, 50), (0, 0, 0), 3));
    assert!(near(px(&r, 75, 50), (255, 255, 255), 3));
    let hex = GRAY_J2K
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<String>()
        + ">";
    let im = stream_obj(
        "/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray \
         /Filter [/ASCIIHexDecode /JPXDecode]",
        hex.as_bytes(),
    );
    let r = render(&page(DRAW, im)).unwrap();
    assert!(near(px(&r, 25, 50), (0, 0, 0), 3));
}

#[test]
fn alpha_channel_follows_smask_in_data() {
    // Without /SMaskInData the alpha channel is dropped: red everywhere.
    let r = render(&page(DRAW, image("", RGBA_JP2))).unwrap();
    assert!(near(px(&r, 75, 50), (255, 0, 0), 3), "{:?}", px(&r, 75, 50));
    // /SMaskInData 1: the right half is transparent.
    let r = render(&page(DRAW, image("/SMaskInData 1", RGBA_JP2))).unwrap();
    assert!(near(px(&r, 25, 50), (255, 0, 0), 3), "{:?}", px(&r, 25, 50));
    assert_eq!(px(&r, 75, 50), (255, 255, 255));
    // A codestream without alpha cannot carry /SMaskInData.
    let err = render(&page(DRAW, image("/SMaskInData 1", RGB_J2K))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn jpx_soft_mask_stream() {
    // An RGB image (left red, right blue) under a JPX-encoded /SMask
    // (left black = transparent, right white = opaque).
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
              /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>"
                .to_vec(),
        ),
        (4, stream_obj("", DRAW)),
        (5, image("/ColorSpace /DeviceRGB /SMask 6 0 R", RGB_J2K)),
        (
            6,
            stream_obj(
                "/Type /XObject /Subtype /Image /Width 4 /Height 4 /Filter /JPXDecode",
                GRAY_J2K,
            ),
        ),
    ]);
    let r = render(&pdf).unwrap();
    assert_eq!(px(&r, 25, 50), (255, 255, 255), "masked out");
    assert!(near(px(&r, 75, 50), (0, 0, 255), 3), "{:?}", px(&r, 75, 50));
}

#[test]
fn refusals_and_limits() {
    // Dictionary size disagrees with the codestream.
    let im = stream_obj(
        "/Type /XObject /Subtype /Image /Width 8 /Height 4 /Filter /JPXDecode",
        RGB_J2K,
    );
    let err = render(&page(DRAW, im)).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // /ColorSpace channel count disagrees with the codestream.
    let err = render(&page(DRAW, image("/ColorSpace /DeviceGray", RGB_J2K))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // JPX image masks are forbidden.
    let err = render(&page(DRAW, image("/ImageMask true", GRAY_J2K))).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
    // Garbage and truncated codestreams are parse errors.
    for data in [&b"\xff\x4f\xff\x51\x00\x00"[..], &RGB_J2K[..60], b"not jpx"] {
        let err = render(&page(DRAW, image("", data))).unwrap_err();
        assert!(
            matches!(err, Error::Parse(_) | Error::Structure(_)),
            "{err:?}"
        );
    }
    // Limits apply before any decoding.
    let doc = Document::from_bytes(&page(DRAW, image("", RGB_J2K))).unwrap();
    let opts =
        RenderOptions::default().with_limits(Limits::default().with_max_image_total_samples(8));
    let err = render_page(&doc, &doc.pages()[0], &opts).unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
    // /JPXDecode on a content stream is not an image.
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_vec(),
        ),
        (4, stream_obj("/Filter /JPXDecode", RGB_J2K)),
    ]);
    let err = render(&pdf).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
}

// A 4 × 1 grey RGBA JP2 (`opj_compress -n 1`) whose colour channels are
// premultiplied with its opacity channel, as /SMaskInData 2 declares:
// stored (64, 64, 64, α 128), (128, 128, 128, α 255), (0, 0, 0, α 0),
// (32, 32, 32, α 64).
const GREY_PM_JP2: &[u8] = b"\x00\x00\x00\x0c\x6a\x50\x20\x20\x0d\x0a\x87\x0a\x00\x00\x00\x14\x66\x74\x79\x70\x6a\x70\x32\x20\x00\x00\x00\x00\x6a\x70\x32\x20\x00\x00\x00\x2d\x6a\x70\x32\x68\x00\x00\x00\x16\x69\x68\x64\x72\x00\x00\x00\x01\x00\x00\x00\x04\x00\x04\x07\x07\x00\x00\x00\x00\x00\x0f\x63\x6f\x6c\x72\x01\x00\x00\x00\x00\x00\x10\x00\x00\x00\x9a\x6a\x70\x32\x63\xff\x4f\xff\x51\x00\x32\x00\x00\x00\x00\x00\x04\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x07\x01\x01\x07\x01\x01\x07\x01\x01\x07\x01\x01\xff\x52\x00\x0c\x00\x00\x00\x01\x01\x00\x04\x04\x00\x01\xff\x5c\x00\x04\x40\x40\xff\x64\x00\x25\x00\x01\x43\x72\x65\x61\x74\x65\x64\x20\x62\x79\x20\x4f\x70\x65\x6e\x4a\x50\x45\x47\x20\x76\x65\x72\x73\x69\x6f\x6e\x20\x32\x2e\x35\x2e\x34\xff\x90\x00\x0a\x00\x00\x00\x00\x00\x1f\x00\x01\xff\x93\xdf\x80\x20\x0e\x3b\x65\x3f\x80\x80\xdf\x80\x28\x0e\x11\xe4\xbe\x09\xff\xd9";

/// The 4 × 1 image at the bottom of the 100 × 100 page (pixmap rows
/// 90..100; each sample 10 pt wide) with `dict` added to its dictionary.
fn grey_pm_page(dict: &str) -> Vec<u8> {
    page(
        b"q 40 0 0 10 0 0 cm /Im Do Q",
        stream_obj(
            &format!("/Type /XObject /Subtype /Image /Width 4 /Height 1 /Filter /JPXDecode {dict}"),
            GREY_PM_JP2,
        ),
    )
}

/// sRGB's transfer function (IEC 61966-2-1) of a linear value.
fn srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

// CalRGB with gamma 1, the D65 white and the sRGB primaries' XYZ as its
// matrix: a grey v is linear light v, displayed as srgb(v) — a
// conversion that is not the identity on the samples.
const CALRGB_LINEAR: &str = "/ColorSpace [/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [1 1 1] \
     /Matrix [0.4124 0.2126 0.0193 0.3576 0.7152 0.1192 0.1805 0.0722 0.9505] >>]";

#[test]
fn premultiplied_samples_are_divided_in_their_own_space() {
    // /SMaskInData 2 (Table 87): the colour is premultiplied with the
    // opacity channel in the image's colour space. Divided there, then
    // converted, then composited on white: α·srgb(c/α) + (1 − α).
    // Dividing after the conversion instead gives srgb(c)/α — for the
    // first sample 0.537/0.502, clipped to white.
    let r = render(&grey_pm_page(&format!("{CALRGB_LINEAR} /SMaskInData 2"))).unwrap();
    for (x, stored, alpha) in [
        (5u32, 64u32, 128u32),
        (15, 128, 255),
        (25, 0, 0),
        (35, 32, 64),
    ] {
        let a = alpha as f64 / 255.0;
        let c = (stored * 255).checked_div(alpha).map_or(0, |v| v.min(255));
        let want = a * srgb(c as f64 / 255.0) + (1.0 - a);
        let w = (want * 255.0).round() as u8;
        let got = px(&r, x, 95);
        assert!(
            near(got, (w, w, w), 2),
            "sample at x {x}: got {got:?}, want {w}"
        );
    }
    // DeviceRGB, the identity: the same values without the transfer.
    let r = render(&grey_pm_page("/ColorSpace /DeviceRGB /SMaskInData 2")).unwrap();
    let got = px(&r, 5, 95);
    let want: f64 = ((128.0 / 255.0) * (128.0 / 255.0) + (1.0 - 128.0 / 255.0)) * 255.0;
    let w = want.round() as u8;
    assert!(near(got, (w, w, w), 2), "{got:?}");
}

#[test]
fn a_soft_mask_overrides_a_colour_key() {
    // §11.6.4.3 / Table 87: with /SMaskInData ≠ 0 the embedded soft mask
    // overrides the /Mask entry — a colour key that matches the stored
    // samples (64, 64, 64) masks nothing. Without a soft mask it does.
    let key = "/Mask [64 64 64 64 64 64]";
    let r = render(&grey_pm_page(&format!(
        "/ColorSpace /DeviceRGB /SMaskInData 2 {key}"
    )))
    .unwrap();
    assert!(px(&r, 5, 95).0 < 250, "keyed out: {:?}", px(&r, 5, 95));
    let r = render(&grey_pm_page(&format!(
        "/ColorSpace /DeviceRGB /SMaskInData 1 {key}"
    )))
    .unwrap();
    assert!(px(&r, 5, 95).0 < 250, "keyed out: {:?}", px(&r, 5, 95));
    let r = render(&grey_pm_page(&format!("/ColorSpace /DeviceRGB {key}"))).unwrap();
    assert_eq!(px(&r, 5, 95), (255, 255, 255));
    // With /SMaskInData ≠ 0 an /SMask is ignored (Table 87), not even
    // read: one that names nothing usable does not fail the image.
    let r = render(&grey_pm_page(
        "/ColorSpace /DeviceRGB /SMaskInData 2 /SMask (not a stream)",
    ))
    .unwrap();
    assert!(px(&r, 5, 95).0 < 250);
}
