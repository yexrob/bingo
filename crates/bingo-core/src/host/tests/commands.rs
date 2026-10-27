//! The kernel's own commands through a real host (ADR-0008 §4).

use super::*;
use crate::settings;

static CONTEXT: PluginManifest = PluginManifest {
    id: "test.context",
    version: "0",
    uses: &[],
    provides: &["compactor"],
    requires: &[],
    config: None,
};

/// Two scripted models with opposite reasoning metadata: the requested
/// level must reach either one.
async fn host_for(
    scripts: Vec<Script>,
    compactor: Option<Arc<dyn Compactor>>,
) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(scripts);
    let mut plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(provider.clone())],
    )];
    if let Some(compactor) = compactor {
        plugins.push(TestPlugin::boxed(
            &CONTEXT,
            vec![Contribution::Compactor(compactor)],
        ));
    }
    let config = HostConfig::new(env()).with_layer(
        "cli",
        json!({
            "model": "m",
            "models": {
                "scripted/m": { "reasoning": true },
                "scripted/m2": { "reasoning": false },
            }
        }),
    );
    (Host::build(plugins, config).await.unwrap(), provider)
}

pub(super) struct Client {
    pub(super) state: SessionState,
    pub(super) events: FrameStream,
    pub(super) handle: SessionHandle,
}

impl Client {
    pub(super) async fn open(host: &Host) -> Self {
        let attachment = host
            .open(
                SessionSelector::Create {
                    spec: spec("/work"),
                },
                who(),
                OpenOptions::default(),
            )
            .await
            .unwrap();
        Self {
            state: attachment.snapshot,
            events: attachment.events,
            handle: attachment.handle,
        }
    }

    /// Submit a line and fold frames until its ack; returns the ack and the
    /// frames before it.
    pub(super) async fn ack(&mut self, line: &str) -> (IntentOutcome, Vec<Frame>) {
        let intent = IntentId::mint();
        self.handle
            .submit(intent.clone(), Input::text(line, Origin::surface("rpc")));
        let mut seen = Vec::new();
        while let Some(frame) = self.events.next().await {
            self.state.apply(&frame);
            if let Event::IntentAck { intent: i, outcome } = &frame.event
                && i == &intent
            {
                return (outcome.clone(), seen);
            }
            seen.push(frame);
        }
        panic!("the stream ended before the ack");
    }

    pub(super) async fn until_turn_completed(&mut self) -> Vec<Frame> {
        let mut seen = Vec::new();
        while let Some(frame) = self.events.next().await {
            self.state.apply(&frame);
            let done = matches!(frame.event, Event::TurnCompleted { .. });
            seen.push(frame);
            if done {
                return seen;
            }
        }
        panic!("the stream ended before the turn completed");
    }
}

pub(super) fn message(outcome: &IntentOutcome) -> String {
    match outcome {
        IntentOutcome::Applied { result } => result["message"].as_str().unwrap().to_string(),
        other => panic!("not applied: {other:?}"),
    }
}

#[tokio::test]
async fn model_and_think_change_the_next_turn_and_are_announced() {
    let (host, provider) = host_for(
        vec![Script::Events(text("hi")), Script::Events(text("again"))],
        None,
    )
    .await;
    let mut client = Client::open(&host).await;
    assert_eq!(client.state.config.kernel["thinking"], json!(null));
    assert!(
        client.state.config.kernel["policy"].is_string(),
        "the kernel names the policy whose slice a client reads"
    );

    let (ack, before) = client.ack("/model m2").await;
    assert_eq!(
        message(&ack),
        "⇄ m2 · scripted · thinking off · this session · next-start default"
    );
    assert_eq!(client.state.summary.model.as_deref(), Some("m2"));
    assert!(
        before
            .iter()
            .any(|f| matches!(&f.event, Event::SessionUpdated { summary } if summary.model.as_deref() == Some("m2"))),
        "the new model is announced before the ack"
    );
    assert!(
        !before
            .iter()
            .any(|f| matches!(f.event, Event::ConfigChanged { .. })),
        "nothing in the config view changed, so nothing was announced"
    );

    let (ack, _) = client.ack("/think high").await;
    assert_eq!(message(&ack), "thinking: high · this session");
    assert_eq!(client.state.config.kernel["thinking"], json!("high"));

    let (ack, _) = client.ack("/model").await;
    let listing = shown(&ack);
    assert!(
        listing.starts_with(
            "model: scripted/m2\nthinking: high\nusage: /model [<provider>/]<model>\n"
        )
    );
    assert!(!listing.lines().any(|id| id == "scripted/m"));
    assert!(listing.contains(" · /models lists them"), "{listing}");

    client.ack("hello").await;
    client.until_turn_completed().await;
    let request = &provider.requests()[0];
    assert_eq!(request.model, "m2");
    assert_eq!(request.reasoning, Some(Effort::High));

    let (ack, _) = client.ack("/think off").await;
    assert_eq!(message(&ack), "thinking: off · this session");
    assert_eq!(client.state.config.kernel["thinking"], json!(null));
    client.ack("and again").await;
    client.until_turn_completed().await;
    assert_eq!(
        provider.requests()[1].reasoning,
        None,
        "off means no reasoning parameter on the wire"
    );

    let (ack, _) = client.ack("/think loud").await;
    assert!(
        matches!(ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::InvalidInput)
    );

    let (ack, _) = client.ack("/model").await;
    let listing = shown(&ack);
    assert!(listing.starts_with("model: scripted/m2\nusage: /model [<provider>/]<model>\n"));
    assert!(!listing.lines().any(|id| id == "scripted/m"));
    assert!(listing.contains(" · /models lists them"), "{listing}");
}

/// The text of a `View::Text` outcome.
fn shown(outcome: &IntentOutcome) -> String {
    match outcome {
        IntentOutcome::Applied { result } => serde_json::from_value::<View>(result["view"].clone())
            .unwrap()
            .fold(),
        other => panic!("not a view: {other:?}"),
    }
}

/// The catalogue is not a gate: every level reaches an unknown model, and
/// `off` alone omits it. The acknowledgement, view and request must agree.
#[tokio::test]
async fn think_reaches_an_unknown_model_at_every_level_and_off_omits_it() {
    let scripts = Effort::words()
        .map(|_| Script::Events(text("hi")))
        .collect();
    let (host, provider) = host_for(scripts, None).await;
    let mut client = Client::open(&host).await;
    let (ack, _) = client.ack("/model plain").await;
    assert_eq!(
        message(&ack),
        "⇄ plain · scripted · thinking off · this session · next-start default"
    );

    for (index, word) in Effort::words().enumerate() {
        let level = Effort::spoken(word).expect("a level");
        let (ack, _) = client.ack(&format!("/think {word}")).await;
        assert_eq!(message(&ack), format!("thinking: {word} · this session"));
        assert_eq!(client.state.config.kernel["thinking"], json!(level));
        let (ack, _) = client.ack("/think").await;
        assert!(shown(&ack).starts_with(&format!("thinking: {word}\nusage:")));

        client.ack("hello").await;
        client.until_turn_completed().await;
        assert_eq!(provider.requests()[index].reasoning, level);
    }
}

#[tokio::test]
async fn compact_is_a_turn_of_its_own_carrying_the_instructions() {
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9_000, 100)]);
    let (host, _) = host_for(
        vec![Script::Events(text("one"))],
        Some(compactor.clone() as Arc<dyn Compactor>),
    )
    .await;
    let mut client = Client::open(&host).await;
    client.ack("hi").await;
    client.until_turn_completed().await;

    // The ack follows the command's return; a fast compaction may already
    // have completed by then, so the turn is waited for only if it has not.
    let (ack, mut frames) = client.ack("/compact keep the names").await;
    assert_eq!(message(&ack), "compacting the conversation");
    if !frames
        .iter()
        .any(|f| matches!(f.event, Event::TurnCompleted { .. }))
    {
        frames.extend(client.until_turn_completed().await);
    }
    assert!(frames.iter().any(|f| matches!(
        f.event,
        Event::TurnStarted {
            origin: TurnOrigin::Auto,
            ..
        }
    )));
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::Compacted { .. }))
    );
    assert_eq!(client.state.transcript.generation(), 1);
    let calls = compactor.calls.lock().unwrap();
    assert!(matches!(
        &calls[0].0,
        CompactReason::Manual { instructions: Some(i) } if i == "keep the names"
    ));
}

#[tokio::test]
async fn the_catalogue_lists_the_builtins_and_the_models() {
    let (host, _) = host_for(vec![], None).await;
    let commands = host.catalog(CatalogKind::Commands).await.unwrap();
    let names: Vec<&str> = commands.entries.iter().map(|e| e.id.as_str()).collect();
    for name in ["model", "think", "rename", "compact"] {
        assert!(names.contains(&name), "{names:?}");
    }
    let models = host.catalog(CatalogKind::Models).await.unwrap();
    assert_eq!(models.entries[0].id, "scripted/m");
}

/// A source's tools and commands are in the catalogue beside the registered
/// ones, a tool's meta riding along (ADR-0009 §1).
#[tokio::test]
async fn the_catalogue_reads_the_sources_too() {
    static SOURCES: PluginManifest = PluginManifest {
        id: "test.sources",
        version: "0",
        uses: &[],
        provides: &["tools:scripted", "commands:scripted"],
        requires: &[],
        config: None,
    };
    let tools = ScriptedToolSource::new();
    tools.set(vec![Arc::new(EchoTool { read_only: true })]);
    let late = ScriptedCommand::new("late", true, Ok(CommandOutcome::Applied { message: None }));
    let plugins = vec![
        TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
        ),
        TestPlugin::boxed(
            &SOURCES,
            vec![
                Contribution::Tools(tools),
                Contribution::Commands(ScriptedCommandSource::new(vec![late])),
            ],
        ),
    ];
    let host = Host::build(plugins, HostConfig::new(env())).await.unwrap();
    let tools = host.catalog(CatalogKind::Tools).await.unwrap();
    let echo = tools
        .entries
        .iter()
        .find(|e| e.id == "Echo")
        .expect("the source's tool");
    assert_eq!(echo.meta["description"], json!("echo"));
    let commands = host.catalog(CatalogKind::Commands).await.unwrap();
    assert!(
        commands.entries.iter().any(|e| e.id == "late"),
        "{commands:?}"
    );
    assert!(
        commands.entries.iter().any(|e| e.id == "model"),
        "the built-ins are there too"
    );
}

/// `/status` is the sheet of facts the surface keeps off its screen: it
/// answers at once, as a key-value view, whether or not a turn has run.
#[tokio::test]
async fn status_answers_with_the_session_s_facts() {
    let (host, _) = host_for(vec![Script::Events(text("hi"))], None).await;
    let mut client = Client::open(&host).await;
    let (ack, _) = client.ack("/status").await;
    let IntentOutcome::Applied { result } = ack else {
        panic!("a view, got {ack:?}");
    };
    let view: View = serde_json::from_value(result["view"].clone()).unwrap();
    let View::KeyValue { rows } = view else {
        panic!("key-value rows, got {view:?}");
    };
    let row = |key: &str| {
        rows.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("no {key} row in {rows:?}"))
    };
    assert_eq!(row("session"), client.state.summary.id.to_string());
    assert_eq!(row("mode"), "default");
    assert_eq!(row("context"), "not measured yet");
    assert_eq!(row("provider"), "scripted");
    assert_eq!(row("tokens"), "0 in · 0 out");
}

/// `/model --keep` explicitly writes the user layer as before, in addition
/// to remembering the next-start fallback. The loader reads user settings
/// first — so a project layer or a
/// `--model` on the command line still wins over it.
#[tokio::test]
async fn model_is_remembered_in_the_user_settings() {
    let home = tempfile::tempdir().expect("a home");
    let host = host_in(home.path(), None).await;
    let mut client = Client::open(&host).await;

    assert_eq!(client.state.summary.model_scope, Some(ChoiceScope::Default));
    client.ack("/model m2").await;
    assert_eq!(
        host.session_state(&client.state.summary.id)
            .await
            .unwrap()
            .summary
            .model_scope,
        Some(ChoiceScope::Session)
    );
    let (ack, _) = client.ack("/model m2 --keep").await;
    assert_eq!(
        host.session_state(&client.state.summary.id)
            .await
            .unwrap()
            .summary
            .model_scope,
        Some(ChoiceScope::Default)
    );
    assert_eq!(
        message(&ack),
        "⇄ m2 · scripted · thinking off · kept for new sessions"
    );

    let document = user_settings(home.path());
    assert_eq!(document["model"], json!("m2"));
    assert_eq!(document["provider"], json!("scripted"));

    let next = host_in(home.path(), Some("m2")).await;
    let opened = Client::open(&next).await;
    assert_eq!(opened.state.summary.model.as_deref(), Some("m2"));
    assert_eq!(opened.state.summary.provider.as_deref(), Some("scripted"));
}

/// `/think` outlives the session as `/model` does: the level is written into
/// the user layer and the next start opens on it (user-reported: "thinking
/// 没有记住"). `off` is written too — it is a choice, not an absence — and in
/// the lowest layer it is spelled by the key not being there (ADR-0058 §4).
#[tokio::test]
async fn thinking_is_remembered_in_the_user_settings() {
    let home = tempfile::tempdir().expect("a home");
    // A model that declares reasoning, so the view can show the level asked.
    let path = settings::user_path(&env_in(home.path()));
    std::fs::create_dir_all(path.parent().expect("a dir")).expect("the dir");
    std::fs::write(&path, "[models.\"scripted/m\"]\nreasoning = true\n").expect("the models");
    let host = host_in(home.path(), None).await;
    let mut client = Client::open(&host).await;

    let (ack, _) = client.ack("/think xhigh --keep").await;
    assert_eq!(message(&ack), "thinking: xhigh · kept for new sessions");
    assert_eq!(user_settings(home.path())["thinking"], json!("xHigh"));

    let next = host_in(home.path(), None).await;
    let opened = Client::open(&next).await;
    assert_eq!(opened.state.config.kernel["thinking"], json!("xHigh"));

    let (ack, _) = client.ack("/think off --keep").await;
    assert_eq!(message(&ack), "thinking: off · kept for new sessions");
    let document = user_settings(home.path());
    assert_eq!(document["thinking"], Value::Null, "the layer says nothing");
    assert_eq!(
        document["models"]["scripted/m"]["reasoning"],
        json!(true),
        "and everything beside it is where it was"
    );
}

/// The user layer as the next start will read it. The layers are TOML
/// (ADR-0058 §1), and a test that asks what a command wrote asks the file.
fn user_settings(home: &std::path::Path) -> Value {
    let path = settings::user_path(&env_in(home));
    let written = std::fs::read_to_string(&path).expect("the user settings were written");
    toml_edit::de::from_str(&written).expect("a settings layer")
}

/// A host reading the settings under `home`. `remembered` is what the user
/// layer is expected to hold by then: the settings are merged once at build,
/// so the second host must be built after the first one has written.
async fn host_in(home: &std::path::Path, remembered: Option<&str>) -> Arc<Host> {
    let env = env_in(home);
    let layers = crate::settings::load(&env, home, None).expect("readable settings");
    assert_eq!(
        layers
            .first()
            .and_then(|l| l.value.get("model"))
            .and_then(Value::as_str),
        remembered,
        "the user layer holds what the last run left there"
    );
    let plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
    )];
    let mut config = HostConfig::new(env);
    config.layers = layers;
    let config = match remembered {
        Some(_) => config,
        None => config.with_layer("cli", json!({ "model": "m" })),
    };
    Host::build(plugins, config).await.expect("a host")
}

/// The rows of a `View::Table` outcome.
pub(super) fn rows(outcome: &IntentOutcome) -> Vec<Vec<String>> {
    let IntentOutcome::Applied { result } = outcome else {
        panic!("not a view: {outcome:?}");
    };
    let view: View = serde_json::from_value(result["view"].clone()).unwrap();
    let View::Table { rows, .. } = view else {
        panic!("a table, got {view:?}");
    };
    rows
}

/// `/plugins` and `/modules` are one table of what will run and what will
/// not, each disabled plugin carrying the reason the load gave it
/// (ADR-0057 §6). It is instant, so it answers whether or not a turn runs.
#[tokio::test]
async fn plugins_and_modules_draw_one_table_with_every_reason_in_it() {
    let (host, _) = host_with(vec![]).await;
    let mut client = Client::open(&host).await;

    let (ack, _) = client.ack("/plugins").await;
    let drawn = rows(&ack);
    assert_eq!(
        drawn.iter().map(|row| row[0].as_str()).collect::<Vec<_>>(),
        ["test.provider", "test.tools", "test.needy"]
    );
    assert_eq!(drawn[0][2], "on");
    assert_eq!(drawn[2][2], "off");
    assert_eq!(drawn[2][3], "unmet requirements: service:missing");
    assert_eq!(drawn[2][4], "built in", "this build shipped it");

    let (alias, _) = client.ack("/modules").await;
    assert_eq!(rows(&alias), drawn, "one table under either name");
}

/// `disable` writes the user layer and says when it takes effect; the next
/// host built on that home opens with the plugin off (ADR-0057 §6).
#[tokio::test]
async fn disable_writes_the_user_layer_and_the_next_start_opens_on_it() {
    let home = tempfile::tempdir().expect("a home");
    let host = host_in(home.path(), None).await;
    let mut client = Client::open(&host).await;

    let (ack, _) = client.ack("/plugins disable test.provider").await;
    assert_eq!(
        message(&ack),
        "test.provider is off at next start (providers)."
    );
    let written = std::fs::read_to_string(settings::user_path(&env_in(home.path())))
        .expect("the user settings were written");
    let document: Value = toml_edit::de::from_str(&written).expect("a settings layer");
    assert_eq!(
        document["enabledPlugins"],
        json!({ "test.provider": false })
    );
    assert!(
        host.registry().enabled("test.provider"),
        "this session is untouched: a switch is taken at the next start"
    );

    let (refused, _) = client.ack("/plugins disable test.nobody").await;
    assert!(
        matches!(&refused, IntentOutcome::Rejected { error }
            if error.code == ErrorCode::InvalidInput && error.message.contains("test.nobody")),
        "{refused:?}"
    );

    let next = host_in(home.path(), None).await;
    assert!(!next.registry().enabled("test.provider"));
}

/// `/rename` is a session saying what it is called: one `SessionUpdated` and
/// nothing else. Bare, it answers the question instead, and a name too long
/// for a row is refused before anything is published.
#[tokio::test]
async fn rename_names_the_session_and_announces_it() {
    let (host, _) = host_for(vec![], None).await;
    let mut client = Client::open(&host).await;
    assert_eq!(client.state.summary.title, None);

    let (ack, before) = client.ack("/rename the release").await;
    assert_eq!(message(&ack), "name: the release");
    assert_eq!(client.state.summary.title.as_deref(), Some("the release"));
    assert!(
        before.iter().any(|f| matches!(
            &f.event,
            Event::SessionUpdated { summary } if summary.title.as_deref() == Some("the release")
        )),
        "the new name is announced before the ack"
    );

    let (ack, _) = client.ack("/rename").await;
    assert_eq!(shown(&ack), "name: the release\nusage: /rename <title>");

    let (ack, _) = client.ack(&format!("/rename {}", "x".repeat(81))).await;
    assert!(
        matches!(ack, IntentOutcome::Rejected { error } if error.code == ErrorCode::InvalidInput)
    );
    assert_eq!(
        client.state.summary.title.as_deref(),
        Some("the release"),
        "a refused name changes nothing"
    );
}

/// A blank title clears the name on a live session, so its row goes back
/// to the first ask instead of showing an empty name.
#[tokio::test]
async fn a_blank_title_clears_a_live_name() {
    let (host, _) = host_for(vec![], None).await;
    let mut client = Client::open(&host).await;
    client.ack("/rename the release").await;
    let id = client.state.summary.id.clone();
    host.reconfigure(&id, SessionChange::Title("  ".into()))
        .await
        .unwrap();
    let (ack, _) = client.ack("/rename").await;
    assert!(shown(&ack).starts_with("name: none yet"), "{}", shown(&ack));
    assert_eq!(client.state.summary.title, None);
}

struct DiscoveredApproval(bingo_sdk::Approval);
#[async_trait]
impl bingo_sdk::PluginSource for DiscoveredApproval {
    fn id(&self) -> &str {
        "discovered"
    }
    async fn plugins(&self) -> Vec<PluginStatus> {
        vec![]
    }
    async fn approval(&self, name: &str) -> Option<bingo_sdk::Approval> {
        (self.0.what == format!("plugin:{name}")).then(|| self.0.clone())
    }
}

#[tokio::test]
async fn plugin_approval_uses_discovery_root_instead_of_session_cwd() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let dir = repo.path().join(".bingo/plugins/p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.json"), "{}").unwrap();
    let request = bingo_sdk::Approval {
        root: repo.path().into(),
        what: "plugin:p".into(),
    };
    let source = Arc::new(DiscoveredApproval(request));
    let host = Host::build(
        vec![TestPlugin::boxed(
            &PROVIDER,
            vec![
                Contribution::Provider(ScriptedProvider::new(vec![])),
                Contribution::Plugins(source),
            ],
        )],
        HostConfig::new(env_in(home.path())).with_layer("cli", json!({"model":"m"})),
    )
    .await
    .unwrap();
    let mut client = Client::open(&host).await;
    let (ack, _) = client.ack("/plugins approve p").await;
    assert!(message(&ack).starts_with("approved p"), "{ack:?}");
    let saved = settings::read_document(&env_in(home.path()).user_settings()).unwrap();
    let root = crate::approval::root_key(repo.path()).unwrap();
    assert!(saved["approved"][root]["plugin:p"].is_string());
}

#[tokio::test]
async fn a_tool_switch_explains_pending_and_keeps_the_running_tools() {
    let home = tempfile::tempdir().unwrap();
    let host = Host::build(
        vec![
            TestPlugin::boxed(
                &PROVIDER,
                vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
            ),
            TestPlugin::boxed(
                &TOOLS,
                vec![Contribution::Tool(Arc::new(EchoTool { read_only: true }))],
            ),
        ],
        HostConfig::new(Env::rooted(home.path())).with_layer("test", json!({"model":"m"})),
    )
    .await
    .unwrap();
    let mut client = Client::open(&host).await;
    let before = host.catalog(CatalogKind::Tools).await.unwrap();
    let (ack, _) = client.ack("/plugins disable test.tools").await;
    assert_eq!(message(&ack), "test.tools is off at next start (tools).");
    assert_eq!(host.catalog(CatalogKind::Tools).await.unwrap(), before);
    let (ack, _) = client.ack("/plugins").await;
    let row = rows(&ack)
        .into_iter()
        .find(|row| row[0] == "test.tools")
        .unwrap();
    assert_eq!(
        (&row[2], &row[3]),
        (&"pending".to_string(), &"off at next start".to_string())
    );
    client.ack("/plugins enable test.tools").await;
    let (ack, _) = client.ack("/plugins").await;
    assert_eq!(
        rows(&ack)
            .into_iter()
            .find(|row| row[0] == "test.tools")
            .unwrap()[2],
        "on"
    );
    host.shutdown().await;
}

#[tokio::test]
async fn project_switch_override_keeps_the_effective_wish_and_names_its_file() {
    static WORDCOUNT: PluginManifest = PluginManifest {
        id: "wordcount",
        version: "0",
        uses: &[],
        provides: &[],
        requires: &[],
        config: None,
    };
    for on in [true, false] {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join(".bingo").join("settings.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("[enabledPlugins]\nwordcount = {on}\n")).unwrap();
        let env = env_in(home.path());
        let mut config = HostConfig::new(env.clone());
        config.layers = settings::load(&env, project.path(), None).unwrap();
        let config = config.with_layer("cli", json!({"model":"m"}));
        let host = Host::build(
            vec![
                TestPlugin::boxed(&WORDCOUNT, vec![]),
                TestPlugin::boxed(
                    &PROVIDER,
                    vec![Contribution::Provider(ScriptedProvider::new(vec![]))],
                ),
            ],
            config,
        )
        .await
        .unwrap();
        let mut client = Client::open(&host).await;
        let verb = if on { "disable" } else { "enable" };
        let (ack, _) = client.ack(&format!("/plugins {verb} wordcount")).await;
        let text = message(&ack);
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(
            text.contains(&format!("keeps it {}", crate::plugins::state(on))),
            "{text}"
        );
        let listed = crate::plugins::listing(host.registry()).await;
        assert_eq!(
            listed
                .iter()
                .find(|row| row.id == "wordcount")
                .unwrap()
                .state
                .is_on(),
            on
        );
        assert_eq!(
            user_settings(home.path())["enabledPlugins"]["wordcount"],
            !on
        );
        assert_eq!(
            settings::merge(&settings::load(&env, project.path(), None).unwrap(), &[])
                .unwrap()
                .kernel
                .enabled_plugins["wordcount"],
            on
        );
        host.shutdown().await;
    }
}

#[tokio::test]
async fn commented_layers_allow_switches_and_invalid_layers_leave_no_write() {
    for (explicit, invalid) in [(false, false), (true, false), (false, true), (true, true)] {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join(if explicit {
            "settings.json"
        } else {
            ".bingo/settings.json"
        });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ // project note\n \"enabledPlugins\": {} }").unwrap();
        let env = env_in(home.path());
        crate::plugins::switch(&env, "test.provider", true).unwrap();
        let user = settings::user_path(&env);
        let before = std::fs::read(&user).unwrap();
        let mut config = HostConfig::new(env.clone());
        config.layers =
            settings::load(&env, project.path(), explicit.then_some(path.as_path())).unwrap();
        let host = Host::build(vec![TestPlugin::boxed(&PROVIDER, vec![])], config)
            .await
            .unwrap();
        if invalid {
            std::fs::write(&path, "{broken").unwrap();
        }
        let result =
            crate::plugins::switch_in(&host, &SessionId::mint(), "test.provider", false).await;
        if invalid {
            assert!(result.is_err());
            assert_eq!(
                std::fs::read(&user).unwrap(),
                before,
                "refusal changed user settings"
            );
            assert!(
                !host
                    .registry
                    .switches
                    .wishes
                    .read()
                    .unwrap()
                    .contains("test.provider")
            );
        } else {
            assert!(result.is_ok(), "commented settings refused");
            assert!(
                host.registry
                    .switches
                    .wishes
                    .read()
                    .unwrap()
                    .contains("test.provider")
            );
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn switching_another_plugin_keeps_required_surfaces_on() {
    static SURFACE: PluginManifest = PluginManifest {
        id: "test.surface",
        version: "0",
        uses: &[],
        provides: &["surface:test"],
        requires: &[],
        config: None,
    };
    let home = tempfile::tempdir().unwrap();
    let host = Host::build(
        vec![
            TestPlugin::boxed(&SURFACE, vec![]),
            TestPlugin::boxed(&PROVIDER, vec![]),
        ],
        HostConfig::new(env_in(home.path()))
            .with_layer("cli", json!({"enabledPlugins":{"test.surface":false}})),
    )
    .await
    .unwrap();
    crate::plugins::switch_in(&host, &SessionId::mint(), "test.provider", false)
        .await
        .unwrap();
    let listed = crate::plugins::listing(host.registry()).await;
    assert_eq!(
        listed
            .iter()
            .find(|row| row.id == "test.surface")
            .unwrap()
            .state,
        PluginState::On
    );
    host.shutdown().await;
}

#[derive(Default)]
struct SlowSwitch {
    block: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    applied: tokio::sync::Notify,
}
#[async_trait]
impl PluginSource for SlowSwitch {
    fn id(&self) -> &str {
        "slow"
    }
    async fn plugins(&self) -> Vec<PluginStatus> {
        if self.block.load(std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        vec![PluginStatus {
            id: "outside".into(),
            version: "0".into(),
            state: PluginState::On,
            from: "slow".into(),
            placement: None,
            grant: None,
        }]
    }
    async fn switch_kind(&self, _: &str) -> bingo_sdk::plugin::PluginSwitch {
        bingo_sdk::plugin::PluginSwitch::Now
    }
    async fn switch(
        &self,
        _: &str,
        _: bool,
    ) -> Result<bingo_sdk::plugin::PluginSwitch, KernelError> {
        self.applied.notify_one();
        Ok(bingo_sdk::plugin::PluginSwitch::Now)
    }
}
#[tokio::test]
async fn cancelling_source_lookup_does_not_lose_a_persisted_switch() {
    let home = tempfile::tempdir().unwrap();
    let source = Arc::new(SlowSwitch::default());
    let host = Host::build(
        vec![TestPlugin::boxed(
            &PROVIDER,
            vec![Contribution::Plugins(source.clone())],
        )],
        HostConfig::new(env_in(home.path())),
    )
    .await
    .unwrap();
    source
        .block
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let caller = {
        let host = host.clone();
        tokio::spawn(async move {
            crate::plugins::switch_in(&host, &SessionId::mint(), "outside", false)
                .await
                .map(|_| ())
        })
    };
    source.entered.notified().await;
    assert_eq!(
        user_settings(home.path())["enabledPlugins"]["outside"],
        false
    );
    caller.abort();
    let _ = caller.await;
    source
        .block
        .store(false, std::sync::atomic::Ordering::SeqCst);
    source.release.notify_one();
    tokio::time::pause();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), source.applied.notified())
            .await
            .is_ok(),
        "persisted apply was cancelled"
    );
    host.shutdown().await;
}
