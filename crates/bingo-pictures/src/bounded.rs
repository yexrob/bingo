//! The picture a model is sent.
//!
//! A provider bills a picture at roughly one token per 750 pixels and throws
//! away everything past its own long edge, so bytes above that buy a model
//! nothing and cost a person a slow upload. This module is the one bound
//! (ADR-0062): a picture inside [`MODEL_BOX`] and under [`MODEL_BUDGET`] is
//! the bytes it came as, and anything larger is decoded once and walked down
//! a ladder — PNG first, so flat colour keeps its text crisp, then JPEG at
//! falling qualities, then smaller — until one encoding fits.

use bingo_sdk::Image;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, Rgba, RgbaImage};

use crate::{PictureError, inside, measured};

/// The pixels a picture a model is sent fits inside. Above every provider's
/// own long edge (Anthropic 1568 or 2576, OpenAI 2048) the pixels are
/// discarded server-side, so this is the last size that is still the picture.
pub const MODEL_BOX: (u32, u32) = (2000, 2000);

/// The encoded bytes a picture a model is sent stays under.
pub const MODEL_BUDGET: usize = 1_000_000;

/// How many times the ladder shrinks before it says the picture will not fit.
/// Three quarters eight times over is a tenth of each side, which no picture
/// worth sending survives anyway; the count is here so the ladder ends.
const SHRINKS: u32 = 8;

/// The JPEG qualities the ladder walks. 85 is where a photograph stops
/// changing to an eye and a model sees nothing so coarse; below 45 the
/// picture is worse than the answer it would buy.
const QUALITIES: &[u8] = &[85, 75, 65, 55, 45];

/// The filter a shrink uses. Lanczos3 rather than the Triangle a thumbnail
/// is fitted with: this is the picture the model reads, not one a person
/// glances at, and the sharpening is worth the milliseconds here.
const FILTER: image::imageops::FilterType = image::imageops::FilterType::Lanczos3;

const PNG: &str = "image/png";
const JPEG: &str = "image/jpeg";

/// The picture as a model is sent it: inside [`MODEL_BOX`] and under
/// [`MODEL_BUDGET`], and byte-identical to `bytes` when it already was.
///
/// This is the expensive call in this crate for a picture that does not fit —
/// a decode, a Lanczos3 resize and up to six encodings. Nothing may make it
/// on a thread that draws or a thread a session answers on (M61).
pub fn bounded(media_type: &str, bytes: &[u8]) -> Result<Image, PictureError> {
    bounded_within(media_type, bytes, MODEL_BOX, MODEL_BUDGET)
}

/// The same, against a box and a budget a test can make small enough to walk
/// the whole ladder in milliseconds.
pub(crate) fn bounded_within(
    media_type: &str,
    bytes: &[u8],
    within: (u32, u32),
    budget: usize,
) -> Result<Image, PictureError> {
    if untouched(media_type, bytes, within, budget) {
        return Ok(Image::from_bytes(media_type, bytes)?);
    }
    let picture = image::load_from_memory(bytes)?;
    ladder(fitted_to_box(picture, within), budget)
}

/// Whether the bytes are already the picture a model is sent: a type the
/// journal keeps, a size inside the box — read off the header, so a picture
/// that fits costs no decode — and a length under the budget.
fn untouched(media_type: &str, bytes: &[u8], within: (u32, u32), budget: usize) -> bool {
    Image::is_known(media_type)
        && bytes.len() <= budget
        && measured(bytes).is_some_and(|(width, height)| inside(width, height, within))
}

/// The picture at no more than `within`, its shape kept by the resize itself.
/// One already inside the box is not blown up: pixels invented here would
/// cost bytes to look worse.
fn fitted_to_box(picture: DynamicImage, within: (u32, u32)) -> DynamicImage {
    match inside(picture.width(), picture.height(), within) {
        true => picture,
        false => picture.resize(within.0.max(1), within.1.max(1), FILTER),
    }
}

/// The first encoding under budget, the picture three quarters the size and
/// tried again when none is, at most [`SHRINKS`] times over.
fn ladder(mut picture: DynamicImage, budget: usize) -> Result<Image, PictureError> {
    let mut least = usize::MAX;
    for _ in 0..=SHRINKS {
        match rung(&picture, budget)? {
            Rung::Under(image) => return Ok(image),
            Rung::Over(bytes) => least = least.min(bytes),
        }
        picture = three_quarters(&picture);
    }
    Err(PictureError::TooBig { bytes: least })
}

/// One size of the picture: the encoding that fit, or the fewest bytes this
/// size could be made into.
enum Rung {
    Under(Image),
    Over(usize),
}

/// The encodings of one size, in the order they are tried. PNG first and at
/// every size: a screenshot of flat colour is smaller as PNG than as JPEG and
/// keeps its small text, and only a picture PNG cannot fit goes lossy.
fn rung(picture: &DynamicImage, budget: usize) -> Result<Rung, PictureError> {
    let png = as_png(picture)?;
    match png.len() <= budget {
        true => Ok(Rung::Under(Image::from_bytes(PNG, &png)?)),
        false => jpegs(picture, budget, png.len()),
    }
}

/// The lossy half of one rung: falling qualities over the picture flattened
/// on white, the first under budget winning. `least` is what the PNG of this
/// size came to, because that may still be the smallest this size can be.
fn jpegs(picture: &DynamicImage, budget: usize, least: usize) -> Result<Rung, PictureError> {
    let flat = flattened(picture);
    let mut least = least;
    for quality in QUALITIES {
        let jpeg = as_jpeg(&flat, *quality)?;
        if jpeg.len() <= budget {
            return Ok(Rung::Under(Image::from_bytes(JPEG, &jpeg)?));
        }
        least = least.min(jpeg.len());
    }
    Ok(Rung::Over(least))
}

/// The picture as PNG at the default compression, not the fast one a
/// terminal's pixels are written with: these bytes cross a network a person
/// waits on, so the moment spent squeezing them is paid back.
fn as_png(picture: &DynamicImage) -> Result<Vec<u8>, PictureError> {
    let mut out = Vec::new();
    picture.write_with_encoder(PngEncoder::new(&mut out))?;
    Ok(out)
}

fn as_jpeg(picture: &DynamicImage, quality: u8) -> Result<Vec<u8>, PictureError> {
    let mut out = Vec::new();
    picture.write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))?;
    Ok(out)
}

/// The picture over white, without an alpha channel. JPEG carries none, and
/// what a transparent pixel means is decided by whoever shows the picture —
/// a chat, a document, a terminal all put white behind it, and black would
/// turn a diagram on a clear ground into a photograph of a hole.
fn flattened(picture: &DynamicImage) -> DynamicImage {
    let mut ground = RgbaImage::from_pixel(picture.width(), picture.height(), Rgba([0xff; 4]));
    image::imageops::overlay(&mut ground, picture, 0, 0);
    DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(ground).to_rgb8())
}

/// The next rung down, and never nothing: a picture of no pixels is not a
/// picture, so a side that has run out stays at one. The division comes
/// first, so no width a decoder can hand over overflows the multiply.
fn three_quarters(picture: &DynamicImage) -> DynamicImage {
    let width = (picture.width() / 4 * 3).max(1);
    let height = (picture.height() / 4 * 3).max(1);
    picture.resize(width, height, FILTER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png_size;
    use crate::testing::{ImageFormat, drawn};

    /// The ladder walked whole is a decode, a Lanczos3 resize and up to six
    /// encodings of every size: seconds on a 12 MP picture in a debug build,
    /// and a test that pins seconds pins the machine it was written on. So a
    /// small box and a small budget stand in for the constants, and the code
    /// under them is the very same.
    const BUDGET: usize = 600_000;

    fn decoded(image: &Image) -> Vec<u8> {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &image.data)
            .expect("base64")
    }

    /// A picture whose pixels no filter predicts, so its PNG weighs what a
    /// photograph's does. A seeded LCG rather than a crate: the same picture
    /// comes out on every machine and the tree gains nothing. `clear` is how
    /// many columns from the left are fully transparent.
    fn noise(width: u32, height: u32, clear: u32) -> DynamicImage {
        let mut seed: u32 = 0x1234_5678;
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
        for _ in 0..height {
            for x in 0..width {
                for _ in 0..3 {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    // The low bits of an LCG barely move; bits 16..24 do.
                    pixels.push((seed >> 16) as u8);
                }
                pixels.push(if x < clear { 0x00 } else { 0xff });
            }
        }
        let picture = RgbaImage::from_raw(width, height, pixels).expect("pixels for the size");
        DynamicImage::ImageRgba8(picture)
    }

    fn noise_png(width: u32, height: u32, clear: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        noise(width, height, clear)
            .write_to(&mut out, ImageFormat::Png)
            .expect("a picture this crate can write");
        out.into_inner()
    }

    /// The whole point of the untouched rung: a screenshot a model can read
    /// as it is reaches the journal as the file's own bytes, so nothing is
    /// softened and nothing is re-compressed.
    #[test]
    fn a_picture_inside_the_box_and_under_the_budget_is_the_bytes_it_came_as() {
        for (format, media_type) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::Gif, "image/gif"),
            (ImageFormat::WebP, "image/webp"),
        ] {
            let bytes = drawn(40, 30, format);
            let image = bounded(media_type, &bytes).expect("a picture");
            assert_eq!(image.media_type, media_type, "{format:?}");
            assert_eq!(decoded(&image), bytes, "{format:?} the very bytes");
            assert_eq!(image.path, None, "the caller says where it is");
        }
    }

    /// Bigger than the box: the shape is the resize's own, not arithmetic
    /// here, and a 4:3 picture stays 4:3.
    #[test]
    fn a_picture_larger_than_the_box_is_fitted_to_it_with_its_shape_kept() {
        let bytes = drawn(400, 300, ImageFormat::Png);
        let image =
            bounded_within("image/png", &bytes, (200, 200), MODEL_BUDGET).expect("a picture");
        assert_eq!(image.media_type, "image/png", "flat colour stays PNG");
        assert_eq!(png_size(&decoded(&image)), Some((200, 150)));
    }

    /// Pixels no PNG can squeeze under the budget: the ladder goes lossy
    /// rather than refusing a picture a model could have read.
    #[test]
    fn a_picture_no_png_fits_comes_back_jpeg_under_the_budget() {
        let bytes = noise_png(600, 600, 0);
        assert!(bytes.len() > BUDGET, "{} bytes of noise", bytes.len());
        let image = bounded_within("image/png", &bytes, (2000, 2000), BUDGET).expect("a picture");
        assert_eq!(image.media_type, "image/jpeg");
        assert!(decoded(&image).len() <= BUDGET, "{}", decoded(&image).len());
        assert_eq!(
            crate::measured(&decoded(&image)),
            Some((600, 600)),
            "a quality was enough; no size was lost"
        );
    }

    /// A budget no quality reaches at this size: the picture is shrunk and the
    /// ladder walked again, so what comes back is smaller than the box.
    #[test]
    fn a_budget_no_quality_reaches_shrinks_the_picture_and_tries_again() {
        let bytes = noise_png(600, 600, 0);
        let image = bounded_within("image/png", &bytes, (2000, 2000), 20_000).expect("a picture");
        let out = decoded(&image);
        assert!(out.len() <= 20_000, "{} bytes", out.len());
        let (width, height) = crate::measured(&out).expect("a size");
        assert!(width < 600 && height < 600, "{width}x{height}");
        assert_eq!(width, height, "and still square");
    }

    /// JPEG carries no alpha, so a clear ground is made white rather than
    /// black: a diagram on nothing must not come back as a photograph of a
    /// hole.
    #[test]
    fn a_picture_with_alpha_comes_back_flattened_on_white() {
        let bytes = noise_png(600, 600, 300);
        let image = bounded_within("image/png", &bytes, (2000, 2000), BUDGET).expect("a picture");
        assert_eq!(image.media_type, "image/jpeg");
        let picture = image::load_from_memory(&decoded(&image)).expect("a picture");
        assert!(!picture.color().has_alpha(), "{:?}", picture.color());
        let ground = picture.to_rgb8();
        let image::Rgb([r, g, b]) = *ground.get_pixel(150, 300);
        assert!(
            r > 200 && g > 200 && b > 200,
            "the clear half is white, not black: {r},{g},{b}"
        );
    }

    /// A type the journal does not keep is decoded whatever its size, because
    /// the table is what a provider takes and nothing else may reach it.
    #[test]
    fn a_type_the_table_refuses_is_re_encoded_even_inside_the_box() {
        let image = bounded("image/bmp", &drawn(6, 3, ImageFormat::Bmp)).expect("a picture");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(png_size(&decoded(&image)), Some((6, 3)));
    }

    /// The bound is idempotent: what came back once comes back again as the
    /// very same bytes, so a picture read twice is never softened twice.
    #[test]
    fn a_bounded_picture_fed_back_is_untouched() {
        let bytes = noise_png(600, 600, 0);
        let once = bounded_within("image/png", &bytes, (2000, 2000), BUDGET).expect("a picture");
        let twice = bounded_within(&once.media_type, &decoded(&once), (2000, 2000), BUDGET)
            .expect("a picture");
        assert_eq!(twice, once);
    }

    /// A budget no encoding of any size reaches: the ladder ends rather than
    /// shrinking forever, and says how close it got.
    #[test]
    fn the_ladder_gives_up_when_even_the_last_shrink_is_over_budget() {
        let bytes = noise_png(64, 64, 0);
        let error = bounded_within("image/png", &bytes, (2000, 2000), 32).expect_err("no picture");
        assert!(
            matches!(error, PictureError::TooBig { bytes } if bytes > 32),
            "{error}"
        );
    }

    /// Bytes a media type calls a picture and no decoder reads are refused
    /// here, not passed on for a provider to refuse.
    #[test]
    fn bytes_no_decoder_reads_are_not_bounded_into_a_picture() {
        let error = bounded("image/png", b"not a picture at all").expect_err("no picture");
        assert!(matches!(error, PictureError::Undecodable(_)), "{error}");
    }
}
