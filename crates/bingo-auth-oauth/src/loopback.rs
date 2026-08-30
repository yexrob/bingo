//! The one-shot HTTP server the browser redirects to.
//!
//! Loopback only, one connection, then gone: the socket exists for the few
//! seconds between opening the browser and the issuer answering, and the
//! `state` nonce is what makes a redirect from anywhere else worthless.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::callback;
use crate::error::AuthError;

/// codex's own callback port; twenty above it is enough for a second attempt
/// while a first one is still bound.
const FIRST_PORT: u16 = 1455;
const PORTS: u16 = 21;

/// A request head longer than this is not a browser redirect.
const MAX_HEAD: usize = 8 * 1024;

/// A bound listener and the redirect URI that names it.
#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    pub async fn bind() -> Result<Self, AuthError> {
        let mut last = None;
        for port in FIRST_PORT..FIRST_PORT + PORTS {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => return Ok(Loopback { listener, port }),
                Err(error) => last = Some(error),
            }
        }
        Err(AuthError::Transport(format!(
            "no free callback port in {FIRST_PORT}..{}: {}",
            FIRST_PORT + PORTS - 1,
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// `localhost` rather than `127.0.0.1`: the issuer's allow-list is written
    /// with the name.
    pub fn redirect_uri(&self) -> String {
        format!("http://localhost:{}{}", self.port, callback::PATH)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Accept one redirect and answer it, whatever it turns out to be: a
    /// browser left staring at a dead socket tells a person nothing.
    pub async fn receive(self, expected_state: &str) -> Result<String, AuthError> {
        let (mut socket, _) = self
            .listener
            .accept()
            .await
            .map_err(|e| AuthError::Transport(format!("accept the callback: {e}")))?;
        let head = read_head(&mut socket).await?;
        let outcome = code(&head, expected_state);
        let _ = socket.write_all(page(&outcome).as_bytes()).await;
        let _ = socket.shutdown().await;
        outcome
    }
}

fn code(head: &str, expected_state: &str) -> Result<String, AuthError> {
    let callback = callback::parse(head)?;
    if callback.state != expected_state {
        return Err(AuthError::Invalid(
            "the callback state does not match".into(),
        ));
    }
    Ok(callback.code)
}

fn page(outcome: &Result<String, AuthError>) -> String {
    let (status, body) = match outcome {
        Ok(_) => (
            "200 OK",
            "<h1>Signed in.</h1><p>You can close this tab.</p>",
        ),
        Err(_) => (
            "400 Bad Request",
            "<h1>Sign-in failed.</h1><p>Return to the terminal.</p>",
        ),
    };
    let body = format!("<!doctype html><meta charset=\"utf-8\"><title>bingo</title>{body}");
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Everything up to the blank line, which is all a redirect carries.
async fn read_head(socket: &mut TcpStream) -> Result<String, AuthError> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while head.len() < MAX_HEAD {
        let read = socket
            .read(&mut chunk)
            .await
            .map_err(|e| AuthError::Transport(format!("read the callback: {e}")))?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..read]);
        if head.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real port, hit by a real client: the parser has its own unit tests,
    /// so what is proved here is the socket half and the answer a browser sees.
    async fn redirect(query: &str, expected_state: &str) -> (u16, Result<String, AuthError>) {
        let loopback = Loopback::bind().await.expect("a callback port");
        let url = format!("{}?{query}", loopback.redirect_uri());
        let request = tokio::spawn(async move { reqwest::get(url).await });
        let outcome = loopback.receive(expected_state).await;
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
        let loopback = Loopback::bind().await.expect("a callback port");
        assert_eq!(
            loopback.redirect_uri(),
            format!("http://localhost:{}/auth/callback", loopback.port())
        );
        assert!((FIRST_PORT..FIRST_PORT + PORTS).contains(&loopback.port()));
    }
}
