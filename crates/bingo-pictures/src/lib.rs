//! A picture as pixels.
//!
//! A terminal that draws pictures takes one format (PNG), a provider takes
//! four (ADR-0040's table: png, jpeg, gif, webp), and a person hands over
//! whatever their camera, their screenshot key, a chat server or a tool gave
//! them. This crate is the one place the wider becomes the narrower, and
//! nothing above it knows a decoder.
//!
//! [`to_png`] answers with the bytes a terminal may draw and the size they
//! draw at. [`sniffed`] and [`accepted`] answer with the one [`Image`] the
//! journal keeps. [`load`] reads a [`Source`] — a path on this machine or a
//! URL this machine fetches (ADR-0041 §3) — and hands the bytes to the first
//! two.
//!
//! A PNG passes through untouched — its size is in its header, so nothing is
//! decoded and nothing is re-encoded. Everything else is decoded once and
//! written back out as PNG.

use base64::Engine;
use bingo_sdk::Image;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

mod accepted;
mod load;
mod source;

pub use accepted::{accepted, sniffed};
pub use load::load;
pub use source::{Source, names_a_picture};

/// A picture in the one format a terminal takes, and the size it draws at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Png {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// The eight bytes every PNG starts with.
const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// The header chunk: its length and type follow the signature, and its first
/// two words are the size.
const IHDR: std::ops::Range<usize> = 12..16;
const WIDTH: std::ops::Range<usize> = 16..20;
const HEIGHT: std::ops::Range<usize> = 20..24;

/// The picture's pixels as PNG. A PNG is already that and is handed back as
/// it came; anything a decoder can read is decoded and written out as one.
///
/// An animation is its first frame: a terminal draws a still, and the first
/// frame is the one a person meant when they pasted it.
pub fn to_png(image: &Image) -> Result<Png, PictureError> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data)?;
    match png_size(&bytes) {
        Some((width, height)) => Ok(Png {
            bytes,
            width,
            height,
        }),
        None => encode(&bytes),
    }
}

/// A PNG's size, read off its header rather than by decoding it: the
/// signature, then the `IHDR` chunk whose first two words are the size.
/// `None` for anything that is not a PNG, which is what sends [`to_png`] to
/// the decoders.
pub fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let head = bytes.get(..HEIGHT.end)?;
    if !head.starts_with(SIGNATURE) || &head[IHDR] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(head[WIDTH].try_into().ok()?);
    let height = u32::from_be_bytes(head[HEIGHT].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Decode whatever this is and write it back out as PNG. The compression is
/// the fast one: these bytes are on their way to a terminal on the same
/// machine, so a second spent squeezing them is a second a person waits.
pub(crate) fn encode(bytes: &[u8]) -> Result<Png, PictureError> {
    let decoded = image::load_from_memory(bytes)?;
    let mut out = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive);
    decoded.write_with_encoder(encoder)?;
    Ok(Png {
        bytes: out,
        width: decoded.width(),
        height: decoded.height(),
    })
}

/// What went wrong with one picture. Its `Display` is what a person is shown
/// — the TUI's notice, `--print`'s stderr, the channel's log line — so each
/// reads as the second half of a sentence whose first half is the source the
/// caller was reading.
#[derive(Debug, thiserror::Error)]
pub enum PictureError {
    #[error("the picture is not base64: {0}")]
    NotBase64(#[from] base64::DecodeError),
    #[error("no decoder read this picture: {0}")]
    Undecodable(#[from] image::ImageError),
    /// The bytes are not a picture at all: a web page behind a URL, a text
    /// file with a picture's name, a download that stopped early.
    #[error("not a picture: no decoder recognises these bytes")]
    NotAPicture,
    #[error("could not be read: {0}")]
    Unreadable(#[from] std::io::Error),
    #[error("could not be fetched: {0}")]
    Unfetchable(reqwest::Error),
    /// The journal's own table and cap (ADR-0040), refusing it in its words.
    #[error(transparent)]
    Refused(#[from] bingo_sdk::ImageError),
}

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests {
    use super::testing::drawn;
    use super::*;
    use image::ImageFormat;

    fn handed(bytes: &[u8], media_type: &str) -> Image {
        Image::from_bytes(media_type, bytes).expect("a picture within the cap")
    }

    #[test]
    fn a_png_passes_through_with_the_size_off_its_header() {
        let bytes = drawn(2, 3, ImageFormat::Png);
        let png = to_png(&handed(&bytes, "image/png")).expect("a picture");
        assert_eq!((png.width, png.height), (2, 3));
        assert_eq!(png.bytes, bytes, "the bytes are the ones handed over");
    }

    #[test]
    fn a_jpeg_is_decoded_and_written_back_out_as_a_png() {
        let bytes = drawn(4, 5, ImageFormat::Jpeg);
        let png = to_png(&handed(&bytes, "image/jpeg")).expect("a picture");
        assert_eq!((png.width, png.height), (4, 5));
        assert_eq!(
            png_size(&png.bytes),
            Some((4, 5)),
            "and what comes back is a PNG"
        );
    }

    #[test]
    fn a_gif_is_its_first_frame() {
        let bytes = drawn(6, 2, ImageFormat::Gif);
        let png = to_png(&handed(&bytes, "image/gif")).expect("a picture");
        assert_eq!((png.width, png.height), (6, 2));
        assert_eq!(png_size(&png.bytes), Some((6, 2)));
    }

    #[test]
    fn a_payload_no_decoder_reads_is_an_error_and_not_a_guess() {
        let image = handed(b"not a picture at all", "image/png");
        assert!(matches!(to_png(&image), Err(PictureError::Undecodable(_))));
    }

    #[test]
    fn a_payload_that_is_not_base64_says_so() {
        let image = Image {
            media_type: "image/png".into(),
            data: "!!!!not base64!!!!".into(),
        };
        assert!(matches!(to_png(&image), Err(PictureError::NotBase64(_))));
    }

    /// The header read, and the three ways it is not a PNG at all.
    #[test]
    fn the_size_comes_off_the_header_and_nothing_else_answers() {
        assert_eq!(png_size(&drawn(7, 11, ImageFormat::Png)), Some((7, 11)));
        assert_eq!(
            png_size(b"\x89PNG\r\n\x1a\n"),
            None,
            "the header is cut off"
        );
        assert_eq!(png_size(&drawn(2, 2, ImageFormat::Jpeg)), None);
        let mut broken = drawn(2, 2, ImageFormat::Png);
        broken[13] = b'x';
        assert_eq!(png_size(&broken), None, "IHDR is not where it must be");
    }
}
