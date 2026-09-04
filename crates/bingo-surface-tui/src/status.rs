//! The one line of furniture, under the input box: mode on the left, what is
//! true now in the middle, where you are on the right (design §3).
//!
//! Every slot is derived at render time — the mode from the config the policy
//! published, the counts from the tree, the context from the reducer's last
//! `ContextUsage` — so nothing here is a second copy of a fact. The middle is
//! the slot that gives way when the line does not fit: it is the one whose
//! words are already the shortest true thing to say.

use bingo_sdk::{Effort, SessionState};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::clock::{self, Now};
use crate::tree::{self, Status, Tree};
use crate::ui::Ui;
use crate::{keys, permission, theme, wake};

/// Cells between two slots.
const GAP: usize = 2;
/// The context count starts warming towards `bad` at this share of the
/// compaction trigger…
const CONTEXT_WARM: u64 = 80;
/// …and says the way out from here, where it is `bad` outright.
const CONTEXT_BAD: u64 = 90;
/// What the middle says when nothing else is true and nothing is typed.
pub const HINT: &str = keys::FOOTER_HINT;

/// The status line at this width.
pub fn line(tree: &Tree, ui: &Ui, width: usize, now: Now) -> Line<'static> {
    place(
        left(tree.viewed()),
        middle(tree, ui, now),
        right(tree),
        width,
    )
}

/// The permission mode, as the policy published it. `default` is what a
/// session already is, so it says nothing at all.
fn left(state: &SessionState) -> Vec<Span<'static>> {
    let mode = permission::mode(state).filter(|mode| *mode != "default");
    mode.map(|mode| {
        vec![
            Span::styled(format!("{} {mode}", theme::glyphs().mode), theme::mode()),
            Span::styled(" (shift+tab to cycle)", theme::dim()),
        ]
    })
    .unwrap_or_default()
}

/// Only what is true now, in the order the eye should take it.
fn middle(tree: &Tree, ui: &Ui, now: Now) -> Vec<Span<'static>> {
    let mut parts: Vec<Span<'static>> = Vec::new();
    // The answer to the key a person just pressed outruns every queued
    // notice: an armed exit says so now, or not at all.
    if ui.exit_armed(now.instant) {
        parts.push(Span::styled(crate::input::ARM_HINT, theme::text()));
    }
    if let Some(waiting) = count(tree, Wants::Attention) {
        parts.push(Span::styled(
            format!("{waiting} needs you (ctrl+g)"),
            theme::attention(now),
        ));
    }
    if let Some(running) = count(tree, Wants::Running) {
        parts.push(Span::styled(format!("{running} running"), theme::dim()));
    }
    parts.extend(waking(tree.viewed(), now));
    parts.extend(notice(ui, now));
    if parts.is_empty() && ui.composer.is_empty() {
        parts.push(Span::styled(HINT, theme::dim()));
    }
    join(parts)
}

/// What the sessions other than the one on screen are doing. The session in
/// view speaks for itself: its turn is the activity row and its card is the
/// brightest thing on the screen.
enum Wants {
    Attention,
    Running,
}

fn count(tree: &Tree, wants: Wants) -> Option<usize> {
    let n = tree
        .rows()
        .iter()
        .filter(|row| row.session != tree.view())
        .filter(|row| match wants {
            Wants::Attention => row.attention,
            Wants::Running => row.status == Some(Status::Running),
        })
        .count();
    (n > 0).then_some(n)
}

/// The wake the model set on this session, counted down against the frame's
/// own clock (ADR-0019 §8). It is the one thing on this line the *model* set
/// in motion, and a person who does not want it types `/wake off`; a moment
/// already past is a wake on its way in, and says nothing rather than a
/// negative.
fn waking(state: &SessionState, now: Now) -> Vec<Span<'static>> {
    let Some(at) = wake::at(state) else {
        return Vec::new();
    };
    let ahead = at.duration_since(now.wall);
    if !ahead.is_positive() {
        return Vec::new();
    }
    vec![Span::styled(
        format!("wake in {}", clock::span(ahead.unsigned_abs())),
        theme::dim(),
    )]
}

/// What the next request carries against the model's window — `41k/200k`,
/// used over the whole — before the model it goes to. Always there once a
/// turn has run, because how much room is left is a fact a person wants
/// without asking; it warms from `dim` towards `bad` across the last fifth of
/// the compaction trigger and, past [`CONTEXT_BAD`], says the way out.
fn usage(state: &SessionState) -> Option<Span<'static>> {
    let context = state.context.filter(|c| c.window > 0)?;
    let share = context
        .used
        .saturating_mul(100)
        .checked_div(context.trigger)
        .unwrap_or(0);
    let tail = match share >= CONTEXT_BAD {
        true => " · /compact",
        false => "",
    };
    Some(Span::styled(
        format!(
            "{}/{}{tail}",
            thousands(context.used),
            thousands(context.window)
        ),
        theme::warming(warmth(share)),
    ))
}

/// Tokens as a person counts them: `41k`, and the bare number under a
/// thousand.
fn thousands(tokens: u64) -> String {
    match tokens {
        t if t < 1000 => t.to_string(),
        t => format!("{}k", t / 1000),
    }
}

/// How far the context count has warmed: nothing until [`CONTEXT_WARM`] % of
/// the trigger, all the way at the trigger itself.
fn warmth(share: u64) -> f32 {
    let span = (100 - CONTEXT_WARM) as f32;
    (share.saturating_sub(CONTEXT_WARM) as f32 / span).clamp(0.0, 1.0)
}

/// What the kernel or the surface had to say, while it is being said: one at
/// a time, arriving out of dim and leaving into it (§6).
fn notice(ui: &Ui, now: Now) -> Vec<Span<'static>> {
    if ui.opening {
        return vec![Span::styled("opening a session…", theme::dim())];
    }
    let Some(notice) = ui.notice() else {
        return Vec::new();
    };
    let Some(strength) = notice.strength(now) else {
        return Vec::new();
    };
    let mut spans = vec![Span::styled(
        notice.text.clone(),
        theme::fading(notice.level, strength),
    )];
    // What the refusal was about is the person's own line: it is said after
    // the reason, and never more loudly than it. The slot's own separator
    // joins the two, as it joins every other pair.
    if let Some(about) = notice.about.as_ref() {
        spans.push(Span::styled(
            about.clone(),
            theme::fading(bingo_sdk::Level::Info, strength),
        ));
    }
    spans
}

/// Where you are, how much of the window is spent, and what answers you: who
/// serves the model, which model, and how hard it is asked to think — the
/// model alone at the root.
///
/// All three are read from the frames that carry them: the summary, which the
/// kernel stamps from the choice a turn will actually run on, and the config
/// view's `thinking`, which is the one place a client reads the level
/// (ADR-0008 §4). Nothing here is a second copy of either.
fn right(tree: &Tree) -> Vec<Span<'static>> {
    let state = tree.viewed();
    let mut parts: Vec<Span<'static>> = Vec::new();
    parts.extend(
        tree.viewing()
            .map(|child| dimmed(format!("in {}", tree::name(child)))),
    );
    parts.extend(usage(state));
    parts.extend(answering(state).map(dimmed));
    parts.extend(effort(state).map(dimmed));
    join(parts)
}

fn dimmed(text: String) -> Span<'static> {
    Span::styled(text, theme::dim())
}

/// `provider/model`, or the model alone where no provider is named — an
/// older journal's summary, or a session nothing answers.
fn answering(state: &SessionState) -> Option<String> {
    let model = state.summary.model.clone().filter(|m| !m.is_empty())?;
    let provider = state.summary.provider.as_deref().filter(|p| !p.is_empty());
    Some(match provider {
        Some(provider) => format!("{provider}/{model}"),
        None => model,
    })
}

/// The reasoning effort the next turn will ask for. A model that does not
/// reason is sent none, and the kernel publishes none, so the slot says
/// nothing rather than a level no request carries.
fn effort(state: &SessionState) -> Option<String> {
    let level = state.config.kernel.get("thinking")?.clone();
    serde_json::from_value::<Effort>(level)
        .ok()
        .map(|level| level.name().to_string())
}

fn join(parts: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for part in parts {
        if !out.is_empty() {
            out.push(Span::styled(" · ", theme::dim()));
        }
        out.push(part);
    }
    out
}

/// Left at the margin, right at the far edge, the middle centred in what is
/// between them — and against the left margin when there is no mode to sit
/// beside, so the hint reads where Claude Code's does.
fn place(
    left: Vec<Span<'static>>,
    middle: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let (left, middle, right) = fit(left, middle, right, width);
    let slack = width.saturating_sub(cells(&left) + cells(&middle) + cells(&right));
    let before = match (left.is_empty(), middle.is_empty()) {
        (_, true) => slack,
        (true, false) => 0,
        (false, false) => slack / 2,
    };
    let mut spans = left;
    spans.push(pad(before));
    spans.extend(middle);
    spans.push(pad(slack - before));
    spans.extend(right);
    Line::from(spans)
}

/// The middle gives way first, then the mode; the place a person is in is the
/// last thing to go.
fn fit(
    left: Vec<Span<'static>>,
    middle: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> (Vec<Span<'static>>, Vec<Span<'static>>, Vec<Span<'static>>) {
    let right = clip(right, width);
    let left = clip(left, width.saturating_sub(cells(&right)));
    let gaps = GAP * (!left.is_empty() as usize + !right.is_empty() as usize);
    let room = width.saturating_sub(cells(&left) + cells(&right) + gaps);
    (left, clip(middle, room), right)
}

fn cells(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn pad(width: usize) -> Span<'static> {
    Span::raw(" ".repeat(width))
}

/// Cut spans to `max` cells, the last one ending in an ellipsis. Nothing is
/// left of a slot with no room for even that. The quick cycle's strip is the
/// other thing this row draws, so it is cut to the row by the same rule.
pub fn clip(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    if cells(&spans) <= max {
        return spans;
    }
    if max <= 1 {
        return Vec::new();
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in spans {
        let room = max - 1 - used;
        let width = span.content.width();
        if width <= room {
            used += width;
            out.push(span);
            continue;
        }
        out.push(cut(&span, room));
        break;
    }
    out.push(Span::styled("…", theme::dim()));
    out
}

fn cut(span: &Span<'static>, room: usize) -> Span<'static> {
    let mut text = String::new();
    let mut used = 0;
    for c in span.content.chars() {
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + width > room {
            break;
        }
        text.push(c);
        used += width;
    }
    Span::styled(text, span.style)
}

/// Every span of a drawn status line with the style it carries, for the tests
/// that assert where a colour landed.
#[cfg(test)]
pub fn styles(line: &Line<'static>) -> Vec<(String, ratatui::style::Style)> {
    line.spans
        .iter()
        .map(|s| (s.content.to_string(), s.style))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bingo_sdk::{ContextUsage, Event, TurnId};

    fn text(tree: &Tree, ui: &Ui, width: usize) -> String {
        at(tree, ui, width, scene().1)
    }

    fn at(tree: &Tree, ui: &Ui, width: usize, now: Now) -> String {
        line(tree, ui, width, now).to_string()
    }

    fn with_context(used: u64) -> Tree {
        solo(&folded(vec![frame(
            1,
            Event::TurnUsage {
                turn: TurnId::from_raw("trn_1"),
                usage: Default::default(),
                context: ContextUsage {
                    used,
                    window: 200_000,
                    trigger: 100_000,
                },
            },
        )]))
    }

    #[test]
    fn an_idle_root_says_only_the_hint_and_the_model() {
        let (ui, _) = scene();
        let drawn = text(&solo(&state()), &ui, 80);
        assert!(drawn.contains(HINT), "{drawn}");
        assert!(drawn.ends_with("fake-1"), "{drawn}");
        assert!(!drawn.contains("in "), "the root is not somewhere else");
        assert_eq!(drawn.width(), 80, "the line fills its row");
    }

    #[test]
    fn the_hint_goes_when_something_is_typed() {
        let (mut ui, now) = scene();
        write(&mut ui, &state(), "x", now);
        assert!(!text(&solo(&state()), &ui, 80).contains(HINT));
    }

    #[test]
    fn the_mode_the_policy_published_sits_on_the_left() {
        let (ui, _) = scene();
        let drawn = text(&solo(&with_permission_mode("acceptEdits")), &ui, 80);
        assert!(
            drawn.starts_with("⏵⏵ acceptEdits (shift+tab to cycle)"),
            "{drawn}"
        );
        assert!(
            !text(&solo(&with_permission_mode("default")), &ui, 80).contains("⏵⏵"),
            "default is what a session already is"
        );
    }

    /// A wake standing on this session, `n` seconds after the scene's own
    /// wall clock — the one the line is drawn against.
    fn with_wake(now: Now, seconds: i64) -> Tree {
        let at = now.wall + jiff::SignedDuration::from_secs(seconds);
        solo(&folded(vec![frame(1, pending_wake(&at.to_string()))]))
    }

    /// The one thing on this line the model set in motion, in the words every
    /// other span of time on the screen is said in.
    #[test]
    fn a_pending_wake_is_counted_down_and_goes_when_it_is_taken_back() {
        let (ui, now) = scene();
        for (seconds, said) in [
            (40, "wake in 40s"),
            (245, "wake in 4m"),
            (3600, "wake in 1h"),
        ] {
            let drawn = at(&with_wake(now, seconds), &ui, 80, now);
            assert!(drawn.contains(said), "{seconds}s: {drawn}");
        }
        let past = at(&with_wake(now, -5), &ui, 80, now);
        assert!(
            !past.contains("wake"),
            "a wake on its way in says nothing: {past}"
        );
        let none = at(&solo(&state()), &ui, 80, now);
        assert!(!none.contains("wake"), "{none}");
    }

    #[test]
    fn a_pending_wake_is_furniture_and_the_whole_line_still_fits() {
        let (ui, now) = scene();
        let tree = with_wake(now, 245);
        let styled = styles(&line(&tree, &ui, 80, now));
        assert_eq!(
            styled
                .iter()
                .find(|(text, _)| text.starts_with("wake in"))
                .map(|(_, style)| *style),
            Some(theme::dim()),
            "it says what is true, it does not ask for anything"
        );
        for width in [40usize, 80, 120] {
            assert_eq!(at(&tree, &ui, width, now).width(), width);
        }
        insta::assert_snapshot!("wake_pending", at(&tree, &ui, 80, now));
    }

    /// The count is always there once a turn has run, used over the whole
    /// window, and sits before the model on the right.
    #[test]
    fn the_context_count_sits_before_the_model_and_says_the_way_out_late() {
        let (ui, _) = scene();
        let early = text(&with_context(41_000), &ui, 80);
        assert!(early.contains("41k/200k"), "{early}");
        assert!(!early.contains("/compact"), "{early}");
        let bad = text(&with_context(90_000), &ui, 80);
        assert!(bad.contains("90k/200k · /compact"), "{bad}");
        assert!(
            !text(&solo(&folded(vec![])), &ui, 80).contains("/200k"),
            "no turn has run, so there is nothing to count"
        );
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(41_900), "41k");
    }

    #[test]
    fn the_context_count_is_dim_until_it_is_bad() {
        let (ui, _) = scene();
        let style = |used| {
            styles(&line(&with_context(used), &ui, 80, scene().1))
                .into_iter()
                .find(|(text, _)| text.contains("/200k"))
                .map(|(_, style)| style)
        };
        assert_eq!(style(70_000), Some(theme::dim()));
        assert_eq!(style(90_000), Some(theme::bad()));
    }

    #[test]
    fn the_children_that_want_you_and_the_ones_at_work_are_counted() {
        let tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, started("trn_9")),
        ]);
        let (ui, _) = scene();
        assert!(text(&tree, &ui, 80).contains("1 running"));

        let waiting = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, opened(child_permission())),
        ]);
        let drawn = text(&waiting, &ui, 80);
        assert!(drawn.contains("1 needs you (ctrl+g)"), "{drawn}");
    }

    #[test]
    fn the_session_in_view_is_never_one_of_its_own_notices() {
        let mut tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, started("trn_9")),
        ]);
        tree.show(&child_id());
        let (ui, _) = scene();
        let drawn = text(&tree, &ui, 80);
        assert!(!drawn.contains("running"), "{drawn}");
        assert!(drawn.contains("in reviewer · fake/fake-1"), "{drawn}");
    }

    /// The whole of a notice's life: the frames it arrives in, its window,
    /// and the frames it leaves in.
    fn notice_life() -> i64 {
        (crate::ui::NOTICE_FADE + crate::ui::NOTICE + crate::ui::NOTICE_FADE).as_millis() as i64
    }

    #[test]
    fn a_notice_takes_the_middle_while_it_lasts() {
        let (mut ui, now) = scene();
        ui.notify(bingo_sdk::Level::Warn, "estimating", now.instant);
        assert!(text(&solo(&state()), &ui, 80).contains("estimating"));
        ui.expire(later(now, notice_life()));
        assert!(!text(&solo(&state()), &ui, 80).contains("estimating"));
    }

    /// Every slot filled: the mode the child's policy published, three
    /// notices, and where the person is with the model that answers there.
    fn every_slot() -> (Tree, Ui) {
        let mut tree = folded_tree(vec![
            // The root is at work, and it is not the session in view.
            frame(1, started("trn_1")),
            child_frame(1, announced("reviewer")),
            child_frame(2, permission_view("acceptEdits")),
            child_frame(
                3,
                Event::TurnUsage {
                    turn: TurnId::from_raw("trn_9"),
                    usage: Default::default(),
                    context: ContextUsage {
                        used: 150_000,
                        window: 200_000,
                        trigger: 180_000,
                    },
                },
            ),
            // A room wants a person, somewhere else in the tree.
            log_frame(1, log_announced("#design")),
            log_frame(2, opened(room_question())),
        ]);
        tree.show(&child_id());
        let (mut ui, now) = scene();
        ui.notify(bingo_sdk::Level::Info, "estimating", now.instant);
        (tree, ui)
    }

    /// A room asking something, so a session other than the one in view
    /// wants a person.
    fn room_question() -> bingo_sdk::Interaction {
        bingo_sdk::Interaction {
            id: bingo_sdk::InteractionId::from_raw("int_3"),
            session: log_id(),
            ..confirm()
        }
    }

    #[test]
    fn every_slot_filled_and_every_slot_empty() {
        let (tree, ui) = every_slot();
        let filled: Vec<String> = [80usize, 120]
            .iter()
            .map(|width| text(&tree, &ui, *width))
            .collect();
        assert_eq!(filled[0].width(), 80);
        assert_eq!(filled[1].width(), 120);
        insta::assert_snapshot!("every_slot_filled", filled.join("\n"));

        // Nothing published, nothing running, nothing to say, and something
        // half-typed — so even the hint is untrue.
        let bare = solo(&state_without_a_model());
        let (mut ui, now) = scene();
        write(&mut ui, bare.viewed(), "half a thought", now);
        let empty: Vec<String> = [80usize, 120]
            .iter()
            .map(|width| text(&bare, &ui, *width))
            .collect();
        assert_eq!(empty[0], " ".repeat(80));
        assert_eq!(empty[1], " ".repeat(120));
        insta::assert_snapshot!("every_slot_empty", empty.join("\n"));
    }

    fn state_without_a_model() -> bingo_sdk::SessionState {
        let mut state = state();
        state.summary.model = None;
        state
    }

    /// Who serves it, which model, and how hard it thinks — the three facts
    /// the right slot carries, each from the frame that says it.
    #[test]
    fn the_right_slot_says_provider_model_and_effort() {
        let (ui, _) = scene();
        let idle = text(&solo(&state()), &ui, 80);
        assert!(idle.ends_with("fake/fake-1"), "{idle}");

        let thinking = solo(&folded(vec![frame(
            1,
            thinking_view(Some(bingo_sdk::Effort::XHigh)),
        )]));
        let drawn = text(&thinking, &ui, 80);
        assert!(drawn.ends_with("fake/fake-1 · xhigh"), "{drawn}");

        let off = solo(&folded(vec![frame(1, thinking_view(None))]));
        let drawn = text(&off, &ui, 80);
        assert!(
            drawn.ends_with("fake/fake-1"),
            "a turn that asks for no effort shows none: {drawn}"
        );
    }

    /// `/rename` reaches the screen the way every other fact does: the
    /// kernel publishes a `SessionUpdated` and the slot reads it at render
    /// time, so nothing here remembers a name.
    #[test]
    fn a_renamed_session_is_renamed_on_the_line() {
        let mut tree = folded_tree(vec![
            child_frame(1, announced("reviewer")),
            child_frame(2, announced("the release")),
        ]);
        tree.show(&child_id());
        let (ui, _) = scene();
        let drawn = text(&tree, &ui, 80);
        assert!(drawn.contains("in the release · fake/fake-1"), "{drawn}");
    }

    /// A summary written before the kernel stamped the provider on it.
    #[test]
    fn a_model_with_no_provider_stands_alone() {
        let (ui, _) = scene();
        let mut state = state();
        state.summary.provider = None;
        let drawn = text(&solo(&state), &ui, 80);
        assert!(drawn.ends_with("fake-1"), "{drawn}");
        assert!(!drawn.contains("/fake-1"), "{drawn}");
    }

    #[test]
    fn the_middle_is_the_slot_that_gives_way() {
        let tree = solo(&with_permission_mode("bypassPermissions"));
        let (mut ui, now) = scene();
        ui.notify(
            bingo_sdk::Level::Info,
            "a notice far longer than any narrow terminal could hold",
            now.instant,
        );
        for width in [40usize, 60, 80] {
            let drawn = text(&tree, &ui, width);
            assert_eq!(drawn.width(), width, "{width}: {drawn}");
            assert!(drawn.ends_with("fake-1"), "{width}: {drawn}");
        }
        assert!(
            text(&tree, &ui, 60).contains('…'),
            "the middle is cut, not dropped, while there is room for it"
        );
    }

    #[test]
    fn a_line_too_narrow_for_three_slots_keeps_the_place() {
        let tree = solo(&with_permission_mode("bypassPermissions"));
        let (ui, _) = scene();
        for width in [1usize, 6, 12, 20] {
            let drawn = text(&tree, &ui, width);
            assert!(drawn.width() <= width, "{width}: {drawn}");
        }
    }
}
