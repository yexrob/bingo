//! `WebSearch`: a query out, at most eight results back.
//!
//! The tool holds a backend and nothing else: which service answers is a
//! setting, and everything the model sees — the domain filter, the count, the
//! markdown — is the same whichever one it is.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::backend::{Hit, SearchBackend};
use crate::hits;

/// A search the network has not answered in this long will not be answered.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A one-character query is a slip, not a search.
const MIN_QUERY: usize = 2;

const DESCRIPTION: &str = "\
Search the web. Results come back as a numbered list of title, URL and summary, \
at most eight of them — use it for anything that postdates your training or \
changes often, then `WebFetch` a result to read the page itself. \
`allowed_domains` keeps results from those domains and their subdomains and \
nothing else; `blocked_domains` drops them. Given both, only `allowed_domains` \
is consulted. Cite the URLs you use.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// What to search for.
    pub query: String,
    /// Keep only results from these domains, subdomains included.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Drop results from these domains, subdomains included.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

#[derive(Debug)]
pub struct WebSearchTool {
    backend: Arc<dyn SearchBackend>,
}

impl WebSearchTool {
    pub fn new(backend: Arc<dyn SearchBackend>) -> Self {
        Self { backend }
    }

    async fn find(&self, query: &str) -> Result<Vec<Hit>, ToolError> {
        tokio::time::timeout(TIMEOUT, self.backend.search(query))
            .await
            .map_err(|_| {
                ToolError::Failed(format!(
                    "the search took longer than {} seconds",
                    TIMEOUT.as_secs()
                ))
            })?
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "WebSearch".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SearchArgs>(),
            meta: Default::default(),
        }
    }

    /// A search reads a public index and changes nothing, whichever backend
    /// answers it.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SearchArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let query = args.query.trim();
        if query.chars().count() < MIN_QUERY {
            return Err(ToolError::InvalidInput(format!(
                "the query must be at least {MIN_QUERY} characters"
            )));
        }
        let found = self.find(query).await?;
        let kept = hits::filter(found, &args.allowed_domains, &args.blocked_domains);
        Ok(ToolOutput::text(hits::render(query, &kept)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::context;

    /// A backend that answers from a script, so the tool can be exercised
    /// without a network.
    #[derive(Debug)]
    struct Canned(Result<Vec<Hit>, &'static str>);

    #[async_trait]
    impl SearchBackend for Canned {
        async fn search(&self, _query: &str) -> Result<Vec<Hit>, ToolError> {
            self.0
                .clone()
                .map_err(|message| ToolError::Failed(message.to_string()))
        }
    }

    /// A backend that never answers.
    #[derive(Debug)]
    struct Silent;

    #[async_trait]
    impl SearchBackend for Silent {
        async fn search(&self, _query: &str) -> Result<Vec<Hit>, ToolError> {
            tokio::time::sleep(Duration::from_secs(600)).await;
            Ok(Vec::new())
        }
    }

    fn hit(url: &str) -> Hit {
        Hit {
            title: "Title".into(),
            url: url.into(),
            snippet: "Snippet".into(),
        }
    }

    fn tool(hits: Vec<Hit>) -> WebSearchTool {
        WebSearchTool::new(Arc::new(Canned(Ok(hits))))
    }

    async fn text(tool: &WebSearchTool, input: Value) -> String {
        let out = tool.call(input, &context()).await.expect("search");
        out.parts[0].as_text().unwrap_or_default().to_string()
    }

    #[test]
    fn the_spec_advertises_the_query_and_the_two_domain_lists() {
        let spec = tool(Vec::new()).spec();
        assert_eq!(spec.name, "WebSearch");
        assert!(spec.input_schema["properties"]["query"]["description"].is_string());
        assert!(spec.input_schema["properties"]["allowed_domains"].is_object());
        assert!(spec.input_schema["properties"]["blocked_domains"].is_object());
        assert_eq!(spec.input_schema["required"], serde_json::json!(["query"]));
    }

    #[test]
    fn a_search_reads_and_changes_nothing() {
        let traits = tool(Vec::new()).traits(&Value::Null);
        assert!(traits.read_only && traits.concurrency_safe && traits.trusted);
        assert_eq!(traits.interrupt, bingo_sdk::Interrupt::Cancel);
    }

    #[tokio::test]
    async fn results_reach_the_model_as_a_numbered_list() {
        let tool = tool(vec![hit("https://a.example/1"), hit("https://b.example/2")]);
        let out = text(&tool, serde_json::json!({ "query": "rust async" })).await;
        assert!(out.starts_with("Results for \"rust async\":"), "got {out}");
        assert!(out.contains("1. Title — https://a.example/1"), "got {out}");
        assert!(out.contains("2. Title — https://b.example/2"), "got {out}");
    }

    #[tokio::test]
    async fn the_domain_lists_are_honoured() {
        let tool = tool(vec![hit("https://a.example/1"), hit("https://b.example/2")]);
        let allowed = text(
            &tool,
            serde_json::json!({ "query": "rust", "allowed_domains": ["a.example"] }),
        )
        .await;
        assert!(allowed.contains("a.example"), "got {allowed}");
        assert!(!allowed.contains("b.example"), "got {allowed}");

        let blocked = text(
            &tool,
            serde_json::json!({ "query": "rust", "blocked_domains": ["a.example"] }),
        )
        .await;
        assert!(!blocked.contains("a.example"), "got {blocked}");
        assert!(blocked.contains("b.example"), "got {blocked}");
    }

    #[tokio::test]
    async fn finding_nothing_is_an_answer_and_not_an_error() {
        let out = tool(Vec::new())
            .call(serde_json::json!({ "query": "nothing" }), &context())
            .await
            .expect("search");
        assert_eq!(out.parts[0].as_text(), Some("No results for \"nothing\"."));
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn a_query_too_short_to_mean_anything_is_invalid_input() {
        let error = tool(Vec::new())
            .call(serde_json::json!({ "query": " a " }), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::InvalidInput(m)) if m.contains("at least")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn what_the_backend_failed_at_is_what_the_model_is_told() {
        let tool = WebSearchTool::new(Arc::new(Canned(Err("the key was rejected"))));
        let error = tool
            .call(serde_json::json!({ "query": "rust" }), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m == "the key was rejected"),
            "got {error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_backend_that_does_not_answer_is_given_twenty_seconds() {
        let tool = WebSearchTool::new(Arc::new(Silent));
        let error = tool
            .call(serde_json::json!({ "query": "rust" }), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("longer than 20 seconds")),
            "got {error:?}"
        );
    }
}
