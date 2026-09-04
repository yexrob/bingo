# 0003 — Settings: three JSONC layers, merged per key by the claiming plugin

## Context

Users edit settings by hand and hosts generate them; the format is consumed independently of any one crate. The old project merged 23 keys field by field in one function that had to be kept in sync with the struct, and had no way to *unset* a union-merged list from a higher layer. Here every plugin owns its own keys, so the kernel cannot know the shape of a key — only how it merges.

## Decision

1. Layers, lowest priority first: `<config_dir>/settings.json` (user), `<cwd>/.bingo/settings.json` (project), `<cwd>/.bingo/settings.local.json` (local), `--settings <file>`, then command-line flags as a synthetic `cli` layer. JSONC (comments, trailing commas). A missing file is skipped; a non-object root is an error.
2. The kernel owns four top-level keys: `provider`, `model`, `thinking`, `maxTokens`. Every other top-level key belongs to the plugin whose `PluginManifest.config` claims it, as dotted paths with a merge rule: `Replace` (default), `Accumulate` (lists concatenate lowest first, first copy of a repeat kept), `ByName` (lists of objects keyed by `name` or `id`, a higher entry replaces the lower in place). Two plugins claiming the same root, or a plugin claiming a kernel key, is a build error.
   *(Amended 2026-09-04, M61: the kernel's keys are six. `pictures` is
   the sixth and the kernel reads none of it — `bingo-pictures` keeps
   the cache and a surface builds it — but it is a kernel key so that no
   plugin may claim it and nobody who sets it is told it is unknown.
   `settings::picture_cache_days` is its one reading, off the layers
   rather than out of `Merged`, because the process that hands the
   number to a surface composes those layers before a host exists.)*
3. Objects merge field by field at every depth; the rule applies to leaves. An explicit `null` in a higher layer clears the value from every layer below it — the tri-state the old project lacked.
4. `bingo_core::settings::merge(layers, claims)` is a pure function returning the kernel settings, one object slice per plugin holding only its roots, and the unclaimed keys with the layer that set them; the host reports those as notices, never silently.
5. Runtime changes (`AllowSession` rules, `/model`) are events, never written back. Writing settings is a command's job and always targets one named layer.
   *(Amended 2026-09-04, user-directed: "应该是记住上次设置的？" — `/model` is both. The change reaches the running session as an event, as it always did, and the same command then writes `provider` and `model` into the **user** layer, so the next start opens on it: a person who picks a model has picked one, not left a note for this process. Claude Code does the same — `/model` "saves your choice as the default for new sessions by writing the `model` field in your user settings" (<https://code.claude.com/docs/en/model-config>), while `--model` "appl[ies] only to the session you launch with", and Codex reads `model`/`model_provider` from `~/.codex/config.toml` with `--model` and `-c key=value` documented as overrides "for a single run" (<https://learn.chatgpt.com/docs/config-file/config-advanced>). So here too: `--model`/`--provider` are the `cli` layer and are never written back. The round trip is `settings::read_document` + `settings::write` — the one settings writer, shared with `bingo provider add` — and it refuses a file with comments in it rather than dropping them. Amended again the same day, user-reported "thinking 没有记住": `/think` writes `thinking` into the user layer by the same round trip — `off` included, as a choice rather than an absence — and a resumed session goes on at the level its own config view last carried, falling back to the settings only for a journal from before the view said it.)*

## Consequences

- A plugin's settings are typed by that plugin (`Registrar::config::<T>()`); the kernel never deserialises them.
- Adding a key needs no kernel change and cannot be forgotten by a merge function.
- Unknown keys are visible at startup (`UNKNOWN_SETTING`), so a typo like `permisions` is caught.
- Schema validation of a claimed slice is the plugin's responsibility until a JSON Schema validator is adopted (not before M5).
- A settings file a command rewrites is re-encoded as plain JSON in the order it was read; a JSONC file is read at startup and refused for writing, so the comments in it survive.

## Supersedes

—
