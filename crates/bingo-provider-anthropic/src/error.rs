//! Failure classification. A status, a body and a `retry-after` header in,
//! one `ProviderError` out.
//!
//! The provider never retries: the turn loop owns the ladder and reads
//! `ProviderError::retryable` and `retry_after_ms`, so everything this module
//! decides is *what kind of failure this was*. Pure — no I/O, no client.

use bingo_sdk::ProviderError;
use serde_json::Value;

/// A 400 or 413 whose body says one of these is the context window, not a
/// malformed request: the loop compacts and re-runs the turn instead of
/// failing it. Ported from the old `api/contract.rs:79-103`.
const OVERFLOW_PHRASES: &[&str] = &[
    "context length",
    "context window",
    "context limit",
    "max context",
    "maximum context",
    "input exceeds",
    "input is too long",
    "input too long",
    "prompt is too long",
    "prompt too long",
    "too many tokens",
    "token limit",
];

/// Free-form messages that read as a transient server condition. Only
/// consulted when no error `type` decided (old `api/contract.rs:285-309`).
const TRANSIENT_PHRASES: &[&str] = &[
    "overloaded",
    "server_error",
    "server error",
    "internal_error",
    "internal error",
    "service_unavailable",
    "service unavailable",
    "too_many_requests",
    "too many requests",
    "rate_limit",
    "rate limit",
    "resource_exhausted",
    "resource exhausted",
    "bad gateway",
    "gateway timeout",
    "try again later",
];

/// The Anthropic status for "overloaded", outside the 5xx range everyone else uses.
const OVERLOADED: u16 = 529;

/// One non-success HTTP response → the error the turn loop reacts to.
pub fn classify(status: u16, body: &str, retry_after: Option<&str>) -> ProviderError {
    let message = message_of(body);
    if says_overflow(body) && matches!(status, 400 | 413) {
        return ProviderError::ContextOverflow { message };
    }
    match status {
        401 | 403 => ProviderError::Auth { message },
        408 => ProviderError::Timeout,
        429 => ProviderError::RateLimited {
            retry_after_ms: retry_after.and_then(retry_after_ms),
        },
        500..600 => ProviderError::Server { status, message },
        _ => ProviderError::Request { message },
    }
}

/// An `error` SSE event's `{type, message}` → the error the stream ends with.
pub fn stream_error(kind: &str, message: &str) -> ProviderError {
    let message = if kind.is_empty() {
        message.to_string()
    } else {
        format!("{kind}: {message}")
    };
    match kind {
        "authentication_error" | "permission_error" => ProviderError::Auth { message },
        "rate_limit_error" => ProviderError::RateLimited {
            retry_after_ms: None,
        },
        "overloaded_error" => ProviderError::Server {
            status: OVERLOADED,
            message,
        },
        "api_error" | "server_error" | "timeout_error" => ProviderError::Server {
            status: 500,
            message,
        },
        "invalid_request_error" | "not_found_error" | "request_too_large" => {
            request_or_overflow(message)
        }
        _ => unnamed_stream_error(message),
    }
}

/// No error type the wire named: read the sentence instead. A transient-looking
/// one is a retryable `Stream` failure, anything else is the request's fault.
fn unnamed_stream_error(message: String) -> ProviderError {
    if says_overflow(&message) {
        return ProviderError::ContextOverflow { message };
    }
    if is_transient(&message) {
        return ProviderError::Stream { message };
    }
    ProviderError::Request { message }
}

fn request_or_overflow(message: String) -> ProviderError {
    if says_overflow(&message) {
        ProviderError::ContextOverflow { message }
    } else {
        ProviderError::Request { message }
    }
}

/// Whether the text names the context window as the thing that was exceeded.
fn says_overflow(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    OVERFLOW_PHRASES.iter().any(|p| text.contains(p))
        || (text.contains("maximum")
            && text.contains("token")
            && (text.contains("prompt") || text.contains("input")))
}

fn is_transient(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    if says_overflow(&text) || text.contains("insufficient_quota") {
        return false;
    }
    names_5xx(&text) || TRANSIENT_PHRASES.iter().any(|p| text.contains(p))
}

/// A 5xx number counts as a status only when it opens the message or follows a
/// status marker: "field exceeds the maximum of 512 characters" is a bad
/// request, not a server error (old `api/contract.rs:323-339`).
fn names_5xx(text: &str) -> bool {
    let mut after_marker = true;
    for token in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        if after_marker && matches!(token.parse::<u16>(), Ok(s) if (500..600).contains(&s)) {
            return true;
        }
        after_marker = matches!(token, "http" | "https" | "status" | "code");
    }
    false
}

/// The sentence inside `{"error":{"message":…}}`, or the body as it came.
/// Printing the envelope is printing punctuation.
pub fn message_of(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.trim().to_string();
    };
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| body.trim().to_string())
}

/// `retry-after` is delay-seconds or an HTTP date (RFC 9110 §10.2.3). A date
/// already past means "now".
pub fn retry_after_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds.saturating_mul(1_000));
    }
    let until = jiff::fmt::rfc2822::parse(value).ok()?.timestamp();
    let millis = until.duration_since(jiff::Timestamp::now()).as_millis();
    Some(u64::try_from(millis).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(kind: &str, message: &str) -> String {
        format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"{message}"}}}}"#)
    }

    #[test]
    fn credentials_are_an_auth_failure_and_never_retried() {
        for status in [401, 403] {
            let error = classify(status, &body("authentication_error", "bad key"), None);
            assert_eq!(
                error,
                ProviderError::Auth {
                    message: "bad key".into()
                }
            );
            assert!(!error.retryable());
        }
    }

    #[test]
    fn a_429_carries_the_retry_after_header_in_milliseconds() {
        let error = classify(429, "{}", Some("30"));
        assert_eq!(
            error,
            ProviderError::RateLimited {
                retry_after_ms: Some(30_000)
            }
        );
        assert_eq!(error.retry_after_ms(), Some(30_000));
    }

    #[test]
    fn a_429_without_the_header_still_rate_limits() {
        assert_eq!(
            classify(429, "{}", None),
            ProviderError::RateLimited {
                retry_after_ms: None
            }
        );
    }

    #[test]
    fn an_http_date_retry_after_becomes_a_delay_and_a_past_date_becomes_zero() {
        let future = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(120);
        let header = jiff::fmt::rfc2822::to_string(&future.to_zoned(jiff::tz::TimeZone::UTC))
            .expect("format an http date");
        let ms = retry_after_ms(&header).expect("a date parses");
        assert!(
            (110_000..=120_000).contains(&ms),
            "{header} gave {ms}ms, expected about two minutes"
        );
        assert_eq!(
            retry_after_ms("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(0),
            "a date already past means now"
        );
        assert_eq!(retry_after_ms("not a date"), None);
    }

    #[test]
    fn a_400_naming_the_context_window_is_an_overflow_the_loop_compacts() {
        for message in [
            "prompt is too long: 214000 tokens > 200000 maximum",
            "input length and `max_tokens` exceed context limit: 190000 + 64000 > 200000",
            "this request exceeds the context window for this model",
        ] {
            let error = classify(400, &body("invalid_request_error", message), None);
            assert!(
                matches!(error, ProviderError::ContextOverflow { .. }),
                "{message} classified as {error:?}"
            );
            assert!(!error.retryable(), "overflow is compacted, not retried");
        }
    }

    #[test]
    fn other_client_errors_are_bad_requests() {
        for status in [400, 404, 422] {
            let error = classify(status, &body("invalid_request_error", "no such tool"), None);
            assert_eq!(
                error,
                ProviderError::Request {
                    message: "no such tool".into()
                },
                "status {status}"
            );
        }
    }

    #[test]
    fn a_408_is_a_timeout() {
        assert_eq!(classify(408, "", None), ProviderError::Timeout);
    }

    #[test]
    fn server_errors_and_529_overloaded_are_retryable() {
        for status in [500, 502, 503, OVERLOADED] {
            let error = classify(status, &body("overloaded_error", "Overloaded"), None);
            assert_eq!(
                error,
                ProviderError::Server {
                    status,
                    message: "Overloaded".into()
                }
            );
            assert!(error.retryable());
        }
    }

    #[test]
    fn a_message_that_merely_contains_512_characters_is_not_a_server_error() {
        assert!(!is_transient("field exceeds the maximum of 512 characters"));
        assert!(is_transient("503 Service Unavailable"));
        assert!(is_transient("upstream returned HTTP 502"));
    }

    #[test]
    fn a_stream_error_event_is_named_by_its_type() {
        assert_eq!(
            stream_error("overloaded_error", "Overloaded"),
            ProviderError::Server {
                status: OVERLOADED,
                message: "overloaded_error: Overloaded".into()
            }
        );
        assert_eq!(
            stream_error("rate_limit_error", "slow down"),
            ProviderError::RateLimited {
                retry_after_ms: None
            }
        );
        assert!(matches!(
            stream_error("authentication_error", "expired"),
            ProviderError::Auth { .. }
        ));
        assert!(matches!(
            stream_error("invalid_request_error", "prompt is too long"),
            ProviderError::ContextOverflow { .. }
        ));
        assert!(matches!(
            stream_error("invalid_request_error", "unknown field"),
            ProviderError::Request { .. }
        ));
    }

    #[test]
    fn an_unnamed_stream_error_is_read_from_its_sentence() {
        assert!(matches!(
            stream_error("", "the gateway is overloaded, try again later"),
            ProviderError::Stream { .. }
        ));
        assert!(matches!(
            stream_error("weird_error", "name exceeds the maximum of 512 characters"),
            ProviderError::Request { .. }
        ));
    }

    #[test]
    fn an_error_envelope_is_unwrapped_and_a_bare_body_is_kept() {
        assert_eq!(message_of(&body("api_error", "boom")), "boom");
        assert_eq!(message_of(r#"{"message":"plain"}"#), "plain");
        assert_eq!(message_of("  gateway timeout  "), "gateway timeout");
        assert_eq!(message_of("{}"), "{}");
    }
}
