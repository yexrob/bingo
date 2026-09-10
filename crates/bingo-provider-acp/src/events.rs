//! ACP's stream in bingo's vocabulary. Pure: no I/O, no clock, no channel.
//!
//! One decision shapes the whole file, and it is not the one ADR-0035 §4
//! wrote. The ADR asks for the agent's tool calls as `ModelEvent::ToolCall`
//! wearing `acp.external: true`, with "the loop never executes what wears the
//! mark". The loop is the kernel, and reading that mark is a kernel change
//! this milestone does not have: today `ModelEvent::ToolCall` has no
//! `provider_options`, every call it carries reaches `gate_call` and the
//! executor, and — worse — a non-empty `tool_calls` sends `Turn::decide` into
//! another round, which for a stateful ACP session means a second
//! `session/prompt` for a turn the agent has already finished.
//!
//! So the mark went where the kernel already carries provider-private data
//! through to the journal untouched: `ReasoningEnd { provider_metadata }`, and
//! `ItemBody::Reasoning { provider_metadata }` after it. An agent's tool call
//! becomes one reasoning block whose text is what a person reads and whose
//! `acp` metadata is the whole call — id, kind, status, title, locations, raw
//! input and output, every content block. Nothing is executed, no second
//! prompt is sent, and the journal holds enough for a surface to draw a tool
//! row the day the kernel learns the mark. Flipping this to `ToolCall` is then
//! one match arm here.

use std::collections::BTreeMap;

use agent_client_protocol_schema::v1::{
    ContentBlock, ContentChunk, PromptResponse, SessionUpdate, StopReason, ToolCall,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolKind, UsageUpdate,
};
use bingo_sdk::{FinishReason, ModelEvent, ProviderMetadata, UnifiedFinish, Usage};
use serde_json::{Value, json};

use crate::render;

/// The provider-metadata key everything ACP-private hangs under, and the flag
/// inside it that says a call was somebody else's to run.
pub const NAMESPACE: &str = "acp";
pub const EXTERNAL: &str = "external";

/// The one text block a chunk without a `messageId` belongs to. Both
/// first-tier adapters send one, but an adapter that does not must still read
/// as one answer rather than one block per token.
const UNKEYED: &str = "message";

/// `claude-agent-acp`'s own status around a compaction, sent as the model's
/// words because the protocol has no other way to say it. The agent did not
/// say them, and a transcript that keeps them reads as if it had — the cut
/// itself is a row of its own (ADR-0055 §4). `Compacting failed…` is not one
/// of them: that one is what a person needs to read.
const BANNERS: [&str; 2] = ["Compacting...", "\n\nCompacting completed."];

/// What an agent last said about the context it holds: its own count, and the
/// window it counted against (ADR-0055 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub used: u64,
    pub window: u64,
}

impl From<&UsageUpdate> for Reading {
    fn from(usage: &UsageUpdate) -> Self {
        Self {
            used: usage.used,
            window: usage.size,
        }
    }
}

/// The running state of one turn's stream. A tool call arrives as a first
/// notification and any number of partial updates, each naming only what
/// changed, so what is open has to be remembered to be closed.
#[derive(Debug, Default)]
pub struct Mapper {
    text: Option<String>,
    thought: Option<String>,
    calls: BTreeMap<String, Call>,
    /// What the agent last said it holds — from this turn, or from the turn
    /// before it, so a fall across a turn boundary is still the cut it was
    /// (ADR-0055 §4).
    held: Option<Reading>,
}

/// One tool call as it stands. Fields are replaced only by an update that
/// names them: ADR-0035 aside, both adapters send partial updates, and a
/// client that overwrites loses the title and the kind on the first one.
#[derive(Debug)]
struct Call {
    id: String,
    title: String,
    kind: ToolKind,
    status: ToolCallStatus,
    content: Vec<ToolCallContent>,
    locations: Vec<ToolCallLocation>,
    raw_input: Option<Value>,
    raw_output: Option<Value>,
}

impl Mapper {
    /// A turn of a conversation somebody has already read: what the agent
    /// last said it holds, so the first reading of this turn is measured
    /// against it (ADR-0055 §4).
    pub fn holding(held: Option<Reading>) -> Self {
        Self {
            held,
            ..Self::default()
        }
    }

    /// What the agent said it holds, for the turn after this one.
    pub fn held(&self) -> Option<Reading> {
        self.held
    }

    /// One update, in bingo's vocabulary. An update this build has no meaning
    /// for — a plan, a mode, a slash-command list (ADR-0035 §6) — is no
    /// events, deliberately.
    pub fn update(&mut self, update: SessionUpdate) -> Vec<ModelEvent> {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.say(chunk),
            SessionUpdate::AgentThoughtChunk(chunk) => self.think(chunk),
            SessionUpdate::ToolCall(call) => self.open_call(call),
            SessionUpdate::ToolCallUpdate(update) => self.advance_call(update),
            SessionUpdate::UsageUpdate(usage) => self.read(&usage),
            _ => Vec::new(),
        }
    }

    /// Every reading is the context as the agent counts it (ADR-0055 §2); one
    /// that fell below the last is the cut the agent made to get there, which
    /// no other frame says at all.
    fn read(&mut self, usage: &UsageUpdate) -> Vec<ModelEvent> {
        let reading = Reading::from(usage);
        let cut = self
            .held
            .filter(|last| reading.used < last.used)
            .map(|last| ModelEvent::Compacted {
                before: last.used,
                after: reading.used,
            });
        self.held = Some(reading);
        cut.into_iter()
            .chain([ModelEvent::Context {
                used: reading.used,
                window: reading.window,
            }])
            .collect()
    }

    /// Close whatever the last update left open. A turn ends once, and every
    /// block it opened has to end with it or a surface waits for ever.
    pub fn finish(&mut self, response: &PromptResponse) -> Vec<ModelEvent> {
        let mut events = self.close_open_blocks();
        events.push(ModelEvent::Finish {
            usage: self.usage(response),
            finish_reason: finish_reason(response.stop_reason),
        });
        events
    }

    /// What the turn cost. The end-turn field is the only per-turn count ACP
    /// has; where an adapter reports none, the context it says it is holding
    /// stands in, because for a stateful session the whole context is what the
    /// turn read. Nothing is invented: an adapter that reports neither reports
    /// zero (ADR-0035 §6).
    fn usage(&self, response: &PromptResponse) -> Usage {
        if let Some(usage) = &response.usage {
            return Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cached_read_tokens.unwrap_or_default(),
                cache_write_tokens: usage.cached_write_tokens.unwrap_or_default(),
                reasoning_tokens: usage.thought_tokens.unwrap_or_default(),
            };
        }
        Usage {
            input_tokens: self.held.map(|held| held.used).unwrap_or_default(),
            ..Usage::default()
        }
    }

    fn close_open_blocks(&mut self) -> Vec<ModelEvent> {
        let mut events = Vec::new();
        if let Some(id) = self.text.take() {
            events.push(ModelEvent::TextEnd { id });
        }
        if let Some(id) = self.thought.take() {
            events.push(ModelEvent::ReasoningEnd {
                id,
                provider_metadata: ProviderMetadata::new(),
            });
        }
        let unfinished: Vec<String> = self.calls.keys().cloned().collect();
        for id in unfinished {
            events.extend(self.close_call(&id));
        }
        events
    }

    fn say(&mut self, chunk: ContentChunk) -> Vec<ModelEvent> {
        let said = plain(&chunk.content);
        if BANNERS.contains(&said.as_str()) {
            return Vec::new();
        }
        let id = key(&chunk, UNKEYED);
        let mut events = Vec::new();
        if self.text.as_deref() != Some(id.as_str()) {
            if let Some(previous) = self.text.replace(id.clone()) {
                events.push(ModelEvent::TextEnd { id: previous });
            }
            events.push(ModelEvent::TextStart { id: id.clone() });
        }
        events.push(ModelEvent::TextDelta { id, delta: said });
        events
    }

    fn think(&mut self, chunk: ContentChunk) -> Vec<ModelEvent> {
        let id = format!("thought:{}", key(&chunk, UNKEYED));
        let mut events = Vec::new();
        if self.thought.as_deref() != Some(id.as_str()) {
            if let Some(previous) = self.thought.replace(id.clone()) {
                events.push(ModelEvent::ReasoningEnd {
                    id: previous,
                    provider_metadata: ProviderMetadata::new(),
                });
            }
            events.push(ModelEvent::ReasoningStart { id: id.clone() });
        }
        events.push(ModelEvent::ReasoningDelta {
            id,
            delta: plain(&chunk.content),
        });
        events
    }

    /// A first notification may already carry content — `codex-acp` announces
    /// a command with its terminal in the same breath — so the opening is the
    /// heading and whatever came with it.
    fn open_call(&mut self, call: ToolCall) -> Vec<ModelEvent> {
        let id = block_id(&call.tool_call_id.0);
        let mut opening = vec![render::heading(call.kind, &call.title)];
        let mut open = Call::from(call);
        let arrived = std::mem::take(&mut open.content);
        opening.extend(open.absorb(Some(arrived)));
        self.calls.insert(id.clone(), open);
        vec![
            ModelEvent::ReasoningStart { id: id.clone() },
            ModelEvent::ReasoningDelta {
                id,
                delta: opening.join("\n"),
            },
        ]
    }

    /// An update for a call nobody announced still becomes a block: an adapter
    /// that skips the opening notification is a bug we show rather than hide.
    fn advance_call(&mut self, update: ToolCallUpdate) -> Vec<ModelEvent> {
        let id = block_id(&update.tool_call_id.0);
        let mut events = Vec::new();
        if !self.calls.contains_key(&id) {
            events.extend(self.open_call(bare_call(&update)));
        }
        let Some(call) = self.calls.get_mut(&id) else {
            return events;
        };
        let added = call.merge(update);
        if !added.is_empty() {
            events.push(ModelEvent::ReasoningDelta {
                id: id.clone(),
                delta: added,
            });
        }
        if call.status.is_over() {
            events.extend(self.close_call(&id));
        }
        events
    }

    fn close_call(&mut self, id: &str) -> Vec<ModelEvent> {
        let Some(call) = self.calls.remove(id) else {
            return Vec::new();
        };
        vec![ModelEvent::ReasoningEnd {
            id: id.to_string(),
            provider_metadata: call.metadata(),
        }]
    }
}

impl Call {
    /// Take what an update names and leave the rest standing, then say what
    /// changed in words.
    fn merge(&mut self, update: ToolCallUpdate) -> String {
        let fields = update.fields;
        let mut said = Vec::new();
        if let Some(title) = fields.title {
            self.title = title;
        }
        if let Some(kind) = fields.kind {
            self.kind = kind;
        }
        if let Some(status) = fields.status
            && status != self.status
        {
            self.status = status;
            said.push(render::outcome(status).to_string());
        }
        if let Some(locations) = fields.locations {
            self.locations = locations;
        }
        if let Some(raw_input) = fields.raw_input {
            self.raw_input = Some(raw_input);
        }
        if let Some(raw_output) = fields.raw_output {
            self.raw_output = Some(raw_output);
        }
        said.extend(self.absorb(fields.content));
        said.join("\n")
    }

    /// Content arrives as a whole list each time. Only what is new is said, so
    /// a repeated list does not repeat itself into the transcript.
    fn absorb(&mut self, content: Option<Vec<ToolCallContent>>) -> Vec<String> {
        let Some(content) = content else {
            return Vec::new();
        };
        let fresh: Vec<ToolCallContent> = content
            .into_iter()
            .filter(|block| !self.content.contains(block))
            .collect();
        let said = fresh.iter().map(render::block).collect();
        self.content.extend(fresh);
        said
    }

    /// The call, whole, for a surface that knows what to do with it — and the
    /// flag that says bingo never ran it (ADR-0035 §4).
    fn metadata(&self) -> ProviderMetadata {
        let mut acp = serde_json::Map::new();
        acp.insert(EXTERNAL.to_string(), Value::Bool(true));
        acp.insert("toolCallId".into(), json!(self.id));
        acp.insert("title".into(), json!(self.title));
        acp.insert("kind".into(), to_value(&self.kind));
        acp.insert("status".into(), to_value(&self.status));
        acp.insert("content".into(), to_value(&self.content));
        acp.insert("locations".into(), to_value(&self.locations));
        if let Some(input) = &self.raw_input {
            acp.insert("rawInput".into(), input.clone());
        }
        if let Some(output) = &self.raw_output {
            acp.insert("rawOutput".into(), output.clone());
        }
        ProviderMetadata::from([(NAMESPACE.to_string(), acp)])
    }
}

impl From<ToolCall> for Call {
    fn from(call: ToolCall) -> Self {
        Self {
            id: call.tool_call_id.0.to_string(),
            title: call.title,
            kind: call.kind,
            status: call.status,
            content: call.content,
            locations: call.locations,
            raw_input: call.raw_input,
            raw_output: call.raw_output,
        }
    }
}

trait Over {
    fn is_over(&self) -> bool;
}

impl Over for ToolCallStatus {
    fn is_over(&self) -> bool {
        matches!(self, ToolCallStatus::Completed | ToolCallStatus::Failed)
    }
}

/// `ToolCalls` is never the reason an ACP turn ends: the agent ran its own
/// tools and finished. `cancelled` is `Other` because it is not the model's
/// decision; the kernel already knows an interrupt by its own token.
fn finish_reason(stop: StopReason) -> FinishReason {
    let unified = match stop {
        StopReason::EndTurn => UnifiedFinish::Stop,
        StopReason::MaxTokens => UnifiedFinish::Length,
        StopReason::Refusal => UnifiedFinish::ContentFilter,
        _ => UnifiedFinish::Other,
    };
    FinishReason {
        unified,
        raw: raw_stop(stop),
    }
}

fn raw_stop(stop: StopReason) -> Option<String> {
    serde_json::to_value(stop)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

fn block_id(tool_call_id: &str) -> String {
    format!("tool:{tool_call_id}")
}

fn key(chunk: &ContentChunk, fallback: &str) -> String {
    chunk
        .message_id
        .as_ref()
        .map(|id| id.0.to_string())
        .unwrap_or_else(|| fallback.to_string())
}

/// A chunk that is not text still says something happened; the alternative is
/// an answer with a silent hole in it.
fn plain(content: &ContentBlock) -> String {
    match content {
        ContentBlock::Text(text) => text.text.clone(),
        other => format!("({})", kind_of(other)),
    }
}

fn kind_of(content: &ContentBlock) -> &'static str {
    match content {
        ContentBlock::Image(_) => "image",
        ContentBlock::Audio(_) => "audio",
        ContentBlock::Resource(_) | ContentBlock::ResourceLink(_) => "resource",
        _ => "content this build does not render",
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;

/// What an update names about a call nobody opened.
fn bare_call(update: &ToolCallUpdate) -> ToolCall {
    ToolCall::new(
        update.tool_call_id.clone(),
        update
            .fields
            .title
            .clone()
            .unwrap_or_else(|| update.tool_call_id.0.to_string()),
    )
    .kind(update.fields.kind.unwrap_or(ToolKind::Other))
}
