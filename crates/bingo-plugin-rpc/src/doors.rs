//! `bingo.host`: the one service the host itself answers (ADR-0033 §1).
//!
//! It is a service like any other. The transport is ADR-0031's lane unchanged
//! — a process sends `service/call` with this key and the hub routes it — and
//! the bridge enters it in the registry through `open_service`, so the key is
//! the host's and no plugin can publish under it. There is no new wire method
//! and no handshake field: a process discovers these doors by calling them,
//! and a method that is not one is answered with the set that is, in the words
//! M28 already refuses an unknown method with.
//!
//! Two doors, and what scopes each is the whole design. `ask` puts a question
//! to the person on a call this process is already running: the call is the
//! grant, because the bridge tracks running calls anyway (ADR-0033 §3 as
//! amended), so nothing is minted, an ended call is not there to ask on, and
//! another connection's call is not in this connection's map at all. `notice`
//! is scoped by nothing: it spends a line and nothing else (§4).
//!
//! Who is asking is bound at the face, not carried in the params — a process
//! that could name itself could name another. [`Doors`] is one object with the
//! doors written once; the registry holds a face bound to this process itself,
//! and each connection's hub holds a face bound to that connection. Two faces
//! of one live object is what `Services` already keeps for every service
//! (ADR-0031 §3).
//!
//! The shapes live here rather than in [`crate::wire`] because they are not
//! the wire's: they ride inside one `service/call`'s opaque params, exactly as
//! any other service's do. [`METHODS`] is this service's one table — the
//! schema writes it down and [`Doors::answer`] dispatches on it.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::{Answer, AnswerSpec, InteractionKind, Level, ServiceError, ToolHost, WireService};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::Bridge;
use crate::notice::{Notice, Notices};
use crate::wire::{Method, schema_of};

/// The key the host's own service is registered under. Reserved: a plugin
/// that declares it is refused the way any second claimant is.
pub const KEY: &str = "bingo.host";
/// Put a question to the person, on the crossing that is already running.
pub const ASK: &str = "ask";
/// Say one line to the person, at any time.
pub const NOTICE: &str = "notice";

/// `bingo.host.ask`: a question from the process, on one of its running calls.
///
/// The call names the crossing and is the whole of the grant: the bridge
/// already tracks running calls, an ended one is not there to ask on, and
/// another connection's is not this caller's to reach (ADR-0033 §3 as
/// amended). Nothing is minted for it.
///
/// The question is the sdk's own `InteractionKind`, so the person is asked by
/// the machinery every in-process tool's question rides. Only a question
/// crosses: a permission, a confirmation or a login is the host's own to open,
/// and a grant is never Allow-shaped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AskParams {
    /// The `callId` this process was given in `tool/call`.
    pub call: String,
    pub question: InteractionKind,
}

/// What the person answered, as the sdk writes it — including the cancel that
/// is an answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AskResult {
    pub answer: Answer,
}

/// `bingo.host.notice`: one line for the person, under the plugin's own name.
///
/// The one door that is scoped by nothing (ADR-0033 §4): it spends nothing but
/// a line, so it needs no crossing to belong to. The level is the kernel's own
/// three and nothing else can be written; an omitted one is the quietest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeParams {
    #[serde(default = "quietest")]
    pub level: Level,
    pub message: String,
}

fn quietest() -> Level {
    Level::Info
}

/// A notice is told, not asked: there is nothing to answer with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoticeResult {}

/// The doors, in one table: the schema walks it and [`Doors::answer`]
/// dispatches on it, so the set a caller is told about is the set it can call.
pub static METHODS: &[Method] = &[
    (ASK, schema_of::<AskParams>, schema_of::<AskResult>),
    (NOTICE, schema_of::<NoticeParams>, schema_of::<NoticeResult>),
];

/// Who is at the door. A process speaks through the bridge that runs it, which
/// is what makes another process's call refusable; the host itself speaks for
/// no plugin and runs no plugin's call.
#[derive(Clone)]
pub enum Caller {
    Plugin { name: String, bridge: Weak<Bridge> },
    Host,
}

impl Caller {
    pub fn plugin(name: &str, bridge: Weak<Bridge>) -> Self {
        Caller::Plugin {
            name: name.to_string(),
            bridge,
        }
    }

    /// Whose name a line is said under.
    fn speaker(&self) -> &str {
        match self {
            Caller::Plugin { name, .. } => name,
            Caller::Host => "the host",
        }
    }

    /// The asking machinery of one call this caller is running, or why there
    /// is none. A call that has ended and a call that was never this caller's
    /// are the same answer on purpose: this map is one connection's, so a
    /// process learns nothing about another's calls, not even that they exist.
    async fn running(&self, call: &str) -> Result<Arc<dyn ToolHost>, ServiceError> {
        let Caller::Plugin { name, bridge } = self else {
            return Err(refused("the host is running no call of its own to ask on"));
        };
        let bridge = bridge
            .upgrade()
            .ok_or_else(|| refused(format!("the {name} plugin is gone")))?;
        let connection = bridge
            .connection()
            .await
            .ok_or_else(|| refused(format!("the {name} plugin is not running")))?;
        connection.asking(call).ok_or_else(|| {
            refused(format!(
                "the call {call} is not one the {name} plugin is running: it has ended, or it was never this plugin's"
            ))
        })
    }
}

/// The doors, written once. What every caller shares is the one notice
/// channel; who is calling arrives with the call.
pub struct Doors {
    notices: Arc<Notices>,
}

impl Doors {
    pub fn new(notices: Arc<Notices>) -> Arc<Self> {
        Arc::new(Self { notices })
    }

    /// This caller's face of these doors.
    pub fn face(self: &Arc<Self>, caller: Caller) -> Arc<dyn WireService> {
        Arc::new(Door {
            doors: Arc::clone(self),
            caller,
        })
    }

    async fn answer(
        &self,
        caller: &Caller,
        method: &str,
        params: Value,
    ) -> Result<Value, ServiceError> {
        match method {
            ASK => self.ask(caller, params).await,
            NOTICE => self.notice(caller, params),
            other => Err(unknown(other)),
        }
    }

    /// One question, on one running call, through that call's own host: the
    /// same path an in-process tool's question takes, so a person answers it
    /// where they answer every other.
    async fn ask(&self, caller: &Caller, params: Value) -> Result<Value, ServiceError> {
        let params: AskParams = read(params)?;
        let question = only_a_question(params.question)?;
        let call = caller.running(&params.call).await?;
        let answer = call
            .ask(question.clone(), answers(&question))
            .await
            .map_err(|error| refused(error.to_string()))?;
        write(AskResult { answer })
    }

    /// One line, under the caller's own name, on the crate's one notice
    /// channel — which the drain says whether or not anything else is running.
    fn notice(&self, caller: &Caller, params: Value) -> Result<Value, ServiceError> {
        let params: NoticeParams = read(params)?;
        self.notices.push(Notice::said(
            caller.speaker(),
            params.level,
            &params.message,
        ));
        write(NoticeResult {})
    }
}

/// One caller's face of the doors. Mechanical: it binds who is asking and
/// changes nothing else.
struct Door {
    doors: Arc<Doors>,
    caller: Caller,
}

#[async_trait]
impl WireService for Door {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ServiceError> {
        self.doors.answer(&self.caller, method, params).await
    }
}

/// A door this service does not have, answered with the ones it does — the
/// same sentence a plugin's own service refuses an unknown method with
/// (ADR-0031 §5).
fn unknown(method: &str) -> ServiceError {
    let spoken: Vec<&str> = METHODS.iter().map(|door| door.0).collect();
    refused(format!(
        "the service {KEY} does not speak {method}; it speaks {}",
        spoken.join(", ")
    ))
}

/// A plugin may ask a question. It may not open a permission prompt, the
/// confirmation a tool owns, or a login: those are the host's own to put, and
/// what comes back from one of them is a verdict. The verdict plane is not a
/// door (ADR-0033 Consequences).
fn only_a_question(question: InteractionKind) -> Result<InteractionKind, ServiceError> {
    if matches!(question, InteractionKind::Question { .. }) {
        return Ok(question);
    }
    Err(refused(
        "a plugin may ask a question; a permission, a confirmation and a login are the host's own",
    ))
}

/// The answers the kernel will accept, read off the question rather than
/// stated a second time beside it: options make a choice answerable, and
/// `freeText` asks for words. A question that offers neither is answerable in
/// words anyway — one with no way out is not a question.
fn answers(question: &InteractionKind) -> Vec<AnswerSpec> {
    let InteractionKind::Question {
        options, free_text, ..
    } = question
    else {
        return vec![AnswerSpec::Cancel];
    };
    let mut specs = Vec::new();
    if !options.is_empty() {
        specs.push(AnswerSpec::Choice);
    }
    if *free_text || options.is_empty() {
        specs.push(AnswerSpec::Text);
    }
    specs.push(AnswerSpec::Cancel);
    specs
}

fn read<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ServiceError> {
    serde_json::from_value(params).map_err(|error| refused(error.to_string()))
}

fn write<T: serde::Serialize>(result: T) -> Result<Value, ServiceError> {
    serde_json::to_value(result).map_err(|error| refused(error.to_string()))
}

fn refused(why: impl Into<String>) -> ServiceError {
    ServiceError(why.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Silent, unanswering};
    use bingo_sdk::QuestionOption;
    use serde_json::json;

    fn doors() -> (Arc<Notices>, Arc<Doors>) {
        let notices = Arc::new(Notices::default());
        (Arc::clone(&notices), Doors::new(notices))
    }

    fn question(options: &[&str]) -> Value {
        json!({
            "kind": "question",
            "question": "Which branch?",
            "options": options
                .iter()
                .enumerate()
                .map(|(at, label)| json!({ "id": at.to_string(), "label": label }))
                .collect::<Vec<_>>(),
            "freeText": false,
            "multi": false,
        })
    }

    /// A plugin whose process is alive, with one call filed as running on it.
    /// The guard is returned: the call is over when a test drops it.
    fn running(
        doors: &Arc<Doors>,
        call_id: &str,
        answer: Answer,
    ) -> (Arc<Bridge>, Arc<dyn WireService>, crate::connection::Watch) {
        let connection = unanswering();
        let bridge = Bridge::live("stub", Arc::clone(&connection));
        let (sender, _tail) = tokio::sync::mpsc::unbounded_channel();
        let watch = connection.watch(call_id, sender, Arc::new(Silent(answer)));
        let face = doors.face(Caller::plugin("stub", Arc::downgrade(&bridge)));
        (bridge, face, watch)
    }

    /// The M28 rule, in the same words: a method that is not a door is
    /// answered with the doors that are.
    #[tokio::test]
    async fn a_method_the_host_does_not_speak_is_refused_with_the_set_it_speaks() {
        let (_notices, doors) = doors();
        let why = doors
            .face(Caller::Host)
            .call("complete", Value::Null)
            .await
            .expect_err("there is no such door")
            .to_string();
        assert_eq!(
            why,
            "the service bingo.host does not speak complete; it speaks ask, notice"
        );
    }

    /// The exit criterion: a question on a running call comes back as the
    /// person answered it, having gone through that call's own host.
    #[tokio::test]
    async fn a_question_on_a_running_call_comes_back_as_the_person_answered() {
        let (_notices, doors) = doors();
        let (_bridge, face, _watch) = running(
            &doors,
            "call_1",
            Answer::Text {
                text: "main".into(),
            },
        );
        let answered = face
            .call(
                ASK,
                json!({ "call": "call_1", "question": question(&["main"]) }),
            )
            .await
            .expect("the call is running");
        assert_eq!(
            answered,
            json!({ "answer": { "kind": "text", "text": "main" } })
        );
    }

    /// A call that has ended is not there to ask on, and the words say so.
    #[tokio::test]
    async fn a_call_that_has_ended_is_refused_in_words() {
        let (_notices, doors) = doors();
        let (_bridge, face, watch) = running(&doors, "call_1", Answer::Cancel);
        drop(watch);
        let why = face
            .call(ASK, json!({ "call": "call_1", "question": question(&[]) }))
            .await
            .expect_err("the call is over")
            .to_string();
        assert_eq!(
            why,
            "the call call_1 is not one the stub plugin is running: it has ended, or it was never this plugin's"
        );
    }

    /// The refusal this door exists to make: one plugin's live call is not
    /// another plugin's to ask on, and the second connection's map is where it
    /// is not.
    #[tokio::test]
    async fn another_connection_s_running_call_is_not_this_caller_s_to_ask_on() {
        let (_notices, doors) = doors();
        let (_mine, _face, _watch) = running(&doors, "call_1", Answer::Cancel);
        let theirs = Bridge::live("other", unanswering());
        let face = doors.face(Caller::plugin("other", Arc::downgrade(&theirs)));
        let why = face
            .call(ASK, json!({ "call": "call_1", "question": question(&[]) }))
            .await
            .expect_err("that call is not this plugin's")
            .to_string();
        assert!(why.contains("the other plugin is running"), "{why}");
    }

    /// The host's own face holds no plugin's calls, so it can open no
    /// question: the face exists to hold the key, not to ask on someone's
    /// behalf.
    #[tokio::test]
    async fn the_host_s_own_face_runs_no_call_and_says_so() {
        let (_notices, doors) = doors();
        let why = doors
            .face(Caller::Host)
            .call(ASK, json!({ "call": "call_1", "question": question(&[]) }))
            .await
            .expect_err("the host runs no plugin call")
            .to_string();
        assert_eq!(why, "the host is running no call of its own to ask on");
    }

    /// Not a door for verdicts: what a permission prompt answers with is
    /// `AllowSession`, and no grant is Allow-shaped.
    #[tokio::test]
    async fn a_permission_prompt_is_not_a_question_a_plugin_may_open() {
        let (_notices, doors) = doors();
        let (_bridge, face, _watch) = running(&doors, "call_1", Answer::AllowOnce);
        for kind in [
            json!({ "kind": "permission", "tool": "Bash", "summary": "rm -rf /" }),
            json!({ "kind": "confirm", "title": "Ship it?", "detail": "to production" }),
            json!({ "kind": "login", "provider": "codex", "flow": { "kind": "paste" } }),
        ] {
            let why = face
                .call(ASK, json!({ "call": "call_1", "question": kind }))
                .await
                .expect_err("only a question crosses")
                .to_string();
            assert_eq!(
                why,
                "a plugin may ask a question; a permission, a confirmation and a login are the host's own"
            );
        }
    }

    /// Read off the question: options make a choice answerable, and a person
    /// may always answer in their own words or not at all.
    #[test]
    fn the_answers_a_question_takes_are_read_off_the_question() {
        let with: InteractionKind =
            serde_json::from_value(question(&["main", "next"])).expect("a question");
        assert_eq!(answers(&with), [AnswerSpec::Choice, AnswerSpec::Cancel]);

        let mut asked = question(&["main"]);
        asked["freeText"] = json!(true);
        let worded: InteractionKind = serde_json::from_value(asked).expect("a question");
        assert_eq!(
            answers(&worded),
            [AnswerSpec::Choice, AnswerSpec::Text, AnswerSpec::Cancel]
        );

        let without: InteractionKind = serde_json::from_value(question(&[])).expect("a question");
        assert_eq!(
            answers(&without),
            [AnswerSpec::Text, AnswerSpec::Cancel],
            "a question that offers nothing is still answerable"
        );
    }

    /// The unscoped door: no call, no crossing, nothing minted — and the line
    /// is on the one channel, under the caller's own name.
    #[tokio::test]
    async fn a_notice_needs_no_call_and_lands_on_the_one_channel() {
        let (notices, doors) = doors();
        let (_bridge, face, _watch) = running(&doors, "call_1", Answer::Cancel);
        let answered = face
            .call(
                NOTICE,
                json!({ "level": "warn", "message": "the index is stale" }),
            )
            .await
            .expect("a notice needs nothing");
        assert_eq!(answered, json!({}));
        let said = notices.drain();
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0].level, Level::Warn);
        assert_eq!(said[0].text, "stub: the index is stale");
    }

    #[tokio::test]
    async fn a_line_that_is_not_what_the_door_takes_is_refused_in_words() {
        let (_notices, doors) = doors();
        let face = doors.face(Caller::Host);
        assert!(
            face.call(NOTICE, json!({ "level": "warn" })).await.is_err(),
            "a notice with nothing to say is not one"
        );
        assert!(
            face.call(ASK, json!({ "question": question(&[]) }))
                .await
                .is_err(),
            "an ask names the call it is on"
        );
    }

    // ------------------------------------------------- the shapes on the wire

    /// The `ask` line: the running call that is the whole of the grant, and
    /// the sdk's own question beside it. Nothing else — no allowance, no
    /// session, no turn — because the call names the crossing already.
    #[test]
    fn an_ask_carries_the_running_call_and_the_sdk_s_own_question() {
        let params = AskParams {
            call: "call_1".into(),
            question: InteractionKind::Question {
                question: "Which branch?".into(),
                header: Some("Release".into()),
                options: vec![QuestionOption {
                    id: "0".into(),
                    label: "main".into(),
                    description: None,
                }],
                free_text: true,
                multi: false,
            },
        };
        let wire = serde_json::to_value(&params).expect("an ask serialises");
        assert_eq!(wire["call"], json!("call_1"));
        assert_eq!(wire["question"]["kind"], json!("question"));
        assert_eq!(wire["question"]["freeText"], json!(true));
        assert_eq!(
            serde_json::from_value::<AskParams>(wire).expect("and parses"),
            params
        );
    }

    /// The answer comes back as the sdk writes it, cancel included: a person
    /// who answered nothing has answered.
    #[test]
    fn an_answer_crosses_as_the_sdk_writes_it() {
        assert_eq!(
            serde_json::to_value(AskResult {
                answer: Answer::Text {
                    text: "main".into()
                },
            })
            .expect("an answer serialises"),
            json!({ "answer": { "kind": "text", "text": "main" } })
        );
        let cancelled: AskResult =
            serde_json::from_value(json!({ "answer": { "kind": "cancel" } })).expect("and parses");
        assert_eq!(cancelled.answer, Answer::Cancel);
    }

    /// The verdict plane is not a door (ADR-0033 Consequences): the shape a
    /// process writes can carry a permission prompt's words, so the door
    /// refuses one — `doors` proves that. What is pinned here is that no
    /// answer to it is Allow-shaped by accident: the only Allow a person can
    /// give is to a permission, which is the one kind the door will not open.
    #[test]
    fn a_process_may_write_a_question_and_the_door_is_what_refuses_the_rest() {
        let permission: AskParams = serde_json::from_value(json!({
            "call": "call_1",
            "question": { "kind": "permission", "tool": "Bash", "summary": "rm -rf /" }
        }))
        .expect("the shape parses; the door is what refuses it");
        assert!(matches!(
            permission.question,
            InteractionKind::Permission { .. }
        ));
    }

    /// A notice is told, not asked: a level from the kernel's three, a line,
    /// and an answer with nothing in it.
    #[test]
    fn a_notice_carries_a_level_and_a_line_and_answers_nothing() {
        let params = NoticeParams {
            level: Level::Warn,
            message: "the index is stale".into(),
        };
        let wire = serde_json::to_value(&params).expect("a notice serialises");
        assert_eq!(
            wire,
            json!({ "level": "warn", "message": "the index is stale" })
        );
        assert_eq!(
            serde_json::from_value::<NoticeParams>(wire).expect("and parses"),
            params
        );
        assert_eq!(
            serde_json::to_value(NoticeResult {}).expect("it serialises"),
            json!({})
        );
    }

    /// The set is the kernel's three and nothing else can be written; a
    /// notice that names no level is the quietest one.
    #[test]
    fn a_level_outside_the_kernel_s_three_is_not_a_notice_at_all() {
        let quiet: NoticeParams =
            serde_json::from_value(json!({ "message": "nothing much" })).expect("a notice");
        assert_eq!(quiet.level, Level::Info);
        for shouted in ["fatal", "critical", "WARN", "debug"] {
            assert!(
                serde_json::from_value::<NoticeParams>(json!({ "level": shouted, "message": "x" }))
                    .is_err(),
                "{shouted} was read as a level"
            );
        }
    }

    /// The doors are one table, as the methods are: what the schema tells a
    /// plugin author is what `doors` dispatches on.
    #[test]
    fn the_host_speaks_two_doors_and_no_more() {
        let named: Vec<&str> = METHODS.iter().map(|door| door.0).collect();
        assert_eq!(named, [ASK, NOTICE]);
        assert_eq!(KEY, "bingo.host");
    }

    /// A question a plugin may write, pinned against the sdk's own type: what
    /// crosses is the kernel's vocabulary, not a copy of it.
    #[test]
    fn the_question_that_crosses_is_the_sdk_s_own() {
        let parsed: InteractionKind =
            serde_json::from_value(question(&["main"])).expect("a question");
        assert!(matches!(
            parsed,
            InteractionKind::Question { ref options, .. } if options == &[QuestionOption {
                id: "0".into(),
                label: "main".into(),
                description: None,
            }]
        ));
    }
}
