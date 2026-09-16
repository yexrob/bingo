//! The picture behind a URL.
//!
//! A `Content-Type` of `image/…` is a claim; the bytes are the evidence. The
//! library crate reads them (ADR-0041 §1): a type the provider table accepts
//! that is already inside the bound is kept exactly as it was served, anything
//! wider or heavier is decoded and encoded down to it, and what no decoder
//! recognises is refused rather than passed on as a picture the provider would
//! reject. No decoder lives in this crate.

use bingo_pictures::PictureError;
use bingo_sdk::{ContentPart, Image, ToolOutput};

/// The picture a body holds, in a type a provider accepts and at the size a
/// model is sent (ADR-0062). It is bounded twice — by the fetch's own cap on
/// the way in, and by the box and budget inside the door — and the error says
/// which of the two refused it. The door is the async one: the bound is a
/// decode, and this runs on a thread a session answers on.
pub(crate) async fn seen(bytes: Vec<u8>) -> Result<Image, PictureError> {
    bingo_pictures::seen(bytes).await
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
