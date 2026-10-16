use crate::*;
use serde_json::{Value, json};
use std::{future::Future, task};

struct Listing(Value);
#[async_trait::async_trait]
impl WireService for Listing {
    async fn call(&self, method: &str, _: Value) -> Result<Value, ServiceError> {
        assert_eq!(method, "list");
        Ok(json!({"Ok":self.0}))
    }
}
fn host(entries: bool) -> testing::TestHost {
    let host = testing::TestHost::default();
    host.on_signal(|_, _, _, _| async { Ok(()) });
    host.on_service_wire(move |key| {
        let listing = match key.as_str() {
            "memory" => json!({"memories":[],"instructions":[{"path":"/unused/AGENTS.md","tokens":1}]}),
            "experience" if entries => json!([{"id":"learned","status":"active","summary":"known","outcomes":[],"error":null,"tokens":2}]),
            "experience" => json!([]),
            _ => return None,
        };
        Some(Arc::new(Listing(listing)))
    });
    host
}
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = Box::pin(future);
    match future
        .as_mut()
        .poll(&mut task::Context::from_waker(task::Waker::noop()))
    {
        task::Poll::Ready(value) => value,
        task::Poll::Pending => panic!("the test host answers at once"),
    }
}
fn context(host: &testing::TestHost) -> HookContext {
    HookContext {
        session: SessionId::from_raw("s"),
        cwd: "/unused".into(),
        host: host.handle(),
        item: None,
        turn: None,
        provider: None,
        model: None,
    }
}

#[test]
fn bare_experience_selects_its_group_and_keeps_its_original_text() {
    for entries in [true, false] {
        let host = host(entries);
        let cx = context(&host);
        let shared = Shared::default();
        let opener = panel::Open(shared.clone(), "/unused".into(), "experience");
        let input = json!({"command":"experience","args":"","surface":"terminal"});
        let expected = input.clone();
        let reply = ready(opener.chain(
            catalogue::COMMAND_RUN,
            &cx,
            input,
            Next::new(move |input| {
                assert_eq!(input, expected);
                Box::pin(async {
                    Ok(json!(CommandOutcome::View {
                        view: View::text("experience list")
                    }))
                })
            }),
        ))
        .unwrap();
        assert_eq!(
            reply["action"],
            json!({"name":"Pane","args":{"plugin":ID,"id":REQUEST}})
        );
        assert_eq!(reply["fallback"], "experience list");
        let state = shared.lock().unwrap();
        let cursor = &state[&cx.session];
        assert_eq!(cursor.selected, 1, "skip the loaded instruction");
        let view = draw::tree(&cursor.groups, cursor.selected, None, 5, 80).fold();
        if entries {
            assert!(matches!(
                cursor.groups[2].rows[0].target,
                rows::Target::Experience(_)
            ));
            assert!(view.contains("learned"));
        } else {
            assert!(
                view.contains("experience"),
                "the empty group stays visible: {view}"
            );
        }
    }
}

#[test]
fn experience_verbs_and_other_surfaces_keep_the_original_command() {
    let host = testing::TestHost::default();
    let cx = context(&host);
    let opener = panel::Open(Shared::default(), "/unused".into(), "experience");
    for (args, surface) in [
        ("forget learned", Some("terminal")),
        ("", Some("text")),
        ("", Some("chat")),
        ("", None),
        ("", Some("terminal")),
    ] {
        let input = json!({"command":"experience","args":args,"surface":surface});
        let expected = input.clone();
        let reply = ready(opener.chain(
            catalogue::COMMAND_RUN,
            &cx,
            input,
            Next::new(move |input| {
                assert_eq!(input, expected);
                Box::pin(async { Ok(json!({"text":"original command"})) })
            }),
        ))
        .unwrap();
        assert_eq!(reply, json!({"text":"original command"}));
    }
}
