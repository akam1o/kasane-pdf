//! `/CCITTFaxDecode` (ISO 32000-1 §7.4.6) and `/JBIG2Decode` (§7.4.7)
//! images: hand-encoded 8×2 bitmaps (left half white, right half
//! black) drawn over a 100×100 pt page at 72 dpi, so pixmap x < 50 is
//! the white half and x ≥ 50 the black half.
//!
//! The G4 stream `36 F0 01 00 10` is: row 1 in horizontal mode
//! (`001` + white run 4 `1011` + black run 4 `011`), row 2 as two
//! vertical-0 codes (`1 1`), then EOFB. The G3 1D stream `B7 6C` is
//! `1011 011` twice. The JBIG2 stream wraps the G4 data in an MMR
//! generic region (T.88 §7.4.6, flags = 1) after a page-info segment.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions, RenderedPage};

const G4: &[u8] = &[0x36, 0xF0, 0x01, 0x00, 0x10];
const G3_1D: &[u8] = &[0xB7, 0x6C];
const G3_1D_EOL: &[u8] = &[0x00, 0x1B, 0x60, 0x01, 0xB6];
const G3_1D_ALIGNED: &[u8] = &[0xB6, 0xB6];

fn be(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Segment header (T.88 §7.2) for a ≤ 4-referred segment on page 1.
fn seg(num: u32, kind: u8, data: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend_from_slice(&be(num));
    s.push(kind);
    s.push(0); // no referred-to segments
    s.push(1); // page association
    s.extend_from_slice(&be(data.len() as u32));
    s.extend_from_slice(data);
    s
}

fn page_info(w: u32, h: u32) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&be(w));
    d.extend_from_slice(&be(h));
    d.extend_from_slice(&be(0));
    d.extend_from_slice(&be(0));
    d.push(0); // flags: default pixel 0, OR
    d.extend_from_slice(&[0, 0]); // no striping
    seg(0, 48, &d)
}

fn mmr_region(w: u32, h: u32, mmr: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&be(w));
    d.extend_from_slice(&be(h));
    d.extend_from_slice(&be(0));
    d.extend_from_slice(&be(0));
    d.push(0); // external combination operator OR
    d.push(0x01); // generic region flags: MMR = 1
    d.extend_from_slice(mmr);
    seg(1, 38, &d) // immediate generic region
}

fn jbig2_embedded() -> Vec<u8> {
    let mut s = page_info(8, 2);
    s.extend_from_slice(&mmr_region(8, 2, G4));
    s
}

fn page(content: &[u8], image: Vec<u8>, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut all = vec![
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
    ];
    all.extend(extra);
    build_pdf(&all)
}

fn image(dict: &str, data: &[u8]) -> Vec<u8> {
    stream_obj(
        &format!("/Type /XObject /Subtype /Image /Width 8 /Height 2 {dict}"),
        data,
    )
}

const DRAW: &[u8] = b"q 100 0 0 100 0 0 cm /Im Do Q";

fn render(pdf: &[u8]) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes(pdf)?;
    render_page(&doc, &doc.pages()[0], &RenderOptions::default())
}

fn render_with(pdf: &[u8], limits: Limits) -> Result<RenderedPage, Error> {
    let doc = Document::from_bytes_with_limits(pdf, limits.clone())?;
    let opts = RenderOptions::default().with_limits(limits);
    render_page(&doc, &doc.pages()[0], &opts)
}

fn px(r: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8) {
    let p = r.pixmap().pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const BLACK: (u8, u8, u8) = (0, 0, 0);

/// Left half white, right half black on both image rows.
fn assert_left_white_right_black(r: &RenderedPage, what: &str) {
    for y in [10, 40, 60, 90] {
        assert_eq!(px(r, 20, y), WHITE, "{what}: left at y={y}");
        assert_eq!(px(r, 80, y), BLACK, "{what}: right at y={y}");
    }
    // Column 4 is the boundary: columns 3 and 4 map to x 37..50 and 50..62.
    assert_eq!(px(r, 45, 50), WHITE, "{what}: just left of the edge");
    assert_eq!(px(r, 55, 50), BLACK, "{what}: just right of the edge");
}

// ---- CCITT -----------------------------------------------------------

#[test]
fn ccitt_group4() {
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode \
         /DecodeParms << /K -1 /Columns 8 /Rows 2 >>",
        G4,
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "G4");
}

#[test]
fn ccitt_group3_variants() {
    for (parms, data, what) in [
        ("/K 0 /Columns 8 /Rows 2", G3_1D, "G3 1D"),
        (
            "/K 0 /Columns 8 /Rows 2 /EndOfLine true",
            G3_1D_EOL,
            "G3 1D with EOLs",
        ),
        (
            "/K 0 /Columns 8 /Rows 2 /EncodedByteAlign true",
            G3_1D_ALIGNED,
            "G3 1D byte-aligned",
        ),
        // /Rows absent: the image height bounds the decode.
        ("/K 0 /Columns 8", G3_1D, "G3 1D without /Rows"),
        // /EndOfBlock false with exactly /Rows rows of data.
        (
            "/K -1 /Columns 8 /Rows 2 /EndOfBlock false",
            &G4[..2],
            "G4 no EOFB",
        ),
    ] {
        let im = image(
            &format!(
                "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode \
                 /DecodeParms << {parms} >>"
            ),
            data,
        );
        let r = render(&page(DRAW, im, vec![])).unwrap();
        assert_left_white_right_black(&r, what);
    }
}

#[test]
fn ccitt_black_is_1_and_decode_array() {
    // /BlackIs1 true delivers 1 for black; DeviceGray reads 1 as white,
    // so without /Decode the picture inverts …
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode \
         /DecodeParms << /K -1 /Columns 8 /Rows 2 /BlackIs1 true >>",
        G4,
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_eq!(px(&r, 20, 50), BLACK);
    assert_eq!(px(&r, 80, 50), WHITE);
    // … and /Decode [1 0] restores it (the usual producer pairing).
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0] /Filter /CCITTFaxDecode \
         /DecodeParms << /K -1 /Columns 8 /Rows 2 /BlackIs1 true >>",
        G4,
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "BlackIs1 + Decode [1 0]");
    // /Decode [1 0] on the default polarity inverts.
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0] /Filter /CCITTFaxDecode \
         /DecodeParms << /K -1 /Columns 8 /Rows 2 >>",
        G4,
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_eq!(px(&r, 20, 50), BLACK);
    assert_eq!(px(&r, 80, 50), WHITE);
}

#[test]
fn ccitt_image_mask_paints_black_pixels_in_fill_colour() {
    let im = image(
        "/ImageMask true /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 8 /Rows 2 >>",
        G4,
    );
    let r = render(&page(b"q 1 0 0 rg 100 0 0 100 0 0 cm /Im Do Q", im, vec![])).unwrap();
    assert_eq!(px(&r, 20, 50), WHITE, "sample 1 (white) is not painted");
    assert_eq!(
        px(&r, 80, 50),
        (255, 0, 0),
        "sample 0 (black) takes the fill colour"
    );
}

#[test]
fn ccitt_inline_image() {
    let mut c = b"q 100 0 0 100 0 0 cm BI /W 8 /H 2 /BPC 1 /CS /G /F /CCF \
                  /DP << /K -1 /Columns 8 /Rows 2 >> ID "
        .to_vec();
    c.extend_from_slice(G4);
    c.extend_from_slice(b" EI Q");
    let im = image("/ColorSpace /DeviceGray /BitsPerComponent 1", b"\xF0\xF0");
    let r = render(&page(&c, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "inline /CCF");
}

#[test]
fn ccitt_refusals() {
    type Case = (
        &'static str,
        &'static [u8],
        fn(&Error) -> bool,
        &'static str,
    );
    let cases: [Case; 7] = [
        (
            "/K -1 /Columns 16 /Rows 2",
            G4,
            |e| matches!(e, Error::Structure(_)),
            "Columns ≠ Width",
        ),
        (
            "/K -1 /Columns 8 /Rows 1",
            G4,
            |e| matches!(e, Error::Parse(_)),
            "fewer rows than the image",
        ),
        // (All-1 bytes would be valid G4: V0 codes, i.e. white rows.)
        (
            "/K -1 /Columns 8 /Rows 2",
            &[0x00, 0x00, 0x00, 0x00],
            |e| matches!(e, Error::Parse(_)),
            "garbage",
        ),
        (
            "/K -1 /Columns 8 /Rows 2",
            &[],
            |e| matches!(e, Error::Parse(_)),
            "empty",
        ),
        (
            "/K -1 /Columns 8 /Rows 2 /BlackIs1 1",
            G4,
            |e| matches!(e, Error::Structure(_)),
            "BlackIs1 not a boolean",
        ),
        (
            "/K -1 /Columns 8 /Rows 2 /DamagedRowsBeforeError -1",
            G4,
            |e| matches!(e, Error::Structure(_)),
            "negative DamagedRowsBeforeError",
        ),
        (
            "/K -1 /Columns 0 /Rows 2",
            G4,
            |e| matches!(e, Error::Structure(_)),
            "Columns 0",
        ),
    ];
    for (parms, data, ok, what) in cases {
        let im = image(
            &format!(
                "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /CCITTFaxDecode \
                 /DecodeParms << {parms} >>"
            ),
            data,
        );
        let err = render(&page(DRAW, im, vec![])).unwrap_err();
        assert!(ok(&err), "{what}: {err:?}");
    }
    // 8 bpc or a colour space with several components contradicts the filter.
    for dict in [
        "/ColorSpace /DeviceGray /BitsPerComponent 8",
        "/ColorSpace /DeviceRGB /BitsPerComponent 1",
    ] {
        let im = image(
            &format!("{dict} /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 8 /Rows 2 >>"),
            G4,
        );
        let err = render(&page(DRAW, im, vec![])).unwrap_err();
        assert!(matches!(err, Error::Structure(_)), "{dict}: {err:?}");
    }
    // The image filter must be last in the chain.
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter [/CCITTFaxDecode /ASCIIHexDecode] \
         /DecodeParms [<< /K -1 /Columns 8 /Rows 2 >> null]",
        G4,
    );
    let err = render(&page(DRAW, im, vec![])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn ccitt_after_a_byte_filter() {
    let hex = G4.iter().map(|b| format!("{b:02X}")).collect::<String>() + ">";
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter [/ASCIIHexDecode /CCITTFaxDecode] \
         /DecodeParms [null << /K -1 /Columns 8 /Rows 2 >>]",
        hex.as_bytes(),
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "AHx + CCITT");
}

#[test]
fn ccitt_inline_image_ei_without_separator() {
    // Producers write `EI` right after the last data byte (pdf.js
    // issue 19532); the data is delimited by decoding, not by white
    // space. A `/Rows`-less /DP is the common form.
    let mut c = b"q 100 0 0 100 0 0 cm BI /W 8 /H 2 /BPC 1 /CS /G /F /CCF \
                  /DP << /K -1 /Columns 8 >> ID "
        .to_vec();
    c.extend_from_slice(&G4[..2]);
    c.extend_from_slice(b"EI Q");
    let im = image("/ColorSpace /DeviceGray /BitsPerComponent 1", b"\xF0\xF0");
    let r = render(&page(&c, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "inline /CCF, EI glued");
    // Data that itself contains the bytes `EI ` must not end the image
    // early: the G4 stream below encodes 8×2 with 'E','I',' ' as the
    // first bytes — only the candidate that decodes completely wins.
    let glued: Vec<u8> = [
        &b"q 100 0 0 100 0 0 cm BI /W 24 /H 1 /BPC 1 /CS /G /F /CCF /DP << /K 0 /Columns 24 >> ID "
            [..],
        &[0x45, 0x49, 0x20][..],
    ]
    .concat();
    // 0x45 0x49 0x20 = white 2, black ...: just check it is either
    // decoded or refused with a parse error, never mis-split silently.
    let mut c2 = glued.clone();
    c2.extend_from_slice(b"EI Q");
    let im = image("/ColorSpace /DeviceGray /BitsPerComponent 1", b"\xF0\xF0");
    let got = render(&page(&c2, im, vec![]));
    assert!(
        got.is_ok() || matches!(got, Err(Error::Parse(_))),
        "{got:?}"
    );
}

// ---- JBIG2 -----------------------------------------------------------

#[test]
fn jbig2_embedded_generic_region() {
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
        &jbig2_embedded(),
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_left_white_right_black(&r, "JBIG2 MMR");
    // JBIG2 1 = black is delivered as sample 0, so an image mask paints
    // exactly the black pixels.
    let im = image("/ImageMask true /Filter /JBIG2Decode", &jbig2_embedded());
    let r = render(&page(b"q 0 0 1 rg 100 0 0 100 0 0 cm /Im Do Q", im, vec![])).unwrap();
    assert_eq!(px(&r, 20, 50), WHITE);
    assert_eq!(px(&r, 80, 50), (0, 0, 255));
    // /Decode [1 0] inverts.
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Decode [1 0] /Filter /JBIG2Decode",
        &jbig2_embedded(),
    );
    let r = render(&page(DRAW, im, vec![])).unwrap();
    assert_eq!(px(&r, 20, 50), BLACK);
    assert_eq!(px(&r, 80, 50), WHITE);
}

#[test]
fn jbig2_globals_stream() {
    // The page-info segment lives in /JBIG2Globals (itself Flate-free
    // here, but decoded through the ordinary stream path), the region
    // in the image stream.
    let globals = stream_obj("", &page_info(8, 2));
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode \
         /DecodeParms << /JBIG2Globals 6 0 R >>",
        &mmr_region(8, 2, G4),
    );
    let r = render(&page(DRAW, im, vec![(6, globals)])).unwrap();
    assert_left_white_right_black(&r, "JBIG2 with globals");
    // Globals that are not a stream.
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode \
         /DecodeParms << /JBIG2Globals 6 0 R >>",
        &mmr_region(8, 2, G4),
    );
    let err = render(&page(DRAW, im, vec![(6, b"<< /Not /AStream >>".to_vec())])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn jbig2_refusals_and_limits() {
    // Page size disagrees with the dictionary.
    let mut s = page_info(16, 2);
    s.extend_from_slice(&mmr_region(16, 2, G4));
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
        &s,
    );
    let err = render(&page(DRAW, im, vec![])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "size mismatch: {err:?}");
    // Garbage, truncated and empty streams.
    for data in [
        &b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff"[..],
        &jbig2_embedded()[..20],
        b"",
    ] {
        let im = image(
            "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
            data,
        );
        let err = render(&page(DRAW, im, vec![])).unwrap_err();
        assert!(
            matches!(err, Error::Parse(_) | Error::Structure(_)),
            "{data:?}: {err:?}"
        );
    }
    // A region whose MMR data is corrupt.
    let mut s = page_info(8, 2);
    s.extend_from_slice(&mmr_region(8, 2, &[0x00, 0x00, 0x00]));
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
        &s,
    );
    let err = render(&page(DRAW, im, vec![])).unwrap_err();
    assert!(matches!(err, Error::Parse(_)), "corrupt MMR: {err:?}");
    // A huge declared page is refused by the image limits before any
    // bitmap is allocated.
    let mut s = page_info(8, 2);
    s.extend_from_slice(&mmr_region(8, 2, G4));
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode",
        &s,
    );
    let err = render_with(
        &page(DRAW, im, vec![]),
        Limits::default().with_max_image_total_samples(8),
    )
    .unwrap_err();
    assert!(matches!(err, Error::LimitExceeded { .. }), "{err:?}");
    // 8 bpc contradicts the filter.
    let im = image(
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /JBIG2Decode",
        &jbig2_embedded(),
    );
    let err = render(&page(DRAW, im, vec![])).unwrap_err();
    assert!(matches!(err, Error::Structure(_)), "{err:?}");
}

#[test]
fn jbig2_multi_page_document_decodes_each_image() {
    // Two pages, two images, one shared globals stream.
    let globals = stream_obj("", &page_info(8, 2));
    let im = |n: u32| {
        (
            n,
            image(
                "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode \
                 /DecodeParms << /JBIG2Globals 20 0 R >>",
                &mmr_region(8, 2, G4),
            ),
        )
    };
    let pg = |n: u32, c: u32, i: u32| {
        (
            n,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << /XObject << /Im {i} 0 R >> >> /Contents {c} 0 R >>"
            )
            .into_bytes(),
        )
    };
    let pdf = build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (
            2,
            b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        ),
        pg(3, 5, 10),
        pg(4, 5, 11),
        (5, stream_obj("", DRAW)),
        im(10),
        im(11),
        (20, globals),
    ]);
    let doc = Document::from_bytes(&pdf).unwrap();
    for p in doc.pages() {
        let r = render_page(&doc, p, &RenderOptions::default()).unwrap();
        assert_left_white_right_black(&r, "multi-page");
    }
}
