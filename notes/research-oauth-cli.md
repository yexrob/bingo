# CLI OAuth 接入调研笔记：Codex CLI × opencode（供 bingo OAuth 方案参考）

> 调研日期 2026，源码均取自 GitHub 主分支（openai/codex `main`、sst/opencode `dev`）。硬事实以源码为准。

## 1. Codex CLI — ChatGPT OAuth

**登录方式**：`codex login` = authorization code + PKCE + **loopback**（本地起 HTTP server 等回调，默认端口 1455，占用自动换 fallback 端口）；`codex login --device-auth` = **自定义 device flow**（非 RFC 8628）。

**client_id**：`app_EMoamEEZ73f0CkXaXp7hrann`（可被 env `CODEX_APP_SERVER_LOGIN_CLIENT_ID` 覆盖）。

**端点**（issuer = `https://auth.openai.com`）：
- authorize：`GET {issuer}/oauth/authorize?response_type=code&client_id=…&redirect_uri=http://localhost:{port}/auth/callback&scope=openid profile email offline_access api.connectors.read api.connectors.invoke&code_challenge=…&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state=…&originator=…`
- 换 token：`POST {issuer}/oauth/token`（form 编码）`grant_type=authorization_code&code=…&redirect_uri=…&client_id=…&code_verifier=…` → `{id_token, access_token, refresh_token}`
- device flow（自研）：`POST {issuer}/api/accounts/deviceauth/usercode` body `{"client_id"}` → `{device_auth_id, user_code, interval}`；轮询 `POST …/deviceauth/token` `{"device_auth_id","user_code"}`（403/404 = pending，15 分钟超时）；成功后返回 **服务端生成的** `{authorization_code, code_challenge, code_verifier}`，再走同一 token endpoint 交换（redirect_uri = `{issuer}/deviceauth/callback`）；浏览器打开 `{issuer}/codex/device` 输 code。

**token 存储**：`$CODEX_HOME/auth.json`（默认 `~/.codex/auth.json`；可选 keyring）：
```json
{"auth_mode":"chatgpt",
 "tokens":{"id_token":"<JWT>","access_token":"<JWT>","refresh_token":"…","account_id":"…"},
 "last_refresh":"2026-…Z"}
```
id_token 是 JWT，内含 `https://api.openai.com/auth` claim（chatgpt_plan_type/user_id/account_id），启动时解析出计划类型、账号。

**refresh**：`POST https://auth.openai.com/oauth/token` JSON `{"client_id","grant_type":"refresh_token","refresh_token"}` → `{id_token?, access_token?, refresh_token?}`（可能轮换）；错误码 `refresh_token_expired / _reused / _invalidated` = 永久失败，提示重登。401 时走 UnauthorizedRecovery 状态机：先刷新重试 → 仍失败才提示重新登录。

**API 走法**：ChatGPT 订阅**不走 api.openai.com**，走 `https://chatgpt.com/backend-api/`（chatgpt_base_url 默认值）：`Authorization: Bearer <access_token>`（新版本升级为 agent-identity 签发的 task JWT）+ `ChatGPT-Account-ID` + `OAI-Product-Sku: codex` 头；requests 的 SSE 协议是私有的 codex responses 格式（response_item.done 等），非公开 Responses API。

来源：[device_code_auth.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/device_code_auth.rs) · [server.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/server.rs) · [auth/manager.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/manager.rs) · [auth/storage.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs) · [chatgpt/chatgpt_client.rs](https://github.com/openai/codex/blob/main/codex-rs/chatgpt/src/chatgpt_client.rs) · [core/src/config/mod.rs](https://github.com/openai/codex/blob/main/codex-rs/core/src/config/mod.rs)

## 2. opencode（sst/opencode）OAuth

**命令**：v2 重构后是 `opencode console login [url]`（原 `opencode auth login`），`console logout/switch/orgs/open`。opencode 订阅走**自建 auth server** `https://console.opencode.ai`（支持传自定义 server URL）。

**OAuth 流程（标准 RFC 8628 device flow）**，client_id = `opencode-cli`：
- `POST /auth/device/code` `{"client_id":"opencode-cli"}` → `{device_code, user_code, verification_uri_complete, expires_in, interval}`
- 浏览器打开 `verification_uri_complete`（已内嵌 user_code）
- 轮询 `POST /auth/device/token` `{"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code","client_id"}` → `{access_token, refresh_token, token_type:"Bearer", expires_in}` 或 `{error, error_description}`（authorization_pending / slow_down / expired_token / access_denied）
- 成功后 GET `/api/user`（id/email）、`/api/orgs`、`/api/config`（请求头 `x-org-id`）拿账号/组织配置。

**refresh**：`POST /auth/device/token` `{"grant_type":"refresh_token","refresh_token","client_id"}`；**过期前 5 分钟主动刷新**（eager refresh）。

**token 存储**：`$XDG_DATA_HOME/opencode/auth.json`（macOS = `~/Library/Application Support/opencode/auth.json`），0600，按 provider ID 为 key：
```json
{"opencode":{"type":"oauth","access":"…","refresh":"…","expires":1730000000000,"accountId":"…"},
 "anthropic":{"type":"api","key":"sk-…"}}
```
env `OPENCODE_AUTH_CONTENT` 可注入整份内容。

**provider 抽象**：provider 声明 `methods: [{type:"oauth"|"api",label,prompts}]`；authorize 返回 `{url, method:"auto"|"code", instructions}`（auto = 开浏览器 + 后台轮询；code = 手动粘贴 code 回传）；callback 写 auth store。多 provider 并存、多账号（account 表 + active 标记）切换粒度到 org。

来源：[packages/opencode/src/auth/index.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/auth/index.ts) · [packages/opencode/src/account/account.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/account/account.ts) · [packages/opencode/src/cli/cmd/account.ts](https://github.com/sst/opencode/blob/dev/packages/opencode/src/cli/cmd/account.ts) · [packages/core/src/plugin/provider/opencode.ts](https://github.com/sst/opencode/blob/dev/packages/core/src/plugin/provider/opencode.ts)

## 3. OpenAI Responses API（bingo 实现参考）

- `POST https://api.openai.com/v1/responses`，`Authorization: Bearer <key>`（可加 `OpenAI-Organization` / `OpenAI-Project`）。
- 请求体要点：`{model, instructions?, input:[{role,content}], tools:[{type:"function",name,description,parameters,strict?}], tool_choice?, stream:true, reasoning:{effort:"low"|"medium"|"high"}, text:{format:{type:"text"|"json_schema",…}}, store?}`。
- SSE 事件序列：
  - 总生命周期：`response.created` → `response.in_progress` → … → `response.completed`（仅此处带 usage；失败为 `response.failed`）
  - 工具调用（对应 Anthropic tool_use）：`response.output_item.added`（item.type=`function_call`，带 id）→ `response.function_call_arguments.delta`（**arguments 为 JSON 字符串增量，须拼接直到 `.done`**）→ `response.output_item.done`
  - 文本：`response.output_item.added`（output_text）→ `response.output_text.delta` → `response.output_text.done` → `response.output_item.done`
  - 每个内容流都是 add/delta/done 三段式，最终文本在 done 事件里。
- 模型名：`gpt-5` / `gpt-5-mini` / `o3` / `o4-mini` / `gpt-5-codex` 等。

来源：[Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events) · [Responses API reference](https://developers.openai.com/api/reference/resources/responses) · [社区 streaming 指南](https://community.openai.com/t/responses-api-streaming-the-simple-guide-to-events/1363122)

## 4. CLI 层 UX 对比（登录/状态/切换/logout）

| 维度 | Codex CLI | opencode |
|---|---|---|
| 登录 | `codex login` 自动开浏览器 + loopback 回调；`--device-auth` 打印「1. 打开链接 2. 输入一次性 code（15 分钟）」；`--api-key` | `opencode console login [url]`：打印 `Go to: <url>` + `Enter code:` → 自动 open 浏览器 → spinner「Waiting for authorization…」→ `Logged in as <email>` |
| 状态 | 启动时 AuthManager 读 auth.json 并解析 id_token（计划类型/账号），限制登录方式与 workspace | `opencode console orgs` 列账号+org，active 用 ● 标记 |
| 多账号/切换 | 单身份为主；`forced_chatgpt_workspace_id` 白名单限制 | `console switch` 跨账号+org 的 picker 选择；`logout [email]` 按账号登出；DB 中 active 标记 |
| logout | `codex logout`：revoke token（`POST {issuer}/oauth/revoke`）+ 删 auth.json | `console logout [email]`：选择账号删除本地记录（无 revoke 调用） |
| fallback | headless/远端用 `--device-auth`；loopback 端口冲突自动换端口 | device flow 天然 headless 友好；`open()` 失败静默，提示用户手动打开 URL |

**对 bingo 的启示**：① 两种可复用模板——Codex 的「auth.openai.com + 自定义 deviceauth」与 opencode 的「自建 server + RFC 8628 标准 device flow + refresh 轮换」；② 存储统一为 `{provider_id: {type, access, refresh, expires}}` 按 provider 分 key 的单 JSON（0600）最简洁；③ 登录 UX 抄 opencode：打印 URL + code → 自动开浏览器 → spinner 轮询 → 成功回显 email；④ 401 恢复 = 先静默 refresh（含过期前主动刷新）再要求重登。

**Sources**:
- [openai/codex — login 源码](https://github.com/openai/codex/tree/main/codex-rs/login)
- [sst/opencode — auth/account 源码](https://github.com/sst/opencode/tree/dev/packages/opencode/src)
- [Responses streaming events | OpenAI API Reference](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [Responses | OpenAI API Reference](https://developers.openai.com/api/reference/resources/responses)
- [Responses API streaming - the simple guide to "events"](https://community.openai.com/t/responses-api-streaming-the-simple-guide-to-events/1363122)
- [Streaming — OpenAI Agents SDK](https://openai.github.io/openai-agents-python/streaming/)
