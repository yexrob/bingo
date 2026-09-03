//! The doors bingo opens for one ACP session (ADR-0036 §§1–2).
//!
//! The transport side of the bridge is written against
//! [`Doors`](crate::bridge::Doors) and knows nothing of turns or kernels; this
//! is the other side of that seam, and the only thing in the crate that holds
//! a `HostHandle`.
//!
//! Two verbs and one fact between them. The fact is the offer, and it is not
//! kept here: it is the tool list of the request being served, cached as each
//! request goes past and read from the tools catalogue before the first one
//! arrives. A tool added to the house therefore reaches the agent the same
//! turn it reaches any other model, with nothing edited here.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bingo_sdk::{CatalogKind, HostHandle, Level, SessionId, ToolCall, ToolOutput, ToolSpec};
use tokio::sync::Mutex;

use crate::bridge::doors::{Doors, Refused};
use crate::bridge::offer;

/// The code a person sees when their row named a tool nothing answers to.
const UNKNOWN: &str = "ACP_TOOLS";

/// One ACP session's share of bingo's tools.
pub struct Shared {
    host: HostHandle,
    session: SessionId,
    /// The row's own name, for anything that has to be said about it.
    adapter: String,
    /// The offer this row chose for itself, if it chose one (ADR-0036 §6).
    chosen: Option<Vec<String>>,
    /// The servers the agent dials itself. Their tools are already in its
    /// hands, so they are not ours to serve (§4).
    forwarded: BTreeSet<String>,
    /// The tool list of the last request served for this session. `None` until
    /// the first one, when the catalogue answers instead.
    latest: Mutex<Option<Vec<ToolSpec>>>,
    /// Whether a row's unanswerable names have been said. Once: the offer is
    /// asked for on every `tools/list`.
    told: AtomicBool,
}

impl Shared {
    pub fn new(
        host: HostHandle,
        session: SessionId,
        adapter: &str,
        chosen: Option<Vec<String>>,
        forwarded: BTreeSet<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            session,
            adapter: adapter.to_string(),
            chosen,
            forwarded,
            latest: Mutex::new(None),
            told: AtomicBool::new(false),
        })
    }

    /// Take in the tool list of a request about to be served, and say whether
    /// what the agent may call has moved. A caller that hears `true` tells the
    /// bridge, which is what `tools/list_changed` is made of.
    pub async fn observe(&self, tools: &[ToolSpec]) -> bool {
        let offered = self.keep(tools);
        let mut latest = self.latest.lock().await;
        let moved = latest.as_deref().map(|held| self.keep(held)) != Some(offered);
        *latest = Some(tools.to_vec());
        moved
    }

    /// What the agent may call, given everything the turn was given.
    fn keep(&self, specs: &[ToolSpec]) -> Vec<ToolSpec> {
        match &self.chosen {
            Some(names) => offer::chosen(specs, names).0,
            None => offer::derived(specs, &self.forwarded),
        }
    }

    /// The tools the house has now, for a session whose first request has not
    /// come yet. An unreadable catalogue is an empty offer rather than a
    /// failure: the first request fills it in a moment.
    async fn catalogued(&self) -> Vec<ToolSpec> {
        match self.host.catalog(CatalogKind::Tools).await {
            Ok(catalog) => offer::from_catalog(catalog.entries),
            Err(_) => Vec::new(),
        }
    }

    /// A row that named tools nothing answers to is told which, once. The
    /// names are the person's own word and are not refused (ADR-0036 §6) —
    /// but a silent one is a tool they think their agent has.
    async fn say_unanswered(&self, specs: &[ToolSpec]) {
        let Some(names) = &self.chosen else { return };
        if self.told.swap(true, Ordering::AcqRel) {
            return;
        }
        let missing = offer::chosen(specs, names).1;
        if missing.is_empty() {
            return;
        }
        let _ = self
            .host
            .notice(
                Level::Warn,
                UNKNOWN,
                &format!(
                    "acp.adapters.{}.tools names {}, which no tool answers to; \
                     the rest were offered",
                    self.adapter,
                    missing.join(", ")
                ),
            )
            .await;
    }
}

#[async_trait]
impl Doors for Shared {
    async fn offer(&self) -> Vec<ToolSpec> {
        let held = self.latest.lock().await.clone();
        let specs = match held {
            Some(specs) => specs,
            None => self.catalogued().await,
        };
        self.say_unanswered(&specs).await;
        self.keep(&specs)
    }

    /// Straight through the kernel's own door: the turn that is asking serves
    /// the call, with its gate, its journal and a token that is a child of its
    /// own. What comes back is the tool's answer, `is_error` and all; what
    /// does not is the kernel's own reason, in its own words.
    async fn call(&self, call: ToolCall) -> Result<ToolOutput, Refused> {
        match self.host.invoke(&self.session, call).await {
            Ok(outcome) => Ok(outcome.output),
            Err(refused) => Err(Refused::new(refused.message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::testing::NoHost;
    use serde_json::json;

    fn spec(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("what {name} does"),
            input_schema: json!({ "type": "object" }),
            meta: serde_json::Map::new(),
        }
    }

    fn sourced(server: &str, tool: &str) -> ToolSpec {
        let mut spec = spec(&format!("mcp__{server}__{tool}"));
        spec.meta.insert("server".into(), json!(server));
        spec
    }

    fn shared(chosen: Option<Vec<String>>, forwarded: &[&str]) -> Arc<Shared> {
        Shared::new(
            HostHandle(Arc::new(NoHost)),
            SessionId::mint(),
            "scripted",
            chosen,
            forwarded.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn names(specs: &[ToolSpec]) -> Vec<&str> {
        specs.iter().map(|spec| spec.name.as_str()).collect()
    }

    /// Before a request has been served there is no request to read, and a
    /// host with no catalogue offers nothing rather than failing.
    #[tokio::test]
    async fn a_session_nobody_has_prompted_yet_offers_what_the_catalogue_holds() {
        assert!(shared(None, &[]).offer().await.is_empty());
    }

    /// The offer converges on the first request and follows every one after
    /// it: no list is kept here (ADR-0036 §1).
    #[tokio::test]
    async fn the_offer_is_the_last_requests_own_tool_list() {
        let doors = shared(None, &[]);
        doors.observe(&[spec("SendMessage"), spec("Read")]).await;
        assert_eq!(names(&doors.offer().await), ["SendMessage"]);

        doors
            .observe(&[spec("SendMessage"), spec("TaskCreate")])
            .await;
        assert_eq!(names(&doors.offer().await), ["SendMessage", "TaskCreate"]);
    }

    /// A request whose offer is the one already being served is not news; one
    /// that changes it is, and that is what a `tools/list_changed` is.
    #[tokio::test]
    async fn only_a_moved_offer_is_worth_telling_the_agent_about() {
        let doors = shared(None, &[]);
        assert!(
            doors.observe(&[spec("SendMessage")]).await,
            "the first request is always news"
        );
        assert!(!doors.observe(&[spec("SendMessage")]).await);
        assert!(
            !doors.observe(&[spec("SendMessage"), spec("Read")]).await,
            "a tool that never crosses moving does not move the offer"
        );
        assert!(doors.observe(&[spec("SendMessage"), spec("Sing")]).await);
    }

    /// A forwarded server's tools are the agent's own already.
    #[tokio::test]
    async fn a_forwarded_servers_tools_are_not_offered_over_the_bridge() {
        let doors = shared(None, &["files"]);
        doors
            .observe(&[
                spec("SendMessage"),
                sourced("files", "read"),
                sourced("weather", "today"),
            ])
            .await;
        assert_eq!(
            names(&doors.offer().await),
            ["SendMessage", "mcp__weather__today"]
        );
    }

    /// A row that chose gets what it chose, and the derivation stands aside.
    #[tokio::test]
    async fn a_row_that_chose_is_offered_what_it_chose() {
        let doors = shared(Some(vec!["Read".into()]), &[]);
        doors.observe(&[spec("SendMessage"), spec("Read")]).await;
        assert_eq!(names(&doors.offer().await), ["Read"]);
    }

    /// No turn is in flight for a session nothing is running, and the agent is
    /// told why in the kernel's own words.
    #[tokio::test]
    async fn a_call_with_nothing_to_serve_it_is_refused_with_a_reason() {
        let refused = shared(None, &[])
            .call(ToolCall {
                call_id: "acp_0_0".into(),
                name: "SendMessage".into(),
                input: json!({}),
            })
            .await
            .expect_err("a refusal");
        assert!(!refused.0.is_empty(), "a refusal says something");
    }
}
