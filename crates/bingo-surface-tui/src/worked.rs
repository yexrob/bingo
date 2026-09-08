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
}
