//! The one type a provider accepts.
//!
//! The journal keeps a picture in a type every provider takes (ADR-0041 §2):
//! ADR-0040's four, and nothing wider. A person's disk and a chat server hold
//! more than that — a screenshot is a BMP on Windows, a scan is a TIFF — so
//! the widening happens here, at the edge, once: a type in the table is kept
//! as it came, anything else a decoder reads is decoded and sent as PNG, and
//! what nothing reads is refused rather than guessed at.

use bingo_sdk::Image;

use crate::{PictureError, encode};

/// What a widened picture is sent as.
const PNG: &str = "image/png";

/// Bytes nobody named: the format is read off the bytes themselves. An
/// extension and a `Content-Type` are both hearsay — the first is a name a
/// person typed, the second a header a server wrote — and a picture the
/// journal cannot replay is worse than one it never took.
pub fn sniffed(bytes: &[u8]) -> Result<Image, PictureError> {
    let media_type = image::guess_format(bytes)
        .map_err(|_| PictureError::NotAPicture)?
        .to_mime_type();
    match Image::is_known(media_type) {
        true => Ok(Image::from_bytes(media_type, bytes)?),
        false => as_png(&encode(bytes)?.bytes),
    }
}

/// A picture already in the [`Image`] shape, whose sender named its type — a
/// stream-json `image` block (ADR-0040 §4). One the table takes is handed
/// back exactly as it arrived, base64 and all: a host's own bytes are not
/// re-encoded on their way through.
pub fn accepted(image: Image) -> Result<Image, PictureError> {
    match Image::is_known(&image.media_type) {
        true => Ok(image),
        false => as_png(&crate::to_png(&image)?.bytes),
    }
}

/// A decoded picture as the `Image` the journal keeps — capped like any
/// other, because a small TIFF can be a large PNG.
fn as_png(bytes: &[u8]) -> Result<Image, PictureError> {
    Ok(Image::from_bytes(PNG, bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png_size;
    use crate::testing::{ImageFormat, drawn, handed};

    #[test]
    fn a_type_the_table_takes_is_sniffed_and_kept_as_it_came() {
        for (format, media_type) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::Gif, "image/gif"),
            (ImageFormat::WebP, "image/webp"),
        ] {
            let bytes = drawn(4, 4, format);
            let image = sniffed(&bytes).expect("a picture");
            assert_eq!(image.media_type, media_type);
            assert_eq!(
                image,
                Image::from_bytes(media_type, &bytes).expect("within the cap"),
                "the bytes are the ones handed over"
            );
        }
    }

    #[test]
    fn a_wider_type_is_decoded_and_sent_as_png() {
        for format in [ImageFormat::Bmp, ImageFormat::Tiff, ImageFormat::Qoi] {
            let image = sniffed(&drawn(6, 3, format)).expect("a picture");
            assert_eq!(image.media_type, "image/png", "{format:?}");
            let bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &image.data)
                    .expect("base64");
            assert_eq!(png_size(&bytes), Some((6, 3)), "{format:?}");
        }
    }

    /// A name is not evidence: a `.png` full of prose, a web page fetched
    /// because a URL ended in `.jpg`.
    #[test]
    fn bytes_no_decoder_recognises_are_not_a_picture() {
        assert!(matches!(
            sniffed(b"<!doctype html><html>not a picture</html>"),
            Err(PictureError::NotAPicture)
        ));
        assert!(matches!(sniffed(b""), Err(PictureError::NotAPicture)));
    }

    #[test]
    fn a_handed_over_picture_the_table_takes_passes_through_untouched() {
        let image = handed(3, 3, ImageFormat::Jpeg);
        assert_eq!(accepted(image.clone()).expect("a picture"), image);
    }

    #[test]
    fn a_handed_over_picture_of_a_wider_type_becomes_png() {
        let bytes = drawn(5, 2, ImageFormat::Bmp);
        let wider = Image {
            media_type: "image/bmp".into(),
            data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
        };
        let image = accepted(wider).expect("a picture");
        assert_eq!(image.media_type, "image/png");
    }

    #[test]
    fn a_handed_over_picture_nothing_reads_is_refused() {
        let wider = Image {
            media_type: "image/heic".into(),
            data: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"not a picture",
            ),
        };
        assert!(matches!(accepted(wider), Err(PictureError::Undecodable(_))));
    }
}
