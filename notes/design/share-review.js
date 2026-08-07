#!/usr/bin/env node
// bingo share 视觉对照评审断言（PRD v1.1 V3 证据 · 事实源 = share-page-template.html）
// 用法：node share-review.mjs <生成的 share.html>
// 全部 PASS 退出码 0；任一 FAIL 退出码 1 并打印差异项。
// 说明：数据空态视图（team/dm/channel 无数据）时对应元素只出现在 CSS 中——
// 本脚本对「CSS 类存在性」与「渲染元素存在性」分别断言，空态不影响 CSS 断言。
'use strict';
const fs = require('fs');
const path = require('path');

const file = process.argv[2];
if (!file) { console.error('usage: node share-review.mjs <share.html>'); process.exit(2); }
const html = fs.readFileSync(file, 'utf8');

// 渲染元素断言（数据存在时）与 CSS 断言（始终）
const checks = [
  // —— 令牌（CSS）——
  ['token --bg:#FAFAF7',          /--bg:\s*#FAFAF7/.test(html)],
  ['token --accent:#B05227',      /--accent:\s*#B05227/.test(html)],
  ['token --accent-border:#E7C4B2',/--accent-border:\s*#E7C4B2/.test(html)],
  ['token --font-sans 存在',      /--font-sans:/.test(html)],
  ['无暗色残留 #0C0C0E/#0B0B0D',  !/#0C0C0E|#0B0B0D/.test(html)],
  // —— 消息结构（CSS 类 + 渲染）——
  ['dec 装饰列 CSS',              /\.dec\b/.test(html)],
  ['anchor 锚点 CSS',             /\.anchor\b/.test(html)],
  ['line 竖线 CSS',               /\.line\b/.test(html)],
  ['限宽三档 CSS',                /\.w-sm\b/.test(html) && /\.w-md\b/.test(html) && /\.w-lg\b/.test(html)],
  ['助手卡 .card CSS',            /\.card\b/.test(html)],
  ['元信息 .meta CSS',            /\.meta\b/.test(html)],
  ['meta 小号大写',               /text-transform:\s*uppercase/.test(html)],
  ['工具两段式 CSS',              /\.tool-title\b/.test(html) && /\.tool-result\b/.test(html)],
  ['tool-args 摘要 CSS',          /\.t-args\b/.test(html)],
  ['图片块 .img-block CSS',       /\.img-block\b/.test(html)],
  // —— 保留类（CSS 类存在性，空态不受影响）——
  ['roster CSS',                  /\.roster\b/.test(html) && /\.r-row\b/.test(html)],
  ['dm CSS',                      /\.dm-agent\b/.test(html) && /\.dm-row\b/.test(html)],
  ['ch CSS',                      /\.ch-block\b/.test(html) && /\.ch-row\b/.test(html)],
  ['tabs CSS',                    /\.tabs\b/.test(html) && /data-tab/.test(html)],
  ['data-view 面板',              /data-view=/.test(html)],
  ['empty 空态 CSS',              /\.empty\b/.test(html)],
  ['打印块',                      /@media print/.test(html)],
  ['reduced-motion',              /prefers-reduced-motion/.test(html)],
  ['skip 跳转链接',               /class="skip"/.test(html)],
  // —— 语言（P2-1 决策：跟模板走英文）——
  ['lang="en"',                   /<html lang="en"/.test(html)],
  // —— 转义抽查（数据区无未转义注入；剥离已知脚本块后断言）——
  ['无 <script> 数据注入',        !/<script/.test(html.replace(/<script>[\s\S]*?<\/script>/g, ''))],
  ['无 onerror= 注入',            !/onerror\s*=/.test(html.replace(/<script>[\s\S]*?<\/script>/g, ''))],
  ['无外链 http(s)',              !/src="http|href="http|<link|@import|url\(http/.test(html)],
  // —— 渲染元素（有数据时）——
  ['消息含 .dec + .anchor',       /<article class="msg[\s\S]*?class="dec"/.test(html) || /class="dec"/.test(html.replace(/<style>[\s\S]*?<\/style>/g, ''))],
  ['锚点 id="msg-"',              /id="msg-\d+"/.test(html)],
  ['图片 data: URI 渲染',         /<img src="data:image\//.test(html)],
  // —— a11y：aria-hidden 不得遮蔽可聚焦锚点（uiux-site 发现，模板 v2.2 已修）——
  ['dec 无 aria-hidden（聚焦锚点可见）', !/<div class="dec"\s+aria-hidden=/.test(html)],
  ['line 承载 aria-hidden',       /<span class="line"\s+aria-hidden="true"/.test(html) || /class="line" aria-hidden="true"/.test(html)],
  // —— 打印/JS 渐进增强（脚本本体存在即可）——
  ['JS 渐进增强存在',             /<script>/.test(html)],
];

let pass = 0, fail = 0;
for (const [name, ok] of checks) {
  if (ok) pass++;
  else { fail++; console.log('FAIL: ' + name); }
}
console.log('---');
console.log(`PASS ${pass} / FAIL ${fail} / ${checks.length}`);
process.exit(fail === 0 ? 0 : 1);
