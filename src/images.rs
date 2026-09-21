//! Clipboard images are normalized to PNG and persisted with each user message.
use crate::llm::ImageAttachment;

pub const MAX_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_PIXELS: usize = 16 * 1024 * 1024;

pub fn from_rgba(width: usize, height: usize, bytes: &[u8]) -> Result<ImageAttachment, String> {
    use base64::Engine;
    use image::ImageEncoder;
    let pixels = width
        .checked_mul(height)
        .filter(|n| *n > 0 && *n <= MAX_PIXELS)
        .ok_or("Image must contain between 1 and 16 million pixels")?;
    if pixels.checked_mul(4) != Some(bytes.len()) {
        return Err("Invalid RGBA image buffer".into());
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            bytes,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    if png.len() > MAX_IMAGE_BYTES {
        return Err("Image exceeds the 5 MiB PNG limit".into());
    }
    Ok(ImageAttachment {
        data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ),
        width: width as u32,
        height: height as u32,
    })
}

/// Validate public input before it enters history or the work journal.
pub fn validate(images: &[ImageAttachment]) -> Result<(), String> {
    use base64::Engine;
    if images.len() > MAX_IMAGES {
        return Err("At most 4 images can be sent per message".into());
    }
    for image in images {
        let encoded = image
            .data_url
            .strip_prefix("data:image/png;base64,")
            .ok_or("Only inline PNG images are supported")?;
        if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("Image exceeds the 5 MiB PNG limit".into());
        }
        let pixels = u64::from(image.width) * u64::from(image.height);
        if pixels == 0 || pixels > MAX_PIXELS as u64 {
            return Err("Image must contain between 1 and 16 million pixels".into());
        }
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| e.to_string())?;
        if png.len() > MAX_IMAGE_BYTES {
            return Err("Image exceeds the 5 MiB PNG limit".into());
        }
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(image.width);
        limits.max_image_height = Some(image.height);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode().map_err(|e| e.to_string())?;
        if (decoded.width(), decoded.height()) != (image.width, image.height) {
            return Err("Image dimensions do not match its PNG data".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn clipboard_pixels_become_a_self_contained_png() {
        let image = from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        let png = base64::engine::general_purpose::STANDARD
            .decode(
                image
                    .data_url
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(pixels.into_raw(), [255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn image_validation_rejects_corruption_oversize_and_mismatched_metadata() {
        let image = from_rgba(1, 1, &[1, 2, 3, 255]).unwrap();
        validate(std::slice::from_ref(&image)).unwrap();
        let mut invalid = image.clone();
        invalid.width = 2;
        assert!(validate(&[invalid]).is_err());
        let mut invalid = image.clone();
        invalid.data_url = "data:image/png;base64,AQID".into();
        assert!(validate(&[invalid]).is_err());
        let mut invalid = image.clone();
        invalid.data_url = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4)
        );
        assert!(validate(&[invalid]).is_err());
        assert!(validate(&vec![image; MAX_IMAGES + 1]).is_err());
    }

    #[test]
    fn invalid_and_excessive_clipboard_dimensions_are_rejected() {
        for (w, h, bytes) in [
            (0, 1, &[][..]),
            (1, 1, &[][..]),
            (usize::MAX, 2, &[][..]),
            (MAX_PIXELS + 1, 1, &[][..]),
        ] {
            assert!(from_rgba(w, h, bytes).is_err());
        }
    }
}
