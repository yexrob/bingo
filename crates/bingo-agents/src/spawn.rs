//! `SpawnAgent`: a child session under the calling tool item, given one task.
//!
//! The child is a session like any other — same journal, same reducer, same
//! gate — so nothing here runs a turn or holds a transcript. It mints the
//! session and delivers the prompt; what differs between the three arms is
//! only what becomes of the answer — waited for, watched for, or, on standby,
//! neither: the prompt is held unread until something else wakes the member
//! (ADR-0027).

use std::path::Path;

use async_trait::async_trait;
use bingo_sdk::{
    Attachment, CatalogKind, Delivery, ErrorCode, HostHandle, Input, IntentId, KernelError,
    OpenOptions, ParentLink, SessionId, SessionSelector, SessionSpec, Subject, Tool, ToolContext,
    ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::definition::Definition;
use crate::{library, message, names, note, watch};

/// What a spawn is called when the call names neither a definition nor an
/// instance.
const DEFAULT_NAME: &str = "agent";

/// This tool's own name, and the one tool a child is never offered: the
/// kernel's depth limit is one, so an agent's agent could not be started.
const SPAWN_AGENT: &str = "SpawnAgent";

const DESCRIPTION: &str = "\
Start a sub-agent: a session of its own, with its own transcript and its own \
context window, working in the same directory. Use one for a large, separable \
piece of work — a search, a review, a build-and-fix loop — that would \
otherwise fill this conversation, or to run several such pieces at once. It \
sees nothing of this conversation and cannot ask the user anything, so the \
prompt has to stand on its own: what to do, what it may assume, and what to \
report back. In the background, which is the default, the call returns the \
agent's name at once and its reply arrives as a message when it finishes; \
with `background: false` the call waits and returns the agent's final text. \
When several agents are to work with each other rather than each report back, \
seat them instead of tasking them: `OpenRoom` naming the roles — and \
`parent` among them when you want to read the room yourself — one \
`standby: true` spawn per role, then a single `SendMessage` to `#room` \
carrying the kickoff and naming with `@name` whoever it is for. A member reads \
its room at the head of its own turn, and being named is what opens that turn \
now, so one post starts everyone it names and each reads its own brief first; \
writing to them one at a time instead makes you the switchboard every step has \
to pass back through. A brief that tells an agent to stand by has to say what \
to stand by for: everything reaching it from elsewhere is labelled — \
`[from <name>]`, `[in #<room>]` — and an unlabelled line in its own \
conversation is the person it works for, or you, writing to it directly, which \
it answers whatever else it was told.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnArgs {
    /// The task, in full: what to do, what it may assume, and what to report
    /// back. The sub-agent reads nothing of this conversation.
    pub prompt: String,
    /// A named definition, `.bingo/agents/<name>.md`: its system prompt,
    /// model and tool set. Without one the sub-agent inherits this session's.
    pub agent: Option<String>,
    /// What to call this one, for `SendMessage` and `WaitAgent`. Defaults to
    /// the definition's name; a name a sibling already holds gets `-2`, `-3`.
    pub name: Option<String>,
    /// Return at once and be told when it finishes (the default), or `false`
    /// to wait for its reply as the result of this call.
    pub background: Option<bool>,
    /// Seat it silent: the prompt is its standing brief, kept unread, and it
    /// runs no turn until something wakes it — a post naming it in a room it
    /// is in, or a message. Nothing here is told when it finishes. Use it for
    /// the members of a room, so one kickoff post starts everyone it names.
    pub standby: Option<bool>,
    /// The model the sub-agent runs on; this session's by default. Call
    /// `ListModels` to see what is available instead of guessing an id.
    pub model: Option<String>,
    /// The provider the sub-agent runs on; this session's by default. Call
    /// `ListModels` to see what is available instead of guessing an id.
    pub provider: Option<String>,
    /// The tools the sub-agent may call, by name. By default it has every
    /// tool this session has, except `SpawnAgent`.
    pub tools: Option<Vec<String>>,
}

impl SpawnArgs {
    fn background(&self) -> bool {
        self.background.unwrap_or(true)
    }

    fn standby(&self) -> bool {
        self.standby.unwrap_or(false)
    }

    /// How the prompt reaches the child. A standby member's is held at the
    /// head of its queue and read by whatever turn something else opens
    /// (ADR-0027 §1); waiting for such an agent is a deadlock asked for by
    /// name, so the pair is refused in words (ADR-0027 §5).
    fn delivery(&self) -> Result<Delivery, String> {
        match (self.standby(), self.background()) {
            (false, _) => Ok(Delivery::Wake),
            (true, true) => Ok(Delivery::Hold),
            (true, false) => Err("a standby agent runs no turn until something wakes it, so \
                 waiting for its reply would wait forever: spawn it with \
                 `background: true` and wake it with a room post or a message, \
                 or drop `standby` to have it answer this prompt now"
                .into()),
        }
    }

    /// What the agent is called before the siblings are consulted: the name
    /// asked for, else the definition's, else `agent`.
    fn base(&self, definition: Option<&Definition>) -> Result<String, KernelError> {
        let base = self
            .name
            .as_deref()
            .or(definition.map(|d| d.name.as_str()))
            .unwrap_or(DEFAULT_NAME);
        names::check(base).map(str::to_string)
    }
}

/// Everything the child is, before it has a name or an id.
struct Plan {
    base: String,
    provider: Option<String>,
    model: Option<String>,
    system_extra: String,
    tools: Option<Vec<String>>,
}

impl Plan {
    /// The call decides over the definition, the definition over the host.
    async fn of(
        args: &SpawnArgs,
        definition: Option<&Definition>,
        host: &HostHandle,
    ) -> Result<Plan, KernelError> {
        let asked = args
            .tools
            .clone()
            .or_else(|| definition.and_then(|d| d.tools.clone()));
        Ok(Plan {
            base: args.base(definition)?,
            provider: args
                .provider
                .clone()
                .or_else(|| definition.and_then(|d| d.provider.clone())),
            model: args
                .model
                .clone()
                .or_else(|| definition.and_then(|d| d.model.clone())),
            system_extra: note::system_extra(definition.map_or("", |d| d.system.as_str())),
            tools: child_tools(host, asked).await,
        })
    }

    fn spec(&self, name: &str, cx: &ToolContext) -> SessionSpec {
        SessionSpec {
            driver: Default::default(),
            cwd: cx.cwd.clone(),
            key: Some(format!("agent/{}/{name}", cx.session)),
            parent: Some(ParentLink {
                session: cx.session.clone(),
                item: Some(cx.item.clone()),
            }),
            title: Some(name.to_string()),
            provider: self.provider.clone(),
            model: self.model.clone(),
            system_extra: Some(self.system_extra.clone()),
            tools: self.tools.clone(),
            // M76 fills this in: a spawn names the level next.
            thinking: None,
        }
    }
}

/// What the child may call: the names the call or the definition asked for,
/// else every tool this host has. Two are dropped from either: `SpawnAgent`,
/// which the depth limit would refuse, and `AskUserQuestion`, which the note
/// tells the child it does not have — a tool that cannot work, or must not,
/// is not offered.
pub(crate) async fn child_tools(
    host: &HostHandle,
    asked: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let mut names = match asked {
        Some(names) => names,
        None => registered(host).await?,
    };
    names.retain(|name| !NOT_A_CHILDS.contains(&name.as_str()));
    Some(names)
}

const NOT_A_CHILDS: [&str; 2] = [SPAWN_AGENT, "AskUserQuestion"];

/// Every tool name the host has now, or nothing when the catalogue cannot be
/// read — in which case the child inherits the whole set, as a session does.
async fn registered(host: &HostHandle) -> Option<Vec<String>> {
    let catalog = host.catalog(CatalogKind::Tools).await.ok()?;
    Some(catalog.entries.into_iter().map(|entry| entry.id).collect())
}

/// The child, under the first free name, with the attachment its creation
/// hands back — opened before anything is delivered, so no frame of the turn
/// that follows can be missed. A sibling's title and a live session's key are
/// two ways for a name to be taken, and the loop treats them alike: the lock
/// tells it what the list did not.
async fn start(
    plan: &Plan,
    mut taken: Vec<String>,
    cx: &ToolContext,
) -> Result<(String, Attachment), KernelError> {
    while let Some(name) = names::free(&plan.base, &taken) {
        let selector = SessionSelector::Create {
            spec: plan.spec(&name, cx),
        };
        let created = cx
            .host
            .open(selector, watch::identity(), OpenOptions::default())
            .await;
        match created {
            Ok(attachment) => return Ok((name, attachment)),
            Err(e) if e.code == ErrorCode::SessionLocked => taken.push(name),
            Err(e) => return Err(e),
        }
    }
    Err(KernelError::new(
        ErrorCode::InvalidInput,
        format!("every name from {} on is taken; name this one", plan.base),
    ))
}

/// The definition a call names, or what it could have named.
fn pick<'a>(
    agent: Option<&str>,
    definitions: &'a [Definition],
) -> Result<Option<&'a Definition>, String> {
    let Some(agent) = agent else {
        return Ok(None);
    };
    match definitions.iter().find(|d| d.name == agent) {
        Some(definition) => Ok(Some(definition)),
        None if definitions.is_empty() => Err(format!(
            "no agent definition is called {agent}, and this directory has none"
        )),
        None => Err(format!(
            "no agent definition is called {agent}; the ones here are: {}",
            library::names(definitions)
        )),
    }
}

/// Starting a session and posting a prompt into it: this session's transcript
/// is untouched, and every call the child then makes is gated in the child.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    /// The child, running, with the prompt already on its way.
    async fn open(
        &self,
        args: &SpawnArgs,
        cx: &ToolContext,
    ) -> Result<(String, SessionId, Attachment), ToolError> {
        let host = &cx.host;
        let delivery = args.delivery().map_err(ToolError::Failed)?;
        let definitions = library::load(&cx.env, &cx.cwd);
        let definition = pick(args.agent.as_deref(), &definitions).map_err(ToolError::Failed)?;
        let plan = Plan::of(args, definition, host).await.map_err(failed)?;
        let taken = names::names_of(&names::children(host, &cx.session).await.map_err(failed)?);
        let (name, attachment) = start(&plan, taken, cx).await.map_err(failed)?;
        let session = attachment.session.clone();
        let prompt = Input::text(args.prompt.clone(), message::origin(None));
        cx.host
            .deliver(&session, IntentId::mint(), prompt, delivery)
            .await
            .map_err(failed)?;
        Ok((name, session, attachment))
    }
}

fn failed(error: KernelError) -> ToolError {
    ToolError::Failed(error.message)
}

/// The address the caller writes to afterwards, as every spawn hands it back.
fn named(name: &str, session: &SessionId) -> String {
    json!({ "name": name, "session": session.as_str() }).to_string()
}

/// A standby member's receipt: the address, and what is true of it — it has
/// read nothing, it will not be waited for, and nothing here is told when its
/// turns end (ADR-0027 §3).
fn seated(name: &str, session: &SessionId) -> String {
    format!(
        "{}\n{name} is seated and idle: its brief is held unread and no turn \
         has opened. Whatever wakes it — a post in a room it is in, a message \
         — opens the turn that reads the brief first. Nothing will be reported \
         back here when its turns end.",
        named(name, session)
    )
}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: SPAWN_AGENT.into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SpawnArgs>(),
            meta: Default::default(),
        }
    }

    /// Interrupting a foreground spawn drops the wait, not the child: the
    /// session it started keeps running and can still be written to by name.
    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<SpawnArgs>(input.clone())
            .ok()
            .and_then(|args| args.agent.or(args.name))
            .map(|name| vec![Subject::Name { name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SpawnArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let (name, session, mut attachment) = match self.open(&args, cx).await {
            Ok(started) => started,
            // Whatever stopped the child before it ran — a name, a
            // definition, a limit — is something the model reads and acts on.
            Err(ToolError::Failed(message)) => return Ok(ToolOutput::error(message)),
            Err(other) => return Err(other),
        };
        // No watcher on a standby member: a teammate is not a one-shot task,
        // and nothing wakes this session when its turns end (ADR-0027 §3).
        if args.standby() {
            return Ok(ToolOutput::text(seated(&name, &session)));
        }
        if !args.background() {
            let reply = watch::next_reply(&cx.host, &mut attachment, &cx.cancel).await?;
            return Ok(watch::output(&name, &session, &reply));
        }
        let host = cx.host.clone();
        let parent = cx.session.clone();
        tokio::spawn(watch::report(attachment, host, parent, name.clone()));
        Ok(ToolOutput::text(named(&name, &session)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, tool_context, turn_completed, turn_failed};
    use std::sync::Arc;

    fn definition(name: &str, body: &str) -> Definition {
        Definition {
            name: name.into(),
            description: "d".into(),
            model: Some("fake-2".into()),
            provider: Some("other".into()),
            tools: Some(vec!["Read".into(), "SpawnAgent".into()]),
            system: body.into(),
        }
    }

    async fn spawned(input: Value) -> (ToolOutput, Arc<Recorder>) {
        let fleet = Fleet::default();
        fleet.script([assistant("hi from the child"), turn_completed()]);
        let root = fleet.root();
        let host = Recorder::new(&fleet);
        let tool = SpawnAgentTool;
        let out = tool
            .call(input, &tool_context(&root, host.clone()))
            .await
            .expect("a spawn this crate can serve");
        (out, host)
    }

    #[tokio::test]
    async fn a_foreground_spawn_returns_the_child_s_final_text() {
        let (out, host) = spawned(json!({ "prompt": "say hi", "background": false })).await;
        assert!(!out.is_error);
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("hi from the child"), "{text}");
        assert!(text.starts_with("agent (ses_"), "{text}");
        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1, "the prompt, and nothing back to itself");
        assert_eq!(delivered[0].2, Delivery::Wake);
    }

    #[tokio::test]
    async fn a_foreground_spawn_reports_a_child_that_failed_as_an_error() {
        let fleet = Fleet::default();
        fleet.script([turn_failed("no key")]);
        let root = fleet.root();
        let host = Recorder::new(&fleet);
        let out = SpawnAgentTool
            .call(
                json!({ "prompt": "go", "background": false }),
                &tool_context(&root, host),
            )
            .await
            .expect("a spawn");
        assert!(out.is_error, "a crash is not an answer");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("failed: no key"), "{text}");
    }

    #[tokio::test]
    async fn a_background_spawn_names_the_child_at_once() {
        let (out, _) = spawned(json!({ "prompt": "look around", "name": "scout" })).await;
        let text = out.parts[0].as_text().unwrap_or_default();
        let named: Value = serde_json::from_str(text).expect("a name and a session");
        assert_eq!(named["name"], "scout");
        assert!(
            named["session"]
                .as_str()
                .is_some_and(|s| s.starts_with("ses_")),
            "{text}"
        );
    }

    #[tokio::test]
    async fn the_child_carries_the_note_the_parent_link_and_a_key_of_its_own() {
        let fleet = Fleet::default();
        fleet.script([turn_completed()]);
        let root = fleet.root();
        let host = Recorder::new(&fleet);
        let cx = tool_context(&root, host.clone());
        SpawnAgentTool
            .call(json!({ "prompt": "go", "name": "reviewer" }), &cx)
            .await
            .expect("a spawn");

        let spec = &host.spawned()[0];
        assert_eq!(spec.title.as_deref(), Some("reviewer"));
        let key = format!("agent/{root}/reviewer");
        assert_eq!(spec.key.as_deref(), Some(key.as_str()));
        assert_eq!(spec.parent.as_ref().map(|p| &p.session), Some(&root));
        assert_eq!(
            spec.parent.as_ref().and_then(|p| p.item.as_ref()),
            Some(&bingo_sdk::ItemId::from_raw("itm_call"))
        );
        assert_eq!(spec.cwd, cx.cwd);
        let extra = spec.system_extra.as_deref().unwrap_or_default();
        assert!(extra.starts_with(note::NOTE), "{extra}");
    }

    #[tokio::test]
    async fn a_child_is_offered_every_tool_but_the_one_it_could_not_use() {
        let fleet = Fleet::default();
        fleet.script([turn_completed()]);
        let root = fleet.root();
        let host = Recorder::new(&fleet);
        SpawnAgentTool
            .call(
                json!({ "prompt": "go" }),
                &tool_context(&root, host.clone()),
            )
            .await
            .expect("a spawn");
        let tools = host.spawned()[0].tools.clone().unwrap_or_default();
        assert!(tools.contains(&"Read".to_string()), "{tools:?}");
        for kept_back in NOT_A_CHILDS {
            assert!(!tools.contains(&kept_back.to_string()), "{tools:?}");
        }
    }

    #[tokio::test]
    async fn a_name_a_sibling_holds_gets_the_next_one() {
        let fleet = Fleet::default();
        fleet.script([turn_completed()]);
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        let host = Recorder::new(&fleet);
        let out = SpawnAgentTool
            .call(
                json!({ "prompt": "go", "name": "reviewer" }),
                &tool_context(&root, host.clone()),
            )
            .await
            .expect("a spawn");
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("reviewer-2"), "{text}");
        assert_eq!(host.spawned()[0].title.as_deref(), Some("reviewer-2"));
    }

    #[tokio::test]
    async fn a_key_a_live_session_holds_is_the_same_kind_of_collision() {
        let fleet = Fleet::default();
        fleet.script([turn_completed()]);
        let root = fleet.root();
        let host = Recorder::new(&fleet);
        host.lock(&format!("agent/{root}/reviewer"));
        SpawnAgentTool
            .call(
                json!({ "prompt": "go", "name": "reviewer" }),
                &tool_context(&root, host.clone()),
            )
            .await
            .expect("a spawn");
        assert_eq!(host.spawned().len(), 2, "the locked name was tried first");
        assert_eq!(host.spawned()[1].title.as_deref(), Some("reviewer-2"));
    }

    /// Long enough for a watcher, had one been left, to have reported: the
    /// fleet's script is ready at once, so a few turns of the scheduler are
    /// the whole of what it needs.
    async fn settled() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    async fn everything_delivered(input: Value) -> Vec<(SessionId, Input, Delivery)> {
        let (_, host) = spawned(input).await;
        settled().await;
        host.delivered()
    }

    /// One spawn's spec, less the two fields a fresh fleet's minted ids make
    /// unique — asserted here rather than compared.
    async fn minted(input: Value) -> SessionSpec {
        let (_, host) = spawned(input).await;
        let mut spec = host.spawned()[0].clone();
        assert!(
            spec.key.take().is_some_and(|key| key.ends_with("/counter")),
            "the key names the child"
        );
        assert!(spec.parent.take().is_some(), "and hangs off the call");
        spec
    }

    /// ADR-0027 §1: the arm changes how the brief arrives, not who arrives.
    #[tokio::test]
    async fn a_standby_member_is_minted_exactly_as_a_woken_one() {
        let woken = minted(json!({ "prompt": "go", "name": "counter" })).await;
        let seated = minted(json!({ "prompt": "go", "name": "counter", "standby": true })).await;
        assert_eq!(woken, seated);
    }

    #[tokio::test]
    async fn a_standby_spawn_names_the_child_and_says_it_has_read_nothing() {
        let (out, host) = spawned(json!({
            "prompt": "count the evens", "name": "counter", "standby": true
        }))
        .await;
        assert!(!out.is_error);
        let text = out.parts[0].as_text().unwrap_or_default();
        let (address, truth) = text.split_once('\n').expect("the address, then the truth");
        let named: Value = serde_json::from_str(address).expect("a name and a session");
        assert_eq!(named["name"], "counter");
        assert!(truth.contains("seated and idle"), "{truth}");
        assert!(truth.contains("Nothing will be reported back"), "{truth}");

        let delivered = host.delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].2, Delivery::Hold, "the brief waits to be read");
        let Input::Text { text, .. } = &delivered[0].1 else {
            panic!("a brief is text");
        };
        assert_eq!(text, "count the evens");
    }

    /// Whether the parent is ever woken about this child is the whole of the
    /// difference (ADR-0027 §3): the same script, the same settling.
    #[tokio::test]
    async fn a_standby_member_leaves_no_watcher_to_wake_its_parent() {
        let watched = everything_delivered(json!({ "prompt": "go" })).await;
        assert_eq!(watched.len(), 2, "the prompt, then the child's end");
        assert_eq!(watched[0].2, Delivery::Wake);

        let seated = everything_delivered(json!({ "prompt": "go", "standby": true })).await;
        assert_eq!(seated.len(), 1, "the brief, and nothing ever after it");
        assert_eq!(seated[0].2, Delivery::Hold);
    }

    /// ADR-0027 §5: a deadlock asked for by name is answered in words, and
    /// no child is minted to be waited on.
    #[tokio::test]
    async fn a_standby_agent_nobody_could_wait_for_is_refused_in_words() {
        let (out, host) =
            spawned(json!({ "prompt": "go", "standby": true, "background": false })).await;
        assert!(out.is_error);
        let text = out.parts[0].as_text().unwrap_or_default();
        assert!(text.contains("wait forever"), "{text}");
        assert!(host.spawned().is_empty(), "nothing to wait on was started");
        assert!(host.delivered().is_empty());
    }

    #[test]
    fn standby_holds_the_brief_and_a_plain_spawn_wakes_on_it() {
        let args = |value: Value| serde_json::from_value::<SpawnArgs>(value).expect("args");
        let plain = args(json!({ "prompt": "p" }));
        assert!(
            !plain.standby(),
            "a spawn is a wake unless it says otherwise"
        );
        assert_eq!(plain.delivery(), Ok(Delivery::Wake));
        assert_eq!(
            args(json!({ "prompt": "p", "standby": true })).delivery(),
            Ok(Delivery::Hold)
        );
        assert!(
            args(json!({ "prompt": "p", "standby": true, "background": false }))
                .delivery()
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_definition_nobody_wrote_says_what_could_have_been_named() {
        let (out, host) = spawned(json!({ "prompt": "go", "agent": "nobody" })).await;
        assert!(out.is_error);
        assert!(host.spawned().is_empty());
    }

    #[test]
    fn a_definition_decides_what_the_call_did_not() {
        let definitions = [definition("reviewer", "You review diffs.")];
        let args: SpawnArgs = serde_json::from_value(json!({
            "prompt": "go", "agent": "reviewer", "model": "fake-1"
        }))
        .expect("args");
        let definition = pick(args.agent.as_deref(), &definitions).expect("found");
        assert_eq!(args.base(definition).as_deref(), Ok("reviewer"));
        assert!(args.background(), "background is the default");
        let picked = definition.expect("the definition");
        assert_eq!(picked.provider.as_deref(), Some("other"));
    }

    /// The room pattern where the model reads it (ADR-0027 §4): the shape is
    /// taught here, or the hub habit is what it falls back on.
    #[test]
    fn the_description_teaches_the_room_pattern_over_per_member_dispatch() {
        assert!(
            DESCRIPTION.contains("`OpenRoom` naming the roles"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("one `standby: true` spawn per role"),
            "{DESCRIPTION}"
        );
        // ADR-0028 §4: the seat is explicit, so the pattern has to say it.
        assert!(
            DESCRIPTION.contains("`parent` among them when you want to read the room yourself"),
            "{DESCRIPTION}"
        );
        // ADR-0034 §3 and §6: the default seat is patient, so the kickoff has
        // to name whoever it is for.
        assert!(
            DESCRIPTION.contains("naming with `@name` whoever it is for"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("one post starts everyone it names"),
            "{DESCRIPTION}"
        );
        assert!(DESCRIPTION.contains("switchboard"), "{DESCRIPTION}");
    }

    /// ADR-0010 §5: an unlabelled line is the person, and a brief that seats an
    /// agent must not leave it deaf to one. The rule is taught where the brief
    /// is written, because that is where the mistake is made.
    #[test]
    fn the_description_says_what_an_unlabelled_line_is() {
        assert!(
            DESCRIPTION.contains("stand by has to say what to stand by for"),
            "{DESCRIPTION}"
        );
        assert!(
            DESCRIPTION.contains("`[from <name>]`, `[in #<room>]`"),
            "the labelled kinds are named, so the unlabelled one is legible"
        );
        assert!(
            DESCRIPTION
                .contains("an unlabelled line in its own conversation is the person it works for"),
            "{DESCRIPTION}"
        );
    }

    #[test]
    fn it_reads_and_a_rule_may_name_the_agent_it_starts() {
        let tool = SpawnAgentTool;
        let spec = tool.spec();
        assert_eq!(spec.name, "SpawnAgent");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema["properties"]["prompt"]["description"].is_string());
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(
            tool.subjects(
                &json!({ "prompt": "p", "agent": "reviewer" }),
                Path::new("/")
            ),
            [Subject::Name {
                name: "reviewer".into()
            }]
        );
    }
}
