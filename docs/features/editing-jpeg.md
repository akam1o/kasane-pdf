# JPEG (`DCTDecode`) image region editing

## JPEG image region editing (M27, inside `remove_image_region` /
`remove_region` / `remove_regions` / `remove_regions_and_document_metadata`)

**Experimental content editing — not redaction.** A `/DCTDecode`
Image XObject is now *editable* when it is a single-filter
**sequential-DCT** JPEG that decodes to 8-bit DeviceGray or
DeviceRGB samples (baseline SOF0 and extended-sequential SOF1 both
satisfy `DctSequential`; differential and arithmetic-coded frames
are refused by the decoder itself):

- **Accepted**: `/Filter /DCTDecode` or `[/DCTDecode]`; the JPEG
  must be **sequential DCT** (`DctSequential` — baseline or
  extended-sequential) and produce `L8` or `RGB24` output
  (jpeg-decoder's pixel formats); `Width`/`Height`/`ColorSpace` in
  the dict must match the JPEG's own header. `/DecodeParms
  /ColorTransform` may be `0` or `1` — but per spec (ISO 32000-1
  §7.4.8) an Adobe APP14 marker in the JPEG *overrides* the PDF
  value, and `0` is refused on chroma-subsampled JPEGs because the
  decoder cannot emit raw subsampled planes as interleaved pixels.
  Every other `DecodeParms` key is refused.
- **Refused**: progressive (`DctProgressive`), lossless, CMYK/YCCK/
  16-bit JPEGs; differential or arithmetic-coded sequential frames
  (refused by the decoder itself as `Unsupported`); any filter chain
  (`[ASCII85Decode DCTDecode]` etc.); `/JPXDecode`; corrupt or
  truncated JPEG data (`Parse`, distinct from `Unsupported`);
  `/ColorTransform 0` on a subsampled JPEG; dict↔JPEG geometry or
  component-count mismatch; and every prior refusal (masks,
  `/Interpolate`, non-default `/Decode`, non-grid-aligned CTM,
  forms, annotations).
- **Decode + edit**: the JPEG is decoded to raw samples, the
  rect-union blackening runs on the decoded buffer exactly as for
  raw/Flate images, and the clone is written **uncompressed**
  (no JPEG re-encode — a `/Filter` key is never emitted on the
  clone; `Width`/`Height`/`ColorSpace`/`BitsPerComponent` match the
  decoded samples). EXIF and other JPEG-app metadata are *not*
  copied to the clone — but the same information may remain
  elsewhere in the file.
- **Budgets**: `max_image_dimension`, `max_image_total_samples`,
  and `max_image_bytes` gate the dict-declared geometry *before*
  `read_info` runs; the decoder's own `max_decoding_buffer_size`
  is set to `max_image_bytes` so a hostile JPEG cannot allocate
  past the cap. `max_edit_content_bytes` covers the decoded clone
  bytes cumulatively. An image whose rects miss it is skipped
  before any decode — no budget is consumed.
- **Preservation guarantee**: "unaltered" means *the decoded
  sample values outside the selected rects are byte-identical*
  in the clone. It does **not** promise identical rendering to
  the original JPEG — a decoder, colour-transform, or interpolation
  difference is inherent to re-encoding JPEG content.

All four entry points share the same `validate_image` /
`decode_image` / `image_pixel_ranges` helpers in
`src/remove_image.rs`; no JPEG logic is duplicated.
