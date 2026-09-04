//! Pictures for a test to hand over.
//!
//! A surface's tests need real pictures — a PNG that decodes, a JPEG that has
//! to be converted, a payload nothing reads — and none of them should mean a
//! decoder in that crate's dependencies or a wall of pasted bytes in its
//! source. They are drawn here, by the crate that reads them back.

// This module is a test's scaffolding wherever it is compiled from, and a
// picture this crate cannot write back to itself is a failure of the suite
// rather than of anything a person is running.
#![allow(clippy::expect_used)]

use bingo_sdk::Image;
pub use image::ImageFormat;

/// A picture of `width` by `height`, in `format`.
pub fn drawn(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x * 40) as u8, (y * 40) as u8, 0x40, 0xff])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut bytes, format)
        .expect("a picture this crate can write");
    bytes.into_inner()
}

/// The same, as the [`Image`] a person or a tool would have handed over.
pub fn handed(width: u32, height: u32, format: ImageFormat) -> Image {
    let media_type = match format {
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => "image/png",
    };
    Image::from_bytes(media_type, &drawn(width, height, format)).expect("a picture within the cap")
}

/// The bytes of a PNG of `width` by `height`, for a test that writes one to
/// a file rather than handing it over.
pub fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    drawn(width, height, ImageFormat::Png)
}

/// A PNG of `width` by `height`: the picture most tests want.
pub fn png(width: u32, height: u32) -> Image {
    handed(width, height, ImageFormat::Png)
}

/// A picture whose pixels do not compress, in PNG.
///
/// A gradient is a few kilobytes however large it is, so a test that wants a
/// payload as heavy as a real screenshot — one whose base64 is longer than the
/// head a size is read from, one whose decode is real work — needs pixels no
/// filter can predict. The noise is a hash of the coordinates, so the same
/// picture comes out on every machine and every run.
pub fn noise(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([mixed(x, y, 1), mixed(x, y, 2), mixed(x, y, 3), 0xff])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut bytes, ImageFormat::Png)
        .expect("a picture this crate can write");
    bytes.into_inner()
}

/// The same, as the [`Image`] a tool would have answered with.
pub fn noisy(width: u32, height: u32) -> Image {
    Image::from_bytes("image/png", &noise(width, height)).expect("a picture within the cap")
}

fn mixed(x: u32, y: u32, channel: u32) -> u8 {
    let mut hash = x
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
        .wrapping_add(channel.wrapping_mul(0xc2b2_ae35));
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2545_f491);
    (hash >> 13) as u8
}

/// A picture no decoder reads, under a media type that says it should.
pub fn unreadable() -> Image {
    Image::from_bytes("image/png", b"not a picture at all").expect("a small payload")
}

/// The bytes of a PNG of these pixels, for a test that writes a picture out
/// to look at it.
///
/// The pixels are four bytes each, row by row from the top left, as
/// [`crate::Pixels`] carries them; a run too short for the size is padded
/// with transparent black rather than refused, because a test that miscounts
/// should see the picture it drew and not an error about it.
pub fn png_of(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut rgba = rgba.to_vec();
    rgba.resize(width as usize * height as usize * 4, 0);
    let picture = image::RgbaImage::from_raw(width, height, rgba).expect("pixels for the size");
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut bytes, ImageFormat::Png)
        .expect("a picture this crate can write");
    bytes.into_inner()
}
