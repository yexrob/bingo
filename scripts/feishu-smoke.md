# Feishu live smoke (M13)

The one thing the test suite cannot do: talk to Feishu. Everything else about
the channel surface is proven offline against the loopback adapter and byte
fixtures (`cargo test -p bingo-channels`, `cargo test -p bingo --test channels`);
this is the half-hour that decides whether the wire notes in ADR-0016 §6 are
still true.

Needs: a Feishu tenant you can create an app in, and about thirty minutes.

## 1. The app

Feishu's long connection is offered to **self-built** (企业自建) apps only.
A marketplace app cannot use it, and Lark International is unverified — if the
smoke is run there, say so in the result, because that is the open question
ADR-0016 records.

1. <https://open.feishu.cn/app> → 创建企业自建应用. Name it anything.
2. **凭证与基础信息**: copy `App ID` (public, `cli_…`) and `App Secret`.
3. **添加应用能力** → 机器人. Without this the app has no chat to be in.
4. **权限管理**, add exactly these and no more:
   - `im:message.p2p_msg` — direct messages to the bot.
   - `im:message.group_at_msg` — group messages that @ the bot.
   - `im:message:send_as_bot` — sending.
   - `cardkit:card:write` — the streaming card.

   The `im:message.p2p_msg:readonly` / `im:message.group_msg` pair are
   *sensitive* permissions needing tenant review; they read every message in
   every chat and this surface does not want them.
5. **事件与回调 → 事件配置**: 订阅方式 = **使用长连接接收事件**. Add the event
   `接收消息 v2.0` (`im.message.receive_v1`).
6. **事件与回调 → 回调配置**: 订阅方式 = **使用长连接接收回调**. Add
   `卡片回传交互` (`card.action.trigger`).

   ⚠️ **The console will not save either switch unless a client is already
   connected.** Start bingo first (step 2 below), leave it running, then set
   the long-connection mode and save. If the page refuses, that is what it is
   telling you.
7. **版本管理与发布** → create a version and publish it. An unpublished app
   receives nothing.

## 2. Run it

```fish
set -x BINGO_FEISHU_APP_ID    cli_xxxxxxxxxxxxxxxx
set -x BINGO_FEISHU_APP_SECRET  <the secret>
bingo channels --channels feishu
```

or, to keep the terminal too — the channel surface is `Concurrent`, so it runs
beside whatever owns the terminal:

```fish
bingo --channels feishu          # a TUI session, with the chat listening
bingo serve --stdio --channels feishu
```

Settings instead of a flag, in `~/.bingo/settings.json`:

```json
{ "channels": { "feishu": { "appId": "cli_xxxxxxxxxxxxxxxx" } } }
```

The **secret never goes in a settings file** — the project layer is committed,
and a committed secret is a rotated secret. `BINGO_FEISHU_APP_SECRET` is the
only place it is read from.

Expect: no output, and no error. A refusal names what is missing.

One process per app: the long connection is cluster-mode, so a second bingo on
the same credentials would take a random half of the events and neither would
know. The second one refuses and names the lock file under
`~/.bingo/data/channels/`.

## 3. The smoke

Tick each line, and paste what you saw.

- [ ] **P2P hello.** Find the bot under 通讯录 → 我的应用, say `hello`.
      A message appears and fills in as the model answers. It is **one**
      message being edited, not a stream of new ones.
- [ ] **Streamed answer.** Ask for something long (`explain what this repo
      does`). The card grows in place, in sentences rather than in
      characters. Nothing is repeated, and the final text is the whole answer.
      Watch for a card that *stops* growing but never finishes: that is the
      ten-minute streaming close, and it means the re-arm is wrong.
- [ ] **Group @mention.** Add the bot to a group. Say something without
      mentioning it — nothing happens, and no session is created. Say
      `@bingo run the tests` — it answers.
- [ ] **A thread.** In a topic group (话题群), reply inside a topic. The answer
      hangs under that topic, and it is a session of its own — its key is
      `feishu/<chat>/<thread>`, so its transcript is separate.
- [ ] **Permission card.** Ask for something that needs a permission
      (`write a file called notes.txt`). A **separate** card appears with
      `Allow once` / `Deny` buttons — never buttons on the streaming card.
      Press one. The buttons disappear and the card says what was decided,
      within three seconds. If a red 200341 toast appears instead, the ack is
      too slow.
- [ ] **The two-surface race.** Ask for a permission again, and this time
      answer it in the TUI (`bingo --channels feishu` in another terminal, or
      `/approve` wherever you are). The card in Feishu loses its buttons and
      reads `approved in the TUI`.
- [ ] **A dropped connection.** Turn the wifi off for a minute and back on.
      Within about two minutes the bot answers again; nothing is said twice.
- [ ] **Rate limits.** Ask two long questions in one chat back to back. Both
      answer; neither stalls. A dropped frame is invisible — the next one
      carries the whole text anyway.

## 4. What to write down

- The tenant: 飞书 (open.feishu.cn) or Lark International (open.larksuite.com).
- Anything the console would not let you save, and what unstuck it.
- Any error code that appeared (`230020`, `230072`, `99991400`, `200341`,
  `1000040350`) and what was happening at the time — these are the numbers
  ADR-0016 §6 names, and a new one is a change to the wire notes.
- Whether the ten-minute streaming close was ever reached.
