# 主流大模型 API 参数调研（context window / max output / thinking / vision）

调研日期：2026-08-13（vision 列补充于 2026-08-21，同样只认官方文档：各家族模型页的输入模态声明）。方法：仅用各家官方 API 文档（WebSearch 定位 + WebFetch 抓取当日页面），逐模型核实 context window、max output tokens、reasoning/thinking 参数及思考 token 是否计入 max output；查不到官方数字的格子标「未查到官方值」，不用第三方转述补。vision 一列 = 该模型是否接受图像输入（输入模态含 Image）。本文数据将喂给「按 id 前缀匹配」的代码表，家族内例外在各表后显式标注，文末给出归并后的最小前缀表。

---

## Anthropic Claude

思考计入口径（全系统一）：**计入**。官方 extended thinking 文档原文："Thinking tokens count toward the `max_tokens` limit for the turn"，且 "`max_tokens` remains the hard ceiling on total output"（出处：https://platform.claude.com/docs/en/build-with-claude/extended-thinking ）。`max_tokens` 为必填参数、无服务端默认值，下表 max output 均为上限。**Vision：全系支持图像输入**——models overview 原文 "All current Claude models support text and image input, text output, multilingual capabilities, and vision"（https://platform.claude.com/docs/en/about-claude/models/overview ）。

| API 模型 id | 前缀家族 | context window | max output（默认/最大） | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| claude-fable-5 | claude- | 1M | 无默认 / 128K | adaptive（**始终开启**，`thinking` 参数只能省略或 `{type:"adaptive"}`） | 计入 | https://platform.claude.com/docs/en/about-claude/models/overview |
| claude-opus-5 | claude- | 1M | 无默认 / 128K | adaptive（省略参数即默认开启；`effort` 为 `xhigh`/`max` 时禁用 thinking 返回 400） | 计入 | 同上 |
| claude-sonnet-5 | claude- | 1M | 无默认 / 128K | adaptive（省略参数即默认开启） | 计入 | 同上 |
| claude-haiku-4-5 | claude-haiku-4-5 | 200K | 无默认 / 64K | extended thinking（`thinking: {type:"enabled", budget_tokens}`，不支持 adaptive） | 计入 | 同上 |
| claude-opus-4-8 | claude- | 1M | 无默认 / 128K | adaptive（需显式设置，省略则不思考） | 计入 | 同上 |
| claude-opus-4-7 | claude- | 1M | 无默认 / 128K | adaptive（需显式设置） | 计入 | 同上 |
| claude-opus-4-6 | claude- | 1M | 无默认 / 128K | adaptive（推荐）+ extended（deprecated） | 计入 | 同上 |
| claude-sonnet-4-6 | claude- | 1M | 无默认 / 128K | adaptive（推荐）+ extended（deprecated） | 计入 | 同上 |
| claude-sonnet-4-5 | claude-sonnet-4-5 | 200K | 无默认 / 64K | extended thinking（budget_tokens） | 计入 | 同上 |
| claude-opus-4-5 | claude-opus-4-5 | 200K | 无默认 / 64K | extended thinking（budget_tokens） | 计入 | 同上 |

**前缀家族内例外（喂代码表时必须单列更长前缀）：**
- `claude-haiku-4-5`、`claude-sonnet-4-5`、`claude-opus-4-5`：窗口 200K、输出 64K，低于 `claude-` 家族通值（1M/128K）。
- thinking 参数形态按代际分裂：4.5 及更早只有 `enabled + budget_tokens`；4.6 两者皆可（extended 已 deprecated）；4.7/4.8/5 只接受 `adaptive`（`budget_tokens` 返回 400）；fable-5 始终思考（显式 `disabled` 返回 400）。
- `claude-mythos-5`：与 fable-5 同规格（1M/128K/始终思考），仅 Project Glasswing 邀请制可用，一般代码表可不收。
- Batch API 例外：Opus 5/4.8/4.7/4.6、Sonnet 5/4.6 在 Message Batches API 上可用 `output-300k-2026-03-24` beta 头扩到 300K 输出（同步 Messages API 仍 128K）。

---

## OpenAI

思考计入口径（全系统一）：**计入**。官方 reasoning 指南："reasoning tokens still occupy space in the model's context window and are billed as output tokens"，`max_output_tokens` 限制 reasoning + 可见输出的总和（出处：https://developers.openai.com/api/docs/guides/reasoning ；platform.openai.com 现 301 到 developers.openai.com）。参数为 `reasoning.effort`（档位全集 none/minimal/low/medium/high/xhigh/max，各型号支持子集）。官方不区分默认/最大，只给单一 max output 上限。**Vision：全系支持图像输入**——gpt-5.6-sol 型号页 "Input modalities: text, image" 且 supported features 含 `image_input`（https://developers.openai.com/api/docs/models/gpt-5.6-sol ），5.x 全系同源；o 系列亦接受图像。

| API 模型 id | 前缀家族 | context window | max output | reasoning 支持（effort 档位） | 计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| gpt-5.6-sol（别名 gpt-5.6） | gpt-5.6 | 1,050,000 | 128,000 | none/low/medium(默认)/high/xhigh/max | 计入 | https://developers.openai.com/api/docs/models/gpt-5.6-sol |
| gpt-5.6-terra | gpt-5.6 | 1,050,000 | 128,000 | 同上 | 计入 | https://developers.openai.com/api/docs/models/gpt-5.6-terra |
| gpt-5.6-luna | gpt-5.6 | 1,050,000 | 128,000 | 同上 | 计入 | https://developers.openai.com/api/docs/models/gpt-5.6-luna |
| gpt-5.6-cyber（Daybreak 门控） | gpt-5.6-cyber | 400,000 | 128,000 | 支持，档位未查到官方值 | 计入 | https://developers.openai.com/api/docs/models/gpt-5.6-cyber |
| gpt-5.5 | gpt-5.5 | 1,050,000 | 128,000 | none/low/medium(默认)/high/xhigh | 计入 | https://developers.openai.com/api/docs/models/gpt-5.5 |
| gpt-5.5-pro | gpt-5.5 | 1,050,000 | 128,000 | medium/high(默认)/xhigh，仅 Responses API | 计入 | https://developers.openai.com/api/docs/models/gpt-5.5-pro |
| gpt-5.5-cyber | gpt-5.5-cyber | 未查到官方值 | 未查到官方值 | 未查到官方值（pricing 页在售，型号页缺失） | 计入（按全系规则） | https://developers.openai.com/api/docs/pricing.md |
| gpt-5.4 | gpt-5.4 | 1,050,000 | 128,000 | none(默认)/low/medium/high/xhigh | 计入 | https://developers.openai.com/api/docs/models/gpt-5.4 |
| gpt-5.4-mini | gpt-5.4-mini | 400,000 | 128,000 | none(默认)/low/medium/high/xhigh | 计入 | https://developers.openai.com/api/docs/models/gpt-5.4-mini |
| gpt-5.4-nano | gpt-5.4-nano | 400,000 | 128,000 | none(默认)/low/medium/high/xhigh | 计入 | https://developers.openai.com/api/docs/models/gpt-5.4-nano |
| gpt-5.4-pro | gpt-5.4 | 1,050,000 | 128,000 | medium(默认)/high/xhigh，仅 Responses | 计入 | https://developers.openai.com/api/docs/models/gpt-5.4-pro |
| gpt-5.2 | gpt-5.2 | 400,000 | 128,000 | none(默认)/low/medium/high/xhigh | 计入 | https://developers.openai.com/api/docs/models/gpt-5.2 |
| gpt-5.2-pro | gpt-5.2 | 400,000 | 128,000 | medium/high/xhigh，仅 Responses | 计入 | https://developers.openai.com/api/docs/models/gpt-5.2-pro |
| gpt-5.1 | gpt-5.1 | 400,000 | 128,000 | none(默认)/low/medium/high（无 xhigh） | 计入 | https://developers.openai.com/api/docs/models/gpt-5.1 |
| gpt-5 | gpt-5 | 400,000 | 128,000 | minimal/low/medium/high（minimal 而非 none）；已弃用，2026-12-11 下线 | 计入 | https://developers.openai.com/api/docs/models/gpt-5 |
| gpt-5-mini | gpt-5 | 400,000 | 128,000 | 支持，档位未查到官方值；2026-12-11 下线 | 计入 | https://developers.openai.com/api/docs/models/gpt-5-mini |
| gpt-5-nano | gpt-5 | 400,000 | 128,000 | 支持，档位未查到官方值；2026-12-11 下线 | 计入 | https://developers.openai.com/api/docs/models/gpt-5-nano |
| gpt-5-pro | gpt-5-pro | 400,000 | 272,000 | 仅 high，仅 Responses+Batch；2026-12-11 下线 | 计入 | https://developers.openai.com/api/docs/models/gpt-5-pro |
| o3 / o3-pro / o3-mini / o4-mini / o1 / o1-pro | o | 200,000 | 100,000 | 支持，档位未查到官方值；o3/o3-pro 2026-12-11、o3-mini/o4-mini 2026-10-23 下线 | 计入 | https://developers.openai.com/api/docs/models/o3 等各型号页 |

**前缀家族内例外：**
- `gpt-5.6-cyber`：窗口 400K，非家族通值 1.05M。
- `gpt-5.4-mini` / `gpt-5.4-nano`：窗口 400K，非 5.4 家族通值 1.05M。
- `gpt-5-pro`：max output 272,000，全 OpenAI 唯一非 128K。
- `gpt-5.1`：effort 无 xhigh；裸 `gpt-5`：最低档叫 minimal 不叫 none。
- 前缀匹配陷阱：`gpt-5` 是 `gpt-5.x` 全部 id 的前缀，代码表必须长前缀优先。
- 已下线勿收：gpt-5-chat-latest、gpt-5.x-codex 系、gpt-5.x-chat-latest 系、deep-research 系（https://developers.openai.com/api/docs/deprecations ）。

---

## DeepSeek

旧型号 `deepseek-chat` / `deepseek-reasoner` 已于 2026-07-24 起全面停用（https://api-docs.deepseek.com/news/news260424/ ），当前仅两个型号。thinking 参数：`thinking: {"type":"enabled"|"disabled"}`（默认 enabled）+ `reasoning_effort: low/high(默认)/max`（medium/xhigh 映射为 high；Anthropic 兼容格式为 `reasoning.effort: none/low/high/max`）。CoT 经独立字段 `reasoning_content` 返回；thinking 模式不支持 temperature/top_p/presence_penalty/frequency_penalty；带 tools 的多轮请求必须回传 `reasoning_content` 否则 400。**Vision：不支持图像输入**——Models & Pricing 页 FEATURES 表仅列 JSON Output / Tool Calls / Responses API / Anthropic API / FIM，无图像输入（https://api-docs.deepseek.com/quick_start/pricing/ ）。

| API 模型 id | 前缀家族 | context window | max output（默认/最大） | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| deepseek-v4-flash | deepseek | 1M | 未查到官方值 / 384K | 支持（thinking.type + reasoning_effort） | 未查到官方明确说明（`max_tokens` 定义未提及 reasoning_content 是否计入） | https://api-docs.deepseek.com/quick_start/pricing/ ；https://api-docs.deepseek.com/guides/thinking_mode/ |
| deepseek-v4-pro | deepseek | 1M | 未查到官方值 / 384K | 同上（两型号参数完全一致） | 同上 | 同上 + https://api-docs.deepseek.com/api/create-chat-completion/ |

例外：无（两型号规格一致）。

---

## Google Gemini

全系 input 1,048,576 / output 65,536，官方只给单一 "Output token limit"，不区分默认/最大。thinking 参数为 `generation_config.thinking_level: minimal/low/medium/high`（3.x 系列文档已不见旧的数值型 `thinkingBudget` 写法）。思考计入口径：**未查到官方明确说明**——官方仅言计费 "response pricing is the sum of output tokens and thinking tokens"（https://ai.google.dev/gemini-api/docs/thinking ），API 参考中 `maxOutputTokens` 与 `thoughtsTokenCount` 为独立字段（https://ai.google.dev/api/generate-content ），是否计入上限官方未言明。**Vision：全系支持**——gemini-3.6-flash 型号页 "Inputs: Text, Image, Video, Audio, and PDF"（https://ai.google.dev/gemini-api/docs/models/gemini-3.6-flash ）。

| API 模型 id | 前缀家族 | context window | max output | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| gemini-3.6-flash | gemini | 1,048,576 | 65,536 | 支持（thinking_level） | 未查到官方明确说明 | https://ai.google.dev/gemini-api/docs/models/gemini-3.6-flash |
| gemini-3.5-flash | gemini | 1,048,576 | 65,536 | 支持 | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-3.5-flash |
| gemini-3.5-flash-lite | gemini | 1,048,576 | 65,536 | 支持 | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-3.5-flash-lite |
| gemini-3.1-flash-lite | gemini | 1,048,576 | 65,536 | 支持（默认 minimal） | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite |
| gemini-3.1-pro-preview | gemini | 1,048,576 | 65,536 | 支持（默认 high 动态） | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-3.1-pro-preview |
| gemini-3-flash-preview | gemini | 1,048,576 | 65,536 | 支持（默认 high 动态） | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-3-flash-preview |
| gemini-2.5-pro | gemini | 1,048,576 | 65,536 | 支持 | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-2.5-pro |
| gemini-2.5-flash | gemini | 1,048,576 | 65,536 | 支持 | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-2.5-flash |
| gemini-2.5-flash-lite | gemini | 1,048,576 | 65,536 | 支持（默认关闭） | 同上 | https://ai.google.dev/gemini-api/docs/models/gemini-2.5-flash-lite |

例外：无（全系数值一致，罕见地干净）。

---

## Qwen（阿里云百炼 / DashScope）

thinking 参数：`enable_thinking` + `thinking_budget`（OpenAI 兼容接口经 `extra_body` 传入；thinking_budget 默认=该模型最大思维链长度）。思考计入口径有官方明文：**不计入**——"max_tokens = 模型回答的最大长度（不包含思维链内容）"，思维链单独受 `thinking_budget` 约束，思考内容按输出 token 计费（https://help.aliyun.com/zh/model-studio/deep-thinking ）。**Vision 按代际分裂**：qwen3.x 主线（如 qwen3.8-max）输入模态为 Image/Text/Video（https://help.aliyun.com/zh/model-studio/qwen3-8-max ），支持图像；旧别名 qwen-plus（https://help.aliyun.com/zh/model-studio/qwen-plus ）输入模态仅 Text，qwen-max 同理——代码表按 `qwen3.`（vision=true）与 `qwen-`（vision=false）分列。

| API 模型 id | 前缀家族 | context window | max output（默认=最大） | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| qwen3.8-max | qwen3. | 1,000,000 | 131,072 | 支持（最大思维链 262,144） | 不计入 | https://help.aliyun.com/zh/model-studio/qwen3-8-max |
| qwen3.7-plus | qwen3. | 1,000,000 | 131,072 | 支持（最大思维链 262,144） | 不计入 | https://help.aliyun.com/zh/model-studio/qwen3-7-plus |
| qwen3.7-flash | qwen3. | 1,000,000 | 131,072 | 支持（最大思维链 262,144） | 不计入 | https://help.aliyun.com/zh/model-studio/qwen3-7-flash |
| qwen-plus（旧别名，仍在售） | qwen- | 1,000,000 | 32,768 | 支持（最大思维链 81,920） | 不计入 | https://help.aliyun.com/zh/model-studio/qwen-plus |
| qwen-flash（旧别名，仍在售） | qwen- | 1,000,000 | 32,768 | 支持 | 不计入 | https://help.aliyun.com/zh/model-studio/qwen-flash |
| qwen-max（旧别名，滞后快照 2025-01-25） | qwen-max | 32,768 | 8,192 | 不支持 | — | https://help.aliyun.com/zh/model-studio/qwen-max |

**前缀家族内例外：**
- `qwen-max`：窗口 32,768 / 输出 8,192 / 无 thinking，与其他 qwen- 型号差一个数量级，必须单列。
- `qwen3.x` 与旧别名 `qwen-plus/flash` 输出上限不同（131,072 vs 32,768）。

---

## Moonshot Kimi

注意：`max_tokens` 已弃用，改用 `max_completion_tokens`。思考计入口径有官方明文：**计入**——"reasoning_content 的 Tokens 数加上 content 的 Tokens 数应小于等于 max_tokens"，官方建议思考模型设 ≥16000（https://platform.kimi.com/docs/guide/use-thinking-models.md ）。文档域名现为 platform.kimi.com（moonshot 官方平台新域名）。**Vision：支持**——kimi-k3 "原生支持视觉理解"（https://platform.kimi.com/docs/models.md ），k2.5/k2.6 "支持视觉与文本输入"；moonshot-v1 系仅 `*-vision-preview` 变体支持图像。

| API 模型 id | 前缀家族 | context window | max output（默认/最大） | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| kimi-k3 | kimi-k3 | 1,048,576 | 131,072 / 1,048,576 | 始终推理，`reasoning_effort: low/high/max`（默认 max） | 计入 | https://platform.kimi.com/docs/api/chat.md ；https://platform.kimi.com/docs/pricing/chat-k3.md |
| kimi-k2.7-code | kimi- | 256K | 未查到官方值 | thinking 强制 `{"type":"enabled","keep":"all"}`，不可关 | 计入 | https://platform.kimi.com/docs/models.md ；https://platform.kimi.com/docs/guide/use-thinking-models.md |
| kimi-k2.7-code-highspeed | kimi- | 256K | 未查到官方值 | 同上（高速版） | 计入 | 同上 |
| kimi-k2.6 | kimi- | 256K | 未查到官方值 | `thinking.type` enabled(默认)/disabled，支持 keep:"all" | 计入 | 同上 |
| kimi-k2.5 | kimi- | 256K | 未查到官方值 | thinking 可开关，不支持 keep:"all" | 计入 | 同上 |
| moonshot-v1-8k / -32k / -128k | moonshot-v1 | 8K/32K/128K（含输入+输出） | 未查到官方值 | 不支持 | — | https://platform.kimi.com/docs/models.md |

**前缀家族内例外：**
- `kimi-k3`：窗口 1M、输出上限有官方数（131,072 默认 / 1,048,576 最大），与 k2.x 的 256K 不同。
- `moonshot-v1-*`：窗口按后缀区分且含输入+输出，无 thinking（有传言 8/31 对新用户下线，未在官方页面核实到）。

---

## 智谱 GLM

thinking 参数统一为 `thinking.type: "enabled"(默认)/"disabled"`，另有 `clear_thinking`（默认 true）。思考计入口径：**未查到官方明确说明**，仅称"思考过程会消耗额外的 Token"（https://docs.bigmodel.cn/cn/guide/capabilities/thinking.md ）。max output 上限来自 API 参考：GLM-5.x/4.7/4.6 系列 128K，GLM-4.5 系列 96K（https://docs.bigmodel.cn/api-reference/模型-api/对话补全 ）。当前在售无 air 变体。**Vision：不支持**——glm-5.2 型号页「输入模态：文本」（https://docs.bigmodel.cn/cn/guide/models/text/glm-5.2 ）。

| API 模型 id | 前缀家族 | context window | max output | thinking 支持 | 思考计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| glm-5.2 | glm-5.2 | 1M | 128K | 支持（可自动判断是否思考） | 未查到官方明确说明 | https://docs.bigmodel.cn/cn/guide/models/text/glm-5.2 |
| glm-5.1 | glm- | 200K | 128K | 支持（多档思考模式） | 同上 | https://docs.bigmodel.cn/cn/guide/models/text/glm-5.1 |
| glm-5.1-highspeed | glm- | 200K | 128K | 支持 | 同上 | https://docs.bigmodel.cn/cn/guide/models/text/glm-5.1-highspeed |
| glm-5 | glm- | 200K | 128K | 支持 | 同上 | https://docs.bigmodel.cn/cn/guide/models/text/glm-5 |
| glm-5-turbo | glm- | 200K | 128K | 支持 | 同上 | https://docs.bigmodel.cn/cn/guide/models/text/glm-5-turbo |
| glm-4.7 | glm- | 200K | 128K | 支持（glm-4.7-flashx 同规格） | 同上 | https://docs.bigmodel.cn/cn/guide/models/text/glm-4.7 |
| glm-4.7-flash（免费） | glm- | 200K | 128K | 支持 | 同上 | https://docs.bigmodel.cn/cn/guide/models/free/glm-4.7-flash |

**前缀家族内例外：**
- `glm-5.2`：窗口 1M（官方称 "Solid 1M 无损上下文"），其余全系 200K——注意 `glm-5` 是 `glm-5.2` 的前缀，代码表长前缀优先。
- 若仍接入 glm-4.5 系列：max_tokens 上限为 96K，非 128K。

---

## xAI Grok

思考计入口径按端点分裂（**关键差异**）：Chat Completions 的 `max_completion_tokens` **不含** reasoning token（官方原文 "only applies to visible output tokens"）；Responses API 的 `max_output_tokens` **包含** reasoning token（"This includes both output and reasoning tokens"）。两参数默认均 128,000（https://docs.x.ai/developers/rest-api-reference/inference/chat ）。xAI 不公布各模型硬性 max output 上限（grok-4.6 官方页明确 "Output limit: No text output limit"）。**Vision：支持**——grok-4.6 与 grok-build-0.1 型号页 "Modalities: text, image → text"（https://docs.x.ai/developers/models/grok-4.6 、https://docs.x.ai/developers/models/grok-build-0.1 ）。

| API 模型 id | 前缀家族 | context window | max output（默认/最大） | reasoning 支持 | 计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| grok-4.6 | grok-4 | 500,000 | 128,000（API 参数默认）/ 无文本硬上限 | `reasoning_effort: low/medium/high(默认)/xhigh`，不可关闭 | chat 不计入 / responses 计入 | https://docs.x.ai/developers/models/grok-4.6 ；https://docs.x.ai/developers/model-capabilities/text/reasoning |
| grok-4.5 | grok-4 | 500,000 | 未查到官方值（API 默认 128,000） | low/medium/high(默认)，xhigh 按 high 处理 | 同上 | https://docs.x.ai/developers/models/grok-4.5 |
| grok-4.3 | grok-4.3 | 1,000,000 | 未查到官方值（API 默认 128,000） | none/low(默认)/medium/high（唯一支持 none） | 同上 | https://docs.x.ai/developers/models/grok-4.3 |
| grok-4.20-0309-reasoning / -non-reasoning | grok-4.20 | 1,000,000 | 未查到官方值 | 由变体固定开/关，effort 未查到官方值 | 同上 | https://docs.x.ai/developers/models/grok-4.20 |
| grok-4.20-multi-agent-0309 | grok-4.20-multi-agent | 1,000,000 | max_tokens 参数不支持 | effort 语义特殊：控制 agent 数量（4/16）而非思考深度 | — | https://docs.x.ai/developers/model-capabilities/text/multi-agent |
| grok-build-0.1（别名 grok-code-fast） | grok-build | 256,000 | 未查到官方值 | Reasoning: Yes，effort 档位未查到官方值 | 同上 | https://docs.x.ai/developers/models/grok-build-0.1 |

**前缀家族内例外：**
- `grok-4.3` / `grok-4.20*`：窗口 1M，非 grok-4.x 通值 500K；`grok-4` 是它们的前缀，长前缀优先。
- `grok-4.20-multi-agent`：不支持 max_tokens 参数，effort 语义是并行 agent 数。
- 端点差异提醒：chat 端点参数文档称 `reasoning_effort` 仅 grok-4.3 支持，4.6/4.5 的 effort 示例走 Responses API / xAI SDK——接入时按端点区分。
- 已退役（2026-05-15）：grok-4-1-fast-*、grok-4-fast-*、grok-4-0709、grok-code-fast-1、grok-3（https://docs.x.ai/developers/migration/may-15-retirement ）。

---

## Mistral

重大变化：**Magistral 原生 reasoning 系列已废弃**（官方 reasoning 文档明文；Magistral Small 1.2 标 2026-04-30 弃用，替代 Mistral Small 4）。reasoning 现经 `reasoning_effort: "high"/"none"` 参数（https://docs.mistral.ai/capabilities/reasoning ）。思考计入口径：**未查到官方明确说明**（仅称 high 档 "at the cost of increased token usage"）。各模型卡均不公布独立 max output 上限。**Vision：支持**——Mistral Large 3 "general-purpose multimodal model"（https://docs.mistral.ai/models/model-cards/mistral-large-3-25-12 ）、Mistral Medium 3.5 "frontier-class multimodal model"（https://docs.mistral.ai/models/model-cards/mistral-medium-3-5-26-04 ）。

| API 模型 id | 前缀家族 | context window | max output | reasoning 支持 | 计入 max output？ | 出处 |
|---|---|---|---|---|---|---|
| mistral-large-2512（Large 3） | mistral-large | 256K | 未查到官方值 | 卡片未标 reasoning | 未查到官方明确说明 | https://docs.mistral.ai/models/model-cards/mistral-large-3-25-12 |
| mistral-medium-3-5（Medium 3.5，dated API 名未核实） | mistral-medium | 256K | 未查到官方值 | 支持（reasoning_effort，官方推荐 agentic/code 用 high） | 同上 | https://docs.mistral.ai/models/model-cards/mistral-medium-3-5-26-04 |
| mistral-small-2603（Small 4，hybrid） | mistral-small | 256K | 未查到官方值 | 支持（reasoning_effort） | 同上 | https://docs.mistral.ai/models/model-cards/mistral-small-4-0-26-03 |

例外：`mistral-large-2512` 模型卡未标 reasoning 支持，家族内唯一。magistral-* 已入弃用通道，代码表不收。另有 Ministral 3 边缘系列在售，本次未逐卡核实。

---

## 按前缀归并的建议表（最终进代码的形态）

取家族内最保守/最通用值；例外单列为更长前缀，**匹配必须长前缀优先**。`thinking` 为布尔（该前缀是否支持 reasoning/thinking 参数）。`vision` 为布尔（该前缀的模型是否接受图像输入，喂给 prompt 的能力块——文本模型不得接看图任务）。`maxTokens` 取官方 max output 上限（无官方值的标注说明，勿猜数字填入）。

| prefix | contextWindow | maxTokens | thinking | vision | 备注 |
|---|---|---|---|---|---|
| `claude-haiku-4-5` | 200000 | 64000 | true | true | extended（budget_tokens）形态；全系 vision |
| `claude-sonnet-4-5` | 200000 | 64000 | true | true | extended 形态；全系 vision |
| `claude-opus-4-5` | 200000 | 64000 | true | true | extended 形态；全系 vision |
| `claude-` | 1000000 | 128000 | true | true | 思考计入 max_tokens；全系 vision |
| `gpt-5.6-cyber` | 400000 | 128000 | true | true | 全系 vision（gpt-5.6-sol 页证实） |
| `gpt-5.6` | 1050000 | 128000 | true | true | |
| `gpt-5.5-cyber` | 未查到官方值 | 未查到官方值 | true | true | 型号页缺失，暂勿依赖 |
| `gpt-5.5` | 1050000 | 128000 | true | true | |
| `gpt-5.4-mini` | 400000 | 128000 | true | true | |
| `gpt-5.4-nano` | 400000 | 128000 | true | true | |
| `gpt-5.4` | 1050000 | 128000 | true | true | |
| `gpt-5.2` | 400000 | 128000 | true | true | |
| `gpt-5.1` | 400000 | 128000 | true | true | effort 无 xhigh |
| `gpt-5-pro` | 400000 | 272000 | true | true | 全 OpenAI 唯一非 128K |
| `gpt-5` | 400000 | 128000 | true | true | 兜底裸 gpt-5/-mini/-nano（2026-12-11 下线）；必须排在所有 gpt-5.x 之后 |
| `o3` / `o4` / `o1` | 200000 | 100000 | true | true | 均在弃用通道 |
| `deepseek` | 1000000 | 384000 | true | **false** | 计入与否官方未明；**无图像输入（FEATURES 表无 vision）** |
| `gemini` | 1048576 | 65536 | true | true | 全系一致；thinking_level 参数；全系 multimodal |
| `qwen-max` | 32768 | 8192 | false | false | 滞后旧快照；文本仅 |
| `qwen3.` | 1000000 | 131072 | true | true | 思维链不计入；Image/Text/Video 输入 |
| `qwen-` | 1000000 | 32768 | true | false | 兜底旧别名 plus/flash；思维链不计入；文本仅 |
| `kimi-k3` | 1048576 | 131072 | true | true | 官方最大可到 1048576；思考计入；原生视觉 |
| `kimi-` | 262144 | 未查到官方值 | true | true | k2.x 家族；思考计入；k2.5/k2.6 支持视觉与文本 |
| `moonshot-v1-8k` | 8192 | 未查到官方值 | false | false | 窗口含输入+输出；仅 -vision-preview 变体有图像 |
| `moonshot-v1-32k` | 32768 | 未查到官方值 | false | false | 同上 |
| `moonshot-v1-128k` | 131072 | 未查到官方值 | false | false | 同上 |
| `glm-5.2` | 1000000 | 128000 | true | false | 注意排在 `glm-5` 前；文本仅 |
| `glm-` | 200000 | 128000 | true | false | 计入与否官方未明；文本仅 |
| `grok-4.3` | 1000000 | 128000 | true | true | maxTokens 取 API 参数默认值 |
| `grok-4.20-multi-agent` | 1000000 | 未查到官方值 | true | true | 不支持 max_tokens 参数，effort 语义特殊 |
| `grok-4.20` | 1000000 | 128000 | true | true | maxTokens 取 API 参数默认值 |
| `grok-build` | 256000 | 128000 | true | true | 同上 |
| `grok-` | 500000 | 128000 | true | true | 兜底 4.6/4.5；chat 端点思考不计入、responses 端点计入 |
| `mistral-large` | 256000 | 未查到官方值 | false | true | 卡片未标 reasoning；Large 3 multimodal |
| `mistral-` | 256000 | 未查到官方值 | true | true | medium/small 支持 reasoning_effort；Medium 3.5 multimodal |

归并原则备注：
1. 长前缀优先是硬前提——`gpt-5` vs `gpt-5.x`、`glm-5` vs `glm-5.2`、`qwen-` vs `qwen-max`、`grok-4` vs `grok-4.3` 都存在前缀包含关系。
2. maxTokens 语义各家不同：Anthropic/OpenAI/Kimi 是「含思考的硬顶」，Qwen 是「不含思维链的回答上限」，xAI 按端点分裂，Gemini/GLM/DeepSeek/Mistral 官方未言明——代码表如需统一语义，建议另加 `thinkingCounted: yes/no/unknown/per-endpoint` 一列。
3. vision 一列的语义是「模型是否接受图像输入」，与端点级 `supportsImages`（协议是否带图）不同；未知家族走保守默认 true（Claude 系基线），文本家族（deepseek/glm/qwen- 旧别名）显式 false。
4. 「未查到官方值」的格子进代码时留空或用保守配置，不要拿本表以外的数字回填。
