//! The kernel's own commands through a real host (ADR-0008 §4).

use super::*;
use crate::settings;

static CONTEXT: PluginManifest = PluginManifest {
    id: "test.context",
    version: "0",
    sdk: "^0.1",
    provides: &["compactor"],
    requires: &[],
    config: None,
};

/// A host whose two scripted models are declared to reason, so `/think`
/// has something to switch.
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
                "scripted/m2": { "reasoning": true },
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
            .submit(intent.clone(), Input::text(line, Origin::surface("test")));
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

    async fn until_turn_completed(&mut self) -> Vec<Frame> {
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

fn message(outcome: &IntentOutcome) -> String {
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
    assert_eq!(client.state.config.kernel, json!({ "thinking": null }));

    let (ack, before) = client.ack("/model m2").await;
    assert_eq!(message(&ack), "model: scripted/m2");
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
    assert_eq!(message(&ack), "thinking: high");
    assert_eq!(client.state.config.kernel, json!({ "thinking": "high" }));

    let (ack, _) = client.ack("/model").await;
    assert_eq!(
        shown(&ack),
        "model: scripted/m2\nthinking: high\nusage: /model [<provider>/]<model>",
        "bare /model says who serves it, which model, and how hard it thinks"
    );

    client.ack("hello").await;
    client.until_turn_completed().await;
    let request = &provider.requests()[0];
    assert_eq!(request.model, "m2");
    assert_eq!(request.reasoning, Some(Effort::High));

    let (ack, _) = client.ack("/think off").await;
    assert_eq!(message(&ack), "thinking: off");
    assert_eq!(client.state.config.kernel, json!({ "thinking": null }));
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
    assert_eq!(
        shown(&ack),
        "model: scripted/m2\nusage: /model [<provider>/]<model>",
        "a turn that asks for no effort reports none"
    );
}

/// The text of a `View::Text` outcome.
fn shown(outcome: &IntentOutcome) -> String {
    match outcome {
        IntentOutcome::Applied { result } => result["view"]["text"].as_str().unwrap().to_string(),
        other => panic!("not a view: {other:?}"),
    }
}

/// `/think` on a model that does not declare reasoning: the level is stored
/// and no turn asks for it, so the reply says both and names the settings key
/// that would say otherwise. The level is kept, so `/model` is all it takes.
#[tokio::test]
async fn think_owns_up_when_the_model_will_not_reason_and_keeps_the_level() {
    let (host, provider) = host_for(vec![Script::Events(text("hi"))], None).await;
    let mut client = Client::open(&host).await;
    let (ack, _) = client.ack("/model plain").await;
    assert_eq!(message(&ack), "model: scripted/plain");

    let caveat = "thinking: high — but scripted/plain does not declare reasoning, so no turn \
                  asks for it; models.\"scripted/plain\".reasoning = true in settings says \
                  otherwise";
    let (ack, _) = client.ack("/think high").await;
    assert_eq!(message(&ack), caveat);
    assert_eq!(
        client.state.config.kernel,
        json!({ "thinking": null }),
        "the config view already said what the turn would ask for; the ack did not"
    );
    let (ack, _) = client.ack("/think").await;
    assert!(shown(&ack).starts_with(caveat), "bare /think says the same");

    // A model that reasons: the same level, no caveat, and the turn asks.
    client.ack("/model m2").await;
    let (ack, _) = client.ack("/think").await;
    assert!(
        shown(&ack).starts_with("thinking: high\nusage:"),
        "the level survived the switch: {}",
        shown(&ack)
    );
    client.ack("hello").await;
    client.until_turn_completed().await;
    assert_eq!(provider.requests()[0].reasoning, Some(Effort::High));
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
    assert_eq!(client.state.history_generation, 1);
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
    for name in ["model", "think", "compact"] {
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
        sdk: "^0.1",
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

/// `/model` outlives the session it was typed in: the next start opens on it
/// (user-reported: "应该是记住上次设置的"). It is written into the user layer,
/// which is the file the loader reads first — so a project layer or a
/// `--model` on the command line still wins over it.
#[tokio::test]
async fn model_is_remembered_in_the_user_settings() {
    let home = tempfile::tempdir().expect("a home");
    let host = host_in(home.path(), None).await;
    let mut client = Client::open(&host).await;

    let (ack, _) = client.ack("/model m2").await;
    assert_eq!(message(&ack), "model: scripted/m2");

    let written = std::fs::read_to_string(settings::user_path(&env_in(home.path())))
        .expect("the user settings were written");
    let document: Value = serde_json::from_str(&written).expect("plain JSON");
    assert_eq!(document["model"], json!("m2"));
    assert_eq!(document["provider"], json!("scripted"));

    let next = host_in(home.path(), Some("m2")).await;
    let opened = Client::open(&next).await;
    assert_eq!(opened.state.summary.model.as_deref(), Some("m2"));
    assert_eq!(opened.state.summary.provider.as_deref(), Some("scripted"));
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
