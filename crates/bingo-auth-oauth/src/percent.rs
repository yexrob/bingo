//! Percent-encoding for the two places a query string is built and read.
//!
//! Hand-rolled rather than pulled from `url`: an authorize query and a
//! callback query are the whole of it, and the unreserved set of RFC 3986 is
//! five lines.

/// RFC 3986 unreserved characters pass; everything else becomes `%XX`.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `%XX` back to bytes; anything else is itself. A malformed escape is left
/// as written rather than dropped, so a code is never silently truncated.
pub fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match hex_pair(bytes, i) {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_pair(bytes: &[u8], at: usize) -> Option<u8> {
    if bytes.get(at) != Some(&b'%') {
        return None;
    }
    let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
    u8::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_uri_survives_a_round_trip() {
        let uri = "http://localhost:1455/auth/callback";
        assert_eq!(
            encode(uri),
            "http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"
        );
        assert_eq!(decode(&encode(uri)), uri);
    }

    #[test]
    fn a_scope_encodes_its_spaces() {
        assert_eq!(encode("openid profile"), "openid%20profile");
    }

    #[test]
    fn a_malformed_escape_is_left_as_written() {
        assert_eq!(decode("ab%zz"), "ab%zz");
        assert_eq!(decode("ab%"), "ab%");
        assert_eq!(decode("plain-code_1~"), "plain-code_1~");
    }
}
