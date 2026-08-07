//! 图片附件发送前的准备：解码 → 缩放到 API 上限内 → PNG/JPEG 编码
//! （体积目标对齐 Claude Code：2000×2000 与 ~3.75MB 原始字节，
//! base64 后 ≤ 5MB API 硬限）。

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use std::io::Cursor;

/// 单边最大像素（超限等比缩小）。
pub const IMAGE_MAX_DIMENSION: u32 = 2000;
/// 目标原始字节上限（base64 后 ≈ 5MB API 硬限）。
pub const IMAGE_TARGET_RAW_SIZE: usize = 3 * 1024 * 1024 + 768 * 1024;
/// 解码上限：超大文件直接拒绝，不浪费解码时间。
const MAX_DECODE_BYTES: usize = 32 * 1024 * 1024;

/// 可发送的图片：编码格式 + base64 数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    pub media_type: String,
    pub data: String,
}

/// 解码/缩放/编码为 API 可接受的体积。失败（非图片、超限压缩不达）返回 None。
pub fn prepare_image(bytes: &[u8]) -> Option<PreparedImage> {
    if bytes.is_empty() || bytes.len() > MAX_DECODE_BYTES {
        return None;
    }
    let mut img = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    if w > IMAGE_MAX_DIMENSION || h > IMAGE_MAX_DIMENSION {
        let scale = (IMAGE_MAX_DIMENSION as f64 / w.max(h) as f64).min(1.0);
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        img = img.resize(nw, nh, image::imageops::FilterType::Triangle);
    }

    // PNG 优先（保留 alpha）；体积仍超目标则逐级 JPEG 降质。
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    if png.len() <= IMAGE_TARGET_RAW_SIZE {
        return Some(PreparedImage {
            media_type: "image/png".into(),
            data: BASE64.encode(&png),
        });
    }
    let rgb = img.to_rgb8();
    for quality in [80u8, 60, 40, 20] {
        let mut buf = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        encoder.encode_image(&rgb).ok()?;
        if buf.len() <= IMAGE_TARGET_RAW_SIZE {
            return Some(PreparedImage {
                media_type: "image/jpeg".into(),
                data: BASE64.encode(&buf),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200u8, 30, 30, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn small_png_stays_png_and_under_limit() {
        let out = prepare_image(&png_bytes(64, 48)).unwrap();
        assert_eq!(out.media_type, "image/png");
        assert!(BASE64.decode(&out.data).unwrap().len() <= IMAGE_TARGET_RAW_SIZE);
    }

    /// 超尺寸缩小到 2000px 内（等比）。
    #[test]
    fn oversize_image_is_downscaled() {
        let out = prepare_image(&png_bytes(4000, 2000)).unwrap();
        let decoded = BASE64.decode(&out.data).unwrap();
        let img = image::ImageReader::new(Cursor::new(&decoded))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(img.width(), 2000);
        assert_eq!(img.height(), 1000, "等比缩放");
    }

    #[test]
    fn garbage_returns_none() {
        assert_eq!(prepare_image(b"not an image"), None);
        assert_eq!(prepare_image(b""), None);
    }
}
