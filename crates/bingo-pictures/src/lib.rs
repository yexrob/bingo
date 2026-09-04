//! A picture as pixels.
//!
//! A terminal that draws pictures takes one format (PNG), a provider takes
//! four (ADR-0040's table: png, jpeg, gif, webp), and a person hands over
//! whatever their camera, their screenshot key, a chat server or a tool gave
//! them. This crate is the one place the wider becomes the narrower, and
//! nothing above it knows a decoder.
//!
//! [`to_png`] answers with the bytes a terminal may draw and the size they
//! draw at, and [`fitted`] with the bytes of the rectangle of cells they will
//! be drawn into — which is all a terminal ever shows. [`size`] answers the
//! one question a *frame* has, how many pixels a picture is, **without
//! decoding one**: it is the header's word, so the caller pays nothing for it
//! (M61). [`sniffed`] and [`accepted`] answer with the one [`Image`] the
//! journal keeps. [`load`] reads a [`Source`] — a path on this machine or a
//! URL this machine fetches (ADR-0041 §3), kept on disk by [`cache`] — and
//! hands the bytes to the first two. [`pixels`] answers with the samples
//! themselves, for a caller that draws a picture rather than sends one.
//!
//! A PNG passes through untouched — its size is in its header, so nothing is
//! decoded and nothing is re-encoded. Everything else is decoded once and
//! written back out as PNG.

use base64::Engine;
use bingo_sdk::Image;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};

mod accepted;
pub mod cache;
mod load;
mod pixels;
mod source;

pub use accepted::{accepted, sniffed};
pub use cache::Cache;
pub use load::load;
pub use pixels::{Pixels, pixels};
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

/// How many pixels a picture is, without decoding one.
///
/// A frame has to know a picture's size before it can say how many cells it
/// takes, and it must not pay for a decode to find out: the size is in the
/// picture's own header — a PNG's `IHDR`, a JPEG's `SOF`, a GIF's screen
/// descriptor — so only the [`HEAD`] of the payload is read back from base64
/// and only its header is parsed. `None` where no decoder recognises the bytes
/// at all, which is the `[image: type]` degrade of design §5.
pub fn size(image: &Image) -> Option<(u32, u32)> {
    if let Some(head) = head(&image.data)
        && let Some(size) = measured(&head)
    {
        return Some(size);
    }
    measured(&decoded(&image.data)?)
}

/// How many characters of the base64 the size is looked for in. They carry
/// 48 KiB of the picture, which every header this crate reads sits well
/// inside, and a whole group of four so the prefix decodes on its own.
const HEAD: usize = 64 * 1024;

/// The head of the payload, or `None` where the whole of it is shorter than
/// [`HEAD`] and is read as it is.
fn head(data: &str) -> Option<Vec<u8>> {
    decoded(data.get(..HEAD)?)
}

fn decoded(data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// A picture's size out of its header: a PNG's own read here, and every other
/// format's asked of its own decoder's header reader. Not one pixel is
/// decoded, which is what lets a frame ask this.
fn measured(bytes: &[u8]) -> Option<(u32, u32)> {
    if let Some(size) = png_size(bytes) {
        return Some(size);
    }
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// The picture as PNG at no more than `within` pixels, its shape kept: the
/// bytes of the rectangle of cells it was drawn into.
///
/// The cells are all the terminal will ever show of a picture, so sending a
/// 4000×3000 screenshot for a twelve-row block is megabytes the terminal
/// throws away. A PNG already inside the box is the bytes that came in,
/// untouched — a thumbnail is never blown up to fill its cells, which would
/// cost bytes to look worse — and everything else is decoded once and written
/// back out at the size the cells hold. The whole picture as PNG is never
/// made on the way, because nothing wants it.
///
/// This is the expensive call in this crate: a decode and a resize, hundreds
/// of milliseconds for a screenshot. Nothing may make it on a thread that
/// draws (M61).
pub fn fitted(image: &Image, within: (u32, u32)) -> Result<Png, PictureError> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data)?;
    match png_size(&bytes) {
        Some((width, height)) if inside(width, height, within) => Ok(Png {
            bytes,
            width,
            height,
        }),
        _ => shrunk(&bytes, within),
    }
}

fn inside(width: u32, height: u32, within: (u32, u32)) -> bool {
    width <= within.0 && height <= within.1
}

/// The filter the shrink uses. Triangle over Lanczos3: at thumbnail sizes the
/// two are indistinguishable and Triangle is several times faster (M48
/// Verified has the measurement).
const FILTER: image::imageops::FilterType = image::imageops::FilterType::Triangle;

/// Decode, fit to `within` where it is bigger than that, and write the result
/// back out as PNG. `DynamicImage::resize` keeps the aspect ratio and fills
/// the box it is given, so the shape is kept by the resize itself and not by
/// arithmetic here — and a picture already inside the box is only re-encoded,
/// never blown up.
fn shrunk(bytes: &[u8], within: (u32, u32)) -> Result<Png, PictureError> {
    let picture = image::load_from_memory(bytes)?;
    match inside(picture.width(), picture.height(), within) {
        true => encoded(&picture),
        false => encoded(&picture.resize(within.0.max(1), within.1.max(1), FILTER)),
    }
}

/// Decode whatever this is and write it back out as PNG.
pub(crate) fn encode(bytes: &[u8]) -> Result<Png, PictureError> {
    encoded(&image::load_from_memory(bytes)?)
}

/// One picture as PNG. The compression is the fast one: these bytes are on
/// their way to a terminal on the same machine, so a second spent squeezing
/// them is a second a person waits.
fn encoded(picture: &image::DynamicImage) -> Result<Png, PictureError> {
    let mut out = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive);
    picture.write_with_encoder(encoder)?;
    Ok(Png {
        bytes: out,
        width: picture.width(),
        height: picture.height(),
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

    /// A picture bigger than its box comes back inside it, with its shape.
    #[test]
    fn a_picture_too_big_for_its_box_is_shrunk_to_fit_it() {
        let big = handed(&drawn(400, 300, ImageFormat::Png), "image/png");
        let small = fitted(&big, (40, 40)).expect("pixels");
        assert_eq!((small.width, small.height), (40, 30), "the shape is kept");
        assert_eq!(png_size(&small.bytes), Some((40, 30)), "and it is a PNG");
        let whole = to_png(&big).expect("pixels");
        assert!(
            small.bytes.len() < whole.bytes.len(),
            "{} is not fewer bytes than {}",
            small.bytes.len(),
            whole.bytes.len()
        );
    }

    /// The point of the shrink: what goes to the terminal is the size of the
    /// cells it will cover, not the size of the screenshot.
    #[test]
    fn only_the_pixels_the_cells_show_are_sent() {
        let shot = handed(&drawn(2000, 1500, ImageFormat::Png), "image/png");
        let block = fitted(&shot, (40 * 10, 12 * 20)).expect("pixels");
        assert_eq!((block.width, block.height), (320, 240));
        let thumbnail = fitted(&shot, (12 * 10, 3 * 20)).expect("pixels");
        assert_eq!((thumbnail.width, thumbnail.height), (80, 60));
        assert!(
            thumbnail.bytes.len() < block.bytes.len(),
            "a thumbnail costs less than a block"
        );
    }

    /// Never upsized: a PNG inside its box is the bytes that came in,
    /// untouched.
    #[test]
    fn a_picture_already_inside_its_box_is_handed_back_as_it_came() {
        let bytes = drawn(8, 6, ImageFormat::Png);
        let small = handed(&bytes, "image/png");
        for within in [(8, 6), (400, 300)] {
            let same = fitted(&small, within).expect("pixels");
            assert_eq!((same.width, same.height), (8, 6), "{within:?}");
            assert_eq!(same.bytes, bytes, "{within:?} the very bytes");
        }
    }

    /// And a picture that is not a PNG is not blown up either: it becomes one
    /// at the size it already was.
    #[test]
    fn a_jpeg_inside_its_box_becomes_a_png_of_its_own_size() {
        let small = handed(&drawn(8, 6, ImageFormat::Jpeg), "image/jpeg");
        let same = fitted(&small, (400, 300)).expect("pixels");
        assert_eq!((same.width, same.height), (8, 6));
        assert_eq!(png_size(&same.bytes), Some((8, 6)));
    }

    /// A box of no pixels still leaves a picture: a rectangle of cells is at
    /// least one cell, so a fit to nothing can only be a caller's slip.
    #[test]
    fn a_box_of_nothing_still_leaves_a_pixel() {
        let big = handed(&drawn(40, 30, ImageFormat::Png), "image/png");
        let least = fitted(&big, (0, 0)).expect("pixels");
        assert_eq!((least.width, least.height), (1, 1));
    }

    #[test]
    fn a_payload_no_decoder_reads_has_no_size_and_will_not_fit() {
        let broken = handed(b"not a picture at all", "image/png");
        assert_eq!(size(&broken), None);
        assert!(fitted(&broken, (10, 10)).is_err());
    }

    /// The frame's own question, on every format this crate reads: the size
    /// comes back and nothing is decoded to find it.
    #[test]
    fn a_pictures_size_is_read_off_its_header_whatever_it_is() {
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Gif,
            ImageFormat::WebP,
            ImageFormat::Bmp,
            ImageFormat::Tiff,
        ] {
            let image = handed(&drawn(9, 4, format), "image/png");
            assert_eq!(size(&image), Some((9, 4)), "{format:?}");
        }
    }

    /// A payload longer than the head that is read: the size is still its
    /// own, because a PNG says it in its first two dozen bytes.
    #[test]
    fn a_picture_larger_than_the_head_is_measured_from_its_head() {
        let big = super::testing::noisy(700, 500);
        assert!(big.data.len() > HEAD, "{} characters", big.data.len());
        assert_eq!(size(&big), Some((700, 500)));
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
