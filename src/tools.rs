use std::sync::Arc;

use crate::query::Session;
use crate::tool::Tool;
use crate::tool::agent::{AgentControlTool, AgentTool, SendMessageTool};
use crate::tool::ask::AskUserQuestionTool;
use crate::tool::bash::BashTool;
use crate::tool::edit::EditTool;
use crate::tool::experience::{
    ExperienceCommitTool, ExperienceForgetTool, ExperienceOutcomeTool, ExperienceProposeTool,
    ExperienceQueryTool,
};
use crate::tool::glob::GlobTool;
use crate::tool::grep::GrepTool;
use crate::tool::read::ReadTool;
use crate::tool::skill::SkillTool;
use crate::tool::task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool};
use crate::tool::webfetch::WebFetchTool;
use crate::tool::websearch::WebSearchTool;
use crate::tool::write::WriteTool;

/// Base tool pool + MCP + subagents.
pub async fn assemble_tools(
    session: &Arc<Session>,
    on_warning: &mut (dyn Fn(String) + Send),
) -> Vec<Box<dyn Tool>> {
    // Skill/agent-definition scanning is synchronous IO: move it off the runtime thread
    // (on a cache hit it's just a few stats).
    let home = session.home.clone();
    let cwd = session.cwd();
    let (skills, agent_defs) = tokio::task::spawn_blocking(move || {
        (
            crate::skills::load_skills(&home, &cwd),
            crate::agents::load_agent_defs(&home, &cwd),
        )
    })
    .await
    .unwrap_or_default();
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(BashTool::with_output_max_chars(
            session
                .settings
                .bash_output_max_chars
                .unwrap_or(crate::tool::bash::DEFAULT_OUTPUT_MAX_CHARS),
        )),
        Box::new(ReadTool::new()),
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(EditTool),
        Box::new(WriteTool),
        Box::new(WebFetchTool),
        Box::new(WebSearchTool),
        Box::new(AgentTool::new(session.clone(), agent_defs)),
        Box::new(TaskCreateTool),
        Box::new(TaskUpdateTool),
        Box::new(TaskGetTool),
        Box::new(TaskListTool),
        Box::new(SkillTool::new(skills)),
        Box::new(ExperienceProposeTool),
        Box::new(ExperienceCommitTool),
        Box::new(ExperienceQueryTool),
        Box::new(ExperienceOutcomeTool),
        Box::new(ExperienceForgetTool),
    ];
    // Every participant speaks with the same verb (D98). `SendMessage` is
    // assembled at every depth and hub-and-spoke is preserved by *addressing*
    // rather than by a second tool: main reaches any instance and any room it is
    // in, a subagent reaches `main` and the rooms it is a member of. `Post` and
    // `notify_user` retired into it — the first was a second name for speaking,
    // the second a second name for speaking to main.
    let channels_on = session.settings.experimental.agent_channels;
    tools.push(Box::new(SendMessageTool::new(session.clone())));
    if session.depth == 0 {
        // Only the session that owns the UI can question the user. A subagent's answer
        // channel is its return value, not a modal — shipping the tool there would just
        // buy an "unanswered" round trip.
        tools.push(Box::new(AskUserQuestionTool));
        // Continuation and lifecycle management stay the main session's:
        // subagents don't manage siblings.
        tools.push(Box::new(AgentControlTool::new(session.clone())));
        // The crew is the project's, not a subagent's: a member that could restart or
        // rewrite the team it belongs to is a loop with the user's consent in the middle.
        tools.push(Box::new(crate::tool::team::TeamTool::new(session.clone())));
        if channels_on {
            tools.push(Box::new(crate::tool::channel::ChannelTool::new(
                session.clone(),
            )));
        }
    } else if channels_on && session.depth == 1 && session.instance.is_some() {
        // Room cohort (experimental): a direct subagent forms rooms of its own
        // (D95). Grouping used to be the main agent's alone, which made every
        // room a room the top of the tree had convened; a room is an arbitrary
        // subset of the team, and two members who need to work something out are
        // exactly such a subset. Speaking in one is `SendMessage(to: "#room")`,
        // gated by the same cohort rule inside the tool.
        tools.push(Box::new(crate::tool::channel::ChannelTool::new(
            session.clone(),
        )));
    }
    let mcp = {
        let mgr = session.runtime.mcp.clone();
        let (tools, warnings, pending) = {
            let mut guard = mgr.lock().await;
            // The manager is shared with subagents, whose on_warning is a no-op: draining
            // there would consume the failure report and the user would never see it.
            let warnings = if session.depth == 0 {
                guard.drain_unreported_failures()
            } else {
                Vec::new()
            };
            let pending = guard.needs_connect();
            guard.mark_connecting(&pending);
            let tools = guard.tools();
            (tools, warnings, pending)
        };
        for warning in warnings {
            on_warning(warning);
        }
        if !pending.is_empty() {
            // Background connect: the turn does not wait for the handshake (a
            // bad server times out into `failures` and is reported once via
            // drain_unreported_failures at the next turn's assemble).
            let quiet = session.quiet;
            tokio::spawn(async move {
                let mut guard = mgr.lock().await;
                let _ = guard.connect_all().await;
                guard.finish_connecting(&pending);
                if !quiet {
                    let count = guard.tools().len();
                    if count > 0 {
                        eprintln!("[bingo] connected {count} MCP tools");
                    }
                }
            });
        }
        tools
    };
    tools.extend(mcp);
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_at_depth(depth: usize) -> Arc<Session> {
        session_with(depth, false)
    }

    fn session_with(depth: usize, channels_on: bool) -> Arc<Session> {
        let mut settings = crate::settings::Settings::default();
        settings.experimental.agent_channels = channels_on;
        std::sync::Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "https://example.com".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings,
            system: Vec::new(),
            depth,
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    #[tokio::test]
    async fn assembles_experience_outcome_exactly_once() {
        let mut warn = |_: String| {};
        let tools = assemble_tools(&session_at_depth(0), &mut warn).await;
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.name() == "ExperienceOutcome")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn assembles_task_tools() {
        let mut warn = |_: String| {};
        let tools = assemble_tools(&session_at_depth(0), &mut warn).await;
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        for expected in ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing {expected}: {names:?}"
            );
        }
    }

    /// hub-and-spoke: continuation/lifecycle tools only assembled for the main session,
    /// not subagents. `SendMessage` left this list in D98 — it is assembled
    /// everywhere now, and the topology is enforced by its addressing rules
    /// instead (see the tool's own tests).
    #[tokio::test]
    async fn main_agent_tools_only_at_depth_zero() {
        let mut warn = |_: String| {};
        let main_tools: Vec<String> = assemble_tools(&session_at_depth(0), &mut warn)
            .await
            .iter()
            .map(|t| t.name())
            .collect();
        for expected in [
            "Agent",
            "SendMessage",
            "AgentControl",
            "AskUserQuestion",
            "Team",
        ] {
            assert!(
                main_tools.iter().any(|n| n == expected),
                "missing {expected}: {main_tools:?}"
            );
        }
        let sub: Vec<String> = assemble_tools(&session_at_depth(1), &mut warn)
            .await
            .iter()
            .map(|t| t.name())
            .collect();
        assert!(
            sub.iter().any(|n| n == "Agent"),
            "subagents can still be spawned"
        );
        // AskUserQuestion needs a prompt surface: only the session that owns the UI has one.
        for absent in ["AgentControl", "AskUserQuestion", "Team"] {
            assert!(
                !sub.iter().any(|n| n == absent),
                "{absent} must not be handed down: {sub:?}"
            );
        }
    }

    /// D98: one speech tool. A subagent gets `SendMessage` — which is how it
    /// reaches main deliberately — and neither of the two tools that used to
    /// share that job: `notify_user` (a second way to speak to main) and `Post`
    /// (a second way to speak to a room).
    #[tokio::test]
    async fn a_subagent_gets_send_message_and_neither_retired_tool() {
        let mut warn = |_: String| {};
        for depth in [1, 2] {
            let sub: Vec<String> = assemble_tools(&session_at_depth(depth), &mut warn)
                .await
                .iter()
                .map(|t| t.name())
                .collect();
            assert!(
                sub.iter().any(|n| n == "SendMessage"),
                "a subagent at depth {depth} needs a road to main: {sub:?}"
            );
            for retired in ["notify_user", "Post"] {
                assert!(
                    !sub.iter().any(|n| n == retired),
                    "{retired} retired into SendMessage: {sub:?}"
                );
            }
        }
        let main_tools: Vec<String> = assemble_tools(&session_with(0, true), &mut warn)
            .await
            .iter()
            .map(|t| t.name())
            .collect();
        for retired in ["notify_user", "Post"] {
            assert!(
                !main_tools.iter().any(|n| n == retired),
                "{retired} is gone from every assembly: {main_tools:?}"
            );
        }
    }

    /// Room management (experimental): not assembled by default; when enabled the
    /// main session and named depth-1 instances get `Channel`, deeper levels none.
    /// Speaking in a room is `SendMessage(to: "#room")` and needs no tool of its own.
    #[tokio::test]
    async fn channel_tools_gated_by_experimental_flag() {
        let mut warn = |_: String| {};
        let names =
            |tools: Vec<Box<dyn Tool>>| -> Vec<String> { tools.iter().map(|t| t.name()).collect() };
        let off = names(assemble_tools(&session_at_depth(0), &mut warn).await);
        assert!(!off.iter().any(|n| n == "Channel"), "{off:?}");

        let main_tools = names(assemble_tools(&session_with(0, true), &mut warn).await);
        assert!(
            main_tools.iter().any(|n| n == "Channel"),
            "missing Channel: {main_tools:?}"
        );
        let sub_session = std::sync::Arc::new(Session {
            instance: Some("a".into()),
            ..(*session_with(1, true)).clone()
        });
        let sub = names(assemble_tools(&sub_session, &mut warn).await);
        assert!(
            sub.iter().any(|n| n == "Channel"),
            "cohort members form rooms of their own (D95): {sub:?}"
        );
        let deep = std::sync::Arc::new(Session {
            instance: Some("d".into()),
            ..(*session_with(2, true)).clone()
        });
        let deep = names(assemble_tools(&deep, &mut warn).await);
        assert!(
            !deep.iter().any(|n| n == "Channel"),
            "deep layers get no room tools: {deep:?}"
        );
    }

    /// MCP connections run in the background: the turn does not wait for
    /// the handshake; failures are reported once, one turn later.
    #[tokio::test]
    async fn mcp_connects_in_background_and_warns_once() {
        use crate::mcp::McpStatus;
        use crate::settings::McpServerConfig;
        let mut session = (*session_with(0, false)).clone();
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "files".to_string(),
            McpServerConfig {
                command: Some("/bin/echo".to_string()),
                args: Vec::new(),
                env: std::collections::HashMap::new(),
                kind: None,
                url: None,
                headers: std::collections::HashMap::new(),
            },
        );
        session.runtime.mcp = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::mcp::McpManager::new(servers, Default::default()),
        ));
        let session = std::sync::Arc::new(session);

        // Turn 1: no waiting for the handshake, returns immediately (no
        // MCP tools), and no warning either.
        let warnings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let snapshot = |warnings: &std::sync::Arc<std::sync::Mutex<Vec<String>>>| {
            warnings.lock().map(|v| v.clone()).unwrap_or_default()
        };
        let mut collect = {
            let warnings = warnings.clone();
            move |msg: String| {
                if let Ok(mut v) = warnings.lock() {
                    v.push(msg);
                }
            }
        };
        let first = assemble_tools(&session, &mut collect).await;
        assert!(!first.iter().any(|t| t.name().starts_with("mcp__")));
        assert!(
            snapshot(&warnings).is_empty(),
            "no failure has happened yet"
        );

        // Wait for the background connect failure to settle.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let failed = {
                let mgr = session.runtime.mcp.lock().await;
                matches!(mgr.status("files"), McpStatus::Failed { .. })
            };
            if failed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "background connect did not finish within the deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Turn 2: the failure is reported once.
        let mut collect = {
            let warnings = warnings.clone();
            move |msg: String| {
                if let Ok(mut v) = warnings.lock() {
                    v.push(msg);
                }
            }
        };
        let second = assemble_tools(&session, &mut collect).await;
        assert!(!second.iter().any(|t| t.name().starts_with("mcp__")));
        let reported = snapshot(&warnings);
        assert_eq!(reported.len(), 1, "reported once: {reported:?}");
        assert!(reported[0].contains("files"), "{}", reported[0]);

        // Turn 3: no repeat.
        let mut collect = {
            let warnings = warnings.clone();
            move |msg: String| {
                if let Ok(mut v) = warnings.lock() {
                    v.push(msg);
                }
            }
        };
        let third = assemble_tools(&session, &mut collect).await;
        assert!(snapshot(&warnings).len() == 1, "not repeated");
        assert!(!third.iter().any(|t| t.name().starts_with("mcp__")));
    }
}
