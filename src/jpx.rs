//! `/JPXDecode` (ISO 32000-1 §7.4.9, ISO/IEC 15444-1) through the
//! pure-Rust `hayro-jpeg2000` crate: raw codestreams (`FF 4F FF 51`)
//! and JP2 containers, any bit depth (scaled to 8 bits by the
//! decoder), palettes resolved, channel definitions applied.
//!
//! PDF semantics layered on top (§7.4.9, Table 1 note on JPX):
//! * `/Width`/`/Height` must equal the codestream's size;
//! * `/BitsPerComponent` is ignored — the decoder delivers 8 bits;
//! * `/ColorSpace`, when present, overrides the codestream's and must
//!   have the codestream's channel count; when absent the codestream's
//!   enumerated space (Gray / sRGB / CMYK) is used, an ICC profile
//!   falls back to the device space with the same channel count and is
//!   reported as an approximation;
//! * `/Decode` is ignored (the default range applies);
//! * an alpha channel is used only when `/SMaskInData` is 1 (straight)
//!   or 2 (premultiplied — undone); with 0 it is dropped.
//!
//! Safety: the crate is `#![forbid(unsafe_code)]`; the dimensions are
//! checked against `max_image_dimension` / `max_image_total_samples` /
//! `max_image_bytes` *before* decoding, and the decoded buffer length
//! is verified against them afterwards.

use crate::error::Error;
use crate::limits::Limits;

/// The colour interpretation the codestream declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JpxSpace {
    Gray,
    Rgb,
    Cmyk,
    /// An embedded ICC profile with `n` channels (not evaluated).
    Icc(u8),
    /// No usable declaration; `n` channels.
    Unknown(u8),
}

/// A decoded JPX image: 8-bit colour samples, interleaved per pixel,
/// plus the alpha plane when the codestream carries one.
pub(crate) struct JpxImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) ncomps: usize,
    pub(crate) space: JpxSpace,
    pub(crate) samples: Vec<u8>,
    pub(crate) alpha: Option<Vec<u8>>,
}

pub(crate) fn decode(data: &[u8], limits: &Limits) -> Result<JpxImage, Error> {
    let settings = hayro_jpeg2000::DecodeSettings::default();
    let image = hayro_jpeg2000::Image::new(data, &settings)
        .map_err(|e| Error::Parse(format!("JPXDecode: {e:?}")))?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(Error::Structure("JPXDecode: empty image".into()));
    }
    if width > limits.max_image_dimension || height > limits.max_image_dimension {
        return Err(Error::LimitExceeded {
            limit: "max_image_dimension",
            max: limits.max_image_dimension as u64,
        });
    }
    let samples = width as u64 * height as u64;
    if samples > limits.max_image_total_samples {
        return Err(Error::LimitExceeded {
            limit: "max_image_total_samples",
            max: limits.max_image_total_samples,
        });
    }
    let space = match image.color_space() {
        hayro_jpeg2000::ColorSpace::Gray => JpxSpace::Gray,
        hayro_jpeg2000::ColorSpace::RGB => JpxSpace::Rgb,
        hayro_jpeg2000::ColorSpace::CMYK => JpxSpace::Cmyk,
        hayro_jpeg2000::ColorSpace::Icc { num_channels, .. } => JpxSpace::Icc(*num_channels),
        hayro_jpeg2000::ColorSpace::Unknown { num_channels } => JpxSpace::Unknown(*num_channels),
    };
    let ncomps = image.color_space().num_channels() as usize;
    let has_alpha = image.has_alpha();
    let channels = ncomps + usize::from(has_alpha);
    if ncomps == 0 || samples.saturating_mul(channels as u64) > limits.max_image_bytes {
        return Err(Error::LimitExceeded {
            limit: "max_image_bytes",
            max: limits.max_image_bytes,
        });
    }
    let mut ctx = hayro_jpeg2000::DecoderContext::default();
    let decoded = image
        .decode(&mut ctx)
        .map_err(|e| Error::Parse(format!("JPXDecode: {e:?}")))?;
    let buf = decoded.data_u8();
    let expect = samples as usize * channels;
    if buf.len() != expect {
        return Err(Error::Parse(format!(
            "JPXDecode: decoded {} bytes for {width}x{height}x{channels}, expected {expect}",
            buf.len()
        )));
    }
    let (samples_out, alpha) = if has_alpha {
        let mut colour = Vec::with_capacity(samples as usize * ncomps);
        let mut alpha = Vec::with_capacity(samples as usize);
        for px in buf.chunks_exact(channels) {
            colour.extend_from_slice(&px[..ncomps]);
            alpha.push(px[ncomps]);
        }
        (colour, Some(alpha))
    } else {
        (buf, None)
    };
    Ok(JpxImage {
        width,
        height,
        ncomps,
        space,
        samples: samples_out,
        alpha,
    })
}
