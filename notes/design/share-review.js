#!/usr/bin/env node
// bingo share visual-conformance review assertions v4.0 (Claude Code app style)
// Source of truth = share-page-template.html (v4.0). Usage: node share-review.js <share.html>
// All PASS → exit 0; any FAIL → exit 1 and print the failing items.
'use strict';
const fs = require('fs');

const file = process.argv[2];
if (!file) { console.error('usage: node share-review.js <share.html>'); process.exit(2); }
const html = fs.readFileSync(file, 'utf8');
const has = (s) => html.includes(s);
const re = (r) => r.test(html);
const stripped = html.replace(/<script>[\s\S]*?<\/script>/g, '');

const checks = [
  // —— tokens (Claude Code app dark palette + bingo semantics) ——
  ['--bg near-black background',        has('--bg:            #0D0D0F') || has('--bg: #0D0D0F') || /--bg:\s*#0D0D0F/.test(html)],
  ['--accent terracotta #D77757',       /--accent:\s*#D77757/.test(html)],
  ['warm-grey user bubble --bubble',    /--bubble:\s*#3A3731/.test(html)],
  ['semantic colors green/red/gold/running', /--green:\s*#4EBA65/.test(html) && /--red:\s*#FF6B80/.test(html) && /--running:\s*#F0A05A/.test(html)],
  ['dual font stacks sans+mono',        has('--font-sans') && has('--font-mono')],
  ['message-flow width limit --maxw',   /--maxw:\s*800px/.test(html)],
  ['bubble width limit --bubble-max',   has('--bubble-max')],
  ['member hues preserved',             has('--hue-0') && has('--hue-5')],

  // —— structure: topbar / layout / message parts ——
  ['topbar sticky',                     has('class="topbar"') && has('position: sticky')],
  ['brand + session title',             has('class="brand"') && has('class="session"')],
  ['meta-line',                         has('class="meta-line"')],
  ['tab navigation',                    has('class="tabs"') && has('data-tab=')],
  ['four views data-view',              has('data-view="conv"') && has('data-view="team"') && has('data-view="dm"') && has('data-view="channel"')],
  ['message anchors id="msg-"',         re(/id="msg-\d+"/)],
  ['msg-user + bubble',                 has('class="msg msg-user"') && has('class="bubble"')],
  ['bubble right-aligned CSS',          re(/\.msg-user\s*\{\s*align-items:\s*flex-end/)],
  ['msg-assistant + content/md',        has('class="msg msg-assistant"') && has('class="md"')],
  ['thinking collapsible block',        has('class="think"') && has('think-body')],
  ['tool collapsible card details.tool', has('class="tool"') && has('t-body')],
  ['tool status badge three states',    has('t-status ok') && has('t-status err') && has('.t-status.running')],
  ['tool name/args/duration',           has('t-name') && has('t-args')],
  ['code-block + copy-button logic',    has('class="code-block"') && has('.copy-btn') && has('addCopyButtons')],
  ['markdown table styles',             re(/\.md\s+th,\s*\.md\s+td/) || re(/\.md th,/)],

  // —— four views (chat-record shapes) ——
  ['Team thread list',                  has('class="thread-list"') && has('class="thread"') && has('data-jump=')],
  ['Team thread jumps to DM',           has('href="#dm-') && has('class="t-avatar"')],
  ['DM chat flow',                      has('class="dm-block"') && has('class="dm-flow"')],
  ['DM user right bubble',              re(/dm-flow[\s\S]*class="msg msg-user"/) || has('class="dm-flow"')],
  ['channel message flow',              has('class="ch-block"') && has('class="ch-flow"') && has('class="ch-msg"')],
  ['channel seq/member badge/mode',     has('ch-seq') && has('ch-from') && has('m-chip') && has('ch-mode serial') && has('ch-mode free')],
  ['user channel right-aligned',        has('ch-msg.ch-user') || has('class="ch-msg ch-user"')],
  ['empty state view-empty',            has('class="view-empty"') || has('view-empty')],

  // —— language / escaping / self-containment / a11y / print ——
  ['lang="en"',                         has('<html lang="en"')],
  ['no <script> data injection',        !/<script/.test(stripped)],
  ['no onerror= injection',             !/onerror\s*=/.test(stripped)],
  ['zero external references',          !re(/src="http|href="http|<link|@import|url\(http/)],
  ['aria-hidden does not hide interactive elements', !re(/aria-hidden[^>]*>.{0,80}(button|a\s)/s)],
  ['noscript fallback hint',            has('<noscript>')],
  ['@media print',                      re(/@media print/)],
  ['prefers-reduced-motion',            re(/prefers-reduced-motion/)],
  ['progressive-enhancement JS present', re(/<script>/)],
  ['anchor copy logic',                 re(/setTimeout\(function\(\)\{ anchor\.textContent = '#'/) || re(/textContent = '✓'/)],

  // —— layout/look contract ——
  ['message flow centered maxw',        re(/main\s*\{\s*max-width:\s*var\(--maxw\)/)],
  ['collapsed by default (no open attr)', !re(/<details class="tool"[^>]*open/)],
];

let pass = 0, fail = 0;
for (const [name, ok] of checks) {
  if (ok) pass++;
  else { fail++; console.log('FAIL: ' + name); }
}
console.log('---');
console.log(`PASS ${pass} / FAIL ${fail} / ${checks.length}`);
process.exit(fail === 0 ? 0 : 1);
