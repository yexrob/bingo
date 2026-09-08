//! The activity band: the rows between the transcript and the input box
//! (design §3), which are what is going on rather than what was said — the
//! verb row while a turn runs, what the session is waiting on between turns
//! (M82), what the last turn cost once it is over ([`crate::worked`], M84),
//! the task list under it or in its place ([`crate::tasks`], M74), and
//! whatever the person has queued behind it.
//!
//! Two rows are always held — the air and the verb row's slot — so a turn
//! starting or ending moves nothing; what the slot says is the one thing that
//! changes. Everything here is derived from the tree of states and the clock,
//! so a frame is a pure function of the two.

use bingo_sdk::{LiveTurn, Origin, QueueEntry, SessionState};
use ratatui::text::{Line, Span};

use crate::clock::{self, Now};
use crate::tree::{self, Scope, Tree, Wants};
use crate::ui::Ui;
use crate::{recap, tasks, theme, transcript, worked};

/// How long a turn must have run before it is worth a row of its own (§6).
const ACTIVITY_AFTER: std::time::Duration = std::time::Duration::from_millis(300);
/// What the activity row's verb becomes once a person has asked the turn to
/// stop: bingo's own words are for what it chose to do, and this it did not.
pub const STOPPING: &str = "Stopping";
/// One breath of bingo's presence while it is thinking: the pace between the
/// other two, and the one a turn starts at (§6).
const BREATH: std::time::Duration = std::time::Duration::from_millis(1600);
/// The breath while words are arriving: quicker, because something is.
const BREATH_ARRIVING: std::time::Duration = std::time::Duration::from_millis(900);
/// The breath while a tool holds the turn: slower, because the waiting is
/// somebody else's and the row says so.
const BREATH_BLOCKED: std::time::Duration = std::time::Duration::from_millis(2200);
/// One turn of the sparkle: four glyphs, 150 ms each (§6).
const SPARKLE: std::time::Duration = std::time::Duration::from_millis(4 * theme::SPARKLE_MS as u64);
/// bingo's own words for working (§4), one per turn.
const VERBS: [&str; 8] = [
    "Simmering",
    "Noodling",
    "Tinkering",
    "Rummaging",
    "Mulling",
    "Weaving",
    "Sketching",
    "Percolating",
];

/// The rows between the transcript and the input box: what the turn is doing
/// — or, between turns, what it is waiting on — the session's task list, and
/// whatever the person has queued behind it.
pub(crate) fn lines(tree: &Tree, ui: &Ui, width: usize, now: Now) -> Vec<Line<'static>> {
    let state = tree.viewed();
    let tasks = tasks::of(state);
    let mut out = band(tree, ui, &tasks, width, now);
    out.extend(queued(state));
    // A blank row between the transcript and these, as between any two blocks
    // (§3): they are not the tail of what was said, they are what is going on.
    if !out.is_empty() {
        out.insert(0, Line::default());
    }
    out
}

/// The verb row with the list hung under it while the turn shows one; the
/// worked row with the list hung under it once the turn is over (M84); the
/// summary with the list standing under it while there are tasks and no turn
/// has run; nothing at all otherwise (M74, Claude Code's own shape). One
/// row's slot either way, so the end of a turn changes the row's words and
/// moves nothing (§3: nothing jumps). `ctrl+t` keeps the rows and the summary
/// off the band and leaves the verb: the task being done is still what the
/// turn is doing.
///
/// The turn's row comes first, the wait after it (M82) — the wake that ends
/// a wait opens a turn, and that turn is what the row is then for — and what
/// the last turn cost after both, because a session still waiting on agents
/// is not done.
fn band(tree: &Tree, ui: &Ui, tasks: &[tasks::Task], width: usize, now: Now) -> Vec<Line<'static>> {
    let listed = !ui.tasks_hidden && !tasks.is_empty();
    let row = working(tree.viewed(), ui, tasks, now)
        .or_else(|| waiting(tree, now))
        .or_else(|| finished(tree.viewed(), now));
    let mut out = match row {
        Some(row) => {
            let mut out = vec![row];
            if listed {
                out.extend(tasks::hung(tasks::rows(tasks, width)));
            }
            out
        }
        None if listed => {
            let mut out = vec![tasks::summary(tasks)];
            out.extend(tasks::rows(tasks, width));
            tasks::standing(out)
        }
        None => Vec::new(),
    };
    out.extend(recapped(tree.viewed(), width));
    out
}

/// `※ recap: …` under the band once the turn is over (M84), for as long as
/// the recap is the last turn's: a turn that has begun takes the rows away,
/// and a recap of any earlier turn is not drawn at all ([`crate::recap`]).
fn recapped(state: &SessionState, width: usize) -> Vec<Line<'static>> {
    if state.busy() {
        return Vec::new();
    }
    recap::of(state)
        .map(|text| recap::rows(&text, width))
        .unwrap_or_default()
}

/// The lines the person is holding behind the turn, dim, each under its `>`.
fn queued(state: &SessionState) -> Vec<Line<'static>> {
    state
        .queue
        .iter()
        .filter(|entry| pending(&entry.origin))
        .map(|entry| {
            Line::from(Span::styled(
                format!("{} {}{}", theme::user(), entry.preview, waits(entry)),
                theme::dim(),
            ))
        })
        .collect()
}

/// The tag a row wears when the running turn will not be steered with it: a
/// line that asked to wait for the turn to end, and a command, which has
/// always waited for one (ADR-0008 §2, amended M68). `steerable` is the one
/// reading of that, so there is nothing else here to keep in step with it.
fn waits(entry: &QueueEntry) -> &'static str {
    match entry.steerable {
        true => "",
        false => " (waits)",
    }
}

/// Whether a queued input is a message the person is waiting to send. A
/// subsystem's entry — a room's post, a spawn's brief, a job reporting in — is
/// a steer in flight rather than something pending (ADR-0028), so it is drawn
/// nowhere here; the turn that absorbs it shows it in the transcript as the
/// quiet notice it is. The boundary is the transcript's own set, so an unknown
/// surface fails to the loud, person's side in both places alike.
fn pending(origin: &Origin) -> bool {
    !transcript::quiet(origin)
}

/// `✻ Simmering… (esc to interrupt · 4s · ↓ 1.2k tokens)` — but only once the
/// turn has been at it for [`ACTIVITY_AFTER`]: a turn that answers at once
/// says nothing at all, because a row that flashes reports nothing (§6).
///
/// A turn a person has asked to stop reads `✻ Stopping… (4s · ↓ 1.2k tokens)`
/// from the frame the key was pressed and keeps its sparkle and its clock
/// until `TurnCompleted` takes the row away. The hint goes with the asking:
/// `esc` has been pressed, and there is nothing further to press.
///
/// While a task on the list is in progress the verb is that task's own words
/// — `✻ Writing the plan…` — as Claude Code's spinner reads (M74); bingo's
/// own word is for a turn that has not said what it is doing.
fn working(
    state: &SessionState,
    ui: &Ui,
    tasks: &[tasks::Task],
    now: Now,
) -> Option<Line<'static>> {
    let turn = state.turn.as_ref()?;
    let elapsed = now.past(turn.started_at);
    let stopping = ui.stop_asked.as_ref() == Some(&turn.id);
    // A row that answers a key is never held back by the delay that spares a
    // fast turn its flash.
    if elapsed < ACTIVITY_AFTER && !stopping {
        return None;
    }
    let (verb, hint) = match stopping {
        true => (STOPPING, ""),
        false => (
            tasks::doing(tasks).unwrap_or_else(|| verb(&turn.id)),
            "esc to interrupt · ",
        ),
    };
    let mut spans = vec![Span::styled(
        format!("{} ", sparkle(now)),
        breathing(state, now),
    )];
    spans.extend(beamed(format!("{verb}{}", theme::ellipsis()), now));
    spans.push(Span::styled(
        format!(" ({hint}{}s{})", elapsed.as_secs(), spent(turn)),
        theme::dim(),
    ));
    if let Some(retry) = turn.retrying {
        spans.push(Span::styled(
            format!(" retrying {}/{}", retry.attempt, retry.max),
            theme::presence(),
        ));
    }
    Some(Line::from(spans))
}

/// `✻ Waiting for 2 background agents to finish` — what an idle session says
/// while agents it started are still at work (M82). A turn that ended to be
/// woken (M81) is not finished work, and the row that left with
/// `TurnCompleted` made it look finished: an empty composer over a dim
/// `2 running` in the status line, which the eye does not find.
///
/// The wait is not a turn, so it wears none of a turn's furniture: no `esc to
/// interrupt`, because there is no turn to end, and no clock and no token
/// count, which are what a turn has spent. The sparkle cycles and breathes at
/// [`BREATH_BLOCKED`] — the pace that already means the waiting is somebody
/// else's — and the input box's border stays dim, because nothing is arriving
/// here ([`crate::view`]'s `border` reads `busy`, which this session is not).
///
/// What it counts is what hangs *under* this session, not every other row the
/// status line counts: a child looking up at a running parent is waiting on
/// nothing of its own.
fn waiting(tree: &Tree, now: Now) -> Option<Line<'static>> {
    let viewed = tree.viewed();
    if viewed.busy() {
        return None;
    }
    let running = tree::count(tree, Wants::Running, Scope::Under(&viewed.summary.id))?;
    Some(Line::from(vec![
        Span::styled(
            format!("{} ", sparkle(now)),
            breathing_at(BREATH_BLOCKED, now),
        ),
        Span::styled(waiting_for(running), theme::text()),
    ]))
}

/// `✻ Worked for 15m 11s · done 13:48` — what the last turn cost, standing
/// in the verb's row once it is over (M84, Claude Code's own shape) until the
/// next turn takes the row back. The sparkle is at rest and nothing breathes:
/// nothing is at work, and the row says what was. A turn that answered at
/// once says nothing, as it drew no row while it ran.
fn finished(state: &SessionState, now: Now) -> Option<Line<'static>> {
    if state.busy() {
        return None;
    }
    let turn = state.last_turn.as_ref()?;
    if worked::ran(turn) < worked::SAY_AFTER {
        return None;
    }
    let (what, when) = worked::words(turn, now.wall, &jiff::tz::TimeZone::system());
    Some(Line::from(vec![
        Span::styled(format!("{} ", theme::spark()), theme::presence()),
        Span::styled(what, theme::dim()),
        Span::styled(when, theme::dim()),
    ]))
}

/// The row's words, in the number of agents there are.
fn waiting_for(running: usize) -> String {
    format!(
        "Waiting for {running} background agent{} to finish",
        if running == 1 { "" } else { "s" }
    )
}

/// How long one light takes to cross the working word and come round again.
/// Slower than the sparkle's breath, so the two are read as two things.
const BEAM: std::time::Duration = std::time::Duration::from_millis(2400);

/// bingo's word with one light walking across it while the turn runs (user,
/// 2026-09-05: the word wanted the beam the border and a landed call have).
/// The crest wears the glow and the rest of the word is `text`, so a frame
/// with no motion is the word alone.
fn beamed(word: String, now: Now) -> Vec<Span<'static>> {
    if !now.motion {
        return vec![Span::styled(word, theme::text())];
    }
    let t = clock::phase(now, BEAM);
    let width = word.chars().count();
    word.chars()
        .enumerate()
        .map(|(at, c)| {
            let lit = clock::sweep(t, at, width);
            Span::styled(c.to_string(), theme::comet(1.0 - lit))
        })
        .collect()
}

/// What the turn has said so far, in the thousands §6 writes it in — and
/// nothing at all before it has said anything.
fn spent(turn: &LiveTurn) -> String {
    match turn.usage.output_tokens {
        0 => String::new(),
        tokens => format!(" · ↓ {:.1}k tokens", tokens as f64 / 1000.0),
    }
}

/// bingo's own word for what it is doing (§4), drawn once per turn from the
/// turn's own id — so the same turn always reads the same way and a test can
/// name what it will say.
fn verb(turn: &bingo_sdk::TurnId) -> &'static str {
    VERBS[seed(turn.as_str()) % VERBS.len()]
}

/// FNV-1a: a stable spread over the words without a dependency to make one.
fn seed(id: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as usize
}

/// The sparkle's frame, or its first one when nothing may move.
fn sparkle(now: Now) -> &'static str {
    match now.motion {
        true => theme::sparkle(clock::cycle(now, SPARKLE)),
        false => theme::spark(),
    }
}

/// bingo breathing: the sparkle and the input box's border share one clock,
/// so the whole surface inhales together.
pub(crate) fn breathing(state: &SessionState, now: Now) -> ratatui::style::Style {
    breathing_at(breath_of(state), now)
}

/// The same breath at a pace the caller names — what the wait row takes,
/// having no turn to read one from. Still, it rests at `presence`: what
/// breathes is the brightness, not the fact that something is at work.
fn breathing_at(period: std::time::Duration, now: Now) -> ratatui::style::Style {
    match now.motion {
        true => theme::breath(clock::breath(now, period)),
        false => theme::presence(),
    }
}

/// How fast it breathes: the rhythm is what the turn is *doing*, so a pulse
/// says more than "a turn is running", which the row's presence already says
/// (§6). Words arriving are quick, a tool holding the turn is slow, and
/// thinking is the pace between them.
///
/// The phase is the wall clock's own turn of the period ([`clock::breath`]),
/// so a change of period changes where in the breath this frame lands. That
/// step is the state change itself, which is the one moment §6 allows a cue
/// to move — and it happens at most twice in a turn.
pub(crate) fn breath_of(state: &SessionState) -> std::time::Duration {
    if state.items.iter().any(arriving) {
        return BREATH_ARRIVING;
    }
    if state.items.iter().any(blocking) {
        return BREATH_BLOCKED;
    }
    BREATH
}

/// Whether an item is an answer still being said.
fn arriving(item: &bingo_sdk::Item) -> bool {
    matches!(item.body, bingo_sdk::ItemBody::Assistant { .. })
        && item.status == bingo_sdk::ItemStatus::Running
}

/// Whether an item is a call the turn is waiting on.
fn blocking(item: &bingo_sdk::Item) -> bool {
    matches!(item.body, bingo_sdk::ItemBody::ToolCall { .. })
        && item.status == bingo_sdk::ItemStatus::Running
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    /// The band's one row, as the terminal drew it, without the quotes a test
    /// backend prints around each row or the padding it is drawn into.
    fn row(tree: &Tree, now: Now) -> String {
        let (ui, _) = scene();
        draw_tree(80, 24, tree, &ui, now)
            .lines()
            .find(|line| line.contains("Waiting for"))
            .map(|line| line.trim_matches('"').trim_end().to_string())
            .unwrap_or_default()
    }

    /// What an idle session says while the agents it started are still at
    /// work, counted the way a person counts (M82).
    #[test]
    fn a_session_idle_over_running_agents_says_how_many_it_waits_for() {
        let (_, now) = scene();
        let one = row(&waited_on(1), now);
        assert!(
            one.ends_with("Waiting for 1 background agent to finish"),
            "{one}"
        );
        let two = row(&waited_on(2), now);
        assert!(
            two.ends_with("Waiting for 2 background agents to finish"),
            "{two}"
        );
    }

    /// The wait is not a turn, so it wears none of a turn's furniture — every
    /// piece of which rides in the row's one pair of parentheses.
    #[test]
    fn the_wait_carries_no_key_no_clock_and_no_token_count() {
        let (_, now) = scene();
        let drawn = row(&waited_on(1), now);
        assert!(!drawn.contains('('), "{drawn}");
    }

    /// Nothing at work, nothing to wait for: an agent that has finished is
    /// not waited on, and a session with none never was.
    #[test]
    fn a_session_with_no_agent_at_work_says_nothing_at_all() {
        let (_, now) = scene();
        assert_eq!(row(&waited_on(0), now), "");
        let mut done = waited_on(1);
        done.apply(&agent_frame(
            2,
            3,
            completed("trn_9", bingo_sdk::TurnStatus::Completed),
        ));
        assert_eq!(row(&done, now), "");
    }

    /// The row counts what hangs under this session, not everything else
    /// that is running: a child looking up at a running parent is waiting on
    /// nothing of its own. The status line's `1 running` is the other sense
    /// of the same count and is unchanged — furniture counts, the row speaks.
    #[test]
    fn a_child_under_a_running_root_is_not_waiting_on_an_agent() {
        let (ui, now) = scene();
        let mut tree = folded_tree(vec![
            frame(1, started("trn_1")),
            child_frame(1, announced("reviewer")),
        ]);
        tree.show(&child_id());
        assert_eq!(row(&tree, now), "");
        let drawn = draw_tree(80, 24, &tree, &ui, now);
        assert!(drawn.contains("1 running"), "{drawn}");
    }

    /// The wake that ends the wait opens a turn, and the turn's own row is
    /// what the band then says: one row's slot either way (§3).
    #[test]
    fn a_turn_on_the_viewed_session_takes_the_row_back_from_the_wait() {
        let (ui, now) = scene();
        let mut woken = waited_on(1);
        woken.apply(&frame(2, started("trn_1")));
        let at = later(now, 1_600);
        assert_eq!(row(&woken, at), "");
        let drawn = draw_tree(80, 24, &woken, &ui, at);
        assert!(drawn.contains("esc to interrupt"), "{drawn}");
    }

    /// The band's row after a turn, found by its verb.
    fn after(tree: &Tree, now: Now, verb: &str) -> String {
        let (ui, _) = scene();
        draw_tree(80, 24, tree, &ui, now)
            .lines()
            .find(|line| line.contains(verb))
            .map(|line| line.trim_matches('"').trim_end().to_string())
            .unwrap_or_default()
    }

    /// A turn that ran `seconds` and ended as `status`, the frames stamped
    /// with the clocks a real journal stamps them with.
    fn ended(seconds: i64, status: bingo_sdk::TurnStatus) -> Tree {
        let opened = frame(1, started("trn_1"));
        let closed = bingo_sdk::Frame {
            ts: opened.ts + jiff::SignedDuration::from_secs(seconds),
            ..frame(2, completed("trn_1", status))
        };
        folded_tree(vec![opened, closed])
    }

    /// Once a turn is over the row says what it cost (M84): the verb its
    /// verdict earns, how long it ran, and the hour it ended on.
    #[test]
    fn a_finished_turn_says_what_it_cost_in_the_verb_row() {
        let (_, now) = scene();
        let now = later(now, 911_000);
        let done = after(
            &ended(911, bingo_sdk::TurnStatus::Completed),
            now,
            "Worked for",
        );
        assert!(done.contains("Worked for 15m 11s · done "), "{done}");
        assert!(!done.contains('('), "no key, no token count: {done}");

        let stopped = bingo_sdk::TurnStatus::Interrupted {
            reason: bingo_sdk::InterruptReason::UserCancel,
        };
        let stopped = after(&ended(42, stopped), now, "Stopped after");
        assert!(stopped.contains("Stopped after 42s · "), "{stopped}");

        let failed = bingo_sdk::TurnStatus::Failed {
            error: bingo_sdk::KernelError::new(bingo_sdk::ErrorCode::Internal, "boom"),
        };
        let failed = after(&ended(7, failed), now, "Failed after");
        assert!(failed.contains("Failed after 7s · "), "{failed}");
    }

    /// A turn that answered at once drew no row while it ran, and draws none
    /// after; a session no turn has run in has nothing to say either.
    #[test]
    fn a_turn_that_answered_at_once_and_a_session_with_no_turn_say_nothing() {
        let (_, now) = scene();
        assert_eq!(
            after(
                &ended(0, bingo_sdk::TurnStatus::Completed),
                now,
                "Worked for"
            ),
            ""
        );
        let fresh = folded_tree(Vec::new());
        assert_eq!(after(&fresh, now, "Worked for"), "");
    }

    /// The next turn takes the row back, and a wait outranks the cost: a
    /// session still waiting on its agents is not done.
    #[test]
    fn a_new_turn_or_a_wait_outranks_the_finished_row() {
        let (ui, now) = scene();
        let mut again = ended(42, bingo_sdk::TurnStatus::Completed);
        again.apply(&frame(3, started("trn_2")));
        let at = later(now, 1_600);
        assert_eq!(after(&again, at, "Worked for"), "");
        assert!(draw_tree(80, 24, &again, &ui, at).contains("esc to interrupt"));

        let mut waited = waited_on(1);
        let closed = bingo_sdk::Frame {
            ts: ts() + jiff::SignedDuration::from_secs(42),
            ..frame(9, completed("trn_1", bingo_sdk::TurnStatus::Completed))
        };
        waited.apply(&closed);
        assert_eq!(after(&waited, at, "Worked for"), "");
        assert!(row(&waited, at).contains("Waiting for"));
    }

    /// The recap draws under the worked row while it is that turn's, and
    /// leaves with the next turn (M84).
    #[test]
    fn a_recap_of_the_last_turn_draws_under_the_worked_row_and_leaves_with_the_next() {
        let (ui, now) = scene();
        let now = later(now, 60_000);
        let mut tree = ended(42, bingo_sdk::TurnStatus::Completed);
        tree.apply(&frame(
            3,
            crate::test_lanes::signalled(
                "_bingo.context",
                "recap",
                serde_json::json!({"turn": "trn_1", "text": "Built the thing. Next: push it."}),
            ),
        ));
        let drawn = draw_tree(80, 24, &tree, &ui, now);
        let rows: Vec<&str> = drawn.lines().map(|l| l.trim_matches('"')).collect();
        let worked = rows
            .iter()
            .position(|l| l.contains("Worked for 42s"))
            .expect("the worked row");
        assert!(
            rows[worked + 1].contains("※ recap: Built the thing. Next: push it."),
            "{drawn}"
        );
        assert!(!drawn.contains("│ recap"), "no rail card: {drawn}");

        tree.apply(&frame(4, started("trn_2")));
        let drawn = draw_tree(80, 24, &tree, &ui, later(now, 1_600));
        assert!(!drawn.contains("recap:"), "{drawn}");
    }

    /// The working word carries one light across itself while the turn runs:
    /// two frames apart, different cells are lit; with motion off it is the
    /// word in `text` and nothing else.
    #[test]
    fn the_working_word_is_beamed_while_the_turn_runs_and_plain_when_still() {
        let (_, now) = crate::test_support::scene();
        let moving = beamed("Rummaging".to_string(), now);
        assert_eq!(moving.len(), 9, "one span per character");
        let later = beamed(
            "Rummaging".to_string(),
            crate::test_support::later(now, 600),
        );
        assert_ne!(
            moving.iter().map(|s| s.style).collect::<Vec<_>>(),
            later.iter().map(|s| s.style).collect::<Vec<_>>(),
            "the light has moved"
        );
        let still = beamed("Rummaging".to_string(), crate::test_support::still(now));
        assert_eq!(still.len(), 1);
        assert_eq!(still[0].style, theme::text());
    }
}
