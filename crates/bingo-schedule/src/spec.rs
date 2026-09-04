//! The grammar and the clock (ADR-0019 §2): the three forms a person may
//! write, and the next time each one fires.
//!
//! Everything here is pure. [`Spec::next_fire`] reckons from a moment it is
//! handed, never from the machine's clock, so a test can stand it either
//! side of midnight, a month end or a DST jump without touching the process.
//! Cron is not here and is not coming until someone needs it.

use std::fmt;
use std::str::FromStr;

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};

const EVERY: &str = "every ";
const DAILY_AT: &str = "daily at ";
const ONCE_AT: &str = "once at ";

/// What every error message ends with: three lines a person can copy.
const FORMS: &str = "expected `every <n>s|m|h`, `daily at HH:MM` or `once at <RFC3339>`";

/// When a schedule fires. `every` counts from the last fire; the other two
/// name a wall-clock moment, and a wall clock is what a DST jump moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spec {
    /// This long after the last fire, whatever the calendar does meanwhile.
    Every(SignedDuration),
    /// This time of day, in the zone the reckoning moment is in.
    DailyAt(Time),
    /// This instant, once.
    OnceAt(Timestamp),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SpecError(String);

impl SpecError {
    fn new(what: impl fmt::Display) -> Self {
        Self(format!("{what} ({FORMS})"))
    }
}

impl Spec {
    /// The first time this fires strictly after `after`, in `after`'s zone.
    /// `None` when it never fires again: a `once at` already past, or a
    /// reckoning that leaves the calendar jiff can represent.
    pub fn next_fire(&self, after: &Zoned) -> Option<Zoned> {
        match self {
            // A non-positive interval is refused at the parse, and refused
            // again here: a schedule that fires the moment it fired would
            // spin the runner rather than run anything.
            Spec::Every(every) if every.is_positive() => after.checked_add(*every).ok(),
            Spec::Every(_) => None,
            Spec::DailyAt(time) => next_daily(*time, after),
            Spec::OnceAt(at) => {
                (*at > after.timestamp()).then(|| at.to_zoned(after.time_zone().clone()))
            }
        }
    }

    /// Whether firing is the last thing it does (ADR-0019 §3).
    pub fn is_once(&self) -> bool {
        matches!(self, Spec::OnceAt(_))
    }
}

/// The next `time` of day after `after`: today's if it is still to come,
/// else tomorrow's.
///
/// Both are built through the zone, so the hour a DST jump deleted resolves
/// forward to the hour that replaced it, and the hour it repeated fires on
/// the first of the two — the schedule fires once a day either way.
fn next_daily(time: Time, after: &Zoned) -> Option<Zoned> {
    let today = at(after.date(), time, after.time_zone())?;
    if today > *after {
        return Some(today);
    }
    at(after.date().tomorrow().ok()?, time, after.time_zone())
}

fn at(date: Date, time: Time, tz: &TimeZone) -> Option<Zoned> {
    tz.to_zoned(date.to_datetime(time)).ok()
}

impl FromStr for Spec {
    type Err = SpecError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let line = s.trim();
        if let Some(rest) = line.strip_prefix(EVERY) {
            return every(rest.trim());
        }
        if let Some(rest) = line.strip_prefix(DAILY_AT) {
            return daily_at(rest.trim());
        }
        if let Some(rest) = line.strip_prefix(ONCE_AT) {
            return once_at(rest.trim());
        }
        Err(SpecError::new(format!("`{line}` is not a schedule")))
    }
}

/// `every 45s`, `every 30m`, `every 2h`.
fn every(rest: &str) -> Result<Spec, SpecError> {
    duration(rest).map(Spec::Every).map_err(SpecError::new)
}

/// A written length of time: `45s`, `30m`, `2h`. Days are not a unit here: a
/// day is a civil thing that DST makes longer or shorter, and `daily at` is
/// where that belongs.
///
/// `every` and a wake's `after` are the same words, so they are one parse.
/// The reason comes back bare: each caller says what it was reading.
pub fn duration(written: &str) -> Result<SignedDuration, String> {
    let rest = written.trim();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let (count, unit) = rest.split_at(digits);
    let seconds = match unit.trim() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        other => return Err(format!("`{other}` is not a unit of time")),
    };
    let count: i64 = count
        .parse()
        .map_err(|_| format!("`{rest}` has no count"))?;
    if count < 1 {
        return Err("an interval of nothing never comes round".to_string());
    }
    let total = count
        .checked_mul(seconds)
        .ok_or_else(|| format!("`{rest}` is longer than a clock can hold"))?;
    Ok(SignedDuration::from_secs(total))
}

/// `daily at 09:00`, in the zone the machine is in when it fires.
fn daily_at(rest: &str) -> Result<Spec, SpecError> {
    let (hour, minute) = rest
        .split_once(':')
        .ok_or_else(|| SpecError::new(format!("`{rest}` is not a time of day")))?;
    let parsed = |part: &str| {
        part.parse::<i8>()
            .map_err(|_| SpecError::new(format!("`{rest}` is not a time of day")))
    };
    Time::new(parsed(hour)?, parsed(minute)?, 0, 0)
        .map(Spec::DailyAt)
        .map_err(|_| SpecError::new(format!("there is no {rest} in a day")))
}

/// `once at 2026-09-01T09:00:00-07:00`: an instant, offset and all.
fn once_at(rest: &str) -> Result<Spec, SpecError> {
    rest.parse::<Timestamp>()
        .map(Spec::OnceAt)
        .map_err(|_| SpecError::new(format!("`{rest}` is not an RFC3339 timestamp")))
}

/// What a person reads back, and what the entry file holds: the same three
/// forms, so a spec survives a round trip through the store.
impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Spec::Every(every) => write!(f, "{EVERY}{}", interval(*every)),
            Spec::DailyAt(time) => {
                write!(f, "{DAILY_AT}{:02}:{:02}", time.hour(), time.minute())
            }
            Spec::OnceAt(at) => write!(f, "{ONCE_AT}{at}"),
        }
    }
}

/// The written interval, in the largest unit that divides it exactly: what
/// was written comes back as it was written, and `every 60m` comes back as
/// the hour it is.
fn interval(every: SignedDuration) -> String {
    let seconds = every.as_secs();
    for (unit, size) in [("h", 3600), ("m", 60)] {
        if seconds % size == 0 {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

impl serde::Serialize for Spec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Spec {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A zone that jumps, without a tzdb to depend on: 2am on the second
    /// Sunday in March, back on the first in November — the United States'
    /// rule, which is the one every DST bug is first found in.
    fn jumpy() -> TimeZone {
        TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").expect("a posix zone")
    }

    fn zoned(s: &str, tz: &TimeZone) -> Zoned {
        let civil: jiff::civil::DateTime = s.parse().expect("a civil datetime");
        tz.to_zoned(civil).expect("a moment in the zone")
    }

    fn spec(s: &str) -> Spec {
        s.parse().expect("a spec")
    }

    /// The fixture table: every form, as it is written and as it reads back.
    #[test]
    fn the_three_forms_round_trip_through_their_own_words() {
        for written in [
            "every 45s",
            "every 30m",
            "every 2h",
            "daily at 09:00",
            "daily at 00:00",
            "daily at 23:59",
            "once at 2026-09-01T16:00:00Z",
        ] {
            let parsed = spec(written);
            assert_eq!(parsed.to_string(), written);
            assert_eq!(written.parse::<Spec>(), Ok(parsed));
        }
    }

    #[test]
    fn an_interval_reads_back_in_the_unit_that_divides_it() {
        assert_eq!(spec("every 60m").to_string(), "every 1h");
        assert_eq!(spec("every 90m").to_string(), "every 90m");
        assert_eq!(spec("every 120s").to_string(), "every 2m");
        assert_eq!(spec("every 3661s").to_string(), "every 3661s");
    }

    #[test]
    fn an_offset_instant_reads_back_as_the_instant_it_is() {
        let parsed = spec("once at 2026-09-01T09:00:00-07:00");
        assert_eq!(parsed.to_string(), "once at 2026-09-01T16:00:00Z");
        assert_eq!(parsed.to_string().parse::<Spec>(), Ok(parsed));
    }

    #[test]
    fn what_is_not_a_spec_says_what_one_is() {
        for wrong in [
            "",
            "* * * * *",
            "every",
            "every 5",
            "every 0s",
            "every -1m",
            "every 2d",
            "every 2 weeks",
            "daily at noon",
            "daily at 25:00",
            "daily at 09",
            "once at tomorrow",
            "once at 2026-09-01",
        ] {
            let error = wrong.parse::<Spec>().expect_err(wrong).to_string();
            assert!(error.contains("daily at HH:MM"), "{wrong}: {error}");
        }
    }

    #[test]
    fn a_day_is_not_a_unit_but_daily_at_is_named_when_one_is_asked_for() {
        let error = "every 2d".parse::<Spec>().expect_err("no days").to_string();
        assert!(error.contains("`d` is not a unit of time"), "{error}");
        assert!(error.contains("daily at"), "{error}");
    }

    /// The one parse `every` and a wake's `after` share, read bare.
    #[test]
    fn a_written_length_of_time_is_the_seconds_it_names() {
        for (written, seconds) in [("45s", 45), ("30m", 1800), ("2h", 7200), (" 1h ", 3600)] {
            assert_eq!(duration(written), Ok(SignedDuration::from_secs(seconds)));
        }
        for wrong in ["", "5", "2d", "0s", "-1m", "many hours"] {
            assert!(duration(wrong).is_err(), "{wrong}");
        }
        assert_eq!(
            duration("2d"),
            Err("`d` is not a unit of time".to_string()),
            "the reason is bare: the caller says what it was reading"
        );
    }

    #[test]
    fn an_interval_fires_that_long_after_the_moment_it_is_given() {
        let tz = jumpy();
        let now = zoned("2026-06-01T10:00:00", &tz);
        let next = spec("every 30m").next_fire(&now).expect("a next fire");
        assert_eq!(next, zoned("2026-06-01T10:30:00", &tz));
    }

    #[test]
    fn a_daily_fires_today_when_the_hour_is_still_to_come_and_tomorrow_when_it_is_not() {
        let tz = jumpy();
        let daily = spec("daily at 09:00");
        let before = daily
            .next_fire(&zoned("2026-06-01T08:59:00", &tz))
            .expect("today's");
        assert_eq!(before.date().to_string(), "2026-06-01");
        assert_eq!((before.hour(), before.minute()), (9, 0));

        let after = daily
            .next_fire(&zoned("2026-06-01T09:00:00", &tz))
            .expect("tomorrow's");
        assert_eq!(
            after.date().to_string(),
            "2026-06-02",
            "the hour itself is past"
        );
    }

    #[test]
    fn a_daily_at_midnight_crosses_the_day_and_the_month_and_the_year() {
        let tz = jumpy();
        let midnight = spec("daily at 00:00");
        for (now, expected) in [
            ("2026-01-31T23:59:59", "2026-02-01"),
            ("2026-02-28T12:00:00", "2026-03-01"),
            ("2028-02-28T12:00:00", "2028-02-29"),
            ("2026-12-31T18:00:00", "2027-01-01"),
            ("2026-06-01T00:00:00", "2026-06-02"),
        ] {
            let next = midnight.next_fire(&zoned(now, &tz)).expect("a next fire");
            assert_eq!(next.date().to_string(), expected, "from {now}");
            assert_eq!((next.hour(), next.minute()), (0, 0), "from {now}");
        }
    }

    /// 2026-03-08: the clocks go 01:59 → 03:00, so there is no 02:30 that
    /// day. The fire is not skipped and is not doubled: it lands on the hour
    /// that replaced it.
    #[test]
    fn an_hour_daylight_saving_deleted_fires_at_the_hour_that_replaced_it() {
        let tz = jumpy();
        let next = spec("daily at 02:30")
            .next_fire(&zoned("2026-03-08T00:30:00", &tz))
            .expect("a next fire");
        assert_eq!(next.date().to_string(), "2026-03-08");
        assert_eq!((next.hour(), next.minute()), (3, 30), "{next}");
    }

    /// 2026-11-01: 01:30 happens twice. The first one is the fire; the
    /// second is already past it, so the day gets one fire, not two.
    #[test]
    fn an_hour_daylight_saving_repeated_fires_once() {
        let tz = jumpy();
        let daily = spec("daily at 01:30");
        let first = daily
            .next_fire(&zoned("2026-11-01T00:30:00", &tz))
            .expect("the first 01:30");
        assert_eq!(
            first.offset().seconds(),
            -4 * 3600,
            "the first 01:30, still on summer time: {first}"
        );
        let second = daily.next_fire(&first).expect("the day after");
        assert_eq!(second.date().to_string(), "2026-11-02", "{second}");
    }

    #[test]
    fn an_interval_across_the_jump_is_the_elapsed_time_it_says() {
        let tz = jumpy();
        let next = spec("every 2h")
            .next_fire(&zoned("2026-03-08T01:00:00", &tz))
            .expect("a next fire");
        // Two hours after 01:00 EST is 04:00 EDT: the wall clock moved three
        // hours because one of them did not exist.
        assert_eq!((next.hour(), next.minute()), (4, 0), "{next}");
    }

    #[test]
    fn a_once_fires_when_it_comes_and_never_again() {
        let tz = jumpy();
        let once = spec("once at 2026-09-01T16:00:00Z");
        assert!(once.is_once());
        let before = zoned("2026-09-01T08:00:00", &tz);
        let fire = once.next_fire(&before).expect("it is still to come");
        assert_eq!(fire.timestamp().to_string(), "2026-09-01T16:00:00Z");
        assert_eq!(once.next_fire(&fire), None, "it has already fired");
        assert!(!spec("every 1h").is_once() && !spec("daily at 09:00").is_once());
    }

    /// A minute of the epoch to a minute of the year 3000, so a property
    /// runs over midnights, month ends, leap days and both DST edges.
    fn moments() -> impl Strategy<Value = Zoned> {
        (0i64..32_503_680_000i64).prop_map(|seconds| {
            Timestamp::from_second(seconds)
                .expect("a timestamp in range")
                .to_zoned(jumpy())
        })
    }

    fn specs() -> impl Strategy<Value = Spec> {
        prop_oneof![
            (1i64..100_000).prop_map(|s| Spec::Every(SignedDuration::from_secs(s))),
            (0i8..24, 0i8..60)
                .prop_map(|(h, m)| Spec::DailyAt(Time::new(h, m, 0, 0).expect("a time of day"))),
        ]
    }

    proptest! {
        /// The one thing every form owes the runner: the next fire is in the
        /// future. A fire that is not would be fired again the moment it was.
        #[test]
        fn a_next_fire_is_always_after_the_moment_it_is_reckoned_from(
            spec in specs(), now in moments()
        ) {
            if let Some(next) = spec.next_fire(&now) {
                prop_assert!(next > now, "{spec} from {now} gave {next}");
            }
        }

        /// A daily lands on the day it said, at the minute it said — unless
        /// that minute did not exist, when it lands after it and inside the
        /// same day.
        #[test]
        fn a_daily_is_within_a_day_and_a_bit_of_now(
            hour in 0i8..24, minute in 0i8..60, now in moments()
        ) {
            let time = Time::new(hour, minute, 0, 0).expect("a time of day");
            let Some(next) = Spec::DailyAt(time).next_fire(&now) else { return Ok(()) };
            let elapsed = next.timestamp().duration_since(now.timestamp());
            prop_assert!(
                elapsed <= SignedDuration::from_hours(26),
                "{time} from {now} is {elapsed:?} away"
            );
            prop_assert!(next.time() >= time, "a fire never lands before the hour it names");
        }

        /// An interval is elapsed time, not calendar time: it is exactly as
        /// far away as it says, whatever the wall clock did in between.
        #[test]
        fn an_interval_is_exactly_as_far_away_as_it_says(
            seconds in 1i64..100_000, now in moments()
        ) {
            let every = SignedDuration::from_secs(seconds);
            let Some(next) = Spec::Every(every).next_fire(&now) else { return Ok(()) };
            prop_assert_eq!(next.timestamp().duration_since(now.timestamp()), every);
        }

        /// Whatever a spec is, it is the same spec after a trip through the
        /// entry file.
        #[test]
        fn a_spec_survives_the_words_it_is_written_in(spec in specs()) {
            prop_assert_eq!(spec.to_string().parse::<Spec>(), Ok(spec));
        }
    }
}
