//! The boundary of ADR-0047 §5 against a real kernel: a level a tool sets
//! inside a turn lands on the next turn and never on the one that set it.
//! The claim is about the request a provider is handed, so it is read off the
//! fake provider's own recording rather than off any view of it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bingo_agents::AgentsPlugin;
use bingo_core::{Host, HostConfig};
use bingo_provider_fake::{FakePlugin, FakeProvider, Script};
use bingo_sdk::{
    Attachment, ClientIdentity, ContentPart, Effort, Env, Event, Frame, HostApi, Input, IntentId,
    ItemBody, OpenOptions, Origin, Plugin, SessionSelector, SessionSpec,
};
use futures::StreamExt;

/// Round one calls the tool on this session, round two answers its result,
/// and the third response belongs to a turn of its own.
const SETS_ITS_OWN_LEVEL: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SetThinking","input":{"level":"low"}}}]},
    {"steps":[{"text":"set"}]},
    {"steps":[{"text":"done"}]}
]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_level_a_turn_sets_lands_on_the_next_turn_and_not_on_its_own() {
    let home = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(
        Script::from_json(SETS_ITS_OWN_LEVEL).unwrap(),
    ));
    let host = host_on(home.path(), provider.clone()).await;
    let mut session = host
        .open(create(home.path()), who(), OpenOptions::default())
        .await
        .unwrap();

    let frames = turn(&mut session, "set your level, then go on").await;
    assert_eq!(
        said(&frames, "SetThinking"),
        "thinking: low for this session, from your next turn"
    );
    assert_eq!(
        asked(&provider),
        [Some(Effort::High), Some(Effort::High)],
        "the turn that set the level finishes at the level it started on"
    );

    turn(&mut session, "now answer").await;
    assert_eq!(
        asked(&provider),
        [Some(Effort::High), Some(Effort::High), Some(Effort::Low)],
        "and the turn after it asks for the level that was set"
    );
}

/// A foreground spawn makes the order of the three requests the tree's:
/// root, child, root (as `rpc.rs` relies on for the same reason).
const SPAWNS_AT_A_LEVEL: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{
        "prompt":"say hi","background":false,"thinking":"low"}}}]},
    {"steps":[{"text":"hi from the child"}]},
    {"steps":[{"text":"the child said hi"}]}
]}"#;

/// ADR-0047 §2 end to end: the word a spawn names is the level the child's
/// very first request asks for, while the parent goes on at its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_child_spawned_at_a_level_asks_for_it_from_its_first_turn() {
    let home = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeProvider::new(
        Script::from_json(SPAWNS_AT_A_LEVEL).unwrap(),
    ));
    let host = host_on(home.path(), provider.clone()).await;
    let mut session = host
        .open(create(home.path()), who(), OpenOptions::default())
        .await
        .unwrap();

    let frames = turn(&mut session, "spawn one").await;
    assert!(
        said(&frames, "SpawnAgent").contains("hi from the child"),
        "the child answered"
    );
    assert_eq!(
        asked(&provider),
        [Some(Effort::High), Some(Effort::Low), Some(Effort::High)],
        "the child asks for the level the spawn named, the parent for its own"
    );
}

/// A kernel with the fake provider and the sub-agents plugin, on a model
/// declared to reason at `high`: without the declaration every level is
/// filtered out of every request (ADR-0004) and this would prove nothing.
async fn host_on(home: &std::path::Path, provider: Arc<FakeProvider>) -> Arc<Host> {
    let plugins: Vec<Box<dyn Plugin>> =
        vec![Box::new(FakePlugin::new(provider)), Box::new(AgentsPlugin)];
    let config = HostConfig::new(Env::rooted(home)).with_layer(
        "cli",
        serde_json::json!({
            "provider": "fake",
            "model": "fake-1",
            "thinking": "high",
            "models": { "fake/fake-1": { "reasoning": true } },
        }),
    );
    Host::build(plugins, config).await.unwrap()
}

fn create(cwd: &std::path::Path) -> SessionSelector {
    SessionSelector::Create {
        spec: SessionSpec {
            cwd: cwd.to_path_buf(),
            ..SessionSpec::default()
        },
    }
}

fn who() -> ClientIdentity {
    ClientIdentity {
        name: "harness".into(),
        surface: "test".into(),
    }
}

/// The effort every request carried, in the order the provider was asked.
fn asked(provider: &FakeProvider) -> Vec<Option<Effort>> {
    provider.requests().iter().map(|r| r.reasoning).collect()
}

/// One line submitted and every frame until the turn it opens completes.
async fn turn(session: &mut Attachment, line: &str) -> Vec<Frame> {
    session
        .handle
        .submit(IntentId::mint(), Input::text(line, Origin::surface("test")));
    let mut seen = Vec::new();
    let folded = async {
        while let Some(frame) = session.events.next().await {
            let done = matches!(frame.event, Event::TurnCompleted { .. });
            seen.push(frame);
            if done {
                return seen;
            }
        }
        panic!("the stream ended before the turn completed");
    };
    tokio::time::timeout(std::time::Duration::from_secs(20), folded)
        .await
        .expect("the turn completes")
}

/// What a completed call of `name` said, and that it did not fail.
fn said(frames: &[Frame], name: &str) -> String {
    frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall {
                    name: called,
                    output: Some(output),
                    ..
                } if called == name => {
                    assert!(!output.is_error, "{name} failed: {output:?}");
                    Some(
                        output
                            .parts
                            .iter()
                            .filter_map(ContentPart::as_text)
                            .collect(),
                    )
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("no completed {name} call in {} frames", frames.len()))
}
