//! `Wake`: the model hands work to a later turn of this same conversation
//! (ADR-0019 §8). One wake stands per session; a second call takes the first
//! one's place, and `stop` leaves none standing.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::render;
use crate::schedules::Schedules;
use crate::wake::{self, Held, WAKE_LEAST, WAKE_MOST, Wake};

/// The whole of the loop discipline the harness asks for, said where the
/// model reads it rather than in a document it may never see.
const DESCRIPTION: &str = "\
Wake yourself later in this same conversation. `after` is how long to wait \
(10s at the least, 1h at the most; anything outside is clamped and the \
result says so) and `note` is the line the next turn opens with — your own \
words to yourself, since nothing else of this turn is repeated for you. One \
wake stands at a time: setting another replaces it, and `stop: true` cancels \
the one that stands. It arrives only once this turn has ended, so finish what \
you are doing.

Use it to pace work you cannot finish now: a build to poll, a job to check \
on, a review to come back to. Before the first wake, decide three things and \
write them into the note — what evidence would prove the work done, what the \
budget is (how many wakes, or by when), and what you will do when the budget \
is spent. Every wake is bounded and idempotent: it checks and reports, it \
does not start the work again. A check that fails goes back to diagnosis, \
not to a shorter interval. When the evidence is there, or the budget is \
spent, stop — say what you found and set no further wake.

The person sees the wake that stands and can end it at any time.";

/// What settings say when wakes are off. Nothing is written, and the model is
/// told whose decision it was rather than left guessing at a failure.
const OFF: &str = "\
Wakes are off here: `schedule.wakes` is false in this person's settings. \
Finish what you can in this turn and say what is left, or ask them to turn \
wakes on.";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Ask {
    /// How long to wait: `30s`, `5m`, `1h`. Held between 10s and 1h.
    /// Required unless `stop` is true.
    #[serde(default)]
    after: Option<String>,
    /// What the next turn opens with — a line to yourself saying what to
    /// check and what you decided. Required unless `stop` is true.
    #[serde(default)]
    note: Option<String>,
    /// Cancel the wake that stands and set none. `after` and `note` are
    /// ignored.
    #[serde(default)]
    stop: bool,
}

/// What a call amounts to, once its words are read.
#[derive(Debug, PartialEq, Eq)]
enum Wanted {
    Set { held: Held, note: String },
    Stop,
}

impl Ask {
    /// What this call wants, or the sentence that says why it is not a call.
    /// Pure: the whole of the input's meaning, decided before anything is
    /// read from disk.
    fn wanted(self) -> Result<Wanted, String> {
        if self.stop {
            return Ok(Wanted::Stop);
        }
        let note = self.note.unwrap_or_default().trim().to_string();
        if note.is_empty() {
            return Err(
                "a wake with nothing to say would open a turn about nothing. \
                        Write the note you want to read when it comes."
                    .into(),
            );
        }
        let after = self.after.ok_or_else(|| {
            "a wake needs an `after`: how long to wait, like `30s` or `5m`.".to_string()
        })?;
        let asked = crate::spec::duration(&after).map_err(|why| {
            format!("{why} — `after` is a length of time, like `30s`, `5m` or `1h`.")
        })?;
        Ok(Wanted::Set {
            held: wake::hold(asked),
            note,
        })
    }
}

/// Sets the one wake this session has, or takes it away.
#[derive(Debug)]
pub struct WakeTool {
    schedules: Arc<Schedules>,
    /// `schedule.wakes`: a person may have none of this (ADR-0019 §8).
    wakes: bool,
}

impl WakeTool {
    pub fn new(schedules: Arc<Schedules>, wakes: bool) -> Self {
        Self { schedules, wakes }
    }

    /// A wake takes the place of the one that stood. It is this process's
    /// own, delivered by the process running this session, so whether the
    /// store's schedules fire here has no bearing on it.
    async fn set(&self, held: Held, note: String, cx: &ToolContext) -> ToolOutput {
        let wake = wake::set(Timestamp::now(), held.after, note);
        self.schedules.wakes().set(&cx.session, wake.clone());
        wake::publish(&cx.host, &cx.session, Some(&wake)).await;
        ToolOutput::text(receipt(&wake, held))
    }

    /// Nothing pending is an answer, not a failure: a model that stopped a
    /// wake it had already spent is where it wanted to be.
    async fn stop(&self, cx: &ToolContext) -> ToolOutput {
        let Some(wake) = self.schedules.wakes().take(&cx.session) else {
            return ToolOutput::text("No wake was standing; nothing to cancel.");
        };
        wake::publish(&cx.host, &cx.session, None).await;
        ToolOutput::text(format!(
            "The wake set for {} is cancelled; none stands.",
            when(&wake)
        ))
    }
}

/// When it comes, what it will say, and — where the bounds moved it — what
/// was asked for instead.
fn receipt(wake: &Wake, held: Held) -> String {
    format!(
        "Waking you at {} with: {}{}",
        when(wake),
        render::head(&wake.note, 60),
        clamp(held),
    )
}

/// The moment a wake comes, in the person's own zone.
fn when(wake: &Wake) -> String {
    render::when(Some(&wake.at.to_zoned(TimeZone::system())))
}

/// What the bounds did, said in the same breath as the wake that was set.
fn clamp(held: Held) -> String {
    let Some(asked) = held.clamped else {
        return String::new();
    };
    let bound = match held.after == WAKE_LEAST {
        true => format!("{} is the least a wake may be", written(WAKE_LEAST)),
        false => format!("{} is the most", written(WAKE_MOST)),
    };
    format!(
        " (you asked for {}, and {bound}.)",
        written(asked.max(SignedDuration::ZERO))
    )
}

/// A length of time in the words the grammar takes back (`spec::duration`).
fn written(span: SignedDuration) -> String {
    let seconds = span.as_secs();
    for (unit, size) in [("h", 3600), ("m", 60)] {
        if seconds % size == 0 && seconds >= size {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

#[async_trait]
impl Tool for WakeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Wake".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Ask>(),
            meta: Default::default(),
        }
    }

    /// A wake touches nothing but this plugin's own memory and posts into
    /// this very session's queue when it comes — the reach `SendMessage`
    /// already has under the same traits (`bingo-tasks`), and what the woken
    /// turn then does is gated in that turn. It is not concurrency-safe: one
    /// wake stands per session, and two calls at once would each set theirs
    /// without the other's.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits {
            read_only: true,
            trusted: true,
            concurrency_safe: false,
            ..ToolTraits::default()
        }
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        if !self.wakes {
            return Ok(ToolOutput::error(OFF));
        }
        let args: Ask =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let wanted = match args.wanted() {
            Ok(wanted) => wanted,
            // Words the tool cannot read are something the model rewrites.
            Err(message) => return Ok(ToolOutput::error(message)),
        };
        Ok(match wanted {
            Wanted::Set { held, note } => self.set(held, note, cx).await,
            Wanted::Stop => self.stop(cx).await,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fixture, text};
    use serde_json::json;

    fn tool(fixture: &Fixture) -> WakeTool {
        WakeTool::new(fixture.schedules.clone(), true)
    }

    async fn wake_call(fixture: &Fixture, input: Value) -> ToolOutput {
        tool(fixture)
            .call(input, &fixture.context())
            .await
            .expect("an answer")
    }

    /// The wake standing on the fixture's session.
    fn standing(fixture: &Fixture) -> Wake {
        fixture
            .schedules
            .wakes()
            .pending(&fixture.context().session)
            .expect("a wake stands")
    }

    #[tokio::test]
    async fn a_wake_stands_on_the_calling_session_and_touches_no_file() {
        let fixture = Fixture::new();
        let before = Timestamp::now();
        let out = wake_call(&fixture, json!({"after": "5m", "note": "look again"})).await;
        assert!(!out.is_error, "{out:?}");
        let wake = standing(&fixture);
        assert_eq!(wake.note, "look again");
        assert!(wake.at >= before + SignedDuration::from_mins(5), "{wake:?}");
        assert!(text(&out).contains("look again"), "{}", text(&out));
        assert!(
            !fixture.dir().exists(),
            "a wake is the session's own, not the store's"
        );

        let published = fixture.host.extended();
        assert_eq!(published.len(), 1, "{published:?}");
        let (session, plugin, kind, payload) = &published[0];
        assert_eq!(session, &fixture.context().session);
        assert_eq!((plugin.as_str(), kind.as_str()), (wake::PLUGIN, wake::KIND));
        assert_eq!(payload[wake::NOTE], "look again");
        assert!(payload[wake::AT].is_string(), "{payload}");
    }

    #[tokio::test]
    async fn a_second_call_takes_the_first_wakes_place() {
        let fixture = Fixture::new();
        wake_call(&fixture, json!({"after": "5m", "note": "the first"})).await;
        wake_call(&fixture, json!({"after": "10m", "note": "the second"})).await;
        assert_eq!(standing(&fixture).note, "the second");
    }

    #[tokio::test]
    async fn stop_leaves_none_standing_and_says_so_when_none_did() {
        let fixture = Fixture::new();
        wake_call(&fixture, json!({"after": "5m", "note": "never mind"})).await;
        let out = wake_call(&fixture, json!({"stop": true})).await;
        assert!(!out.is_error, "{out:?}");
        assert!(text(&out).contains("cancelled"), "{}", text(&out));
        assert_eq!(
            fixture
                .schedules
                .wakes()
                .pending(&fixture.context().session),
            None,
            "none stands"
        );
        assert_eq!(
            fixture.host.extended().last().map(|told| told.3.clone()),
            Some(Value::Null),
            "and the pending wake is taken back"
        );

        let again = wake_call(&fixture, json!({"stop": true})).await;
        assert!(
            !again.is_error,
            "stopping nothing is an answer, not a failure"
        );
        assert!(
            text(&again).contains("No wake was standing"),
            "{}",
            text(&again)
        );
    }

    #[tokio::test]
    async fn a_wake_outside_the_bounds_is_held_and_the_result_says_so() {
        let fixture = Fixture::new();
        let brief = wake_call(&fixture, json!({"after": "1s", "note": "too soon"})).await;
        assert!(
            text(&brief).contains("10s is the least"),
            "{}",
            text(&brief)
        );
        assert!(
            text(&brief).contains("you asked for 1s"),
            "{}",
            text(&brief)
        );
        assert!(
            standing(&fixture).at <= Timestamp::now() + WAKE_LEAST + SignedDuration::from_secs(1),
            "it was still set, at the bound"
        );

        let long = wake_call(&fixture, json!({"after": "9h", "note": "too late"})).await;
        assert!(text(&long).contains("1h is the most"), "{}", text(&long));

        let inside = wake_call(&fixture, json!({"after": "5m", "note": "just right"})).await;
        assert!(!text(&inside).contains("asked for"), "{}", text(&inside));
    }

    #[tokio::test]
    async fn words_the_tool_cannot_read_write_nothing_and_say_what_it_takes() {
        let fixture = Fixture::new();
        for (input, expected) in [
            (json!({"after": "2d", "note": "n"}), "is not a unit of time"),
            (
                json!({"after": "soon", "note": "n"}),
                "is not a unit of time",
            ),
            (json!({"note": "n"}), "needs an `after`"),
            (json!({"after": "5m", "note": "  "}), "nothing to say"),
        ] {
            let out = wake_call(&fixture, input.clone()).await;
            assert!(out.is_error, "{input}");
            assert!(text(&out).contains(expected), "{input}: {}", text(&out));
        }
        assert_eq!(
            fixture
                .schedules
                .wakes()
                .pending(&fixture.context().session),
            None,
            "nothing was set"
        );
    }

    #[tokio::test]
    async fn wakes_a_person_turned_off_are_refused_and_nothing_is_written() {
        let fixture = Fixture::new();
        let out = WakeTool::new(fixture.schedules.clone(), false)
            .call(json!({"after": "5m", "note": "no"}), &fixture.context())
            .await
            .expect("an answer");
        assert!(out.is_error);
        assert!(text(&out).contains("schedule.wakes"), "{}", text(&out));
        assert_eq!(
            fixture
                .schedules
                .wakes()
                .pending(&fixture.context().session),
            None
        );
    }

    #[test]
    fn the_spec_teaches_the_loop_and_asks_nobody() {
        let fixture = Fixture::new();
        let spec = tool(&fixture).spec();
        assert_eq!(spec.name, "Wake");
        assert!(spec.input_schema["properties"]["after"]["description"].is_string());
        assert!(spec.input_schema["properties"]["stop"]["description"].is_string());
        let traits = tool(&fixture).traits(&Value::Null);
        assert!(traits.trusted && traits.read_only);
        assert!(!traits.edit && !traits.destructive && !traits.concurrency_safe);
        insta::assert_snapshot!("wake_description", spec.description);
    }

    #[test]
    fn a_length_of_time_reads_back_in_the_words_the_grammar_takes() {
        assert_eq!(written(SignedDuration::from_secs(1)), "1s");
        assert_eq!(written(SignedDuration::from_secs(45)), "45s");
        assert_eq!(written(SignedDuration::from_mins(5)), "5m");
        assert_eq!(written(SignedDuration::from_hours(9)), "9h");
        assert_eq!(written(SignedDuration::from_secs(90)), "90s");
        assert_eq!(written(SignedDuration::ZERO), "0s");
    }

    #[test]
    fn what_a_call_wants_is_decided_before_anything_is_read() {
        let stop: Ask = serde_json::from_value(json!({"stop": true})).expect("an ask");
        assert_eq!(stop.wanted(), Ok(Wanted::Stop));
        let set: Ask =
            serde_json::from_value(json!({"after": " 5m ", "note": " hi "})).expect("an ask");
        assert_eq!(
            set.wanted(),
            Ok(Wanted::Set {
                held: wake::hold(SignedDuration::from_mins(5)),
                note: "hi".into()
            })
        );
        let both: Ask =
            serde_json::from_value(json!({"after": "nonsense", "stop": true})).expect("an ask");
        assert_eq!(
            both.wanted(),
            Ok(Wanted::Stop),
            "a stop reads nothing else: there is nothing to set"
        );
    }
}
