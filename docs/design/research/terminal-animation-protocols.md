# 现代终端动图与图形协议调研

> 只读调研，2026-09-04。优先采用协议作者、终端项目官方文档和源代码；“支持”指终端实现明确处理该协议，不把第三方脚本或传闻算作协议能力。版本会变化，矩阵中的“未确认”不是“不支持”。

## 结论先行

1. **Kitty graphics protocol 是目前唯一在协议层明确建模动图的主流终端图形协议。** 它不是把 GIF 文件交给终端解码，而是先传输一个根图像，再传输以根图像为基础、可带局部矩形和增量合成的帧；每帧有毫秒间隔，终端可独立播放、暂停、循环或等待更多帧。详见 [Kitty animation](https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation)。
2. **iTerm2 inline image protocol 对 GIF 有明确的原生动画支持，但不是通用的“协议帧控制”。** 其 `File` OSC 携带完整文件，终端通过 macOS 图像解码能力显示；官方只明确写出 animated GIF（iTerm2 2.9.20150512 起），并未在协议文档中承诺 APNG 或提供逐帧 seek/pause/loop 控制。见 [iTerm2 Images](https://iterm2.com/documentation-images.html)。
3. **Sixel 是光栅图像传输/绘制格式，不是动图协议。** 要动起来，发送端必须定时发送新图像（通常整帧重绘，或自行计算差分）；没有标准的帧间隔、播放状态、循环、帧 ID 或终端驱动播放语义。DEC 的图形命令定义见 [VT330/VT340 Graphics Programming](https://vt100.net/docs/vt3xx-gp/)；现代实现参考 [xterm graphics](https://invisible-island.net/xterm/)。
4. **不要把终端“能显示静态 kitty 图”当作能显示 kitty 动图。** kitty 的动画是 0.20.0 才加入的扩展，转发器和兼容实现可能只实现静态传输/放置。应用应按能力探测、设置预算，并准备静态首帧或文本占位回退。
5. **Bingo 当前 TUI 已采用正确的静态架构方向：** kitty APC 负责存储图片，Unicode placeholder 负责让 ratatui 的文本滚动携带图片；tmux 走 passthrough。动图应是 `graphics` 层的可选能力，不应进入 `SessionState` 或持久化事件；首版默认静态首帧、动图按明确开关启用。

## 1. Kitty graphics protocol：传输与逐帧控制

### 1.1 基本封装和传输

命令形状为 `ESC _ G <key=value,...> ; <payload> ESC \\`（APC）。payload 是 Base64，避免旧终端把二进制误识别为控制字符。格式包括：

- `f=24`：RGB；`f=32`：RGBA；`f=100`：PNG；协议**不要求终端理解 GIF/APNG/JPEG 等文件格式**。
- `t=d`：数据在控制序列中；`t=f` / `t=t`：文件/临时文件；`t=s`：共享内存。远程客户端通常用 `t=d`。
- 远程 payload 分块，每块 Base64 不超过 4096 字节；首块给出图像属性，后续块只给 `m`（是否还有后续）。终端收到并验证完整图像前不得显示。
- 图片先以 image ID 存储，再用 placement ID 多次放置；`a=d` 释放放置或存储。协议还规定 quota、清屏、alternate screen 和滚动时的生命周期行为。

来源：[graphics escape code](https://sw.kovidgoyal.net/kitty/graphics-protocol/#the-graphics-escape-code)、[pixel data](https://sw.kovidgoyal.net/kitty/graphics-protocol/#transferring-pixel-data)、[deleting images](https://sw.kovidgoyal.net/kitty/graphics-protocol/#deleting-images)、[interaction with terminal actions](https://sw.kovidgoyal.net/kitty/graphics-protocol/#interaction-with-other-terminal-actions)。

### 1.2 动画模型

动画必须绑定到某个 image ID：

1. 先发送普通根图像；
2. `a=f,i=<id>` 发送帧。帧可为全图或 `x,y,s,v` 指定的局部矩形；可从前一帧继承 (`c`)、选择背景色 (`Y`)、覆盖或 alpha blend；
3. `z` 指定帧间隔毫秒数，负值是 gapless（可作为中间合成帧，不直接显示）；
4. `a=c` 可把一个帧的矩形合成到另一个帧，适合“静态背景 + 小对象移动”，减少带宽；
5. `a=a` 控制播放：`s=1` 停止，`s=2` 播放并在末尾等待新帧，`s=3` 正常循环；`v` 控制循环次数；`c` 直接切换当前帧；也可用 `r,z` 修改指定帧间隔；
6. `a=d,d=f` 删除动画帧。

这是**终端驱动播放**与**客户端驱动切帧**两种模式。终端驱动模式解决 SSH 延迟和客户端退出问题；客户端驱动模式可精确控制但每次切帧都要发送控制序列，并要求客户端保持运行。来源：[Animation](https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation)、[frame data](https://sw.kovidgoyal.net/kitty/graphics-protocol/#transferring-animation-frame-data)、[animation control](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-animations)、[frame composition](https://sw.kovidgoyal.net/kitty/graphics-protocol/#composing-animation-frames)。

### 1.3 Unicode placeholders 的意义和限制

Kitty 的 `U=1` 建立虚拟放置，实际屏幕位置由 `U+10EEEE` 及行列 combining marks 表示，image ID 放入前景色。因为这些是普通 Unicode 单元，tmux、vim、weechat 等宿主只要能保留 Unicode 和转发控制序列，文本移动时图片也能随之移动。Bingo 当前正是这一模式。

但 placeholder 不是动画控制器：它只标识“此处显示某 image ID”。动画帧替换发生在终端的图片存储/渲染层，应用仍须处理帧缓存、删除、重绘和终端失去焦点等问题。长图还有协议定义的 row/column combining mark 上限，且并非所有 kitty-compatible 实现都正确实现 placeholder。来源：[Unicode placeholders](https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders)。

## 2. iTerm2 inline images：GIF、APNG 与控制边界

协议是私有 OSC：`ESC ] 1337 ; File=... BEL`（也可用 ST）。参数包含 `name`、`size`、`width`、`height`、`preserveAspectRatio`、`inline=1`。内容是完整文件的 Base64；iTerm2 3.5 增加 `MultipartFile`/`FilePart`/`FileEnd` 以适应 tmux 和 1 MiB 序列限制。

官方明确说：**animated GIFs supported since 2.9.20150512**。同时说 inline image 会接受 macOS 能显示的图像格式（示例含 PNG、GIF、PDF、PICT）。这足以确认 GIF 原生动，但不能据此把 APNG 宣称为协议保证：官方协议页没有 APNG 示例、帧控制键或 APNG 兼容承诺；应以目标 iTerm2/macOS 版本实测为准。该协议没有 image ID、帧 ID、帧间隔、暂停、跳帧、终端驱动循环等 kitty animation 等价物。

来源：[iTerm2 Inline Images Protocol](https://iterm2.com/documentation-images.html)。

## 3. Sixel：逐帧重绘而非动画

Sixel 把彩色像素编码为字符序列（一个字符代表垂直方向 6 个像素），通过 DCS 传输给终端。它描述“绘制这一张图”，没有 kitty 那样的图像对象/帧对象/定时器/播放状态。故动画只能由客户端循环发送 Sixel：

- 全帧发送：实现简单，但带宽、解析和终端重绘成本高；
- 客户端差分：应用自行生成局部图像并定位重绘，但差分格式和清除语义不是统一动画契约；
- SSH 下帧率受 RTT、压缩、PTY/转发器缓冲影响，不能靠终端缓存自动平滑播放。

“支持 Sixel”也不能推出“支持 GIF”：通常发送端（如 `chafa`/`img2sixel`）负责解码 GIF 并生成一系列 Sixel。协议资料：[VT3xx graphics programming](https://vt100.net/docs/vt3xx-gp/)、[libsixel](https://github.com/saitoha/libsixel)（库与工具实现，不是新协议规范）。

## 4. 终端与转发器兼容边界

下表只记录官方页面或官方源码可直接支持的范围；“静态”不等于“动画”。版本更新应重新探测，不应仅依据 `$TERM`。

| 终端/层 | Kitty graphics | Kitty animation | iTerm2 File | Sixel | 证据/边界 |
|---|---:|---:|---:|---:|---|
| kitty | 原生 | **协议原生** | 非目标重点 | 非目标重点 | [官方 graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)；动画自 0.20.0 |
| WezTerm | 官方 features 列出支持 | **未从官方文档确认完整 `a=f/a=a`** | 原生 | 实验性 | [features](https://wezterm.org/features.html)、[imgcat](https://wezterm.org/imgcat.html)；其文档还提示 multiplexer 尚未完整处理 image protocol |
| Ghostty | kitty 官方兼容列表列出 Ghostty | **未单独确认版本/完整度** | **未确认** | **未确认** | [kitty implementations list](https://sw.kovidgoyal.net/kitty/graphics-protocol/#applications-using-the-kitty-graphics-protocol)；不能把列表当作动画 conformance 证明 |
| Konsole | kitty 官方兼容列表列出（MR 594） | **未确认** | **未确认** | **未确认** | [Konsole MR 594](https://invent.kde.org/utilities/konsole/-/merge_requests/594)；应按发行版版本实测 |
| foot | 官方文档未确认图形协议 | 否/未确认 | 否/未确认 | 否/未确认 | [foot repository](https://codeberg.org/dnkl/foot)；“终端可运行”不代表图形协议支持 |
| Windows Terminal | 官方资料与此调研未确认 kitty | 未确认 | 未确认 | 版本/发行渠道相关，未在此确认 | [Windows Terminal repository](https://github.com/microsoft/terminal)；不得以 Windows Console 的能力推断 |
| macOS Terminal | 未确认 | 未确认 | 非 iTerm2 | 未确认 | Apple Terminal 与 iTerm2 是不同实现，不能互相继承协议 |
| iTerm2 | kitty 官方兼容列表列出实现提交 | **非 kitty 动画契约** | **GIF 原生** | 未确认 | [iTerm2 image docs](https://iterm2.com/documentation-images.html) |
| Alacritty | 官方 FAQ/仓库未确认内建图形 | 未确认 | 未确认 | 未确认 | [Alacritty repository](https://github.com/alacritty/alacritty)；不要因为 truecolor/Unicode 支持就推断图片支持 |

### tmux、screen、SSH

- tmux 是持久 server + client + PTY 的中间层；运行其中的程序看到的 `$TERM` 通常是 `screen`/`tmux`（[tmux manual](https://man7.org/linux/man-pages/man1/tmux.1.html)），不应据此判断外层终端能力。
- Kitty 图形序列要穿过 tmux passthrough；tmux 版本和配置（尤其 `allow-passthrough`）决定能否到达外层。序列还受旧 tmux 长度限制，必须小块分片。iTerm2 3.5 的 multipart 明确针对 tmux 限制，但不代表任意 tmux/screen 都能处理。
- GNU screen 的 DCS 转发与终端图形兼容不能假定；若 passthrough 不透明，应回退。screen/tmux 会重放或保存文本屏幕，不一定保存终端 GPU 的图像对象。
- SSH 只传输字节，不提升终端能力：远端应用发出的协议最终由本地外层终端解释。kitty 的 `t=f/s` 文件或共享内存通常只适用于本机；远程应使用 `t=d`，并承担 Base64 膨胀、带宽、RTT 和转发器上限。

## 5. 性能、带宽、清屏、滚动与生命周期

### 带宽和节流

RGBA 原始数据约为 `width × height × 4` 字节；Base64 约增加 4/3。静态 PNG 或 zlib 可显著降低首帧，kitty 的局部帧/合成能进一步降低动图成本。Sixel 的实际大小取决于颜色、重复行和编码；没有统一的帧压缩或确认机制。

应用应：限制像素尺寸、帧数、总缓存和帧率；以单调时钟调度；发送队列有界，落后时丢弃中间帧而不是无限堆积；把“最终一致的当前帧”优先于每帧必达。终端无响应时不可把每个帧写入 TUI 主绘制路径。

### 清屏、滚动与重排

Kitty 规范要求 reset、切换 alternate screen、`CSI 2J` 清除可见图像；普通擦除命令不会自动删除图像对象，客户端仍要发专用 delete。滚动时图像应与文本滚动并裁剪，但 placeholder 方案依赖宿主正确移动 Unicode 单元。窗口 resize、ratatui reflow、tmux pane resize 都可能使原 placement 的 cell 矩形失效，须重新测量 cell 像素尺寸并重建/重放。

### 生命周期

应把生命周期分成：`Decoded`（应用像素缓存）、`Uploaded`（终端 image ID）、`Placed`（当前屏幕/滚动回滚中的引用）、`Animated`（帧缓存/播放状态）。离开视口时可停止播放，离开会话或收到 reset/alternate screen 切换时删除 placement；图片无引用时再释放 bytes。Kitty 的 quota 会淘汰无 placement 的旧图片，故 image ID 不能被永久假定有效，必要时使用 query/ack 后重传。

## 6. 替代机制

- **Unicode placeholder**：kitty 的 `U+10EEEE` 不是另一种图片格式，而是让图片坐标进入普通文本流的定位机制。适合 TUI 滚动、tmux、vim；不适合要求任意终端兼容或独立帧控制的场景。见 [kitty placeholders](https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders)。
- **OSC 1337**：iTerm2 私有 File 协议，文件级传输，GIF 可由 iTerm2 原生播放；没有 kitty 式帧 API。见 [iTerm2 docs](https://iterm2.com/documentation-images.html)。
- **半块/彩色 Unicode**：用 `▀`/`▄`、braille 或 ANSI 真彩色把像素降采样成文本。兼容面最大、可由 Bingo 自己控制每帧，但带宽和 CPU 可能随帧数上升，画质与字符宽高比受限。
- **纯文本/静态首帧**：最可靠回退；为动图保留 alt 文本、帧数/时长摘要，避免用户只看到空白。

## 7. 对当前 Bingo Rust TUI 的分层建议（不改代码）

### 保持的边界

当前 `crates/bingo-surface-tui/src/graphics/` 已将 `kitty`、`placeholders`、`probe`、`tmux`、`stored`、`decoded`、`picture` 分开；`Graphics::from` 以主动 probe 和已知 placeholder 能力决定是否启用，而不是相信 `$TERM`。这与协议现实一致，应保持：

1. **SDK/core**：只保存图片内容与语义（当前 ADR-0040 的 `Image`/`ContentPart`）；不保存终端 image ID、placement、帧计时器或 ANSI/APC 字节。
2. **TUI graphics adapter**：把已解码图像映射到某个协议，维护终端侧资源、预算、取消和重放；Kitty 静态与动画共用 image ID/placeholder 资源模型。
3. **transport**：裸 PTY、tmux passthrough、未来 screen/SSH 路径独立；transport 失败应降级而非污染会话事实。
4. **rendering**：普通 frame 只产生 placeholder cells/回退文本；动画调度器在 adapter 层触发帧更新，不能让 `SessionState::apply` 变成时钟驱动。

### 建议的动图策略

- 首版：仅支持 kitty 动画的**能力探测后 opt-in**；默认静态首帧或低帧率预览。未确认完整动画 conformance 的 WezTerm/Ghostty/Konsole 不自动开启协议动画。
- 探测：区分“kitty APC query 成功”“placeholder 正确实现”“animation command 可用”三个事实；不要把一个布尔值复用于三者。若动画 query 不可靠，发送极小测试动画并在超时后删除，或采用静态策略。
- 调度：限帧（例如 10–15 fps 的产品预算，而非协议要求）、限单图总帧/像素/内存；背压时保留最新帧；用户滚动离开、窗口失焦、暂停 TUI 或进入 pager 时停止终端驱动播放。
- 重绘：resize、alternate-screen 切换、clear/reset、tmux attach/detach、terminal capability 变化都视为失效边界，停止旧动画、清理可见 placement、重新上传/放置。
- 回退：kitty 不可达 → 静态首帧；所有图形协议不可达 → `[animated image: N frames, duration]` 文本卡片。不能用 Sixel 或 iTerm2 GIF 作为跨终端“等价动画”保证。
- 测试：除纯函数/协议字节 fixture 外，增加真实 PTY smoke matrix：kitty bare、kitty+tmux passthrough、WezTerm static、iTerm2 GIF；记录终端版本、tmux 版本、SSH 路径、resize/scroll/clear 行为。`TestBackend` 只能验证 placeholder 文本，不足以证明 GPU 图像或动图播放。

## 未确认事项与研究边界

- 本文没有在每个目标终端的每个发行版版本上运行 live matrix；Ghostty、Konsole、WezTerm 的 kitty 动画完整度尤其不能从“支持 kitty 图形”列表推出。
- iTerm2 官方页明确 GIF，但未承诺 APNG；APNG 是否播放、循环/透明度细节需按 macOS/iTerm2 版本实测。
- Windows Terminal、macOS Terminal、foot、Alacritty 的图形/动图能力在本次一手资料检索中未获得足够明确的官方协议承诺，故表中保守标为未确认，而非绝对断言“不支持”。
- tmux passthrough 的精确版本矩阵、GNU screen 的 DCS 行为、SSH 中不同压缩/RTT 的帧率曲线需单独搭建可重复实验；本文只给出协议层风险。
- Kitty 官方协议允许动画，但具体实现的磁盘缓存、quota、GPU 上传和后台窗口节流属于终端实现细节，不能作为跨终端契约。
