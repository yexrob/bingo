//! The panel's person-only lines through an isolated real process.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use crate::support;
use bingo_sdk::render::{Component, RenderRequest, SurfaceName, Viewport};
use bingo_sdk::*;
use serde_json::{Value, json};
use support::{LIMIT, Server, ack_for, create, ready, who};
async fn command(a: &mut Attachment, line: &str) -> Value {
    let id = IntentId::mint();
    a.handle.submit(
        id.clone(),
        Input::text(line, Origin::surface("tui").on(SurfaceName::Terminal)),
    );
    let outcome = ack_for(a, &id).await;
    assert!(
        matches!(outcome, IntentOutcome::Applied { .. }),
        "{outcome:?}"
    );
    serde_json::to_value(outcome).unwrap()["result"].clone()
}
fn request() -> RenderRequest {
    RenderRequest {
        component: Component::Pane,
        request_id: "schedules".into(),
        surface: SurfaceName::Terminal,
        props: json!({"owner":"bingo.mod.schedule"}),
        viewport: Viewport {
            place: None,
            columns: 108,
            rows: 20,
        },
    }
}
async fn press(host: &impl HostApi, a: &mut Attachment, key: &str) -> Value {
    let tree = host.render(&a.session, request()).await.unwrap();
    let View::Box { layout, .. } = tree else {
        panic!("pane")
    };
    let mut action = layout.on_key.unwrap();
    action.args["value"] = json!({"key":key});
    let id = IntentId::mint();
    a.handle
        .submit(id.clone(), Input::action(action, Origin::surface("tui")));
    let ack = serde_json::to_value(ack_for(a, &id).await).unwrap();
    assert_eq!(ack["result"]["drawnBy"], "bingo.mod.schedule", "{ack}");
    ack["result"]["surface"]["args"]["value"].clone()
}
async fn stop(host: &bingo_surface_rpc::RemoteKernel, server: &mut Server) {
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
async fn person_lines_and_panel_keys_share_one_store() {
    let mut server = Server::spawn(r#"{"responses":[{"steps":[{"text":"ran once"}]}]}"#);
    let host = ready(&mut server).await;
    let mut a = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut a, "/schedule add every 2h -- check the build").await;
    confirm_model_entry(&host, &mut a, &server.cwd()).await;
    command(&mut a, "/schedule").await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("check the build")
    );
    let pause = press(&host, &mut a, "space").await;
    command(&mut a, pause["run"].as_str().unwrap()).await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("off")
    );
    let resume = press(&host, &mut a, "space").await;
    command(&mut a, resume["run"].as_str().unwrap()).await;
    let run = press(&host, &mut a, "r").await;
    command(&mut a, run["run"].as_str().unwrap()).await;
    let sessions = host.sessions(SessionFilter::default()).await.unwrap();
    let target = sessions
        .iter()
        .find(|s| s.key.as_deref().is_some_and(|k| k.starts_with("schedule/")))
        .unwrap();
    assert_eq!(
        press(&host, &mut a, "right").await["view"],
        json!(target.id)
    );
    command(&mut a, "/schedule").await;
    press(&host, &mut a, "d").await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("its session stays")
    );
    let forget = press(&host, &mut a, "enter").await;
    command(&mut a, forget["run"].as_str().unwrap()).await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("no schedules yet")
    );
    assert!(
        host.sessions(SessionFilter::default())
            .await
            .unwrap()
            .iter()
            .any(|s| s.id == target.id)
    );
    stop(&host, &mut server).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn disabled_panel_keeps_lines_and_the_wake_on_status() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    std::fs::write(
        home.path().join(".bingo/settings.json"),
        r#"{"enabledPlugins":{"bingo.mod.schedule":false},"update":{"check":false}}"#,
    )
    .unwrap();
    let mut server = Server::spawn_at(
        home,
        r#"{"responses":[{"steps":[{"toolCall":{"name":"Wake","input":{"after":"4m","note":"check CI"}}}]},{"steps":[{"text":"waiting"}]}]}"#,
        &[],
    );
    let host = ready(&mut server).await;
    let mut a = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut a, "/schedule add every 2h -- check").await;
    assert!(command(&mut a, "/schedule").await["view"].is_object());
    a.handle.submit(
        IntentId::mint(),
        Input::text("set a wake", Origin::surface("tui")),
    );
    support::until_completed(&mut a).await;
    command(&mut a, "/status").await;
    let status = RenderRequest {
        component: Component::Status,
        request_id: "status".into(),
        props: json!({}),
        ..request()
    };
    let tree = host.render(&a.session, status.clone()).await.unwrap();
    assert!(tree.fold().contains("check CI"), "{tree:?}");
    for _ in 0..10 {
        let tree = host.render(&a.session, status.clone()).await.unwrap();
        let selected = tree.fold().contains("❯ wake");
        let View::Box { layout, .. } = tree else {
            panic!("status")
        };
        let mut action = layout.on_key.unwrap();
        action.args["value"] = json!({"key":if selected {"enter"} else {"down"}});
        let intent = IntentId::mint();
        a.handle.submit(
            intent.clone(),
            Input::action(action, Origin::surface("tui")),
        );
        let ack = serde_json::to_value(ack_for(&mut a, &intent).await).unwrap();
        if selected {
            assert_eq!(
                ack["result"]["surface"],
                json!({"name":"Composer","args":{"text":"/wake off"}})
            );
            break;
        }
    }
    command(&mut a, "/wake off").await;
    assert!(
        !host
            .render(&a.session, status)
            .await
            .unwrap()
            .fold()
            .contains("check CI")
    );
    stop(&host, &mut server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_panel_is_dormant_but_runs_once() {
    let mut owner = Server::spawn(r#"{"responses":[{"steps":[{"text":"manual completed"}]}]}"#);
    let first = ready(&mut owner).await;
    let mut process = tokio::process::Command::from(support::command::bingo());
    process
        .args(["serve", "--stdio", "--cwd"])
        .arg(owner.cwd())
        .envs(support::home::home_env(owner.cwd()))
        .env("BINGO_FAKE_SCRIPT", owner.cwd().join("script.json"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut second = bingo_process::tree(process).unwrap();
    let host = bingo_surface_rpc::RemoteKernel::connect(
        second.take_stdout().unwrap(),
        second.take_stdin().unwrap(),
    );
    host.initialize(who()).await.unwrap();
    let mut a = host
        .open(create(owner.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut a, "/schedule add every 2h -- manual check").await;
    command(&mut a, "/schedule").await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("dormant — held by another bingo")
    );
    let run = press(&host, &mut a, "r").await;
    command(&mut a, run["run"].as_str().unwrap()).await;
    let sessions = host.sessions(SessionFilter::default()).await.unwrap();
    let scheduled = sessions
        .iter()
        .find(|s| s.key.as_deref().is_some_and(|k| k.starts_with("schedule/")))
        .unwrap();
    let mut turn = host
        .open(
            SessionSelector::ById {
                id: scheduled.id.clone(),
            },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    if turn.snapshot.summary.busy {
        support::until_completed(&mut turn).await;
    }
    let journal = std::fs::read_to_string(
        owner
            .sessions_dir()
            .join(scheduled.id.as_str())
            .join("journal.jsonl"),
    )
    .unwrap();
    assert_eq!(
        journal
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|frame| frame["event"]["type"] == "turnStarted")
            .count(),
        1,
        "{journal}"
    );
    host.shutdown().await.unwrap();
    assert!(
        tokio::time::timeout(LIMIT, second.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    stop(&first, &mut owner).await;
}

async fn confirm_model_entry(host: &impl HostApi, a: &mut Attachment, home: &std::path::Path) {
    let directory = home.join(".bingo/data/schedules");
    let path = std::fs::read_dir(directory)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|e| e == "json"))
        .unwrap();
    let mut entry: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(entry["by"], "person");
    assert!(entry.get("permissionMode").is_none());
    entry["by"] = json!("tool");
    std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
    command(a, "/schedule").await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("model")
    );
    assert!(press(host, a, "c").await["run"].is_null());
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("as yours")
    );
    let confirm = press(host, a, "enter").await;
    let line = confirm["run"].as_str().unwrap();
    assert!(line.starts_with("/schedule confirm "));
    reject_changed_confirmation(a, line, &path, &mut entry).await;
    command(a, line).await;
    assert!(
        host.render(&a.session, request())
            .await
            .unwrap()
            .fold()
            .contains("you")
    );
    let entry: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(entry["by"], "person");
}

async fn reject_changed_confirmation(
    a: &mut Attachment,
    line: &str,
    path: &std::path::Path,
    entry: &mut Value,
) {
    let original = entry["text"].clone();
    entry["text"] = json!("changed after Enter");
    std::fs::write(path, serde_json::to_vec(entry).unwrap()).unwrap();
    let intent = IntentId::mint();
    a.handle
        .submit(intent.clone(), Input::text(line, Origin::surface("tui")));
    let IntentOutcome::Rejected { error } = ack_for(a, &intent).await else {
        panic!("a stale confirmation must be refused");
    };
    assert!(error.message.contains("changed since it was shown"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(saved["by"], "tool");
    entry["text"] = original;
    std::fs::write(path, serde_json::to_vec(entry).unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn bare_wake_selects_its_panel_row_and_d_ends_only_the_wake() {
    let mut server = Server::spawn(
        r#"{"responses":[{"steps":[{"toolCall":{"name":"Wake","input":{"after":"4m","note":"check CI"}}}]},{"steps":[{"text":"waiting"}]}]}"#,
    );
    let host = ready(&mut server).await;
    let mut a = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut a, "/schedule add every 2h -- keep this schedule").await;
    a.handle.submit(
        IntentId::mint(),
        Input::text("set a wake", Origin::surface("tui")),
    );
    support::until_completed(&mut a).await;
    let opened = command(&mut a, "/wake").await;
    assert_eq!(opened["surface"]["name"], "Pane", "{opened}");
    let tree = host.render(&a.session, request()).await.unwrap();
    assert!(tree.fold().contains("❯ wake"), "{tree:?}");
    assert!(tree.fold().contains("check CI"), "{tree:?}");
    assert!(tree.fold().contains("keep this schedule"), "{tree:?}");
    let cancel = press(&host, &mut a, "d").await;
    assert_eq!(cancel, json!({"run":"/wake off"}));
    let ended = command(&mut a, cancel["run"].as_str().unwrap()).await;
    assert!(ended["surface"].is_null(), "{ended}");
    let tree = host.render(&a.session, request()).await.unwrap();
    assert!(!tree.fold().contains("check CI"), "{tree:?}");
    assert!(tree.fold().contains("keep this schedule"), "{tree:?}");
    stop(&host, &mut server).await;
}
