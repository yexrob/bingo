//! 频道工具（实验特性 `experimental.agentChannels`）。
//!
//! `Post`：任何成员（hub + depth-1 子代理）向频道发言——发件人由会话
//! 实例名盖戳（模型无法伪造）；serial 频道落后即弹回附增量（工具结果，
//! 非错误——模型在同回合内阅读增量后自判照发/改发/放弃）。
//! `Channel`：hub 专用的房间管理（create/invite/kick/list）。
//! 投递唤醒：消息进全体成员信箱；空闲成员立即唤醒，忙碌成员回合边界
//! 批量注入；hub 走 hub_mail 在下一轮推理前注入。沉默 = 不调用 Post。

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::DepositOutcome;
use crate::channels::{ChannelMode, PostOutcome, HUB_NAME};
use crate::query::Session;
use crate::tool::agent::{absorb_inbox, excerpt, spawn_agent_loop};
use crate::tool::{parse_input, Tool, ToolContext, ToolError, ToolResult};
use crate::watch::{WatchKind, WatchState};

/// 频道展示行（◇ #名字 · N 条 · 最近……）：无轮询，post 时主动更新。
struct ChannelWatch {
    label: String,
}

impl crate::watch::Watchable for ChannelWatch {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: WatchState::Running,
            detail: Some("0 条".to_string()),
            payload: None,
            signal: None,
        }
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        None
    }
    fn kind(&self) -> WatchKind {
        WatchKind::Channel
    }
}

/// post 后刷新频道展示行（detail = 条数与最近发言，payload = 日志尾部）。
fn refresh_channel_row(session: &Arc<Session>, name: &str) {
    if let Some((Some(id), detail, payload)) = session.channels.row_snapshot(name) {
        session.watch.set_state(
            id,
            WatchState::Running,
            Some(detail),
            Some(serde_json::json!(payload)),
        );
    }
}

/// 本会话在频道中的成员名：子代理 = 实例名，主会话 = main。
fn sender_of(session: &Arc<Session>) -> String {
    session
        .instance
        .clone()
        .unwrap_or_else(|| HUB_NAME.to_string())
}

/// 发言结果（deliver_post 的两种出口）。
pub(crate) enum PostDelivery {
    Sent { seq: u64 },
    Stale { missed: Vec<crate::channels::ChannelMessage> },
}

/// 发言 + 投递唤醒 + 展示行刷新——Post 工具与 TUI 频道房间共用同一条
/// 路径（用户在房间里以 `user` 身份发言走的也是这里）。
pub(crate) fn deliver_post(
    session: &Arc<Session>,
    watch: &Arc<crate::watch::WatchRegistry>,
    from: &str,
    channel: &str,
    text: &str,
) -> Result<PostDelivery, String> {
    match session.channels.post(from, channel, text)? {
        PostOutcome::Sent { seq, deliveries } => {
            refresh_channel_row(session, channel);
            // 投递唤醒：空闲成员立即开回合；忙碌成员信箱累积。
            for (member, msg) in deliveries {
                let item = crate::agents::InboxItem::Channel {
                    channel: channel.to_string(),
                    from: msg.from.clone(),
                    text: msg.text.clone(),
                    seq: msg.seq,
                };
                if let DepositOutcome::Start {
                    session: sub,
                    history,
                    items,
                } = session.agents.deposit(&member, item)
                {
                    let prompt = absorb_inbox(&sub.channels, &member, &items);
                    let n = session.agents.next_run(&member);
                    spawn_agent_loop(
                        session.agents.clone(),
                        watch.clone(),
                        member.clone(),
                        sub,
                        history,
                        prompt.clone(),
                        format!("{member} #{n} · {}", excerpt(&prompt)),
                        Vec::new(),
                    );
                }
            }
            Ok(PostDelivery::Sent { seq })
        }
        PostOutcome::Stale { missed } => Ok(PostDelivery::Stale { missed }),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct PostInput {
    #[schemars(description = "频道名（不带 #）")]
    channel: String,
    #[schemars(description = "发言内容")]
    message: String,
}

/// 向频道发言（发件人 = 本会话实例名，runtime 盖戳）。
pub struct PostTool {
    session: Arc<Session>,
}

impl PostTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for PostTool {
    fn name(&self) -> String {
        "Post".to_string()
    }
    fn description(&self) -> String {
        let who = sender_of(&self.session);
        format!(
            "向 agent 频道发言。你在频道中的名字是 {who}（发件人由运行时盖戳，不可伪造）。\
频道消息会进入全体成员的上下文（同一顺序）；serial 频道里若你落后于最新消息，\
发送会被退回并附上新增内容——阅读后重新决定照发/修改/放弃。\
不需要发言时不调用本工具即可（沉默无成本，也不会唤醒他人）。"
        )
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<PostInput>()
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
        let params: PostInput = parse_input(&input)?;
        let from = sender_of(&self.session);
        let channel = params.channel.trim_start_matches('#').to_string();
        match deliver_post(&self.session, &ctx.watch, &from, &channel, &params.message)
            .map_err(ToolError::failed)?
        {
            PostDelivery::Sent { seq } => Ok(ToolResult {
                content: serde_json::Value::String(format!(
                    "已发送（#{channel} 第 {seq} 条）"
                )),
                is_error: false,
                diff: None,
            }),
            PostDelivery::Stale { missed } => {
                let lines: Vec<String> = missed
                    .iter()
                    .map(|m| format!("[#{channel} 第{}条] {}: {}", m.seq, m.from, m.text))
                    .collect();
                Ok(ToolResult {
                    content: serde_json::Value::String(format!(
                        "未送出——你拟发言期间频道已有新消息：\n{}\n\
请基于最新内容重新决定：照发（原样重新调用）、修改后再发、或放弃发言。",
                        lines.join("\n")
                    )),
                    is_error: false,
                    diff: None,
                })
            }
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChannelAction {
    /// 建频道（hub 自动入席）。
    Create,
    /// 拉成员入频道（从当前消息头开始听，无 backlog）。
    Invite,
    /// 移出成员。
    Kick,
    /// 列出全部频道。
    List,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ChannelInput {
    #[schemars(description = "操作：create 建频道 / invite 拉人 / kick 移出 / list 列出")]
    action: ChannelAction,
    #[serde(default)]
    #[schemars(description = "频道名（create/invite/kick 必填，不带 #）")]
    channel: Option<String>,
    #[serde(default)]
    #[schemars(description = "成员实例名：create 为初始名单，invite/kick 为目标（单个）")]
    members: Option<Vec<String>>,
    #[serde(default)]
    #[schemars(description = "发言模式（create 用）：serial（开口前必须见过最新，落后弹回）/ free（允许交叉）；缺省 serial")]
    mode: Option<String>,
}

/// 频道管理（hub 专用）：建房、成员进出、清单。
pub struct ChannelTool {
    session: Arc<Session>,
}

impl ChannelTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    fn require_channel(input: &ChannelInput) -> Result<&str, ToolError> {
        input
            .channel
            .as_deref()
            .map(|c| c.trim_start_matches('#'))
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ToolError::failed("需要 channel 参数（频道名）"))
    }

    /// cohort 校验：成员必须是已存在的直接子代理（depth==1）。
    fn validate_member(&self, member: &str) -> Result<(), ToolError> {
        if member == HUB_NAME {
            return Ok(());
        }
        match self.session.agents.depth_of(member) {
            Some(1) => Ok(()),
            Some(_) => Err(ToolError::failed(format!(
                "{member} 不是直接子代理（频道成员限主会话直接派生的实例）"
            ))),
            None => Err(ToolError::failed(format!(
                "没有名为 {member} 的子代理实例（先用 Agent 派生）"
            ))),
        }
    }
}

#[async_trait]
impl Tool for ChannelTool {
    fn name(&self) -> String {
        "Channel".to_string()
    }
    fn description(&self) -> String {
        "管理 agent 频道：create 建频道（members 为子代理实例名单，你自动入席为 main；mode 缺省 serial）、invite/kick 成员进出、list 清单。频道消息进全体成员上下文（同序）；成员发言用 Post。".to_string()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<ChannelInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        input.get("action").and_then(|a| a.as_str()) == Some("list")
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: ChannelInput = parse_input(&input)?;
        let text = match params.action {
            ChannelAction::Create => {
                let name = Self::require_channel(&params)?.to_string();
                let members = params.members.clone().unwrap_or_default();
                for m in &members {
                    self.validate_member(m)?;
                }
                let mode = match &params.mode {
                    Some(m) => ChannelMode::parse(m).map_err(ToolError::failed)?,
                    None => ChannelMode::Serial,
                };
                self.session
                    .channels
                    .create(&name, members.clone(), mode)
                    .map_err(ToolError::failed)?;
                let id = ctx.watch.register_with_conditions(
                    Box::new(ChannelWatch {
                        label: format!("#{name}"),
                    }),
                    Vec::new(),
                );
                self.session.channels.set_watch(&name, id);
                format!(
                    "已建频道 #{name}（{}，成员：main{}{}）",
                    mode.label(),
                    if members.is_empty() { "" } else { ", " },
                    members.join(", ")
                )
            }
            ChannelAction::Invite => {
                let name = Self::require_channel(&params)?.to_string();
                let member = params
                    .members
                    .as_deref()
                    .and_then(|m| m.first())
                    .ok_or_else(|| ToolError::failed("invite 需要 members（目标实例名）"))?
                    .clone();
                self.validate_member(&member)?;
                self.session
                    .channels
                    .invite(&name, &member)
                    .map_err(ToolError::failed)?;
                format!("{member} 已加入 #{name}（从当前消息头开始听）")
            }
            ChannelAction::Kick => {
                let name = Self::require_channel(&params)?.to_string();
                let member = params
                    .members
                    .as_deref()
                    .and_then(|m| m.first())
                    .ok_or_else(|| ToolError::failed("kick 需要 members（目标实例名）"))?
                    .clone();
                self.session
                    .channels
                    .kick(&name, &member)
                    .map_err(ToolError::failed)?;
                format!("{member} 已移出 #{name}")
            }
            ChannelAction::List => {
                let statuses = self.session.channels.list();
                if statuses.is_empty() {
                    "当前没有频道".to_string()
                } else {
                    statuses
                        .iter()
                        .map(|s| {
                            format!(
                                "- #{}（{}，{} 条{}）：{}",
                                s.name,
                                s.mode.label(),
                                s.seq,
                                if s.frozen { "，已冻结" } else { "" },
                                s.members.join(", ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentRegistry;

    fn hub_session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
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
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
        })
    }

    /// 同一注册表/频道表下的 depth-1 子会话（实例名盖戳）。
    fn sub_session(hub: &Arc<Session>, instance: &str) -> Arc<Session> {
        Arc::new(Session {
            depth: 1,
            instance: Some(instance.to_string()),
            ..(**hub).clone()
        })
    }

    fn ctx(session: &Arc<Session>) -> ToolContext {
        ToolContext {
            cwd: std::path::PathBuf::from("/tmp"),
            watch: session.watch.clone(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }
    }

    #[tokio::test]
    async fn create_validates_cohort_and_registers_row() {
        let hub = hub_session();
        let tool = ChannelTool::new(hub.clone());
        // 未知成员拒绝。
        let err = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["ghost"]}),
                &ctx(&hub),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        // depth-1 成员通过；hub 自动入席。
        hub.agents
            .insert("a", None, "a".into(), sub_session(&hub, "a"));
        let out = tool
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a"]}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(text.contains("#t") && text.contains("serial"), "{text}");
        // 深层实例拒绝入频道。
        let deep = Arc::new(Session {
            depth: 2,
            ..(*sub_session(&hub, "deep")).clone()
        });
        hub.agents.insert("deep", None, "d".into(), deep);
        let err = tool
            .call(
                serde_json::json!({"action": "invite", "channel": "t", "members": ["deep"]}),
                &ctx(&hub),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("直接子代理"), "{err}");
        // list 输出成员与模式。
        let out = tool
            .call(serde_json::json!({"action": "list"}), &ctx(&hub))
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("main, user, a"));
        assert!(tool.is_read_only(&serde_json::json!({"action": "list"})));
    }

    #[tokio::test]
    async fn post_stamps_sender_and_queues_to_running_members() {
        let hub = hub_session();
        hub.agents
            .insert("a", None, "a".into(), sub_session(&hub, "a"));
        hub.agents
            .insert("b", None, "b".into(), sub_session(&hub, "b"));
        let mgmt = ChannelTool::new(hub.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "t", "members": ["a", "b"], "mode": "free"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        // a（Running 状态）视角发言：盖戳 a；b 在 Running → 信箱累积。
        let post_a = PostTool::new(sub_session(&hub, "a"));
        let out = post_a
            .call(
                serde_json::json!({"channel": "t", "message": "大家好"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("第 1 条"));
        let (_, items) = hub
            .agents
            .finish("b", Vec::new())
            .unwrap_or_else(|| panic!("b 信箱应有消息"));
        assert!(
            matches!(&items[..], [crate::agents::InboxItem::Channel { from, text, .. }]
                if from == "a" && text == "大家好"),
            "盖戳为 a"
        );
        // hub 发言：盖戳 main；hub_mail 只收别人的。
        let post_hub = PostTool::new(hub.clone());
        let _ = post_hub
            .call(
                serde_json::json!({"channel": "t", "message": "肃静"}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let mail = hub.channels.drain_hub_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("a: 大家好"));
        // 非成员发言报错。
        let post_c = PostTool::new(sub_session(&hub, "c"));
        let err = post_c
            .call(serde_json::json!({"channel": "t", "message": "x"}), &ctx(&hub))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不是"), "{err}");
    }

    #[tokio::test]
    async fn serial_bounce_returns_increments_as_result() {
        let hub = hub_session();
        hub.agents
            .insert("a", None, "a".into(), sub_session(&hub, "a"));
        hub.agents
            .insert("b", None, "b".into(), sub_session(&hub, "b"));
        let mgmt = ChannelTool::new(hub.clone());
        let _ = mgmt
            .call(
                serde_json::json!({"action": "create", "channel": "count", "members": ["a", "b"]}),
                &ctx(&hub),
            )
            .await
            .unwrap();
        let post_a = PostTool::new(sub_session(&hub, "a"));
        let post_b = PostTool::new(sub_session(&hub, "b"));
        let _ = post_a
            .call(serde_json::json!({"channel": "count", "message": "1"}), &ctx(&hub))
            .await
            .unwrap();
        // b 落后 → 弹回（工具结果，非错误），附增量。
        let out = post_b
            .call(serde_json::json!({"channel": "count", "message": "1"}), &ctx(&hub))
            .await
            .unwrap();
        let text = out.content.as_str().unwrap();
        assert!(!out.is_error);
        assert!(text.contains("未送出") && text.contains("a: 1"), "{text}");
        // 重发落地（模型改口）。
        let out = post_b
            .call(serde_json::json!({"channel": "count", "message": "2"}), &ctx(&hub))
            .await
            .unwrap();
        assert!(out.content.as_str().unwrap().contains("第 2 条"));
    }
}
