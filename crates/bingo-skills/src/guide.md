# Skills

A skill is a `SKILL.md` file: optional YAML frontmatter and a markdown body
that becomes a prompt. Each one is three things at once — a `/name` command, a
name the `Skill` tool can be called with, and a line in the system prompt
saying it exists.

## Where they are read from

In order, the first of a name winning:

1. `~/.bingo/skills/<name>/SKILL.md` — the person's own.
2. `.bingo/skills/<name>/SKILL.md`, from the working directory upwards to the
   filesystem root; the nearest wins.
3. What the binary ships: the `guide` map, and one `guide-<name>` page per
   loaded plugin. A skill on disk of the same name overrides one of these.

A directory with no `SKILL.md` in it is not a skill. A symlinked skill loads; a
dangling link is simply not there. A file that is edited, added or removed is
seen on the next look — no restart, and no cache to clear.

## Frontmatter

Read only when `---` is the file's first line:

- `name` — what the skill answers to; the directory's name when absent.
- `description` — what it is for, and its line in the system prompt; the body's
  first line when absent.
- `argument-hint` — what a client shows beside the name while completing.
- `arguments` — names for the positional arguments, `a b` or `[a, b]`.
- `allowed-tools`, `model` — recorded exactly as written, never enforced.

A key this plugin does not read (`when_to_use`, `license`, `metadata`, …) is
ignored rather than refused.

## The body

- `$ARGUMENTS` — everything typed after the name.
- `$1` … `$9` — its whitespace-separated words. **1-based**: `$1` is the first.
- `$name` — the word at the position `arguments` gave that name. A named
  placeholder with no word there becomes empty, while an indexed one is left as
  written.
- `${BINGO_SKILL_DIR}` — the directory holding this `SKILL.md`.

One left-to-right pass: a value that itself contains `$1` is inserted as text
and never expanded again, and `\$1` escapes nothing. Arguments no placeholder
asked for are appended as `ARGUMENTS: <text>` rather than dropped.

A skill that has a directory is handed over under one line — `Base directory
for this skill: <path>` — so a body may say `scripts/check.sh` and mean the
file beside its own `SKILL.md`. A skill in the binary has no directory and gets
no line.

## The two ways in

- The `Skill` tool takes `name` and optional `arguments`, and returns that
  skill's instructions as the tool result, expanded. It is read-only, and a
  name nobody has says which names exist.
- `/name args` expands the same way and becomes the turn's prompt. It is not
  instant: a skill opens a turn, so it queues behind a running one.

## Pages

Every loaded plugin may write pages about the nouns it owns, and each is a
skill in the binary named `guide-<name>`. `guide` is the map and ends with the
list of them; the `# Skills` block in the system prompt is the index. Read one
with `Skill guide-<name>` or `/guide-<name>`.
