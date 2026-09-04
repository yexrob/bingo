//! What a served page is: one path for the page, one for its answer.
//!
//! Pure. The socket reads the request, this decides what the request is for,
//! and [`serve`](crate::serve) is the only place the two meet.

use crate::request::Head;
use crate::script;
use crate::token::Token;

/// What lives under a page's own path.
const ANSWER: &str = "answer";

/// Where the browser is sent. `127.0.0.1` rather than `localhost`: that is what
/// was bound, and a name a resolver answers with `::1` first would reach
/// nothing.
pub fn url(port: u16, token: &Token) -> String {
    format!("http://127.0.0.1:{port}/{}", token.as_str())
}

/// The document a `GET` is answered with: the caller's HTML with the script in
/// it, under a doctype and a title when the caller wrote no document of its own.
pub fn document(title: &str, html: &str) -> String {
    let page = script::inject(html);
    match html.to_ascii_lowercase().contains("<html") {
        true => page,
        false => format!(
            "<!doctype html>\n<meta charset=\"utf-8\">\n<title>{}</title>\n{page}",
            escaped(title)
        ),
    }
}

/// What one request is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `GET /<token>` — the page itself.
    Page,
    /// `POST /<token>/answer` — what the page has to say.
    Answer,
    /// Anything else, including the right path with the wrong token.
    Missing,
}

pub fn route(head: &Head, token: &Token) -> Route {
    let Some(under) = under_token(head.path(), token) else {
        return Route::Missing;
    };
    match (head.method.as_str(), under) {
        ("GET", "") => Route::Page,
        ("POST", ANSWER) => Route::Answer,
        _ => Route::Missing,
    }
}

/// The path with the token taken off the front, or nothing at all when the
/// token is not what was asked for.
fn under_token<'a>(path: &'a str, token: &Token) -> Option<&'a str> {
    let path = path.strip_prefix('/')?;
    let (offered, under) = match path.split_once('/') {
        Some((offered, under)) => (offered, under),
        None => (path, ""),
    };
    token.matches(offered).then_some(under)
}

/// A title is text, not markup: the model wrote it, and a `<` in it must not
/// open a tag.
fn escaped(title: &str) -> String {
    title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> Token {
        Token::from_raw("tok")
    }

    fn head(method: &str, target: &str) -> Head {
        Head {
            method: method.into(),
            target: target.into(),
            content_length: 0,
        }
    }

    #[test]
    fn the_url_is_the_bound_port_and_the_token() {
        assert_eq!(url(41234, &token()), "http://127.0.0.1:41234/tok");
    }

    #[test]
    fn the_page_and_its_answer_are_the_only_two_paths() {
        assert_eq!(route(&head("GET", "/tok"), &token()), Route::Page);
        assert_eq!(route(&head("GET", "/tok/"), &token()), Route::Page);
        assert_eq!(route(&head("GET", "/tok?again=1"), &token()), Route::Page);
        assert_eq!(route(&head("POST", "/tok/answer"), &token()), Route::Answer);
    }

    /// The token is the whole authority, so everything that is not it — a
    /// neighbour's guess, a favicon, the answer path without the token, the
    /// page's own path posted to — is the same 404.
    #[test]
    fn everything_else_is_missing() {
        for (method, target) in [
            ("GET", "/"),
            ("GET", "/other"),
            ("GET", "/tok/answer"),
            ("GET", "/favicon.ico"),
            ("GET", "/tok/deeper/still"),
            ("POST", "/tok"),
            ("POST", "/answer"),
            ("POST", "/other/answer"),
            ("POST", "/tok/answer/more"),
            ("GET", "tok"),
        ] {
            assert_eq!(
                route(&head(method, target), &token()),
                Route::Missing,
                "{method} {target}"
            );
        }
    }

    #[test]
    fn a_document_the_caller_wrote_is_served_as_it_wrote_it() {
        let page = document("Three layouts", "<html><body><p>pick</p></body></html>");
        assert!(page.starts_with("<html>"), "{page}");
        assert!(!page.contains("Three layouts"), "{page}");
        assert_eq!(page.matches("window.bingo").count(), 1, "{page}");
    }

    #[test]
    fn a_fragment_gets_a_doctype_a_charset_and_the_title_it_was_given() {
        let page = document("三个方案", "<p>pick</p>");
        assert!(page.starts_with("<!doctype html>\n"), "{page}");
        assert!(page.contains("<meta charset=\"utf-8\">"), "{page}");
        assert!(page.contains("<title>三个方案</title>"), "{page}");
        assert_eq!(page.matches("window.bingo").count(), 1, "{page}");
    }

    #[test]
    fn a_title_is_text_and_cannot_open_a_tag() {
        let page = document("<script>x</script> & co", "<p>pick</p>");
        assert!(
            page.contains("<title>&lt;script&gt;x&lt;/script&gt; &amp; co</title>"),
            "{page}"
        );
        assert_eq!(page.matches("<script>").count(), 1, "{page}");
    }
}
