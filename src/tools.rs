use std::sync::Arc;

use crate::query::Session;
use crate::tool::agent::{AgentControlTool, AgentTool, SendMessageTool};
use crate::tool::ask::AskUserQuestionTool;
use crate::tool::bash::BashTool;
use crate::tool::edit::EditTool;
use crate::tool::experience::{
    ExperienceCommitTool, ExperienceForgetTool, ExperienceProposeTool, ExperienceQueryTool,
};
use crate::tool::glob::GlobTool;
use crate::tool::grep::GrepTool;
use crate::tool::read::ReadTool;
use crate::tool::skill::SkillTool;
use crate::tool::task::{TaskCreateTool, TaskGetTool, TaskListTool, TaskUpdateTool};
use crate::tool::webfetch::WebFetchTool;
use crate::tool::websearch::WebSearchTool;
use crate::tool::write::WriteTool;
use crate::tool::Tool;

/// Base tool pool + MCP + subagents.
pub async fn assemble_tools(
    session: &Arc<Session>,
    on_warning: &mut (dyn Fn(String) + Send),
) -> Vec<Box<dyn Tool>> {
    // Skill/agent-definition scanning is synchronous IO: move it off the runtime thread
    // (on a cache hit it's just a few stats).
    let home = session.home.clone();
    let (skills, agent_defs) = tokio::task::spawn_blocking(move || {
        let cwd = std::env::current_dir().unwrap_or_default();
        (
            crate::skills::load_skills(&home, &cwd),
            crate::agents::load_agent_defs(&home, &cwd),
        )
    })
    .await
    .unwrap_or_default();
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(BashTool::with_max_output_chars(
            session.settings.max_bash_output_chars,
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
        Box::new(AskUserQuestionTool),
        Box::new(SkillTool::new(skills)),
        Box::new(ExperienceProposeTool),
        Box::new(ExperienceCommitTool),
        Box::new(ExperienceQueryTool),
        Box::new(ExperienceForgetTool),
    ];
    // hub-and-spoke: continuation and lifecycle management only on the main session
    // (subagents don't manage siblings).
    let channels_on = session.settings.experimental.agent_channels;
    if session.depth == 0 {
        tools.push(Box::new(SendMessageTool::new(session.clone())));
        tools.push(Box::new(AgentControlTool::new(session.clone())));
        if channels_on {
            tools.push(Box::new(crate::tool::channel::ChannelTool::new(
                session.clone(),
            )));
            tools.push(Box::new(crate::tool::channel::PostTool::new(
                session.clone(),
            )));
        }
    } else if channels_on && session.depth == 1 && session.instance.is_some() {
        // Channel cohort (experimental): direct subagents only get the posting tool.
        tools.push(Box::new(crate::tool::channel::PostTool::new(
            session.clone(),
        )));
    }
    let mcp = {
        let mgr = session.runtime.mcp.clone();
        let (tools, warnings, pending) = {
            let mut guard = mgr.lock().await;
            let warnings = guard.drain_unreported_failures();
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
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            last_task_reminder_turn: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
        })
    }

    #[tokio::test]
    async fn assembles_task_tools() {
        let mut warn = |_: String| {};
        let tools = assemble_tools(&session_at_depth(0), &mut warn).await;
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        for expected in ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}: {names:?}");
        }
    }

    /// hub-and-spoke: continuation/lifecycle tools only assembled for the main session,
    /// not subagents.
    #[tokio::test]
    async fn hub_agent_tools_only_at_depth_zero() {
        let mut warn = |_: String| {};
        let hub: Vec<String> = assemble_tools(&session_at_depth(0), &mut warn)
            .await
            .iter()
            .map(|t| t.name())
            .collect();
        for expected in ["Agent", "SendMessage", "AgentControl"] {
            assert!(hub.iter().any(|n| n == expected), "missing {expected}: {hub:?}");
        }
        let sub: Vec<String> = assemble_tools(&session_at_depth(1), &mut warn)
            .await
            .iter()
            .map(|t| t.name())
            .collect();
        assert!(sub.iter().any(|n| n == "Agent"), "子代理仍可派生");
        for absent in ["SendMessage", "AgentControl"] {
            assert!(!sub.iter().any(|n| n == absent), "{absent} 不应下发: {sub:?}");
        }
    }

    /// Channel tools (experimental): not assembled by default; when enabled the hub gets
    /// Channel+Post, named depth-1 instances only get Post, deeper levels none.
    #[tokio::test]
    async fn channel_tools_gated_by_experimental_flag() {
        let mut warn = |_: String| {};
        let names = |tools: Vec<Box<dyn Tool>>| -> Vec<String> {
            tools.iter().map(|t| t.name()).collect()
        };
        let off = names(assemble_tools(&session_at_depth(0), &mut warn).await);
        assert!(!off.iter().any(|n| n == "Channel" || n == "Post"), "{off:?}");

        let hub = names(assemble_tools(&session_with(0, true), &mut warn).await);
        for expected in ["Channel", "Post"] {
            assert!(hub.iter().any(|n| n == expected), "missing {expected}: {hub:?}");
        }
        let sub_session = std::sync::Arc::new(Session {
            instance: Some("a".into()),
            ..(*session_with(1, true)).clone()
        });
        let sub = names(assemble_tools(&sub_session, &mut warn).await);
        assert!(sub.iter().any(|n| n == "Post"), "cohort 成员可发言: {sub:?}");
        assert!(!sub.iter().any(|n| n == "Channel"), "频道管理仅 hub: {sub:?}");
        let deep = std::sync::Arc::new(Session {
            instance: Some("d".into()),
            ..(*session_with(2, true)).clone()
        });
        let deep = names(assemble_tools(&deep, &mut warn).await);
        assert!(!deep.iter().any(|n| n == "Post"), "深层不入频道: {deep:?}");
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
        assert!(snapshot(&warnings).is_empty(), "失败尚未发生");

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
                "后台连接未在期限内完成"
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
        assert_eq!(reported.len(), 1, "只报一次: {reported:?}");
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
        assert!(snapshot(&warnings).len() == 1, "不再重复");
        assert!(!third.iter().any(|t| t.name().starts_with("mcp__")));
    }
}
