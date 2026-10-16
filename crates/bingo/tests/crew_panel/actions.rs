use super::*;

async fn tab(host: &impl HostApi, root: &mut Attachment, steps: usize) {
    assert_eq!(command(root, "/crew").await["kind"], "applied");
    for _ in 0..steps {
        key(host, root, "]").await;
    }
}

async fn prefill(
    host: &impl HostApi,
    root: &mut Attachment,
    key_name: &str,
    expected: &str,
    text: &str,
) {
    let fill = key(host, root, key_name).await;
    assert_eq!(fill["result"]["surface"]["name"], "Composer");
    assert_eq!(fill["result"]["surface"]["args"]["text"], expected);
    assert!(fill["result"].get("view").is_none());
    let receipt = command(root, &format!("{expected}{text}")).await;
    assert_eq!(receipt["kind"], "applied", "{receipt}");
    assert!(receipt["result"].get("surface").is_none());
}

pub(super) async fn finish(host: &bingo_surface_rpc::RemoteKernel, server: &mut Server) {
    host.shutdown().await.unwrap();
    assert!(
        tokio::time::timeout(LIMIT, server.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_prefill_reaches_the_typed_command_path() {
    let mut server =
        Server::spawn(r#"{"responses":[{"steps":[{"delay":{"ms":60000}},{"text":"Done"}]}]}"#);
    let host = ready(&mut server).await;
    let mut root = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    let reviewer = child(&host, &root, &server.cwd(), "reviewer").await;
    tab(&host, &mut root, 0).await;
    let viewed = key(&host, &mut root, "enter").await;
    assert_eq!(
        viewed["result"]["surface"],
        json!({"name":"View","args":{"view":reviewer.session}})
    );
    tab(&host, &mut root, 0).await;
    prefill(
        &host,
        &mut root,
        "m",
        "/agents message reviewer ",
        "Review the cut",
    )
    .await;
    tab(&host, &mut root, 0).await;
    stop_and_follow(&host, &mut root).await;
    prefill(
        &host,
        &mut root,
        "m",
        "/agents reopen reviewer ",
        "Review again",
    )
    .await;
    command(&mut root, "/rooms design reviewer").await;
    room_edits(&host, &mut root).await;
    task_edits(&host, &mut root).await;
    finish(&host, &mut server).await;
}

async fn room_edits(host: &impl HostApi, root: &mut Attachment) {
    for (key_name, verb, text) in [
        ("p", "purpose", "Check authority"),
        ("u", "unseat", "reviewer"),
        ("a", "seat", "reviewer"),
    ] {
        tab(host, root, 1).await;
        prefill(
            host,
            root,
            key_name,
            &format!("/rooms {verb} #design "),
            text,
        )
        .await;
    }
    tab(host, root, 1).await;
    let view = draw(host, &root.session).await.fold();
    assert!(
        view.contains("Check authority") && view.contains("reviewer"),
        "{view}"
    );
    key(host, root, "c").await;
    assert!(
        draw(host, &root.session)
            .await
            .fold()
            .contains("Close #design?")
    );
    key(host, root, "esc").await;
    assert!(
        !draw(host, &root.session)
            .await
            .fold()
            .contains("Close #design?")
    );
    key(host, root, "c").await;
    let receipt = key(host, root, "enter").await;
    assert_eq!(receipt["result"]["message"], "#design: closed");
}

async fn task_edits(host: &impl HostApi, root: &mut Attachment) {
    tab(host, root, 2).await;
    prefill(host, root, "+", "/tasks add ", "Check the cut").await;
    tab(host, root, 2).await;
    prefill(host, root, "o", "/tasks assign 1 ", "reviewer").await;
    tab(host, root, 2).await;
    assert!(draw(host, &root.session).await.fold().contains("reviewer"));
    key(host, root, "space").await;
    assert!(draw(host, &root.session).await.fold().contains("☑ #1"));
    key(host, root, "space").await;
    assert!(draw(host, &root.session).await.fold().contains("pending"));
}

async fn generated_key(host: &impl HostApi, root: &mut Attachment, key_name: &str) -> Value {
    let View::Box { layout, .. } = draw(host, &root.session).await else {
        panic!("crew pane")
    };
    let action = layout
        .on_key
        .unwrap()
        .with_value(json!({"key":key_name}))
        .unwrap();
    let intent = IntentId::mint();
    root.handle.submit(
        intent.clone(),
        Input::action(action, Origin::surface("plugin:probe")),
    );
    serde_json::to_value(ack_for(root, &intent).await).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_generated_press_cannot_borrow_the_drawings_person_authority() {
    let mut server = Server::spawn(r#"{"responses":[]}"#);
    let host = ready(&mut server).await;
    let mut root = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    let reviewer = child(&host, &root, &server.cwd(), "reviewer").await;
    tab(&host, &mut root, 0).await;
    for key_name in ["s", "x"] {
        refused(generated_key(&host, &mut root, key_name).await);
    }
    key(&host, &mut root, "x").await;
    refused(generated_key(&host, &mut root, "enter").await);
    assert!(has_session(&host, &reviewer.session).await);
    key(&host, &mut root, "esc").await;
    key(&host, &mut root, "right").await;
    refused(generated_key(&host, &mut root, "enter").await);
    command(&mut root, "/rooms design reviewer").await;
    tab(&host, &mut root, 1).await;
    refused(generated_key(&host, &mut root, "c").await);
    key(&host, &mut root, "c").await;
    refused(generated_key(&host, &mut root, "enter").await);
    finish(&host, &mut server).await;
}

fn refused(receipt: Value) {
    assert!(
        receipt["result"]["message"]
            .as_str()
            .is_some_and(|s| s.contains("own control")),
        "{receipt}"
    );
}
