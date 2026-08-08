//! Pre-send preparation of image attachments: decode → scale down to the API
//! limit → encode as PNG/JPEG (size target aligned with Claude Code:
//! 2000×2000 and ~3.75MB of raw bytes, ≤ 5MB API hard limit after base64).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::io::Cursor;

/// Maximum pixels per side (oversized images are scaled down proportionally).
pub const IMAGE_MAX_DIMENSION: u32 = 2000;
/// Target raw-byte ceiling (≈ 5MB API hard limit after base64).
pub const IMAGE_TARGET_RAW_SIZE: usize = 3 * 1024 * 1024 + 768 * 1024;
/// Decode ceiling: oversized files are rejected outright instead of wasting
/// decode time.
const MAX_DECODE_BYTES: usize = 32 * 1024 * 1024;

/// A sendable image: encoding format + base64 data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    pub media_type: String,
    pub data: String,
}

/// Image placeholder reference (`#[image N]` → the Nth attachment, 1-based).
static MARKER_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"#\[image (\d+)\]").expect("static regex"));

/// Image placeholder text: `#[image N]`.
pub fn marker(id: usize) -> String {
    format!("#[image {id}]")
}

/// Whether the text references an attachment at all.
pub fn has_marker(text: &str) -> bool {
    MARKER_RE.is_match(text)
}

/// Session-scoped attachment table. Images the user mounts on the input box live here as
/// base64; the message text carries only `#[image N]` markers, so the model has a stable
/// handle it can repeat — including when handing work to a subagent, which is why the table
/// belongs to the session rather than to the input box.
#[derive(Default)]
pub struct Attachments {
    inner: std::sync::Mutex<Vec<crate::api::types::ImageAttachment>>,
}

impl Attachments {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<crate::api::types::ImageAttachment>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Raw bytes → compressed attachment; returns the 1-based placeholder id (None = not an image).
    pub fn register(&self, bytes: &[u8]) -> Option<usize> {
        let prepared = prepare_image(bytes)?;
        let mut inner = self.lock();
        inner.push(crate::api::types::ImageAttachment {
            media_type: prepared.media_type,
            data: prepared.data,
        });
        Some(inner.len())
    }

    /// Markers in `text` → attachments, deduplicated, in order of first appearance.
    /// Unknown ids are ignored: a stale marker degrades to plain text, never to an error.
    pub fn resolve(&self, text: &str) -> Vec<crate::api::types::ImageAttachment> {
        let inner = self.lock();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for cap in MARKER_RE.captures_iter(text) {
            if let Ok(n) = cap[1].parse::<usize>()
                && n >= 1
                && n <= inner.len()
                && seen.insert(n)
            {
                out.push(inner[n - 1].clone());
            }
        }
        out
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Nth attachment, 1-based (mirrors the placeholder numbering).
    #[cfg(test)]
    pub fn get(&self, id: usize) -> Option<crate::api::types::ImageAttachment> {
        self.lock().get(id.checked_sub(1)?).cloned()
    }
}

/// Decode / scale / encode down to an API-acceptable size. Returns `None` on
/// failure (not an image, or the compression never reaches the limit).
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

    // PNG first (keeps alpha); if the size still exceeds the target, degrade
    // through JPEG quality levels.
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

    /// Oversized images are scaled down to within 2000px (proportionally).
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
