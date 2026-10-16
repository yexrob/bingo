# Choosing a model

`/model` opens the model picker. `/model <filter>` narrows the list; an exact
model id switches immediately. Provider prefixes such as `openai/` filter one
provider. Select a provider's hidden-model row to include snapshots and non-chat
models. Non-chat models cannot be selected for a conversation.

Use ↑↓ to select, ←→ to choose an offered thinking effort, Tab to keep the
choice for new sessions, and * to pin or unpin. Enter acts; Escape returns one
sheet or closes the picker. Typing only changes the filter.

Alt+M runs `/model next`, cycling the pins in their saved order. Alt+T runs
`/think next`, cycling this model's offered efforts. Both changes belong to
this session. Command bindings take priority over a plugin band's hotkey;
configure `keys` to rebind them. Ctrl+P has no model binding.

Select a provider to test its connection, refresh models, replace its key or
sign out. Named instances also offer an address editor and removal with
confirmation. An environment credential must be unset in the shell.

The palette has one `/model` entry. Typed `/models` still opens its complete
list; bare `/provider` opens the same picker. Their argument-taking commands
continue to work, including `/models refresh` and `/provider remove <name>`.

`+` adds a provider. Paste a key or address into the sealed, masked sign-in
field. Review the inferred name, shape and address before adding. Credentials
are never shown in the review or sent as command arguments. The provider's
normal sign-in interaction handles browser, device and paste flows.

A smaller model may require compaction on the current provider before the
switch. Pictures remain in the transcript when the chosen model cannot read
them. The outcome line reports both conditions.

Disable `bingo.mod.models` to retain the plain `/model`, `/think`, `/models`
and `/provider` commands. The palette and other built-ins are independent.
