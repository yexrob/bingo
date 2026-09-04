//! What a client is answered with.
//!
//! Three fields, because that is all a page and its answer need. Every request
//! is answered, the refused ones included: a browser left staring at a dead
//! socket tells a person nothing.

const HTML: &str = "text/html; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    status: &'static str,
    content_type: &'static str,
    body: String,
}

impl Response {
    /// A page for a person to read.
    pub fn html(status: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: HTML,
            body: body.into(),
        }
    }

    /// A line for a script to read, or to ignore.
    pub fn text(status: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: TEXT,
            body: body.into(),
        }
    }

    /// The one answer that is not about the page: the path asked for is not a
    /// path this server has. It says no more than that — a token that was
    /// wrong and a token that was absent read the same from outside.
    pub fn not_found() -> Self {
        Self::text("404 Not Found", "not found")
    }

    /// `Connection: close` and a length, so no client waits for a second
    /// request on a socket that is about to go.
    pub fn bytes(&self) -> Vec<u8> {
        format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n{}",
            self.status,
            self.content_type,
            self.body.len(),
            self.body
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(response: &Response) -> String {
        String::from_utf8(response.bytes()).expect("utf-8")
    }

    #[test]
    fn a_page_carries_its_length_its_type_and_the_close() {
        let wire = wire(&Response::html("200 OK", "<p>hi</p>"));
        assert!(wire.starts_with("HTTP/1.1 200 OK\r\n"), "{wire}");
        assert!(
            wire.contains("Content-Type: text/html; charset=utf-8\r\n"),
            "{wire}"
        );
        assert!(wire.contains("Content-Length: 9\r\n"), "{wire}");
        assert!(wire.contains("Connection: close\r\n"), "{wire}");
        assert!(wire.ends_with("\r\n\r\n<p>hi</p>"), "{wire}");
    }

    /// The length is bytes, not characters: a page a person wrote in Chinese
    /// would be cut short by a count of `chars`.
    #[test]
    fn the_length_counts_bytes() {
        assert!(wire(&Response::html("200 OK", "三个方案")).contains("Content-Length: 12\r\n"));
    }

    #[test]
    fn a_missing_path_says_nothing_about_why() {
        let wire = wire(&Response::not_found());
        assert!(wire.starts_with("HTTP/1.1 404 Not Found\r\n"), "{wire}");
        assert!(!wire.contains("token"), "{wire}");
    }
}
