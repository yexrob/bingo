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
pub mod pictures;
pub mod posted;
pub mod send;
pub mod token;
pub mod upload;
pub mod ws;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use bingo_sdk::CancellationToken;
use serde_json::{Value, json};

use crate::adapter::{
    Acknowledge, Buttons, ChannelAdapter, Edit, Files, Inbox, Mark, Mode, Outcome, Outgoing,
    Threads,
};
use crate::conversation::{Conversation, Posted};
use crate::error::ChannelError;
use crate::limits::{Dialect, Encoding, Limits};
use crate::question::Question;
use api::{Api, ApiError};
use posted::Handle;
use send::Queue;
use upload::{Endpoint, Route};

/// Who this bot is, asked once at startup: there is no `is_mentioned` flag on
/// an event, only a list of mentions to look ourselves up in.
const WHOAMI: &str = "/open-apis/bot/v3/info";
const MESSAGES: &str = "/open-apis/im/v1/messages";
const IMAGES: &str = "/open-apis/im/v1/images";
const FILES: &str = "/open-apis/im/v1/files";
const REACTIONS: &str = "reactions";
const CARDS: &str = "/open-apis/cardkit/v1/cards";

/// A card is capped at 30 KB serialised, and JSON escaping is not free, so the
/// text this surface will put in one stops short of it.
const MAX_TEXT: usize = 20_000;

/// The sign that the bot is working, and the one a failure leaves behind.
/// Both are keys from Feishu's own emoji list; a key it does not know is
/// refused whole.
/// <https://open.feishu.cn/document/server-docs/im-v1/message-reaction/emojis-introduce>
const WORKING: &str = "Typing";
const FAILED: &str = "CrossMark";

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

    /// Post under the message that started this where there is one, and as a
    /// message of its own where there is not — what a reply already does.
    async fn deliver(
        &self,
        to: &Conversation,
        parent: Option<&Posted>,
        kind: &str,
        content: Value,
    ) -> Result<Handle, ChannelError> {
        match parent {
            Some(parent) => self.post_reply(to, parent, kind, content).await,
            None => self.post(to, kind, content).await,
        }
    }

    /// The bytes up, and the content of the message that will carry them back.
    async fn uploaded(&self, route: &Route, file: &Outgoing) -> Result<Value, ChannelError> {
        match route.endpoint {
            Endpoint::Image => {
                let key = self
                    .upload(IMAGES, &[("image_type", route.file_type)], "image", file)
                    .await?;
                Ok(json!({ "image_key": key }))
            }
            Endpoint::File => {
                let fields = [("file_type", route.file_type), ("file_name", &*file.name)];
                let key = self.upload(FILES, &fields, "file", file).await?;
                Ok(json!({ "file_key": key }))
            }
        }
    }

    /// One multipart upload, and the single key its answer is worth. Both
    /// endpoints answer `data.<field>_key` and nothing else is read.
    async fn upload(
        &self,
        path: &str,
        fields: &[(&str, &str)],
        field: &str,
        file: &Outgoing,
    ) -> Result<String, ChannelError> {
        let (content_type, body) =
            upload::multipart(&boundary(), fields, (field, &file.name, &file.bytes));
        let answer = self.api.post_multipart(path, &content_type, body).await?;
        let key = format!("{field}_key");
        answer["data"][&key]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ChannelError::Platform(format!("feishu kept no {key}")))
    }

    /// One emoji on one message.
    async fn react(&self, message_id: &str, emoji: &str) -> Result<Value, ChannelError> {
        let body = json!({ "reaction_type": { "emoji_type": emoji } });
        let path = format!("{MESSAGES}/{message_id}/{REACTIONS}");
        Ok(self.api.post(&path, body).await?)
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

/// A boundary the body it delimits cannot contain: the nanosecond it was
/// minted, which no file being uploaded has a copy of.
fn boundary() -> String {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("----bingo{nonce:x}")
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

    fn files(&self) -> Option<&dyn Files> {
        Some(self)
    }

    fn acknowledge(&self) -> Option<&dyn Acknowledge> {
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
        // `settings` is a JSON *string*, not an object: the endpoint answers
        // an object with 9499 and the card never closes, which costs the
        // whole answer a second time as a plain message.
        // <https://open.feishu.cn/document/cardkit-v1/card/settings>
        let body = json!({
            "settings": json!({ "config": { "streaming_mode": false } }).to_string(),
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

#[async_trait]
impl Files for Feishu {
    /// The bytes go up first and come back as a key; the message that carries
    /// the key is a message like any other, so it queues per chat with the
    /// rest and hangs under whatever a reply would (ADR-0051 §3).
    ///
    /// A caption is its own text message after the file, not a `post` around
    /// it: a picture inside a rich post is not a picture a person can open
    /// full-screen, and the words are worth more than the layout.
    async fn post(
        &self,
        to: &Conversation,
        parent: Option<&Posted>,
        file: Outgoing,
    ) -> Result<Posted, ChannelError> {
        let route = upload::route(&file.name, &file.bytes);
        let content = self.uploaded(&route, &file).await?;
        let handle = self.deliver(to, parent, route.msg_type, content).await?;
        if let Some(caption) = &file.caption {
            self.deliver(to, parent, "text", json!({ "text": caption }))
                .await?;
        }
        Ok(handle.posted())
    }
}

/// A reaction on the message that spoke, taken off when the turn ends and a
/// `CrossMark` left in its place when that turn failed (ADR-0051 §5). Wanting
/// `im:message.reactions:write_only`.
/// <https://open.feishu.cn/document/server-docs/im-v1/message-reaction/create>
/// <https://open.feishu.cn/document/server-docs/im-v1/message-reaction/delete>
#[async_trait]
impl Acknowledge for Feishu {
    async fn begin(&self, at: &Posted) -> Result<Mark, ChannelError> {
        let answer = self.react(&reactable(at)?, WORKING).await?;
        answer["data"]["reaction_id"]
            .as_str()
            .map(|id| Mark(id.to_string()))
            .ok_or_else(|| ChannelError::Platform("feishu added no reaction".into()))
    }

    /// The cross is added after the sign comes off and is left in place: it is
    /// the record that this message's turn went wrong.
    async fn end(&self, at: &Posted, mark: Mark, outcome: Outcome) -> Result<(), ChannelError> {
        let message_id = reactable(at)?;
        let path = format!("{MESSAGES}/{message_id}/{REACTIONS}/{}", mark.0);
        self.api.delete(&path).await?;
        if outcome == Outcome::Failed {
            self.react(&message_id, FAILED).await?;
        }
        Ok(())
    }
}

/// Only a message carries reactions. A card sent by its id is not one, and
/// nothing this surface knows can turn it into one.
fn reactable(at: &Posted) -> Result<String, ChannelError> {
    match Handle::of(at) {
        Some(Handle::Message(id)) => Ok(id),
        _ => Err(ChannelError::Unsupported("reacting to a card")),
    }
}

#[cfg(test)]
mod tests;
