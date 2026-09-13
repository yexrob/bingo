//! Snapshot refresh through real compaction events and request assembly.

use super::*;

struct InventorySnapshot;

#[async_trait]
impl ContextContributor for InventorySnapshot {
    fn id(&self) -> &str {
        "inventory"
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        Ok(
            ContextPiece::snapshot(self.id(), "Inventory: two entries.", query.items)
                .into_iter()
                .collect(),
        )
    }
}

fn snapshot_count(items: &[Item]) -> usize {
    items
        .iter()
        .filter(|item| {
            matches!(&item.body, ItemBody::User { origin, .. }
            if origin.surface == "contributor:inventory")
        })
        .count()
}

async fn run_and_journal(cfg: &TurnConfig, frames: &mut Vec<Frame>, kind: TurnKind) {
    let host = RecordingHost::new();
    let outcome = run_turn(
        cfg,
        TurnRun {
            turn: TurnId::mint(),
            history: frames.clone(),
            generation: 0,
            cancel: CancellationToken::new(),
            kind,
        },
        &host,
    )
    .await;
    assert_eq!(outcome.status, TurnStatus::Completed);
    for event in host.events() {
        frames.push(Frame {
            seq: Seq(frames.len() as u64 + 1),
            ts: Timestamp::from_second(0).unwrap(),
            session: cfg.session.id.clone(),
            cause: None,
            event,
        });
    }
    assert_eq!(ContextView::items(frames), outcome.items);
}

#[tokio::test]
async fn compaction_replay_reissues_a_removed_snapshot_then_keeps_the_request_prefix() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(text("ready")),
        Script::Events(text("unchanged")),
        Script::Events(tool_call("Echo", json!({"say": "fresh result"}))),
        Script::Events(text("done")),
    ]);
    let mut cfg = config(
        provider.clone(),
        vec![Arc::new(EchoTool { read_only: true })],
    );
    cfg.contributors = ContributorSet {
        fixed: vec![Arc::new(InventorySnapshot)],
        sources: vec![],
    };
    let mut frames = history("hello");
    run_and_journal(&cfg, &mut frames, TurnKind::Respond).await;
    let initial_items = ContextView::items(&frames);
    assert_eq!(snapshot_count(&initial_items), 1);
    run_and_journal(&cfg, &mut frames, TurnKind::Respond).await;
    let before_cut = ContextView::items(&frames);
    assert_eq!(
        snapshot_count(&before_cut),
        1,
        "unchanged state adds no item"
    );
    assert_eq!(&before_cut[..initial_items.len()], initial_items);

    let boundary = before_cut
        .iter()
        .find(|item| matches!(item.body, ItemBody::Assistant { .. }))
        .unwrap()
        .id
        .clone();
    cfg.compactor =
        CompactorSet::fixed(Some(ScriptedCompactor::new(vec![ScriptedCompactor::cut(
            boundary.as_str(),
            9_000,
            1_000,
        )])));
    run_and_journal(&cfg, &mut frames, TurnKind::Compact { instructions: None }).await;
    assert!(
        frames
            .iter()
            .any(|frame| matches!(frame.event, Event::Compacted { .. }))
    );
    let mut replayed: Vec<Frame> =
        serde_json::from_str(&serde_json::to_string(&frames).unwrap()).unwrap();
    let after_cut = ContextView::items(&replayed);
    assert!(
        !after_cut.is_empty(),
        "refresh is tested against real surviving history"
    );
    assert_eq!(snapshot_count(&after_cut), 0);
    assert!(
        after_cut
            .iter()
            .any(|item| matches!(item.body, ItemBody::Compaction { .. }))
    );

    run_and_journal(&cfg, &mut replayed, TurnKind::Respond).await;
    let refreshed = ContextView::items(&replayed);
    assert_eq!(
        snapshot_count(&refreshed),
        1,
        "reissue once despite two rounds"
    );
    assert_eq!(&refreshed[..after_cut.len()], after_cut);
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|request| request.system == requests[0].system)
    );
    assert_eq!(
        &requests[1].messages[..requests[0].messages.len()],
        requests[0].messages
    );
    assert_eq!(
        &requests[3].messages[..requests[2].messages.len()],
        requests[2].messages
    );
    let snapshot_parts = requests[2]
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter(|part| part.as_text() == Some("Inventory: two entries."))
        .count();
    assert_eq!(
        snapshot_parts, 1,
        "the provider receives the refreshed state"
    );
}
