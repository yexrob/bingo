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

- [ ] commit → query → outcome → revise-under-same-id round-trips on disk; the adversarial-content entry survives byte-exact
- [ ] a recall line lands in the transcript as a contributor user item; an empty store contributes nothing and costs no scan
- [ ] the index block lists ≤ 10 with the overflow pointer; `/experience` renders the table in the TUI and folds in `--print`
- [ ] `ExperienceOutcome` without evidence is an input error; recording an outcome never changes status
- [ ] `schema/plugin.json` drift test green; an unknown protocol major refuses the handshake with a notice
- [ ] the wordcount tool runs through the permission gate untrusted (the card shows `plugin__wordcount__…`), `/wordcount` answers, both driven black-box through the real binary (test skips without `python3`)
- [ ] a killed plugin process leaves one notice, empty sources, and a working respawn on the next turn
- [ ] every gate green: fmt, check, clippy, test, discipline, budget, deny, tui-smoke

## Non-goals

Auto-extraction of experiences; gc/TTL/caps; cross-store dedup with `bingo-context` memory; hooks/contributors/providers over the bridge; a plugin marketplace; sandboxing bridge processes (the gate already asks per call).

## Risks

R-frontmatter — escaping is where the old one silently corrupted; the adversarial round-trip test is the contract, written before the serializer. R-python — CI machines without `python3` skip the exit test; the drift test still pins the schema. R-scope-creep — the bridge invites "one more contribution kind"; v1 is Tool + Command, the enum is the map, each later kind needs its own ADR line.
