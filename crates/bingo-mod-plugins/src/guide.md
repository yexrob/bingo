# Plugins and MCP servers

`/plugins` (also `/modules`) opens plugins and MCP servers in one panel.
Typed `/mcp` opens it with the MCP group first and its first server selected.
Built-ins start folded; Enter expands them. At narrow widths the
selected card is below the list. Escape closes the panel.

Use Up/Down to select. Space switches the selected entry. A plugin with tools
changes at the next start; eligible drawing mods change between turns. The card
shows a pending change truthfully. Required store and surface plugins stay on.

For an external plugin, `r` runs `/plugins reload`; `t` chooses its trust tier:

- tighten: deny, ask, change your prompt
- rewrite: also change tool inputs and results
- full: also allow a call without asking

Full requires Enter twice. A repository plugin waits for your yes; Enter opens
the trust list, with a `not now` choice. Grants are kept in
`~/.bingo/settings.toml`, never the repository. Command equivalents are
`/plugins trust <name> <tier>` and `/plugins approve <name> [tier]`.
Reload applies module grants; process plugins use the grant at their next start.

For MCP, Enter lists connected tools, signs in, retries a failed connection,
turns on a disabled server or approves a waiting repository entry. `r` reconnects
connected or failed servers; `o` signs out. MCP tools always ask. Sign-in uses
the ordinary sealed sign-in interaction. Escape still cancels that interaction.

Disable `bingo.mod.plugins` to keep the original `/plugins` and `/mcp` tables.
