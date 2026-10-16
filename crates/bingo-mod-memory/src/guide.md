# Memory

`/memory` opens memories, loaded instruction files and this project's
experience in one panel. Arrow keys select a row; Enter reads its full text
in the pager. Escape returns from the pager, then closes the panel.
Typed `/experience` opens this panel on its experience group.

`d` asks to forget a memory or experience entry. Enter confirms; Escape
cancels. Instruction files cannot be forgotten here: edit the file itself.
The panel never launches an editor or writes a new memory.

You can also use `/memory forget [user|project] <name>` or
`/experience forget <id>`. If a memory name exists in both scopes, specify
one. Forget removes the memory file and every index line linking it.
A session's prompt may retain it until next composed. Say in chat when a
correction should not be learned again from the same conversation.

Token estimates measure the current capped memory index lines, loaded
instructions, and active experience index lines. Whole memory and experience
bodies do not count. Existing prompt snapshots may retain earlier material.

Set `enabledPlugins["bingo.mod.memory"]` to `false` for the original
`/memory` and `/experience` tables. The mod switches immediately; the
context and experience owners contribute request material and services,
so their switches take effect at the next start.
