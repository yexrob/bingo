# 0003 — Settings: three JSONC layers, merged per key by the claiming plugin

## Context

Users edit settings by hand and hosts generate them; the format is consumed independently of any one crate. The old project merged 23 keys field by field in one function that had to be kept in sync with the struct, and had no way to *unset* a union-merged list from a higher layer. Here every plugin owns its own keys, so the kernel cannot know the shape of a key — only how it merges.

## Decision

1. Layers, lowest priority first: `<config_dir>/settings.json` (user), `<cwd>/.bingo/settings.json` (project), `<cwd>/.bingo/settings.local.json` (local), `--settings <file>`, then command-line flags as a synthetic `cli` layer. JSONC (comments, trailing commas). A missing file is skipped; a non-object root is an error.
2. The kernel owns four top-level keys: `provider`, `model`, `thinking`, `maxTokens`. Every other top-level key belongs to the plugin whose `PluginManifest.config` claims it, as dotted paths with a merge rule: `Replace` (default), `Accumulate` (lists concatenate lowest first, first copy of a repeat kept), `ByName` (lists of objects keyed by `name` or `id`, a higher entry replaces the lower in place). Two plugins claiming the same root, or a plugin claiming a kernel key, is a build error.
3. Objects merge field by field at every depth; the rule applies to leaves. An explicit `null` in a higher layer clears the value from every layer below it — the tri-state the old project lacked.
4. `bingo_core::settings::merge(layers, claims)` is a pure function returning the kernel settings, one object slice per plugin holding only its roots, and the unclaimed keys with the layer that set them; the host reports those as notices, never silently.
5. Runtime changes (`AllowSession` rules, `/model`) are events, never written back. Writing settings is a command's job and always targets one named layer.

## Consequences

- A plugin's settings are typed by that plugin (`Registrar::config::<T>()`); the kernel never deserialises them.
- Adding a key needs no kernel change and cannot be forgotten by a merge function.
- Unknown keys are visible at startup (`UNKNOWN_SETTING`), so a typo like `permisions` is caught.
- Schema validation of a claimed slice is the plugin's responsibility until a JSON Schema validator is adopted (not before M5).

## Supersedes

—
