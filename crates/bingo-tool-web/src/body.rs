//! What a response body becomes. HTML becomes its article; text, JSON and the
//! other text-shaped types come back as they are; a picture comes back as the
//! picture itself; a PDF or anything else is refused by name, because bytes the
//! model cannot read are context spent for nothing.

use bingo_sdk::Image;

use crate::picture;
use crate::readable::{self, NotMarkdown};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Unreadable {
    #[error("cannot read a {0} body as text; fetch a page or a text document instead")]
    ContentType(String),
    #[error(transparent)]
    NotMarkdown(#[from] NotMarkdown),
    /// Served as a picture and not one: a page behind the URL, a download that
    /// stopped early, a format no decoder reads.
    #[error(transparent)]
    NotAPicture(#[from] bingo_pictures::PictureError),
}

/// What one body reaches the model as.
#[derive(Debug)]
pub(crate) enum Content {
    Page(String),
    Picture(Image),
}

/// The content a body of this media type reaches the model as.
pub(crate) fn render(content_type: &str, bytes: &[u8], url: &str) -> Result<Content, Unreadable> {
    let media_type = media_type(content_type);
    match classify(&media_type) {
        Some(Kind::Html) => Ok(Content::Page(readable::markdown(&text(bytes), url)?)),
        Some(Kind::Text) => Ok(Content::Page(text(bytes))),
        Some(Kind::Picture) => Ok(Content::Picture(picture::seen(bytes)?)),
        None => Err(Unreadable::ContentType(media_type)),
    }
}

enum Kind {
    Html,
    Text,
    Picture,
}

/// The type without its parameters: `text/html; charset=utf-8` is `text/html`.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase()
}

/// A body read as text. A server that mislabels its encoding costs a character
/// here, never the page.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A server that names no type is far more often serving text than bytes, and a
/// refusal there would cost a page that would have read perfectly well.
fn classify(media_type: &str) -> Option<Kind> {
    match media_type {
        "text/html" | "application/xhtml+xml" => Some(Kind::Html),
        "" => Some(Kind::Text),
        other if other.starts_with("text/") => Some(Kind::Text),
        // Before the picture arm, and that is the whole of it: `image/svg+xml`
        // starts with `image/` and is text no decoder reads.
        other if other.ends_with("+json") || other.ends_with("+xml") => Some(Kind::Text),
        other if other.starts_with("image/") => Some(Kind::Picture),
        "application/json"
        | "application/xml"
        | "application/javascript"
        | "application/x-ndjson"
        | "application/yaml" => Some(Kind::Text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::png_bytes;

    const HTML: &[u8] = b"<h1>Title</h1><p>Body</p>";

    /// The page a body of this type reads as, or the test's failure.
    fn page(content_type: &str, body: &[u8]) -> String {
        match render(content_type, body, "https://example.com/") {
            Ok(Content::Page(text)) => text,
            other => panic!("{content_type}: expected a page, got {other:?}"),
        }
    }

    #[test]
    fn html_reaches_the_model_as_markdown() {
        let out = page("text/html; charset=utf-8", HTML);
        assert!(out.contains("# Title"), "got {out}");
        assert!(!out.contains("<h1>"), "got {out}");
    }

    #[test]
    fn text_and_json_come_back_as_they_are() {
        for content_type in [
            "text/plain",
            "text/markdown; charset=utf-8",
            "application/json",
            "application/vnd.api+json",
            "application/xml",
        ] {
            assert_eq!(page(content_type, b"{\"a\": 1}"), "{\"a\": 1}");
        }
    }

    #[test]
    fn a_body_of_no_stated_type_is_read_as_text() {
        assert_eq!(page("", b"plain words"), "plain words");
    }

    #[test]
    fn a_picture_comes_back_as_the_picture() {
        let bytes = png_bytes(3, 2);
        match render("image/png", &bytes, "https://example.com/shot.png") {
            Ok(Content::Picture(image)) => {
                assert_eq!(image.media_type, "image/png");
                assert_eq!(
                    image,
                    Image::from_bytes("image/png", &bytes).expect("within the cap")
                );
            }
            other => panic!("expected a picture, got {other:?}"),
        }
    }

    /// The header is a claim and the bytes are the evidence, in both
    /// directions: a page served as a PNG is refused, and an SVG — text no
    /// decoder reads — never reaches the decoder at all.
    #[test]
    fn what_is_served_as_a_picture_is_read_as_one_only_if_it_is_one() {
        let error = render(
            "image/png",
            b"<!doctype html><html></html>",
            "https://e.com/",
        )
        .err();
        assert!(
            matches!(&error, Some(Unreadable::NotAPicture(_))),
            "got {error:?}"
        );
        assert_eq!(page("image/svg+xml", b"<svg/>"), "<svg/>");
    }

    #[test]
    fn anything_else_is_refused_by_name() {
        let error = render("application/pdf", b"%PDF-1.7", "https://example.com/").err();
        assert!(
            matches!(&error, Some(Unreadable::ContentType(t)) if t == "application/pdf"),
            "got {error:?}"
        );
    }
}
