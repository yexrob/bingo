//! The host of a URL, for `WebFetch(domain:…)` rules.
//!
//! A rule must never match a host the fetcher would not actually contact, so
//! this reads only the shapes it is sure of — scheme, optional userinfo, a
//! plain hostname, an optional numeric port — and answers `None` for anything
//! else. `None` means no domain rule holds, which leaves the call to the rest
//! of the ladder.

pub fn host(url: &str) -> Option<&str> {
    let (_scheme, rest) = url.split_once("://")?;
    // A backslash ends the authority in the URL standard; treating it as an
    // ordinary character is how `https://good.example\@evil.example/` gets read
    // as the wrong host.
    let authority = rest.split(['/', '?', '#', '\\']).next()?;
    let host = match authority.rsplit_once('@') {
        Some((_userinfo, host)) => host,
        None => authority,
    };
    let host = strip_port(host)?;
    is_hostname(host).then_some(host)
}

fn strip_port(host: &str) -> Option<&str> {
    match host.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Some(host)
        }
        // Anything else with a colon is a shape this parser does not read
        // (an IPv6 literal, a malformed port); it must not guess.
        Some(_) => None,
        None => Some(host),
    }
}

fn is_hostname(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_url_yields_its_host() {
        assert_eq!(host("https://example.com/page"), Some("example.com"));
        assert_eq!(host("http://sub.example.com"), Some("sub.example.com"));
        assert_eq!(host("https://example.com:8443/x"), Some("example.com"));
        assert_eq!(host("https://user:pw@example.com/x"), Some("example.com"));
        assert_eq!(host("https://example.com?q=1"), Some("example.com"));
        assert_eq!(host("https://example.com#top"), Some("example.com"));
    }

    #[test]
    fn a_host_this_parser_cannot_read_is_no_host_at_all() {
        for url in [
            "example.com/x",
            "https://",
            "https:///path",
            "https://[::1]/x",
            "https://exa mple.com/",
            "https://exa%6dple.com/",
            "https://.example.com/",
            "https://example.com:port/",
        ] {
            assert_eq!(host(url), None, "{url}");
        }
    }

    #[test]
    fn a_backslash_ends_the_authority() {
        assert_eq!(
            host("https://good.example\\@evil.example/x"),
            Some("good.example"),
            "the userinfo trick must not rename the host"
        );
    }
}
