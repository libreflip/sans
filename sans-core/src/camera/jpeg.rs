//! JPEG validation at the Camera adapter boundary.

use image::RgbImage;
use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

pub(super) fn decode_camera_jpeg(jpeg: &[u8]) -> Result<RgbImage, String> {
    // image's convenience decoder tolerates truncated entropy data and fills
    // missing rows with gray. Reject the truncation errors the decoder detects.
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let pixels = decoder.decode().map_err(|error| error.to_string())?;
    let (width, height) = decoder
        .dimensions()
        .ok_or("decoded JPEG has no dimensions")?;
    let width = u32::try_from(width).map_err(|error| error.to_string())?;
    let height = u32::try_from(height).map_err(|error| error.to_string())?;
    RgbImage::from_raw(width, height, pixels)
        .ok_or_else(|| "decoded JPEG pixel length does not match its dimensions".into())
}

#[cfg(test)]
mod tests {
    use image::{codecs::jpeg::JpegEncoder, Rgb, RgbImage};

    use super::decode_camera_jpeg;

    fn jpeg() -> Vec<u8> {
        let source = RgbImage::from_fn(256, 256, |x, y| {
            Rgb([(x % 200) as u8, (y % 200) as u8, 220])
        });
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&source)
            .unwrap();
        jpeg
    }

    #[test]
    fn complete_camera_jpeg_decodes_to_rgb() {
        let decoded = decode_camera_jpeg(&jpeg()).unwrap();
        assert_eq!(decoded.dimensions(), (256, 256));
        assert!(decoded.get_pixel(32, 32)[2] > 200);
    }

    #[test]
    fn truncated_camera_jpeg_is_rejected_instead_of_filled_with_gray() {
        let jpeg = jpeg();
        let truncated = &jpeg[..jpeg.len() / 2];
        assert!(decode_camera_jpeg(truncated).is_err());
    }
}
