# ADR-0099 — Remember the last model

Status: accepted · 2026-10-27 · Plan: M203 · Amends: ADR-0085 §1

## Context

The last deliberate model choice should survive a restart without changing
handwritten defaults. The kernel already resolves the successful choice and
owns the command; a surface-owned preference would duplicate that fact.
No new kernel door or dependency is needed.

## Decision

- After successful root-session core `/model` switches, save one versioned
  `{ "version": 1, "provider": "…", "model": "…" }` record in
  `Env.data_dir/last-model.json`. Write a unique, exclusively created temporary
  file, then atomically rename it; concurrent writers publish whole pairs.
- At startup, precedence is CLI > explicit settings > project local > project >
  remembered pair > user defaults. Any non-user layer containing either key
  suppresses the remembered pair entirely, including JSON null. Other settings
  retain their existing per-key merge.
- Read the candidate once before plugins start. Missing, unreadable, malformed or
  unsupported records and providers absent or not signed in are ignored.
  Retry validation after plugins start for late provider sources;
  roots opened during startup retain their resolved choice. No startup network
  probe; unlisted IDs stay valid.
- CLI flags, children, generic reconfigure, resume and startup never save this
  record. Thinking stays session-only. This is a boot snapshot, not a live default.
- `--keep` continues writing the user settings and applying Default scope.
  A failed preference write warns without failing the successful session switch.

## Consequences

Ordinary root choices leave settings untouched but become next-start fallbacks.
Picker/outcome copy distinguishes this from explicitly keeping settings. Model
existence cannot be established offline for servers accepting unlisted IDs.

## Supersedes

ADR-0085 §1's no-write promise for ordinary `/model` only; `/think` and CLI
non-persistence, explicit `--keep`, and the rest of ADR-0085 are unchanged.
