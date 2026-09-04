//! A picture as the pixels behind it.
//!
//! [`crate::to_png`] and [`crate::fitted`] answer with bytes a *terminal*
//! draws. This answers with the samples a *caller* reads: the one place a
//! surface that wants to look at a picture — the opening shot's billboard,
//! which draws her out of characters — gets at its pixels without a decoder
//! of its own.

use crate::PictureError;

/// A decoded picture: four bytes a pixel, row by row from the top left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Pixels {
    /// The pixel at `x`, `y`. Transparent black outside the picture, so a
    /// caller sampling a rectangle need not clamp its own coordinates.
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let start = ((y as usize * self.width as usize) + x as usize) * 4;
        match self.rgba.get(start..start + 4) {
            Some([r, g, b, a]) => [*r, *g, *b, *a],
            _ => [0, 0, 0, 0],
        }
    }
}

/// Whatever a decoder reads, as the pixels behind it.
pub fn pixels(bytes: &[u8]) -> Result<Pixels, PictureError> {
    let picture = image::load_from_memory(bytes)?.to_rgba8();
    Ok(Pixels {
        width: picture.width(),
        height: picture.height(),
        rgba: picture.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::png_bytes;

    #[test]
    fn a_png_decodes_to_four_bytes_a_pixel() {
        let decoded = pixels(&png_bytes(3, 2)).expect("a picture");
        assert_eq!((decoded.width, decoded.height), (3, 2));
        assert_eq!(decoded.rgba.len(), 3 * 2 * 4);
    }

    #[test]
    fn a_sample_outside_the_picture_is_transparent_black() {
        let decoded = pixels(&png_bytes(2, 2)).expect("a picture");
        assert_eq!(decoded.at(0, 0)[3], 0xff, "inside is opaque");
        assert_eq!(decoded.at(2, 0), [0, 0, 0, 0]);
        assert_eq!(decoded.at(0, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn bytes_no_decoder_reads_are_refused() {
        assert!(pixels(b"not a picture at all").is_err());
    }
}
