use super::testing::Journal;
use super::*;
use crate::InstructionsContributor;
use bingo_sdk::Event;
use serde_json::Value;

#[test]
fn persisted_blocks_round_trip_through_the_extension_fold() {
    let payload: Value = serde_json::from_str(include_str!("payload.json")).expect("fixture");
    let baseline: Baseline = serde_json::from_value(payload.clone()).expect("baseline");
    assert_eq!(baseline.history_generation, 7);
    assert_eq!(baseline.blocks.len(), 2);
    assert_eq!(serde_json::to_value(&baseline).expect("payload"), payload);
    let journal = Journal::at(std::path::Path::new("/work"));
    journal.apply(Event::Extension {
        plugin: PLUGIN.into(),
        kind: CONTRIBUTORS[0].into(),
        payload: payload.clone(),
    });
    let replay: bingo_sdk::SessionState =
        serde_json::from_value(serde_json::to_value(&*journal.state()).expect("state"))
            .expect("replayed state");
    assert_eq!(replay.extensions[PLUGIN][CONTRIBUTORS[0]], payload);
}

#[tokio::test]
async fn disk_changes_do_not_rewrite_an_existing_instruction_baseline() {
    let config = tempfile::tempdir().expect("config");
    let cwd = tempfile::tempdir().expect("cwd");
    let path = config.path().join("AGENTS.md");
    std::fs::write(&path, "first instructions").expect("instructions");
    let contributor = InstructionsContributor::new(config.path().into());
    let journal = Journal::at(cwd.path());
    let first = journal.contribute(&contributor, cwd.path()).await;
    std::fs::write(&path, "second instructions").expect("changed instructions");
    let second = journal.contribute(&contributor, cwd.path()).await;
    assert_eq!(first, second);
    assert_eq!(journal.state().seq.0, 1, "capture only once");
}

#[tokio::test]
async fn retained_blocks_never_poll_the_renderer_even_in_a_later_turn() {
    let cwd = tempfile::tempdir().expect("cwd");
    let journal = Journal::at(cwd.path());
    let asked = crate::query::Asked::at(cwd.path());
    let host = journal.handle();
    let mut query = asked.query();
    query.host = &host;
    let blocks = vec![SystemBlock {
        text: "exact bytes\n".into(),
        cache: false,
    }];
    let first = contribute(CONTRIBUTORS[0], query, async { blocks.clone() })
        .await
        .expect("capture");
    let later = bingo_sdk::TurnId::from_raw("trn_later");
    query.turn = &later;
    query.round = 9;
    let second = contribute(CONTRIBUTORS[0], query, async { panic!("must not render") })
        .await
        .expect("retained");
    assert_eq!(first, second);
    assert_eq!(
        first,
        blocks
            .into_iter()
            .map(ContextPiece::System)
            .collect::<Vec<_>>()
    );
    assert_eq!(journal.state().seq.0, 1);
}

#[tokio::test]
async fn an_unavailable_journal_does_not_render_unrecorded_knowledge() {
    let asked = crate::query::Asked::at(std::path::Path::new("/work"));
    let result = contribute(CONTRIBUTORS[0], asked.query(), async {
        panic!("must not render")
    })
    .await;
    assert!(result.is_err());
}
