# Sessions

`/sessions` or `bingo --resume` opens the session directory. `/resume` and
Ctrl+R keep their recent-session palette; choose **All sessions…** for the directory.

Type to filter titles and first asks. Tab switches this directory/all.
Choose **more…** to load another 50 sessions. Children sit below parents;
routing keys such as `schedule/…` fold into a group. Enter expands a group.

- Up/Down selects a row; Enter resumes it.
- Right previews its last exchange without sending a prompt.
- `r` edits the row’s title in place; Enter saves it and Escape cancels.
- `d` asks before deleting a session and its named descendants, children first.
- `f` forks a root at its latest completed turn and resumes the copy.
- Escape backs out of a preview or confirmation, then closes the directory.

The letter shortcuts apply when the filter is empty. `/` starts search mode
so a filter can begin with `r`, `d` or `f`; Escape leaves search mode.
Preview and fork read stored history without opening the source. Renaming a stored
session takes a brief lock and preserves its activity time. A session open in
another process cannot be renamed here: close it there first. Busy or locked
rows are marked in delete confirmation; the whole deletion is refused.
A fork preserves item IDs and the source model, has no children or routing key,
and records its origin. Busy sessions cannot be forked.

An unnamed session takes its first ask as its title. `/rename` always wins.
Disable `bingo.mod.sessions` to turn off both this directory and auto-titling;
the recent-session palette still works.
