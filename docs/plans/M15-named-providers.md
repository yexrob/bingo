# M15 — Named provider instances, keys by login

## Goal

`bingo login work` beside `bingo login personal` — two codex subscriptions in one `auth.json`; `openai.instances.proxy1` beside `proxy2` — many OpenAI- or Anthropic-shaped endpoints, each addressable as `--provider <name>` / `/model <name>/<model>`; and a pasted key for any of them through the same `/login` the subscription already has (ADR-0017). The kernel is untouched.

## Bricks, in build order (worker)

1. **Instance settings + registration** — `instances: { <name>: {…} }` on each of the three keys; one provider registered per instance under its own name; collisions with built-ins or siblings refused at boot with the offending name in the error.
2. **Credential resolution per instance** — key instances: `auth.json` `Api` entry under the instance name, else the instance's `apiKey`; env vars feed the default instances only. Codex instances: `TokenSource` keyed by instance name.
3. **Paste login for keys** — `LoginMethod::Paste` on key-based providers stores/deletes the `Api` entry; browser/device stay `Unsupported`; auth status names the key's source.
4. **`bingo provider add`** (ADR-0017 §6) — terminal prompts for name / shape / base url / an optional hidden key; the instance into the user settings layer (JSON round-trip, `preserve_order`; an unparseable file is left alone and named), the key into `auth.json`; refuses a name the registry already owns; closes with `bingo --provider <name>`.
5. **Black-box** — two codex instances hold two token entries (wiremock issuer, distinct refresh bodies); `/login proxy1` paste → a turn runs with the pasted key on the wire; `--provider` and `/model name/model` resolve instances; a colliding name refuses at boot with exit ≠ 0 and the name in stderr; `OPENAI_API_KEY` set does not leak into a named instance; `provider add` against a temp HOME writes both files and a following run uses them (prompts driven over the pty or stdin per the login subcommand's tests).

## Files

`crates/bingo-provider-openai/src/lib.rs` (+ a module if the loop crowds it), `crates/bingo-provider-anthropic/src/lib.rs`, `crates/bingo/src/` (the `provider add` subcommand, binary edge only), `crates/bingo/tests/cli/login.rs` (or a sibling), no other crate.

## Exit criteria

- [x] two codex instances, two `auth.json` entries, each refreshed independently (wiremock)
- [x] `/login <key-instance>` pastes a key into `auth.json` (0600) and the next turn sends it; `/logout` removes it
- [x] env feeds only the defaults; a named instance with no key fails with a message naming `/login <name>`
- [x] a colliding instance name is refused at boot, named
- [x] the catalogue lists instances; `/model <name>/<model>` switches to one
- [x] `bingo provider add` writes the instance and the key; the next run resolves `--provider <name>`; an unparseable settings file is left untouched
- [x] every gate green (fmt, check, clippy, test, discipline, budget unchanged, deny, tui-smoke)

## Non-goals

New wire shapes (an instance is one of the three existing ones); per-instance model allow-lists; an in-session `/provider add` (registration is a boot-time fact; a hot provider source is its own ADR the day it is needed); migrating existing settings; preserving comments in a settings file the add rewrites (JSON in, JSON out).

## Risks

R-order — the credential resolution order is where a proxy user gets silently the wrong key; the env-only-defaults rule has its own test. R-replace — `Merge::Replace` on the parent key replaces the instances too; documented in the ADR, not worked around.

## Verified — 2026-08-31, merged `cfbede5`

The worker was stopped by accident after its last commit and before its
report; the integrator verified from the worktree instead. Four commits:
instances + paste for each provider crate, the end-to-end tests, and the
`provider add` subcommand.

```
provider crates                 73 + 12 + 101 tests, all ok
cli black-box                   72 (instances.rs, provider_add.rs among them)
fmt / clippy / discipline / deny            all 0
scripts/budget.sh               297 (max 297) — unchanged, no new crate
quiet rerun (load 5.2)          the three wall-clock TUI tests 3/3; tui-smoke exit 0
```

(The loaded-machine episode repeated during the merge gates — load hit
42/92/117 and the three wall-clock tests plus one smoke scene failed; both
passed untouched once the machine quieted. Same rule as M11/M13: read
`uptime` before reading a timing failure.)

Probed on the real binary (tmux): `bingo provider add` walks name →
protocol (an explicit `openai`/`anthropic` choice) → base url → hidden key;
the instance lands in settings **without** the secret, the key lands in
`auth.json` (0600) under the instance's name, and the pasted key never
appears on the pane.

Resolution notes: `key.rs` reads `auth.json` first so `/login`/`/logout`
mean something in a shell that already exports a key; a named instance is
built with no environment variable at all, so the rule is unrepresentable
rather than checked. A cross-plugin instance-name collision falls to the
registry's later-duplicate-dropped rule (each plugin sees only its own
settings); recorded as accepted.
