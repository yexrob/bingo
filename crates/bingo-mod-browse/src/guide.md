# Browse and rewind

Press **Ctrl+F** or run `/browse` to read the conversation by turns.
`[` and `]` select the previous or next turn. Type a query, then press
Enter to finish searching; `n` and `N` walk matching lines. Search reads
full tool results and reasoning, including folded text. `/` starts a new
query. Enter opens the selected block in the pager; `y` copies its full text.
Escape clears the query first; another Escape leaves Browse and restores every
temporary fold. Escape from the pager returns to Browse at the same match.
Search keeps up to 1000 matching lines, shown as `1000+` at the limit.

Press `r` to preview a rewind to the selected turn. Typed `/rewind` opens
Browse with this sheet ready. The sheet lists each
tracked file and whether it will be restored, removed or skipped. Files
written by shell commands are not tracked. A file marked **changed since the
turn** may contain later edits: confirming still replaces those edits. Old
checkpoints without post-turn hashes also carry this warning. Left and right choose:

- **both**: restore files, then rewind the conversation.
- **conversation**: rewind the conversation and keep files as they are.
- **files**: restore files and keep the conversation.

Enter applies the previewed turn and chosen mode. New transcript items refresh
the preview before it can apply; a running turn must finish first. Escape returns to Browse. The empty
composer's double-Escape card opens this same preview. After a conversation
rewind, its original prompt returns to an empty composer; an existing draft
is kept and a notice explains why the prompt was not inserted.

Typed `/rewind <turn>` applies immediately, with no confirmation and mode
`both`. Append `conversation` or `files` to choose a different mode.

Disable `bingo.mod.browse` to retain Ctrl+F line search and direct rewind
from the double-Escape card.
