//! The keyed backend: the Brave Search API. A JSON answer with a documented
//! shape, which is why it is the one to configure when the keyless endpoint
//! starts asking whether the caller is a person.

use std::fmt;

use async_trait::async_trait;
use bingo_sdk::ToolError;
use reqwest::{Client, header};
use serde::Deserialize;
use url::Url;

use crate::backend::{Hit, SearchBackend};
use crate::hits::MAX_HITS;
use crate::html_text;

const ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

pub struct Brave {
    http: Client,
    key: String,
}

impl Brave {
    pub fn new(http: Client, key: String) -> Self {
        Self { http, key }
    }

    async fn body(&self, query: &str) -> Result<String, ToolError> {
        let count = MAX_HITS.to_string();
        let endpoint = Url::parse_with_params(ENDPOINT, &[("q", query), ("count", &count)])
            .map_err(|e| ToolError::Failed(format!("the search url: {e}")))?;
        let response = self
            .http
            .get(endpoint)
            .header("X-Subscription-Token", &self.key)
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| ToolError::Failed(format!("the search failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::Failed(match status.as_u16() {
                401 | 403 => "the brave search key was rejected".to_string(),
                code => format!("the search failed: HTTP {code}"),
            }));
        }
        response
            .text()
            .await
            .map_err(|e| ToolError::Failed(format!("reading the results failed: {e}")))
    }
}

/// The key is not part of what this is; a struct that prints it puts it in
/// every log line that ever prints the tool.
impl fmt::Debug for Brave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Brave").finish_non_exhaustive()
    }
}

#[async_trait]
impl SearchBackend for Brave {
    async fn search(&self, query: &str) -> Result<Vec<Hit>, ToolError> {
        parse(&self.body(query).await?)
    }
}

/// Only the three fields a hit is made of; the rest of the answer is metadata
/// this tool has no use for.
#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    web: Option<Web>,
}

#[derive(Debug, Deserialize)]
struct Web {
    #[serde(default)]
    results: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    title: String,
    url: String,
    /// Brave marks the query terms in it, so it arrives as markup.
    #[serde(default)]
    description: String,
}

fn parse(body: &str) -> Result<Vec<Hit>, ToolError> {
    let answer: Answer = serde_json::from_str(body)
        .map_err(|e| ToolError::Failed(format!("the search results did not parse: {e}")))?;
    Ok(answer
        .web
        .map(|web| web.results)
        .unwrap_or_default()
        .into_iter()
        .map(hit)
        .collect())
}

fn hit(entry: Entry) -> Hit {
    Hit {
        title: html_text::plain(&entry.title),
        url: entry.url,
        snippet: html_text::plain(&entry.description),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = include_str!("../fixtures/brave-results.json");

    #[test]
    fn every_web_result_becomes_a_hit() {
        let hits = parse(RESULTS).expect("hits");
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0],
            Hit {
                title: "Asynchronous Programming in Rust".into(),
                url: "https://rust-lang.github.io/async-book/".into(),
                snippet: "This book aims to be a complete guide to async Rust.".into(),
            }
        );
    }

    #[test]
    fn a_result_without_a_description_keeps_its_title_and_url() {
        let hits = parse(RESULTS).expect("hits");
        assert_eq!(hits[2].url, "https://blog.spam.example/async");
        assert_eq!(hits[2].snippet, "");
    }

    #[test]
    fn an_answer_with_no_web_section_holds_no_hits() {
        assert_eq!(parse(r#"{"type":"search"}"#).ok(), Some(vec![]));
    }

    #[test]
    fn an_answer_that_is_not_the_documented_shape_says_so() {
        let error = parse("not json").err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("did not parse")),
            "got {error:?}"
        );
    }

    #[test]
    fn the_key_never_reaches_a_debug_line() {
        let brave = Brave::new(Client::new(), "secret-key-value".into());
        assert!(!format!("{brave:?}").contains("secret-key-value"));
    }
}
