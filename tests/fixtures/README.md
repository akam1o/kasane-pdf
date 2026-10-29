
## Test fonts and font-derived fixtures

| File | Provenance | Licence |
|---|---|---|
| `KasaneTest.ttf` | 5-glyph synthetic font drawn with fontTools by `make_font.py` (not in the repository — fontTools is a build-time tool, not a dependency); every glyph is a simple rectangular outline so pixel checks are trivially verifiable; no external font data is copied | none (generated) |
| `KasaneCIDTest.ttf` | 6-glyph synthetic font (`make_cid_font.py`) — .notdef/box/tri/bar/circle/hole at non-identity GIDs | none (generated) |
| `KasaneJP.ttf` | 17-glyph subset of **Noto Sans JP** (`NotoSansJP-VF.ttf`, notofonts/noto-cjk `Sans2.004`) produced by `make_jp_font.py`; TrueType `glyf`, UPM = 1000; renamed so the OFL Reserved Font Name 'Source' is not used | SIL Open Font License 1.1 — Copyright 2014-2021 Adobe; text in [`OFL-1.1.txt`](OFL-1.1.txt), notice in the top-level `NOTICE` |
| `type1_cff.bin` | 800-byte bare CFF program — a `/FontFile3` `/Type1C` subset of **Source Sans Pro Bold** (charset A–f, subset tag `PAXEKO`) as embedded in a PDF of the compatibility corpus. The font name strings inside the program are renamed `SourceSansPro` → `KasaneSansPro` (same length, so every CFF INDEX offset is unchanged) because the OFL reserves the name 'Source' for the unmodified font; the embedded copyright string is kept | SIL Open Font License 1.1 — Copyright 2010, 2012 Adobe Systems Incorporated (from the program's own copyright string); text in [`OFL-1.1.txt`](OFL-1.1.txt), notice in `NOTICE` |
| `eexec_cmsy10.bin` | 7 KB `/FontFile` (Adobe Type 1, eexec-encrypted) program — a 2-glyph subset (`.notdef`, `periodcentered`, subset tag `ASJIQB`) of **CMBSY10** version 003.002 (AMS Computer Modern bold math symbols, Type 1 release of 2009) as embedded in an arXiv PDF of the compatibility corpus. Provenance and licence are stated by the program's own clear-text header: `Copyright (c) 1997, 2009 American Mathematical Society, with Reserved Font Name CMBSY10. This Font Software is licensed under the SIL Open Font License, Version 1.1`. Because the OFL reserves that name, every identifier in the clear-text part is renamed `CMBSY10` → `KSBSY10` (the copyright notice itself is kept verbatim); the eexec section is untouched | SIL Open Font License 1.1 — Copyright (c) 1997, 2009 American Mathematical Society; text in [`OFL-1.1.txt`](OFL-1.1.txt), notice in `NOTICE` |

WinAnsiEncoding tables in `src/font.rs` are derived from ISO 32000-1
Annex D and the Adobe Glyph List (public-domain name→Unicode mapping
shipped by fontTools).

## JPEG fixtures

The unit tests in `src/remove_region.rs` embed three small baseline
JPEGs as hex constants:

- `JPEG_G8_HEX` — 8×8 DeviceGray, 375 bytes
- `JPEG_C8_HEX` — 8×8 DeviceRGB, 659 bytes
- `JPEG_C16_HEX` — 16×16 DeviceRGB, 702 bytes

All were generated with `cjpeg` (libjpeg-turbo) from deterministic
PGM/PPM patterns — no external image data is redistributed. The
generator script is a *build-time* tool only; the embedded bytes
ship inside the test source so `cargo test` needs no network or
external tools. The PPM/PGM sources are one-pixel-per-value
gradients — trivially regenerable:

```sh
# gray 8x8: value = (x*31 + y*17) % 256
python3 -c "
import sys
w=h=8
sys.stdout.buffer.write(b'P5\n8 8\n255\n')
sys.stdout.buffer.write(bytes((x*31+y*17)%256 for y in range(h) for x in range(w)))
" > g8.pgm
cjpeg -grayscale -quality 90 -outfile g8.jpg g8.pgm

# RGB 8x8: r=x*32, g=y*32, b=(x+y)*16 (mod 256); RGB 16x16 similar.
cjpeg -quality 90 -outfile c8.jpg c8.ppm
cjpeg -quality 90 -outfile c16.jpg c16.ppm
```

`cjpeg`/`djpeg` are development-machine tools, not library
dependencies — the JPEG decoder used by the library is the pure-Rust
`jpeg-decoder` crate already declared in `Cargo.toml`.

### Coding-process and colour-transform constants

Three more constants exercise the coding-process check and the
`/ColorTransform` precedence rules:

- `JPEG_C8_PROG_HEX` — 8×8 DeviceRGB *progressive* JPEG, 553 bytes,
  `cjpeg -progressive -quality 90 -outfile c8_prog.jpg c8.ppm`.
  Must be refused as `Unsupported` (SOF2), not accepted as a
  sequential-DCT frame.
- `JPEG_C8_RGB_HEX` — 8×8 DeviceRGB baseline JPEG *encoded as RGB*
  (Adobe APP14 `transform`=0, component IDs `R`,`G`,`B`, no chroma
  subsampling), 374 bytes, `cjpeg -rgb -quality 90 -outfile
  c8_rgb.jpg c8.ppm`. Decodes to raw RGB samples.
- `JPEG_C8_NOJFIF_HEX` — 8×8 DeviceRGB baseline JPEG with the JFIF
  APP0 segment manually removed (the first APPn segment after SOI is
  deleted), 641 bytes. Component IDs `1,2,3` still mark it as YCbCr
  and no Adobe APP14 is present, so the decoder's default transform
  applies — equivalent to `/ColorTransform 1`.

All are derived from the same deterministic `c8.ppm` as
`JPEG_C8_HEX`. Expected decoded samples are taken from `djpeg`
(libjpeg-turbo) output — e.g. `djpeg -pnm c8.jpg` starts with
`01 00 00 16 07 0c` and `djpeg -pnm c8_rgb.jpg` (raw RGB) starts
with `00 00 00 1f 00 0f`. jpeg-decoder's fixed-point YCbCr→RGB
rounds ±1 from libjpeg; that tolerance is asserted in the tests.

### Effective colour-transform constants

Five more constants exercise the *effective* colour-transform
decision — APP14-vs-component-ID precedence, the spec default, and
the ambiguous/malformed-marker refusals:

- `JPEG_C8_444_HEX` — 8×8 DeviceRGB baseline JPEG, 4:4:4 sampling
  (no chroma subsampling), component IDs `1,2,3`, JFIF APP0, 648
  bytes; `cjpeg -sample 1x1 -quality 90 -outfile c8_444.jpg c8.ppm`.
- `JPEG_C8_444_APP14_0_HEX` — `c8_444` plus an Adobe APP14
  `transform=0` injected after SOI, 664 bytes. APP14 wins over the
  `(1,2,3)` component IDs → raw component values.
- `JPEG_C8_RGB_APP14_1_HEX` — `c8_rgb` with its APP14 rewritten to
  `transform=1`, 374 bytes. APP14 wins over the `(R,G,B)` IDs →
  YCbCr→RGB applied to RGB-encoded data.
- `JPEG_C8_APP14_BAD_HEX` — `c8_444` plus APP14 `transform=3`,
  664 bytes; values outside 0–2 are undefined in TN #5116 → refused.
- `JPEG_C8_APP14_DUP_HEX` — `c8_444` with two contradictory APP14
  markers (`transform=0` then `1`), 680 bytes → refused.

APP14 segments were injected by hand: `FF EE 00 0E "Adobe" 00 64
00 00 00 00 <tf>` — a 12-byte payload whose last byte is the
transform flag (the layout the decoder's `parse_app` reads).

`dct_64x64_red_blue.jpg` — 64×64 baseline JPEG, left half red, right half blue, written by Pillow (quality 95, no chroma subsampling); used by `tests/render_image_limits.rs` to exercise the reduced-DCT decode of an image larger than `max_image_dimension`. Generated; no licence.
