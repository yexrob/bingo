//! How the agent's own session id comes back after a restart.
//!
//! It is journaled once as an extension (ADR-0035 §3), and a session re-states
//! every extension it holds onto the stream when it starts. So this plugin
//! listens rather than asks: asking would mean opening the session, and a
//! starting session answers nothing but its summary until its start hooks are
//! done — which is a deadlock when the thing asking *is* one of them.
//!
//! The same ear hears a session end, and the adapter child ends with it.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Event, Frame, Hook, HookContext, HookMatcher, HookPoint};

use crate::session::{self, Sessions};

pub struct Ear {
    sessions: Arc<Sessions>,
}

impl Ear {
    pub fn new(sessions: Arc<Sessions>) -> Self {
        Self { sessions }
    }
}

#[async_trait]
impl Hook for Ear {
    fn id(&self) -> &str {
        "acp.journal"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Event],
            tool: None,
        }
    }

    async fn on_event(&self, frame: &Frame, _cx: &HookContext) {
        match &frame.event {
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == session::PLUGIN => {
                let Some(adapter) = kind.strip_prefix(session::KIND_PREFIX) else {
                    return;
                };
                if let Some(acp) = session::session_id_from(payload) {
                    self.sessions.remember(&frame.session, adapter, acp).await;
                }
            }
            // The child dies with the session it was opened for.
            Event::SessionClosed { .. } => self.sessions.forget(&frame.session).await,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{CloseReason, Env, SessionId};
    use jiff::Timestamp;
    use serde_json::json;

    fn frame(session: &SessionId, event: Event) -> Frame {
        Frame {
            seq: bingo_sdk::Seq(1),
            ts: Timestamp::now(),
            session: session.clone(),
            cause: None,
            event,
        }
    }

    fn cx(session: &SessionId) -> HookContext {
        HookContext {
            session: session.clone(),
            turn: None,
            cwd: std::env::temp_dir(),
            provider: None,
            model: None,
            host: bingo_sdk::HostHandle(Arc::new(bingo_sdk::testing::NoHost)),
        }
    }

    fn sessions() -> Arc<Sessions> {
        Sessions::new(Env::rooted(std::env::temp_dir()))
    }

    /// A session start re-states what it holds; that restatement is the whole
    /// of how the id comes back.
    #[tokio::test]
    async fn a_restated_extension_is_how_the_id_comes_back() {
        let pool = sessions();
        let ear = Ear::new(pool.clone());
        let session = SessionId::mint();
        ear.on_event(
            &frame(
                &session,
                Event::Extension {
                    plugin: session::PLUGIN.into(),
                    kind: session::kind("claude"),
                    payload: json!({ "sessionId": "sess_abc", "adapter": "claude" }),
                },
            ),
            &cx(&session),
        )
        .await;
        assert_eq!(
            pool.known_id(&session, "claude").await.as_deref(),
            Some("sess_abc")
        );
    }

    /// Another plugin's state is not ours to read, and a payload with no id in
    /// it says nothing.
    #[tokio::test]
    async fn nothing_else_on_the_stream_is_mistaken_for_ours() {
        let pool = sessions();
        let ear = Ear::new(pool.clone());
        let session = SessionId::mint();
        for event in [
            Event::Extension {
                plugin: "bingo.rooms".into(),
                kind: session::kind("claude"),
                payload: json!({ "sessionId": "not-ours" }),
            },
            Event::Extension {
                plugin: session::PLUGIN.into(),
                kind: "something-else".into(),
                payload: json!({ "sessionId": "wrong-kind" }),
            },
            Event::Extension {
                plugin: session::PLUGIN.into(),
                kind: session::kind("claude"),
                payload: json!({ "adapter": "claude" }),
            },
        ] {
            ear.on_event(&frame(&session, event), &cx(&session)).await;
        }
        assert_eq!(pool.known_id(&session, "claude").await, None);
    }

    #[tokio::test]
    async fn a_session_that_ended_keeps_no_child_and_no_id() {
        let pool = sessions();
        let ear = Ear::new(pool.clone());
        let session = SessionId::mint();
        pool.remember(&session, "claude", "sess_abc").await;
        ear.on_event(
            &frame(
                &session,
                Event::SessionClosed {
                    reason: CloseReason::Client,
                },
            ),
            &cx(&session),
        )
        .await;
        assert_eq!(pool.known_id(&session, "claude").await, None);
    }
}
