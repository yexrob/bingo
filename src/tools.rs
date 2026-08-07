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

/// 基础工具池 + MCP + 子代理。
pub async fn assemble_tools(
    session: &Arc<Session>,
    on_warning: &mut (dyn Fn(String) + Send),
) -> Vec<Box<dyn Tool>> {
    // 技能/agent 定义扫描是同步 IO：挪出运行时线程（缓存命中时也只是几次 stat）。
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
        Box::new(BashTool::new()),
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
    // hub-and-spoke：续话与生命周期管理只在主会话（子代理不管理兄弟）。
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
        // 频道 cohort（实验特性）：直接子代理只拿发言工具。
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
            // 后台连接：回合不等待握手（坏服务器超时后记失败，
            // 下一回合 assemble 时经 drain_unreported_failures 报告一次）。
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

    /// hub-and-spoke：续话/生命周期工具只装配给主会话，子代理没有。
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

    /// 频道工具（实验特性）：默认不装配；开启后 hub 拿 Channel+Post，
    /// depth-1 具名实例只拿 Post，更深层没有。
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

    /// MCP 连接在后台执行：回合不等待握手，失败延迟一回合报告一次。
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

        // 第一回合：不等待握手，立即返回（无 MCP 工具），也无警告。
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

        // 等后台连接失败落定。
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

        // 第二回合：报告一次失败。
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

        // 第三回合：不再重复。
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
