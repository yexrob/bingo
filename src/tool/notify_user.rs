//! The `notify_user` tool (D94): a subagent's deliberate line to the user.
//!
//! Assembled for subagents only (`src/tools.rs`, `session.depth > 0`). The main
//! agent is already in the hub — everything it says reaches the user by being
//! said, so a tool that "notifies" would only be a second, worse way to talk.
//!
//! The rate limiting, and therefore the verdict this tool reports, lives in
//! [`crate::notify_user::Relay`]; this file is the model-facing surface.

use async_trait::async_trait;

use crate::notify_user::NotifyLevel;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input, schema_for};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
struct NotifyUserInput {
    #[schemars(
        description = "What to tell the user. One or two sentences, complete on its own — \
                       it is read out of context, beside the user's other conversations."
    )]
    text: String,
    #[schemars(
        description = "\"info\" (default): a line in the hub, read when the user next looks. \
                       \"urgent\": also rings the terminal's attention channel, which \
                       interrupts a user who is in another window. Reserve urgent for \
                       something that is blocking and cannot wait."
    )]
    level: Option<NotifyLevel>,
}

/// `notify_user`: reach the user through the hub.
pub struct NotifyUserTool;

#[async_trait]
impl Tool for NotifyUserTool {
    fn name(&self) -> String {
        "notify_user".to_string()
    }

    fn description(&self) -> String {
        // The whole point of the tool is restraint, so the description spends
        // most of its words on when *not* to call it. An agent that narrates
        // progress here turns the user's one quiet surface into a log.
        "Send the user a short, deliberate notice through their main conversation. \
         Your ordinary work does not need this: everything you do is already visible \
         to the user in your DM, and your final reply is delivered to whoever started \
         you. Use notify_user only for something the user would want to look up for:\n\
         - the overall task you were given is finished\n\
         - you are blocked and need the user (a decision, a credential, access)\n\
         - you found something important enough to change what they are doing\n\n\
         Do not use it for progress updates, for acknowledging an instruction, for \
         narrating steps, or to repeat something already in your reply. \
         Notices are rate limited to one line per minute per agent: extra notices are \
         counted and reported as a single \"N more\" line rather than shown, so calling \
         this often makes you quieter, not louder. Set level to \"urgent\" only when the \
         user must act now — it interrupts them wherever they are."
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        schema_for::<NotifyUserInput>()
    }

    /// Nothing on disk, nothing in the working tree: it writes a line to a screen.
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    /// Two notices racing produce two lines, and the relay's own lock keeps the
    /// window consistent.
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: NotifyUserInput = parse_input(&input)?;
        let text = input.text.trim();
        if text.is_empty() {
            return Err(ToolError::failed(
                "text is empty; a notice the user cannot read is not worth interrupting them for",
            ));
        }
        // The tool is assembled for subagents only, so `instance` is set in every
        // real call. A name is still required to address the line, so a missing
        // one falls back rather than failing the call.
        let agent = ctx.instance.clone().unwrap_or_else(|| "agent".to_string());
        let verdict = ctx.notify_user.notify(
            &agent,
            text,
            input.level.unwrap_or_default(),
            std::time::Instant::now(),
        );
        Ok(ToolResult {
            content: serde_json::Value::String(verdict.tool_result()),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify_user::{Notice, Relay};
    use std::sync::{Arc, Mutex};

    fn ctx_with(relay: Arc<Relay>, instance: Option<&str>) -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            home: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                &format!("notify-user-{}", std::process::id()),
            )),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: instance.map(str::to_string),
            rewind: Default::default(),
            notify_user: relay,
        }
    }

    #[tokio::test]
    async fn a_notice_reaches_the_relay_under_the_calling_agent_s_name() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let relay = Relay::new(Arc::new(move |n: Notice| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).push(n);
        }));
        let ctx = ctx_with(relay, Some("scout"));

        let out = NotifyUserTool
            .call(serde_json::json!({ "text": "the migration is done" }), &ctx)
            .await
            .expect("notify_user succeeds");
        assert_eq!(out.content, serde_json::json!("delivered"));
        assert!(!out.is_error);

        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            seen.as_slice(),
            [Notice::Relay {
                agent: "scout".to_string(),
                text: "the migration is done".to_string(),
                level: NotifyLevel::Info,
                notifier: false,
            }]
        );
    }

    #[tokio::test]
    async fn urgent_is_carried_through_to_the_relay() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let relay = Relay::new(Arc::new(move |n: Notice| {
            sink.lock().unwrap_or_else(|e| e.into_inner()).push(n);
        }));
        let ctx = ctx_with(relay, Some("scout"));
        NotifyUserTool
            .call(
                serde_json::json!({ "text": "need the deploy key", "level": "urgent" }),
                &ctx,
            )
            .await
            .expect("notify_user succeeds");
        assert!(matches!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).first(),
            Some(Notice::Relay {
                level: NotifyLevel::Urgent,
                notifier: true,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_second_notice_in_the_window_is_reported_as_queued() {
        let ctx = ctx_with(Relay::detached(), Some("scout"));
        let first = NotifyUserTool
            .call(serde_json::json!({ "text": "one" }), &ctx)
            .await
            .expect("first succeeds");
        assert_eq!(first.content, serde_json::json!("delivered"));

        let second = NotifyUserTool
            .call(serde_json::json!({ "text": "two" }), &ctx)
            .await
            .expect("second succeeds");
        let text = second.content.as_str().unwrap_or_default().to_string();
        assert!(text.starts_with("queued:"), "{text}");
        assert!(
            !second.is_error,
            "being rate limited is not an error the model should react to"
        );
    }

    #[tokio::test]
    async fn empty_text_is_refused() {
        let ctx = ctx_with(Relay::detached(), Some("scout"));
        let err = NotifyUserTool
            .call(serde_json::json!({ "text": "   " }), &ctx)
            .await
            .expect_err("blank text is refused");
        assert!(err.to_string().contains("text is empty"), "{err}");
    }

    #[test]
    fn the_schema_matches_the_input_struct() {
        let schema = NotifyUserTool.input_schema();
        assert_eq!(schema["required"], serde_json::json!(["text"]));
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert!(schema["properties"]["level"].is_object());
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn the_description_says_when_not_to_call_it() {
        let description = NotifyUserTool.description();
        for phrase in [
            "blocked",
            "Do not use it for progress updates",
            "rate limited",
        ] {
            assert!(description.contains(phrase), "missing {phrase:?}");
        }
    }
}
