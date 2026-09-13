# Channels

A session in a chat thread. One concurrent surface holds an adapter per
platform, and each chat — or each topic thread inside one — is one session,
keyed `<adapter>/<chat>[/<thread>]` and reopened by that key for as long as the
chat lives. It is a client of the one event stream like every other surface:
what a person sees in the chat is derived from the same frames a terminal folds.

## Configuring one

Under `channels`, one entry per adapter. Two exist: `feishu`, the first real
platform, and `loopback`, the in-process one the contract is tested against.

```jsonc
{
  "channels": {
    "feishu": { "appId": "cli_…", "access": { "admins": ["ou_…"] } },
    "coalesce": { "minChars": 48, "intervalMs": 700 }
  }
}
```

- `channels.feishu.appId` — the app id, which is public. The **secret never
  lives in a settings file**: it comes from `BINGO_FEISHU_APP_SECRET`, else the
  credential store, because the project layer is a committed file. `appId` may
  also come from `BINGO_FEISHU_APP_ID`.
- `base` (`channels.feishu.base`) — where the API lives, for a tenant that is
  not on the default host.
- `coalesce` — how often a streaming answer is redrawn: `minChars`
  (48) new characters worth a redraw, `intervalMs` (700) the longest anyone
  waits to see something new.
- `channels.loopback` — a platform made of settings, for tests and for seeing
  the shape of the wire: `peer` is a `host:port` to speak NDJSON with, and
  `edits`, `buttons`, `typing`, `threads`, `files`, `acknowledge` turn each
  mechanism off, `mention` is the word a group message must carry (`@bingo`),
  `maxText` and `maxActions` are the platform's limits.

`bingo --channels <adapter>[=<value>]` says the same thing for one run.

## Who may speak

Each adapter takes an `access` block — `channels.feishu.access` — which is a
pure policy, read where a mention alone used to decide:

- `admins` — principals who pass every rule but `off`.
- `direct` and `group` — one rule each, for private chats and for the rest.
- `rules` — one rule per chat id, replacing `group` for that chat entirely.

A rule is `policy`, `list` and `mention`. `policy` is `open` (anyone here),
`allowlist` (only the `list`), `blocklist` (anyone but the `list`), `admins`
(only an admin) or `off` (nobody, admins included). `mention: false` engages a
group on every message instead of only when the bot is addressed; in a direct
chat it means nothing, since a direct message is always addressed. The default
is `open` with `mention: true`, so a bot upgraded into a policy does not go
quiet. A refused message is dropped without a word.

## What a message becomes

A reply that answers the open question is that answer; anything else is the
next thing to work on, submitted as a prompt that wakes the session. So `/`
commands and `!` shell lines work in a chat exactly as they do anywhere: the
session actor parses them, not this surface.

A question reaches the chat as buttons where the platform has them and as a
numbered list where it does not — the key a button carries is the number a
person types, so both rungs mean the same thing, and words of your own are an
answer where the question takes them. While a turn started by a message runs,
the message is marked (`Typing` on Feishu), and the mark is taken off when the
turn ends — replaced by `CrossMark` if it failed.

## What a message carries

Pictures in a message become images of the turn, the way a paste does anywhere.
Every other attachment lands on disk at the edge and becomes a path in the
words: `<data>/channels/feishu/attachments/<message id>/<name>`, with one line
`[file: <name> → <path>]` added to the text. Read it with the fs tools already
in hand — nothing here transcribes audio or video. A `text/plain` or
`text/markdown` file of at most 100 KiB is also inlined under its line, because
making the model read back a path it was handed one line earlier is a round
trip for nothing. Anything over 30 MiB is not kept, entries older than a
fortnight are swept, and an attachment that will not fetch is dropped with a
warning while the words still go.

Rich messages are all heard: a `post` becomes markdown (links, fences, `@name`,
`---`), a card becomes its title and lines, a shared chat becomes its name and
id, a forward becomes its title and the messages inside it, and a type nothing
knows becomes `[<type>]`.

## Sending a file out

`SendFile` is the one way a file leaves this machine for a chat — never a tag
in prose. `path` (absolute, or relative to the working directory) and an
optional `caption`. It appears in the conversation, under the message being
replied to where the chat threads. Use it for something a person should open or
keep; a session that does not live in a chat is refused in words, and 30 MiB is
the ceiling. It is gated on the path, so a permission rule can allow it.

## Running one

- `bingo channels` — listen on the configured channels and nothing else. No
  terminal is taken.
- `bingo channels add <adapter>` — ask for the app id and the secret together
  and write each where the next run reads it.
- `bingo channels secret <adapter>` — paste the secret on its own, for rotation.
- `bingo gateway start | stop | restart | status | logs | doctor | install |
  uninstall` — the same work resident: one gateway per data directory, ended by
  a signal rather than by a terminal, and `install` keeps it alive across
  logins through launchd or systemd. `doctor` reads the settings, the
  credentials and every lock and says what to do; `--fix` removes exactly the
  locks whose process is gone.
