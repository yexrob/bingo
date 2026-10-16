use crate::tests::{Fleet, command_context};
use bingo_sdk::*;
use serde_json::json;
use std::sync::Arc;

async fn answer(verb: &str, via_service: bool, peer: bool) -> Result<String, String> {
    let fleet = Fleet::default();
    let root = fleet.root();
    fleet.child(&root, "reviewer");
    let caller = if peer {
        fleet.child(&root, "scout")
    } else {
        root
    };
    let args = if matches!(verb, "message" | "reopen") {
        "reviewer hello"
    } else {
        "reviewer"
    };
    if via_service {
        let service = Arc::new(crate::service::AgentService::new(
            Env::rooted("/tmp"),
            Arc::default(),
            Arc::default(),
        ));
        let host = fleet.handle();
        service.bind(&host);
        let wire = bingo_services::agents_control::wire(service);
        return wire
            .call(
                verb,
                json!({"session":caller,"name":"reviewer","text":"hello"}),
            )
            .await
            .map(|v| v.as_str().unwrap().to_string())
            .map_err(|e| e.0);
    }
    match crate::AgentsCommand
        .run(&format!("{verb} {args}"), &command_context(&caller, &fleet))
        .await
    {
        Ok(CommandOutcome::Applied { message }) => Ok(message.unwrap_or_default()),
        Ok(other) => panic!("unexpected {other:?}"),
        Err(error) => Err(error.message),
    }
}
#[tokio::test]
async fn commands_and_services_share_receipts_and_refusals() {
    for verb in ["stop", "dismiss", "message", "reopen"] {
        for peer in [false, true] {
            let direct = answer(verb, false, peer).await;
            let service = answer(verb, true, peer).await;
            // Dismiss receipts include the fresh session identifier.
            let normalize = |v: Result<String, String>| {
                v.map(|s| {
                    s.split_whitespace()
                        .map(|word| {
                            if word.starts_with("ses_") {
                                "<session>"
                            } else {
                                word
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
            };
            assert_eq!(normalize(direct), normalize(service), "{verb} peer={peer}");
        }
    }
}

#[tokio::test]
async fn team_service_and_command_share_the_declared_and_seated_roles() {
    let tree = crate::tests::Tree::new();
    let cwd = tree.team(
        "work",
        r#"{"roles":[{"name":"reviewer","agent":"reviewer"},{"name":"scout"}]}"#,
    );
    let mut parent = crate::tests::summary("ses_parent", None, None);
    parent.cwd = cwd.display().to_string();
    let mut reviewer =
        crate::tests::summary("ses_child", Some("Reviewer"), Some(parent.id.clone()));
    reviewer.busy = true;
    let host = testing::TestHost::default();
    let own = parent.clone();
    host.on_summary(move |session| {
        let own = own.clone();
        async move {
            assert_eq!(session, own.id);
            Ok(own)
        }
    });
    let child = reviewer.clone();
    host.on_sessions(move |filter| {
        let child = child.clone();
        async move {
            assert_eq!(
                filter.parent,
                child.parent.as_ref().map(|parent| parent.session.clone())
            );
            Ok(vec![child])
        }
    });
    let handle = host.handle();
    let service = Arc::new(crate::service::AgentService::new(
        Env::rooted(tree.root()),
        Arc::default(),
        Arc::default(),
    ));
    service.bind(&handle);
    let answer = bingo_services::agents_control::wire(service)
        .call("team", json!({"session":parent.id}))
        .await
        .unwrap();
    let rows: Vec<bingo_services::agents_control::TeamRow> =
        serde_json::from_value(answer).unwrap();
    assert_eq!(rows[0].role, "reviewer");
    assert_eq!(rows[0].agent.as_deref(), Some("reviewer"));
    assert_eq!(rows[0].summary.as_ref(), Some(&reviewer));
    assert_eq!(rows[1].role, "scout");
    assert!(rows[1].summary.is_none());
    let cx = CommandContext {
        session: parent.id,
        cwd,
        host: handle,
        cancel: Default::default(),
    };
    let CommandOutcome::View {
        view: View::Table { rows: plain, .. },
    } = crate::TeamCommand.run("", &cx).await.unwrap()
    else {
        panic!("the command retains its table");
    };
    assert_eq!(
        plain[0],
        ["reviewer", "reviewer", reviewer.id.as_str(), "working"]
    );
    assert_eq!(plain[1], ["scout", "-", "", "not seated"]);
}
