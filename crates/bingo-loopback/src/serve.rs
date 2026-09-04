//! Holding one page open until it answers.
//!
//! The loop ends on the answer and on nothing else: the timeout and the turn's
//! interrupt are the caller's to race this against (ADR-0042 §4). A request that
//! is not this page's is a 404 and the page goes on waiting — so a reload works,
//! a favicon costs nothing, and no other process on the machine can answer for
//! the person.

use crate::answer::{self, Answer};
use crate::error::LoopbackError;
use crate::page::{self, Route};
use crate::response::Response;
use crate::server::Loopback;
use crate::token::Token;

/// Serve `document` at `GET /<token>` until `POST /<token>/answer` arrives.
pub async fn until_answered(
    loopback: Loopback,
    token: &Token,
    document: &str,
) -> Result<Answer, LoopbackError> {
    loop {
        let mut connection = loopback.accept().await?;
        let request = match connection.request().await {
            Ok(Some(request)) => request,
            // A socket a browser opened and never spoke on.
            Ok(None) => continue,
            Err(error) => {
                connection.reply(&refusal(&error)).await;
                continue;
            }
        };
        match page::route(&request.head, token) {
            Route::Page => connection.reply(&Response::html("200 OK", document)).await,
            Route::Missing => connection.reply(&Response::not_found()).await,
            Route::Answer => {
                let answered = answer::parse(&request.body);
                connection.reply(&receipt(&answered)).await;
                if let Ok(answer) = answered {
                    return Ok(answer);
                }
            }
        }
    }
}

/// A request that could not be read is answered and forgotten: the page itself
/// may still be about to submit.
fn refusal(error: &LoopbackError) -> Response {
    match error {
        LoopbackError::TooLarge(_) => Response::text("413 Content Too Large", error.to_string()),
        _ => Response::text("400 Bad Request", error.to_string()),
    }
}

/// What the script reads back, which is only whether it was understood.
fn receipt(answered: &Result<Answer, LoopbackError>) -> Response {
    match answered {
        Ok(_) => Response::text("200 OK", "answered"),
        Err(error) => Response::text("400 Bad Request", error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::MAX_BODY;
    use crate::server::tests::{get, post, send};
    use serde_json::json;

    /// The page one test serves, and the token it is served under.
    struct Served {
        port: u16,
        token: Token,
        answered: tokio::task::JoinHandle<Result<Answer, LoopbackError>>,
    }

    async fn served() -> Served {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let token = Token::mint().expect("a token");
        let document = page::document("Three layouts", "<body><p>pick</p></body>");
        let held = token.clone();
        let answered =
            tokio::spawn(async move { until_answered(loopback, &held, &document).await });
        Served {
            port,
            token,
            answered,
        }
    }

    impl Served {
        fn path(&self) -> String {
            format!("/{}", self.token.as_str())
        }

        async fn answer(self) -> Answer {
            self.answered
                .await
                .expect("the serving task")
                .expect("an answer")
        }
    }

    #[tokio::test]
    async fn the_page_is_served_at_its_token_with_the_script_in_it() {
        let served = served().await;
        let answer = send(served.port, &get(&served.path())).await;
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert!(answer.contains("<p>pick</p>"), "{answer}");
        assert!(answer.contains("window.bingo"), "{answer}");
        assert!(answer.contains("<title>Three layouts</title>"), "{answer}");

        // Still waiting: a page that was only read has not answered.
        let path = format!("{}/answer", served.path());
        send(served.port, &post(&path, r#"{"value":"done"}"#)).await;
        assert_eq!(served.answer().await, Answer::Submitted(json!("done")));
    }

    #[tokio::test]
    async fn a_reload_is_served_the_same_page_again() {
        let served = served().await;
        let first = send(served.port, &get(&served.path())).await;
        let again = send(served.port, &get(&served.path())).await;
        assert_eq!(first, again);
        send(
            served.port,
            &post(
                &format!("{}/answer", served.path()),
                r#"{"cancelled":true}"#,
            ),
        )
        .await;
        assert_eq!(served.answer().await, Answer::Cancelled);
    }

    /// The exit criterion: a POST without the token, or from a page that is not
    /// this one, is refused and the call goes on waiting for the real answer.
    #[tokio::test]
    async fn an_answer_without_the_token_is_refused_and_the_page_keeps_waiting() {
        let served = served().await;
        for path in [
            "/answer".to_string(),
            "/other/answer".to_string(),
            format!("/{}/answer", "x".repeat(43)),
            format!("{}x/answer", served.path()),
        ] {
            let answer = send(served.port, &post(&path, r#"{"value":"stolen"}"#)).await;
            assert!(
                answer.starts_with("HTTP/1.1 404 Not Found\r\n"),
                "{path}: {answer}"
            );
        }
        send(
            served.port,
            &post(&format!("{}/answer", served.path()), r#"{"value":"mine"}"#),
        )
        .await;
        assert_eq!(served.answer().await, Answer::Submitted(json!("mine")));
    }

    #[tokio::test]
    async fn an_answer_that_is_not_the_envelope_is_refused_and_the_page_keeps_waiting() {
        let served = served().await;
        let path = format!("{}/answer", served.path());
        let answer = send(served.port, &post(&path, "{}")).await;
        assert!(
            answer.starts_with("HTTP/1.1 400 Bad Request\r\n"),
            "{answer}"
        );
        assert!(
            answer.contains("cancelled"),
            "the refusal says what it wanted"
        );

        send(served.port, &post(&path, r#"{"value":1}"#)).await;
        assert_eq!(served.answer().await, Answer::Submitted(json!(1)));
    }

    #[tokio::test]
    async fn an_answer_over_the_cap_is_refused_and_the_page_keeps_waiting() {
        let served = served().await;
        let path = format!("{}/answer", served.path());
        let oversized = format!(
            "POST {path} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        let answer = send(served.port, &oversized).await;
        assert!(
            answer.starts_with("HTTP/1.1 413 Content Too Large\r\n"),
            "{answer}"
        );

        send(served.port, &post(&path, r#"{"value":"small"}"#)).await;
        assert_eq!(served.answer().await, Answer::Submitted(json!("small")));
    }
}
