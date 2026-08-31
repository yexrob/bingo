# 0018 — Background commands, and async by default

## Context

The old project backgrounded shell commands but shipped half the feature: no way to read a running command's output (one 4,000-char blob at exit), no way to kill anything (`WatchState::Cancelled` was dead code from birth), and a wake trigger that lived only in the TUI, so a headless client could start background work and never receive it. What it got right: notify conditions (`notify_on`/`notify_regex`) that make "do not poll" an honest rule, auto-backgrounding of commands that can never finish (`tail -f`, `watch`, `while true`), and promoting a *running* command to the background — a judgment a person makes thirty seconds in, not a flag guessed up front. The new tree already has the surface-independent half the old one lacked: `deliver(…, Delivery::Wake)` opens a turn on any session from anywhere, which is how an agent's followup lands today, and ADR-0013's live lane draws a plugin's running state in every surface for free. The user's ruling, now policy: **everything is asynchronous unless the caller must wait to continue** — `SpawnAgent` already defaults `background: true`; the shell joins it.

## Decision

1. **The principle**: a tool that can run detached offers `background` and leans async in its description; synchronous waiting is for results the very next step needs. The gate is unchanged — a background call is asked about exactly as a foreground one.
2. **Three verbs in `bingo-tool-bash`**, no kernel change. `Bash{background: true}` starts the process and returns at once with a minted job id and the log path. `BashOutput{id, cursor?}` returns the next chunk of output, a new cursor and the job's state — `SelfBounded`, read-only, trusted; the model pulls as much as it wants, when it wants. `KillShell{id}` ends the process group (TERM, then KILL after a grace) and reports what the exit was.
3. **Output lives on disk**: `<data_dir>/bash/<id>.log`, streamed as the process writes, capped by size with the cap noted in the log itself. The file is the one representation; `BashOutput` is a window over it, and a person may `tail -f` it themselves.
4. **Completion delivers, conditions deliver, growth does not.** The plugin's reader task watches each job; on exit — and on a `notify_on` substring or `notify_regex` hit — it sends one concise notification into the owning session via `deliver(Delivery::Wake)`, the same door an agent's followup uses, so `--print`, RPC, channels and the TUI all hear it. Mere output growth never wakes anything: that is what `BashOutput` is for.
5. **Commands that cannot finish are backgrounded unbidden.** The tool already parses commands (tree-sitter-bash); a shape that never exits — `watch`, `tail -f`, an unbounded `while`/`for` loop, a trailing `&` — is started in the background with a note saying so, whatever the flag said. A foreground call for one of these is always a mistake; failing closed costs nothing.
6. **A running command can be promoted.** The in-flight call listens for a promote signal beside the child; a surface fires it as an `Input::Action` naming the call (the TUI binds `ctrl+b` on the running block). The same process, pipes and buffer move to the job table — nothing restarts, the foreground timeout is dropped — and the call returns early with the job id.
7. **The jobs are visible for free**: the plugin publishes one live signal (ADR-0013's lane) listing running jobs — id, command head, age — so the rail draws them; a completion's notification is the durable trace in the transcript.

## Consequences

- `bingo-tool-bash` grows a job table (one noun, its own module), two tools and the reader tasks; `bingo-surface-tui` grows one keybinding and nothing else it does not already render. No new crate, no new dependency, no sdk or kernel touch.
- A background job lives exactly as long as the bingo process: no daemon, no persisted queue. The log file survives; the notification for a job whose session is gone goes nowhere, and says so in the log. Out-living the process is ADR-0019's schedule territory, not this one's.
- `Interrupt` semantics are unchanged: interrupting a turn never kills a background job (that is what `KillShell` is for); a foreground call being interrupted keeps today's Block behaviour.
- The permission gate sees `Bash` as it always did; `BashOutput` is read-only and free; `KillShell` asks in default mode like any other untrusted-adjacent act — killing what you started is cheap to approve and `acceptEdits` does not auto-allow it.

## Supersedes

—
