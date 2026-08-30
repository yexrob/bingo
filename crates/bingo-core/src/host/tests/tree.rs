//! A tree attachment (ADR-0010 §3): the root's client sees its children's
//! frames, answers their interactions, and outlives their lag.

use super::*;

static ASKING: PluginManifest = PluginManifest {
    id: "test.asking",
    version: "0",
    sdk: "^0.1",
    provides: &["policy:asking"],
    requires: &[],
    config: None,
};

/// Asks about every call.
struct AskAll;

#[async_trait]
impl PermissionPolicy for AskAll {
    fn id(&self) -> &str {
        "asking"
    }
    async fn decide(&self, _: PolicyInput<'_>) -> Decision {
        Decision::Ask {
            reason: Reason::Default,
            scope: None,
        }
    }
}

async fn asking_host(scripts: Vec<Script>) -> Arc<Host> {
    let provider = ScriptedProvider::new(scripts);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider)]),
        TestPlugin::boxed(
            &TOOLS,
            vec![Contribution::Tool(Arc::new(EchoTool { read_only: true }))],
        ),
        TestPlugin::boxed(&ASKING, vec![Contribution::Policy(Arc::new(AskAll))]),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({ "model": "m" }));
    Host::build(plugins, config).await.unwrap()
}

fn create(cwd: &str, parent: Option<ParentLink>) -> SessionSelector {
    SessionSelector::Create {
        spec: SessionSpec {
            parent,
            ..spec(cwd)
        },
    }
}

fn under(root: &SessionId) -> ParentLink {
    ParentLink {
        session: root.clone(),
        item: Some(ItemId::mint()),
    }
}

/// `(session, label)` per frame until `stop`.
async fn tagged_until(
    attachment: &mut Attachment,
    mut stop: impl FnMut(&Frame) -> bool,
) -> Vec<(SessionId, String)> {
    let mut seen = Vec::new();
    while let Some(frame) = attachment.events.next().await {
        seen.push((frame.session.clone(), label(&frame.event)));
        if stop(&frame) {
            break;
        }
    }
    seen
}

#[tokio::test]
async fn a_child_opened_after_the_attachment_streams_from_its_head_and_is_answered_through_the_root()
 {
    let host = asking_host(vec![
        Script::Events(tool_call("Echo", json!({ "v": 1 }))),
        Script::Events(text("child done")),
    ])
    .await;
    let mut root = host
        .open(create("/work", None), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let link = under(&root.session);
    let child = host
        .open(
            create("/work", Some(link.clone())),
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();

    let head = tagged_until(&mut root, |f| {
        matches!(f.event, Event::SessionUpdated { .. })
    })
    .await;
    assert_eq!(head.last().map(|(s, _)| s), Some(&child.session));
    assert_eq!(head.len(), 1, "the child's first frame is its head");

    child.handle.submit(
        IntentId::mint(),
        Input::text("echo", Origin::surface("test")),
    );
    let mut answered = false;
    let mut order = Vec::new();
    while let Some(frame) = root.events.next().await {
        assert_eq!(frame.session, child.session, "only the child is busy");
        match &frame.event {
            Event::InteractionOpened { interaction } => {
                assert_eq!(interaction.session, child.session);
                root.handle.answer(
                    IntentId::mint(),
                    interaction.id.clone(),
                    Answer::AllowOnce,
                    Activation::Pointer,
                );
                answered = true;
            }
            Event::InteractionResolved { .. } => order.push("resolved"),
            Event::ItemCompleted { item } if kind(item) == "tool/completed" => {
                order.push("tool ran")
            }
            Event::TurnCompleted { .. } => break,
            _ => {}
        }
    }
    assert!(answered);
    assert_eq!(order, ["resolved", "tool ran"]);
}

#[tokio::test]
async fn a_child_that_already_exists_is_followed_from_its_head_too() {
    let host = asking_host(vec![Script::Events(text("hi"))]).await;
    let mut root = host
        .open(create("/work", None), who(), OpenOptions::default())
        .await
        .unwrap();
    let child = host
        .open(
            create("/work", Some(under(&root.session))),
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    child.handle.submit(
        IntentId::mint(),
        Input::text("hello", Origin::surface("test")),
    );
    let mut own = child;
    while let Some(frame) = own.events.next().await {
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            break;
        }
    }

    let _ = host.close(&root.session, CloseReason::Client).await;
    root = host
        .open(
            SessionSelector::ById {
                id: root.session.clone(),
            },
            who(),
            OpenOptions::with_children(),
        )
        .await
        .unwrap();
    let mut seqs = Vec::new();
    while let Some(frame) = root.events.next().await {
        assert_eq!(frame.session, own.session);
        seqs.push(frame.seq);
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            break;
        }
    }
    assert_eq!(seqs.first(), Some(&Seq(1)), "replayed from the head");
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
}

#[tokio::test]
async fn a_lagging_child_is_healed_and_the_client_never_sees_a_marker() {
    let host = asking_host(vec![]).await;
    let mut root = host
        .open(create("/work", None), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let child = host
        .open(
            create("/work", Some(under(&root.session))),
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    // Each held delivery is two durable frames; far more than either channel
    // holds, while the client reads nothing.
    let n = 3 * crate::session::SUBSCRIBER_CAPACITY;
    let mailbox = host.live(&child.session).unwrap().mailbox;
    for i in 0..n {
        mailbox.deliver(
            IntentId::mint(),
            Input::text(format!("held {i}"), Origin::surface("agent")),
            Delivery::Hold,
        );
    }
    let mut last = Seq::ZERO;
    let mut queued = 0;
    let mut lagged = false;
    while let Some(frame) = root.events.next().await {
        if frame.session != child.session {
            continue;
        }
        lagged |= matches!(frame.event, Event::Lagged { .. });
        assert_eq!(frame.seq, last.next(), "nothing durable is skipped");
        last = frame.seq;
        if let Event::QueueChanged { entries, .. } = &frame.event {
            queued = entries.len();
            if queued == n {
                break;
            }
        }
    }
    assert!(!lagged, "the marker stays inside the kernel");
    assert_eq!(queued, n, "the last queue view is whole");
}

#[tokio::test]
async fn deleting_the_root_deletes_its_children_first() {
    let host = asking_host(vec![]).await;
    let mut root = host
        .open(create("/work", None), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let child = host
        .open(
            create("/work", Some(under(&root.session))),
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    tagged_until(&mut root, |f| {
        matches!(f.event, Event::SessionUpdated { .. })
    })
    .await;

    let root_id = root.session.clone();
    host.delete(&root_id).await.unwrap();
    let closed = tagged_until(&mut root, |f| {
        matches!(f.event, Event::SessionClosed { .. }) && f.session == root_id
    })
    .await;
    let closed: Vec<&SessionId> = closed
        .iter()
        .filter(|(_, l)| l.starts_with("SessionClosed"))
        .map(|(s, _)| s)
        .collect();
    assert_eq!(closed, [&child.session, &root.session]);
    assert!(root.events.next().await.is_none(), "the tree stream ends");
    let children = host
        .sessions(SessionFilter {
            parent: Some(root.session.clone()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    assert!(children.is_empty());
    assert_eq!(
        host.open(
            SessionSelector::ById { id: child.session },
            who(),
            OpenOptions::default()
        )
        .await
        .err()
        .map(|e| e.code),
        Some(ErrorCode::SessionNotFound)
    );
}
