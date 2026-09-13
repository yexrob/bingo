# Agents

An agent is a child session: its own journal, its own transcript, the same
working directory, gated in itself against its own rules. It sees nothing of
this conversation and cannot put a question to the person, so a prompt has to
stand on its own — what to do, what it may assume, what to report back.

## `SpawnAgent`

`prompt` is the task. Everything else is optional: `agent` names a definition,
`name` what to call this one, `model`, `provider`, `thinking` (`off` or a
level) and `tools` staff it, and two words decide what becomes of its answer.

- **Background** (the default) — the call returns `{"name": …, "session": …}`
  at once and the agent's reply arrives later as a message from that name.
  Carry on with whatever does not depend on it, or end your turn: the reply
  wakes you and there is nothing to poll. Until it comes the agent is still
  running — say so if asked, and never guess at what it will say.
- **`background: false`** — the call waits and returns the agent's final text.
  For when you cannot go on without it. A turn that failed or was cut short
  comes back as an error, not as an answer.
- **`standby: true`** — the prompt is a standing brief, held unread until
  something else wakes the agent; no turn opens and nothing is reported back
  here when its turns end. `standby` with `background: false` is refused: it
  would wait forever.

A name is one word, no slashes, never `parent` and never starting with `#`; a
name a sibling holds gets `-2`, `-3`. A child is never offered `SpawnAgent`
(the depth limit is one) or `AskUserQuestion` (there is nobody to ask); it
inherits every other tool this session has unless `tools` says otherwise.

## Working with several

For agents that report back one by one, spawn and read their replies. When
they are to work **with each other**, seat them instead of tasking them:
`OpenRoom` naming the roles — and `parent` among them if you want to read the
room yourself — one `standby: true` spawn per role, then a single
`SendMessage` to `#room` carrying the kickoff and naming with `@name` whoever
it is for. Writing to them one at a time makes you the switchboard every step
has to pass back through.

A brief that tells an agent to stand by must say what to stand by for:
everything reaching it from elsewhere is labelled `[from <name>]` or
`[in #<room>]`, and an unlabelled line is the person it works for, or you,
writing directly.

## `SendMessage`

`to` is an agent you started, a teammate beside you, `parent`, or a room's
`#name`. `text` is what to say. The message arrives whatever the target is
doing: an idle session takes it up as its next turn, a working one reads it
mid-run.

**A direct message asks for nothing back**: it is read, and that is all it is
owed. When you need an answer, ask in a room with `@name` — a mention is owed
an answer, a message is not.

A room hands a post back when somebody spoke while you were writing. Read what
they said, then post the same words again with `again: true` and no `text`:
the draft is still in your own bounced call, so repeating it costs nothing.
`again` is only for a room that bounced you, and never beside `text`.

## `ListAgents`, `ListModels`, `SetThinking`

`ListAgents` names who you can write to: the ones you started, and — under
`Beside you` — the ones the same agent started alongside you. Each row is an
`agent`, a `session` and a `state`, `busy` or `idle`.

`ListModels` lists the providers this build has, whether each is signed in,
and the models it serves with their context window, output cap and whether
they reason or read images. Its facts come from a snapshot embedded in the
build, never a live call, so a model listed without them is one the snapshot
does not know — which says nothing about whether it works. Call it before
staffing an agent on a `provider` or `model` you are unsure of.

`SetThinking` moves how hard a session thinks — this one, or a sub-agent named
with `agent`. It lands on the next turn and never inside the one running now,
so set your own level before the work you want it for.

## What a child is told

Every child's system prompt opens with the same note, so you can rely on it:
its final text is the answer to the call that started it; `SendMessage(to:
"parent")` is for being blocked or for something that changes what the parent
is doing, not for progress; teammates are written to directly rather than
routed through the parent; its turn ends when it stops calling tools, and only
a message opens another one.

## Definitions

`<name>.md` under `.bingo/agents` at each level from the working directory up,
then `<config_dir>/agents` — the project speaks before the person, the other
way round from skills, and the first definition of a name wins. Optional
frontmatter: `name`, `description`, `model`, `provider`, `thinking`, `tools`;
the body is the child's system prompt, after the note. Files are read afresh
every time, so an edit needs no restart.

## The team

`.bingo/team.json`, the nearest at or above the working directory, declares
`roles` — each a `name`, and any of `agent` (a definition), `system`, `model`,
`provider`, `tools`. When a root session opens in that project the roles are
seated as its children, once: one already there is reopened, which is how a
role keeps its memory across a resumed root, and creating one opens no turn.
A `norms` file, or `team-norms.md` beside the team file, is prepended to every
role's system prompt. The file's `rooms` key belongs to rooms.

`/agents` is the roster of this session, `/team` what the project declared and
which roles are seated. In the composer, `@name rest` sends that line to the
child of that name instead of to the session it was typed in.
