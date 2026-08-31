//! Frames a test writes a scenario in. The same shape every surface's tests
//! use: a session state, frames folded into it, and nothing invented that the
//! kernel would not send.

use bingo_sdk::{
    Answer, AnswerSpec, Event, Frame, Interaction, InteractionId, InteractionKind, Item, ItemBody,
    ItemId, ItemStatus, Origin, QuestionOption, ResolvedBy, Seq, SessionId, SessionState,
    SessionSummary, TurnId, TurnOrigin, TurnStatus, Usage,
};
use jiff::Timestamp;

pub const SESSION: &str = "ses_1";
pub const TURN: &str = "trn_1";

pub fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a fixed instant")
}

pub fn summary() -> SessionSummary {
    SessionSummary {
        id: SessionId::from_raw(SESSION),
        key: Some("loopback/oc_1".into()),
        title: None,
        cwd: "/tmp".into(),
        parent: None,
        driver: Default::default(),
        model: Some("fake-1".into()),
        system_extra: None,
        tools: None,
        provider: Some("fake".into()),
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
    }
}

pub fn state() -> SessionState {
    SessionState::new(summary())
}

pub fn frame(seq: u64, event: Event) -> Frame {
    Frame {
        seq: Seq(seq),
        ts: ts(),
        session: SessionId::from_raw(SESSION),
        cause: None,
        event,
    }
}

pub fn assistant(id: &str, text: &str, status: ItemStatus) -> Item {
    Item {
        id: ItemId::from_raw(id),
        turn: Some(TurnId::from_raw(TURN)),
        round: 0,
        status,
        started_at: ts(),
        completed_at: status.is_terminal().then(ts),
        intent: None,
        body: ItemBody::Assistant { text: text.into() },
        meta: Default::default(),
    }
}

pub fn user(id: &str, text: &str, origin: Origin) -> Item {
    Item {
        body: ItemBody::User {
            parts: vec![bingo_sdk::ContentPart::text(text)],
            origin,
        },
        ..assistant(id, "", ItemStatus::Completed)
    }
}

pub fn turn_started(seq: u64) -> Frame {
    frame(
        seq,
        Event::TurnStarted {
            turn: TurnId::from_raw(TURN),
            inputs: Vec::new(),
            origin: TurnOrigin::Submit,
        },
    )
}

pub fn turn_completed(seq: u64) -> Frame {
    turn_ended(seq, TurnStatus::Completed)
}

pub fn turn_ended(seq: u64, status: TurnStatus) -> Frame {
    frame(
        seq,
        Event::TurnCompleted {
            turn: TurnId::from_raw(TURN),
            status,
            usage: Usage::default(),
        },
    )
}

/// The assistant item as it grows: one frame carrying the whole text so far,
/// which is what a client folding deltas ends up with.
pub fn says(seq: u64, id: &str, text: &str) -> Frame {
    frame(
        seq,
        Event::ItemUpdated {
            item: assistant(id, text, ItemStatus::Running),
        },
    )
}

pub fn said(seq: u64, id: &str, text: &str) -> Frame {
    frame(
        seq,
        Event::ItemCompleted {
            item: assistant(id, text, ItemStatus::Completed),
        },
    )
}

pub fn permission(scope: Option<&str>) -> Interaction {
    Interaction {
        id: InteractionId::from_raw("int_1"),
        session: SessionId::from_raw(SESSION),
        turn: Some(TurnId::from_raw(TURN)),
        item: Some(ItemId::from_raw("itm_2")),
        opened_at: ts(),
        guard_until: None,
        expires_at: None,
        kind: InteractionKind::Permission {
            tool: "Bash".into(),
            summary: "run `cargo test`".into(),
            preview: None,
            session_scope: scope.map(str::to_owned),
        },
        answers: vec![
            AnswerSpec::AllowOnce,
            AnswerSpec::AllowSession,
            AnswerSpec::Deny,
        ],
    }
}

pub fn choice_question(options: &[(&str, &str)]) -> Interaction {
    Interaction {
        kind: InteractionKind::Question {
            question: "Which file?".into(),
            header: None,
            options: options
                .iter()
                .map(|(id, label)| QuestionOption {
                    id: (*id).into(),
                    label: (*label).into(),
                    description: None,
                })
                .collect(),
            free_text: false,
            multi: false,
        },
        answers: vec![AnswerSpec::Choice, AnswerSpec::Cancel],
        ..permission(None)
    }
}

pub fn asks(seq: u64, interaction: Interaction) -> Frame {
    frame(seq, Event::InteractionOpened { interaction })
}

pub fn resolved(seq: u64, answer: Answer, by: ResolvedBy) -> Frame {
    frame(
        seq,
        Event::InteractionResolved {
            id: InteractionId::from_raw("int_1"),
            answer,
            by,
        },
    )
}

pub fn in_the_tui() -> ResolvedBy {
    ResolvedBy::Client {
        name: "tui".into(),
        surface: "tui".into(),
    }
}
