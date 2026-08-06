# AGENTS.md — bingo

Rust 实现的 agent CLI（本地 agent harness）。

> 架构与选型决策见 [`notes/research.md`](./notes/research.md)（D1-D14，含分层对标 Claude Code 的实现顺序）；改动架构前先对表。

## 语言与风格

- 用 Rust 2024 edition；错误处理用 thiserror，避免 unwrap/expect（测试与不可达处除外）。
- 代码写成周围代码的样子；无注释优先，命名自明，注释只解释"为什么"。
- 不加不需要的依赖；造轮子前先看 crates.io 是否已有成熟轮子。
- Using english to write code and comments

## 架构规则

- 核心分层参照 agent harness 惯例：Tool 协议（Zod 等价物即 serde schema）、统一权限门、流式主循环、Hooks 扩展点。意图层病不要用药在代码层。
- 默认做减法：能删的代码、依赖、特性，删。加东西需要理由。
- 边界被独立消费时（公共 API、跨进程协议、持久化格式）先定契约（trait/serde schema），各实现共同对表；内部重构不立契约。

## 验证

- 每次改动跑 `cargo build` 与 `cargo clippy -- -D warnings`；相关逻辑必带测试（`cargo test`）。
- 未验证的不称完成；失败原样呈现。

## 提交

- Conventional Commits，祈使句，短。仅在真有信息时写正文与 issue 脚注。
- 只提交用户要求的变更；不提交 secrets。

## 禁止

1. 使用unsafe
2. unwrap 或 expect，必须处理每个异常
