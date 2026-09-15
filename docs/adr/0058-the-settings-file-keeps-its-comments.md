# 0058 — Settings are TOML, and the file keeps its comments

## Context

ADR-0003 chose JSONC for the three settings layers so a person could annotate a file by hand, and then had to refuse every write into such a file, because rewriting it through `serde_json` would drop the comments (§5). So the file a person writes carefully is the one `/model`, `bingo provider add` and `bingo mcp add` cannot touch, and the file those commands write is the one a person cannot annotate. A `null` is the only way to clear a lower layer's value, and JSON is the only format that spells one. The user reported (2026-09-15) that the JSON support "is sometimes not very good" and asked for TOML with a migration on the next version's first start.

## Decision

1. **The layers are `settings.toml`**: `<config_dir>/settings.toml`, `<cwd>/.bingo/settings.toml`, `<cwd>/.bingo/settings.local.toml`, lowest first, and `--settings <file>` above them, read by its extension — `.toml` as TOML, anything else as JSONC, exactly as before. A layer directory that has no `settings.toml` reads its `settings.json` as it always did: JSON stays a format bingo reads. Where both exist, only the TOML is read; §2 makes that state momentary.
2. **The first start migrates.** For each of the three layer paths, a `settings.json` beside no `settings.toml` is converted and written as `settings.toml`, and the JSON is renamed `settings.json.bak` — never deleted, because a JSONC file's comments do not cross (§4) and the `.bak` is where they stay. A `.bak` already there is not overwritten: the JSON stays where it is, unread, and the notice says so. Every migration is one `SETTINGS_MIGRATED` notice on stderr naming both files. A conversion that cannot be lossless — a `null` anywhere, which TOML cannot spell — is refused with a notice naming the key, and that layer keeps its JSON until a person rewrites it. A `--settings` file is never migrated. The one function, `settings::migrate_one`, runs for every layer at the start of every run and once more inside any write to a TOML path whose JSON sibling is still the layer, so a write never shadows the keys a JSON held.
3. **A write keeps the file.** Writing a TOML layer goes through `toml_edit`: the document is parsed whole, the value the caller asked for is diffed against what the document already says, and only the leaves that differ are set or removed — every comment, blank line and ordering elsewhere survives, at any depth. The `write(path, &Map)` and `remember(path, keys)` signatures are unchanged, so every command that writes settings gains this without knowing it. A JSON path is written as before, and a JSON file with comments is refused as before.
4. **What TOML cannot say.** No `null`: a write carrying one into a TOML layer is an error naming the key, and the tri-state of ADR-0003 §3 is available only from a JSON layer or `--settings <file>.json`. Comments do not migrate. A TOML datetime read into a layer arrives as whatever `toml_edit`'s deserializer makes of it; no setting is one.
5. **One dependency, `toml_edit`** (`serde` feature, `+6`: `toml_edit`, `serde_spanned`, `toml_datetime`, `toml_parser`, `toml_writer`, `winnow`; budget 335 → 341). The `toml` crate is not taken: a document is the reader too, so one crate parses, edits and prints, and the parse-only crate would have bought nothing but a second copy of the parser. `winnow` arrives at two majors, which `deny.toml` warns on and does not refuse.
6. **The user layer's path is the sdk's.** `Env::user_settings()` spells `settings.toml` once; `bingo_core::settings::user_path` delegates, and the providers' hints ("set `openai.apiKey` in …") name the same file rather than a spelling of their own.

## Consequences

- ADR-0003 §1 and §5 are amended: the files are `.toml`, a write keeps comments. The merge, the claims and the kernel keys are untouched — a layer is an object whatever file it came from.
- Every black-box test that reads a settings file back after a command wrote it reads TOML. Tests that only seed a `settings.json` keep working through §1 and migrate on their first run.
- `--mcp-config` stays JSON: it is Claude Code's flag and a host's bundle, not a settings layer.
- The site's guide pages name `settings.json` and are updated separately.

## Supersedes

ADR-0003 §1 (the file names and format) and §5 (a commented file is refused for writing), in part.
