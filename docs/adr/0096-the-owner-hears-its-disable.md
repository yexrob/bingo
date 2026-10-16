# ADR-0096 — The owner hears its disable

## Context

The kernel owns the running plugin switch set. Plugins with transient UI state
currently infer disable by intercepting one command spelling and rereading its
result. Other switch routes evade that inference. Refusing a lifecycle callback
would force every plugin to reconstruct a kernel-owned state transition from
commands and catalogue snapshots. A setting wish is not the running state.

## Decision

- A native plugin may register `Contribution::Lifecycle(Arc<dyn PluginLifecycle>)`.
  Registration stamps its owner, as for other contributions.
- `PluginLifecycle::disabled(&HostHandle)` is called after an effective running
  on-to-off transition. Delivery targets that owner and bypasses disabled command
  and hook dispatch. The existing running set remains the only state source.
- Invoke callbacks without holding the switch-set lock. Repeated off, enable,
  refused or overridden changes, and pending restart/after-turn wishes do not
  announce a disable that has not occurred.
- Cleanup runs before the switch acknowledgement and under existing switch
  serialization. The existing hook deadline bounds each callback. Errors,
  panics and timeouts warn without undoing the switch or skipping remaining
  owner callbacks. Cleanup must not await another plugin switch synchronously.
- Existing switch routes share this transition seam. No settings watcher or
  new wire event is introduced. Source-managed plugins keep their existing
  unload lifecycle.

## Consequences

Plugins clear their own transient state and signals without parsing commands.
The kernel knows no feature names and stores no mirror of plugin UI state.
Settings persistence and switch eligibility retain ADR-0088 semantics.

## Supersedes

The command-string disable observers introduced in M191. Supplements ADR-0088;
does not replace source unload behavior in ADR-0057.
