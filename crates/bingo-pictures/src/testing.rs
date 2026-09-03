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

/// A picture no decoder reads, under a media type that says it should.
pub fn unreadable() -> Image {
    Image::from_bytes("image/png", b"not a picture at all").expect("a small payload")
}
