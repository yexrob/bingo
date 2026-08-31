//! Where a session lives on a platform, and the handle a platform gives back
//! for something this surface posted.

/// One chat, or one topic thread inside one chat. The adapter's id is not
/// here: the host knows which adapter an inbound event came from, and holding
/// it twice would let the two disagree.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Conversation {
    pub chat: String,
    /// A thread the platform gives an id of its own; a reply that merely hangs
    /// under a message is not one (ADR-0016 §4).
    pub thread: Option<String>,
    /// Several people can speak here, so it engages only on a mention.
    pub group: bool,
}

impl Conversation {
    pub fn direct(chat: impl Into<String>) -> Self {
        Self {
            chat: chat.into(),
            thread: None,
            group: false,
        }
    }

    pub fn group(chat: impl Into<String>) -> Self {
        Self {
            group: true,
            ..Self::direct(chat)
        }
    }

    pub fn in_thread(self, thread: impl Into<String>) -> Self {
        Self {
            thread: Some(thread.into()),
            ..self
        }
    }

    /// The session key's tail: `<chat>[/<thread>]`. The adapter's id is the
    /// head, added by the host, which owns the whole key (ADR-0016 §4).
    pub fn path(&self) -> String {
        match &self.thread {
            Some(thread) => format!("{}/{thread}", self.chat),
            None => self.chat.clone(),
        }
    }
}

/// What a platform calls something this surface posted. Opaque: only the
/// adapter that minted it knows whether it is a message id, a card id, or
/// both — so only that adapter ever takes it apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posted(pub String);

impl Posted {
    pub fn new(id: impl Into<String>) -> Self {
        Posted(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Posted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_extends_its_chats_path_and_a_chat_alone_is_the_path() {
        assert_eq!(Conversation::direct("oc_1").path(), "oc_1");
        assert_eq!(
            Conversation::group("oc_1").in_thread("omt_9").path(),
            "oc_1/omt_9"
        );
    }
}
