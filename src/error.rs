//! 错误码契约（单一来源，D-feedback）。
//!
//! 规范见 `notes/design/feedback-states.md` §4.3（C 出口映射）：
//! - 每个模块错误 enum 实现 [`ErrorCode`]：match **穷尽所有 variant、无 `_` 臂**，
//!   暂未分配稳定码的 variant **显式返回 [`GENERIC`]**（显式行为，非隐式兜底）。
//! - 出口（CLI 日志 / TUI 渲染）共用 [`map_error`]，禁止各自实现映射。
//! - 码值 **semver**：一经发布只增不改不重用；新增码 = 新增 variant 映射 +
//!   在防漂移单测补断言，缺一环 CI 红。

/// 已发布稳定码：此路径暂未分配稳定码（错误语义降级为通用）。
pub const GENERIC: &str = "GENERIC";

/// 稳定错误码：`SCREAMING_SNAKE`（如 `CONFIG_INVALID`）。
pub trait ErrorCode {
    fn error_code(&self) -> &'static str;
}

/// 呈现级别（文档 §3 三级错误态 + §4.4 TIMEOUT 双呈现注）。
/// TUI 渲染按级别分支（页面级/字段级 = 错误行高亮，全流程级 = 整屏态）。
/// **级别由触发上下文决定，不单由 code 推断**（如 `TIMEOUT` 短同步=页面级、
/// 长回合=全流程级，见 §4.4 注与 AC 表 v1.9.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorLevel {
    /// 字段级：仅标错误对象（如 `CONFIG_INVALID` 配置校验）。
    // 短操作错误（页面级/字段级）接入 `UiEvent::Error` 时启用——当前生产
    // 路径仅回合级错误（`Full`），Field/Page 由测试 fixture 注入覆盖。
    #[allow(dead_code)]
    Field,
    /// 页面级：错误行高亮 + 重试可达（短同步读/写超时等）。
    #[allow(dead_code)]
    Page,
    /// 全流程级：整屏错误态 + 返回路径（长回合失败、认证/权限等）。
    Full,
}

/// 错误触发上下文（#14 契约第三维，qa #69 / main #71 增量 2）：**呈现级别由
/// 它决定，不单由 `code` 推断**——`TIMEOUT` 短同步=页面级、长回合=全流程级。
/// 生产者发射 `UiEvent::Error` 时已知并显式携带（devex #81「级别不全是码的
/// 固有属性」，渲染层不推导、测试侧不复制映射表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorContext {
    /// 短同步操作（list_models/count_tokens/complete_text）→ 页面级。
    // 短操作错误接入 `UiEvent::Error` 时启用——当前生产路径仅回合级
    // （LongTurn），ShortSync 由测试 fixture 注入覆盖。
    #[allow(dead_code)]
    ShortSync,
    /// agent 长回合（流式 + 多轮工具，传输层超时/中断）→ 全流程级。
    LongTurn,
}

/// 显式返回 `GENERIC` 时的 debug 告警（提醒补登记）。
/// release 下静默——`GENERIC` 语义已知（暂未分配稳定码），不是意外丢失。
/// 调用方：显式 `GENERIC` 返回路径 + boxed 出口（宏登记表漏登记自动落
/// `GENERIC`）分支（debug 构建）；release 下函数整体 cfg 掉，无调用属预期。
#[cfg(debug_assertions)]
pub fn missing_code<T: std::fmt::Debug + ?Sized>(err: &T) {
    eprintln!(
        "[bingo] error: {err:?} 使用 GENERIC（missing stable error code）"
    );
}

/// 单出口映射：GUI（TUI）与 CLI 出口都必须经由本函数取码，禁止各自实现。
pub fn map_error<E: ErrorCode + ?Sized>(err: &E) -> &'static str {
    err.error_code()
}

/// 非 TTY 错误契约的 msg 转义（AC-31/32）：换行/制表符/回车归一化为空格，
/// 主 msg 截断 200 字符——单行稳定，防破坏 `key=value` 解析。
/// 多行堆栈另走 `detail=`（`--verbose`），本函数只负责主 `msg` 字段。
pub fn sanitize_msg(msg: &str) -> String {
    let normalized: String = msg
        .chars()
        .map(|c| if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c })
        .collect();
    normalized.chars().take(200).collect()
}

/// 显式 `GENERIC` 的 allowlist（防漂移单测的豁免表）。
/// 条目用可定位路径（如 `"tool::bash::Error::NonZeroExit"`），
/// 每条必须带 `TODO(generic-allow): <issue>/<日期> <理由>` 注释。
/// 仅防漂移单测读取（发布面 = 契约文件本身），非 test 构建无引用属预期。
#[cfg_attr(not(test), allow(dead_code))]
pub const GENERIC_ALLOWLIST: &[&str] = &[];

/// 装箱错误（`Box<dyn Error>` 顶层）取稳定码：沿 cause 链找最近一个实现
/// [`ErrorCode`] 的错误。未登记类型落 `GENERIC`（显式语义，见 [`GENERIC`]）。
///
/// 注意：这里 downcast 只是「从 `dyn Error` 找回具体类型以调用其
/// `error_code()`」的通道，映射逻辑全部在各类型自己的 `ErrorCode` 实现里，
/// 不在出口判断——禁止在出口按类型名做映射 match。
pub fn error_code_boxed(err: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(code) = downcast_error_code(e) {
            return code;
        }
        cur = e.source();
    }
    // 宏登记表漏登记（或类型未实现 ErrorCode）落入 GENERIC：debug 下告警
    // （v1.14 要求），release 下语义已知（暂未分配稳定码）。
    #[cfg(debug_assertions)]
    missing_code(err);
    GENERIC
}

/// 把 `&dyn Error` downcast 到实现 `ErrorCode` 的具体类型并取码。
/// 宏列出全部已知类型：新增实现 `ErrorCode` 的错误类型时在此登记。
macro_rules! downcast_error_code {
    ($err:expr, $($t:ty),+ $(,)?) => {{
        let mut found: Option<&'static str> = None;
        $(
            if found.is_none()
                && let Some(e) = $err.downcast_ref::<$t>()
            {
                found = Some(<$t as $crate::error::ErrorCode>::error_code(e));
            }
        )+
        found
    }};
}

fn downcast_error_code(err: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    downcast_error_code!(
        err,
        crate::api::client::ClientError,
        crate::query::QueryError,
        crate::tool::ToolError,
        crate::settings::SettingsError,
        crate::team::TeamError,
        crate::tasks::TaskError,
        crate::transcript::TranscriptError,
        crate::experience::ExperienceError,
        crate::hooks::HookError,
        crate::mcp::McpError,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 稳定码必须 SCREAMING_SNAKE（AC-35）。
    fn assert_screaming_snake(code: &str) {
        assert!(
            !code.is_empty()
                && code.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "错误码 {code:?} 必须匹配 ^[A-Z][A-Z0-9_]*$"
        );
    }

    /// 显式 GENERIC 的 variant 必须是已发布稳定码（GENERIC_ALLOWLIST 登记）。
    fn is_allowed_generic(path: &str) -> bool {
        GENERIC_ALLOWLIST.contains(&path)
    }

    /// 枚举每模块每 variant 断言映射到非 GENERIC 稳定码（AC-40/41/43）：
    /// 未显式登记 GENERIC_ALLOWLIST 的 variant 一律不允许落 GENERIC。
    fn assert_stable_codes<'a, T: ErrorCode + std::fmt::Debug + 'a>(
        path: &str,
        variants: impl IntoIterator<Item = &'a T>,
    ) {
        for variant in variants {
            let code = variant.error_code();
            assert_screaming_snake(code);
            assert!(
                is_allowed_generic(path) || code != GENERIC,
                "{path}: {variant:?} 落 GENERIC 未登记 allowlist"
            );
        }
    }

    #[test]
    fn client_error_codes() {
        use crate::api::client::ClientError;
        let variants = vec![
            ClientError::MissingApiKey,
            ClientError::InvalidApiKey("k".into()),
            ClientError::Api { status: 401, body: String::new() },
            ClientError::Api { status: 403, body: String::new() },
            ClientError::Api { status: 429, body: String::new() },
            ClientError::Api { status: 500, body: String::new() },
            ClientError::Stream("s".into()),
            ClientError::Timeout,
        ];
        assert_stable_codes("api::client::ClientError", &variants);
        assert_eq!(ClientError::Timeout.error_code(), "TIMEOUT");
        assert_eq!(ClientError::MissingApiKey.error_code(), "AUTH_REQUIRED");
        let denied = ClientError::Api { status: 403, body: String::new() };
        assert_eq!(denied.error_code(), "PERMISSION_DENIED");
        let server = ClientError::Api { status: 500, body: String::new() };
        assert_eq!(server.error_code(), "SERVER_ERROR");
        // `ClientError::Transport` 变体不可运行时构造（reqwest::Error 无公开
        // 构造 API，0.13.x 全 pub(crate)）：映射由 `transport_offline_code`
        // 锁定并在此断言（与防漂移单测其余变体同源）。
        assert_eq!(
            crate::api::client::transport_offline_code(),
            "OFFLINE"
        );
    }

    #[test]
    fn query_error_forwards_client_and_tool() {
        use crate::api::client::ClientError;
        use crate::query::QueryError;
        assert_eq!(QueryError::Protocol("p".into()).error_code(), "SERVER_ERROR");
        assert_eq!(
            QueryError::Client(ClientError::Timeout).error_code(),
            "TIMEOUT"
        );
        assert_eq!(
            QueryError::Tool(crate::tool::ToolError::failed("x")).error_code(),
            "TOOL_FAILED"
        );
    }

    #[test]
    fn config_and_storage_errors() {
        use crate::settings::SettingsError;
        assert_eq!(SettingsError::Io(std::io::Error::other("x")).error_code(), "CONFIG_INVALID");
        assert_eq!(SettingsError::Parse(serde_json::from_str::<()>("x").unwrap_err()).error_code(), "CONFIG_INVALID");
        // TeamError 全部 3 个 variant 显式枚举（护栏 5：逐 variant 断言）。
        use crate::team::TeamError;
        let team_variants = vec![
            TeamError::Invalid("x".into()),
            TeamError::Io(std::io::Error::other("x")),
            TeamError::Parse(serde_json::from_str::<()>("x").unwrap_err()),
        ];
        assert_stable_codes("team::TeamError", &team_variants);
        for v in &team_variants {
            assert_eq!(v.error_code(), "CONFIG_INVALID", "TeamError 全 variant 落配置错误");
        }
        // TaskError 全部 5 个 variant 显式枚举（AC-40）。
        use crate::tasks::TaskError;
        let task_variants = vec![
            TaskError::Io(std::io::Error::other("x")),
            TaskError::InvalidId("x".into()),
            TaskError::Serialize(serde_json::from_str::<()>("x").unwrap_err()),
            TaskError::CreateConflict("1".into()),
            TaskError::Parse { path: "p".into(), detail: "d".into() },
        ];
        assert_stable_codes("tasks::TaskError", &task_variants);
        for v in &task_variants {
            assert_eq!(v.error_code(), "STORAGE_ERROR", "TaskError 全 variant 落存储错误");
        }
        use crate::transcript::TranscriptError;
        assert_eq!(TranscriptError::Io(std::io::Error::other("x")).error_code(), "STORAGE_ERROR");
        assert_eq!(TranscriptError::Parse(serde_json::from_str::<()>("x").unwrap_err()).error_code(), "STORAGE_ERROR");
        use crate::experience::ExperienceError;
        assert_eq!(ExperienceError::Io(std::io::Error::other("x")).error_code(), "STORAGE_ERROR");
    }

    #[test]
    fn hook_and_mcp_and_tool() {
        use crate::hooks::HookError;
        assert_eq!(HookError::Failed("x".into()).error_code(), "HOOK_FAILED");
        use crate::mcp::McpError;
        assert_eq!(McpError::Connect { server: "s".into(), detail: "d".into() }.error_code(), "SERVER_ERROR");
        use crate::tool::ToolError;
        assert_eq!(ToolError::failed("x").error_code(), "TOOL_FAILED");
    }

    #[test]
    fn boxed_error_walks_cause_chain() {
        use crate::api::client::ClientError;
        use crate::query::QueryError;
        let q = QueryError::Client(ClientError::Timeout);
        assert_eq!(error_code_boxed(&q), "TIMEOUT");
        // 未登记类型落 GENERIC。
        let unknown: Box<dyn std::error::Error> = std::io::Error::other("x").into();
        assert_eq!(error_code_boxed(&*unknown), GENERIC);
    }

    /// 宏登记表覆盖所有 ErrorCode 实现类型（护栏 4「登记即契约第二处」）：
    /// 10 个登记类型逐一经 boxed 出口断言非 GENERIC——新增实现 ErrorCode 的
    /// 类型若只在 TUI 出口生效而漏登记 downcast 宏，CLI 出口静默落 GENERIC、
    /// 本测试红。与 `downcast_error_code` 宏清单为对照（双处登记，缺一 CI 红）。
    #[test]
    fn boxed_export_covers_all_registered_modules() {
        use crate::api::client::ClientError;
        use crate::experience::ExperienceError;
        use crate::hooks::HookError;
        use crate::mcp::McpError;
        use crate::query::QueryError;
        use crate::settings::SettingsError;
        use crate::tasks::TaskError;
        use crate::team::TeamError;
        use crate::tool::ToolError;
        use crate::transcript::TranscriptError;
        let samples: Vec<Box<dyn std::error::Error>> = vec![
            Box::new(ClientError::MissingApiKey),
            Box::new(QueryError::Protocol("p".into())),
            Box::new(ToolError::failed("x")),
            Box::new(SettingsError::Io(std::io::Error::other("x"))),
            Box::new(TeamError::Invalid("x".into())),
            Box::new(TaskError::InvalidId("x".into())),
            Box::new(TranscriptError::Io(std::io::Error::other("x"))),
            Box::new(ExperienceError::Io(std::io::Error::other("x"))),
            Box::new(HookError::Failed("x".into())),
            Box::new(McpError::Connect { server: "s".into(), detail: "d".into() }),
        ];
        assert_eq!(
            samples.len(),
            10,
            "boxed 出口应有 10 个登记类型：新增实现 ErrorCode 的类型须在 \
             `downcast_error_code` 宏登记 + 本测试补实例，双处缺一即 CI 红"
        );
        for e in &samples {
            let code = error_code_boxed(e.as_ref());
            assert!(
                code != GENERIC,
                "boxed 出口落 GENERIC：{e:?} 未在 downcast 宏登记表登记"
            );
            assert_screaming_snake(code);
        }
    }

    #[test]
    fn sanitize_msg_normalizes_and_truncates() {
        // AC-31：换行/制表符/回车归一化为空格，单行不被破坏。
        assert_eq!(
            crate::error::sanitize_msg("line1\nline2\tline3\rline4"),
            "line1 line2 line3 line4"
        );
        // AC-32：截断 200 字符（字符数，非字节数；中文逐字符计）。
        let long = "长".repeat(300);
        assert_eq!(crate::error::sanitize_msg(&long).chars().count(), 200);
        let ascii = "x".repeat(250);
        assert_eq!(crate::error::sanitize_msg(&ascii).len(), 200);
    }
}
