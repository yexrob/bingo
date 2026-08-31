//! The Feishu adapter (ADR-0016 §6): the first real platform.
//!
//! Every mechanism it hands over is the one the platform actually has, which
//! is not the obvious one. Editing is **not** `PUT /im/v1/messages/:id` — that
//! is capped at twenty edits per message for the life of the message, which
//! works in a demo and dies in a week. It is CardKit: a card entity, sent by
//! id, then updated with the whole text under a sequence that only ever goes
//! up. Buttons are a card of their own, because callbacks do not fire while a
//! card is streaming.
//!
//! The credentials never come from the settings file: the app id is public and
//! lives there, the secret comes from the environment.

pub mod api;
pub mod bootstrap;
pub mod card;
pub mod chunks;
pub mod event;
pub mod frame;
pub mod posted;
pub mod send;
pub mod token;
pub mod ws;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bingo_sdk::CancellationToken;
use serde_json::{Value, json};

use crate::adapter::{Buttons, ChannelAdapter, Edit, Inbox, Mode, Threads};
use crate::conversation::{Conversation, Posted};
use crate::error::ChannelError;
use crate::limits::{Dialect, Encoding, Limits};
use crate::question::Question;
use api::{Api, ApiError};
use posted::Handle;
use send::Queue;

/// Who this bot is, asked once at startup: there is no `is_mentioned` flag on
/// an event, only a list of mentions to look ourselves up in.
const WHOAMI: &str = "/open-apis/bot/v3/info";
const MESSAGES: &str = "/open-apis/im/v1/messages";
const CARDS: &str = "/open-apis/cardkit/v1/cards";

/// A card is capped at 30 KB serialised, and JSON escaping is not free, so the
/// text this surface will put in one stops short of it.
const MAX_TEXT: usize = 20_000;

pub struct Config {
    pub app_id: String,
    pub app_secret: String,
    /// Where the API lives. Overridable so a test can be Feishu.
    pub base: String,
}

pub struct Feishu {
    api: Api,
    app_secret: String,
    limits: Limits,
    queue: Queue,
    /// This bot's own open id, once `run` has asked for it.
    me: Mutex<String>,
    /// The next `sequence` for each streaming card. Strictly increasing per
    /// card and never rewound, not even after a failed update.
    sequences: Mutex<HashMap<String, u64>>,
    /// Which chat each thing we posted went to, so its queue can be found
    /// again from an edit that carries only the handle.
    chats: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for Feishu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feishu").field("api", &self.api).finish()
    }
}

impl Feishu {
    pub const ID: &'static str = "feishu";

    pub fn new(config: Config) -> Self {
        Self {
            api: Api::new(config.base, &config.app_id, &config.app_secret),
            app_secret: config.app_secret,
            limits: Limits {
                max_text: (MAX_TEXT, Encoding::Utf8Bytes),
                dialect: Dialect::Markdown,
                // Four buttons is what a permission ladder needs and what a
                // card row shows without wrapping.
                max_actions: 4,
                max_label: 30,
            },
            queue: Queue::default(),
            me: Mutex::new(String::new()),
            sequences: Mutex::new(HashMap::new()),
            chats: Mutex::new(HashMap::new()),
        }
    }

    async fn whoami(&self) -> Result<String, ChannelError> {
        let answer = self.api.get(WHOAMI).await?;
        answer["bot"]["open_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ChannelError::Refused("feishu did not say who this bot is".into()))
    }

    /// Post one message to a chat, waiting for that chat's turn first.
    async fn post(
        &self,
        to: &Conversation,
        kind: &str,
        content: Value,
    ) -> Result<Handle, ChannelError> {
        self.queue.turn(&to.chat).await;
        let body = json!({
            "receive_id": to.chat,
            "msg_type": kind,
            "content": content.to_string(),
        });
        let path = format!("{MESSAGES}?receive_id_type=chat_id");
        let answer = self.api.post(&path, body).await?;
        Ok(Handle::Message(message_id(&answer)?))
    }

    /// The same, hung under a message so the platform keeps the thread.
    async fn post_reply(
        &self,
        to: &Conversation,
        parent: &Posted,
        kind: &str,
        content: Value,
    ) -> Result<Handle, ChannelError> {
        let Some(Handle::Message(parent)) = Handle::of(parent) else {
            return self.post(to, kind, content).await;
        };
        self.queue.turn(&to.chat).await;
        let body = json!({ "msg_type": kind, "content": content.to_string() });
        let answer = self
            .api
            .post(&format!("{MESSAGES}/{parent}/reply"), body)
            .await?;
        Ok(Handle::Message(message_id(&answer)?))
    }

    /// A card entity, sent to the chat by id. What comes back is the card, not
    /// the message: `im/v1` cannot touch a card sent this way, and CardKit can.
    async fn open_card(
        &self,
        to: &Conversation,
        parent: Option<&Posted>,
    ) -> Result<Handle, ChannelError> {
        let created = self
            .api
            .post(CARDS, card::entity(&card::streaming()))
            .await?;
        let card_id = created["data"]["card_id"]
            .as_str()
            .ok_or_else(|| ChannelError::Platform("feishu created no card".into()))?
            .to_string();
        let content = card::by_id(&card_id);
        match parent {
            Some(parent) => self.post_reply(to, parent, "interactive", content).await?,
            None => self.post(to, "interactive", content).await?,
        };
        self.remember(&card_id, &to.chat);
        Ok(Handle::Card(card_id))
    }

    fn remember(&self, id: &str, chat: &str) {
        locked(&self.chats).insert(id.to_string(), chat.to_string());
    }

    fn chat_of(&self, id: &str) -> Option<String> {
        locked(&self.chats).get(id).cloned()
    }

    /// The next sequence for a card. Never rewound: the platform refuses an
    /// update that goes backwards, so a failed one still spends its number.
    fn sequence(&self, card_id: &str) -> u64 {
        let mut sequences = locked(&self.sequences);
        let next = sequences.entry(card_id.to_string()).or_insert(1);
        let sequence = *next;
        *next += 1;
        sequence
    }

    /// Write the whole text into a streaming card. The platform diffs it, so
    /// a partial update would replace rather than extend (ADR-0016 §6).
    async fn write(&self, card_id: &str, text: &str) -> Result<(), ChannelError> {
        if let Some(chat) = self.chat_of(card_id) {
            self.queue.turn(&chat).await;
        }
        let sequence = self.sequence(card_id);
        let path = format!("{CARDS}/{card_id}/elements/{}/content", card::ANSWER);
        let body = json!({
            "content": text,
            "sequence": sequence,
            "uuid": format!("{card_id}-{sequence}"),
        });
        self.spend(self.api.put(&path, body).await)
    }

    /// A rate limit or a busy card costs this frame, not the stream.
    fn spend(&self, outcome: Result<Value, ApiError>) -> Result<(), ChannelError> {
        match outcome {
            Ok(_) => Ok(()),
            Err(error) if error.transient() => {
                tracing::debug!(%error, "a streamed frame was dropped");
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn message_id(answer: &Value) -> Result<String, ChannelError> {
    answer["data"]["message_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ChannelError::Platform("feishu sent no message".into()))
}

fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[async_trait]
impl ChannelAdapter for Feishu {
    fn id(&self) -> &str {
        Self::ID
    }

    fn limits(&self) -> &Limits {
        &self.limits
    }

    fn credential(&self) -> String {
        self.api.app_id().to_string()
    }

    async fn run(&self, inbox: Inbox, cancel: CancellationToken) -> Result<(), ChannelError> {
        // A credential that is missing is refused here rather than at
        // registration: an unconfigured chat must not stop `bingo --print`.
        if self.api.app_id().is_empty() || self.app_secret.is_empty() {
            return Err(ChannelError::Refused(format!(
                "the feishu channel needs an app: set {} and {}",
                crate::settings::APP_ID,
                crate::settings::APP_SECRET
            )));
        }
        let me = self.whoami().await?;
        *locked(&self.me) = me.clone();
        ws::listen(&self.api, &self.app_secret, &me, &inbox, &cancel).await
    }

    async fn send(
        &self,
        to: &Conversation,
        text: &str,
        mode: Mode,
    ) -> Result<Posted, ChannelError> {
        let handle = match mode {
            Mode::Stream => self.open_card(to, None).await?,
            Mode::Once => self.post(to, "text", json!({ "text": text })).await?,
        };
        Ok(handle.posted())
    }

    fn edit(&self) -> Option<&dyn Edit> {
        Some(self)
    }

    fn buttons(&self) -> Option<&dyn Buttons> {
        Some(self)
    }

    /// None: a card that is visibly writing itself is the affordance, and
    /// Feishu has no typing indicator for a bot anyway.
    fn typing(&self) -> Option<&dyn crate::adapter::Typing> {
        None
    }

    fn threads(&self) -> Option<&dyn Threads> {
        Some(self)
    }
}

#[async_trait]
impl Edit for Feishu {
    async fn replace(&self, at: &Posted, text: &str) -> Result<(), ChannelError> {
        let Some(Handle::Card(card_id)) = Handle::of(at) else {
            return Err(ChannelError::Unsupported("editing a plain message"));
        };
        self.write(&card_id, text).await
    }

    /// The last text, then streaming off — which is also what re-opens the
    /// card to callbacks and stops the ten-minute clock.
    async fn finish(&self, at: &Posted, text: &str) -> Result<(), ChannelError> {
        let Some(Handle::Card(card_id)) = Handle::of(at) else {
            return Err(ChannelError::Unsupported("editing a plain message"));
        };
        self.write(&card_id, text).await?;
        let sequence = self.sequence(&card_id);
        let body = json!({
            "settings": { "config": { "streaming_mode": false } },
            "sequence": sequence,
            "uuid": format!("{card_id}-{sequence}"),
        });
        self.spend(
            self.api
                .patch(&format!("{CARDS}/{card_id}/settings"), body)
                .await,
        )
    }
}

#[async_trait]
impl Buttons for Feishu {
    /// Its own card, sent in full rather than by id: a card sent by id cannot
    /// be edited through `im/v1`, and this one has to be, to settle it.
    async fn ask(&self, to: &Conversation, question: &Question) -> Result<Posted, ChannelError> {
        let content = card::question(to, question, &self.limits);
        let handle = self.post(to, "interactive", content).await?;
        if let Handle::Message(id) = &handle {
            self.remember(id, &to.chat);
        }
        Ok(handle.posted())
    }

    async fn settle(
        &self,
        at: &Posted,
        question: &Question,
        outcome: &str,
    ) -> Result<(), ChannelError> {
        let Some(Handle::Message(message_id)) = Handle::of(at) else {
            return Err(ChannelError::Unsupported("editing a card by its id"));
        };
        if let Some(chat) = self.chat_of(&message_id) {
            self.queue.turn(&chat).await;
        }
        let content = card::settled(&question.prompt, outcome);
        let body = json!({ "content": content.to_string() });
        self.api
            .patch(&format!("{MESSAGES}/{message_id}"), body)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl Threads for Feishu {
    async fn reply(
        &self,
        to: &Conversation,
        parent: &Posted,
        text: &str,
        mode: Mode,
    ) -> Result<Posted, ChannelError> {
        let handle = match mode {
            Mode::Stream => self.open_card(to, Some(parent)).await?,
            Mode::Once => {
                self.post_reply(to, parent, "text", json!({ "text": text }))
                    .await?
            }
        };
        Ok(handle.posted())
    }
}

#[cfg(test)]
mod tests;
