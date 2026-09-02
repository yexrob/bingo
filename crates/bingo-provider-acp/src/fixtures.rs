//! Recorded ACP bodies, exactly as they go over the wire.
//!
//! These are the contract (AGENTS.md, "Contracts first"): every message this
//! plugin sends or reads has one here, and `method::tests` proves the schema
//! types read it and write it back unchanged. A schema bump that quietly
//! reshapes a field fails here rather than in front of a person.
//!
//! They are written from the protocol documentation and from what the two
//! first-tier adapters actually send; where the two adapters differ, both
//! shapes are recorded and the difference is named in the comment.

use serde_json::{Value, json};

pub fn initialize_request() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false
        },
        "clientInfo": { "name": "bingo", "version": "0.4.2" }
    })
}

/// What an agent that can be resumed answers. `loadSession` is the older,
/// flat flag; `sessionCapabilities.resume` is the newer door, and an adapter
/// may have either, both or neither — which is the whole reason the restore
/// ladder has three rungs.
pub fn initialize_response() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "loadSession": true,
            "promptCapabilities": { "image": true, "audio": false, "embeddedContext": true },
            "sessionCapabilities": { "resume": {} }
        },
        "agentInfo": { "name": "claude-agent-acp", "version": "0.23.1" }
    })
}

/// The other end of the ladder: no `loadSession`, no `resume`. A restore here
/// can only be a fresh session that is told where to read what happened.
pub fn initialize_response_without_restore() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "promptCapabilities": { "image": false, "audio": false, "embeddedContext": false }
        },
        "agentInfo": { "name": "codex-acp", "version": "0.5.0" }
    })
}

/// An agent that has no credential yet says so here, and `session/new` is
/// what fails. The methods are the agent's own login, never bingo's.
pub fn initialize_response_needing_auth() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": {
            "promptCapabilities": { "image": false, "audio": false, "embeddedContext": false }
        },
        "authMethods": [
            { "id": "claude-login", "name": "Log in with Claude Code", "description": "Run `claude login`" }
        ],
        "agentInfo": { "name": "claude-agent-acp", "version": "0.23.1" }
    })
}

/// `mcpServers` is empty on purpose: our tools do not cross (ADR-0035 §6).
pub fn new_session_request() -> Value {
    json!({ "cwd": "/work/repo", "mcpServers": [] })
}

pub fn new_session_response() -> Value {
    json!({ "sessionId": "sess_abc123" })
}

pub fn load_session_request() -> Value {
    json!({ "mcpServers": [], "cwd": "/work/repo", "sessionId": "sess_abc123" })
}

pub fn load_session_response() -> Value {
    json!({})
}

/// Unlike `session/load`, a resume omits an empty `mcpServers`: the schema
/// skips the field when it is empty, and what the type writes is what the
/// adapter reads.
pub fn resume_session_request() -> Value {
    json!({ "sessionId": "sess_abc123", "cwd": "/work/repo" })
}

pub fn resume_session_response() -> Value {
    json!({})
}

/// One turn: only the new user message crosses, because an ACP session is
/// stateful and holds everything before it (ADR-0035 §3).
pub fn prompt_request() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "prompt": [{ "type": "text", "text": "rename the module" }]
    })
}

/// claude-agent-acp fills `usage`; the field is `unstable_end_turn_token_usage`
/// in the schema crate and the only per-turn token count ACP has.
pub fn prompt_response_with_usage() -> Value {
    json!({
        "stopReason": "end_turn",
        "usage": {
            "totalTokens": 1088,
            "inputTokens": 1024,
            "outputTokens": 64,
            "cachedReadTokens": 512
        }
    })
}

/// codex-acp reports no tokens. Zero would be a lie the ruler would believe;
/// absent is the truth (ADR-0035 §6).
pub fn prompt_response_bare() -> Value {
    json!({ "stopReason": "end_turn" })
}

pub fn prompt_response_cancelled() -> Value {
    json!({ "stopReason": "cancelled" })
}

pub fn cancel_notification() -> Value {
    json!({ "sessionId": "sess_abc123" })
}

/// `messageId` is a sibling of `content`, not `_meta`, and both first-tier
/// adapters send it: it is what says two chunks belong to one block.
pub fn update_agent_message_chunk() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "Renaming " },
            "messageId": "msg_1"
        }
    })
}

pub fn update_agent_message_chunk_more() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "the module." },
            "messageId": "msg_1"
        }
    })
}

/// An adapter that says nothing about which message a chunk belongs to. The
/// stream must still read as one block rather than one block per chunk.
pub fn update_agent_message_chunk_unkeyed() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": { "type": "text", "text": "no id here" }
        }
    })
}

pub fn update_agent_thought_chunk() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "agent_thought_chunk",
            "content": { "type": "text", "text": "the import list moves too" }
        }
    })
}

/// A tool call the agent is about to run on its own machine. The shape is
/// `claude-agent-acp`'s `toolCallNotification()`: a `pending` status, the raw
/// input, `locations`, and the adapter's own `_meta`.
pub fn update_tool_call() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "tool_call",
            "toolCallId": "toolu_01Read",
            "title": "Read src/lib.rs (1 - 50)",
            "kind": "read",
            "status": "pending",
            "content": [],
            "locations": [{ "path": "/work/repo/src/lib.rs", "line": 1 }],
            "rawInput": { "file_path": "/work/repo/src/lib.rs", "offset": 1 },
            "_meta": { "claudeCode": { "toolName": "Read" } }
        }
    })
}

/// The same call, finished. An update names only the fields it changes, and
/// every field it leaves out means "unchanged" — both first-tier adapters
/// send partial updates, so a client that replaces rather than merges loses
/// the title and the kind.
pub fn update_tool_call_completed() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "toolu_01Read",
            "status": "completed",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "pub mod wire;" } }
            ],
            "rawOutput": { "lines": 1 }
        }
    })
}

/// An edit reports itself as a diff, which is the whole reason the agent's
/// calls are worth carrying structurally rather than as prose. `codex-acp`
/// hangs its own `_meta.kind` off the block and writes `oldText: null` for a
/// file it is creating.
pub fn update_tool_call_diff() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "toolu_02Edit",
            "status": "completed",
            "content": [{
                "type": "diff",
                "path": "/work/repo/src/lib.rs",
                "oldText": "pub mod wire;",
                "newText": "pub mod envelope;",
                "_meta": { "kind": "update" }
            }],
            "locations": [{ "path": "/work/repo/src/lib.rs" }]
        }
    })
}

/// A terminal the agent owns. This client declares no terminal capability, so
/// the id is a handle to somebody else's process — shown, never joined
/// (ADR-0035 §6).
pub fn update_tool_call_terminal() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "tool_call",
            "toolCallId": "command-123",
            "kind": "execute",
            "title": "npm test",
            "status": "in_progress",
            "content": [{ "type": "terminal", "terminalId": "command-123" }],
            "rawInput": { "command": "npm test", "cwd": "/work/repo" },
            "_meta": { "terminal_info": { "cwd": "/work/repo", "terminal_id": "command-123" } }
        }
    })
}

/// A failed call is still a call: the status is the fact, not an error to
/// swallow.
pub fn update_tool_call_failed() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "tool_call_update",
            "toolCallId": "toolu_04Bash",
            "status": "failed",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "no such file" } }
            ]
        }
    })
}

/// A variant this build's schema does not know. `codex-acp` ships the subagent
/// RFD ahead of the specification; a client that never asked for it must still
/// not fall over when it arrives.
pub fn update_from_a_newer_adapter() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "subagent_spawned",
            "subagentSessionId": "child-1",
            "name": "weather_research",
            "task": "look it up",
            "capabilities": {}
        }
    })
}

/// The stable usage notification: the window as the agent sees it, and what
/// the turn has cost in the agent's own currency. `cost` is
/// `claude-agent-acp`'s; `codex-acp` sends `used` and `size` alone, and
/// `Usage` has no home for money either way.
pub fn update_usage() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "usage_update",
            "used": 12000,
            "size": 200000,
            "cost": { "amount": 0.031, "currency": "USD" }
        }
    })
}

/// Three updates ADR-0035 §6 leaves unmapped. They are recorded anyway: a
/// fixture is how "we ignore this" stays a decision rather than a crash.
pub fn update_plan() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "plan",
            "entries": [{ "content": "read the module", "priority": "high", "status": "pending" }]
        }
    })
}

pub fn update_available_commands() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": {
            "sessionUpdate": "available_commands_update",
            "availableCommands": [{ "name": "review", "description": "review the diff" }]
        }
    })
}

pub fn update_current_mode() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "update": { "sessionUpdate": "current_mode_update", "currentModeId": "default" }
    })
}

/// The one request this client answers, in `claude-agent-acp`'s spelling. The
/// option ids are the agent's own and the answer must name one of them back —
/// which is why the person is shown the agent's options rather than bingo's
/// allow/deny, and why nothing here matches on `kind` or on position.
pub fn request_permission() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "toolCall": {
            "toolCallId": "toolu_02Edit",
            "status": "pending",
            "title": "Edit src/lib.rs",
            "kind": "edit",
            "content": [],
            "locations": [],
            "rawInput": { "file_path": "/work/repo/src/lib.rs" }
        },
        "options": [
            { "optionId": "allow-once", "name": "Yes", "kind": "allow_once" },
            {
                "optionId": "allow-with-updates",
                "name": "Yes, and don't ask again for edits to this file",
                "kind": "allow_always"
            },
            { "optionId": "reject", "name": "No", "kind": "reject_once" }
        ],
        "_meta": { "permission": { "version": 1, "title": "Edit src/lib.rs" } }
    })
}

/// The same door, in `codex-acp`'s spelling: four options, two of them
/// rejects, and ids that are nothing like the kinds. An id is the only thing
/// either adapter will accept back.
pub fn request_permission_codex() -> Value {
    json!({
        "sessionId": "sess_abc123",
        "toolCall": {
            "toolCallId": "command-item",
            "kind": "execute",
            "status": "pending",
            "title": "Run command with file reads",
            "rawInput": { "command": "npm test", "cwd": "/work/repo" },
            "locations": [{ "path": "/work/repo/src/a.ts" }]
        },
        "options": [
            { "optionId": "allow_once", "name": "Yes, proceed", "kind": "allow_once" },
            {
                "optionId": "allow_for_session",
                "name": "Yes, and don't ask again for this command in this session",
                "kind": "allow_always"
            },
            { "optionId": "decline", "name": "No, continue without running it", "kind": "reject_once" },
            {
                "optionId": "cancel",
                "name": "No, and tell Codex what to do differently",
                "kind": "reject_once"
            }
        ],
        "_meta": {
            "permission": {
                "version": 1,
                "title": "Run command?",
                "description": "Needed to verify the changes."
            }
        }
    })
}

pub fn request_permission_selected() -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": "allow-once" } })
}

pub fn request_permission_cancelled() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

/// What this client sends back to either adapter: the id of the option the
/// agent itself marked a rejection (ADR-0035 §5).
pub fn request_permission_refused() -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": "reject" } })
}

/// The other door an agent may knock on to reach a person. This client
/// declares no elicitation capability, so a conformant agent never sends it;
/// the one that does is answered rather than left waiting.
pub fn elicitation_create() -> Value {
    json!({
        "mode": "form",
        "sessionId": "sess_abc123",
        "requestedSchema": {
            "type": "object",
            "properties": { "branch": { "type": "string", "title": "Branch" } }
        },
        "message": "Which branch should I push to?"
    })
}

pub fn elicitation_declined() -> Value {
    json!({ "action": "decline" })
}
