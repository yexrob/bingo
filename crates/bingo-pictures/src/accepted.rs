//! The one type a provider accepts.
//!
//! The journal keeps a picture in a type every provider takes (ADR-0041 §2):
//! ADR-0040's four, and nothing wider. A person's disk and a chat server hold
//! more than that — a screenshot is a BMP on Windows, a scan is a TIFF — so
//! the widening happens here, at the edge, once, and what nothing reads is
//! refused rather than guessed at.
//!
//! Both doors answer with [`crate::bounded`], so what the journal keeps is
//! the picture a model is sent and not a byte more (ADR-0062 §2): a type in
//! the table, inside the box and under the budget, is kept exactly as it
//! came; everything else is decoded once and encoded down the ladder.

use base64::Engine;
use bingo_sdk::Image;

use crate::{PictureError, bounded};

/// Bytes nobody named: the format is read off the bytes themselves. An
/// extension and a `Content-Type` are both hearsay — the first is a name a
/// person typed, the second a header a server wrote — and a picture the
/// journal cannot replay is worse than one it never took.
pub fn sniffed(bytes: &[u8]) -> Result<Image, PictureError> {
    let media_type = image::guess_format(bytes)
        .map_err(|_| PictureError::NotAPicture)?
        .to_mime_type();
    bounded(media_type, bytes)
}

/// A picture already in the [`Image`] shape, whose sender named its type — a
/// stream-json `image` block (ADR-0040 §4). One the table takes that is
/// already inside the bound arrives back byte for byte; a larger one is the
/// bounded rendering of it, still at the file it named (ADR-0062 §3).
pub fn accepted(image: Image) -> Result<Image, PictureError> {
    let seen = bounded(&image.media_type, &payload(&image.data)?)?;
    Ok(match image.path {
        Some(path) => seen.at(path),
        None => seen,
    })
}

/// The bytes behind the base64 a sender handed over.
fn payload(data: &str) -> Result<Vec<u8>, PictureError> {
    Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
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
            path: None,
        };
        let image = accepted(wider).expect("a picture");
        assert_eq!(image.media_type, "image/png");
    }

    /// The bound, through the door bytes nobody named come in by: a picture
    /// heavier than the wire carries is the rendering of it, not the file.
    #[test]
    fn a_picture_over_the_budget_is_sniffed_into_the_bound() {
        let bytes = crate::testing::noise(600, 600);
        assert!(bytes.len() > crate::MODEL_BUDGET, "{} bytes", bytes.len());
        let image = sniffed(&bytes).expect("a picture");
        assert_eq!(image.media_type, "image/jpeg");
        assert!(image.decoded_len() <= crate::MODEL_BUDGET);
    }

    /// And through the door a sender's own `Image` comes in by — where the
    /// file it named survives the rendering (ADR-0062 §3).
    #[test]
    fn a_handed_over_picture_over_the_budget_is_bounded_and_keeps_its_file() {
        let file = std::path::Path::new("shot.png");
        let handed = Image::from_bytes("image/png", &crate::testing::noise(600, 600))
            .expect("within the cap")
            .at(file);
        let image = accepted(handed).expect("a picture");
        assert_eq!(image.media_type, "image/jpeg");
        assert!(image.decoded_len() <= crate::MODEL_BUDGET);
        assert_eq!(image.path.as_deref(), Some(file));
    }

    #[test]
    fn a_handed_over_picture_nothing_reads_is_refused() {
        let wider = Image {
            media_type: "image/heic".into(),
            data: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"not a picture",
            ),
            path: None,
        };
        assert!(matches!(accepted(wider), Err(PictureError::Undecodable(_))));
    }
}
