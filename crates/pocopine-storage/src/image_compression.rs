use fast_image_resize::images::Image;
use fast_image_resize::{PixelType, ResizeOptions, Resizer};
use jpeg_encoder::{ColorType, Encoder};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

use crate::{
    ImageVariantFormat, ImageVariantResize, ImageVariantSpec, StorageError, StorageResult,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageSourceInfo {
    pub width: u32,
    pub height: u32,
    pub content_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedImageVariant {
    pub name: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub content_type: String,
}

struct DecodedImage {
    info: ImageSourceInfo,
    pixels: Vec<u8>,
}

pub fn inspect_image(bytes: &[u8]) -> StorageResult<ImageSourceInfo> {
    Ok(decode_image(bytes)?.info)
}

pub fn compress_image_variant(
    source_name: &str,
    bytes: &[u8],
    variant: &ImageVariantSpec,
) -> StorageResult<CompressedImageVariant> {
    let (_, mut variants) = compress_image_variants(source_name, bytes, [variant.clone()])?;
    variants
        .pop()
        .ok_or_else(|| StorageError::client("image compression produced no variant"))
}

pub fn compress_image_variants(
    source_name: &str,
    bytes: &[u8],
    variants: impl IntoIterator<Item = ImageVariantSpec>,
) -> StorageResult<(ImageSourceInfo, Vec<CompressedImageVariant>)> {
    let decoded = decode_image(bytes)?;
    let mut compressed = Vec::new();
    for variant in variants {
        variant.validate()?;
        compressed.push(compress_decoded_image(source_name, &decoded, &variant)?);
    }
    Ok((decoded.info, compressed))
}

fn decode_image(bytes: &[u8]) -> StorageResult<DecodedImage> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return decode_jpeg(bytes);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return decode_png(bytes);
    }
    Err(StorageError::unsupported(
        "image compression currently supports JPEG and PNG inputs",
    ))
}

fn decode_jpeg(bytes: &[u8]) -> StorageResult<DecodedImage> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let pixels = decoder
        .decode()
        .map_err(|err| StorageError::client(format!("decode JPEG image: {err}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| StorageError::client("JPEG decoder omitted image info"))?;
    let width = u32::from(info.width);
    let height = u32::from(info.height);
    validate_rgb_len(width, height, pixels.len())?;
    Ok(DecodedImage {
        info: ImageSourceInfo {
            width,
            height,
            content_type: "image/jpeg".to_string(),
        },
        pixels,
    })
}

fn decode_png(bytes: &[u8]) -> StorageResult<DecodedImage> {
    let options = DecoderOptions::default().png_set_strip_to_8bit(true);
    let mut decoder = PngDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|err| StorageError::client(format!("decode PNG headers: {err}")))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| StorageError::client("PNG decoder omitted dimensions"))?;
    let colorspace = decoder
        .colorspace()
        .ok_or_else(|| StorageError::client("PNG decoder omitted colorspace"))?;
    let decoded = decoder
        .decode()
        .map_err(|err| StorageError::client(format!("decode PNG image: {err}")))?;
    let samples = decoded
        .u8()
        .ok_or_else(|| StorageError::unsupported("PNG decoder returned non-8-bit samples"))?;
    let width =
        u32::try_from(width).map_err(|_| StorageError::unsupported("PNG width is too large"))?;
    let height =
        u32::try_from(height).map_err(|_| StorageError::unsupported("PNG height is too large"))?;
    let pixels = png_to_rgb(samples, colorspace)?;
    validate_rgb_len(width, height, pixels.len())?;
    Ok(DecodedImage {
        info: ImageSourceInfo {
            width,
            height,
            content_type: "image/png".to_string(),
        },
        pixels,
    })
}

fn png_to_rgb(samples: Vec<u8>, colorspace: ColorSpace) -> StorageResult<Vec<u8>> {
    match colorspace {
        ColorSpace::RGB => Ok(samples),
        ColorSpace::Luma => {
            let mut out = Vec::with_capacity(samples.len() * 3);
            for luma in samples {
                out.extend_from_slice(&[luma, luma, luma]);
            }
            Ok(out)
        }
        ColorSpace::RGBA => {
            let mut out = Vec::with_capacity(samples.len() / 4 * 3);
            for pixel in samples.chunks_exact(4) {
                push_matted_rgb(&mut out, pixel[0], pixel[1], pixel[2], pixel[3]);
            }
            Ok(out)
        }
        ColorSpace::LumaA => {
            let mut out = Vec::with_capacity(samples.len() / 2 * 3);
            for pixel in samples.chunks_exact(2) {
                push_matted_rgb(&mut out, pixel[0], pixel[0], pixel[0], pixel[1]);
            }
            Ok(out)
        }
        ColorSpace::BGR => {
            let mut out = Vec::with_capacity(samples.len());
            for pixel in samples.chunks_exact(3) {
                out.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            Ok(out)
        }
        ColorSpace::BGRA => {
            let mut out = Vec::with_capacity(samples.len() / 4 * 3);
            for pixel in samples.chunks_exact(4) {
                push_matted_rgb(&mut out, pixel[2], pixel[1], pixel[0], pixel[3]);
            }
            Ok(out)
        }
        ColorSpace::ARGB => {
            let mut out = Vec::with_capacity(samples.len() / 4 * 3);
            for pixel in samples.chunks_exact(4) {
                push_matted_rgb(&mut out, pixel[1], pixel[2], pixel[3], pixel[0]);
            }
            Ok(out)
        }
        other => Err(StorageError::unsupported(format!(
            "PNG colorspace is not supported for JPEG variants: {other:?}"
        ))),
    }
}

fn push_matted_rgb(out: &mut Vec<u8>, r: u8, g: u8, b: u8, alpha: u8) {
    out.push(matte_over_white(r, alpha));
    out.push(matte_over_white(g, alpha));
    out.push(matte_over_white(b, alpha));
}

fn matte_over_white(channel: u8, alpha: u8) -> u8 {
    let alpha = u16::from(alpha);
    let channel = u16::from(channel);
    ((channel * alpha + 255 * (255 - alpha) + 127) / 255) as u8
}

fn compress_decoded_image(
    source_name: &str,
    decoded: &DecodedImage,
    variant: &ImageVariantSpec,
) -> StorageResult<CompressedImageVariant> {
    let ImageVariantFormat::Jpeg = variant.format;
    let (width, height, resize_options) =
        variant_dimensions(decoded.info.width, decoded.info.height, variant.resize)?;
    ensure_jpeg_dimensions(width, height)?;

    let src = Image::from_vec_u8(
        decoded.info.width,
        decoded.info.height,
        decoded.pixels.clone(),
        PixelType::U8x3,
    )
    .map_err(|err| StorageError::client(format!("prepare source image: {err}")))?;
    let mut dst = Image::new(width, height, PixelType::U8x3);
    let mut resizer = Resizer::new();
    resizer
        .resize(&src, &mut dst, Some(&resize_options))
        .map_err(|err| StorageError::client(format!("resize image variant: {err}")))?;

    let mut bytes = Vec::new();
    Encoder::new(&mut bytes, variant.quality)
        .encode(dst.buffer(), width as u16, height as u16, ColorType::Rgb)
        .map_err(|err| StorageError::client(format!("encode JPEG variant: {err}")))?;

    Ok(CompressedImageVariant {
        name: variant.name.clone(),
        file_name: variant.output_file_name(source_name),
        bytes,
        width,
        height,
        content_type: variant.format.content_type().to_string(),
    })
}

fn variant_dimensions(
    source_width: u32,
    source_height: u32,
    resize: ImageVariantResize,
) -> StorageResult<(u32, u32, ResizeOptions)> {
    if source_width == 0 || source_height == 0 {
        return Err(StorageError::client("image dimensions must be non-zero"));
    }
    match resize {
        ImageVariantResize::Fit {
            max_width,
            max_height,
        } => {
            resize.validate()?;
            if source_width <= max_width && source_height <= max_height {
                return Ok((source_width, source_height, ResizeOptions::new()));
            }
            let source_width = u64::from(source_width);
            let source_height = u64::from(source_height);
            let max_width = u64::from(max_width);
            let max_height = u64::from(max_height);
            let (width, height) = if source_width * max_height > source_height * max_width {
                (
                    max_width,
                    ((source_height * max_width) + source_width / 2) / source_width,
                )
            } else {
                (
                    ((source_width * max_height) + source_height / 2) / source_height,
                    max_height,
                )
            };
            Ok((
                u32::try_from(width.max(1))
                    .map_err(|_| StorageError::unsupported("image variant width is too large"))?,
                u32::try_from(height.max(1))
                    .map_err(|_| StorageError::unsupported("image variant height is too large"))?,
                ResizeOptions::new(),
            ))
        }
        ImageVariantResize::Cover { width, height } => {
            resize.validate()?;
            Ok((
                width,
                height,
                ResizeOptions::new().fit_into_destination(Some((0.5, 0.5))),
            ))
        }
    }
}

fn validate_rgb_len(width: u32, height: u32, len: usize) -> StorageResult<()> {
    let expected = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| StorageError::unsupported("image dimensions are too large"))?;
    if len != expected {
        return Err(StorageError::client(format!(
            "decoded image length mismatch: expected {expected} bytes, got {len}"
        )));
    }
    Ok(())
}

fn ensure_jpeg_dimensions(width: u32, height: u32) -> StorageResult<()> {
    if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
        return Err(StorageError::unsupported(
            "JPEG variants cannot exceed 65535 pixels on either axis",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_fixture(width: u16, height: u16) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
        for y in 0..height {
            for x in 0..width {
                pixels.push((x % 251) as u8);
                pixels.push((y % 251) as u8);
                pixels.push(((x + y) % 251) as u8);
            }
        }

        let mut bytes = Vec::new();
        Encoder::new(&mut bytes, 92)
            .encode(&pixels, width, height, ColorType::Rgb)
            .unwrap();
        bytes
    }

    #[test]
    fn fit_variant_downscales_without_upscaling() {
        let image = jpeg_fixture(80, 40);
        let small = ImageVariantSpec::fit("small", 20, 20);
        let variant = compress_image_variant("avatar.jpeg", &image, &small).unwrap();
        assert_eq!(variant.width, 20);
        assert_eq!(variant.height, 10);
        assert_eq!(variant.file_name, "avatar-small.jpg");

        let large = ImageVariantSpec::fit("large", 200, 200);
        let variant = compress_image_variant("avatar.jpeg", &image, &large).unwrap();
        assert_eq!(variant.width, 80);
        assert_eq!(variant.height, 40);
    }

    #[test]
    fn cover_variant_uses_exact_target_dimensions() {
        let image = jpeg_fixture(80, 40);
        let variant = compress_image_variant(
            "avatar.jpeg",
            &image,
            &ImageVariantSpec::cover("square", 24, 24).quality(76),
        )
        .unwrap();

        assert_eq!(variant.width, 24);
        assert_eq!(variant.height, 24);
        assert_eq!(variant.content_type, "image/jpeg");
        assert!(!variant.bytes.is_empty());
    }
}
