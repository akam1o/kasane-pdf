//! A tiling pattern used inside a Type 3 glyph description (§8.7.2):
//! the pattern matrix maps pattern space to the default space of the
//! pattern's parent content stream — the glyph description, i.e. glyph
//! space — so the cells scale with the glyph. Cells whose `/BBox`
//! exceeds the step are executed one by one and count against
//! `max_pattern_tiles`.

mod common;

use common::{build_pdf, stream_obj};
use kasane_pdf::{render_page, Document, Error, Limits, RenderOptions};

/// One glyph, 100 pt (FontMatrix 0.001, Tfs 100), filling the 1000-unit
/// square [0 0 1000 1000] of glyph space with a pattern of step 100
/// glyph units (10 pt on the page): a black 50 × 50 square per cell,
/// `/BBox` 120 × 120 (overlapping, so cells run one at a time).
fn pdf() -> Vec<u8> {
    build_pdf(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 120] /Contents 4 0 R \
              /Resources << /Font << /F 5 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", b"BT /F 100 Tf 10 10 Td (a) Tj ET")),
        (
            5,
            b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
              /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /g 6 0 R >> \
              /Encoding << /Differences [97 /g] >> /FirstChar 97 /LastChar 97 /Widths [1000] \
              /Resources << /Pattern << /P 7 0 R >> >> >>"
                .to_vec(),
        ),
        (6, stream_obj("", b"1000 0 d0 /Pattern cs /P scn 0 0 1000 1000 re f")),
        (
            7,
            stream_obj(
                "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 120 120] /XStep 100 /YStep 100",
                b"0 g 0 0 50 50 re f",
            ),
        ),
    ])
}

fn render(tiles: u64) -> Result<kasane_pdf::RenderedPage, Error> {
    let doc = Document::from_bytes(&pdf()).unwrap();
    let opts = RenderOptions::new()
        .with_dpi(72.0)
        .with_limits(Limits::default().with_max_pattern_tiles(tiles));
    render_page(&doc, &doc.pages()[0], &opts)
}

#[test]
fn the_cells_live_in_glyph_space() {
    let r = render(4096).unwrap();
    let dark = |x: u32, y: u32| r.pixmap().pixel(x, 120 - y).unwrap().red() < 128;
    // Glyph origin at page (10, 10); a cell is 10 pt, its square 5 pt.
    for (x, y) in [(12, 12), (22, 12), (12, 22), (102, 102)] {
        assert!(dark(x, y), "square at ({x}, {y})");
    }
    for (x, y) in [(17, 12), (12, 17), (27, 27)] {
        assert!(!dark(x, y), "gap at ({x}, {y})");
    }
}

#[test]
fn overlapping_cells_count_against_max_pattern_tiles() {
    // 10 × 10 cells cover the square; the bound adds at most one cell
    // per side for the overlap and one for rounding: 12 × 12 = 144.
    assert!(render(144).is_ok());
    match render(99) {
        Err(Error::LimitExceeded { limit, max }) => {
            assert_eq!((limit, max), ("max_pattern_tiles", 99))
        }
        other => panic!("expected the tile limit, got {:?}", other.map(|_| ())),
    }
}
