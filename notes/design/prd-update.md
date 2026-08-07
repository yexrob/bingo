# PRD: 版本检测 + 欢迎卡片提示 + `bingo update` 命令

> 状态：v1.1（pm 对齐稿，2026-08-09）
> **v1.1 修订（pm，2026-08-09）**：uiux 视觉规格 `update-banner.md` v1.1 已定稿（commit 607c353）——C 组视觉锚点整体切换为以该规格为唯一事实源（PRD C 组只验收不定义）：文案改 `New version {v} available — run bingo update`（无 ✦ 前缀/命令引号）；动效范围 = **版本号与 `bingo update` 两段同相位正弦呼吸**（C3 同步）；降级链补充 `motion: off` / `BINGO_NO_MOTION` 主动关停；新增截断链（50/43/15）与特效范围断言（欢迎卡其余行任意两帧一致）；身份行硬编码修正（E3 保留）。
> 现状锚点：bingo v0.2.1（Cargo.toml）；GitHub Releases `yexrob/bingo`，资产 = `bingo-<target-triple>.tar.gz/.zip` × 4 平台 + `checksums.txt`，latest 指向最新 tag。
> 欢迎卡片实现位置：`src/tui/chat.rs:4998` `welcome_rows`（注意：版本行硬编码 `bingo v0.1.0`，本次应改用编译期版本 `CARGO_PKG_VERSION`，与检测对比同源）。
> 视觉事实源：[`update-banner.md`](./update-banner.md) v1.1（布局/文案/动效/降级/锚点 11 条——本文 C 组只验收不定义）。
> CLI 结构：clap 子命令（`src/main.rs:84` `Command` enum，现有 `share` 快路径模式可复用）。
> 网络：`reqwest`（rustls）已在依赖中，无需新增 HTTP 依赖。

## 1. 目标与用户场景

**一句话**：用户启动 bingo 时异步知晓新版本（欢迎卡片提示），`bingo update` 一条命令自动下载、校验、替换，离线/失败时安静降级、绝不阻塞。

**用户场景**：

1. **发现**：用户日常启动 TUI，欢迎卡片上出现一行克制的提示 `✦ v0.3.0 available — run 'bingo update'`（版本号着色、轻微闪动），不影响任何操作。
2. **主动检查**：`bingo update --check` 输出当前版本 vs latest，脚本可 grep（`[update] ...` 单行格式）。
3. **更新**：`bingo update` 自动完成 下载 → sha256 校验 → 原子替换 → 提示重启生效。
4. **失败/离线**：断网或 GitHub 不可达 → 检测静默跳过（有 TTL，不反复重试）；`bingo update` 失败 → 明确错误码 + 手动下载指引。

**非目标**：不做自动安装（只提示不更新）、不做签名验证（v1 checksum 即可）、不做回滚命令、不做安装器。

## 2. 范围边界（v1 明确不做）

| 不做 | 理由 |
|---|---|
| 自动更新推送（后台静默安装） | 用户需知情与控制；v1 只提示 + 显式命令 |
| 签名验证（notarization / Authenticode / sigstore） | 发布流程尚无签名链；checksum 已防损坏与篡改（传输中），签名留 v2 |
| 更新回滚命令（`--rollback` / 备份保留） | 原子替换保证无半更新态；失败即回退到旧版本，无需回滚功能 |
| Windows 安装器（MSI/NSIS） | v1 仅 zip 资产 + 原位替换（exe 占用则给手动指引） |
| 增量/断点续传、多线程下载 | 单文件几十 MB 级，直下即可 |
| `--target` 手动指定平台资产 | 自动探测即可；交叉下载留 P2 |
| 预发布/旧版渠道选择 | 只跟 `releases/latest` 稳定版 |
| Homebrew/包管理器集成 | 无 formula；P2 若有 brew 渠道再议 |
| 欢迎卡提示的持久化 dismiss | TTL 缓存已限频；加 dismiss 状态是额外持久化，需求未证实 |

## 3. 功能清单

| # | 功能 | 描述 | 优先级 |
|---|---|---|---|
| F1 | 启动异步版本检测 | TUI 启动后异步请求 `releases/latest`，不阻塞首帧渲染与任何输入 | P0 |
| F2 | 检测结果 TTL 缓存 | `~/.local/share/bingo/update-check.json`：成功 TTL 24h、失败 TTL 1h，期内不再发请求 | P0 |
| F3 | 检测失败静默 | 超时（5s）/连接失败/解析失败 → 不提示、不报错、不阻塞；写失败时间戳防重试风暴 | P0 |
| F4 | 欢迎卡片提示行 | 有新版时欢迎卡片新增提示行（文案/样式/位置以 `update-banner.md` v1.1 为准） | P0 |
| F5 | 提示行动效 | 版本号与 `bingo update` 两段同相位正弦呼吸（类 Claude Code thinking，随 TUI tick 驱动；视觉规格见 `update-banner.md` §2） | P0 |
| F6 | 动效降级 | `motion: off` / `BINGO_NO_MOTION` → 静态 rest；无 truecolor → 离散两步；`NO_COLOR` → 静态 bold（提示保留不消失） | P0 |
| F7 | `bingo update` 命令 | 下载当前平台资产 → checksum 校验 → 解压 → 原子替换 → 提示重启 | P0 |
| F8 | `bingo update --check` | 只检查打印结果，不下载不替换；headless 可用 | P0 |
| F9 | 错误码契约 | `UPDATE_*` 错误码登记 `src/error.rs` + 防漂移测试 | P0 |
| F10 | settings 开关 `updateCheck` | 总开关（默认 true），敏感/离线环境可关闭检测 | P1 |
| F11 | 内置技能同步 | guide.md 命令速查 + 诊断指南更新 | P0 |

## 4. 方案要点

### 4.1 检测策略

- **时机**：TUI 启动后 spawn 异步任务（Tokio `spawn`），结果经 channel 送达 UI；`--print`/headless 与 `share` 等子命令快路径**不检测**（脚本场景不被打扰）。
- **数据源**：`GET https://api.github.com/repos/yexrob/bingo/releases/latest`（须带 User-Agent），取 `tag_name`。API 限频 60/h 无认证——TTL 24h 已充分保护；实现可备选「跟 302 到 /releases/latest 从 Location 取 tag」的无 API 方案。
- **版本对比**：semver 比较（tag 去 `v` 前缀；解析失败视为无新版，静默）。`0.2.1 < 0.2.10`、`0.2.1 < 0.3.0`、预发布 tag（`-rc`/`-beta`）不与正式版混排，解析不出合法 semver 一律忽略。
- **缓存**：`update-check.json` = `{ checked_at: epoch_secs, latest: Option<String>, ok: bool }`。启动时读缓存，TTL 内直接用缓存结果（有新版仍提示，但不再发请求）；TTL 过期才异步重检。失败写 `ok:false` + 时间戳，1h 内不重试。
- **接入点**：检测结果（或失败）到达前欢迎卡片按无提示渲染；结果到达后插入提示行/更新该行（结果最迟在首轮渲染完成前到达的典型路径；实现以「不重绘已 flush 的 scrollback」为约束，见 C4）。

### 4.2 欢迎卡片提示

**视觉唯一事实源 = [`update-banner.md`](./update-banner.md) v1.1**（commit 607c353）——布局、文案、动效、降级、实现方案全部以该规格为准，本文只定义验收与出现条件。规格要点：

- **位置**：版本身份行（`bingo vX.Y.Z · …`）正上方、cwd 行之下（空行节奏：cwd 与提示行之间一个空行，与身份行相邻构成「旧 vs 新」对照块）。
- **文案**：`New version v0.3.0 available — run bingo update`（无 ✦ 前缀、无命令引号）。三段样式：静态段（`New version ` / ` available — run `）`theme.inactive`；呼吸段①版本号 `vX.Y.Z` 呼吸色；呼吸段②`bingo update` 呼吸色 + bold（与①同相位）。
- **动效**：正弦呼吸（不是硬闪烁/扫光/ANSI 闪烁码），两档品牌橙间 sRGB 线性插值——暗色 `#D77757 ↔ #E8896B`（全程 ≥6.24:1）、浅色 `#B05227 ↔ #9A4A24`（全程 ≥4.72:1）；周期 3.0s（90 帧 @30fps，复用既有 TICK），总时长 9s（3 个呼吸）后静止在 rest 色；相位函数 `t = 0.5 − 0.5·cos(2π·phase/90)`（phase 0 = rest，无突跳）。**特效范围仅此一行内两个关键词段，欢迎卡其余一切元素任何一帧不参与动画；无入场动画（静默插入）**。
- **降级链**：`motion: "off"`（settings 新增键，默认 auto）/ `BINGO_NO_MOTION=1` → 静态 rest（提示保留不消失）；无 truecolor → 离散两步（2s 周期、peak 400ms，不崩溃）；`NO_COLOR`/单色 → 静态 bold；用户输入 → 提前停止（P1）。
- **窄屏截断链**（`banner_line(v, width)` 纯函数）：inner_w ≥50 完整行 / ≥43 去 available 分句 / ≥15 只留 `bingo update` / <15 隐藏；任何档位命令可见（<17 列除外）、不溢出卡框。
- **可忽略性**：提示只是卡片上一行，无交互、无阻塞、不抢焦点；TTL 保证一天最多提示一次。不引入 dismiss 持久化（v1 减法）。
- **实现约束**（规格 §3.2 方案 A）：动画窗口 9s 远早于欢迎卡落盘时机，窗口内保持欢迎卡为活文档行、到期静止后以 rest 色自然落盘——全程不触碰 scrollback（视口以上永不重绘不变量保持）。接线：`Chat` 持有 `UpdateBanner { latest, anim_until_tick }`，`has_dynamic_rows()` 在动画窗口内持续置 dirty，`update_color(theme, phase)` 为纯函数可直接单测。

### 4.3 `bingo update` 命令

```
bingo update [--check]
```

| 参数 | 说明 |
|---|---|
| `--check` | 只检测并打印结果，不下载不替换 |

**流程**（`bingo update`，无 `--check`）：

1. 请求 latest tag；若当前已是最新 → 输出 `[update] up-to-date v0.2.1`，exit 0，不发下载请求。
2. 平台资产映射（`std::env::consts` 探测）：

   | 平台 | 资产 |
   |---|---|
   | `aarch64-apple-darwin`（Apple Silicon） | `bingo-aarch64-apple-darwin.tar.gz` |
   | `x86_64-apple-darwin`（Intel Mac） | `bingo-x86_64-apple-darwin.tar.gz` |
   | `x86_64-pc-windows-msvc` | `bingo-x86_64-pc-windows-msvc.zip` |
   | `x86_64-unknown-linux-gnu` | `bingo-x86_64-unknown-linux-gnu.tar.gz` |
   | 其他 | 明确报错 `UPDATE_UNSUPPORTED_PLATFORM` |

3. 下载资产 + `checksums.txt` 到 `~/.local/share/bingo/update/` 临时区；sha256 与 `checksums.txt` 中对应文件名行比对，**不匹配 → 拒绝安装**、清理临时文件、非零退出（`UPDATE_CHECKSUM_MISMATCH`）。`checksums.txt` 缺失或行缺失同样拒绝（安全优先）。
4. 解压（新增 `tar` + `flate2` / `zip` 依赖），取二进制（`bingo` / `bingo.exe`）。
5. **原子替换**：目标 = `std::env::current_exe()`。POSIX：临时区二进制落盘为同目录新文件后原子 `rename` 覆盖（必要时先 `rename` 旧文件为 `.old` 再换入——两段式保证任一步失败旧版本仍可用，成功后清理 `.old`）。替换成功 → 输出 `[update] updated to v0.3.0 — restart bingo to apply`，exit 0。
6. **权限失败**：`current_exe` 目录不可写（如 `/usr/local/bin`）→ 不静默降级，报 `UPDATE_PERMISSION` + 安装路径 + 指引（`sudo bingo update` 或手动下载 URL）。
7. **解压失败/资产损坏** → 报错、清理临时文件、非零退出。
8. Windows 上运行中 exe 无法原位替换 → 报错 + 手动替换指引（v1 不做安装器）。

**输出契约**（headless 可 grep，复用现有单行风格）：
- `[update] latest v0.3.0 (current v0.2.1)` — `--check` 有新版
- `[update] up-to-date v0.2.1` — 已最新
- `[error] code=UPDATE_* msg=...` — 失败，非零退出（走统一错误码出口）

**错误码**（登记 `src/error.rs`，SCREAMING_SNAKE、只增不改）：`UPDATE_CHECK_FAILED`（网络/API）、`UPDATE_CHECKSUM_MISMATCH`、`UPDATE_DOWNLOAD_FAILED`、`UPDATE_EXTRACT_FAILED`、`UPDATE_PERMISSION`、`UPDATE_UNSUPPORTED_PLATFORM`、`UPDATE_INSTALL_FAILED`（替换阶段）。

**macOS 风险提示**：非公证二进制经下载会带 quarantine 属性，Gatekeeper 可能拦截首次运行——更新成功提示语后追加一行指引（`xattr -d com.apple.quarantine <path>`），v1 不自动清除（安全考虑，留给用户判断）。

## 5. 验收标准（每项可验证）

### A. 版本检测逻辑
- A1. semver 对比正确：`0.2.1 < 0.2.10`、`0.2.1 < 0.3.0`、`0.3.0` vs `0.3.0` 视为已最新；tag 带 `v` 前缀可解析（单测）。
- A2. tag 解析失败（非 semver / 空）→ 静默视为无新版，不提示不报错。
- A3. 检测到新版 → 缓存与提示同源（提示行版本号 = 检测结果版本号）。

### B. 缓存与异步
- B1. TTL 生效：首次检查写缓存后，24h 内再启动不发网络请求（注入时钟/缓存路径可测；mock 服务器计数断言请求数=1）。
- B2. 失败限频：网络失败写 `ok:false` 时间戳，1h 内不重试。
- B3. 首帧不阻塞：mock 网络延迟（如 3s 超时前）下 TUI 首帧照常渲染，无等待；检测结果到达后提示行出现/更新。
- B4. `--print` / 子命令快路径（`share` 等）：不触发检测、不输出任何 update 相关行。

### C. 欢迎卡片提示（视觉以 `update-banner.md` v1.1 为唯一事实源，锚点 1-11 条为完整断言；本组为 PRD 层合并项）
- C1. 有新版（缓存或实时检测结果）→ 欢迎卡片出现提示行，文案 = `New version {v} available — run bingo update`（三段样式：静态段 inactive、版本号与 `bingo update` 呼吸色且命令 bold）；无新版 → 欢迎卡布局与现状逐行一致（回归）。
- C2. 检测失败 / 缓存无结果 → 欢迎卡片与现状完全一致，无提示行。
- C3. 动效范围（特效范围断言）：任意两帧渲染中，欢迎卡其余行（✻ 问候/╭╮ 边框//help/cwd/身份行）完全一致（对 doc.rows 静态行快照断言）；提示行出现无入场动画（静默插入）。
- C4. 呼吸正确性（纯函数 `update_color`）：truecolor 下 phase 0 = rest、phase 45 ≈ peak（±1/255）、phase 90 = rest，0→45 单调上升、45→90 单调下降；**版本号段与命令段在同一 phase 取相同 Color（同相位）**；行内静态段恒为 `theme.inactive`（任意 phase 不变）；帧循环动画窗口内持续置 dirty、窗口外恢复 idle（零写入）。
- C5. 窗口与停止：9s（270 帧）后静止 rest 色，`needs_tick()` 恢复 false；欢迎卡落盘后为静止 rest 色（scrollback 不变量）；窗口内 resize → rehydrate 后动画继续、无重复动画副本、视口以上零重绘。
- C6. 降级链：`motion: "off"` / `BINGO_NO_MOTION=1` → 全程静态 rest、提示行仍在；无 truecolor → 离散两步（peak 400ms / rest 1600ms）不崩溃；`NO_COLOR` → 静态 bold。用户输入提前停止（若实现 P1）。
- C7. 窄屏截断链：inner_w 50/43/15 边界逐档核对（`banner_line` 纯函数），任何档位 `bingo update` 可见（<17 列除外）、不溢出卡框、不换行。
- C8. 对比度：暗色每帧 ≥6.24:1、浅色每帧 ≥4.72:1（停驻帧 = rest）；浅色主题不得出现 `#D77757` 亮橙档。
- C9. 布局稳定与不阻塞：提示行与卡片边框对齐（`│` 包裹内），重排/滚动/落盘不闪屏、不截断；出现期间输入、命令、滚动全部正常（无焦点抢占）。
- C10. 无 ANSI 闪烁：输出不含 `\e[5m`（grep 断言）。

### D. `bingo update`
- D1. 平台资产映射：四平台探测各自命中正确文件名（单测：`aarch64-apple-darwin` → `.tar.gz` 等）；未知平台 → `UPDATE_UNSUPPORTED_PLATFORM`。
- D2. `--check` 输出契约：有新版 `[update] latest v0.3.0 (current v0.2.1)` exit 0；已最新 `[update] up-to-date v0.2.1` exit 0；网络失败 `[error] code=UPDATE_CHECK_FAILED ...` 非零退出。
- D3. 已最新时执行 `bingo update`：不下载任何资产，输出 up-to-date，exit 0。
- D4. 更新成功：mock 服务器验证「下载 → sha256 匹配 → 解压 → 替换」全链路，替换后 `current_exe` 为新版二进制（测试用伪造二进制 + 临时安装目录），输出含新版本号与重启提示，exit 0。
- D5. checksum 不匹配：拒绝安装、临时文件已清理、非零退出 + `UPDATE_CHECKSUM_MISMATCH`（mock 服务器返回篡改资产）。
- D6. `checksums.txt` 缺失 / 无对应资产行：同样拒绝安装（安全优先），明确报错。
- D7. 权限不足（安装目录只读）：报 `UPDATE_PERMISSION` + 安装路径 + sudo/手动指引，不产生半更新状态。
- D8. 解压失败 / 资产损坏：报错、临时文件清理、非零退出。
- D9. 替换失败（如模拟 rename 失败）：旧二进制保持可用（两段式回退），报 `UPDATE_INSTALL_FAILED`，非零退出。
- D10. 所有 `UPDATE_*` 错误走统一错误码出口（TUI/CLI 双出口一致），exit=1 + `[error] code=...` 单行格式。

### E. 质量与契约
- E1. `cargo build`、`cargo clippy -- -D warnings`、`cargo test` 全绿；检测/缓存/映射/校验逻辑带内联单测。
- E2. `src/error.rs` 登记全部 `UPDATE_*` 码 + 防漂移单测（枚举每个 variant）。
- E3. 欢迎卡版本行改用编译期版本（`CARGO_PKG_VERSION`），与检测对比同源（顺手修正 v0.1.0 硬编码）。
- E4. 内置技能 `src/skills/bundled/guide.md` 同步：`bingo update [--check]` 命令速查、updateCheck 配置表、诊断指南（网络失败/权限/checksum 场景）。
- E5. 新增依赖仅 `tar`/`flate2`/`zip` 三件（解压用），不加其他。

## 6. 验收顺序建议（依赖关系）

1. 检测核心（semver 对比 + TTL 缓存 + 失败限频，纯函数可先测）→ 2. 异步接线（spawn + 不阻塞首帧）→ 3. 欢迎卡片提示行（静态 → 动效/降级，按 `update-banner.md` §5 锚点）→ 4. `bingo update`（映射/下载/校验/替换/权限）→ 5. 错误码 + 文档（guide.md）收口。

## 7. 风险与未决项

- **动效与文档模型冲突**（已解）：欢迎卡 flush 进 scrollback 后动画停止、静态落盘——uiux 规格 §3.2 方案 A 已给出接线（9s 动画窗口远早于正常落盘时机，窗口内保持活行、到期静止后自然落盘，不碰 scrollback）；实现按规格执行即可，不再需要降级预案。
- **GitHub 可达性**（限频/地区网络）：TTL + 静默失败 + `--check` 手动入口三重缓解；update 失败报错里附手动下载 URL。
- **checksums.txt 维护**：发布流程手工维护，缺失/滞后会导致 update 拒绝安装——这是安全优先的预期行为，发布 checklist 需保证 checksums 同步（发布者责任）。
- **macOS Gatekeeper**：v1 不做签名，quarantine 拦截风险以更新成功后的指引提示处理；如用户反馈频繁，v2 评估签名。
- **`sudo bingo update` 场景**：以 root 运行 update 会替换 root 拥有的安装目录文件，但缓存/临时区仍在用户 home——实现注意权限边界，root 下写入用户缓存需显式处理（P2 细究）。
