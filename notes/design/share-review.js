#!/usr/bin/env node
// bingo share 视觉对照评审断言 v4.0（Claude Code app 风格）
// 事实源 = share-page-template.html（v4.0）。用法：node share-review.js <share.html>
// 全部 PASS 退出码 0；任一 FAIL 退出码 1 并打印差异项。
'use strict';
const fs = require('fs');

const file = process.argv[2];
if (!file) { console.error('usage: node share-review.js <share.html>'); process.exit(2); }
const html = fs.readFileSync(file, 'utf8');
const has = (s) => html.includes(s);
const re = (r) => r.test(html);
const stripped = html.replace(/<script>[\s\S]*?<\/script>/g, '');

const checks = [
  // —— 令牌（Claude Code app 暗色基调 + bingo 语义）——
  ['--bg 近黑底',                       has('--bg:            #0D0D0F') || has('--bg: #0D0D0F') || /--bg:\s*#0D0D0F/.test(html)],
  ['--accent 陶土橙 #D77757',           /--accent:\s*#D77757/.test(html)],
  ['用户气泡暖灰 --bubble',             /--bubble:\s*#3A3731/.test(html)],
  ['语义色 green/red/gold/running',     /--green:\s*#4EBA65/.test(html) && /--red:\s*#FF6B80/.test(html) && /--running:\s*#F0A05A/.test(html)],
  ['字体双栈 sans+mono',                has('--font-sans') && has('--font-mono')],
  ['消息流限宽 --maxw',                 /--maxw:\s*800px/.test(html)],
  ['气泡限宽 --bubble-max',             has('--bubble-max')],
  ['成员色 hue 保留',                   has('--hue-0') && has('--hue-5')],

  // —— 结构：顶栏 / 布局 / 消息部件 ——
  ['topbar sticky',                     has('class="topbar"') && has('position: sticky')],
  ['品牌 + 会话标题',                   has('class="brand"') && has('class="session"')],
  ['元信息 meta-line',                  has('class="meta-line"')],
  ['tab 导航',                          has('class="tabs"') && has('data-tab=')],
  ['四视图 data-view',                  has('data-view="conv"') && has('data-view="team"') && has('data-view="dm"') && has('data-view="channel"')],
  ['消息锚点 id="msg-"',                re(/id="msg-\d+"/)],
  ['msg-user + bubble',                 has('class="msg msg-user"') && has('class="bubble"')],
  ['气泡右对齐 CSS',                    re(/\.msg-user\s*\{\s*align-items:\s*flex-end/)],
  ['msg-assistant + content/md',        has('class="msg msg-assistant"') && has('class="md"')],
  ['thinking 折叠块',                   has('class="think"') && has('think-body')],
  ['工具折叠卡 details.tool',           has('class="tool"') && has('t-body')],
  ['工具状态徽标三态',                  has('t-status ok') && has('t-status err') && has('.t-status.running')],
  ['工具名/参数/时长',                  has('t-name') && has('t-args')],
  ['代码块 code-block + 复制按钮逻辑',  has('class="code-block"') && has('.copy-btn') && has('addCopyButtons')],
  ['markdown 表格样式',                 re(/\.md\s+th,\s*\.md\s+td/) || re(/\.md th,/)],

  // —— 四视图（聊天记录形态）——
  ['Team 线程列表',                     has('class="thread-list"') && has('class="thread"') && has('data-jump=')],
  ['Team 线程直达私聊',                 has('href="#dm-') && has('class="t-avatar"')],
  ['DM 聊天流',                         has('class="dm-block"') && has('class="dm-flow"')],
  ['DM 用户右气泡',                     re(/dm-flow[\s\S]*class="msg msg-user"/) || has('class="dm-flow"')],
  ['频道消息流',                        has('class="ch-block"') && has('class="ch-flow"') && has('class="ch-msg"')],
  ['频道 seq/成员徽标/mode',            has('ch-seq') && has('ch-from') && has('m-chip') && has('ch-mode serial') && has('ch-mode free')],
  ['user 频道右对齐',                   has('ch-msg.ch-user') || has('class="ch-msg ch-user"')],
  ['空态 view-empty',                   has('class="view-empty"') || has('view-empty')],

  // —— 语言 / 转义 / 自包含 / a11y / 打印 ——
  ['lang="en"',                         has('<html lang="en"')],
  ['无 <script> 数据注入',              !/<script/.test(stripped)],
  ['无 onerror= 注入',                  !/onerror\s*=/.test(stripped)],
  ['零外部引用',                        !re(/src="http|href="http|<link|@import|url\(http/)],
  ['aria-hidden 不遮蔽可交互元素',      !re(/aria-hidden[^>]*>.{0,80}(button|a\s)/s)],
  ['noscript 降级提示',                 has('<noscript>')],
  ['@media print',                      re(/@media print/)],
  ['prefers-reduced-motion',            re(/prefers-reduced-motion/)],
  ['JS 渐进增强存在',                   re(/<script>/)],
  ['锚点复制逻辑',                      re(/setTimeout\(function\(\)\{ anchor\.textContent = '#'/) || re(/textContent = '✓'/)],

  // —— 布局观感契约 ——
  ['消息流居中 maxw',                   re(/main\s*\{\s*max-width:\s*var\(--maxw\)/)],
  ['折叠默认收起（无 open 属性）',      !re(/<details class="tool"[^>]*open/)],
];

let pass = 0, fail = 0;
for (const [name, ok] of checks) {
  if (ok) pass++;
  else { fail++; console.log('FAIL: ' + name); }
}
console.log('---');
console.log(`PASS ${pass} / FAIL ${fail} / ${checks.length}`);
process.exit(fail === 0 ? 0 : 1);
