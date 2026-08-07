#!/usr/bin/env node
// bingo share 视觉对照评审断言 v3.0（opencode 完全复刻版）
// 事实源 = share-page-template.html（v3.0）。用法：node share-review.js <share.html>
// 全部 PASS 退出码 0；任一 FAIL 退出码 1 并打印差异项。
// 说明：空态视图（team/dm/channel 无数据）时对应元素只出现在 CSS——脚本对
// 「CSS/契约存在性」与「渲染元素存在性」分别断言，空态不影响前者。
'use strict';
const fs = require('fs');

const file = process.argv[2];
if (!file) { console.error('usage: node share-review.js <share.html>'); process.exit(2); }
const html = fs.readFileSync(file, 'utf8');
const has = (s) => html.includes(s);
const re = (r) => r.test(html);

const checks = [
  // —— 令牌（opencode / Starlight）——
  ['--sl-color-bg-surface',             has('--sl-color-bg-surface')],
  ['--sl-color-divider',                has('--sl-color-divider')],
  ['--sl-color-text-secondary',         has('--sl-color-text-secondary')],
  ['--sl-color-text-dimmed',            has('--sl-color-text-dimmed')],
  ['--color-background',                has('--color-background')],
  ['--sm/md/lg-tool-width 三档',        has('--sm-tool-width') && has('--md-tool-width') && has('--lg-tool-width')],
  ['--term-icon 终端三点图标',          has('--term-icon')],
  ['暗色 prefers-color-scheme',         re(/@media \(prefers-color-scheme: dark\)/)],

  // —— 结构：opencode data-component / data-slot（契约存在性）——
  ['data-component=share',              has('data-component="share"')],
  ['header 部件',                       has('data-component="header"') && has('data-component="header-title"') && has('data-component="header-stats"')],
  ['part 部件',                         has('data-component="part"')],
  ['decoration + anchor/bar/tooltip',   has('data-component="decoration"') && has('data-slot="anchor"') && has('data-slot="bar"') && has('data-slot="tooltip"')],
  ['content 部件',                      has('data-component="content"')],
  ['user-text',                         has('data-component="user-text"')],
  ['assistant-text + markdown',         has('data-component="assistant-text"') && has('data-slot="markdown"')],
  ['assistant-reasoning',               has('data-component="assistant-reasoning"')],
  ['step-start',                        has('data-component="step-start"') && has('data-slot="provider"') && has('data-slot="model"')],
  ['tool-title (name/target)',          has('data-component="tool-title"') && has('data-slot="name"') && has('data-slot="target"')],
  ['tool-args 网格',                    has('data-component="tool-args"')],
  ['tool-result',                       has('data-component="tool-result"')],
  ['content 五件套',                    has('data-component="content-markdown"') && has('data-component="content-text"') && has('data-component="content-code"') && has('data-component="content-error"') && has('data-component="content-bash"')],
  ['copy-button',                       has('data-component="copy-button"') && has('class="copy-root"')],
  ['模块根类齐全',                      has('class="part-root"') && has('class="cm-root"') && has('class="ct-root"') && has('class="cc-root"') && has('class="ce-root"') && has('class="cb-root"')],
  ['scroll-button',                     has('class="scroll-button"')],

  // —— bingo 四视图适配（聊天记录样式）——
  ['四视图 data-view',                  has('data-view="conv"') && has('data-view="team"') && has('data-view="dm"') && has('data-view="channel"')],
  ['视图标题 view-title',               has('data-component="view-title"')],
  ['Team 线程列表',                     has('class="thread-list"') && has('thread-row') && has('data-jump=')],
  ['Team 线程直达私聊锚点',             has('href="#dm-')],
  ['DM 聊天流 dm-msg',                  has('class="dm-msg"') && has('data-type="thread"')],
  ['DM user 靠右',                      has('class="dm-msg dm-user"')],
  ['频道 part 消息流',                  has('class="ch-stream"') && has('data-component="channel"')],
  ['频道 seq 保留',                     has('class="ch-row-seq"') && has('#0001')],
  ['频道成员徽标 + mode',               has('class="m-chip"') && has('ch-mode serial') && has('ch-mode free')],
  ['tab 导航',                          has('data-component="tabs"') && has('data-tab=')],
  ['成员色 hue 保留',                   has('--hue-0') && has('--hue-5')],

  // —— 渲染元素（有数据时）——
  ['锚点 id="msg-"',                    re(/id="msg-\d+"/)],
  ['消息渲染 part-root',                re(/class="part-root"/)],
  ['bash Shell 头',                     re(/data-slot="header"><span>Shell<\/span>/)],
  ['错误 pre 红标',                     re(/data-color="red" data-marker="label"/)],

  // —— 语言 / 转义 / 自包含 / a11y ——
  ['lang="en"',                         has('<html lang="en"')],
  ['无 <script> 数据注入',              !/<script/.test(html.replace(/<script>[\s\S]*?<\/script>/g, ''))],
  ['无 onerror= 注入',                  !/onerror\s*=/.test(html.replace(/<script>[\s\S]*?<\/script>/g, ''))],
  ['零外部引用',                        !re(/src="http|href="http|<link|@import|url\(http/)],
  ['aria-hidden 不遮蔽锚点',            !re(/<div data-component="decoration"[^>]*aria-hidden/)],
  ['noscript 降级提示',                 has('<noscript>')],

  // —— 打印 / 减弱动效 ——
  ['@media print',                      re(/@media print/)],
  ['prefers-reduced-motion',            re(/prefers-reduced-motion/)],
  ['JS 渐进增强存在',                   re(/<script>/)],

  // —— opencode 交互契约（JS 行为，静态存在性）——
  ['锚点复制逻辑',                      re(/data-status['\"]\s*,\s*['\"]copied/) || re(/setAttribute\('data-status', 'copied'\)/)],
  ['复制按钮逻辑',                      re(/data-copied/)],
  ['展开按钮逻辑',                      re(/data-more/)],
];

let pass = 0, fail = 0;
for (const [name, ok] of checks) {
  if (ok) pass++;
  else { fail++; console.log('FAIL: ' + name); }
}
console.log('---');
console.log(`PASS ${pass} / FAIL ${fail} / ${checks.length}`);
process.exit(fail === 0 ? 0 : 1);
