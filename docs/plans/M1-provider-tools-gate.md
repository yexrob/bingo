# M1 — Real provider, real tools, permission gate

## Goal

`bingo --print --provider anthropic "fix the failing test"` runs a Claude-Code-shaped coding turn against the Anthropic Messages API with `Read Glob Grep Edit Write Bash AskUserQuestion`, under the five permission modes and an allow/deny/ask rule table read from three settings layers; approvals and questions work in `--print` on a TTY and fail closed off it.

## Bricks, in build order (owner)

1. `bingo-core::settings` (kernel) — three layers `~/.bingo/settings.json` < `.bingo/settings.json` < `.bingo/settings.local.json` < CLI flags; JSONC; per-key merge by the claiming plugin's `Merge` (`Replace` | `Accumulate` for lists | `ByName` for named entries); tri-state: an explicit `null` in a higher layer clears the lower value; unclaimed top-level keys are a startup `Notice{UNKNOWN_SETTING}`. Kernel keys: `provider`, `model`, `thinking`, `maxTokens`. Pure `merge(layers, claims) -> (kernel, per-plugin slices, unknown)` first, I/O second.
2. `bingo-core::context::system` (kernel) — the built-in `System{order: 0}` contributor: identity, an `<env>` block (cwd, platform, shell, date), tool-use guidance. `HostConfig.system_prompt` becomes an *extra* block, not the whole prompt.
3. `bingo-provider-anthropic` (worker A) — Messages API: request encoding (system blocks, `cache_control` on ≤4 blocks when `caching`, `thinking` per old `providers/anthropic.rs:189-204,889-910`, signatures round-tripped through `provider_metadata["anthropic"]`, images, tool_use/tool_result pairs); SSE decoding to `ModelEvent` (usage folded from `message_start` and `message_delta`, `input_json_delta` → `ToolInputDelta`, `signature_delta` → reasoning metadata, `ping` ignored, `error` events); error classification (401/403 Auth, 429 RateLimited with `retry-after`, 400 overflow phrase table → ContextOverflow, 5xx/529 Server, "512 characters" is not a 5xx; 60 s idle per chunk → Timeout); `count_tokens`; `models` via `GET /v1/models`; API key from `ANTHROPIC_API_KEY` or the `anthropic.apiKey` settings claim, `anthropic.baseUrl` override; a built-in capabilities table for the Claude families (replaced by models.dev in M2).
4. `bingo-tool-fs` (worker B) — `Glob` (`ignore` + `globset`, respects `.gitignore`, sorted by mtime, capped), `Grep` (`grep-searcher` + `grep-regex`; modes `files_with_matches` default / `content` / `count`; `-i`, `glob`, `type`, `head_limit`, context lines), `Edit` (exact `old_string` must be unique unless `replace_all`; `Preview::Diff` and `Display::Diff` via `similar`), `Write` (creates parents; refuses to overwrite a file it cannot read; diff preview on overwrite), `AskUserQuestion` (`InteractionKind::Question`, single/multi/free text; answers become the tool result). `Edit`/`Write` are `ToolTraits::edit()`, subjects `Path`.
5. `bingo-tool-bash` (worker C) — foreground `Bash`: `bash -c` (falls back to `sh`), own process group via `process-wrap`, stdin null, timeout 120 s default / 600 s max, 48 000-char output cap keeping head and tail, `$ cmd … [Exited with code N]` shape, live tail (5 lines every 100 ms) through `ToolContext::progress`, `Interrupt::Block`, the interactive-command rejection table and the periodic-command rejection ported from `src/tool/bash.rs:25-357`, description states the real shell. Subjects `Command`; traits fail closed except `trusted`.
6. `bingo-permissions` (worker D) — the one `PermissionPolicy`: modes `default | acceptEdits | plan | bypassPermissions | dontAsk`; rules `Tool`, `Tool(arg)`, `Tool(prefix:*)`, `Read(path)` with `~`/relative normalisation and no fs lookup, `WebFetch(domain:host)`, `Skill(name:*)`, `mcp__server`; Bash commands split with `tree-sitter-bash` (`&& || ; |` and `$()`; any ERROR node fails closed) and `shlex`; deny/ask on any sub-command, allow only when every sub-command is covered; sensitive paths (`.git .claude .vscode .idea`) and `Tool::confirm` prompt in every mode; `additionalDirectories`; the seven-step order of `src/permission.rs:325-408`; `AllowSession` installs a runtime rule via `on_verdict` (never persisted). Config claim `permissions` (`defaultMode` Replace, `allow/deny/ask/additionalDirectories` Accumulate).
7. `bingo` (kernel) — `--provider anthropic`, `--permission-mode`, `--dangerously-skip-permissions`, `--settings <file>`, `--allowed-tools`; compose the new plugins; `--print` interaction prompts already exist (M0).

Kernel changes are limited to 1, 2, 7 and whatever the gate needs to carry `Decision::Ask{scope}` through (`gate.rs` already does).

## Files

`crates/bingo-core/src/{settings.rs,settings/merge.rs,context/system.rs}`, `crates/bingo-provider-anthropic/src/{lib,request,sse,error,models}.rs` + `fixtures/*.sse`, `crates/bingo-tool-fs/src/{glob,grep,edit,write,ask}.rs`, `crates/bingo-tool-bash/src/{lib,reject,run,tail}.rs`, `crates/bingo-permissions/src/{lib,rules,split,modes}.rs`, `crates/bingo/src/main.rs`, `crates/bingo/tests/cli.rs`.

## Dependencies (each verified on crates.io 2026-08-29)

`reqwest 0.13.4` (rustls, json, stream) — the HTTP client; `similar 3.2` — Edit/Write diffs; `ignore 0.4.33` + `globset 0.4.20` — Glob and gitignore; `grep-regex 0.1.14` + `grep-searcher 0.1.17` — Grep on ripgrep's engine; `tree-sitter 0.26.13` + `tree-sitter-bash 0.25.1` + `shlex 2.0.1` — the permission splitter; `process-wrap 10.0` — process groups and kill-on-drop; `jsonc-parser 0.33.1` — settings with comments; dev: `wiremock 0.6.5` (provider SSE fixtures), `proptest 1.11` (matcher and splitter properties). Run `scripts/budget.sh` after each addition; the 260 cap holds.

## Exit criteria

- [ ] `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `scripts/check_discipline.sh`, `scripts/budget.sh`, `cargo deny check`
- [ ] Anthropic: request-body snapshots for text / tools / thinking+signature / images / cache_control; SSE fixtures for a text turn, a tool turn, a max_tokens stop, a mid-stream `error`, a 429 with `retry-after`, a 400 overflow; the retry ladder observed through the loop with a fake 529 then 200
- [ ] Permissions: the old `permission.rs` test table ported row by row; proptest — a command with an ERROR node is never allowed, deny beats allow, `ask` still asks under `bypassPermissions` for `confirm`/sensitive paths, `plan` denies every non-read-only tool
- [ ] Bash: rejection table cases, timeout kills the process group, output cap keeps head and tail, live tail frames appear as `ItemDelta{Tail}`, a mid-run interrupt returns the real partial result (Block) while a `Read` in the same batch is cancelled
- [ ] fs: Edit rejects non-unique and missing strings, `replace_all`, diff preview; Write refuses an unreadable target; Grep modes; Glob honours `.gitignore`; AskUserQuestion round-trips through `--print`
- [ ] Settings: layering fixtures (user < project < local < flag), `null` clears, unknown key notice, `Accumulate` for permission lists
- [ ] Black-box: `--permission-mode plan` denies `Write` with `PERMISSION_DENIED` in the tool result; a non-TTY `--print` denies an ask; `--dangerously-skip-permissions` allows `Bash(echo hi)`; `--provider anthropic` without a key fails with `[error] code=AUTH_REQUIRED` before any turn
- [ ] One manual live smoke against Anthropic (`ANTHROPIC_API_KEY=… bingo --print --provider anthropic "list the crates in this workspace"`), output pasted below

## Non-goals

OpenAI (M2), models.dev catalog (M2), `WebFetch`/`WebSearch` (M2), persistence and rewind checkpoints (M3), Bash background mode and the `!` command (with the command dispatcher, M3), the loopback SSE server in `bingo-provider-fake` (M5), OAuth (M10), Windows shell dialects (M6).

## Risks touched

R1 sdk churn — expected: `Env` may gain `shell`; every sdk change lists the plugins it touches in the commit body. R4 provider quirks — every quirk lands with a fixture. R6 fail-open — the invariants are separate tests, not incidental assertions.
