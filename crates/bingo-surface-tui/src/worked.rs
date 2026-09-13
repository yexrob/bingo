//! What the turn cost, said once it is over (M84): `✻ Worked for 15m 11s ·
//! done 13:48`, Claude Code's own shape. Everything here is words over the
//! last turn the fold kept ([`bingo_sdk::LastTurn`]) and the clock; nothing
//! is stored, so a session read back from its journal says it too.

use std::time::Duration;

use bingo_sdk::{LastTurn, TurnStatus};
use jiff::Timestamp;
use jiff::tz::TimeZone;

/// A turn that answered at once says nothing here, as it drew no verb row
/// while it ran (§6): a row that reports a second reports nothing.
pub const SAY_AFTER: Duration = Duration::from_secs(1);

/// The verb the verdict earns, and the word before the clock: a turn that
/// finished was *done* at that time; one that was stopped or failed was
/// simply at it.
fn verb(status: &TurnStatus) -> (&'static str, &'static str) {
    match status {
        TurnStatus::Completed => ("Worked for", "done "),
        TurnStatus::Interrupted { .. } => ("Stopped after", ""),
        TurnStatus::Failed { .. } => ("Failed after", ""),
    }
}

/// How long the turn ran, from the frame that opened it to the one that
/// closed it.
pub fn ran(turn: &LastTurn) -> Duration {
    turn.ended_at.duration_since(turn.started_at).unsigned_abs()
}

/// `42s`, `15m 11s`, `1h 5m`: two units at most, and the seconds go once
/// the hours arrive — nobody reads the seconds of an hour.
pub fn duration(ran: Duration) -> String {
    let secs = ran.as_secs();
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m"),
    }
}

/// The wall clock the turn ended on, in the zone the terminal keeps: `13:48`,
/// and with the day in front — `Sep 7 13:48` — when that day is not today,
/// which a session read back from its journal may well find.
pub fn clock(ended: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let ended = ended.to_zoned(zone.clone());
    let today = now.to_zoned(zone.clone()).date();
    match ended.date() == today {
        true => ended.strftime("%H:%M").to_string(),
        false => ended.strftime("%b %-d %H:%M").to_string(),
    }
}

/// The row's two parts: what the turn did for how long, and when that was.
pub fn words(turn: &LastTurn, now: Timestamp, zone: &TimeZone) -> (String, String) {
    let (verb, at) = verb(&turn.status);
    (
        format!("{verb} {}", duration(ran(turn))),
        format!(" · {at}{}", clock(turn.ended_at, now, zone)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Now;
    use crate::test_support::*;
    use crate::tree::Tree;
    use bingo_sdk::{ErrorCode, InterruptReason, KernelError, TurnId, Usage};

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + second).expect("an instant")
    }

    fn turn(status: TurnStatus, seconds: i64) -> LastTurn {
        LastTurn {
            id: TurnId::from_raw("trn_1"),
            status,
            started_at: at(0),
            ended_at: at(seconds),
            usage: Usage::default(),
        }
    }

    #[test]
    fn a_duration_is_two_units_at_most() {
        assert_eq!(duration(Duration::from_secs(0)), "0s");
        assert_eq!(duration(Duration::from_secs(42)), "42s");
        assert_eq!(duration(Duration::from_secs(15 * 60 + 11)), "15m 11s");
        assert_eq!(duration(Duration::from_secs(3600 + 5 * 60 + 9)), "1h 5m");
        assert_eq!(duration(Duration::from_secs(26 * 3600)), "26h 0m");
    }

    #[test]
    fn the_clock_is_the_hour_today_and_the_day_before_it_otherwise() {
        let zone = TimeZone::UTC;
        // 2023-11-14 22:13:20 UTC.
        assert_eq!(clock(at(0), at(60), &zone), "22:13");
        assert_eq!(clock(at(0), at(2 * 24 * 3600), &zone), "Nov 14 22:13");
        let tokyo = TimeZone::get("Asia/Tokyo").expect("a zone");
        assert_eq!(clock(at(0), at(60), &tokyo), "07:13");
    }

    #[test]
    fn the_verdict_picks_the_verb() {
        let zone = TimeZone::UTC;
        assert_eq!(
            words(&turn(TurnStatus::Completed, 911), at(911), &zone),
            (
                "Worked for 15m 11s".to_string(),
                " · done 22:28".to_string()
            )
        );
        let stopped = TurnStatus::Interrupted {
            reason: InterruptReason::UserCancel,
        };
        assert_eq!(
            words(&turn(stopped, 42), at(42), &zone).0,
            "Stopped after 42s"
        );
        let failed = TurnStatus::Failed {
            error: KernelError::new(ErrorCode::Internal, "boom"),
        };
        let (what, when) = words(&turn(failed, 7), at(7), &zone);
        assert_eq!(what, "Failed after 7s");
        assert_eq!(when, " · 22:13");
    }

    /// The closing row after a turn, found by its verb, wherever the screen
    /// draws it.
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

    /// Once a turn is over the transcript closes with what it cost (M84):
    /// the verb its verdict earns, how long it ran, and the hour it ended on.
    #[test]
    fn a_finished_turn_says_what_it_cost_under_its_answer() {
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

    /// The next turn takes the closing away. A wait on agents does not: the
    /// turn is over and says what it cost under its answer, and the wait is
    /// the band's own row over the composer.
    #[test]
    fn a_new_turn_takes_the_closing_away_and_a_wait_stands_beside_it() {
        let (ui, now) = scene();
        let mut again = ended(42, bingo_sdk::TurnStatus::Completed);
        again.apply(&frame(3, started("trn_2")));
        let at = later(now, 1_600);
        assert_eq!(after(&again, at, "Worked for"), "");
        assert!(draw_tree(80, 24, &again, &ui, at).contains("esc to interrupt"));

        let mut waited = waited_on(1);
        waited.apply(&frame(8, started("trn_1")));
        let closed = bingo_sdk::Frame {
            ts: ts() + jiff::SignedDuration::from_secs(42),
            ..frame(9, completed("trn_1", bingo_sdk::TurnStatus::Completed))
        };
        waited.apply(&closed);
        assert!(after(&waited, at, "Worked for").contains("Worked for 42s"));
        assert!(draw_tree(80, 24, &waited, &ui, at).contains("Waiting for"));
    }

    /// The closing is the transcript's, not the band's: it stands under the
    /// answer and scrolls with it, above the composer's band and the box.
    #[test]
    fn the_closing_stands_under_the_answer_and_not_over_the_composer() {
        let (ui, now) = scene();
        let now = later(now, 60_000);
        let drawn = draw_tree(
            80,
            24,
            &ended(42, bingo_sdk::TurnStatus::Completed),
            &ui,
            now,
        );
        let rows: Vec<&str> = drawn.lines().map(|l| l.trim_matches('"')).collect();
        let worked = rows
            .iter()
            .position(|l| l.contains("Worked for 42s"))
            .expect("the worked row");
        let composer = rows
            .iter()
            .position(|l| l.contains("ask anything"))
            .expect("the composer");
        assert!(worked < composer, "{drawn}");
        assert!(
            rows[worked + 1..composer]
                .iter()
                .any(|l| l.trim().is_empty()),
            "a blank between the closing and the composer's band: {drawn}"
        );
    }
}
