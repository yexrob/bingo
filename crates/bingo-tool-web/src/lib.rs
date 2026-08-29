//! Web tools: one page read as markdown.
//!
//! The HTTP client is built here — the timeout, the redirect bound and the user
//! agent are one fact, and a connection pool is worth more than one of them.

mod approved;
mod body;
mod cache;
mod canonical;
mod fetch;
mod output;
mod readable;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{Plugin, PluginError, PluginManifest, Registrar, Tool};
use reqwest::Client;

pub use fetch::{FetchArgs, WebFetchTool};

/// What one request may take, from the connection to the last byte.
const TIMEOUT: Duration = Duration::from_secs(60);

/// Hops a fetch follows before it decides the page is not coming.
const MAX_REDIRECTS: usize = 10;

/// A site serves something thinner to a caller that announces itself as a
/// robot, so the client asks the way a browser asks.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tools.web",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["tool:WebFetch"],
    requires: &[],
    config: None,
};

/// Registers `WebFetch`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WebPlugin;

#[async_trait]
impl Plugin for WebPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let http = client().map_err(|e| PluginError::Failed(format!("the http client: {e}")))?;
        registrar.tool(Arc::new(WebFetchTool::new(http)) as Arc<dyn Tool>);
        Ok(())
    }
}

/// The client the tools make their requests with.
pub fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::any::Any;
    use std::path::PathBuf;

    use bingo_sdk::{
        Answer, AnswerSpec, CancellationToken, Contribution, Env, Input, IntentId, InteractionKind,
        ItemBody, ItemId, KernelError, Prompter, SessionId, SessionSpec, ToolContext, ToolHost,
        TurnId,
    };

    /// A tool host that answers nothing: the web tools reach none of it.
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

        async fn spawn_session(&self, _spec: SessionSpec) -> Result<SessionId, KernelError> {
            Ok(SessionId::from_raw("ses_test"))
        }

        fn submit(&self, _to: &SessionId, _intent: IntentId, _input: Input) {}

        fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
            None
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
            host: Arc::new(NullHost),
        }
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
}
