use std::time::Duration;

use bingo_sdk::{
    Answer, CancelReason, ErrorCode, Event, InteractionId, KernelError, Level, ResolvedBy,
    TurnStatus,
};

use super::*;
use crate::fixtures::*;
use crate::limits::{Dialect, Encoding};

const HERE: &str = "loopback/oc_1";

fn limits() -> Limits {
    Limits {
        max_text: (4000, Encoding::Chars),
        dialect: Dialect::Markdown,
        max_actions: 3,
        max_label: 40,
    }
}

fn gate() -> Gate {
    Gate {
        min_chars: 20,
        interval: Duration::from_millis(500),
    }
}

/// A conversation being driven: the reducer, the state it folds into, and a
/// clock the test moves by hand.
struct Chat {
    deliverer: Deliverer,
    state: SessionState,
    now: Instant,
}

impl Chat {
    fn new() -> Self {
        Self::with(limits(), gate())
    }

    fn with(limits: Limits, gate: Gate) -> Self {
        Self {
            deliverer: Deliverer::new(limits, gate, HERE),
            state: state(),
            now: Instant::now(),
        }
    }

    fn feed(&mut self, frame: Frame) -> Vec<Op> {
        self.state.apply(&frame);
        self.deliverer.apply(&frame, &self.state, self.now)
    }

    fn wait(&mut self, millis: u64) -> Vec<Op> {
        self.now += Duration::from_millis(millis);
        self.deliverer.tick(self.now)
    }

    fn texts(ops: Vec<Op>) -> Vec<String> {
        ops.into_iter()
            .filter_map(|op| match op {
                Op::Replace { full } => Some(full),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn the_first_words_open_a_message_and_the_gate_holds_the_rest() {
    let mut chat = Chat::new();
    assert!(chat.feed(turn_started(1)).is_empty());
    assert_eq!(
        chat.feed(says(2, "itm_1", "Look")),
        [
            Op::Open,
            Op::Replace {
                full: "Look".into()
            }
        ],
        "a person sees the answer start at once"
    );
    assert!(
        chat.feed(says(3, "itm_1", "Looking")).is_empty(),
        "three more characters is not worth a redraw"
    );
    assert_eq!(
        Chat::texts(chat.feed(says(4, "itm_1", "Looking at the tests."))),
        Vec::<String>::new(),
        "the ASCII full stop is not a boundary"
    );
    assert_eq!(
        Chat::texts(chat.feed(says(5, "itm_1", "Looking at the tests.\n"))),
        ["Looking at the tests."],
        "a newline is"
    );
}

/// A `!` line runs no turn and asks the model for nothing, so a chat is told
/// about it beside the answer rather than inside one (M65).
#[test]
fn a_shell_line_the_person_ran_lands_beside_the_answer() {
    let mut chat = Chat::new();
    let ran = |seq, id, command, output, exit| {
        frame(
            seq,
            Event::ItemCompleted {
                item: shell(id, command, output, exit),
            },
        )
    };
    assert_eq!(
        chat.feed(ran(1, "itm_1", "echo hi", "hi\n", Some(0))),
        [Op::Status {
            text: "$ echo hi\n```\nhi\n```".into()
        }]
    );
    assert_eq!(
        chat.feed(ran(2, "itm_2", "false", "", Some(3))),
        [Op::Status {
            text: "$ false\n[exit 3]".into()
        }],
        "a line that failed says so"
    );
}

#[test]
fn enough_new_characters_open_the_gate_without_a_boundary() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "ab"));
    assert!(chat.feed(says(3, "itm_1", "ab0123456789")).is_empty());
    assert_eq!(
        Chat::texts(chat.feed(says(4, "itm_1", "ab01234567890123456789"))),
        ["ab01234567890123456789"]
    );
}

#[test]
fn the_timer_shows_what_the_other_two_gates_held() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "ab"));
    assert!(chat.feed(says(3, "itm_1", "abc")).is_empty());
    assert!(chat.wait(499).is_empty(), "not yet");
    assert_eq!(Chat::texts(chat.wait(1)), ["abc"]);
    assert!(chat.wait(1000).is_empty(), "nothing is held any more");
}

#[test]
fn only_the_newest_snapshot_is_pending_and_the_older_ones_are_forgotten() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "ab"));
    chat.feed(says(3, "itm_1", "abc"));
    chat.feed(says(4, "itm_1", "abcd"));
    assert_eq!(
        Chat::texts(chat.wait(600)),
        ["abcd"],
        "one pending snapshot per conversation, replaced by newer frames"
    );
}

#[test]
fn the_pending_snapshot_never_overwrites_the_final_text() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "Hel"));
    assert!(
        chat.feed(says(3, "itm_1", "Hell")).is_empty(),
        "held back by the gate"
    );
    assert!(chat.feed(said(4, "itm_1", "Hello there")).is_empty());
    assert_eq!(
        chat.feed(turn_completed(5)),
        [Op::Finalize {
            text: "Hello there".into(),
            question: None,
        }]
    );
    assert!(
        chat.wait(5_000).is_empty(),
        "the queue drained before the finalize, so nothing stale can follow it"
    );
}

#[test]
fn a_turn_with_nothing_to_say_delivers_nothing() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    assert!(chat.feed(turn_completed(2)).is_empty());
}

#[test]
fn a_failed_turn_finalizes_what_there_was_and_then_says_why() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "Trying."));
    chat.feed(said(3, "itm_1", "Trying."));
    assert_eq!(
        chat.feed(turn_ended(
            4,
            TurnStatus::Failed {
                error: KernelError::new(ErrorCode::ProviderUnavailable, "no provider"),
            }
        )),
        [
            Op::Finalize {
                text: "Trying.".into(),
                question: None,
            },
            Op::Status {
                text: "no provider".into(),
            },
        ]
    );
}

#[test]
fn a_warning_is_a_status_beside_the_answer_and_a_note_is_not() {
    let mut chat = Chat::new();
    let notice = |level| {
        frame(
            9,
            Event::Notice {
                level,
                code: "X".into(),
                text: "the model is slow".into(),
            },
        )
    };
    assert_eq!(
        chat.feed(notice(Level::Warn)),
        [Op::Status {
            text: "the model is slow".into()
        }]
    );
    assert!(chat.feed(notice(Level::Info)).is_empty());
}

#[test]
fn a_question_finalizes_the_stream_and_carries_both_rungs() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "I need to run the tests.\n"));
    let ops = chat.feed(asks(3, permission(Some("Bash(cargo test:*)"))));
    let Some(Op::Finalize { text, question }) = ops.first() else {
        panic!("expected a finalize, got {ops:?}");
    };
    assert_eq!(text, "I need to run the tests.");
    let question = question.as_ref().expect("the question rides with it");
    assert_eq!(question.id, InteractionId::from_raw("int_1"));
    assert_eq!(
        question.buttons(&limits()).expect("three fit").len(),
        3,
        "the buttons rung"
    );
    assert!(
        question.numbered().contains("1. Allow once"),
        "the numbered rung: {}",
        question.numbered()
    );
}

#[test]
fn text_after_a_question_opens_a_new_message_and_repeats_nothing() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "First.\n"));
    chat.feed(asks(3, permission(None)));
    chat.feed(resolved(4, Answer::AllowOnce, in_the_tui()));
    assert_eq!(
        chat.feed(said(5, "itm_2", "Second.\n")),
        [
            Op::Open,
            Op::Replace {
                full: "Second.".into()
            }
        ],
        "what the first message already carries is not said twice"
    );
    assert_eq!(
        chat.feed(turn_completed(6)),
        [Op::Finalize {
            text: "Second.".into(),
            question: None,
        }]
    );
}

#[test]
fn a_resolution_anywhere_settles_the_question_this_chat_showed() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(asks(2, permission(None)));
    assert_eq!(
        chat.feed(resolved(3, Answer::AllowOnce, in_the_tui())),
        [Op::Resolved {
            question: InteractionId::from_raw("int_1"),
            outcome: "approved in the TUI".into(),
        }]
    );
    assert!(
        chat.feed(resolved(4, Answer::AllowOnce, in_the_tui()))
            .is_empty(),
        "a question is settled once"
    );
}

#[test]
fn a_resolution_in_this_chat_does_not_claim_to_have_happened_elsewhere() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(asks(2, permission(None)));
    assert_eq!(
        chat.feed(resolved(
            3,
            Answer::Deny { feedback: None },
            ResolvedBy::Client {
                name: HERE.into(),
                surface: "channels".into(),
            }
        )),
        [Op::Resolved {
            question: InteractionId::from_raw("int_1"),
            outcome: "denied".into(),
        }]
    );
}

#[test]
fn a_cancelled_question_takes_its_buttons_with_it() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(asks(2, permission(None)));
    assert_eq!(
        chat.feed(frame(
            3,
            Event::InteractionCancelled {
                id: InteractionId::from_raw("int_1"),
                reason: CancelReason::TurnEnded,
            }
        )),
        [Op::Resolved {
            question: InteractionId::from_raw("int_1"),
            outcome: "withdrawn: the turn ended".into(),
        }]
    );
}

#[test]
fn a_question_this_chat_never_showed_settles_nothing() {
    let mut chat = Chat::new();
    assert!(
        chat.feed(resolved(1, Answer::AllowOnce, in_the_tui()))
            .is_empty()
    );
}

#[test]
fn a_login_has_no_rung_so_the_chat_says_where_to_answer_it() {
    let mut chat = Chat::new();
    let login = bingo_sdk::Interaction {
        kind: bingo_sdk::InteractionKind::Login {
            provider: "codex".into(),
            flow: bingo_sdk::LoginFlow::Paste,
        },
        answers: vec![bingo_sdk::AnswerSpec::Text],
        ..permission(None)
    };
    assert_eq!(
        chat.feed(asks(1, login)),
        [Op::Status {
            text: "codex is asking to be signed in; that has to happen where bingo runs".into(),
        }]
    );
}

#[test]
fn the_platforms_dialect_and_length_are_applied_once_here() {
    let mut chat = Chat::with(
        Limits {
            max_text: (12, Encoding::Chars),
            dialect: Dialect::Plain,
            ..limits()
        },
        gate(),
    );
    chat.feed(turn_started(1));
    chat.feed(said(2, "itm_1", "look:\n```rust\nfn main() {}\n```"));
    assert_eq!(
        chat.feed(turn_completed(3)),
        [Op::Finalize {
            text: "look:\nfn ma…".into(),
            question: None,
        }],
        "the fence is dropped for a plain chat, and what is left is cut to fit"
    );
}

#[test]
fn several_assistant_items_in_one_turn_read_as_one_answer() {
    let mut chat = Chat::new();
    chat.feed(turn_started(1));
    chat.feed(said(2, "itm_1", "First."));
    chat.feed(said(3, "itm_2", "Second."));
    assert_eq!(
        chat.feed(turn_completed(4)),
        [Op::Finalize {
            text: "First.\n\nSecond.".into(),
            question: None,
        }]
    );
}

#[test]
fn the_timer_is_only_armed_while_something_is_held() {
    let mut chat = Chat::new();
    assert!(chat.deliverer.due().is_none());
    chat.feed(turn_started(1));
    chat.feed(says(2, "itm_1", "ab"));
    assert!(chat.deliverer.due().is_none(), "nothing is held yet");
    chat.feed(says(3, "itm_1", "abc"));
    assert!(chat.deliverer.due().is_some());
    chat.wait(600);
    assert!(chat.deliverer.due().is_none());
}
