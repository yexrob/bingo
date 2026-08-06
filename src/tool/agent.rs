use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;

use crate::permission::PermissionMode;
use crate::query::{Session, UiHooks};
use crate::tool::{parse_input, Tool, ToolContext, ToolError, ToolResult};

const MAX_AGENT_DEPTH: usize = 3;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentInput {
    #[schemars(description = "子代理的独立任务指令")]
    prompt: String,
    /// 后台化：立即返回 async_launched，完成时通知主 agent。
    #[serde(default)]
    #[schemars(description = "异步执行（默认 true）：立即返回任务 id，主 agent 不等待；设 false 则同步等待结果")]
    background: Option<bool>,
    /// 通知条件：子 agent 产出内容出现任一字样即通知主 agent。
    #[serde(default)]
    #[schemars(description = "通知条件：子 agent 产出内容命中任一字样即通知")]
    notify_on: Option<Vec<String>>,
    /// 任务简述（可选），随 header 显示。
    #[serde(default)]
    #[allow(dead_code)]
    #[schemars(description = "任务简述（可选）")]
    description: Option<String>,
    /// 子代理模型（可选）：缺省继承父会话模型。
    #[serde(default)]
    #[schemars(description = "子代理使用的模型（可选，缺省继承父会话）")]
    model: Option<String>,
    /// 子代理 provider（可选，settings.json 的 providers 段）：指定后子代理
    /// 使用该 provider 的端点与 key（独立于父会话的当前 provider）。
    #[serde(default)]
    #[schemars(description = "子代理使用的 provider（可选，settings 的 providers 段；缺省跟随父会话）")]
    provider: Option<String>,
}

/// 子代理工具（D14）：递归 queryLoop，
/// 独立消息历史，结果文本回填父模型。
pub struct AgentTool {
    session: Arc<Session>,
}

impl AgentTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

/// 子代理 UI：捕获文本、无交互（写工具在非 bypass 模式下被拒）。
/// cell 记录已产出字符数（后台 agent 的 interval 进度检查）。
fn subagent_hooks(
    output: Arc<Mutex<String>>,
    cell: Arc<AgentCell>,
    permission_mode: PermissionMode,
    watch: std::sync::Arc<crate::watch::WatchRegistry>,
    id: crate::watch::WatchId,
) -> UiHooks {
    let bypass = permission_mode == PermissionMode::BypassPermissions;
    UiHooks {
        on_event: Box::new(move |event| {
            if let crate::api::types::StreamEvent::TextDelta { text, .. } = event
                && let Ok(mut output) = output.lock()
            {
                output.push_str(text);
                cell.record_chars(text.chars().count());
                // 产出文本进条件引擎（notify_on 命中 → 信号通知）。
                watch.feed_content(id, text);
            }
        }),
        on_tool_ready: Box::new(|_name, _input, _standalone| {}),
        on_tool_done: Box::new(|_| {}),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(|_| {}),
        ask: std::sync::Arc::new(move |_tool_name, _reason| Box::pin(async move { bypass })),
        // 子代理无 UI 可问：AskUserQuestion 视为未回答（模型应避免在子代理中询问）。
        ask_question: std::sync::Arc::new(|_title, _question, _options| {
            Box::pin(async { None })
        }),
    }
}

/// 后台化子 agent：注册 watchable（interval 检查产出量）+ spawn 执行，
/// 立即返回 async_launched；完成/失败经 registry 通知。
impl AgentTool {
    fn launch_background(
        &self,
        params: &AgentInput,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let cell = Arc::new(AgentCell::new());
        let label = agent_label(params);
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![crate::watch::NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = ctx
            .watch
            .register_with_conditions(
                Box::new(AgentWatch {
                    cell: cell.clone(),
                    label: label.clone(),
                    interval: Some(std::time::Duration::from_secs(5)),
                }),
                conditions,
            );
        let sub_session = self.build_sub_session(params)?;
        let prompt = params.prompt.clone();
        let watch = ctx.watch.clone();
        let permission_mode = sub_session.permission_mode;
        tokio::spawn(async move {
            let output = Arc::new(Mutex::new(String::new()));
            let mut ui = subagent_hooks(output.clone(), cell, permission_mode, watch.clone(), id);
            match crate::query::run_query(&sub_session, Vec::new(), &prompt, &mut ui, None).await {
                Ok(_messages) => {
                    let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    watch.set_state(
                        id,
                        crate::watch::WatchState::Done,
                        Some("完成".to_string()),
                        Some(serde_json::json!(if text.trim().is_empty() {
                            "[subagent returned no text]"
                        } else {
                            text.as_str()
                        })),
                    );
                }
                Err(e) => {
                    watch.set_state(
                        id,
                        crate::watch::WatchState::Failed,
                        Some(format!("subagent failed: {e}")),
                        None,
                    );
                }
            }
        });
        Ok(ToolResult {
            content: serde_json::Value::String(serde_json::json!({
                "status": "async_launched",
                "task_id": id.0,
                "label": label,
                "note": "子代理已在后台执行，完成通知会注入下一轮上下文",
            })
            .to_string()),
            is_error: false,
            diff: None,
        })
    }

    /// 构造子代理会话：`model`/`provider` 参数可覆盖继承值（provider 指定时
    /// fork 独立端点 Client，互不影响父会话的当前 provider）。
    fn build_sub_session(&self, params: &AgentInput) -> Result<Arc<Session>, ToolError> {
        let model = params
            .model
            .clone()
            .unwrap_or_else(|| self.session.runtime.model.borrow().clone());
        let client = match &params.provider {
            Some(name) => self
                .session
                .client
                .with_provider(name)
                .map_err(ToolError::failed)?,
            None => self.session.client.clone(),
        };
        let provider_name = params
            .provider
            .clone()
            .unwrap_or_else(|| self.session.runtime.provider.borrow().clone());
        let runtime = crate::query::Runtime::new(
            model,
            None,
            self.session
                .runtime
                .permissions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        );
        let _ = runtime.provider_tx.send(provider_name);
        Ok(Arc::new(Session {
            client,
            runtime,
            permission_mode: self.session.permission_mode,
            settings: self.session.settings.clone(),
            system: self.session.system.clone(),
            depth: self.session.depth + 1,
            home: self.session.home.clone(),
            quiet: self.session.quiet,
            compact_failures: self.session.compact_failures.clone(),
            watch: self.session.watch.clone(),
            tasks: self.session.tasks.clone(),
            last_task_reminder_turn: self.session.last_task_reminder_turn.clone(),
            expand_tasks: self.session.expand_tasks.clone(),
        }))
    }
}

/// 后台 agent 进度：已产出字符数（interval poll 用）。
struct AgentCell {
    chars: std::sync::atomic::AtomicUsize,
}

impl AgentCell {
    fn new() -> Self {
        Self {
            chars: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    fn record_chars(&self, n: usize) {
        self.chars
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: crate::watch::WatchState::Running,
            detail: Some(format!(
                "已产出 {} 字符",
                self.chars.load(std::sync::atomic::Ordering::SeqCst)
            )),
            payload: None,
            signal: None,
        }
    }
}

struct AgentWatch {
    cell: Arc<AgentCell>,
    label: String,
    interval: Option<std::time::Duration>,
}

impl crate::watch::Watchable for AgentWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        self.cell.poll()
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        self.interval
    }
}

#[async_trait]
impl Tool for AgentTool {
    fn name(&self) -> String {
        "Agent".to_string()
    }

    fn description(&self) -> String {
        "派生子代理执行独立任务（深度受限）。默认异步执行：立即返回 async_launched 任务 id，主 agent 不等待，子代理完成时自动通知；background:false 可同步等待结果；notify_on 条件命中子代理产出内容时也会通知。"
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<AgentInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: AgentInput = parse_input(&input)?;
        if self.session.depth >= MAX_AGENT_DEPTH {
            return Err(ToolError::failed(format!(
                "max agent depth ({MAX_AGENT_DEPTH}) exceeded"
            )));
        }
        // 默认异步：主 agent 不等待子 agent，完成通知注入下一轮。
        if params.background.unwrap_or(true) {
            return self.launch_background(&params, ctx);
        }

        let sub_session = self.build_sub_session(&params)?;

        // 前台子 agent 同样可 watch：Running（产出字符量）→ Done/Failed。
        let cell = Arc::new(AgentCell::new());
        let label = agent_label(&params);
        let conditions = params
            .notify_on
            .clone()
            .map(|p| vec![crate::watch::NotifyCondition::Contains(p)])
            .unwrap_or_default();
        let id = ctx
            .watch
            .register_with_conditions(
                Box::new(AgentWatch {
                    cell: cell.clone(),
                    label: label.clone(),
                    interval: Some(std::time::Duration::from_secs(5)),
                }),
                conditions,
            );
        let output = Arc::new(Mutex::new(String::new()));
        let mut ui = subagent_hooks(
            output.clone(),
            cell.clone(),
            sub_session.permission_mode,
            ctx.watch.clone(),
            id,
        );
        match crate::query::run_query(&sub_session, Vec::new(), &params.prompt, &mut ui, None)
            .await
        {
            Ok(_messages) => {
                let text = output.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let content = if text.trim().is_empty() {
                    "[subagent returned no text]".to_string()
                } else {
                    text
                };
                ctx.watch.set_state(
                    id,
                    crate::watch::WatchState::Done,
                    Some("完成".to_string()),
                    Some(serde_json::json!(content.clone())),
                );
                Ok(ToolResult {
                    content: serde_json::Value::String(content),
                    is_error: false,
                    diff: None,
                })
            }
            Err(e) => {
                ctx.watch.set_state(
                    id,
                    crate::watch::WatchState::Failed,
                    Some(format!("subagent failed: {e}")),
                    None,
                );
                Err(ToolError::failed(format!("subagent failed: {e}")))
            }
        }
    }
}

/// Watch 行 label：优先 description，否则 prompt 摘要。
fn agent_label(params: &AgentInput) -> String {
    format!(
        "Agent: {}",
        params
            .description
            .clone()
            .unwrap_or_else(|| params.prompt.chars().take(40).collect())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{Runtime, Session};

    fn parent_session() -> (Arc<Session>, Arc<crate::api::client::Client>) {
        let mut settings = crate::settings::Settings::default();
        settings.api_key = Some("sk-parent".into());
        settings.api_base_url = Some("https://parent.example".into());
        settings.providers.insert(
            "ds".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
            },
        );
        let client = Arc::new(crate::api::client::Client::from_settings(&settings).unwrap());
        let mut runtime = Runtime::new("parent-model".into(), None, Default::default());
        runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
            Default::default(),
            Default::default(),
        )));
        let session = Arc::new(Session {
            client: (*client).clone(),
            runtime,
            permission_mode: crate::permission::PermissionMode::Default,
            settings,
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        });
        (session, client)
    }

    fn params(prompt: &str) -> AgentInput {
        AgentInput {
            prompt: prompt.into(),
            background: None,
            notify_on: None,
            description: None,
            model: None,
            provider: None,
        }
    }

    #[test]
    fn sub_session_inherits_model_and_shared_endpoint() {
        let (session, client) = parent_session();
        let tool = AgentTool::new(session.clone());
        let sub = tool.build_sub_session(&params("do it")).unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "parent-model");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-parent".to_string(), "https://parent.example".to_string())
        );
        // 不指定 provider：共享父端点（切换父 provider 子跟随）。
        client.set_provider("ds").unwrap();
        assert_eq!(
            sub.client.current_endpoint().0,
            "sk-ds",
            "共享端点跟随父会话切换"
        );
    }

    #[test]
    fn sub_session_overrides_model_and_provider() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session.clone());
        let mut p = params("do it");
        p.model = Some("sub-model".into());
        p.provider = Some("ds".into());
        let sub = tool.build_sub_session(&p).unwrap();
        assert_eq!(*sub.runtime.model.borrow(), "sub-model");
        assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
        assert_eq!(
            sub.client.current_endpoint(),
            ("sk-ds".to_string(), "https://api.deepseek.com".to_string())
        );
        // fork 独立端点：父会话不受影响。
        assert_eq!(session.client.current_endpoint().0, "sk-parent");
    }

    #[test]
    fn sub_session_unknown_provider_errors() {
        let (session, _client) = parent_session();
        let tool = AgentTool::new(session);
        let mut p = params("do it");
        p.provider = Some("nope".into());
        assert!(tool.build_sub_session(&p).is_err(), "未知 provider 报错");
    }

    #[test]
    fn schema_exposes_model_and_provider() {
        let tool = AgentTool::new(Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
        }));
        let schema = tool.input_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("model"), "schema 含 model");
        assert!(props.contains_key("provider"), "schema 含 provider");
    }
}
