use crate::{opening::Opening, state::Tab};
use bingo_sdk::*;
use serde_json::{Value, json};

fn context() -> (testing::TestHost, HookContext) {
    let host = testing::TestHost::default();
    host.on_signal(|_, _, _, _| async { Ok(()) });
    let cx = HookContext {
        session: SessionId::from_raw("ses_parent"),
        turn: None,
        item: None,
        cwd: "/unused".into(),
        provider: None,
        model: None,
        host: host.handle(),
    };
    (host, cx)
}

fn fallback() -> CommandOutcome {
    CommandOutcome::View {
        view: View::Table {
            headers: vec!["name".into()],
            rows: vec![vec!["plain result".into()]],
        },
    }
}

#[tokio::test]
async fn every_bare_folded_name_opens_its_tab_and_preserves_the_text_view() {
    for (name, tab) in crate::opening::FOLDED {
        for surface in ["terminal", "desktop"] {
            let (_, cx) = context();
            let shared = crate::Shared::default();
            let answer = Opening {
                shared: shared.clone(),
                name,
                tab,
            }
            .chain(
                catalogue::COMMAND_RUN,
                &cx,
                json!({"command":name,"args":"","surface":surface}),
                Next::new(|_| Box::pin(async { Ok(json!(fallback())) })),
            )
            .await
            .unwrap();
            let CommandOutcome::Surface {
                action,
                fallback: text,
            } = serde_json::from_value(answer).unwrap()
            else {
                panic!("the bare command opens crew");
            };
            assert_eq!(action.name, "Pane");
            assert_eq!(action.args, json!({"plugin":crate::ID,"id":"crew"}));
            let CommandOutcome::View { view } = fallback() else {
                unreachable!()
            };
            assert_eq!(text, Some(view.fold_markdown()));
            assert_eq!(shared.lock().unwrap()[&cx.session].tab, tab);
        }
    }
}

#[tokio::test]
async fn an_empty_roster_opens_its_tab_and_preserves_the_plain_message() {
    let (_, cx) = context();
    let shared = crate::Shared::default();
    let answer = Opening {
        shared: shared.clone(),
        name: "agents",
        tab: Tab::Agents,
    }
    .chain(
        catalogue::COMMAND_RUN,
        &cx,
        json!({"command":"agents","args":"","surface":"terminal"}),
        Next::new(|_| {
            Box::pin(async {
                Ok(json!(CommandOutcome::Applied {
                    message: Some("no agents are running in this session".into()),
                }))
            })
        }),
    )
    .await
    .unwrap();
    let CommandOutcome::Surface { action, fallback } = serde_json::from_value(answer).unwrap()
    else {
        panic!("an empty roster still opens crew");
    };
    assert_eq!(action.name, "Pane");
    assert_eq!(
        fallback.as_deref(),
        Some("no agents are running in this session")
    );
    assert_eq!(shared.lock().unwrap()[&cx.session].tab, Tab::Agents);
}

#[tokio::test]
async fn arguments_and_nonvisual_callers_keep_the_original_command() {
    for (name, tab) in crate::opening::FOLDED {
        for input in [
            json!({"command":name,"args":"stop x","surface":"terminal"}),
            json!({"command":name,"args":""}),
            json!({"command":name,"args":"","surface":"text"}),
        ] {
            let (host, cx) = context();
            let shared = crate::Shared::default();
            let wanted = input.clone();
            let answer = Opening {
                shared: shared.clone(),
                name,
                tab,
            }
            .chain(
                catalogue::COMMAND_RUN,
                &cx,
                input,
                Next::new(move |seen| {
                    Box::pin(async move {
                        assert_eq!(seen, wanted);
                        Ok(json!(fallback()))
                    })
                }),
            )
            .await
            .unwrap();
            assert_eq!(answer, json!(fallback()));
            assert!(shared.lock().unwrap().is_empty());
            assert!(host.calls().is_empty());
        }
    }
}

#[tokio::test]
async fn crew_arguments_select_each_tab_and_refuse_unknown_tabs() {
    let (_, hook) = context();
    let cx = CommandContext {
        session: hook.session,
        cwd: hook.cwd,
        host: hook.host,
        cancel: Default::default(),
    };
    let shared = crate::Shared::default();
    let command = crate::command::Open(shared.clone());
    for (args, tab) in
        std::iter::once(("", Tab::Agents)).chain(Tab::ALL.into_iter().map(|tab| (tab.label(), tab)))
    {
        command.run(args, &cx).await.unwrap();
        assert_eq!(shared.lock().unwrap()[&cx.session].tab, tab);
    }
    assert!(command.run("unknown", &cx).await.is_err());
    let ArgSpec::Words { values, .. } = command.spec().args else {
        panic!("crew offers its tabs inline");
    };
    assert_eq!(values, ["agents", "rooms", "tasks", "team"]);
}

#[test]
fn crew_registers_its_fold_alongside_its_openers() {
    let mut registrar = Registrar::new(
        crate::ID,
        Value::Null,
        Env::rooted("/unused"),
        Default::default(),
    );
    crate::Crew::default().register(&mut registrar).unwrap();
    let folds: Vec<_> = registrar
        .into_contributions()
        .into_iter()
        .filter_map(|entry| {
            if let Contribution::CommandFold(fold) = entry {
                Some(fold)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].into, "crew");
    assert_eq!(folds[0].names, ["agents", "tasks", "room", "rooms", "team"]);
}
