use std::sync::Arc;

use crate::query::Session;
use crate::tool::agent::{AgentControlTool, AgentTool, SendMessageTool};
use crate::tool::ask::AskUserQuestionTool;
use crate::tool::bash::BashTool;
use crate::tool::edit::EditTool;
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
    ];
    // hub-and-spoke：续话与生命周期管理只在主会话（子代理不管理兄弟）。
    if session.depth == 0 {
        tools.push(Box::new(SendMessageTool::new(session.clone())));
        tools.push(Box::new(AgentControlTool::new(session.clone())));
    }
    let mcp = {
        let mut mgr = session.runtime.mcp.lock().await;
        let results = mgr.connect_all().await;
        for (name, result) in results {
            if let Err(detail) = result {
                on_warning(format!("MCP {name}: {detail}"));
            }
        }
        mgr.tools()
    };
    if !session.quiet && !mcp.is_empty() {
        eprintln!("[bingo] connected {} MCP tools", mcp.len());
    }
    tools.extend(mcp);
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_at_depth(depth: usize) -> Arc<Session> {
        std::sync::Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "https://example.com".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
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
}
