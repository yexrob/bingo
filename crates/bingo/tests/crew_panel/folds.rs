use super::*;

fn crew_marks(catalog: &Catalog, present: bool) {
    for name in ["agents", "tasks", "room", "team"] {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == name)
            .unwrap();
        assert_eq!(
            entry.meta["foldedInto"].as_str(),
            present.then_some("crew"),
            "{name}"
        );
        if name == "room" {
            assert_eq!(entry.meta["aliases"], json!(["rooms"]));
        }
    }
}

async fn selected(host: &impl HostApi, root: &mut Attachment, line: &str, tab: &str) {
    let opened = command(root, line).await;
    assert_eq!(
        opened["result"]["surface"]["name"], "Pane",
        "{line}: {opened}"
    );
    let view = draw(host, &root.session).await.fold();
    assert!(view.contains(&format!("❯ {tab} (")), "{line}: {view}");
}

async fn plain(root: &mut Attachment, line: &str) -> Value {
    let intent = IntentId::mint();
    root.handle.submit(
        intent.clone(),
        Input::text(line, Origin::surface("rpc").on(render::SurfaceName::Text)),
    );
    serde_json::to_value(ack_for(root, &intent).await).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn folded_commands_open_their_places_and_keep_plain_callers_and_verbs() {
    let mut server =
        Server::spawn(r#"{"responses":[{"steps":[{"delay":{"ms":60000}},{"text":"Reviewed"}]}]}"#);
    let host = ready(&mut server).await;
    let mut root = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    selected(&host, &mut root, "/agents", "agents").await;
    assert!(
        draw(&host, &root.session)
            .await
            .fold()
            .contains("No agents here yet.")
    );
    let empty = plain(&mut root, "/agents").await;
    assert_eq!(
        empty["result"]["message"],
        "no agents are running in this session"
    );
    assert!(empty["result"].get("surface").is_none());
    let mut reviewer = child(&host, &root, &server.cwd(), "reviewer").await;
    command(&mut root, "/room design reviewer").await;
    command(&mut root, "/tasks add Review the cut").await;
    std::fs::write(
        server.cwd().join(".bingo/team.json"),
        r#"{"roles":[{"name":"reviewer","agent":"reviewer"},{"name":"scout"}]}"#,
    )
    .unwrap();
    crew_marks(&host.catalog(CatalogKind::Commands).await.unwrap(), true);
    bare_names(&host, &mut root).await;
    room_enter(&host, &mut root).await;
    full_verbs(&host, &mut root, &mut reviewer).await;
    assert!(has_session(&host, &reviewer.session).await);
    disable_crew(&host, &mut root).await;
    super::actions::finish(&host, &mut server).await;
}

async fn bare_names(host: &impl HostApi, root: &mut Attachment) {
    for (line, tab) in [
        ("/agents", "agents"),
        ("/tasks", "tasks"),
        ("/room", "rooms"),
        ("/rooms", "rooms"),
        ("/team", "team"),
    ] {
        selected(host, root, line, tab).await;
        let receipt = plain(root, line).await;
        assert!(receipt["result"].get("view").is_some(), "{line}: {receipt}");
        assert!(
            receipt["result"].get("surface").is_none(),
            "{line}: {receipt}"
        );
    }
    let team = draw(host, &root.session).await.fold();
    assert!(
        team.contains("reviewer") && team.contains("scout   not seated"),
        "{team}"
    );
}

async fn full_verbs(host: &impl HostApi, root: &mut Attachment, reviewer: &mut Attachment) {
    let intent = IntentId::mint();
    reviewer.handle.submit(
        intent.clone(),
        Input::text("Review the cut", Origin::surface("tui")),
    );
    assert!(matches!(
        ack_for(reviewer, &intent).await,
        IntentOutcome::TurnStarted { .. }
    ));
    for line in [
        "/agents stop reviewer",
        "/room close design",
        "/tasks done 1",
    ] {
        let receipt = command(root, line).await;
        assert_eq!(receipt["kind"], "applied", "{line}: {receipt}");
        assert!(
            receipt["result"].get("surface").is_none(),
            "{line}: {receipt}"
        );
    }
    support::until_completed(reviewer).await;
    assert!(matches!(
        reviewer.snapshot.last_status(),
        Some(TurnStatus::Interrupted { .. })
    ));
    assert!(!reviewer.snapshot.busy());
    selected(host, root, "/crew rooms", "rooms").await;
    assert!(
        draw(host, &root.session)
            .await
            .fold()
            .contains("#design   closed")
    );
    selected(host, root, "/crew tasks", "tasks").await;
    assert!(draw(host, &root.session).await.fold().contains("☑ #1"));
}

async fn disable_crew(host: &impl HostApi, root: &mut Attachment) {
    assert!(crew_pane(root));
    let disabled = command(root, "/plugins disable bingo.mod.crew").await;
    assert_eq!(disabled["kind"], "applied", "{disabled}");
    assert_eq!(disabled["result"]["message"], "bingo.mod.crew is off now.");
    assert!(!root.snapshot.signals.contains_key("bingo.mod.crew"));
    assert!(!crew_pane(root));
    crew_marks(&host.catalog(CatalogKind::Commands).await.unwrap(), false);
    crew_marks(
        &host
            .catalog_for(&root.session, CatalogKind::Commands)
            .await
            .unwrap(),
        false,
    );
    let receipt = command(root, "/agents").await;
    assert!(receipt["result"].get("view").is_some());
    assert!(receipt["result"].get("surface").is_none());
    let enabled = command(root, "/plugins enable bingo.mod.crew").await;
    assert_eq!(enabled["kind"], "applied", "{enabled}");
    assert_eq!(enabled["result"]["message"], "bingo.mod.crew is on now.");
    crew_marks(&host.catalog(CatalogKind::Commands).await.unwrap(), true);
    crew_marks(
        &host
            .catalog_for(&root.session, CatalogKind::Commands)
            .await
            .unwrap(),
        true,
    );
    selected(host, root, "/agents", "agents").await;
    assert!(crew_pane(root));
}

fn crew_pane(root: &Attachment) -> bool {
    slot::placed(&root.snapshot).iter().any(|placed| {
        placed.plugin == "bingo.mod.crew"
            && matches!(&placed.slot, slot::Slot::Pane { id, .. } if id == "crew")
    })
}

async fn room_enter(host: &impl HostApi, root: &mut Attachment) {
    let room = host
        .sessions(SessionFilter {
            parent: Some(root.session.clone()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_iter()
        .find(|summary| summary.title.as_deref() == Some("#design"))
        .unwrap();
    for line in ["/crew rooms", "/room", "/rooms"] {
        selected(host, root, line, "rooms").await;
        let viewed = key(host, root, "enter").await;
        assert_eq!(
            viewed["result"]["surface"],
            json!({"name":"View","args":{"view":room.id}})
        );
    }
}
