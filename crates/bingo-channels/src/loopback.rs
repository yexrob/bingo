//! The adapter that is the contract (ADR-0016 §7).
//!
//! Every mechanism is switchable and every limit is settable, so one crate
//! proves both sides of every ladder: buttons and numbered replies, editing
//! and not editing, a group that engages only on a mention. It records what it
//! was asked to send, which is what its own tests read; with a `peer` it also
//! speaks that record as NDJSON over a socket, which is what the black-box
//! tests read through the real binary.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bingo_sdk::{CancellationToken, InteractionId};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::adapter::{Buttons, ChannelAdapter, Edit, Inbox, Incoming, Mode, Threads, Typing};
use crate::conversation::{Conversation, Posted};
use crate::error::ChannelError;
use crate::limits::{Dialect, Encoding, Limits};
use crate::question::{Choice, Question};

/// What this loopback can do and how far it will go. Everything a real
/// platform decides for you, a test decides here.
#[derive(Clone, Debug)]
pub struct Config {
    pub limits: Limits,
    pub edits: bool,
    pub buttons: bool,
    pub typing: bool,
    pub threads: bool,
    /// What a group message must contain for the bot to be addressed.
    pub mention: String,
    /// `host:port` to speak NDJSON to. Without one the adapter only records.
    pub peer: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            limits: Limits {
                max_text: (4000, Encoding::Chars),
                dialect: Dialect::Markdown,
                max_actions: 3,
                max_label: 40,
            },
            edits: true,
            buttons: true,
            typing: true,
            threads: true,
            mention: "@bingo".into(),
            peer: None,
        }
    }
}

/// One call this adapter was asked to make.
#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Send {
        to: Conversation,
        id: Posted,
        text: String,
        mode: Mode,
    },
    Replace {
        at: Posted,
        text: String,
    },
    Finish {
        at: Posted,
        text: String,
    },
    Ask {
        to: Conversation,
        id: Posted,
        question: Question,
    },
    Settle {
        at: Posted,
        outcome: String,
    },
    Typing {
        to: Conversation,
    },
    Reply {
        to: Conversation,
        parent: Posted,
        id: Posted,
        text: String,
        mode: Mode,
    },
}

pub struct Loopback {
    config: Config,
    posted: AtomicU64,
    records: Mutex<Vec<Record>>,
    /// Mechanisms this adapter will refuse next time they are asked for.
    ///
    /// A refusal is part of the contract, not an accident outside it: every
    /// real platform rate-limits, closes a card out from under a long answer,
    /// or declines a button layout. The fixture that stands for a platform is
    /// therefore where a refusal is arranged, so what the surface does about
    /// one is asserted rather than hoped for.
    refusals: Mutex<Vec<&'static str>>,
    /// The socket writer's end, once a peer is connected.
    lines: Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// Where events go once the surface has started this adapter. A caller
    /// with no socket speaks through it directly.
    inbox: watch::Sender<Option<Inbox>>,
}

impl std::fmt::Debug for Loopback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loopback")
            .field("peer", &self.config.peer)
            .finish_non_exhaustive()
    }
}

impl Loopback {
    pub const ID: &'static str = "loopback";

    pub fn new(config: Config) -> Self {
        Self {
            config,
            posted: AtomicU64::new(0),
            records: Mutex::new(Vec::new()),
            refusals: Mutex::new(Vec::new()),
            lines: Mutex::new(None),
            inbox: watch::channel(None).0,
        }
    }

    /// Refuse the next call of this mechanism, once. `"finish"`, `"ask"`,
    /// `"send"` and `"replace"` are the names.
    pub fn refuse_once(&self, mechanism: &'static str) {
        locked(&self.refusals).push(mechanism);
    }

    /// Whether this call is the one that was arranged to fail.
    fn refused(&self, mechanism: &str) -> Result<(), ChannelError> {
        let mut refusals = locked(&self.refusals);
        let Some(at) = refusals.iter().position(|which| *which == mechanism) else {
            return Ok(());
        };
        refusals.remove(at);
        Err(ChannelError::Refused(format!(
            "the loopback was told to refuse one {mechanism}"
        )))
    }

    /// Say something to the surface, waiting for it to have started this
    /// adapter — what the peer's socket does, without a socket.
    pub async fn hear(&self, event: Incoming) -> Result<(), ChannelError> {
        let mut started = self.inbox.subscribe();
        loop {
            let inbox = started.borrow_and_update().clone();
            if let Some(inbox) = inbox {
                return inbox.post(event).await;
            }
            started
                .changed()
                .await
                .map_err(|_| ChannelError::Transport("the loopback never started".into()))?;
        }
    }

    /// What this adapter has been asked to do, in order.
    pub fn records(&self) -> Vec<Record> {
        locked(&self.records).clone()
    }

    fn mint(&self) -> Posted {
        Posted::new(format!(
            "m{}",
            self.posted.fetch_add(1, Ordering::Relaxed) + 1
        ))
    }

    /// Keep the call, and say it on the wire when there is one.
    fn record(&self, record: Record) {
        if let Some(lines) = &*locked(&self.lines) {
            let _ = lines.send(spoken(&record).to_string());
        }
        locked(&self.records).push(record);
    }

    /// One inbound line, as the surface would hear it.
    fn heard(&self, line: &str) -> Option<Incoming> {
        let value: Value = serde_json::from_str(line).ok()?;
        let conversation = self.conversation(&value);
        let principal = value["principal"].as_str().unwrap_or("someone").to_string();
        match value["kind"].as_str()? {
            "message" => {
                let text = value["text"].as_str()?.to_string();
                Some(Incoming::Message {
                    addressed: !conversation.group || text.contains(&self.config.mention),
                    parent: value["parent"].as_str().map(Posted::new),
                    conversation,
                    principal,
                    text,
                    images: Vec::new(),
                })
            }
            "click" => Some(Incoming::Click {
                question: InteractionId::from_raw(value["question"].as_str()?),
                choice: value["choice"].as_str()?.to_string(),
                conversation,
                principal,
            }),
            _ => None,
        }
    }

    fn conversation(&self, value: &Value) -> Conversation {
        let chat = value["chat"].as_str().unwrap_or("chat").to_string();
        let conversation = match value["group"].as_bool().unwrap_or(false) {
            true => Conversation::group(chat),
            false => Conversation::direct(chat),
        };
        match value["thread"].as_str() {
            Some(thread) => conversation.in_thread(thread),
            None => conversation,
        }
    }

    /// Read the peer's lines until it hangs up or the surface stops.
    async fn pump(&self, inbox: Inbox, cancel: CancellationToken) -> Result<(), ChannelError> {
        let _ = self.inbox.send(Some(inbox.clone()));
        let Some(peer) = self.config.peer.clone() else {
            cancel.cancelled().await;
            return Ok(());
        };
        let socket = TcpStream::connect(&peer)
            .await
            .map_err(|e| ChannelError::Refused(format!("the loopback peer {peer}: {e}")))?;
        let (reader, writer) = socket.into_split();
        *locked(&self.lines) = Some(self.speak(writer));
        let mut lines = BufReader::new(reader).lines();
        loop {
            let line = tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                line = lines.next_line() => line.map_err(|e| ChannelError::transport("the loopback peer", e))?,
            };
            let Some(line) = line else { return Ok(()) };
            if let Some(event) = self.heard(&line) {
                inbox.post(event).await?;
            }
        }
    }

    /// The writer half on its own task, so a slow peer never holds up a turn.
    fn speak(&self, mut writer: tokio::net::tcp::OwnedWriteHalf) -> mpsc::UnboundedSender<String> {
        let (lines, mut spoken) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = spoken.recv().await {
                if writer
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        lines
    }
}

fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// One record as the peer reads it. This is the black-box wire: a change here
/// is a change to what `crates/bingo/tests/channels.rs` asserts.
fn spoken(record: &Record) -> Value {
    match record {
        Record::Send { to, id, text, mode } => json!({
            "op": "send", "chat": to.chat, "thread": to.thread, "id": id.as_str(),
            "text": text, "mode": mode_name(*mode),
        }),
        Record::Replace { at, text } => json!({"op": "replace", "id": at.as_str(), "text": text}),
        Record::Finish { at, text } => json!({"op": "finish", "id": at.as_str(), "text": text}),
        Record::Ask { to, id, question } => json!({
            "op": "ask", "chat": to.chat, "id": id.as_str(),
            "question": question.id.as_str(), "prompt": question.prompt,
            "choices": question.choices.iter().map(|c: &Choice| json!([c.key, c.label])).collect::<Vec<_>>(),
        }),
        Record::Settle { at, outcome } => {
            json!({"op": "settle", "id": at.as_str(), "outcome": outcome})
        }
        Record::Typing { to } => json!({"op": "typing", "chat": to.chat}),
        Record::Reply {
            to,
            parent,
            id,
            text,
            mode,
        } => json!({
            "op": "reply", "chat": to.chat, "parent": parent.as_str(), "id": id.as_str(),
            "text": text, "mode": mode_name(*mode),
        }),
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Once => "once",
        Mode::Stream => "stream",
    }
}

#[async_trait]
impl ChannelAdapter for Loopback {
    fn id(&self) -> &str {
        Self::ID
    }

    fn limits(&self) -> &Limits {
        &self.config.limits
    }

    fn credential(&self) -> String {
        self.config.peer.clone().unwrap_or_else(|| "offline".into())
    }

    async fn run(&self, inbox: Inbox, cancel: CancellationToken) -> Result<(), ChannelError> {
        self.pump(inbox, cancel).await
    }

    async fn send(
        &self,
        to: &Conversation,
        text: &str,
        mode: Mode,
    ) -> Result<Posted, ChannelError> {
        let id = self.mint();
        self.record(Record::Send {
            to: to.clone(),
            id: id.clone(),
            text: text.to_string(),
            mode,
        });
        Ok(id)
    }

    fn edit(&self) -> Option<&dyn Edit> {
        self.config.edits.then_some(self as &dyn Edit)
    }

    fn buttons(&self) -> Option<&dyn Buttons> {
        self.config.buttons.then_some(self as &dyn Buttons)
    }

    fn typing(&self) -> Option<&dyn Typing> {
        self.config.typing.then_some(self as &dyn Typing)
    }

    fn threads(&self) -> Option<&dyn Threads> {
        self.config.threads.then_some(self as &dyn Threads)
    }
}

#[async_trait]
impl Edit for Loopback {
    async fn replace(&self, at: &Posted, text: &str) -> Result<(), ChannelError> {
        self.record(Record::Replace {
            at: at.clone(),
            text: text.to_string(),
        });
        Ok(())
    }

    async fn finish(&self, at: &Posted, text: &str) -> Result<(), ChannelError> {
        self.refused("finish")?;
        self.record(Record::Finish {
            at: at.clone(),
            text: text.to_string(),
        });
        Ok(())
    }
}

#[async_trait]
impl Buttons for Loopback {
    async fn ask(&self, to: &Conversation, question: &Question) -> Result<Posted, ChannelError> {
        self.refused("ask")?;
        let id = self.mint();
        self.record(Record::Ask {
            to: to.clone(),
            id: id.clone(),
            question: question.clone(),
        });
        Ok(id)
    }

    async fn settle(
        &self,
        at: &Posted,
        _question: &Question,
        outcome: &str,
    ) -> Result<(), ChannelError> {
        self.record(Record::Settle {
            at: at.clone(),
            outcome: outcome.to_string(),
        });
        Ok(())
    }
}

#[async_trait]
impl Typing for Loopback {
    async fn poke(&self, to: &Conversation) -> Result<(), ChannelError> {
        self.record(Record::Typing { to: to.clone() });
        Ok(())
    }
}

#[async_trait]
impl Threads for Loopback {
    async fn reply(
        &self,
        to: &Conversation,
        parent: &Posted,
        text: &str,
        mode: Mode,
    ) -> Result<Posted, ChannelError> {
        let id = self.mint();
        self.record(Record::Reply {
            to: to.clone(),
            parent: parent.clone(),
            id: id.clone(),
            text: text.to_string(),
            mode,
        });
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn without(config: Config) -> Loopback {
        Loopback::new(config)
    }

    #[test]
    fn a_mechanism_that_is_off_hands_over_nothing() {
        let bare = without(Config {
            edits: false,
            buttons: false,
            typing: false,
            threads: false,
            ..Config::default()
        });
        assert!(bare.edit().is_none());
        assert!(bare.buttons().is_none());
        assert!(bare.typing().is_none());
        assert!(bare.threads().is_none());
        let full = without(Config::default());
        assert!(full.edit().is_some());
        assert!(full.buttons().is_some());
    }

    #[tokio::test]
    async fn every_call_is_recorded_in_order_with_the_ids_it_minted() {
        let loopback = without(Config::default());
        let chat = Conversation::direct("oc_1");
        let first = loopback.send(&chat, "", Mode::Stream).await.unwrap();
        loopback
            .edit()
            .unwrap()
            .replace(&first, "Hel")
            .await
            .unwrap();
        loopback
            .edit()
            .unwrap()
            .finish(&first, "Hello")
            .await
            .unwrap();
        assert_eq!(
            loopback.records(),
            [
                Record::Send {
                    to: chat.clone(),
                    id: Posted::new("m1"),
                    text: String::new(),
                    mode: Mode::Stream,
                },
                Record::Replace {
                    at: Posted::new("m1"),
                    text: "Hel".into(),
                },
                Record::Finish {
                    at: Posted::new("m1"),
                    text: "Hello".into(),
                },
            ]
        );
    }

    #[test]
    fn a_group_is_addressed_only_on_a_mention() {
        let loopback = without(Config::default());
        let heard = |line: &str| loopback.heard(line);
        assert!(matches!(
            heard(r#"{"kind":"message","chat":"oc_1","group":true,"text":"hello"}"#),
            Some(Incoming::Message {
                addressed: false,
                ..
            })
        ));
        assert!(matches!(
            heard(r#"{"kind":"message","chat":"oc_1","group":true,"text":"@bingo hello"}"#),
            Some(Incoming::Message {
                addressed: true,
                ..
            })
        ));
        assert!(matches!(
            heard(r#"{"kind":"message","chat":"oc_1","text":"hello"}"#),
            Some(Incoming::Message {
                addressed: true,
                ..
            })
        ));
    }

    #[test]
    fn a_click_carries_the_question_and_the_key_the_button_held() {
        let loopback = without(Config::default());
        assert_eq!(
            loopback
                .heard(r#"{"kind":"click","chat":"oc_1","principal":"u_9","question":"int_1","choice":"2"}"#),
            Some(Incoming::Click {
                conversation: Conversation::direct("oc_1"),
                principal: "u_9".into(),
                question: InteractionId::from_raw("int_1"),
                choice: "2".into(),
            })
        );
    }

    #[test]
    fn a_line_that_is_not_an_event_is_ignored_rather_than_guessed_at() {
        let loopback = without(Config::default());
        assert!(loopback.heard("not json").is_none());
        assert!(loopback.heard(r#"{"kind":"shrug"}"#).is_none());
        assert!(
            loopback
                .heard(r#"{"kind":"message","chat":"oc_1"}"#)
                .is_none()
        );
    }

    #[test]
    fn the_spoken_wire_is_the_one_the_black_box_tests_read() {
        let question = Question {
            id: InteractionId::from_raw("int_1"),
            prompt: "Bash: run tests".into(),
            choices: vec![Choice {
                key: "1".into(),
                label: "Allow once".into(),
                answer: bingo_sdk::Answer::AllowOnce,
            }],
            free_text: false,
        };
        insta::assert_json_snapshot!(
            "loopback-wire",
            [
                spoken(&Record::Send {
                    to: Conversation::group("oc_1").in_thread("omt_2"),
                    id: Posted::new("m1"),
                    text: "hi".into(),
                    mode: Mode::Stream,
                }),
                spoken(&Record::Replace {
                    at: Posted::new("m1"),
                    text: "hi there".into(),
                }),
                spoken(&Record::Finish {
                    at: Posted::new("m1"),
                    text: "hi there!".into(),
                }),
                spoken(&Record::Ask {
                    to: Conversation::direct("oc_1"),
                    id: Posted::new("m2"),
                    question,
                }),
                spoken(&Record::Settle {
                    at: Posted::new("m2"),
                    outcome: "approved in the TUI".into(),
                }),
                spoken(&Record::Typing {
                    to: Conversation::direct("oc_1"),
                }),
                spoken(&Record::Reply {
                    to: Conversation::direct("oc_1"),
                    parent: Posted::new("m1"),
                    id: Posted::new("m3"),
                    text: "under it".into(),
                    mode: Mode::Once,
                }),
            ]
        );
    }
}
