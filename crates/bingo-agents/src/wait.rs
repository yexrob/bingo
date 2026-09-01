//! `WaitAgent`: hold this turn until the agents it names are idle, and read
//! what each of them said. One deadline covers the whole join, and every
//! outcome is reported in the order it was asked for.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    Interrupt, SessionId, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{names, watch};

const DESCRIPTION: &str = "\
Wait for one or several agents to finish, and read what each of them said. \
They may be ones you started in the background or teammates beside you; an \
agent that is already idle answers at once with what it last said. Name \
several and they are joined: one deadline covers them all, and every outcome \
comes back in the order you named them. Use it only when you cannot go on \
without the result; otherwise let the agents report back on their own and \
keep working. Waiting does not stop them: a timeout leaves them running.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitArgs {
    /// The agents to wait for: the names `SpawnAgent` gave back, or
    /// teammates' names. `ListAgents` names both. Name each one once.
    pub agents: Vec<String>,
    /// Give up after this many seconds — one deadline for the whole call.
    /// Without one, the wait lasts as long as their turns do.
    pub timeout_s: Option<u64>,
}

/// Watching sessions that are already running; it starts nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct WaitAgentTool;

/// The one moment every wait in this call ends at, kept beside the number the
/// caller asked for so the report can say it. Computed once: two agents
/// waited on in turn would otherwise be given `timeout_s` each.
struct Deadline {
    seconds: u64,
    at: tokio::time::Instant,
}

impl Deadline {
    fn new(seconds: u64) -> Self {
        Self {
            seconds,
            at: tokio::time::Instant::now() + Duration::from_secs(seconds),
        }
    }
}

/// What waiting on one agent came to. Only a completed turn is an answer;
/// the rest are the states the caller has to act on rather than read.
#[derive(Debug)]
enum Outcome {
    /// Its turn ended, or it was idle and had already said everything.
    Ended(watch::Reply),
    /// No turn has ever run: the agent is seated and nothing has woken it.
    Seated,
    /// The deadline passed with the agent still at work.
    Working { after: u64 },
}

impl Outcome {
    fn is_answer(&self) -> bool {
        matches!(self, Outcome::Ended(reply) if !reply.is_error())
    }
}

/// The names to wait for, as the call asked for them: at least one, each
/// named once. A name asked for twice would buy two reports of one agent, so
/// it is refused rather than guessed at — and, like an unknown name, refused
/// before anything is waited on.
fn asked(agents: &[String]) -> Result<Vec<&str>, String> {
    let names: Vec<&str> = agents.iter().map(|name| name.trim()).collect();
    if names.is_empty() {
        return Err("name at least one agent to wait for".into());
    }
    for (nth, name) in names.iter().enumerate() {
        if names[..nth].contains(name) {
            return Err(format!("{name} is named twice; name each agent once"));
        }
    }
    Ok(names)
}

/// Every name resolved, in the same address space `SendMessage` has
/// (ADR-0024 §3), before anything is waited on: one unknown name fails the
/// call with the roster hint, and nothing has been waited for behind it.
async fn roster(cx: &ToolContext, names: &[&str]) -> Result<Vec<(String, SessionId)>, String> {
    let mut roster = Vec::with_capacity(names.len());
    for name in names {
        let agent = names::agent(&cx.host, &cx.session, name)
            .await
            .map_err(|e| e.message)?;
        roster.push(((*name).to_string(), agent.id));
    }
    Ok(roster)
}

/// What one agent's wait comes to. An idle agent has already said everything
/// it is going to say; one with no turn behind it has said nothing yet, and
/// will not until something wakes it (ADR-0027).
async fn outcome(
    cx: &ToolContext,
    session: &SessionId,
    deadline: Option<&Deadline>,
) -> Result<Outcome, ToolError> {
    let mut attachment = watch::follow(&cx.host, session)
        .await
        .map_err(|e| ToolError::Failed(e.message))?;
    if attachment.snapshot.turn.is_none() {
        return Ok(match watch::last_reply(&attachment.snapshot) {
            Some(reply) => Outcome::Ended(reply),
            None => Outcome::Seated,
        });
    }
    let wait = watch::next_reply(&cx.host, &mut attachment, &cx.cancel);
    let Some(deadline) = deadline else {
        return wait.await.map(Outcome::Ended);
    };
    match tokio::time::timeout_at(deadline.at, wait).await {
        Ok(reply) => reply.map(Outcome::Ended),
        Err(_) => Ok(Outcome::Working {
            after: deadline.seconds,
        }),
    }
}

/// Every agent waited for at once, under the one deadline: a join lasts as
/// long as the slowest of them, never as long as their sum. Cancelling the
/// turn drops the whole join, and none of the agents.
async fn join(
    cx: &ToolContext,
    roster: &[(String, SessionId)],
    deadline: Option<&Deadline>,
) -> Result<Vec<Outcome>, ToolError> {
    let waits = roster
        .iter()
        .map(|(_, session)| outcome(cx, session, deadline));
    futures::future::try_join_all(waits).await
}

/// One agent's part of the report, in the voice a single wait has always had.
fn section(name: &str, session: &SessionId, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ended(reply) => watch::replied(name, session, reply),
        Outcome::Seated => format!(
            "{name} ({session}) is seated and nothing has woken it; write to it \
             or post in a room it is in."
        ),
        Outcome::Working { after } => format!(
            "{name} ({session}) was still working after {after}s; it is still \
             running, so wait again or write to it"
        ),
    }
}

/// The join as the caller reads it: one section per agent in the order asked,
/// an error result when any of them is not an answer — with what the ones
/// that did finish said still in it, so a deadline never swallows a reply.
fn report(roster: &[(String, SessionId)], outcomes: &[Outcome]) -> ToolOutput {
    let sections: Vec<String> = roster
        .iter()
        .zip(outcomes)
        .map(|((name, session), outcome)| section(name, session, outcome))
        .collect();
    let text = sections.join("\n\n");
    match outcomes.iter().all(Outcome::is_answer) {
        true => ToolOutput::text(text),
        false => ToolOutput::error(text),
    }
}

#[async_trait]
impl Tool for WaitAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "WaitAgent".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<WaitArgs>(),
            meta: Default::default(),
        }
    }

    /// Interrupting the turn drops the wait, never the agents.
    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits(Interrupt::Cancel)
    }

    /// One subject per agent, by the name the call wrote: an ask a rule could
    /// not read — none, or one name twice — names nobody.
    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        let Ok(args) = serde_json::from_value::<WaitArgs>(input.clone()) else {
            return Vec::new();
        };
        asked(&args.agents)
            .unwrap_or_default()
            .into_iter()
            .map(|name| Subject::Name { name: name.into() })
            .collect()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WaitArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let names = match asked(&args.agents) {
            Ok(names) => names,
            Err(refused) => return Ok(ToolOutput::error(refused)),
        };
        let roster = match roster(cx, &names).await {
            Ok(roster) => roster,
            Err(unknown) => return Ok(ToolOutput::error(unknown)),
        };
        let deadline = args.timeout_s.map(Deadline::new);
        let outcomes = join(cx, &roster, deadline.as_ref()).await?;
        Ok(report(&roster, &outcomes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, tool_context, turn_completed};
    use serde_json::json;

    async fn waited(fleet: &Fleet, caller: &bingo_sdk::SessionId, input: Value) -> ToolOutput {
        let host = Recorder::new(fleet);
        WaitAgentTool
            .call(input, &tool_context(caller, host))
            .await
            .expect("a wait this crate can serve")
    }

    fn text_of(out: &ToolOutput) -> String {
        out.parts[0].as_text().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn an_agent_still_working_is_waited_for() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.set_busy(&child, true);
        fleet.script([assistant("done at last"), turn_completed()]);

        let out = waited(&fleet, &root, json!({ "agents": ["reviewer"] })).await;
        let text = text_of(&out);
        assert!(text.contains("done at last"), "{text}");
    }

    #[tokio::test]
    async fn an_idle_agent_answers_with_what_it_last_said() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.said(&child, "the diff is fine");

        let out = waited(&fleet, &root, json!({ "agents": ["reviewer"] })).await;
        let text = text_of(&out);
        assert!(text.starts_with("reviewer (ses_"), "{text}");
        assert!(text.contains("the diff is fine"), "{text}");
    }

    #[tokio::test]
    async fn an_idle_agent_whose_last_turn_failed_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.failed(&child, "no key");

        let out = waited(&fleet, &root, json!({ "agents": ["reviewer"] })).await;
        assert!(out.is_error, "what it last did was fail");
        let text = text_of(&out);
        assert!(text.contains("failed: no key"), "{text}");
    }

    #[tokio::test]
    async fn an_agent_that_does_not_finish_in_time_is_still_running() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.set_busy(&child, true);
        fleet.script([assistant("still going")]);

        let out = waited(
            &fleet,
            &root,
            json!({ "agents": ["reviewer"], "timeout_s": 0 }),
        )
        .await;
        assert!(out.is_error, "the work is not done");
        let text = text_of(&out);
        assert!(text.contains("still working after 0s"), "{text}");
        assert!(text.contains("still running"), "{text}");
    }

    /// The same address space `SendMessage` has (ADR-0024 §3): what a caller
    /// can write to, it can wait for.
    #[tokio::test]
    async fn a_teammate_can_be_waited_for_too() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let builder = fleet.child(&root, "builder");
        let reviewer = fleet.child(&root, "reviewer");
        fleet.said(&reviewer, "the diff is fine");

        let out = waited(&fleet, &builder, json!({ "agents": ["reviewer"] })).await;
        let text = text_of(&out);
        assert!(text.contains("the diff is fine"), "{text}");
    }

    // ---- the join --------------------------------------------------------

    #[tokio::test]
    async fn a_join_answers_for_every_agent_in_the_order_asked() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let alpha = fleet.child(&root, "alpha");
        let beta = fleet.child(&root, "beta");
        fleet.said(&alpha, "alpha is done");
        fleet.said(&beta, "beta is done");

        let out = waited(&fleet, &root, json!({ "agents": ["beta", "alpha"] })).await;
        assert!(!out.is_error, "both answered");
        let text = text_of(&out);
        let beta_at = text
            .find("beta is done")
            .unwrap_or_else(|| panic!("{text}"));
        let alpha_at = text
            .find("alpha is done")
            .unwrap_or_else(|| panic!("{text}"));
        assert!(beta_at < alpha_at, "the order asked, not the order listed");
        assert_eq!(text.matches("replied:").count(), 2, "{text}");
    }

    /// A deadline that passes must not swallow the work that did land: the
    /// call is an error, and the reply of the agent that finished is in it.
    #[tokio::test]
    async fn a_deadline_names_who_finished_and_who_is_still_working() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let done = fleet.child(&root, "done");
        let slow = fleet.child(&root, "slow");
        fleet.said(&done, "the diff is fine");
        fleet.set_busy(&slow, true);
        fleet.script([assistant("still going")]);

        let out = waited(
            &fleet,
            &root,
            json!({ "agents": ["done", "slow"], "timeout_s": 0 }),
        )
        .await;
        assert!(out.is_error, "one of them is not an answer");
        let text = text_of(&out);
        assert!(text.contains("the diff is fine"), "{text}");
        assert!(text.contains("slow (ses_"), "{text}");
        assert!(text.contains("still working after 0s"), "{text}");
    }

    /// ADR-0027: a seated member's brief is journalled only when absorbed, so
    /// a session nothing has woken is empty — and reading that as an answer
    /// would tell the caller its teammate had finished saying nothing.
    #[tokio::test]
    async fn a_member_nothing_has_woken_is_seated_not_finished() {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "understudy");

        let out = waited(&fleet, &root, json!({ "agents": ["understudy"] })).await;
        assert!(out.is_error, "nothing has been said to be read");
        let text = text_of(&out);
        assert!(
            text.contains("is seated and nothing has woken it"),
            "{text}"
        );
        assert!(text.contains("post in a room it is in"), "{text}");
        assert!(!text.contains("finished without saying anything"), "{text}");
    }

    #[tokio::test]
    async fn one_unknown_name_fails_the_call_and_nothing_is_waited_on() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.said(&reviewer, "the diff is fine");

        let out = waited(&fleet, &root, json!({ "agents": ["reviewer", "nobody"] })).await;
        assert!(out.is_error);
        let text = text_of(&out);
        assert!(text.contains("nothing is called nobody"), "{text}");
        assert!(
            !text.contains("the diff is fine"),
            "the call was refused before anything was waited for: {text}"
        );
        assert!(fleet.opened().is_empty(), "{:?}", fleet.opened());
    }

    #[tokio::test]
    async fn an_agent_named_twice_is_refused_before_anything_is_waited_on() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        fleet.said(&reviewer, "the diff is fine");

        let out = waited(
            &fleet,
            &root,
            json!({ "agents": ["reviewer", " reviewer "] }),
        )
        .await;
        assert!(out.is_error);
        let text = text_of(&out);
        assert!(text.contains("named twice"), "{text}");
        assert!(fleet.opened().is_empty(), "{:?}", fleet.opened());
    }

    /// Interrupting the turn drops the join and leaves the agents running:
    /// nothing is delivered to them either way.
    #[tokio::test]
    async fn a_cancelled_turn_drops_the_join_and_not_the_agents() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let child = fleet.child(&root, "reviewer");
        fleet.set_busy(&child, true);
        fleet.script([assistant("still going")]);

        let cx = tool_context(&root, Recorder::new(&fleet));
        cx.cancel.cancel();
        let error = WaitAgentTool
            .call(json!({ "agents": ["reviewer"] }), &cx)
            .await
            .expect_err("the turn was interrupted");
        assert!(matches!(error, ToolError::Cancelled), "{error}");
        assert!(fleet.delivered().is_empty(), "the agent was left alone");
    }

    #[tokio::test]
    async fn a_call_that_names_nobody_says_so() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let out = waited(&fleet, &root, json!({ "agents": [] })).await;
        assert!(out.is_error);
        assert!(text_of(&out).contains("name at least one agent"));
    }

    #[test]
    fn an_ask_is_read_once_and_the_same_way_for_subjects_as_for_the_wait() {
        assert_eq!(asked(&["  reviewer ".into()]), Ok(vec!["reviewer"]));
        assert!(asked(&[]).is_err());
        assert!(asked(&["a".into(), "b".into(), "a".into()]).is_err());
    }

    /// The words the model chooses the tool by (M23 brick 7).
    #[test]
    fn the_description_says_it_joins_several_under_one_deadline() {
        assert!(
            DESCRIPTION.contains("one or several agents"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("one deadline covers them all"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("in the order you named them"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("only when you cannot go on without the result"),
            "{DESCRIPTION}"
        );
    }

    #[test]
    fn it_reads_and_a_rule_may_name_the_agents_it_waits_for() {
        let tool = WaitAgentTool;
        assert_eq!(tool.spec().name, "WaitAgent");
        assert!(tool.spec().input_schema.get("$schema").is_none());
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(traits.interrupt, Interrupt::Cancel);
        assert_eq!(
            tool.subjects(&json!({ "agents": ["reviewer", "scout"] }), Path::new("/")),
            [
                Subject::Name {
                    name: "reviewer".into()
                },
                Subject::Name {
                    name: "scout".into()
                }
            ]
        );
        assert!(
            tool.subjects(&json!({ "agents": ["a", "a"] }), Path::new("/"))
                .is_empty(),
            "an ask that is refused names nobody"
        );
    }
}
