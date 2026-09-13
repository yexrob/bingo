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

/// The one port a registration already named, because a registered redirect
/// URI is exact-matched and a different port is a different client. `None`,
/// or a port somebody else holds, falls back to the range: the caller then
/// registers again (ADR-0050, R-port).
pub async fn bind_named(port: Option<u16>) -> Result<Loopback, AuthError> {
    match port {
        Some(port) => match Loopback::in_range(port, 1).await {
            Ok(loopback) => Ok(loopback),
            Err(_) => bind().await,
        },
        None => bind().await,
    }
}

/// `localhost` rather than `127.0.0.1`: the issuer's allow-list is written
/// with the name.
pub fn uri(port: u16) -> String {
    format!("http://localhost:{port}{}", callback::PATH)
}

/// Both spellings of the same socket, which is what a registration names:
/// MCP's own text says `localhost`, RFC 8252 prefers the literal address, and
/// a server that exact-matches one of them will not match the other.
pub fn uris(port: u16) -> Vec<String> {
    vec![
        uri(port),
        format!("http://127.0.0.1:{port}{}", callback::PATH),
    ]
}

/// The port a redirect URI names, so a login can try the one a registration
/// was made with before making another.
pub fn port_of(uri: &str) -> Option<u16> {
    uri.rsplit_once(':')?
        .1
        .split('/')
        .next()?
        .parse::<u16>()
        .ok()
}

/// Accept the redirect and answer it, whatever it turns out to be: a browser
/// left staring at a dead socket tells a person nothing.
pub async fn receive(
    loopback: Loopback,
    expected_state: &str,
) -> Result<callback::Callback, AuthError> {
    loop {
        let mut connection = loopback.accept().await?;
        let outcome = match connection.request().await {
            // A browser opens sockets it never sends on; the redirect is still
            // coming.
            Ok(None) => continue,
            Ok(Some(request)) => checked(&request.head.target, expected_state),
            Err(error) => Err(error.into()),
        };
        connection.reply(&page(&outcome)).await;
        return outcome;
    }
}

fn checked(target: &str, expected_state: &str) -> Result<callback::Callback, AuthError> {
    let callback = callback::parse(target)?;
    if callback.state != expected_state {
        return Err(AuthError::Invalid(
            "the callback state does not match".into(),
        ));
    }
    Ok(callback)
}

fn page(outcome: &Result<callback::Callback, AuthError>) -> Response {
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
    async fn redirect(
        query: &str,
        expected_state: &str,
    ) -> (u16, Result<callback::Callback, AuthError>) {
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
        assert_eq!(outcome.expect("a code").code, "ac-1");
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

    #[test]
    fn a_registration_names_both_spellings_of_the_one_socket() {
        assert_eq!(
            uris(1455),
            [
                "http://localhost:1455/auth/callback",
                "http://127.0.0.1:1455/auth/callback",
            ]
        );
        assert_eq!(port_of("http://localhost:1460/auth/callback"), Some(1460));
        assert_eq!(port_of("http://127.0.0.1:1460/auth/callback"), Some(1460));
        assert_eq!(port_of("https://example.com/auth/callback"), None);
    }

    /// R-port: the next login takes the port the registration named, and a
    /// port already held falls back rather than failing the login.
    #[tokio::test]
    async fn the_named_port_is_taken_when_it_is_free_and_given_up_when_it_is_not() {
        let first = bind_named(None).await.expect("any port");
        let named = first.port();
        let again = bind_named(Some(named)).await.expect("a fallback port");
        assert_ne!(again.port(), named, "the port is held by the first bind");
        drop(first);
        drop(again);
        let free = bind().await.expect("a port");
        let port = free.port();
        drop(free);
        assert_eq!(
            bind_named(Some(port)).await.expect("the named port").port(),
            port
        );
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
