//! `WebFetch`: one page, read as markdown.
//!
//! The traits are the interesting part. A fetch is `trusted` and cancellable,
//! and it claims `read_only` for exactly the calls whose canonical URL is on the
//! documentation list — that claim is what carries a docs lookup through the
//! default gate while every other host still asks.

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Preview, ResultLimit, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use futures::StreamExt;
use reqwest::{Client, Response, header};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::body::Content;
use crate::cache::Cache;
use crate::canonical::Canonical;
use crate::{approved, body, output, picture};

/// A body past this is not a document the turn wanted.
const MAX_BYTES: usize = 10 * 1024 * 1024;

/// What the request will take, in the order it prefers to be answered:
/// documents first, then pictures, which the tool reads as pictures.
const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain,\
application/json;q=0.9,image/*;q=0.8,*/*;q=0.7";

const DESCRIPTION: &str = "\
Fetch a URL and return the page as markdown. `http` URLs are upgraded to \
`https`. An HTML page comes back as its article — navigation, scripts and page \
chrome are dropped — rewritten as markdown; text and JSON documents come back \
as they are; a picture comes back as the picture itself, which you see and \
which is placed in the user's transcript beside this call, where their surface \
can draw it; any other content type is refused by name. The same page fetched \
again within fifteen minutes is answered from an in-process cache. Long pages \
are truncated, and say so on the last line. For GitHub URLs prefer the `gh` \
CLI through Bash.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchArgs {
    /// Absolute URL of the page to read, e.g. `https://docs.rs/tokio/latest/tokio/`.
    pub url: String,
}

#[derive(Debug)]
pub struct WebFetchTool {
    http: Client,
    cache: Cache,
}

impl WebFetchTool {
    pub fn new(http: Client) -> Self {
        Self {
            http,
            cache: Cache::default(),
        }
    }

    /// The URL a call names, as the gate, the cache and the request all see it.
    fn target(input: &Value) -> Option<Canonical> {
        let args: FetchArgs = serde_json::from_value(input.clone()).ok()?;
        Canonical::parse(&args.url).ok()
    }

    /// What the URL holds, from the cache when it is still fresh and from the
    /// network otherwise.
    async fn retrieve(&self, url: &Canonical) -> Result<ToolOutput, ToolError> {
        if let Some(cached) = self.cache.get(url.as_str()) {
            return Ok(ToolOutput::text(cached));
        }
        let response = self.get(url).await?;
        let content_type = content_type(&response);
        let bytes = read_capped(response).await?;
        match body::render(&content_type, &bytes, url.as_str())
            .map_err(|e| ToolError::Failed(e.to_string()))?
        {
            Content::Page(markdown) => Ok(ToolOutput::text(self.kept(url, &markdown))),
            Content::Picture(image) => Ok(picture::output(image)),
        }
    }

    /// The page as the model will read it, kept for the next quarter hour.
    /// Only text is kept: a picture is already bounded by the journal's cap,
    /// and a second copy of it in this process costs more than fetching it
    /// again.
    fn kept(&self, url: &Canonical, markdown: &str) -> String {
        let page = output::cap(markdown);
        self.cache.put(url.as_str(), &page);
        page
    }

    async fn get(&self, url: &Canonical) -> Result<Response, ToolError> {
        let response = self
            .http
            .get(url.as_str())
            .header(header::ACCEPT, ACCEPT)
            .send()
            .await
            .map_err(|e| ToolError::Failed(format!("fetching {url} failed: {e}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::Failed(format!(
                "fetching {url} failed: HTTP {}",
                status.as_u16()
            )));
        }
        Ok(response)
    }
}

fn content_type(response: &Response) -> String {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Read the body chunk by chunk and stop at the cap. Reading it whole first
/// would mean holding in memory exactly what the cap exists to refuse.
async fn read_capped(response: Response) -> Result<Vec<u8>, ToolError> {
    let mut stream = response.bytes_stream();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| ToolError::Failed(format!("reading the body failed: {e}")))?;
        if body.len() + chunk.len() > MAX_BYTES {
            return Err(ToolError::Failed(format!(
                "the page is larger than the {MAX_BYTES} byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "WebFetch".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<FetchArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, input: &Value) -> ToolTraits {
        ToolTraits {
            trusted: true,
            concurrency_safe: true,
            read_only: Self::target(input).is_some_and(|url| approved::is_documentation(&url)),
            // The page is already capped at a hundred thousand characters; the
            // kernel's global clip would cut it again at half that.
            result_limit: ResultLimit::SelfBounded,
            ..ToolTraits::default()
        }
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        Self::target(input)
            .map(|url| {
                vec![Subject::Url {
                    url: url.to_string(),
                }]
            })
            .unwrap_or_default()
    }

    fn preview(&self, input: &Value, _cwd: &Path) -> Option<Preview> {
        Self::target(input).map(|url| Preview::Url {
            url: url.to_string(),
        })
    }

    async fn call(&self, input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: FetchArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let url = Canonical::parse(&args.url)
            .map_err(|e| ToolError::InvalidInput(format!("invalid url: {e}")))?;
        self.retrieve(&url).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ContentPart, Image};

    use crate::tests::context;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PAGE: &str = "<html><body><article><h1>Title</h1>\
        <p>A paragraph with a <a href=\"https://example.com/x\">link</a> in it, long \
        enough that the extractor keeps the article rather than deciding the page \
        holds nothing worth reading.</p></article></body></html>";

    fn tool() -> WebFetchTool {
        WebFetchTool::new(Client::new())
    }

    fn args(url: &str) -> Value {
        serde_json::json!({ "url": url })
    }

    /// One page, served at `/page`, answered at most `expected` times.
    async fn serve(server: &MockServer, response: ResponseTemplate, expected: u64) {
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(response)
            .expect(expected)
            .mount(server)
            .await;
    }

    /// `set_body_raw`, not `set_body_string`: the latter would overwrite the
    /// content type with `text/plain` and the tool would never see HTML.
    fn html_page() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(PAGE, "text/html; charset=utf-8")
    }

    #[test]
    fn the_spec_advertises_one_url_argument() {
        let spec = tool().spec();
        assert_eq!(spec.name, "WebFetch");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema["properties"]["url"]["description"].is_string());
        assert!(spec.input_schema["properties"].get("prompt").is_none());
    }

    #[test]
    fn the_subject_and_the_preview_are_the_canonical_url() {
        let input = args("http://Example.com/docs");
        assert_eq!(
            tool().subjects(&input, Path::new("/work")),
            vec![Subject::Url {
                url: "https://example.com/docs".into()
            }]
        );
        assert_eq!(
            tool().preview(&input, Path::new("/work")),
            Some(Preview::Url {
                url: "https://example.com/docs".into()
            })
        );
    }

    #[test]
    fn a_url_that_does_not_parse_names_no_subject() {
        assert!(
            tool()
                .subjects(&args("not a url"), Path::new("/"))
                .is_empty()
        );
        assert!(tool().preview(&args("not a url"), Path::new("/")).is_none());
    }

    #[test]
    fn a_fetch_is_trusted_cancellable_and_safe_beside_other_calls() {
        let traits = tool().traits(&args("https://example.com/"));
        assert!(traits.trusted && traits.concurrency_safe);
        assert_eq!(traits.result_limit, ResultLimit::SelfBounded);
        assert!(!traits.destructive && !traits.edit);
    }

    #[test]
    fn only_the_documentation_list_is_claimed_read_only() {
        let read_only = |url: &str| tool().traits(&args(url)).read_only;
        assert!(read_only("https://docs.rs/tokio/latest/tokio/"));
        assert!(read_only(
            "https://github.com/anthropics/anthropic-sdk-python"
        ));
        assert!(!read_only("https://example.com/"));
        assert!(!read_only("https://github.com/other/repo"));
        assert!(!read_only("not a url"));
    }

    #[tokio::test]
    async fn a_page_comes_back_as_markdown() {
        let server = MockServer::start().await;
        serve(&server, html_page(), 1).await;
        let cx = context();

        let out = tool()
            .call(args(&format!("{}/page", server.uri())), &cx)
            .await
            .expect("fetch");
        let text = out.parts[0].as_text().expect("text").to_string();
        assert!(text.contains("# Title"), "got {text}");
        assert!(text.contains("[link](https://example.com/x)"), "got {text}");
        assert!(!out.is_error);
        assert!(out.display.is_none());
    }

    #[tokio::test]
    async fn a_second_fetch_within_the_ttl_never_reaches_the_server() {
        let server = MockServer::start().await;
        serve(&server, html_page(), 1).await;
        let tool = tool();
        let cx = context();
        let input = args(&format!("{}/page", server.uri()));

        let first = tool.call(input.clone(), &cx).await.expect("fetch");
        let second = tool.call(input, &cx).await.expect("cached fetch");
        assert_eq!(first.parts, second.parts);
    }

    #[tokio::test]
    async fn a_body_over_the_cap_fails_and_nothing_is_kept() {
        let server = MockServer::start().await;
        let oversized = ResponseTemplate::new(200)
            .insert_header("content-type", "text/plain")
            .set_body_string("a".repeat(MAX_BYTES + 1));
        serve(&server, oversized, 1).await;
        let tool = tool();
        let url = format!("{}/page", server.uri());

        let error = tool.call(args(&url), &context()).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("larger than")),
            "got {error:?}"
        );
        assert_eq!(tool.cache.get(&url), None);
    }

    /// The request says it takes pictures, and says so after the types it
    /// would rather have.
    #[test]
    fn the_request_asks_for_pictures_after_the_documents() {
        let documents = ACCEPT.find("text/html").expect("html is asked for");
        let pictures = ACCEPT.find("image/*").expect("pictures are asked for");
        assert!(documents < pictures, "{ACCEPT}");
    }

    /// A picture URL reaches the model as the picture, and nothing of it is
    /// kept: the second call goes to the server again, which is why the mock
    /// expects two.
    #[tokio::test]
    async fn a_picture_comes_back_as_the_picture_and_is_never_cached() {
        let server = MockServer::start().await;
        let bytes = bingo_pictures::testing::png_bytes(6, 4);
        let png = ResponseTemplate::new(200).set_body_raw(bytes.clone(), "image/png");
        serve(&server, png, 2).await;
        let tool = tool();
        let url = format!("{}/page", server.uri());

        let out = tool.call(args(&url), &context()).await.expect("fetch");
        assert_eq!(
            out.parts,
            vec![ContentPart::Image(
                Image::from_bytes("image/png", &bytes).expect("within the cap")
            )]
        );
        assert!(!out.is_error);
        assert_eq!(tool.cache.get(&url), None, "a picture is not cached");
        tool.call(args(&url), &context())
            .await
            .expect("a second fetch reaches the server");
    }

    /// The `Content-Type` is a claim and the bytes are the evidence: a page
    /// served as a PNG is refused rather than journaled as a picture.
    #[tokio::test]
    async fn a_body_served_as_a_picture_that_is_not_one_is_refused() {
        let server = MockServer::start().await;
        let lie = ResponseTemplate::new(200).set_body_raw(PAGE, "image/png");
        serve(&server, lie, 1).await;

        let error = tool()
            .call(args(&format!("{}/page", server.uri())), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("not a picture")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_content_type_that_is_not_text_is_refused_by_name() {
        let server = MockServer::start().await;
        let pdf = ResponseTemplate::new(200).set_body_raw("%PDF-1.7", "application/pdf");
        serve(&server, pdf, 1).await;

        let error = tool()
            .call(args(&format!("{}/page", server.uri())), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("application/pdf")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_status_that_is_not_a_success_names_the_status() {
        let server = MockServer::start().await;
        serve(&server, ResponseTemplate::new(404), 1).await;

        let error = tool()
            .call(args(&format!("{}/page", server.uri())), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("HTTP 404")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_url_that_fails_validation_never_reaches_the_network() {
        let error = tool()
            .call(args("https://user:secret@example.com/"), &context())
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::InvalidInput(m)) if m.contains("credentials")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        let error = tool().call(serde_json::json!({}), &context()).await.err();
        assert!(matches!(error, Some(ToolError::InvalidInput(_))));
    }
}
