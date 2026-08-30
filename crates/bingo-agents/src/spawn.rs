//! `SpawnAgent`: a child session under the calling tool item, given one task.
//!
//! The child is a session like any other — same journal, same reducer, same
//! gate — so nothing here runs a turn or holds a transcript. It mints the
//! session, delivers the prompt, and either waits for the reply or leaves a
//! watcher to bring it back.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Attachment, CatalogKind, Delivery, ErrorCode, HostHandle, Input, IntentId, Interrupt,
    KernelError, ParentLink, SessionId, SessionSpec, Subject, Tool, ToolContext, ToolError,
    ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::definition::Definition;
use crate::handle::LateHost;
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
with `background: false` the call waits and returns the agent's final text.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnArgs {
    /// The task, in full: what to do, what it may assume, and what to report
    /// back. The sub-agent reads nothing of this conversation.
    pub prompt: String,
    /// A named definition, `.bingo/agents/<name>.md`: its system prompt,
    /// model and tool set. Without one the sub-agent inherits this session's.
    pub agent: Option<String>,
    /// What to call this one, for `SendMessage`, `FollowupTask` and
    /// `WaitAgent`. Defaults to the definition's name; a name a sibling
    /// already holds gets `-2`, `-3`.
    pub name: Option<String>,
    /// Return at once and be told when it finishes (the default), or `false`
    /// to wait for its reply as the result of this call.
    pub background: Option<bool>,
    /// The model the sub-agent runs on; this session's by default.
    pub model: Option<String>,
    /// The provider the sub-agent runs on; this session's by default.
    pub provider: Option<String>,
    /// The tools the sub-agent may call, by name. By default it has every
    /// tool this session has, except `SpawnAgent`.
    pub tools: Option<Vec<String>>,
}

impl SpawnArgs {
    fn background(&self) -> bool {
        self.background.unwrap_or(true)
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
            cwd: cx.cwd.clone(),
            key: Some(format!("agent/{}/{name}", cx.session)),
            parent: Some(ParentLink {
                session: cx.session.clone(),
                item: cx.item.clone(),
            }),
            title: Some(name.to_string()),
            provider: self.provider.clone(),
            model: self.model.clone(),
            system_extra: Some(self.system_extra.clone()),
            tools: self.tools.clone(),
        }
    }
}

/// What the child may call: the names the call or the definition asked for,
/// else every tool this host has. Two are dropped from either: `SpawnAgent`,
/// which the depth limit would refuse, and `AskUserQuestion`, which the note
/// tells the child it does not have — a tool that cannot work, or must not,
/// is not offered.
async fn child_tools(host: &HostHandle, asked: Option<Vec<String>>) -> Option<Vec<String>> {
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

/// The child, under the first free name. A sibling's title and a live
/// session's key are two ways for a name to be taken, and the loop treats
/// them alike: the lock tells it what the list did not.
async fn start(
    plan: &Plan,
    mut taken: Vec<String>,
    cx: &ToolContext,
) -> Result<(String, SessionId), KernelError> {
    while let Some(name) = names::free(&plan.base, &taken) {
        match cx.host.spawn_session(plan.spec(&name, cx)).await {
            Ok(session) => return Ok((name, session)),
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
#[derive(Debug)]
pub struct SpawnAgentTool {
    host: Arc<LateHost>,
}

impl SpawnAgentTool {
    pub fn new(host: Arc<LateHost>) -> Self {
        Self { host }
    }

    /// The child, running, with the prompt already on its way and an
    /// attachment opened before the delivery so no frame of the turn is lost.
    async fn open(
        &self,
        args: &SpawnArgs,
        cx: &ToolContext,
    ) -> Result<(String, SessionId, Attachment), ToolError> {
        let host = self.host.require().map_err(failed)?;
        let definitions = library::load(&cx.env, &cx.cwd);
        let definition = pick(args.agent.as_deref(), &definitions).map_err(ToolError::Failed)?;
        let plan = Plan::of(args, definition, host).await.map_err(failed)?;
        let taken = names::names_of(&names::children(host, &cx.session).await.map_err(failed)?);
        let (name, session) = start(&plan, taken, cx).await.map_err(failed)?;
        let attachment = watch::follow(host, &session).await.map_err(failed)?;
        let prompt = Input::text(args.prompt.clone(), message::origin(None));
        cx.host
            .deliver(&session, IntentId::mint(), prompt, Delivery::Wake)
            .map_err(failed)?;
        Ok((name, session, attachment))
    }
}

fn failed(error: KernelError) -> ToolError {
    ToolError::Failed(error.message)
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
        crate::traits(Interrupt::Cancel)
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
        if !args.background() {
            let reply = watch::next_reply(&mut attachment, &cx.cancel).await?;
            return Ok(watch::output(&name, &session, &reply));
        }
        let host = Arc::clone(&cx.host);
        let parent = cx.session.clone();
        tokio::spawn(watch::report(attachment, host, parent, name.clone()));
        Ok(ToolOutput::text(
            json!({ "name": name, "session": session.as_str() }).to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, Recorder, assistant, tool_context, turn_completed, turn_failed};

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
        let tool = SpawnAgentTool::new(fleet.late());
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
        let out = SpawnAgentTool::new(fleet.late())
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
        SpawnAgentTool::new(fleet.late())
            .call(json!({ "prompt": "go", "name": "reviewer" }), &cx)
            .await
            .expect("a spawn");

        let spec = &host.spawned()[0];
        assert_eq!(spec.title.as_deref(), Some("reviewer"));
        let key = format!("agent/{root}/reviewer");
        assert_eq!(spec.key.as_deref(), Some(key.as_str()));
        assert_eq!(spec.parent.as_ref().map(|p| &p.session), Some(&root));
        assert_eq!(
            spec.parent.as_ref().map(|p| p.item.as_str()),
            Some("itm_call")
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
        SpawnAgentTool::new(fleet.late())
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
        let out = SpawnAgentTool::new(fleet.late())
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
        SpawnAgentTool::new(fleet.late())
            .call(
                json!({ "prompt": "go", "name": "reviewer" }),
                &tool_context(&root, host.clone()),
            )
            .await
            .expect("a spawn");
        assert_eq!(host.spawned().len(), 2, "the locked name was tried first");
        assert_eq!(host.spawned()[1].title.as_deref(), Some("reviewer-2"));
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

    #[test]
    fn it_reads_and_a_rule_may_name_the_agent_it_starts() {
        let tool = SpawnAgentTool::new(Arc::new(LateHost::default()));
        let spec = tool.spec();
        assert_eq!(spec.name, "SpawnAgent");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema["properties"]["prompt"]["description"].is_string());
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(traits.interrupt, Interrupt::Cancel);
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
