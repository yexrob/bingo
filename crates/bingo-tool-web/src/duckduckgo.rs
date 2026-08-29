//! The keyless backend: DuckDuckGo's HTML endpoint. Its results have no API,
//! only a shape, so the parser reads the two class names the page is built
//! around and nothing else about the markup.

use async_trait::async_trait;
use bingo_sdk::ToolError;
use regex::{Captures, Regex};
use reqwest::{Client, header};
use url::Url;

use crate::backend::{Hit, SearchBackend};
use crate::html_text;

const ENDPOINT: &str = "https://html.duckduckgo.com/html/";

/// A result block: the title link, its text, and the snippet after it. `(?s)`
/// because the markup wraps lines between the two.
const BLOCK: &str = r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?class="result__snippet"[^>]*>(.*?)</a>"#;

/// The class the endpoint serves when it decides the caller is a robot. It
/// answers 200 with no results, which is otherwise a search that found nothing.
const CHALLENGE: &str = "anomaly-modal";

#[derive(Debug)]
pub struct DuckDuckGo {
    http: Client,
    results: Results,
}

impl DuckDuckGo {
    /// Fails only if the block pattern stops compiling, which is a bug in this
    /// file and belongs at startup rather than in the middle of a turn.
    pub fn new(http: Client) -> Result<Self, regex::Error> {
        Ok(Self {
            http,
            results: Results::new()?,
        })
    }

    async fn page(&self, query: &str) -> Result<String, ToolError> {
        let endpoint = Url::parse_with_params(ENDPOINT, &[("q", query)])
            .map_err(|e| ToolError::Failed(format!("the search url: {e}")))?;
        let response = self
            .http
            .get(endpoint)
            .header(header::ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|e| ToolError::Failed(format!("the search failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::Failed(format!(
                "the search failed: HTTP {}",
                status.as_u16()
            )));
        }
        response
            .text()
            .await
            .map_err(|e| ToolError::Failed(format!("reading the results failed: {e}")))
    }
}

#[async_trait]
impl SearchBackend for DuckDuckGo {
    async fn search(&self, query: &str) -> Result<Vec<Hit>, ToolError> {
        self.results.read(&self.page(query).await?)
    }
}

/// The result blocks of one page.
#[derive(Debug)]
struct Results {
    block: Regex,
}

impl Results {
    fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            block: Regex::new(BLOCK)?,
        })
    }

    /// A page with no results is a search that found nothing, unless it is the
    /// challenge, which found nothing because it never searched.
    fn read(&self, page: &str) -> Result<Vec<Hit>, ToolError> {
        let hits = self.parse(page);
        if hits.is_empty() && page.contains(CHALLENGE) {
            return Err(ToolError::Failed(
                "duckduckgo answered with an anti-bot challenge instead of results; \
                 set `web.search` to \"brave\" and give a key to search with one"
                    .into(),
            ));
        }
        Ok(hits)
    }

    fn parse(&self, page: &str) -> Vec<Hit> {
        self.block
            .captures_iter(page)
            .map(|block| Hit {
                url: direct_url(&html_text::decode(&group(&block, 1))),
                title: html_text::plain(&group(&block, 2)),
                snippet: html_text::plain(&group(&block, 3)),
            })
            .collect()
    }
}

fn group(block: &Captures<'_>, index: usize) -> String {
    block
        .get(index)
        .map(|m| m.as_str())
        .unwrap_or_default()
        .to_string()
}

/// A result arrives wrapped in `//duckduckgo.com/l/?uddg=<the real URL>`.
/// Unwrapped it is a URL the model can fetch and the domain filter can read;
/// left wrapped, every hit looks like it came from duckduckgo.com.
fn direct_url(href: &str) -> String {
    let absolute = match href.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => href.to_string(),
    };
    let Ok(parsed) = Url::parse(&absolute) else {
        return absolute;
    };
    if !is_duckduckgo(&parsed) {
        return absolute;
    }
    parsed
        .query_pairs()
        .find(|(key, _)| key == "uddg")
        .map(|(_, target)| target.into_owned())
        .unwrap_or(absolute)
}

fn is_duckduckgo(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../fixtures/duckduckgo-results.html");
    const ANOMALY: &str = include_str!("../fixtures/duckduckgo-anomaly.html");

    fn read(page: &str) -> Result<Vec<Hit>, ToolError> {
        Results::new()
            .expect("the block pattern compiles")
            .read(page)
    }

    #[test]
    fn every_result_block_on_the_page_becomes_a_hit() {
        let hits = read(RESULTS).expect("hits");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[1],
            Hit {
                title: "tokio - Rust".into(),
                url: "https://docs.rs/tokio/latest/tokio/".into(),
                snippet: "A runtime for writing reliable network applications without \
                          compromising speed."
                    .into(),
            }
        );
    }

    #[test]
    fn the_redirect_wrapper_is_unwrapped_to_the_page_it_points_at() {
        let hits = read(RESULTS).expect("hits");
        assert_eq!(hits[0].url, "https://rust-lang.github.io/async-book/");
        assert_eq!(hits[2].url, "https://blog.spam.example/async?utm=1");
    }

    #[test]
    fn titles_and_snippets_arrive_as_text() {
        let hits = read(RESULTS).expect("hits");
        assert_eq!(
            hits[0].title,
            "Asynchronous Programming in Rust & the async book"
        );
        assert_eq!(
            hits[0].snippet,
            "This book aims to be a complete guide to async Rust, covering the language's \
             futures and the runtimes around them."
        );
        assert_eq!(hits[2].title, "Async, explained — spam.example");
    }

    #[test]
    fn a_challenge_page_is_a_failure_and_not_an_empty_result() {
        let error = read(ANOMALY).err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("challenge")),
            "got {error:?}"
        );
    }

    #[test]
    fn a_page_with_no_results_and_no_challenge_is_empty() {
        assert_eq!(
            read("<html><body>nothing here</body></html>").ok(),
            Some(vec![])
        );
    }

    #[test]
    fn a_link_that_is_not_a_redirect_is_left_alone() {
        assert_eq!(direct_url("https://other.org/x"), "https://other.org/x");
        assert_eq!(
            direct_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.example%2Fx%3Fq%3D1"),
            "https://a.example/x?q=1"
        );
        assert_eq!(direct_url("//example.com/x"), "https://example.com/x");
        assert_eq!(direct_url("not a url"), "not a url");
    }
}
