//! Web tools: one page read as markdown, and a search for pages.
//!
//! Both share one HTTP client, built here — the timeout, the redirect bound and
//! the user agent are one fact, and a connection pool is worth more than two of
//! them. Which service answers a search is the plugin's one setting.

mod approved;
mod backend;
mod body;
mod brave;
mod cache;
mod canonical;
mod duckduckgo;
mod fetch;
mod hits;
mod html_text;
mod output;
mod picture;
mod readable;
mod search;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{ConfigClaim, Merge, Plugin, PluginError, PluginManifest, Registrar, Tool};
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;

pub use backend::{Hit, SearchBackend};
pub use brave::Brave;
pub use duckduckgo::DuckDuckGo;
pub use fetch::{FetchArgs, WebFetchTool};
pub use search::{SearchArgs, WebSearchTool};

/// What one request may take, from the connection to the last byte.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Hops a fetch follows before it decides the page is not coming.
const MAX_REDIRECTS: usize = 10;

/// Both endpoints serve something thinner to a caller that announces itself as
/// a robot, so the client asks the way a browser asks.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

const BRAVE_KEY_ENV: &str = "BRAVE_API_KEY";

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tools.web",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["tool:WebFetch", "tool:WebSearch"],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[("web", Merge::Replace)],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub web: Web,
}

/// A typo here would silently leave a configured backend unused, so an unknown
/// key is a startup failure rather than a silence.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Web {
    /// Which service answers a search.
    #[serde(default)]
    pub search: Backend,
    /// The Brave Search subscription key. `BRAVE_API_KEY` overrides it.
    #[serde(default)]
    pub brave_api_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Keyless, and the default for that reason.
    #[default]
    Duckduckgo,
    Brave,
}

/// Registers `WebFetch` and `WebSearch`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WebPlugin;

#[async_trait]
impl Plugin for WebPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let http = client().map_err(|e| PluginError::Failed(format!("the http client: {e}")))?;
        let searcher = backend(&settings.web, brave_key(&settings.web), http.clone())?;
        registrar.tool(Arc::new(WebFetchTool::new(http)) as Arc<dyn Tool>);
        registrar.tool(Arc::new(WebSearchTool::new(searcher)) as Arc<dyn Tool>);
        Ok(())
    }
}

/// The client both tools make their requests with.
pub fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
}

/// A backend with no key to give is a search that fails on every call; saying
/// so at startup is the only place a person can act on it.
fn backend(
    web: &Web,
    brave_key: Option<String>,
    http: Client,
) -> Result<Arc<dyn SearchBackend>, PluginError> {
    match web.search {
        Backend::Duckduckgo => {
            let duckduckgo = DuckDuckGo::new(http)
                .map_err(|e| PluginError::Failed(format!("the result pattern: {e}")))?;
            Ok(Arc::new(duckduckgo))
        }
        Backend::Brave => {
            let key = brave_key.ok_or_else(|| {
                PluginError::Config(format!(
                    "web.search is \"brave\" but there is no key: set {BRAVE_KEY_ENV} or \
                     web.braveApiKey"
                ))
            })?;
            Ok(Arc::new(Brave::new(http, key)))
        }
    }
}

fn brave_key(web: &Web) -> Option<String> {
    resolve(std::env::var(BRAVE_KEY_ENV).ok(), web.brave_api_key.clone())
}

/// The environment first: a key exported in this shell is the one a person just
/// chose, and it never has to be written to a file to be used.
fn resolve(from_env: Option<String>, from_settings: Option<String>) -> Option<String> {
    [from_env, from_settings]
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use std::path::PathBuf;

    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Contribution, Env, InteractionKind, ItemBody,
        ItemId, KernelError, Prompter, SessionId, ToolContext, ToolHost, TurnId,
    };

    /// A tool host that answers nothing: neither web tool reaches any of it.
    #[derive(Debug)]
    struct NullHost;

    #[async_trait]
    impl Prompter for NullHost {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            Ok(Answer::Cancel)
        }
    }

    #[async_trait]
    impl ToolHost for NullHost {
        fn progress(&self, _item: &ItemId, _tail: String) {}

        async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
            Ok(ItemId::from_raw("itm_test"))
        }
    }

    pub(crate) fn context() -> ToolContext {
        ToolContext {
            call_id: "call_test".into(),
            session: SessionId::from_raw("ses_test"),
            turn: TurnId::from_raw("trn_test"),
            item: ItemId::from_raw("itm_test"),
            cwd: PathBuf::from("/tmp"),
            cancel: CancellationToken::new(),
            env: Arc::new(Env::rooted("/tmp")),
            host: bingo_sdk::testing::NoHost::handle(),
            call: Arc::new(NullHost),
        }
    }

    fn settings(value: serde_json::Value) -> Result<Settings, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn the_plugin_registers_every_tool_its_manifest_promises() {
        let mut registrar = Registrar::new(
            MANIFEST.id,
            serde_json::json!({}),
            bingo_sdk::Env::rooted("/tmp"),
        );
        WebPlugin.register(&mut registrar).expect("register");
        let names: Vec<String> = registrar
            .into_contributions()
            .iter()
            .map(|c| match c {
                Contribution::Tool(tool) => tool.spec().name.clone(),
                other => panic!("expected a tool, got {other:?}"),
            })
            .collect();
        let promised: Vec<String> = MANIFEST
            .provides
            .iter()
            .map(|p| p.trim_start_matches("tool:").to_string())
            .collect();
        assert_eq!(names, promised);
    }

    #[test]
    fn an_empty_slice_searches_without_a_key() {
        let settings = settings(serde_json::json!({})).expect("defaults");
        assert_eq!(settings.web.search, Backend::Duckduckgo);
        assert!(settings.web.brave_api_key.is_none());
    }

    #[test]
    fn the_backend_is_named_in_the_settings_as_the_model_of_it_is_written() {
        let settings =
            settings(serde_json::json!({ "web": { "search": "brave" } })).expect("a named backend");
        assert_eq!(settings.web.search, Backend::Brave);
    }

    #[test]
    fn a_key_that_is_not_claimed_is_a_startup_failure() {
        let error = settings(serde_json::json!({ "web": { "braveApiKeys": "x" } })).err();
        assert!(error.is_some(), "an unknown key deserialized");
    }

    #[test]
    fn brave_without_a_key_says_where_to_put_one() {
        let web = Web {
            search: Backend::Brave,
            brave_api_key: None,
        };
        let error = backend(&web, None, Client::new()).err();
        assert!(
            matches!(&error, Some(PluginError::Config(m))
                if m.contains("BRAVE_API_KEY") && m.contains("web.braveApiKey")),
            "got {error:?}"
        );
    }

    #[test]
    fn brave_with_a_key_is_the_backend_that_answers() {
        let web = Web {
            search: Backend::Brave,
            brave_api_key: Some("key".into()),
        };
        let backend = backend(&web, Some("key".into()), Client::new()).expect("a backend");
        assert!(format!("{backend:?}").starts_with("Brave"));
    }

    #[test]
    fn the_environment_beats_the_settings_and_blank_counts_as_absent() {
        assert_eq!(
            resolve(Some("from-env".into()), Some("from-file".into())),
            Some("from-env".to_string())
        );
        assert_eq!(
            resolve(Some("  ".into()), Some("from-file".into())),
            Some("from-file".to_string())
        );
        assert_eq!(resolve(None, None), None);
    }
}
