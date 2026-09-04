//! The picture behind a URL.
//!
//! A `Content-Type` of `image/…` is a claim; the bytes are the evidence. The
//! library crate reads them (ADR-0041 §1): a type the provider table accepts
//! is kept exactly as it was served, a wider one is decoded and sent as PNG,
//! and what no decoder recognises is refused rather than passed on as a
//! picture the provider would reject. No decoder lives in this crate.

use bingo_pictures::PictureError;
use bingo_sdk::{ContentPart, Image, ToolOutput};

/// The picture a body holds, in a type a provider accepts. It is bounded
/// twice — by the fetch's own cap on the way in, and by the journal's cap
/// inside `sniffed` — and the error says which of the two refused it.
pub(crate) fn seen(bytes: &[u8]) -> Result<Image, PictureError> {
    bingo_pictures::sniffed(bytes)
}

/// What one picture reaches the model as: the picture and no words beside it.
/// It is the part the person's surface draws in the transcript too, so the two
/// of them are looking at the same thing.
pub(crate) fn output(image: Image) -> ToolOutput {
    ToolOutput {
        parts: vec![ContentPart::Image(image)],
        is_error: false,
        display: None,
    }
}
