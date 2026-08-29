//! What a response body becomes. HTML becomes its article; text, JSON and the
//! other text-shaped types come back as they are; a PDF or an image is refused
//! by name, because bytes the model cannot read are context spent for nothing.

use crate::readable::{self, NotMarkdown};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Unreadable {
    #[error("cannot read a {0} body as text; fetch a page or a text document instead")]
    ContentType(String),
    #[error(transparent)]
    NotMarkdown(#[from] NotMarkdown),
}

/// The markdown a body of this media type reaches the model as.
pub(crate) fn render(content_type: &str, body: &str, url: &str) -> Result<String, Unreadable> {
    match classify(&media_type(content_type)) {
        Some(Kind::Html) => Ok(readable::markdown(body, url)?),
        Some(Kind::Text) => Ok(body.to_string()),
        None => Err(Unreadable::ContentType(media_type(content_type))),
    }
}

enum Kind {
    Html,
    Text,
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

/// A server that names no type is far more often serving text than bytes, and a
/// refusal there would cost a page that would have read perfectly well.
fn classify(media_type: &str) -> Option<Kind> {
    match media_type {
        "text/html" | "application/xhtml+xml" => Some(Kind::Html),
        "" => Some(Kind::Text),
        other if other.starts_with("text/") => Some(Kind::Text),
        other if other.ends_with("+json") || other.ends_with("+xml") => Some(Kind::Text),
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

    fn render_type(content_type: &str) -> Result<String, Unreadable> {
        render(
            content_type,
            "<h1>Title</h1><p>Body</p>",
            "https://example.com/",
        )
    }

    #[test]
    fn html_reaches_the_model_as_markdown() {
        let out = render_type("text/html; charset=utf-8").expect("markdown");
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
            let out = render(content_type, "{\"a\": 1}", "https://example.com/")
                .unwrap_or_else(|e| panic!("{content_type}: {e}"));
            assert_eq!(out, "{\"a\": 1}");
        }
    }

    #[test]
    fn a_body_of_no_stated_type_is_read_as_text() {
        assert_eq!(
            render("", "plain words", "https://example.com/").ok(),
            Some("plain words".to_string())
        );
    }

    #[test]
    fn anything_else_is_refused_by_name() {
        let error = render_type("application/pdf").err();
        assert!(
            matches!(&error, Some(Unreadable::ContentType(t)) if t == "application/pdf"),
            "got {error:?}"
        );
        assert!(matches!(
            render_type("image/png; q=1").err(),
            Some(Unreadable::ContentType(_))
        ));
    }
}
