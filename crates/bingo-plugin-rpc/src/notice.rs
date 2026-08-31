//! What the bridge has to tell a person, kept until something with a session
//! in reach can say it.
//!
//! A plugin process dies, or refuses the handshake, in places that hold no
//! session: `Plugin::start` and a `ToolSource` read (ADR-0009 §1) are handed
//! neither one nor a way to reach a stream. The one notice channel the sdk
//! offers a plugin is `ToolHost::record` inside a call, so a notice waits here
//! until the next bridge tool call records it. Every one is logged as well, so
//! a notice nothing ever drains is not a notice nobody could have read.

use std::collections::VecDeque;
use std::sync::Mutex;

use bingo_sdk::{ItemBody, Level};

/// How many wait at once. A plugin that dies in a loop must not grow this;
/// past the cap the oldest goes, because the newest is the one that explains
/// what the person is looking at.
const CAP: usize = 16;

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
}

impl Notices {
    pub fn push(&self, notice: Notice) {
        tracing::warn!(code = %notice.code, text = %notice.text, "plugin bridge");
        let mut waiting = self.lock();
        if waiting.len() == CAP {
            waiting.pop_front();
        }
        waiting.push_back(notice);
    }

    /// Everything unsaid, and now said.
    pub fn drain(&self) -> Vec<Notice> {
        self.lock().drain(..).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Notice>> {
        // A panic elsewhere must not hide a notice from the person.
        self.waiting.lock().unwrap_or_else(|held| held.into_inner())
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
}
