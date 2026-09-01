//! What the bridge has to tell a person, and the one drain that says it.
//!
//! A plugin process dies, or refuses the handshake, or a hook never decides,
//! or a plugin says a line of its own through `bingo.host` — all in places
//! that hold no session: `Plugin::start` and a `ToolSource` read (ADR-0009 §1)
//! are handed neither one nor a way to reach a stream. So a notice waits here
//! until something says it.
//!
//! What says it is [`drain`], one task the manager starts with the host it was
//! given (ADR-0033 §4). It runs on its own: a notice is said the moment it is
//! pushed, whatever the bridge is doing, and a session whose plugin serves no
//! tool at all hears it like any other — which is the defect M29 carried, and
//! the reason there is exactly one drain and not one per crossing. A line
//! nobody is open to hear yet is kept, not lost, and said to the first session
//! that opens. Every one is logged as well, so a notice nothing ever drains is
//! not a notice nobody could have read.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bingo_sdk::{CancellationToken, GatewayEvent, HostHandle, ItemBody, Level};
use futures::{StreamExt, stream};
use tokio::sync::Notify;

/// How many wait at once. A plugin that dies in a loop must not grow this;
/// past the cap the oldest goes, because the newest is the one that explains
/// what the person is looking at.
const CAP: usize = 16;

/// What a plugin's own line is filed under. One code, because a plugin's
/// words are the plugin's; the bridge's own notices name what happened.
pub const PLUGIN_SAID: &str = "PLUGIN_NOTICE";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    pub code: String,
    pub text: String,
}

impl Notice {
    pub fn warn(code: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            level: Level::Warn,
            code: code.into(),
            text: text.into(),
        }
    }

    /// A line a plugin asked to have said, under the name it is installed as:
    /// the person reads who is talking to them before they read what was said.
    pub fn said(speaker: &str, level: Level, message: &str) -> Self {
        Self {
            level,
            code: PLUGIN_SAID.to_string(),
            text: format!("{speaker}: {message}"),
        }
    }

    pub fn body(self) -> ItemBody {
        ItemBody::Notice {
            level: self.level,
            code: self.code,
            text: self.text,
        }
    }
}

#[derive(Debug, Default)]
pub struct Notices {
    waiting: Mutex<VecDeque<Notice>>,
    /// Woken by every push, so the drain says a line when it happens rather
    /// than when something else happens to look.
    arrived: Notify,
}

impl Notices {
    pub fn push(&self, notice: Notice) {
        tracing::warn!(code = %notice.code, text = %notice.text, "plugin bridge");
        let mut waiting = self.lock();
        if waiting.len() == CAP {
            waiting.pop_front();
        }
        waiting.push_back(notice);
        drop(waiting);
        self.arrived.notify_one();
    }

    /// Everything unsaid, and now said.
    pub fn drain(&self) -> Vec<Notice> {
        self.lock().drain(..).collect()
    }

    /// Everything waiting, said through the host. A line nobody is open to
    /// hear is kept where it was — a person who has no session open has not
    /// missed it, they have not got there yet.
    pub async fn say(&self, host: &HostHandle) {
        let mut unheard = Vec::new();
        for notice in self.drain() {
            if host
                .notice(notice.level, &notice.code, &notice.text)
                .await
                .is_err()
            {
                unheard.push(notice);
            }
        }
        self.keep(unheard);
    }

    /// Back to the front, in the order they were said in. Already logged, so
    /// one that no longer fits is old news rather than lost news.
    fn keep(&self, unheard: Vec<Notice>) {
        let mut waiting = self.lock();
        for notice in unheard.into_iter().rev() {
            if waiting.len() == CAP {
                break;
            }
            waiting.push_front(notice);
        }
    }

    async fn arrived(&self) {
        self.arrived.notified().await;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Notice>> {
        // A panic elsewhere must not hide a notice from the person.
        self.waiting.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// The one drain: say what is waiting, then wait for the next thing that could
/// change the answer — a notice pushed, or a session opening for one that had
/// nobody to hear it. Ends when the manager stops.
pub async fn drain(notices: Arc<Notices>, host: HostHandle, stop: CancellationToken) {
    // Chained with a stream that never ends, so a host whose gateway closes
    // leaves this waiting on pushes instead of spinning on `None`.
    let mut sessions = Box::pin(
        host.gateway_events()
            .chain(stream::pending::<GatewayEvent>()),
    );
    loop {
        notices.say(&host).await;
        tokio::select! {
            () = stop.cancelled() => return,
            () = notices.arrived() => {}
            _ = sessions.next() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_waits_until_it_is_drained_and_then_is_gone() {
        let notices = Notices::default();
        notices.push(Notice::warn("PLUGIN_DIED", "wordcount ended"));
        assert_eq!(notices.drain().len(), 1);
        assert!(notices.drain().is_empty());
    }

    #[test]
    fn a_notice_becomes_the_transcript_body_a_surface_renders() {
        let body = Notice::warn("PLUGIN_DIED", "wordcount ended").body();
        assert_eq!(
            body,
            ItemBody::Notice {
                level: Level::Warn,
                code: "PLUGIN_DIED".into(),
                text: "wordcount ended".into()
            }
        );
    }

    #[test]
    fn a_plugin_s_own_line_is_said_under_the_name_it_is_installed_as() {
        let said = Notice::said("wordcount", Level::Info, "the index is stale");
        assert_eq!(said.level, Level::Info);
        assert_eq!(said.code, PLUGIN_SAID);
        assert_eq!(said.text, "wordcount: the index is stale");
    }

    #[test]
    fn a_plugin_that_dies_in_a_loop_keeps_the_newest_not_the_oldest() {
        let notices = Notices::default();
        for n in 0..CAP + 3 {
            notices.push(Notice::warn("PLUGIN_DIED", format!("death {n}")));
        }
        let waiting = notices.drain();
        assert_eq!(waiting.len(), CAP);
        assert_eq!(waiting[0].text, "death 3");
        assert_eq!(waiting[CAP - 1].text, format!("death {}", CAP + 2));
    }

    /// The whole point of keeping rather than losing: a host with nobody
    /// listening changes nothing, and the line is still there for the session
    /// that opens next.
    #[tokio::test]
    async fn a_line_nobody_is_open_to_hear_waits_where_it_was() {
        let notices = Notices::default();
        notices.push(Notice::warn("PLUGIN_DIED", "one"));
        notices.push(Notice::warn("PLUGIN_DIED", "two"));
        notices.say(&bingo_sdk::testing::NoHost::handle()).await;
        let waiting = notices.drain();
        assert_eq!(
            waiting.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
            ["one", "two"],
            "in the order they were said in"
        );
    }
}
