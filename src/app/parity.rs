//! The parity ledger: every user-observable behaviour, and which of the two
//! homes it has.
//!
//! The spec ("Meaning of parity") asks for exactly one thing of every CLI action
//! and domain state: either it is an [`AppCore`](crate::app::AppCore) action,
//! state or event **both frontends reach**, or it is **frontend-local
//! presentation** with no effect on bingo state. A behaviour that is neither is
//! how the console and `bingo app-server` drift apart again — one product
//! becoming two that agree by hand.
//!
//! So this module is a checklist, and the tests at the bottom are what makes it
//! one. Five inventories are enumerated here:
//!
//! | Inventory | Source of truth | Kept honest by |
//! |---|---|---|
//! | slash commands | [`COMMANDS`] | set equality with this table |
//! | typed actions | [`ACTIONS`] | set equality with this table |
//! | server notifications | [`ServerNotification::METHODS`] | set equality with this table |
//! | submission branches | [`Composed`] / [`Decision`] / [`Performed`] | exhaustive `match` |
//! | terminal events | [`UiEvent`] | exhaustive `match` |
//!
//! The first three are string tables, so a new entry with no ledger row fails a
//! test. The last two are Rust enums, so a new variant with no ledger row fails
//! to **compile** — the classification and the `match` are one text, and there
//! is no arm to forget separately from the row.
//!
//! What the ledger is not: a list of what the wire *could* carry. Terminal
//! layout, cursor, scroll, folds, roster focus, page breaks and image cell
//! geometry are local and always will be (spec "Explicit non-goals for 1.0").
//! Rows marked [`Local`] say what shared state stands behind them, when any
//! does, so "local" never becomes a place to hide a divergence.

use crate::app::action::{ACTIONS, COMMANDS};
use crate::app::submit::{Composed, Decision, Performed};
use crate::app_server::protocol::notifications::ServerNotification;
use crate::ui::UiEvent;

/// Where one user-observable behaviour lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Home {
    /// The core owns it and both frontends reach the same state. The note names
    /// the contract that carries it.
    Shared,
    /// Presentation, with no effect on bingo state. The note names the shared
    /// state behind it, or says there is none.
    Local,
}

use Home::{Local, Shared};

/// One ledger entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
    /// The inventory key: a command name, an action id, a notification method,
    /// or a variant name.
    key: &'static str,
    home: Home,
    /// What carries it, or why it stays where it is.
    note: &'static str,
}

const fn row(key: &'static str, home: Home, note: &'static str) -> Row {
    Row { key, home, note }
}

/// A ledger over a Rust enum.
///
/// The row and the `match` arm are written once, so a variant added to the enum
/// is a compile error here until somebody says where it lives. The generated
/// function is called by one test per inventory — its exhaustiveness is the real
/// check, and calling it is what proves the two halves name the same rows.
macro_rules! variant_ledger {
    (
        $rows:ident, $keyed:ident, $ty:ty,
        $( $pat:pat => $key:literal, $home:expr, $note:literal ; )+
    ) => {
        const $rows: &[Row] = &[$(row($key, $home, $note)),+];

        fn $keyed(value: &$ty) -> &'static str {
            match value {
                $($pat => $key),+
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// Every slash command. All shared: since B5 one table parses every line and
/// says what it *is* — an action the core applies, a session lifecycle call, or
/// a structured read. What a read looks like on the screen is each frontend's,
/// which is why a read's row names the method rather than the view.
const COMMAND_ROWS: &[Row] = &[
    row(
        "help",
        Shared,
        "`action/list`: the command table itself. Each frontend draws its own help.",
    ),
    row("clear", Shared, "`action/execute` session.reset."),
    row(
        "compact",
        Shared,
        "`action/execute` conversation.compact; the summary is an item and an operation.",
    ),
    row(
        "model",
        Shared,
        "`catalog/read` models, then `action/execute` model.select.",
    ),
    row("cd", Shared, "`action/execute` session.cd."),
    row(
        "resume",
        Shared,
        "`session/list` bare, `session/resume` with a name.",
    ),
    row("rename", Shared, "`action/execute` session.rename."),
    row("gc", Shared, "`action/execute` session.gc."),
    row("share", Shared, "`action/execute` session.share."),
    row("context", Shared, "`session/read`'s context usage."),
    row("status", Shared, "`session/read`."),
    row("config", Shared, "`config/read`."),
    row(
        "permissions",
        Shared,
        "`config/read` permissions, plus permission.ruleAdd/ruleRemove.",
    ),
    row(
        "theme",
        Shared,
        "`action/execute` theme.set. Which colours a theme means is the frontend's.",
    ),
    row("images", Shared, "`catalog/read` images."),
    row(
        "mcp",
        Shared,
        "`config/read` mcp, plus mcp.enable/disable/reconnect.",
    ),
    row(
        "provider",
        Shared,
        "`catalog/read` providers, plus provider.select/login/logout.",
    ),
    row("think", Shared, "`action/execute` thinking.select."),
    row("skills", Shared, "`catalog/read` skills."),
    row("tasks", Shared, "`resource/read` tasks."),
    row(
        "team",
        Shared,
        "the team reads, plus team.start/assign/stop/scaffold/memoryGc.",
    ),
    row("join", Shared, "`action/execute` room.join."),
    row("leave", Shared, "`action/execute` room.leave."),
    row("exit", Shared, "`session/close`."),
];

// ---------------------------------------------------------------------------
// Typed actions
// ---------------------------------------------------------------------------

/// Every action. All shared by construction — an action is what
/// `action/execute` executes — so the notes carry the useful fact instead: how
/// the console reaches it, and where the two frontends can ask for different
/// things through the same action.
const ACTION_ROWS: &[Row] = &[
    row("session.reset", Shared, "`/clear`."),
    row(
        "session.rename",
        Shared,
        "`/rename`. The transcript and its sidecars move, and the core's locator follows them (D155).",
    ),
    row("session.gc", Shared, "`/gc`."),
    row("session.share", Shared, "`/share`."),
    row("session.cd", Shared, "`/cd`."),
    row("conversation.compact", Shared, "`/compact`."),
    row(
        "conversation.rewind",
        Shared,
        "esc-esc opens the console's checkpoint gesture; the wire has preview and apply. The two shapes have not been reconciled (open decision, D155).",
    ),
    row("model.select", Shared, "`/model <id>` or the picker."),
    row("provider.select", Shared, "`/provider <name>`."),
    row(
        "provider.login",
        Shared,
        "`/provider login`. The browser flow runs in the core; `--manual <token>` is passed in-process and never becomes a wire field.",
    ),
    row("provider.logout", Shared, "`/provider logout`."),
    row("thinking.select", Shared, "`/think <level>`."),
    row(
        "permission.mode",
        Shared,
        "shift+tab cycles it; there is no typed line.",
    ),
    row(
        "permission.ruleAdd",
        Shared,
        "`/permissions <decision> <rule>`.",
    ),
    row(
        "permission.ruleRemove",
        Shared,
        "`/permissions remove <decision> <rule>`.",
    ),
    row("mcp.enable", Shared, "`/mcp enable <server>`."),
    row("mcp.disable", Shared, "`/mcp disable <server>`."),
    row(
        "mcp.reconnect",
        Shared,
        "`/mcp reconnect [server]`. With no server the action reconnects every enabled one, from either frontend (D157).",
    ),
    row(
        "skill.invoke",
        Shared,
        "a skill is a command called by its own name, so the table cannot list it in advance.",
    ),
    row(
        "team.start",
        Shared,
        "`/team start` always brings the whole chart up; the action's `members` selects part of it and only a wire client can ask for that.",
    ),
    row("team.assign", Shared, "`/team assign <member> <task>`."),
    row(
        "team.stop",
        Shared,
        "`/team stop` stands the whole crew down; the action's `member` stops one instance and only a wire client can ask for that.",
    ),
    row("team.scaffold", Shared, "`/team new <name>`."),
    row("team.memoryGc", Shared, "`/team memory gc`."),
    row("room.join", Shared, "`/join <room>`."),
    row("room.leave", Shared, "`/leave <room>`."),
    row(
        "command.promote",
        Shared,
        "ctrl+b backgrounds the foreground command; there is no typed line.",
    ),
    row("theme.set", Shared, "`/theme <choice>`."),
];

// ---------------------------------------------------------------------------
// Server notifications
// ---------------------------------------------------------------------------

/// Every notification method. All shared — a notification *is* the shared
/// contract — so each note names what reads it on the console side, and says so
/// plainly when nothing does.
const NOTIFICATION_ROWS: &[Row] = &[
    row("session/updated", Shared, "store: the session summary."),
    row(
        "session/closed",
        Shared,
        "the console leaves by its own route; the transport reports it to a client.",
    ),
    row(
        "session/deleted",
        Shared,
        "no console reader: `/gc` and `session/delete` report their own result.",
    ),
    row("conversation/created", Shared, "store: the page roster."),
    row(
        "conversation/updated",
        Shared,
        "store: summaries, unread, obligations.",
    ),
    row("conversation/removed", Shared, "store: the page goes."),
    row("turn/started", Shared, "the `❯` row and the running state."),
    row("turn/roundStarted", Shared, "store: the round counter."),
    row(
        "turn/retrying",
        Shared,
        "the live tail is withdrawn to the checkpoint.",
    ),
    row("turn/roundCompleted", Shared, "the round's rows settle."),
    row(
        "turn/usageUpdated",
        Shared,
        "the context bar and the output-token rate.",
    ),
    row(
        "turn/completed",
        Shared,
        "the turn's one ending: a completion row, or the turn-level error.",
    ),
    row("item/started", Shared, "a tool row appears."),
    row("item/textDelta", Shared, "the assistant's prose, appended."),
    row(
        "item/reasoningDelta",
        Shared,
        "the thinking block, appended.",
    ),
    row(
        "item/commandTailUpdated",
        Shared,
        "the foreground command's dim tail.",
    ),
    row(
        "item/updated",
        Shared,
        "the tool call's resolved input: the fold decision.",
    ),
    row(
        "item/completed",
        Shared,
        "the authoritative row: tool result, notice, peer message, interruption.",
    ),
    row("queue/itemAdded", Shared, "store: the composer's queue."),
    row("queue/itemRemoved", Shared, "store: the queue shrinks."),
    row(
        "queue/itemAbsorbed",
        Shared,
        "the `↪` row, where the model read it.",
    ),
    row(
        "interaction/opened",
        Shared,
        "the permission or question dialog.",
    ),
    row("interaction/resolved", Shared, "the dialog closes."),
    row(
        "interaction/cancelled",
        Shared,
        "the dialog closes without an answer.",
    ),
    row("agent/changed", Shared, "store: the roster."),
    row("agent/removed", Shared, "store: the roster shrinks."),
    row("room/changed", Shared, "store: the room list."),
    row("task/changed", Shared, "store: `/tasks`."),
    row("task/removed", Shared, "store: `/tasks`."),
    row("delivery/changed", Shared, "store: direct-message state."),
    row(
        "command/changed",
        Shared,
        "store: background commands. The console still draws its watch rows from the registry broadcast instead (known duplication, D155).",
    ),
    row("operation/started", Shared, "store: operations in flight."),
    row(
        "operation/progress",
        Shared,
        "store: an operation's progress line.",
    ),
    row(
        "operation/completed",
        Shared,
        "store: the operation's ending.",
    ),
    row(
        "config/changed",
        Shared,
        "store: model, provider, thinking, permission mode, theme.",
    ),
    row(
        "catalog/changed",
        Shared,
        "no console reader: the pickers fetch a catalog when they open.",
    ),
    row(
        "asset/available",
        Shared,
        "no console reader: the terminal loads its own image bytes and measures its own cells.",
    ),
    row("feedback/raised", Shared, "the warning tier."),
    row(
        "feedback/cleared",
        Shared,
        "no console reader: the tiers expire on their own clock.",
    ),
];

// ---------------------------------------------------------------------------
// Submission branches
// ---------------------------------------------------------------------------

variant_ledger! {
    COMPOSED_ROWS, composed_key, Composed,
    Composed::Empty => "Composed::Empty", Shared,
        "nothing was typed.";
    Composed::Shell(_) => "Composed::Shell", Shared,
        "the `!` prefix. Shell mode is grammar, not a console mode flag.";
    Composed::Command(_) => "Composed::Command", Shared,
        "a leading slash. One table parses it for both frontends.";
    Composed::Direct { .. } => "Composed::Direct", Shared,
        "the `@name`/`#room` grammar, resolved against the live registries.";
    Composed::Prose(_) => "Composed::Prose", Shared,
        "an ordinary prompt, including an unresolved sigil.";
}

variant_ledger! {
    DECISION_ROWS, decision_key, Decision,
    Decision::Nothing => "Decision::Nothing", Shared,
        "an empty submission changes nothing.";
    Decision::Turn { .. } => "Decision::Turn", Shared,
        "main is idle: the prose starts a turn.";
    Decision::Shell { .. } => "Decision::Shell", Shared,
        "a shell line always runs in the console's context, whichever page it was typed on.";
    Decision::Command { .. } => "Decision::Command", Shared,
        "a command keeps the page it was typed on (D135a).";
    Decision::Queue(_) => "Decision::Queue", Shared,
        "main is busy: the entry waits, and a tool barrier may absorb it.";
    Decision::Deliver { .. } => "Decision::Deliver", Shared,
        "an addressee that runs no turn for it: the message enters an inbox or a room log.";
}

variant_ledger! {
    PERFORMED_ROWS, performed_key, Performed,
    Performed::Nothing => "Performed::Nothing", Shared,
        "nothing was submitted.";
    Performed::Turn { .. } => "Performed::Turn", Shared,
        "the item exists, the turn is open, the engine has the work.";
    Performed::Shell { .. } => "Performed::Shell", Shared,
        "the `!` line's run is open.";
    Performed::Command { .. } => "Performed::Command", Local,
        "the one arm the core does not perform: a command's *view* is the frontend's, so the line comes back with the page it was typed on. What it changes is still an action the core applied.";
    Performed::Queued(_) => "Performed::Queued", Shared,
        "the placement on the queue, keyed.";
    Performed::Delivered { .. } => "Performed::Delivered", Shared,
        "the message reached an inbox or a room log, and the item names it.";
    Performed::Undelivered { .. } => "Performed::Undelivered", Shared,
        "the domain refused, in the domain's own words.";
    Performed::Unavailable => "Performed::Unavailable", Shared,
        "this session has no engine.";
}

// ---------------------------------------------------------------------------
// Terminal events
// ---------------------------------------------------------------------------

variant_ledger! {
    UI_EVENT_ROWS, ui_event_key, UiEvent,
    UiEvent::Submitted(_) => "UiEvent::Submitted", Shared,
        "`turn/started`'s input items: the `❯` row.";
    UiEvent::TurnStart => "UiEvent::TurnStart", Shared,
        "`turn/started`.";
    UiEvent::StreamRetry => "UiEvent::StreamRetry", Shared,
        "`turn/retrying`'s authoritative checkpoint replacement.";
    UiEvent::RoundEnd => "UiEvent::RoundEnd", Shared,
        "`turn/roundCompleted`.";
    UiEvent::TextDelta(_) => "UiEvent::TextDelta", Shared,
        "`item/textDelta`.";
    UiEvent::ThinkingDelta(_) => "UiEvent::ThinkingDelta", Shared,
        "`item/reasoningDelta`.";
    UiEvent::ContextUsage(_) => "UiEvent::ContextUsage", Shared,
        "`turn/usageUpdated`'s context usage.";
    UiEvent::OutputTokens { .. } => "UiEvent::OutputTokens", Shared,
        "`turn/usageUpdated`'s usage. The between-frames estimate is the console's own arithmetic over the shared deltas; the authoritative value is the core's.";
    UiEvent::ToolStart { .. } => "UiEvent::ToolStart", Shared,
        "`item/started` with a tool body.";
    UiEvent::ToolReady { .. } => "UiEvent::ToolReady", Shared,
        "`item/updated`: the resolved input the fold decision reads.";
    UiEvent::ToolDone(_) => "UiEvent::ToolDone", Shared,
        "`item/completed` with a tool body.";
    UiEvent::WatchEvent { .. } => "UiEvent::WatchEvent", Shared,
        "agent, task and background-command transitions. The state is the core's — `agent/changed`, `task/changed`, `command/changed` — but the console subscribes to the registry broadcast rather than reading the store (known duplication, D155).";
    UiEvent::ModelsLoaded { .. } => "UiEvent::ModelsLoaded", Local,
        "the picker's own asynchronous fetch. The shared facts are `catalog/read` and `catalog/changed`.";
    UiEvent::ImageReady { .. } => "UiEvent::ImageReady", Local,
        "terminal cell geometry and renderer-ready bytes. The shared fact is the asset (`asset/available`, `asset/readChunk`).";
    UiEvent::TurnEnd => "UiEvent::TurnEnd", Shared,
        "`turn/completed`.";
    UiEvent::Mail { .. } => "UiEvent::Mail", Shared,
        "`item/completed` with a peer message, on the page it landed on.";
    UiEvent::Steered { .. } => "UiEvent::Steered", Shared,
        "`queue/itemAbsorbed`.";
    UiEvent::BashTail(_) => "UiEvent::BashTail", Shared,
        "`item/commandTailUpdated`, main's foreground command only.";
    UiEvent::Interrupted(_) => "UiEvent::Interrupted", Shared,
        "`item/completed` with the interruption marker the transcript recorded.";
    UiEvent::Warning(_) => "UiEvent::Warning", Shared,
        "`feedback/raised`, and a notice at warning level.";
    UiEvent::SlashOutput(_) => "UiEvent::SlashOutput", Local,
        "a feedback tier. What produced it is shared — an action's result, or a read the frontend rendered — but which tier a terminal shows it on is not session state (spec: not a wire event).";
    UiEvent::SlashError(_) => "UiEvent::SlashError", Local,
        "a feedback tier. The core's half arrives as a notice at error level.";
    UiEvent::SlashInfo(_) => "UiEvent::SlashInfo", Local,
        "a feedback tier. The core's half arrives as a notice at info level.";
    UiEvent::RewindDone(_) => "UiEvent::RewindDone", Local,
        "the rewind's state line, written into the flow rather than a tier that expires. The rewind itself is an action and an operation.";
    UiEvent::PinPanel { .. } => "UiEvent::PinPanel", Local,
        "a pinned panel above the prompt (spec: not a wire event). The operation behind it is shared.";
    UiEvent::Unpin { .. } => "UiEvent::Unpin", Local,
        "the panel goes (spec: not a wire event).";
    UiEvent::Error { .. } => "UiEvent::Error", Shared,
        "`turn/completed` with a failed status, carrying the core's stable error code. The console raises the same variant for its own local failures, which have no session state to share.";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every inventory key has exactly one row, and every row names something
    /// that exists. This is the check the campaign was for: a new command,
    /// action or notification cannot land unclassified.
    fn covers(rows: &[Row], inventory: &[&str], what: &str) {
        let mut keys: Vec<&str> = rows.iter().map(|row| row.key).collect();
        keys.sort_unstable();
        let listed = keys.len();
        keys.dedup();
        assert_eq!(listed, keys.len(), "two {what} rows share a key");
        let mut have: Vec<&str> = inventory.to_vec();
        have.sort_unstable();
        assert_eq!(
            keys, have,
            "the {what} ledger and the {what} table disagree: every entry needs a home, \
             and every row needs an entry"
        );
        for row in rows {
            assert!(
                !row.note.is_empty(),
                "{} is classified and unexplained",
                row.key
            );
        }
    }

    #[test]
    fn every_slash_command_has_a_home() {
        let names: Vec<&str> = COMMANDS.iter().map(|spec| spec.name).collect();
        covers(COMMAND_ROWS, &names, "slash command");
    }

    #[test]
    fn every_action_has_a_home() {
        let ids: Vec<&str> = ACTIONS.iter().map(|spec| spec.id).collect();
        covers(ACTION_ROWS, &ids, "action");
    }

    #[test]
    fn every_notification_has_a_home() {
        covers(
            NOTIFICATION_ROWS,
            ServerNotification::METHODS,
            "notification",
        );
    }

    /// The enum ledgers are exhaustive by compilation; this is what proves the
    /// two halves of each one name the same rows, and that the classifier is
    /// reachable rather than a comment with a `match` in it.
    #[test]
    fn every_submission_branch_has_a_home() {
        for (rows, key) in [
            (COMPOSED_ROWS, composed_key(&Composed::Empty)),
            (DECISION_ROWS, decision_key(&Decision::Nothing)),
            (PERFORMED_ROWS, performed_key(&Performed::Nothing)),
        ] {
            assert!(
                rows.iter().any(|row| row.key == key),
                "{key} is classified by the match and missing from the table"
            );
            covers(
                rows,
                &rows.iter().map(|row| row.key).collect::<Vec<_>>(),
                key,
            );
        }
    }

    #[test]
    fn every_terminal_event_has_a_home() {
        let key = ui_event_key(&UiEvent::TurnStart);
        assert!(
            UI_EVENT_ROWS.iter().any(|row| row.key == key),
            "{key} is classified by the match and missing from the table"
        );
        covers(
            UI_EVENT_ROWS,
            &UI_EVENT_ROWS.iter().map(|row| row.key).collect::<Vec<_>>(),
            "terminal event",
        );
    }

    /// The ledger's own size, stated. A row that quietly disappears is as much a
    /// drift as one that was never written, and the D record quotes these
    /// numbers.
    #[test]
    fn the_ledger_covers_what_it_says_it_covers() {
        assert_eq!(COMMAND_ROWS.len(), 24, "slash commands");
        assert_eq!(ACTION_ROWS.len(), 28, "actions");
        assert_eq!(NOTIFICATION_ROWS.len(), 39, "notifications");
        assert_eq!(COMPOSED_ROWS.len(), 5, "composed lines");
        assert_eq!(DECISION_ROWS.len(), 6, "routing decisions");
        assert_eq!(PERFORMED_ROWS.len(), 8, "performed submissions");
        assert_eq!(UI_EVENT_ROWS.len(), 27, "terminal events");
    }

    /// Which rows are local, listed. The spec names five things that are not
    /// wire events and reasons about a sixth; anything else claiming to be
    /// presentation-only has to be argued for here rather than asserted in a
    /// note nobody reads.
    #[test]
    fn what_stays_in_the_frontend_is_a_short_list() {
        let local: Vec<&str> = COMMAND_ROWS
            .iter()
            .chain(ACTION_ROWS)
            .chain(NOTIFICATION_ROWS)
            .chain(COMPOSED_ROWS)
            .chain(DECISION_ROWS)
            .chain(PERFORMED_ROWS)
            .chain(UI_EVENT_ROWS)
            .filter(|row| row.home == Local)
            .map(|row| row.key)
            .collect();
        assert_eq!(
            local,
            vec![
                "Performed::Command",
                "UiEvent::ModelsLoaded",
                "UiEvent::ImageReady",
                "UiEvent::SlashOutput",
                "UiEvent::SlashError",
                "UiEvent::SlashInfo",
                "UiEvent::RewindDone",
                "UiEvent::PinPanel",
                "UiEvent::Unpin",
            ],
            "a behaviour moving into the frontend-local column is a parity \
             decision, not an implementation detail"
        );
    }
}
