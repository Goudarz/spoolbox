//! 180×120 JPEG thumbnails: images larger than the box are scaled to
//! cover it and centre-cropped; smaller ones are centred on white without upscaling.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits, Rgb, RgbImage};

const WIDTH: u32 = 180;
const HEIGHT: u32 = 120;

/// Decoder limits so that a small "decompression bomb" cannot exhaust memory.
fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(10_000);
    l.max_image_height = Some(10_000);
    l.max_alloc = Some(128 * 1024 * 1024);
    l
}

fn decode(data: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits());
    reader.decode().ok()
}

fn encode(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
    if img.write_with_encoder(enc).is_err() {
        return Vec::new();
    }
    out
}

/// White 180×120 JPEG.
pub(crate) fn blank() -> Vec<u8> {
    encode(&RgbImage::from_pixel(WIDTH, HEIGHT, Rgb([255, 255, 255])))
}

pub(crate) fn generate(data: &[u8]) -> Vec<u8> {
    let Some(img) = decode(data) else {
        return blank();
    };
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return blank();
    }
    let mut canvas = RgbImage::from_pixel(WIDTH, HEIGHT, Rgb([255, 255, 255]));
    let rgba = if w > WIDTH || h > HEIGHT {
        let scale = f64::max(
            f64::from(WIDTH) / f64::from(w),
            f64::from(HEIGHT) / f64::from(h),
        );
        let nw = ((f64::from(w) * scale).round() as u32).max(WIDTH);
        let nh = ((f64::from(h) * scale).round() as u32).max(HEIGHT);
        img.resize_exact(nw, nh, FilterType::Lanczos3)
            .crop_imm((nw - WIDTH) / 2, (nh - HEIGHT) / 2, WIDTH, HEIGHT)
            .to_rgba8()
    } else {
        img.to_rgba8()
    };
    let (ox, oy) = ((WIDTH - rgba.width()) / 2, (HEIGHT - rgba.height()) / 2);
    for (x, y, p) in rgba.enumerate_pixels() {
        // Composite transparent pixels over white.
        let a = u32::from(p[3]);
        let mix = |c: u8| ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8;
        canvas.put_pixel(ox + x, oy + y, Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
    }
    encode(&canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dims(jpeg: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(jpeg).expect("jpeg");
        (img.width(), img.height())
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = RgbImage::from_pixel(w, h, Rgb([200, 10, 10]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png");
        out
    }

    #[test]
    fn always_180_by_120() {
        assert_eq!(dims(&blank()), (WIDTH, HEIGHT));
        assert_eq!(dims(&generate(&png(1, 1))), (WIDTH, HEIGHT));
        assert_eq!(dims(&generate(&png(1000, 200))), (WIDTH, HEIGHT));
        assert_eq!(dims(&generate(b"not an image")), (WIDTH, HEIGHT));
    }

    #[test]
    fn oversized_dimensions_are_refused() {
        // A tiny PNG header claiming 100000×100000 pixels must not be decoded.
        let mut img = png(1, 1);
        img[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        img[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(decode(&img).is_none());
    }
}
