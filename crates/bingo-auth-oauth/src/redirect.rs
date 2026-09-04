//! The redirect this flow waits for: one port, one request, one page.
//!
//! The socket is `bingo_loopback`'s (ADR-0042 §1). What is this crate's is the
//! ports the issuer's allow-list names, the `state` check, and what a browser
//! is left looking at.

use bingo_loopback::{Loopback, Response};

use crate::callback;
use crate::error::AuthError;

/// codex's own callback port; twenty above it is enough for a second attempt
/// while a first one is still bound.
const FIRST_PORT: u16 = 1455;
const PORTS: u16 = 21;

/// A port for the redirect to land on.
pub async fn bind() -> Result<Loopback, AuthError> {
    Ok(Loopback::in_range(FIRST_PORT, PORTS).await?)
}

/// `localhost` rather than `127.0.0.1`: the issuer's allow-list is written
/// with the name.
pub fn uri(port: u16) -> String {
    format!("http://localhost:{port}{}", callback::PATH)
}

/// Accept the redirect and answer it, whatever it turns out to be: a browser
/// left staring at a dead socket tells a person nothing.
pub async fn receive(loopback: Loopback, expected_state: &str) -> Result<String, AuthError> {
    loop {
        let mut connection = loopback.accept().await?;
        let outcome = match connection.request().await {
            // A browser opens sockets it never sends on; the redirect is still
            // coming.
            Ok(None) => continue,
            Ok(Some(request)) => code(&request.head.target, expected_state),
            Err(error) => Err(error.into()),
        };
        connection.reply(&page(&outcome)).await;
        return outcome;
    }
}

fn code(target: &str, expected_state: &str) -> Result<String, AuthError> {
    let callback = callback::parse(target)?;
    if callback.state != expected_state {
        return Err(AuthError::Invalid(
            "the callback state does not match".into(),
        ));
    }
    Ok(callback.code)
}

fn page(outcome: &Result<String, AuthError>) -> Response {
    match outcome {
        Ok(_) => Response::html(
            "200 OK",
            document("<h1>Signed in.</h1><p>You can close this tab.</p>"),
        ),
        Err(_) => Response::html(
            "400 Bad Request",
            document("<h1>Sign-in failed.</h1><p>Return to the terminal.</p>"),
        ),
    }
}

fn document(body: &str) -> String {
    format!("<!doctype html><meta charset=\"utf-8\"><title>bingo</title>{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real port, hit by a real client: the parser has its own unit tests,
    /// so what is proved here is the socket half and the answer a browser sees.
    async fn redirect(query: &str, expected_state: &str) -> (u16, Result<String, AuthError>) {
        let loopback = bind().await.expect("a callback port");
        let url = format!("{}?{query}", uri(loopback.port()));
        let request = tokio::spawn(async move { reqwest::get(url).await });
        let outcome = receive(loopback, expected_state).await;
        let status = request
            .await
            .expect("the request task")
            .expect("a response")
            .status()
            .as_u16();
        (status, outcome)
    }

    #[tokio::test]
    async fn the_right_state_yields_the_code_and_a_page_that_says_so() {
        let (status, outcome) = redirect("code=ac-1&state=st-1", "st-1").await;
        assert_eq!(status, 200);
        assert_eq!(outcome.expect("a code"), "ac-1");
    }

    #[tokio::test]
    async fn a_wrong_state_is_refused_and_the_browser_is_told() {
        let (status, outcome) = redirect("code=ac-1&state=st-other", "st-1").await;
        assert_eq!(status, 400);
        assert!(matches!(outcome, Err(AuthError::Invalid(_))), "{outcome:?}");
    }

    #[tokio::test]
    async fn a_redirect_without_a_code_is_refused_the_same_way() {
        let (status, outcome) = redirect("error=access_denied", "st-1").await;
        assert_eq!(status, 400);
        assert!(matches!(outcome, Err(AuthError::Invalid(_))), "{outcome:?}");
    }

    #[tokio::test]
    async fn the_redirect_uri_names_the_port_that_was_bound() {
        let loopback = bind().await.expect("a callback port");
        assert_eq!(
            uri(loopback.port()),
            format!("http://localhost:{}/auth/callback", loopback.port())
        );
        assert!((FIRST_PORT..FIRST_PORT + PORTS).contains(&loopback.port()));
    }
}
