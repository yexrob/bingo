# M98 — Settings are TOML, and the file keeps its comments

## Goal

User, 2026-09-15: "JSON support is sometimes not very good"; add TOML, and
have the new version migrate `settings.json` to TOML on start. ADR-0058
decides: the layers are `settings.toml` (user, project, local), read by
extension with JSON still read where no TOML stands beside it; the first
start converts each `settings.json` losslessly to `settings.toml` and keeps
the original as `settings.json.bak`; a TOML write goes through `toml_edit`
and changes only the leaves that differ, so comments survive; TOML has no
`null`, so a conversion or a write that carries one is refused naming the
key; `Env::user_settings()` spells the user file once.

## Bricks, in build order

1. `Cargo.toml` — `toml_edit` (workspace, `features = ["serde"]`,
   `default-features = false` plus `parse`, `display`) with the ADR line;
   `scripts/budget.toml` 335 → 341 with the measured six. `cargo deny check`
   passes (`winnow` at two majors is a warn).
2. sdk `tool.rs` — `Env::user_settings(&self) -> PathBuf`
   (`config_dir/settings.toml`); core `settings::user_path` delegates;
   `bingo-provider-{anthropic,openai}/src/instances.rs` use it for their
   hints; the tests that assert the hint text follow.
3. core `settings/format.rs` (new, pure) — `Format::{Toml, Jsonc}` from a
   path's extension; `json_sibling(path)`: `settings.toml` →
   `settings.json`, `settings.local.toml` → `settings.local.json`
   (`.toml` → `.json` on the same stem); `parse(format, path, text) -> Value`
   (TOML through `toml_edit::DocumentMut` then `de::from_document`, JSONC as
   today). Tests: both formats parse to the same `Value`; a TOML root is
   always an object; a datetime does not panic.
4. core `settings/edit.rs` (new) — `diff(old: &Map, new: &Map) -> Vec<Change>`
   (`Change { path: Vec<String>, op: Set(Value) | Remove }`, recursing into
   objects, replacing arrays and scalars whole, ordered as `new` is) and
   `apply(doc: &mut DocumentMut, changes) -> Result<(), SettingsError>`: an
   object set at a path becomes a standard table (via
   `toml_edit::ser::to_document` on `{key: value}` and taking the item),
   a scalar or array a value; a `null` anywhere is `SettingsError::Null { key }`.
   Tests: a comment above an untouched key survives; a comment above a
   changed scalar survives; a removed key's comment goes with it; a nested
   leaf change leaves its siblings byte-identical; `null` names the dotted
   key.
5. core `settings.rs` — `layer_paths` returns the three `.toml` paths;
   `load` reads each path, else its `json_sibling`, else skips;
   `read_layer` and `read_document` parse by `Format`; `write` on a TOML
   path parses the existing document (empty when absent), applies
   `diff(existing, document)`, prints, and goes through the temp file and
   rename as today (the temp name is `<file name>.<pid>.<n>.tmp`); a JSON
   path is written exactly as before. Existing tests keep passing with
   `.toml` where they wrote `.json`; new: a round trip on TOML keeps a
   comment.
6. core `settings/migrate.rs` (new) — `migrate_one(toml: &Path) -> Result<Migration, SettingsError>`
   with `Migration::{Nothing, Done { from, to, kept }, Refused { from, key }}`:
   no JSON sibling or a TOML already there → `Nothing`; else read the JSON
   as a document (JSONC is fine here), refuse on a `null` (naming the key,
   the JSON stays), else write the TOML through brick 5's writer, rename
   the JSON to `settings.json.bak` unless one exists (then `kept: true`,
   the JSON stays unread). `migrate_all(env, cwd) -> Vec<Result<Migration, SettingsError>>`
   over `layer_paths`. `write` calls `migrate_one` first when its path is
   TOML and a JSON sibling stands beside no TOML, so a write never shadows
   the JSON's keys. Tests: a JSONC with comments migrates and the `.bak`
   keeps them; a `null` refuses and leaves both files as they were; an
   existing `.bak` is not overwritten; a second run is `Nothing`; a write
   into a directory that still holds only JSON migrates first.
7. bin `main.rs` — `run` calls `settings::migrate_all` first thing, before
   `before_any_host`, printing one `SETTINGS_MIGRATED` notice per `Done`
   (`moved <json> to <toml>; the original is <bak>`), one
   `SETTINGS_KEPT_JSON` per `Refused` (naming the key and "TOML has no
   null"), and a `SettingsError` as the run's error. Notices go through
   `notice_report` on stderr as every diagnostic does.
8. Tests that read a settings file back after a command wrote it read
   `.toml` (through `toml_edit::de::from_str::<serde_json::Value>` in a
   shared test helper): `crates/bingo/tests/{rpc.rs,cli/gateway.rs,cli/provider_add.rs,cli/mcp.rs}`,
   `crates/bingo-core/src/host/tests/commands.rs`, the gateway doctor's.
   Tests that only seed JSON stay as they are and cover §1's fallback; one
   black-box test seeds `.bingo/settings.json` with a comment, runs
   `bingo plugins list` or any host-less verb, and finds the `.toml`, the
   `.bak` and the notice.
9. ADR-0003 §1 and §5 amended and dated; `docs/adr/0058` is the record.

## Files

- `Cargo.toml`, `crates/bingo-core/Cargo.toml`, `scripts/budget.toml`, `deny.toml` (only if a ban bites)
- `crates/bingo-sdk/src/tool.rs`
- `crates/bingo-core/src/{settings.rs,settings/format.rs,settings/edit.rs,settings/migrate.rs}`
- `crates/bingo-provider-anthropic/src/instances.rs`, `crates/bingo-provider-openai/src/instances.rs`
- `crates/bingo/src/main.rs`, `crates/bingo/tests/**` (the readers), `crates/bingo-gateway/src/doctor.rs` (tests)
- `docs/adr/0003-settings.md`

## Exit criteria

- [x] a TOML layer and a JSONC layer with the same content merge identically.
- [x] a write into a commented TOML changes one leaf and nothing else.
- [x] a `null` is refused by name, on write and on migration.
- [x] the first run moves each `settings.json` to `.toml` + `.bak` with one
      notice each; the second run says nothing; a `.bak` already there is
      never overwritten.
- [x] `--settings x.json` reads as JSONC and is never migrated.
- [x] `cargo check -p bingo-core --all-targets --target x86_64-pc-windows-msvc`
      (renames and paths).
- [x] every gate green; `budget.sh` at 341; `cargo deny check` clean.

## Non-goals

- Migrating comments: the `.bak` keeps them.
- A `null` spelling for TOML: the JSON layer keeps that power.
- `--mcp-config` and `plugin.json`: not settings layers.
- The site's guide pages: updated in `../bingo-site` separately.

## Risks

- R-datetime: `from_document` into `serde_json::Value` on a TOML datetime;
  test it, whatever it yields is fine, a panic is not.
- R-tables: `to_document` decides inline vs standard tables; the diff/apply
  test pins that a new nested object lands as `[a.b]`, and an existing
  inline table that changes stays inline (edit in place).
- R-parallel: M97 adds a kernel key to `settings.rs`/`merge.rs` and an arm
  to `main.rs` at the same time. Touch only the read/write half here.

## Verified (2026-09-15)

Branch `m98-toml-settings`, off `29552c1f`. Every exit criterion is ticked;
the gates ran one at a time, none in the background. `merge.rs`,
`KERNEL_KEYS` and `KernelSettings` are untouched, and every public signature
in `settings.rs` is unchanged, so M97 and M99 merge over this cleanly.

Where each is covered:

- Both formats one layer — `settings::format::tests::the_same_settings_in_either_language_parse_to_the_same_value`;
  `settings::tests::a_layer_directory_with_no_toml_reads_the_json_that_stands_there`
  asserts the TOML layer's value equals the JSON's it replaced.
- One leaf and nothing else — `settings::edit::tests::{a_comment_above_an_untouched_key_survives,
  a_nested_leaf_change_leaves_its_siblings_byte_identical, a_removed_keys_comment_goes_with_it}`,
  and black-box `provider_add::a_comment_in_the_settings_survives_the_command_that_writes_beside_it`.
  `TableLike::insert` reformats the key it lands on, which would have dropped
  the comment above a *changed* key; `edit::put` carries the key's decor and
  the value's across the replacement.
- A `null` by name — `edit::tests::a_null_names_the_dotted_key_it_was_written_at`,
  `migrate::tests::a_null_refuses_by_name_and_moves_nothing`,
  `settings::tests::a_write_into_a_json_layer_that_spells_a_null_is_refused_by_name`.
- The move, once — `settings::migrate::tests` (five cases) and
  `provider_add::a_settings_json_left_from_an_older_bingo_crosses_on_the_first_run`,
  which asserts the `SETTINGS_MIGRATED` line on stderr.
- `--settings` untouched — `settings::tests::an_explicit_settings_file_is_read_where_it_is_and_never_migrated`.

One case the plan did not settle: `/think off` hands `remember` a top-level
`null`. That is not the tri-state of ADR-0003 §3 — it is "this layer says
nothing about this key" — so `remember` removes the key, which is the same
fact in the user layer, the lowest and the only one a command writes. A
`null` deeper inside a written document is still the error. Recorded in
ADR-0058 §4 and ADR-0003 §5.

```
$ cargo fmt --all -- --check                      # exit 0, no output
$ cargo check --workspace --all-targets --locked
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.67s
$ cargo clippy --workspace --all-targets --locked -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.56s
$ cargo test --workspace --locked       # 89 targets, 4743 passed, 0 failed, 2 ignored
   Running unittests src/lib.rs (.../bingo_core-1fe174c408a04cad)
test result: ok. 369 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s
   Running tests/cli/main.rs (.../cli-37388ab0375bae4a)
test result: ok. 226 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 23.63s
   Running tests/pty/main.rs (.../pty-e8dbc0cad072bfa9)
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.88s
   Running unittests src/lib.rs (.../bingo_gateway-2fdd90974dedce7f)
test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
  (the acp_bridge token test did not hang; nothing was skipped)
$ cargo check -p bingo-core --all-targets --locked --target x86_64-pc-windows-msvc
    Checking bingo-core v0.6.3
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 18.25s
$ scripts/check_discipline.sh
dependency direction ok / cohesion ok / kernel names no tool
  (no new file- or function-length warning: the three `fn` warnings are
   compact.rs, turn.rs and state.rs, all from before this milestone)
discipline ok
$ scripts/budget.sh
dependencies (unique, normal): 341 (max  341)
warm cargo check -p bingo-core: 0s (max  20s)
relink isolation: touching the TUI recompiled 0 crates for core (must be 0)
budget ok
$ cargo deny check
warning[duplicate]: found 2 duplicate entries for crate 'winnow'   # 0.7.15 + 1.0.4, the ADR's warn
advisories ok, bans ok, licenses ok, sources ok
```

The +6 was measured by resolving the same tree with and without the crate:
`cargo tree --workspace -e normal` went 335 → 341, the six being exactly
`toml_edit`, `serde_spanned`, `toml_datetime`, `toml_parser`, `toml_writer`
and `winnow`. Nothing else moved.

Two bricks landed differently. `toml_edit::ser::to_document` writes every
object inline, so `edit::standard` turns a new object into the standard
tables a person would have written (`[openai.instances.proxy1]`) and gives a
table holding nothing but tables no header of its own (R-tables). And a TOML
datetime reads as `toml_edit`'s own one-key object
(`{"$__toml_private_datetime": "..."}`), pinned by
`format::tests::a_datetime_reads_as_a_value_rather_than_a_panic` — R-datetime
answered: a value, not a panic.
