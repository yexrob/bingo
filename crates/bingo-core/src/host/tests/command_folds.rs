use super::*;

static PANEL: PluginManifest = PluginManifest {
    id: "test.panel",
    version: "0",
    uses: &[],
    provides: &[],
    requires: &[],
    config: None,
};
static OTHER: PluginManifest = PluginManifest {
    id: "test.other",
    ..PANEL
};

fn command(name: &'static str) -> Contribution {
    Contribution::Command(ScriptedCommand::new(
        name,
        true,
        Ok(CommandOutcome::Applied {
            message: Some(name.into()),
        }),
    ))
}

fn fold(into: &str, names: &[&str]) -> Contribution {
    Contribution::CommandFold(CommandFold {
        into: into.into(),
        names: names.iter().map(|name| (*name).into()).collect(),
    })
}

fn plugins(panel: bool) -> Vec<Box<dyn Plugin>> {
    let mut plugins = vec![TestPlugin::boxed(
        &PROVIDER,
        ["first", "second", "third", "fourth"]
            .into_iter()
            .map(command)
            .collect(),
    )];
    if panel {
        plugins.push(TestPlugin::boxed(
            &PANEL,
            vec![
                command("panel"),
                fold("panel", &["first", "second", "third", "fourth"]),
            ],
        ));
    }
    plugins
}

async fn marked(host: &Host, expected: Option<&str>) {
    let catalog = host.catalog(CatalogKind::Commands).await.unwrap();
    for name in ["first", "second", "third", "fourth"] {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == name)
            .unwrap();
        assert_eq!(entry.meta["foldedInto"].as_str(), expected, "{name}");
    }
}

async fn log_client(host: &Host) -> commands::Client {
    let mut spec = spec("/work");
    spec.driver = Driver::Log;
    let attachment = host
        .open(
            SessionSelector::Create { spec },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    commands::Client {
        state: attachment.snapshot,
        events: attachment.events,
        handle: attachment.handle,
    }
}

#[tokio::test]
async fn the_owning_mod_alone_marks_folded_commands() {
    for (present, enabled) in [(false, true), (true, false), (true, true)] {
        let home = tempfile::tempdir().unwrap();
        let host = Host::build(
            plugins(present),
            HostConfig::new(env_in(home.path()))
                .with_layer("cli", json!({"enabledPlugins":{"test.panel":enabled}})),
        )
        .await
        .unwrap();
        marked(&host, (present && enabled).then_some("panel")).await;
        host.shutdown().await;
    }
}

#[tokio::test]
async fn switching_the_owner_reprojects_the_next_catalogue() {
    for initial in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let env = env_in(home.path());
        crate::plugins::switch(&env, PANEL.id, initial).unwrap();
        let mut config = HostConfig::new(env.clone());
        config.layers = settings::load(&env, home.path(), None).unwrap();
        let host = Host::build(plugins(true), config).await.unwrap();
        marked(&host, initial.then_some("panel")).await;
        for enabled in [!initial, initial] {
            let switched = crate::plugins::switch_in(&host, &SessionId::mint(), PANEL.id, enabled)
                .await
                .unwrap();
            assert!(
                matches!(switched, crate::plugins::Switched::Applied(now, _) if now == enabled)
            );
            marked(&host, enabled.then_some("panel")).await;
        }
        host.shutdown().await;
    }
}

#[tokio::test]
async fn a_fold_changes_neither_dispatch_nor_the_command_spec() {
    let home = tempfile::tempdir().unwrap();
    let first = ScriptedCommand::new(
        "first",
        true,
        Ok(CommandOutcome::Applied {
            message: Some("first".into()),
        }),
    );
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Command(first.clone())]),
        TestPlugin::boxed(&PANEL, vec![command("panel"), fold("panel", &["first"])]),
    ];
    let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
        .await
        .unwrap();
    let mut client = log_client(&host).await;
    let (ack, _) = client.ack("/first keep arguments").await;
    assert_eq!(commands::message(&ack), "first");
    assert_eq!(first.calls(), ["keep arguments"]);
    let command = host.registry.command_sources()[0]
        .commands(home.path())
        .await
        .into_iter()
        .find(|command| command.spec().name == "first")
        .unwrap();
    assert!(
        serde_json::to_value(command.spec())
            .unwrap()
            .get("foldedInto")
            .is_none()
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_conflicting_fold_is_refused_whole_with_a_notice() {
    let home = tempfile::tempdir().unwrap();
    let mut plugins = plugins(true);
    plugins.push(TestPlugin::boxed(
        &OTHER,
        vec![
            command("other"),
            command("fifth"),
            fold("other", &["first", "fifth"]),
        ],
    ));
    let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
        .await
        .unwrap();
    marked(&host, Some("panel")).await;
    let catalog = host.catalog(CatalogKind::Commands).await.unwrap();
    let fifth = catalog
        .entries
        .iter()
        .find(|entry| entry.id == "fifth")
        .unwrap();
    assert!(fifth.meta.get("foldedInto").is_none());
    assert!(host.notices().iter().any(|(code, text)| {
        code == "COMMAND_FOLD_CONFLICT"
            && text.contains(OTHER.id)
            && text.contains("first")
            && text.contains(PANEL.id)
    }));
    let client = log_client(&host).await;
    assert!(client.state.transcript.items().iter().any(|item| {
        matches!(&item.body, ItemBody::Notice { level: Level::Warn, code, .. }
            if code == "COMMAND_FOLD_CONFLICT")
    }));
    host.shutdown().await;
}

#[tokio::test]
async fn an_absent_destination_never_hides_a_command() {
    let home = tempfile::tempdir().unwrap();
    let mut plugins = plugins(false);
    plugins.push(TestPlugin::boxed(&PANEL, vec![fold("missing", &["first"])]));
    let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
        .await
        .unwrap();
    marked(&host, None).await;
    host.shutdown().await;
}

struct Aliased;
#[async_trait]
impl Command for Aliased {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "first".into(),
            aliases: vec!["alternate".into()],
            hint: "A command with another spelling".into(),
            family: "test".into(),
            args: ArgSpec::None,
            keys: vec![],
            hidden: false,
            opens: false,
            instant: true,
        }
    }
    async fn run(&self, _: &str, _: &CommandContext) -> Result<CommandOutcome, KernelError> {
        unreachable!("only the command catalogue is read")
    }
}

#[tokio::test]
async fn aliases_registered_after_folds_cannot_claim_a_second_destination() {
    let home = tempfile::tempdir().unwrap();
    let plugins = vec![
        TestPlugin::boxed(&PANEL, vec![command("panel"), fold("panel", &["first"])]),
        TestPlugin::boxed(
            &OTHER,
            vec![command("other"), fold("other", &["alternate"])],
        ),
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Command(Arc::new(Aliased))]),
    ];
    let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
        .await
        .unwrap();
    let catalogue = host.catalog(CatalogKind::Commands).await.unwrap();
    let first = catalogue
        .entries
        .iter()
        .find(|entry| entry.id == "first")
        .unwrap();
    assert_eq!(first.meta["foldedInto"], "panel");
    assert!(
        host.notices()
            .iter()
            .any(|(code, text)| { code == "COMMAND_FOLD_CONFLICT" && text.contains(OTHER.id) })
    );
    host.shutdown().await;
}

#[tokio::test]
async fn a_destination_cannot_fold_itself_or_an_alias() {
    for name in ["first", "alternate"] {
        let home = tempfile::tempdir().unwrap();
        let plugins = vec![TestPlugin::boxed(
            &PANEL,
            vec![
                fold("first", &[name]),
                Contribution::Command(Arc::new(Aliased)),
            ],
        )];
        let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
            .await
            .unwrap();
        let catalogue = host.catalog(CatalogKind::Commands).await.unwrap();
        let first = catalogue
            .entries
            .iter()
            .find(|entry| entry.id == "first")
            .unwrap();
        assert!(first.meta.get("foldedInto").is_none());
        assert!(
            host.notices()
                .iter()
                .any(|(code, _)| code == "COMMAND_FOLD_INVALID")
        );
        host.shutdown().await;
    }
}

#[tokio::test]
async fn a_cycle_refuses_the_second_fold_and_keeps_a_destination_visible() {
    let home = tempfile::tempdir().unwrap();
    let plugins = vec![
        TestPlugin::boxed(&PANEL, vec![command("panel"), fold("panel", &["first"])]),
        TestPlugin::boxed(&OTHER, vec![command("first"), fold("first", &["panel"])]),
    ];
    let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
        .await
        .unwrap();
    let catalogue = host.catalog(CatalogKind::Commands).await.unwrap();
    let first = catalogue
        .entries
        .iter()
        .find(|entry| entry.id == "first")
        .unwrap();
    let panel = catalogue
        .entries
        .iter()
        .find(|entry| entry.id == "panel")
        .unwrap();
    assert_eq!(first.meta["foldedInto"], "panel");
    assert!(panel.meta.get("foldedInto").is_none());
    assert!(
        host.notices()
            .iter()
            .any(|(code, _)| code == "COMMAND_FOLD_INVALID")
    );
    host.shutdown().await;
}

struct DescribeAliases(Vec<(&'static str, &'static str)>);
#[async_trait]
impl Interceptor for DescribeAliases {
    fn id(&self) -> &str {
        "test.aliases"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher::at(bingo_sdk::catalogue::COMMAND_DESCRIBE)
    }
    async fn chain(
        &self,
        _: &bingo_sdk::catalogue::Entry,
        _: &HookContext,
        input: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let mut answer = next.run(input).await?;
        if let Some((_, alias)) = self.0.iter().find(|(name, _)| answer["name"] == *name) {
            answer["aliases"] = json!([alias]);
        }
        Ok(answer)
    }
}

#[tokio::test]
async fn described_aliases_cannot_hide_destinations_or_create_fold_chains() {
    for aliases in [
        vec![("panel", "first")],
        vec![("panel", "second")],
        vec![("panel", "second"), ("other", "first")],
    ] {
        let home = tempfile::tempdir().unwrap();
        let plugins = vec![
            TestPlugin::boxed(
                &PANEL,
                vec![
                    command("panel"),
                    command("first"),
                    fold("panel", &["first"]),
                ],
            ),
            TestPlugin::boxed(
                &OTHER,
                vec![
                    command("other"),
                    command("second"),
                    fold("other", &["second"]),
                ],
            ),
            TestPlugin::boxed(
                &PROVIDER,
                vec![Contribution::Interceptor {
                    placement: bingo_sdk::plugin::Placement::Outer,
                    external: None,
                    link: Arc::new(DescribeAliases(aliases.clone())),
                }],
            ),
        ];
        let host = Host::build(plugins, HostConfig::new(env_in(home.path())))
            .await
            .unwrap();
        let catalogue = host.catalog(CatalogKind::Commands).await.unwrap();
        for (name, expected) in [
            ("first", Some("panel")),
            ("second", Some("other")),
            ("panel", None),
            ("other", None),
        ] {
            let entry = catalogue
                .entries
                .iter()
                .find(|entry| entry.id == name)
                .unwrap();
            assert_eq!(
                entry.meta["foldedInto"].as_str(),
                expected,
                "{name}: {aliases:?}"
            );
        }
        for (name, alias) in aliases {
            let entry = catalogue
                .entries
                .iter()
                .find(|entry| entry.id == name)
                .unwrap();
            assert_eq!(entry.meta["aliases"], json!([alias]), "the interceptor ran");
        }
        host.shutdown().await;
    }
}
