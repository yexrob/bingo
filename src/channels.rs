//! agent 频道（D29 第二步，实验特性 `experimental.agentChannels`）。
//!
//! 引擎只有四个原语，其余全是提示词：
//! 1. 频道 = 成员名单（可见性：消息进全体成员信箱，全序投递）；
//! 2. serial | free 提交校验（serial：发送者落后于频道头即弹回并附增量，
//!    运行时只判"陈旧"，语义冲突由模型自判——乐观锁，模型做冲突解决器）；
//! 3. 唤醒跟随投递（能力普遍、选择自主：沉默 = 醒后不 Post，零成本吸收态）；
//! 4. 发件人 runtime 盖戳（from 取自会话实例名，不可伪造）+ 预算闸
//!    （超限冻结频道并通知主 agent，不静默烧钱）。
//!
//! 本模块是纯状态（无 watch/agents 依赖）；投递唤醒与展示行更新
//! 由工具层（`tool::channel`）编排。主 agent 的成员名恒为 `main`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 主 agent 在频道中的保留成员名。
pub const HUB_NAME: &str = "main";
/// 用户（人）在频道中的保留成员名：TUI 频道房间里以此身份发言，
/// 微信式视图中靠右显示；与 main 一样自动入席、不可移出、预算豁免。
pub const USER_NAME: &str = "user";

/// 频道发言模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// 提交校验：开口前必须见过最新消息，落后即弹回（涌现定序）。
    Serial,
    /// 允许交叉（头脑风暴、并行独立产出）。
    Free,
}

impl ChannelMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Serial => "serial",
            Self::Free => "free",
        }
    }
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "serial" => Ok(Self::Serial),
            "free" => Ok(Self::Free),
            other => Err(format!("未知模式 {other}（可用：serial / free）")),
        }
    }
}

/// 一条频道消息（seq 为频道内全序）。
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub seq: u64,
    pub from: String,
    pub text: String,
}

/// 预算：超限冻结（读 settings.experimental，缺省 500/50）。
#[derive(Debug, Clone, Copy)]
pub struct ChannelLimits {
    /// 每频道消息总上限。
    pub channel_total: u64,
    /// 每 agent 每频道发言上限。
    pub per_agent: u64,
}

impl Default for ChannelLimits {
    fn default() -> Self {
        Self {
            channel_total: 500,
            per_agent: 50,
        }
    }
}

impl ChannelLimits {
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        let d = Self::default();
        Self {
            channel_total: settings
                .experimental
                .channel_message_limit
                .unwrap_or(d.channel_total),
            per_agent: settings
                .experimental
                .agent_message_limit
                .unwrap_or(d.per_agent),
        }
    }
}

/// post 的结果。
#[derive(Debug)]
pub enum PostOutcome {
    /// 已落地：向这些成员投递（不含发送者与 hub——hub 走 hub_mail）。
    Sent {
        seq: u64,
        deliveries: Vec<(String, ChannelMessage)>,
    },
    /// serial 落后：未送出，附错过的消息（已计入发送者已读——
    /// 消息经工具结果进入其上下文，由它自判照发/改发/放弃）。
    Stale { missed: Vec<ChannelMessage> },
}

/// list 快照。
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub name: String,
    pub members: Vec<String>,
    pub mode: ChannelMode,
    pub seq: u64,
    pub frozen: bool,
}

struct Channel {
    members: Vec<String>,
    mode: ChannelMode,
    seq: u64,
    log: Vec<ChannelMessage>,
    /// 每成员已见到的频道序号（serial 提交校验的游标）。
    seen: HashMap<String, u64>,
    /// 每成员发言计数（per_agent 预算）。
    sent: HashMap<String, u64>,
    frozen: bool,
    /// 频道级消息总上限覆盖（D31 team.json channel.messageLimit；
    /// None = 用 registry 级 ChannelLimits.channel_total）。
    message_limit: Option<u64>,
    /// 展示行（◇ #名字）的 watch 条目。
    watch_id: Option<crate::watch::WatchId>,
}

struct Inner {
    channels: HashMap<String, Channel>,
    /// 待注入主 agent 上下文的频道消息（格式化文本）。
    hub_mail: Vec<String>,
    limits: ChannelLimits,
}

/// 会话级频道注册中心（Session 持有 Arc，子会话共享）。
pub struct ChannelRegistry {
    inner: Mutex<Inner>,
}

fn format_hub_line(channel: &str, msg: &ChannelMessage) -> String {
    format!("[#{channel} 第{}条] {}: {}", msg.seq, msg.from, msg.text)
}

impl ChannelRegistry {
    pub fn new(limits: ChannelLimits) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                channels: HashMap::new(),
                hub_mail: Vec::new(),
                limits,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 建频道（hub 自动入成员）。成员的存在性/深度由工具层校验。
    pub fn create(
        &self,
        name: &str,
        members: Vec<String>,
        mode: ChannelMode,
    ) -> Result<(), String> {
        let name = name.trim_start_matches('#');
        if name.is_empty() {
            return Err("频道名不能为空".to_string());
        }
        let mut inner = self.lock();
        if inner.channels.contains_key(name) {
            return Err(format!("频道 #{name} 已存在"));
        }
        let mut all = vec![HUB_NAME.to_string(), USER_NAME.to_string()];
        for m in members {
            if m != HUB_NAME && m != USER_NAME && !all.contains(&m) {
                all.push(m);
            }
        }
        inner.channels.insert(
            name.to_string(),
            Channel {
                members: all,
                mode,
                seq: 0,
                log: Vec::new(),
                seen: HashMap::new(),
                sent: HashMap::new(),
                frozen: false,
                message_limit: None,
                watch_id: None,
            },
        );
        Ok(())
    }

    /// 频道级消息总上限覆盖（D31 team.json channel.messageLimit）。
    pub fn set_message_limit(&self, name: &str, limit: u64) -> Result<(), String> {
        if limit == 0 {
            return Err("messageLimit 必须为正整数".to_string());
        }
        let mut inner = self.lock();
        let Some(ch) = inner.channels.get_mut(name) else {
            return Err(format!("没有频道 #{name}"));
        };
        ch.message_limit = Some(limit);
        Ok(())
    }

    pub fn set_watch(&self, name: &str, id: crate::watch::WatchId) {
        if let Some(ch) = self.lock().channels.get_mut(name) {
            ch.watch_id = Some(id);
        }
    }

    pub fn invite(&self, name: &str, member: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let Some(ch) = inner.channels.get_mut(name) else {
            return Err(format!("没有频道 #{name}"));
        };
        if ch.members.iter().any(|m| m == member) {
            return Err(format!("{member} 已在 #{name} 中"));
        }
        ch.members.push(member.to_string());
        // 迟入不补发 backlog：从当前头开始"听"（seen 置为当前 seq，
        // serial 校验不会因为入场前的历史弹回）。
        let seq = ch.seq;
        ch.seen.insert(member.to_string(), seq);
        Ok(())
    }

    pub fn kick(&self, name: &str, member: &str) -> Result<(), String> {
        if member == HUB_NAME || member == USER_NAME {
            return Err(format!("{member} 是保留成员，不可移出频道"));
        }
        let mut inner = self.lock();
        let Some(ch) = inner.channels.get_mut(name) else {
            return Err(format!("没有频道 #{name}"));
        };
        let before = ch.members.len();
        ch.members.retain(|m| m != member);
        if ch.members.len() == before {
            return Err(format!("{member} 不在 #{name} 中"));
        }
        Ok(())
    }

    /// 实例删除时清出全部频道（工具层在 AgentControl delete 时调用）。
    pub fn remove_member_everywhere(&self, member: &str) {
        let mut inner = self.lock();
        for ch in inner.channels.values_mut() {
            ch.members.retain(|m| m != member);
        }
    }

    /// 发消息。运行时只做三件事：盖戳（from 由调用方从会话实例名取，
    /// 模型无法指定）、serial 陈旧校验、预算闸；说什么/是否重发全归模型。
    pub fn post(&self, from: &str, name: &str, text: &str) -> Result<PostOutcome, String> {
        let mut inner = self.lock();
        let limits = inner.limits;
        let hub_line;
        let outcome = {
            let Some(ch) = inner.channels.get_mut(name) else {
                return Err(format!("没有频道 #{name}"));
            };
            if !ch.members.iter().any(|m| m == from) {
                return Err(format!("{from} 不是 #{name} 的成员"));
            }
            // 频道级上限：team 覆盖优先，否则 registry 级。
            let channel_total = ch.message_limit.unwrap_or(limits.channel_total);
            if ch.frozen {
                return Err(format!(
                    "#{name} 已冻结（达消息总上限 {channel_total}），不再接收发言"
                ));
            }
            // serial 提交校验：落后即弹回 + 增量（弹回内容进上下文，视为已读）。
            if ch.mode == ChannelMode::Serial {
                let seen = ch.seen.get(from).copied().unwrap_or(0);
                if seen < ch.seq {
                    let missed: Vec<ChannelMessage> =
                        ch.log.iter().filter(|m| m.seq > seen).cloned().collect();
                    ch.seen.insert(from.to_string(), ch.seq);
                    return Ok(PostOutcome::Stale { missed });
                }
            }
            let sent = ch.sent.get(from).copied().unwrap_or(0);
            if from != HUB_NAME && from != USER_NAME && sent >= limits.per_agent {
                return Err(format!(
                    "你在 #{name} 的发言已达上限 {}（预算闸）",
                    limits.per_agent
                ));
            }
            if ch.seq >= channel_total {
                ch.frozen = true;
                inner.hub_mail.push(format!(
                    "⚠ 频道 #{name} 已达消息总上限 {channel_total}，已冻结（后续发言将被拒绝）",
                ));
                return Err(format!(
                    "#{name} 达消息总上限 {channel_total}，频道已冻结"
                ));
            }
            ch.seq += 1;
            let msg = ChannelMessage {
                seq: ch.seq,
                from: from.to_string(),
                text: text.to_string(),
            };
            ch.log.push(msg.clone());
            ch.seen.insert(from.to_string(), ch.seq);
            *ch.sent.entry(from.to_string()).or_insert(0) += 1;
            let deliveries: Vec<(String, ChannelMessage)> = ch
                .members
                .iter()
                .filter(|m| {
                    m.as_str() != from && m.as_str() != HUB_NAME && m.as_str() != USER_NAME
                })
                .map(|m| (m.clone(), msg.clone()))
                .collect();
            hub_line = if from != HUB_NAME && ch.members.iter().any(|m| m == HUB_NAME) {
                Some(format_hub_line(name, &msg))
            } else {
                None
            };
            PostOutcome::Sent {
                seq: msg.seq,
                deliveries,
            }
        };
        if let Some(line) = hub_line {
            inner.hub_mail.push(line);
        }
        Ok(outcome)
    }

    /// 成员的信箱消化到 seq（其运行回合注入了该频道至 seq 的消息）。
    pub fn mark_seen(&self, member: &str, name: &str, seq: u64) {
        if let Some(ch) = self.lock().channels.get_mut(name) {
            let cursor = ch.seen.entry(member.to_string()).or_insert(0);
            if *cursor < seq {
                *cursor = seq;
            }
        }
    }

    /// 展示行快照：（watch_id, detail, 日志尾部文本）。
    pub fn row_snapshot(
        &self,
        name: &str,
    ) -> Option<(Option<crate::watch::WatchId>, String, String)> {
        const TAIL: usize = 50;
        let inner = self.lock();
        let ch = inner.channels.get(name)?;
        let detail = match ch.log.last() {
            Some(last) => format!(
                "{} 条 · 最近 {}: {}",
                ch.seq,
                last.from,
                crate::tool::agent::excerpt(&last.text)
            ),
            None => "0 条".to_string(),
        };
        let skipped = ch.log.len().saturating_sub(TAIL);
        let mut lines: Vec<String> = Vec::new();
        if skipped > 0 {
            lines.push(format!("…（前 {skipped} 条略）"));
        }
        lines.extend(
            ch.log
                .iter()
                .skip(skipped)
                .map(|m| format!("{}. {}: {}", m.seq, m.from, m.text)),
        );
        Some((ch.watch_id, detail, lines.join("\n")))
    }

    /// 单频道快照（TUI 房间头部）。
    pub fn info(&self, name: &str) -> Option<ChannelStatus> {
        let inner = self.lock();
        inner.channels.get(name).map(|ch| ChannelStatus {
            name: name.to_string(),
            members: ch.members.clone(),
            mode: ch.mode,
            seq: ch.seq,
            frozen: ch.frozen,
        })
    }

    /// 全量消息记录（TUI 房间渲染；克隆，调用方按帧取）。
    pub fn log_of(&self, name: &str) -> Vec<ChannelMessage> {
        self.lock()
            .channels
            .get(name)
            .map(|ch| ch.log.clone())
            .unwrap_or_default()
    }

    pub fn list(&self) -> Vec<ChannelStatus> {
        let inner = self.lock();
        let mut out: Vec<ChannelStatus> = inner
            .channels
            .iter()
            .map(|(name, ch)| ChannelStatus {
                name: name.clone(),
                members: ch.members.clone(),
                mode: ch.mode,
                seq: ch.seq,
                frozen: ch.frozen,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn has_hub_mail(&self) -> bool {
        !self.lock().hub_mail.is_empty()
    }

    /// 取走待注入主 agent 的频道消息（回合边界批量注入）。
    pub fn drain_hub_mail(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().hub_mail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<ChannelRegistry> {
        ChannelRegistry::new(ChannelLimits::default())
    }

    fn sent(outcome: PostOutcome) -> (u64, Vec<(String, ChannelMessage)>) {
        match outcome {
            PostOutcome::Sent { seq, deliveries } => (seq, deliveries),
            PostOutcome::Stale { .. } => panic!("应落地"),
        }
    }

    #[test]
    fn create_invite_kick_and_list() {
        let reg = registry();
        reg.create("table", vec!["a".into(), "b".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.create("table", vec![], ChannelMode::Free).is_err(), "重名");
        assert!(reg.create("", vec![], ChannelMode::Free).is_err(), "空名");
        let st = &reg.list()[0];
        assert_eq!(
            st.members,
            vec!["main", "user", "a", "b"],
            "hub 与 user 自动入席且排头"
        );
        reg.invite("table", "c").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.invite("table", "c").is_err(), "重复邀请");
        reg.kick("table", "b").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.kick("table", "b").is_err(), "不在场");
        assert!(reg.kick("table", "main").is_err(), "hub 不可移出");
        assert!(reg.kick("table", "user").is_err(), "user 不可移出");
        assert_eq!(reg.list()[0].members, vec!["main", "user", "a", "c"]);
        reg.remove_member_everywhere("a");
        assert_eq!(reg.list()[0].members, vec!["main", "user", "c"]);
        // 单频道快照与全量日志访问器。
        assert_eq!(reg.info("table").unwrap_or_else(|| panic!("有")).seq, 0);
        assert!(reg.info("nope").is_none());
        assert!(reg.log_of("table").is_empty());
    }

    #[test]
    fn post_fans_out_excluding_sender_and_hub() {
        let reg = registry();
        reg.create("t", vec!["a".into(), "b".into(), "c".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let (seq, deliveries) = sent(reg.post("a", "t", "大家好").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 1);
        let names: Vec<&str> = deliveries.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(names, vec!["b", "c"], "不投给发送者与 hub");
        assert!(deliveries.iter().all(|(_, m)| m.from == "a" && m.text == "大家好"));
        // hub 是成员：消息进 hub_mail；hub 自己发不进。
        assert!(reg.has_hub_mail());
        let mail = reg.drain_hub_mail();
        assert_eq!(mail, vec!["[#t 第1条] a: 大家好"]);
        let _ = sent(reg.post("main", "t", "肃静").unwrap_or_else(|e| panic!("{e}")));
        assert!(!reg.has_hub_mail(), "hub 自己的发言不回流");
        // user（人）是天然成员：可发言，hub 能听到，不占 per_agent 预算。
        let (_, deliveries) =
            sent(reg.post("user", "t", "都停一下").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(
            deliveries.iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"],
            "user 的发言唤醒全部 agent 成员"
        );
        assert!(reg.drain_hub_mail()[0].contains("user: 都停一下"));
        // 非成员/未知频道报错。
        assert!(reg.post("ghost", "t", "x").is_err());
        assert!(reg.post("a", "nope", "x").is_err());
    }

    #[test]
    fn serial_bounces_stale_sender_with_increments() {
        let reg = registry();
        reg.create("count", vec!["a".into(), "b".into()], ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "count", "1").unwrap_or_else(|e| panic!("{e}")));
        // b 没见过 a 的 "1"（seen=0 < seq=1）→ 弹回附增量。
        match reg.post("b", "count", "1").unwrap_or_else(|e| panic!("{e}")) {
            PostOutcome::Stale { missed } => {
                assert_eq!(missed.len(), 1);
                assert_eq!(missed[0].from, "a");
                assert_eq!(missed[0].text, "1");
            }
            PostOutcome::Sent { .. } => panic!("应弹回"),
        }
        // 弹回视为已读：重发落地（模型改口 "2"）。
        let (seq, _) = sent(reg.post("b", "count", "2").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 2, "重试成功，顺序涌现");
        // mark_seen：信箱注入后 a 的游标推进，不弹回。
        reg.mark_seen("a", "count", 2);
        let (seq, _) = sent(reg.post("a", "count", "3").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 3);
        // free 模式不校验。
        reg.create("brainstorm", vec!["a".into(), "b".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "brainstorm", "想法一").unwrap_or_else(|e| panic!("{e}")));
        let _ = sent(reg.post("b", "brainstorm", "想法二").unwrap_or_else(|e| panic!("{e}")));
    }

    #[test]
    fn late_joiner_starts_from_current_head() {
        let reg = registry();
        reg.create("t", vec!["a".into()], ChannelMode::Serial)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "旧闻").unwrap_or_else(|e| panic!("{e}")));
        reg.invite("t", "late").unwrap_or_else(|e| panic!("{e}"));
        // 迟入者 seen=入场时的头：无 backlog 弹回，直接可发言。
        let (seq, _) = sent(reg.post("late", "t", "我来了").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(seq, 2);
    }

    #[test]
    fn per_channel_message_limit_overrides_registry() {
        let reg = ChannelRegistry::new(ChannelLimits {
            channel_total: 100,
            per_agent: 100,
        });
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        // 频道级覆盖为 1：第二条就冻结。
        reg.set_message_limit("t", 1).unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "1").unwrap_or_else(|e| panic!("{e}")));
        let err = reg.post("a", "t", "2").unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        assert!(reg.list()[0].frozen);
        // 0 拒绝；未知频道报错。
        assert!(reg.set_message_limit("t", 0).is_err());
        assert!(reg.set_message_limit("nope", 5).is_err());
    }

    #[test]
    fn budgets_freeze_channel_and_notify_hub_once() {
        let reg = ChannelRegistry::new(ChannelLimits {
            channel_total: 2,
            per_agent: 2,
        });
        reg.create("t", vec!["a".into(), "b".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = sent(reg.post("a", "t", "1").unwrap_or_else(|e| panic!("{e}")));
        let _ = sent(reg.post("a", "t", "2").unwrap_or_else(|e| panic!("{e}")));
        // a 达 per_agent 上限。
        let err = reg.post("a", "t", "3").unwrap_err();
        assert!(err.contains("上限 2"), "{err}");
        // b 触发频道总上限：冻结 + hub 收到一次警示。
        let _ = reg.drain_hub_mail();
        let err = reg.post("b", "t", "x").unwrap_err();
        assert!(err.contains("冻结"), "{err}");
        assert!(reg.list()[0].frozen);
        let mail = reg.drain_hub_mail();
        assert_eq!(mail.len(), 1, "{mail:?}");
        assert!(mail[0].contains("已冻结"));
        // 冻结后再发：拒绝且不再重复通知。
        let err = reg.post("b", "t", "y").unwrap_err();
        assert!(err.contains("已冻结"), "{err}");
        assert!(!reg.has_hub_mail());
    }

    #[test]
    fn row_snapshot_summarizes_log() {
        let reg = registry();
        reg.create("t", vec!["a".into()], ChannelMode::Free)
            .unwrap_or_else(|e| panic!("{e}"));
        let (_, detail, payload) = reg.row_snapshot("t").unwrap_or_else(|| panic!("有频道"));
        assert_eq!(detail, "0 条");
        assert!(payload.is_empty());
        let _ = sent(reg.post("a", "t", "第一句").unwrap_or_else(|e| panic!("{e}")));
        let (_, detail, payload) = reg.row_snapshot("t").unwrap_or_else(|| panic!("有频道"));
        assert!(detail.contains("1 条") && detail.contains("a: 第一句"), "{detail}");
        assert_eq!(payload, "1. a: 第一句");
        assert!(reg.row_snapshot("nope").is_none());
    }
}
