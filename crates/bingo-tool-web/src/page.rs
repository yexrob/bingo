//! `ShowPage`: a page in the person's browser, and what it posts back.
//!
//! It is the multiple-choice question's bigger sibling and shares its one hard
//! trait — it holds the turn on a person, so nothing runs beside it. Unlike a
//! question it is not read-only: what it opens is a window on someone's screen
//! running HTML this turn wrote, so the gate asks once before it opens
//! (ADR-0042). The socket, the token and the document are `bingo_loopback`'s;
//! what is here is the three ways a page ends.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
// `page` is what this module is called; `served` is what the library calls the
// same noun, and naming it so keeps the two apart at every use.
use bingo_loopback::page as served;
use bingo_loopback::{Answer, Loopback, LoopbackError, Token, browser, serve};
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// How long a page waits for a person when the model names nothing, and the
/// longest it may wait when it does: a page is someone's attention, and a call
/// that waits an afternoon is a turn nobody can finish.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);

const DESCRIPTION: &str = "\
Show the user a web page and wait for what they choose on it. Use it when a \
multiple-choice question is not enough: layouts or designs to compare side by \
side, more than four options, a form with several fields, a multi-step choice, \
or anything where seeing beats reading. For a plain one-of-four question ask a \
multiple-choice question instead, and where a sensible default exists take it \
rather than interrupt at all.\n\
\n\
Write `html` as one self-contained page: inline every style and script, embed \
pictures as data URLs, and load nothing from the network. The page must call \
`window.bingo.submit(value)` with a plain JSON value — an object naming what \
was chosen reads best — when the user is done, and may call \
`window.bingo.cancel()` if they decline. Both are defined for you already: do \
not write them, fetch them, or post anywhere yourself. The result is the JSON \
the page submitted. It is served on this machine only, opens in the user's \
browser, and stops the moment it answers — nothing in it is hosted or shared.";

/// An unknown field is refused rather than defaulted: a misspelled
/// `timeout_secs` would otherwise cost the person ten silent minutes.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageArgs {
    /// What the page is for, in a few words: the browser tab's title.
    pub title: String,
    /// The page itself: one self-contained HTML document that calls
    /// `window.bingo.submit(value)` when the user is done.
    pub html: String,
    /// How long to wait for the user, in seconds. 600 by default, 3600 at most.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Who opens the URL: the person's browser, or a scripted client in a test —
/// the only way a page answers with nobody there to click.
pub type Opener = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct ShowPageTool {
    open: Opener,
}

impl std::fmt::Debug for ShowPageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowPageTool").finish_non_exhaustive()
    }
}

impl Default for ShowPageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ShowPageTool {
    pub fn new() -> Self {
        Self {
            open: Arc::new(browser::open),
        }
    }

    /// The seam the tests reach through, and nothing else.
    pub fn opened_by(open: Opener) -> Self {
        Self { open }
    }
}

#[async_trait]
impl Tool for ShowPageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ShowPage".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<PageArgs>(),
            meta: Default::default(),
        }
    }

    /// It reads nothing and writes nothing, and it holds the turn on a person,
    /// so nothing else may run beside it.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            concurrency_safe: false,
            read_only: false,
            trusted: true,
            ..ToolTraits::default()
        }
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: PageArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let waiting = waited(args.timeout_secs)?;
        let loopback = Loopback::any().await.map_err(failed)?;
        let token = Token::mint().map_err(failed)?;
        let url = served::url(loopback.port(), &token);
        // The one place a person sees the URL while the call is open: the row's
        // live line. It is also the fallback when no browser opened.
        cx.progress(format!("{} — {url}", args.title.trim()));
        if !(self.open)(&url) {
            return Err(ToolError::Failed(format!(
                "no browser opened on this machine. The page is waiting at {url}: \
                 ask the user to open it, or ask a multiple-choice question instead."
            )));
        }
        let document = served::document(&args.title, &args.html);
        ended(
            serve::until_answered(loopback, &token, &document),
            cx,
            waiting,
            &url,
        )
        .await
    }
}

/// The three ways a page ends: the person answers it, the turn is interrupted,
/// or nobody came. The interrupt is first, so an `esc` that has already
/// happened is never overtaken by a page arriving in the same breath.
async fn ended(
    serving: impl Future<Output = Result<Answer, LoopbackError>>,
    cx: &ToolContext,
    waiting: Duration,
    url: &str,
) -> Result<ToolOutput, ToolError> {
    tokio::select! {
        biased;
        () = cx.cancel.cancelled() => Err(ToolError::Cancelled),
        answer = serving => answered(answer.map_err(failed)?),
        () = tokio::time::sleep(waiting) => Err(ToolError::Failed(format!(
            "the page at {url} was not answered within {}s",
            waiting.as_secs()
        ))),
    }
}

fn answered(answer: Answer) -> Result<ToolOutput, ToolError> {
    match answer {
        Answer::Submitted(value) => Ok(ToolOutput::text(format!(
            "The page answered:\n{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        ))),
        Answer::Cancelled => Ok(ToolOutput::error(
            "The user closed the page without answering.",
        )),
    }
}

/// Seconds the model may name, bounded at both ends.
fn waited(seconds: Option<u64>) -> Result<Duration, ToolError> {
    let Some(seconds) = seconds else {
        return Ok(DEFAULT_TIMEOUT);
    };
    let asked = Duration::from_secs(seconds);
    if asked.is_zero() || asked > MAX_TIMEOUT {
        return Err(ToolError::InvalidInput(format!(
            "timeout_secs is 1 to {}, not {seconds}",
            MAX_TIMEOUT.as_secs()
        )));
    }
    Ok(asked)
}

fn failed(error: LoopbackError) -> ToolError {
    ToolError::Failed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    use crate::tests::context;

    fn args(html: &str) -> Value {
        serde_json::json!({ "title": "Three layouts", "html": html })
    }

    const PAGE: &str = "<body><button onclick=\"bingo.submit({picked:'b'})\">b</button></body>";

    /// The browser stand-in: it fetches the page the way a browser would, keeps
    /// what it was served for the test to read, then posts what a person
    /// clicking in the page would have posted.
    fn opened_by(answer: Value, seen: Arc<Mutex<String>>) -> Opener {
        Arc::new(move |url: &str| {
            let (url, answer, seen) = (url.to_string(), answer.clone(), Arc::clone(&seen));
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                if let Ok(response) = http.get(&url).send().await
                    && let Ok(html) = response.text().await
                {
                    *seen.lock().expect("the served page") = html;
                }
                let _ = http
                    .post(format!("{url}/answer"))
                    .json(&answer)
                    .send()
                    .await;
            });
            true
        })
    }

    /// One call, with a scripted browser: the output, and what the browser saw.
    async fn shown(html: &str, answer: Value) -> (Result<ToolOutput, ToolError>, String) {
        let seen = Arc::new(Mutex::new(String::new()));
        let tool = ShowPageTool::opened_by(opened_by(answer, Arc::clone(&seen)));
        let out = tool.call(args(html), &context()).await;
        let page = seen.lock().expect("the served page").clone();
        (out, page)
    }

    #[test]
    fn the_spec_advertises_a_title_a_page_and_a_bounded_wait() {
        let spec = ShowPageTool::new().spec();
        assert_eq!(spec.name, "ShowPage");
        assert_eq!(spec.input_schema["type"], "object");
        for field in ["title", "html", "timeout_secs"] {
            assert!(
                spec.input_schema["properties"][field]["description"].is_string(),
                "{field} is undescribed in {:?}",
                spec.input_schema["properties"]
            );
        }
        assert_eq!(
            spec.input_schema["required"],
            serde_json::json!(["title", "html"])
        );
    }

    #[test]
    fn the_description_tells_the_model_how_a_page_answers() {
        assert!(DESCRIPTION.contains("window.bingo.submit"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("window.bingo.cancel"), "{DESCRIPTION}");
        assert!(DESCRIPTION.contains("self-contained"), "{DESCRIPTION}");
    }

    /// It holds the turn on a person like a question does, and unlike a
    /// question it is not read-only: the gate asks before a window opens.
    #[test]
    fn a_page_holds_the_turn_and_is_never_waved_through() {
        let traits = ShowPageTool::new().traits(&Value::Null);
        assert!(!traits.concurrency_safe && !traits.read_only);
        assert!(traits.trusted && !traits.destructive && !traits.edit);
        assert!(
            ShowPageTool::new()
                .subjects(&Value::Null, Path::new("/"))
                .is_empty()
        );
    }

    #[tokio::test]
    async fn what_the_page_submits_is_the_result() {
        let (out, page) = shown(PAGE, serde_json::json!({ "value": { "picked": "b" } })).await;
        let out = out.expect("an answered page");
        let text = out.parts[0].as_text().expect("text").to_string();
        assert!(text.starts_with("The page answered:\n"), "{text}");
        assert!(text.contains("\"picked\": \"b\""), "{text}");
        assert!(!out.is_error);
        // The page the browser was served is the model's, with the script in it.
        assert!(page.contains("bingo.submit({picked:'b'})"), "{page}");
        assert!(page.contains("window.bingo"), "{page}");
        assert!(page.contains("<title>Three layouts</title>"), "{page}");
    }

    #[tokio::test]
    async fn a_page_the_user_dismisses_answers_nothing_and_says_so() {
        let (out, _) = shown(PAGE, serde_json::json!({ "cancelled": true })).await;
        let out = out.expect("a dismissed page is not a failure");
        assert!(out.is_error);
        assert_eq!(
            out.parts[0].as_text(),
            Some("The user closed the page without answering.")
        );
    }

    /// Fail closed, at once, with the URL: a person at the machine can still
    /// open it, and the turn is not left holding a browser that never came.
    #[tokio::test]
    async fn a_machine_with_no_browser_fails_at_once_with_the_url() {
        let tool = ShowPageTool::opened_by(Arc::new(|_url: &str| false));
        let error = tool.call(args(PAGE), &context()).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m))
                if m.contains("http://127.0.0.1:") && m.contains("no browser opened")),
            "got {error:?}"
        );
    }

    /// One `esc` drops a page like any other call in flight.
    #[tokio::test]
    async fn an_interrupted_turn_drops_the_page() {
        let cx = context();
        let cancel = cx.cancel.clone();
        let tool = ShowPageTool::opened_by(Arc::new(move |_url: &str| {
            cancel.cancel();
            true
        }));
        let error = tool.call(args(PAGE), &cx).await.err();
        assert!(matches!(error, Some(ToolError::Cancelled)), "got {error:?}");
    }

    /// Paused time, so the ten minutes cost the suite nothing.
    #[tokio::test(start_paused = true)]
    async fn a_page_nobody_answers_times_out_naming_the_url() {
        let tool = ShowPageTool::opened_by(Arc::new(|_url: &str| true));
        let error = tool.call(args(PAGE), &context()).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m))
                if m.contains("was not answered within 600s") && m.contains("http://127.0.0.1:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_wait_the_model_names_is_bounded_at_both_ends() {
        let tool = ShowPageTool::opened_by(Arc::new(|_url: &str| true));
        for seconds in [0, 3601] {
            let mut input = args(PAGE);
            input["timeout_secs"] = serde_json::json!(seconds);
            let error = tool.call(input, &context()).await.err();
            assert!(
                matches!(&error, Some(ToolError::InvalidInput(m)) if m.contains("timeout_secs")),
                "{seconds}s: got {error:?}"
            );
        }
        assert_eq!(waited(None).expect("a default"), DEFAULT_TIMEOUT);
        assert_eq!(
            waited(Some(30)).expect("a named wait"),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        for input in [
            serde_json::json!({ "html": "<p>x</p>" }),
            serde_json::json!({ "title": "t" }),
            // A field nobody claims would otherwise be a silent ten minutes.
            serde_json::json!({ "title": "t", "html": "<p>x</p>", "timeoutSecs": 5 }),
        ] {
            let error = ShowPageTool::new()
                .call(input.clone(), &context())
                .await
                .err();
            assert!(
                matches!(error, Some(ToolError::InvalidInput(_))),
                "{input}: {error:?}"
            );
        }
    }
}
