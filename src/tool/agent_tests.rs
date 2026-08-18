//! `tool::agent` tests.
//!
//! Split out of `agent.rs` (D132) the way `agent_notes.rs` was before it (D114)
//! and `query_tests.rs` was beside it (D130): the file is at its line cap and
//! the tests are what carried it over. The module is `tool::agent::tests`
//! exactly as before — `use super::*` still reaches the tool's own items, so
//! nothing here changed but the file it lives in.

use super::*;
use crate::query::{Runtime, Session};

/// The exact capability block a subagent with the given (unknown-to-the-
/// table → conservative defaults) model carries. Unknown models keep the
/// default: vision yes, thinking yes.
fn capability_block(model: &str, provider: &str) -> String {
    format!(
        "{}\nActive model: {model} (provider: {provider})\n- Vision: yes — accepts image input; \
         you can act on screenshots and rendered output\n- Thinking: yes — bingo may send \
         thinking parameters for this model",
        crate::system::MODEL_CAPABILITIES_HEADING
    )
}

fn parent_session() -> (Arc<Session>, Arc<crate::api::client::Client>) {
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-parent".into()),
        api_base_url: Some("https://parent.example".into()),
        // Explicitly opted out: models a compat proxy that speaks the protocol but rejects
        // image blocks. Image support is otherwise the default.
        send_images: Some(false),
        ..Default::default()
    };
    settings.providers.insert(
        "ds".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    );
    // An image-capable endpoint next to a text-only default: the shape that lets a text-only
    // session delegate an attachment to a subagent.
    settings.providers.insert(
        "vision".to_string(),
        crate::settings::ProviderConfig {
            env_key: None,
            models: None,
            api_key: Some("sk-v".into()),
            api_base_url: "https://vision.example".into(),
            supports_images: Some(true),
            protocol: None,
            oauth: None,
        },
    );
    let client = Arc::new(crate::api::client::Client::from_settings(&settings).unwrap());
    let mut runtime = Runtime::new("parent-model".into(), None, Default::default());
    runtime.mcp = Arc::new(tokio::sync::Mutex::new(crate::mcp::McpManager::new(
        Default::default(),
        Default::default(),
    )));
    let session = Arc::new(Session {
        client: (*client).clone(),
        runtime,
        permission_mode: crate::permission::PermissionMode::Default,
        settings,
        system: vec![SystemBlock {
            text: "parent system".into(),
            cache: false,
        }],
        depth: 0,
        cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
        home: std::env::temp_dir(),
        user_config_dir: std::env::temp_dir().join(".config"),
        quiet: true,
        compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        watch: crate::app::AppCore::start(Default::default()).watch(),
        tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
        expand_tasks: tokio::sync::watch::channel(false).0,
        agents: AgentRegistry::new(),
        channels: crate::channels::ChannelRegistry::new(Default::default()),
        instance: None,
        attachments: crate::api::image::Attachments::new(),
    });
    (session, client)
}

/// A project directory with no crew pinned. Never the ambient cwd: these tests assert
/// the exact system blocks a sub-session gets, and running them inside a repo that has
/// its own `.bingo/team.json` would add the hire's blocks and fail them for a reason
/// that has nothing to do with what they check.
fn crewless() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("bingo-crewless-{}", std::process::id()))
}

fn params(prompt: &str) -> AgentInput {
    AgentInput {
        prompt: prompt.into(),
        background: None,
        notify_on: None,
        description: None,
        model: None,
        provider: None,
        thinking: None,
        name: None,
        agent: None,
    }
}

fn def(name: &str) -> AgentDef {
    AgentDef {
        name: name.into(),
        description: format!("{name} description"),
        model: Some("def-model".into()),
        provider: Some("ds".into()),
        thinking: Some("high".into()),
        system: "You are the reviewer.".into(),
        inherit_system: true,
        source: crate::agents::AgentDefSource::Unknown,
    }
}

/// Extract build_sub_session's error text (Arc<Session> has no Debug, so unwrap_err is unavailable).
fn sub_err(r: Result<Arc<Session>, ToolError>) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(_) => panic!("expected build_sub_session error"),
    }
}

/// A project directory with a pinned crew and a written agreement.
fn crewed_project(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bingo-hire-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".bingo")).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        dir.join(crate::team::TEAM_FILE),
        r#"{"name":"dev-room","members":[{"name":"Mira","agent":"qa"}]}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        dir.join(crate::team::NORMS_FILE),
        "# Team norms\n\n- Report outcomes as they are.\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    dir
}

/// A spawn in a crewed project is a hire and is told so (D53): it carries the crew's
/// agreement, and it knows it is not on the crew. Without the second block "temporary"
/// would be bookkeeping the instance itself never learns, and it would plan as if there
/// were a next session in which it is asked again.
#[test]
fn a_spawn_beside_a_crew_is_a_hire_and_knows_it() {
    let (session, _client) = parent_session();
    let project = crewed_project("standing");
    let tool = AgentTool::new(session.clone(), Vec::new());
    let sub = tool
        .build_sub_session(&params("one job"), None, "temp", &project)
        .unwrap_or_else(|e| panic!("{e}"));
    let has = |head: &str| sub.system.iter().any(|b| b.text.starts_with(head));
    assert!(has("# Team norms (dev-room)"), "{:?}", sub.system);
    assert!(has("# You are a temporary hire"), "{:?}", sub.system);
    let standing = sub
        .system
        .iter()
        .find(|b| b.text.starts_with("# You are a temporary hire"))
        .unwrap_or_else(|| panic!("expected the standing block"));
    assert!(
        standing.text.contains(crate::team::TEAM_FILE)
            && standing.text.contains("not written into"),
        "it is told it never joins the blueprint: {}",
        standing.text
    );
    std::fs::remove_dir_all(&project).unwrap_or_else(|e| panic!("{e}"));
}

/// With no crew pinned, an ad-hoc subagent is the ordinary way to work: telling it that
/// it is temporary relative to a team that does not exist would be a lie, and it is the
/// same session it has always been.
#[test]
fn a_spawn_with_no_crew_is_told_nothing_about_one() {
    let (session, _client) = parent_session();
    let empty = std::env::temp_dir().join(format!("bingo-nocrew-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap_or_else(|e| panic!("{e}"));
    let tool = AgentTool::new(session.clone(), Vec::new());
    let sub = tool
        .build_sub_session(&params("do it"), None, "solo", &empty)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !sub.system
            .iter()
            .any(|b| b.text.contains("temporary hire") || b.text.starts_with("# Team norms")),
        "{:?}",
        sub.system
    );
    std::fs::remove_dir_all(&empty).unwrap_or_else(|e| panic!("{e}"));
}

/// The acceptance criterion in one assertion: hiring leaves the blueprint byte-identical.
/// A hire that could edit `.bingo/team.json` would make the crew something the model
/// grows on its own, which is exactly the decision the user keeps.
#[tokio::test]
async fn hiring_never_touches_the_blueprint() {
    let (session, _client) = parent_session();
    let project = crewed_project("blueprint");
    let path = project.join(crate::team::TEAM_FILE);
    let before = std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"));
    let tool = AgentTool::new(session.clone(), Vec::new());
    let ctx = ToolContext {
        cwd: project.clone(),
        ..main_ctx(&session)
    };
    let out = tool
        .call(
            serde_json::json!({"prompt": "look at one thing", "description": "one job"}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.content.as_str().unwrap_or_default().contains("name"),
        "the spawn returns an addressable instance"
    );
    assert_eq!(
        std::fs::read(&path).unwrap_or_else(|e| panic!("{e}")),
        before,
        "the blueprint is byte-identical before and after a hire"
    );
    let listed = session.agents.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].kind,
        AgentKind::Hire,
        "an Agent-tool spawn is never a crew member"
    );
    std::fs::remove_dir_all(&project).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn sub_session_inherits_model_and_shared_endpoint() {
    let (session, client) = parent_session();
    let _ = session.runtime.thinking_tx.send(Some("medium".into()));
    let tool = AgentTool::new(session.clone(), Vec::new());
    let sub = tool
        .build_sub_session(&params("do it"), None, "sub", &crewless())
        .unwrap();
    assert_eq!(*sub.runtime.model.borrow(), "parent-model");
    assert_eq!(
        sub.client.current_endpoint(),
        (
            Some("sk-parent".to_string()),
            "https://parent.example".to_string()
        )
    );
    assert_eq!(
        sub.system[0].text, "parent system",
        "inherits the parent system when no definition is given"
    );
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        Some("medium"),
        "inherits the parent session's current thinking level when neither explicit nor defined"
    );
    // No provider specified: shares the parent endpoint (follows the parent's provider switch).
    client.set_provider("ds").unwrap();
    assert_eq!(
        sub.client.current_endpoint().0.as_deref(),
        Some("sk-ds"),
        "the shared endpoint follows the parent session's switches"
    );
}

#[test]
fn sub_session_overrides_model_and_provider() {
    let (session, _client) = parent_session();
    let tool = AgentTool::new(session.clone(), Vec::new());
    let mut p = params("do it");
    p.model = Some("sub-model".into());
    p.provider = Some("ds".into());
    p.thinking = Some("xhigh".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(*sub.runtime.model.borrow(), "sub-model");
    assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
    assert_eq!(
        sub.client.current_endpoint(),
        (
            Some("sk-ds".to_string()),
            "https://api.deepseek.com".to_string()
        )
    );
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        Some("xhigh"),
        "an explicit thinking level takes effect"
    );
    // Forked independent endpoint: the parent session is unaffected.
    assert_eq!(
        session.client.current_endpoint().0.as_deref(),
        Some("sk-parent")
    );
}

#[test]
fn named_def_supplies_system_and_defaults() {
    let (session, _client) = parent_session();
    let d = def("reviewer");
    let tool = AgentTool::new(session.clone(), vec![d.clone()]);
    // The definition supplies system/model/provider/thinking defaults.
    let sub = tool
        .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
        .unwrap();
    // Default is append: parent system + persona + the subagent note block
    // + the instance's own capability block.
    let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "parent system",
            "You are the reviewer.",
            SUBAGENT_NOTE,
            &capability_block("def-model", "ds")
        ],
        "a named definition appends by default rather than replacing"
    );
    assert_eq!(*sub.runtime.model.borrow(), "def-model");
    assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        Some("high"),
        "the definition provides the thinking-level default"
    );
    // Explicit parameters take precedence over the definition.
    let mut p = params("review");
    p.model = Some("explicit".into());
    p.thinking = Some("off".into());
    let sub = tool
        .build_sub_session(&p, Some(&d), "sub", &crewless())
        .unwrap();
    assert_eq!(*sub.runtime.model.borrow(), "explicit");
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        None,
        "explicit off normalizes to no parameter"
    );
    // resolve_def: an unknown definition errors out and lists the available ones.
    let mut p = params("x");
    p.agent = Some("nope".into());
    let err = tool.resolve_def(&p).unwrap_err().to_string();
    assert!(err.contains("nope") && err.contains("reviewer"), "{err}");
}

#[test]
fn sub_session_unknown_provider_errors() {
    let (session, _client) = parent_session();
    let tool = AgentTool::new(session, Vec::new());
    let mut p = params("do it");
    p.provider = Some("nope".into());
    assert!(
        tool.build_sub_session(&p, None, "sub", &crewless())
            .is_err(),
        "unknown provider errors"
    );
}

#[test]
fn sub_session_cross_provider_requires_model() {
    // Parent provider = "default" (the parent_session default).
    let (session, _client) = parent_session();
    // Only a provider given, no model → fail early: the parent model is not inherited (so claude-sonnet-5 never
    // lands on a DeepSeek endpoint as "model not found").
    let tool = AgentTool::new(session.clone(), Vec::new());
    let mut p = params("do it");
    p.provider = Some("ds".into());
    let err = sub_err(tool.build_sub_session(&p, None, "sub", &crewless()));
    assert!(
        err.contains("requires a model") && err.contains("ds"),
        "crossing providers requires an explicit model: {err}"
    );
    // The definition provides a provider but no model → errors the same way.
    let mut d = def("reviewer");
    d.model = None;
    let tool = AgentTool::new(session.clone(), vec![d.clone()]);
    let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub", &crewless()));
    assert!(
        err.contains("requires a model"),
        "the definition-side cross-provider case errors the same way: {err}"
    );
    // Same provider (the parent's current is ds) → inherits the model, no error.
    let _ = session.runtime.provider_tx.send("ds".into());
    let tool = AgentTool::new(session.clone(), Vec::new());
    let mut p = params("do it");
    p.provider = Some("ds".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(
        *sub.runtime.model.borrow(),
        "parent-model",
        "same provider inherits the parent model"
    );
}

#[test]
fn sub_session_cross_provider_defaults_thinking_off() {
    let (session, _client) = parent_session();
    let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
    let tool = AgentTool::new(session.clone(), Vec::new());
    // Crossing providers with no explicit/defined thinking → defaults to off (no thinking parameter,
    // compatible with DeepSeek/Ollama endpoints).
    let mut p = params("do it");
    p.provider = Some("ds".into());
    p.model = Some("ds-model".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        None,
        "crossing providers defaults to off"
    );
    // An explicit thinking level still applies when crossing providers.
    let mut p = params("do it");
    p.provider = Some("ds".into());
    p.model = Some("ds-model".into());
    p.thinking = Some("high".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(sub.runtime.thinking.borrow().as_deref(), Some("high"));
}

#[test]
fn sub_session_same_provider_inherits_thinking() {
    let (session, _client) = parent_session();
    let _ = session.runtime.thinking_tx.send(Some("xhigh".into()));
    let _ = session.runtime.provider_tx.send("ds".into());
    let tool = AgentTool::new(session.clone(), Vec::new());
    let mut p = params("do it");
    p.provider = Some("ds".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(
        sub.runtime.thinking.borrow().as_deref(),
        Some("xhigh"),
        "same provider keeps the inherited snapshot"
    );
}

#[test]
fn sub_session_default_provider_aliases_parent_endpoint() {
    let (session, client) = parent_session();
    let tool = AgentTool::new(session.clone(), Vec::new());
    // Explicit "default": shares the parent endpoint, no fork, no error.
    let mut p = params("do it");
    p.provider = Some("default".into());
    let sub = tool
        .build_sub_session(&p, None, "sub", &crewless())
        .unwrap();
    assert_eq!(sub.runtime.provider.borrow().as_str(), "default");
    assert_eq!(
        sub.client.current_endpoint(),
        (
            Some("sk-parent".to_string()),
            "https://parent.example".to_string()
        )
    );
    // The shared endpoint follows the parent's switches ("default" and unset are equivalent).
    client.set_provider("ds").unwrap();
    let _ = session.runtime.provider_tx.send("ds".into());
    assert_eq!(sub.client.current_endpoint().0.as_deref(), Some("sk-ds"));
    // AgentDef frontmatter provider: default takes the same path (follows the parent's current provider name).
    let mut d = def("reviewer");
    d.provider = Some("default".into());
    let tool = AgentTool::new(session.clone(), vec![d.clone()]);
    let sub = tool
        .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
        .unwrap();
    assert_eq!(sub.runtime.provider.borrow().as_str(), "ds");
}

#[test]
fn sub_session_rejects_invalid_thinking() {
    let (session, _client) = parent_session();
    let tool = AgentTool::new(session.clone(), Vec::new());
    for bad in ["auto", "super", "HIGH"] {
        let mut p = params("do it");
        p.thinking = Some(bad.into());
        let err = sub_err(tool.build_sub_session(&p, None, "sub", &crewless()));
        assert!(
            err.contains("invalid thinking level"),
            "invalid level {bad:?} should error: {err}"
        );
    }
    // An invalid definition-side value errors the same way.
    let mut d = def("reviewer");
    d.thinking = Some("bogus".into());
    let tool = AgentTool::new(session.clone(), vec![d.clone()]);
    let err = sub_err(tool.build_sub_session(&params("review"), Some(&d), "sub", &crewless()));
    assert!(
        err.contains("invalid thinking level"),
        "definition-side invalid value should error: {err}"
    );
}

#[test]
fn schema_exposes_name_and_agent() {
    let (session, _client) = parent_session();
    let tool = AgentTool::new(session, vec![def("reviewer")]);
    let schema = tool.input_schema();
    let props = schema["properties"].as_object().unwrap();
    for key in ["model", "provider", "thinking", "name", "agent"] {
        assert!(props.contains_key(key), "schema contains {key}");
    }
    assert!(
        tool.description()
            .contains("- reviewer: reviewer description"),
        "the description lists the named definitions"
    );
}

#[test]
fn excerpt_is_single_line_and_bounded() {
    assert_eq!(excerpt("short task"), "short task");
    assert_eq!(excerpt("first line\nsecond line"), "first line…");
    let long = "x".repeat(50);
    let cut = excerpt(&long);
    assert!(cut.chars().count() <= 41, "{cut}");
    assert!(cut.ends_with('…'));
}

#[test]
fn agent_control_list_reports_relative_last_activity() {
    assert_eq!(format_last_active(std::time::Duration::ZERO), "active now");
    assert_eq!(
        format_last_active(std::time::Duration::from_secs(3)),
        "active 3s ago"
    );
    assert_eq!(
        format_last_active(std::time::Duration::from_secs(125)),
        "active 2min ago"
    );
    assert_eq!(
        format_last_active(std::time::Duration::from_secs(7_200)),
        "active 2h ago"
    );
}

#[tokio::test]
async fn agent_control_list_stop_delete() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "scout",
        AgentKind::Hire,
        None,
        "research".into(),
        session.clone(),
    );
    let ctl = AgentControlTool::new(session.clone());
    let ctx = crate::tool::ToolContext {
        home: std::env::temp_dir(),
        cwd: std::path::PathBuf::from("/tmp"),
        watch: session.watch.clone(),
        live: Default::default(),
        http: reqwest::Client::new(),
        tasks: session.tasks.clone(),
        hooks: crate::settings::HooksConfig::default(),
        permission_mode: "default".into(),
        expand_tasks: tokio::sync::watch::channel(false).0,
        ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        instance: None,
        rewind: Default::default(),
    };
    assert!(ctl.is_read_only(&serde_json::json!({"action": "list"})));
    assert!(!ctl.is_read_only(&serde_json::json!({"action": "stop", "agent": "scout"})));
    let out = ctl
        .call(serde_json::json!({"action": "list"}), &ctx)
        .await
        .unwrap();
    let text = out.content.as_str().unwrap();
    assert!(
        text.contains("scout") && text.contains("running") && text.contains("active now"),
        "{text}"
    );
    let out = ctl
        .call(
            serde_json::json!({"action": "stop", "agent": "scout"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.as_str().unwrap().contains("stopped"), "stop");
    // After stopping, SendMessage resumes the instance (D105a): the
    // delivery lands and scout leaves the stopped state. Delete still
    // works below because it stops whatever it finds first.
    let send = SendMessageTool::new(session.clone());
    send.call(serde_json::json!({"to": "scout", "message": "hi"}), &ctx)
        .await
        .unwrap_or_else(|e| panic!("a stopped instance accepts a direct message: {e}"));
    assert_ne!(
        session
            .agents
            .list()
            .iter()
            .find(|s| s.name == "scout")
            .map(|s| s.state),
        Some(crate::agents::AgentState::Stopped),
        "resumed, not refused"
    );
    let out = ctl
        .call(
            serde_json::json!({"action": "delete", "agent": "scout"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.content.as_str().unwrap().contains("deleted"));
    assert!(session.agents.list().is_empty());
    // Unknown instance: stop errors out.
    let err = ctl
        .call(
            serde_json::json!({"action": "stop", "agent": "ghost"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ghost"), "{err}");
}

#[tokio::test]
async fn send_message_starts_an_idle_instance_before_returning() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "do work".into(),
        session.clone(),
    );
    session.agents.mark_idle("worker");
    let out = SendMessageTool::new(session.clone())
        .call(
            serde_json::json!({"to": "worker", "message": "start now", "ack_timeout": 0}),
            &main_ctx(&session),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let receipt: serde_json::Value = serde_json::from_str(out.content.as_str().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(receipt["status"], "queued");
    let status = &session.agents.list()[0];
    assert_eq!(status.state, crate::agents::AgentState::Running);
    assert_eq!(status.pending, 0, "the idle inbox was claimed immediately");
    let acks = session
        .agents
        .acks_of("worker")
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        acks[0].state,
        crate::agents::AckState::Delivered { run: 1 }
    ));
    let _ = session.agents.stop("worker");
}

#[tokio::test]
async fn send_message_keeps_running_instance_queued_for_its_next_tool_round() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "do work".into(),
        session.clone(),
    );
    let send = SendMessageTool::new(session.clone());
    let ctx = main_ctx(&session);
    // The acknowledgement wait is opt-in: omitting it keeps the plain fire-and-forget path.
    let schema = send.input_schema();
    assert!(schema["properties"]["ack_timeout"].is_object());
    assert_eq!(schema["required"], serde_json::json!(["message", "to"]));
    let out = send
        .call(
            serde_json::json!({"to": "worker", "message": "add more"}),
            &ctx,
        )
        .await
        .unwrap();
    // A running receiver keeps it queued until its query loop reaches the next tool round.
    let receipt: serde_json::Value = serde_json::from_str(out.content.as_str().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(receipt["status"], "queued");
    assert_eq!(receipt["message_id"], 1);
    let status = &session.agents.list()[0];
    assert_eq!(status.pending, 1);
    assert_eq!(status.unacked, 1, "queued is not yet a receipt");
    // Unknown instance: the error lists the existing instance names.
    let err = send
        .call(serde_json::json!({"to": "nobody", "message": "x"}), &ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("worker"), "{err}");
}

/// The chase protects a sender who never thought to ask for it — that is the whole point of a
/// default. Opting out has to be said out loud.
#[tokio::test]
async fn the_reply_check_is_on_by_default_and_zero_turns_it_off() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "do work".into(),
        session.clone(),
    );
    let send = SendMessageTool::new(session.clone());
    let ctx = main_ctx(&session);
    let receipt = |out: ToolResult| -> serde_json::Value {
        serde_json::from_str(out.content.as_str().unwrap_or_default())
            .unwrap_or_else(|e| panic!("{e}"))
    };

    let out = send
        .call(
            serde_json::json!({"to": "worker", "message": "default"}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        receipt(out)["ack_timeout_secs"],
        DEFAULT_ACK_TIMEOUT_SECS,
        "it is watched even without a request"
    );

    let out = send
        .call(
            serde_json::json!({"to": "worker", "message": "no wait for a reply", "ack_timeout": 0}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        receipt(out)["ack_timeout_secs"].is_null(),
        "0 = explicitly off"
    );

    let acks = session
        .agents
        .acks_of("worker")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(
        acks[0].timeout,
        Some(std::time::Duration::from_secs(DEFAULT_ACK_TIMEOUT_SECS))
    );
    assert_eq!(acks[1].timeout, None);
}

fn main_ctx(session: &Arc<Session>) -> crate::tool::ToolContext {
    crate::tool::ToolContext {
        home: std::env::temp_dir(),
        cwd: std::path::PathBuf::from("/tmp"),
        watch: session.watch.clone(),
        live: Default::default(),
        http: reqwest::Client::new(),
        tasks: session.tasks.clone(),
        hooks: crate::settings::HooksConfig::default(),
        permission_mode: "default".into(),
        expand_tasks: tokio::sync::watch::channel(false).0,
        ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        instance: None,
        rewind: Default::default(),
    }
}

/// A depth-1 sub-session under the same registries, instance name stamped —
/// the shape `build_sub_session` produces, minus the model plumbing these
/// tests do not exercise.
fn sub_of(parent: &Arc<Session>, instance: &str, rooms: bool) -> Arc<Session> {
    let mut settings = parent.settings.clone();
    settings.experimental.agent_channels = rooms;
    Arc::new(Session {
        depth: 1,
        instance: Some(instance.to_string()),
        settings,
        ..(**parent).clone()
    })
}

/// Hub-and-spoke was an addressing rule rather than a withheld tool, and D137
/// retired the rule while leaving the tool exactly where it was: a subagent
/// reaches a sibling, and the message arrives in that sibling's inbox under the
/// sender's own name — which is the whole reason the rule could go. A colleague
/// that could be written to but not identified is the D63 confusion.
#[tokio::test]
async fn a_subagent_reaches_a_sibling_and_the_sibling_learns_who_wrote() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "sibling",
        AgentKind::Hire,
        None,
        "work".into(),
        session.clone(),
    );
    let ctx = main_ctx(&session);
    let send = SendMessageTool::new(sub_of(&session, "scout", false));

    let out = send
        .call(
            serde_json::json!({"to": "sibling", "message": "take this"}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!out.is_error, "a peer is addressable now: {out:?}");
    assert_eq!(
        session.agents.pending_of("sibling"),
        vec![("scout".to_string(), "take this".to_string())],
        "it is in the sibling's inbox, from scout — not from main"
    );

    // A name nobody claimed still fails, and the failure names who exists.
    let err = send
        .call(
            serde_json::json!({"to": "ghost", "message": "hello?"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no subagent named ghost"), "{err}");

    let err = send
        .call(serde_json::json!({"to": "scout", "message": "note"}), &ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is you"), "{err}");

    // And main is reachable.
    let out = send
        .call(
            serde_json::json!({"to": "main", "message": "the migration is done"}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!out.is_error);
}

/// The receiver's context is where it matters: a colleague's message is headed
/// with the sender, and main's is not — the one voice "unmarked" is allowed to
/// mean, because the note promises it does.
#[test]
fn a_peer_s_message_arrives_headed_and_main_s_stays_bare() {
    let (session, _client) = parent_session();
    let absorb = |from: &str| {
        crate::tool::agent::absorb_inbox(
            &session.channels,
            "qa",
            &[crate::agents::InboxItem::Direct {
                id: crate::agents::MsgId(1),
                from: from.to_string(),
                text: "look at the parser".into(),
                images: Vec::new(),
            }],
        )
        .0
    };
    assert_eq!(
        absorb(crate::channels::MAIN_NAME),
        "look at the parser",
        "main is the default voice and stays verbatim"
    );
    assert_eq!(
        absorb("dev"),
        "[message from @dev]\nlook at the parser",
        "a colleague is named, in the shape main's own messages have always worn"
    );
    assert_eq!(
        absorb(crate::channels::USER_NAME),
        format!("{DM_FROM_USER_MARKER}\nlook at the parser"),
        "and the human keeps the marker D64 gave them"
    );
}

/// A chase names whoever is waiting, and tells the receiver where the answer
/// has to go — turn text reaches main and nobody else.
#[test]
fn a_chase_names_the_sender_that_is_still_waiting() {
    let (session, _client) = parent_session();
    let reg = &session.agents;
    reg.insert("qa", AgentKind::Hire, None, "w".into(), session.clone());
    let id = reg
        .deliver("qa", "dev", "does the parser handle EOF?", Vec::new(), None)
        .unwrap_or_else(|e| panic!("{e}"));
    let _ = reg.take_running("qa", 0);
    assert_eq!(
        reg.follow_up("qa", id),
        crate::agents::FollowUp::Sent { round: 1 }
    );
    let items = reg.take_running("qa", 0);
    let prompt = crate::tool::agent::absorb_inbox(&session.channels, "qa", &items).0;
    assert!(
        prompt.contains("@dev sent you message"),
        "the chase says who is waiting: {prompt}"
    );
    assert!(
        !prompt.contains("Main sent you"),
        "and does not put it on main: {prompt}"
    );
    assert!(
        prompt.contains("SendMessage(to: \"@dev\")"),
        "turn text does not reach a peer, so the chase says where the answer goes: {prompt}"
    );
}

/// The message lands in the store the query layer drains into main's next
/// turn, under the calling instance's real name — not main's, which is
/// what the old sender field hardcoded.
#[tokio::test]
async fn a_message_to_main_lands_in_the_inbox_under_the_sender_s_own_name() {
    let (session, _client) = parent_session();
    let ctx = main_ctx(&session);
    SendMessageTool::new(sub_of(&session, "scout", false))
        .call(
            serde_json::json!({"to": "@main", "message": "the migration is done"}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(session.channels.has_main_mail());
    assert!(
        !session.channels.take_main_mail_urgent(),
        "an ordinary message does not ring"
    );
    let mail = session.channels.drain_main_mail();
    assert_eq!(
        mail,
        vec!["[message from @scout]\nthe migration is done".to_string()],
        "the marker names who, and the text follows it"
    );
}

/// The `summary` field (D108): offered where it is drawn, accepted when
/// written, omitted when it is not, and never in the envelope.
///
/// The last is the load-bearing one. CC's *teammate* runtime carries the
/// summary to the recipient as a `summary="…"` attribute
/// (`utils/teammateMailbox.ts:386`); its *subagent* runtime — the one v4
/// replicates — passes only the message (`SendMessageTool.ts:810-814`). So
/// `main_mail` stays byte-identical and the preview is a fact about the
/// screen alone.
#[tokio::test]
async fn a_summary_previews_the_message_without_entering_it() {
    let (session, _client) = parent_session();
    let ctx = main_ctx(&session);
    let sub = SendMessageTool::new(sub_of(&session, "scout", false));

    let schema = sub.input_schema();
    assert_eq!(
        schema["properties"]["summary"]["type"],
        serde_json::json!(["string", "null"]),
        "offered to a subagent, and optional: {schema}"
    );
    assert_eq!(schema["required"], serde_json::json!(["message", "to"]));
    let mains = SendMessageTool::new(session.clone()).input_schema();
    assert!(
        mains["properties"].get("summary").is_none(),
        "and left off main's own schema, where nothing would draw it: {mains}"
    );

    sub.call(
        serde_json::json!({"to": "main", "message": "the migration is done", "summary": "migration done"}),
        &ctx,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        session.channels.drain_main_arrivals()[0].summary.as_deref(),
        Some("migration done")
    );
    assert_eq!(
        session.channels.drain_main_mail(),
        vec!["[message from @scout]\nthe migration is done".to_string()],
        "the model reads what was said to it, and nothing about the screen"
    );

    // Omitted: no preview on the arrival, and the renderer's fallback is
    // what it always was.
    sub.call(
        serde_json::json!({"to": "main", "message": "and the indexes"}),
        &ctx,
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(session.channels.drain_main_arrivals()[0].summary, None);

    // Main may still pass one: the field is off its schema, not out of the
    // parser — `deny_unknown_fields` would make a harmless word an error.
    assert!(
        serde_json::from_value::<SendMessageInput>(
            serde_json::json!({"to": "worker", "message": "look again", "summary": "recheck"})
        )
        .is_ok()
    );
}

/// `urgent` is the harness's bell and it has exactly one meaning: an agent
/// needs the user. Anywhere else there is nobody on the other end, so it is
/// refused rather than quietly ignored.
#[tokio::test]
async fn urgent_is_a_subagent_to_main_flag_and_refused_elsewhere() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "work".into(),
        session.clone(),
    );
    let ctx = main_ctx(&session);

    SendMessageTool::new(sub_of(&session, "scout", false))
        .call(
            serde_json::json!({"to": "main", "message": "I need the deploy key", "urgent": true}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        session.channels.take_main_mail_urgent(),
        "the bell is owed on arrival"
    );

    let err = SendMessageTool::new(session.clone())
        .call(
            serde_json::json!({"to": "worker", "message": "look now", "urgent": true}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("urgent only applies"), "{err}");
}

/// Room addressing is still behind the experimental gate, and a member has
/// to be a member. Both refusals name the room.
#[tokio::test]
async fn room_addressing_is_gated_and_checked() {
    let (session, _client) = parent_session();
    let ctx = main_ctx(&session);

    let err = SendMessageTool::new(sub_of(&session, "scout", false))
        .call(serde_json::json!({"to": "#build", "message": "hi"}), &ctx)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("#build"), "{err}");

    let with_rooms = sub_of(&session, "scout", true);
    let err = SendMessageTool::new(with_rooms.clone())
        .call(serde_json::json!({"to": "#ghost", "message": "hi"}), &ctx)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not a member of #ghost"),
        "an unknown room and a room you are not in are the same refusal: {err}"
    );

    session
        .channels
        .create(
            "build",
            vec!["scout".into()],
            crate::channels::ChannelMode::Free,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let out = SendMessageTool::new(with_rooms)
        .call(serde_json::json!({"to": "#build", "message": "hi"}), &ctx)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.content
            .as_str()
            .unwrap_or_default()
            .contains("#build msg #1"),
        "{out:?}"
    );
}

/// Contract 3's discriminator, in isolation: what the run was woken *by*
/// decides whether its end is main's business.
#[test]
fn only_an_all_user_batch_keeps_its_end_to_itself() {
    let user_item = |text: &str| InboxItem::Direct {
        id: MsgId(1),
        from: crate::channels::USER_NAME.to_string(),
        text: text.to_string(),
        images: Vec::new(),
    };
    let main_item = InboxItem::Direct {
        id: MsgId(2),
        from: crate::channels::MAIN_NAME.to_string(),
        text: "carry on".to_string(),
        images: Vec::new(),
    };
    assert!(
        wakes_owner(&[]),
        "a dispatch has no items: the Agent call itself is the trigger"
    );
    assert!(!wakes_owner(&[user_item("are you there?")]));
    assert!(!wakes_owner(&[user_item("one"), user_item("two")]));
    assert!(
        wakes_owner(&[user_item("one"), main_item]),
        "one main-origin item in the batch and the reply answers it"
    );
    assert!(wakes_owner(&[InboxItem::Channel {
        channel: "build".into(),
        from: "zoe".into(),
        text: "the tests pass".into(),
        seq: 3,
    }]));
}

/// A message that is never picked up is chased on the sender's own clock and then reported:
/// three follow-ups ride along with it, and the give-up lands in main's notification queue
/// rather than staying an unanswered "queued" nobody looks at again.
#[tokio::test(start_paused = true)]
async fn unacknowledged_message_is_chased_three_times_then_reported() {
    let (session, _client) = parent_session();
    // Running without a query loop: the dispatcher cannot claim it, so the message stays queued.
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "do work".into(),
        session.clone(),
    );
    let ctx = main_ctx(&session);
    let out = SendMessageTool::new(session.clone())
        .call(
            serde_json::json!({"to": "worker", "message": "check the logs", "ack_timeout": 1}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let receipt: serde_json::Value = serde_json::from_str(out.content.as_str().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        receipt["ack_timeout_secs"], 5,
        "waits below the lower bound are clamped"
    );

    // Four deadlines: three follow-ups, then the give-up.
    tokio::time::sleep(std::time::Duration::from_secs(5 * 5)).await;

    let acks = session
        .agents
        .acks_of("worker")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(
        acks[0].follow_ups, MAX_FOLLOW_UPS,
        "chased until the budget runs out"
    );
    assert_eq!(
        session.agents.list()[0].pending,
        1 + MAX_FOLLOW_UPS as usize,
        "one follow-up per round, in the inbox with the original"
    );
    let notes = session.watch.consume_notifications(None).await;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("follow-ups") && n.contains("worker")),
        "main is told after giving up: {notes:?}"
    );
}

/// Being read is not being answered: an instance that takes the message and stays quiet is
/// chased exactly like one that never picked it up, and the sender hears about it.
#[tokio::test(start_paused = true)]
async fn a_receiver_that_reads_and_says_nothing_is_still_chased() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "mute",
        AgentKind::Hire,
        None,
        "silent".into(),
        session.clone(),
    );
    let ctx = main_ctx(&session);
    SendMessageTool::new(session.clone())
        .call(
            serde_json::json!({"to": "mute", "message": "report progress", "ack_timeout": 5}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    // A turn ends without a word and takes the queued message into the next one: delivered,
    // unanswered, and still Running — so the flush the watchdog retries stays a no-op here.
    assert!(session.agents.finish("mute", Vec::new(), 0).is_some());
    assert!(matches!(
        session
            .agents
            .acks_of("mute")
            .unwrap_or_else(|| unreachable!())[0]
            .state,
        crate::agents::AckState::Delivered { .. }
    ));

    tokio::time::sleep(std::time::Duration::from_secs(5 * 5)).await;

    let acks = session
        .agents
        .acks_of("mute")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(
        acks[0].follow_ups, MAX_FOLLOW_UPS,
        "read-but-silent is still chased to the end"
    );
    assert_eq!(session.agents.list()[0].pending, MAX_FOLLOW_UPS as usize);
    let notes = session.watch.consume_notifications(None).await;
    assert!(
        notes.iter().any(|n| n.contains("still has not replied")),
        "silence is eventually reported to main: {notes:?}"
    );
}

/// The silent half of the same mechanism: a message answered inside its wait leaves no watch
/// line and no notification — the chase only speaks when something went wrong.
#[tokio::test(start_paused = true)]
async fn an_acknowledged_message_reports_nothing() {
    let (session, _client) = parent_session();
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "do work".into(),
        session.clone(),
    );
    let ctx = main_ctx(&session);
    SendMessageTool::new(session.clone())
        .call(
            serde_json::json!({"to": "worker", "message": "check the logs", "ack_timeout": 60}),
            &ctx,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    // The receiver picks it up at the boundary, then that run ends with something to say.
    assert!(session.agents.finish("worker", Vec::new(), 1).is_some());
    assert!(session.agents.finish("worker", Vec::new(), 2).is_none());
    tokio::time::sleep(std::time::Duration::from_secs(120)).await;
    let acks = session
        .agents
        .acks_of("worker")
        .unwrap_or_else(|| unreachable!());
    assert!(matches!(
        acks[0].state,
        crate::agents::AckState::Answered { .. }
    ));
    assert_eq!(
        acks[0].follow_ups, 0,
        "an on-time reply does not trigger chasing"
    );
    assert!(
        session.watch.consume_notifications(None).await.is_empty(),
        "no news, no nagging main"
    );
    assert!(
        session.watch.snapshot().is_empty(),
        "and leaves no board line"
    );
}

/// Main forwards an image to a subagent by repeating its `#[image N]` marker: the
/// attachment table is shared with the sub-session, and the resolved images ride along with
/// the queued instruction so a busy instance still receives them.
#[test]
fn image_markers_resolve_for_spawn_and_follow_up() {
    let (session, _client) = parent_session();
    let png = {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap_or_else(|_| unreachable!());
        out
    };
    assert_eq!(session.attachments.register(&png), Some(1));

    // Spawn: markers in the prompt resolve against the session table.
    let images = session
        .attachments
        .resolve("look at this #[image 1] and decide");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].media_type, "image/png");
    // Sub-sessions share the table, so a nested spawn can resolve the same marker.
    let sub = build_sub_session(
        &session,
        None,
        None,
        None,
        None,
        "worker",
        MemberContext::default(),
    )
    .unwrap();
    assert_eq!(sub.attachments.resolve("#[image 1]").len(), 1);

    // Follow-up: a queued instruction keeps its images until it is delivered.
    session
        .agents
        .insert("worker", AgentKind::Hire, None, "d".into(), sub.clone());
    let id = session
        .agents
        .deliver(
            "worker",
            crate::channels::MAIN_NAME,
            "compare #[image 1]",
            images.clone(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let (prompt, carried) = match session.agents.finish("worker", Vec::new(), 1) {
        Some(next) => absorb_inbox(&sub.channels, "worker", &next.items),
        None => unreachable!("queued messages should be claimed by the receiver"),
    };
    let acks = session
        .agents
        .acks_of("worker")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(acks[0].id, id);
    assert_eq!(prompt, "compare #[image 1]");
    assert_eq!(
        carried.len(),
        1,
        "images arrive with the queued instruction"
    );
    assert_eq!(carried[0].data, images[0].data);
}

/// D64: who wrote a direct message is part of the message. The user's DMs arrive under
/// the `[DM from user]` line — alone or batched with main traffic — while a single main
/// instruction stays byte-identical, so the common SendMessage path is unchanged.
#[test]
fn absorb_inbox_names_the_user_and_keeps_main_verbatim() {
    let (session, _client) = parent_session();
    let sub = build_sub_session(
        &session,
        None,
        None,
        None,
        None,
        "worker",
        MemberContext::default(),
    )
    .unwrap_or_else(|e| panic!("spawn: {e}"));
    session
        .agents
        .insert("worker", AgentKind::Hire, None, "d".into(), sub.clone());

    let deliver = |from: &str, text: &str| {
        session
            .agents
            .deliver("worker", from, text, Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
    };
    let absorb = || match session.agents.finish("worker", Vec::new(), 0) {
        Some(next) => absorb_inbox(&sub.channels, "worker", &next.items).0,
        None => unreachable!("queued messages should be claimed by the receiver"),
    };

    deliver(crate::channels::USER_NAME, "are you there?");
    assert_eq!(absorb(), format!("{DM_FROM_USER_MARKER}\nare you there?"));

    deliver(crate::channels::MAIN_NAME, "map the module");
    assert_eq!(absorb(), "map the module", "main singles stay verbatim");

    deliver(crate::channels::MAIN_NAME, "first");
    deliver(crate::channels::USER_NAME, "second");
    assert_eq!(
        absorb(),
        format!("[follow-up instruction] first\n{DM_FROM_USER_MARKER}\nsecond"),
        "a batch labels main's line and marks the user's"
    );
}

/// A text-only main session can still get an image looked at: the attachment table is
/// session-scoped and independent of endpoint capability, so a subagent forked onto an
/// image-capable provider resolves the same `#[image N]` marker and actually receives it.
#[test]
fn text_only_parent_can_hand_an_image_to_a_vision_subagent() {
    let (parent, _client) = parent_session();
    let png = {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([9u8, 9, 9, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap_or_else(|_| unreachable!());
        out
    };
    assert_eq!(parent.attachments.register(&png), Some(1));
    assert!(
        !parent.client.supports_images(),
        "the parent endpoint does not accept images (a precondition of this test)"
    );

    // Markers resolve regardless of what the parent endpoint can carry.
    let images = parent.attachments.resolve("describe #[image 1]");
    assert_eq!(
        images.len(),
        1,
        "resolution is unaffected by the endpoint's capabilities"
    );

    // Forked onto the vision provider, the sub-session is the one whose capability decides.
    let sub = build_sub_session(
        &parent,
        Some("vision-model".into()),
        Some("vision".into()),
        None,
        None,
        "looker",
        MemberContext::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        sub.client.supports_images(),
        "the sub-session endpoint accepts images"
    );
    assert!(
        Arc::ptr_eq(&sub.attachments, &parent.attachments),
        "the attachment table is shared; restating the placeholder hits it"
    );
    assert!(
        parent
            .client
            .image_capable_providers()
            .contains(&"vision".to_string()),
        "the path pointed to in the prompt is discoverable: {:?}",
        parent.client.image_capable_providers()
    );
}

/// `inherit_system: false` opts back into wholesale replacement; the subagent note is still
/// appended, because it describes the runtime rather than the persona.
#[test]
fn inherit_system_false_replaces_parent_blocks() {
    let (session, _client) = parent_session();
    let mut d = def("reviewer");
    d.inherit_system = false;
    let tool = AgentTool::new(session, vec![d.clone()]);
    let sub = tool
        .build_sub_session(&params("review"), Some(&d), "sub", &crewless())
        .unwrap();
    let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "You are the reviewer.",
            SUBAGENT_NOTE,
            &capability_block("def-model", "ds")
        ]
    );
}

/// Channel etiquette rides in the system prompt, and only when channels are on.
///
/// The placement is the point: it outlives compaction. That is not asserted here because it
/// cannot fail — `compact::maybe_compact` takes `&Session`, so the borrow checker forbids it
/// from touching `Session::system` at all; it splices `messages` and builds its summary
/// request with `system: Vec::new()`. A test that re-stated that would prove nothing.
#[test]
fn channel_note_is_gated_by_the_flag() {
    let (off, _c1) = parent_session();
    assert!(!off.settings.experimental.agent_channels, "off by default");
    let sub = build_sub_session(
        &off,
        None,
        None,
        None,
        None,
        "solo",
        MemberContext::default(),
    )
    .unwrap_or_else(|e| panic!("spawn: {e}"));
    assert!(
        !sub.system.iter().any(|b| b.text == CHANNEL_NOTE),
        "channel etiquette must not be injected when channels are off"
    );

    let (mut on, _c2) = parent_session();
    let session = Arc::get_mut(&mut on).unwrap_or_else(|| panic!("exclusive"));
    session.settings.experimental.agent_channels = true;
    let sub = build_sub_session(
        &on,
        None,
        None,
        None,
        None,
        "member",
        MemberContext::default(),
    )
    .unwrap_or_else(|e| panic!("spawn: {e}"));
    assert!(sub.system.iter().any(|b| b.text == CHANNEL_NOTE));
    // Both failure modes have to survive edits to this text: the storm it was written
    // for, and the over-correction where nobody answers the human at all.
    assert!(
        CHANNEL_NOTE.contains("Never `@` the person you are answering"),
        "v7 R3: the reply-to-replies storm is closed by a rule about the sigil, not by \
         an appeal to restraint — a ping-pong needs the `@` to keep going"
    );
    assert!(
        CHANNEL_NOTE.contains("An acknowledgement is not an answer"),
        "v7 R2: without it a member discharges an `@` with \"got it\""
    );
    assert!(
        CHANNEL_NOTE.contains("A name you are quoting is written without the `@`"),
        "v7 R5: `@` is a summons — a recap that names people would otherwise put the \
         whole room on the hook"
    );
    assert!(
        CHANNEL_NOTE.contains("a question from `user`"),
        "v7 R1's one exception, and D48's lesson in observable form: the obligation \
         follows who asked, not a judgement about what is still unanswered"
    );
    assert!(
        CHANNEL_NOTE.contains("Never work out what you owe by judging"),
        "v7 R1: the inference the model has no signal for is banned outright — D124 \
         is what one of those judgement calls cost"
    );
    assert!(
        CHANNEL_NOTE.contains("puts words in the room"),
        "must state that the turn body never reaches the channel — otherwise members think they already answered"
    );
    assert!(
        CHANNEL_NOTE.contains("The `@` decides what you owe"),
        "the owing rule is the @ (D119) — without it the D112 who-spoke doctrine \
         resurfaces and every unnamed user line demands a chorus again"
    );
    assert!(
        CHANNEL_NOTE.contains("one *covered* answer"),
        "@all keeps the covered-answer clause — the anti-chorus half of D112 survives \
         on the one broadcast form left"
    );

    assert!(
        CHANNEL_NOTE.contains("Silence belongs in the room, not in your turn text"),
        "D124: \"end the turn without posting\" alone reads as \"produce nothing at all\", \
         and a turn with neither text nor tool call reports nothing to main (the anchor \
         phrase must sit on one line)"
    );
    assert!(
        CHANNEL_NOTE.contains("fire alarm"),
        "@all needs a cost the model can feel, or every FYI wears it"
    );
    assert!(
        CHANNEL_NOTE.contains("Never answer a direct message in the room"),
        "must keep a private question out of the room, whoever asked it"
    );
    assert!(
        CHANNEL_NOTE.contains("A colleague reads none of it")
            && CHANNEL_NOTE.contains("SendMessage(to: \"@name\")"),
        "a peer does not read turn text, so the note must say where its answer goes — \
         without it a member answers a colleague in prose and believes it has replied (D137)"
    );
    assert!(
        CHANNEL_NOTE.contains(crate::channels::AGENT_MESSAGE_PREFIX),
        "the lane rule needs the observable tag for a colleague's message too, not just the concept"
    );
    assert!(
        CHANNEL_NOTE.contains("stays private"),
        "must forbid relaying DM content into a channel, not just answering there"
    );
    assert!(
        CHANNEL_NOTE.contains(DM_FROM_USER_MARKER),
        "the medium rule needs the observable tag, not just the concept"
    );
    assert!(
        CHANNEL_NOTE.contains("without waiting to be asked"),
        "must impose the proactive duty to speak in the room — otherwise a team-wide finding \
         reaches only main as turn text and the room works on stale ground (D67)"
    );
    assert!(
        CHANNEL_NOTE.contains("stays in your turn text"),
        "must keep member status out of the room — without this second half the venue rule \
         reopens the reply storm through a new door (D67)"
    );
    assert!(
        CHANNEL_NOTE.contains("Every room message reaches you at once")
            && CHANNEL_NOTE.contains("not when a line reaches you"),
        "must state v7's wake rule and separate it from the `@` — the note carried v6's \
         \"unnamed traffic arrives later\" until D131, which D129 had already made false, \
         and a member that believes a line is still in flight has a reason not to answer it"
    );

    // Main's half (D119; the narration ban reversed by D123 on the user's
    // ruling): the anchors keep the briefing duty, and its form, in place.
    let main_note = crate::tool::agent_notes::MAIN_CHANNEL_NOTE;
    assert!(
        main_note.contains("every\none of them reaches you"),
        "main is a member and reads the room whole (v7) — the tier it used to be told about \
         was the delivery gate D129 deleted"
    );
    assert!(
        main_note.contains("Keep the user posted on their team"),
        "main is the user's eyes on the team (D123) — a digest read in silence is the defect"
    );
    assert!(
        main_note.contains("everything that reaches you about somebody else"),
        "D129: the tiers cover task notifications too — five members each reporting that \
         they read a greeting is five lines saying nothing happened"
    );
    assert!(
        main_note.contains("say nothing, and know it"),
        "v7: pure progress is held, not narrated — main's value is being current, not \
         reading the room aloud"
    );
    assert!(
        main_note.contains("Never state a position the user has not taken"),
        "v7 R7c: main may answer for the user where it knows, and the one thing it must \
         never do is invent them — the model is at its most fluent exactly here"
    );
    assert!(
        main_note.contains("A briefing is not a transcript"),
        "the flood guard is form, not silence: compressed, own words, verbatim stays on \
         the room's page (and the anchor sits on one line)"
    );
    assert!(
        main_note.contains("SendMessage(to: \"#room\")"),
        "main answers a room in the room — prose is a note to the user"
    );
    assert!(
        main_note.contains("fire alarm"),
        "the @ discipline binds main too (and the anchor sits on one line)"
    );
}

/// The user reads a member's turn text (D57, and since D105 in the zoomed view), so the
/// subagent note may not claim the user never sees it. That claim is what made a member
/// written to privately believe the only way to reach the human was a room message (D63).
///
/// Reworded for D108: the note named "your direct-message window", a v3 surface that
/// retired with the buffers. The claim it was making — that the user has a private line
/// to this instance and reads its turns — is what is asserted, through the marker that
/// identifies such a message and the sentence that says the user can write one.
#[test]
fn subagent_note_knows_the_user_can_write_to_it() {
    assert!(
        SUBAGENT_NOTE.contains("Three voices write to you privately"),
        "must name the private lines an instance is reachable on — the human's above all"
    );
    assert!(
        SUBAGENT_NOTE.contains(crate::channels::AGENT_MESSAGE_PREFIX)
            && SUBAGENT_NOTE.contains("A colleague does not read it"),
        "a colleague writes here too now, and the one thing the model cannot observe is that \
         its prose does not reach them (D137)"
    );
    assert!(
        !SUBAGENT_NOTE.contains("not displayed to the user"),
        "the old claim was false once the private line existed, and it routed private answers into channels"
    );
    assert!(
        SUBAGENT_NOTE.contains(DM_FROM_USER_MARKER),
        "must teach the tag that identifies the human's messages (D64)"
    );
}

/// A crew member's memory arrives as a system block, not as history and not as
/// a message: nobody said it, and the whole point of D51 is that the past stays
/// on disk until the member decides to fetch it. An ad-hoc subagent has no past
/// and is told nothing.
#[test]
fn memory_note_rides_the_system_prompt_when_there_is_one() {
    let (parent, _c) = parent_session();
    let note = "your past is at /tmp/qa.md".to_string();
    let sub = build_sub_session(
        &parent,
        None,
        None,
        None,
        None,
        "qa",
        MemberContext {
            memory: Some(note.clone()),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("spawn: {e}"));
    assert!(
        sub.system.iter().any(|b| b.text == note),
        "the pointer is in the system prompt"
    );
    assert!(
        sub.system.iter().all(|b| !b.cache),
        "a per-member tail block must not open another cache breakpoint"
    );

    let solo = build_sub_session(
        &parent,
        None,
        None,
        None,
        None,
        "solo",
        MemberContext::default(),
    )
    .unwrap_or_else(|e| panic!("spawn: {e}"));
    assert!(
        !solo
            .system
            .iter()
            .any(|b| b.text.contains("your past is at")),
        "an ad-hoc subagent is told nothing about a past it does not have"
    );
}

/// No named definition: the parent's system carries over, plus the note.
#[test]
fn plain_subagent_inherits_parent_system_plus_note() {
    let (session, _client) = parent_session();
    let sub = build_sub_session(
        &session,
        None,
        None,
        None,
        None,
        "worker",
        MemberContext::default(),
    )
    .unwrap();
    let texts: Vec<&str> = sub.system.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(
        texts,
        [
            "parent system",
            SUBAGENT_NOTE,
            &capability_block("parent-model", "default")
        ]
    );
    let moved = std::env::temp_dir().join("bingo-subagent-shared-cwd");
    session.set_cwd(moved.clone());
    assert_eq!(
        sub.cwd(),
        moved,
        "ad-hoc subagents follow the parent session's cwd"
    );
    assert!(
        !sub.system.last().map(|b| b.cache).unwrap_or(true),
        "the note block does not occupy a cache breakpoint"
    );
}

/// MCP connections and the permission table are shared handles, not snapshots: a subagent
/// sees the parent's MCP tools, and `/permissions` edits reach instances already running.
#[test]
fn sub_session_shares_parent_mcp_and_permissions() {
    let (parent, _) = parent_session();
    let sub = build_sub_session(
        &parent,
        None,
        None,
        None,
        None,
        "worker",
        MemberContext::default(),
    )
    .unwrap();
    assert!(
        Arc::ptr_eq(&sub.runtime.mcp, &parent.runtime.mcp),
        "the MCP manager should be shared, otherwise subagents get no MCP tools"
    );
    assert!(
        Arc::ptr_eq(&sub.runtime.permissions, &parent.runtime.permissions),
        "the permission tables should be shared, otherwise /permissions changes after spawn never reach subagents"
    );
}

/// A sink bound to `worker`, with the receiver to read back what a run put on
/// it.
fn worker_sink() -> (
    crate::ui::EventSink,
    tokio::sync::mpsc::UnboundedReceiver<crate::ui::Addressed>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        crate::ui::EventSink::new(crate::ui::ConvKey::Agent("worker".into()), tx),
        rx,
    )
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::ui::Addressed>) -> Vec<UiEvent> {
    let mut out = Vec::new();
    while let Ok(addressed) = rx.try_recv() {
        assert_eq!(
            addressed.to,
            crate::ui::ConvKey::Agent("worker".into()),
            "every event names the conversation it happened in"
        );
        out.push(addressed.event);
    }
    out
}

/// A subagent's turn reaches the console as the same events main's does (D134),
/// addressed to the instance — reasoning, the call, its answer and the prose,
/// in the order they happened — while the flat reply stays prose-only, because
/// that string is the spawn's return value.
#[tokio::test]
async fn a_subagents_turn_streams_as_addressed_events() {
    let output = Arc::new(Mutex::new(String::new()));
    let (sink, mut rx) = worker_sink();
    let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
    let watch = crate::app::AppCore::start(Default::default()).watch();
    let registry = AgentRegistry::new();
    let cell = Arc::new(AgentCell::new(registry.clone()));
    let id = register_run_watch(
        &watch,
        "think".into(),
        cell.clone(),
        Vec::new(),
        None,
        true,
        true,
    );
    let ui = subagent_hooks(
        SubagentOutput {
            text: output.clone(),
            progress,
        },
        Some(sink),
        cell,
        watch,
        id,
        "worker".into(),
        None,
    );
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "first ".into(),
        });
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "phase".into(),
        });
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: serde_json::json!({"file_path": "a"}),
        standalone: false,
    });
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "second phase".into(),
        });
    ui.events
        .emit(EngineEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: "a".into(),
            output: "one line".into(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 4,
        }));
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "the answer".into(),
        });

    let events = drain(&mut rx);
    let thinking: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            UiEvent::ThinkingDelta(text) => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        thinking,
        ["first ", "phase", "second phase"],
        "reasoning streams delta by delta, as main's does — the console folds \
         the phases, because folding them twice is what D132 was about: {events:?}"
    );
    // The call carries what was called and what came back (D132), and both
    // reach the console in the round they happen rather than at run end.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::ToolReady { name, input, .. }
            if name == "Read" && input["file_path"] == "a")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::ToolDone(done)
            if done.tool_call_id == "test-tool" && done.output == "one line")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, UiEvent::TextDelta(text) if text == "the answer")),
        "{events:?}"
    );
    assert_eq!(
        &*output.lock().unwrap_or_else(|e| e.into_inner()),
        "the answer",
        "reasoning never leaks into the flat reply"
    );
}

#[tokio::test]
async fn subagent_retry_restores_the_current_attempt_checkpoint() {
    let output = Arc::new(Mutex::new(String::new()));
    let (sink, mut rx) = worker_sink();
    let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
    let watch = crate::app::AppCore::start(Default::default()).watch();
    let registry = AgentRegistry::new();
    let cell = Arc::new(AgentCell::new(registry.clone()));
    let id = register_run_watch(
        &watch,
        "retry".into(),
        cell.clone(),
        Vec::new(),
        None,
        true,
        true,
    );
    let ui = subagent_hooks(
        SubagentOutput {
            text: output.clone(),
            progress: progress.clone(),
        },
        Some(sink),
        cell.clone(),
        watch,
        id,
        "worker".into(),
        None,
    );
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "committed".into(),
        });
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: serde_json::json!({"file_path":"a"}),
        standalone: false,
    });
    ui.events.emit(EngineEvent::RoundEnd);
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "partial".into(),
        });
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: serde_json::json!({"command":"bad"}),
        standalone: false,
    });
    ui.events.emit(EngineEvent::StreamRetry);
    ui.events.warn("Reconnecting... 2/10");
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "answer".into(),
        });

    assert_eq!(
        &*output.lock().unwrap_or_else(|e| e.into_inner()),
        "committedanswer"
    );
    let events = drain(&mut rx);
    // The *rendered* half of the rollback is the console's, and it always was:
    // `StreamRetry` is what unwinds main's failed attempt, and an instance's
    // turn is on the same channel now. What this hook still owns is the flat
    // reply, the produced-character count and the progress cell.
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::StreamRetry)),
        "the console is told to unwind the attempt: {events:?}"
    );
    assert!(
        events.iter().any(
            |e| matches!(e, UiEvent::Warning(text) if text == "@worker · Reconnecting... 2/10")
        ),
        "a reconnect notice takes the warning tier instead of being spliced into the \
         instance's own prose — and wears whose stream it is about, because the tier \
         is shared and an unattributed one reads as the console's: {events:?}"
    );
    let progress = progress.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(progress.tool_uses, 1);
    assert_eq!(cell.chars(), "committedanswer".chars().count());
}

#[tokio::test]
async fn subagent_progress_accumulates_tokens_tools_and_recent_activity() {
    let output = Arc::new(Mutex::new(String::new()));
    let progress = Arc::new(Mutex::new(crate::agents::AgentProgress::default()));
    progress
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .start_run();
    let watch = crate::app::AppCore::start(Default::default()).watch();
    let registry = AgentRegistry::new();
    let id = register_run_watch(
        &watch,
        "progress".into(),
        Arc::new(AgentCell::new(registry.clone())),
        Vec::new(),
        None,
        true,
        true,
    );
    let ui = subagent_hooks(
        SubagentOutput {
            text: output,
            progress: progress.clone(),
        },
        None,
        Arc::new(AgentCell::new(registry.clone())),
        watch,
        id,
        "worker".into(),
        None,
    );
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::StopReason {
            stop_reason: Some("tool_use".into()),
            output_tokens: Some(12),
        });
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: serde_json::json!({"file_path":"src/main.rs"}),
        standalone: false,
    });
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::StopReason {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(7),
        });
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: serde_json::json!({"command":"cargo check"}),
        standalone: false,
    });
    let progress = progress.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(progress.output_tokens, 19);
    assert_eq!(progress.tool_uses, 2);
    assert_eq!(progress.recent_activity.len(), 2);
    assert!(progress.recent_activity[0].contains("Read"));
    assert!(progress.recent_activity[1].contains("Bash"));
}

/// A subagent's Ask decision is forwarded to the attached prompt surface, stamped with the
/// instance name — never silently auto-denied (or auto-allowed under bypass).
#[tokio::test]
async fn subagent_hooks_touch_activity_on_stream_and_tool_signals() {
    let session = parent_session().0;
    session.agents.insert(
        "worker",
        AgentKind::Hire,
        None,
        "work".into(),
        session.clone(),
    );
    let watch = crate::app::AppCore::start(Default::default()).watch();
    let registry = session.agents.clone();
    let id = register_run_watch(
        &watch,
        "l".into(),
        Arc::new(AgentCell::new(registry.clone())),
        Vec::new(),
        None,
        true,
        true,
    );
    let ui = subagent_hooks(
        SubagentOutput {
            text: Arc::new(Mutex::new(String::new())),
            progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
        },
        None,
        Arc::new(AgentCell::new(registry.clone())),
        watch,
        id,
        "worker".into(),
        None,
    );
    let inserted = session.agents.list()[0].last_active;
    std::thread::sleep(std::time::Duration::from_millis(2));
    ui.events
        .emit_stream(&crate::api::contract::StreamEvent::TextDelta {
            index: 0,
            text: "hi".into(),
        });
    let streamed = session.agents.list()[0].last_active;
    assert!(streamed > inserted);

    std::thread::sleep(std::time::Duration::from_millis(2));
    ui.events.emit(EngineEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: serde_json::json!({"file_path": "a"}),
        standalone: false,
    });
    let ready = session.agents.list()[0].last_active;
    assert!(ready > streamed);

    std::thread::sleep(std::time::Duration::from_millis(2));
    ui.events
        .emit(EngineEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: String::new(),
            output: String::new(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 1,
        }));
    assert!(session.agents.list()[0].last_active > ready);
}

#[tokio::test]
async fn subagent_ask_forwards_to_attached_prompt() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = seen.clone();
    let ask: Arc<crate::query::AskFn> = Arc::new(move |request| {
        recorder
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "{}|{}|{}",
                request.tool,
                request.reason,
                request.scope.unwrap_or("-")
            ));
        Box::pin(async { crate::query::AskOutcome::Allow })
    });
    let watch = crate::app::AppCore::start(Default::default()).watch();
    let registry = AgentRegistry::new();
    let id = register_run_watch(
        &watch,
        "l".into(),
        Arc::new(AgentCell::new(registry.clone())),
        Vec::new(),
        None,
        true,
        true,
    );
    let ui = subagent_hooks(
        SubagentOutput {
            text: Arc::new(Mutex::new(String::new())),
            progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
        },
        None,
        Arc::new(AgentCell::new(registry.clone())),
        watch.clone(),
        id,
        "worker".into(),
        Some(ask),
    );
    let input = serde_json::json!({ "file_path": "/tmp/x.txt" });
    let request = crate::query::AskContext {
        tool: "Write",
        reason: "Write needs permission",
        input: &input,
        cwd: &std::env::temp_dir(),
        scope: Some("Write(/tmp/)"),
        diff: None,
    };
    assert!((ui.requests.ask)(&request).await.allowed());
    assert_eq!(
        seen.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
        ["Write|worker · Write needs permission|Write(/tmp/)"],
        "the instance stamps the reason; the scope travels untouched"
    );

    // Nothing attached (embedded/test path): deny rather than block on a modal nobody shows.
    let ui = subagent_hooks(
        SubagentOutput {
            text: Arc::new(Mutex::new(String::new())),
            progress: Arc::new(Mutex::new(crate::agents::AgentProgress::default())),
        },
        None,
        Arc::new(AgentCell::new(registry.clone())),
        watch,
        id,
        "worker".into(),
        None,
    );
    assert_eq!(
        (ui.requests.ask)(&request).await,
        crate::query::AskOutcome::Deny { feedback: None }
    );
}
