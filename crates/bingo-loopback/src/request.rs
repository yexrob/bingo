//! What a client asked for, read out of the bytes it sent.
//!
//! A parser rather than a server: the socket owns the reading and this owns the
//! meaning, so every shape a browser can send — a target with a query, a header
//! cased however it likes, a length longer than anything worth reading — has a
//! test that needs no port.
//!
//! Only `Content-Length` bodies are read. Nothing that talks to this server
//! sends a chunked one: `fetch` with a string body always declares its length.

use crate::error::LoopbackError;

/// A head longer than this is not a request meant for a page on this machine.
pub const MAX_HEAD: usize = 8 * 1024;

/// What a page may post back. A megabyte of JSON is already more than a turn
/// wants; past it the answer is refused rather than read into memory.
pub const MAX_BODY: usize = 1024 * 1024;

/// The blank line that ends a head.
pub(crate) const END: &[u8] = b"\r\n\r\n";

/// The request line, and the one header that says how much more there is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    /// Upper-cased, so a comparison never has to remember to.
    pub method: String,
    /// The target as it was written: a path, and a query if there was one.
    pub target: String,
    pub content_length: usize,
}

/// One request: its head, and the body the head declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub head: Head,
    pub body: Vec<u8>,
}

impl Head {
    /// `POST /t/answer HTTP/1.1\r\nContent-Length: 2\r\n` → the three fields.
    pub fn parse(head: &str) -> Result<Self, LoopbackError> {
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut words = request_line.split_whitespace();
        let (Some(method), Some(target)) = (words.next(), words.next()) else {
            return Err(LoopbackError::Malformed(format!(
                "the request line is {request_line:?}"
            )));
        };
        Ok(Head {
            method: method.to_ascii_uppercase(),
            target: target.to_string(),
            content_length: content_length(lines)?,
        })
    }

    /// The target without its query, which is what a route is decided on.
    pub fn path(&self) -> &str {
        match self.target.split_once('?') {
            Some((path, _)) => path,
            None => &self.target,
        }
    }
}

/// Absent means empty, which is what a `GET` carries. Present and unreadable
/// is a refusal: a length nobody can agree on is not a body.
fn content_length<'a>(headers: impl Iterator<Item = &'a str>) -> Result<usize, LoopbackError> {
    for line in headers {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            return value
                .parse()
                .map_err(|_| LoopbackError::Malformed(format!("Content-Length is {value:?}")));
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(lines: &[&str]) -> String {
        lines.join("\r\n")
    }

    #[test]
    fn a_get_names_its_method_and_target_and_carries_no_body() {
        let head = Head::parse(&head(&["GET /tok HTTP/1.1", "Host: 127.0.0.1:9"]))
            .expect("a request line");
        assert_eq!(
            head,
            Head {
                method: "GET".into(),
                target: "/tok".into(),
                content_length: 0,
            }
        );
        assert_eq!(head.path(), "/tok");
    }

    #[test]
    fn a_post_reads_its_length_whatever_the_header_is_cased_like() {
        for name in ["Content-Length", "content-length", "CONTENT-LENGTH"] {
            let head = Head::parse(&head(&[
                "POST /tok/answer HTTP/1.1",
                &format!("{name}:  12  "),
                "Content-Type: application/json",
            ]))
            .expect("a length");
            assert_eq!(head.content_length, 12, "{name}");
            assert_eq!(head.method, "POST");
        }
    }

    #[test]
    fn a_query_is_part_of_the_target_and_not_part_of_the_path() {
        let head = Head::parse("GET /auth/callback?code=ac&state=st HTTP/1.1").expect("a target");
        assert_eq!(head.target, "/auth/callback?code=ac&state=st");
        assert_eq!(head.path(), "/auth/callback");
    }

    /// The method is compared, so it is stored the one way a comparison can
    /// trust rather than however the client happened to write it.
    #[test]
    fn a_lower_cased_method_is_read_as_the_method_it_is() {
        assert_eq!(
            Head::parse("post /x HTTP/1.1").expect("a method").method,
            "POST"
        );
    }

    #[test]
    fn a_head_without_a_method_and_a_target_is_refused() {
        for line in ["", "GET", "   ", "\r\nHost: x"] {
            assert!(
                matches!(Head::parse(line), Err(LoopbackError::Malformed(_))),
                "{line:?} is not a request line"
            );
        }
    }

    #[test]
    fn a_length_that_is_not_a_number_is_refused_rather_than_guessed() {
        let error = Head::parse(&head(&["POST /x HTTP/1.1", "Content-Length: soon"])).err();
        assert!(
            matches!(&error, Some(LoopbackError::Malformed(m)) if m.contains("soon")),
            "got {error:?}"
        );
    }
}
