//! The one URL a fetch is about: validated once, upgraded to https, written in
//! canonical form. The permission subject, the cache key and the request all
//! read this, never the string the model wrote — so no two of them can disagree
//! about which page was approved and which page was read.

use std::fmt;

use url::Url;

/// Past this a URL is a mistake, not a request.
const MAX_LEN: usize = 2_000;

#[derive(Debug, thiserror::Error)]
pub enum Invalid {
    #[error("longer than {MAX_LEN} characters")]
    TooLong,
    #[error("not a URL: {0}")]
    Unparsable(String),
    #[error("the scheme must be http or https, not {0}")]
    Scheme(String),
    #[error("a URL with credentials in it is never fetched")]
    Credentials,
    #[error("the host must have at least two parts")]
    Host,
}

/// A URL that passed every check, with `http` upgraded to `https`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canonical(Url);

impl Canonical {
    pub fn parse(input: &str) -> Result<Self, Invalid> {
        if input.len() > MAX_LEN {
            return Err(Invalid::TooLong);
        }
        let mut url = Url::parse(input).map_err(|e| Invalid::Unparsable(e.to_string()))?;
        check_scheme(&url)?;
        check_credentials(&url)?;
        check_host(&url)?;
        upgrade(&mut url);
        Ok(Self(url))
    }

    /// Empty is unreachable: `parse` refuses a URL without a host.
    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }

    pub fn path(&self) -> &str {
        self.0.path()
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Canonical {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

fn check_scheme(url: &Url) -> Result<(), Invalid> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(Invalid::Scheme(other.to_string())),
    }
}

fn check_credentials(url: &Url) -> Result<(), Invalid> {
    let has_password = url.password().is_some_and(|p| !p.is_empty());
    if url.username().is_empty() && !has_password {
        Ok(())
    } else {
        Err(Invalid::Credentials)
    }
}

/// A host without a dot is a search term, an intranet name or a typo; none of
/// the three is a public document.
fn check_host(url: &Url) -> Result<(), Invalid> {
    match url.host_str() {
        Some(host) if host.contains('.') => Ok(()),
        _ => Err(Invalid::Host),
    }
}

/// A plaintext request leaves the page open to whoever carries it, so `http`
/// becomes `https`. A loopback address carries nothing across a network and has
/// no certificate to offer, so it keeps the scheme it was given.
fn upgrade(url: &mut Url) {
    if url.scheme() == "http" && !is_loopback(url) {
        let _ = url.set_scheme("https");
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<String, Invalid> {
        Canonical::parse(input).map(|c| c.to_string())
    }

    #[test]
    fn a_url_longer_than_the_cap_is_refused_before_it_is_parsed() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_LEN));
        assert!(matches!(parse(&long), Err(Invalid::TooLong)));
    }

    #[test]
    fn what_is_not_a_url_says_so() {
        assert!(matches!(parse("not a url"), Err(Invalid::Unparsable(_))));
    }

    #[test]
    fn only_http_and_https_are_fetched() {
        assert!(matches!(parse("ftp://example.com/x"), Err(Invalid::Scheme(s)) if s == "ftp"));
        assert!(matches!(parse("file:///etc/hosts"), Err(Invalid::Scheme(s)) if s == "file"));
        assert!(parse("https://example.com/x").is_ok());
    }

    #[test]
    fn credentials_in_a_url_are_refused() {
        assert!(matches!(
            parse("https://user@example.com/"),
            Err(Invalid::Credentials)
        ));
        assert!(matches!(
            parse("https://user:secret@example.com/"),
            Err(Invalid::Credentials)
        ));
    }

    #[test]
    fn a_host_without_a_dot_is_refused() {
        assert!(matches!(parse("https://localhost/x"), Err(Invalid::Host)));
        assert!(matches!(parse("https://intranet/"), Err(Invalid::Host)));
    }

    #[test]
    fn http_is_upgraded_to_https() {
        assert_eq!(
            parse("http://example.com/docs").ok(),
            Some("https://example.com/docs".to_string())
        );
    }

    #[test]
    fn a_loopback_address_keeps_the_scheme_it_was_given() {
        assert_eq!(
            parse("http://127.0.0.1:8080/page").ok(),
            Some("http://127.0.0.1:8080/page".to_string())
        );
    }

    #[test]
    fn the_canonical_form_is_what_the_cache_and_the_gate_both_see() {
        let canonical = Canonical::parse("http://Example.COM?b=2&a=1").expect("valid");
        assert_eq!(canonical.as_str(), "https://example.com/?b=2&a=1");
        assert_eq!(canonical.host(), "example.com");
        assert_eq!(canonical.path(), "/");
    }
}
