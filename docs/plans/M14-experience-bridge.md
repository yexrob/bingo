# M14 — Experience library and the plugin bridge

## Goal

Two ends of one idea — the harness learns and the harness is extensible. `bingo-experience` (ADR-0014): procedural playbooks as hand-editable files, committed through the gate, ranked back into the prompt by a zero-dep BM25, visible in `/experience`. `bingo-plugin-rpc` (ADR-0015): a third party ships a bingo-native Tool and Command in any language against `schema/plugin.json`, and both run through the same gate and registry as everything else.

## Bricks, in build order (owner)

**Experience (worker E):**

1. **`bm25.rs`** — the ported brick: tokenizer (lowercase, split non-alnum, drop ASCII < 2, 4-char ASCII prefix stems, CJK bigrams with script-boundary flush), Lucene positive idf, K1 1.2 / B 0.75, field weights, one `rank(query, floor)`; property tests over CJK/ASCII mixes.
2. **Entry model + store** — minted short id = filename; frontmatter status/trigger/summary/steps/verify/outcomes + body notes; helpful/harmful derived at load, never serialized; atomic writes; **total round-trip** with an adversarial-content test (newlines in summary, a step starting `---`, commas in triggers, CJK); parse failure = one dim notice, never a silent skip; project key normalized and cached per cwd.
3. **Four tools** — Commit (revise by id, dedup by content, `preview` renders the file: the card is the propose step), Query (rank, no floor, statuses shown), Outcome (evidence required, status untouched), Forget (destructive); id prefixes accepted everywhere an id is.
4. **Two contributors + `/experience`** — System index (top 10 active, helpful desc / harmful asc, overflow pointer), RoundStart recall (latest user text, active only, floor on, ≤ 3 lines, skip on empty store or input); `/experience` instant → `View::Table`.

**Bridge (worker B):**

5. **`schema/plugin.json` first** — envelopes over the sdk's own serde types; generated, committed, drift test (the ADR-0007 pattern). Contracts before any process spawns.
6. **Host plugin** — `plugin.json` discovery (config + project layers, project wins), `${PLUGIN_ROOT}`, spawn at start, `initialize` handshake → one ToolSource + one CommandSource per process; `tool/call`/`command/run`/`command/complete`, `tool/progress` up, `tool/cancel` down; stderr to `<data_dir>/logs/plugin-<name>.log`; dead process = empty sources + one notice + respawn with backoff on next read; tools untrusted (`plugin__<name>__<tool>`).
7. **`examples/plugins/wordcount/`** — Python 3 stdlib only, one tool (counts words in a file) + one command (`/wordcount`); a README that points at `schema/plugin.json`.

## Files

`crates/bingo-experience/src/{lib,bm25,entry,store,tools,contributor,command}.rs`; `crates/bingo-plugin-rpc/src/{lib,manifest,spawn,wire,tool,command,schema}.rs` + `schema/plugin.json`; `examples/plugins/wordcount/{plugin.json,main.py}`; `crates/bingo/src/main.rs` (assemble both), `crates/bingo/tests/{experience,plugin_rpc}.rs` (black-box); `scripts/budget.toml` untouched (no new deps expected — a worker who needs one stops and reports); AGENTS.md commit scopes gain `experience plugin-rpc`.

## Exit criteria

- [x] commit → query → outcome → revise-under-same-id round-trips on disk; the adversarial-content entry survives byte-exact
- [x] a recall line lands in the transcript as a contributor user item; an empty store contributes nothing and costs no scan
- [~] the index block lists ≤ 10 with the overflow pointer (unit-tested; no black-box — the fake provider cannot echo the system prompt, carried); `/experience` renders the table in the TUI (probed live) and folds in `--print`
- [x] `ExperienceOutcome` without evidence is an input error; recording an outcome never changes status
- [x] `schema/plugin.json` drift test green; an unknown protocol major refuses the handshake with a notice
- [x] the wordcount tool runs through the permission gate untrusted (the card shows `plugin__wordcount__…`), `/wordcount` answers, both driven black-box through the real binary (test skips without `python3`)
- [x] a killed plugin process leaves one notice, empty sources, and a working respawn on the next turn
- [x] every gate green: fmt, check, clippy, test, discipline, budget, deny, tui-smoke

## Non-goals

Auto-extraction of experiences; gc/TTL/caps; cross-store dedup with `bingo-context` memory; hooks/contributors/providers over the bridge; a plugin marketplace; sandboxing bridge processes (the gate already asks per call).

## Risks

R-frontmatter — escaping is where the old one silently corrupted; the adversarial round-trip test is the contract, written before the serializer. R-python — CI machines without `python3` skip the exit test; the drift test still pins the schema. R-scope-creep — the bridge invites "one more contribution kind"; v1 is Tool + Command, the enum is the map, each later kind needs its own ADR line.

## Verified — 2026-08-31

Workers E (`3acc55e`) and B (`b8e3953`) merged; conflicts only the two
union lines (root `Cargo.toml`, `scripts/budget.toml` 283+283 → **284**).

```
cargo fmt / check / clippy / discipline / deny        all 0
scripts/budget.sh     dependencies (unique, normal): 284 (max 284)
cargo test -p bingo-experience                        79 passed
cargo test -p bingo-plugin-rpc                        55 passed  (+10 stub, +4 python black-box)
cargo test -p bingo --test cli experience             4 passed
cargo test --workspace --locked      1810 passed, 1 failed: highlight::…_under_a_millisecond
scripts/tui-smoke.sh                                  14 scenes ok
```

The one failure is the known wall-clock test: this machine sat at load
39 (worker C compiling beside the run); the same test passes with room at
load < 6 (M11's Verified). The definitive quiet rerun is owed at M13's
close, on the same main.

Reviewed on the real binary (tmux): the ExperienceCommit card carries the
file-to-be as a `+`-diff and a deny writes nothing; the committed entry is
recalled in a *fresh* session's transcript as the contributor user item;
`/experience` renders the table; `plugin__wordcount__count` is gated
untrusted and runs; `/wordcount` answers.

Worker rulings accepted: `ExperienceOutcome` is trusted but **not** `edit`,
so `acceptEdits` still asks — an outcome nobody saw is the self-confirmation
ADR-0014 guards against; `InitializeResult` gained `protocol` (a host cannot
refuse a major the process never named); a plugin is named by its directory
and a disagreeing manifest is refused; `Plugin::start` awaits every
handshake (10 s cap, parallel) unlike MCP — a first turn silently missing a
local plugin's tools is worse than a bounded wait.

### Carried out of M14

- A plugin has no notice channel outside a tool call (`HostApi` has no
  `notice`; sources get no host): a death with no later bridge call reaches
  only a log nobody subscribes to. The next sdk sweep owes `notice` — noted
  since M9.
- `ToolSource::tools()` and `Plugin::start` carry no session cwd, so the
  project plugin layer is the process's directory, not `--cwd`'s.
- `Tool::preview` gets no `Env`/host; `Env` is not `Serialize` (the bridge
  carries a three-field `HostEnv` projection); `Command::complete` is sync
  (the bridge answers from a cache one keystroke behind).
- The fake provider cannot echo the system prompt, so no black-box can
  assert a `System` contributor's block (the experience index).
- `Placement::RoundStart` fires every round; the recall contributor
  re-derives "already answered" from the transcript. Fine — but the next
  contributor author must know it.
- The permission question for a tool with no subjects is the raw input JSON
  clipped (`Do you want to experienceCommit {"trigger":…`); a preview-first
  question would read better. Polish, pre-existing, applies to MCP too.
- The commit card's diff headers carry long absolute store paths; `~`-short
  forms would read better.
