# Rendering & text — support matrix

Page→pixmap rendering (`render_page`), text rendering/extraction,
and the related limits. Anything outside the listed subset is a
typed `Error::Unsupported` — never a partial render.


## Rendering (ISO 32000-1 §8, §9)

`render_page` executes supported operations into a `Pixmap`
(`tiny_skia`) over a white background; `pixmap_to_png` encodes it.
Only pages whose operations all fall inside this subset render —
anything else fails with `Error::Unsupported` and produces no output.

| Feature | Status | Notes |
|---|---|---|
| `q`/`Q`/`cm` | ✅ | *special graphics state* ops (§8.2 Table 51), allowed at page description level only — inside a path object they are `Error::Structure` (§8.2 Figure 9). Paths are built in user space; `CTM × page→pixmap` is applied at paint time |
| `m` `l` `c` `v` `y` `h` `re` | ✅ | accumulate in the current (user-space) path; `m`/`re` begin a path object |
| `S` `s` `f` `F` `f*` `B` `B*` `b` `b*` `n` | ✅ | nonzero-winding and even-odd rules; open subpaths are implicitly closed for filling |
| `W` `W*` | ✅ | mark the current path as the clip operand (must follow the *last* construction op — §8.5.4); the clip installs when the path is *consumed*: a paint op draws under the **pre-W** clip first, then the clip updates; `W`+`n` installs without painting; `W` on an empty path empties the clip; `q`/`Q` save/restore |

**Placement enforcement** (§8.2 Figure 9, §8.5.4): inside a path
object only path-construction ops, `W`/`W*`, and the terminating
painting/`n` op are allowed. The renderer rejects out-of-order
operators with `Error::Structure` — e.g. `cm`, `q`/`Q`, colour ops,
`m`/`l`/`re` after `W`/`W*`, a second `W`/`W*` in the same path, or
`l`/`c`/`v`/`y` without a current point. The parse-only
`page_operations()` API still returns these sequences; placement is
a *render-time* restriction, not a parsing one.
| `w` `J` `j` `M` `d` `i` | ✅ | line width 0 → 1-device-pixel hairline (§8.4.3.2) |
| `G` `g` `RG` `rg` `K` `k` | ✅ | DeviceGray/RGB/CMYK |
| `CS` `cs` + `SC` `sc` | ✅ | `DeviceGray`/`DeviceRGB`/`DeviceCMYK` and `DefaultGray`/`DefaultRGB`/`DefaultCMYK` |
| `SCN` `scn` pattern name | ❌ | `Error::Unsupported` — patterns not rendered |
| `CS`/`cs` non-device space | ❌ | `Error::Unsupported` — no ICCBased/Separation/DeviceN/Pattern |
| text ops `Tf` `Tj` `TJ` `'` `"` | ✅ | embedded `/TrueType` + `/FontFile2` + `WinAnsiEncoding` (M5) *or* `/Type0` + Identity-H + CIDFontType2 + `/FontFile2` (M7) *or* `/Type1`/`/MMType1` + `/FontFile3` (raw CFF); `BT`/`ET` init `Tm`/`Tlm`; `Tc`/`Tw`/`Tz`/`TL`/`Tr`/`Ts` honoured — `Tw` applies to single-byte code 32 only (never to 2-byte Identity-H codes, §9.3.3); modes 0–3 painted (4–7 → `Unsupported`); PDF widths (`/Widths`, `/DW`/`/W`) used for advance — font-internal `hmtx` never consulted for positioning. `/Type3` supported: each glyph's `/CharProcs` stream runs like a mini Form XObject under `ctm×trm×/FontMatrix` (nested `BT…ET` bounded by `max_xobject_depth`). Refused: `/FontFile` (Adobe Type 1/eexec), `/FontFile3` CIDFontType0C, non-device encodings |
| text-state ops `Tc` `Tw` `Tz` `TL` `Tr` `Ts` `Td` `TD` `Tm` `T*` | ✅ | state persists across text objects; `Tm`/`Tlm` reinitialized by `BT` |
| `extract_text` (§9.10) | ✅ | one `CharInfo` per character code (1- or 2-byte per font), content order incl. Form XObjects; `/ToUnicode` CMap (1- or 2-byte codespace, `bfchar`, `bfrange` sequential + array, UTF-16BE/surrogates/multi-scalar) → WinAnsi glyph-name→AGL→Unicode fallback for simple fonts / `Unresolved` for Identity-H composites (no guessing); `/Span` `/ActualText` and `/OC` membership → `Error::Unsupported`; headless — no rasterizing, clip masks, or image decoding; non-embedded Type1 fonts extract (metrics-only: `/Widths`+encoding, `bounds`→`None`) even though render refuses; user-space coords (`/Rotate` not applied); budgets: `max_text_codes`, `max_extracted_unicode_bytes` |
| `Do` | ✅ | Form XObject (§8.10): gs saved, `/Matrix` concatenated, clipped to `/BBox`, form content runs under its own `/Resources`, gs restored; nested forms and recursion detection bounded by `max_xobject_depth`/`max_xobject_invocations`. Image XObject (§8.9): `/DeviceGray`/`/DeviceRGB` 8-bit only, raw/`FlateDecode`/`DCTDecode`, `/Decode`, `/Interpolate` (nearest-neighbour — spec permits ignoring interpolation). `/ImageMask`, `/Mask`, `/SMask`, `/SMaskInData` ≠ 0, `/OC`, `/Group`, `/Ref`, non-8-bit, non-device colour spaces → `Error::Unsupported` |
| `sh` | ❌ | `Error::Unsupported` — shading not rendered |
| `gs` | ✅ | `/ExtGState` applied (§8.4.5): `LW`/`LC`/`LJ`/`ML`/`D` map onto the same line fields as `w`/`J`/`j`/`M`/`d`; `ca`/`CA` multiply the paint-time alpha; `BM` maps all 16 standard blend modes to tiny-skia (`/Normal`,`/Compatible`→SourceOver); `RI`, overprint `OP`/`op`/`OPM`, `SA`, `AIS`, `TK`, `SMask` `/None`, `TR`/`TR2`/`BG`/`UCR` `/Default`, `HT`/`FL`/`SM` accepted. Real `/SMask` (luminosity/alpha), non-`/Default` transfer functions, `/Font` via gs, and unknown keys → `Error::Unsupported` |
| `BMC` `BDC` `EMC` `MP` `DP` | ✅ | plain marked content — metadata only (§10.5); `/Span` property lists carrying `/ActualText` (§14.9.4) are refused in render and extraction |
| `/OC` `/OCMD` marked content | ❌ | `Error::Unsupported` — OCG/OCMD visibility evaluation is not implemented; only `/OC` tags whose property list is an OCG/OCMD dict count (spec 8.11.2); `/OCMD` without a membership dict is plain marked content; unresolvable `/Properties` names error |
| `BX` `EX` `Unknown` | ✅ | §7.8.2 compatibility — ignored |
| page annotations (`/Annots`) | ❌ | `Error::Unsupported` — page is refused, not drawn without them |
| `/UserUnit` ≠ 1 | ❌ | `Error::Unsupported` |
| `MediaBox`/`CropBox`/`Rotate` | ✅ | `CropBox` defaults to `MediaBox`; Rotate 0/90/180/270; non-zero origins; `/Rotate` is clockwise on display |
| DPI scaling | ✅ | any positive finite dpi; pixel dims = pts × dpi/72 |

### Test fixtures (font provenance)

| File | Provenance | Licence |
|---|---|---|
| `KasaneTest.ttf` | 5-glyph synthetic font drawn via fontTools by `make_fixtures.py` — no real typeface data | none (generated) |
| `KasaneCIDTest.ttf` | 6-glyph synthetic font (`make_cid_font.py`) — .notdef/box/tri/bar/circle/hole at non-identity GIDs | none (generated) |
| `KasaneJP.ttf` | 17-glyph subset of **Noto Sans JP** (`NotoSansJP-VF.ttf`, notofonts/noto-cjk `Sans2.004`) via `make_jp_font.py`; TrueType `glyf`, UPM=1000, renamed to avoid the OFL Reserved Font Name 'Source' | SIL Open Font License 1.1 — © 2014-2021 Adobe; subsetting/redistribution permitted |
| `type1_cff.bin` | 800-byte raw CFF program — a real `/FontFile3` `/Type1C` subset (SourceSansPro-Bold, charset A–f) extracted from a corpus PDF | Adobe Source Sans Pro — SIL OFL 1.1 |

### Colour conversion notes

- `DeviceCMYK` → RGB uses the naive subtractive form
  `1 − min(1, c + k)` (§10.4.3 note). It is **not** colour-managed;
  ICC profile conversion is out of scope.
- Colour values are clamped to [0,1] for output encoding; out-of-range
  input saturates rather than erroring (the spec clamps at the device).
