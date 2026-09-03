//! What an adapter serves, asked before anybody has said a word to it.
//!
//! An ACP agent declares its models when a session opens and at no other door
//! (ADR-0037 §2), so an instance nobody has prompted has nothing of the
//! agent's to serve. The answer is to open one on purpose: spawn the adapter,
//! shake hands, `session/new`, keep what the answer declared, drop the child.
//! Nothing is ever prompted, so it costs no model time — verified by hand
//! against codex-acp 1.8.0, whose `session/new` answers with the whole
//! catalogue. The agent keeps a session of its own on the far side that
//! nobody will ever use again; that is the price, and it is recorded rather
//! than cured (Plan M44).
//!
//! Nothing here is a second way of opening a child. The spawn, the handshake
//! and the `session/new` are [`crate::session`]'s own three steps, borrowed
//! whole. What this module adds is the rest of the sentence: where the answer
//! is kept, how long it may take, and what is said when it does not come.
//! What it leaves out is everything a *person's* session needs — the tool
//! bridge, the restore ladder, the journal, the row's own options. A session
//! that will never be prompted would read none of them, and the row's options
//! move a knob's value rather than the list of values, so skipping them
//! changes nothing about what is harvested.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bingo_sdk::{ModelInfo, ProviderError};
use tokio::sync::{Mutex, OnceCell};

use crate::config::Adapter;
use crate::knobs::Declared;
use crate::session::{Sessions, Where, handshake};

/// The code a person sees when a cold ask came to nothing.
pub const PROBE: &str = "ACP_PROBE";

/// How long the whole ask may take. A ceiling and not a wait: the usual answer
/// arrives in the time a process takes to start. It is generous because the
/// first-tier adapters are `npx` packages that fetch themselves on first use,
/// and because CI is slower than the desk this was written on.
const DEADLINE: Duration = Duration::from_secs(30);

/// What each adapter of this run has declared to a cold ask.
///
/// One cell per adapter, so two adapters asked together are two children at
/// once rather than one after the other, and two callers of the same adapter
/// are one child: whoever arrives second waits for the answer the first is
/// already getting.
#[derive(Default)]
pub struct Cold {
    asked: Mutex<BTreeMap<String, Arc<OnceCell<Answer>>>>,
}

impl Cold {
    /// What this adapter says it serves, asked at most once per run.
    ///
    /// Once whatever the answer: a failure is an answer too — `agent` alone,
    /// which is what an instance nobody can reach honestly serves — and a
    /// child per listing would be a spawn every time somebody opened a menu.
    /// How often a *process* asks again is the host's to decide: it asks an
    /// endpoint whose cached list is missing or a day old, and asks every one
    /// of them when a person says `/models refresh` (ADR-0026 §4).
    pub async fn models(
        &self,
        sessions: &Sessions,
        name: &str,
        adapter: &Adapter,
    ) -> Vec<ModelInfo> {
        let cell = self.cell(name).await;
        let answer = cell.get_or_init(|| ask(sessions, name, adapter)).await;
        answer.say(sessions, name).await;
        answer.declared.models()
    }

    async fn cell(&self, name: &str) -> Arc<OnceCell<Answer>> {
        self.asked
            .lock()
            .await
            .entry(name.to_string())
            .or_default()
            .clone()
    }
}

/// One adapter's cold answer: what it declared, and the word still owed to a
/// person if it declared nothing because nothing could be asked.
struct Answer {
    declared: Declared,
    /// Why the ask came to nothing, until somebody has been told. Kept rather
    /// than said and forgotten: the background top-up that finds an adapter
    /// missing usually runs before there is a session to say anything to, and
    /// a person who asks what an adapter serves and is answered `agent` is
    /// owed the reason.
    unheard: Mutex<Option<String>>,
}

impl Answer {
    fn of(declared: Declared) -> Self {
        Self {
            declared,
            unheard: Mutex::new(None),
        }
    }

    fn failed(why: String) -> Self {
        Self {
            declared: Declared::default(),
            unheard: Mutex::new(Some(why)),
        }
    }

    /// Said once — once *heard*, which is not the same thing.
    async fn say(&self, sessions: &Sessions, name: &str) {
        let mut unheard = self.unheard.lock().await;
        let Some(why) = unheard.as_deref() else {
            return;
        };
        if sessions.heard(PROBE, &could_not(name, why)).await {
            *unheard = None;
        }
    }
}

/// One cold ask, bounded, and what a failed one leaves: nothing of the
/// agent's, which the catalogue serves as its own `agent` label alone. Never
/// an error — a refresh that could not reach one adapter still answers for the
/// rest, and a person choosing a model must not be shown a failure where a
/// list belongs.
async fn ask(sessions: &Sessions, name: &str, adapter: &Adapter) -> Answer {
    match tokio::time::timeout(DEADLINE, harvest(sessions, name, adapter)).await {
        Ok(Ok(declared)) => Answer::of(declared),
        Ok(Err(why)) => Answer::failed(why.to_string()),
        Err(_) => Answer::failed(too_long()),
    }
}

/// Spawn, shake hands, open a session, keep what it declared. The child goes
/// when this returns: the connection and the process handle are locals here,
/// and dropping the handle ends the tree (`crate::child`).
async fn harvest(
    sessions: &Sessions,
    name: &str,
    adapter: &Adapter,
) -> Result<Declared, ProviderError> {
    let inbox = sessions.inbox(name, None).await;
    let cwd = sessions.env().home.clone();
    let (connection, _child) = sessions.spawn(adapter, &cwd, inbox)?;
    connection.call(handshake()).await?;
    Ok(sessions
        .fresh(&connection, &Where::bare(&cwd))
        .await?
        .declared)
}

fn too_long() -> String {
    format!("it did not answer within {} seconds", DEADLINE.as_secs())
}

/// A notice a person can act on: which row could not be started, in the row's
/// own words, and what the catalogue shows in the meantime.
fn could_not(name: &str, why: &str) -> String {
    format!(
        "{name} could not be asked what it serves: {why}. `agent` is the only \
         model listed for it until a session opens; `acp.adapters.{name}` is \
         where its command is."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A person whose adapter will not start reads this and knows which row to
    /// fix and what they are looking at until they do.
    #[test]
    fn what_a_failed_ask_says_names_the_row_and_what_is_served_instead() {
        let said = could_not("codex-acp", "could not start the ACP adapter `npx`");
        assert!(said.contains("acp.adapters.codex-acp"), "{said}");
        assert!(said.contains("`agent`"), "{said}");
        assert!(said.contains("npx"), "the adapter's own words: {said}");
        assert!(could_not("claude", &too_long()).contains("30 seconds"));
    }

    /// A row nothing can start is one ask and an empty answer — never an
    /// error, and never a second child for the next caller.
    #[tokio::test]
    async fn an_adapter_that_will_not_start_is_asked_once_and_serves_nothing() {
        let sessions = Sessions::new(bingo_sdk::Env::rooted(std::env::temp_dir()));
        let adapter: Adapter =
            serde_json::from_value(serde_json::json!({ "command": "bingo-no-such-adapter-xyz" }))
                .expect("an adapter");
        let cold = Cold::default();
        assert!(cold.models(&sessions, "missing", &adapter).await.is_empty());
        assert!(
            cold.asked.lock().await["missing"].initialized(),
            "the answer is kept, so nobody spawns again to be told the same"
        );
        assert!(cold.models(&sessions, "missing", &adapter).await.is_empty());
    }

    /// The reason is kept until it lands. This pool has no host at all, so
    /// nobody ever hears it, and it is still owed after the second asking.
    #[tokio::test]
    async fn a_reason_nobody_heard_is_still_owed() {
        let sessions = Sessions::new(bingo_sdk::Env::rooted(std::env::temp_dir()));
        let answer = Answer::failed("no such command".to_string());
        answer.say(&sessions, "missing").await;
        assert_eq!(
            answer.unheard.lock().await.as_deref(),
            Some("no such command"),
            "a notice nobody was there for was not said"
        );
        assert!(
            Answer::of(Declared::default())
                .unheard
                .lock()
                .await
                .is_none()
        );
    }
}
